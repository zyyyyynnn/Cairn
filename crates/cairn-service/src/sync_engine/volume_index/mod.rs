// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

mod presort;
mod refresh;

use self::presort::PresortedCache;
use super::index_journal::IndexJournal;
use super::volume_status::VolumeStatus;
use crate::persistence::{LoadedIndexData, headroom_for_arena, headroom_for_records};
use cairn_core::DirectorySizeIndex;
use cairn_core::arena::NamePool;
use cairn_core::frn_index::{FrnLookup, IndexMemoryFootprint};
use cairn_core::model::{
    FLAG_DIRECTORY, FLAG_INDEXED_ATTRIBUTES_MASK, FileRecord, flags_from_win32_attributes,
};
use cairn_core::path_cache::{DirectoryPathCache, root_path_for_drive};
use cairn_core::property_store::{RecordPropertyData, SourceId};
use cairn_core::query::SourceView;
use cairn_core::sort::{PresortedIndices, SortColumn};
use cairn_ntfs::mft::ExtractedItem;
use cairn_ntfs::usn::{UsnChange, file_record_flags};
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

    #[inline(always)]
    pub(super) fn contains(&self, bit: usize) -> bool {
        self.words
            .get(bit / 64)
            .is_some_and(|word| word & (1u64 << (bit % 64)) != 0)
    }

    pub(super) fn ensure_capacity(&mut self, bits: usize) {
        let words = bits.div_ceil(64);
        if self.words.len() < words {
            self.words.resize(words, 0);
        }
    }

    pub(super) fn merge(&mut self, other: &Self) {
        for (word, &incoming) in self.words.iter_mut().zip(&other.words) {
            *word |= incoming;
        }
    }
}

struct ExclusionStatesCache {
    segments: Vec<String>,
    epoch: u64,
    states: Arc<Vec<u8>>,
}

const COMPACT_MIN_RECORDS: usize = 100_000;

const COMPACT_MIN_DROPPABLE_PERCENT: usize = 25;

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

const SUBTREE_UNKNOWN: u8 = 0;
const SUBTREE_KEPT: u8 = 1;
pub(super) const SUBTREE_EXCLUDED: u8 = 2;
const SUBTREE_IN_PROGRESS: u8 = 3;

fn apply_size_delta(value: u64, delta: i128) -> u64 {
    if delta >= 0 {
        value.saturating_add(delta as u64)
    } else {
        value.saturating_sub((-delta) as u64)
    }
}

pub(crate) fn build_file_row<F, P>(
    view: &SourceView<'_>,
    record_idx: u32,
    folded_terms: &[Vec<u8>],
    snippet_lookup: F,
    property_lookup: P,
) -> Option<FileRowData>
where
    F: FnOnce(u64) -> Vec<String>,
    P: FnOnce(u64, i64) -> Option<RecordPropertyData>,
{
    let rec = view.records.get(record_idx as usize)?;
    let name = view.arena.get_name(rec.name_offset, rec.name_len);
    let parent_path = view.path_cache.get_parent_path_with_root(
        view.records,
        view.arena,
        view.root_prefix,
        record_idx,
    );

    let mut name_highlights = Vec::new();
    let mut path_highlights = Vec::new();

    if !folded_terms.is_empty() {
        let folded_name = view.arena.get_folded(rec.name_offset, rec.name_len);
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

pub struct VolumeIndex {
    pub drive_letter: char,
    root_prefix: Arc<str>,
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
    excluded_frns: HashSet<u64>,
    pub last_usn: i64,
    pub journal_id: u64,
    pub volume_serial: u64,
    pub usn_enabled: bool,
    pub(super) usn_parse_failed: bool,
    generation: u64,
    live_count: u64,
    live_file_bytes: u64,
    directory_sizes_valid: bool,
    exclusion_epoch: u64,
    full_path_exclusion_rules: bool,
    exclusion_cache: Mutex<Option<ExclusionStatesCache>>,
}

impl VolumeIndex {
    pub fn new(drive_letter: char) -> Self {
        let drive_letter = drive_letter.to_ascii_uppercase();
        Self {
            drive_letter,
            root_prefix: root_path_for_drive(drive_letter),
            volume_guid: format!("VOL_{drive_letter}:"),
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
            usn_parse_failed: false,
            generation: 0,
            live_count: 0,
            live_file_bytes: 0,
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
        self.usn_parse_failed = false;
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
                Some(SUBTREE_KEPT)
            } else {
                entry.states.get(index as usize).copied()
            }
        };
        state_of(old_parent) != Some(SUBTREE_KEPT) || state_of(new_parent) != Some(SUBTREE_KEPT)
    }

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

    fn compact_with_states(&mut self, excluded: &[u8], kept_name_bytes: usize) -> CompactionReport {
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
        self.generation = self.generation.wrapping_add(1);
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
        Some(self.compact_with_states(&states, kept_name_bytes))
    }

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

    pub fn view(&self) -> SourceView<'_> {
        let source_id =
            SourceId::for_drive(self.drive_letter).expect("NTFS drive letter maps to a SourceId");
        SourceView {
            source_id,
            root_prefix: &self.root_prefix,
            records: &self.records,
            arena: &self.arena,
            path_cache: &self.path_cache,
            generation: self.generation,
        }
    }

    pub(crate) fn frn_lookup(&self) -> &FrnLookup {
        &self.frn_to_idx
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
        if let Some(record) = records.get(idx as usize)
            && record.is_dir()
            && !record.is_tombstone()
        {
            matched_dirs.insert(idx as usize);
            has_matched_dirs = true;
        }
    }

    let mut seen = BitSet::with_capacity(records.len());
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
