// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Presorted index management and lazy rebuilding (FST-01, FST-02).

use super::MultiVolumeEngine;
use cairn_core::sort::SortColumn;

impl MultiVolumeEngine {
    /// Ensures the requested sort column across all volumes. Only the four columns with a
    /// presorted order take work; the volume decides whether that is a merge or a rebuild.
    pub fn ensure_presorted_indices(&self, sort_by: SortColumn) {
        if !matches!(
            sort_by,
            SortColumn::Name | SortColumn::Size | SortColumn::DateModified | SortColumn::Path
        ) {
            return;
        }
        let needs_work = {
            let read_lock = self.volumes.read().unwrap();
            read_lock
                .values()
                .any(|v| v.needs_presorted_column(sort_by))
        };
        if needs_work {
            let mut write_lock = self.volumes.write().unwrap();
            for vol in write_lock.values_mut() {
                vol.ensure_presorted_column(sort_by);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::sync_engine::{MatchSet, MultiVolumeEngine, VolumeIndex};
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

        // The unbuilt column must be built before it can answer an ordered read.
        assert!(vol.needs_presorted_column(SortColumn::Name));

        // Add volume to engine
        engine.add_volume(vol);

        // The all-files query is the one that consumes the order, so it triggers the lazy build.
        let cancel = CancellationToken::new();
        let outcome = engine.execute_search_multi(
            "",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        let results = engine.test_items(outcome.rows);
        let total = outcome.total;
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

    /// One active volume beside an empty one still gets its order built and its view served.
    /// The build predicate and the view predicate are the same one.
    #[test]
    fn a_single_active_volume_still_serves_the_view_beside_an_empty_volume() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);
        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![ExtractedItem {
            frn: 10,
            parent_frn: 5,
            name: "alpha.txt".to_string(),
            is_dir: false,
            size: 10,
            created_ts: 0,
            modified_ts: 0,
            flags: 0,
            is_reparse_point: false,
        }]);
        engine.add_volume(vol);
        engine.add_volume(VolumeIndex::new('D'));

        let cancel = CancellationToken::new();
        let outcome = engine.execute_search_multi(
            "C: ",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert!(matches!(outcome.rows, MatchSet::AllFiles(_)));
        assert_eq!(outcome.total, 1);
        engine.with_volumes(|volumes| {
            assert!(
                volumes[&'C'].has_presorted_column(SortColumn::Name),
                "the viewed volume's order is built"
            );
        });
    }

    /// A textless whole-volume query is answered by the view, not by a materialized list;
    /// its exact totals still describe the same records.
    #[test]
    fn the_all_files_view_carries_the_exact_totals() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);
        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![
            cairn_ntfs::mft::ExtractedItem {
                frn: 10,
                parent_frn: 5,
                name: "alpha.txt".to_string(),
                is_dir: false,
                size: 10,
                created_ts: 0,
                modified_ts: 0,
                flags: 0,
                is_reparse_point: false,
            },
            cairn_ntfs::mft::ExtractedItem {
                frn: 11,
                parent_frn: 5,
                name: "beta.txt".to_string(),
                is_dir: false,
                size: 20,
                created_ts: 0,
                modified_ts: 0,
                flags: 0,
                is_reparse_point: false,
            },
            cairn_ntfs::mft::ExtractedItem {
                frn: 5,
                parent_frn: 5,
                name: ".".to_string(),
                is_dir: true,
                size: 0,
                created_ts: 0,
                modified_ts: 0,
                flags: cairn_core::FLAG_DIRECTORY,
                is_reparse_point: false,
            },
        ]);
        engine.add_volume(vol);

        let cancel = CancellationToken::new();
        let defaults = QueryDefaults::default();
        let outcome = engine.execute_search_multi("", &defaults, SortColumn::Name, true, &cancel);
        assert!(matches!(outcome.rows, MatchSet::AllFiles(_)));
        assert_eq!(outcome.total, 3);
        assert_eq!(outcome.total_bytes, 30);
        let rows = engine.test_items(outcome.rows);
        assert_eq!(rows.len(), 3);

        let filtered =
            engine.execute_search_multi("alpha", &defaults, SortColumn::Name, true, &cancel);
        assert!(matches!(filtered.rows, MatchSet::Items(_)));
        assert_eq!(filtered.total, 1);
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
        let outcome = engine.execute_search_multi(
            "C: ",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        let results = engine.test_items(outcome.rows);
        let total = outcome.total;
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
