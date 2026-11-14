// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Long-lived Named Pipe session management with transparent reconnect and independent cancellation.

use crate::client::{CairnClient, ClientError};
use cairn_protocol::message::{Envelope, QueryDefaults, ResponsePayload, SortColumn, PIPE_NAME};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

#[derive(Debug)]
struct SessionInner {
    pipe_name: String,
    read_timeout: Duration,
    primary: Mutex<Option<CairnClient>>,
    control: Mutex<Option<CairnClient>>,
    cancel: Mutex<Option<CairnClient>>,
}

/// Persistent IPC session over Named Pipes.
///
/// Reuses a handshaken primary connection for queries and pages, a separate control
/// connection for status, tree and configuration requests, and a third for `Cancel`, so a long
/// query never stalls a Treemap or settings request and cancellation never stalls behind either.
#[derive(Clone, Debug)]
pub struct ServiceSession {
    inner: Arc<SessionInner>,
}

impl Default for ServiceSession {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceSession {
    /// Creates a session targeting the installed Cairn service pipe.
    pub fn new() -> Self {
        Self::for_pipe(PIPE_NAME)
    }

    /// Creates a session targeting an explicit Named Pipe endpoint.
    pub fn for_pipe(pipe_name: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(SessionInner {
                pipe_name: pipe_name.into(),
                read_timeout: CairnClient::DEFAULT_READ_TIMEOUT,
                primary: Mutex::new(None),
                control: Mutex::new(None),
                cancel: Mutex::new(None),
            }),
        }
    }

    /// Overrides the per-response read timeout used on new connections from this session.
    pub fn with_read_timeout(self, read_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(SessionInner {
                pipe_name: self.inner.pipe_name.clone(),
                read_timeout,
                primary: Mutex::new(None),
                control: Mutex::new(None),
                cancel: Mutex::new(None),
            }),
        }
    }

    pub async fn get_status(&self) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.call_control(|client| Box::pin(client.get_status()))
            .await
    }

    pub async fn query(
        &self,
        query_id: u64,
        raw_query: &str,
        defaults: QueryDefaults,
        sort_column: SortColumn,
        sort_asc: bool,
        first_page_limit: usize,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        let raw_query = raw_query.to_string();
        self.call_primary(|client| {
            let raw_query = raw_query.clone();
            Box::pin(async move {
                client
                    .query(
                        query_id,
                        &raw_query,
                        defaults,
                        sort_column,
                        sort_asc,
                        first_page_limit,
                    )
                    .await
            })
        })
        .await
    }

    pub async fn get_rows(
        &self,
        session_id: u64,
        offset: usize,
        limit: usize,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.call_primary(|client| Box::pin(client.get_rows(session_id, offset, limit)))
            .await
    }

    pub async fn get_tree(
        &self,
        path: &str,
        max_depth: usize,
    ) -> Result<Envelope<ResponsePayload>, ClientError> {
        let path = path.to_string();
        self.call_control(|client| {
            let path = path.clone();
            Box::pin(async move { client.get_tree(&path, max_depth).await })
        })
        .await
    }

    pub async fn get_content_index_status(&self) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.call_control(|client| Box::pin(client.get_content_index_status()))
            .await
    }

    pub async fn clear_content_index(&self) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.call_control(|client| Box::pin(client.clear_content_index()))
            .await
    }

    pub async fn get_config(&self) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.call_control(|client| Box::pin(client.get_config()))
            .await
    }

    pub async fn reload_service_config(&self) -> Result<(bool, String), ClientError> {
        self.call_control(|client| Box::pin(client.reload_service_config()))
            .await
    }

    pub async fn submit_service_config(
        &self,
        config_toml: &str,
    ) -> Result<(bool, String), ClientError> {
        let config_toml = config_toml.to_string();
        self.call_control(|client| {
            let config_toml = config_toml.clone();
            Box::pin(async move { client.submit_service_config(&config_toml).await })
        })
        .await
    }

    /// Sends query cancellation on a connection independent of the primary slot.
    pub async fn cancel(&self, query_id: u64) -> Result<Envelope<ResponsePayload>, ClientError> {
        self.call_slot(&self.inner.cancel, |client| {
            Box::pin(client.cancel(query_id))
        })
        .await
    }

    /// Drops every cached connection.
    pub async fn disconnect(&self) {
        *self.inner.primary.lock().await = None;
        *self.inner.control.lock().await = None;
        *self.inner.cancel.lock().await = None;
    }

    async fn call_primary<T, F>(&self, op: F) -> Result<T, ClientError>
    where
        F: for<'a> FnMut(
            &'a mut CairnClient,
        ) -> std::pin::Pin<
            Box<dyn Future<Output = Result<T, ClientError>> + Send + 'a>,
        >,
    {
        self.call_slot(&self.inner.primary, op).await
    }

    async fn call_control<T, F>(&self, op: F) -> Result<T, ClientError>
    where
        F: for<'a> FnMut(
            &'a mut CairnClient,
        ) -> std::pin::Pin<
            Box<dyn Future<Output = Result<T, ClientError>> + Send + 'a>,
        >,
    {
        self.call_slot(&self.inner.control, op).await
    }

    async fn call_slot<T, F>(
        &self,
        slot: &Mutex<Option<CairnClient>>,
        mut op: F,
    ) -> Result<T, ClientError>
    where
        F: for<'a> FnMut(
            &'a mut CairnClient,
        ) -> std::pin::Pin<
            Box<dyn Future<Output = Result<T, ClientError>> + Send + 'a>,
        >,
    {
        let mut guard = slot.lock().await;
        let mut used_cached = guard.is_some();

        loop {
            if guard.is_none() {
                let mut client = CairnClient::connect_to(&self.inner.pipe_name).await?;
                client.set_read_timeout(self.inner.read_timeout);
                *guard = Some(client);
                used_cached = false;
            }
            let client = guard.as_mut().expect("client just connected");
            match op(client).await {
                Ok(value) => return Ok(value),
                Err(err) => {
                    let can_retry = used_cached && should_retry_cached(&err);
                    if is_connection_poisoned(&err) {
                        *guard = None;
                    }
                    if can_retry {
                        used_cached = false;
                        continue;
                    }
                    return Err(err);
                }
            }
        }
    }
}

fn is_connection_poisoned(err: &ClientError) -> bool {
    matches!(
        err,
        ClientError::ConnectFailed(_)
            | ClientError::ConnectionClosed
            | ClientError::Timeout
            | ClientError::Codec(_)
            | ClientError::IncompatibleProtocolVersion { .. }
            | ClientError::HandshakeRejected { .. }
    )
}

fn should_retry_cached(err: &ClientError) -> bool {
    matches!(
        err,
        ClientError::ConnectFailed(_) | ClientError::ConnectionClosed
    )
}
