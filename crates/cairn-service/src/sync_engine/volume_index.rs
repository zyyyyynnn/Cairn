// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Indexed record table of one volume: ingestion, journal changes, folder totals and compaction.

use super::volume_status::VolumeStatus;
use cairn_core::arena::NamePool;
use cairn_core::model::FileRecord;
use cairn_core::path_cache::DirectoryPathCache;
use cairn_core::sort::PresortedIndices;
use cairn_core::treemap::DirectorySizeIndex;
use cairn_ntfs::mft::ExtractedItem;
use cairn_ntfs::usn::{file_record_flags, UsnChange};
use cairn_protocol::message::FileRowData;
use cairn_protocol::status::{VolumeState, VolumeStatusSummary};
use std::collections::{HashMap, HashSet};

pub(super) struct BitSet {
    pub(super) words: Vec<u64>,
}

impl BitSet {
    pub(super) fn with_capacity(bits: usize) -> Self {
        let word_count = bits.div_ceil(64);
        Self {
            words: vec![0; word_count],
        }
    }

    #[inline(always)]
    pub(super) fn insert(&mut self, bit: usize) -> bool {
        let word_idx = bit / 64;
        let bit_mask = 1u64 << (bit % 64);
        if let Some(word) = self.words.get_mut(word_idx) {
            let was_set = (*word & bit_mask) != 0;
            *word |= bit_mask;
            !was_set
        } else {
            false
        }
    }

    /// Whether `bit` is a member. A bit beyond the capacity is never a member.
    #[inline(always)]
    pub(super) fn contains(&self, bit: usize) -> bool {
        self.words
            .get(bit / 64)
            .is_some_and(|word| word & (1u64 << (bit % 64)) != 0)
    }

    /// Unions `other` into `self`. Both sets describe the same record table, so their word
    /// vectors have equal length.
    pub(super) fn merge(&mut self, other: &Self) {
        for (word, &incoming) in self.words.iter_mut().zip(&other.words) {
            *word |= incoming;
        }
    }
}

/// A smaller volume is rebuilt quickly enough that compaction on every start would cost more than
/// it returns.
const COMPACT_MIN_RECORDS: usize = 100_000;

/// Compaction only pays off once at least this percentage of the table is dead weight.
const COMPACT_MIN_DROPPABLE_PERCENT: usize = 25;

/// Compaction verdict for a record's ancestor chain. A tombstone is not part of it: a dead parent
/// does not make a live child service data, but a tombstoned directory can still sit below an
/// excluded root, so the walk has to continue through it.
const SUBTREE_UNKNOWN: u8 = 0;
const SUBTREE_KEPT: u8 = 1;
const SUBTREE_EXCLUDED: u8 = 2;
const SUBTREE_IN_PROGRESS: u8 = 3;

/// Applies a signed byte delta to an unsigned aggregate. Saturating keeps a corrupt journal from
/// wrapping a directory's total around; a negative step cannot go below zero.
fn apply_size_delta(value: u64, delta: i128) -> u64 {
    if delta >= 0 {
        value.saturating_add(delta as u64)
    } else {
        value.saturating_sub((-delta) as u64)
    }
}

pub struct VolumeIndex {
    pub drive_letter: char,
    pub volume_guid: String,
    pub(super) status: VolumeStatus,
    pub(crate) records: Vec<FileRecord>,
    pub(crate) arena: NamePool,
    pub(crate) path_cache: DirectoryPathCache,
    pub(crate) presorted: PresortedIndices,
    pub(crate) presorted_dirty: bool,
    pub(crate) frn_to_idx: HashMap<u64, Vec<u32>>,
    pub(crate) pending_renames: HashMap<u64, (u64, String)>,
    pub(crate) pending_data_changes: HashSet<u64>,
    pub(crate) pending_content_deletions: HashSet<(u64, u64)>,
    /// FRNs of service-owned directories and everything indexed below them. They are filtered out
    /// of scans, journal events and loaded caches so the index never describes its own writes.
    pub(crate) excluded_frns: HashSet<u64>,
    pub last_usn: i64,
    pub journal_id: u64,
    pub volume_serial: u64,
    pub usn_enabled: bool,
    /// Monotonic version of the record table. Every operation that rebuilds or reorders the
    /// table (`clear`, `ingest_items`, compaction) increments it, so a query session can refuse
    /// record indices that no longer describe the same rows.
    pub(crate) generation: u64,
    /// Live records (tombstones excluded), maintained incrementally so status summaries stay O(1).
    pub(super) live_count: u64,
    /// Whether `FileRecord.size` on directory rows is the recursive aggregate of their indexed
    /// descendants. A cache written before folder sizes existed cannot prove this, so a load
    /// aggregates once and rewrites the flag; every later update maintains the totals in place.
    pub(crate) directory_sizes_valid: bool,
}

impl VolumeIndex {
    pub fn new(drive_letter: char) -> Self {
        Self {
            drive_letter,
            volume_guid: format!("VOL_{}:", drive_letter),
            status: VolumeStatus::rebuilding(),
            records: Vec::new(),
            arena: NamePool::default(),
            path_cache: DirectoryPathCache::new(),
            presorted: PresortedIndices::default(),
            presorted_dirty: false,
            frn_to_idx: HashMap::new(),
            pending_renames: HashMap::new(),
            pending_data_changes: HashSet::new(),
            pending_content_deletions: HashSet::new(),
            excluded_frns: HashSet::new(),
            last_usn: 0,
            journal_id: 0,
            volume_serial: 0,
            usn_enabled: true,
            generation: 0,
            live_count: 0,
            // An empty table has no totals to be wrong about; the invariant holds trivially.
            directory_sizes_valid: true,
        }
    }

    /// Clears all records and indexes for this volume. The excluded service data roots survive:
    /// they are configuration derived from the data layout, not index data, so a rebuild must keep
    /// filtering the same subtree.
    pub fn clear(&mut self) {
        self.records.clear();
        self.live_count = 0;
        self.arena.clear();
        self.path_cache.clear();
        self.presorted = PresortedIndices::default();
        self.presorted_dirty = false;
        self.frn_to_idx.clear();
        self.pending_renames.clear();
        self.pending_data_changes.clear();
        self.pending_content_deletions.clear();
        self.last_usn = 0;
        self.journal_id = 0;
        // Record indices captured by an open session stop describing rows the moment the table is
        // emptied; the generation lets the pagination path notice that instead of serving a row
        // that now belongs to another file.
        self.generation = self.generation.wrapping_add(1);
        self.directory_sizes_valid = true;
    }

    /// Registers directory FRNs whose whole subtree must stay out of this volume's index. Adding a
    /// root that is already known is a no-op, so callers may seed the set more than once.
    pub(crate) fn set_excluded_roots(&mut self, roots: impl IntoIterator<Item = u64>) {
        self.excluded_frns.extend(roots);
    }

    /// Flushes and processes pending data changes (debounce batch size/timestamp refresh) (CNT-05).
    pub fn flush_pending_data_changes(&mut self) -> HashSet<u64> {
        std::mem::take(&mut self.pending_data_changes)
    }

    /// Drains `(volume_id, frn)` pairs whose full-text document must be removed. Deletion is
    /// separate from data changes because the record may already be a tombstone by drain time.
    pub fn flush_pending_content_deletions(&mut self) -> HashSet<(u64, u64)> {
        std::mem::take(&mut self.pending_content_deletions)
    }

    pub(crate) fn content_volume_id(&self) -> u64 {
        FileRecord::volume_id_from_drive(self.drive_letter)
            .expect("volume drive letter is an ASCII letter") as u64
    }

    /// Atomically persists volume records and name pool to the specified directory.
    pub fn save_to_dir(
        &self,
        dir: &std::path::Path,
    ) -> Result<std::path::PathBuf, crate::persistence::PersistenceError> {
        crate::persistence::save_volume_index_atomic(
            dir,
            self.drive_letter,
            self.volume_serial,
            self.journal_id,
            self.last_usn,
            &self.records,
            &self.arena,
            self.directory_sizes_valid,
        )
    }

    /// Read-only slice of indexed records.
    pub fn records(&self) -> &[FileRecord] {
        &self.records
    }

    /// Retrieves the original name of a record.
    pub fn record_name(&self, rec: &FileRecord) -> &str {
        self.arena.get_name(rec.name_offset, rec.name_len)
    }

    /// Reconstructs full row data and highlights for a matched record index, encapsulating
    /// arena string extraction and path cache traversal.
    pub fn build_file_row<F>(
        &self,
        record_idx: u32,
        folded_terms: &[Vec<u8>],
        snippet_lookup: F,
    ) -> Option<FileRowData>
    where
        F: FnOnce(u64) -> Option<String>,
    {
        let rec = self.records.get(record_idx as usize)?;
        let name = self.arena.get_name(rec.name_offset, rec.name_len);
        let parent_path = self.path_cache.get_parent_path(
            &self.records,
            &self.arena,
            self.drive_letter,
            record_idx,
        );

        let mut name_highlights = Vec::new();
        let mut path_highlights = Vec::new();

        if !folded_terms.is_empty() {
            let folded_name = self.arena.get_folded(rec.name_offset, rec.name_len);
            for term in folded_terms {
                let term_bytes = term.as_slice();
                if term_bytes.is_empty() {
                    continue;
                }
                let mut curr = 0;
                while let Some(pos) = memchr::memmem::find(&folded_name[curr..], term_bytes) {
                    let start = curr + pos;
                    let end = start + term_bytes.len();
                    name_highlights.push((start, end));
                    curr = end;
                }
            }

            let path_bytes = parent_path.as_bytes();
            let mut folded_path = path_bytes.to_vec();
            folded_path.make_ascii_lowercase();
            for term in folded_terms {
                let term_bytes = term.as_slice();
                if term_bytes.is_empty() {
                    continue;
                }
                let mut curr = 0;
                while let Some(pos) = memchr::memmem::find(&folded_path[curr..], term_bytes) {
                    let start = curr + pos;
                    let end = start + term_bytes.len();
                    path_highlights.push((start, end));
                    curr = end;
                }
            }

            if name_highlights.len() > 1 {
                name_highlights.sort_unstable();
                name_highlights.dedup();
            }
            if path_highlights.len() > 1 {
                path_highlights.sort_unstable();
                path_highlights.dedup();
            }
        }

        Some(FileRowData {
            frn: rec.frn,
            name: name.to_string(),
            path: parent_path.to_string(),
            size: rec.size,
            modified_ts: rec.modified_ts,
            created_ts: rec.created_ts,
            is_dir: rec.is_dir(),
            flags: rec.flags,
            name_highlights,
            path_highlights,
            snippet: snippet_lookup(rec.frn),
        })
    }

    /// Ingests parsed extracted items (from MFT scan or test fixtures).
    pub fn ingest_items(&mut self, items: Vec<ExtractedItem>) {
        self.clear();
        self.directory_sizes_valid = false;
        self.append_items(items);
        self.recompute_directory_sizes();

        // Build presorted indices
        self.presorted = PresortedIndices::build(
            &self.records,
            &self.arena,
            &self.path_cache,
            self.drive_letter,
        );
        self.presorted_dirty = false;
        // A snapshot by itself proves nothing about freshness: the journal position is only known
        // once the scan has been reconciled with the change journal. Until then the volume is
        // still being rebuilt, so no path can report it as in sync with data it never read.
        self.status.set(VolumeState::Rebuilding);
    }

    /// Appends extracted items without discarding what the volume already holds. A scan
    /// delivers batches while it walks, so the table can show rows before the walk finishes;
    /// the presorted arrays go stale and queries fall back to their full sort until the caller
    /// finishes the volume scan.
    ///
    /// Parents must arrive before their children, which is the order the scanner walks a tree in.
    /// A parent that is not present stays `ROOT_PARENT`, exactly as in [`Self::ingest_items`].
    pub fn append_items(&mut self, items: Vec<ExtractedItem>) {
        self.records.reserve(items.len());
        self.arena.reserve(items.len() * 24);
        self.frn_to_idx.reserve(items.len());
        let first_new = self.records.len();
        let mut parent_frns: Vec<u64> = Vec::with_capacity(items.len());

        for item in items {
            // Service-owned data is not user content, and every Tantivy commit or index save writes
            // more of it. The roots arrive by FRN, so a hard link of an excluded file is covered
            // too. An excluded parent condemns the child; remembering only directories keeps the
            // set small while still propagating to the whole subtree.
            if self.excluded_frns.contains(&item.frn) {
                continue;
            }
            if self.excluded_frns.contains(&item.parent_frn) {
                if item.is_dir || item.flags & cairn_core::FLAG_DIRECTORY != 0 {
                    self.excluded_frns.insert(item.frn);
                }
                continue;
            }

            let (name_offset, name_len, ext_offset) = self.arena.insert(&item.name);
            let idx = self.records.len() as u32;

            self.records.push(FileRecord {
                frn: item.frn,
                parent_idx: FileRecord::ROOT_PARENT,
                name_offset,
                name_len,
                ext_offset,
                volume_id: FileRecord::volume_id_from_drive(self.drive_letter)
                    .expect("volume drive letter is an ASCII letter"),
                flags: item.flags,
                size: item.size,
                modified_ts: item.modified_ts,
                created_ts: item.created_ts,
            });
            self.live_count = self.live_count.saturating_add(1);

            self.frn_to_idx.entry(item.frn).or_default().push(idx);
            parent_frns.push(item.parent_frn);
        }

        // Link after the whole batch is inserted, so an MFT scan whose children precede their
        // parents still resolves. A parent from an earlier batch is found in the same map.
        for (offset, &parent_frn) in parent_frns.iter().enumerate() {
            if let Some(parent_idx) = self
                .frn_to_idx
                .get(&parent_frn)
                .and_then(|indices| indices.first().copied())
            {
                self.records[first_new + offset].parent_idx = parent_idx;
            }
        }

        // Folder totals stay current between batches instead of waiting for finalization: each new
        // file's own bytes walk up its parent chain once, so a directory already reports the sum of
        // every child indexed so far while the scan is still running. Directories start at zero and
        // collect their children the same way. The finalize pass still aggregates the whole table,
        // which keeps the persisted totals authoritative even if a batch was skipped or duplicated.
        if self.directory_sizes_valid {
            for offset in 0..parent_frns.len() {
                let idx = first_new + offset;
                let record = &self.records[idx];
                if record.is_file() {
                    let own_size = record.size as i128;
                    self.adjust_record_ancestors(idx as u32, own_size);
                }
            }
        }

        self.presorted_dirty = true;
    }

    /// Aggregates the indexed descendants of every directory into `FileRecord.size`. The record's
    /// existing `size` field already costs 8 bytes per entry, so folder sizes need no second
    /// resident structure; files keep their own size and tombstones are ignored.
    pub(crate) fn recompute_directory_sizes(&mut self) {
        DirectorySizeIndex::apply_directory_sizes(&mut self.records, &self.arena);
        self.directory_sizes_valid = true;
        self.presorted_dirty = true;
    }

    /// Flags the totals as stale without paying for a rebuild per event. Callers that can change a
    /// record's kind in a batch mark here and run [`Self::ensure_directory_sizes`] once at the end.
    pub(crate) fn mark_directory_sizes_dirty(&mut self) {
        self.directory_sizes_valid = false;
        self.presorted_dirty = true;
    }

    /// Rebuilds the folder totals only when an event invalidated them.
    pub(crate) fn ensure_directory_sizes(&mut self) {
        if !self.directory_sizes_valid {
            self.recompute_directory_sizes();
        }
    }

    /// Moves one size delta up the parent chain. Tombstoned ancestors stop the walk: the deleted
    /// subtree's total was already removed when that ancestor was deleted.
    fn adjust_parent_sizes(&mut self, mut parent_idx: u32, delta: i128) {
        let mut steps = 0usize;
        while parent_idx != FileRecord::ROOT_PARENT && (parent_idx as usize) < self.records.len() {
            let record = &mut self.records[parent_idx as usize];
            if record.is_tombstone() {
                break;
            }
            record.size = apply_size_delta(record.size, delta);
            if record.parent_idx == parent_idx {
                break;
            }
            parent_idx = record.parent_idx;
            steps += 1;
            if steps > self.records.len() {
                break;
            }
        }
    }

    fn has_tombstone_ancestor(&self, mut parent_idx: u32) -> bool {
        let mut steps = 0usize;
        while parent_idx != FileRecord::ROOT_PARENT && (parent_idx as usize) < self.records.len() {
            let record = &self.records[parent_idx as usize];
            if record.is_tombstone() {
                return true;
            }
            if record.parent_idx == parent_idx {
                return false;
            }
            parent_idx = record.parent_idx;
            steps += 1;
            if steps > self.records.len() {
                return true;
            }
        }
        false
    }

    fn adjust_record_ancestors(&mut self, index: u32, delta: i128) {
        if delta == 0 {
            return;
        }
        let Some(record) = self.records.get(index as usize) else {
            return;
        };
        if record.is_tombstone() {
            return;
        }
        let parent_idx = record.parent_idx;
        if !self.has_tombstone_ancestor(parent_idx) {
            self.adjust_parent_sizes(parent_idx, delta);
        }
    }

    /// Replaces a live file's size and moves the difference through its ancestors. Directory
    /// records are skipped: their size is the aggregate maintained here, not a filesystem value.
    pub(crate) fn update_file_size(&mut self, index: u32, new_size: u64) {
        let Some(record) = self.records.get(index as usize) else {
            return;
        };
        if record.is_tombstone() || record.is_dir() || record.size == new_size {
            return;
        }
        let delta = new_size as i128 - record.size as i128;
        self.records[index as usize].size = new_size;
        self.adjust_record_ancestors(index, delta);
        self.presorted_dirty = true;
    }

    /// `true` when a live record for this `(frn, parent, name)` triple is already indexed.
    fn has_live_record_at(&self, frn: u64, parent_frn: u64, name: &str) -> bool {
        self.frn_to_idx.get(&frn).is_some_and(|indices| {
            indices.iter().copied().any(|idx| {
                let rec = &self.records[idx as usize];
                if rec.is_tombstone() || self.arena.get_name(rec.name_offset, rec.name_len) != name
                {
                    return false;
                }
                match rec.parent_idx {
                    // The snapshot could not resolve this parent, so the name is all there is.
                    FileRecord::ROOT_PARENT => true,
                    parent_idx if (parent_idx as usize) < self.records.len() => {
                        self.records[parent_idx as usize].frn == parent_frn
                    }
                    _ => false,
                }
            })
        })
    }

    /// Applies one USN change, rebuilding folder sizes immediately. Test entry points use this;
    /// production paths apply batches so `k` kind changes cost one rebuild, not `k`.
    #[cfg(test)]
    pub fn apply_usn_change(&mut self, change: UsnChange) {
        self.apply_usn_change_inner(change);
        self.ensure_directory_sizes();
    }

    /// Applies a journal batch, aggregating folder sizes at most once for the whole batch. Live
    /// polling and cold-start replay use this so `k` kind changes cost one rebuild, not `k`.
    pub fn apply_usn_changes(&mut self, changes: impl IntoIterator<Item = UsnChange>) {
        for change in changes {
            self.apply_usn_change_inner(change);
        }
        self.ensure_directory_sizes();
    }

    fn apply_usn_change_inner(&mut self, change: UsnChange) {
        self.presorted_dirty = true;
        match change {
            UsnChange::Deleted { frn } => {
                self.pending_renames.remove(&frn);
                // The root disappeared: any later create under a recycled FRN has to be judged on
                // its own. A file that was never indexed has no row and no content document.
                if self.excluded_frns.remove(&frn) {
                    return;
                }
                if !self.frn_to_idx.contains_key(&frn) {
                    return;
                }
                self.pending_content_deletions
                    .insert((self.content_volume_id(), frn));
                if let Some(indices) = self.frn_to_idx.get(&frn).cloned() {
                    for idx in indices {
                        let parent_idx = self.records[idx as usize].parent_idx;
                        let size = self.records[idx as usize].size as i128;
                        // A descendant of an already deleted directory has already left the
                        // ancestor totals with that directory's recursive size.
                        if !self.records[idx as usize].is_tombstone()
                            && !self.has_tombstone_ancestor(parent_idx)
                        {
                            self.adjust_parent_sizes(parent_idx, -size);
                        }
                        if !self.records[idx as usize].is_tombstone() {
                            self.live_count = self.live_count.saturating_sub(1);
                        }
                        self.records[idx as usize].mark_tombstone();
                    }
                }
            }
            UsnChange::Created {
                frn,
                parent_frn,
                name,
                is_dir,
                attributes,
                ts,
            } => {
                // The journal carries the attribute set that says whether this is a directory or a
                // reparse point. Losing it here would let a freshly created symlink look like an
                // ordinary file and reach the content worker.
                let mut flags = file_record_flags(attributes);
                if is_dir {
                    flags |= cairn_core::FLAG_DIRECTORY;
                }

                if self.excluded_frns.contains(&frn) {
                    return;
                }
                if self.excluded_frns.contains(&parent_frn) {
                    if flags & cairn_core::FLAG_DIRECTORY != 0 {
                        self.excluded_frns.insert(frn);
                    }
                    return;
                }

                // A full `$MFT` scan and the replay that follows it overlap on `[T0, T1)`, so a
                // create can describe a file the snapshot already captured. Re-inserting it would
                // yield the same path twice; a different name or parent is a real hard link and
                // must still become its own record (IDX-06).
                if self.has_live_record_at(frn, parent_frn, &name) {
                    return;
                }

                let (name_offset, name_len, ext_offset) = self.arena.insert(&name);
                let parent_idx = self
                    .frn_to_idx
                    .get(&parent_frn)
                    .and_then(|v| v.first().copied())
                    .unwrap_or(FileRecord::ROOT_PARENT);

                let new_idx = self.records.len() as u32;
                self.records.push(FileRecord {
                    frn,
                    parent_idx,
                    name_offset,
                    name_len,
                    ext_offset,
                    volume_id: FileRecord::volume_id_from_drive(self.drive_letter)
                        .expect("volume drive letter is an ASCII letter"),
                    flags,
                    size: 0,
                    modified_ts: ts,
                    created_ts: ts,
                });
                self.live_count = self.live_count.saturating_add(1);
                self.frn_to_idx.entry(frn).or_default().push(new_idx);
                // The journal record carries no size, and its timestamp is the event time rather
                // than the file's. Read the file once on the next debounce pass, or every file
                // created during a catch-up would be displayed as 0 B.
                if !is_dir {
                    self.pending_data_changes.insert(frn);
                }
            }
            UsnChange::Renamed {
                frn,
                parent_frn,
                name,
                is_dir,
                is_new_name,
            } => {
                if self.excluded_frns.contains(&frn) {
                    self.pending_renames.remove(&frn);
                    return;
                }
                if self.excluded_frns.contains(&parent_frn) {
                    // The target lies in service data, so this path must not become a row. Only a
                    // new-name record describes the destination; the old-name half of a rename out
                    // of the subtree must not keep a directory excluded forever.
                    self.pending_renames.remove(&frn);
                    if is_new_name && is_dir {
                        self.excluded_frns.insert(frn);
                    }
                    return;
                }
                if !is_new_name {
                    self.pending_renames.insert(frn, (parent_frn, name));
                } else {
                    let old_info = self.pending_renames.remove(&frn);
                    if let Some(indices) = self.frn_to_idx.get(&frn).cloned() {
                        let target_idx = if let Some((_old_parent, old_name)) = old_info {
                            indices
                                .iter()
                                .copied()
                                .find(|&idx| {
                                    let r = &self.records[idx as usize];
                                    !r.is_tombstone()
                                        && self.arena.get_name(r.name_offset, r.name_len)
                                            == old_name
                                })
                                .or_else(|| indices.first().copied())
                        } else {
                            indices.first().copied()
                        };

                        if let Some(idx) = target_idx {
                            let (name_offset, name_len, ext_offset) = self.arena.insert(&name);
                            let parent_idx = self
                                .frn_to_idx
                                .get(&parent_frn)
                                .and_then(|v| v.first().copied())
                                .unwrap_or(FileRecord::ROOT_PARENT);

                            let (old_parent, moved_size) = {
                                let rec = &self.records[idx as usize];
                                (rec.parent_idx, rec.size as i128)
                            };
                            if old_parent != parent_idx
                                && !self.records[idx as usize].is_tombstone()
                            {
                                if !self.has_tombstone_ancestor(old_parent) {
                                    self.adjust_parent_sizes(old_parent, -moved_size);
                                }
                                if !self.has_tombstone_ancestor(parent_idx) {
                                    self.adjust_parent_sizes(parent_idx, moved_size);
                                }
                            }

                            let rec = &mut self.records[idx as usize];
                            rec.name_offset = name_offset;
                            rec.name_len = name_len;
                            rec.ext_offset = ext_offset;
                            rec.parent_idx = parent_idx;

                            self.path_cache.clear();
                            // The content document stores the path; a rename has to replace it,
                            // and a rename out of the extension whitelist has to delete it.
                            self.pending_data_changes.insert(frn);
                        }
                    }
                }
            }
            UsnChange::BasicInfoChanged {
                frn,
                ts,
                attributes,
            } => {
                if self.excluded_frns.contains(&frn) {
                    return;
                }
                let mapped = file_record_flags(attributes);
                let mut kind_changed = false;
                if let Some(indices) = self.frn_to_idx.get(&frn) {
                    for &idx in indices {
                        let record = &mut self.records[idx as usize];
                        record.modified_ts = ts;
                        let was_dir = record.is_dir();
                        // The event owns only the directory/reparse bits; tombstone and the other
                        // flags stay as they are.
                        record.flags = (record.flags
                            & !(cairn_core::FLAG_DIRECTORY | cairn_core::FLAG_REPARSE_POINT))
                            | mapped;
                        if was_dir != record.is_dir() {
                            // The size now means the other thing: a file's stream size or a
                            // directory's aggregate. A fresh aggregation restores the tree.
                            record.size = 0;
                            kind_changed = true;
                        }
                    }
                }
                if kind_changed {
                    // One aggregation per applied batch, not one per event; the batch caller runs
                    // `ensure_directory_sizes` after the last change.
                    self.mark_directory_sizes_dirty();
                }
                // A file that became a reparse point (or a directory) is no longer eligible
                // content. Re-running the record through the debounce pass removes the document
                // whose path can no longer be opened without following the link.
                self.pending_data_changes.insert(frn);
            }
            UsnChange::DataChanged { frn } => {
                if self.excluded_frns.contains(&frn) {
                    return;
                }
                self.pending_data_changes.insert(frn);
            }
        }
    }

    /// Classifies every record by whether it belongs to an excluded subtree: its own FRN is an
    /// excluded root, or one of its ancestors is. The walk follows parent indices and marks
    /// visited nodes, so a parent chain is resolved once per record instead of once per
    /// descendant. Tombstones are not part of this verdict; a cycle (or a self-parent) is kept and
    /// the walk stops there, because corrupt input must not hang startup.
    fn subtree_states_by(&self, is_excluded_root: impl Fn(usize, &FileRecord) -> bool) -> Vec<u8> {
        let count = self.records.len();
        let mut states = vec![SUBTREE_UNKNOWN; count];
        let mut stack: Vec<u32> = Vec::new();

        for start in 0..count {
            if states[start] != SUBTREE_UNKNOWN {
                continue;
            }
            stack.clear();
            let mut current = start as u32;
            let verdict = loop {
                let idx = current as usize;
                match states[idx] {
                    SUBTREE_IN_PROGRESS => break SUBTREE_KEPT,
                    SUBTREE_KEPT => break SUBTREE_KEPT,
                    SUBTREE_EXCLUDED => break SUBTREE_EXCLUDED,
                    SUBTREE_UNKNOWN => {}
                    _ => unreachable!("record compaction state"),
                }

                let record = &self.records[idx];
                if is_excluded_root(idx, record) {
                    states[idx] = SUBTREE_EXCLUDED;
                    break SUBTREE_EXCLUDED;
                }

                states[idx] = SUBTREE_IN_PROGRESS;
                stack.push(current);
                match record.parent_idx {
                    parent
                        if parent != FileRecord::ROOT_PARENT
                            && (parent as usize) < count
                            && parent as usize != idx =>
                    {
                        current = parent;
                    }
                    _ => break SUBTREE_KEPT,
                }
            };

            for &pending in &stack {
                if states[pending as usize] == SUBTREE_IN_PROGRESS {
                    states[pending as usize] = verdict;
                }
            }
        }

        states
    }

    fn excluded_subtree_states(&self) -> Vec<u8> {
        self.subtree_states_by(|_, record| self.excluded_frns.contains(&record.frn))
    }

    /// Precomputes which records the query's `excluded_paths` rules remove.
    ///
    /// Rebuilding and normalizing a full path for every match just to test a handful of segment
    /// rules is O(records × path). For the common rule form — a bare directory/file segment such
    /// as `$Recycle.Bin` or `System Volume Information` — the same verdict is a name comparison
    /// along the parent chain, so one memoized pass produces an O(1) lookup table. Rules with globs, embedded separators,
    /// or a drive-qualified path keep the reference full-path behavior: this returns `None` and
    /// the caller applies `is_path_excluded` per match.
    pub(crate) fn excluded_record_mask(&self, rules: &[String]) -> Option<Vec<bool>> {
        let segments = crate::exclusion::ExclusionRules::parse(rules).single_segments()?;
        let states = self.subtree_states_by(|_, record| {
            let name = self.arena.get_name(record.name_offset, record.name_len);
            segments
                .iter()
                .any(|segment| name.eq_ignore_ascii_case(segment))
        });
        Some(
            states
                .into_iter()
                .map(|state| state == SUBTREE_EXCLUDED)
                .collect(),
        )
    }

    /// Rewrites the table without tombstones or excluded subtrees, returning how many records were
    /// dropped. Names are re-packed and every parent reference is remapped; a record whose parent
    /// was dropped becomes a root instead of pointing at a slot that no longer exists.
    // Production goes through [`Self::compact_if_needed`] so the classification that decides
    // whether to compact is not thrown away; this direct entry point is for tests.
    #[cfg(test)]
    pub(crate) fn compact(&mut self) -> usize {
        let states = self.excluded_subtree_states();
        self.compact_with_states(states)
    }

    fn compact_with_states(&mut self, excluded: Vec<u8>) -> usize {
        let old_len = self.records.len();
        let mut remap = vec![u32::MAX; old_len];
        let mut kept = 0usize;
        for (idx, &state) in excluded.iter().enumerate() {
            if state != SUBTREE_EXCLUDED && !self.records[idx].is_tombstone() {
                remap[idx] = kept as u32;
                kept += 1;
            }
        }

        let dropped = old_len - kept;
        if dropped == 0 {
            return 0;
        }

        let mut records = Vec::with_capacity(kept);
        let mut arena = NamePool::default();
        for (old_idx, record) in self.records.iter().enumerate() {
            if remap[old_idx] == u32::MAX {
                continue;
            }
            let mut record = *record;
            let name = self.arena.get_name(record.name_offset, record.name_len);
            let (name_offset, name_len, ext_offset) = arena.insert(name);
            record.name_offset = name_offset;
            record.name_len = name_len;
            record.ext_offset = ext_offset;
            record.parent_idx = match record.parent_idx {
                FileRecord::ROOT_PARENT => FileRecord::ROOT_PARENT,
                parent if (parent as usize) < old_len && remap[parent as usize] != u32::MAX => {
                    remap[parent as usize]
                }
                _ => FileRecord::ROOT_PARENT,
            };
            records.push(record);
        }

        let mut frn_to_idx: HashMap<u64, Vec<u32>> = HashMap::with_capacity(records.len());
        for (idx, record) in records.iter().enumerate() {
            frn_to_idx.entry(record.frn).or_default().push(idx as u32);
        }

        self.records = records;
        self.live_count = self.records.len() as u64;
        self.arena = arena;
        self.frn_to_idx = frn_to_idx;
        self.path_cache.clear();
        // The pending sets are keyed by FRN (or volume id + FRN), not by record index, so
        // compaction cannot invalidate them. They usually hold the journal backlog replayed just
        // before startup compaction; clearing them here would drop metadata refreshes, content
        // rebuilds and deletion commits for everything that changed while the service was down.
        // Entries whose records were dropped are skipped safely by the later drain paths.
        self.generation = self.generation.wrapping_add(1);
        // Dropping tombstones and excluded subtrees can leave ancestor totals from an older
        // session; one linear aggregation restores the invariant before sorting is rebuilt.
        self.recompute_directory_sizes();
        self.presorted = PresortedIndices::build(
            &self.records,
            &self.arena,
            &self.path_cache,
            self.drive_letter,
        );
        self.presorted_dirty = false;
        dropped
    }

    /// Compacts large volumes once enough of the table is dead weight. A small volume, or one whose
    /// tombstones are still a small fraction, is left untouched so a normal start pays nothing.
    pub(crate) fn compact_if_needed(&mut self) -> bool {
        let len = self.records.len();
        if len < COMPACT_MIN_RECORDS {
            return false;
        }
        let states = self.excluded_subtree_states();
        let droppable = states
            .iter()
            .enumerate()
            .filter(|&(idx, &state)| state == SUBTREE_EXCLUDED || self.records[idx].is_tombstone())
            .count();
        if droppable * 100 < len * COMPACT_MIN_DROPPABLE_PERCENT {
            return false;
        }
        self.compact_with_states(states);
        true
    }

    pub fn to_summary(&self) -> VolumeStatusSummary {
        VolumeStatusSummary {
            drive_letter: self.drive_letter,
            volume_guid: self.volume_guid.clone(),
            status: self.status.state(),
            indexed_count: self.live_count,
            usn_enabled: self.usn_enabled,
        }
    }

    /// Sums the own sizes of matched files.
    ///
    /// Directories are containers: they contribute zero here, so the total always describes the
    /// visible result set and never counts an excluded descendant through an ancestor's aggregate.
    pub(crate) fn calculate_matched_bytes(&self, matched_indices: &[u32]) -> u64 {
        if matched_indices.is_empty() || self.records.is_empty() {
            return 0;
        }

        if matched_indices.len() <= 64 {
            let mut total: u64 = 0;
            for (i, &idx) in matched_indices.iter().enumerate() {
                let index = idx as usize;
                if matched_indices[..i].contains(&idx) {
                    continue;
                }
                if let Some(record) = self.records.get(index) {
                    if record.is_file() && !record.is_tombstone() {
                        total = total.saturating_add(record.size);
                    }
                }
            }
            return total;
        }

        let max_idx = matched_indices.iter().copied().max().unwrap_or(0) as usize;
        let bit_capacity = (max_idx + 1).min(self.records.len());
        let mut seen = BitSet::with_capacity(bit_capacity);
        let mut total: u64 = 0;

        for &idx in matched_indices {
            let index = idx as usize;
            if index < self.records.len() && seen.insert(index) {
                let record = &self.records[index];
                if record.is_file() && !record.is_tombstone() {
                    total = total.saturating_add(record.size);
                }
            }
        }

        total
    }
}
