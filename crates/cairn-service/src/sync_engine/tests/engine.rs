// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::config::ServiceConfig;
use crate::persistence;
use crate::sync_engine::{
    apply_post_scan_decision, post_scan_decision, MultiVolumeEngine, PostScanDecision, VolumeIndex,
};
use cairn_core::engine::CancellationToken;
use cairn_core::sort::SortColumn;
use cairn_core::syntax::QueryDefaults;
use cairn_ntfs::mft::ExtractedItem;
use cairn_ntfs::usn::UsnChange;
use cairn_protocol::status::VolumeState;
use std::sync::atomic::Ordering;

#[test]
fn test_multi_volume_engine_save_load_lifecycle() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);

    // Create volume X
    let mut vol_x = VolumeIndex::new('X');
    vol_x.volume_serial = 0x1111;
    vol_x.journal_id = 100;
    vol_x.last_usn = 5000;
    vol_x.ingest_items(vec![ExtractedItem {
        frn: 100,
        parent_frn: 5,
        name: "x_file.txt".to_string(),
        is_dir: false,
        size: 100,
        created_ts: 100,
        modified_ts: 100,
        flags: 0,
        is_reparse_point: false,
    }]);

    // Create volume Y
    let mut vol_y = VolumeIndex::new('Y');
    vol_y.volume_serial = 0x2222;
    vol_y.journal_id = 200;
    vol_y.last_usn = 6000;
    vol_y.ingest_items(vec![ExtractedItem {
        frn: 200,
        parent_frn: 5,
        name: "y_file.txt".to_string(),
        is_dir: false,
        size: 200,
        created_ts: 200,
        modified_ts: 200,
        flags: 0,
        is_reparse_point: false,
    }]);

    engine.add_volume(vol_x);
    engine.add_volume(vol_y);

    // Atomic save to disk
    engine.save_all_indexes().expect("save_all_indexes");

    // Restore into a new MultiVolumeEngine from the same temp_dir
    let loaded_engine =
        MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec!['X', 'Y']);

    let cancel = CancellationToken::new();
    let (results, total, highlights, _, _) = loaded_engine.execute_search_multi(
        "file.txt",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );

    assert_eq!(total, 2);
    assert_eq!(results.len(), 2);
    assert!(highlights.contains(&"file.txt".to_string()));

    let summaries = loaded_engine.get_volume_summaries();
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].drive_letter, 'X');
    assert_eq!(summaries[0].indexed_count, 1);
    assert_eq!(summaries[1].drive_letter, 'Y');
    assert_eq!(summaries[1].indexed_count, 1);
}

#[test]
fn volume_summary_counts_live_records_not_tombstones() {
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![
        index_item(1, 0, "C:", true),
        index_item(2, 1, "live.txt", false),
        index_item(3, 1, "deleted.txt", false),
    ]);
    vol.apply_usn_change(UsnChange::Deleted { frn: 3 });

    assert_eq!(
        vol.to_summary().indexed_count,
        2,
        "deleted records must not inflate the count the status bar animates toward"
    );
}

#[test]
fn the_exclusion_mask_matches_the_full_path_reference() {
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![
        index_item(1, 0, "C:", true),
        index_item(2, 1, "Users", true),
        index_item(3, 2, "alice.txt", false),
        index_item(4, 1, "$Recycle.Bin", true),
        index_item(5, 4, "deleted.txt", false),
        index_item(6, 4, "nested", true),
        index_item(7, 6, "deeper.txt", false),
        index_item(8, 1, "System Volume Information", true),
        index_item(9, 8, "tracking.log", false),
        index_item(10, 2, "$recycle.bin", false),
        // A directory whose name starts with a ruled name but is not it. The fast path
        // compares whole segments while the full-path check matches a substring; both
        // routes must agree on this file.
        index_item(11, 1, "node_modules", true),
        index_item(12, 11, "kept.txt", false),
        index_item(13, 1, "node_modules_cache", true),
        index_item(14, 13, "also-kept.txt", false),
    ]);
    let rules = vec![
        "$Recycle.Bin".to_string(),
        "System Volume Information".to_string(),
        "node_modules".to_string(),
    ];

    let mask = vol
        .excluded_record_mask(&rules)
        .expect("bare segment rules must use the memoized mask");
    for (idx, excluded) in mask.iter().enumerate() {
        let path = vol
            .path_cache
            .get_full_path(&vol.records, &vol.arena, 'C', idx as u32);
        assert_eq!(
            *excluded,
            crate::exclusion::is_path_excluded(&path, &rules),
            "mask disagrees with the reference for {path}"
        );
    }
    assert_eq!(
        mask.iter().filter(|excluded| !**excluded).count(),
        5,
        "only the root, Users, alice.txt and the two files under a differently named \
             directory survive; the lowercase file matches the rule"
    );

    let temp_dir = tempfile::tempdir().expect("tempdir");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);
    engine.add_volume(vol);
    // The query has to run under the same rules the mask was built from, so the two routes
    // are compared on one configuration rather than on two.
    let mut config = engine.get_config();
    config.excluded_paths = rules;
    engine.update_config(config);
    let cancel = CancellationToken::new();
    let (_, total, _, _, _) = engine.execute_search_multi(
        "",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(
        total, 5,
        "the all-files query must apply the same exclusions as the reference"
    );
}

#[test]
fn the_query_total_bytes_sum_matched_files_without_double_counting_directories() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![
        index_item(1, 0, "C:", true),
        sized_item(2, 1, "a.txt", false, 100),
        sized_item(3, 1, "b.bin", false, 200),
        index_item(4, 1, "folder", true),
        sized_item(5, 4, "c.txt", false, 300),
    ]);
    engine.add_volume(vol);
    let cancel = CancellationToken::new();

    let (_, total, _, _, bytes) = engine.execute_search_multi(
        "",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total, 5, "directories are results too");
    assert_eq!(
        bytes, 600,
        "only file rows contribute bytes; the folder's recursive size is not added on top"
    );

    let (_, total, _, _, bytes) = engine.execute_search_multi(
        "a.txt",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total, 1);
    assert_eq!(bytes, 100);

    // Directories are containers: folder-only results report 0 bytes because files inside
    // are not matched.
    let (_, total, _, _, bytes) = engine.execute_search_multi(
        "folder:",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total, 2, "matches C: and folder");
    assert_eq!(bytes, 0, "directories contribute zero bytes");

    let (_, total, _, _, bytes) = engine.execute_search_multi(
        "folder",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total, 1, "matches only the folder named 'folder'");
    assert_eq!(bytes, 0, "directory contributes zero bytes");
}

#[test]
fn unsupported_exclusion_rule_forms_keep_the_reference_fallback() {
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![index_item(1, 0, "C:", true)]);
    for rule in ["*.tmp", "target\\debug", r"C:\Users"] {
        assert!(
            vol.excluded_record_mask(&[rule.to_string()]).is_none(),
            "{rule} must fall back to the full-path reference"
        );
    }
    assert_eq!(
        vol.excluded_record_mask(&[]),
        Some(vec![false]),
        "no rules means nothing is excluded"
    );
}

#[test]
fn a_contended_volume_lock_serves_the_cached_summary() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![index_item(1, 0, "C:", true)]);
    engine.add_volume(vol);

    let first = engine.get_volume_summaries();
    assert_eq!(first.len(), 1);

    // A query holds this write guard for its whole run. The envelope must answer from the
    // cache instead of waiting for the read lock (which would deadlock this thread here).
    let guard = engine.volumes.write().unwrap();
    let cached = engine.get_volume_summaries();
    drop(guard);
    assert_eq!(
        cached, first,
        "a connection handshake must not wait behind an in-flight query"
    );
}

#[test]
fn test_volume_cache_invalidation_does_not_silently_return_stale_data() {
    let temp_dir = tempfile::tempdir().expect("tempdir");

    // 1. Save a cached index for volume 'Z' with old records
    let mut vol_z = VolumeIndex::new('Z');
    vol_z.volume_serial = 0xEEEE;
    vol_z.journal_id = 999;
    vol_z.last_usn = 1000;
    vol_z.ingest_items(vec![ExtractedItem {
        frn: 50,
        parent_frn: 5,
        name: "stale_secret.txt".to_string(),
        is_dir: false,
        size: 500,
        created_ts: 100,
        modified_ts: 100,
        flags: 0,
        is_reparse_point: false,
    }]);

    persistence::save_volume_index_atomic(
        temp_dir.path(),
        'Z',
        vol_z.volume_serial,
        vol_z.journal_id,
        vol_z.last_usn,
        &vol_z.records,
        &vol_z.arena,
        vol_z.directory_sizes_valid,
    )
    .expect("save_volume_index_atomic");

    // Corrupt cache file (simulating invalidation / corrupt index)
    let cache_file = temp_dir.path().join("vol_Z.idx");
    std::fs::write(&cache_file, b"corrupted content").expect("write corrupted");

    // Load into engine: cache is invalid and live MFT scan for Z: fails
    let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec!['Z']);

    let summaries = engine.get_volume_summaries();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].drive_letter, 'Z');
    // Must NOT remain in VolumeState::Synced and must NOT retain stale records (IDX-05, IDX-07)
    assert_ne!(
        summaries[0].status,
        VolumeState::Synced,
        "Volume must not remain Synced when cache invalidation occurs"
    );
    assert_eq!(
        summaries[0].indexed_count, 0,
        "Stale records must be cleared"
    );

    // Search must return 0 results
    let cancel = CancellationToken::new();
    let (results, total, ..) = engine.execute_search_multi(
        "stale_secret",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total, 0);
    assert!(results.is_empty());
}

#[test]
fn test_journal_id_mismatch_and_usn_out_of_range_invalidates_volume() {
    let mut vol = VolumeIndex::new('C');
    vol.volume_serial = 0x1111;
    vol.ingest_items(vec![ExtractedItem {
        frn: 10,
        parent_frn: 5,
        name: "cached_file.txt".to_string(),
        is_dir: false,
        size: 100,
        created_ts: 10,
        modified_ts: 10,
        flags: 0,
        is_reparse_point: false,
    }]);
    vol.journal_id = 500;
    vol.last_usn = 1000;
    assert_eq!(vol.records.len(), 1);

    // A journal recreated under a snapshot leaves it on no valid timeline (IDX-05), and the
    // rebuild verdict comes from the same function the full-scan paths run through.
    assert_eq!(
        post_scan_decision(true, 1000, Some(2000), Some(2000)),
        PostScanDecision::Rebuild
    );
    assert!(
        !apply_post_scan_decision(&mut vol, PostScanDecision::Rebuild),
        "an unreconcilable volume must not be kept"
    );

    // Must be marked Rebuilding with records wiped (never Synced with stale data)
    assert_eq!(vol.status.state(), VolumeState::Rebuilding);
    assert_eq!(vol.records.len(), 0);
    assert_eq!(vol.arena.len(), 0);
}

#[test]
fn test_usn_create_already_captured_by_the_snapshot_is_not_duplicated() {
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![ExtractedItem {
        frn: 20,
        parent_frn: 5,
        name: "mid_scan.txt".to_string(),
        is_dir: false,
        size: 7,
        created_ts: 100,
        modified_ts: 100,
        flags: 0,
        is_reparse_point: false,
    }]);
    assert_eq!(vol.records.len(), 1);

    // `[T0, T1)` overlaps the scan itself, so the create for a captured file arrives again.
    vol.apply_usn_change(UsnChange::Created {
        frn: 20,
        parent_frn: 5,
        name: "mid_scan.txt".to_string(),
        is_dir: false,
        attributes: 0,
        ts: 300,
    });
    assert_eq!(
        vol.records.len(),
        1,
        "replaying a create the snapshot already holds must not double the row"
    );
    assert_eq!(
        vol.records[0].size, 7,
        "the real size beats the journal's zero"
    );

    // A different name for the same FRN is a hard link and stays a record of its own (IDX-06).
    vol.apply_usn_change(UsnChange::Created {
        frn: 20,
        parent_frn: 9,
        name: "hard_link.txt".to_string(),
        is_dir: false,
        attributes: 0,
        ts: 300,
    });
    assert_eq!(vol.records.len(), 2);
}

#[test]
fn test_excluded_drives_filtering() {
    let temp_dir = tempfile::tempdir().expect("tempdir");

    let config = ServiceConfig {
        excluded_drives: vec!['D'],
        ..Default::default()
    };

    // 1. Initialize engine with drives C and D, but D is in excluded_drives
    let engine = MultiVolumeEngine::with_index_dir_drives_and_config(
        temp_dir.path(),
        vec!['C', 'D'],
        config,
    );

    let summaries = engine.get_volume_summaries();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].drive_letter, 'C');
    assert!(
        !summaries.iter().any(|s| s.drive_letter == 'D'),
        "Excluded drive D must not be initialized (IDX-01)"
    );

    // 2. Add an excluded volume manually - add_volume must reject it
    let mut vol_d = VolumeIndex::new('D');
    vol_d.ingest_items(vec![ExtractedItem {
        frn: 10,
        parent_frn: 5,
        name: "d_file.txt".to_string(),
        is_dir: false,
        size: 10,
        created_ts: 10,
        modified_ts: 10,
        flags: 0,
        is_reparse_point: false,
    }]);
    engine.add_volume(vol_d);
    assert!(
        !engine
            .get_volume_summaries()
            .iter()
            .any(|s| s.drive_letter == 'D'),
        "add_volume must reject excluded drive D"
    );

    // 3. Hot update: add 'C' to excluded_drives
    let mut new_cfg = engine.get_config();
    new_cfg.excluded_drives.push('C');
    engine.update_config(new_cfg);

    // Volume C must be purged from memory and summaries
    let summaries_after = engine.get_volume_summaries();
    assert!(
        summaries_after.is_empty(),
        "Newly excluded drive C must be purged from memory (IDX-01)"
    );

    let cancel = CancellationToken::new();
    let (results, total, ..) = engine.execute_search_multi(
        "",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total, 0);
    assert!(results.is_empty());
}

#[test]
fn test_excluded_paths_filtering_and_hot_apply() {
    let temp_dir = tempfile::tempdir().expect("tempdir");

    let config = ServiceConfig {
        excluded_drives: vec![],
        excluded_paths: vec![
            r"$Recycle.Bin".to_string(),
            r"System Volume Information".to_string(),
            r"C:/Secret/".to_string(),
            r"node_modules/".to_string(),
            r"target/debug".to_string(),
            r"**/*.tmp".to_string(),
        ],
        ..Default::default()
    };

    let engine =
        MultiVolumeEngine::with_index_dir_drives_and_config(temp_dir.path(), vec![], config);

    // Populate volume C
    let mut vol = VolumeIndex::new('C');
    let items = vec![
        ExtractedItem {
            frn: 10,
            parent_frn: 5,
            name: "$Recycle.Bin".to_string(),
            is_dir: true,
            size: 0,
            created_ts: 100,
            modified_ts: 100,
            flags: cairn_core::FLAG_DIRECTORY,
            is_reparse_point: false,
        },
        ExtractedItem {
            frn: 11,
            parent_frn: 10,
            name: "deleted_file.txt".to_string(),
            is_dir: false,
            size: 100,
            created_ts: 100,
            modified_ts: 100,
            flags: 0,
            is_reparse_point: false,
        },
        ExtractedItem {
            frn: 20,
            parent_frn: 5,
            name: "Secret".to_string(),
            is_dir: true,
            size: 0,
            created_ts: 100,
            modified_ts: 100,
            flags: cairn_core::FLAG_DIRECTORY,
            is_reparse_point: false,
        },
        ExtractedItem {
            frn: 21,
            parent_frn: 20,
            name: "confidential.pdf".to_string(),
            is_dir: false,
            size: 200,
            created_ts: 100,
            modified_ts: 100,
            flags: 0,
            is_reparse_point: false,
        },
        ExtractedItem {
            frn: 30,
            parent_frn: 5,
            name: "scratch.tmp".to_string(),
            is_dir: false,
            size: 50,
            created_ts: 100,
            modified_ts: 100,
            flags: 0,
            is_reparse_point: false,
        },
        ExtractedItem {
            frn: 35,
            parent_frn: 5,
            name: "node_modules".to_string(),
            is_dir: true,
            size: 0,
            created_ts: 100,
            modified_ts: 100,
            flags: cairn_core::FLAG_DIRECTORY,
            is_reparse_point: false,
        },
        ExtractedItem {
            frn: 36,
            parent_frn: 35,
            name: "package.json".to_string(),
            is_dir: false,
            size: 150,
            created_ts: 100,
            modified_ts: 100,
            flags: 0,
            is_reparse_point: false,
        },
        ExtractedItem {
            frn: 40,
            parent_frn: 5,
            name: "public_doc.txt".to_string(),
            is_dir: false,
            size: 300,
            created_ts: 100,
            modified_ts: 100,
            flags: 0,
            is_reparse_point: false,
        },
    ];
    vol.ingest_items(items);
    engine.add_volume(vol);

    let cancel = CancellationToken::new();

    // 1. Search without query text (returns all allowed)
    let (results, total, ..) = engine.execute_search_multi(
        "",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    // Only public_doc.txt (frn 40) should be returned:
    // - deleted_file.txt in $Recycle.Bin is excluded
    // - confidential.pdf in C:\Secret is excluded (via C:/Secret/ rule)
    // - scratch.tmp (*.tmp) is excluded (via **/*.tmp rule)
    // - package.json in node_modules is excluded (via node_modules/ rule)
    // - $Recycle.Bin, Secret, node_modules directories match excluded rules
    assert_eq!(total, 1);
    assert_eq!(results.len(), 1);

    // 2. Direct search for deleted_file must be excluded
    let (results, total, ..) = engine.execute_search_multi(
        "deleted_file",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total, 0);
    assert!(results.is_empty());

    // 3. Direct search for confidential must be excluded
    let (results, total, ..) = engine.execute_search_multi(
        "confidential",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total, 0);
    assert!(results.is_empty());

    // 4. Direct search for package.json in node_modules must be excluded
    let (results, total, ..) = engine.execute_search_multi(
        "package.json",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total, 0);
    assert!(results.is_empty());

    // 5. Hot config update: remove **/*.tmp rule (IDX-09)
    let mut new_cfg = engine.get_config();
    new_cfg.excluded_paths.retain(|p| !p.contains("*.tmp"));
    engine.update_config(new_cfg);

    // Now search for .tmp finds scratch.tmp immediately without restart
    let (results, total, ..) = engine.execute_search_multi(
        ".tmp",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total, 1);
    assert_eq!(results.len(), 1);
}

#[test]
fn test_snapshot_mode_fallback_and_search() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);

    let mut vol = VolumeIndex::new('E');
    vol.ingest_items(vec![ExtractedItem {
        frn: 55,
        parent_frn: 5,
        name: "snapshot_file.rs".to_string(),
        is_dir: false,
        size: 1024,
        created_ts: 100,
        modified_ts: 100,
        flags: 0,
        is_reparse_point: false,
    }]);

    // Downgrade to SnapshotMode (IDX-08)
    vol.status.set(VolumeState::SnapshotMode);
    vol.usn_enabled = false;

    engine.add_volume(vol);

    let summaries = engine.get_volume_summaries();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].drive_letter, 'E');
    assert_eq!(summaries[0].status, VolumeState::SnapshotMode);
    assert!(!summaries[0].usn_enabled);
    assert_eq!(summaries[0].indexed_count, 1);

    // SnapshotMode volume must participate in search and return results
    let cancel = CancellationToken::new();
    let (results, total, ..) = engine.execute_search_multi(
        "snapshot_file",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total, 1);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].drive_letter, 'E');

    // rescan_snapshot_volumes should execute cleanly without panic
    engine.rescan_snapshot_volumes();
}
#[test]
fn an_untrusted_configuration_cannot_be_reloaded() {
    let temp = tempfile::tempdir().expect("tempdir");
    let index = temp.path().join("index");
    let content = temp.path().join("content");
    let config = temp.path().join("service.toml");
    let engine = MultiVolumeEngine::with_paths(
        &index,
        &content,
        &config,
        Vec::new(),
        ServiceConfig::default(),
    );
    engine.config_trusted.store(false, Ordering::SeqCst);
    std::fs::write(&config, "admin_only_query = true\n").expect("write config");

    assert!(
        engine.reload_config_from(&config).is_err(),
        "a configuration rejected at startup must not become trusted on reload"
    );
    assert!(!engine.get_config().admin_only_query);
}

fn engine_with_config_file(temp: &std::path::Path) -> MultiVolumeEngine {
    MultiVolumeEngine::with_paths(
        &temp.join("index"),
        &temp.join("content"),
        &temp.join("service.toml"),
        Vec::new(),
        ServiceConfig::default(),
    )
}

/// The admin-only switch is compared against the live value while the configuration is being
/// written, so the decision cannot be taken against a value another connection has replaced.
/// A submission that keeps the current value is not a change and stays available to standard
/// users; only the secure persistence of `service.toml` can still refuse it.
#[test]
fn changing_admin_only_query_requires_privilege_inside_the_configuration_write() {
    let temp = tempfile::tempdir().expect("tempdir");
    let engine = engine_with_config_file(temp.path());

    let err = engine
        .apply_config_toml("admin_only_query = true\n", false)
        .expect_err("a standard user must not switch administrators-only mode on");
    assert!(err.contains("administrator"), "{err}");
    assert!(!engine.get_config().admin_only_query);

    if let Err(err) = engine.apply_config_toml("admin_only_query = false\n", false) {
        assert!(
            err.contains("could not persist"),
            "keeping the switch must only be refused by the protected write: {err}"
        );
    }
    assert!(!engine.get_config().admin_only_query);

    // The comparison uses the live value: a standard user's request that was authorized while
    // the mode was off must not switch off an enabled mode.
    let temp = tempfile::tempdir().expect("tempdir");
    let engine = engine_with_config_file(temp.path());
    engine.update_config(ServiceConfig {
        admin_only_query: true,
        ..engine.get_config()
    });
    let err = engine
        .apply_config_toml("admin_only_query = false\n", false)
        .expect_err("a standard user must not defeat an enabled administrators-only mode");
    assert!(err.contains("administrator"), "{err}");
    assert!(engine.get_config().admin_only_query);
}

#[test]
fn an_administrator_may_change_admin_only_query_inside_the_configuration_write() {
    let temp = tempfile::tempdir().expect("tempdir");
    let engine = engine_with_config_file(temp.path());

    match engine.apply_config_toml("admin_only_query = true\n", true) {
        Ok(()) => assert!(engine.get_config().admin_only_query),
        Err(err) => assert!(
            err.contains("could not persist"),
            "a privileged change must only be refused by the protected write: {err}"
        ),
    }
}

#[cfg(windows)]
#[test]
fn a_rebuild_is_visible_in_the_cached_summary() {
    let temp = tempfile::tempdir().expect("tempdir");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(temp.path(), Vec::new());
    let mut volume = VolumeIndex::new('C');
    volume.status.reconcile(0, 0, 0);
    engine.add_volume(volume);
    let _ = engine.get_volume_summaries();

    let previous = engine
        .mark_volume_rebuilding('C')
        .expect("the tracked volume is marked rebuilding");
    assert_eq!(
        engine.get_volume_summaries()[0].status,
        VolumeState::Rebuilding
    );

    engine.restore_volume_status('C', previous);
    assert_eq!(engine.get_volume_summaries()[0].status, VolumeState::Synced);
}
