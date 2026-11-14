// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Presorted index management and lazy rebuilding (FST-01, FST-02).

use super::MultiVolumeEngine;
use cairn_core::sort::SortColumn;

impl MultiVolumeEngine {
    /// Ensures presorted indices across all volumes are up to date for the requested sort column.
    pub fn ensure_presorted_indices(&self, sort_by: SortColumn) {
        let column_has_presort = matches!(
            sort_by,
            SortColumn::Name | SortColumn::Size | SortColumn::DateModified | SortColumn::Path
        );
        let needs_rebuild = {
            let read_lock = self.volumes.read().unwrap();
            read_lock.values().any(|v| {
                v.is_presorted_dirty()
                    || (column_has_presort
                        && !v.records().is_empty()
                        && !v.has_presorted_column(sort_by))
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

        // After ingest, presorted is clean and lazily unbuilt.
        assert!(!vol.is_presorted_dirty());
        assert!(!vol.has_presorted_column(SortColumn::Name));

        // Apply a USN change
        vol.apply_usn_changes([UsnChange::Created {
            frn: 11,
            parent_frn: 5,
            name: "new_file.txt".to_string(),
            is_dir: false,
            attributes: 0,
            ts: 200,
        }]);

        // Dirty flag must be set
        assert!(vol.is_presorted_dirty());

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

        // Dirty flag must now be cleared and ONLY the requested Name column is materialized.
        engine.with_volumes(|volumes| {
            let v = &volumes[&'C'];
            assert!(
                !v.is_presorted_dirty(),
                "presorted_dirty must be cleared by the all-files search"
            );
            assert_eq!(
                v.presorted_order(SortColumn::Name).map(<[u32]>::len),
                Some(2),
                "requested presorted column must be built"
            );
            assert!(
                !v.has_presorted_column(SortColumn::Size),
                "unrequested presorted columns must stay unbuilt"
            );
        });
    }

    /// The sidebar's all-files view is a drive-scoped textless query (`C: `). It is the same
    /// whole-volume view, so it must build and consume the presorted order instead of sorting
    /// every match per request.
    #[test]
    fn a_drive_scoped_all_files_query_uses_the_presorted_column() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);

        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(
            ["beta.txt", "alpha.txt"]
                .into_iter()
                .enumerate()
                .map(|(ix, name)| ExtractedItem {
                    frn: 10 + ix as u64,
                    parent_frn: 5,
                    name: name.to_string(),
                    is_dir: false,
                    size: 100,
                    created_ts: 100,
                    modified_ts: 100,
                    flags: 0,
                    is_reparse_point: false,
                })
                .collect(),
        );
        engine.add_volume(vol);

        let cancel = CancellationToken::new();
        let (results, total, ..) = engine.execute_search_multi(
            "C: ",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(total, 2);
        assert_eq!(results.len(), 2);

        engine.with_volumes(|volumes| {
            let v = &volumes[&'C'];
            assert!(
                !v.is_presorted_dirty(),
                "a drive-scoped all-files query builds the order"
            );
            assert_eq!(
                v.presorted_order(SortColumn::Name).map(<[u32]>::len),
                Some(2),
                "the drive-scoped query consumes the requested column"
            );
        });
    }
}
