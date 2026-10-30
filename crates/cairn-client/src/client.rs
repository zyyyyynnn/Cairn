// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Named Pipe IPC client for Cairn GUI and CLI applications.

use cairn_protocol::codec::{encode_frame, try_decode_frame, CodecError};
use cairn_protocol::message::{
    ClientRequest, Envelope, QueryDefaults, ResponsePayload, ERROR_CODE_ADMIN_ONLY_QUERY,
    ERROR_CODE_HANDSHAKE_REQUIRED, ERROR_CODE_PROTOCOL_VERSION_MISMATCH, ERROR_CODE_UNAUTHORIZED,
    PIPE_NAME, PROTOCOL_VERSION,
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
        "Incompatible protocol version: server={server_version}, supported={supported_version}"
    )]
    IncompatibleProtocolVersion {
        server_version: u32,
        supported_version: u32,
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

pub struct CairnClient {
    pipe: NamedPipeClient,
    read_buf: Vec<u8>,
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
                        read_buf: Vec::with_capacity(65536),
                        server_protocol_version: 0,
                        caller_is_admin: false,
                    };
                    // Negotiate protocol; `hello` records the confirmed version and privilege.
                    client.hello().await?;
                    return Ok(client);
                }
                Err(e) => {
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
                client_version: PROTOCOL_VERSION as u32,
                pid,
            })
            .await?;

        match &resp.payload {
            ResponsePayload::HelloAck {
                server_version,
                caller_is_admin,
            } => {
                if *server_version != PROTOCOL_VERSION as u32 {
                    return Err(ClientError::IncompatibleProtocolVersion {
                        server_version: *server_version,
                        supported_version: PROTOCOL_VERSION as u32,
                    });
                }
                // Record what the service actually confirmed. Privileged calls rely on this;
                // the client never asserts a privilege the service did not report.
                self.server_protocol_version = *server_version;
                self.caller_is_admin = *caller_is_admin;
                Ok(resp)
            }
            ResponsePayload::Error { code, message } => Err(Self::handshake_error(*code, message)),
            _ => Err(ClientError::UnexpectedResponse),
        }
    }

    fn handshake_error(code: u32, message: &str) -> ClientError {
        match code {
            ERROR_CODE_PROTOCOL_VERSION_MISMATCH | ERROR_CODE_HANDSHAKE_REQUIRED => {
                ClientError::HandshakeRejected {
                    message: message.to_string(),
                }
            }
            ERROR_CODE_UNAUTHORIZED | ERROR_CODE_ADMIN_ONLY_QUERY => ClientError::Unauthorized {
                message: message.to_string(),
            },
            _ => ClientError::ServerProtocolError {
                code,
                message: message.to_string(),
            },
        }
    }

    /// Turns a service denial payload into a typed error, leaving successful payloads alone.
    fn unwrap_service_error(payload: ResponsePayload) -> Result<ResponsePayload, ClientError> {
        match payload {
            ResponsePayload::Error { code, message } => Err(match code {
                ERROR_CODE_UNAUTHORIZED | ERROR_CODE_ADMIN_ONLY_QUERY => {
                    ClientError::Unauthorized { message }
                }
                ERROR_CODE_PROTOCOL_VERSION_MISMATCH | ERROR_CODE_HANDSHAKE_REQUIRED => {
                    ClientError::HandshakeRejected { message }
                }
                other => ClientError::ServerProtocolError {
                    code: other,
                    message,
                },
            }),
            other => Ok(other),
        }
    }

    /// Sends a query request and retrieves the first page of results.
    ///
    /// `defaults` carries the UI match toggles and the hidden/system filter as query-wide
    /// defaults; they are not part of `raw_query`.
    pub async fn query(
        &mut self,
        query_id: u64,
        raw_query: &str,
        defaults: QueryDefaults,
        sort_by: &str,
        sort_asc: bool,
        first_page_limit: usize,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::Query {
            query_id,
            raw_query: raw_query.to_string(),
            defaults,
            sort_by_name: sort_by.to_string(),
            sort_asc,
            first_page_limit,
        })
        .await
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

    /// Cancels an in-flight query.
    pub async fn cancel(&mut self, query_id: u64) -> Result<(), ClientError> {
        let frame = encode_frame(&ClientRequest::Cancel { query_id })?;
        self.pipe.write_all(&frame).await?;
        self.pipe.flush().await?;
        let _ = self.read_response().await?;
        Ok(())
    }

    /// Requests rescanning of volumes.
    pub async fn rescan(
        &mut self,
        volume_id: Option<u16>,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::Rescan { volume_id })
            .await
    }

    /// Reads the current state of every volume the service indexes. The state rides in the
    /// envelope's volume list; the payload only acknowledges the read.
    pub async fn get_status(&mut self) -> Result<Envelope<ResponsePayload>, ClientError> {
        let mut envelope = self.send_request(&ClientRequest::GetStatus).await?;
        envelope.payload = Self::unwrap_service_error(envelope.payload)?;
        Ok(envelope)
    }

    /// Reads the directory tree below `path` from the service's volume index.
    /// Empty entries mean the path is not indexed.
    pub async fn get_tree(
        &mut self,
        path: &str,
        max_depth: usize,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::GetTree {
            path: path.to_string(),
            max_depth,
        })
        .await
    }

    /// Requests reloading configuration from %ProgramData%\Cairn\service.toml (SVC-05).
    pub async fn reload_config(&mut self) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::ReloadConfig).await
    }

    /// Reads the content-index lifecycle, including backfill progress and the last error.
    pub async fn get_content_index_status(
        &mut self,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::GetContentIndexStatus)
            .await
    }

    /// Stops a running backfill and turns the content-index master switch off.
    pub async fn cancel_content_index(&mut self) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::CancelContentIndex).await
    }

    /// Clears the entire full-text content index (CNT-07).
    pub async fn clear_content_index(&mut self) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.send_request(&ClientRequest::ClearContentIndex).await
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

    async fn send_request(
        &mut self,
        req: &ClientRequest,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        let frame = encode_frame(req)?;
        self.pipe.write_all(&frame).await?;
        self.pipe.flush().await?;

        self.read_response().await
    }

    async fn read_response(&mut self) -> Result<Envelope<ResponsePayload>, ClientError> {
        tokio::time::timeout(Self::DEFAULT_READ_TIMEOUT, self.read_response_inner())
            .await
            .map_err(|_| ClientError::Timeout)?
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
                    if self.read_buf.is_empty() && self.read_buf.capacity() > 65536 {
                        self.read_buf.shrink_to(65536);
                    }
                    return Ok(envelope);
                }
                Ok(None) => {}
                // A frame whose header declares another version is a protocol mismatch, not a
                // transport failure: say so, so the GUI can show an update hint (SVC-03).
                Err(CodecError::IncompatibleVersion { received }) => {
                    return Err(ClientError::IncompatibleProtocolVersion {
                        server_version: received as u32,
                        supported_version: PROTOCOL_VERSION as u32,
                    })
                }
                Err(other) => return Err(ClientError::Codec(other)),
            }

            let n = self.pipe.read(&mut temp_buf).await?;
            if n == 0 {
                return Err(ClientError::ConnectionClosed);
            }
            self.read_buf.extend_from_slice(&temp_buf[..n]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

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
            server_version: 2,
            supported_version: PROTOCOL_VERSION as u32,
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
    async fn query_sends_defaults_as_a_separate_field() {
        let (mut server, mut client) = stub_pipe_pair(r"\\.\pipe\cairn.client.stub.query").await;
        let defaults = QueryDefaults {
            case_sensitive: true,
            match_path: true,
            exclude_hidden: true,
            ..Default::default()
        };
        let raw = "报告 季度";
        let task =
            tokio::spawn(async move { client.query(4, raw, defaults, "name", true, 10).await });

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
                sort_by_name,
                first_page_limit,
                ..
            } => {
                assert_eq!(raw_query, raw, "the text must reach the service as typed");
                assert!(
                    !raw_query.contains("case:") && !raw_query.contains("path:"),
                    "toggles must not be spliced into the text"
                );
                assert_eq!(received, defaults);
                assert_eq!(sort_by_name, "name");
                assert_eq!(first_page_limit, 10);
            }
            other => panic!("Expected a Query request, got {other:?}"),
        }

        let frame = encode_frame(&Envelope {
            volumes: Vec::new(),
            payload: ResponsePayload::QueryResult {
                query_id: 4,
                session_id: 1,
                total_matches: 0,
                total_bytes: 0,
                first_page: Vec::new(),
            },
        })
        .expect("encode stub answer");
        server.write_all(&frame).await.expect("stub write");
        server.flush().await.expect("stub flush");
        task.await
            .expect("query task")
            .expect("query must succeed over the stub");
    }

    #[tokio::test]
    #[cfg(windows)]
    async fn hello_reports_a_service_of_another_version_as_incompatible() {
        let (mut server, mut client) = stub_pipe_pair(r"\\.\pipe\cairn.client.stub.version").await;
        let reply = ResponsePayload::HelloAck {
            server_version: 999,
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
                server_version,
                supported_version,
            } => {
                assert_eq!(server_version, 999);
                assert_eq!(supported_version, PROTOCOL_VERSION as u32);
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
            server_version: PROTOCOL_VERSION as u32,
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

    #[tokio::test]
    #[cfg(windows)]
    async fn cancel_reads_response_and_keeps_pipe_in_sync() {
        let (mut server, mut client) =
            stub_pipe_pair(r"\\.\pipe\cairn.client.stub.cancel_sync").await;
        let task = tokio::spawn(async move {
            client.cancel(42).await.expect("cancel must succeed");
            client
        });

        server.connect().await.expect("stub connect");
        let mut buf = [0u8; 4096];
        let read = server.read(&mut buf).await.expect("stub read");
        let (_, request) = try_decode_frame::<ClientRequest>(&buf[..read])
            .expect("stub decode")
            .expect("complete frame");
        assert!(matches!(request, ClientRequest::Cancel { query_id: 42 }));

        let frame = encode_frame(&Envelope {
            volumes: Vec::new(),
            payload: ResponsePayload::CancelAck,
        })
        .expect("encode stub answer");
        server.write_all(&frame).await.expect("stub write");
        server.flush().await.expect("stub flush");

        let client = task.await.expect("task must succeed");
        // Verify pipe buffer on client is clean and ready for subsequent request
        assert!(client.read_buf.is_empty());
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
}
