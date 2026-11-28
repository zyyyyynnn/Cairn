// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use super::*;
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

    let writer = ContentIndex::open_or_create(engine.content_dir()).expect("content index");
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

    let outcome = engine.execute_search_multi(
        "content:架构",
        &QueryDefaults::default(),
        &sort(SortColumn::Name, true),
        &cancel,
    );
    let results = engine.test_items(outcome.rows);
    let total = outcome.total;
    let snippets = outcome.snippets;
    assert_eq!(total, 1);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].source_id.drive_letter(), Some('C'));
    assert_eq!(results[0].record_idx, 0);
    let c_source = SourceId::for_drive('C').unwrap();
    assert!(snippets.contains_key(&(c_source.0, 101)));

    let mut vol_d = VolumeIndex::new('D');
    vol_d.ingest_items(vec![ExtractedItem {
        frn: 101,
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

    let outcome = engine.execute_search_multi(
        "content:架构",
        &QueryDefaults::default(),
        &sort(SortColumn::Name, true),
        &cancel,
    );
    let results_mv = engine.test_items(outcome.rows);
    let total_mv = outcome.total;
    assert_eq!(total_mv, 1);
    assert_eq!(results_mv[0].source_id.drive_letter(), Some('C'));

    let outcome = engine.execute_search_multi(
        "ext:md content:架构",
        &QueryDefaults::default(),
        &sort(SortColumn::Name, true),
        &cancel,
    );
    let results_combined = engine.test_items(outcome.rows);
    let total_c = outcome.total;
    assert_eq!(total_c, 1);
    assert_eq!(results_combined.len(), 1);

    let outcome = engine.execute_search_multi(
        "ext:txt content:架构",
        &QueryDefaults::default(),
        &sort(SortColumn::Name, true),
        &cancel,
    );
    let results_mismatch = engine.test_items(outcome.rows);
    let total_m = outcome.total;
    assert_eq!(total_m, 0);
    assert!(results_mismatch.is_empty());

    let outcome = engine.execute_search_multi(
        "content:不存在的词",
        &QueryDefaults::default(),
        &sort(SortColumn::Name, true),
        &cancel,
    );
    let results_none = engine.test_items(outcome.rows);
    let total_n = outcome.total;
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

    let writer = ContentIndex::open_or_create(engine.content_dir()).expect("content index");
    writer
        .index_document_deferred(2, 101, 100, 100, r"C:\test.txt", "全文搜索关键词")
        .unwrap();
    writer.commit().expect("commit content index");
    writer.finish().expect("finish content index");
    engine.reopen_content_index_ready();
    assert!(engine.content_index_status().state.is_ready());

    let cancel = CancellationToken::new();
    let outcome = engine.execute_search_multi(
        "content:关键词",
        &QueryDefaults::default(),
        &sort(SortColumn::Name, true),
        &cancel,
    );
    let total = outcome.total;
    assert_eq!(total, 1);

    engine.clear_content_index().unwrap();

    let outcome = engine.execute_search_multi(
        "content:关键词",
        &QueryDefaults::default(),
        &sort(SortColumn::Name, true),
        &cancel,
    );
    let total_after = outcome.total;
    assert_eq!(total_after, 0);
}

#[test]
fn the_service_never_creates_a_content_index() {
    let index = tempfile::tempdir().expect("index");
    let engine = MultiVolumeEngine::with_index_dir_and_drives(index.path(), vec![]);

    assert!(engine.get_or_create_content_searcher().is_none());
    assert!(
        !engine.content_dir().exists(),
        "the query path must not create an index, a directory or a writer"
    );
}

#[test]
fn positive_content_conjunction_never_truncates_beyond_top_k_before_mft_filter() {
    let temp = tempfile::tempdir().expect("tempdir");
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
    engine.reopen_content_index_ready();

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
    let MatchSet::Items(items) = &outcome.rows else {
        panic!("a text query produces a materialized match list");
    };
    let item = items.last().expect("single matched item");
    assert!(
        !outcome
            .snippets
            .get(&(item.source_id.0, total_docs as u64))
            .is_none_or(Vec::is_empty),
        "surviving low-BM25 row past top-K must still receive snippets"
    );
}

#[cfg(windows)]
#[test]
fn clear_content_runs_even_after_business_cancel_and_stops_on_shutdown() {
    let temp = tempfile::tempdir().expect("tempdir");
    let index_dir = temp.path().join("index");
    let content_dir = temp.path().join("content");
    std::fs::create_dir_all(&content_dir).expect("content dir");
    std::fs::write(content_dir.join("seed.bin"), b"indexed bytes").expect("seed file");

    let args_log = temp.path().join("clear-args.txt");
    let fake_worker = temp.path().join("fake_clear_worker.cmd");
    std::fs::write(
        &fake_worker,
        format!(
            "@echo off\r\necho %* > \"{}\"\r\necho {{\"processed\":0,\"failed\":0}}\r\n",
            args_log.display()
        ),
    )
    .expect("write fake worker");

    let cfg = ServiceConfig {
        content_index_enabled: false,
        ..Default::default()
    };
    let engine = MultiVolumeEngine::with_paths(
        &index_dir,
        &content_dir,
        &temp.path().join("service.toml"),
        vec![],
        cfg,
    )
    .with_worker_locator(Arc::new(crate::sync_engine::FixedWorkerBinaryLocator(
        fake_worker,
    )));

    engine
        .content_pipeline
        .cancel_flag()
        .store(true, std::sync::atomic::Ordering::SeqCst);

    engine.run_clear_content();
    let logged = std::fs::read_to_string(&args_log).expect("worker should have been invoked");
    assert!(logged.contains("--clear"), "{logged}");
    let status = engine.content_index_status();
    assert_eq!(status.state, ContentIndexState::Disabled);
    assert_eq!(status.indexed_documents, 0);
    assert_eq!(status.index_bytes, 0);

    std::fs::remove_file(&args_log).expect("remove log");
    engine.stop_content_worker();
    engine.run_clear_content();
    assert!(
        !args_log.exists(),
        "clear after service shutdown must not spawn worker"
    );
    assert_eq!(
        engine.content_index_status().state,
        ContentIndexState::Disabled
    );
}
