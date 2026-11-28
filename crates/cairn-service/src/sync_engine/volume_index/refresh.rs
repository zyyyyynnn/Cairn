// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use super::*;

impl VolumeIndex {
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

    /// Ingests parsed extracted items (from MFT scan or test fixtures).
    pub fn ingest_items(&mut self, items: Vec<ExtractedItem>) {
        self.clear();
        self.directory_sizes_valid = false;
        self.append_items(items);
        self.recompute_directory_sizes();

        self.presorted.reset(false);
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

        for (offset, &parent_frn) in parent_frns.iter().enumerate() {
            if let Some(parent_idx) = self.frn_to_idx.first(parent_frn) {
                self.records[first_new + offset].parent_idx = parent_idx;
            }
        }

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

    pub(crate) fn mark_directory_sizes_dirty(&mut self) {
        self.directory_sizes_valid = false;
    }

    pub(crate) fn ensure_directory_sizes(&mut self) {
        if !self.directory_sizes_valid {
            self.recompute_directory_sizes();
        }
    }

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

    fn has_live_record_at(&self, frn: u64, parent_frn: u64, name: &str) -> bool {
        self.frn_to_idx.get(frn).is_some_and(|indices| {
            indices.iter().copied().any(|idx| {
                let rec = &self.records[idx as usize];
                if rec.is_tombstone() || self.arena.get_name(rec.name_offset, rec.name_len) != name
                {
                    return false;
                }
                match rec.parent_idx {
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
                        // Directory and reparse bits are replaced from the event; other attribute
                        // bits are ORed in and re-synced from disk by the debounce pass. The
                        // tombstone bit always survives.
                        record.flags = (record.flags
                            & !(cairn_core::FLAG_DIRECTORY | cairn_core::FLAG_REPARSE_POINT))
                            | mapped;
                        if was_dir != record.is_dir() {
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
                    if journal.is_some()
                        && let Some(idx) = modified_idx
                    {
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
                if kind_changed {
                    self.mark_directory_sizes_dirty();
                }
                self.pending_data_changes.insert(frn);
            }
            UsnChange::DataChanged { frn } => {
                if self.excluded_frns.contains(&frn) {
                    return;
                }
                if journal.is_some()
                    && let Some(idx) = self.first_record_index_for_frn(frn)
                {
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
        if !self.has_tombstone_ancestor(parent_idx) {
            self.adjust_parent_sizes(parent_idx, -size);
        }
        self.live_count = self.live_count.saturating_sub(1);
        if was_file {
            self.live_file_bytes = self.live_file_bytes.saturating_sub(size as u64);
        }
        self.records[idx as usize].mark_tombstone();
        self.presorted.bump_version();
    }
}
