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
use crate::session::{RowsError, SessionManager};
use crate::sync_engine::MultiVolumeEngine;
use cairn_core::engine::CancellationToken;
use cairn_core::sort::SortColumn;
use cairn_protocol::codec::{encode_frame_into, peek_frame_header, try_decode_frame};
use cairn_protocol::message::{
    ClientRequest, Envelope, ResponsePayload, ERROR_CODE_HANDSHAKE_REQUIRED,
    ERROR_CODE_PROTOCOL_VERSION_MISMATCH, ERROR_CODE_SESSION_EXPIRED, PIPE_NAME, PROTOCOL_VERSION,
};
use cairn_protocol::status::WorkerStatus;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tracing::{error, info, warn};

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
            protocol_version: PROTOCOL_VERSION,
        }
    }

    /// Serves a different pipe name; used by `--console` development runs and by the integration
    /// tests, which must not collide with the installed service.
    pub fn with_pipe_name(
        engine: Arc<MultiVolumeEngine>,
        sessions: Arc<SessionManager>,
        name: &str,
    ) -> Self {
        Self {
            pipe_name: name.to_string(),
            ..Self::new(engine, sessions)
        }
    }

    /// The protocol version this service instance negotiates. Production builds keep
    /// [`PROTOCOL_VERSION`]; tests set it to another value so the same version gate can be exercised
    /// from both directions — a new client against an older service and the reverse (SVC-03).
    pub fn with_protocol_version(mut self, version: u16) -> Self {
        self.protocol_version = version;
        self
    }

    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Starting Named Pipe server on {}", self.pipe_name);

        let mut server = self.create_pipe_instance(true)?;

        loop {
            // Wait for client to connect
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
            if let Ok(mut sd) = SafeSecurityDescriptor::create(admin_only_query) {
                // SAFETY: pointer to SECURITY_ATTRIBUTES passed into windows named pipe creation
                unsafe {
                    return options.create_with_security_attributes_raw(
                        &self.pipe_name,
                        &mut sd.sa as *mut _ as *mut _,
                    );
                }
            }
        }

        options.create(&self.pipe_name)
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
    let caller =
        match authenticate_pipe_client(std::os::windows::io::AsRawHandle::as_raw_handle(&pipe)) {
            Ok(identity) => {
                info!(
                    "pipe client pid {} ({}) authorized as {:?}",
                    identity.client_pid, identity.user_sid, identity.authorization
                );
                identity.authorization()
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
    let caller = CallerAuthorization::Unauthenticated;

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
                        volumes: engine.get_volume_summaries(),
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
                    process_request(req, &engine, &sessions, caller, admin_only_query, &cancel)
                };

                let envelope = Envelope {
                    volumes: engine.get_volume_summaries(),
                    payload: resp_payload,
                };

                write_buf.clear();
                encode_frame_into(&mut write_buf, PROTOCOL_VERSION, &envelope)?;
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

fn version_mismatch(server_version: u16, client_version: u16) -> ResponsePayload {
    ResponsePayload::Error {
        code: ERROR_CODE_PROTOCOL_VERSION_MISMATCH,
        message: format!(
            "Incompatible protocol version: client={client_version}, server={server_version}"
        ),
    }
}

/// Maps a `Rescan` volume id back to the drive letter it was derived from (`FileRecord::volume_id`,
/// a zero-based offset from `A:`). Only the 26 possible letters are in range; anything else yields
/// `None` so the caller refuses the request instead of rescanning a fabricated drive.
fn drive_from_volume_id(volume_id: u16) -> Option<char> {
    if volume_id >= 26 {
        return None;
    }
    Some(char::from(b'A' + volume_id as u8))
}

fn process_request(
    req: ClientRequest,
    engine: &MultiVolumeEngine,
    sessions: &SessionManager,
    caller: CallerAuthorization,
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
            if client_version != PROTOCOL_VERSION as u32 {
                version_mismatch(PROTOCOL_VERSION, client_version as u16)
            } else {
                info!(
                    "handshake with self-reported pid {pid} accepted; caller classified as {caller:?}"
                );
                ResponsePayload::HelloAck {
                    server_version: PROTOCOL_VERSION as u32,
                    caller_is_admin: caller.is_privileged(),
                }
            }
        }
        ClientRequest::Query {
            query_id,
            raw_query,
            defaults,
            sort_by_name,
            sort_asc,
            first_page_limit,
        } => {
            let sort_col = match sort_by_name.to_ascii_lowercase().as_str() {
                "path" => SortColumn::Path,
                "size" => SortColumn::Size,
                "modified" | "datemodified" => SortColumn::DateModified,
                "created" | "datecreated" => SortColumn::DateCreated,
                "ext" | "extension" => SortColumn::Extension,
                _ => SortColumn::Name,
            };

            let (matched_items, total_matches, highlight_terms, snippets) =
                engine.execute_search_multi(&raw_query, &defaults, sort_col, sort_asc, cancel);

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
        ClientRequest::Cancel { query_id: _ } => {
            cancel.cancel();
            ResponsePayload::StatusUpdate {
                worker_status: WorkerStatus::Idle,
            }
        }
        ClientRequest::SubscribeStatus => ResponsePayload::StatusUpdate {
            worker_status: WorkerStatus::Idle,
        },
        ClientRequest::Rescan { volume_id } => {
            let outcome = match volume_id {
                Some(id) => match drive_from_volume_id(id) {
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
            // everyone else out) or off (defeating it); only an administrator may change it.
            let parsed: crate::config::ServiceConfig = match toml::from_str(&config_toml) {
                Ok(config) => config,
                Err(err) => {
                    return ResponsePayload::SetConfigAck {
                        success: false,
                        message: format!("invalid service configuration: {err}"),
                    }
                }
            };
            if parsed.admin_only_query != engine.get_config().admin_only_query
                && !caller.is_privileged()
            {
                return ResponsePayload::SetConfigAck {
                    success: false,
                    message: "admin_only_query can only be changed by an administrator".to_string(),
                };
            }
            match engine.apply_config_toml(&config_toml) {
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
            let entries = engine.treemap_tree(&path, max_depth).unwrap_or_default();
            ResponsePayload::TreeResult { path, entries }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServiceConfig;
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
                sort_by_name: "name".to_string(),
                sort_asc: true,
                first_page_limit: 10,
            },
            ClientRequest::GetRows {
                session_id: 1,
                offset: 0,
                limit: 10,
            },
            ClientRequest::GetConfig,
            ClientRequest::SubscribeStatus,
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
                    client_version: PROTOCOL_VERSION as u32,
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
            ResponsePayload::StatusUpdate { .. }
        ));
    }

    #[test]
    fn hello_ack_reports_the_callers_privilege_not_a_fixed_value() {
        let harness = Harness::new(ServiceConfig::default());
        let hello = ClientRequest::Hello {
            client_version: PROTOCOL_VERSION as u32,
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
                    assert_eq!(server_version, PROTOCOL_VERSION as u32);
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

    #[test]
    fn volume_id_maps_back_to_the_drive_letter_it_came_from() {
        assert_eq!(drive_from_volume_id(0), Some('A'));
        assert_eq!(drive_from_volume_id(2), Some('C'));
        assert_eq!(drive_from_volume_id(25), Some('Z'));
        assert_eq!(drive_from_volume_id(26), None);
        assert_eq!(drive_from_volume_id(60000), None);
    }

    #[test]
    fn incompatible_client_version_is_rejected_at_the_message_gate() {
        let harness = Harness::new(ServiceConfig::default());
        let (code, message) = denial(harness.handle(
            ClientRequest::Hello {
                client_version: PROTOCOL_VERSION as u32 + 7,
                pid: 1,
            },
            CallerAuthorization::Administrator,
        ));
        assert_eq!(code, ERROR_CODE_PROTOCOL_VERSION_MISMATCH);
        assert!(
            message.contains("Incompatible protocol version"),
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
                sort_by_name: "name".to_string(),
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
}
