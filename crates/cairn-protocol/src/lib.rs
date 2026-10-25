// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]

//! `cairn-protocol`: IPC protocol definitions, length-prefixed framing codec,
//! and volume status models for Cairn Named Pipe communication.

pub mod codec;
pub mod message;
pub mod status;

pub use codec::{
    decode_frame, encode_frame, encode_frame_with_version, peek_frame_header, try_decode_frame,
    CodecError, FrameHeader, HEADER_SIZE, MAX_FRAME_PAYLOAD,
};
pub use message::{
    ClientRequest, Envelope, FileRowData, QueryDefaults, ResponsePayload,
    ERROR_CODE_ADMIN_ONLY_QUERY, ERROR_CODE_HANDSHAKE_REQUIRED,
    ERROR_CODE_PROTOCOL_VERSION_MISMATCH, ERROR_CODE_SESSION_EXPIRED, ERROR_CODE_UNAUTHORIZED,
    PIPE_NAME, PROTOCOL_VERSION, USER_PIPE_NAME,
};
pub use status::{
    ContentIndexState, ContentIndexStatus, ContentIndexTask, VolumeState, VolumeStatusSummary,
    WorkerStatus,
};
