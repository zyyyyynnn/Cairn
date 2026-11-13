// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use super::backfill::ContentRules;
use super::status::unix_now;
use super::worker_process::worker_exit_is_transient;
use super::*;
use crate::config::ServiceConfig;
use crate::sync_engine::{MultiVolumeEngine, VolumeIndex};
use cairn_content::ContentIndex;
use cairn_core::engine::CancellationToken;
use cairn_core::sort::SortColumn;
use cairn_core::syntax::QueryDefaults;
use cairn_ntfs::mft::ExtractedItem;
use cairn_ntfs::usn::{UsnChange, FILE_ATTRIBUTE_REPARSE_POINT};
use cairn_protocol::status::{ContentIndexState, ContentIndexTask};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// Builds volume `C` from relative paths (directories first, file last) and returns the file
/// FRNs in input order. Reparse points are expressed through `flags`, as the scanner does.
fn volume_with_files(files: &[(&[&str], u16)]) -> (VolumeIndex, Vec<u64>) {
    let mut items = Vec::new();
    let mut directories: HashMap<String, u64> = HashMap::new();
    let mut next_frn = 1u64;
    let mut file_frns = Vec::new();
    for (components, flags) in files {
        let mut parent_frn = 0u64;
        for depth in 1..components.len() {
            let key = components[..depth].join("\\").to_ascii_lowercase();
            if let Some(&frn) = directories.get(&key) {
                parent_frn = frn;
                continue;
            }
            let frn = next_frn;
            next_frn += 1;
            items.push(ExtractedItem {
                frn,
                parent_frn,
                name: components[depth - 1].to_string(),
                is_dir: true,
                size: 0,
                created_ts: 1,
                modified_ts: 1,
                flags: cairn_core::FLAG_DIRECTORY,
                is_reparse_point: false,
            });
            directories.insert(key, frn);
            parent_frn = frn;
        }
        let frn = next_frn;
        next_frn += 1;
        items.push(ExtractedItem {
            frn,
            parent_frn,
            name: components.last().expect("file name").to_string(),
            is_dir: false,
            size: 32,
            created_ts: 1,
            modified_ts: 1,
            flags: *flags,
            is_reparse_point: *flags & cairn_core::FLAG_REPARSE_POINT != 0,
        });
        file_frns.push(frn);
    }
    let mut volume = VolumeIndex::new('C');
    volume.ingest_items(items);
    (volume, file_frns)
}

fn content_config() -> ServiceConfig {
    ServiceConfig {
        content_index_extensions: ["json", "toml", "log", "txt"]
            .into_iter()
            .map(String::from)
            .collect(),
        content_index_max_bytes: 1024,
        content_excluded_paths: Vec::new(),
        ..Default::default()
    }
}

#[test]
fn service_data_files_are_never_content_tasks() {
    let (volume, frns) = volume_with_files(&[
        (&["ProgramData", "Cairn", "content", "meta.json"], 0),
        (&["PROGRAMDATA", "CAIRN", "CONTENT", "META.JSON"], 0),
        (&["ProgramData", "Cairn", "content", ".managed.json"], 0),
        (&["ProgramData", "Cairn", "service.toml"], 0),
        (&["ProgramData", "Cairn", "logs", "cairn.log"], 0),
        (&["Users", "Public", "notes.json"], 0),
    ]);
    let index = tempfile::tempdir().expect("index");
    let root = std::path::Path::new(r"C:\ProgramData\Cairn");
    let engine = MultiVolumeEngine::with_paths(
        index.path(),
        &root.join("content"),
        &root.join("service.toml"),
        vec![],
        content_config(),
    )
    .with_service_data_root(root);
    let cfg = engine.get_config();
    let index_of = |volume: &VolumeIndex, frn: u64| {
        volume.first_record_index_for_frn(frn).expect("record") as usize
    };

    for (i, frn) in frns.iter().take(5).enumerate() {
        assert!(
            engine
                .content_task(&volume, index_of(&volume, *frn), &ContentRules::new(&cfg))
                .is_none(),
            "the service's own data (case {i}) must never reach the content worker"
        );
    }
    let user_task = engine
        .content_task(
            &volume,
            index_of(&volume, frns[5]),
            &ContentRules::new(&cfg),
        )
        .expect("a user file outside the data root stays eligible");
    assert_eq!(user_task.path, r"C:\Users\Public\notes.json");

    engine.add_volume(volume);
    let changes = HashMap::from([('C', frns.iter().copied().collect::<HashSet<_>>())]);
    let (tasks, deletions) = engine.changed_content_tasks(&changes, &ContentRules::new(&cfg));
    assert_eq!(tasks.len(), 1, "only the user file is an incremental task");
    assert_eq!(tasks[0].path, r"C:\Users\Public\notes.json");
    assert!(
        deletions.is_empty(),
        "own writes must not become deletion commits, which would rewrite the index"
    );
}

#[test]
fn a_sibling_of_the_service_data_root_is_not_excluded() {
    let (volume, frns) = volume_with_files(&[(&["ProgramData", "Cairn2", "notes.json"], 0)]);
    let index = tempfile::tempdir().expect("index");
    let root = std::path::Path::new(r"C:\ProgramData\Cairn");
    let engine = MultiVolumeEngine::with_paths(
        index.path(),
        &root.join("content"),
        &root.join("service.toml"),
        vec![],
        content_config(),
    )
    .with_service_data_root(root);
    let index_of = |volume: &VolumeIndex, frn: u64| {
        volume.first_record_index_for_frn(frn).expect("record") as usize
    };

    let task = engine
        .content_task(
            &volume,
            index_of(&volume, frns[0]),
            &ContentRules::new(&engine.get_config()),
        )
        .expect("a directory whose name merely starts with Cairn is user content");
    assert_eq!(task.path, r"C:\ProgramData\Cairn2\notes.json");
}

#[test]
fn reparse_point_records_are_never_content_tasks() {
    let (volume, frns) = volume_with_files(&[
        (
            &["Users", "Public", "link.txt"],
            cairn_core::FLAG_REPARSE_POINT,
        ),
        (&["Users", "Public", "real.txt"], 0),
    ]);
    let index = tempfile::tempdir().expect("index");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(index.path(), vec![]);
    let cfg = content_config();
    let index_of = |volume: &VolumeIndex, frn: u64| {
        volume.first_record_index_for_frn(frn).expect("record") as usize
    };

    assert!(
        engine
            .content_task(
                &volume,
                index_of(&volume, frns[0]),
                &ContentRules::new(&cfg)
            )
            .is_none(),
        "a reparse point must not be opened through the worker"
    );
    assert!(
        engine
            .content_task(
                &volume,
                index_of(&volume, frns[1]),
                &ContentRules::new(&cfg)
            )
            .is_some(),
        "a regular file beside the reparse point stays eligible"
    );

    engine.add_volume(volume);
    let changes = HashMap::from([('C', frns.iter().copied().collect::<HashSet<_>>())]);
    let (tasks, _) = engine.changed_content_tasks(&changes, &ContentRules::new(&cfg));
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].path, r"C:\Users\Public\real.txt");
}

#[test]
fn a_created_reparse_point_never_becomes_a_content_task() {
    let (mut volume, _) = volume_with_files(&[(&["Users", "Public", "seed.txt"], 0)]);
    let public_frn = volume
        .records()
        .iter()
        .find(|record| {
            volume
                .arena()
                .get_name(record.name_offset, record.name_len)
                .eq_ignore_ascii_case("Public")
        })
        .expect("Public directory")
        .frn;

    volume.apply_usn_changes([UsnChange::Created {
        frn: 900,
        parent_frn: public_frn,
        name: "link.txt".to_string(),
        is_dir: false,
        attributes: FILE_ATTRIBUTE_REPARSE_POINT,
        ts: 5,
    }]);

    let index = |volume: &VolumeIndex, frn: u64| {
        volume.first_record_index_for_frn(frn).expect("record") as usize
    };
    let record = &volume.records()[index(&volume, 900)];
    assert!(record.is_reparse_point());
    assert!(!record.is_dir());

    let index_dir = tempfile::tempdir().expect("index");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(index_dir.path(), vec![]);
    engine.add_volume(volume);
    let changes = HashMap::from([('C', HashSet::from([900u64]))]);
    let (tasks, deletions) =
        engine.changed_content_tasks(&changes, &ContentRules::new(&engine.get_config()));
    assert!(
        tasks.is_empty(),
        "a created reparse point must not reach the content worker"
    );
    assert_eq!(deletions, vec![(2, 900)]);
}

#[test]
fn content_exclusions_skip_system_dependency_and_cache_trees() {
    let (volume, frns) = volume_with_files(&[
        (&["Users", "Alice", "Documents", "keep.txt"], 0),
        (&["Users", "Alice", "AppData", "Local", "cache.txt"], 0),
        (&["Program Files", "App", "readme.txt"], 0),
        (
            &["Workspace", "proj", "node_modules", "pkg", "index.txt"],
            0,
        ),
        (&["Windows", "Logs", "system.txt"], 0),
        (&["dev", ".cargo", "registry", "lib.txt"], 0),
        (&["dev", "proj", "target", "debug", "out.txt"], 0),
    ]);
    let index = tempfile::tempdir().expect("index");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(index.path(), vec![]);
    let cfg = backfill_config();
    let record_index = |frn: u64| volume.first_record_index_for_frn(frn).expect("record") as usize;

    let keep = engine
        .content_task(&volume, record_index(frns[0]), &ContentRules::new(&cfg))
        .expect("a user document must stay eligible");
    assert!(keep.path.contains("Documents"));

    for (position, frn) in frns.iter().enumerate().skip(1) {
        assert!(
            engine
                .content_task(&volume, record_index(*frn), &ContentRules::new(&cfg))
                .is_none(),
            "content task {position} is under an excluded tree"
        );
    }
}

#[test]
fn a_basic_info_change_to_a_reparse_point_queues_a_deletion() {
    let (mut volume, frns) = volume_with_files(&[(&["Users", "Public", "notes.txt"], 0)]);
    let frn = frns[0];

    volume.apply_usn_changes([UsnChange::BasicInfoChanged {
        frn,
        ts: 2,
        attributes: FILE_ATTRIBUTE_REPARSE_POINT,
    }]);

    let record = volume
        .records()
        .iter()
        .find(|record| record.frn == frn)
        .expect("record");
    assert!(
        record.is_reparse_point(),
        "the attributes must reach the record"
    );
    assert!(
        !record.is_dir(),
        "a file attribute set without DIRECTORY must not make it a directory"
    );
    assert!(
        volume.has_pending_data_change(frn),
        "the content pipeline must re-evaluate the changed record"
    );

    let index_dir = tempfile::tempdir().expect("index");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(index_dir.path(), vec![]);
    engine.add_volume(volume);
    let changes = HashMap::from([('C', HashSet::from([frn]))]);
    let (tasks, deletions) =
        engine.changed_content_tasks(&changes, &ContentRules::new(&engine.get_config()));
    assert!(
        tasks.is_empty(),
        "a record that became a reparse point must not be re-extracted"
    );
    assert_eq!(deletions, vec![(2, frn)]);
}

/// Startup replays the change journal into the pending sets and only then compacts the cached
/// table. Compaction must not swallow that replay: a create/modify would keep its stale
/// metadata and missing document, and a deletion would leave its body document behind.
#[test]
fn compaction_keeps_pending_work_replayed_before_startup() {
    let (mut volume, frns) = volume_with_files(&[
        (&["Users", "Public", "keep.txt"], 0),
        (&["Users", "Public", "gone.txt"], 0),
        (&["ProgramData", "Cairn", "content", "meta.json"], 0),
    ]);
    let keep = frns[0];
    let gone = frns[1];

    volume.apply_usn_changes([
        UsnChange::DataChanged { frn: keep },
        UsnChange::Renamed {
            frn: keep,
            parent_frn: 0,
            name: "old-name.txt".to_string(),
            is_dir: false,
            is_new_name: false,
        },
        UsnChange::Deleted { frn: gone },
    ]);

    // A cache written before the service data root was excluded still holds its files.
    let cairn = volume
        .records()
        .iter()
        .find(|record| volume.record_name(record) == "Cairn")
        .expect("Cairn directory")
        .frn;
    volume.set_excluded_roots([cairn]);

    assert!(
        volume
            .compact_with_policy(crate::sync_engine::volume_index::CompactPolicy {
                min_records: 1,
                min_droppable_percent: 25,
                min_arena_waste_percent: 25,
            })
            .is_some_and(|report| report.dropped_records > 0),
        "the tombstone and the service-data subtree must be dropped"
    );
    assert!(
        volume.records().iter().all(|record| record.frn != gone),
        "a compacted tombstone must not stay in the table"
    );
    assert!(volume.has_pending_data_change(keep));
    assert!(volume.has_pending_rename(keep));
    assert!(volume.has_pending_content_deletion(2, gone));
    assert!(
        volume.is_excluded_frn(cairn),
        "the exclusion itself is configuration and must survive"
    );

    // Both downstream consumers still find their work through the existing paths.
    let index = tempfile::tempdir().expect("index");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(index.path(), vec![]);
    let cfg = content_config();
    engine.add_volume(volume);

    let changes = engine.drain_pending_data_changes();
    let (tasks, ineligible) = engine.changed_content_tasks(&changes, &ContentRules::new(&cfg));
    assert_eq!(
        tasks.len(),
        1,
        "the change replayed before compaction still applies"
    );
    assert_eq!(tasks[0].frn, keep);

    let mut pending = engine.drain_pending_content_deletions();
    pending.extend(ineligible);
    let deletions = engine.resolve_content_deletions(pending, &ContentRules::new(&cfg));
    assert_eq!(
        deletions,
        vec![(2, gone)],
        "the deletion replayed before compaction must still remove its document"
    );
}

/// A USN `Created` record can arrive without any attribute bits. The debounce pass reads the
/// entry itself, so the record still learns that the path is a reparse point.
#[cfg(windows)]
#[test]
fn the_debounce_refresh_marks_a_created_symlink_as_reparse() {
    use std::os::windows::fs::symlink_file;
    use std::path::Component;

    let tree = tempfile::tempdir().expect("tree");
    let target = tree.path().join("target.txt");
    std::fs::write(&target, "payload").expect("target file");
    let link = tree.path().join("link.txt");
    if let Err(err) = symlink_file(&target, &link) {
        eprintln!("skipping symlink refresh test: {err}");
        return;
    }

    let mut items = Vec::new();
    let mut parent_frn = 0u64;
    let mut next_frn = 1u64;
    for component in tree.path().components() {
        let Component::Normal(name) = component else {
            continue;
        };
        items.push(ExtractedItem {
            frn: next_frn,
            parent_frn,
            name: name.to_string_lossy().into_owned(),
            is_dir: true,
            size: 0,
            created_ts: 1,
            modified_ts: 1,
            flags: cairn_core::FLAG_DIRECTORY,
            is_reparse_point: false,
        });
        parent_frn = next_frn;
        next_frn += 1;
    }

    items.push(ExtractedItem {
        frn: next_frn,
        parent_frn,
        name: "link.txt".to_string(),
        is_dir: false,
        size: 0,
        created_ts: 1,
        modified_ts: 1,
        flags: cairn_core::FLAG_HIDDEN,
        is_reparse_point: false,
    });

    let mut volume = VolumeIndex::new('C');
    volume.ingest_items(items);

    let index_dir = tempfile::tempdir().expect("index");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(index_dir.path(), vec![]);
    engine.add_volume(volume);
    engine.update_volume('C', |volume| {
        volume.apply_usn_changes([UsnChange::DataChanged { frn: next_frn }]);
    });

    engine.process_pending_data_changes();

    let record = engine
        .with_volumes(|volumes| {
            volumes
                .get(&'C')
                .expect("volume C")
                .records()
                .iter()
                .find(|record| record.frn == next_frn)
                .copied()
        })
        .expect("created record");
    assert!(
        record.is_reparse_point(),
        "metadata refresh must add the reparse flag a missing USN attribute hid"
    );
    assert!(!record.is_dir());
    assert!(
        record.is_hidden(),
        "refresh must keep flags it does not own"
    );
}

#[test]
fn test_pending_data_changes_tracking_and_drain() {
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![ExtractedItem {
        frn: 42,
        parent_frn: 5,
        name: "data.txt".to_string(),
        is_dir: false,
        size: 512,
        created_ts: 100,
        modified_ts: 100,
        flags: 0,
        is_reparse_point: false,
    }]);

    // A DataChanged event is debounced until the next drain.
    vol.apply_usn_changes([UsnChange::DataChanged { frn: 42 }]);
    assert!(vol.has_pending_data_change(42));

    let temp_dir = tempfile::tempdir().expect("tempdir");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(temp_dir.path(), vec![]);
    engine.add_volume(vol);

    let drained = engine.drain_pending_data_changes();
    assert_eq!(drained.get(&'C').map(|s| s.contains(&42)), Some(true));

    // After drain, pending set is empty.
    let drained_again = engine.drain_pending_data_changes();
    assert!(drained_again.is_empty());
}

#[test]
fn excluded_paths_are_not_sent_to_the_content_worker() {
    use std::path::Component;

    let tree = tempfile::tempdir().expect("tree");
    std::fs::create_dir_all(tree.path().join("skip")).expect("skip dir");
    std::fs::write(tree.path().join("keep.txt"), "keep").expect("keep file");
    std::fs::write(tree.path().join("skip").join("secret.txt"), "secret").expect("secret");

    let mut items = Vec::new();
    let mut parent_frn = 0u64;
    let mut next_frn = 1u64;
    for component in tree.path().components() {
        let Component::Normal(name) = component else {
            continue;
        };
        items.push(ExtractedItem {
            frn: next_frn,
            parent_frn,
            name: name.to_string_lossy().into_owned(),
            is_dir: true,
            size: 0,
            created_ts: 1,
            modified_ts: 1,
            flags: cairn_core::FLAG_DIRECTORY,
            is_reparse_point: false,
        });
        parent_frn = next_frn;
        next_frn += 1;
    }

    let keep_frn = next_frn;
    items.push(ExtractedItem {
        frn: keep_frn,
        parent_frn,
        name: "keep.txt".to_string(),
        is_dir: false,
        size: 4,
        created_ts: 1,
        modified_ts: 1,
        flags: 0,
        is_reparse_point: false,
    });
    next_frn += 1;
    let skip_dir_frn = next_frn;
    items.push(ExtractedItem {
        frn: skip_dir_frn,
        parent_frn,
        name: "skip".to_string(),
        is_dir: true,
        size: 0,
        created_ts: 1,
        modified_ts: 1,
        flags: cairn_core::FLAG_DIRECTORY,
        is_reparse_point: false,
    });
    next_frn += 1;
    let secret_frn = next_frn;
    items.push(ExtractedItem {
        frn: secret_frn,
        parent_frn: skip_dir_frn,
        name: "secret.txt".to_string(),
        is_dir: false,
        size: 6,
        created_ts: 1,
        modified_ts: 1,
        flags: 0,
        is_reparse_point: false,
    });

    let mut volume = VolumeIndex::new('C');
    volume.ingest_items(items);
    let cfg = ServiceConfig {
        content_index_extensions: vec!["txt".to_string()],
        content_index_max_bytes: 1024,
        excluded_paths: vec![tree.path().join("skip").to_string_lossy().into_owned()],
        content_excluded_paths: Vec::new(),
        ..Default::default()
    };
    let index_of = |frn: u64| volume.first_record_index_for_frn(frn).expect("record") as usize;
    let index = tempfile::tempdir().expect("index");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(index.path(), vec![]);

    assert!(
        engine
            .content_task(&volume, index_of(keep_frn), &ContentRules::new(&cfg))
            .is_some(),
        "an eligible file outside the rules must be indexed"
    );
    assert!(
        engine
            .content_task(&volume, index_of(secret_frn), &ContentRules::new(&cfg))
            .is_none(),
        "a file under an excluded path must never reach the content worker"
    );
}

/// A create event only says a name appeared. The row must still report the file's real size and
/// timestamps, and that has to happen with content indexing off, because the table shows it
/// either way.
#[test]
fn a_created_event_reads_the_real_size_and_time_from_disk() {
    use std::path::Component;

    let tree = tempfile::tempdir().expect("tree");
    let file = tree.path().join("created.bin");
    std::fs::write(&file, vec![7u8; 1234]).expect("fixture file");

    let mut items = Vec::new();
    let mut parent_frn = 0u64;
    let mut next_frn = 1u64;
    for component in tree.path().components() {
        let Component::Normal(name) = component else {
            continue;
        };
        items.push(ExtractedItem {
            frn: next_frn,
            parent_frn,
            name: name.to_string_lossy().into_owned(),
            is_dir: true,
            size: 0,
            created_ts: 0,
            modified_ts: 0,
            flags: cairn_core::FLAG_DIRECTORY,
            is_reparse_point: false,
        });
        parent_frn = next_frn;
        next_frn += 1;
    }

    let mut volume = VolumeIndex::new('C');
    volume.ingest_items(items);

    let index = tempfile::tempdir().expect("index");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(index.path(), vec![]);
    engine.add_volume(volume);
    engine.update_volume('C', |volume| {
        volume.apply_usn_changes([UsnChange::Created {
            frn: next_frn,
            parent_frn,
            name: "created.bin".to_string(),
            is_dir: false,
            attributes: 0,
            ts: 1,
        }])
    });

    let created_size = |engine: &MultiVolumeEngine| {
        engine.with_volumes(|volumes| {
            volumes
                .get(&'C')
                .expect("volume C")
                .records()
                .iter()
                .find(|record| record.frn == next_frn)
                .expect("created record")
                .size
        })
    };
    assert_eq!(
        created_size(&engine),
        0,
        "the journal record carries no size, so the row starts at zero"
    );

    engine.process_pending_data_changes();

    let (size, created_ts, modified_ts, parent_size) = engine.with_volumes(|volumes| {
        let records = volumes.get(&'C').expect("volume C").records();
        let record = records
            .iter()
            .find(|record| record.frn == next_frn)
            .expect("created record");
        let parent_size = records
            .iter()
            .find(|record| record.frn == parent_frn)
            .expect("parent record")
            .size;
        (
            record.size,
            record.created_ts,
            record.modified_ts,
            parent_size,
        )
    });
    assert_eq!(size, 1234);
    assert_eq!(
        parent_size, 1234,
        "a discovered file size must reach its directory aggregate"
    );
    assert!(created_ts > 0 && modified_ts > 0, "timestamps are FILETIME");
}

#[test]
fn test_content_query_intersection_with_mft() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let config = ServiceConfig {
        content_index_enabled: true,
        ..Default::default()
    };
    let engine =
        MultiVolumeEngine::with_index_dir_drives_and_config(temp_dir.path(), vec![], config);
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![
        ExtractedItem {
            frn: 101,
            parent_frn: 5,
            name: "architecture.md".to_string(),
            is_dir: false,
            size: 2048,
            created_ts: 100,
            modified_ts: 100,
            flags: 0,
            is_reparse_point: false,
        },
        ExtractedItem {
            frn: 102,
            parent_frn: 5,
            name: "guide.txt".to_string(),
            is_dir: false,
            size: 1024,
            created_ts: 100,
            modified_ts: 100,
            flags: 0,
            is_reparse_point: false,
        },
    ]);
    engine.add_volume(vol);

    // 2. Index documents with a writer (the worker's role), then publish a read-only searcher.
    let writer = ContentIndex::open_or_create(&engine.content_dir).expect("content index");
    writer
        .index_document_deferred(
            2,
            101,
            2048,
            100,
            r"C:\docs\architecture.md",
            "Cairn 架构设计与并发求交链路实现",
        )
        .expect("index doc 101");
    writer
        .index_document_deferred(
            2,
            102,
            1024,
            100,
            r"C:\docs\guide.txt",
            "使用指南与操作说明",
        )
        .expect("index doc 102");
    writer.commit().expect("commit content index");
    writer.finish().expect("finish content index");
    engine.reopen_content_index_ready();
    assert!(engine.content_index_status().state.is_ready());

    let cancel = CancellationToken::new();

    // Query 1: Pure content query "content:架构" -> should match architecture.md (frn 101 on C:)
    let (results, total, _, snippets, _) = engine.execute_search_multi(
        "content:架构",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total, 1);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].drive_letter, 'C');
    assert_eq!(results[0].record_idx, 0); // 101
    assert!(snippets.contains_key(&('C', 101)));

    // Multi-volume isolation: Add volume D with an unrelated file having identical FRN 101
    let mut vol_d = VolumeIndex::new('D');
    vol_d.ingest_items(vec![ExtractedItem {
        frn: 101, // Identical FRN on Drive D
        parent_frn: 5,
        name: "d_unrelated.txt".to_string(),
        is_dir: false,
        size: 500,
        modified_ts: 100,
        created_ts: 100,
        flags: 0,
        is_reparse_point: false,
    }]);
    engine.add_volume(vol_d);

    // Content search should STILL only return C:\docs\architecture.md and NOT D:\d_unrelated.txt
    let (results_mv, total_mv, ..) = engine.execute_search_multi(
        "content:架构",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total_mv, 1);
    assert_eq!(results_mv[0].drive_letter, 'C');

    // Query 2: Combined MFT filter and content "ext:md content:架构" -> matches 101
    let (results_combined, total_c, ..) = engine.execute_search_multi(
        "ext:md content:架构",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total_c, 1);
    assert_eq!(results_combined.len(), 1);

    // Query 3: Combined MFT filter mismatch "ext:txt content:架构" -> 0 matches
    let (results_mismatch, total_m, ..) = engine.execute_search_multi(
        "ext:txt content:架构",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total_m, 0);
    assert!(results_mismatch.is_empty());

    // Query 4: Content query for non-existent word -> 0 matches
    let (results_none, total_n, ..) = engine.execute_search_multi(
        "content:不存在的词",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total_n, 0);
    assert!(results_none.is_empty());
}

#[test]
fn test_clear_content_index_roundtrip() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let config = ServiceConfig {
        content_index_enabled: true,
        ..Default::default()
    };
    let engine =
        MultiVolumeEngine::with_index_dir_drives_and_config(temp_dir.path(), vec![], config);
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(vec![ExtractedItem {
        frn: 101,
        parent_frn: 5,
        name: "test.txt".to_string(),
        is_dir: false,
        size: 100,
        created_ts: 100,
        modified_ts: 100,
        flags: 0,
        is_reparse_point: false,
    }]);
    engine.add_volume(vol);

    let writer = ContentIndex::open_or_create(&engine.content_dir).expect("content index");
    writer
        .index_document_deferred(2, 101, 100, 100, r"C:\test.txt", "全文搜索关键词")
        .unwrap();
    writer.commit().expect("commit content index");
    writer.finish().expect("finish content index");
    engine.reopen_content_index_ready();
    assert!(engine.content_index_status().state.is_ready());

    let cancel = CancellationToken::new();
    let (_, total, ..) = engine.execute_search_multi(
        "content:关键词",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total, 1);

    // Clear content index (CNT-07)
    engine.clear_content_index().unwrap();

    let (_, total_after, ..) = engine.execute_search_multi(
        "content:关键词",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total_after, 0);
}

#[test]
fn the_service_never_creates_a_content_index() {
    let index = tempfile::tempdir().expect("index");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(index.path(), vec![]);

    assert!(engine.get_or_create_content_searcher().is_none());
    assert!(
        !engine.content_dir.exists(),
        "the query path must not create an index, a directory or a writer"
    );
}

#[test]
fn stale_batches_and_old_temps_are_removed_without_touching_index_files() {
    let index = tempfile::tempdir().expect("index");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(index.path(), vec![]);
    let dir = engine.content_dir.clone();
    std::fs::create_dir_all(&dir).expect("content dir");

    std::fs::write(dir.join("batch-1.jsonl"), "{}\n").expect("batch file");
    std::fs::write(dir.join("deletes-1.jsonl"), "[]\n").expect("delete batch file");
    std::fs::write(dir.join(".tmp-old"), "stale").expect("old temp file");
    let old = std::fs::File::options()
        .write(true)
        .open(dir.join(".tmp-old"))
        .expect("old temp file");
    old.set_modified(SystemTime::now() - Duration::from_secs(2 * 60 * 60))
        .expect("set old mtime");
    drop(old);
    std::fs::write(dir.join(".tmp-fresh"), "fresh").expect("fresh temp file");
    let index_files = [
        "meta.json",
        ".managed.json",
        "seg.idx",
        ".tantivy-writer.lock",
    ];
    for name in index_files {
        std::fs::write(dir.join(name), "keep").expect("index file");
    }

    engine.cleanup_stale_content_files();

    assert!(!dir.join("batch-1.jsonl").exists());
    assert!(!dir.join("deletes-1.jsonl").exists());
    assert!(!dir.join(".tmp-old").exists());
    assert!(
        dir.join(".tmp-fresh").exists(),
        "a recently written temp file may still be in use"
    );
    for name in index_files {
        assert!(
            dir.join(name).exists(),
            "cleanup must not delete index file {name}"
        );
    }
}

fn backfill_config() -> ServiceConfig {
    ServiceConfig {
        content_index_enabled: true,
        content_index_extensions: vec!["txt".to_string()],
        content_index_max_bytes: 1024,
        ..Default::default()
    }
}

fn backfill_marker_for(config: &ServiceConfig) -> BackfillMarker {
    BackfillMarker {
        schema: CONTENT_BACKFILL_SCHEMA,
        extensions: config.content_index_extensions.clone(),
        max_bytes: config.content_index_max_bytes,
        excluded_paths: config.excluded_paths.clone(),
        content_excluded_paths: config.content_excluded_paths.clone(),
        documents: 1,
        completed_ts: unix_now(),
    }
}

/// Builds a real on-disk index with one searchable document and records the completion marker
/// through the production writer, exactly as a finished backfill does.
fn engine_with_completed_backfill(
    index_dir: &std::path::Path,
    config: &ServiceConfig,
) -> MultiVolumeEngine {
    let engine =
        MultiVolumeEngine::with_index_dir_drives_and_config(index_dir, vec![], config.clone());
    let writer = ContentIndex::open_or_create(&engine.content_dir).expect("content index");
    writer
        .index_document_deferred(2, 101, 7, 1, r"C:\docs\note.txt", "resumable needle")
        .expect("index document");
    writer.commit().expect("commit content index");
    writer.finish().expect("finish content index");
    engine.reopen_content_index_ready();
    assert!(engine.content_index_status().state.is_ready());
    engine.write_backfill_marker(config);
    assert!(engine.backfill_marker_path().exists(), "marker was written");
    engine
}

/// The marker has to describe the configuration the backfill ran with. A change made while the
/// job is running cannot retroactively cover the files it already skipped, so the marker must
/// keep the old scope; the restart check then sees the difference and rebuilds.
#[test]
fn the_backfill_marker_keeps_the_configuration_the_backfill_ran_with() {
    let index = tempfile::tempdir().expect("index");
    let config = backfill_config();
    let engine =
        MultiVolumeEngine::with_index_dir_drives_and_config(index.path(), vec![], config.clone());
    let writer = ContentIndex::open_or_create(&engine.content_dir).expect("content index");
    writer
        .index_document_deferred(2, 101, 7, 1, r"C:\docs\note.txt", "resumable needle")
        .expect("index document");
    writer.commit().expect("commit content index");
    writer.finish().expect("finish content index");
    engine.reopen_content_index_ready();
    assert!(engine.content_index_status().state.is_ready());

    let snapshot = engine.get_config();
    let mut changed = snapshot.clone();
    changed.content_index_extensions = vec!["md".to_string()];
    changed
        .content_excluded_paths
        .push("changed-during-backfill".to_string());
    engine.update_config(changed);

    engine.write_backfill_marker(&snapshot);

    let marker: BackfillMarker =
        serde_json::from_slice(&std::fs::read(engine.backfill_marker_path()).expect("marker file"))
            .expect("decode marker");
    assert_eq!(
        marker.extensions, snapshot.content_index_extensions,
        "the marker must describe the scope the backfill actually indexed"
    );
    assert_eq!(marker.max_bytes, snapshot.content_index_max_bytes);
    assert_eq!(marker.excluded_paths, snapshot.excluded_paths);
    assert_eq!(
        marker.content_excluded_paths,
        snapshot.content_excluded_paths
    );
}

#[test]
fn only_file_handle_conflicts_count_as_transient_worker_failures() {
    for exit_code in [5, 32] {
        assert!(
            worker_exit_is_transient(Some(exit_code)),
            "worker exit {exit_code} is a sharing conflict that must be retried once"
        );
    }
    for exit_code in [None, Some(0), Some(1), Some(2), Some(0xc0000409_u32 as i32)] {
        assert!(
            !worker_exit_is_transient(exit_code),
            "{exit_code:?} is not a sharing conflict"
        );
    }
}

/// A single bad file (including a worker crash) must not turn the whole content index into
/// Error. The old searcher keeps answering from its last committed snapshot, and removing the
/// resume marker makes the next start rebuild the gaps that this batch could not apply.
#[test]
fn a_failed_incremental_batch_keeps_the_ready_snapshot() {
    let index = tempfile::tempdir().expect("index");
    let config = backfill_config();
    let engine = engine_with_completed_backfill(index.path(), &config);
    let before = engine.content_index_status();
    assert_eq!(before.state, ContentIndexState::Ready);
    assert!(engine.backfill_marker_path().exists());
    let searcher = engine
        .content_searcher()
        .expect("a completed backfill publishes a searcher");

    let tasks = vec![ContentIndexTask {
        volume_id: 2,
        frn: 101,
        size: 8,
        modified_ts: 1,
        path: r"C:\docs\new.txt".to_string(),
    }];
    engine.run_incremental_with(tasks, Vec::new(), |_, _, _| {
        // A crashed worker (WER 0xc0000409) is a permanent failure for this batch, but still
        // not a reason to disable content search for every other file.
        BatchOutcome::failed("CairnWorker exited with 0xc0000409")
    });

    let after = engine.content_index_status();
    assert_eq!(after.state, ContentIndexState::Ready);
    assert_eq!(after.indexed_documents, before.indexed_documents);
    let retained = engine
        .content_searcher()
        .expect("the last committed snapshot must keep serving queries");
    assert!(
        Arc::ptr_eq(&searcher, &retained),
        "the failure must not replace the published searcher"
    );
    assert!(
        !engine.backfill_marker_path().exists(),
        "the next start must rebuild instead of resuming an incomplete index"
    );
}

#[test]
fn a_completed_backfill_is_reused_on_restart_without_a_writer() {
    let index = tempfile::tempdir().expect("index");
    let config = backfill_config();
    let engine = engine_with_completed_backfill(index.path(), &config);
    // A writer creates this lock file. The resume path must not recreate it.
    let writer_lock = engine.content_dir.join(".tantivy-writer.lock");
    let _ = std::fs::remove_file(&writer_lock);
    drop(engine);

    let engine = MultiVolumeEngine::with_index_dir_drives_and_config(index.path(), vec![], config);
    assert!(
        engine.try_resume_content_index(),
        "a completed backfill must be reusable after a restart"
    );
    let status = engine.content_index_status();
    assert_eq!(status.state, ContentIndexState::Ready);
    assert_eq!(status.indexed_documents, 1);
    let searcher = engine
        .get_or_create_content_searcher()
        .expect("the resumed index must answer queries");
    let hits = searcher.search("needle", 10).expect("query");
    assert_eq!(hits.len(), 1, "the indexed document must stay searchable");
    assert!(
        !writer_lock.exists(),
        "resuming the index must never open a writer"
    );
}

#[test]
fn a_marker_that_disagrees_with_the_configuration_is_discarded() {
    let index = tempfile::tempdir().expect("index");
    let config = backfill_config();
    let prepared = engine_with_completed_backfill(index.path(), &config);
    let valid = backfill_marker_for(&config);
    drop(prepared);

    let engine = MultiVolumeEngine::with_index_dir_drives_and_config(index.path(), vec![], config);
    let write_marker = |marker: &BackfillMarker| {
        std::fs::write(
            engine.backfill_marker_path(),
            serde_json::to_vec(marker).expect("encode marker"),
        )
        .expect("write marker");
    };

    let mut wrong_schema = valid.clone();
    wrong_schema.schema += 1;
    write_marker(&wrong_schema);
    assert!(
        !engine.try_resume_content_index(),
        "an unknown marker schema must force a rebuild"
    );
    assert!(
        !engine.backfill_marker_path().exists(),
        "a rejected marker must be deleted"
    );

    let mut wrong_extensions = valid.clone();
    wrong_extensions.extensions = vec!["md".to_string()];
    write_marker(&wrong_extensions);
    assert!(
        !engine.try_resume_content_index(),
        "changed extensions must force a rebuild"
    );
    assert!(!engine.backfill_marker_path().exists());

    let mut wrong_max_bytes = valid.clone();
    wrong_max_bytes.max_bytes += 1;
    write_marker(&wrong_max_bytes);
    assert!(
        !engine.try_resume_content_index(),
        "a changed size limit must force a rebuild"
    );
    assert!(!engine.backfill_marker_path().exists());

    let mut wrong_exclusions = valid.clone();
    wrong_exclusions
        .excluded_paths
        .push(r"C:\extra".to_string());
    write_marker(&wrong_exclusions);
    assert!(
        !engine.try_resume_content_index(),
        "changed exclusions must force a rebuild"
    );
    assert!(!engine.backfill_marker_path().exists());
}

#[test]
fn a_marker_without_a_usable_index_is_discarded() {
    let config = backfill_config();

    // The marker exists but no index was ever written.
    let missing = tempfile::tempdir().expect("index");
    let engine =
        MultiVolumeEngine::with_index_dir_drives_and_config(missing.path(), vec![], config.clone());
    std::fs::create_dir_all(&engine.content_dir).expect("content dir");
    std::fs::write(
        engine.backfill_marker_path(),
        serde_json::to_vec(&backfill_marker_for(&config)).expect("encode marker"),
    )
    .expect("write marker");
    assert!(
        !engine.try_resume_content_index(),
        "a marker without an index must not produce Ready"
    );
    assert!(
        !engine.backfill_marker_path().exists(),
        "the unusable marker must be deleted"
    );
    assert!(!engine.content_index_status().state.is_ready());

    // The index opens but holds no documents.
    let empty = tempfile::tempdir().expect("index");
    let engine =
        MultiVolumeEngine::with_index_dir_drives_and_config(empty.path(), vec![], config.clone());
    let writer = ContentIndex::open_or_create(&engine.content_dir).expect("content index");
    writer.finish().expect("finish empty index");
    engine.reopen_content_index_ready();
    assert!(engine.content_index_status().state.is_ready());
    engine.write_backfill_marker(&config);
    assert!(engine.backfill_marker_path().exists());
    assert!(
        !engine.try_resume_content_index(),
        "an empty index is not a completed backfill"
    );
    assert!(!engine.backfill_marker_path().exists());
}

/// A USN burst writes through the worker while the service keeps answering `content:` queries.
/// The published searcher must not be dropped when the incremental batch starts, and the
/// end-of-batch reopen must publish the new count without a Backfilling gap.
#[test]
fn an_incremental_commit_keeps_the_index_searchable_until_reopen() {
    let index = tempfile::tempdir().expect("index");
    let engine = MultiVolumeEngine::with_index_dir_drives_and_config(
        index.path(),
        vec![],
        backfill_config(),
    );

    // Finished backfill: one committed document and a published read-only searcher.
    let writer = ContentIndex::open_or_create(&engine.content_dir).expect("content index");
    writer
        .index_document_deferred(2, 101, 7, 1, r"C:\docs\first.txt", "first needle")
        .expect("index first document");
    writer.commit().expect("commit content index");
    writer.finish().expect("finish content index");
    engine.reopen_content_index_ready();
    assert!(engine.content_index_status().state.is_ready());
    assert!(engine.content_index_status().state.is_ready());

    // The worker commits its batch while the service still holds the existing reader.
    let worker =
        ContentIndex::open_or_create(&engine.content_dir).expect("incremental content index");
    worker
        .index_document_deferred(2, 102, 7, 1, r"C:\docs\second.txt", "second needle")
        .expect("index second document");
    worker.commit().expect("commit incremental batch");
    worker.finish().expect("finish incremental batch");

    assert!(
        engine.content_index_status().state.is_ready(),
        "an incremental commit must not take the published index out of Ready"
    );
    let searcher = engine
        .get_or_create_content_searcher()
        .expect("the published searcher must survive the batch");
    assert_eq!(
        searcher.search("needle", 10).expect("search").len(),
        2,
        "the reader must keep serving after the worker commit"
    );

    // Reopening after the batch publishes the new document count and stays Ready.
    engine.reopen_content_index_ready();
    let status = engine.content_index_status();
    assert_eq!(status.state, ContentIndexState::Ready);
    assert_eq!(status.indexed_documents, 2);
    let searcher = engine
        .get_or_create_content_searcher()
        .expect("reopened searcher");
    assert_eq!(searcher.search("needle", 10).expect("search").len(), 2);
}

#[test]
fn cleanup_keeps_the_backfill_marker_and_remove_deletes_it() {
    let index = tempfile::tempdir().expect("index");
    let config = backfill_config();
    let engine =
        MultiVolumeEngine::with_index_dir_drives_and_config(index.path(), vec![], config.clone());
    std::fs::create_dir_all(&engine.content_dir).expect("content dir");
    std::fs::write(
        engine.backfill_marker_path(),
        serde_json::to_vec(&backfill_marker_for(&config)).expect("encode marker"),
    )
    .expect("write marker");
    std::fs::write(engine.content_dir.join(CONTENT_BACKFILL_MARKER_TMP), "{}")
        .expect("write tmp marker");

    // Age both files beyond the stale threshold: the completion marker must survive any age,
    // while a leftover half-written replacement is cleaned up.
    for name in [CONTENT_BACKFILL_MARKER, CONTENT_BACKFILL_MARKER_TMP] {
        let file = std::fs::File::options()
            .write(true)
            .open(engine.content_dir.join(name))
            .expect("marker file");
        file.set_modified(SystemTime::now() - Duration::from_secs(2 * 60 * 60))
            .expect("set old mtime");
    }
    engine.cleanup_stale_content_files();
    assert!(
        engine.backfill_marker_path().exists(),
        "cleanup must keep the completed-backfill marker"
    );
    assert!(
        !engine
            .content_dir
            .join(CONTENT_BACKFILL_MARKER_TMP)
            .exists(),
        "a stale half-written marker is removed"
    );

    engine.remove_backfill_marker();
    assert!(!engine.backfill_marker_path().exists());
}

#[test]
fn a_content_command_without_a_receiver_reports_an_error() {
    let index = tempfile::tempdir().expect("index");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(index.path(), vec![]);
    let (tx, rx) = std::sync::mpsc::channel();
    drop(rx);
    *engine.content_commands.lock().unwrap() = Some(tx);

    assert!(
        !engine.send_content_command(ContentCommand::Start),
        "a command whose receiver is gone was not delivered"
    );
    match engine.content_index_status().state {
        ContentIndexState::Error(message) => {
            assert!(message.contains("stopped unexpectedly"), "{message}");
        }
        other => panic!("a lost content command must be observable, got {other:?}"),
    }
}

/// The workspace-built content worker, injected into the engine's test seam so
/// the harness location never decides how the service finds its worker.
fn content_worker_path() -> std::path::PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(std::path::Path::parent)
                .expect("workspace root")
                .join("target")
        });
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
    let worker = target.join(profile).join(name);
    assert!(
        worker.is_file(),
        "build the content worker first: {}",
        worker.display()
    );
    worker
}

/// End-to-end backfill: enables the switch, waits for the controller, and verifies that a file
/// which existed before enabling becomes searchable. Ignored by default because it spawns the real
/// `CairnWorker`; the Service job in `.github/workflows/ci.yml` builds the worker and runs it
/// explicitly.
#[test]
#[ignore = "requires target/debug/CairnWorker.exe"]
fn a_full_backfill_reaches_ready_and_indexes_existing_files() {
    use std::path::Component;

    let tree = tempfile::tempdir().expect("tree");
    let file = tree.path().join("hello.txt");
    std::fs::write(&file, "hello full backfill").expect("fixture file");

    let mut items = Vec::new();
    let mut parent_frn = 0u64;
    let mut next_frn = 1u64;
    for component in tree.path().components() {
        let Component::Normal(name) = component else {
            continue;
        };
        items.push(ExtractedItem {
            frn: next_frn,
            parent_frn,
            name: name.to_string_lossy().into_owned(),
            is_dir: true,
            size: 0,
            created_ts: 1,
            modified_ts: 1,
            flags: cairn_core::FLAG_DIRECTORY,
            is_reparse_point: false,
        });
        parent_frn = next_frn;
        next_frn += 1;
    }
    let hello_frn = next_frn;
    items.push(ExtractedItem {
        frn: hello_frn,
        parent_frn,
        name: "hello.txt".to_string(),
        is_dir: false,
        size: 19,
        created_ts: 1,
        modified_ts: 1,
        flags: 0,
        is_reparse_point: false,
    });

    let mut volume = VolumeIndex::new('C');
    volume.ingest_items(items);

    let index = tempfile::tempdir().expect("index");
    let config = ServiceConfig {
        content_index_enabled: true,
        content_index_extensions: vec!["txt".to_string()],
        content_index_max_bytes: 1024 * 1024,
        // The fixture tree lives under %TEMP%, which the shipped content exclusions cover; this
        // test exercises the backfill pipeline, not the default rule set.
        excluded_paths: Vec::new(),
        content_excluded_paths: Vec::new(),
        ..Default::default()
    };
    let mut engine =
        MultiVolumeEngine::with_index_dir_drives_and_config(index.path(), vec![], config);
    engine.set_worker_binary(content_worker_path());
    let engine = Arc::new(engine);
    engine.add_volume(volume);
    engine.ensure_content_runtime();

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = engine.content_index_status();
        match &status.state {
            ContentIndexState::Ready => {
                assert!(
                    status.indexed_documents >= 1,
                    "ready index must contain the existing file: {status:?}"
                );
                break;
            }
            ContentIndexState::Error(err) => panic!("backfill failed: {err}"),
            _ => {}
        }
        assert!(
            Instant::now() < deadline,
            "backfill did not finish: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    let cancel = CancellationToken::new();
    let (_, total, ..) = engine.execute_search_multi(
        "content:hello",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total, 1, "the pre-existing file must be searchable");

    // A file created after the ready state is queued through the USN path and sent to a fresh
    // worker batch; it must become searchable without a second manual backfill.
    let later = tree.path().join("later.txt");
    std::fs::write(&later, "later needle content").expect("later fixture");
    let later_frn = next_frn + 1;
    engine.update_volume('C', |volume| {
        volume.append_items(vec![ExtractedItem {
            frn: later_frn,
            parent_frn,
            name: "later.txt".to_string(),
            is_dir: false,
            size: 19,
            created_ts: 1,
            modified_ts: 1,
            flags: 0,
            is_reparse_point: false,
        }]);
        volume.apply_usn_changes([UsnChange::DataChanged { frn: later_frn }]);
    });
    engine.process_pending_data_changes();

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = engine.content_index_status();
        if status.state.is_ready() && status.indexed_documents >= 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "incremental indexing did not finish: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let (_, total, ..) = engine.execute_search_multi(
        "content:needle",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total, 1, "the new file must be searchable");

    // A deleted file must lose its body document; keeping it would return stale content and
    // could expose a previous file after NTFS reuses the FRN.
    std::fs::remove_file(&later).expect("remove later fixture");
    engine.update_volume('C', |volume| {
        volume.apply_usn_changes([UsnChange::Deleted { frn: later_frn }]);
    });
    engine.process_pending_data_changes();

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = engine.content_index_status();
        if status.state.is_ready() && status.indexed_documents == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "content deletion did not finish: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let (_, total, ..) = engine.execute_search_multi(
        "content:needle",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total, 0, "a deleted file must not stay searchable");

    // Renaming out of the whitelist is a deletion, not a stale document under the old name.
    engine.update_volume('C', |volume| {
        volume.apply_usn_changes([UsnChange::Renamed {
            frn: hello_frn,
            parent_frn,
            name: "hello.bin".to_string(),
            is_dir: false,
            is_new_name: true,
        }]);
    });
    engine.process_pending_data_changes();

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = engine.content_index_status();
        if status.state.is_ready() && status.indexed_documents == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "whitelist rename deletion did not finish: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let (_, total, ..) = engine.execute_search_multi(
        "content:hello",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(total, 0, "a renamed-out file must not stay searchable");
}

#[test]
fn worker_port_full_backfill_retries_transient_failure_without_sleeping() {
    use super::worker_process::WorkerProcessPort;
    use std::cell::RefCell;

    struct FakeWorkerPort<'a> {
        engine: &'a MultiVolumeEngine,
        calls: RefCell<Vec<usize>>,
        retry_waits: RefCell<usize>,
    }

    impl WorkerProcessPort for FakeWorkerPort<'_> {
        fn run_batch(
            &self,
            tasks: &[ContentIndexTask],
            _deletions: &[(u64, u64)],
            clear: bool,
        ) -> BatchOutcome {
            let mut calls = self.calls.borrow_mut();
            calls.push(tasks.len());
            if calls.len() == 1 {
                return BatchOutcome::worker_failed(
                    "transient sharing violation",
                    Some(i32::from(cairn_protocol::worker_exit::SHARING_VIOLATION)),
                );
            }
            if !tasks.is_empty() || clear {
                let writer =
                    ContentIndex::open_or_create(&self.engine.content_dir).expect("content dir");
                if clear {
                    writer.clear().expect("clear");
                }
                for task in tasks {
                    writer
                        .index_document_deferred(
                            task.volume_id,
                            task.frn,
                            task.size,
                            task.modified_ts,
                            &task.path,
                            "fake indexed body",
                        )
                        .expect("index doc");
                }
                writer.commit().expect("commit");
                writer.finish().expect("finish");
            }
            BatchOutcome::Completed
        }

        fn wait_before_retry(&self) -> bool {
            *self.retry_waits.borrow_mut() += 1;
            true
        }
    }

    let dir = tempfile::tempdir().expect("dir");
    let (vol, _) = volume_with_files(&[(&["docs", "note.txt"], 0)]);
    let config = backfill_config();
    let engine = MultiVolumeEngine::with_index_dir_drives_and_config(dir.path(), vec![], config);
    engine.add_volume(vol);

    let port = FakeWorkerPort {
        engine: &engine,
        calls: RefCell::new(Vec::new()),
        retry_waits: RefCell::new(0),
    };
    engine.run_full_backfill_with(&port);

    assert_eq!(&*port.calls.borrow(), &[1, 1]);
    assert_eq!(*port.retry_waits.borrow(), 1);
    let status = engine.content_index_status();
    assert_eq!(status.state, ContentIndexState::Ready);
    assert_eq!(status.indexed_documents, 1);
    assert!(engine.backfill_marker_path().exists());

    // Cancelling during retry backoff aborts without running a duplicate worker batch.
    struct CancelOnWaitPort {
        calls: RefCell<usize>,
    }

    impl WorkerProcessPort for CancelOnWaitPort {
        fn run_batch(
            &self,
            _tasks: &[ContentIndexTask],
            _deletions: &[(u64, u64)],
            clear: bool,
        ) -> BatchOutcome {
            assert!(clear);
            *self.calls.borrow_mut() += 1;
            BatchOutcome::worker_failed(
                "transient access denied",
                Some(i32::from(cairn_protocol::worker_exit::ACCESS_DENIED)),
            )
        }

        fn wait_before_retry(&self) -> bool {
            false
        }
    }

    let cancel_port = CancelOnWaitPort {
        calls: RefCell::new(0),
    };
    engine.run_clear_content_with(&cancel_port);
    assert_eq!(*cancel_port.calls.borrow(), 1);
    assert_eq!(
        engine.content_index_status().state,
        ContentIndexState::Disabled
    );
    assert!(!engine.backfill_marker_path().exists());
}
