// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Named Pipe IPC client for Cairn GUI and CLI applications.

use cairn_protocol::codec::{CodecError, encode_frame_into, try_decode_frame};
use cairn_protocol::message::{
    ClientRequest, ERROR_CODE_ADMIN_ONLY_QUERY, ERROR_CODE_HANDSHAKE_REQUIRED,
    ERROR_CODE_PROTOCOL_VERSION_MISMATCH, ERROR_CODE_UNAUTHORIZED, Envelope, PIPE_NAME,
    PROTOCOL_VERSION, QueryDefaults, ResponsePayload, SortSpec, SourceId,
};
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

#[derive(Error, Debug)]
pub enum ClientError {
    #[error("Service connection failed (pipe not available): {0}")]
    ConnectFailed(#[from] std::io::Error),
    #[error("Codec error: {0}")]
    Codec(#[from] CodecError),
    #[error("Server returned protocol error {code}: {message}")]
    ServerProtocolError { code: u32, message: String },
    #[error(
        "The Cairn service speaks protocol version {server_protocol_version}; this app speaks protocol version {client_protocol_version}. Update the app and the service together."
    )]
    IncompatibleProtocolVersion {
        /// The protocol version the service declared in its frame header or handshake.
        server_protocol_version: u32,
        /// The protocol version this build declares.
        client_protocol_version: u32,
    },
    #[error("The Cairn service refused the protocol handshake: {message}")]
    HandshakeRejected { message: String },
    #[error("The Cairn service denied this request: {message}")]
    Unauthorized { message: String },
    #[error("Unexpected response payload received from service")]
    UnexpectedResponse,
    #[error("Service connection closed unexpectedly")]
    ConnectionClosed,
    #[error("Connection timeout")]
    Timeout,
}

/// `ERROR_PIPE_BUSY`: every instance of the pipe is occupied; the next open may succeed.
const ERROR_PIPE_BUSY: i32 = 231;

const READ_BUF_BASELINE: usize = 65_536;
const WRITE_BUF_BASELINE: usize = 4_096;

fn pipe_is_busy(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(ERROR_PIPE_BUSY)
}

impl ClientError {
    /// `true` when the service is reachable and healthy but speaks another protocol version, so a
    /// frontend can distinguish "service not running" from "service needs updating" (SVC-03, SVC-04).
    pub fn is_protocol_incompatible(&self) -> bool {
        matches!(
            self,
            ClientError::IncompatibleProtocolVersion { .. } | ClientError::HandshakeRejected { .. }
        )
    }

    /// `true` when the connected Windows account lacks the rights the request needs (SVC-05).
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, ClientError::Unauthorized { .. })
    }
}

#[derive(Debug)]
pub struct CairnClient {
    pipe: NamedPipeClient,
    read_buf: Vec<u8>,
    write_buf: Vec<u8>,
    read_timeout: Duration,
    server_protocol_version: u32,
    caller_is_admin: bool,
}

impl CairnClient {
    /// Default timeout for waiting on IPC responses from the Cairn service (10 seconds).
    pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);

    /// Connects to the running Cairn service and negotiates the protocol version.
    pub async fn connect() -> Result<Self, ClientError> {
        Self::connect_to(PIPE_NAME).await
    }

    /// Connects to a specific pipe name. Development runs and integration tests serve a pipe other
    /// than the installed service's, and must exercise the same handshake and authorization path.
    pub async fn connect_to(pipe_name: &str) -> Result<Self, ClientError> {
        let max_retries = 10;
        let mut delay = Duration::from_millis(50);
        let mut attempt = 0;

        loop {
            match Self::open_pipe_to(pipe_name) {
                Ok(pipe) => {
                    let mut client = Self {
                        pipe,
                        read_buf: Vec::with_capacity(READ_BUF_BASELINE),
                        write_buf: Vec::with_capacity(WRITE_BUF_BASELINE),
                        read_timeout: Self::DEFAULT_READ_TIMEOUT,
                        server_protocol_version: 0,
                        caller_is_admin: false,
                    };
                    // Negotiate protocol; `hello` records the confirmed version and privilege.
                    client.hello().await?;
                    return Ok(client);
                }
                Err(e) => {
                    // Only a busy pipe is transient: every server instance is occupied and the
                    // next open may succeed. A missing pipe means the service is not running, or
                    // not installed, which no amount of waiting can fix.
                    if !pipe_is_busy(&e) {
                        return Err(ClientError::ConnectFailed(e));
                    }
                    attempt += 1;
                    if attempt >= max_retries {
                        return Err(ClientError::ConnectFailed(e));
                    }
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_millis(500));
                }
            }
        }
    }

    /// Opens a pipe to `PIPE_NAME` requesting the impersonation security quality of service.
    ///
    /// The service authorizes requests from the *access token of this process*, which it can only
    /// read (including group membership) when the client asks for `SECURITY_IMPERSONATION` while
    /// opening the pipe. Without it the service treats the caller as a standard user and refuses
    /// privileged requests (SPEC SVC-02, SVC-05).
    fn open_pipe_to(pipe_name: &str) -> std::io::Result<NamedPipeClient> {
        #[cfg(windows)]
        {
            use windows::Win32::Storage::FileSystem::{
                SECURITY_IMPERSONATION, SECURITY_SQOS_PRESENT,
            };
            ClientOptions::new()
                .security_qos_flags(SECURITY_IMPERSONATION.0 | SECURITY_SQOS_PRESENT.0)
                .open(pipe_name)
        }
        #[cfg(not(windows))]
        {
            ClientOptions::new().open(pipe_name)
        }
    }

    /// Protocol version reported by the service during the handshake (0 before `hello`).
    pub fn server_protocol_version(&self) -> u32 {
        self.server_protocol_version
    }

    /// Whether the service classified *this process* as administrator or SYSTEM, derived from the
    /// access token on the server side. A frontend uses it to decide whether editing index scope is
    /// possible without elevation (SVC-05).
    pub fn caller_is_admin(&self) -> bool {
        self.caller_is_admin
    }

    /// Performs handshake with the server and validates protocol version compatibility (SVC-03).
    pub async fn hello(&mut self) -> Result<Envelope<ResponsePayload>, ClientError> {
        let pid = std::process::id();
        let resp = self
            .send_request(&ClientRequest::Hello {
                client_protocol_version: PROTOCOL_VERSION as u32,
                pid,
            })
            .await?;

        match &resp.payload {
            ResponsePayload::HelloAck {
                server_protocol_version,
                caller_is_admin,
            } => {
                if *server_protocol_version != PROTOCOL_VERSION as u32 {
                    return Err(ClientError::IncompatibleProtocolVersion {
                        server_protocol_version: *server_protocol_version,
                        client_protocol_version: u32::from(PROTOCOL_VERSION),
                    });
                }
                // Record what the service actually confirmed. Privileged calls rely on this;
                // the client never asserts a privilege the service did not report.
                self.server_protocol_version = *server_protocol_version;
                self.caller_is_admin = *caller_is_admin;
                Ok(resp)
            }
            ResponsePayload::Error { code, message } => Err(Self::handshake_error(*code, message)),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    fn handshake_error(code: u32, message: &str) -> ClientError {
        Self::service_error(code, message.to_string())
    }

    /// The one code-to-error mapping both the handshake and the reply envelope use.
    fn service_error(code: u32, message: String) -> ClientError {
        match code {
            ERROR_CODE_PROTOCOL_VERSION_MISMATCH | ERROR_CODE_HANDSHAKE_REQUIRED => {
                ClientError::HandshakeRejected { message }
            }
            ERROR_CODE_UNAUTHORIZED | ERROR_CODE_ADMIN_ONLY_QUERY => {
                ClientError::Unauthorized { message }
            }
            other => ClientError::ServerProtocolError {
                code: other,
                message,
            },
        }
    }

    /// Turns a service denial payload into a typed error, leaving successful payloads alone.
    fn unwrap_service_error(payload: ResponsePayload) -> Result<ResponsePayload, ClientError> {
        match payload {
            ResponsePayload::Error { code, message } => Err(Self::service_error(code, message)),
            other => Ok(other),
        }
    }

    /// Sends a query request and retrieves the first page of results.
    ///
    /// `defaults` carries the UI match toggles and the hidden/system filter as query-wide
    /// defaults; they are not part of `raw_query`. `source_scope` restricts the search to one
    /// indexed source; `None` searches every indexed source.
    pub async fn query(
        &mut self,
        query_id: u64,
        raw_query: &str,
        defaults: QueryDefaults,
        sort_spec: &SortSpec,
        first_page_limit: usize,
        source_scope: Option<SourceId>,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::Query {
            query_id,
            raw_query: raw_query.to_string(),
            defaults,
            sort_spec: sort_spec.clone(),
            first_page_limit,
            source_scope,
        })
        .await
    }

    /// Cancels an in-flight query by id.
    ///
    /// The request must travel on its own connection: the service answers each connection in
    /// order, so a cancel queued behind the query it targets would not be read until that query
    /// finished.
    pub async fn cancel(
        &mut self,
        query_id: u64,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::Cancel { query_id }).await
    }

    /// Fetches subsequent page of results for an active query session.
    pub async fn get_rows(
        &mut self,
        session_id: u64,
        offset: usize,
        limit: usize,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::GetRows {
            session_id,
            offset,
            limit,
        })
        .await
    }

    /// Reads the current state of every volume the service indexes. The state rides in the
    /// envelope's volume list; the payload only acknowledges the read.
    pub async fn get_status(&mut self) -> Result<Envelope<ResponsePayload>, ClientError> {
        let mut envelope = self.send_request(&ClientRequest::GetStatus).await?;
        envelope.payload = Self::unwrap_service_error(envelope.payload)?;
        Ok(envelope)
    }

    /// Reads the content-index lifecycle, including backfill progress and the last error.
    pub async fn get_content_index_status(
        &mut self,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::GetContentIndexStatus)
            .await
    }

    /// Clears the entire full-text content index (CNT-07).
    pub async fn clear_content_index(&mut self) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::ClearContentIndex).await
    }

    /// Triggers an asynchronous rescan of one volume (`Some(drive_index)`) or all indexed sources.
    pub async fn rescan(
        &mut self,
        volume_id: Option<u16>,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::Rescan { volume_id })
            .await
    }

    /// Lists all active indexed sources (NTFS volumes and folder sources).
    pub async fn get_sources(&mut self) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::GetSources).await
    }

    /// Registers a folder path as an indexed folder source and persists the configuration.
    pub async fn add_folder_source(
        &mut self,
        path: &str,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::AddFolderSource {
            path: path.to_string(),
        })
        .await
    }

    /// Unregisters a folder source by path and persists the configuration.
    pub async fn remove_folder_source(
        &mut self,
        path: &str,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::RemoveFolderSource {
            path: path.to_string(),
        })
        .await
    }

    /// Triggers a targeted rescan of a single indexed source.
    pub async fn rescan_source(
        &mut self,
        source_id: SourceId,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::RescanSource { source_id })
            .await
    }

    /// Reads the current status of the optional extended-property side-table index.
    pub async fn get_property_index_status(
        &mut self,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::GetPropertyIndexStatus)
            .await
    }

    /// Enables or disables extended-property indexing and persists the setting.
    pub async fn set_property_index_enabled(
        &mut self,
        enabled: bool,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::SetPropertyIndexEnabled { enabled })
            .await
    }

    /// Reads bounded index journal entries recorded strictly after `after_seq`.
    pub async fn get_index_journal(
        &mut self,
        after_seq: u64,
        limit: usize,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::GetIndexJournal { after_seq, limit })
            .await
    }

    /// Enables or disables bounded index journal recording and persists the setting.
    pub async fn set_index_journal_enabled(
        &mut self,
        enabled: bool,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::SetIndexJournalEnabled { enabled })
            .await
    }

    /// Retrieves service configuration TOML string.
    pub async fn get_config(&mut self) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::GetConfig).await
    }

    /// Requests reloading configuration from `%ProgramData%\Cairn\service.toml`.
    ///
    /// Any authenticated local caller may ask; when `admin_only_query` is active the service
    /// refuses standard users, and `Ok((false, message))` carries its reason. A reload that is
    /// allowed but cannot read the file also reports `Ok((false, message))`.
    pub async fn reload_service_config(&mut self) -> Result<(bool, String), ClientError> {
        let envelope = self.send_request(&ClientRequest::ReloadConfig).await?;
        match Self::unwrap_service_error(envelope.payload)? {
            ResponsePayload::ReloadConfigAck { success, message } => Ok((success, message)),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Submits a full service configuration to the service.
    ///
    /// Index configuration is a local-user operation: the service validates the document, persists
    /// it with its SYSTEM/Administrators descriptor and applies it without a UAC prompt.
    /// `Ok((false, message))` carries a policy refusal (a standard user changing
    /// `admin_only_query`, for example) or an invalid document.
    pub async fn submit_service_config(
        &mut self,
        config_toml: &str,
    ) -> Result<(bool, String), ClientError> {
        let envelope = self
            .send_request(&ClientRequest::SetConfig {
                config_toml: config_toml.to_string(),
            })
            .await?;
        match Self::unwrap_service_error(envelope.payload)? {
            ResponsePayload::SetConfigAck { success, message } => Ok((success, message)),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    /// Maps a service response into a typed error when the service denied the request, otherwise
    /// hands the payload back untouched. Frontends that use the envelope-returning methods call this
    /// before reading the payload, so a denial can never be mistaken for an empty result.
    pub fn unwrap_service_response(
        payload: ResponsePayload,
    ) -> Result<ResponsePayload, ClientError> {
        Self::unwrap_service_error(payload)
    }

    /// Overrides the per-response read timeout on this client connection.
    pub fn set_read_timeout(&mut self, timeout: Duration) {
        self.read_timeout = timeout;
    }

    /// Current allocated capacity of the inbound frame decode buffer.
    pub fn read_buf_capacity(&self) -> usize {
        self.read_buf.capacity()
    }

    /// Current allocated capacity of the outbound request encode buffer.
    pub fn write_buf_capacity(&self) -> usize {
        self.write_buf.capacity()
    }

    async fn send_request(
        &mut self,
        req: &ClientRequest,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.write_buf.clear();
        encode_frame_into(&mut self.write_buf, PROTOCOL_VERSION, req)?;
        let write_result = self.pipe.write_all(&self.write_buf).await;
        self.write_buf.clear();
        if self.write_buf.capacity() > WRITE_BUF_BASELINE {
            self.write_buf.shrink_to(WRITE_BUF_BASELINE);
        }
        write_result?;
        self.pipe.flush().await?;

        self.read_response().await
    }

    async fn read_response(&mut self) -> Result<Envelope<ResponsePayload>, ClientError> {
        tokio::time::timeout(self.read_timeout, self.read_response_inner())
            .await
            .map_err(|_| {
                self.reset_read_buf();
                ClientError::Timeout
            })?
    }

    fn shrink_read_buf_to_baseline(&mut self) {
        if self.read_buf.capacity() > READ_BUF_BASELINE && self.read_buf.len() <= READ_BUF_BASELINE
        {
            self.read_buf.shrink_to(READ_BUF_BASELINE);
        }
    }

    fn reset_read_buf(&mut self) {
        self.read_buf.clear();
        if self.read_buf.capacity() > READ_BUF_BASELINE {
            self.read_buf.shrink_to(READ_BUF_BASELINE);
        }
    }

    async fn read_response_inner(&mut self) -> Result<Envelope<ResponsePayload>, ClientError> {
        let mut temp_buf = [0u8; 8192];
        loop {
            match try_decode_frame::<Envelope<ResponsePayload>>(&self.read_buf) {
                Ok(Some((consumed, envelope))) => {
                    if consumed == self.read_buf.len() {
                        self.read_buf.clear();
                    } else {
                        self.read_buf.drain(0..consumed);
                    }
                    self.shrink_read_buf_to_baseline();
                    return Ok(envelope);
                }
                Ok(None) => {}
                // A frame whose header declares another version is a protocol mismatch, not a
                // transport failure: say so, so the GUI can show an update hint (SVC-03).
                Err(CodecError::IncompatibleVersion { received }) => {
                    self.reset_read_buf();
                    return Err(ClientError::IncompatibleProtocolVersion {
                        server_protocol_version: u32::from(received),
                        client_protocol_version: u32::from(PROTOCOL_VERSION),
                    });
                }
                Err(other) => {
                    self.reset_read_buf();
                    return Err(ClientError::Codec(other));
                }
            }

            let n = self.pipe.read(&mut temp_buf).await.inspect_err(|_| {
                self.reset_read_buf();
            })?;
            if n == 0 {
                self.reset_read_buf();
                return Err(ClientError::ConnectionClosed);
            }
            self.read_buf.extend_from_slice(&temp_buf[..n]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_protocol::codec::{encode_frame, encode_frame_with_version};
    #[cfg(windows)]
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

    #[tokio::test]
    #[cfg(windows)]
    async fn a_missing_pipe_fails_without_waiting_out_the_backoff() {
        let started = std::time::Instant::now();
        let pipe = format!(r"\\.\pipe\cairn.client.missing.{}", std::process::id());
        let Err(error) = CairnClient::connect_to(&pipe).await else {
            panic!("a missing pipe cannot connect");
        };
        assert!(matches!(error, ClientError::ConnectFailed(_)));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "a missing pipe must fail immediately, not wait out the retry backoff"
        );
    }

    #[test]
    fn only_a_busy_pipe_counts_as_a_transient_open_failure() {
        assert!(pipe_is_busy(&std::io::Error::from_raw_os_error(231)));
        assert!(!pipe_is_busy(&std::io::Error::from_raw_os_error(2)));
        assert!(!pipe_is_busy(&std::io::Error::from(
            std::io::ErrorKind::NotFound
        )));
    }

    #[test]
    fn service_denials_are_classified_for_the_frontend() {
        // The client-side mapping of the codes the service uses. The gates themselves are
        // exercised end to end over a real named pipe by the `stub_pipe_pair` tests below and
        // by cairn-service's caller-authentication tests.
        let unauthorized = ClientError::Unauthorized {
            message: "denied".to_string(),
        };
        assert!(unauthorized.is_unauthorized());
        assert!(!unauthorized.is_protocol_incompatible());

        let incompatible = ClientError::IncompatibleProtocolVersion {
            server_protocol_version: 2,
            client_protocol_version: u32::from(PROTOCOL_VERSION),
        };
        assert!(incompatible.is_protocol_incompatible());
        assert!(!incompatible.is_unauthorized());

        let rejected = ClientError::HandshakeRejected {
            message: "send Hello first".to_string(),
        };
        assert!(rejected.is_protocol_incompatible());

        // A standard user on a multi-user machine is not a protocol problem and not a service outage.
        let admin_only = CairnClient::unwrap_service_response(ResponsePayload::Error {
            code: ERROR_CODE_ADMIN_ONLY_QUERY,
            message: "administrators-only query mode".to_string(),
        });
        assert!(matches!(admin_only, Err(ClientError::Unauthorized { .. })));

        // Accepted payloads pass through untouched.
        let accepted = CairnClient::unwrap_service_response(ResponsePayload::ConfigResult {
            config_toml: "x = 1".to_string(),
        });
        assert!(matches!(accepted, Ok(ResponsePayload::ConfigResult { .. })));
    }

    #[cfg(windows)]
    fn client_over(pipe: NamedPipeClient) -> CairnClient {
        CairnClient {
            pipe,
            read_buf: Vec::with_capacity(4096),
            write_buf: Vec::with_capacity(WRITE_BUF_BASELINE),
            read_timeout: CairnClient::DEFAULT_READ_TIMEOUT,
            server_protocol_version: 0,
            caller_is_admin: false,
        }
    }

    /// Opens a pipe pair on a private name. `ClientOptions::open` blocks until an instance is ready,
    /// so it runs on a blocking thread while the test task arms the server.
    #[cfg(windows)]
    async fn stub_pipe_pair(name: &str) -> (NamedPipeServer, CairnClient) {
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(name)
            .expect("create stub pipe");
        let owned = name.to_string();
        let pipe = tokio::task::spawn_blocking(move || CairnClient::open_pipe_to(&owned))
            .await
            .expect("client thread")
            .expect("open stub pipe");
        (server, client_over(pipe))
    }

    /// Answers exactly one request with `reply`, using the real codec.
    #[cfg(windows)]
    async fn answer_one_request(server: &mut NamedPipeServer, reply: ResponsePayload) {
        server.connect().await.expect("stub connect");
        let mut buf = [0u8; 4096];
        let read = server.read(&mut buf).await.expect("stub read");
        assert!(
            read > 0,
            "the client must have sent a request frame before awaiting a reply"
        );
        let frame = encode_frame(&Envelope {
            volumes: Vec::new(),
            payload: reply,
        })
        .expect("encode stub answer");
        server.write_all(&frame).await.expect("stub write");
        server.flush().await.expect("stub flush");
    }

    /// The toggles travel as structured defaults, never spliced into the query text: the
    /// service must receive the text the user typed plus the flags separately.
    #[tokio::test]
    #[cfg(windows)]
    async fn query_sends_defaults_and_sort_spec_over_wire_and_decodes_rows() {
        use cairn_protocol::message::{
            FileRowData, PropertyKind, QueryDegradedReason, RecordPropertyData, SortColumn,
            SortDirection, SortKey, SortSpec,
        };

        let (mut server, mut client) = stub_pipe_pair(r"\\.\pipe\cairn.client.stub.query").await;
        let defaults = QueryDefaults {
            case_sensitive: true,
            match_path: true,
            exclude_hidden: true,
            exclude_system: true,
            ..Default::default()
        };
        let requested_sort = SortSpec {
            keys: vec![
                SortKey::new(SortColumn::Size, SortDirection::Descending),
                SortKey::new(SortColumn::Name, SortDirection::Ascending),
            ],
        };
        let raw = "报告 季度";
        let expected_sort = requested_sort.clone();
        let expected_scope = Some(SourceId(1_000));
        let task = tokio::spawn(async move {
            client
                .query(4, raw, defaults, &expected_sort, 10, expected_scope)
                .await
        });

        server.connect().await.expect("stub connect");
        let mut buf = [0u8; 4096];
        let read = server.read(&mut buf).await.expect("stub read");
        let (_, request) = try_decode_frame::<ClientRequest>(&buf[..read])
            .expect("stub decode")
            .expect("complete frame");

        match request {
            ClientRequest::Query {
                raw_query,
                defaults: received,
                sort_spec,
                first_page_limit,
                source_scope,
                ..
            } => {
                assert_eq!(raw_query, raw, "the text must reach the service as typed");
                assert!(
                    !raw_query.contains("case:") && !raw_query.contains("path:"),
                    "toggles must not be spliced into the text"
                );
                assert_eq!(received, defaults);
                assert_eq!(sort_spec, requested_sort);
                assert_eq!(first_page_limit, 10);
                assert_eq!(source_scope, expected_scope);
            }
            other => panic!("Expected a Query request, got {other:?}"),
        }

        let sample_row = FileRowData {
            frn: 42,
            name: "报告.docx".to_string(),
            path: r"C:\docs\报告.docx".to_string(),
            size: 4096,
            modified_ts: 1_700_000_000,
            created_ts: 1_690_000_000,
            is_dir: false,
            flags: 0,
            name_highlights: vec![(0, 2)],
            path_highlights: vec![],
            snippets: vec!["第一季度摘要".to_string(), "第二季度总结".to_string()],
            properties: Some(RecordPropertyData {
                length_ms: Some(12_000),
                author: Some("Alice".to_string()),
                tags: Some("finance".to_string()),
                dimensions: Some((1920, 1080)),
                framerate_mfps: Some(30_000),
            }),
        };
        let fallback_sort = SortSpec::default();
        let degraded_reason = QueryDegradedReason::PropertyIndexDisabled {
            requested: vec![PropertyKind::Author],
        };
        let frame = encode_frame(&Envelope {
            volumes: Vec::new(),
            payload: ResponsePayload::QueryResult {
                query_id: 4,
                session_id: 1,
                total_matches: 1,
                total_bytes: 4096,
                first_page: vec![sample_row.clone()],
                applied_sort: fallback_sort.clone(),
                degraded: Some(degraded_reason.clone()),
            },
        })
        .expect("encode stub answer");
        server.write_all(&frame).await.expect("stub write");
        server.flush().await.expect("stub flush");
        let env = task
            .await
            .expect("query task")
            .expect("query must succeed over the stub");
        match env.payload {
            ResponsePayload::QueryResult {
                first_page,
                applied_sort,
                degraded,
                ..
            } => {
                assert_eq!(first_page, vec![sample_row]);
                assert_eq!(applied_sort, fallback_sort);
                assert_eq!(degraded, Some(degraded_reason));
            }
            other => panic!("Expected QueryResult, got {other:?}"),
        }
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn cancel_sends_the_query_id() {
        let (mut server, mut client) = stub_pipe_pair(r"\\.\pipe\cairn.client.stub.cancel").await;
        let task = tokio::spawn(async move { client.cancel(9).await });

        server.connect().await.expect("stub connect");
        let mut buf = [0u8; 4096];
        let read = server.read(&mut buf).await.expect("stub read");
        let (_, request) = try_decode_frame::<ClientRequest>(&buf[..read])
            .expect("stub decode")
            .expect("complete frame");
        match request {
            ClientRequest::Cancel { query_id } => assert_eq!(query_id, 9),
            other => panic!("Expected a Cancel request, got {other:?}"),
        }

        let frame = encode_frame(&Envelope {
            volumes: Vec::new(),
            payload: ResponsePayload::CancelAck,
        })
        .expect("encode stub answer");
        server.write_all(&frame).await.expect("stub write");
        server.flush().await.expect("stub flush");
        task.await
            .expect("cancel task")
            .expect("cancel must succeed over the stub");
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn hello_reports_a_service_of_another_version_as_incompatible() {
        let (mut server, mut client) = stub_pipe_pair(r"\\.\pipe\cairn.client.stub.version").await;
        let reply = ResponsePayload::HelloAck {
            server_protocol_version: 999,
            caller_is_admin: false,
        };
        let task = tokio::spawn(async move { client.hello().await });
        answer_one_request(&mut server, reply).await;

        let error = task
            .await
            .unwrap()
            .expect_err("version 999 must not be accepted");
        match error {
            ClientError::IncompatibleProtocolVersion {
                server_protocol_version,
                client_protocol_version,
            } => {
                assert_eq!(server_protocol_version, 999);
                assert_eq!(client_protocol_version, u32::from(PROTOCOL_VERSION));
            }
            other => panic!("expected IncompatibleProtocolVersion, got {other:?}"),
        }
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn a_foreign_frame_header_is_reported_as_incompatible() {
        let (mut server, mut client) =
            stub_pipe_pair(r"\\.\pipe\cairn.client.stub.foreign_frame").await;
        let task = tokio::spawn(async move { client.get_status().await });

        server.connect().await.expect("stub connect");
        let mut buf = [0u8; 4096];
        let read = server.read(&mut buf).await.expect("stub read");
        assert!(read > 0, "the client must send its request first");

        let peer_protocol_version = PROTOCOL_VERSION + 1;
        let frame = encode_frame_with_version(
            peer_protocol_version,
            &Envelope {
                volumes: Vec::new(),
                payload: ResponsePayload::CancelAck,
            },
        )
        .expect("encode foreign frame");
        server.write_all(&frame).await.expect("stub write");
        server.flush().await.expect("stub flush");

        let error = task
            .await
            .unwrap()
            .expect_err("the foreign frame header must be rejected");
        match error {
            ClientError::IncompatibleProtocolVersion {
                server_protocol_version,
                client_protocol_version,
            } => {
                assert_eq!(server_protocol_version, u32::from(peer_protocol_version));
                assert_eq!(client_protocol_version, u32::from(PROTOCOL_VERSION));
            }
            other => panic!("expected IncompatibleProtocolVersion, got {other:?}"),
        }
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn a_rejected_handshake_becomes_an_explainable_client_error() {
        let (mut server, mut client) = stub_pipe_pair(r"\\.\pipe\cairn.client.stub.rejected").await;
        let reply = ResponsePayload::Error {
            code: ERROR_CODE_PROTOCOL_VERSION_MISMATCH,
            message: "Incompatible protocol version: client=2, server=1".to_string(),
        };
        let task = tokio::spawn(async move { client.hello().await });
        answer_one_request(&mut server, reply).await;

        let error = task
            .await
            .unwrap()
            .expect_err("a rejected handshake is an error");
        assert!(error.is_protocol_incompatible(), "{error:?}");
        assert!(
            error.to_string().contains("client=2"),
            "the service reason must reach the user: {error}"
        );
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn reload_of_a_denied_request_surfaces_unauthorized() {
        let (mut server, mut client) = stub_pipe_pair(r"\\.\pipe\cairn.client.stub.denied").await;
        let reply = ResponsePayload::Error {
            code: ERROR_CODE_UNAUTHORIZED,
            message: "StandardUser may not change service configuration (SVC-05)".to_string(),
        };
        let task = tokio::spawn(async move { client.reload_service_config().await });
        answer_one_request(&mut server, reply).await;

        let error = task
            .await
            .unwrap()
            .expect_err("a denial is not a successful reload");
        assert!(error.is_unauthorized(), "{error:?}");
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn successful_handshake_keeps_the_privilege_the_service_observed() {
        let (mut server, client) = stub_pipe_pair(r"\\.\pipe\cairn.client.stub.privilege").await;
        let reply = ResponsePayload::HelloAck {
            server_protocol_version: PROTOCOL_VERSION as u32,
            caller_is_admin: false,
        };
        let task = tokio::spawn(async move {
            let mut client = client;
            client.hello().await?;
            Ok::<_, ClientError>(client)
        });
        answer_one_request(&mut server, reply).await;

        let client = task.await.unwrap().expect("compatible handshake");
        assert_eq!(client.server_protocol_version(), PROTOCOL_VERSION as u32);
        assert!(
            !client.caller_is_admin(),
            "the client must not invent a privilege the service did not confirm"
        );
    }

    #[tokio::test(start_paused = true)]
    #[cfg(windows)]
    async fn read_response_times_out_when_service_hangs() {
        let (mut server, mut client) =
            stub_pipe_pair(r"\\.\pipe\cairn.client.stub.read_timeout").await;
        let task = tokio::spawn(async move { client.get_status().await });

        server.connect().await.expect("stub connect");
        let mut buf = [0u8; 4096];
        let read = server.read(&mut buf).await.expect("stub read");
        assert!(read > 0);

        // Advance time beyond DEFAULT_READ_TIMEOUT without server writing any response
        tokio::time::advance(CairnClient::DEFAULT_READ_TIMEOUT + Duration::from_secs(1)).await;

        let result = task.await.expect("task must complete");
        assert!(
            matches!(result, Err(ClientError::Timeout)),
            "expected Timeout, got {result:?}"
        );
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn platform_control_methods_round_trip_each_request_and_response() {
        use cairn_protocol::message::SourceId;
        use cairn_protocol::status::{
            ContentIndexState, IndexJournalEntry, IndexJournalStatus, JournalChangeKind,
            PropertyIndexStatus, SourceKind, SourceStatusSummary, VolumeState,
        };
        use tokio::net::windows::named_pipe::ServerOptions;

        let pipe = r"\\.\pipe\cairn.client.stub.platform_controls";
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(pipe)
            .expect("create server");
        let pipe_name = pipe.to_string();
        let client_fut = tokio::spawn(async move {
            let mut client = CairnClient::connect_to(&pipe_name).await?;
            let r1 = client.rescan(Some(2)).await?;
            let r2 = client.get_sources().await?;
            let r3 = client.add_folder_source(r"D:\Work").await?;
            let r4 = client.remove_folder_source(r"D:\Work").await?;
            let r5 = client.rescan_source(SourceId(1_000)).await?;
            let r6 = client.get_property_index_status().await?;
            let r7 = client.set_property_index_enabled(true).await?;
            let r8 = client.get_index_journal(10, 50).await?;
            let r9 = client.set_index_journal_enabled(false).await?;
            Ok::<_, ClientError>((r1, r2, r3, r4, r5, r6, r7, r8, r9))
        });

        server.connect().await.expect("accept");
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        let sample_source = SourceStatusSummary {
            source_id: SourceId(1_000),
            kind: SourceKind::Folder,
            root_path: r"D:\Work".to_string(),
            volume_guid: String::new(),
            status: VolumeState::Synced,
            indexed_count: 12,
            live_watch_enabled: true,
        };
        let sample_prop_status = PropertyIndexStatus {
            state: ContentIndexState::Ready,
            enabled: true,
            indexed_records: 8,
            side_table_bytes: 512,
            last_updated_ts: 1_700_000_000,
        };
        let sample_journal_status = IndexJournalStatus {
            enabled: true,
            entry_count: 1,
            resident_bytes: 128,
            max_entries: 200_000,
            max_bytes: 32 * 1024 * 1024,
            oldest_seq: 11,
            newest_seq: 11,
        };

        loop {
            let req = loop {
                if let Some((consumed, req)) = try_decode_frame::<ClientRequest>(&buf).unwrap() {
                    buf.drain(0..consumed);
                    break Some(req);
                }
                let n = server.read(&mut tmp).await.unwrap();
                if n == 0 {
                    break None;
                }
                buf.extend_from_slice(&tmp[..n]);
            };
            let Some(req) = req else {
                break;
            };
            let reply = match req {
                ClientRequest::Hello { .. } => ResponsePayload::HelloAck {
                    server_protocol_version: PROTOCOL_VERSION as u32,
                    caller_is_admin: true,
                },
                ClientRequest::Rescan { volume_id } => {
                    assert_eq!(volume_id, Some(2));
                    ResponsePayload::RescanAck {
                        volume_id,
                        success: true,
                    }
                }
                ClientRequest::GetSources => ResponsePayload::SourcesResult {
                    sources: vec![sample_source.clone()],
                },
                ClientRequest::AddFolderSource { path } => {
                    assert_eq!(path, r"D:\Work");
                    ResponsePayload::SourcesResult {
                        sources: vec![sample_source.clone()],
                    }
                }
                ClientRequest::RemoveFolderSource { path } => {
                    assert_eq!(path, r"D:\Work");
                    ResponsePayload::SourcesResult { sources: vec![] }
                }
                ClientRequest::RescanSource { source_id } => {
                    assert_eq!(source_id, SourceId(1_000));
                    ResponsePayload::RescanSourceAck {
                        source_id,
                        success: true,
                    }
                }
                ClientRequest::GetPropertyIndexStatus => ResponsePayload::PropertyIndexStatus {
                    status: PropertyIndexStatus::default(),
                },
                ClientRequest::SetPropertyIndexEnabled { enabled } => {
                    assert!(enabled);
                    ResponsePayload::PropertyIndexStatus {
                        status: sample_prop_status.clone(),
                    }
                }
                ClientRequest::GetIndexJournal { after_seq, limit } => {
                    assert_eq!((after_seq, limit), (10, 50));
                    ResponsePayload::IndexJournalResult {
                        status: sample_journal_status.clone(),
                        entries: vec![IndexJournalEntry {
                            seq: 11,
                            timestamp_ts: 1_700_000_005,
                            source_id: SourceId(1_000),
                            frn: 7,
                            kind: JournalChangeKind::Created,
                            path: r"D:\Work\a.txt".to_string(),
                            old_path: None,
                        }],
                    }
                }
                ClientRequest::SetIndexJournalEnabled { enabled } => {
                    assert!(!enabled);
                    ResponsePayload::IndexJournalStatus {
                        status: IndexJournalStatus {
                            enabled: false,
                            ..sample_journal_status.clone()
                        },
                    }
                }
                other => panic!("unexpected request: {other:?}"),
            };
            let frame = encode_frame(&Envelope {
                volumes: Vec::new(),
                payload: reply,
            })
            .unwrap();
            server.write_all(&frame).await.unwrap();
            server.flush().await.unwrap();
        }

        let (r1, r2, r3, r4, r5, r6, r7, r8, r9) =
            client_fut.await.unwrap().expect("platform calls succeed");
        assert!(matches!(
            r1.payload,
            ResponsePayload::RescanAck {
                volume_id: Some(2),
                success: true
            }
        ));
        assert!(matches!(
            r2.payload,
            ResponsePayload::SourcesResult { ref sources } if sources.len() == 1
        ));
        assert!(matches!(
            r3.payload,
            ResponsePayload::SourcesResult { ref sources } if sources.len() == 1
        ));
        assert!(matches!(
            r4.payload,
            ResponsePayload::SourcesResult { ref sources } if sources.is_empty()
        ));
        assert!(matches!(
            r5.payload,
            ResponsePayload::RescanSourceAck {
                source_id: SourceId(1_000),
                success: true
            }
        ));
        assert!(matches!(
            r6.payload,
            ResponsePayload::PropertyIndexStatus { ref status } if !status.enabled
        ));
        assert!(matches!(
            r7.payload,
            ResponsePayload::PropertyIndexStatus { ref status } if status.enabled
        ));
        assert!(matches!(
            r8.payload,
            ResponsePayload::IndexJournalResult { ref entries, .. } if entries.len() == 1
        ));
        assert!(matches!(
            r9.payload,
            ResponsePayload::IndexJournalStatus { ref status } if !status.enabled
        ));
    }
}
