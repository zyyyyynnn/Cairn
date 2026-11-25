// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::config::ServiceConfig;
use cairn_core::sort::SortColumn;
use cairn_protocol::message::ERROR_CODE_ADMIN_ONLY_QUERY;

struct Harness {
    _dirs: tempfile::TempDir,
    engine: Arc<MultiVolumeEngine>,
    sessions: SessionManager,
}

impl Harness {
    fn new(config: ServiceConfig) -> Self {
        let dirs = tempfile::tempdir().expect("tempdir");
        let root = dirs.path().to_path_buf();
        let engine = Arc::new(MultiVolumeEngine::with_paths(
            &root.join("index"),
            &root.join("content"),
            &root.join("service.toml"),
            vec![],
            config,
        ));
        Self {
            _dirs: dirs,
            engine,
            sessions: SessionManager::default(),
        }
    }

    fn handle(&self, req: ClientRequest, caller: CallerAuthorization) -> ResponsePayload {
        self.handle_with_protocol_version(req, caller, PROTOCOL_VERSION)
    }

    fn handle_with_protocol_version(
        &self,
        req: ClientRequest,
        caller: CallerAuthorization,
        protocol_version: u16,
    ) -> ResponsePayload {
        let admin_only_query = self.engine.admin_only_query();
        process_request(
            req,
            &self.engine,
            &self.sessions,
            caller,
            &QueryOwner::default(),
            admin_only_query,
            protocol_version,
        )
    }
}

fn denial(payload: ResponsePayload) -> (u32, String) {
    match payload {
        ResponsePayload::Error { code, message } => (code, message),
        other => panic!("expected a denial, got {other:?}"),
    }
}

#[test]
fn index_configuration_requests_are_accepted_from_a_standard_user_caller() {
    for req in [
        ClientRequest::ReloadConfig,
        ClientRequest::ClearContentIndex,
        ClientRequest::Rescan { volume_id: None },
        ClientRequest::SetConfig {
            config_toml: "content_index_enabled = true".to_string(),
        },
    ] {
        let harness = Harness::new(ServiceConfig::default());
        if let ResponsePayload::Error { code, message } =
            harness.handle(req.clone(), CallerAuthorization::StandardUser)
        {
            panic!("{req:?} must be a local-user operation, got {code}: {message}")
        }
    }
}

#[test]
fn a_standard_user_can_set_index_configuration_but_not_admin_only_query() {
    let harness = Harness::new(ServiceConfig::default());

    // The authorization boundary is what this test can decide without a LocalSystem process:
    // an index change from a standard user must not be a 403. Persisting the document requires
    // the service's SYSTEM/Administrators descriptor, which the real service (LocalSystem)
    // exercises in the installer test rather than in this unprivileged unit process.
    let payload = harness.handle(
        ClientRequest::SetConfig {
            config_toml: "content_index_enabled = true".to_string(),
        },
        CallerAuthorization::StandardUser,
    );
    match payload {
        ResponsePayload::SetConfigAck { .. } => {}
        ResponsePayload::Error { code, message } => {
            panic!(
                "a standard user must be authorized to change index configuration: {code}: {message}"
            )
        }
        other => panic!("unexpected response: {other:?}"),
    }

    let payload = harness.handle(
        ClientRequest::SetConfig {
            config_toml: "admin_only_query = true".to_string(),
        },
        CallerAuthorization::StandardUser,
    );
    match payload {
        ResponsePayload::SetConfigAck { success, message } => {
            assert!(!success);
            assert!(message.contains("administrator"), "{message}");
        }
        other => panic!("unexpected response: {other:?}"),
    }
    assert!(
        !harness.engine.get_config().admin_only_query,
        "the refused configuration must never reach the engine"
    );
}

/// Keeping the current value is not a change to the switch, so the authorization gate lets it
/// through; only the protected write of `service.toml` can still refuse it in this process.
#[test]
fn a_standard_user_may_submit_the_current_admin_only_query_value() {
    let harness = Harness::new(ServiceConfig::default());
    match harness.handle(
        ClientRequest::SetConfig {
            config_toml: "admin_only_query = false".to_string(),
        },
        CallerAuthorization::StandardUser,
    ) {
        ResponsePayload::SetConfigAck { message, .. } => assert!(
            !message.contains("administrator"),
            "a same-value submission must not be refused as an administrative change: {message}"
        ),
        other => panic!("unexpected response: {other:?}"),
    }
    assert!(!harness.engine.get_config().admin_only_query);
}

#[test]
fn an_administrator_may_change_admin_only_query() {
    let harness = Harness::new(ServiceConfig::default());
    match harness.handle(
        ClientRequest::SetConfig {
            config_toml: "admin_only_query = true".to_string(),
        },
        CallerAuthorization::Administrator,
    ) {
        ResponsePayload::SetConfigAck { success, message } => {
            if success {
                assert!(harness.engine.get_config().admin_only_query);
            } else {
                assert!(
                    message.contains("could not persist"),
                    "a privileged change must only be refused by the protected write: {message}"
                );
            }
        }
        other => panic!("unexpected response: {other:?}"),
    }
}

#[test]
fn reload_config_is_applied_for_an_administrator_caller() {
    let harness = Harness::new(ServiceConfig::default());
    // The harness config path does not exist yet, so a reload resolves to the shipped defaults.
    let payload = harness.handle(
        ClientRequest::ReloadConfig,
        CallerAuthorization::Administrator,
    );
    match payload {
        ResponsePayload::ReloadConfigAck { success, message } => {
            assert!(success, "{message}");
            assert!(!message.is_empty());
        }
        other => panic!("unexpected response: {other:?}"),
    }
}

#[test]
fn admin_only_query_switch_denies_reads_for_standard_users_immediately() {
    // The switch is read from the engine's live configuration, which is the same state the
    // authorization of a request uses; enabling it through the engine is what a ReloadConfig does.
    let harness = Harness::new(ServiceConfig {
        admin_only_query: true,
        ..ServiceConfig::default()
    });

    for req in [
        ClientRequest::Query {
            query_id: 1,
            raw_query: "rs".to_string(),
            defaults: Default::default(),
            sort_spec: SortSpec::default(),
            first_page_limit: 10,
            source_scope: None,
        },
        ClientRequest::GetRows {
            session_id: 1,
            offset: 0,
            limit: 10,
        },
        ClientRequest::GetConfig,
        ClientRequest::GetStatus,
    ] {
        let (code, _) = denial(harness.handle(req, CallerAuthorization::StandardUser));
        assert_eq!(code, ERROR_CODE_ADMIN_ONLY_QUERY);
    }

    // Administrators keep full access, and a standard user may still negotiate and cancel.
    assert!(matches!(
        harness.handle(ClientRequest::GetConfig, CallerAuthorization::Administrator),
        ResponsePayload::ConfigResult { .. }
    ));
    assert!(matches!(
        harness.handle(
            ClientRequest::Hello {
                client_protocol_version: PROTOCOL_VERSION as u32,
                pid: 1,
            },
            CallerAuthorization::StandardUser
        ),
        ResponsePayload::HelloAck { .. }
    ));
    assert!(matches!(
        harness.handle(
            ClientRequest::Cancel { query_id: 1 },
            CallerAuthorization::StandardUser
        ),
        ResponsePayload::CancelAck
    ));
}

#[test]
fn a_cancel_request_cancels_the_matching_in_flight_query() {
    let harness = Harness::new(ServiceConfig::default());
    let token = harness.sessions.begin_query(&QueryOwner::default(), 7);
    assert!(!token.is_cancelled());

    assert!(matches!(
        harness.handle(
            ClientRequest::Cancel { query_id: 7 },
            CallerAuthorization::StandardUser
        ),
        ResponsePayload::CancelAck
    ));
    assert!(token.is_cancelled(), "the named query must be cancelled");
}

#[test]
fn hello_ack_reports_the_callers_privilege_not_a_fixed_value() {
    let harness = Harness::new(ServiceConfig::default());
    let hello = ClientRequest::Hello {
        client_protocol_version: PROTOCOL_VERSION as u32,
        pid: 4242,
    };

    for (caller, expected) in [
        (CallerAuthorization::StandardUser, false),
        (CallerAuthorization::Unauthenticated, false),
        (CallerAuthorization::Administrator, true),
        (CallerAuthorization::LocalSystem, true),
    ] {
        match harness.handle(hello.clone(), caller) {
            ResponsePayload::HelloAck {
                server_protocol_version,
                caller_is_admin,
            } => {
                assert_eq!(server_protocol_version, PROTOCOL_VERSION as u32);
                assert_eq!(caller_is_admin, expected, "{caller:?}");
            }
            other => panic!("unexpected response for {caller:?}: {other:?}"),
        }
    }
}

#[test]
fn rescan_reports_failure_instead_of_claiming_success() {
    let harness = Harness::new(ServiceConfig::default());
    // No volume is indexed in this harness, so rescan requests must return failure.
    match harness.handle(
        ClientRequest::Rescan { volume_id: None },
        CallerAuthorization::Administrator,
    ) {
        ResponsePayload::RescanAck { success, .. } => assert!(!success),
        other => panic!("unexpected response: {other:?}"),
    }
    match harness.handle(
        ClientRequest::Rescan {
            volume_id: Some(25),
        },
        CallerAuthorization::LocalSystem,
    ) {
        ResponsePayload::RescanAck { volume_id, success } => {
            assert_eq!(volume_id, Some(25));
            assert!(!success, "an unknown volume must not report success");
        }
        other => panic!("unexpected response: {other:?}"),
    }
}

#[test]
fn a_mismatched_peer_receives_a_frame_under_its_own_protocol_version() {
    let peer_protocol_version = PROTOCOL_VERSION + 1;
    let frame = protocol_mismatch_frame(peer_protocol_version, PROTOCOL_VERSION).expect("encode");
    let header = peek_frame_header(&frame)
        .expect("read header")
        .expect("complete header");
    assert_eq!(header.version, peer_protocol_version);

    let envelope: Envelope<ResponsePayload> =
        postcard::from_bytes(&frame[cairn_protocol::codec::HEADER_SIZE..])
            .expect("peer-readable payload");
    assert!(matches!(
        envelope.payload,
        ResponsePayload::Error {
            code: ERROR_CODE_PROTOCOL_VERSION_MISMATCH,
            ..
        }
    ));
}

#[test]
fn hello_uses_the_connection_protocol_version_and_keeps_full_diagnostics() {
    let harness = Harness::new(ServiceConfig::default());
    let accepted = harness.handle_with_protocol_version(
        ClientRequest::Hello {
            client_protocol_version: 7,
            pid: 1,
        },
        CallerAuthorization::StandardUser,
        7,
    );
    match accepted {
        ResponsePayload::HelloAck {
            server_protocol_version,
            ..
        } => assert_eq!(server_protocol_version, 7),
        other => panic!("expected HelloAck, got {other:?}"),
    }

    let client_protocol_version = 7u32 + 0x1_0000;
    let (code, message) = denial(harness.handle_with_protocol_version(
        ClientRequest::Hello {
            client_protocol_version,
            pid: 1,
        },
        CallerAuthorization::StandardUser,
        7,
    ));
    assert_eq!(code, ERROR_CODE_PROTOCOL_VERSION_MISMATCH);
    assert!(
        message.contains(&client_protocol_version.to_string()),
        "{message}"
    );
    assert!(message.contains("service version 7"), "{message}");
}

/// A peer whose protocol version differs from this build's is refused at the message gate.
#[test]
fn a_client_from_another_build_is_rejected_at_the_message_gate() {
    let harness = Harness::new(ServiceConfig::default());
    let (code, message) = denial(harness.handle(
        ClientRequest::Hello {
            client_protocol_version: PROTOCOL_VERSION as u32 + 7,
            pid: 1,
        },
        CallerAuthorization::Administrator,
    ));
    assert_eq!(code, ERROR_CODE_PROTOCOL_VERSION_MISMATCH);
    assert!(
        message.contains(&format!(
            "client version {}, service version {PROTOCOL_VERSION}",
            PROTOCOL_VERSION + 7
        )),
        "{message}"
    );
}

fn volume_with(name: &str, frn: u64) -> crate::sync_engine::VolumeIndex {
    let mut volume = crate::sync_engine::VolumeIndex::new('C');
    volume.ingest_items(vec![cairn_ntfs::mft::ExtractedItem {
        frn,
        parent_frn: 0,
        name: name.to_string(),
        is_dir: false,
        size: 1,
        created_ts: 1,
        modified_ts: 1,
        flags: 0,
        is_reparse_point: false,
    }]);
    volume
}

fn query_session(harness: &Harness) -> (u64, Vec<cairn_protocol::message::FileRowData>) {
    match harness.handle(
        ClientRequest::Query {
            query_id: 1,
            raw_query: "notes".to_string(),
            defaults: Default::default(),
            sort_spec: SortSpec::default(),
            first_page_limit: 10,
            source_scope: None,
        },
        CallerAuthorization::Administrator,
    ) {
        ResponsePayload::QueryResult {
            session_id,
            first_page,
            ..
        } => (session_id, first_page),
        other => panic!("unexpected response: {other:?}"),
    }
}

#[test]
fn a_rebuilt_volume_expires_the_session_instead_of_paging_other_rows() {
    let harness = Harness::new(ServiceConfig::default());
    harness.engine.add_volume(volume_with("notes.txt", 10));
    let (session_id, first_page) = query_session(&harness);
    assert_eq!(first_page.len(), 1);
    assert_eq!(first_page[0].name, "notes.txt");

    // Unchanged volumes keep paging normally.
    match harness.handle(
        ClientRequest::GetRows {
            session_id,
            offset: 0,
            limit: 10,
        },
        CallerAuthorization::Administrator,
    ) {
        ResponsePayload::RowsResult { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].name, "notes.txt");
        }
        other => panic!("unexpected response: {other:?}"),
    }

    // A rescan replaces every record index; the old session must not return the new file in
    // the row that used to describe notes.txt.
    harness.engine.update_volume('C', |volume| {
        volume.ingest_items(vec![cairn_ntfs::mft::ExtractedItem {
            frn: 20,
            parent_frn: 0,
            name: "replacement.bin".to_string(),
            is_dir: false,
            size: 2,
            created_ts: 2,
            modified_ts: 2,
            flags: 0,
            is_reparse_point: false,
        }]);
    });

    for attempt in 0..2 {
        match harness.handle(
            ClientRequest::GetRows {
                session_id,
                offset: 0,
                limit: 10,
            },
            CallerAuthorization::Administrator,
        ) {
            ResponsePayload::Error { code, .. } => assert_eq!(
                code, ERROR_CODE_SESSION_EXPIRED,
                "attempt {attempt}: a stale or expired session must return ERROR_CODE_SESSION_EXPIRED"
            ),
            other => panic!("unexpected response: {other:?}"),
        }
    }
}

fn content_query_request(terms: usize) -> ClientRequest {
    let raw_query = (0..terms)
        .map(|i| format!("content:t{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    ClientRequest::Query {
        query_id: 1,
        raw_query,
        defaults: Default::default(),
        sort_spec: SortSpec::default(),
        first_page_limit: 10,
        source_scope: None,
    }
}

/// The content-term limit is request validation: a query above it is refused before the
/// engine evaluates anything, and a query at it is answered normally. The refusal must not
/// touch the active-query registry, whose new query would cancel every other in-flight one.
#[test]
fn a_query_above_the_content_term_limit_is_rejected_before_evaluation() {
    use cairn_core::syntax::MAX_CONTENT_TERMS;
    use cairn_protocol::message::ERROR_CODE_QUERY_TOO_COMPLEX;

    let harness = Harness::new(ServiceConfig::default());
    let in_flight = harness.sessions.begin_query(&QueryOwner::default(), 41);
    let (code, message) = denial(harness.handle(
        content_query_request(MAX_CONTENT_TERMS + 1),
        CallerAuthorization::Administrator,
    ));
    assert_eq!(code, ERROR_CODE_QUERY_TOO_COMPLEX);
    assert!(
        message.contains(&MAX_CONTENT_TERMS.to_string()),
        "{message}"
    );
    assert!(
        !in_flight.is_cancelled(),
        "a refused request must not cancel another connection's query"
    );
    harness.sessions.finish_query(&QueryOwner::default(), 41);

    // No content index is ready in this harness, so the query at the limit answers an empty
    // result set instead of a rejection.
    assert!(matches!(
        harness.handle(
            content_query_request(MAX_CONTENT_TERMS),
            CallerAuthorization::Administrator,
        ),
        ResponsePayload::QueryResult {
            total_matches: 0,
            ..
        }
    ));
}

fn child_filter_query_request(patterns: usize) -> ClientRequest {
    let raw_query = (0..patterns)
        .map(|i| format!("child:t{i}.rs"))
        .collect::<Vec<_>>()
        .join(" ");
    ClientRequest::Query {
        query_id: 1,
        raw_query,
        defaults: Default::default(),
        sort_spec: SortSpec::default(),
        first_page_limit: 10,
        source_scope: None,
    }
}

#[test]
fn a_query_above_the_child_filter_limit_is_rejected_before_evaluation() {
    use cairn_core::syntax::MAX_CHILD_FILTERS;
    use cairn_protocol::message::ERROR_CODE_QUERY_TOO_COMPLEX;

    let harness = Harness::new(ServiceConfig::default());
    let (code, message) = denial(harness.handle(
        child_filter_query_request(MAX_CHILD_FILTERS + 1),
        CallerAuthorization::Administrator,
    ));
    assert_eq!(code, ERROR_CODE_QUERY_TOO_COMPLEX);
    assert!(
        message.contains(&MAX_CHILD_FILTERS.to_string()),
        "{message}"
    );
    assert!(matches!(
        harness.handle(
            child_filter_query_request(MAX_CHILD_FILTERS),
            CallerAuthorization::Administrator,
        ),
        ResponsePayload::QueryResult {
            total_matches: 0,
            ..
        }
    ));
}

#[test]
fn a_query_for_an_inactive_source_scope_is_rejected() {
    use cairn_protocol::message::ERROR_CODE_INVALID_REQUEST;
    use cairn_protocol::message::SourceId;

    let harness = Harness::new(ServiceConfig::default());
    let mut request = content_query_request(0);
    if let ClientRequest::Query { source_scope, .. } = &mut request {
        *source_scope = Some(SourceId(9_999));
    }
    let (code, message) = denial(harness.handle(request, CallerAuthorization::Administrator));
    assert_eq!(code, ERROR_CODE_INVALID_REQUEST);
    assert!(message.contains("source_scope"), "{message}");
}

#[test]
fn pipe_server_with_pipe_name_sets_custom_name() {
    use crate::session::{DEFAULT_SESSION_TTL, SystemClock};
    let engine = Arc::new(MultiVolumeEngine::new());
    let sessions = Arc::new(SessionManager::new(
        10,
        DEFAULT_SESSION_TTL,
        SystemClock::new(),
    ));
    let custom_pipe = format!(r"\\.\pipe\Cairn.TestSeam.{}", std::process::id());
    let server = PipeServer::with_pipe_name(engine, sessions, &custom_pipe);
    assert_eq!(server.pipe_name, custom_pipe);
    #[cfg(windows)]
    {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        let instance = rt.block_on(async { server.create_pipe_instance(true) });
        assert!(
            instance.is_ok(),
            "pipe instance should be created on custom name: {:?}",
            instance.err()
        );
    }
}

#[cfg(windows)]
#[test]
fn slow_query_does_not_block_concurrent_hello_or_status_on_single_worker_runtime() {
    use tokio::net::windows::named_pipe::ClientOptions;

    async fn send_and_recv(
        pipe: &mut tokio::net::windows::named_pipe::NamedPipeClient,
        req: &ClientRequest,
    ) -> Envelope<ResponsePayload> {
        let mut out = Vec::new();
        encode_frame_into(&mut out, PROTOCOL_VERSION, req).expect("encode frame");
        pipe.write_all(&out).await.expect("write frame");
        pipe.flush().await.expect("flush frame");

        let mut read_buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            if let Some((_consumed, env)) =
                try_decode_frame::<Envelope<ResponsePayload>>(&read_buf).expect("decode envelope")
            {
                return env;
            }
            let n = pipe.read(&mut tmp).await.expect("read envelope");
            assert!(n > 0, "pipe closed before response");
            read_buf.extend_from_slice(&tmp[..n]);
        }
    }

    let dirs = tempfile::tempdir().expect("tempdir");
    let engine = Arc::new(MultiVolumeEngine::with_index_dir_and_drives(
        dirs.path(),
        vec![],
    ));
    engine.add_volume(crate::sync_engine::VolumeIndex::new('C'));
    let sessions = Arc::new(SessionManager::default());
    let custom_pipe = format!(r"\\.\pipe\Cairn.TestConcurrency.{}", std::process::id());
    let server = Arc::new(PipeServer::with_pipe_name(
        engine.clone(),
        sessions,
        &custom_pipe,
    ));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("1-worker runtime");

    struct ReleaseOnDrop(Option<std::sync::mpsc::Sender<()>>);
    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let _release_guard = ReleaseOnDrop(Some(release_tx.clone()));

    let server_task = {
        let server = server.clone();
        rt.spawn(async move {
            let _ = server.run().await;
        })
    };

    let mut client_a = rt.block_on(async {
        let mut client = loop {
            match ClientOptions::new().open(&custom_pipe) {
                Ok(c) => break c,
                Err(_) => tokio::task::yield_now().await,
            }
        };
        let hello_a = send_and_recv(
            &mut client,
            &ClientRequest::Hello {
                client_protocol_version: PROTOCOL_VERSION as u32,
                pid: std::process::id(),
            },
        )
        .await;
        assert!(matches!(hello_a.payload, ResponsePayload::HelloAck { .. }));
        client
    });

    // Block execute_search_multi inside process_request by holding the volumes write lock
    // on a helper thread via the public update_volume boundary.
    let (locked_tx, locked_rx) = std::sync::mpsc::channel::<()>();
    let blocker_engine = engine.clone();
    let blocker = std::thread::spawn(move || {
        blocker_engine.update_volume('C', |_| {
            let _ = locked_tx.send(());
            let _ = release_rx.recv();
        });
    });
    locked_rx.recv().expect("volumes write lock acquired");

    let query_task = rt.spawn(async move {
        let env = send_and_recv(
            &mut client_a,
            &ClientRequest::Query {
                query_id: 1,
                raw_query: "notes".to_string(),
                defaults: Default::default(),
                sort_spec: SortSpec::default(),
                first_page_limit: 10,
                source_scope: None,
            },
        )
        .await;
        env.payload
    });

    // Give the server's worker time to pick up Query and block on volumes.read().
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Connect a second client from an independent OS thread while client A's synchronous
    // Query holds the worker thread unless process_request runs via spawn_blocking.
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let pipe_b = custom_pipe.clone();
    let client_b_thread = std::thread::spawn(move || {
        let client_rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("client_b runtime");
        client_rt.block_on(async move {
            let mut client_b = ClientOptions::new().open(&pipe_b).expect("open client b");
            let hello_b = send_and_recv(
                &mut client_b,
                &ClientRequest::Hello {
                    client_protocol_version: PROTOCOL_VERSION as u32,
                    pid: std::process::id(),
                },
            )
            .await;
            assert!(matches!(hello_b.payload, ResponsePayload::HelloAck { .. }));

            let status_b = send_and_recv(&mut client_b, &ClientRequest::GetStatus).await;
            assert!(matches!(status_b.payload, ResponsePayload::StatusAck));
            let _ = done_tx.send(());
        });
    });

    done_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("Hello/GetStatus on client B stalled behind blocked Query");
    client_b_thread.join().expect("client_b thread joined");

    let _ = release_tx.send(());
    blocker.join().expect("blocker thread joined");
    let query_payload = rt
        .block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(2), query_task).await
        })
        .expect("Query did not complete after lock release")
        .expect("query task join");
    assert!(matches!(query_payload, ResponsePayload::QueryResult { .. }));

    server_task.abort();
}

#[test]
fn oversized_write_buffer_shrinks_back_after_large_response() {
    let mut small = Vec::with_capacity(128 * 1024);
    shrink_oversized_write_buf(&mut small);
    assert_eq!(small.capacity(), 128 * 1024);

    let mut large = Vec::with_capacity(2 * 1024 * 1024);
    shrink_oversized_write_buf(&mut large);
    assert!(large.capacity() <= WRITE_BUF_RETAIN_BYTES * 2);
    assert!(large.capacity() >= WRITE_BUF_RETAIN_BYTES);
}

#[test]
fn platform_source_property_and_journal_requests_round_trip_with_auth_enforced() {
    use cairn_core::property_store::SourceId;
    use cairn_protocol::message::ERROR_CODE_UNAUTHORIZED;
    use cairn_protocol::status::SourceKind;

    let dir = tempfile::tempdir().expect("tempdir");
    let watch = dir.path().join("docs");
    std::fs::create_dir_all(&watch).unwrap();
    std::fs::write(watch.join("plan.txt"), "hello").unwrap();

    let harness = Harness::new(ServiceConfig {
        excluded_paths: Vec::new(),
        ..ServiceConfig::default()
    });

    // Pre-populate a folder source without privileged service.toml persistence so the IPC
    // read/query/rescan/journal endpoints can be exercised deterministically unprivileged.
    let added = harness
        .engine
        .add_folder_source(&watch, false)
        .expect("register in-memory folder source");
    assert_eq!(added.kind, SourceKind::Folder);

    let folder_id =
        match harness.handle(ClientRequest::GetSources, CallerAuthorization::StandardUser) {
            ResponsePayload::SourcesResult { sources } => {
                sources
                    .into_iter()
                    .find(|s| s.kind == SourceKind::Folder)
                    .expect("folder source")
                    .source_id
            }
            other => panic!("unexpected GetSources response: {other:?}"),
        };

    assert!(matches!(
        harness.handle(
            ClientRequest::RescanSource {
                source_id: folder_id
            },
            CallerAuthorization::StandardUser,
        ),
        ResponsePayload::RescanSourceAck {
            source_id,
            success: true
        } if source_id == folder_id
    ));
    assert!(matches!(
        harness.handle(
            ClientRequest::RescanSource {
                source_id: SourceId(99_999),
            },
            CallerAuthorization::StandardUser,
        ),
        ResponsePayload::RescanSourceAck {
            source_id: SourceId(99_999),
            success: false,
        }
    ));

    match harness.handle(
        ClientRequest::Query {
            query_id: 4,
            raw_query: "length:>0".to_string(),
            defaults: Default::default(),
            sort_spec: SortSpec::single(
                SortColumn::Size,
                cairn_core::sort::SortDirection::Descending,
            ),
            first_page_limit: 10,
            source_scope: None,
        },
        CallerAuthorization::StandardUser,
    ) {
        ResponsePayload::QueryResult {
            degraded,
            applied_sort,
            ..
        } => {
            assert_eq!(
                degraded,
                Some(QueryDegradedReason::PropertyIndexDisabled {
                    requested: vec![cairn_core::property_store::PropertyKind::Length],
                })
            );
            assert_eq!(applied_sort.primary().column, SortColumn::Size);
        }
        other => panic!("unexpected degraded QueryResult: {other:?}"),
    }

    // Toggle in-memory property index & inspect status + journal over IPC.
    let status = harness.engine.set_property_index_enabled(true);
    assert!(status.enabled);
    match harness.handle(
        ClientRequest::GetPropertyIndexStatus,
        CallerAuthorization::StandardUser,
    ) {
        ResponsePayload::PropertyIndexStatus { status } => assert!(status.enabled),
        other => panic!("unexpected GetPropertyIndexStatus: {other:?}"),
    }
    assert!(matches!(
        harness.handle(
            ClientRequest::GetIndexJournal {
                after_seq: 0,
                limit: 50
            },
            CallerAuthorization::StandardUser,
        ),
        ResponsePayload::IndexJournalResult { .. }
    ));

    // Persisting mutations pass caller auth (`!= 401/403`); when unprivileged on Windows,
    // `save_atomic_secure` fails closed instead of silently skipping ACL enforcement.
    for req in [
        ClientRequest::AddFolderSource {
            path: watch.to_string_lossy().into_owned(),
        },
        ClientRequest::SetPropertyIndexEnabled { enabled: false },
        ClientRequest::SetIndexJournalEnabled { enabled: false },
        ClientRequest::RemoveFolderSource {
            path: watch.to_string_lossy().into_owned(),
        },
    ] {
        let (code, _) = denial(harness.handle(req.clone(), CallerAuthorization::Unauthenticated));
        assert_eq!(code, ERROR_CODE_UNAUTHORIZED, "{req:?}");

        match harness.handle(req, CallerAuthorization::StandardUser) {
            ResponsePayload::Error { code, message } => {
                assert_eq!(code, ERROR_CODE_INVALID_REQUEST);
                assert!(
                    message.contains("could not persist"),
                    "unexpected persistence failure message: {message}"
                );
            }
            ResponsePayload::SourcesResult { .. }
            | ResponsePayload::PropertyIndexStatus { .. }
            | ResponsePayload::IndexJournalStatus { .. } => {}
            other => panic!("unexpected persistence mutation response: {other:?}"),
        }
    }
}

#[test]
fn clear_content_index_short_circuits_when_disable_config_fails() {
    let root = tempfile::tempdir().expect("tempdir");
    let content_dir = root.path().join("content");
    let config_dir_blocker = root.path().join("config-as-dir");
    std::fs::create_dir_all(&config_dir_blocker).unwrap();

    // Seed an indexable document and marker to verify clearing does NOT execute when disabling
    // `content_index_enabled` fails.
    {
        let writer = cairn_content::ContentWriter::open_or_create(&content_dir).unwrap();
        writer
            .index_document_deferred(1, 1, 9, 100, "C:\\keep.txt", "preserved")
            .unwrap();
        writer.commit().unwrap();
        writer.finish().unwrap();
    }
    let cfg = ServiceConfig {
        content_index_enabled: true,
        ..Default::default()
    };
    let engine = Arc::new(MultiVolumeEngine::with_paths(
        &root.path().join("index"),
        &content_dir,
        &config_dir_blocker,
        vec![],
        cfg,
    ));
    let sessions = SessionManager::default();

    let resp = process_request(
        ClientRequest::ClearContentIndex,
        &engine,
        &sessions,
        CallerAuthorization::LocalSystem,
        &QueryOwner::default(),
        false,
        PROTOCOL_VERSION,
    );
    assert!(matches!(
        resp,
        ResponsePayload::ClearContentIndexAck { success: false }
    ));
    let searcher = cairn_content::ContentSearcher::open(&content_dir).unwrap();
    assert_eq!(searcher.document_count().unwrap(), 1);
}

#[test]
fn rejected_hello_does_not_mark_connection_authenticated() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let root = tempfile::tempdir().unwrap();
        let engine = Arc::new(MultiVolumeEngine::with_paths(
            &root.path().join("index"),
            &root.path().join("content"),
            &root.path().join("service.toml"),
            vec![],
            ServiceConfig::default(),
        ));
        let sessions = Arc::new(SessionManager::default());
        let pipe_name = format!(
            r"\\.\pipe\cairn-test-hello-{}",
            std::process::id() ^ 0x5A5A_u32
        );
        let server = Arc::new(PipeServer::with_pipe_name(
            Arc::clone(&engine),
            Arc::clone(&sessions),
            pipe_name.clone(),
        ));
        let srv_task = {
            let server = Arc::clone(&server);
            tokio::spawn(async move {
                let _ = server.run().await;
            })
        };

        let mut client = None;
        for _ in 0..40 {
            match tokio::net::windows::named_pipe::ClientOptions::new().open(&pipe_name) {
                Ok(c) => {
                    client = Some(c);
                    break;
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(25)).await,
            }
        }
        let mut client = client.expect("connect test client");

        let bad_hello = cairn_protocol::codec::encode_frame(&ClientRequest::Hello {
            client_protocol_version: u32::from(PROTOCOL_VERSION) + 99,
            pid: std::process::id(),
        })
        .unwrap();
        client.write_all(&bad_hello).await.unwrap();
        client.flush().await.unwrap();

        let mut reply_buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let ack: Envelope<ResponsePayload> = loop {
            let n = client.read(&mut chunk).await.unwrap();
            assert!(n > 0, "pipe closed unexpectedly after mismatched Hello");
            reply_buf.extend_from_slice(&chunk[..n]);
            if let Some((consumed, env)) =
                try_decode_frame::<Envelope<ResponsePayload>>(&reply_buf).unwrap()
            {
                reply_buf.drain(0..consumed);
                break env;
            }
        };
        assert!(matches!(
            ack.payload,
            ResponsePayload::Error {
                code: ERROR_CODE_PROTOCOL_VERSION_MISMATCH,
                ..
            }
        ));

        let follow_up = cairn_protocol::codec::encode_frame(&ClientRequest::GetStatus).unwrap();
        client.write_all(&follow_up).await.unwrap();
        client.flush().await.unwrap();

        let next_env: Envelope<ResponsePayload> = loop {
            if let Some((consumed, env)) =
                try_decode_frame::<Envelope<ResponsePayload>>(&reply_buf).unwrap()
            {
                reply_buf.drain(0..consumed);
                break env;
            }
            let n = client.read(&mut chunk).await.unwrap();
            assert!(n > 0, "pipe closed unexpectedly before second response");
            reply_buf.extend_from_slice(&chunk[..n]);
        };
        assert!(matches!(
            next_env.payload,
            ResponsePayload::Error {
                code: ERROR_CODE_HANDSHAKE_REQUIRED,
                ..
            }
        ));

        srv_task.abort();
        engine.stop_content_worker();
        engine.stop_property_extraction();
    });
}

#[test]
fn frame_read_times_out_for_partial_and_idle_connections() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let pipe_name = format!(
            r"\\.\pipe\cairn-test-timeout-{}",
            std::process::id() ^ 0xA5A5_u32
        );
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe_name)
            .unwrap();

        let client_task = tokio::spawn({
            let pipe_name = pipe_name.clone();
            async move {
                let _client = tokio::net::windows::named_pipe::ClientOptions::new()
                    .open(&pipe_name)
                    .unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            }
        });

        server.connect().await.unwrap();
        let mut buf = [0u8; 64];

        let partial_err = read_frame_chunk_with_timeout(
            &mut server,
            &mut buf,
            true,
            std::time::Duration::from_millis(30),
        )
        .await
        .unwrap_err();
        assert_eq!(partial_err.kind(), std::io::ErrorKind::TimedOut);

        let idle_err = read_frame_chunk_with_timeout(
            &mut server,
            &mut buf,
            false,
            std::time::Duration::from_millis(10),
        )
        .await
        .unwrap_err();
        assert_eq!(idle_err.kind(), std::io::ErrorKind::TimedOut);

        let idle_eof = read_frame_chunk_with_timeout(
            &mut server,
            &mut buf,
            false,
            std::time::Duration::from_millis(200),
        )
        .await
        .unwrap();
        assert_eq!(idle_eof, 0);
        client_task.await.unwrap();
    });
}
