// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! View-level tests for the presorted order (FST-01, FST-02). The order itself is owned by
//! `VolumeIndex`; these tests exercise it through whole queries.

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

    /// A column without a presorted array must not disturb the warm order an open view uses.
    #[test]
    fn a_created_sorted_view_keeps_the_warm_name_order() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);
        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![ExtractedItem {
            frn: 10,
            parent_frn: 5,
            name: "alpha.txt".to_string(),
            is_dir: false,
            size: 10,
            created_ts: 1,
            modified_ts: 1,
            flags: 0,
            is_reparse_point: false,
        }]);
        engine.add_volume(vol);

        let cancel = CancellationToken::new();
        let defaults = QueryDefaults::default();
        engine.execute_search_multi("C: ", &defaults, SortColumn::Name, true, &cancel);
        let version = engine.with_volumes(|volumes| volumes[&'C'].presorted_version());

        let outcome =
            engine.execute_search_multi("C: ", &defaults, SortColumn::DateCreated, true, &cancel);
        assert!(matches!(outcome.rows, MatchSet::Items(_)));
        assert_eq!(outcome.total, 1);
        engine.with_volumes(|volumes| {
            assert!(
                volumes[&'C'].has_presorted_column(SortColumn::Name),
                "the warm order survives a column without a presorted array"
            );
            assert_eq!(
                volumes[&'C'].presorted_version(),
                version,
                "the warm order is not invalidated"
            );
        });
    }

    /// A drive-scoped view is served for its own volume no matter how many other volumes are
    /// indexed, and only its order is built.
    #[test]
    fn a_drive_scoped_view_serves_beside_another_indexed_volume() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);
        for (drive, name) in [('C', "alpha.txt"), ('D', "beta.txt")] {
            let mut vol = VolumeIndex::new(drive);
            vol.ingest_items(vec![ExtractedItem {
                frn: 10,
                parent_frn: 5,
                name: name.to_string(),
                is_dir: false,
                size: 10,
                created_ts: 0,
                modified_ts: 0,
                flags: 0,
                is_reparse_point: false,
            }]);
            engine.add_volume(vol);
        }

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
            assert!(volumes[&'C'].has_presorted_column(SortColumn::Name));
            assert!(
                !volumes[&'D'].has_presorted_column(SortColumn::Name),
                "the unscoped volume's order is not built for a scoped view"
            );
        });

        let all = engine.execute_search_multi(
            "",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert!(matches!(all.rows, MatchSet::Items(_)));
        assert_eq!(all.total, 2);
    }

    /// A rename repositions equal-key rows through the name tiebreak in every keyed column.
    #[test]
    fn a_rename_merges_the_size_order_to_the_rebuilt_order() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);
        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![
            ExtractedItem {
                frn: 10,
                parent_frn: 5,
                name: "beta.txt".to_string(),
                is_dir: false,
                size: 10,
                created_ts: 0,
                modified_ts: 0,
                flags: 0,
                is_reparse_point: false,
            },
            ExtractedItem {
                frn: 11,
                parent_frn: 5,
                name: "alpha.txt".to_string(),
                is_dir: false,
                size: 10,
                created_ts: 0,
                modified_ts: 0,
                flags: 0,
                is_reparse_point: false,
            },
            ExtractedItem {
                frn: 12,
                parent_frn: 5,
                name: "delta.txt".to_string(),
                is_dir: false,
                size: 10,
                created_ts: 0,
                modified_ts: 0,
                flags: 0,
                is_reparse_point: false,
            },
            ExtractedItem {
                frn: 13,
                parent_frn: 5,
                name: "gamma.txt".to_string(),
                is_dir: false,
                size: 10,
                created_ts: 0,
                modified_ts: 0,
                flags: 0,
                is_reparse_point: false,
            },
            ExtractedItem {
                frn: 14,
                parent_frn: 5,
                name: "epsilon.txt".to_string(),
                is_dir: false,
                size: 10,
                created_ts: 0,
                modified_ts: 0,
                flags: 0,
                is_reparse_point: false,
            },
        ]);
        engine.add_volume(vol);
        engine.update_volume('C', |volume| {
            volume.ensure_presorted_column(SortColumn::Size)
        });

        engine.update_volume('C', |volume| {
            volume.apply_usn_changes([UsnChange::Renamed {
                frn: 10,
                parent_frn: 5,
                name: "0.txt".to_string(),
                is_dir: false,
                is_new_name: true,
            }]);
        });
        engine.update_volume('C', |volume| {
            volume.ensure_presorted_column(SortColumn::Size)
        });
        let merged = engine.with_volumes(|volumes| {
            volumes[&'C']
                .presorted_order(SortColumn::Size)
                .expect("merged order")
                .to_vec()
        });

        engine.update_volume('C', |volume| {
            volume.ensure_presorted_column(SortColumn::Name)
        });
        engine.update_volume('C', |volume| {
            volume.ensure_presorted_column(SortColumn::Size)
        });
        let rebuilt = engine.with_volumes(|volumes| {
            volumes[&'C']
                .presorted_order(SortColumn::Size)
                .expect("rebuilt order")
                .to_vec()
        });
        assert_eq!(merged, rebuilt);
    }

    /// A cancelled textless query with path rules reports no rows instead of walking the order.
    #[test]
    fn a_cancelled_view_query_reports_no_rows() {
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

        let cancel = CancellationToken::new();
        cancel.cancel();
        let outcome = engine.execute_search_multi(
            "C: ",
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert_eq!(outcome.total, 0);
        assert!(matches!(outcome.rows, MatchSet::Items(items) if items.is_empty()));
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

    /// A directory rename invalidates the path order; the next path-sorted view rebuilds it
    /// instead of silently falling back to a materialized result.
    #[test]
    fn a_directory_rename_rebuilds_the_path_view() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);
        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![
            ExtractedItem {
                frn: 1,
                parent_frn: 0,
                name: "C:".to_string(),
                is_dir: true,
                size: 0,
                created_ts: 0,
                modified_ts: 0,
                flags: 0,
                is_reparse_point: false,
            },
            ExtractedItem {
                frn: 2,
                parent_frn: 1,
                name: "docs".to_string(),
                is_dir: true,
                size: 0,
                created_ts: 0,
                modified_ts: 0,
                flags: 0,
                is_reparse_point: false,
            },
            ExtractedItem {
                frn: 3,
                parent_frn: 2,
                name: "a.txt".to_string(),
                is_dir: false,
                size: 10,
                created_ts: 0,
                modified_ts: 0,
                flags: 0,
                is_reparse_point: false,
            },
        ]);
        engine.add_volume(vol);

        let cancel = CancellationToken::new();
        let defaults = QueryDefaults::default();
        let first = engine.execute_search_multi("C: ", &defaults, SortColumn::Path, true, &cancel);
        assert!(matches!(first.rows, MatchSet::AllFiles(_)));

        engine.update_volume('C', |volume| {
            volume.apply_usn_changes([UsnChange::Renamed {
                frn: 2,
                parent_frn: 1,
                name: "readme".to_string(),
                is_dir: true,
                is_new_name: true,
            }]);
        });

        let second = engine.execute_search_multi("C: ", &defaults, SortColumn::Path, true, &cancel);
        assert!(
            matches!(second.rows, MatchSet::AllFiles(_)),
            "the stale path order is rebuilt instead of falling back"
        );
        engine.with_volumes(|volumes| {
            assert_eq!(
                volumes[&'C']
                    .presorted_order(SortColumn::Path)
                    .map(<[u32]>::len),
                Some(3),
                "the rebuilt path order answers the next view"
            );
        });
    }
}
