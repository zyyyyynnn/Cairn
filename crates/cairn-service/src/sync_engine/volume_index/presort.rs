// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use super::*;

pub(super) struct PresortedCache {
    pub(super) indices: PresortedIndices,
    pub(super) column: Option<SortColumn>,
    pub(super) pending: BitSet,
    pub(super) removed: BitSet,
    pub(super) delta: Vec<u32>,
    pub(super) path_stale: bool,
    pub(super) version: u64,
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
    pub(super) fn supports_column(col: SortColumn) -> bool {
        matches!(
            col,
            SortColumn::Name | SortColumn::Size | SortColumn::DateModified | SortColumn::Path
        )
    }

    pub(super) fn reset(&mut self, increment_version: bool) {
        self.indices.clear_and_shrink();
        self.column = None;
        self.clear_tracking();
        if increment_version {
            self.version = self.version.wrapping_add(1);
        }
    }

    pub(super) fn clear_tracking(&mut self) {
        self.pending = BitSet::with_capacity(0);
        self.removed = BitSet::with_capacity(0);
        self.delta.clear();
        self.path_stale = false;
    }

    pub(super) fn bump_version(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    pub(super) fn column_slice(&self, col: SortColumn) -> Option<&[u32]> {
        let slice = match col {
            SortColumn::Name => &self.indices.by_name,
            SortColumn::Size => &self.indices.by_size,
            SortColumn::DateModified => &self.indices.by_date_modified,
            SortColumn::Path => &self.indices.by_path,
            _ => return None,
        };
        (!slice.is_empty()).then_some(slice.as_slice())
    }

    pub(super) fn order_snapshot(&self, sort_by: SortColumn) -> Option<&[u32]> {
        if self.column != Some(sort_by) {
            return None;
        }
        self.column_slice(sort_by)
    }

    pub(super) fn order(&self, sort_by: SortColumn) -> Option<&[u32]> {
        if self.column != Some(sort_by) || !self.delta.is_empty() {
            return None;
        }
        if sort_by == SortColumn::Path && self.path_stale {
            return None;
        }
        self.column_slice(sort_by)
    }

    pub(super) fn needs_column(&self, record_count: usize, col: SortColumn) -> bool {
        if record_count == 0 || !Self::supports_column(col) {
            return false;
        }
        self.column != Some(col)
            || !self.delta.is_empty()
            || (col == SortColumn::Path && self.path_stale)
    }

    pub(super) fn mark_appended(
        &mut self,
        record_count: usize,
        indices: impl IntoIterator<Item = u32>,
    ) {
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

    pub(super) fn mark_repositioned(
        &mut self,
        record_count: usize,
        indices: impl IntoIterator<Item = u32>,
        names: bool,
        sizes: bool,
        dates: bool,
    ) {
        let affects = match self.column {
            Some(SortColumn::Name | SortColumn::Path) => names,
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

    pub(super) fn mark_path_stale(&mut self) {
        self.path_stale = true;
        self.bump_version();
    }

    pub(super) fn ensure_column(
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

impl VolumeIndex {
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
}
