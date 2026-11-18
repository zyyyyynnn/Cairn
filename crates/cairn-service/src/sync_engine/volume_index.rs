// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Indexed record table of one volume: ingestion, journal changes, folder totals and compaction.

use super::index_journal::IndexJournal;
use super::volume_status::VolumeStatus;
use crate::persistence::{headroom_for_arena, headroom_for_records, LoadedIndexData};
use cairn_core::arena::NamePool;
use cairn_core::frn_index::{FrnLookup, IndexMemoryFootprint};
use cairn_core::model::{
    flags_from_win32_attributes, FileRecord, FLAG_DIRECTORY, FLAG_INDEXED_ATTRIBUTES_MASK,
};
use cairn_core::path_cache::{root_path_for_drive, DirectoryPathCache};
use cairn_core::property_store::{PropertySideTables, RecordPropertyData, SourceId};
use cairn_core::query::SourceIndex;
use cairn_core::sort::{PresortedIndices, SortColumn};
use cairn_core::DirectorySizeIndex;
use cairn_ntfs::mft::ExtractedItem;
use cairn_ntfs::usn::{file_record_flags, UsnChange};
use cairn_ntfs::volume_identity::VolumeIdentity;
use cairn_protocol::message::FileRowData;
use cairn_protocol::status::{
    JournalChangeKind, SourceKind, SourceStatusSummary, VolumeState, VolumeStatusSummary,
};
use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::{Arc, Mutex};

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

    /// Grows the set so `bits` are addressable; existing bits are untouched.
    pub(super) fn ensure_capacity(&mut self, bits: usize) {
        let words = bits.div_ceil(64);
        if self.words.len() < words {
            self.words.resize(words, 0);
        }
    }

    /// Unions `other` into `self`. Both sets describe the same record table, so their word
    /// vectors have equal length.
    pub(super) fn merge(&mut self, other: &Self) {
        for (word, &incoming) in self.words.iter_mut().zip(&other.words) {
            *word |= incoming;
        }
    }
}

/// The per-volume segment-exclusion state table, keyed by the rules and the path epoch.
struct ExclusionStatesCache {
    segments: Vec<String>,
    epoch: u64,
    states: Arc<Vec<u8>>,
}

/// A smaller volume is rebuilt quickly enough that compaction on every start would cost more than
/// it returns.
const COMPACT_MIN_RECORDS: usize = 100_000;

/// Compaction only pays off once at least this percentage of the table is dead weight.
const COMPACT_MIN_DROPPABLE_PERCENT: usize = 25;

/// Compaction also reclaims arena rename/delete bloat once dead name bytes pass this share.
const COMPACT_MIN_ARENA_WASTE_PERCENT: usize = 25;

/// Policy governing whether a volume index should be compacted to drop dead records and arena slack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompactPolicy {
    pub min_records: usize,
    pub min_droppable_percent: usize,
    pub min_arena_waste_percent: usize,
}

impl CompactPolicy {
    pub const PRODUCTION: Self = Self {
        min_records: COMPACT_MIN_RECORDS,
        min_droppable_percent: COMPACT_MIN_DROPPABLE_PERCENT,
        min_arena_waste_percent: COMPACT_MIN_ARENA_WASTE_PERCENT,
    };
}

/// Summary returned when [`VolumeIndex::compact_with_policy`] rewrites the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompactionReport {
    pub dropped_records: usize,
    pub reclaimed_arena_bytes: usize,
}

/// Incremental metadata refresh outcome applied inside [`VolumeIndex::apply_refreshed_metadata`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefreshedRecordMetadata {
    pub index: u32,
    pub frn: u64,
    pub size: u64,
    pub created_ts: i64,
    pub modified_ts: i64,
    pub win32_attributes: u32,
}

/// Compaction verdict for a record's ancestor chain. A tombstone is not part of it: a dead parent
/// does not make a live child service data, but a tombstoned directory can still sit below an
/// excluded root, so the walk has to continue through it.
const SUBTREE_UNKNOWN: u8 = 0;
const SUBTREE_KEPT: u8 = 1;
pub(super) const SUBTREE_EXCLUDED: u8 = 2;
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

/// Borrowed table slice unifying `VolumeIndex` and `FolderSource` row serialization.
pub(crate) struct SourceTableView<'a> {
    pub root_prefix: Arc<str>,
    pub records: &'a [FileRecord],
    pub arena: &'a NamePool,
    pub path_cache: &'a DirectoryPathCache,
    pub generation: u64,
}

impl<'a> SourceTableView<'a> {
    pub(crate) fn build_file_row<F, P>(
        &self,
        record_idx: u32,
        folded_terms: &[Vec<u8>],
        snippet_lookup: F,
        property_lookup: P,
    ) -> Option<FileRowData>
    where
        F: FnOnce(u64) -> Vec<String>,
        P: FnOnce(u64, i64) -> Option<RecordPropertyData>,
    {
        let rec = self.records.get(record_idx as usize)?;
        let name = self.arena.get_name(rec.name_offset, rec.name_len);
        let parent_path = self.path_cache.get_parent_path_with_root(
            self.records,
            self.arena,
            &self.root_prefix,
            record_idx,
        );

        let mut name_highlights = Vec::new();
        let mut path_highlights = Vec::new();

        if !folded_terms.is_empty() {
            let folded_name = self.arena.get_folded(rec.name_offset, rec.name_len);
            collect_term_highlights(folded_name, folded_terms, &mut name_highlights);

            let mut folded_path = parent_path.as_bytes().to_vec();
            folded_path.make_ascii_lowercase();
            collect_term_highlights(&folded_path, folded_terms, &mut path_highlights);
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
            snippets: snippet_lookup(rec.frn),
            properties: property_lookup(rec.frn, rec.modified_ts),
        })
    }
}

struct PresortedCache {
    indices: PresortedIndices,
    column: Option<SortColumn>,
    pending: BitSet,
    removed: BitSet,
    delta: Vec<u32>,
    path_stale: bool,
    version: u64,
}

impl Default for PresortedCache {
    fn default() -> Self {
        Self {
            indices: PresortedIndices::default(),
            column: None,
            pending: BitSet::with_capacity(0),
            removed: BitSet::with_capacity(0),
            delta: Vec::new(),
            path_stale: false,
            version: 0,
        }
    }
}

impl PresortedCache {
    fn supports_column(col: SortColumn) -> bool {
        matches!(
            col,
            SortColumn::Name | SortColumn::Size | SortColumn::DateModified | SortColumn::Path
        )
    }

    fn reset(&mut self, increment_version: bool) {
        self.indices.clear_and_shrink();
        self.column = None;
        self.clear_tracking();
        if increment_version {
            self.version = self.version.wrapping_add(1);
        }
    }

    fn clear_tracking(&mut self) {
        self.pending = BitSet::with_capacity(0);
        self.removed = BitSet::with_capacity(0);
        self.delta.clear();
        self.path_stale = false;
    }

    fn bump_version(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    fn column_slice(&self, col: SortColumn) -> Option<&[u32]> {
        let slice = match col {
            SortColumn::Name => &self.indices.by_name,
            SortColumn::Size => &self.indices.by_size,
            SortColumn::DateModified => &self.indices.by_date_modified,
            SortColumn::Path => &self.indices.by_path,
            _ => return None,
        };
        (!slice.is_empty()).then_some(slice.as_slice())
    }

    fn order_snapshot(&self, sort_by: SortColumn) -> Option<&[u32]> {
        if self.column != Some(sort_by) {
            return None;
        }
        self.column_slice(sort_by)
    }

    fn order(&self, sort_by: SortColumn) -> Option<&[u32]> {
        if self.column != Some(sort_by) || !self.delta.is_empty() {
            return None;
        }
        if sort_by == SortColumn::Path && self.path_stale {
            return None;
        }
        self.column_slice(sort_by)
    }

    fn needs_column(&self, record_count: usize, col: SortColumn) -> bool {
        if record_count == 0 || !Self::supports_column(col) {
            return false;
        }
        self.column != Some(col)
            || !self.delta.is_empty()
            || (col == SortColumn::Path && self.path_stale)
    }

    fn mark_appended(&mut self, record_count: usize, indices: impl IntoIterator<Item = u32>) {
        if self.column.is_none() {
            return;
        }
        self.pending.ensure_capacity(record_count);
        for index in indices {
            if self.pending.insert(index as usize) {
                self.delta.push(index);
            }
        }
    }

    fn mark_repositioned(
        &mut self,
        record_count: usize,
        indices: impl IntoIterator<Item = u32>,
        names: bool,
        sizes: bool,
        dates: bool,
    ) {
        let affects = match self.column {
            Some(SortColumn::Name) | Some(SortColumn::Path) => names,
            Some(SortColumn::Size) => sizes,
            Some(SortColumn::DateModified) => dates,
            _ => false,
        };
        if !affects {
            return;
        }
        self.pending.ensure_capacity(record_count);
        self.removed.ensure_capacity(record_count);
        for index in indices {
            if self.pending.insert(index as usize) {
                self.delta.push(index);
            }
            self.removed.insert(index as usize);
        }
    }

    fn mark_path_stale(&mut self) {
        self.path_stale = true;
        self.bump_version();
    }

    fn ensure_column(
        &mut self,
        col: SortColumn,
        records: &[FileRecord],
        arena: &NamePool,
        path_cache: &DirectoryPathCache,
        drive_letter: char,
    ) {
        if records.is_empty() || !Self::supports_column(col) {
            return;
        }
        let built = self.column == Some(col) && self.column_slice(col).is_some();
        let path_stale = col == SortColumn::Path && self.path_stale;
        if built && self.delta.is_empty() && !path_stale {
            return;
        }
        let rebuild = !built || path_stale || self.delta.len().saturating_mul(4) > records.len();
        if rebuild {
            self.indices.clear_and_shrink();
            self.indices
                .ensure_column(col, records, arena, path_cache, drive_letter);
            self.column = Some(col);
            self.clear_tracking();
            self.bump_version();
            return;
        }

        let mut changed = std::mem::take(&mut self.delta);
        let root = root_path_for_drive(drive_letter);
        let sort_ctx = cairn_core::SortContext::for_root(records, arena, path_cache, &root);
        let removed = &self.removed;
        self.indices.merge_changed(
            col,
            &mut changed,
            |i| removed.contains(i as usize),
            &sort_ctx,
        );
        self.pending = BitSet::with_capacity(0);
        self.removed = BitSet::with_capacity(0);
        self.bump_version();
    }
}

pub struct VolumeIndex {
    pub drive_letter: char,
    pub volume_guid: String,
    pub(super) status: VolumeStatus,
    records: Vec<FileRecord>,
    arena: NamePool,
    path_cache: DirectoryPathCache,
    presorted: PresortedCache,
    frn_to_idx: FrnLookup,
    pending_renames: HashMap<u64, (u64, String)>,
    pending_data_changes: HashSet<u64>,
    pending_content_deletions: HashSet<(u64, u64)>,
    /// FRNs of service-owned directories and everything indexed below them. They are filtered out
    /// of scans, journal events and loaded caches so the index never describes its own writes.
    excluded_frns: HashSet<u64>,
    pub last_usn: i64,
    pub journal_id: u64,
    pub volume_serial: u64,
    pub usn_enabled: bool,
    /// Monotonic version of the record table. Every operation that rebuilds or reorders the
    /// table (`clear`, `ingest_items`, compaction) increments it, so a query session can refuse
    /// record indices that no longer describe the same rows.
    generation: u64,
    /// Live records (tombstones excluded), maintained incrementally so status summaries stay O(1).
    live_count: u64,
    /// Sum of live file sizes. The all-files view reports it without scanning the order.
    live_file_bytes: u64,
    /// Whether `FileRecord.size` on directory rows is the recursive aggregate of their indexed
    /// descendants. A cache written before folder sizes existed cannot prove this, so a load
    /// aggregates once and rewrites the flag; every later update maintains the totals in place.
    directory_sizes_valid: bool,
    /// Bumped when a rename can change an exclusion verdict or the table is replaced, so the
    /// cached exclusion states rebuild.
    exclusion_epoch: u64,
    /// Whether the active rule set has forms the segment table cannot answer, so every rename
    /// bumps `exclusion_epoch` to invalidate open views.
    full_path_exclusion_rules: bool,
    /// Segment-exclusion states are the same for every query until a path changes; rebuilding
    /// the 1.5M-entry table per query would cost more than the search itself.
    exclusion_cache: Mutex<Option<ExclusionStatesCache>>,
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
            presorted: PresortedCache::default(),
            frn_to_idx: FrnLookup::new(),
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
            live_file_bytes: 0,
            // An empty table has no totals to be wrong about; the invariant holds trivially.
            directory_sizes_valid: true,
            exclusion_epoch: 0,
            full_path_exclusion_rules: false,
            exclusion_cache: Mutex::new(None),
        }
    }

    /// Clears all records and indexes for this volume. The excluded service data roots survive:
    /// they are configuration derived from the data layout, not index data, so a rebuild must keep
    /// filtering the same subtree.
    pub fn clear(&mut self) {
        self.records = Vec::new();
        self.live_count = 0;
        self.live_file_bytes = 0;
        self.arena.clear_and_shrink();
        self.path_cache.clear();
        self.presorted.reset(false);
        self.frn_to_idx.clear_and_shrink();
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
        self.exclusion_epoch = self.exclusion_epoch.wrapping_add(1);
        self.exclusion_cache = Mutex::new(None);
    }

    /// Populates this volume from persisted index data, building a compact FRN map without
    /// eagerly constructing any presorted index columns.
    pub fn restore_from_loaded(&mut self, loaded: LoadedIndexData) {
        self.path_cache.clear();
        self.presorted.reset(false);
        self.pending_renames.clear();
        self.pending_data_changes.clear();
        self.pending_content_deletions.clear();

        self.records = loaded.records;
        self.arena = loaded.arena;
        self.last_usn = loaded.last_usn;
        self.journal_id = loaded.journal_id;
        self.volume_serial = loaded.volume_serial;
        self.directory_sizes_valid = loaded.directory_sizes_valid;

        let mut live_count = 0u64;
        let mut live_file_bytes = 0u64;
        let mut frn_to_idx = FrnLookup::with_capacity(self.records.len());
        for (idx, record) in self.records.iter().enumerate() {
            if !record.is_tombstone() {
                live_count = live_count.saturating_add(1);
                if record.is_file() {
                    live_file_bytes = live_file_bytes.saturating_add(record.size);
                }
                frn_to_idx.push(record.frn, idx as u32);
            }
        }
        self.live_count = live_count;
        self.live_file_bytes = live_file_bytes;
        self.frn_to_idx = frn_to_idx;
        self.generation = self.generation.wrapping_add(1);
        self.exclusion_epoch = self.exclusion_epoch.wrapping_add(1);
        self.exclusion_cache = Mutex::new(None);
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

    pub(crate) fn snapshot(&self) -> crate::persistence::VolumeSnapshot<'_> {
        crate::persistence::VolumeSnapshot {
            drive_letter: self.drive_letter,
            volume_serial: self.volume_serial,
            journal_id: self.journal_id,
            last_usn: self.last_usn,
            records: &self.records,
            arena: &self.arena,
            directory_sizes_valid: self.directory_sizes_valid,
        }
    }

    /// Atomically persists volume records and name pool to the specified directory.
    pub fn save_to_dir(
        &self,
        dir: &std::path::Path,
    ) -> Result<std::path::PathBuf, crate::persistence::PersistenceError> {
        let saved = crate::persistence::save_volume_index_atomic(dir, &self.snapshot())?;
        if !self.volume_guid.is_empty() && !self.volume_guid.starts_with("VOL_") {
            crate::persistence::save_volume_guid_sidecar(
                dir,
                self.drive_letter,
                &self.volume_guid,
            )?;
        }
        Ok(saved)
    }

    /// Read-only slice of indexed records.
    pub fn records(&self) -> &[FileRecord] {
        &self.records
    }

    /// Retrieves the original name of a record.
    pub fn record_name(&self, rec: &FileRecord) -> &str {
        self.arena.get_name(rec.name_offset, rec.name_len)
    }

    pub fn arena(&self) -> &NamePool {
        &self.arena
    }

    pub fn path_cache(&self) -> &DirectoryPathCache {
        &self.path_cache
    }

    pub fn clear_path_cache(&self) {
        self.path_cache.clear();
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn set_generation(&mut self, generation: u64) {
        self.generation = generation;
    }

    pub fn live_count(&self) -> u64 {
        self.live_count
    }

    /// Sum of the live files' own bytes; directories contribute through their contents only.
    pub fn live_file_bytes(&self) -> u64 {
        self.live_file_bytes
    }

    pub fn directory_sizes_valid(&self) -> bool {
        self.directory_sizes_valid
    }

    #[inline]
    pub fn record_indices_for_frn(&self, frn: u64) -> Option<&[u32]> {
        self.frn_to_idx.get(frn)
    }

    #[inline]
    pub fn first_record_index_for_frn(&self, frn: u64) -> Option<u32> {
        self.frn_to_idx.first(frn)
    }

    #[inline]
    pub fn contains_frn(&self, frn: u64) -> bool {
        self.frn_to_idx.contains_key(frn)
    }

    pub fn is_excluded_frn(&self, frn: u64) -> bool {
        self.excluded_frns.contains(&frn)
    }

    pub fn has_pending_data_change(&self, frn: u64) -> bool {
        self.pending_data_changes.contains(&frn)
    }

    pub fn has_pending_rename(&self, frn: u64) -> bool {
        self.pending_renames.contains_key(&frn)
    }

    pub fn has_pending_content_deletion(&self, volume_id: u64, frn: u64) -> bool {
        self.pending_content_deletions.contains(&(volume_id, frn))
    }

    /// Whether the cached order holds records waiting to be merged. An unbuilt column is not
    /// dirty: it simply has not been requested yet.
    pub fn is_presorted_dirty(&self) -> bool {
        !self.presorted.delta.is_empty()
    }

    /// Whether this volume needs work before `col` can answer an ordered read. False means
    /// either the column is current, the table is empty, or `col` has no presorted array at
    /// all (created time, extension); callers still read through `presorted_order`.
    pub fn needs_presorted_column(&self, col: SortColumn) -> bool {
        self.presorted.needs_column(self.records.len(), col)
    }

    /// Invalidates every column: the table itself changed (reconciliation, compaction).
    pub fn mark_presorted_dirty(&mut self) {
        self.presorted.reset(true);
    }

    /// Records appended since the last build. With no cached column the next build is full
    /// anyway, so the indices would only cost memory.
    pub fn mark_presorted_appended(&mut self, indices: impl IntoIterator<Item = u32>) {
        self.presorted.mark_appended(self.records.len(), indices);
    }

    /// Records renamed or moved. Name and path columns move with the new name; size and
    /// modification columns move too because the name is their tiebreak.
    pub fn mark_presorted_names_changed(&mut self, indices: impl IntoIterator<Item = u32>) {
        self.presorted
            .mark_repositioned(self.records.len(), indices, true, true, true);
    }

    /// Records whose size key changed.
    pub fn mark_presorted_sizes_changed(&mut self, indices: impl IntoIterator<Item = u32>) {
        self.presorted
            .mark_repositioned(self.records.len(), indices, false, true, false);
    }

    /// Records whose modification time changed.
    pub fn mark_presorted_dates_changed(&mut self, indices: impl IntoIterator<Item = u32>) {
        self.presorted
            .mark_repositioned(self.records.len(), indices, false, false, true);
    }

    /// A directory rename or move invalidates the path order for its whole subtree.
    pub fn mark_presorted_path_stale(&mut self) {
        self.presorted.mark_path_stale();
    }

    pub fn has_presorted_column(&self, col: SortColumn) -> bool {
        self.presorted.column == Some(col) && self.presorted.order(col).is_some()
    }

    /// The cached order as an open view captured it: pending merges belong to future reads,
    /// and the view's slots still describe its own snapshot.
    pub fn presorted_order_snapshot(&self, sort_by: SortColumn) -> Option<&[u32]> {
        self.presorted.order_snapshot(sort_by)
    }

    pub fn presorted_order(&self, sort_by: SortColumn) -> Option<&[u32]> {
        self.presorted.order(sort_by)
    }

    /// Ensures a specific presorted column is up-to-date.
    pub fn ensure_presorted_column(&mut self, col: SortColumn) {
        self.presorted.ensure_column(
            col,
            &self.records,
            &self.arena,
            &self.path_cache,
            self.drive_letter,
        );
    }

    /// Version of the current presorted order; changes whenever the order is rebuilt or merged.
    pub fn presorted_version(&self) -> u64 {
        self.presorted.version
    }

    /// Full heap-allocation ledger of this volume's resident structures.
    pub fn memory_footprint(&self) -> IndexMemoryFootprint {
        let (arena_bytes, folded_arena_bytes) = self.arena.allocated_bytes();
        IndexMemoryFootprint {
            record_count: self.records.len(),
            live_count: self.live_count as usize,
            records_bytes: self.records.capacity() * size_of::<FileRecord>(),
            arena_bytes,
            folded_arena_bytes,
            frn_lookup_bytes: self.frn_to_idx.allocated_bytes(),
            presorted_bytes: self.presorted.indices.allocated_bytes(),
            property_store_bytes: 0,
            directory_sizes_bytes: 0,
            path_cache_bytes: self.path_cache.allocated_bytes(),
            scratch_bytes: 0,
        }
    }

    fn push_record_bounded(&mut self, record: FileRecord) {
        if self.records.len() == self.records.capacity() && self.records.len() >= 1_024 {
            let step = (self.records.len() / 16).max(64);
            self.records.reserve_exact(step);
        }
        self.records.push(record);
    }

    pub fn apply_refreshed_metadata(&mut self, update: RefreshedRecordMetadata) {
        let Some(record) = self.records.get_mut(update.index as usize) else {
            return;
        };
        if record.frn != update.frn || record.is_tombstone() {
            return;
        }
        let was_dir = record.is_dir();
        let old_size = record.size;
        let refreshed_flags = flags_from_win32_attributes(update.win32_attributes);
        let is_dir = (refreshed_flags & FLAG_DIRECTORY) != 0;
        let flags = (record.flags & !FLAG_INDEXED_ATTRIBUTES_MASK) | refreshed_flags;
        let modified_changed = record.modified_ts != update.modified_ts;
        record.flags = flags;
        record.created_ts = update.created_ts;
        record.modified_ts = update.modified_ts;
        let kind_changed = was_dir != is_dir;
        if kind_changed {
            record.size = if is_dir { 0 } else { update.size };
            self.live_file_bytes = if is_dir {
                self.live_file_bytes.saturating_sub(old_size)
            } else {
                self.live_file_bytes.saturating_add(update.size)
            };
        }
        if modified_changed {
            self.mark_presorted_dates_changed(std::iter::once(update.index));
        }
        if kind_changed {
            self.mark_presorted_sizes_changed(std::iter::once(update.index));
            self.mark_directory_sizes_dirty();
        } else if !is_dir {
            self.update_file_size(update.index, update.size);
        }
    }

    /// Reconstructs full row data and highlights for a matched record index, encapsulating
    /// arena string extraction and path cache traversal.
    pub(crate) fn table_view(&self) -> SourceTableView<'_> {
        SourceTableView {
            root_prefix: root_path_for_drive(self.drive_letter),
            records: &self.records,
            arena: &self.arena,
            path_cache: &self.path_cache,
            generation: self.generation,
        }
    }

    /// Ingests parsed extracted items (from MFT scan or test fixtures).
    pub fn ingest_items(&mut self, items: Vec<ExtractedItem>) {
        self.clear();
        self.directory_sizes_valid = false;
        self.append_items(items);
        self.recompute_directory_sizes();

        // Keep presorted arrays empty until an all-files query actually requests a column.
        self.presorted.reset(false);
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
        let name_bytes: usize = items
            .iter()
            .map(|item| item.name.len().min(u16::MAX as usize))
            .sum();
        if self.records.is_empty() {
            self.records.reserve_exact(
                items
                    .len()
                    .saturating_add(headroom_for_records(items.len())),
            );
            self.arena
                .reserve(name_bytes.saturating_add(headroom_for_arena(name_bytes)));
        } else {
            self.records.reserve(items.len());
            self.arena.reserve(name_bytes);
        }
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
            if self.records[idx as usize].is_file() {
                self.live_file_bytes = self.live_file_bytes.saturating_add(item.size);
            }

            self.frn_to_idx.push(item.frn, idx);
            parent_frns.push(item.parent_frn);
        }

        // Link after the whole batch is inserted, so an MFT scan whose children precede their
        // parents still resolves. A parent from an earlier batch is found in the same map.
        for (offset, &parent_frn) in parent_frns.iter().enumerate() {
            if let Some(parent_idx) = self.frn_to_idx.first(parent_frn) {
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

        self.mark_presorted_appended((first_new..self.records.len()).map(|index| index as u32));
    }

    /// Aggregates the indexed descendants of every directory into `FileRecord.size`. The record's
    /// existing `size` field already costs 8 bytes per entry, so folder sizes need no second
    /// resident structure; files keep their own size and tombstones are ignored.
    pub(crate) fn recompute_directory_sizes(&mut self) {
        DirectorySizeIndex::apply_directory_sizes(&mut self.records, &self.arena);
        self.directory_sizes_valid = true;
        let directories: Vec<u32> = self
            .records
            .iter()
            .enumerate()
            .filter(|(_, record)| record.is_dir() && !record.is_tombstone())
            .map(|(index, _)| index as u32)
            .collect();
        self.mark_presorted_sizes_changed(directories);
    }

    /// Flags the totals as stale without paying for a rebuild per event. Callers that can change a
    /// record's kind in a batch mark here and run [`Self::ensure_directory_sizes`] once at the end.
    pub(crate) fn mark_directory_sizes_dirty(&mut self) {
        self.directory_sizes_valid = false;
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
        let tracks_sizes = self.presorted.column == Some(SortColumn::Size);
        let mut changed: Vec<u32> = Vec::new();
        let mut steps = 0usize;
        while parent_idx != FileRecord::ROOT_PARENT && (parent_idx as usize) < self.records.len() {
            let record = &mut self.records[parent_idx as usize];
            if record.is_tombstone() {
                break;
            }
            record.size = apply_size_delta(record.size, delta);
            if tracks_sizes {
                changed.push(parent_idx);
            }
            if record.parent_idx == parent_idx {
                break;
            }
            parent_idx = record.parent_idx;
            steps += 1;
            if steps > self.records.len() {
                break;
            }
        }
        if !changed.is_empty() {
            self.mark_presorted_sizes_changed(changed);
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
        self.live_file_bytes = if delta >= 0 {
            self.live_file_bytes.saturating_add(delta as u64)
        } else {
            self.live_file_bytes.saturating_sub((-delta) as u64)
        };
        self.adjust_record_ancestors(index, delta);
        self.mark_presorted_sizes_changed(std::iter::once(index));
    }

    /// `true` when a live record for this `(frn, parent, name)` triple is already indexed.
    fn has_live_record_at(&self, frn: u64, parent_frn: u64, name: &str) -> bool {
        self.frn_to_idx.get(frn).is_some_and(|indices| {
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

    /// Applies a journal batch, aggregating folder sizes at most once for the whole batch. Live
    /// polling and cold-start replay use this so `k` kind changes cost one rebuild, not `k`.
    pub fn apply_usn_changes(&mut self, changes: impl IntoIterator<Item = UsnChange>) {
        self.apply_usn_changes_with_journal(changes, None, 0);
    }

    /// Applies a journal batch and optionally appends normalized events to [`IndexJournal`].
    pub fn apply_usn_changes_with_journal(
        &mut self,
        changes: impl IntoIterator<Item = UsnChange>,
        mut journal: Option<&mut IndexJournal>,
        now_unix: i64,
    ) {
        for change in changes {
            self.apply_usn_change_inner(change, journal.as_deref_mut(), now_unix);
        }
        self.ensure_directory_sizes();
    }

    fn record_journal_event(
        &self,
        journal: &mut Option<&mut IndexJournal>,
        frn: u64,
        kind: JournalChangeKind,
        path: String,
        old_path: Option<String>,
        now_unix: i64,
    ) {
        if let (Some(j), Some(sid)) = (
            journal.as_deref_mut(),
            SourceId::for_drive(self.drive_letter),
        ) {
            j.push(sid, frn, kind, path, old_path, now_unix);
        }
    }

    fn apply_usn_change_inner(
        &mut self,
        change: UsnChange,
        mut journal: Option<&mut IndexJournal>,
        now_unix: i64,
    ) {
        match change {
            UsnChange::Deleted { frn } => {
                self.pending_renames.remove(&frn);
                // The root disappeared: any later create under a recycled FRN has to be judged on
                // its own. A file that was never indexed has no row and no content document.
                if self.excluded_frns.remove(&frn) {
                    return;
                }
                let Some(indices) = self.frn_to_idx.get(frn) else {
                    return;
                };
                let deleted_path = journal.as_ref().map(|_| {
                    self.path_cache.get_full_path(
                        &self.records,
                        &self.arena,
                        self.drive_letter,
                        indices[0],
                    )
                });
                self.pending_content_deletions
                    .insert((self.content_volume_id(), frn));
                if indices.len() == 1 {
                    self.tombstone_record_at(indices[0]);
                } else {
                    let indices = indices.to_vec();
                    for idx in indices {
                        self.tombstone_record_at(idx);
                    }
                }
                if let Some(path) = deleted_path {
                    self.record_journal_event(
                        &mut journal,
                        frn,
                        JournalChangeKind::Deleted,
                        path,
                        None,
                        now_unix,
                    );
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
                    .first(parent_frn)
                    .unwrap_or(FileRecord::ROOT_PARENT);

                let new_idx = self.records.len() as u32;
                self.push_record_bounded(FileRecord {
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
                self.frn_to_idx.push(frn, new_idx);
                self.mark_presorted_appended(std::iter::once(new_idx));
                if journal.is_some() {
                    let path = self.path_cache.get_full_path(
                        &self.records,
                        &self.arena,
                        self.drive_letter,
                        new_idx,
                    );
                    self.record_journal_event(
                        &mut journal,
                        frn,
                        JournalChangeKind::Created,
                        path,
                        None,
                        now_unix,
                    );
                }
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
                    let target_idx = self.frn_to_idx.get(frn).and_then(|indices| {
                        if let Some((_old_parent, old_name)) = old_info {
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
                        }
                    });

                    if let Some(idx) = target_idx {
                        let old_path = journal.as_ref().map(|_| {
                            self.path_cache.get_full_path(
                                &self.records,
                                &self.arena,
                                self.drive_letter,
                                idx,
                            )
                        });
                        let (name_offset, name_len, ext_offset) = self.arena.insert(&name);
                        let parent_idx = self
                            .frn_to_idx
                            .first(parent_frn)
                            .unwrap_or(FileRecord::ROOT_PARENT);

                        let (old_parent, moved_size, old_name) = {
                            let rec = &self.records[idx as usize];
                            (
                                rec.parent_idx,
                                rec.size as i128,
                                self.arena
                                    .get_name(rec.name_offset, rec.name_len)
                                    .to_string(),
                            )
                        };
                        if old_parent != parent_idx && !self.records[idx as usize].is_tombstone() {
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
                        if let Some(old_path) = old_path {
                            let new_path = self.path_cache.get_full_path(
                                &self.records,
                                &self.arena,
                                self.drive_letter,
                                idx,
                            );
                            self.record_journal_event(
                                &mut journal,
                                frn,
                                JournalChangeKind::Renamed,
                                new_path,
                                Some(old_path),
                                now_unix,
                            );
                        }
                        // The content document stores the path; a rename has to replace it,
                        // and a rename out of the extension whitelist has to delete it.
                        self.pending_data_changes.insert(frn);
                        self.mark_presorted_names_changed(std::iter::once(idx));
                        if self.records[idx as usize].is_dir() {
                            self.mark_presorted_path_stale();
                        }
                        if self.full_path_exclusion_rules
                            || self.rename_changes_exclusion_states(
                                &old_name, &name, old_parent, parent_idx,
                            )
                        {
                            self.exclusion_epoch = self.exclusion_epoch.wrapping_add(1);
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
                if let Some(indices) = self.frn_to_idx.get(frn) {
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
                            if !was_dir {
                                self.live_file_bytes =
                                    self.live_file_bytes.saturating_sub(record.size);
                            }
                            record.size = 0;
                            kind_changed = true;
                        }
                    }
                }
                if let Some(indices) = self.frn_to_idx.get(frn) {
                    let affected: Vec<u32> = indices.to_vec();
                    let modified_idx = affected.first().copied();
                    self.mark_presorted_dates_changed(affected.iter().copied());
                    if kind_changed {
                        self.mark_presorted_sizes_changed(affected);
                    }
                    if journal.is_some() {
                        if let Some(idx) = modified_idx {
                            let path = self.path_cache.get_full_path(
                                &self.records,
                                &self.arena,
                                self.drive_letter,
                                idx,
                            );
                            self.record_journal_event(
                                &mut journal,
                                frn,
                                JournalChangeKind::AttributeChanged,
                                path,
                                None,
                                now_unix,
                            );
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
                if journal.is_some() {
                    if let Some(idx) = self.first_record_index_for_frn(frn) {
                        let path = self.path_cache.get_full_path(
                            &self.records,
                            &self.arena,
                            self.drive_letter,
                            idx,
                        );
                        self.record_journal_event(
                            &mut journal,
                            frn,
                            JournalChangeKind::Modified,
                            path,
                            None,
                            now_unix,
                        );
                    }
                }
                self.pending_data_changes.insert(frn);
            }
        }
    }

    fn tombstone_record_at(&mut self, idx: u32) {
        let rec = &self.records[idx as usize];
        if rec.is_tombstone() {
            return;
        }
        let parent_idx = rec.parent_idx;
        let size = rec.size as i128;
        let was_file = !rec.is_dir();
        // A descendant of an already deleted directory has already left the ancestor totals with
        // that directory's recursive size.
        if !self.has_tombstone_ancestor(parent_idx) {
            self.adjust_parent_sizes(parent_idx, -size);
        }
        self.live_count = self.live_count.saturating_sub(1);
        if was_file {
            self.live_file_bytes = self.live_file_bytes.saturating_sub(size as u64);
        }
        self.records[idx as usize].mark_tombstone();
        // A view that pages the live order counts visible rows; removing one shifts every
        // later offset, so the order's version moves even though the column itself is intact.
        self.presorted.bump_version();
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

    /// Precomputes which records the given single-segment rules remove.
    ///
    /// Rebuilding and normalizing a full path for every match just to test a handful of segment
    /// rules is O(records × path). For a bare directory/file segment such as `$Recycle.Bin` or
    /// `System Volume Information` the same verdict is a name comparison along the parent chain,
    /// so one memoized pass produces an O(1) lookup table. Rules with globs, embedded separators,
    /// or a drive-qualified path never reach this method; `PathExclusions` answers those from the
    /// parsed rule set against the full path.
    /// Per-record exclusion state table built from segment-only exclusion rules.
    ///
    /// Path exclusions are tested against every match after query evaluation. For a broad query
    /// returning most of the volume, calling `get_full_path` + glob evaluation against the parsed
    /// rules is O(records × path). For a bare directory/file segment such as `$Recycle.Bin` or
    /// `System Volume Information` the same verdict is a name comparison along the parent chain,
    /// so one memoized pass produces an O(1) lookup table. Rules with globs, embedded separators,
    /// or a drive-qualified path never reach this method; `PathExclusions` answers those from the
    /// parsed rule set against the full path.
    pub(crate) fn segment_excluded_states(&self, segments: &[String]) -> Vec<u8> {
        self.subtree_states_by(|_, record| {
            let name = self.arena.get_name(record.name_offset, record.name_len);
            segments
                .iter()
                .any(|segment| name.eq_ignore_ascii_case(segment))
        })
    }

    /// Version of the path-derived exclusion verdicts. It moves when a rename can change a
    /// verdict, on every rename while full-path rules are active, and when the table is
    /// replaced.
    pub fn exclusion_epoch(&self) -> u64 {
        self.exclusion_epoch
    }

    /// Records whether the active rules need the full path, so a rename may change a verdict
    /// without the segment table noticing.
    pub fn set_full_path_exclusion_rules(&mut self, active: bool) {
        self.full_path_exclusion_rules = active;
    }

    /// The segment-exclusion states, cached until a verdict can change. Appended records extend
    /// the cached table; a verdict-changing rename or a rebuilt table starts it over.
    pub(crate) fn segment_excluded_states_cached(&self, segments: &[String]) -> Arc<Vec<u8>> {
        let mut cache = self.exclusion_cache.lock().unwrap();
        let needs_rebuild = match cache.as_ref() {
            Some(entry) => {
                entry.epoch != self.exclusion_epoch
                    || entry.segments.as_slice() != segments
                    || entry.states.len() > self.records.len()
            }
            None => true,
        };
        if needs_rebuild {
            let states = Arc::new(self.segment_excluded_states(segments));
            *cache = Some(ExclusionStatesCache {
                segments: segments.to_vec(),
                epoch: self.exclusion_epoch,
                states: states.clone(),
            });
            return states;
        }

        let entry = cache.as_mut().expect("cache was checked");
        if entry.states.len() < self.records.len() {
            let mut states = (*entry.states).clone();
            self.extend_segment_states(&mut states, segments);
            entry.states = Arc::new(states);
        }
        entry.states.clone()
    }

    /// Whether a rename can change any record's exclusion verdict. An ordinary rename under
    /// kept parents keeps the cached table valid; only a name that matches a rule or a move
    /// across an excluded boundary can move records between verdicts.
    fn rename_changes_exclusion_states(
        &self,
        old_name: &str,
        new_name: &str,
        old_parent: u32,
        new_parent: u32,
    ) -> bool {
        let cache = self.exclusion_cache.lock().unwrap();
        let Some(entry) = cache.as_ref() else {
            return false;
        };
        let matches = |name: &str| {
            entry
                .segments
                .iter()
                .any(|segment| name.eq_ignore_ascii_case(segment))
        };
        if matches(old_name) || matches(new_name) {
            return true;
        }
        let state_of = |index: u32| {
            if index == FileRecord::ROOT_PARENT {
                // Records at the volume root sit under no named segment.
                Some(SUBTREE_KEPT)
            } else {
                entry.states.get(index as usize).copied()
            }
        };
        state_of(old_parent) != Some(SUBTREE_KEPT) || state_of(new_parent) != Some(SUBTREE_KEPT)
    }

    /// Classifies records appended since `states` was built. Ancestors below the old length
    /// already carry a final verdict, so the same walk reaches it without revisiting them.
    fn extend_segment_states(&self, states: &mut Vec<u8>, segments: &[String]) {
        let from = states.len();
        let count = self.records.len();
        if from >= count {
            return;
        }
        states.resize(count, SUBTREE_UNKNOWN);
        let mut stack: Vec<u32> = Vec::new();
        for start in from..count {
            if states[start] != SUBTREE_UNKNOWN {
                continue;
            }
            stack.clear();
            let mut current = start as u32;
            let verdict = loop {
                let idx = current as usize;
                match states[idx] {
                    SUBTREE_IN_PROGRESS | SUBTREE_KEPT => break SUBTREE_KEPT,
                    SUBTREE_EXCLUDED => break SUBTREE_EXCLUDED,
                    SUBTREE_UNKNOWN => {}
                    _ => unreachable!("record exclusion state"),
                }
                let record = &self.records[idx];
                let name = self.arena.get_name(record.name_offset, record.name_len);
                if segments
                    .iter()
                    .any(|segment| name.eq_ignore_ascii_case(segment))
                {
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
    }

    /// Rewrites the table without tombstones or excluded subtrees and repacks the name arena with
    /// controlled growth headroom.
    fn compact_with_states(
        &mut self,
        excluded: Vec<u8>,
        kept_name_bytes: usize,
    ) -> CompactionReport {
        let old_len = self.records.len();
        let old_arena_len = self.arena.len();
        let mut remap = vec![u32::MAX; old_len];
        let mut kept = 0usize;
        for (idx, &state) in excluded.iter().enumerate() {
            if state != SUBTREE_EXCLUDED && !self.records[idx].is_tombstone() {
                remap[idx] = kept as u32;
                kept += 1;
            }
        }

        let dropped_records = old_len - kept;
        let arena_waste = old_arena_len.saturating_sub(kept_name_bytes);
        if dropped_records == 0 && arena_waste == 0 {
            return CompactionReport {
                dropped_records: 0,
                reclaimed_arena_bytes: 0,
            };
        }

        let mut records = Vec::with_capacity(kept.saturating_add(headroom_for_records(kept)));
        let mut arena = NamePool::with_capacity(
            kept_name_bytes.saturating_add(headroom_for_arena(kept_name_bytes)),
        );
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

        let mut frn_to_idx = FrnLookup::with_capacity(records.len());
        for (idx, record) in records.iter().enumerate() {
            frn_to_idx.push(record.frn, idx as u32);
        }

        let new_arena_len = arena.len();
        self.records = records;
        self.live_count = self.records.len() as u64;
        self.live_file_bytes = self
            .records
            .iter()
            .filter(|record| record.is_file())
            .map(|record| record.size)
            .fold(0u64, u64::saturating_add);
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
        // session; one linear aggregation restores the invariant while leaving presorted indices
        // lazy until an all-files query requests a column.
        self.recompute_directory_sizes();
        self.presorted.reset(false);
        self.exclusion_epoch = self.exclusion_epoch.wrapping_add(1);
        self.exclusion_cache = Mutex::new(None);
        CompactionReport {
            dropped_records,
            reclaimed_arena_bytes: old_arena_len.saturating_sub(new_arena_len),
        }
    }

    /// Evaluates compaction against `policy` using the same classification and rewrite pipeline
    /// across production and tests. Compacts if either dead records or arena rename bloat exceeds
    /// the policy threshold.
    pub fn compact_with_policy(&mut self, policy: CompactPolicy) -> Option<CompactionReport> {
        let len = self.records.len();
        if len < policy.min_records {
            return None;
        }
        let states = self.excluded_subtree_states();
        let mut droppable = 0usize;
        let mut kept_name_bytes = 0usize;
        for (idx, &state) in states.iter().enumerate() {
            let rec = &self.records[idx];
            if state == SUBTREE_EXCLUDED || rec.is_tombstone() {
                droppable += 1;
            } else {
                kept_name_bytes += rec.name_len as usize;
            }
        }
        let records_over_limit = droppable * 100 >= len * policy.min_droppable_percent;
        let arena_len = self.arena.len();
        let arena_waste = arena_len.saturating_sub(kept_name_bytes);
        let arena_over_limit =
            arena_len > 0 && arena_waste * 100 >= arena_len * policy.min_arena_waste_percent;
        if !records_over_limit && !arena_over_limit {
            return None;
        }
        Some(self.compact_with_states(states, kept_name_bytes))
    }

    /// Compacts large volumes once dead weight or arena rename bloat crosses the production
    /// threshold.
    pub(crate) fn compact_if_needed(&mut self) -> bool {
        self.compact_with_policy(CompactPolicy::PRODUCTION)
            .is_some()
    }

    pub fn set_identity(&mut self, identity: VolumeIdentity) {
        self.volume_guid = identity.volume_guid;
        self.volume_serial = identity.volume_serial;
    }

    pub fn matches_volume_identity(&self, identity: &VolumeIdentity) -> bool {
        self.volume_guid.eq_ignore_ascii_case(&identity.volume_guid)
            && self.volume_serial == identity.volume_serial
    }

    pub fn has_tombstones(&self) -> bool {
        self.live_count < self.records.len() as u64
    }

    pub fn is_active(&self, excluded_drives: &[char]) -> bool {
        !excluded_drives
            .iter()
            .any(|drive| drive.eq_ignore_ascii_case(&self.drive_letter))
            && (self.status.state().is_synced() || !self.records.is_empty())
    }

    pub fn as_source_index<'a>(
        &'a self,
        root_prefix: &'a str,
        properties: &'a PropertySideTables,
    ) -> SourceIndex<'a> {
        let source_id =
            SourceId::for_drive(self.drive_letter).expect("NTFS drive letter maps to a SourceId");
        SourceIndex {
            source_id,
            root_prefix,
            records: &self.records,
            arena: &self.arena,
            path_cache: &self.path_cache,
            presorted: Some(&self.presorted.indices),
            properties,
            has_tombstones: self.has_tombstones(),
        }
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

    pub fn to_source_summary(&self) -> SourceStatusSummary {
        let source_id = SourceId::for_drive(self.drive_letter).unwrap_or(SourceId(0));
        SourceStatusSummary {
            source_id,
            kind: SourceKind::NtfsVolume,
            root_path: format!("{}:\\", self.drive_letter),
            volume_guid: self.volume_guid.clone(),
            status: self.status.state(),
            indexed_count: self.live_count,
            live_watch_enabled: self.usn_enabled,
        }
    }

    /// Sums the space the matched set describes at its highest hit point: a matched directory
    /// contributes its recursive size once, and a match already covered by a matched ancestor
    /// directory adds nothing. A folder-only hit therefore reports the folder's space without
    /// any file being counted twice.
    pub(crate) fn calculate_matched_bytes(&self, matched_indices: &[u32]) -> u64 {
        calculate_matched_bytes_for_records(&self.records, matched_indices)
    }
}

pub(crate) fn calculate_matched_bytes_for_records(
    records: &[FileRecord],
    matched_indices: &[u32],
) -> u64 {
    if matched_indices.is_empty() || records.is_empty() {
        return 0;
    }

    let mut matched_dirs = BitSet::with_capacity(records.len());
    let mut has_matched_dirs = false;
    for &idx in matched_indices {
        if let Some(record) = records.get(idx as usize) {
            if record.is_dir() && !record.is_tombstone() {
                matched_dirs.insert(idx as usize);
                has_matched_dirs = true;
            }
        }
    }

    let mut seen = BitSet::with_capacity(records.len());
    // Coverage is only consulted when a matched directory can cover a descendant; a
    // file-only match set sums its own sizes without the lookup table.
    let mut coverage = has_matched_dirs.then(|| vec![0u8; records.len()]);
    let mut path = Vec::new();
    let mut total: u64 = 0;

    for &idx in matched_indices {
        let index = idx as usize;
        if index >= records.len() || !seen.insert(index) {
            continue;
        }
        let record = &records[index];
        if record.is_tombstone() {
            continue;
        }
        // The volume root is its own parent; a record is never covered by itself.
        let covered = record.parent_idx != index as u32
            && coverage.as_mut().is_some_and(|coverage| {
                parent_is_covered_in_records(
                    records,
                    record.parent_idx,
                    &matched_dirs,
                    coverage,
                    &mut path,
                )
            });
        if !covered {
            total = total.saturating_add(record.size);
        }
    }

    total
}

/// Whether `parent` or one of its ancestors is a matched live directory.
fn parent_is_covered_in_records(
    records: &[FileRecord],
    mut parent: u32,
    matched_dirs: &BitSet,
    coverage: &mut [u8],
    path: &mut Vec<u32>,
) -> bool {
    path.clear();
    let mut steps = 0usize;
    let covered = loop {
        if parent == FileRecord::ROOT_PARENT || parent as usize >= records.len() {
            break false;
        }
        steps += 1;
        if steps > records.len() {
            break false;
        }
        match coverage[parent as usize] {
            1 => break true,
            2 => break false,
            _ => {}
        }
        if matched_dirs.contains(parent as usize) {
            break true;
        }
        path.push(parent);
        let record = &records[parent as usize];
        if record.parent_idx == parent {
            break false;
        }
        parent = record.parent_idx;
    };
    for &index in path.iter() {
        coverage[index as usize] = if covered { 1 } else { 2 };
    }
    covered
}

fn collect_term_highlights(
    haystack: &[u8],
    folded_terms: &[Vec<u8>],
    out: &mut Vec<(usize, usize)>,
) {
    for term in folded_terms {
        let term_bytes = term.as_slice();
        if term_bytes.is_empty() {
            continue;
        }
        let mut curr = 0;
        while let Some(pos) = memchr::memmem::find(&haystack[curr..], term_bytes) {
            let start = curr + pos;
            let end = start + term_bytes.len();
            out.push((start, end));
            curr = end;
        }
    }
    if out.len() > 1 {
        out.sort_unstable();
        out.dedup();
    }
}
