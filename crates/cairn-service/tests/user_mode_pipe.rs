// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The user-privilege indexer as the desktop app uses it: real pipe, real protocol, real scan.

use cairn_client::client::CairnClient;
use cairn_protocol::message::{ResponsePayload, USER_PIPE_NAME};
use cairn_protocol::status::VolumeState;
use cairn_protocol::QueryDefaults;
use cairn_service::user_mode::{UserModeLayout, UserModeRuntime};
use std::time::{Duration, Instant};

#[tokio::test]
async fn a_user_mode_indexer_answers_queries_on_its_own_pipe() {
    let temp = tempfile::tempdir().expect("tempdir");
    let layout = UserModeLayout::new(&temp.path().join("data"));
    let tree = temp.path().join("tree");
    std::fs::create_dir_all(tree.join("docs")).expect("tree");
    std::fs::write(tree.join("docs").join("report.txt"), "report").expect("fixture");

    let runtime =
        UserModeRuntime::build_in(&layout, Some(vec![('C', tree)])).expect("user-mode runtime");
    let engine = runtime.engine.clone();
    let serving = tokio::spawn(runtime.run(std::future::pending()));

    // The scan runs beside the pipe; wait for the snapshot rather than racing it.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let summaries = engine.get_volume_summaries();
        if summaries
            .iter()
            .all(|summary| summary.status == VolumeState::SnapshotMode)
        {
            assert!(summaries[0].indexed_count >= 2, "{summaries:?}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "scan did not finish: {summaries:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let mut client = CairnClient::connect_to(USER_PIPE_NAME)
        .await
        .expect("the user pipe answers a standard user");
    let status = client.get_status().await.expect("status");
    assert!(status
        .volumes
        .iter()
        .all(|volume| volume.status == VolumeState::SnapshotMode));

    let response = client
        .query(1, "", QueryDefaults::default(), "name", true, 50)
        .await
        .expect("all-files query");
    match response.payload {
        ResponsePayload::QueryResult {
            total_matches,
            total_bytes,
            first_page,
            ..
        } => {
            assert!(
                total_matches >= 2,
                "the all-files view must return the scanned tree"
            );
            assert!(
                first_page.iter().any(|row| row.name == "report.txt"),
                "{first_page:?}"
            );
            assert_eq!(
                total_bytes, 6,
                "the all-files total must sum the visible file bytes"
            );
        }
        other => panic!("unexpected answer: {other:?}"),
    }

    let folder_response = client
        .query(2, "folder:", QueryDefaults::default(), "name", true, 50)
        .await
        .expect("folder query");
    match folder_response.payload {
        ResponsePayload::QueryResult {
            total_matches,
            total_bytes,
            first_page,
            ..
        } => {
            assert!(total_matches >= 1, "must match folder");
            assert!(first_page.iter().any(|row| row.name == "docs"));
            assert_eq!(
                total_bytes, 0,
                "directory rows do not contribute file bytes"
            );
        }
        other => panic!("unexpected answer: {other:?}"),
    }

    serving.abort();
}
