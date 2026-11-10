// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::sync_engine::search::presorted_order;
use crate::sync_engine::{MultiVolumeEngine, VolumeIndex};
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
