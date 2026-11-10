// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::config::ServiceConfig;
use crate::sync_engine::{MultiVolumeEngine, VolumeIndex};
use cairn_core::engine::CancellationToken;
use cairn_core::sort::SortColumn;
use cairn_core::syntax::QueryDefaults;

/// An engine with a real on-disk content index (four documents) and matching volume rows.
/// The tests below cross the public `execute_search_multi` seam, so the content combination
/// is exercised exactly as a client sees it.
fn engine_with_indexed_content() -> (MultiVolumeEngine, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("index dir");
    let config = ServiceConfig {
        content_index_enabled: true,
        ..Default::default()
    };
    let engine = MultiVolumeEngine::with_index_dir_drives_and_config(dir.path(), vec![], config);
    let writer =
        cairn_content::ContentIndex::open_or_create(&engine.content_dir).expect("content index");
    for (frn, path, body) in [
        (101, r"C:\docs\report.txt", "resumable needle"),
        (102, r"C:\docs\report-draft.txt", "quarterly summary"),
        (103, r"C:\docs\other.txt", "resumable needle"),
        (104, r"C:\docs\other-notes.txt", "plain filler"),
    ] {
        writer
            .index_document(2, frn, 16, 1, path, body)
            .expect("index document");
    }
    writer.finish().expect("finish content index");
    assert!(engine.get_or_create_content_searcher().is_some());
    engine.mark_content_index_ready_for_test();

    let mut volume = VolumeIndex::new('C');
    volume.ingest_items(vec![
        index_item(1, 0, "C:", true),
        index_item(9, 1, "docs", true),
        index_item(101, 9, "report.txt", false),
        index_item(102, 9, "report-draft.txt", false),
        index_item(103, 9, "other.txt", false),
        index_item(104, 9, "other-notes.txt", false),
    ]);
    engine.add_volume(volume);
    (engine, dir)
}

fn matched_content_frns(engine: &MultiVolumeEngine, query: &str) -> Vec<u64> {
    let cancel = CancellationToken::new();
    let (results, ..) = engine.execute_search_multi(
        query,
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    let volumes = engine.volumes.read().unwrap();
    let mut frns: Vec<u64> = results
        .iter()
        .map(|item| volumes[&item.drive_letter].records[item.record_idx as usize].frn)
        .collect();
    frns.sort_unstable();
    frns
}

#[test]
fn a_negated_content_term_excludes_its_hits_from_the_result_set() {
    let (engine, _dir) = engine_with_indexed_content();
    assert_eq!(
        matched_content_frns(&engine, "!content:needle"),
        vec![1, 9, 102, 104],
        "every record without the needle hit, directories included, must survive the negation"
    );
}

#[test]
fn a_metadata_or_content_branch_returns_the_union() {
    let (engine, _dir) = engine_with_indexed_content();
    assert_eq!(
        matched_content_frns(&engine, "report|content:needle"),
        vec![101, 102, 103],
        "the metadata branch and the content branch must union under Or"
    );
}

#[test]
fn a_metadata_and_content_conjunction_keeps_requiring_both() {
    let (engine, _dir) = engine_with_indexed_content();
    assert_eq!(
        matched_content_frns(&engine, "report content:needle"),
        vec![101]
    );
}

#[test]
fn a_pure_positive_content_query_returns_its_hits() {
    let (engine, _dir) = engine_with_indexed_content();
    assert_eq!(
        matched_content_frns(&engine, "content:needle"),
        vec![101, 103]
    );
}

#[test]
fn a_content_or_content_branch_returns_the_union() {
    let (engine, _dir) = engine_with_indexed_content();
    assert_eq!(
        matched_content_frns(&engine, "content:needle|content:quarterly"),
        vec![101, 102, 103]
    );
}

#[test]
fn a_negated_content_term_inside_a_metadata_conjunction_subtracts_hits() {
    let (engine, _dir) = engine_with_indexed_content();
    assert_eq!(
        matched_content_frns(&engine, "report !content:needle"),
        vec![102]
    );
}

#[test]
fn a_negated_content_term_without_hits_keeps_every_record() {
    let (engine, _dir) = engine_with_indexed_content();
    assert_eq!(
        matched_content_frns(&engine, "!content:absent"),
        vec![1, 9, 101, 102, 103, 104]
    );
}

#[test]
fn an_empty_content_term_matches_nothing() {
    let (engine, _dir) = engine_with_indexed_content();
    assert!(matched_content_frns(&engine, "content:").is_empty());
    assert!(matched_content_frns(&engine, "content:needle content:").is_empty());
}

#[test]
fn an_or_content_query_keeps_snippets_on_content_hit_rows() {
    let (engine, _dir) = engine_with_indexed_content();
    let cancel = CancellationToken::new();
    let (results, .., snippets, _) = engine.execute_search_multi(
        "content:needle|content:quarterly",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    assert_eq!(results.len(), 3);
    let volumes = engine.volumes.read().unwrap();
    for item in &results {
        let frn = volumes[&item.drive_letter].records[item.record_idx as usize].frn;
        assert!(
            snippets.contains_key(&(item.drive_letter, frn)),
            "row frn {frn} matched its content term and must carry a snippet"
        );
    }
}

#[test]
fn a_negated_content_term_never_supplies_a_content_snippet() {
    let (engine, _dir) = engine_with_indexed_content();
    let cancel = CancellationToken::new();
    let (results, .., snippets, _) = engine.execute_search_multi(
        "report|!content:needle",
        &QueryDefaults::default(),
        SortColumn::Name,
        true,
        &cancel,
    );
    let volumes = engine.volumes.read().unwrap();
    let mut frns: Vec<u64> = results
        .iter()
        .map(|item| volumes[&item.drive_letter].records[item.record_idx as usize].frn)
        .collect();
    frns.sort_unstable();
    assert_eq!(
        frns,
        vec![1, 9, 101, 102, 104],
        "the union of the report branch and the negated term must be present"
    );
    for item in &results {
        let frn = volumes[&item.drive_letter].records[item.record_idx as usize].frn;
        assert!(
            !snippets.contains_key(&(item.drive_letter, frn)),
            "row frn {frn} must not carry a snippet from a negated content term"
        );
    }
}

#[test]
fn a_cancelled_query_returns_no_rows_in_either_content_shape() {
    let (engine, _dir) = engine_with_indexed_content();
    for query in [
        "!content:needle",
        "content:needle|content:quarterly",
        "content:needle",
    ] {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (results, total, ..) = engine.execute_search_multi(
            query,
            &QueryDefaults::default(),
            SortColumn::Name,
            true,
            &cancel,
        );
        assert!(
            results.is_empty(),
            "{query} must not return partial rows after a cancel"
        );
        assert_eq!(total, 0, "{query} must not report a partial total");
    }
}
