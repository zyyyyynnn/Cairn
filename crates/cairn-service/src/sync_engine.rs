// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Multi-volume synchronization engine and index lifecycle manager.

use cairn_content::ContentSearcher;
use cairn_core::arena::NamePool;
use cairn_core::engine::CancellationToken;
use cairn_core::model::FileRecord;
use cairn_core::path_cache::DirectoryPathCache;
use cairn_core::sort::{PresortedIndices, SortColumn, SortDirection};
use cairn_core::syntax::{parse_query_with, QueryDefaults, QueryExpr};
use cairn_core::treemap::DirectorySizeIndex;
use cairn_protocol::message::TreeEntryData;
use rayon::prelude::*;

/// Flattens a finished tree pre-order (parent before children) for the wire.
fn flatten_tree(
    node: &cairn_core::treemap::TreemapTreeNode,
    depth: usize,
    out: &mut Vec<TreeEntryData>,
) {
    out.push(TreeEntryData {
        name: node.name.clone(),
        size: node.size,
        is_dir: node.is_dir,
        modified_ts: node.modified_ts,
        descendant_items: node.descendant_items,
        depth,
    });
    for child in &node.children {
        flatten_tree(child, depth + 1, out);
    }
}
use cairn_ntfs::mft::ExtractedItem;
use cairn_ntfs::usn::{file_record_flags, UsnChange};
use cairn_ntfs::volume::enumerate_fixed_drives;
use cairn_protocol::message::FileRowData;
use cairn_protocol::status::{
    ContentIndexState, ContentIndexStatus, VolumeState, VolumeStatusSummary,
};
use rayon::slice::ParallelSliceMut;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};

/// A journal backlog beyond this size is cheaper to recover with a fresh `$MFT` scan than by
/// replaying hundreds of thousands of USN records. It also bounds how long a cold start can show
/// the loading skeleton.
const MAX_USN_REPLAY_BACKLOG_BYTES: i64 = 64 * 1024 * 1024;

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

pub mod content_pipeline;
pub mod journal_reconciliation;
pub mod post_scan;
pub mod presorted;

pub use journal_reconciliation::*;
pub use post_scan::*;

pub struct VolumeIndex {
    pub drive_letter: char,
    pub volume_guid: String,
    pub status: VolumeState,
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
            status: VolumeState::Rebuilding,
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
            // An empty table has no totals to be wrong about; the invariant holds trivially.
            directory_sizes_valid: true,
        }
    }

    /// Clears all records and indexes for this volume. The excluded service data roots survive:
    /// they are configuration derived from the data layout, not index data, so a rebuild must keep
    /// filtering the same subtree.
    pub fn clear(&mut self) {
        self.records.clear();
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
        (self.drive_letter as u16 - b'A' as u16) as u64
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
        self.status = VolumeState::Rebuilding;
    }

    /// Appends extracted items without discarding what the volume already holds. The user-mode
    /// directory scan delivers batches while it walks, so the table can show rows before the walk
    /// finishes; the presorted arrays go stale and queries fall back to their full sort until
    /// [`Self::finalize_snapshot_scan`] rebuilds them.
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
                volume_id: (self.drive_letter as u16) - (b'A' as u16),
                flags: item.flags,
                size: item.size,
                modified_ts: item.modified_ts,
                created_ts: item.created_ts,
            });

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

    /// Completes a user-privilege directory scan. There is no change journal behind this data, so
    /// the volume is a snapshot (IDX-08) rather than a journal-synced index, and an empty store is
    /// a legitimate result for an account that cannot read anything.
    pub fn finalize_snapshot_scan(&mut self) {
        self.recompute_directory_sizes();
        self.presorted = PresortedIndices::build(
            &self.records,
            &self.arena,
            &self.path_cache,
            self.drive_letter,
        );
        self.presorted_dirty = false;
        self.usn_enabled = false;
        self.status = VolumeState::SnapshotMode;
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

    /// Applies real-time USN changes.
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
                    volume_id: (self.drive_letter as u16) - (b'A' as u16),
                    flags,
                    size: 0,
                    modified_ts: ts,
                    created_ts: ts,
                });
                self.frn_to_idx.entry(frn).or_default().push(new_idx);
                // The journal record carries no size, and its timestamp is the event time rather
                // than the file's. Read the file once on the next debounce pass, or every file
                // created during a catch-up would be displayed as 0 B.
                if !is_dir {
                    self.pending_data_changes.insert(frn);
                }
                if !self.presorted.by_name.is_empty() {
                    self.presorted.by_name.push(new_idx);
                }
                if !self.presorted.by_size.is_empty() {
                    self.presorted.by_size.push(new_idx);
                }
                if !self.presorted.by_date_modified.is_empty() {
                    self.presorted.by_date_modified.push(new_idx);
                }
                if !self.presorted.by_path.is_empty() {
                    self.presorted.by_path.push(new_idx);
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
    fn excluded_subtree_states(&self) -> Vec<u8> {
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
                if self.excluded_frns.contains(&record.frn) {
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

    /// Rewrites the table without tombstones or excluded subtrees, returning how many records were
    /// dropped. Names are re-packed and every parent reference is remapped; a record whose parent
    /// was dropped becomes a root instead of pointing at a slot that no longer exists.
    // Production goes through [`Self::compact_if_needed`] so the classification that decides
    // whether to compact is not thrown away; the direct entry point exists for tests.
    #[cfg_attr(not(test), allow(dead_code))]
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
            status: self.status,
            indexed_count: self.records.len() as u64,
            usn_enabled: self.usn_enabled,
        }
    }
}

pub struct MultiVolumeEngine {
    volumes: RwLock<HashMap<char, VolumeIndex>>,
    index_dir: std::path::PathBuf,
    content_dir: std::path::PathBuf,
    /// Directories written by this process (MFT cache, Tantivy index, configuration and logs).
    /// Content extraction must never read them back: a Tantivy commit rewrites `meta.json`, and
    /// indexing that file would feed the index's own writes back in as new user content.
    self_data_dirs: Vec<std::path::PathBuf>,
    config_path: std::path::PathBuf,
    config: RwLock<ServiceConfig>,
    content_searcher: RwLock<Option<Arc<ContentSearcher>>>,
    content_status: RwLock<ContentIndexStatus>,
    content_cancel: Arc<AtomicBool>,
    content_commands: Mutex<Option<std::sync::mpsc::Sender<content_pipeline::ContentCommand>>>,
    /// Service data is owned by SYSTEM, so its configuration is written with the restricted
    /// descriptor. A user-mode indexer owns its own settings and writes them as the user.
    secure_config: bool,
}

impl Default for MultiVolumeEngine {
    fn default() -> Self {
        Self::with_config(ServiceConfig::default())
    }
}

use crate::config::ServiceConfig;
use crate::persistence;
use crate::session::SearchResultItem;
use std::path::Path;

pub type MultiSearchResult = (
    Vec<SearchResultItem>,
    usize,
    Vec<String>,
    HashMap<(char, u64), String>,
);

impl MultiVolumeEngine {
    pub fn new() -> Self {
        Self::with_config(ServiceConfig::default())
    }

    pub fn with_config(config: ServiceConfig) -> Self {
        let root = crate::security::default_service_data_root();
        Self::with_paths(
            &root.join("index"),
            &root.join("content"),
            std::path::Path::new(crate::config::SERVICE_CONFIG_FILE),
            enumerate_fixed_drives(),
            config,
        )
        .with_service_data_root(&root)
    }

    /// Builds the engine around the SPEC SET-02 data layout, whose access control has already been
    /// enforced by [`crate::security::prepare_service_data_layout`].
    pub fn for_data_layout(
        layout: &crate::security::ServiceDataLayout,
        config: ServiceConfig,
    ) -> Self {
        Self::with_paths(
            &layout.index_dir,
            &layout.content_dir,
            &layout.config_path,
            enumerate_fixed_drives(),
            config,
        )
        .with_service_data_root(&layout.root)
    }

    pub fn with_index_dir_and_drives(index_dir: &Path, drives: Vec<char>) -> Self {
        Self::with_index_dir_drives_and_config(index_dir, drives, ServiceConfig::default())
    }

    /// Test and development layout: the content index lives below `index_dir`, while the service
    /// configuration keeps its real location.
    pub fn with_index_dir_drives_and_config(
        index_dir: &Path,
        drives: Vec<char>,
        config: ServiceConfig,
    ) -> Self {
        Self::with_paths(
            index_dir,
            &index_dir.join("content"),
            std::path::Path::new(crate::config::SERVICE_CONFIG_FILE),
            drives,
            config,
        )
    }

    pub fn with_paths(
        index_dir: &Path,
        content_dir: &Path,
        config_path: &Path,
        drives: Vec<char>,
        config: ServiceConfig,
    ) -> Self {
        let mut self_data_dirs = Vec::new();
        Self::push_self_data_dir(&mut self_data_dirs, content_dir);
        Self::push_self_data_dir(&mut self_data_dirs, index_dir);
        // Configuration and logs sit beside these two directories. Protecting their common parent
        // covers the whole service data tree without having to enumerate every child.
        if let Some(parent) = content_dir.parent() {
            Self::push_self_data_dir(&mut self_data_dirs, parent);
        }
        // Resolve the roots to file ids once, before any volume exists, so an initial $MFT scan, a
        // cached-index replay and every later USN batch all filter the same subtree by identity.
        let excluded_roots = Self::excluded_roots_by_drive(&self_data_dirs);
        let mut map = HashMap::new();

        // Filter excluded_drives during volume initialization and scanning (IDX-01)
        let active_drives: Vec<char> = drives
            .into_iter()
            .filter(|&d| {
                !config
                    .excluded_drives
                    .iter()
                    .any(|&ex| ex.eq_ignore_ascii_case(&d))
            })
            .collect();

        for d in active_drives {
            let mut vol = VolumeIndex::new(d);
            if let Some(roots) = excluded_roots.get(&d) {
                vol.set_excluded_roots(roots.iter().copied());
            }
            let cache_file = index_dir.join(format!("vol_{}.idx", d));
            let mut loaded_ok = false;
            if cache_file.exists() {
                match persistence::load_volume_index(&cache_file) {
                    Ok(loaded) => {
                        let directory_sizes_valid = loaded.directory_sizes_valid;
                        vol.records = loaded.records;
                        vol.arena = loaded.arena;
                        vol.last_usn = loaded.last_usn;
                        vol.journal_id = loaded.journal_id;
                        vol.volume_serial = loaded.volume_serial;
                        vol.directory_sizes_valid = directory_sizes_valid;

                        for (idx, r) in vol.records.iter().enumerate() {
                            if !r.is_tombstone() {
                                vol.frn_to_idx.entry(r.frn).or_default().push(idx as u32);
                            }
                        }

                        // Caches written before folder sizes were persisted still store zeroes for
                        // every directory. Aggregate once and immediately rewrite the flag, so only
                        // the first launch after the upgrade pays for it.
                        if !vol.directory_sizes_valid {
                            vol.recompute_directory_sizes();
                            match vol.save_to_dir(index_dir) {
                                Ok(_) => tracing::info!(
                                    "Migrated cached index for {}: computed directory sizes",
                                    d
                                ),
                                Err(error) => tracing::warn!(
                                    "Cached index for {} has computed directory sizes that could not be persisted: {}",
                                    d,
                                    error
                                ),
                            }
                        }
                        // The presorted size array is built from whatever the records now hold, so
                        // the first page and size sort agree with the table.
                        vol.presorted = PresortedIndices::build(
                            &vol.records,
                            &vol.arena,
                            &vol.path_cache,
                            vol.drive_letter,
                        );
                        vol.presorted_dirty = false;

                        // Validate against live volume if accessible and catch up USN (IDX-04, IDX-05, IDX-07)
                        #[cfg(windows)]
                        if enumerate_fixed_drives().contains(&d) {
                            match cairn_ntfs::RawVolumeHandle::open(d) {
                                Ok(handle) => match handle.query_usn_journal() {
                                    Ok(usn_data) => {
                                        // A saved position must lie inside the journal's current
                                        // [first, next] window. Checking only the lower bound left a
                                        // cache whose USN was ahead of the journal (a reset/replaced
                                        // journal or a stale cache) permanently CatchingUp: every
                                        // read started past the tail and the index could never catch
                                        // up. Treat any out-of-window position as a rebuild.
                                        if !Self::cached_position_is_usable(
                                            vol.journal_id,
                                            vol.last_usn,
                                            &usn_data,
                                        ) {
                                            tracing::warn!(
                                                "USN journal mismatch or out of range for {} (cached journal {}, last_usn {}; current journal {}, first {}, next {}): will rebuild",
                                                d,
                                                vol.journal_id,
                                                vol.last_usn,
                                                usn_data.journal_id,
                                                usn_data.first_usn,
                                                usn_data.next_usn
                                            );
                                            vol.clear();
                                            vol.status = VolumeState::Rebuilding;
                                            loaded_ok = false;
                                        } else if Self::backlog_is_too_large(
                                            vol.last_usn,
                                            usn_data.next_usn,
                                        ) {
                                            tracing::warn!(
                                                "USN backlog for {} is {} bytes; rebuilding instead of replaying",
                                                d,
                                                usn_data.next_usn.saturating_sub(vol.last_usn)
                                            );
                                            vol.clear();
                                            vol.status = VolumeState::Rebuilding;
                                            loaded_ok = false;
                                        } else {
                                            // Replay the change journal from the saved position up to
                                            // the journal tail (IDX-04). An index whose backlog is not
                                            // exhausted is reported as CatchingUp, never as Synced,
                                            // because stale results must be annotated (IDX-07).
                                            let mut buf = vec![0u8; 512 * 1024];
                                            match replay_usn_backlog(
                                                &handle,
                                                &mut vol,
                                                &mut buf,
                                                usn_data.journal_id,
                                                usn_data.next_usn,
                                            ) {
                                                Ok(()) => {
                                                    loaded_ok = true;
                                                    tracing::info!(
                                                        "Loaded cached index for {}: {} records, state {:?}",
                                                        d,
                                                        vol.records.len(),
                                                        vol.status
                                                    );
                                                }
                                                Err(e) => {
                                                    tracing::warn!(
                                                        "USN catchup read for {}: failed ({:?}), will rebuild",
                                                        d,
                                                        e
                                                    );
                                                    vol.clear();
                                                    vol.status = VolumeState::Rebuilding;
                                                    loaded_ok = false;
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "USN query failed for {}: ({:?}), falling back to SnapshotMode (IDX-08)",
                                            d,
                                            e
                                        );
                                        vol.status = VolumeState::SnapshotMode;
                                        vol.usn_enabled = false;
                                        loaded_ok = true;
                                    }
                                },
                                Err(e) => {
                                    // The cached index is retained but its freshness cannot be
                                    // established, so it is Offline rather than Synced (IDX-07).
                                    tracing::warn!(
                                        "Cannot open live handle for {} ({:?}), retaining cached index as Offline",
                                        d,
                                        e
                                    );
                                    vol.status = VolumeState::Offline;
                                    loaded_ok = true;
                                }
                            }
                        } else {
                            // The drive is not present anymore: the cached index can only be stale.
                            tracing::info!("Volume {} is no longer enumerated: marked Offline", d);
                            vol.status = VolumeState::Offline;
                            loaded_ok = true;
                        }
                        #[cfg(not(windows))]
                        {
                            // No change journal exists here, so the cached index is served as a
                            // snapshot whose freshness nobody has proven (IDX-07).
                            vol.status = VolumeState::SnapshotMode;
                            vol.usn_enabled = false;
                            loaded_ok = true;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Failed to load cached index for {}: {}, will rebuild",
                            d,
                            e
                        );
                        vol.clear();
                        vol.status = VolumeState::Rebuilding;
                        loaded_ok = false;
                    }
                }
            }

            if !loaded_ok && vol.status != VolumeState::Offline {
                let mut scan_succeeded = false;
                #[cfg(windows)]
                {
                    tracing::info!(
                        "No cache or invalid cache for {}: attempting live MFT scan...",
                        d
                    );
                    // T0 is captured before the scan runs, so every record produced while `$MFT` is
                    // being read is replayed afterwards instead of being skipped forever (IDX-07).
                    let scan_start = open_journal_at_scan_start(d);
                    match cairn_ntfs::scan_live_volume_mft(d, 4096) {
                        Ok(items) if !items.is_empty() => {
                            tracing::info!(
                                "Scanned {} items from live MFT for {}:",
                                items.len(),
                                d
                            );
                            vol.ingest_items(items);
                            scan_succeeded =
                                finalize_scanned_volume(&mut vol, index_dir, scan_start).is_ok();
                        }
                        Ok(_) => {
                            tracing::debug!("Live MFT scan for {}: returned 0 items", d);
                        }
                        Err(err) => {
                            tracing::debug!("Live MFT scan for {}: not available ({})", d, err);
                        }
                    }
                }

                // Without a usable scan the volume holds no records at all, so it must not be
                // reported as fresh data (IDX-05, IDX-07).
                if !scan_succeeded {
                    vol.clear();
                    vol.status = VolumeState::Rebuilding;
                }
            }

            // Caches written before the service data root was excluded still hold its files and an
            // unbounded tombstone history. Reclaim both before the volume serves queries.
            let before_compact = vol.records.len();
            if vol.compact_if_needed() {
                tracing::info!(
                    "Volume {}: compacted index from {} to {} records ({} dropped)",
                    d,
                    before_compact,
                    vol.records.len(),
                    before_compact.saturating_sub(vol.records.len())
                );
            }

            map.insert(d, vol);
        }

        let content_enabled = config.content_index_enabled;
        Self {
            volumes: RwLock::new(map),
            index_dir: index_dir.to_path_buf(),
            content_dir: content_dir.to_path_buf(),
            self_data_dirs,
            config_path: config_path.to_path_buf(),
            config: RwLock::new(config),
            content_searcher: RwLock::new(None),
            content_status: RwLock::new(if content_enabled {
                ContentIndexStatus {
                    state: ContentIndexState::Backfilling {
                        processed: 0,
                        total: 0,
                    },
                    ..Default::default()
                }
            } else {
                ContentIndexStatus::default()
            }),
            content_cancel: Arc::new(AtomicBool::new(false)),
            content_commands: Mutex::new(None),
            secure_config: true,
        }
    }

    /// Adds the data root that holds this indexer's content index, MFT cache, configuration and
    /// logs. The root itself is protected, not just its known children, so `service.toml` and the
    /// log directory are covered without enumerating them here. A bare volume root is ignored:
    /// protecting it would disable content indexing for the whole disk.
    pub(crate) fn with_service_data_root(mut self, root: &Path) -> Self {
        if Self::push_self_data_dir(&mut self.self_data_dirs, root) {
            // Volumes already loaded (the service layout) need the root for the USN events that
            // follow; a fresh scan is not repeated here.
            self.add_self_data_exclusion(root);
        }
        self
    }

    fn push_self_data_dir(dirs: &mut Vec<std::path::PathBuf>, dir: &Path) -> bool {
        let normalized = dir.to_string_lossy().replace('/', "\\");
        let trimmed = normalized.trim_end_matches('\\');
        if trimmed.is_empty() || (trimmed.len() == 2 && trimmed.ends_with(':')) {
            return false;
        }
        if dirs.iter().any(|existing| existing == dir) {
            return false;
        }
        dirs.push(dir.to_path_buf());
        true
    }

    /// Resolves every service data directory to the FRN of the volume that owns it. A path that no
    /// longer exists (an old layout, an unmounted drive) is skipped: it cannot be indexed either.
    fn excluded_roots_by_drive(dirs: &[std::path::PathBuf]) -> HashMap<char, HashSet<u64>> {
        let mut roots: HashMap<char, HashSet<u64>> = HashMap::new();
        for dir in dirs {
            let Some(drive) = Self::drive_letter_of_path(dir) else {
                continue;
            };
            match cairn_ntfs::directory_file_id(dir) {
                Some(frn) => {
                    roots.entry(drive).or_default().insert(frn);
                }
                None => tracing::debug!(
                    "Cannot resolve the file id of service data directory {}; it stays indexed",
                    dir.display()
                ),
            }
        }
        roots
    }

    /// Seeds an already loaded volume with one excluded root. Used when the data root is attached
    /// after the volumes were built, so subsequent USN events are filtered immediately.
    fn add_self_data_exclusion(&self, dir: &Path) {
        let Some(drive) = Self::drive_letter_of_path(dir) else {
            return;
        };
        let Some(frn) = cairn_ntfs::directory_file_id(dir) else {
            tracing::debug!(
                "Cannot resolve the file id of service data directory {}; it stays indexed",
                dir.display()
            );
            return;
        };
        let mut volumes = self.volumes.write().unwrap();
        if let Some(volume) = volumes.get_mut(&drive) {
            volume.set_excluded_roots(std::iter::once(frn));
        }
    }

    fn drive_letter_of_path(path: &Path) -> Option<char> {
        #[cfg(windows)]
        {
            use std::path::{Component, Prefix};
            match path.components().next()? {
                Component::Prefix(prefix) => match prefix.kind() {
                    Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                        Some((letter as char).to_ascii_uppercase())
                    }
                    _ => None,
                },
                _ => None,
            }
        }
        #[cfg(not(windows))]
        {
            let _ = path;
            None
        }
    }

    /// Persists configuration with the caller's own rights instead of the SYSTEM/Administrators
    /// descriptor. Only a user-mode indexer may use this: its settings belong to that user.
    pub fn with_plain_config_persistence(mut self) -> Self {
        self.secure_config = false;
        self
    }

    /// Returns the current service configuration.
    pub fn get_config(&self) -> ServiceConfig {
        self.config.read().unwrap().clone()
    }

    /// Reads the directory tree below `path` from the owning volume's index,
    /// flattened pre-order for the wire. `None` when no loaded volume covers
    /// the drive in `path` (the caller then answers with empty entries).
    ///
    /// The size index is built per request: it is an O(records) pass the
    /// Treemap only asks for when it opens a drive, and keeping it resident
    /// for one view would tax every query in between.
    pub fn treemap_tree(&self, path: &str, max_depth: usize) -> Option<Vec<TreeEntryData>> {
        let drive = path.chars().next()?.to_ascii_uppercase();
        let volumes = self.volumes.read().unwrap();
        let volume = volumes.get(&drive)?;
        let dir_index = DirectorySizeIndex::build(&volume.records, &volume.arena);

        let is_root = path
            .trim_end_matches('\\')
            .eq_ignore_ascii_case(&format!("{drive}:"));
        let (dir_idx, root_name) = if is_root {
            (None, format!("{drive}:\\"))
        } else {
            let idx = dir_index
                .resolve_path_to_idx(&volume.records, &volume.arena, path)
                .flatten()?;
            let record = &volume.records[idx as usize];
            (
                Some(idx),
                volume
                    .arena
                    .get_name(record.name_offset, record.name_len)
                    .to_string(),
            )
        };

        let tree = dir_index.build_tree_node(
            &volume.records,
            &volume.arena,
            dir_idx,
            &root_name,
            max_depth,
        );
        let mut entries = Vec::new();
        flatten_tree(&tree, 0, &mut entries);
        Some(entries)
    }

    /// `true` when the service must answer queries from administrators only (SPEC SVC-02, SVC-05).
    /// Read per request, so a configuration change takes effect without a new pipe instance.
    pub fn admin_only_query(&self) -> bool {
        self.config.read().unwrap().admin_only_query
    }

    /// Updates service configuration dynamically (IDX-09 hot apply).
    pub fn update_config(&self, new_config: ServiceConfig) {
        let previous_enabled = self.config.read().unwrap().content_index_enabled;
        let next_enabled = new_config.content_index_enabled;
        {
            let mut cfg = self.config.write().unwrap();
            *cfg = new_config.clone();
        }

        let sender = self.content_commands.lock().unwrap().clone();
        if previous_enabled != next_enabled {
            if next_enabled {
                self.content_cancel
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                self.set_content_progress(0, 0);
                if let Some(tx) = sender.clone() {
                    let _ = tx.send(content_pipeline::ContentCommand::Start);
                }
            } else {
                self.content_cancel
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                self.set_content_state(ContentIndexState::Disabled);
                if let Some(tx) = sender {
                    let _ = tx.send(content_pipeline::ContentCommand::Cancel);
                }
            }
        } else if next_enabled
            && matches!(
                self.content_index_status().state,
                ContentIndexState::Error(_)
            )
        {
            // Saving the same enabled value is the explicit retry path: the previous run failed,
            // and the user asked again. A full backfill clears the partial index first.
            self.content_cancel
                .store(false, std::sync::atomic::Ordering::SeqCst);
            self.set_content_progress(0, 0);
            if let Some(tx) = sender {
                let _ = tx.send(content_pipeline::ContentCommand::Start);
            }
        }

        // Remove any volumes that are now excluded so they don't consume memory (IDX-01)
        let mut lock = self.volumes.write().unwrap();
        lock.retain(|&d, _| {
            !new_config
                .excluded_drives
                .iter()
                .any(|&ex| ex.eq_ignore_ascii_case(&d))
        });
    }

    /// Reloads configuration from the service configuration file and applies it (SVC-05, IDX-09).
    pub fn reload_config(&self) -> Result<(), String> {
        let path = self.config_path.clone();
        self.reload_config_from(&path)
    }

    /// Reloads configuration from an explicit path (IDX-09). A missing file resets the engine to the
    /// shipped defaults instead of keeping a configuration nobody can account for.
    pub fn reload_config_from(&self, path: &std::path::Path) -> Result<(), String> {
        if path.exists() {
            let new_cfg = ServiceConfig::load(path)
                .map_err(|e| format!("Failed to load config from {}: {}", path.display(), e))?;
            self.update_config(new_cfg);
        } else {
            self.update_config(ServiceConfig::default());
        }
        Ok(())
    }

    /// Applies a configuration submitted by a privileged caller: validate, persist with the
    /// SYSTEM/Administrators descriptor required by SET-02, then hot-apply (SVC-05, IDX-09).
    ///
    /// Nothing is applied unless the file could be written securely, so a configuration that a
    /// standard account could still edit never reaches the running index.
    pub fn apply_config_toml(&self, config_toml: &str) -> Result<(), String> {
        let parsed: ServiceConfig = toml::from_str(config_toml)
            .map_err(|e| format!("invalid service configuration: {e}"))?;
        let path = self.config_path.clone();
        if self.secure_config {
            parsed
                .save_atomic_secure(&path)
                .map_err(|e| format!("could not persist {}: {e}", path.display()))?;
        } else {
            parsed
                .save_atomic(&path)
                .map_err(|e| format!("could not persist {}: {e}", path.display()))?;
        }
        self.update_config(parsed);
        Ok(())
    }

    /// Rescans every volume running in SnapshotMode (IDX-08) and reports how many were rebuilt.
    pub fn rescan_snapshot_volumes(&self) -> usize {
        let snapshot_drives: Vec<char> = {
            let vols = self.volumes.read().unwrap();
            let cfg = self.config.read().unwrap();
            vols.values()
                .filter(|v| v.status == VolumeState::SnapshotMode)
                .map(|v| v.drive_letter)
                .filter(|d| {
                    !cfg.excluded_drives
                        .iter()
                        .any(|ex| ex.eq_ignore_ascii_case(d))
                })
                .collect()
        };

        snapshot_drives
            .into_iter()
            .filter(|drive| self.rescan_volume(*drive).is_ok())
            .count()
    }

    /// Rebuilds one volume from a full `$MFT` scan (IDX-08). Errors whenever the volume is not
    /// tracked, the scan yields nothing, or its result cannot be reconciled with the change journal,
    /// so a rescan is never reported as a silent success.
    pub fn rescan_volume(&self, drive: char) -> Result<usize, String> {
        #[cfg(windows)]
        {
            if !self.volumes.read().unwrap().contains_key(&drive) {
                return Err(format!("volume {drive}: is not indexed"));
            }

            // T0 must be read before the scan and the scan must stay outside the volume lock, so the
            // journal is bracketed here and only reconciled once the records are in hand (IDX-07).
            let scan_start = open_journal_at_scan_start(drive);
            match cairn_ntfs::scan_live_volume_mft(drive, 4096) {
                Ok(items) if !items.is_empty() => {
                    let count = items.len();
                    let mut lock = self.volumes.write().unwrap();
                    let vol = match lock.get_mut(&drive) {
                        Some(vol) => vol,
                        None => return Err(format!("volume {drive}: is not indexed")),
                    };
                    vol.ingest_items(items);
                    finalize_scanned_volume(&mut *vol, &self.index_dir, scan_start)?;
                    tracing::info!(
                        "Volume {drive}: rescanned {} records, state {:?}",
                        vol.records.len(),
                        vol.status
                    );
                    Ok(count)
                }
                Ok(_) => Err(format!("live MFT scan for {drive}: returned no records")),
                Err(e) => Err(format!("live MFT scan for {drive}: failed: {e}")),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = drive;
            Err("MFT scanning is only available on Windows".to_string())
        }
    }

    /// Mutates one tracked volume; `false` when it is not indexed (or is excluded by configuration).
    pub fn update_volume<F: FnOnce(&mut VolumeIndex)>(&self, drive: char, update: F) -> bool {
        let mut lock = self.volumes.write().unwrap();
        match lock.get_mut(&drive) {
            Some(volume) => {
                update(volume);
                true
            }
            None => false,
        }
    }

    /// Adds or replaces a volume in the multi-volume engine. A replacement starts one generation
    /// past the table it hides, so a query session created against the old table cannot page into
    /// records that now belong to the new one.
    pub fn add_volume(&self, vol: VolumeIndex) {
        let cfg = self.config.read().unwrap();
        if cfg
            .excluded_drives
            .iter()
            .any(|&ex| ex.eq_ignore_ascii_case(&vol.drive_letter))
        {
            return;
        }
        drop(cfg);
        let mut lock = self.volumes.write().unwrap();
        let generation = lock
            .get(&vol.drive_letter)
            .map(|previous| previous.generation.wrapping_add(1));
        match generation {
            Some(generation) => {
                let mut vol = vol;
                vol.generation = generation;
                lock.insert(vol.drive_letter, vol);
            }
            None => {
                lock.insert(vol.drive_letter, vol);
            }
        }
    }

    /// Atomically saves all non-empty volume indexes to disk.
    pub fn save_all_indexes(&self) -> Result<(), persistence::PersistenceError> {
        let vols = self.volumes.read().unwrap();
        for vol in vols.values() {
            if !vol.records.is_empty() {
                vol.save_to_dir(&self.index_dir)?;
                tracing::info!(
                    "Atomically saved volume index for {}: ({} records, last_usn: {})",
                    vol.drive_letter,
                    vol.records.len(),
                    vol.last_usn
                );
            }
        }
        Ok(())
    }

    /// A cached journal position is usable only when it belongs to the current journal and lies in
    /// the current `[first_usn, next_usn]` window. A position ahead of `next_usn` cannot be caught
    /// up by reading records (there are none), so it must rebuild instead of polling forever.
    fn cached_position_is_usable(
        cached_journal_id: u64,
        last_usn: i64,
        current: &cairn_ntfs::usn::UsnJournalData,
    ) -> bool {
        (cached_journal_id == 0 || cached_journal_id == current.journal_id)
            && last_usn >= current.first_usn
            && last_usn <= current.next_usn
    }

    fn backlog_is_too_large(last_usn: i64, next_usn: i64) -> bool {
        next_usn.saturating_sub(last_usn) > MAX_USN_REPLAY_BACKLOG_BYTES
    }

    /// `true` while at least one volume is replaying changes, so the poll loop can shorten its
    /// interval and drain the backlog quickly.
    pub fn is_catching_up(&self) -> bool {
        self.volumes.read().unwrap().values().any(|volume| {
            matches!(
                volume.status,
                VolumeState::CatchingUp(_) | VolumeState::Rebuilding
            )
        })
    }

    /// Polls real-time USN change journal on all supported fixed NTFS volumes.
    pub fn poll_usn_changes(&self) {
        #[cfg(windows)]
        {
            // Larger reads mean fewer 50ms rounds during a cold catch-up; 512 KiB is still a
            // transient allocation, not part of the steady-state memory budget.
            let mut buf = vec![0u8; 512 * 1024];
            let present: HashSet<char> = enumerate_fixed_drives().into_iter().collect();
            let drives: Vec<char> = {
                let lock = self.volumes.read().unwrap();
                let cfg = self.config.read().unwrap();
                lock.keys()
                    .copied()
                    .filter(|&d| {
                        !cfg.excluded_drives
                            .iter()
                            .any(|&ex| ex.eq_ignore_ascii_case(&d))
                    })
                    .collect()
            };

            for drive in drives {
                if !present.contains(&drive) {
                    // The volume is gone (ejected, unmounted): the retained index is stale by
                    // definition, so it must not stay reported as Synced (IDX-07).
                    let mut lock = self.volumes.write().unwrap();
                    if let Some(vol) = lock.get_mut(&drive) {
                        if vol.status != VolumeState::Offline {
                            tracing::info!(
                                "Volume {} disappeared from the system: marked Offline",
                                drive
                            );
                            vol.status = VolumeState::Offline;
                        }
                    }
                    continue;
                }

                let (journal_id, last_usn, usn_enabled) = {
                    let lock = self.volumes.read().unwrap();
                    if let Some(vol) = lock.get(&drive) {
                        (vol.journal_id, vol.last_usn, vol.usn_enabled)
                    } else {
                        continue;
                    }
                };

                if !usn_enabled {
                    continue;
                }

                let Ok(handle) = cairn_ntfs::RawVolumeHandle::open(drive) else {
                    continue;
                };

                let (active_jid, query_usn) = if journal_id == 0 {
                    match handle.query_usn_journal() {
                        Ok(data) => (data.journal_id, data.next_usn),
                        Err(_) => continue,
                    }
                } else {
                    (journal_id, last_usn)
                };

                match handle.read_usn_journal(active_jid, query_usn, &mut buf) {
                    Ok((next_usn, changes)) => {
                        // Query the tail for *every* batch, including an empty one. An empty read
                        // means no records were available from `query_usn`, not that the caller is
                        // at the tail; only a real tail can prove Synced. Omitting this query on an
                        // empty batch left a fully caught-up volume stuck in CatchingUp forever.
                        let tail_usn = handle.query_usn_journal().ok().map(|data| data.next_usn);
                        let mut lock = self.volumes.write().unwrap();
                        if let Some(vol) = lock.get_mut(&drive) {
                            let previous_usn = vol.last_usn;
                            vol.journal_id = active_jid;
                            vol.last_usn = next_usn;
                            if !changes.is_empty() {
                                vol.apply_usn_changes(changes);
                                vol.path_cache.clear();
                                // Mark dirty; do NOT synchronously rebuild PresortedIndices under write lock (SPEC SM-4, QRY-05)
                                vol.presorted_dirty = true;
                            }
                            let tracked_live = matches!(
                                vol.status,
                                VolumeState::Synced | VolumeState::CatchingUp(_)
                            );
                            if tracked_live {
                                if let Some(state) =
                                    live_state_after_poll(previous_usn, next_usn, tail_usn)
                                {
                                    vol.status = state;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::trace!("USN read on drive {}: returned {:?}", drive, e);
                    }
                }
            }
        }
        self.process_pending_data_changes();
    }

    pub fn get_volume_summaries(&self) -> Vec<VolumeStatusSummary> {
        let lock = self.volumes.read().unwrap();
        let cfg = self.config.read().unwrap();
        let mut summaries: Vec<_> = lock
            .values()
            .filter(|v| {
                !cfg.excluded_drives
                    .iter()
                    .any(|&ex| ex.eq_ignore_ascii_case(&v.drive_letter))
            })
            .map(|v| v.to_summary())
            .collect();
        summaries.sort_by_key(|s| s.drive_letter);
        summaries
    }

    /// Evaluates search across all volumes, filtering excluded paths, sorting by column,
    /// returning highlight terms and content snippets. `defaults` carries the query-wide match
    /// toggles and attribute filters the client sent with the text.
    pub fn execute_search_multi(
        &self,
        raw_query: &str,
        defaults: &QueryDefaults,
        sort_by: SortColumn,
        sort_asc: bool,
        cancel: &CancellationToken,
    ) -> MultiSearchResult {
        let query_expr = parse_query_with(raw_query, defaults);
        let highlight_terms = query_expr.collect_highlight_terms();
        let content_queries = query_expr.extract_content_queries();
        let mft_query_expr = query_expr
            .strip_content_queries()
            .unwrap_or_else(|| QueryExpr::Term(cairn_core::syntax::SearchTerm::default()));

        // Only the all-files query can consume a presorted order. A filtered query scans the
        // records and sorts just its matches, so rebuilding the volume's whole name/path/size
        // array first is pure latency: measured 0.65-0.9 s per filtered query on the live index.
        if content_queries.is_empty() && query_expr.is_empty() {
            self.ensure_presorted_indices(sort_by);
        }

        let (excluded_drives, excluded_paths) = {
            let cfg = self.config.read().unwrap();
            (cfg.excluded_drives.clone(), cfg.excluded_paths.clone())
        };

        let mut all_results = Vec::new();
        let mut snippets = HashMap::new();

        let lock;

        if !content_queries.is_empty() {
            // Decoupled Tantivy inverted index search: run without holding self.volumes lock
            // to avoid blocking real-time USN writer threads.
            let content_q = content_queries.join(" ");
            let content_hits = if self.content_index_status().state.is_ready() {
                if let Some(ci) = self.get_or_create_content_searcher() {
                    ci.search(&content_q, 50_000).unwrap_or_default()
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };

            if cancel.is_cancelled() || content_hits.is_empty() {
                return (all_results, 0, highlight_terms, snippets);
            }

            // Precompile MFT filter outside the lock to keep lock duration minimal
            let compiled_mft = if !mft_query_expr.is_empty() {
                Some(cairn_core::engine::CompiledQuery::compile(&mft_query_expr))
            } else {
                None
            };

            // Acquire read lock with minimal critical section to convert and merge results
            lock = self.volumes.read().unwrap();

            for hit in &content_hits {
                if cancel.is_cancelled() {
                    break;
                }
                if hit.volume_id >= 26 {
                    continue;
                }
                let drive_letter = (b'A' + hit.volume_id as u8) as char;
                if excluded_drives
                    .iter()
                    .any(|&ex| ex.eq_ignore_ascii_case(&drive_letter))
                {
                    continue;
                }
                let Some(vol) = lock.get(&drive_letter) else {
                    continue;
                };
                if !vol.status.is_synced() && vol.records.is_empty() {
                    continue;
                }
                let Some(indices) = vol.frn_to_idx.get(&hit.frn) else {
                    continue;
                };

                for &idx in indices {
                    let rec = &vol.records[idx as usize];
                    if rec.is_tombstone() {
                        continue;
                    }
                    if let Some(ref compiled) = compiled_mft {
                        if !compiled.evaluate_record(
                            rec,
                            idx,
                            &vol.records,
                            &vol.arena,
                            &vol.path_cache,
                            vol.drive_letter,
                            &mft_query_expr,
                        ) {
                            continue;
                        }
                    }
                    if !excluded_paths.is_empty() {
                        let full_path = vol.path_cache.get_full_path(
                            &vol.records,
                            &vol.arena,
                            vol.drive_letter,
                            idx,
                        );
                        if is_path_excluded(&full_path, &excluded_paths) {
                            continue;
                        }
                    }
                    all_results.push(SearchResultItem {
                        drive_letter: vol.drive_letter,
                        record_idx: idx,
                        generation: vol.generation,
                    });
                    snippets.insert((vol.drive_letter, rec.frn), hit.snippet.clone());
                }
            }
        } else {
            lock = self.volumes.read().unwrap();
            let mut single_vol_presorted = false;
            let active_vols: Vec<_> = lock
                .values()
                .filter(|v| {
                    if excluded_drives
                        .iter()
                        .any(|&ex| ex.eq_ignore_ascii_case(&v.drive_letter))
                    {
                        return false;
                    }
                    v.status.is_synced() || !v.records.is_empty()
                })
                .collect();

            if active_vols.len() == 1 {
                let vol = active_vols[0];
                let presorted_col = presorted_order(vol, sort_by);
                single_vol_presorted = presorted_col.is_some();

                let matches = cairn_core::engine::execute_query_ordered(
                    &vol.records,
                    &vol.arena,
                    &vol.path_cache,
                    vol.drive_letter,
                    &query_expr,
                    presorted_col,
                    cancel,
                );

                all_results.reserve(matches.len());
                for idx in matches {
                    if !excluded_paths.is_empty() {
                        let full_path = vol.path_cache.get_full_path(
                            &vol.records,
                            &vol.arena,
                            vol.drive_letter,
                            idx,
                        );
                        if is_path_excluded(&full_path, &excluded_paths) {
                            continue;
                        }
                    }
                    all_results.push(SearchResultItem {
                        drive_letter: vol.drive_letter,
                        record_idx: idx,
                        generation: vol.generation,
                    });
                }

                if single_vol_presorted && !sort_asc {
                    all_results.reverse();
                }
            } else {
                for vol in active_vols {
                    if cancel.is_cancelled() {
                        break;
                    }
                    let presorted_col = presorted_order(vol, sort_by);
                    let matches = cairn_core::engine::execute_query_ordered(
                        &vol.records,
                        &vol.arena,
                        &vol.path_cache,
                        vol.drive_letter,
                        &query_expr,
                        presorted_col,
                        cancel,
                    );
                    for idx in matches {
                        if !excluded_paths.is_empty() {
                            let full_path = vol.path_cache.get_full_path(
                                &vol.records,
                                &vol.arena,
                                vol.drive_letter,
                                idx,
                            );
                            if is_path_excluded(&full_path, &excluded_paths) {
                                continue;
                            }
                        }
                        all_results.push(SearchResultItem {
                            drive_letter: vol.drive_letter,
                            record_idx: idx,
                            generation: vol.generation,
                        });
                    }
                }
            }

            if single_vol_presorted {
                let total = all_results.len();
                return (all_results, total, highlight_terms, snippets);
            }
        }

        // Multi-volume parallel sort
        let dir = if sort_asc {
            SortDirection::Ascending
        } else {
            SortDirection::Descending
        };

        if all_results.len() > 1 && !cancel.is_cancelled() {
            let mut vol_by_letter: [Option<&VolumeIndex>; 26] = [None; 26];
            for (ch, vol) in lock.iter() {
                let upper = ch.to_ascii_uppercase();
                if upper.is_ascii_uppercase() {
                    let idx = (upper as u8 - b'A') as usize;
                    if idx < 26 {
                        vol_by_letter[idx] = Some(vol);
                    }
                }
            }

            let get_vol = |letter: char| -> &VolumeIndex {
                let upper = letter.to_ascii_uppercase();
                if upper.is_ascii_uppercase() {
                    let idx = (upper as u8 - b'A') as usize;
                    if let Some(vol) = vol_by_letter.get(idx).copied().flatten() {
                        return vol;
                    }
                }
                lock.get(&letter).expect("volume index exists for drive")
            };

            enum SortKey<'a> {
                Name(&'a str),
                Path(std::sync::Arc<str>, &'a str),
                Size(u64, &'a str),
                DateModified(i64, &'a str),
                DateCreated(i64, &'a str),
                Extension(&'a str, &'a str),
            }

            let mut decorated: Vec<(SortKey, SearchResultItem)> = all_results
                .into_par_iter()
                .map(|item| {
                    let vol = get_vol(item.drive_letter);
                    let rec = &vol.records[item.record_idx as usize];
                    let name = vol.arena.get_name(rec.name_offset, rec.name_len);
                    let key = match sort_by {
                        SortColumn::Name => SortKey::Name(name),
                        SortColumn::Path => {
                            let parent = vol.path_cache.get_parent_path(
                                &vol.records,
                                &vol.arena,
                                vol.drive_letter,
                                item.record_idx,
                            );
                            SortKey::Path(parent, name)
                        }
                        SortColumn::Size => SortKey::Size(rec.size, name),
                        SortColumn::DateModified => SortKey::DateModified(rec.modified_ts, name),
                        SortColumn::DateCreated => SortKey::DateCreated(rec.created_ts, name),
                        SortColumn::Extension => {
                            let ext =
                                vol.arena
                                    .get_ext(rec.name_offset, rec.name_len, rec.ext_offset);
                            SortKey::Extension(ext, name)
                        }
                    };
                    (key, item)
                })
                .collect();

            decorated.par_sort_unstable_by(|(ka, a), (kb, b)| {
                let ord = match (ka, kb) {
                    (SortKey::Name(na), SortKey::Name(nb)) => cairn_core::sort::natural_cmp(na, nb),
                    (SortKey::Path(pa, na), SortKey::Path(pb, nb)) => pa
                        .cmp(pb)
                        .then_with(|| cairn_core::sort::natural_cmp(na, nb)),
                    (SortKey::Size(sa, na), SortKey::Size(sb, nb)) => sa
                        .cmp(sb)
                        .then_with(|| cairn_core::sort::natural_cmp(na, nb)),
                    (SortKey::DateModified(ma, na), SortKey::DateModified(mb, nb)) => ma
                        .cmp(mb)
                        .then_with(|| cairn_core::sort::natural_cmp(na, nb)),
                    (SortKey::DateCreated(ca, na), SortKey::DateCreated(cb, nb)) => ca
                        .cmp(cb)
                        .then_with(|| cairn_core::sort::natural_cmp(na, nb)),
                    (SortKey::Extension(ea, na), SortKey::Extension(eb, nb)) => {
                        cairn_core::sort::cmp_ascii_ignore_case(ea, eb)
                            .then_with(|| cairn_core::sort::natural_cmp(na, nb))
                    }
                    _ => std::cmp::Ordering::Equal,
                };

                let primary = if dir == SortDirection::Descending {
                    ord.reverse()
                } else {
                    ord
                };

                primary
                    .then_with(|| a.drive_letter.cmp(&b.drive_letter))
                    .then_with(|| a.record_idx.cmp(&b.record_idx))
            });

            all_results = decorated.into_iter().map(|(_, item)| item).collect();
        }

        let total = all_results.len();
        (all_results, total, highlight_terms, snippets)
    }

    /// Single volume search compatibility method.
    #[allow(dead_code)]
    pub fn execute_search(
        &self,
        raw_query: &str,
        sort_by: SortColumn,
        sort_asc: bool,
        cancel: &CancellationToken,
    ) -> (Vec<u32>, usize) {
        let (items, count, ..) = self.execute_search_multi(
            raw_query,
            &QueryDefaults::default(),
            sort_by,
            sort_asc,
            cancel,
        );
        let indices = items.into_iter().map(|it| it.record_idx).collect();
        (indices, count)
    }

    pub fn with_volumes<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&HashMap<char, VolumeIndex>) -> R,
    {
        let lock = self.volumes.read().unwrap();
        f(&lock)
    }

    #[allow(dead_code)]
    pub fn with_primary_volume<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&VolumeIndex) -> R,
    {
        let lock = self.volumes.read().unwrap();
        let vol = lock.get(&'C').or_else(|| lock.values().next())?;
        Some(f(vol))
    }

    #[allow(dead_code)]
    pub fn with_primary_volume_mut<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&mut VolumeIndex) -> R,
    {
        let mut lock = self.volumes.write().unwrap();
        let vol = if lock.contains_key(&'C') {
            lock.get_mut(&'C')?
        } else {
            lock.values_mut().next()?
        };
        Some(f(vol))
    }
}

/// Presorted index to use as the order for `sort_by`, or `None` when it cannot be trusted: a
/// dirty index no longer matches the records, and DateCreated/Extension have no presorted array.
fn presorted_order(vol: &VolumeIndex, sort_by: SortColumn) -> Option<&[u32]> {
    if vol.presorted_dirty {
        return None;
    }
    let order = match sort_by {
        SortColumn::Name => &vol.presorted.by_name,
        SortColumn::Size => &vol.presorted.by_size,
        SortColumn::DateModified => &vol.presorted.by_date_modified,
        SortColumn::Path => &vol.presorted.by_path,
        SortColumn::DateCreated | SortColumn::Extension => return None,
    };
    (!order.is_empty()).then_some(order.as_slice())
}

/// Checks if a file path matches any exclusion rule (IDX-09).
pub fn is_path_excluded(full_path: &str, rules: &[String]) -> bool {
    if rules.is_empty() || full_path.is_empty() {
        return false;
    }

    let full_path_normalized = full_path.replace('/', "\\");
    let full_path_lower = full_path_normalized.to_ascii_lowercase();

    for rule in rules {
        let rule_trimmed = rule.trim();
        if rule_trimmed.is_empty() {
            continue;
        }

        let rule_normalized = rule_trimmed.replace('/', "\\");

        // 1. Wildcard / glob matching
        if rule_normalized.contains('*') || rule_normalized.contains('?') {
            if cairn_core::engine::glob_match(&rule_normalized, &full_path_normalized, false) {
                return true;
            }
            let prefixed = format!(r"**\{}", rule_normalized.trim_start_matches(r"**\"));
            if cairn_core::engine::glob_match(&prefixed, &full_path_normalized, false) {
                return true;
            }
            continue;
        }

        let rule_lower = rule_normalized.to_ascii_lowercase();
        let rule_clean = rule_lower.trim_end_matches('\\');

        if rule_clean.is_empty() {
            continue;
        }

        // 2. Absolute path prefix match (e.g. "C:\Windows\Temp" or "C:\")
        if rule_clean.len() >= 2 && rule_clean.as_bytes()[1] == b':' {
            if full_path_lower == rule_clean {
                return true;
            }
            if full_path_lower.starts_with(rule_clean)
                && (rule_clean.ends_with('\\')
                    || full_path_lower.as_bytes().get(rule_clean.len()) == Some(&b'\\'))
            {
                return true;
            }
            continue;
        }

        // 3. Exact match against normalized path
        if full_path_lower == rule_clean {
            return true;
        }

        // 4. Segment or directory fragment match (e.g. "$Recycle.Bin", "node_modules", "target\debug")
        for segment in full_path_lower.split('\\') {
            if segment == rule_clean {
                return true;
            }
        }
        if full_path_lower.contains(&format!(r"\{}\", rule_clean))
            || full_path_lower.ends_with(&format!(r"\{}", rule_clean))
            || full_path_lower.starts_with(&format!(r"{}\", rule_clean))
        {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use cairn_core::syntax::parse_query;

    use super::*;
    use cairn_core::sort::SortColumn;

    fn index_item(frn: u64, parent_frn: u64, name: &str, is_dir: bool) -> ExtractedItem {
        ExtractedItem {
            frn,
            parent_frn,
            name: name.to_string(),
            is_dir,
            size: if is_dir { 0 } else { 16 },
            created_ts: 1,
            modified_ts: 1,
            flags: if is_dir {
                cairn_core::FLAG_DIRECTORY
            } else {
                0
            },
            is_reparse_point: false,
        }
    }

    fn sized_item(frn: u64, parent_frn: u64, name: &str, is_dir: bool, size: u64) -> ExtractedItem {
        let mut item = index_item(frn, parent_frn, name, is_dir);
        item.size = size;
        item
    }

    fn record_index(vol: &VolumeIndex, frn: u64) -> u32 {
        vol.frn_to_idx
            .get(&frn)
            .and_then(|indices| indices.first().copied())
            .expect("fixture record must exist")
    }

    #[test]
    fn directory_sizes_aggregate_their_indexed_descendants() {
        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![
            index_item(1, 0, "C:", true),
            index_item(2, 1, "A", true),
            sized_item(3, 2, "a.bin", false, 100),
            sized_item(4, 2, "b.bin", false, 50),
            index_item(5, 1, "B", true),
            sized_item(6, 5, "c.bin", false, 25),
        ]);

        assert_eq!(vol.records[record_index(&vol, 2) as usize].size, 150);
        assert_eq!(vol.records[record_index(&vol, 5) as usize].size, 25);
        assert_eq!(vol.records[record_index(&vol, 1) as usize].size, 175);
        assert_eq!(vol.records[record_index(&vol, 3) as usize].size, 100);
    }

    #[test]
    fn directory_sizes_follow_file_resizes_and_deletes() {
        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![
            index_item(1, 0, "C:", true),
            index_item(2, 1, "A", true),
            sized_item(3, 2, "a.bin", false, 100),
            sized_item(4, 2, "b.bin", false, 50),
        ]);
        let a = record_index(&vol, 3);
        vol.update_file_size(a, 200);
        assert_eq!(vol.records[record_index(&vol, 2) as usize].size, 250);
        assert_eq!(vol.records[record_index(&vol, 1) as usize].size, 250);

        vol.apply_usn_change(UsnChange::Deleted { frn: 3 });
        assert_eq!(vol.records[record_index(&vol, 2) as usize].size, 50);
        assert_eq!(vol.records[record_index(&vol, 1) as usize].size, 50);

        // Deleting the directory removes its remaining descendant total; the child's later
        // tombstone must not subtract the same bytes again.
        vol.apply_usn_change(UsnChange::Deleted { frn: 2 });
        assert_eq!(vol.records[record_index(&vol, 1) as usize].size, 0);
        vol.apply_usn_change(UsnChange::Deleted { frn: 4 });
        assert_eq!(vol.records[record_index(&vol, 1) as usize].size, 0);
    }

    #[test]
    fn directory_sizes_follow_a_move_across_parents() {
        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![
            index_item(1, 0, "C:", true),
            index_item(2, 1, "A", true),
            sized_item(3, 2, "a.bin", false, 100),
            index_item(4, 1, "B", true),
        ]);

        vol.apply_usn_change(UsnChange::Renamed {
            frn: 3,
            parent_frn: 4,
            name: "a.bin".to_string(),
            is_dir: false,
            is_new_name: true,
        });

        assert_eq!(vol.records[record_index(&vol, 2) as usize].size, 0);
        assert_eq!(vol.records[record_index(&vol, 4) as usize].size, 100);
        assert_eq!(vol.records[record_index(&vol, 1) as usize].size, 100);
    }

    #[test]
    fn usn_delete_removes_pending_renames_to_prevent_leak() {
        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![
            index_item(1, 0, "C:", true),
            index_item(2, 1, "test.txt", false),
        ]);

        // First half of a rename: records pending rename
        vol.apply_usn_change(UsnChange::Renamed {
            frn: 2,
            parent_frn: 1,
            name: "test.txt".to_string(),
            is_dir: false,
            is_new_name: false,
        });
        assert!(vol.pending_renames.contains_key(&2));

        // File is deleted before second half of rename arrives
        vol.apply_usn_change(UsnChange::Deleted { frn: 2 });
        assert!(!vol.pending_renames.contains_key(&2));
    }

    #[test]
    fn directory_sizes_survive_compaction() {
        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![
            index_item(1, 0, "C:", true),
            index_item(2, 1, "A", true),
            sized_item(3, 2, "a.bin", false, 100),
            index_item(4, 2, "dead", true),
        ]);
        vol.apply_usn_change(UsnChange::Deleted { frn: 4 });
        vol.compact();
        assert_eq!(vol.records[record_index(&vol, 2) as usize].size, 100);
        assert_eq!(vol.records[record_index(&vol, 1) as usize].size, 100);
    }

    #[test]
    fn appended_batches_report_folder_sizes_before_finalize() {
        let mut vol = VolumeIndex::new('C');
        vol.append_items(vec![
            index_item(1, 0, "C:", true),
            index_item(2, 1, "A", true),
            sized_item(3, 2, "a.bin", false, 100),
        ]);

        // Still mid-scan: no finalize ran, yet the parent already totals its indexed child.
        assert_eq!(
            vol.records[record_index(&vol, 2) as usize].size,
            100,
            "a folder must not read 0 B while its children are already indexed"
        );
        assert_eq!(vol.records[record_index(&vol, 1) as usize].size, 100);

        vol.append_items(vec![
            index_item(4, 2, "B", true),
            sized_item(5, 4, "b.bin", false, 50),
            sized_item(6, 2, "c.bin", false, 25),
        ]);

        assert_eq!(vol.records[record_index(&vol, 2) as usize].size, 175);
        assert_eq!(vol.records[record_index(&vol, 4) as usize].size, 50);
        assert_eq!(vol.records[record_index(&vol, 1) as usize].size, 175);
        assert_eq!(vol.records[record_index(&vol, 3) as usize].size, 100);

        vol.finalize_snapshot_scan();
        assert_eq!(
            vol.records[record_index(&vol, 2) as usize].size,
            175,
            "the authoritative pass must agree with the incremental totals"
        );
        assert_eq!(vol.records[record_index(&vol, 4) as usize].size, 50);
        assert_eq!(vol.records[record_index(&vol, 1) as usize].size, 175);
        assert!(vol.directory_sizes_valid);
    }

    #[test]
    fn ingest_items_aggregates_directory_sizes_and_marks_valid() {
        let mut vol = VolumeIndex::new('X');
        // Initial ingest of nested tree with file sizes
        vol.ingest_items(vec![
            index_item(1, 0, "X:", true),
            index_item(2, 1, "dir", true),
            sized_item(3, 2, "file1.txt", false, 400),
            sized_item(4, 2, "file2.txt", false, 600),
        ]);

        assert!(vol.directory_sizes_valid);
        assert_eq!(vol.records[record_index(&vol, 2) as usize].size, 1000);
        assert_eq!(vol.records[record_index(&vol, 1) as usize].size, 1000);
    }

    #[test]
    fn a_batch_with_several_kind_changes_ends_with_one_correct_aggregate() {
        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![
            index_item(1, 0, "C:", true),
            index_item(2, 1, "A", true),
            sized_item(3, 2, "a.bin", false, 100),
            sized_item(4, 2, "b.bin", false, 50),
            index_item(5, 1, "B", true),
            sized_item(6, 5, "c.bin", false, 25),
        ]);

        // Two files turn into directories, and another file moves from B into A in the same batch.
        vol.apply_usn_changes(vec![
            UsnChange::BasicInfoChanged {
                frn: 3,
                ts: 2,
                attributes: cairn_ntfs::usn::FILE_ATTRIBUTE_DIRECTORY,
            },
            UsnChange::BasicInfoChanged {
                frn: 4,
                ts: 2,
                attributes: cairn_ntfs::usn::FILE_ATTRIBUTE_DIRECTORY,
            },
            UsnChange::Renamed {
                frn: 6,
                parent_frn: 2,
                name: "c.bin".to_string(),
                is_dir: false,
                is_new_name: true,
            },
        ]);

        assert!(vol.directory_sizes_valid, "the batch ends fully aggregated");
        assert_eq!(vol.records[record_index(&vol, 3) as usize].size, 0);
        assert_eq!(vol.records[record_index(&vol, 4) as usize].size, 0);
        assert_eq!(
            vol.records[record_index(&vol, 2) as usize].size,
            25,
            "A now holds two empty directories and the moved file"
        );
        assert_eq!(vol.records[record_index(&vol, 5) as usize].size, 0);
        assert_eq!(vol.records[record_index(&vol, 1) as usize].size, 25);
    }

    #[test]
    fn a_cache_without_the_directory_size_flag_is_recomputed_and_migrated() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let mut vol = VolumeIndex::new('Z');
        vol.volume_serial = 0x5151;
        vol.ingest_items(vec![
            index_item(1, 0, "Z:", true),
            index_item(2, 1, "A", true),
            sized_item(3, 2, "a.bin", false, 100),
            sized_item(4, 2, "b.bin", false, 50),
        ]);
        // Exactly what an older cache holds: every directory size is still zero, and the header
        // has no validity flag because that byte did not exist.
        for record in &mut vol.records {
            if record.is_dir() && !record.is_tombstone() {
                record.size = 0;
            }
        }
        vol.directory_sizes_valid = false;
        vol.save_to_dir(temp_dir.path()).expect("legacy cache");

        let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec!['Z']);
        engine.with_volumes(|volumes| {
            let volume = &volumes[&'Z'];
            assert_eq!(
                volume.records[record_index(volume, 2) as usize].size,
                150,
                "the one-time load pass must rebuild folder totals"
            );
            assert_eq!(volume.records[record_index(volume, 1) as usize].size, 150);
            assert!(volume.directory_sizes_valid);
        });

        let migrated =
            persistence::load_volume_index(&temp_dir.path().join("vol_Z.idx")).expect("cache");
        assert!(
            migrated.directory_sizes_valid,
            "the migration must rewrite the flag so later launches skip the aggregation"
        );
    }

    #[test]
    fn a_cache_with_a_valid_flag_keeps_its_stored_folder_sizes() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let mut vol = VolumeIndex::new('Y');
        vol.volume_serial = 0x4242;
        vol.ingest_items(vec![
            index_item(1, 0, "Y:", true),
            index_item(2, 1, "A", true),
            sized_item(3, 2, "a.bin", false, 100),
        ]);
        // A sentinel no aggregation would produce: keeping it proves the load trusted the flag
        // instead of walking the whole table again.
        let a = record_index(&vol, 2) as usize;
        vol.records[a].size = 777;
        assert!(vol.directory_sizes_valid);
        vol.save_to_dir(temp_dir.path()).expect("valid cache");

        let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec!['Y']);
        engine.with_volumes(|volumes| {
            let volume = &volumes[&'Y'];
            assert_eq!(volume.records[record_index(volume, 2) as usize].size, 777);
            assert!(volume.directory_sizes_valid);
        });
    }

    #[test]
    fn append_items_drops_the_excluded_service_data_subtree() {
        let mut vol = VolumeIndex::new('C');
        vol.set_excluded_roots([2]);
        vol.append_items(vec![
            index_item(1, 0, "C:", true),
            index_item(2, 1, "Cairn", true),
            index_item(4, 2, "vol_C.idx", false),
            index_item(3, 1, "Users", true),
            index_item(5, 3, "notes.txt", false),
        ]);

        let frns: Vec<u64> = vol.records().iter().map(|record| record.frn).collect();
        assert_eq!(frns, [1, 3, 5]);
        let notes = &vol.records[2];
        assert_eq!(
            vol.records[notes.parent_idx as usize].frn, 3,
            "a kept child must still resolve its parent after skipped rows"
        );
        assert!(vol.excluded_frns.contains(&2));
        assert!(!vol.excluded_frns.contains(&4));
        assert!(!vol.frn_to_idx.contains_key(&2));
        assert!(!vol.frn_to_idx.contains_key(&4));
    }

    #[test]
    fn usn_events_for_the_excluded_subtree_never_enter_the_index() {
        let mut vol = VolumeIndex::new('C');
        vol.set_excluded_roots([2]);
        vol.append_items(vec![
            index_item(1, 0, "C:", true),
            index_item(3, 1, "Users", true),
        ]);
        let before = vol.records.len();

        vol.apply_usn_change(UsnChange::Created {
            frn: 4,
            parent_frn: 2,
            name: "vol_C.idx".to_string(),
            is_dir: false,
            attributes: 0,
            ts: 10,
        });
        assert_eq!(vol.records.len(), before);
        assert!(!vol.frn_to_idx.contains_key(&4));

        vol.apply_usn_change(UsnChange::Deleted { frn: 4 });
        assert!(
            vol.pending_content_deletions.is_empty(),
            "a file that was never indexed has no content document to delete"
        );
        assert!(vol.records.iter().all(|record| !record.is_tombstone()));

        vol.apply_usn_change(UsnChange::Created {
            frn: 6,
            parent_frn: 3,
            name: "new.txt".to_string(),
            is_dir: false,
            attributes: 0,
            ts: 11,
        });
        assert_eq!(vol.records.len(), before + 1);
        assert_eq!(vol.records.last().expect("new record").frn, 6);
    }

    #[test]
    fn compact_drops_tombstones_and_excluded_subtrees_and_remaps_parents() {
        let mut vol = VolumeIndex::new('C');
        vol.append_items(vec![
            index_item(1, 0, "C:", true),
            index_item(2, 1, "Cairn", true),
            index_item(4, 2, "vol_C.idx", false),
            index_item(7, 2, "Nested", true),
            index_item(8, 7, "sub.bin", false),
            index_item(3, 1, "Users", true),
            index_item(5, 3, "notes.txt", false),
            index_item(9, 3, "Temp", true),
            index_item(10, 9, "scratch.txt", false),
            index_item(6, 3, "deleted.txt", false),
        ]);
        // Loaded caches predate the exclusion, so the roots are registered after the records.
        vol.set_excluded_roots([2]);
        vol.apply_usn_change(UsnChange::Deleted { frn: 7 });
        vol.apply_usn_change(UsnChange::Deleted { frn: 9 });
        vol.apply_usn_change(UsnChange::Deleted { frn: 6 });
        vol.last_usn = 777;
        vol.journal_id = 9;
        vol.volume_serial = 42;
        vol.usn_enabled = false;
        vol.status = VolumeState::CatchingUp(50);
        assert_eq!(vol.records.len(), 10);

        // 2, 4, 7 and 8 are service data (7 is also a tombstone); 9 and 6 are tombstones. 10's
        // parent is gone but its own row is still live, so it is kept and re-rooted.
        assert_eq!(vol.compact(), 6);
        let frns: Vec<u64> = vol.records.iter().map(|record| record.frn).collect();
        assert_eq!(frns, [1, 3, 5, 10]);
        assert_eq!(vol.record_name(&vol.records[2]), "notes.txt");
        assert_eq!(vol.record_name(&vol.records[3]), "scratch.txt");
        assert_eq!(vol.records[2].parent_idx, 1);
        assert_eq!(vol.records[1].frn, 3);
        assert_eq!(vol.records[3].parent_idx, FileRecord::ROOT_PARENT);

        for frn in [2, 4, 6, 7, 8, 9] {
            assert!(!vol.frn_to_idx.contains_key(&frn));
        }
        assert_eq!(vol.frn_to_idx[&5], vec![2]);
        assert!(vol.excluded_frns.contains(&2), "configuration must survive");
        assert_eq!(vol.last_usn, 777);
        assert_eq!(vol.journal_id, 9);
        assert_eq!(vol.volume_serial, 42);
        assert!(!vol.usn_enabled);
        assert_eq!(vol.status, VolumeState::CatchingUp(50));
        assert!(!vol.presorted_dirty);
        assert_eq!(vol.presorted.by_name.len(), 4);

        let cancel = CancellationToken::new();
        let matches = cairn_core::engine::execute_query(
            &vol.records,
            &vol.arena,
            &vol.path_cache,
            vol.drive_letter,
            &parse_query("notes"),
            &cancel,
        );
        assert_eq!(matches, vec![2]);
        let row = vol.build_file_row(2, &[], |_| None).expect("file row");
        assert_eq!(row.name, "notes.txt");

        assert_eq!(
            vol.compact(),
            0,
            "a compacted volume has nothing left to drop"
        );
    }

    #[test]
    fn compact_if_needed_leaves_a_small_index_alone() {
        let mut vol = VolumeIndex::new('C');
        vol.append_items(vec![index_item(1, 0, "only.txt", false)]);
        vol.apply_usn_change(UsnChange::Deleted { frn: 1 });
        assert!(
            !vol.compact_if_needed(),
            "a table below the size threshold must not be compacted"
        );
        assert_eq!(vol.records.len(), 1);
        assert!(vol.records[0].is_tombstone());
    }

    #[test]
    fn test_volume_index_ingest_and_usn_changes() {
        let mut vol = VolumeIndex::new('C');

        // Initial ingest of 3 items
        let items = vec![
            ExtractedItem {
                frn: 10,
                parent_frn: 5,
                name: "documents".to_string(),
                is_dir: true,
                size: 0,
                created_ts: 1000,
                modified_ts: 1000,
                flags: cairn_core::FLAG_DIRECTORY,
                is_reparse_point: false,
            },
            ExtractedItem {
                frn: 11,
                parent_frn: 10,
                name: "annual_report.pdf".to_string(),
                is_dir: false,
                size: 2048,
                created_ts: 1001,
                modified_ts: 1001,
                flags: 0,
                is_reparse_point: false,
            },
            ExtractedItem {
                frn: 12,
                parent_frn: 10,
                name: "budget.xlsx".to_string(),
                is_dir: false,
                size: 4096,
                created_ts: 1002,
                modified_ts: 1002,
                flags: 0,
                is_reparse_point: false,
            },
        ];
        vol.ingest_items(items);

        let cancel = CancellationToken::new();

        // 1. Initial search finds report
        let q_report = parse_query("annual");
        let matches = cairn_core::engine::execute_query(
            &vol.records,
            &vol.arena,
            &vol.path_cache,
            vol.drive_letter,
            &q_report,
            &cancel,
        );
        assert_eq!(matches.len(), 1);

        // 2. USN Create: add "presentation.pptx"
        vol.apply_usn_change(UsnChange::Created {
            frn: 13,
            parent_frn: 10,
            name: "presentation.pptx".to_string(),
            is_dir: false,
            attributes: 0,
            ts: 1003,
        });

        let q_pres = parse_query("presentation");
        let matches_pres = cairn_core::engine::execute_query(
            &vol.records,
            &vol.arena,
            &vol.path_cache,
            vol.drive_letter,
            &q_pres,
            &cancel,
        );
        assert_eq!(matches_pres.len(), 1);

        // 3. USN Rename: rename "annual_report.pdf" to "q4_report.pdf"
        vol.apply_usn_change(UsnChange::Renamed {
            frn: 11,
            parent_frn: 10,
            name: "annual_report.pdf".to_string(),
            is_dir: false,
            is_new_name: false,
        });
        vol.apply_usn_change(UsnChange::Renamed {
            frn: 11,
            parent_frn: 10,
            name: "q4_report.pdf".to_string(),
            is_dir: false,
            is_new_name: true,
        });

        let q_annual = parse_query("annual");
        let matches_annual = cairn_core::engine::execute_query(
            &vol.records,
            &vol.arena,
            &vol.path_cache,
            vol.drive_letter,
            &q_annual,
            &cancel,
        );
        assert_eq!(matches_annual.len(), 0);

        let q_q4 = parse_query("q4_report");
        let matches_q4 = cairn_core::engine::execute_query(
            &vol.records,
            &vol.arena,
            &vol.path_cache,
            vol.drive_letter,
            &q_q4,
            &cancel,
        );
        assert_eq!(matches_q4.len(), 1);

        // 4. USN Delete: delete "budget.xlsx"
        vol.apply_usn_change(UsnChange::Deleted { frn: 12 });
        let q_budget = parse_query("budget");
        let matches_budget = cairn_core::engine::execute_query(
            &vol.records,
            &vol.arena,
            &vol.path_cache,
            vol.drive_letter,
            &q_budget,
            &cancel,
        );
        assert_eq!(matches_budget.len(), 0);
    }

    #[test]
    fn test_multi_volume_engine_save_load_lifecycle() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);

        // Create volume X
        let mut vol_x = VolumeIndex::new('X');
        vol_x.volume_serial = 0x1111;
        vol_x.journal_id = 100;
        vol_x.last_usn = 5000;
        vol_x.ingest_items(vec![ExtractedItem {
            frn: 100,
            parent_frn: 5,
            name: "x_file.txt".to_string(),
            is_dir: false,
            size: 100,
            created_ts: 100,
            modified_ts: 100,
            flags: 0,
            is_reparse_point: false,
        }]);

        // Create volume Y
        let mut vol_y = VolumeIndex::new('Y');
        vol_y.volume_serial = 0x2222;
        vol_y.journal_id = 200;
        vol_y.last_usn = 6000;
        vol_y.ingest_items(vec![ExtractedItem {
            frn: 200,
            parent_frn: 5,
            name: "y_file.txt".to_string(),
            is_dir: false,
            size: 200,
            created_ts: 200,
            modified_ts: 200,
            flags: 0,
            is_reparse_point: false,
        }]);

        engine.add_volume(vol_x);
        engine.add_volume(vol_y);

        // Atomic save to disk
        engine.save_all_indexes().expect("save_all_indexes");

        // Restore into a new MultiVolumeEngine from the same temp_dir
        let loaded_engine =
            MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec!['X', 'Y']);

        let cancel = CancellationToken::new();
        let (results, total, highlights, _) = loaded_engine.execute_search_multi(
            "file.txt",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );

        assert_eq!(total, 2);
        assert_eq!(results.len(), 2);
        assert!(highlights.contains(&"file.txt".to_string()));

        let summaries = loaded_engine.get_volume_summaries();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].drive_letter, 'X');
        assert_eq!(summaries[0].indexed_count, 1);
        assert_eq!(summaries[1].drive_letter, 'Y');
        assert_eq!(summaries[1].indexed_count, 1);
    }

    #[test]
    fn test_volume_cache_invalidation_does_not_silently_return_stale_data() {
        let temp_dir = tempfile::tempdir().expect("tempdir");

        // 1. Save a cached index for volume 'Z' with old records
        let mut vol_z = VolumeIndex::new('Z');
        vol_z.volume_serial = 0xEEEE;
        vol_z.journal_id = 999;
        vol_z.last_usn = 1000;
        vol_z.ingest_items(vec![ExtractedItem {
            frn: 50,
            parent_frn: 5,
            name: "stale_secret.txt".to_string(),
            is_dir: false,
            size: 500,
            created_ts: 100,
            modified_ts: 100,
            flags: 0,
            is_reparse_point: false,
        }]);

        persistence::save_volume_index_atomic(
            temp_dir.path(),
            'Z',
            vol_z.volume_serial,
            vol_z.journal_id,
            vol_z.last_usn,
            &vol_z.records,
            &vol_z.arena,
            vol_z.directory_sizes_valid,
        )
        .expect("save_volume_index_atomic");

        // Corrupt cache file (simulating invalidation / corrupt index)
        let cache_file = temp_dir.path().join("vol_Z.idx");
        std::fs::write(&cache_file, b"corrupted content").expect("write corrupted");

        // Load into engine: cache is invalid and live MFT scan for Z: fails
        let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec!['Z']);

        let summaries = engine.get_volume_summaries();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].drive_letter, 'Z');
        // Must NOT remain in VolumeState::Synced and must NOT retain stale records (IDX-05, IDX-07)
        assert_ne!(
            summaries[0].status,
            VolumeState::Synced,
            "Volume must not remain Synced when cache invalidation occurs"
        );
        assert_eq!(
            summaries[0].indexed_count, 0,
            "Stale records must be cleared"
        );

        // Search must return 0 results
        let cancel = CancellationToken::new();
        let (results, total, ..) = engine.execute_search_multi(
            "stale_secret",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total, 0);
        assert!(results.is_empty());
    }

    #[test]
    fn test_journal_id_mismatch_and_usn_out_of_range_invalidates_volume() {
        let mut vol = VolumeIndex::new('C');
        vol.volume_serial = 0x1111;
        vol.ingest_items(vec![ExtractedItem {
            frn: 10,
            parent_frn: 5,
            name: "cached_file.txt".to_string(),
            is_dir: false,
            size: 100,
            created_ts: 10,
            modified_ts: 10,
            flags: 0,
            is_reparse_point: false,
        }]);
        vol.journal_id = 500;
        vol.last_usn = 1000;
        assert_eq!(vol.records.len(), 1);

        // A journal recreated under a snapshot leaves it on no valid timeline (IDX-05), and the
        // rebuild verdict comes from the same function the full-scan paths run through.
        assert_eq!(
            post_scan_decision(true, 1000, Some(2000), Some(2000)),
            PostScanDecision::Rebuild
        );
        assert!(
            !apply_post_scan_decision(&mut vol, PostScanDecision::Rebuild),
            "an unreconcilable volume must not be kept"
        );

        // Must be marked Rebuilding with records wiped (never Synced with stale data)
        assert_eq!(vol.status, VolumeState::Rebuilding);
        assert_eq!(vol.records.len(), 0);
        assert_eq!(vol.arena.len(), 0);
    }

    #[test]
    fn test_usn_create_already_captured_by_the_snapshot_is_not_duplicated() {
        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![ExtractedItem {
            frn: 20,
            parent_frn: 5,
            name: "mid_scan.txt".to_string(),
            is_dir: false,
            size: 7,
            created_ts: 100,
            modified_ts: 100,
            flags: 0,
            is_reparse_point: false,
        }]);
        assert_eq!(vol.records.len(), 1);

        // `[T0, T1)` overlaps the scan itself, so the create for a captured file arrives again.
        vol.apply_usn_change(UsnChange::Created {
            frn: 20,
            parent_frn: 5,
            name: "mid_scan.txt".to_string(),
            is_dir: false,
            attributes: 0,
            ts: 300,
        });
        assert_eq!(
            vol.records.len(),
            1,
            "replaying a create the snapshot already holds must not double the row"
        );
        assert_eq!(
            vol.records[0].size, 7,
            "the real size beats the journal's zero"
        );

        // A different name for the same FRN is a hard link and stays a record of its own (IDX-06).
        vol.apply_usn_change(UsnChange::Created {
            frn: 20,
            parent_frn: 9,
            name: "hard_link.txt".to_string(),
            is_dir: false,
            attributes: 0,
            ts: 300,
        });
        assert_eq!(vol.records.len(), 2);
    }

    #[test]
    fn test_full_scan_paths_can_only_reach_synced_through_journal_reconciliation() {
        let src = include_str!("sync_engine.rs");
        let production = &src[..src.find("#[cfg(test)]").expect("unit test module")];
        let recon_src = include_str!("sync_engine/journal_reconciliation.rs");
        let recon_prod = &recon_src[..recon_src.find("#[cfg(test)]").unwrap_or(recon_src.len())];

        // `catch_up_state` is the one producer of `Synced` and the polling loop only recognises it.
        // A third mention means some path declared itself fresh without a journal position.
        let total_synced_count = production.matches("VolumeState::Synced").count()
            + recon_prod.matches("VolumeState::Synced").count();
        assert_eq!(
            total_synced_count, 2,
            "`Synced` may only be handed out by catch_up_state and matched by the polling gate"
        );

        for entry_point in [
            "if !loaded_ok && vol.status != VolumeState::Offline",
            "pub fn rescan_volume(",
        ] {
            let body = body_of(src, entry_point);
            assert!(
                !body.contains("VolumeState::Synced"),
                "{entry_point} is assigning a terminal state itself instead of reconciling"
            );
            assert!(
                body.contains("finalize_scanned_volume"),
                "{entry_point} must hand its scan result to the journal reconciliation"
            );
        }
    }

    /// Byte slice from the first `{` after `needle` up to its matching closing brace.
    fn body_of<'a>(src: &'a str, needle: &str) -> &'a str {
        let start = src
            .find(needle)
            .unwrap_or_else(|| panic!("`{needle}` is gone from sync_engine.rs"));
        let open = start + src[start..].find('{').expect("an opening brace");
        let mut depth = 0usize;
        for (offset, ch) in src[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &src[open..open + offset + 1];
                    }
                }
                _ => {}
            }
        }
        panic!("unterminated body after `{needle}`");
    }

    #[test]
    fn test_excluded_drives_filtering() {
        let temp_dir = tempfile::tempdir().expect("tempdir");

        let config = ServiceConfig {
            excluded_drives: vec!['D'],
            ..Default::default()
        };

        // 1. Initialize engine with drives C and D, but D is in excluded_drives
        let engine = MultiVolumeEngine::with_index_dir_drives_and_config(
            temp_dir.path(),
            vec!['C', 'D'],
            config,
        );

        let summaries = engine.get_volume_summaries();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].drive_letter, 'C');
        assert!(
            !summaries.iter().any(|s| s.drive_letter == 'D'),
            "Excluded drive D must not be initialized (IDX-01)"
        );

        // 2. Add an excluded volume manually - add_volume must reject it
        let mut vol_d = VolumeIndex::new('D');
        vol_d.ingest_items(vec![ExtractedItem {
            frn: 10,
            parent_frn: 5,
            name: "d_file.txt".to_string(),
            is_dir: false,
            size: 10,
            created_ts: 10,
            modified_ts: 10,
            flags: 0,
            is_reparse_point: false,
        }]);
        engine.add_volume(vol_d);
        assert!(
            !engine
                .get_volume_summaries()
                .iter()
                .any(|s| s.drive_letter == 'D'),
            "add_volume must reject excluded drive D"
        );

        // 3. Hot update: add 'C' to excluded_drives
        let mut new_cfg = engine.get_config();
        new_cfg.excluded_drives.push('C');
        engine.update_config(new_cfg);

        // Volume C must be purged from memory and summaries
        let summaries_after = engine.get_volume_summaries();
        assert!(
            summaries_after.is_empty(),
            "Newly excluded drive C must be purged from memory (IDX-01)"
        );

        let cancel = CancellationToken::new();
        let (results, total, ..) = engine.execute_search_multi(
            "",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total, 0);
        assert!(results.is_empty());
    }

    #[test]
    fn test_excluded_paths_filtering_and_hot_apply() {
        let temp_dir = tempfile::tempdir().expect("tempdir");

        let config = ServiceConfig {
            excluded_drives: vec![],
            excluded_paths: vec![
                r"$Recycle.Bin".to_string(),
                r"System Volume Information".to_string(),
                r"C:/Secret/".to_string(),
                r"node_modules/".to_string(),
                r"target/debug".to_string(),
                r"**/*.tmp".to_string(),
            ],
            ..Default::default()
        };

        let engine =
            MultiVolumeEngine::with_index_dir_drives_and_config(temp_dir.path(), vec![], config);

        // Populate volume C
        let mut vol = VolumeIndex::new('C');
        let items = vec![
            ExtractedItem {
                frn: 10,
                parent_frn: 5,
                name: "$Recycle.Bin".to_string(),
                is_dir: true,
                size: 0,
                created_ts: 100,
                modified_ts: 100,
                flags: cairn_core::FLAG_DIRECTORY,
                is_reparse_point: false,
            },
            ExtractedItem {
                frn: 11,
                parent_frn: 10,
                name: "deleted_file.txt".to_string(),
                is_dir: false,
                size: 100,
                created_ts: 100,
                modified_ts: 100,
                flags: 0,
                is_reparse_point: false,
            },
            ExtractedItem {
                frn: 20,
                parent_frn: 5,
                name: "Secret".to_string(),
                is_dir: true,
                size: 0,
                created_ts: 100,
                modified_ts: 100,
                flags: cairn_core::FLAG_DIRECTORY,
                is_reparse_point: false,
            },
            ExtractedItem {
                frn: 21,
                parent_frn: 20,
                name: "confidential.pdf".to_string(),
                is_dir: false,
                size: 200,
                created_ts: 100,
                modified_ts: 100,
                flags: 0,
                is_reparse_point: false,
            },
            ExtractedItem {
                frn: 30,
                parent_frn: 5,
                name: "scratch.tmp".to_string(),
                is_dir: false,
                size: 50,
                created_ts: 100,
                modified_ts: 100,
                flags: 0,
                is_reparse_point: false,
            },
            ExtractedItem {
                frn: 35,
                parent_frn: 5,
                name: "node_modules".to_string(),
                is_dir: true,
                size: 0,
                created_ts: 100,
                modified_ts: 100,
                flags: cairn_core::FLAG_DIRECTORY,
                is_reparse_point: false,
            },
            ExtractedItem {
                frn: 36,
                parent_frn: 35,
                name: "package.json".to_string(),
                is_dir: false,
                size: 150,
                created_ts: 100,
                modified_ts: 100,
                flags: 0,
                is_reparse_point: false,
            },
            ExtractedItem {
                frn: 40,
                parent_frn: 5,
                name: "public_doc.txt".to_string(),
                is_dir: false,
                size: 300,
                created_ts: 100,
                modified_ts: 100,
                flags: 0,
                is_reparse_point: false,
            },
        ];
        vol.ingest_items(items);
        engine.add_volume(vol);

        let cancel = CancellationToken::new();

        // 1. Search without query text (returns all allowed)
        let (results, total, ..) = engine.execute_search_multi(
            "",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        // Only public_doc.txt (frn 40) should be returned:
        // - deleted_file.txt in $Recycle.Bin is excluded
        // - confidential.pdf in C:\Secret is excluded (via C:/Secret/ rule)
        // - scratch.tmp (*.tmp) is excluded (via **/*.tmp rule)
        // - package.json in node_modules is excluded (via node_modules/ rule)
        // - $Recycle.Bin, Secret, node_modules directories match excluded rules
        assert_eq!(total, 1);
        assert_eq!(results.len(), 1);

        // 2. Direct search for deleted_file must be excluded
        let (results, total, ..) = engine.execute_search_multi(
            "deleted_file",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total, 0);
        assert!(results.is_empty());

        // 3. Direct search for confidential must be excluded
        let (results, total, ..) = engine.execute_search_multi(
            "confidential",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total, 0);
        assert!(results.is_empty());

        // 4. Direct search for package.json in node_modules must be excluded
        let (results, total, ..) = engine.execute_search_multi(
            "package.json",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total, 0);
        assert!(results.is_empty());

        // 5. Hot config update: remove **/*.tmp rule (IDX-09)
        let mut new_cfg = engine.get_config();
        new_cfg.excluded_paths.retain(|p| !p.contains("*.tmp"));
        engine.update_config(new_cfg);

        // Now search for .tmp finds scratch.tmp immediately without restart
        let (results, total, ..) = engine.execute_search_multi(
            ".tmp",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total, 1);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_snapshot_mode_fallback_and_search() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);

        let mut vol = VolumeIndex::new('E');
        vol.ingest_items(vec![ExtractedItem {
            frn: 55,
            parent_frn: 5,
            name: "snapshot_file.rs".to_string(),
            is_dir: false,
            size: 1024,
            created_ts: 100,
            modified_ts: 100,
            flags: 0,
            is_reparse_point: false,
        }]);

        // Downgrade to SnapshotMode (IDX-08)
        vol.status = VolumeState::SnapshotMode;
        vol.usn_enabled = false;

        engine.add_volume(vol);

        let summaries = engine.get_volume_summaries();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].drive_letter, 'E');
        assert_eq!(summaries[0].status, VolumeState::SnapshotMode);
        assert!(!summaries[0].usn_enabled);
        assert_eq!(summaries[0].indexed_count, 1);

        // SnapshotMode volume must participate in search and return results
        let cancel = CancellationToken::new();
        let (results, total, ..) = engine.execute_search_multi(
            "snapshot_file",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total, 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].drive_letter, 'E');

        // rescan_snapshot_volumes should execute cleanly without panic
        engine.rescan_snapshot_volumes();
    }

    fn sorted_test_item(frn: u64, name: &str, created_ts: i64) -> ExtractedItem {
        ExtractedItem {
            frn,
            parent_frn: 5,
            name: name.to_string(),
            is_dir: false,
            size: 0,
            created_ts,
            modified_ts: 0,
            flags: 0,
            is_reparse_point: false,
        }
    }

    fn engine_with_sorting_fixture() -> (tempfile::TempDir, MultiVolumeEngine) {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);

        // Record order deliberately differs from every sorted order.
        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![
            sorted_test_item(1, "charlie.txt", 200),
            sorted_test_item(2, "alpha.md", 300),
            sorted_test_item(3, "bravo.png", 100),
        ]);
        engine.add_volume(vol);
        (temp_dir, engine)
    }

    fn result_names(
        engine: &MultiVolumeEngine,
        sort_by: SortColumn,
        sort_asc: bool,
    ) -> Vec<String> {
        let cancel = CancellationToken::new();
        let (results, total, ..) =
            engine.execute_search_multi("", &QueryDefaults::default(), sort_by, sort_asc, &cancel);
        assert_eq!(total, results.len());
        engine.with_volumes(|vols| {
            let vol = &vols[&'C'];
            results
                .iter()
                .map(|r| {
                    vol.record_name(&vol.records[r.record_idx as usize])
                        .to_string()
                })
                .collect()
        })
    }

    #[test]
    fn a_filtered_query_does_not_rebuild_the_whole_volume_sort() {
        let (_temp_dir, engine) = engine_with_sorting_fixture();
        engine
            .with_primary_volume_mut(|volume| volume.presorted_dirty = true)
            .expect("primary volume");

        let cancel = CancellationToken::new();
        let (results, total, ..) = engine.execute_search_multi(
            "alpha",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total, 1);
        assert_eq!(results.len(), 1);
        assert!(
            engine.with_volumes(|volumes| volumes[&'C'].presorted_dirty),
            "a filtered query has no use for the volume-wide presorted order"
        );

        let (_, total, ..) = engine.execute_search_multi(
            "",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total, 3);
        assert!(
            !engine.with_volumes(|volumes| volumes[&'C'].presorted_dirty),
            "the all-files query does need the order and must rebuild it"
        );
    }

    #[test]
    fn test_single_volume_query_sorts_columns_without_presorted_arrays() {
        let (_temp_dir, engine) = engine_with_sorting_fixture();

        // DateCreated and Extension have no presorted array, so the single-volume fast path must
        // fall back to the global sort instead of returning record order.
        assert_eq!(
            result_names(&engine, SortColumn::DateCreated, true),
            ["bravo.png", "charlie.txt", "alpha.md"]
        );
        assert_eq!(
            result_names(&engine, SortColumn::DateCreated, false),
            ["alpha.md", "charlie.txt", "bravo.png"]
        );
        assert_eq!(
            result_names(&engine, SortColumn::Extension, true),
            ["alpha.md", "bravo.png", "charlie.txt"]
        );
        assert_eq!(
            result_names(&engine, SortColumn::Extension, false),
            ["charlie.txt", "bravo.png", "alpha.md"]
        );
    }

    #[test]
    fn test_single_volume_name_query_falls_back_when_presorted_index_is_empty() {
        let (_temp_dir, engine) = engine_with_sorting_fixture();
        engine
            .with_primary_volume_mut(|vol| vol.presorted.by_name.clear())
            .expect("primary volume");

        assert_eq!(
            result_names(&engine, SortColumn::Name, true),
            ["alpha.md", "bravo.png", "charlie.txt"]
        );
        assert_eq!(
            result_names(&engine, SortColumn::Name, false),
            ["charlie.txt", "bravo.png", "alpha.md"]
        );
    }

    #[test]
    fn test_presorted_order_rejects_dirty_or_missing_columns() {
        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![sorted_test_item(1, "a.txt", 1)]);

        assert!(presorted_order(&vol, SortColumn::Name).is_some());
        assert!(presorted_order(&vol, SortColumn::DateCreated).is_none());
        assert!(presorted_order(&vol, SortColumn::Extension).is_none());

        vol.presorted.by_name.clear();
        assert!(presorted_order(&vol, SortColumn::Name).is_none());

        vol.presorted_dirty = true;
        assert!(
            presorted_order(&vol, SortColumn::Size).is_none(),
            "a dirty presorted index is not a valid order"
        );
    }

    #[test]
    fn a_cached_usn_outside_the_current_journal_window_is_not_usable() {
        let current = cairn_ntfs::usn::UsnJournalData {
            journal_id: 7,
            first_usn: 100,
            next_usn: 200,
            lowest_valid_usn: 100,
        };
        assert!(MultiVolumeEngine::cached_position_is_usable(
            7, 150, &current
        ));
        assert!(
            MultiVolumeEngine::cached_position_is_usable(0, 150, &current),
            "an unknown cached journal id is acceptable when the position is in range"
        );
        assert!(
            !MultiVolumeEngine::cached_position_is_usable(7, 99, &current),
            "a position before the journal's first USN cannot be replayed"
        );
        assert!(
            !MultiVolumeEngine::cached_position_is_usable(7, 201, &current),
            "a position ahead of the journal tail can never catch up"
        );
        assert!(
            !MultiVolumeEngine::cached_position_is_usable(8, 150, &current),
            "a different journal generation invalidates the saved position"
        );
    }

    #[test]
    fn an_enormous_usn_backlog_is_rebuilt_instead_of_replayed() {
        assert!(!MultiVolumeEngine::backlog_is_too_large(
            0,
            64 * 1024 * 1024
        ));
        assert!(MultiVolumeEngine::backlog_is_too_large(
            0,
            64 * 1024 * 1024 + 1
        ));
    }

    #[test]
    fn test_multi_volume_schwartzian_transform_sort() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);

        let mut vol_c = VolumeIndex::new('C');
        vol_c.ingest_items(vec![
            ExtractedItem {
                frn: 1,
                parent_frn: 5,
                name: "file_10.txt".to_string(),
                is_dir: false,
                size: 50,
                created_ts: 100,
                modified_ts: 300,
                flags: 0,
                is_reparse_point: false,
            },
            ExtractedItem {
                frn: 2,
                parent_frn: 5,
                name: "file_2.txt".to_string(),
                is_dir: false,
                size: 200,
                created_ts: 300,
                modified_ts: 100,
                flags: 0,
                is_reparse_point: false,
            },
        ]);

        let mut vol_d = VolumeIndex::new('D');
        vol_d.ingest_items(vec![
            ExtractedItem {
                frn: 1,
                parent_frn: 5,
                name: "file_1.txt".to_string(),
                is_dir: false,
                size: 100,
                created_ts: 200,
                modified_ts: 200,
                flags: 0,
                is_reparse_point: false,
            },
            ExtractedItem {
                frn: 2,
                parent_frn: 5,
                name: "file_20.txt".to_string(),
                is_dir: false,
                size: 10,
                created_ts: 400,
                modified_ts: 400,
                flags: 0,
                is_reparse_point: false,
            },
        ]);

        engine.add_volume(vol_c);
        engine.add_volume(vol_d);

        let cancel = CancellationToken::new();

        // 1. Sort by Name ascending (natural sort: file_1, file_2, file_10, file_20)
        let (results, total, ..) = engine.execute_search_multi(
            "",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total, 4);
        let names: Vec<String> = engine.with_volumes(|vols| {
            results
                .iter()
                .map(|r| {
                    let vol = &vols[&r.drive_letter];
                    vol.record_name(&vol.records[r.record_idx as usize])
                        .to_string()
                })
                .collect()
        });
        assert_eq!(
            names,
            ["file_1.txt", "file_2.txt", "file_10.txt", "file_20.txt"]
        );

        // 2. Sort by Name descending
        let (results_desc, ..) = engine.execute_search_multi(
            "",
            &QueryDefaults::default(),
            SortColumn::Name,
            false,
            &cancel,
        );
        let names_desc: Vec<String> = engine.with_volumes(|vols| {
            results_desc
                .iter()
                .map(|r| {
                    let vol = &vols[&r.drive_letter];
                    vol.record_name(&vol.records[r.record_idx as usize])
                        .to_string()
                })
                .collect()
        });
        assert_eq!(
            names_desc,
            ["file_20.txt", "file_10.txt", "file_2.txt", "file_1.txt"]
        );

        // 3. Sort by Size ascending (10, 50, 100, 200)
        let (results_size, ..) = engine.execute_search_multi(
            "",
            &QueryDefaults::default(),
            SortColumn::Size,
            true,
            &cancel,
        );
        let sizes: Vec<u64> = engine.with_volumes(|vols| {
            results_size
                .iter()
                .map(|r| {
                    let vol = &vols[&r.drive_letter];
                    vol.records[r.record_idx as usize].size
                })
                .collect()
        });
        assert_eq!(sizes, [10, 50, 100, 200]);

        // 4. Sort by DateModified ascending (100, 200, 300, 400)
        let (results_mod, ..) = engine.execute_search_multi(
            "",
            &QueryDefaults::default(),
            SortColumn::DateModified,
            true,
            &cancel,
        );
        let dates: Vec<i64> = engine.with_volumes(|vols| {
            results_mod
                .iter()
                .map(|r| {
                    let vol = &vols[&r.drive_letter];
                    vol.records[r.record_idx as usize].modified_ts
                })
                .collect()
        });
        assert_eq!(dates, [100, 200, 300, 400]);

        // 5. Sort by Extension ascending with mixed case
        let mut vol_e = VolumeIndex::new('E');
        vol_e.ingest_items(vec![
            ExtractedItem {
                frn: 10,
                parent_frn: 5,
                name: "notes.TXT".to_string(),
                is_dir: false,
                size: 1,
                created_ts: 1,
                modified_ts: 1,
                flags: 0,
                is_reparse_point: false,
            },
            ExtractedItem {
                frn: 11,
                parent_frn: 5,
                name: "archive.zip".to_string(),
                is_dir: false,
                size: 1,
                created_ts: 1,
                modified_ts: 1,
                flags: 0,
                is_reparse_point: false,
            },
            ExtractedItem {
                frn: 12,
                parent_frn: 5,
                name: "doc.bin".to_string(),
                is_dir: false,
                size: 1,
                created_ts: 1,
                modified_ts: 1,
                flags: 0,
                is_reparse_point: false,
            },
        ]);
        engine.add_volume(vol_e);
        let (results_ext, ..) = engine.execute_search_multi(
            "",
            &QueryDefaults::default(),
            SortColumn::Extension,
            true,
            &cancel,
        );
        let exts: Vec<String> = engine.with_volumes(|vols| {
            results_ext
                .iter()
                .map(|r| {
                    let vol = &vols[&r.drive_letter];
                    let rec = &vol.records[r.record_idx as usize];
                    vol.arena
                        .get_ext(rec.name_offset, rec.name_len, rec.ext_offset)
                        .to_ascii_lowercase()
                })
                .collect()
        });
        assert_eq!(exts, ["bin", "txt", "txt", "txt", "txt", "txt", "zip"]);
    }
}
