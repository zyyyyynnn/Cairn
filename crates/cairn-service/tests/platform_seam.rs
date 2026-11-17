// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Integration seams for indexed platform sources, bounded change journal, real volume identity,
//! metadata refresh attribute parity, isolated property extraction, and un-truncated conjunctions.

use cairn_content::{ContentWriter, CONTENT_RESULT_TOP_K};
use cairn_core::engine::CancellationToken;
use cairn_core::property_store::{PropertyKind, SourceId};
use cairn_core::query::QueryDegradedReason;
use cairn_core::sort::{SortColumn, SortDirection, SortSpec};
use cairn_core::syntax::QueryDefaults;
use cairn_core::{
    FLAG_HIDDEN, FLAG_READONLY, FLAG_REPARSE_POINT, FLAG_SYSTEM, WIN32_ATTR_ARCHIVE,
    WIN32_ATTR_HIDDEN, WIN32_ATTR_READONLY, WIN32_ATTR_REPARSE_POINT, WIN32_ATTR_SYSTEM,
};
use cairn_ntfs::mft::ExtractedItem;
use cairn_ntfs::volume_identity::VolumeIdentity;
use cairn_protocol::status::{JournalChangeKind, SourceKind, VolumeState};
use cairn_service::config::ServiceConfig;
use cairn_service::sync_engine::folder_source::{
    FolderSource, FolderWatchEvent, FOLDER_CHANGE_BUFFER_CAP,
};
use cairn_service::sync_engine::index_journal::{
    IndexJournal, INDEX_JOURNAL_MAX_BYTES, INDEX_JOURNAL_MAX_ENTRIES,
};
use cairn_service::sync_engine::MultiVolumeEngine;
use cairn_service::sync_engine::{RefreshedRecordMetadata, VolumeIndex};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tempfile::tempdir;

fn cairn_worker_exe() -> PathBuf {
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let name = if cfg!(windows) {
        "CairnWorker.exe"
    } else {
        "CairnWorker"
    };
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(std::path::Path::parent)
                .expect("workspace root")
                .join("target")
        });
    let candidate = target.join(profile).join(name);
    if candidate.is_file() {
        return candidate;
    }
    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors().skip(1) {
            let fallback = ancestor.join(name);
            if fallback.is_file() {
                return fallback;
            }
        }
    }
    panic!(
        "CairnWorker executable must exist at {} (build cairn-worker first)",
        candidate.display()
    );
}

fn minimal_png_bytes(width: u32, height: u32) -> Vec<u8> {
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    out.extend_from_slice(&13u32.to_be_bytes());
    out.extend_from_slice(b"IHDR");
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&[8, 6, 0, 0, 0]);
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(b"IEND");
    out.extend_from_slice(&[0; 4]);
    out
}

#[test]
fn folder_source_scans_updates_incrementally_and_recovers_on_buffer_overflow() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path().join("workspace");
    let external = temp.path().join("external_loop");
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::create_dir_all(&external).unwrap();
    fs::write(root.join("alpha.txt"), "hello").unwrap();
    fs::write(root.join("sub\\beta.md"), "world").unwrap();
    fs::write(external.join("loop_target.txt"), "skip me").unwrap();

    // Real NTFS directory junction must not cause infinite loop or pull external targets.
    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(root.join("junction_out"))
        .arg(&external)
        .output()
        .expect("mklink /J");
    assert!(status.status.success(), "mklink /J failed");

    let mut source = FolderSource::new(SourceId(1000), &root).expect("folder source");
    source.rescan(&[]).expect("rescan");
    assert_eq!(source.to_summary().status, VolumeState::SnapshotMode);
    assert!(
        source.live_count() >= 4,
        "expected root + sub + junction + 2 files"
    );

    let names: Vec<&str> = source
        .records()
        .iter()
        .filter(|r| !r.is_tombstone())
        .map(|r| source.arena().get_name(r.name_offset, r.name_len))
        .collect();
    assert!(names.contains(&"alpha.txt"));
    assert!(names.contains(&"beta.md"));
    assert!(!names.contains(&"loop_target.txt"));

    // Incremental event applies directly without overflow.
    let new_file = root.join("sub\\gamma.txt");
    fs::write(&new_file, "added incrementally").unwrap();
    let mut journal = IndexJournal::new(true);
    let overflowed = source.apply_folder_events(
        &[FolderWatchEvent::Added(new_file.clone())],
        Some(&mut journal),
        1_700_000_001,
    );
    assert!(!overflowed);
    assert!(source
        .records()
        .iter()
        .any(|r| !r.is_tombstone()
            && source.arena().get_name(r.name_offset, r.name_len) == "gamma.txt"));
    assert_eq!(journal.status().entry_count, 1);

    // Writing another file missed by buffered watcher triggers fallback rescan when > cap.
    fs::write(root.join("delta.txt"), "missed during overflow").unwrap();
    let mut burst = Vec::with_capacity(FOLDER_CHANGE_BUFFER_CAP + 1);
    for idx in 0..=FOLDER_CHANGE_BUFFER_CAP {
        burst.push(FolderWatchEvent::Modified(
            root.join(format!("phantom-{idx}.txt")),
        ));
    }
    assert!(
        source.apply_folder_events(&burst, Some(&mut journal), 1_700_000_002),
        "exceeding FOLDER_CHANGE_BUFFER_CAP must trigger fallback rescan"
    );
    source.rescan(&[]).expect("overflow fallback rescan");
    assert!(source
        .records()
        .iter()
        .any(|r| !r.is_tombstone()
            && source.arena().get_name(r.name_offset, r.name_len) == "delta.txt"));

    // Deleting the root folder marks the source Offline cleanly.
    fs::remove_dir_all(&root).unwrap();
    assert!(source.rescan(&[]).is_err());
    assert_eq!(source.to_summary().status, VolumeState::Offline);
}

#[test]
fn engine_folder_source_watch_and_persistence_work_end_to_end() {
    let dir = tempdir().expect("tempdir");
    let watch_root = dir.path().join("notes");
    fs::create_dir_all(&watch_root).unwrap();
    fs::write(watch_root.join("initial.md"), "# Start").unwrap();

    let mut config = ServiceConfig {
        excluded_paths: Vec::new(),
        content_excluded_paths: Vec::new(),
        ..Default::default()
    };
    let engine = MultiVolumeEngine::with_paths(
        &dir.path().join("index"),
        &dir.path().join("content"),
        &dir.path().join("service.toml"),
        vec![],
        config.clone(),
    );

    let added = engine
        .add_folder_source(&watch_root, false)
        .expect("add folder source");
    assert_eq!(added.kind, SourceKind::Folder);
    let scan_deadline = Instant::now() + Duration::from_secs(5);
    let summary = loop {
        if let Some(summary) = engine
            .get_sources()
            .into_iter()
            .find(|source| source.source_id == added.source_id)
        {
            if summary.status == VolumeState::Synced {
                break summary;
            }
        }
        assert!(
            Instant::now() < scan_deadline,
            "initial folder scan did not reach Synced"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        summary.live_watch_enabled,
        "folder watcher must be armed before creating live files"
    );

    // Live watcher should pick up a file created right inside the watched folder.
    let created = watch_root.join("live_watch_target.txt");
    fs::write(&created, "watched").unwrap();

    let cancel = CancellationToken::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let outcome = engine.execute_search_multi(
            "live_watch_target.txt",
            &QueryDefaults::default(),
            &SortSpec::default(),
            &cancel,
        );
        if outcome.total >= 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "ReadDirectoryChangesW live watcher did not index {}",
            created.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let entries = engine.get_index_journal(0, 100);
    assert!(
        entries
            .iter()
            .any(|e| e.path.contains("live_watch_target.txt")),
        "live folder event must appear in read-only IndexJournal: {entries:?}"
    );

    // Config reload/restart retains folder sources cleanly.
    config.folder_sources = vec![watch_root.to_string_lossy().into_owned()];
    let restarted = MultiVolumeEngine::with_paths(
        &dir.path().join("index"),
        &dir.path().join("content"),
        &dir.path().join("service.toml"),
        vec![],
        config,
    );
    restarted.ensure_folder_sources_loaded();
    let sources = restarted.get_sources();
    assert!(
        sources.iter().any(|s| s.kind == SourceKind::Folder),
        "restarted engine must reload configured folder_sources"
    );
}

#[test]
fn index_journal_enforces_entry_and_byte_caps_and_drops_memory_when_disabled() {
    let mut journal = IndexJournal::new(true);
    assert_eq!(journal.status().entry_count, 0);

    // Push past INDEX_JOURNAL_MAX_ENTRIES with short paths -> capped strictly at 200_000.
    for i in 0..(INDEX_JOURNAL_MAX_ENTRIES + 25) {
        journal.push(
            SourceId(2),
            i as u64 + 1,
            JournalChangeKind::Created,
            format!("C:\\f{i}"),
            None,
            100,
        );
    }
    let st = journal.status();
    assert_eq!(st.entry_count, INDEX_JOURNAL_MAX_ENTRIES);
    assert_eq!(st.oldest_seq, 26);
    assert_eq!(st.newest_seq, (INDEX_JOURNAL_MAX_ENTRIES + 25) as u64);

    let page = journal.query(25, 3);
    assert_eq!(page.len(), 3);
    assert_eq!(page[0].seq, 26);

    // Large payloads must trigger the 32 MiB byte ceiling long before another 200k entries.
    let mut sized_journal = IndexJournal::new(true);
    let long_path = format!("C:\\{}\\{}.bin", "segment\\".repeat(120), "payload");
    let count = (INDEX_JOURNAL_MAX_BYTES / long_path.len()) + 500;
    for i in 0..count {
        sized_journal.push(
            SourceId(2),
            i as u64 + 1,
            JournalChangeKind::Renamed,
            format!("{long_path}_{i}"),
            Some(format!("{long_path}_old_{i}")),
            200,
        );
    }
    let sized_status = sized_journal.status();
    assert!(
        sized_status.resident_bytes <= INDEX_JOURNAL_MAX_BYTES,
        "resident bytes {} exceeded cap {}",
        sized_status.resident_bytes,
        INDEX_JOURNAL_MAX_BYTES
    );
    assert!(sized_status.oldest_seq > 1);

    sized_journal.set_enabled(false);
    let disabled = sized_journal.status();
    assert!(!disabled.enabled);
    assert_eq!(disabled.entry_count, 0);
    assert_eq!(disabled.resident_bytes, 0);
}

#[test]
fn volume_identity_guid_mismatch_prevents_reusing_stale_drive_letter_cache() {
    let mut vol = VolumeIndex::new('E');
    vol.set_identity(VolumeIdentity {
        drive_letter: Some('E'),
        mount_point: "E:\\".to_string(),
        volume_guid: r"\\?\Volume{11111111-1111-1111-1111-111111111111}\".to_string(),
        volume_serial: 0xAA11_BB22,
    });
    assert!(vol.matches_volume_identity(&VolumeIdentity {
        drive_letter: Some('E'),
        mount_point: "E:\\".to_string(),
        volume_guid: r"\\?\Volume{11111111-1111-1111-1111-111111111111}\".to_string(),
        volume_serial: 0xAA11_BB22,
    }));
    assert!(
        !vol.matches_volume_identity(&VolumeIdentity {
            drive_letter: Some('E'),
            mount_point: "E:\\".to_string(),
            volume_guid: r"\\?\Volume{22222222-2222-2222-2222-222222222222}\".to_string(),
            volume_serial: 0xAA11_BB22,
        }),
        "same drive letter + serial under a different Volume GUID must be treated as a distinct volume"
    );
}

#[test]
fn apply_refreshed_metadata_synchronizes_all_indexed_attribute_bits() {
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![ExtractedItem {
        frn: 55,
        parent_frn: 0,
        name: "attr_test.txt".to_string(),
        is_dir: false,
        size: 10,
        created_ts: 1,
        modified_ts: 1,
        flags: FLAG_HIDDEN | FLAG_SYSTEM,
        is_reparse_point: false,
    }]);

    // Turn off Hidden/System and turn on ReadOnly + ReparsePoint via refreshed Win32 attributes.
    vol.apply_refreshed_metadata(RefreshedRecordMetadata {
        index: 0,
        frn: 55,
        size: 42,
        created_ts: 1,
        modified_ts: 2,
        win32_attributes: WIN32_ATTR_READONLY | WIN32_ATTR_REPARSE_POINT | WIN32_ATTR_ARCHIVE,
    });
    let rec = vol.records()[0];
    assert!(!rec.is_hidden(), "cleared Hidden attribute must not linger");
    assert!(
        (rec.flags & FLAG_SYSTEM) == 0,
        "cleared System attribute must not linger"
    );
    assert!(
        (rec.flags & FLAG_READONLY) != 0,
        "new ReadOnly attribute must be set"
    );
    assert!(
        (rec.flags & FLAG_REPARSE_POINT) != 0,
        "new ReparsePoint bit must be set"
    );

    // And setting Hidden | System turns them back on.
    vol.apply_refreshed_metadata(RefreshedRecordMetadata {
        index: 0,
        frn: 55,
        size: 42,
        created_ts: 1,
        modified_ts: 3,
        win32_attributes: WIN32_ATTR_HIDDEN | WIN32_ATTR_SYSTEM,
    });
    let updated = vol.records()[0];
    assert!(updated.is_hidden());
    assert!((updated.flags & FLAG_SYSTEM) != 0);
    assert!((updated.flags & FLAG_READONLY) == 0);
}

#[test]
fn property_index_degrades_explicitly_when_off_and_handles_missing_worker_cleanly() {
    let temp = tempdir().expect("tempdir");
    let media_dir = temp.path().join("media");
    fs::create_dir_all(&media_dir).unwrap();
    let good_png = media_dir.join("banner.png");
    fs::write(&good_png, minimal_png_bytes(1920, 1080)).unwrap();

    let config = ServiceConfig {
        excluded_paths: Vec::new(),
        content_excluded_paths: Vec::new(),
        property_index_enabled: false,
        ..Default::default()
    };
    let engine = MultiVolumeEngine::with_paths(
        &temp.path().join("index"),
        &temp.path().join("content"),
        &temp.path().join("service.toml"),
        vec![],
        config,
    );
    engine.add_folder_source(&media_dir, false).unwrap();

    let cancel = CancellationToken::new();
    let disabled_outcome = engine.execute_search_multi(
        "dimensions:1920x1080",
        &QueryDefaults::default(),
        &SortSpec::single(SortColumn::Dimensions, SortDirection::Descending),
        &cancel,
    );
    assert_eq!(
        disabled_outcome.degraded,
        Some(QueryDegradedReason::PropertyIndexDisabled {
            requested: vec![PropertyKind::Dimensions],
        })
    );
    assert_eq!(engine.memory_footprint().property_store_bytes, 0);

    // Enabling without an available worker binary must isolate failure without crashing or bloating store.
    engine.set_property_index_enabled(true);
    let missing_worker = temp.path().join("missing-worker.exe");
    let status = engine
        .run_property_extraction_with_worker(&missing_worker, Duration::from_secs(5))
        .expect("worker invocation failure stays contained");
    assert!(status.enabled);
    assert_eq!(status.indexed_records, 0);
    assert_eq!(engine.memory_footprint().property_store_bytes, 0);

    engine.set_property_index_enabled(false);
    assert!(!engine.property_index_status().enabled);
    assert_eq!(engine.memory_footprint().property_store_bytes, 0);
}

#[test]
#[ignore = "requires target/debug/CairnWorker.exe"]
fn property_index_extracts_through_real_worker_and_recovers_from_corrupt_files() {
    let temp = tempdir().expect("tempdir");
    let media_dir = temp.path().join("media");
    fs::create_dir_all(&media_dir).unwrap();
    let good_png = media_dir.join("banner.png");
    let corrupt_png = media_dir.join("broken.png");
    fs::write(&good_png, minimal_png_bytes(1920, 1080)).unwrap();
    fs::write(&corrupt_png, b"not a valid png image payload").unwrap();

    let config = ServiceConfig {
        excluded_paths: Vec::new(),
        content_excluded_paths: Vec::new(),
        property_index_enabled: false,
        ..Default::default()
    };
    let engine = MultiVolumeEngine::with_paths(
        &temp.path().join("index"),
        &temp.path().join("content"),
        &temp.path().join("service.toml"),
        vec![],
        config,
    );
    engine.add_folder_source(&media_dir, false).unwrap();

    let cancel = CancellationToken::new();
    // Enable properties and invoke real CairnWorker extraction across good + corrupt files.
    engine.set_property_index_enabled(true);
    engine
        .run_property_extraction_with_worker(&cairn_worker_exe(), Duration::from_secs(10))
        .expect("isolated extraction");

    let prop_status = engine.property_index_status();
    assert!(prop_status.enabled);
    assert!(prop_status.indexed_records >= 1);
    assert!(engine.memory_footprint().property_store_bytes > 0);

    let enabled_outcome = engine.execute_search_multi(
        "dimensions:1920x1080",
        &QueryDefaults::default(),
        &SortSpec::single(SortColumn::Dimensions, SortDirection::Descending),
        &cancel,
    );
    assert_eq!(enabled_outcome.degraded, None);
    assert_eq!(enabled_outcome.total, 1);

    // Turning properties back off drops all side-table heap memory immediately.
    engine.set_property_index_enabled(false);
    assert_eq!(engine.memory_footprint().property_store_bytes, 0);
}

#[test]
fn positive_content_conjunction_never_truncates_beyond_top_k_before_mft_filter() {
    let temp = tempdir().expect("tempdir");
    let index_dir = temp.path().join("index");
    let content_dir = temp.path().join("content");

    let total_docs = CONTENT_RESULT_TOP_K + 32;
    let mut mft_items = Vec::with_capacity(total_docs);
    {
        let writer = ContentWriter::open_or_create(&content_dir).expect("writer");
        for i in 1..=total_docs {
            let frn = i as u64;
            let is_target = i == total_docs;
            let name = if is_target {
                "target_match.rs".to_string()
            } else {
                format!("noise_{i}.txt")
            };
            let body = if is_target {
                "alpha filler beta".to_string()
            } else {
                format!("alpha beta alpha beta alpha beta filler {i}")
            };
            mft_items.push(ExtractedItem {
                frn,
                parent_frn: 0,
                name: name.clone(),
                is_dir: false,
                size: 64,
                created_ts: 1,
                modified_ts: 1,
                flags: 0,
                is_reparse_point: false,
            });
            writer
                .index_document_deferred(2, frn, 64, 1, &format!("C:\\{name}"), &body)
                .unwrap();
        }
        writer.commit().unwrap();
        writer.finish().unwrap();
    }

    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(mft_items);

    let cfg = ServiceConfig {
        content_index_enabled: true,
        excluded_paths: Vec::new(),
        content_excluded_paths: Vec::new(),
        ..Default::default()
    };
    let engine = MultiVolumeEngine::with_paths(
        &index_dir,
        &content_dir,
        &temp.path().join("service.toml"),
        vec![],
        cfg,
    );
    engine.add_volume(vol);
    engine.mark_content_ready_for_test(total_docs as u64);

    let cancel = CancellationToken::new();
    let outcome = engine.execute_search_multi(
        "content:alpha content:beta ext:rs",
        &QueryDefaults::default(),
        &SortSpec::default(),
        &cancel,
    );
    assert_eq!(
        outcome.total, 1,
        "positive conjunction must evaluate full hit set before MFT ext:rs filter, not truncate at top-K"
    );
    let item = outcome.items_for_test().pop().expect("single matched item");
    assert!(
        !outcome
            .snippets
            .get(&(item.source_id.0, total_docs as u64))
            .is_none_or(Vec::is_empty),
        "surviving low-BM25 row past top-K must still receive snippets"
    );
}
