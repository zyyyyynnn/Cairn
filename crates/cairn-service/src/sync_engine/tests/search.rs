// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use super::{index_item, sized_item};
use crate::sync_engine::{MatchSet, MultiVolumeEngine, VolumeIndex};
use cairn_core::engine::CancellationToken;
use cairn_core::sort::SortColumn;
use cairn_core::syntax::QueryDefaults;
use cairn_ntfs::mft::ExtractedItem;

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

fn result_names(engine: &MultiVolumeEngine, sort_by: SortColumn, sort_asc: bool) -> Vec<String> {
    let cancel = CancellationToken::new();
    let outcome =
        engine.execute_search_multi("", &QueryDefaults::default(), sort_by, sort_asc, &cancel);
    let results = engine.test_items(outcome.rows);
    let total = outcome.total;
    assert_eq!(total, results.len());
    engine.with_volumes(|vols| {
        let vol = &vols[&'C'];
        results
            .iter()
            .map(|r| {
                vol.record_name(&vol.records()[r.record_idx as usize])
                    .to_string()
            })
            .collect()
    })
}

/// A folder hit carries its recursive space; a descendant that also matches adds nothing.
#[test]
fn a_folder_match_reports_its_recursive_space_once() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![
        ExtractedItem {
            frn: 10,
            parent_frn: 5,
            name: "folderX".to_string(),
            is_dir: true,
            size: 0,
            created_ts: 0,
            modified_ts: 0,
            flags: cairn_core::FLAG_DIRECTORY,
            is_reparse_point: false,
        },
        ExtractedItem {
            frn: 11,
            parent_frn: 10,
            name: "inside.txt".to_string(),
            is_dir: false,
            size: 50,
            created_ts: 0,
            modified_ts: 0,
            flags: 0,
            is_reparse_point: false,
        },
        ExtractedItem {
            frn: 12,
            parent_frn: 5,
            name: "outside.txt".to_string(),
            is_dir: false,
            size: 20,
            created_ts: 0,
            modified_ts: 0,
            flags: 0,
            is_reparse_point: false,
        },
    ]);
    engine.add_volume(vol);

    let cancel = CancellationToken::new();
    let defaults = QueryDefaults::default();
    let folder = engine.execute_search_multi("folderX", &defaults, SortColumn::Name, true, &cancel);
    assert_eq!(folder.total, 1);
    assert_eq!(folder.total_bytes, 50, "the folder's recursive space");

    let both =
        engine.execute_search_multi("folderX|inside", &defaults, SortColumn::Name, true, &cancel);
    assert_eq!(both.total, 2);
    assert_eq!(
        both.total_bytes, 50,
        "the covered child is not counted twice"
    );
}

#[test]
fn a_filtered_query_does_not_rebuild_the_whole_volume_sort() {
    let (_temp_dir, engine) = engine_with_sorting_fixture();
    engine.update_volume('C', |volume| volume.mark_presorted_dirty());

    let cancel = CancellationToken::new();
    let outcome = engine.execute_search_multi(
        "alpha",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    let results = engine.test_items(outcome.rows);
    let total = outcome.total;
    assert_eq!(total, 1);
    assert_eq!(results.len(), 1);
    assert!(
        engine.with_volumes(|volumes| volumes[&'C'].needs_presorted_column(SortColumn::Name)),
        "a filtered query has no use for the volume-wide presorted order"
    );

    let outcome = engine.execute_search_multi(
        "",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    let total = outcome.total;
    assert_eq!(total, 3);
    assert!(
        !engine.with_volumes(|volumes| volumes[&'C'].needs_presorted_column(SortColumn::Name)),
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
    engine.update_volume('C', |vol| vol.mark_presorted_dirty());

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

    assert!(vol.presorted_order(SortColumn::Name).is_none());
    vol.ensure_presorted_column(SortColumn::Name);
    assert!(vol.presorted_order(SortColumn::Name).is_some());
    assert!(vol.presorted_order(SortColumn::DateCreated).is_none());
    assert!(vol.presorted_order(SortColumn::Extension).is_none());

    vol.mark_presorted_dirty();
    assert!(
        vol.presorted_order(SortColumn::Name).is_none(),
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
    let outcome = engine.execute_search_multi(
        "",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    let results = engine.test_items(outcome.rows);
    let total = outcome.total;
    assert_eq!(total, 4);
    let names: Vec<String> = engine.with_volumes(|vols| {
        results
            .iter()
            .map(|r| {
                let vol = &vols[&r.drive_letter];
                vol.record_name(&vol.records()[r.record_idx as usize])
                    .to_string()
            })
            .collect()
    });
    assert_eq!(
        names,
        ["file_1.txt", "file_2.txt", "file_10.txt", "file_20.txt"]
    );

    // 2. Sort by Name descending
    let outcome = engine.execute_search_multi(
        "",
        &QueryDefaults::default(),
        SortColumn::Name,
        false,
        &cancel,
    );
    let results_desc = engine.test_items(outcome.rows);
    let names_desc: Vec<String> = engine.with_volumes(|vols| {
        results_desc
            .iter()
            .map(|r| {
                let vol = &vols[&r.drive_letter];
                vol.record_name(&vol.records()[r.record_idx as usize])
                    .to_string()
            })
            .collect()
    });
    assert_eq!(
        names_desc,
        ["file_20.txt", "file_10.txt", "file_2.txt", "file_1.txt"]
    );

    // 3. Sort by Size ascending (10, 50, 100, 200)
    let outcome = engine.execute_search_multi(
        "",
        &QueryDefaults::default(),
        SortColumn::Size,
        true,
        &cancel,
    );
    let results_size = engine.test_items(outcome.rows);
    let sizes: Vec<u64> = engine.with_volumes(|vols| {
        results_size
            .iter()
            .map(|r| {
                let vol = &vols[&r.drive_letter];
                vol.records()[r.record_idx as usize].size
            })
            .collect()
    });
    assert_eq!(sizes, [10, 50, 100, 200]);

    // 4. Sort by DateModified ascending (100, 200, 300, 400)
    let outcome = engine.execute_search_multi(
        "",
        &QueryDefaults::default(),
        SortColumn::DateModified,
        true,
        &cancel,
    );
    let results_mod = engine.test_items(outcome.rows);
    let dates: Vec<i64> = engine.with_volumes(|vols| {
        results_mod
            .iter()
            .map(|r| {
                let vol = &vols[&r.drive_letter];
                vol.records()[r.record_idx as usize].modified_ts
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
    let outcome = engine.execute_search_multi(
        "",
        &QueryDefaults::default(),
        SortColumn::Extension,
        true,
        &cancel,
    );
    let results_ext = engine.test_items(outcome.rows);
    let exts: Vec<String> = engine.with_volumes(|vols| {
        results_ext
            .iter()
            .map(|r| {
                let vol = &vols[&r.drive_letter];
                let rec = &vol.records()[r.record_idx as usize];
                vol.arena()
                    .get_ext(rec.name_offset, rec.name_len, rec.ext_offset)
                    .to_ascii_lowercase()
            })
            .collect()
    });
    assert_eq!(exts, ["bin", "txt", "txt", "txt", "txt", "txt", "zip"]);
}

#[test]
fn single_volume_filtered_sort_and_exclusions_keep_presorted_unbuilt() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cfg = crate::config::ServiceConfig {
        excluded_paths: vec!["ExcludedDir".to_string()],
        ..Default::default()
    };
    let engine = MultiVolumeEngine::with_index_dir_drives_and_config(temp.path(), vec![], cfg);

    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![
        index_item(1, 0, "C:", true),
        index_item(2, 1, "ExcludedDir", true),
        sized_item(3, 2, "hidden_big.bin", false, 3 * 1024 * 1024),
        sized_item(4, 1, "zeta.bin", false, 2 * 1024 * 1024),
        sized_item(5, 1, "alpha.bin", false, 4 * 1024 * 1024),
        sized_item(6, 1, "small.bin", false, 512),
    ]);
    engine.add_volume(vol);

    let cancel = CancellationToken::new();
    // The recursive directory size makes `"C:"` > 1 MB as well, and a matched folder carries
    // its whole subtree: the excluded `ExcludedDir` is not a row, but its bytes are part of
    // the matched root's space.
    let outcome = engine.execute_search_multi(
        "size:>1mb",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    let results = engine.test_items(outcome.rows);
    let total = outcome.total;
    let total_bytes = outcome.total_bytes;
    assert_eq!(total, 3);
    assert_eq!(total_bytes, 9 * 1024 * 1024 + 512);
    let names = engine.with_volumes(|vols| {
        let v = &vols[&'C'];
        assert_eq!(v.memory_footprint().presorted_bytes, 0);
        results
            .iter()
            .map(|r| {
                v.record_name(&v.records()[r.record_idx as usize])
                    .to_string()
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(names, vec!["alpha.bin", "C:", "zeta.bin"]);

    let outcome = engine.execute_search_multi(
        "file: size:>1mb",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    let file_results = engine.test_items(outcome.rows);
    let file_total = outcome.total;
    let file_bytes = outcome.total_bytes;
    assert_eq!(file_total, 2);
    assert_eq!(file_bytes, 6 * 1024 * 1024);
    let file_names = engine.with_volumes(|vols| {
        let v = &vols[&'C'];
        assert_eq!(v.memory_footprint().presorted_bytes, 0);
        file_results
            .iter()
            .map(|r| {
                v.record_name(&v.records()[r.record_idx as usize])
                    .to_string()
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(file_names, vec!["alpha.bin", "zeta.bin"]);
}

/// An excluded subtree hides rows but stays inside the space of the matched root, so the total
/// does not depend on whether the view or a materialized result answers the query.
#[test]
fn an_excluded_subtree_keeps_the_all_files_space_total() {
    fn fixture() -> VolumeIndex {
        let mut vol = VolumeIndex::new('C');
        vol.ingest_items(vec![
            index_item(1, 0, "C:", true),
            index_item(2, 1, "ExcludedDir", true),
            sized_item(3, 2, "hidden.bin", false, 3 * 1024 * 1024),
            sized_item(4, 1, "visible.bin", false, 1024),
        ]);
        vol
    }

    let temp = tempfile::tempdir().expect("tempdir");
    let cfg = crate::config::ServiceConfig {
        excluded_paths: vec!["ExcludedDir".to_string()],
        ..Default::default()
    };
    let engine = MultiVolumeEngine::with_index_dir_drives_and_config(temp.path(), vec![], cfg);
    engine.add_volume(fixture());

    let cancel = CancellationToken::new();
    let defaults = QueryDefaults::default();
    let view = engine.execute_search_multi("C: ", &defaults, SortColumn::Name, true, &cancel);
    assert!(matches!(view.rows, MatchSet::AllFiles(_)));
    assert_eq!(
        view.total, 2,
        "the excluded directory and its file are not rows"
    );
    assert_eq!(
        view.total_bytes,
        3 * 1024 * 1024 + 1024,
        "the matched root's recursive space includes the excluded subtree"
    );

    let materialized =
        engine.execute_search_multi("C: ", &defaults, SortColumn::DateCreated, true, &cancel);
    assert!(matches!(materialized.rows, MatchSet::Items(_)));
    assert_eq!(
        materialized.total_bytes, view.total_bytes,
        "the space total does not depend on how the rows are served"
    );

    let plain_temp = tempfile::tempdir().expect("tempdir");
    let plain = MultiVolumeEngine::with_index_dir_and_drives(plain_temp.path(), vec![]);
    plain.add_volume(fixture());
    let unruled = plain.execute_search_multi("C: ", &defaults, SortColumn::Name, true, &cancel);
    assert_eq!(
        unruled.total_bytes,
        3 * 1024 * 1024 + 1024,
        "the same fixture without rules reports the same space"
    );
}

/// A rule that excludes the volume root leaves its children visible; the total then comes from
/// the visible top-level records rather than from the root's recursive size.
#[test]
fn an_excluded_root_totals_the_visible_top_level_records() {
    let temp = tempfile::tempdir().expect("tempdir");
    let cfg = crate::config::ServiceConfig {
        excluded_paths: vec!["*:".to_string()],
        ..Default::default()
    };
    let engine = MultiVolumeEngine::with_index_dir_drives_and_config(temp.path(), vec![], cfg);
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![
        index_item(1, 0, "C:", true),
        index_item(2, 1, "ExcludedDir", true),
        sized_item(3, 2, "hidden.bin", false, 3 * 1024 * 1024),
        sized_item(4, 1, "visible.bin", false, 1024),
    ]);
    engine.add_volume(vol);

    let cancel = CancellationToken::new();
    let outcome = engine.execute_search_multi(
        "C: ",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert!(matches!(outcome.rows, MatchSet::AllFiles(_)));
    assert_eq!(
        outcome.total, 3,
        "the root is excluded; its descendants remain"
    );
    assert_eq!(
        outcome.total_bytes,
        3 * 1024 * 1024 + 1024,
        "the visible top-level records cover the same space"
    );
}
