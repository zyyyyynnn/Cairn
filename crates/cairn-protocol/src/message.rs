// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Named Pipe IPC protocol messages and row formats.

use crate::status::{ContentIndexStatus, VolumeStatusSummary};
pub use cairn_core::syntax::QueryDefaults;
use serde::{Deserialize, Serialize};

/// The version both ends negotiate before any request is accepted. postcard is not
/// self-describing, so an older frame would decode into shifted fields rather than fail, and the
/// handshake gate is the only thing that can catch that. Any request/response or row-layout change
/// requires a coordinated client+service bump.
pub const PROTOCOL_VERSION: u16 = 6;
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
    /// Reads the current state of every volume the service indexes. The reply carries the state
    /// in the envelope's volume list; the payload only acknowledges that the read happened.
    GetStatus,
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

/// Responses accepted from the pipe.
///
/// postcard writes a variant as its position in the enum, so those positions are the published
/// wire contract rather than an implementation detail. The service answers a peer that speaks
/// another protocol version with the rejection, using its own payload layout; that peer has to be
/// able to read it, or a service that needs updating is indistinguishable from a broken one
/// (SVC-03). A new payload is therefore appended at the end and never inserted in the middle.
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
        /// Sum of the matched files' own sizes. Directories are containers and do not add their
        /// recursive size here, so the all-files total matches the disk usage without double
        /// counting a folder and its contents.
        total_bytes: u64,
        first_page: Vec<FileRowData>,
    },
    RowsResult {
        session_id: u64,
        offset: usize,
        rows: Vec<FileRowData>,
    },
    /// The acknowledgement of a status read. The state itself rides in the envelope's volume
    /// list, so the payload carries nothing else.
    StatusAck,
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
    /// The acknowledgement of a cancelled query. A cancel says nothing about the worker's
    /// state, so it answers with its own acknowledgement.
    ///
    /// It sits at the end of the enum on purpose. postcard writes a variant as its position, so a
    /// payload inserted in the middle would move every slot behind it — including the rejection a
    /// peer of another protocol version has to read (SVC-03).
    CancelAck,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{encode_frame, HEADER_SIZE};

    /// The slot each published payload occupies on the wire.
    ///
    /// postcard writes a variant as its position in the enum, so a slot is part of the published
    /// contract rather than an implementation detail. The service answers a peer that speaks
    /// another protocol version with the rejection, and that peer has to be able to read it, or a
    /// service that needs updating looks like a broken one (SVC-03). A payload inserted in the
    /// middle of the enum would move every slot behind it, so a new payload is appended instead.
    const PUBLISHED_RESPONSE_SLOTS: [(&str, u8); 14] = [
        ("hello_ack", 0),
        ("query_result", 1),
        ("rows_result", 2),
        ("status_ack", 3),
        ("rescan_ack", 4),
        ("config_result", 5),
        ("set_config_ack", 6),
        ("reload_config_ack", 7),
        ("content_index_status", 8),
        ("cancel_content_index_ack", 9),
        ("clear_content_index_ack", 10),
        ("tree_result", 11),
        ("error", 12),
        ("cancel_ack", 13),
    ];

    /// The slot each published request occupies on the wire. A request is only decoded by a peer
    /// of the same protocol version, so the rule is the same: a renamed request keeps its slot and
    /// a new one is appended.
    const PUBLISHED_REQUEST_SLOTS: [(&str, u8); 13] = [
        ("hello", 0),
        ("query", 1),
        ("cancel", 2),
        ("get_rows", 3),
        ("get_status", 4),
        ("rescan", 5),
        ("get_config", 6),
        ("set_config", 7),
        ("reload_config", 8),
        ("get_content_index_status", 9),
        ("cancel_content_index", 10),
        ("clear_content_index", 11),
        ("get_tree", 12),
    ];

    /// One payload per published slot, in the published order.
    fn published_responses() -> Vec<(&'static str, ResponsePayload)> {
        vec![
            (
                "hello_ack",
                ResponsePayload::HelloAck {
                    server_version: PROTOCOL_VERSION as u32,
                    caller_is_admin: false,
                },
            ),
            (
                "query_result",
                ResponsePayload::QueryResult {
                    query_id: 1,
                    session_id: 2,
                    total_matches: 0,
                    total_bytes: 0,
                    first_page: Vec::new(),
                },
            ),
            (
                "rows_result",
                ResponsePayload::RowsResult {
                    session_id: 1,
                    offset: 0,
                    rows: Vec::new(),
                },
            ),
            ("status_ack", ResponsePayload::StatusAck),
            (
                "rescan_ack",
                ResponsePayload::RescanAck {
                    volume_id: None,
                    success: true,
                },
            ),
            (
                "config_result",
                ResponsePayload::ConfigResult {
                    config_toml: String::new(),
                },
            ),
            (
                "set_config_ack",
                ResponsePayload::SetConfigAck {
                    success: true,
                    message: String::new(),
                },
            ),
            (
                "reload_config_ack",
                ResponsePayload::ReloadConfigAck {
                    success: true,
                    message: String::new(),
                },
            ),
            (
                "content_index_status",
                ResponsePayload::ContentIndexStatus {
                    status: ContentIndexStatus::default(),
                },
            ),
            (
                "cancel_content_index_ack",
                ResponsePayload::CancelContentIndexAck { success: true },
            ),
            (
                "clear_content_index_ack",
                ResponsePayload::ClearContentIndexAck { success: true },
            ),
            (
                "tree_result",
                ResponsePayload::TreeResult {
                    path: String::new(),
                    entries: Vec::new(),
                },
            ),
            (
                "error",
                ResponsePayload::Error {
                    code: ERROR_CODE_PROTOCOL_VERSION_MISMATCH,
                    message: String::new(),
                },
            ),
            ("cancel_ack", ResponsePayload::CancelAck),
        ]
    }

    /// One request per published slot, in the published order.
    fn published_requests() -> Vec<(&'static str, ClientRequest)> {
        vec![
            (
                "hello",
                ClientRequest::Hello {
                    client_version: PROTOCOL_VERSION as u32,
                    pid: 1,
                },
            ),
            (
                "query",
                ClientRequest::Query {
                    query_id: 1,
                    raw_query: String::new(),
                    defaults: QueryDefaults::default(),
                    sort_by_name: String::new(),
                    sort_asc: true,
                    first_page_limit: 0,
                },
            ),
            ("cancel", ClientRequest::Cancel { query_id: 1 }),
            (
                "get_rows",
                ClientRequest::GetRows {
                    session_id: 1,
                    offset: 0,
                    limit: 0,
                },
            ),
            ("get_status", ClientRequest::GetStatus),
            ("rescan", ClientRequest::Rescan { volume_id: None }),
            ("get_config", ClientRequest::GetConfig),
            (
                "set_config",
                ClientRequest::SetConfig {
                    config_toml: String::new(),
                },
            ),
            ("reload_config", ClientRequest::ReloadConfig),
            (
                "get_content_index_status",
                ClientRequest::GetContentIndexStatus,
            ),
            ("cancel_content_index", ClientRequest::CancelContentIndex),
            ("clear_content_index", ClientRequest::ClearContentIndex),
            (
                "get_tree",
                ClientRequest::GetTree {
                    path: String::new(),
                    max_depth: 0,
                },
            ),
        ]
    }

    /// The byte postcard writes for a payload is its slot: the enum variant index, as a varint.
    /// Every published slot is below the varint's two-byte threshold, so it is one byte.
    fn wire_slot<T: serde::Serialize>(payload: &T) -> u8 {
        let frame = encode_frame(payload).expect("encode");
        frame[HEADER_SIZE]
    }

    #[test]
    fn every_published_response_keeps_the_wire_slot_it_was_published_with() {
        let responses = published_responses();
        assert_eq!(
            responses.len(),
            PUBLISHED_RESPONSE_SLOTS.len(),
            "every published slot needs a payload that occupies it"
        );
        for ((name, payload), (slot_name, slot)) in
            responses.iter().zip(PUBLISHED_RESPONSE_SLOTS.iter())
        {
            assert_eq!(
                name, slot_name,
                "the published order must not be reshuffled"
            );
            assert_eq!(
                wire_slot(payload),
                *slot,
                "{name} moved off its published wire slot"
            );
        }
    }

    #[test]
    fn every_published_request_keeps_the_wire_slot_it_was_published_with() {
        let requests = published_requests();
        assert_eq!(
            requests.len(),
            PUBLISHED_REQUEST_SLOTS.len(),
            "every published slot needs a request that occupies it"
        );
        for ((name, request), (slot_name, slot)) in
            requests.iter().zip(PUBLISHED_REQUEST_SLOTS.iter())
        {
            assert_eq!(
                name, slot_name,
                "the published order must not be reshuffled"
            );
            assert_eq!(
                wire_slot(request),
                *slot,
                "{name} moved off its published wire slot"
            );
        }
    }

    /// The rejection is the one payload a peer of another protocol version must be able to read:
    /// the service answers a version mismatch with it, using the payload layout of its own
    /// version. It therefore has to stay where it was published.
    #[test]
    fn the_rejection_stays_readable_for_a_peer_of_another_protocol_version() {
        let rejection = ResponsePayload::Error {
            code: ERROR_CODE_PROTOCOL_VERSION_MISMATCH,
            message: "Incompatible protocol version".to_string(),
        };
        let frame = encode_frame(&rejection).expect("encode");
        assert_eq!(
            frame[HEADER_SIZE],
            PUBLISHED_RESPONSE_SLOTS
                .iter()
                .find(|(name, _)| *name == "error")
                .expect("the rejection has a published slot")
                .1,
            "the rejection moved off its published wire slot"
        );
    }
}
