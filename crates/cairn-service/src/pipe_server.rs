// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Async Named Pipe IPC server listening on `\\.\pipe\Cairn.Service`.
//!
//! Every connection is attributed to the OS identity of the client process (see
//! [`crate::caller_auth`]); authorization is then evaluated per request, against the configuration
//! the engine currently holds, so `admin_only_query` takes effect on the next request rather than on
//! the next pipe instance.

use crate::caller_auth::{authenticate_pipe_client, authorize, CallerAuthorization};
use crate::security::SafeSecurityDescriptor;
use crate::session::{QueryOwner, RowsError, SessionManager};
use crate::sync_engine::MultiVolumeEngine;
use cairn_core::engine::CancellationToken;
use cairn_core::syntax::{parse_query_with, MAX_CONTENT_TERMS};
use cairn_protocol::codec::{encode_frame_into, peek_frame_header, try_decode_frame};
use cairn_protocol::message::{
    ClientRequest, Envelope, ResponsePayload, ERROR_CODE_HANDSHAKE_REQUIRED,
    ERROR_CODE_PROTOCOL_VERSION_MISMATCH, ERROR_CODE_QUERY_TOO_COMPLEX, ERROR_CODE_SESSION_EXPIRED,
    MAX_TREE_DEPTH, PIPE_NAME, PRODUCT_VERSION, WIRE_GUARD,
};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tracing::{error, info, warn};

/// The wire form of a tree failure: the code chooses the client's copy.
fn tree_error_payload(reason: crate::sync_engine::TreeUnavailable) -> ResponsePayload {
    ResponsePayload::Error {
        code: reason.code(),
        message: reason.message().to_string(),
    }
}

pub struct PipeServer {
    engine: Arc<MultiVolumeEngine>,
    sessions: Arc<SessionManager>,
    pipe_name: String,
    protocol_version: u16,
}

impl PipeServer {
    pub fn new(engine: Arc<MultiVolumeEngine>, sessions: Arc<SessionManager>) -> Self {
        engine.ensure_content_runtime();
        Self {
            engine,
            sessions,
            pipe_name: PIPE_NAME.to_string(),
            protocol_version: WIRE_GUARD,
        }
    }

    pub fn with_pipe_name(
        engine: Arc<MultiVolumeEngine>,
        sessions: Arc<SessionManager>,
        pipe_name: impl Into<String>,
    ) -> Self {
        Self {
            pipe_name: pipe_name.into(),
            ..Self::new(engine, sessions)
        }
    }

    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Starting Named Pipe server on {}", self.pipe_name);

        let mut server = self.create_pipe_instance(true)?;

        loop {
            server.connect().await?;
            let connected_server = server;

            // Immediately create next pipe instance to listen for concurrent clients
            server = self.create_pipe_instance(false)?;

            let engine = self.engine.clone();
            let sessions = self.sessions.clone();
            let protocol_version = self.protocol_version;
            let pipe_name = self.pipe_name.clone();

            tokio::spawn(async move {
                if let Err(e) =
                    handle_client(connected_server, engine, sessions, protocol_version).await
                {
                    if let Some(io_err) = e.downcast_ref::<std::io::Error>() {
                        if is_pipe_closed_error(io_err) {
                            return;
                        }
                    }
                    error!("Client connection on {pipe_name} error: {}", e);
                }
            });
        }
    }

    fn create_pipe_instance(
        &self,
        first_instance: bool,
    ) -> Result<NamedPipeServer, std::io::Error> {
        let mut options = ServerOptions::new();
        options.first_pipe_instance(first_instance);
        // SPEC SVC-02: no client arriving over the network may reach the index.
        options.reject_remote_clients(true);

        // The DACL is read from the live configuration, so the switch applies to every instance
        // created from now on. Requests are additionally authorized per request, which covers
        // instances that were created before the switch changed.
        #[cfg(windows)]
        {
            let admin_only_query = self.engine.admin_only_query();
            let mut sd = SafeSecurityDescriptor::create(admin_only_query).map_err(|err| {
                error!(
                    "refusing to create {} without its access-controlled DACL: {err}",
                    self.pipe_name
                );
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("control pipe DACL: {err}"),
                )
            })?;
            // SAFETY: pointer to SECURITY_ATTRIBUTES passed into windows named pipe creation
            unsafe {
                options.create_with_security_attributes_raw(
                    &self.pipe_name,
                    &mut sd.sa as *mut _ as *mut _,
                )
            }
        }
        #[cfg(not(windows))]
        {
            options.create(&self.pipe_name)
        }
    }
}

fn is_pipe_closed_error(err: &std::io::Error) -> bool {
    match err.kind() {
        std::io::ErrorKind::BrokenPipe
        | std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::UnexpectedEof => true,
        _ => {
            #[cfg(windows)]
            if let Some(code) = err.raw_os_error() {
                // 109: ERROR_BROKEN_PIPE (The pipe has been ended)
                // 232: ERROR_NO_DATA (The pipe is being closed)
                // 233: ERROR_PIPE_NOT_CONNECTED (No process is on the other end of the pipe)
                return code == 109 || code == 232 || code == 233;
            }
            false
        }
    }
}

async fn handle_client(
    mut pipe: NamedPipeServer,
    engine: Arc<MultiVolumeEngine>,
    sessions: Arc<SessionManager>,
    protocol_version: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    // Identity comes from the OS token of the connected process, once per connection: the client
    // cannot change it while the pipe instance is alive. The call is synchronous and performs no
    // await, so thread-bound impersonation cannot outlive it.
    #[cfg(windows)]
    let (caller, owner) =
        match authenticate_pipe_client(std::os::windows::io::AsRawHandle::as_raw_handle(&pipe)) {
            Ok(identity) => {
                info!(
                    "pipe client pid {} ({}) authorized as {:?}",
                    identity.client_pid, identity.user_sid, identity.authorization
                );
                let owner = QueryOwner {
                    client_pid: identity.client_pid,
                    user_sid: identity.user_sid.clone(),
                };
                (identity.authorization(), owner)
            }
            Err(err) => {
                // Unattributable connections are closed instead of being served: the pipe grants
                // interactive users connect access, so the token is the only thing standing between
                // them and a privileged operation.
                warn!("rejecting pipe connection: {err}");
                return Ok(());
            }
        };
    #[cfg(not(windows))]
    let (caller, owner) = (CallerAuthorization::Unauthenticated, QueryOwner::default());

    let mut read_buf = Vec::with_capacity(65536);
    let mut write_buf = Vec::with_capacity(4096);
    let mut temp_buf = [0u8; 8192];
    let mut cancel = CancellationToken::new();
    let mut hello_done = false;

    loop {
        // The frame header carries the version, so the gate runs before any payload is deserialized:
        // a peer of another version may serialize a message that would decode into the wrong variant.
        if let Some(header) = peek_frame_header(&read_buf)? {
            if header.version != protocol_version {
                write_buf.clear();
                encode_frame_into(
                    &mut write_buf,
                    header.version,
                    &Envelope {
                        volumes: Vec::new(),
                        payload: version_mismatch(protocol_version, header.version),
                    },
                )?;
                if let Err(e) = pipe.write_all(&write_buf).await {
                    if is_pipe_closed_error(&e) {
                        return Ok(());
                    }
                    return Err(e.into());
                }
                if let Err(e) = pipe.flush().await {
                    if is_pipe_closed_error(&e) {
                        return Ok(());
                    }
                    return Err(e.into());
                }
                return Ok(());
            }

            if let Some((consumed, req)) = try_decode_frame::<ClientRequest>(&read_buf)? {
                if consumed == read_buf.len() {
                    read_buf.clear();
                } else {
                    read_buf.drain(0..consumed);
                }
                if read_buf.is_empty() && read_buf.capacity() > 65536 {
                    read_buf.shrink_to(65536);
                }

                if matches!(req, ClientRequest::Query { .. }) {
                    cancel = CancellationToken::new();
                }

                let resp_payload = if !hello_done && !matches!(req, ClientRequest::Hello { .. }) {
                    // Version negotiation is explicit (SVC-03): nothing is served before it.
                    ResponsePayload::Error {
                        code: ERROR_CODE_HANDSHAKE_REQUIRED,
                        message: "protocol negotiation missing: send Hello first".to_string(),
                    }
                } else {
                    if matches!(req, ClientRequest::Hello { .. }) {
                        hello_done = true;
                    }
                    let admin_only_query = engine.admin_only_query();
                    process_request(
                        req,
                        &engine,
                        &sessions,
                        caller,
                        &owner,
                        admin_only_query,
                        &cancel,
                    )
                };

                let omit_volumes = matches!(
                    resp_payload,
                    ResponsePayload::Error {
                        code: ERROR_CODE_HANDSHAKE_REQUIRED
                            | ERROR_CODE_PROTOCOL_VERSION_MISMATCH
                            | cairn_protocol::message::ERROR_CODE_UNAUTHORIZED
                            | cairn_protocol::message::ERROR_CODE_ADMIN_ONLY_QUERY,
                        ..
                    }
                );
                // A query answer decides whether the table is still loading, so its envelope
                // reads the volume state after the query released the index. Every other
                // envelope may use the cache and keep its connection responsive.
                let volumes = if omit_volumes {
                    Vec::new()
                } else if matches!(&resp_payload, ResponsePayload::QueryResult { .. }) {
                    engine.get_volume_summaries_fresh()
                } else {
                    engine.get_volume_summaries()
                };
                let envelope = Envelope {
                    volumes,
                    payload: resp_payload,
                };

                write_buf.clear();
                encode_frame_into(&mut write_buf, WIRE_GUARD, &envelope)?;
                if let Err(e) = pipe.write_all(&write_buf).await {
                    if is_pipe_closed_error(&e) {
                        return Ok(());
                    }
                    return Err(e.into());
                }
                if let Err(e) = pipe.flush().await {
                    if is_pipe_closed_error(&e) {
                        return Ok(());
                    }
                    return Err(e.into());
                }
                continue;
            }
        }

        let n = match pipe.read(&mut temp_buf).await {
            Ok(0) => break, // Client disconnected
            Ok(n) => n,
            Err(e) if is_pipe_closed_error(&e) => break, // Normal pipe closure on Windows
            Err(e) => return Err(e.into()),
        };
        read_buf.extend_from_slice(&temp_buf[..n]);
    }

    Ok(())
}

fn version_mismatch(server_guard: u16, client_guard: u16) -> ResponsePayload {
    ResponsePayload::Error {
        code: ERROR_CODE_PROTOCOL_VERSION_MISMATCH,
        message: format!(
            "Incompatible build: client guard {client_guard}, service guard {server_guard}; the service is built from {PRODUCT_VERSION}."
        ),
    }
}

fn process_request(
    req: ClientRequest,
    engine: &MultiVolumeEngine,
    sessions: &SessionManager,
    caller: CallerAuthorization,
    owner: &QueryOwner,
    admin_only_query: bool,
    cancel: &CancellationToken,
) -> ResponsePayload {
    if let Err((code, message)) = authorize(&req, caller, admin_only_query) {
        warn!("denied {code} from {caller:?}: {message}");
        return ResponsePayload::Error { code, message };
    }

    match req {
        ClientRequest::Hello {
            client_version,
            pid,
        } => {
            if client_version != WIRE_GUARD as u32 {
                version_mismatch(WIRE_GUARD, client_version as u16)
            } else {
                info!(
                    "handshake with self-reported pid {pid} accepted; caller classified as {caller:?}"
                );
                ResponsePayload::HelloAck {
                    server_version: WIRE_GUARD as u32,
                    caller_is_admin: caller.is_privileged(),
                }
            }
        }
        ClientRequest::Query {
            query_id,
            raw_query,
            defaults,
            sort_column,
            sort_asc,
            first_page_limit,
        } => {
            // Request validation, not evaluation: the engine stays a pure evaluator, and a query
            // above the content-term limit is refused before any hit set is built. The check
            // precedes begin_query, which cancels every other in-flight query.
            let content_terms = parse_query_with(&raw_query, &defaults).content_term_count();
            if content_terms > MAX_CONTENT_TERMS {
                return ResponsePayload::Error {
                    code: ERROR_CODE_QUERY_TOO_COMPLEX,
                    message: format!(
                        "query carries {content_terms} content terms; the limit is {MAX_CONTENT_TERMS}"
                    ),
                };
            }
            let query_cancel = sessions.begin_query(owner, query_id, cancel);
            let (matched_items, total_matches, highlight_terms, snippets, total_bytes) = engine
                .execute_search_multi(&raw_query, &defaults, sort_column, sort_asc, &query_cancel);
            sessions.finish_query(owner, query_id);

            let session_id =
                sessions.create_session(raw_query, highlight_terms, matched_items, snippets);

            let first_page = engine.with_volumes(|vols| {
                sessions.get_rows_multi(session_id, 0, first_page_limit, vols)
            });
            let first_page = match first_page {
                Ok(rows) => rows,
                Err(err) => {
                    // A volume was rebuilt between the query and its first page. The cached
                    // indices no longer describe those rows, so the session is dropped rather
                    // than serving rows from the new table under old offsets.
                    warn!("query session {session_id} expired before its first page: {err:?}");
                    sessions.invalidate(session_id);
                    Vec::new()
                }
            };

            ResponsePayload::QueryResult {
                query_id,
                session_id,
                total_matches,
                total_bytes,
                first_page,
            }
        }
        ClientRequest::GetRows {
            session_id,
            offset,
            limit,
        } => {
            let rows = engine
                .with_volumes(|vols| sessions.get_rows_multi(session_id, offset, limit, vols));
            match rows {
                Ok(rows) => ResponsePayload::RowsResult {
                    session_id,
                    offset,
                    rows,
                },
                Err(RowsError::UnknownSession) => {
                    warn!("query session {session_id} not found");
                    ResponsePayload::Error {
                        code: ERROR_CODE_SESSION_EXPIRED,
                        message: format!("query session {session_id} expired or unknown"),
                    }
                }
                Err(err @ RowsError::Stale { .. }) => {
                    warn!("dropping stale session {session_id}: {err:?}");
                    sessions.invalidate(session_id);
                    ResponsePayload::Error {
                        code: ERROR_CODE_SESSION_EXPIRED,
                        message: format!("query session {session_id} is stale: {err:?}"),
                    }
                }
            }
        }
        ClientRequest::Cancel { query_id } => {
            cancel.cancel();
            sessions.cancel_query(owner, query_id);
            ResponsePayload::CancelAck
        }
        ClientRequest::GetStatus => ResponsePayload::StatusAck,
        ClientRequest::Rescan { volume_id } => {
            let outcome = match volume_id {
                Some(id) => match cairn_core::FileRecord::drive_from_volume_id(id) {
                    Some(drive) => engine.rescan_volume(drive).map(|_| ()),
                    None => Err(format!(
                        "volume id {id} is outside the drive-letter range 0..=25, nothing was rescanned"
                    )),
                },
                None => {
                    if engine.rescan_snapshot_volumes() > 0 {
                        Ok(())
                    } else {
                        Err("no snapshot-mode volume was rescanned".to_string())
                    }
                }
            };
            match outcome {
                Ok(()) => ResponsePayload::RescanAck {
                    volume_id,
                    success: true,
                },
                Err(reason) => {
                    info!("rescan of {volume_id:?} did not run: {reason}");
                    ResponsePayload::RescanAck {
                        volume_id,
                        success: false,
                    }
                }
            }
        }
        ClientRequest::GetConfig => {
            let cfg = engine.get_config();
            let config_toml = toml::to_string_pretty(&cfg).unwrap_or_default();
            ResponsePayload::ConfigResult { config_toml }
        }
        ClientRequest::SetConfig { config_toml } => {
            // Index configuration is a local-user operation, but `admin_only_query` decides who
            // may query at all. A standard user must not be able to switch that mode on (locking
            // everyone else out) or off (defeating it); only an administrator may change it. The
            // engine compares the submitted value with the live one inside its configuration
            // write, so two concurrent submissions cannot both be decided against a stale value.
            match engine.apply_config_toml(&config_toml, caller.is_privileged()) {
                Ok(()) => ResponsePayload::SetConfigAck {
                    success: true,
                    message: "Service configuration persisted and applied".to_string(),
                },
                Err(reason) => ResponsePayload::SetConfigAck {
                    success: false,
                    message: reason,
                },
            }
        }
        ClientRequest::ReloadConfig => match engine.reload_config() {
            Ok(()) => ResponsePayload::ReloadConfigAck {
                success: true,
                message: "Service configuration reloaded from disk".to_string(),
            },
            Err(err_msg) => ResponsePayload::ReloadConfigAck {
                success: false,
                message: err_msg,
            },
        },
        ClientRequest::GetContentIndexStatus => ResponsePayload::ContentIndexStatus {
            status: engine.content_index_status(),
        },
        ClientRequest::GetTree { path, max_depth } => {
            match engine.treemap_tree(&path, tree_depth(max_depth)) {
                Ok(entries) => ResponsePayload::TreeResult { path, entries },
                Err(reason) => tree_error_payload(reason),
            }
        }
        ClientRequest::CancelContentIndex => ResponsePayload::CancelContentIndexAck {
            success: engine.disable_content_index().is_ok(),
        },
        ClientRequest::ClearContentIndex => {
            // Disabling first stops the active worker batch; the controller then clears the index
            // after the current batch unwinds. The ack means "accepted", and the index page waits
            // for the status to show zero documents.
            let disabled = engine.disable_content_index().is_ok();
            let accepted = engine.request_clear_content_index();
            ResponsePayload::ClearContentIndexAck {
                success: disabled && accepted,
            }
        }
    }
}

/// Clamps a client's requested tree depth to the frame budget.
fn tree_depth(requested: usize) -> usize {
    requested.min(MAX_TREE_DEPTH)
}

#[cfg(test)]
mod tests {

    #[test]
    fn tree_failures_carry_distinct_protocol_codes() {
        use crate::sync_engine::TreeUnavailable;
        for (reason, expected) in [
            (
                TreeUnavailable::NotIndexed,
                cairn_protocol::message::ERROR_CODE_TREE_NOT_INDEXED,
            ),
            (
                TreeUnavailable::NotDirectory,
                cairn_protocol::message::ERROR_CODE_TREE_NOT_DIRECTORY,
            ),
            (
                TreeUnavailable::TooLarge,
                cairn_protocol::message::ERROR_CODE_TREE_TOO_LARGE,
            ),
        ] {
            match tree_error_payload(reason) {
                cairn_protocol::message::ResponsePayload::Error { code, .. } => {
                    assert_eq!(code, expected)
                }
                other => panic!("a tree failure became {other:?}"),
            }
        }
    }
    use super::*;
    use crate::config::ServiceConfig;
    use cairn_core::sort::SortColumn;
    use cairn_protocol::message::ERROR_CODE_ADMIN_ONLY_QUERY;

    struct Harness {
        _dirs: tempfile::TempDir,
        engine: MultiVolumeEngine,
        sessions: SessionManager,
        cancel: CancellationToken,
    }

    impl Harness {
        fn new(config: ServiceConfig) -> Self {
            let dirs = tempfile::tempdir().expect("tempdir");
            let root = dirs.path().to_path_buf();
            let engine = MultiVolumeEngine::with_paths(
                &root.join("index"),
                &root.join("content"),
                &root.join("service.toml"),
                vec![],
                config,
            );
            Self {
                _dirs: dirs,
                engine,
                sessions: SessionManager::default(),
                cancel: CancellationToken::new(),
            }
        }

        fn handle(&self, req: ClientRequest, caller: CallerAuthorization) -> ResponsePayload {
            let admin_only_query = self.engine.admin_only_query();
            process_request(
                req,
                &self.engine,
                &self.sessions,
                caller,
                &QueryOwner::default(),
                admin_only_query,
                &self.cancel,
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
    fn a_tree_request_cannot_outgrow_the_frame_budget() {
        assert_eq!(tree_depth(0), 0);
        assert_eq!(tree_depth(2), 2);
        assert_eq!(tree_depth(6), MAX_TREE_DEPTH);
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
                panic!("a standard user must be authorized to change index configuration: {code}: {message}")
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
                sort_column: SortColumn::Name,
                sort_asc: true,
                first_page_limit: 10,
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
                    client_version: WIRE_GUARD as u32,
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
        let token =
            harness
                .sessions
                .begin_query(&QueryOwner::default(), 7, &CancellationToken::new());
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
            client_version: WIRE_GUARD as u32,
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
                    server_version,
                    caller_is_admin,
                } => {
                    assert_eq!(server_version, WIRE_GUARD as u32);
                    assert_eq!(caller_is_admin, expected, "{caller:?}");
                }
                other => panic!("unexpected response for {caller:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn rescan_reports_failure_instead_of_claiming_success() {
        let harness = Harness::new(ServiceConfig::default());
        // No volume is indexed in this harness, so a rescan cannot succeed; the old code answered
        // `success: true` unconditionally.
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

    /// A peer whose guard differs from this build's is refused at the message gate, and the reason
    /// names the product the service was built from rather than a counter.
    #[test]
    fn a_client_from_another_build_is_rejected_at_the_message_gate() {
        let harness = Harness::new(ServiceConfig::default());
        let (code, message) = denial(harness.handle(
            ClientRequest::Hello {
                client_version: WIRE_GUARD as u32 + 7,
                pid: 1,
            },
            CallerAuthorization::Administrator,
        ));
        assert_eq!(code, ERROR_CODE_PROTOCOL_VERSION_MISMATCH);
        assert!(
            message.contains(&format!("the service is built from {PRODUCT_VERSION}")),
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
                sort_column: SortColumn::Name,
                sort_asc: true,
                first_page_limit: 10,
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
            sort_column: SortColumn::Name,
            sort_asc: true,
            first_page_limit: 10,
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
        let in_flight =
            harness
                .sessions
                .begin_query(&QueryOwner::default(), 41, &CancellationToken::new());
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

    #[test]
    fn pipe_server_with_pipe_name_sets_custom_name() {
        let engine = Arc::new(MultiVolumeEngine::new());
        let sessions = Arc::new(SessionManager::new(10));
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
}
