// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Presorted index management and lazy rebuilding (FST-01, FST-02).

use super::{MultiVolumeEngine, VolumeIndex};
use cairn_core::sort::SortColumn;

impl VolumeIndex {
    /// Ensures presorted indices are up-to-date for the requested column, lazily rebuilding if dirty.
    pub fn ensure_presorted(&mut self) {
        self.ensure_presorted_column(SortColumn::Name);
    }

    /// Ensures a specific presorted column is up-to-date, lazily rebuilding if dirty.
    pub fn ensure_presorted_column(&mut self, col: SortColumn) {
        if self.presorted_dirty {
            self.presorted.clear();
            self.presorted_dirty = false;
        }
        self.presorted.ensure_column(
            col,
            &self.records,
            &self.arena,
            &self.path_cache,
            self.drive_letter,
        );
    }
}

impl MultiVolumeEngine {
    /// Ensures presorted indices across all volumes are up to date for the requested sort column.
    pub fn ensure_presorted_indices(&self, sort_by: SortColumn) {
        let needs_rebuild = {
            let read_lock = self.volumes.read().unwrap();
            read_lock.values().any(|v| {
                v.presorted_dirty
                    || match sort_by {
                        SortColumn::Name => v.presorted.by_name.is_empty(),
                        SortColumn::Size => v.presorted.by_size.is_empty(),
                        SortColumn::DateModified => v.presorted.by_date_modified.is_empty(),
                        SortColumn::Path => v.presorted.by_path.is_empty(),
                        _ => false,
                    }
            })
        };
        if needs_rebuild {
            let mut write_lock = self.volumes.write().unwrap();
            for vol in write_lock.values_mut() {
                vol.ensure_presorted_column(sort_by);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::sync_engine::{MultiVolumeEngine, VolumeIndex};
    use cairn_core::engine::CancellationToken;
    use cairn_core::sort::SortColumn;
    use cairn_core::syntax::QueryDefaults;
    use cairn_ntfs::mft::ExtractedItem;
    use cairn_ntfs::usn::UsnChange;

    #[test]
    fn test_usn_polling_marks_presorted_dirty_and_lazy_builds() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);

        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![ExtractedItem {
            frn: 10,
            parent_frn: 5,
            name: "initial.txt".to_string(),
            is_dir: false,
            size: 100,
            created_ts: 100,
            modified_ts: 100,
            flags: 0,
            is_reparse_point: false,
        }]);

        // After ingest, presorted is fresh
        assert!(!vol.presorted_dirty);

        // Apply a USN change
        vol.apply_usn_change(UsnChange::Created {
            frn: 11,
            parent_frn: 5,
            name: "new_file.txt".to_string(),
            is_dir: false,
            attributes: 0,
            ts: 200,
        });

        // Dirty flag must be set
        assert!(vol.presorted_dirty);

        // Add volume to engine
        engine.add_volume(vol);

        // The all-files query is the one that consumes the order, so it triggers the lazy build.
        let cancel = CancellationToken::new();
        let (results, total, ..) = engine.execute_search_multi(
            "",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total, 2);
        assert_eq!(results.len(), 2);

        // Dirty flag must now be cleared and presorted by_name contains both records
        engine.with_primary_volume(|v| {
            assert!(
                !v.presorted_dirty,
                "presorted_dirty must be cleared by the all-files search"
            );
            assert_eq!(
                v.presorted.by_name.len(),
                2,
                "presorted index must be rebuilt"
            );
        });
    }
}
