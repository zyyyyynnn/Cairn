// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Async Named Pipe IPC server listening on `\\.\pipe\Cairn.Service`.
//!
//! Every connection is attributed to the OS identity of the client process (see
//! [`crate::caller_auth`]); authorization is then evaluated per request, against the configuration
//! the engine currently holds, so `admin_only_query` takes effect on the next request rather than on
//! the next pipe instance.

use crate::caller_auth::{CallerAuthorization, authenticate_pipe_client, authorize};
use crate::security::SafeSecurityDescriptor;
use crate::session::{QueryOwner, RowsError, SessionManager};
use crate::sync_engine::MultiVolumeEngine;
use cairn_core::property_store::SourceId;
use cairn_core::query::QueryDegradedReason;
use cairn_core::sort::SortSpec;
use cairn_core::syntax::{MAX_CHILD_FILTERS, MAX_CONTENT_TERMS, QueryDefaults, parse_query_with};
use cairn_protocol::codec::{
    encode_frame_into, encode_frame_with_version, peek_frame_header, try_decode_frame,
};
use cairn_protocol::message::{
    ClientRequest, ERROR_CODE_HANDSHAKE_REQUIRED, ERROR_CODE_INVALID_REQUEST,
    ERROR_CODE_PROTOCOL_VERSION_MISMATCH, ERROR_CODE_QUERY_TOO_COMPLEX, ERROR_CODE_SESSION_EXPIRED,
    Envelope, FileRowData, MAX_SORT_KEYS, PIPE_NAME, PROTOCOL_VERSION, ResponsePayload,
};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use tokio::sync::Semaphore;
use tracing::{error, info, warn};

const WRITE_BUF_SHRINK_THRESHOLD_BYTES: usize = 256 * 1024;
const WRITE_BUF_RETAIN_BYTES: usize = 64 * 1024;
const MAX_ACTIVE_CLIENTS: usize = 64;
const FRAME_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const IDLE_FRAME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const FRAME_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn shrink_oversized_write_buf(buf: &mut Vec<u8>) {
    if buf.is_empty() && buf.capacity() > WRITE_BUF_SHRINK_THRESHOLD_BYTES {
        buf.shrink_to(WRITE_BUF_RETAIN_BYTES);
    }
}

pub struct PipeServer {
    engine: Arc<MultiVolumeEngine>,
    sessions: Arc<SessionManager>,
    pipe_name: String,
    protocol_version: u16,
    active_clients: Arc<Semaphore>,
    heavy_tasks: Arc<Semaphore>,
}

fn heavy_task_limit() -> usize {
    std::thread::available_parallelism().map_or(4, |parallelism| parallelism.get().clamp(2, 8))
}

fn is_heavy_request(req: &ClientRequest) -> bool {
    match req {
        ClientRequest::Query { .. }
        | ClientRequest::Rescan { .. }
        | ClientRequest::RescanSource { .. } => true,
        ClientRequest::GetRows { limit, .. } | ClientRequest::GetIndexJournal { limit, .. } => {
            *limit > 500
        }
        _ => false,
    }
}

impl PipeServer {
    pub fn new(engine: Arc<MultiVolumeEngine>, sessions: Arc<SessionManager>) -> Self {
        engine.ensure_content_runtime();
        engine.ensure_property_runtime();
        Self {
            engine,
            sessions,
            pipe_name: PIPE_NAME.to_string(),
            protocol_version: PROTOCOL_VERSION,
            active_clients: Arc::new(Semaphore::new(MAX_ACTIVE_CLIENTS)),
            heavy_tasks: Arc::new(Semaphore::new(heavy_task_limit())),
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
            let client_permit = self
                .active_clients
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| std::io::Error::other("pipe client limiter closed"))?;
            server.connect().await?;
            let connected_server = server;

            server = self.create_pipe_instance(false)?;

            let engine = self.engine.clone();
            let sessions = self.sessions.clone();
            let protocol_version = self.protocol_version;
            let pipe_name = self.pipe_name.clone();
            let heavy_tasks = Arc::clone(&self.heavy_tasks);

            tokio::spawn(async move {
                let _client_permit = client_permit;
                if let Err(e) = handle_client(
                    connected_server,
                    engine,
                    sessions,
                    protocol_version,
                    heavy_tasks,
                )
                .await
                {
                    if let Some(io_err) = e.downcast_ref::<std::io::Error>()
                        && is_pipe_closed_error(io_err)
                    {
                        return;
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
        options.reject_remote_clients(true);

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
                use windows::Win32::Foundation::{
                    ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED,
                };
                return code == ERROR_BROKEN_PIPE.0 as i32
                    || code == ERROR_NO_DATA.0 as i32
                    || code == ERROR_PIPE_NOT_CONNECTED.0 as i32;
            }
            false
        }
    }
}

struct ClientLease {
    sessions: Arc<SessionManager>,
    owner: QueryOwner,
}

impl Drop for ClientLease {
    fn drop(&mut self) {
        self.sessions.release_client(&self.owner);
    }
}

async fn write_frame_with_timeout(
    pipe: &mut NamedPipeServer,
    bytes: &[u8],
) -> Result<bool, std::io::Error> {
    match tokio::time::timeout(FRAME_WRITE_TIMEOUT, pipe.write_all(bytes)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) if is_pipe_closed_error(&e) => return Ok(false),
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out writing response frame",
            ));
        }
    }
    match tokio::time::timeout(FRAME_WRITE_TIMEOUT, pipe.flush()).await {
        Ok(Ok(())) => Ok(true),
        Ok(Err(e)) if is_pipe_closed_error(&e) => Ok(false),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "timed out flushing response frame",
        )),
    }
}

async fn read_frame_chunk_with_timeout(
    pipe: &mut NamedPipeServer,
    temp_buf: &mut [u8],
    has_partial_frame: bool,
    timeout: std::time::Duration,
) -> Result<usize, std::io::Error> {
    let io_result = tokio::time::timeout(timeout, pipe.read(temp_buf))
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                if has_partial_frame {
                    "timed out waiting for complete request frame"
                } else {
                    "timed out waiting for request frame"
                },
            )
        })?;
    match io_result {
        Ok(n) => Ok(n),
        Err(e) if is_pipe_closed_error(&e) => Ok(0),
        Err(e) => Err(e),
    }
}

async fn handle_client(
    mut pipe: NamedPipeServer,
    engine: Arc<MultiVolumeEngine>,
    sessions: Arc<SessionManager>,
    protocol_version: u16,
    heavy_tasks: Arc<Semaphore>,
) -> Result<(), Box<dyn std::error::Error>> {
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
                warn!("rejecting pipe connection: {err}");
                return Ok(());
            }
        };
    #[cfg(not(windows))]
    let (caller, owner) = (CallerAuthorization::Unauthenticated, QueryOwner::default());

    sessions.register_client(&owner);
    let _lease = ClientLease {
        sessions: Arc::clone(&sessions),
        owner: owner.clone(),
    };

    let mut read_buf = Vec::with_capacity(65536);
    let mut write_buf = Vec::with_capacity(4096);
    let mut temp_buf = [0u8; 8192];
    let mut hello_done = false;
    let mut frame_deadline: Option<Instant> = None;

    loop {
        if let Some(header) = peek_frame_header(&read_buf)? {
            if header.version != protocol_version {
                write_buf = protocol_mismatch_frame(header.version, protocol_version)?;
                let _ = write_frame_with_timeout(&mut pipe, &write_buf).await?;
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

                let (resp_payload, volumes) =
                    if !hello_done && !matches!(req, ClientRequest::Hello { .. }) {
                        (
                            ResponsePayload::Error {
                                code: ERROR_CODE_HANDSHAKE_REQUIRED,
                                message: "protocol negotiation missing: send Hello first"
                                    .to_string(),
                            },
                            Vec::new(),
                        )
                    } else {
                        let _heavy_permit = if is_heavy_request(&req) {
                            Some(heavy_tasks.clone().acquire_owned().await.map_err(|_| {
                                std::io::Error::other("heavy request limiter closed")
                            })?)
                        } else {
                            None
                        };
                        let admin_only_query = engine.admin_only_query();
                        let engine_blocking = Arc::clone(&engine);
                        let sessions_blocking = Arc::clone(&sessions);
                        let owner_blocking = owner.clone();
                        tokio::task::spawn_blocking(move || {
                            let resp_payload = process_request(
                                req,
                                &engine_blocking,
                                &sessions_blocking,
                                caller,
                                &owner_blocking,
                                admin_only_query,
                                protocol_version,
                            );
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
                            let volumes = if omit_volumes {
                                Vec::new()
                            } else if matches!(&resp_payload, ResponsePayload::QueryResult { .. }) {
                                engine_blocking.get_volume_summaries_fresh()
                            } else {
                                engine_blocking.get_volume_summaries()
                            };
                            (resp_payload, volumes)
                        })
                        .await
                        .map_err(|err| format!("blocking request task failed: {err}"))?
                    };
                if matches!(resp_payload, ResponsePayload::HelloAck { .. }) {
                    hello_done = true;
                }
                let envelope = Envelope {
                    volumes,
                    payload: resp_payload,
                };

                write_buf.clear();
                encode_frame_into(&mut write_buf, protocol_version, &envelope)?;
                if !write_frame_with_timeout(&mut pipe, &write_buf).await? {
                    return Ok(());
                }
                write_buf.clear();
                shrink_oversized_write_buf(&mut write_buf);
                continue;
            }
        }

        let starting_frame = read_buf.is_empty();
        if starting_frame {
            let timeout = if hello_done {
                IDLE_FRAME_TIMEOUT
            } else {
                FRAME_READ_TIMEOUT
            };
            frame_deadline = Some(Instant::now() + timeout);
        }
        let remaining = frame_deadline
            .expect("frame deadline set")
            .saturating_duration_since(Instant::now());
        let n = read_frame_chunk_with_timeout(
            &mut pipe,
            &mut temp_buf,
            !read_buf.is_empty(),
            remaining,
        )
        .await?;
        if starting_frame && n > 0 {
            frame_deadline = Some(Instant::now() + FRAME_READ_TIMEOUT);
        }
        if n == 0 {
            break;
        }
        read_buf.extend_from_slice(&temp_buf[..n]);
    }

    Ok(())
}

fn protocol_mismatch_frame(
    peer_protocol_version: u16,
    server_protocol_version: u16,
) -> Result<Vec<u8>, cairn_protocol::codec::CodecError> {
    encode_frame_with_version(
        peer_protocol_version,
        &Envelope {
            volumes: Vec::new(),
            payload: version_mismatch(server_protocol_version, u32::from(peer_protocol_version)),
        },
    )
}

fn version_mismatch(server_protocol_version: u16, client_protocol_version: u32) -> ResponsePayload {
    ResponsePayload::Error {
        code: ERROR_CODE_PROTOCOL_VERSION_MISMATCH,
        message: format!(
            "Incompatible protocol: client version {client_protocol_version}, service version {server_protocol_version}."
        ),
    }
}

struct QueryParams {
    query_id: u64,
    raw_query: String,
    defaults: QueryDefaults,
    sort_spec: SortSpec,
    first_page_limit: usize,
    source_scope: Option<SourceId>,
}

fn handle_query_request(
    engine: &Arc<MultiVolumeEngine>,
    sessions: &SessionManager,
    owner: &QueryOwner,
    params: QueryParams,
) -> ResponsePayload {
    let QueryParams {
        query_id,
        raw_query,
        defaults,
        mut sort_spec,
        first_page_limit,
        source_scope,
    } = params;

    if let Some(source_id) = source_scope
        && !engine.is_known_source(source_id)
    {
        return ResponsePayload::Error {
            code: ERROR_CODE_INVALID_REQUEST,
            message: format!(
                "query source_scope {} is not an active indexed source",
                source_id.0
            ),
        };
    }
    let parsed = parse_query_with(&raw_query, &defaults);
    let content_terms = parsed.content_term_count();
    if content_terms > MAX_CONTENT_TERMS {
        return ResponsePayload::Error {
            code: ERROR_CODE_QUERY_TOO_COMPLEX,
            message: format!(
                "query carries {content_terms} content terms; the limit is {MAX_CONTENT_TERMS}"
            ),
        };
    }
    if parsed.child_filter_count() > MAX_CHILD_FILTERS {
        return ResponsePayload::Error {
            code: ERROR_CODE_QUERY_TOO_COMPLEX,
            message: format!(
                "query carries {} child filters; the limit is {MAX_CHILD_FILTERS}",
                parsed.child_filter_count()
            ),
        };
    }
    sort_spec.keys.truncate(MAX_SORT_KEYS);

    struct QueryReply {
        session_id: u64,
        total_matches: usize,
        total_bytes: u64,
        first_page: Vec<FileRowData>,
        applied_sort: SortSpec,
        degraded: Option<QueryDegradedReason>,
    }

    struct ActiveQueryGuard<'a> {
        sessions: &'a SessionManager,
        owner: &'a crate::session::QueryOwner,
        query_id: u64,
    }

    impl Drop for ActiveQueryGuard<'_> {
        fn drop(&mut self) {
            self.sessions.finish_query(self.owner, self.query_id);
        }
    }

    let query_cancel = sessions.begin_query(owner, query_id);
    let active_guard = ActiveQueryGuard {
        sessions,
        owner,
        query_id,
    };
    let mut answered: Option<QueryReply> = None;
    let mut fallback: Option<QueryReply> = None;
    for attempt in 0..2 {
        let outcome = engine.execute_search_multi_scoped(
            &raw_query,
            &defaults,
            &sort_spec,
            &query_cancel,
            source_scope,
        );
        let total_matches = outcome.total;
        let total_bytes = outcome.total_bytes;
        let applied_sort = outcome.applied_sort;
        let degraded = outcome.degraded;
        let session_id = sessions.create_session(
            owner.clone(),
            outcome.rows,
            &outcome.highlight_terms,
            outcome.snippets,
        );
        let first_page = engine.with_sources_for_rows(|sources| {
            sessions.get_rows_multi(owner, session_id, 0, first_page_limit, &sources)
        });
        match first_page {
            Ok(rows) => {
                answered = Some(QueryReply {
                    session_id,
                    total_matches,
                    total_bytes,
                    first_page: rows,
                    applied_sort,
                    degraded,
                });
                break;
            }
            Err(err) => {
                warn!(
                    "query session {session_id} expired before its first page \
                     (attempt {}): {err:?}",
                    attempt + 1
                );
                sessions.invalidate(session_id);
                fallback = Some(QueryReply {
                    session_id,
                    total_matches,
                    total_bytes,
                    first_page: Vec::new(),
                    applied_sort,
                    degraded,
                });
            }
        }
    }
    drop(active_guard);

    let QueryReply {
        session_id,
        total_matches,
        total_bytes,
        first_page,
        applied_sort,
        degraded,
    } = answered
        .or(fallback)
        .expect("the first attempt always answers or falls back");
    ResponsePayload::QueryResult {
        query_id,
        session_id,
        total_matches,
        total_bytes,
        first_page,
        applied_sort,
        degraded,
    }
}

fn process_request(
    req: ClientRequest,
    engine: &Arc<MultiVolumeEngine>,
    sessions: &SessionManager,
    caller: CallerAuthorization,
    owner: &QueryOwner,
    admin_only_query: bool,
    protocol_version: u16,
) -> ResponsePayload {
    if let Err((code, message)) = authorize(&req, caller, admin_only_query) {
        warn!("denied {code} from {caller:?}: {message}");
        return ResponsePayload::Error { code, message };
    }

    match req {
        ClientRequest::Hello {
            client_protocol_version,
            pid,
        } => {
            if client_protocol_version != u32::from(protocol_version) {
                version_mismatch(protocol_version, client_protocol_version)
            } else {
                info!(
                    "handshake with self-reported pid {pid} accepted; caller classified as {caller:?}"
                );
                ResponsePayload::HelloAck {
                    server_protocol_version: u32::from(protocol_version),
                    caller_is_admin: caller.is_privileged(),
                }
            }
        }
        ClientRequest::Query {
            query_id,
            raw_query,
            defaults,
            sort_spec,
            first_page_limit,
            source_scope,
        } => handle_query_request(
            engine,
            sessions,
            owner,
            QueryParams {
                query_id,
                raw_query,
                defaults,
                sort_spec,
                first_page_limit,
                source_scope,
            },
        ),
        ClientRequest::GetRows {
            session_id,
            offset,
            limit,
        } => {
            let rows = engine.with_sources_for_rows(|sources| {
                sessions.get_rows_multi(owner, session_id, offset, limit, &sources)
            });
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
                    let mut rescanned = 0;
                    for drive in engine.active_drives() {
                        match engine.rescan_volume(drive) {
                            Ok(_) => rescanned += 1,
                            Err(reason) => {
                                warn!(drive = %drive, error = %reason, "volume rescan failed");
                            }
                        }
                    }
                    rescanned += engine.rescan_folder_sources();
                    if rescanned > 0 {
                        Ok(())
                    } else {
                        Err("no indexed source was rescanned".to_string())
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
        ClientRequest::CancelContentIndex => ResponsePayload::CancelContentIndexAck {
            success: engine.disable_content_index().is_ok(),
        },
        ClientRequest::ClearContentIndex => {
            if engine.disable_content_index().is_err() {
                ResponsePayload::ClearContentIndexAck { success: false }
            } else {
                ResponsePayload::ClearContentIndexAck {
                    success: engine.request_clear_content_index(),
                }
            }
        }
        ClientRequest::GetSources => ResponsePayload::SourcesResult {
            sources: engine.get_sources(),
        },
        ClientRequest::AddFolderSource { path } => {
            match engine.add_folder_source(std::path::Path::new(&path), true) {
                Ok(_) => ResponsePayload::SourcesResult {
                    sources: engine.get_sources(),
                },
                Err(message) => ResponsePayload::Error {
                    code: ERROR_CODE_INVALID_REQUEST,
                    message,
                },
            }
        }
        ClientRequest::RemoveFolderSource { path } => {
            match engine.remove_folder_source(std::path::Path::new(&path), true) {
                Ok(true) => ResponsePayload::SourcesResult {
                    sources: engine.get_sources(),
                },
                Ok(false) => ResponsePayload::Error {
                    code: ERROR_CODE_INVALID_REQUEST,
                    message: format!("folder source is not configured: {path}"),
                },
                Err(message) => ResponsePayload::Error {
                    code: ERROR_CODE_INVALID_REQUEST,
                    message,
                },
            }
        }
        ClientRequest::RescanSource { source_id } => {
            let success = match engine.rescan_source(source_id) {
                Ok(_) => true,
                Err(reason) => {
                    info!("rescan of source {} did not run: {reason}", source_id.0);
                    false
                }
            };
            ResponsePayload::RescanSourceAck { source_id, success }
        }
        ClientRequest::GetPropertyIndexStatus => ResponsePayload::PropertyIndexStatus {
            status: engine.property_index_status(),
        },
        ClientRequest::SetPropertyIndexEnabled { enabled } => {
            match engine.set_property_index_enabled_persisted(enabled, true) {
                Ok(status) => ResponsePayload::PropertyIndexStatus { status },
                Err(message) => ResponsePayload::Error {
                    code: ERROR_CODE_INVALID_REQUEST,
                    message,
                },
            }
        }
        ClientRequest::GetIndexJournal { after_seq, limit } => {
            ResponsePayload::IndexJournalResult {
                status: engine.index_journal_status(),
                entries: engine.get_index_journal(after_seq, limit),
            }
        }
        ClientRequest::SetIndexJournalEnabled { enabled } => {
            match engine.set_index_journal_enabled_persisted(enabled, true) {
                Ok(status) => ResponsePayload::IndexJournalStatus { status },
                Err(message) => ResponsePayload::Error {
                    code: ERROR_CODE_INVALID_REQUEST,
                    message,
                },
            }
        }
    }
}

#[cfg(test)]
mod tests;
