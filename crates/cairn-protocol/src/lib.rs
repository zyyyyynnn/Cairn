// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]

//! `cairn-protocol`: IPC protocol definitions, length-prefixed framing codec,
//! and volume status models for Cairn Named Pipe communication.

pub mod codec;
pub mod message;
pub mod status;
pub mod worker_exit;

pub use codec::{
    CodecError, FrameHeader, HEADER_SIZE, MAX_FRAME_PAYLOAD, encode_frame,
    encode_frame_with_version, peek_frame_header, try_decode_frame,
};
pub use message::{
    ClientRequest, ERROR_CODE_ADMIN_ONLY_QUERY, ERROR_CODE_HANDSHAKE_REQUIRED,
    ERROR_CODE_INVALID_REQUEST, ERROR_CODE_PROTOCOL_VERSION_MISMATCH, ERROR_CODE_SESSION_EXPIRED,
    ERROR_CODE_UNAUTHORIZED, Envelope, FileRowData, PIPE_NAME, PROTOCOL_VERSION, PropertyKind,
    QueryDefaults, QueryDegradedReason, RecordPropertyData, ResponsePayload, SortColumn,
    SortDirection, SortKey, SortSpec, SourceId,
};
pub use status::{
    ContentIndexState, ContentIndexStatus, ContentIndexTask, IndexJournalEntry, IndexJournalStatus,
    JournalChangeKind, PropertyExtractResult, PropertyExtractTask, PropertyIndexStatus, SourceKind,
    SourceStatusSummary, VolumeState, VolumeStatusSummary,
};
