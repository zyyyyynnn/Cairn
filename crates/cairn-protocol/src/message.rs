// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Named Pipe IPC protocol messages and row formats.

use crate::status::{
    ContentIndexStatus, IndexJournalEntry, IndexJournalStatus, PropertyIndexStatus,
    SourceStatusSummary, VolumeStatusSummary,
};
pub use cairn_core::property_store::{PropertyKind, RecordPropertyData, SourceId};
pub use cairn_core::query::QueryDegradedReason;
pub use cairn_core::sort::{SortColumn, SortDirection, SortKey, SortSpec};
pub use cairn_core::syntax::QueryDefaults;
use serde::{Deserialize, Serialize};

/// Product version from Cargo workspace metadata.
pub const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Wire guard exchanged at `Hello` handshake and carried in every frame header.
///
/// Encodes `major.minor.patch` so any protocol-bearing patch bump (`0.2.1`) rejects older
/// builds (`0.2.0`) before `postcard` deserialization (SPEC SVC-03).
pub const WIRE_GUARD: u16 = pack_wire_guard(PRODUCT_VERSION);

const fn pack_wire_guard(version: &str) -> u16 {
    let bytes = version.as_bytes();
    let mut parts = [0u32; 3];
    let mut part = 0usize;
    let mut index = 0usize;
    while index < bytes.len() && part < 3 {
        let b = bytes[index];
        if b == b'.' {
            part += 1;
        } else if b >= b'0' && b <= b'9' {
            parts[part] = parts[part] * 10 + (b - b'0') as u32;
        } else {
            break;
        }
        index += 1;
    }
    (((parts[0] & 0x0F) << 12) | ((parts[1] & 0x3F) << 6) | (parts[2] & 0x3F)) as u16
}

pub const PIPE_NAME: &str = r"\\.\pipe\Cairn.Service";

/// The installed service's SCM name. The installer registers it, the service hosts it, and the
/// app starts it by this name.
pub const SERVICE_NAME: &str = "CairnService";

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
/// The query carries more content terms than one evaluation may resolve
/// (`cairn_core::syntax::MAX_CONTENT_TERMS`); the request is refused before evaluation.
pub const ERROR_CODE_QUERY_TOO_COMPLEX: u32 = 416;
/// The request parameters are invalid or refer to an unknown source.
pub const ERROR_CODE_INVALID_REQUEST: u32 = 417;

/// Upper bound on multi-key sort clauses accepted from a client.
pub const MAX_SORT_KEYS: usize = 8;

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
        sort_spec: SortSpec,
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
    /// Returns every indexed source (NTFS volumes and folder sources).
    GetSources,
    /// Registers and starts indexing a folder source.
    AddFolderSource {
        path: String,
    },
    /// Stops monitoring and removes a folder source from the index.
    RemoveFolderSource {
        path: String,
    },
    /// Triggers a rescan for a single indexed source.
    RescanSource {
        source_id: SourceId,
    },
    /// Reads the current state of the optional extended-property index.
    GetPropertyIndexStatus,
    /// Enables or disables optional extended-property indexing and persists the setting.
    SetPropertyIndexEnabled {
        enabled: bool,
    },
    /// Queries bounded read-only index journal entries after `after_seq`.
    GetIndexJournal {
        after_seq: u64,
        limit: usize,
    },
    /// Enables or disables index journal recording.
    SetIndexJournalEnabled {
        enabled: bool,
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
        /// Space the matched set describes, counted once at its highest hit point: a matched
        /// directory contributes its recursive size, and matches under a matched ancestor add
        /// nothing.
        total_bytes: u64,
        first_page: Vec<FileRowData>,
        applied_sort: SortSpec,
        degraded: Option<QueryDegradedReason>,
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
    /// A retired directory-tree response. The empty payload keeps `Error` at its published
    /// slot so a peer of another protocol version can still read the rejection.
    RetiredTree,
    Error {
        code: u32,
        message: String,
    },
    /// The acknowledgement of a cancelled query.
    CancelAck,
    SourcesResult {
        sources: Vec<SourceStatusSummary>,
    },
    RescanSourceAck {
        source_id: SourceId,
        success: bool,
    },
    PropertyIndexStatus {
        status: PropertyIndexStatus,
    },
    IndexJournalResult {
        status: IndexJournalStatus,
        entries: Vec<IndexJournalEntry>,
    },
    IndexJournalStatus {
        status: IndexJournalStatus,
    },
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
    pub snippets: Vec<String>,
    pub properties: Option<RecordPropertyData>,
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
    const PUBLISHED_RESPONSE_SLOTS: [(&str, u8); 19] = [
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
        ("retired_tree", 11),
        ("error", 12),
        ("cancel_ack", 13),
        ("sources_result", 14),
        ("rescan_source_ack", 15),
        ("property_index_status", 16),
        ("index_journal_result", 17),
        ("index_journal_status", 18),
    ];

    /// The slot each published request occupies on the wire. A request is only decoded by a peer
    /// of the same protocol version, so the rule is the same: a renamed request keeps its slot and
    /// a new one is appended.
    const PUBLISHED_REQUEST_SLOTS: [(&str, u8); 20] = [
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
        ("get_sources", 12),
        ("add_folder_source", 13),
        ("remove_folder_source", 14),
        ("rescan_source", 15),
        ("get_property_index_status", 16),
        ("set_property_index_enabled", 17),
        ("get_index_journal", 18),
        ("set_index_journal_enabled", 19),
    ];

    #[test]
    fn the_version_rejection_stays_at_its_published_wire_slot() {
        let encoded = postcard::to_allocvec(&ResponsePayload::Error {
            code: ERROR_CODE_PROTOCOL_VERSION_MISMATCH,
            message: "update both sides".to_string(),
        })
        .expect("encode rejection");
        assert_eq!(encoded[0], 12, "Error must stay at response slot 12");
    }

    /// One payload per published slot, in the published order.
    fn published_responses() -> Vec<(&'static str, ResponsePayload)> {
        vec![
            (
                "hello_ack",
                ResponsePayload::HelloAck {
                    server_version: WIRE_GUARD as u32,
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
                    applied_sort: SortSpec::default(),
                    degraded: None,
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
            ("retired_tree", ResponsePayload::RetiredTree),
            (
                "error",
                ResponsePayload::Error {
                    code: ERROR_CODE_PROTOCOL_VERSION_MISMATCH,
                    message: String::new(),
                },
            ),
            ("cancel_ack", ResponsePayload::CancelAck),
            (
                "sources_result",
                ResponsePayload::SourcesResult {
                    sources: Vec::new(),
                },
            ),
            (
                "rescan_source_ack",
                ResponsePayload::RescanSourceAck {
                    source_id: SourceId(0),
                    success: true,
                },
            ),
            (
                "property_index_status",
                ResponsePayload::PropertyIndexStatus {
                    status: PropertyIndexStatus::default(),
                },
            ),
            (
                "index_journal_result",
                ResponsePayload::IndexJournalResult {
                    status: IndexJournalStatus::default(),
                    entries: Vec::new(),
                },
            ),
            (
                "index_journal_status",
                ResponsePayload::IndexJournalStatus {
                    status: IndexJournalStatus::default(),
                },
            ),
        ]
    }

    /// One request per published slot, in the published order.
    fn published_requests() -> Vec<(&'static str, ClientRequest)> {
        vec![
            (
                "hello",
                ClientRequest::Hello {
                    client_version: WIRE_GUARD as u32,
                    pid: 1,
                },
            ),
            (
                "query",
                ClientRequest::Query {
                    query_id: 1,
                    raw_query: String::new(),
                    defaults: QueryDefaults::default(),
                    sort_spec: SortSpec::single(SortColumn::Name, SortDirection::Ascending),
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
            ("get_sources", ClientRequest::GetSources),
            (
                "add_folder_source",
                ClientRequest::AddFolderSource {
                    path: String::new(),
                },
            ),
            (
                "remove_folder_source",
                ClientRequest::RemoveFolderSource {
                    path: String::new(),
                },
            ),
            (
                "rescan_source",
                ClientRequest::RescanSource {
                    source_id: SourceId(0),
                },
            ),
            (
                "get_property_index_status",
                ClientRequest::GetPropertyIndexStatus,
            ),
            (
                "set_property_index_enabled",
                ClientRequest::SetPropertyIndexEnabled { enabled: false },
            ),
            (
                "get_index_journal",
                ClientRequest::GetIndexJournal {
                    after_seq: 0,
                    limit: 0,
                },
            ),
            (
                "set_index_journal_enabled",
                ClientRequest::SetIndexJournalEnabled { enabled: false },
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

    /// Deriving the guard from `major.minor.patch` ensures any protocol-bearing patch bump (`0.2.1`)
    /// rejects an older peer (`0.2.0`) at the handshake instead of misreading shifted fields.
    #[test]
    fn the_wire_guard_tracks_full_product_version() {
        assert_eq!(pack_wire_guard("0.1.0"), 0x0040);
        assert_eq!(pack_wire_guard("0.2.0"), 0x0080);
        assert_eq!(pack_wire_guard("0.2.1"), 0x0081);
        assert_ne!(pack_wire_guard("0.2.1"), 0x0002);
        assert_eq!(pack_wire_guard("1.2.3"), 0x1083);
        assert_eq!(pack_wire_guard("0.10.5"), 0x0285);
        assert_eq!(WIRE_GUARD, pack_wire_guard(PRODUCT_VERSION));
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
