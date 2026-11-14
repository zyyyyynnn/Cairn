// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::persistence;
use crate::sync_engine::treemap_build::budgeted_tree_depth;
use crate::sync_engine::volume_index::BitSet;
use crate::sync_engine::{MultiVolumeEngine, TreeUnavailable, VolumeIndex};
use cairn_core::engine::CancellationToken;
use cairn_core::model::FileRecord;
use cairn_core::sort::SortColumn;
use cairn_core::syntax::parse_query;
use cairn_ntfs::mft::ExtractedItem;
use cairn_ntfs::usn::UsnChange;
use cairn_protocol::status::VolumeState;
use std::collections::HashMap;

#[test]
fn directory_sizes_aggregate_their_indexed_descendants() {
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![
        index_item(1, 0, "C:", true),
        index_item(2, 1, "A", true),
        sized_item(3, 2, "a.bin", false, 100),
        sized_item(4, 2, "b.bin", false, 50),
        index_item(5, 1, "B", true),
        sized_item(6, 5, "c.bin", false, 25),
    ]);

    assert_eq!(vol.records()[record_index(&vol, 2) as usize].size, 150);
    assert_eq!(vol.records()[record_index(&vol, 5) as usize].size, 25);
    assert_eq!(vol.records()[record_index(&vol, 1) as usize].size, 175);
    assert_eq!(vol.records()[record_index(&vol, 3) as usize].size, 100);
}

#[test]
fn directory_sizes_follow_file_resizes_and_deletes() {
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![
        index_item(1, 0, "C:", true),
        index_item(2, 1, "A", true),
        sized_item(3, 2, "a.bin", false, 100),
        sized_item(4, 2, "b.bin", false, 50),
    ]);
    let a = record_index(&vol, 3);
    vol.update_file_size(a, 200);
    assert_eq!(vol.records()[record_index(&vol, 2) as usize].size, 250);
    assert_eq!(vol.records()[record_index(&vol, 1) as usize].size, 250);

    vol.apply_usn_changes([UsnChange::Deleted { frn: 3 }]);
    assert_eq!(vol.records()[record_index(&vol, 2) as usize].size, 50);
    assert_eq!(vol.records()[record_index(&vol, 1) as usize].size, 50);

    // Deleting the directory removes its remaining descendant total; the child's later
    // tombstone must not subtract the same bytes again.
    vol.apply_usn_changes([UsnChange::Deleted { frn: 2 }]);
    assert_eq!(vol.records()[record_index(&vol, 1) as usize].size, 0);
    vol.apply_usn_changes([UsnChange::Deleted { frn: 4 }]);
    assert_eq!(vol.records()[record_index(&vol, 1) as usize].size, 0);
}

/// One oversized subtree shallows its own depth instead of failing the request, because the
/// children it does get still carry exact totals.
#[test]
fn an_oversized_subtree_shallows_its_depth_before_failing() {
    // Depth 2 wants 500 entries, depth 1 wants 40, and the budget is 100.
    assert_eq!(
        budgeted_tree_depth(|depth| if depth == 2 { 500 } else { 40 }, 2, 100),
        Some((1, 40))
    );

    // A subtree that fits is never shallowed.
    assert_eq!(budgeted_tree_depth(|_| 40, 2, 100), Some((2, 40)));

    // Even the immediate children over the budget leaves no answer to give.
    assert_eq!(budgeted_tree_depth(|_| 500, 2, 100), None);

    // A caller who asked for one level is never served a deeper tree.
    assert_eq!(budgeted_tree_depth(|_| 10, 1, 100), Some((1, 10)));
}

/// The budget decides the depth and the built tree must then fit it. A directory whose
/// two-level subtree overflows one response is served one level shallower, with the exact
/// totals on the capped leaves, instead of failing the request.
#[test]
fn a_tree_over_the_entry_budget_is_served_one_level_shallower() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);

    // Two children, one of them holding one entry more than a whole response may carry:
    // depth 2 counts 1 + 2 + 100_000, depth 1 counts 3.
    let deep = MultiVolumeEngine::MAX_TREE_ENTRIES;
    let mut items = vec![
        index_item(1, 0, "C:", true),
        index_item(2, 1, "Huge", true),
        index_item(3, 2, "Big", true),
        index_item(4, 2, "Small", true),
    ];
    for offset in 0..deep {
        items.push(index_item(
            5 + offset as u64,
            3,
            &format!("f{offset}"),
            false,
        ));
    }
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(items);
    engine.add_volume(vol);

    let entries = engine
        .treemap_tree("C:\\Huge", 2)
        .expect("one level fits the budget");
    assert_eq!(
        entries.iter().map(|entry| entry.depth).max(),
        Some(1),
        "the response must stop at the depth the budget paid for"
    );
    assert_eq!(
        entries.len(),
        3,
        "the root and its two children, and nothing from the level that overflowed"
    );
    let big = entries
        .iter()
        .find(|entry| entry.name == "Big")
        .expect("the capped directory is still listed");
    assert_eq!(
        big.descendant_items, deep,
        "a capped node carries the exact total the summary block stands for"
    );

    assert!(
        matches!(
            engine.treemap_tree("C:\\Huge\\Big", 2),
            Err(TreeUnavailable::TooLarge)
        ),
        "a directory whose immediate children overflow has no shallower answer"
    );
}

#[test]
fn directory_sizes_follow_a_move_across_parents() {
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![
        index_item(1, 0, "C:", true),
        index_item(2, 1, "A", true),
        sized_item(3, 2, "a.bin", false, 100),
        index_item(4, 1, "B", true),
    ]);

    vol.apply_usn_changes([UsnChange::Renamed {
        frn: 3,
        parent_frn: 4,
        name: "a.bin".to_string(),
        is_dir: false,
        is_new_name: true,
    }]);

    assert_eq!(vol.records()[record_index(&vol, 2) as usize].size, 0);
    assert_eq!(vol.records()[record_index(&vol, 4) as usize].size, 100);
    assert_eq!(vol.records()[record_index(&vol, 1) as usize].size, 100);
}

#[test]
fn usn_delete_removes_pending_renames_to_prevent_leak() {
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![
        index_item(1, 0, "C:", true),
        index_item(2, 1, "test.txt", false),
    ]);

    // First half of a rename: records pending rename
    vol.apply_usn_changes([UsnChange::Renamed {
        frn: 2,
        parent_frn: 1,
        name: "test.txt".to_string(),
        is_dir: false,
        is_new_name: false,
    }]);
    assert!(vol.has_pending_rename(2));

    // File is deleted before second half of rename arrives
    vol.apply_usn_changes([UsnChange::Deleted { frn: 2 }]);
    assert!(!vol.has_pending_rename(2));
}

#[test]
fn directory_sizes_survive_compaction() {
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![
        index_item(1, 0, "C:", true),
        index_item(2, 1, "A", true),
        sized_item(3, 2, "a.bin", false, 100),
        index_item(4, 2, "dead", true),
    ]);
    vol.apply_usn_changes([UsnChange::Deleted { frn: 4 }]);
    vol.compact_with_policy(crate::sync_engine::volume_index::CompactPolicy {
        min_records: 1,
        min_droppable_percent: 25,
        min_arena_waste_percent: 25,
    });
    assert_eq!(vol.records()[record_index(&vol, 2) as usize].size, 100);
    assert_eq!(vol.records()[record_index(&vol, 1) as usize].size, 100);
}

#[test]
fn appended_batches_report_folder_sizes() {
    let mut vol = VolumeIndex::new('C');
    vol.append_items(vec![
        index_item(1, 0, "C:", true),
        index_item(2, 1, "A", true),
        sized_item(3, 2, "a.bin", false, 100),
    ]);

    // Still mid-scan: no finalize ran, yet the parent already totals its indexed child.
    assert_eq!(
        vol.records()[record_index(&vol, 2) as usize].size,
        100,
        "a folder must not read 0 B while its children are already indexed"
    );
    assert_eq!(vol.records()[record_index(&vol, 1) as usize].size, 100);

    vol.append_items(vec![
        index_item(4, 2, "B", true),
        sized_item(5, 4, "b.bin", false, 50),
        sized_item(6, 2, "c.bin", false, 25),
    ]);

    assert_eq!(vol.records()[record_index(&vol, 2) as usize].size, 175);
    assert_eq!(vol.records()[record_index(&vol, 4) as usize].size, 50);
    assert_eq!(vol.records()[record_index(&vol, 1) as usize].size, 175);
    assert_eq!(vol.records()[record_index(&vol, 3) as usize].size, 100);
}

#[test]
fn ingest_items_aggregates_directory_sizes_and_marks_valid() {
    let mut vol = VolumeIndex::new('X');
    // Initial ingest of nested tree with file sizes
    vol.ingest_items(vec![
        index_item(1, 0, "X:", true),
        index_item(2, 1, "dir", true),
        sized_item(3, 2, "file1.txt", false, 400),
        sized_item(4, 2, "file2.txt", false, 600),
    ]);

    assert!(vol.directory_sizes_valid());
    assert_eq!(vol.records()[record_index(&vol, 2) as usize].size, 1000);
    assert_eq!(vol.records()[record_index(&vol, 1) as usize].size, 1000);
}

#[test]
fn a_batch_with_several_kind_changes_ends_with_one_correct_aggregate() {
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![
        index_item(1, 0, "C:", true),
        index_item(2, 1, "A", true),
        sized_item(3, 2, "a.bin", false, 100),
        sized_item(4, 2, "b.bin", false, 50),
        index_item(5, 1, "B", true),
        sized_item(6, 5, "c.bin", false, 25),
    ]);

    // Two files turn into directories, and another file moves from B into A in the same batch.
    vol.apply_usn_changes(vec![
        UsnChange::BasicInfoChanged {
            frn: 3,
            ts: 2,
            attributes: cairn_ntfs::usn::FILE_ATTRIBUTE_DIRECTORY,
        },
        UsnChange::BasicInfoChanged {
            frn: 4,
            ts: 2,
            attributes: cairn_ntfs::usn::FILE_ATTRIBUTE_DIRECTORY,
        },
        UsnChange::Renamed {
            frn: 6,
            parent_frn: 2,
            name: "c.bin".to_string(),
            is_dir: false,
            is_new_name: true,
        },
    ]);

    assert!(
        vol.directory_sizes_valid(),
        "the batch ends fully aggregated"
    );
    assert_eq!(vol.records()[record_index(&vol, 3) as usize].size, 0);
    assert_eq!(vol.records()[record_index(&vol, 4) as usize].size, 0);
    assert_eq!(
        vol.records()[record_index(&vol, 2) as usize].size,
        25,
        "A now holds two empty directories and the moved file"
    );
    assert_eq!(vol.records()[record_index(&vol, 5) as usize].size, 0);
    assert_eq!(vol.records()[record_index(&vol, 1) as usize].size, 25);
}

#[test]
fn a_cache_without_the_directory_size_flag_is_recomputed_and_migrated() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let mut vol = VolumeIndex::new('Z');
    vol.volume_serial = 0x5151;
    vol.ingest_items(vec![
        index_item(1, 0, "Z:", true),
        index_item(2, 1, "A", true),
        sized_item(3, 2, "a.bin", false, 100),
        sized_item(4, 2, "b.bin", false, 50),
    ]);
    // Exactly what an older cache holds: every directory size is still zero, and the header
    // has no validity flag because that byte did not exist.
    let mut legacy_records = vol.records().to_vec();
    for record in &mut legacy_records {
        if record.is_dir() && !record.is_tombstone() {
            record.size = 0;
        }
    }
    persistence::save_volume_index_atomic(
        temp_dir.path(),
        'Z',
        vol.volume_serial,
        vol.journal_id,
        vol.last_usn,
        &legacy_records,
        vol.arena(),
        false,
    )
    .expect("legacy cache");

    let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec!['Z']);
    engine.with_volumes(|volumes| {
        let volume = &volumes[&'Z'];
        assert_eq!(
            volume.records()[record_index(volume, 2) as usize].size,
            150,
            "the one-time load pass must rebuild folder totals"
        );
        assert_eq!(volume.records()[record_index(volume, 1) as usize].size, 150);
        assert!(volume.directory_sizes_valid());
    });

    let migrated =
        persistence::load_volume_index(&temp_dir.path().join("vol_Z.idx")).expect("cache");
    assert!(
        migrated.directory_sizes_valid,
        "the migration must rewrite the flag so later launches skip the aggregation"
    );
}

#[test]
fn a_cache_with_a_valid_flag_keeps_its_stored_folder_sizes() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let mut vol = VolumeIndex::new('Y');
    vol.volume_serial = 0x4242;
    vol.ingest_items(vec![
        index_item(1, 0, "Y:", true),
        index_item(2, 1, "A", true),
        sized_item(3, 2, "a.bin", false, 100),
    ]);
    // A sentinel no aggregation would produce: keeping it proves the load trusted the flag
    // instead of walking the whole table again.
    let a = record_index(&vol, 2) as usize;
    let mut seeded_records = vol.records().to_vec();
    seeded_records[a].size = 777;
    assert!(vol.directory_sizes_valid());
    persistence::save_volume_index_atomic(
        temp_dir.path(),
        'Y',
        vol.volume_serial,
        vol.journal_id,
        vol.last_usn,
        &seeded_records,
        vol.arena(),
        true,
    )
    .expect("valid cache");

    let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec!['Y']);
    engine.with_volumes(|volumes| {
        let volume = &volumes[&'Y'];
        assert_eq!(volume.records()[record_index(volume, 2) as usize].size, 777);
        assert!(volume.directory_sizes_valid());
    });
}

#[test]
fn append_items_drops_the_excluded_service_data_subtree() {
    let mut vol = VolumeIndex::new('C');
    vol.set_excluded_roots([2]);
    vol.append_items(vec![
        index_item(1, 0, "C:", true),
        index_item(2, 1, "Cairn", true),
        index_item(4, 2, "vol_C.idx", false),
        index_item(3, 1, "Users", true),
        index_item(5, 3, "notes.txt", false),
    ]);

    let frns: Vec<u64> = vol.records().iter().map(|record| record.frn).collect();
    assert_eq!(frns, [1, 3, 5]);
    let notes = &vol.records()[2];
    assert_eq!(
        vol.records()[notes.parent_idx as usize].frn,
        3,
        "a kept child must still resolve its parent after skipped rows"
    );
    assert!(vol.is_excluded_frn(2));
    assert!(!vol.is_excluded_frn(4));
    assert!(!vol.contains_frn(2));
    assert!(!vol.contains_frn(4));
}

#[test]
fn usn_events_for_the_excluded_subtree_never_enter_the_index() {
    let mut vol = VolumeIndex::new('C');
    vol.set_excluded_roots([2]);
    vol.append_items(vec![
        index_item(1, 0, "C:", true),
        index_item(3, 1, "Users", true),
    ]);
    let before = vol.records().len();

    vol.apply_usn_changes([UsnChange::Created {
        frn: 4,
        parent_frn: 2,
        name: "vol_C.idx".to_string(),
        is_dir: false,
        attributes: 0,
        ts: 10,
    }]);
    assert_eq!(vol.records().len(), before);
    assert!(!vol.contains_frn(4));

    vol.apply_usn_changes([UsnChange::Deleted { frn: 4 }]);
    assert!(
        vol.flush_pending_content_deletions().is_empty(),
        "a file that was never indexed has no content document to delete"
    );
    assert!(vol.records().iter().all(|record| !record.is_tombstone()));

    vol.apply_usn_changes([UsnChange::Created {
        frn: 6,
        parent_frn: 3,
        name: "new.txt".to_string(),
        is_dir: false,
        attributes: 0,
        ts: 11,
    }]);
    assert_eq!(vol.records().len(), before + 1);
    assert_eq!(vol.records().last().expect("new record").frn, 6);
}

#[test]
fn compact_drops_tombstones_and_excluded_subtrees_and_remaps_parents() {
    use crate::sync_engine::volume_index::CompactPolicy;
    use cairn_core::sort::SortColumn;

    let mut vol = VolumeIndex::new('C');
    vol.append_items(vec![
        index_item(1, 0, "C:", true),
        index_item(2, 1, "Cairn", true),
        index_item(4, 2, "vol_C.idx", false),
        index_item(7, 2, "Nested", true),
        index_item(8, 7, "sub.bin", false),
        index_item(3, 1, "Users", true),
        index_item(5, 3, "notes.txt", false),
        index_item(9, 3, "Temp", true),
        index_item(10, 9, "scratch.txt", false),
        index_item(6, 3, "deleted.txt", false),
    ]);
    // Loaded caches predate the exclusion, so the roots are registered after the records.
    vol.set_excluded_roots([2]);
    vol.apply_usn_changes([
        UsnChange::Deleted { frn: 7 },
        UsnChange::Deleted { frn: 9 },
        UsnChange::Deleted { frn: 6 },
    ]);
    vol.last_usn = 777;
    vol.journal_id = 9;
    vol.volume_serial = 42;
    vol.usn_enabled = false;
    vol.status.set(VolumeState::CatchingUp(50));
    assert_eq!(vol.records().len(), 10);

    let policy = CompactPolicy {
        min_records: 1,
        min_droppable_percent: 25,
        min_arena_waste_percent: 25,
    };
    // 2, 4, 7 and 8 are service data (7 is also a tombstone); 9 and 6 are tombstones. 10's
    // parent is gone but its own row is still live, so it is kept and re-rooted.
    assert_eq!(
        vol.compact_with_policy(policy).map(|r| r.dropped_records),
        Some(6)
    );
    let frns: Vec<u64> = vol.records().iter().map(|record| record.frn).collect();
    assert_eq!(frns, [1, 3, 5, 10]);
    assert_eq!(vol.record_name(&vol.records()[2]), "notes.txt");
    assert_eq!(vol.record_name(&vol.records()[3]), "scratch.txt");
    assert_eq!(vol.records()[2].parent_idx, 1);
    assert_eq!(vol.records()[1].frn, 3);
    assert_eq!(vol.records()[3].parent_idx, FileRecord::ROOT_PARENT);

    for frn in [2, 4, 6, 7, 8, 9] {
        assert!(!vol.contains_frn(frn));
    }
    assert_eq!(vol.record_indices_for_frn(5), Some([2].as_slice()));
    assert!(vol.is_excluded_frn(2), "configuration must survive");
    assert_eq!(vol.last_usn, 777);
    assert_eq!(vol.journal_id, 9);
    assert_eq!(vol.volume_serial, 42);
    assert!(!vol.usn_enabled);
    assert_eq!(vol.status.state(), VolumeState::CatchingUp(50));
    assert!(!vol.is_presorted_dirty());
    assert!(!vol.has_presorted_column(SortColumn::Name));
    vol.ensure_presorted_column(SortColumn::Name);
    assert_eq!(
        vol.presorted_order(SortColumn::Name).map(<[u32]>::len),
        Some(4)
    );

    let cancel = CancellationToken::new();
    let matches = cairn_core::engine::execute_query(
        vol.records(),
        vol.arena(),
        vol.path_cache(),
        vol.drive_letter,
        &parse_query("notes"),
        &cancel,
    );
    assert_eq!(matches, vec![2]);
    let row = vol.build_file_row(2, &[], |_| None).expect("file row");
    assert_eq!(row.name, "notes.txt");

    assert!(
        vol.compact_with_policy(policy).is_none(),
        "a compacted volume has nothing left to drop"
    );
}

#[test]
fn compact_if_needed_leaves_a_small_index_alone() {
    let mut vol = VolumeIndex::new('C');
    vol.append_items(vec![index_item(1, 0, "only.txt", false)]);
    vol.apply_usn_changes([UsnChange::Deleted { frn: 1 }]);
    assert!(
        !vol.compact_if_needed(),
        "a table below the size threshold must not be compacted"
    );
    assert_eq!(vol.records().len(), 1);
    assert!(vol.records()[0].is_tombstone());
}

#[test]
fn test_volume_index_ingest_and_usn_changes() {
    let mut vol = VolumeIndex::new('C');

    // Initial ingest of 3 items
    let items = vec![
        ExtractedItem {
            frn: 10,
            parent_frn: 5,
            name: "documents".to_string(),
            is_dir: true,
            size: 0,
            created_ts: 1000,
            modified_ts: 1000,
            flags: cairn_core::FLAG_DIRECTORY,
            is_reparse_point: false,
        },
        ExtractedItem {
            frn: 11,
            parent_frn: 10,
            name: "annual_report.pdf".to_string(),
            is_dir: false,
            size: 2048,
            created_ts: 1001,
            modified_ts: 1001,
            flags: 0,
            is_reparse_point: false,
        },
        ExtractedItem {
            frn: 12,
            parent_frn: 10,
            name: "budget.xlsx".to_string(),
            is_dir: false,
            size: 4096,
            created_ts: 1002,
            modified_ts: 1002,
            flags: 0,
            is_reparse_point: false,
        },
    ];
    vol.ingest_items(items);

    let cancel = CancellationToken::new();

    // 1. Initial search finds report
    let q_report = parse_query("annual");
    let matches = cairn_core::engine::execute_query(
        vol.records(),
        vol.arena(),
        vol.path_cache(),
        vol.drive_letter,
        &q_report,
        &cancel,
    );
    assert_eq!(matches.len(), 1);

    // 2. USN Create: add "presentation.pptx"
    vol.apply_usn_changes([UsnChange::Created {
        frn: 13,
        parent_frn: 10,
        name: "presentation.pptx".to_string(),
        is_dir: false,
        attributes: 0,
        ts: 1003,
    }]);

    let q_pres = parse_query("presentation");
    let matches_pres = cairn_core::engine::execute_query(
        vol.records(),
        vol.arena(),
        vol.path_cache(),
        vol.drive_letter,
        &q_pres,
        &cancel,
    );
    assert_eq!(matches_pres.len(), 1);

    // 3. USN Rename: rename "annual_report.pdf" to "q4_report.pdf"
    vol.apply_usn_changes([
        UsnChange::Renamed {
            frn: 11,
            parent_frn: 10,
            name: "annual_report.pdf".to_string(),
            is_dir: false,
            is_new_name: false,
        },
        UsnChange::Renamed {
            frn: 11,
            parent_frn: 10,
            name: "q4_report.pdf".to_string(),
            is_dir: false,
            is_new_name: true,
        },
    ]);

    let q_annual = parse_query("annual");
    let matches_annual = cairn_core::engine::execute_query(
        vol.records(),
        vol.arena(),
        vol.path_cache(),
        vol.drive_letter,
        &q_annual,
        &cancel,
    );
    assert_eq!(matches_annual.len(), 0);

    let q_q4 = parse_query("q4_report");
    let matches_q4 = cairn_core::engine::execute_query(
        vol.records(),
        vol.arena(),
        vol.path_cache(),
        vol.drive_letter,
        &q_q4,
        &cancel,
    );
    assert_eq!(matches_q4.len(), 1);

    // 4. USN Delete: delete "budget.xlsx"
    vol.apply_usn_changes([UsnChange::Deleted { frn: 12 }]);
    let q_budget = parse_query("budget");
    let matches_budget = cairn_core::engine::execute_query(
        vol.records(),
        vol.arena(),
        vol.path_cache(),
        vol.drive_letter,
        &q_budget,
        &cancel,
    );
    assert_eq!(matches_budget.len(), 0);
}

#[test]
fn matched_bytes_are_counted_once_at_the_highest_matched_point() {
    let mut vol = VolumeIndex::new('C');
    // Simulate NTFS hierarchy:
    // FRN 5 (root, parent 5), aggregate size 1000
    // FRN 10 (dir Windows, parent 5), aggregate size 400
    // FRN 20 (dir System32, parent 10), aggregate size 200
    // FRN 30 (file notepad.exe, parent 20), size 50
    // FRN 40 (dir Users, parent 5), aggregate size 500
    // FRN 50 (file outside.txt, parent 5), size 100
    vol.ingest_items(vec![
        ExtractedItem {
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
        ExtractedItem {
            frn: 10,
            parent_frn: 5,
            name: "Windows".to_string(),
            is_dir: true,
            size: 0,
            created_ts: 0,
            modified_ts: 0,
            flags: cairn_core::FLAG_DIRECTORY,
            is_reparse_point: false,
        },
        ExtractedItem {
            frn: 20,
            parent_frn: 10,
            name: "System32".to_string(),
            is_dir: true,
            size: 0,
            created_ts: 0,
            modified_ts: 0,
            flags: cairn_core::FLAG_DIRECTORY,
            is_reparse_point: false,
        },
        ExtractedItem {
            frn: 30,
            parent_frn: 20,
            name: "notepad.exe".to_string(),
            is_dir: false,
            size: 50,
            created_ts: 0,
            modified_ts: 0,
            flags: 0,
            is_reparse_point: false,
        },
        ExtractedItem {
            frn: 40,
            parent_frn: 5,
            name: "Users".to_string(),
            is_dir: true,
            size: 0,
            created_ts: 0,
            modified_ts: 0,
            flags: cairn_core::FLAG_DIRECTORY,
            is_reparse_point: false,
        },
        ExtractedItem {
            frn: 50,
            parent_frn: 5,
            name: "outside.txt".to_string(),
            is_dir: false,
            size: 100,
            created_ts: 0,
            modified_ts: 0,
            flags: 0,
            is_reparse_point: false,
        },
    ]);

    // After ingest, directory sizes are aggregated:
    // notepad: 50, outside: 100
    // System32: 50
    // Windows: 50
    // Users: 0
    // Root (FRN 5): 150
    let root_idx = vol.first_record_index_for_frn(5).expect("root");
    let win_idx = vol.first_record_index_for_frn(10).expect("win");
    let sys_idx = vol.first_record_index_for_frn(20).expect("sys");
    let users_idx = vol.first_record_index_for_frn(40).expect("users");
    let note_idx = vol.first_record_index_for_frn(30).expect("note");
    let out_idx = vol.first_record_index_for_frn(50).expect("out");

    // 1. Every folder matched: the root is the highest hit point and owns the whole tree.
    let all_folders = vec![root_idx, win_idx, sys_idx, users_idx];
    assert_eq!(vol.calculate_matched_bytes(&all_folders), 150);

    // 2. A folder-only hit reports the folder's recursive size; the nested System32 is
    //    covered by Windows and adds nothing.
    let subfolders = vec![win_idx, sys_idx, users_idx];
    assert_eq!(vol.calculate_matched_bytes(&subfolders), 50);

    // 3. A single folder hit reports its recursive size on its own.
    assert_eq!(vol.calculate_matched_bytes(&[sys_idx]), 50);

    // 4. Sibling directories keep their own shares.
    let siblings = vec![win_idx, users_idx];
    assert_eq!(vol.calculate_matched_bytes(&siblings), 50);

    // 5. Pure file query: files contribute their size.
    let files = vec![note_idx, out_idx];
    assert_eq!(vol.calculate_matched_bytes(&files), 150);

    // 6. Mixed query: Windows covers its matched descendant; the outside file counts once.
    let mixed = vec![win_idx, note_idx, out_idx];
    assert_eq!(vol.calculate_matched_bytes(&mixed), 150);

    // 7. Duplicate indices are counted once.
    let duplicates = vec![note_idx, note_idx, out_idx, out_idx];
    assert_eq!(vol.calculate_matched_bytes(&duplicates), 150);

    // 8. A many-times repeated index is still one record.
    let mut large_query = Vec::new();
    for _ in 0..50 {
        large_query.push(note_idx);
        large_query.push(out_idx);
    }
    assert!(large_query.len() > 64);
    assert_eq!(vol.calculate_matched_bytes(&large_query), 150);

    // 9. Tombstoned records contribute zero bytes.
    vol.apply_usn_changes([UsnChange::Deleted { frn: 30 }]);
    assert_eq!(
        vol.calculate_matched_bytes(&[note_idx, out_idx]),
        100,
        "tombstoned file contributes zero bytes"
    );
}

#[test]
fn bitset_contains_reports_membership_within_its_capacity() {
    let empty = BitSet::with_capacity(0);
    assert!(!empty.contains(0), "an empty set holds no member");

    let mut bs = BitSet::with_capacity(128);
    for bit in [0, 63, 64, 127] {
        assert!(
            !bs.contains(bit),
            "bit {bit} is not a member before insertion"
        );
    }

    for bit in [0, 63, 64, 127] {
        bs.insert(bit);
    }
    for bit in [0, 63, 64, 127] {
        assert!(bs.contains(bit), "bit {bit} is a member after insertion");
    }
    for bit in [1, 62, 65, 126] {
        assert!(!bs.contains(bit), "bit {bit} was never inserted");
    }

    // Beyond the capacity there is no member, and the lookup must not panic.
    assert!(!bs.contains(128));
    assert!(!bs.contains(usize::MAX));
}

#[test]
fn bitset_operations_and_boundaries() {
    let mut bs = BitSet::with_capacity(128);
    assert_eq!(bs.words.len(), 2);

    // Insertion of boundary bits
    assert!(bs.insert(0));
    assert!(bs.insert(63));
    assert!(bs.insert(64));
    assert!(bs.insert(127));

    // Re-insertion of existing bits returns false
    assert!(!bs.insert(0));
    assert!(!bs.insert(63));
    assert!(!bs.insert(64));
    assert!(!bs.insert(127));

    // Out of bounds insertion returns false
    assert!(!bs.insert(128));
    assert!(!bs.insert(1000));
}

#[test]
fn loaded_volume_absorbs_small_usn_burst_without_doubling_capacity() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut source = VolumeIndex::new('C');
    let mut items = Vec::with_capacity(2_000);
    items.push(index_item(5, 0, "C:", true));
    for i in 0..1_999u64 {
        items.push(sized_item(
            100 + i,
            5,
            &format!("file_{i:04}.txt"),
            false,
            128,
        ));
    }
    source.ingest_items(items);
    let saved = source.save_to_dir(temp.path()).expect("save index");
    let loaded = crate::persistence::load_volume_index(&saved).expect("load index");

    let mut vol = VolumeIndex::new('C');
    vol.restore_from_loaded(loaded);
    let before = vol.memory_footprint();
    assert_eq!(before.presorted_bytes, 0);

    let burst: Vec<UsnChange> = (0..40u64)
        .map(|k| UsnChange::Created {
            frn: 50_000 + k,
            parent_frn: 5,
            name: format!("burst_{k:02}.log"),
            is_dir: false,
            attributes: 0,
            ts: 99,
        })
        .collect();
    vol.apply_usn_changes(burst);
    let after = vol.memory_footprint();
    assert_eq!(after.records_bytes, before.records_bytes);
    assert_eq!(after.arena_bytes, before.arena_bytes);
    assert_eq!(after.folded_arena_bytes, before.folded_arena_bytes);
}

#[test]
fn compaction_triggers_on_arena_rename_bloat_and_shrinks_capacity() {
    let mut vol = VolumeIndex::new('C');
    let mut items = Vec::with_capacity(100);
    items.push(index_item(5, 0, "C:", true));
    for i in 0..99u64 {
        items.push(index_item(100 + i, 5, &format!("short_{i:02}.txt"), false));
    }
    vol.ingest_items(items);

    for round in 0..12u64 {
        let mut renames = Vec::new();
        for i in 0..50u64 {
            renames.push(UsnChange::Renamed {
                frn: 100 + i,
                parent_frn: 5,
                name: format!("renamed_long_prefix_{round:02}_{i:02}_artifact_payload.txt"),
                is_dir: false,
                is_new_name: true,
            });
        }
        vol.apply_usn_changes(renames);
    }

    let bloated = vol.memory_footprint();
    let policy = super::super::volume_index::CompactPolicy {
        min_records: 50,
        min_droppable_percent: 25,
        min_arena_waste_percent: 25,
    };
    let report = vol
        .compact_with_policy(policy)
        .expect("arena rename bloat must trigger compaction even with zero tombstones");
    assert!(report.reclaimed_arena_bytes > 0);

    let shrunk = vol.memory_footprint();
    assert!(
        shrunk.arena_bytes < bloated.arena_bytes / 2,
        "compaction must shrink over-allocated arena: before={} after={}",
        bloated.arena_bytes,
        shrunk.arena_bytes
    );
}

#[test]
fn switching_the_presorted_column_releases_the_previous_order() {
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![
        index_item(1, 0, "C:", true),
        sized_item(3, 1, "a.bin", false, 100),
        sized_item(4, 1, "b.bin", false, 50),
    ]);

    vol.ensure_presorted_column(SortColumn::Name);
    assert!(vol.has_presorted_column(SortColumn::Name));
    vol.ensure_presorted_column(SortColumn::Size);
    assert!(vol.has_presorted_column(SortColumn::Size));
    assert!(
        !vol.has_presorted_column(SortColumn::Name),
        "only the requested presorted column may stay resident"
    );
}

#[test]
fn the_public_ledger_covers_an_ingested_volume_and_its_single_column() {
    let mut vol = VolumeIndex::new('C');
    let mut items = vec![index_item(1, 0, "C:", true)];
    for i in 0..2_000u64 {
        items.push(sized_item(
            10 + i,
            1,
            &format!("file_{i:05}.txt"),
            false,
            64,
        ));
    }
    vol.ingest_items(items);

    let idle = vol.memory_footprint();
    assert_eq!(idle.record_count, 2_001);
    assert_eq!(idle.live_count, 2_001);
    assert!(
        idle.frn_lookup_bytes > 0,
        "the compact FRN lookup is part of the ledger"
    );
    assert_eq!(idle.presorted_bytes, 0);
    assert!(idle.bytes_per_record() < 125.0);

    vol.ensure_presorted_column(SortColumn::Name);
    let warm = vol.memory_footprint();
    assert!(warm.presorted_bytes >= 2_001 * 4);
    assert!(warm.bytes_per_record() < 125.0);
    assert!(warm.total_bytes() > idle.total_bytes());

    vol.ensure_presorted_column(SortColumn::Size);
    let switched = vol.memory_footprint();
    assert_eq!(
        switched.presorted_bytes, warm.presorted_bytes,
        "the column swap stays within the single-column budget"
    );
}

/// Ordinary renames keep the shared exclusion table; only a rule-relevant move rebuilds it.
#[test]
fn ordinary_renames_keep_the_cached_exclusion_states() {
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
    let segments = vec!["$Recycle.Bin".to_string()];
    let before = vol.segment_excluded_states_cached(&segments);

    vol.apply_usn_changes([UsnChange::Renamed {
        frn: 10,
        parent_frn: 5,
        name: "renamed.txt".to_string(),
        is_dir: false,
        is_new_name: true,
    }]);
    let after = vol.segment_excluded_states_cached(&segments);
    assert!(
        std::sync::Arc::ptr_eq(&before, &after),
        "an ordinary rename keeps the cached table"
    );

    vol.apply_usn_changes([UsnChange::Renamed {
        frn: 10,
        parent_frn: 5,
        name: "$Recycle.Bin".to_string(),
        is_dir: false,
        is_new_name: true,
    }]);
    let after = vol.segment_excluded_states_cached(&segments);
    assert!(
        !std::sync::Arc::ptr_eq(&before, &after),
        "a rename into a rule segment rebuilds the table"
    );
}

/// The segment-exclusion table is shared between queries and extended by appends.
#[test]
fn appended_records_extend_the_cached_exclusion_states() {
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
    let segments = vec!["$Recycle.Bin".to_string()];
    assert_eq!(vol.segment_excluded_states_cached(&segments).len(), 1);

    vol.apply_usn_changes([UsnChange::Created {
        frn: 11,
        parent_frn: 5,
        name: "$Recycle.Bin".to_string(),
        is_dir: true,
        attributes: 0,
        ts: 1,
    }]);
    let states = vol.segment_excluded_states_cached(&segments);
    assert_eq!(states.len(), 2, "the appended record extends the table");

    let volumes = HashMap::from([('C', vol)]);
    let exclusions = crate::sync_engine::path_exclusions::PathExclusions::new(&volumes, &segments);
    let excluded = volumes[&'C']
        .first_record_index_for_frn(11)
        .expect("the new directory");
    let kept = volumes[&'C']
        .first_record_index_for_frn(10)
        .expect("the old file");
    assert!(exclusions.excludes(&volumes[&'C'], excluded));
    assert!(!exclusions.excludes(&volumes[&'C'], kept));
}

/// A pending change set makes the cached order unreadable; the merged order must match a
/// fresh build exactly.
#[test]
fn an_order_with_pending_changes_reads_as_unbuilt_and_merges_to_the_rebuilt_order() {
    let mut vol = VolumeIndex::new('C');
    let file = |frn: u64, name: &str, size: u64| ExtractedItem {
        frn,
        parent_frn: 5,
        name: name.to_string(),
        is_dir: false,
        size,
        created_ts: 0,
        modified_ts: 0,
        flags: 0,
        is_reparse_point: false,
    };
    vol.ingest_items(vec![file(10, "beta.txt", 10), file(11, "alpha.txt", 20)]);
    vol.ensure_presorted_column(SortColumn::Name);

    vol.apply_usn_changes([
        UsnChange::Created {
            frn: 12,
            parent_frn: 5,
            name: "aardvark.txt".to_string(),
            is_dir: false,
            attributes: 0,
            ts: 1,
        },
        UsnChange::Renamed {
            frn: 10,
            parent_frn: 5,
            name: "zulu.txt".to_string(),
            is_dir: false,
            is_new_name: true,
        },
    ]);
    assert!(
        vol.presorted_order(SortColumn::Name).is_none(),
        "a pending change set is not a readable order"
    );

    vol.ensure_presorted_column(SortColumn::Name);
    let merged: Vec<u32> = vol
        .presorted_order(SortColumn::Name)
        .expect("merged order")
        .to_vec();

    // Switching columns releases the order; switching back rebuilds it from scratch.
    vol.ensure_presorted_column(SortColumn::Size);
    vol.ensure_presorted_column(SortColumn::Name);
    let rebuilt: Vec<u32> = vol
        .presorted_order(SortColumn::Name)
        .expect("rebuilt order")
        .to_vec();
    assert_eq!(merged, rebuilt, "the merge equals a fresh build");
}

/// A directory rename invalidates the path order for the whole subtree.
#[test]
fn a_directory_rename_invalidates_the_path_order() {
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![
        ExtractedItem {
            frn: 10,
            parent_frn: 5,
            name: "folder".to_string(),
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
            size: 10,
            created_ts: 0,
            modified_ts: 0,
            flags: 0,
            is_reparse_point: false,
        },
    ]);
    vol.ensure_presorted_column(SortColumn::Path);
    vol.apply_usn_changes([UsnChange::Renamed {
        frn: 10,
        parent_frn: 5,
        name: "renamed".to_string(),
        is_dir: true,
        is_new_name: true,
    }]);
    assert!(
        vol.presorted_order(SortColumn::Path).is_none(),
        "descendant paths changed with the directory"
    );
    vol.ensure_presorted_column(SortColumn::Path);
    assert!(vol.presorted_order(SortColumn::Path).is_some());
}

/// A file resize moves its ancestors in the size order, not just the file.
#[test]
fn a_resize_repositions_the_ancestors_in_the_size_order() {
    let mut vol = VolumeIndex::new('C');
    let dir = |frn: u64, name: &str| ExtractedItem {
        frn,
        parent_frn: 5,
        name: name.to_string(),
        is_dir: true,
        size: 0,
        created_ts: 0,
        modified_ts: 0,
        flags: cairn_core::FLAG_DIRECTORY,
        is_reparse_point: false,
    };
    let file = |frn: u64, parent: u64, name: &str, size: u64| ExtractedItem {
        frn,
        parent_frn: parent,
        name: name.to_string(),
        is_dir: false,
        size,
        created_ts: 0,
        modified_ts: 0,
        flags: 0,
        is_reparse_point: false,
    };
    vol.ingest_items(vec![
        dir(10, "small"),
        file(11, 10, "a.txt", 10),
        dir(12, "large"),
        file(13, 12, "b.txt", 30),
    ]);
    vol.ensure_presorted_column(SortColumn::Size);

    let a = vol.first_record_index_for_frn(11).expect("a.txt");
    vol.update_file_size(a, 100);
    vol.ensure_presorted_column(SortColumn::Size);

    let order: Vec<u64> = vol
        .presorted_order(SortColumn::Size)
        .expect("merged order")
        .iter()
        .map(|&idx| vol.records()[idx as usize].frn)
        .collect();
    let small = order.iter().position(|&frn| frn == 10).expect("small");
    let large = order.iter().position(|&frn| frn == 12).expect("large");
    assert!(
        large < small,
        "the enlarged folder moves after the smaller one"
    );
}

/// A rule set the segment table cannot answer invalidates open views on every rename.
#[test]
fn a_full_path_rule_rename_invalidates_exclusion_verdicts() {
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
    let volumes = HashMap::from([('C', vol)]);
    let _exclusions =
        crate::sync_engine::path_exclusions::PathExclusions::new(&volumes, &["*.tmp".to_string()]);
    let epoch = volumes[&'C'].exclusion_epoch();

    let mut volumes = volumes;
    volumes
        .get_mut(&'C')
        .expect("volume")
        .apply_usn_changes([UsnChange::Renamed {
            frn: 10,
            parent_frn: 5,
            name: "alpha.tmp".to_string(),
            is_dir: false,
            is_new_name: true,
        }]);
    assert!(
        volumes[&'C'].exclusion_epoch() > epoch,
        "a full-path rule rename can move the record's verdict"
    );
}

/// The all-files total is a maintained counter, not a scan of the order.
#[test]
fn live_file_bytes_tracks_ingest_resize_and_delete() {
    let mut vol = VolumeIndex::new('C');
    let file = |frn: u64, name: &str, size: u64| ExtractedItem {
        frn,
        parent_frn: 5,
        name: name.to_string(),
        is_dir: false,
        size,
        created_ts: 0,
        modified_ts: 0,
        flags: 0,
        is_reparse_point: false,
    };
    vol.ingest_items(vec![
        file(10, "alpha.txt", 10),
        file(11, "beta.txt", 20),
        ExtractedItem {
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
    assert_eq!(vol.live_file_bytes(), 30);

    let beta = vol.first_record_index_for_frn(11).expect("beta");
    vol.update_file_size(beta, 25);
    assert_eq!(vol.live_file_bytes(), 35);

    vol.apply_usn_changes([UsnChange::Deleted { frn: 11 }]);
    assert_eq!(vol.live_file_bytes(), 10);
}

/// A record appended after the order was built merges into it; the table is not re-sorted.
#[test]
fn appended_records_merge_into_the_cached_presorted_column() {
    let mut vol = VolumeIndex::new('C');
    let make = |frn: u64, name: &str| ExtractedItem {
        frn,
        parent_frn: 5,
        name: name.to_string(),
        is_dir: false,
        size: 100,
        created_ts: 0,
        modified_ts: 0,
        flags: 0,
        is_reparse_point: false,
    };
    vol.ingest_items(vec![make(10, "beta.txt"), make(11, "alpha.txt")]);
    vol.ensure_presorted_column(SortColumn::Name);
    let version = vol.presorted_version();

    vol.apply_usn_changes([UsnChange::Created {
        frn: 12,
        parent_frn: 5,
        name: "aardvark.txt".to_string(),
        is_dir: false,
        attributes: 0,
        ts: 1,
    }]);
    assert!(vol.needs_presorted_column(SortColumn::Name));
    vol.ensure_presorted_column(SortColumn::Name);
    assert!(
        vol.presorted_version() > version,
        "the merged order is a new version"
    );

    let names: Vec<&str> = vol
        .presorted_order(SortColumn::Name)
        .expect("order")
        .iter()
        .map(|&idx| {
            let record = &vol.records()[idx as usize];
            vol.arena().get_name(record.name_offset, record.name_len)
        })
        .collect();
    assert_eq!(names, ["aardvark.txt", "alpha.txt", "beta.txt"]);
}

/// A column is invalidated only by the events whose key it sorts on.
#[test]
fn an_event_invalidates_only_the_orders_its_key_moves() {
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![ExtractedItem {
        frn: 10,
        parent_frn: 5,
        name: "alpha.txt".to_string(),
        is_dir: false,
        size: 100,
        created_ts: 0,
        modified_ts: 0,
        flags: 0,
        is_reparse_point: false,
    }]);
    vol.ensure_presorted_column(SortColumn::Name);

    vol.mark_presorted_sizes_changed([0]);
    assert!(
        vol.presorted_order(SortColumn::Name).is_some(),
        "a size change does not move a record in the name order"
    );
    assert!(vol.needs_presorted_column(SortColumn::Size));

    vol.mark_presorted_names_changed([0]);
    assert!(
        vol.needs_presorted_column(SortColumn::Name),
        "a rename is merged into the name order"
    );
    vol.ensure_presorted_column(SortColumn::Name);
    assert!(vol.presorted_order(SortColumn::Name).is_some());
}
