// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Framing codec for Named Pipe length-prefixed postcard streams.

use crate::message::PROTOCOL_VERSION;
use serde::de::DeserializeOwned;
use serde::Serialize;
use thiserror::Error;

pub const MAX_FRAME_PAYLOAD: usize = 16 * 1024 * 1024; // 16 MiB limit to prevent OOM
/// `[u32 length][u16 version]` preceding every payload.
pub const HEADER_SIZE: usize = 6;

#[derive(Error, Debug)]
pub enum CodecError {
    #[error("Frame payload too large: {0} bytes (max {MAX_FRAME_PAYLOAD})")]
    FrameTooLarge(usize),
    #[error("Incompatible protocol version: received {received}, supported {PROTOCOL_VERSION}")]
    IncompatibleVersion { received: u16 },
    #[error("Serialization error: {0}")]
    Serialization(#[from] postcard::Error),
    #[error("Incomplete frame: expected {expected} bytes, got {actual}")]
    Incomplete { expected: usize, actual: usize },
}

/// Encodes a message into a length-prefixed frame:
/// `[u32 LE: payload_len][u16 LE: protocol_version][postcard payload]`
pub fn encode_frame<T: Serialize>(payload: &T) -> Result<Vec<u8>, CodecError> {
    encode_frame_with_version(PROTOCOL_VERSION, payload)
}

struct VecWriter<'a>(&'a mut Vec<u8>);

impl<'a> Extend<u8> for VecWriter<'a> {
    fn extend<I: IntoIterator<Item = u8>>(&mut self, iter: I) {
        self.0.extend(iter);
    }
}

/// Encodes a message into an existing byte buffer, appending the length-prefixed frame:
/// `[u32 LE: payload_len][u16 LE: protocol_version][postcard payload]`
pub fn encode_frame_into<T: Serialize>(
    buf: &mut Vec<u8>,
    version: u16,
    payload: &T,
) -> Result<(), CodecError> {
    let start_offset = buf.len();
    buf.resize(start_offset + HEADER_SIZE, 0u8);
    if let Err(e) = postcard::to_extend(payload, VecWriter(buf)) {
        buf.truncate(start_offset);
        return Err(CodecError::Serialization(e));
    }
    let payload_len = buf.len() - start_offset - HEADER_SIZE;
    if payload_len > MAX_FRAME_PAYLOAD {
        buf.truncate(start_offset);
        return Err(CodecError::FrameTooLarge(payload_len));
    }

    let payload_len_u32 = payload_len as u32;
    buf[start_offset..start_offset + 4].copy_from_slice(&payload_len_u32.to_le_bytes());
    buf[start_offset + 4..start_offset + 6].copy_from_slice(&version.to_le_bytes());
    Ok(())
}

/// Encodes a message with an explicit protocol version in the frame header.
///
/// Needed to answer a client that speaks another version: a rejection the recipient cannot decode is
/// indistinguishable from a broken service, and SVC-03 requires the GUI to explain the mismatch.
pub fn encode_frame_with_version<T: Serialize>(
    version: u16,
    payload: &T,
) -> Result<Vec<u8>, CodecError> {
    let mut frame = Vec::with_capacity(512);
    encode_frame_into(&mut frame, version, payload)?;
    Ok(frame)
}

/// A frame header: how many payload bytes follow and which protocol version the sender declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    pub payload_len: u32,
    pub version: u16,
}

impl FrameHeader {
    /// Total size of the frame including its header.
    pub fn total_len(&self) -> usize {
        HEADER_SIZE + self.payload_len as usize
    }
}

/// Reads the frame header without touching the payload, `Ok(None)` while it is incomplete.
///
/// The payload is deliberately not decoded here: a peer of another protocol version may serialize a
/// message that would deserialize into the wrong variant, so the version gate has to run first.
pub fn peek_frame_header(buf: &[u8]) -> Result<Option<FrameHeader>, CodecError> {
    if buf.len() < HEADER_SIZE {
        return Ok(None);
    }

    let payload_len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if payload_len as usize > MAX_FRAME_PAYLOAD {
        return Err(CodecError::FrameTooLarge(payload_len as usize));
    }

    let version = u16::from_le_bytes([buf[4], buf[5]]);
    Ok(Some(FrameHeader {
        payload_len,
        version,
    }))
}

/// Decodes a frame from bytes if a complete frame is present.
/// Returns `Ok(Some((consumed_bytes, payload)))` or `Ok(None)` if more data is needed.
pub fn try_decode_frame<T: DeserializeOwned>(buf: &[u8]) -> Result<Option<(usize, T)>, CodecError> {
    let Some(header) = peek_frame_header(buf)? else {
        return Ok(None);
    };

    let total_len = header.total_len();
    if buf.len() < total_len {
        return Ok(None);
    }

    if header.version != PROTOCOL_VERSION {
        return Err(CodecError::IncompatibleVersion {
            received: header.version,
        });
    }

    let payload = postcard::from_bytes(&buf[HEADER_SIZE..total_len])?;
    Ok(Some((total_len, payload)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ClientRequest;

    #[test]
    fn test_encode_decode_roundtrip() {
        let req = ClientRequest::Hello {
            client_protocol_version: 1,
            pid: 1234,
        };

        let frame = encode_frame(&req).expect("encode failed");
        assert!(frame.len() >= 6);

        let decoded = try_decode_frame::<ClientRequest>(&frame)
            .expect("decode failed")
            .expect("should be complete");

        assert_eq!(decoded.0, frame.len());
        match decoded.1 {
            ClientRequest::Hello {
                client_protocol_version,
                pid,
            } => {
                assert_eq!(client_protocol_version, 1);
                assert_eq!(pid, 1234);
            }
            _ => panic!("Unexpected decoded variant"),
        }
    }

    /// The query-wide toggles are part of the request on the wire, not encoded in its text:
    /// a frame must carry them to the service unchanged.
    #[test]
    fn query_request_round_trips_its_defaults() {
        let defaults = cairn_core::syntax::QueryDefaults {
            case_sensitive: true,
            whole_words: false,
            match_path: true,
            is_regex: false,
            content: true,
            pinyin: true,
            ignore_punctuation: false,
            ignore_whitespace: true,
            ignore_diacritics: false,
            exclude_hidden: true,
            exclude_system: false,
        };
        let sort_spec =
            cairn_core::sort::SortSpec::parse_clause("-name,+size").expect("valid sort spec");
        let req = ClientRequest::Query {
            query_id: 7,
            raw_query: "报告 季度".to_string(),
            defaults,
            sort_spec: sort_spec.clone(),
            first_page_limit: 50,
            source_scope: Some(cairn_core::property_store::SourceId(1_000)),
        };

        let frame = encode_frame(&req).expect("encode failed");
        let decoded = try_decode_frame::<ClientRequest>(&frame)
            .expect("decode failed")
            .expect("should be complete")
            .1;

        match decoded {
            ClientRequest::Query {
                query_id,
                raw_query,
                defaults: received,
                sort_spec: decoded_sort,
                first_page_limit,
                source_scope,
            } => {
                assert_eq!(query_id, 7);
                assert_eq!(raw_query, "报告 季度");
                assert_eq!(received, defaults, "toggles must survive the frame");
                assert_eq!(decoded_sort, sort_spec);
                assert_eq!(first_page_limit, 50);
                assert_eq!(
                    source_scope,
                    Some(cairn_core::property_store::SourceId(1_000)),
                    "the source scope must survive the frame"
                );
            }
            other => panic!("Unexpected decoded request: {other:?}"),
        }

        // Every flag is individually addressable on the wire: one differing bit is one
        // different request, so a toggled switch cannot be lost silently.
        let mut flipped = defaults;
        flipped.case_sensitive = false;
        let other = ClientRequest::Query {
            query_id: 7,
            raw_query: "报告 季度".to_string(),
            defaults: flipped,
            sort_spec,
            first_page_limit: 50,
            source_scope: None,
        };
        assert_ne!(
            encode_frame(&req).expect("encode"),
            encode_frame(&other).expect("encode"),
            "a single toggle must change the frame"
        );
    }

    #[test]
    fn extended_payloads_round_trip_without_data_loss() {
        use crate::message::{
            FileRowData, PropertyKind, QueryDegradedReason, RecordPropertyData, ResponsePayload,
            SourceId,
        };
        use crate::status::{
            IndexJournalEntry, IndexJournalStatus, JournalChangeKind, SourceKind,
            SourceStatusSummary, VolumeState,
        };

        let row = FileRowData {
            frn: 42,
            name: "clip.mp4".to_string(),
            path: r"C:\Media\clip.mp4".to_string(),
            size: 4096,
            modified_ts: 1_700_000_000,
            created_ts: 1_699_000_000,
            is_dir: false,
            flags: 0,
            name_highlights: vec![(0, 4)],
            path_highlights: Vec::new(),
            snippets: vec!["line one".to_string(), "line two".to_string()],
            properties: Some(RecordPropertyData {
                length_ms: Some(12_345),
                author: Some("Cairn".to_string()),
                tags: Some("demo;video".to_string()),
                dimensions: Some((1920, 1080)),
                framerate_mfps: Some(29_970),
            }),
        };
        let payload = ResponsePayload::QueryResult {
            query_id: 9,
            session_id: 3,
            total_matches: 1,
            total_bytes: 4096,
            first_page: vec![row.clone()],
            applied_sort: cairn_core::sort::SortSpec::parse_clause("-length,+name").unwrap(),
            degraded: Some(QueryDegradedReason::PropertyIndexDisabled {
                requested: vec![PropertyKind::Length],
            }),
        };
        let encoded = encode_frame(&payload).expect("encode query result");
        let (_, decoded) = try_decode_frame::<ResponsePayload>(&encoded)
            .expect("decode")
            .expect("complete");
        match decoded {
            ResponsePayload::QueryResult {
                first_page,
                applied_sort,
                degraded,
                ..
            } => {
                assert_eq!(first_page, vec![row]);
                assert_eq!(applied_sort.to_clause(), "length-desc,name-asc");
                assert_eq!(
                    degraded,
                    Some(QueryDegradedReason::PropertyIndexDisabled {
                        requested: vec![PropertyKind::Length],
                    })
                );
            }
            other => panic!("unexpected payload: {other:?}"),
        }

        let journal = ResponsePayload::IndexJournalResult {
            status: IndexJournalStatus {
                enabled: true,
                entry_count: 1,
                resident_bytes: 128,
                max_entries: 200_000,
                max_bytes: 32 * 1024 * 1024,
                oldest_seq: 1,
                newest_seq: 1,
            },
            entries: vec![IndexJournalEntry {
                seq: 1,
                timestamp_ts: 1_700_000_123,
                source_id: SourceId(7),
                frn: 88,
                kind: JournalChangeKind::Renamed,
                path: r"C:\Docs\new.md".to_string(),
                old_path: Some(r"C:\Docs\old.md".to_string()),
            }],
        };
        let encoded_journal = encode_frame(&journal).expect("encode journal");
        let (_, decoded_journal) = try_decode_frame::<ResponsePayload>(&encoded_journal)
            .expect("decode journal")
            .expect("complete");
        assert!(matches!(
            decoded_journal,
            ResponsePayload::IndexJournalResult { ref entries, .. } if entries.len() == 1
        ));

        let sources = ResponsePayload::SourcesResult {
            sources: vec![SourceStatusSummary {
                source_id: SourceId(2),
                kind: SourceKind::Folder,
                root_path: r"D:\Notes".to_string(),
                volume_guid: String::new(),
                status: VolumeState::Synced,
                indexed_count: 15,
                live_watch_enabled: true,
            }],
        };
        let encoded_sources = encode_frame(&sources).expect("encode sources");
        let (_, decoded_sources) = try_decode_frame::<ResponsePayload>(&encoded_sources)
            .expect("decode sources")
            .expect("complete");
        assert!(matches!(
            decoded_sources,
            ResponsePayload::SourcesResult { ref sources } if sources.len() == 1
        ));
    }

    #[test]
    fn frame_header_reports_the_declared_version() {
        let req = ClientRequest::GetStatus;

        let current = encode_frame(&req).expect("encode failed");
        assert_eq!(
            peek_frame_header(&current)
                .expect("header")
                .expect("complete"),
            FrameHeader {
                payload_len: (current.len() - HEADER_SIZE) as u32,
                version: PROTOCOL_VERSION,
            }
        );

        let foreign = encode_frame_with_version(PROTOCOL_VERSION + 1, &req).expect("encode failed");
        let header = peek_frame_header(&foreign)
            .expect("header")
            .expect("complete");
        assert_eq!(header.version, PROTOCOL_VERSION + 1);
        assert_eq!(header.total_len(), foreign.len());

        // A header that has not fully arrived yet is not an error: the reader waits for more bytes.
        assert_eq!(
            peek_frame_header(&foreign[..HEADER_SIZE - 1]).unwrap(),
            None
        );
    }

    #[test]
    fn decoding_a_frame_of_another_version_is_refused_before_the_payload_is_trusted() {
        let legacy = encode_frame_with_version(PROTOCOL_VERSION - 1, &ClientRequest::GetConfig)
            .expect("encode failed");

        assert!(
            matches!(
                try_decode_frame::<ClientRequest>(&legacy),
                Err(CodecError::IncompatibleVersion {
                    received: v
                }) if v == PROTOCOL_VERSION - 1
            ),
            "a frame of another version must never be deserialized"
        );
    }

    #[test]
    fn an_absurd_frame_length_is_rejected_without_allocating() {
        let mut buf = vec![0u8; HEADER_SIZE];
        buf[..4].copy_from_slice(&(MAX_FRAME_PAYLOAD as u32 + 1).to_le_bytes());
        assert!(matches!(
            try_decode_frame::<ClientRequest>(&buf),
            Err(CodecError::FrameTooLarge(_))
        ));
        assert!(matches!(
            peek_frame_header(&buf),
            Err(CodecError::FrameTooLarge(_))
        ));
    }

    #[test]
    fn a_partial_frame_waits_for_the_rest_of_its_payload() {
        let frame = encode_frame(&ClientRequest::GetConfig).expect("encode failed");
        assert_eq!(
            try_decode_frame::<ClientRequest>(&frame[..frame.len() - 1]).expect("partial"),
            None
        );
        assert!(try_decode_frame::<ClientRequest>(&frame)
            .expect("complete")
            .is_some());
    }
}

/// Pins the serialized `ClientRequest::Query` payload; the header carries the protocol version.
#[cfg(test)]
mod wire_layout {
    use super::{encode_frame, HEADER_SIZE};
    use crate::message::{
        ClientRequest, SortColumn, SortDirection, SortSpec, SourceId, PROTOCOL_VERSION,
    };
    use std::str::FromStr;

    #[test]
    fn the_query_wire_layout_is_pinned() {
        let req = ClientRequest::Query {
            query_id: 7,
            raw_query: "rs".to_string(),
            defaults: crate::message::QueryDefaults::default(),
            sort_spec: SortSpec::single(SortColumn::Size, SortDirection::Ascending),
            first_page_limit: 50,
            source_scope: None,
        };
        let mut expected = vec![25, 0, 0, 0]; // payload length
        expected.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
        expected.extend_from_slice(&[
            1, // Query variant
            7, // query_id
            2, b'r', b's', // raw_query
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // QueryDefaults toggles, in declaration order
            1, 4, b's', b'i', b'z', b'e', 0,  // sort_spec: 1 key, column="size", Ascending(0)
            50, // first_page_limit
            0,  // source_scope: None
        ]);
        assert_eq!(encode_frame(&req).expect("encode"), expected);
    }

    /// The scope travels as a postcard `Option`: a present id appends the variant tag and the
    /// varint the protocol already publishes for `SourceId` elsewhere.
    #[test]
    fn a_scoped_query_appends_the_source_id_to_the_pinned_payload() {
        let req = ClientRequest::Query {
            query_id: 7,
            raw_query: "rs".to_string(),
            defaults: crate::message::QueryDefaults::default(),
            sort_spec: SortSpec::single(SortColumn::Size, SortDirection::Ascending),
            first_page_limit: 50,
            source_scope: Some(SourceId(1_000)),
        };
        let scoped = encode_frame(&req).expect("encode");
        let unscoped = encode_frame(&ClientRequest::Query {
            query_id: 7,
            raw_query: "rs".to_string(),
            defaults: crate::message::QueryDefaults::default(),
            sort_spec: SortSpec::single(SortColumn::Size, SortDirection::Ascending),
            first_page_limit: 50,
            source_scope: None,
        })
        .expect("encode");
        let scoped_payload = &scoped[HEADER_SIZE..];
        let unscoped_payload = &unscoped[HEADER_SIZE..];
        assert_eq!(scoped_payload.len(), unscoped_payload.len() + 2);
        assert_eq!(
            &scoped_payload[..unscoped_payload.len() - 1],
            &unscoped_payload[..unscoped_payload.len() - 1]
        );
        assert_eq!(
            &scoped_payload[unscoped_payload.len() - 1..],
            &[1, 0xE8, 0x07],
            "Some(SourceId(1000)) replaces the None tag with the id"
        );
    }

    /// The typed column keeps the released field shape: the enum's postcard encoding is exactly
    /// the key string it stands for, and an unknown value is refused rather than defaulted.
    #[test]
    fn the_typed_sort_column_travels_as_its_key_string() {
        let enum_bytes = postcard::to_extend(&SortColumn::Size, Vec::new()).expect("encode enum");
        let string_bytes =
            postcard::to_extend(&"size".to_string(), Vec::new()).expect("encode string");
        assert_eq!(enum_bytes, string_bytes);

        let bogus = postcard::to_extend(&"bogus".to_string(), Vec::new()).expect("encode bogus");
        assert!(
            postcard::from_bytes::<SortColumn>(&bogus).is_err(),
            "an unknown key must not decode into a default"
        );
        assert!(cairn_core::sort::SortColumn::from_str("bogus")
            .expect_err("the key vocabulary is closed")
            .to_string()
            .contains("unknown sort column"));
    }
}
