// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use super::super::*;
use crate::config::ServiceConfig;
use crate::sync_engine::MultiVolumeEngine;
use cairn_content::ContentWriter;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[test]
fn reset_cancel_stays_latched_after_shutdown_even_under_concurrency() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = ServiceConfig {
        content_index_enabled: true,
        ..Default::default()
    };
    let engine = Arc::new(MultiVolumeEngine::with_paths(
        &temp.path().join("index"),
        &temp.path().join("content"),
        &temp.path().join("service.toml"),
        vec![],
        cfg,
    ));

    let stop_engine = Arc::clone(&engine);
    let mut handle = None;
    for iteration in 0..32 {
        if iteration == 8 {
            let stop = Arc::clone(&stop_engine);
            handle = Some(std::thread::spawn(move || stop.stop_content_worker()));
        }
        let reset_ok = engine.content_pipeline.reset_cancel();
        if !reset_ok {
            assert!(engine.content_pipeline.is_cancelled());
        }
        std::thread::yield_now();
    }
    if let Some(h) = handle {
        h.join().unwrap();
    }

    assert!(!engine.content_pipeline.reset_cancel());
    assert!(engine.content_pipeline.is_cancelled());
}

#[test]
fn ensure_content_runtime_resumes_and_clear_inline_leaves_clean_state() {
    let temp = tempfile::tempdir().unwrap();
    let content_dir = temp.path().join("content");
    let cfg = ServiceConfig {
        content_index_enabled: true,
        ..Default::default()
    };

    {
        let writer = ContentWriter::open_or_create(&content_dir).unwrap();
        writer
            .index_document_deferred(1, 2, 11, 100, "C:\\sample.txt", "hello world")
            .unwrap();
        writer.commit().unwrap();
        writer.finish().unwrap();
    }

    let engine = Arc::new(MultiVolumeEngine::with_paths(
        &temp.path().join("index"),
        &content_dir,
        &temp.path().join("service.toml"),
        vec![],
        cfg.clone(),
    ));
    engine.content_pipeline.set_ready(1, 64);
    engine.content_pipeline.write_backfill_marker(&cfg);
    assert!(engine.backfill_marker_path().exists());

    let tmps: Vec<_> = std::fs::read_dir(&content_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(tmps.is_empty(), "leftover tmp markers: {tmps:?}");

    engine.ensure_content_runtime();
    let deadline = Instant::now() + Duration::from_secs(5);
    while engine.content_index_status().state != ContentIndexState::Ready {
        assert!(
            Instant::now() < deadline,
            "content thread did not resume in time"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    engine.stop_content_worker();

    let offline_engine = Arc::new(MultiVolumeEngine::with_paths(
        &temp.path().join("index2"),
        &content_dir,
        &temp.path().join("service2.toml"),
        vec![],
        ServiceConfig {
            content_index_enabled: false,
            ..Default::default()
        },
    ));
    assert!(offline_engine.request_clear_content_index());
    assert_eq!(
        offline_engine.content_index_status().state,
        ContentIndexState::Disabled
    );
    assert!(!offline_engine.backfill_marker_path().exists());
}
