// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Named Pipe IPC protocol messages and row formats.

use crate::status::{ContentIndexStatus, VolumeStatusSummary, WorkerStatus};
pub use cairn_core::syntax::QueryDefaults;
use serde::{Deserialize, Serialize};

/// Bumped to 3 because `ClientRequest::SetConfig` dropped its client-supplied `admin_token`
/// (authority comes from the OS caller token, never from the frame). postcard is not
/// self-describing, so an older frame would decode into shifted fields rather than fail, and the
/// handshake gate is the only thing that can catch that.
pub const PROTOCOL_VERSION: u16 = 4;
pub const PIPE_NAME: &str = r"\\.\pipe\Cairn.Service";
/// The pipe of a user-privilege indexer started by the desktop app when the installed LocalSystem
/// service is not available. It is a separate name so the two indexers never compete for the same
/// pipe, and so a running service is always preferred over a local scan.
pub const USER_PIPE_NAME: &str = r"\\.\pipe\Cairn.User";

/// Machine-readable `ResponsePayload::Error` codes. Text stays in English and technical:
/// localizing it is the presentation layer's responsibility (SPEC UI-07).
pub const ERROR_CODE_PROTOCOL_VERSION_MISMATCH: u32 = 409;
/// The caller identity could not be derived from the OS access token, or the request is refused by
/// policy (an unauthenticated caller, or a non-administrator changing `admin_only_query`).
pub const ERROR_CODE_UNAUTHORIZED: u32 = 403;
/// `admin_only_query` is enabled and the caller is not an administrator (SPEC SVC-02, SVC-05).
pub const ERROR_CODE_ADMIN_ONLY_QUERY: u32 = 401;
/// A request arrived before the `Hello` negotiation, so no version has been agreed on yet (SVC-03).
pub const ERROR_CODE_HANDSHAKE_REQUIRED: u32 = 400;
/// The requested query session does not exist or has expired (e.g. invalidation, reload, or compaction).
pub const ERROR_CODE_SESSION_EXPIRED: u32 = 410;

/// Requests accepted from a pipe client.
///
/// `pid` and `client_version` are self-reported hints only; the service derives the real caller
/// identity from the pipe client token and enforces authorization server-side (SPEC SVC-02).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientRequest {
    Hello {
        client_version: u32,
        pid: u32,
    },
    /// The query text plus the UI match toggles as structured defaults. The toggles are not
    /// spliced into `raw_query`: they are query-wide, so `parse_query_with` applies them to
    /// every search item and an item that writes its own modifier still wins.
    Query {
        query_id: u64,
        raw_query: String,
        defaults: QueryDefaults,
        sort_by_name: String,
        sort_asc: bool,
        first_page_limit: usize,
    },
    Cancel {
        query_id: u64,
    },
    GetRows {
        session_id: u64,
        offset: usize,
        limit: usize,
    },
    SubscribeStatus,
    Rescan {
        volume_id: Option<u16>,
    },
    GetConfig,
    SetConfig {
        config_toml: String,
    },
    ReloadConfig,
    GetContentIndexStatus,
    CancelContentIndex,
    ClearContentIndex,
    /// The directory tree below `path` (drive root when it names a drive),
    /// aggregated and sorted, from the service's volume index. Empty entries
    /// mean the path is not indexed; the caller then falls back to its own
    /// scan.
    GetTree {
        path: String,
        max_depth: usize,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub volumes: Vec<VolumeStatusSummary>,
    pub payload: T,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ResponsePayload {
    HelloAck {
        server_version: u32,
        /// Whether the *caller* is an administrator or SYSTEM, as derived by the service from the
        /// pipe client's access token. It is not a claim the client can make (SPEC SVC-05).
        caller_is_admin: bool,
    },
    QueryResult {
        query_id: u64,
        session_id: u64,
        total_matches: usize,
        first_page: Vec<FileRowData>,
    },
    RowsResult {
        session_id: u64,
        offset: usize,
        rows: Vec<FileRowData>,
    },
    StatusUpdate {
        worker_status: WorkerStatus,
    },
    RescanAck {
        volume_id: Option<u16>,
        success: bool,
    },
    ConfigResult {
        config_toml: String,
    },
    SetConfigAck {
        success: bool,
        message: String,
    },
    ReloadConfigAck {
        success: bool,
        message: String,
    },
    ContentIndexStatus {
        status: ContentIndexStatus,
    },
    CancelContentIndexAck {
        success: bool,
    },
    ClearContentIndexAck {
        success: bool,
    },
    TreeResult {
        path: String,
        /// Pre-order entries, parent before children; `depth` 0 is the root.
        /// Empty when the service does not index `path`.
        entries: Vec<TreeEntryData>,
    },
    Error {
        code: u32,
        message: String,
    },
}

/// One node of a [`ResponsePayload::TreeResult`]. A depth-capped leaf (a
/// directory at the requested depth) still carries its exact recursive size
/// and item count; nodes with children carry sizes already aggregated.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TreeEntryData {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub modified_ts: i64,
    pub descendant_items: usize,
    /// Nesting level below the requested root: the root itself is 0, its
    /// children 1, and so on.
    pub depth: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct FileRowData {
    pub frn: u64,
    pub name: String,
    pub path: String,
    pub size: u64,
    pub modified_ts: i64,
    pub created_ts: i64,
    pub is_dir: bool,
    pub flags: u16,
    pub name_highlights: Vec<(usize, usize)>,
    pub path_highlights: Vec<(usize, usize)>,
    pub snippet: Option<String>,
}
