// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! On-disk snapshot format for a volume index: a fixed header, the compact records and the name
//! arena. The layout is the persistence contract between the service that writes the cache and
//! every reader.

use crate::model::FileRecord;
use std::mem::size_of;
use thiserror::Error;

pub const INDEX_MAGIC: &[u8; 8] = b"CAIRNIDX";
pub const INDEX_VERSION: u32 = 1;
pub const HEADER_SIZE: usize = 64;

/// Header byte 13 says whether the saved records already carry correct recursive directory sizes.
/// Zero means the writer could not prove them, so a loader must aggregate once instead of trusting
/// the stored zeroes; the reserved bytes around it stay zero.
pub const HEADER_DIRECTORY_SIZES_VALID: usize = 13;

/// A record's encoded size. `FileRecord` is `#[repr(C)]` with 8-byte alignment and fields that sum
/// to this size without padding, so the encoded form is field-for-field identical to the in-memory
/// one.
pub const RECORD_SIZE: usize = size_of::<FileRecord>();

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IndexFileError {
    #[error("invalid index file magic signature: expected CAIRNIDX")]
    InvalidMagic,
    #[error("unsupported index format version: {0}")]
    UnsupportedVersion(u32),
}

/// The 64-byte file header. Reserved bytes stay zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexHeader {
    pub drive_letter: char,
    pub directory_sizes_valid: bool,
    pub volume_serial: u64,
    pub journal_id: u64,
    pub last_usn: i64,
    pub record_count: u32,
    pub arena_len: u32,
    pub folded_len: u32,
    pub checksum: u32,
}

impl IndexHeader {
    pub fn encode(&self) -> [u8; HEADER_SIZE] {
        let mut header = [0u8; HEADER_SIZE];
        header[0..8].copy_from_slice(INDEX_MAGIC);
        header[8..12].copy_from_slice(&INDEX_VERSION.to_le_bytes());
        header[12] = self.drive_letter as u8;
        header[HEADER_DIRECTORY_SIZES_VALID] = u8::from(self.directory_sizes_valid);
        header[16..24].copy_from_slice(&self.volume_serial.to_le_bytes());
        header[24..32].copy_from_slice(&self.journal_id.to_le_bytes());
        header[32..40].copy_from_slice(&self.last_usn.to_le_bytes());
        header[40..44].copy_from_slice(&self.record_count.to_le_bytes());
        header[44..48].copy_from_slice(&self.arena_len.to_le_bytes());
        header[48..52].copy_from_slice(&self.folded_len.to_le_bytes());
        header[52..56].copy_from_slice(&self.checksum.to_le_bytes());
        header
    }

    pub fn decode(bytes: &[u8; HEADER_SIZE]) -> Result<Self, IndexFileError> {
        if &bytes[0..8] != INDEX_MAGIC {
            return Err(IndexFileError::InvalidMagic);
        }
        let version = u32::from_le_bytes(bytes[8..12].try_into().expect("four bytes"));
        if version != INDEX_VERSION {
            return Err(IndexFileError::UnsupportedVersion(version));
        }
        Ok(Self {
            drive_letter: bytes[12] as char,
            directory_sizes_valid: bytes[HEADER_DIRECTORY_SIZES_VALID] != 0,
            volume_serial: u64::from_le_bytes(bytes[16..24].try_into().expect("eight bytes")),
            journal_id: u64::from_le_bytes(bytes[24..32].try_into().expect("eight bytes")),
            last_usn: i64::from_le_bytes(bytes[32..40].try_into().expect("eight bytes")),
            record_count: u32::from_le_bytes(bytes[40..44].try_into().expect("four bytes")),
            arena_len: u32::from_le_bytes(bytes[44..48].try_into().expect("four bytes")),
            folded_len: u32::from_le_bytes(bytes[48..52].try_into().expect("four bytes")),
            checksum: u32::from_le_bytes(bytes[52..56].try_into().expect("four bytes")),
        })
    }
}

/// Encodes one record into its 48-byte on-disk form.
pub fn encode_record(record: &FileRecord, out: &mut [u8; RECORD_SIZE]) {
    out[0..8].copy_from_slice(&record.frn.to_le_bytes());
    out[8..12].copy_from_slice(&record.parent_idx.to_le_bytes());
    out[12..16].copy_from_slice(&record.name_offset.to_le_bytes());
    out[16..18].copy_from_slice(&record.name_len.to_le_bytes());
    out[18..20].copy_from_slice(&record.ext_offset.to_le_bytes());
    out[20..22].copy_from_slice(&record.volume_id.to_le_bytes());
    out[22..24].copy_from_slice(&record.flags.to_le_bytes());
    out[24..32].copy_from_slice(&record.size.to_le_bytes());
    out[32..40].copy_from_slice(&record.modified_ts.to_le_bytes());
    out[40..48].copy_from_slice(&record.created_ts.to_le_bytes());
}

/// Decodes one 48-byte on-disk record.
pub fn decode_record(bytes: &[u8; RECORD_SIZE]) -> FileRecord {
    FileRecord {
        frn: u64::from_le_bytes(bytes[0..8].try_into().expect("eight bytes")),
        parent_idx: u32::from_le_bytes(bytes[8..12].try_into().expect("four bytes")),
        name_offset: u32::from_le_bytes(bytes[12..16].try_into().expect("four bytes")),
        name_len: u16::from_le_bytes(bytes[16..18].try_into().expect("two bytes")),
        ext_offset: u16::from_le_bytes(bytes[18..20].try_into().expect("two bytes")),
        volume_id: u16::from_le_bytes(bytes[20..22].try_into().expect("two bytes")),
        flags: u16::from_le_bytes(bytes[22..24].try_into().expect("two bytes")),
        size: u64::from_le_bytes(bytes[24..32].try_into().expect("eight bytes")),
        modified_ts: i64::from_le_bytes(bytes[32..40].try_into().expect("eight bytes")),
        created_ts: i64::from_le_bytes(bytes[40..48].try_into().expect("eight bytes")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::offset_of;

    fn sample_record() -> FileRecord {
        FileRecord {
            frn: 0x1122_3344_5566_7788,
            parent_idx: 0x99AA_BBCC,
            name_offset: 0x0102_0304,
            name_len: 0x0506,
            ext_offset: 0x0708,
            volume_id: 0x090A,
            flags: 0x0B0C,
            size: 0x0D0E_0F10_1112_1314,
            modified_ts: -2,
            created_ts: 1_700_000_000,
        }
    }

    #[test]
    fn the_record_layout_matches_the_encoded_form() {
        assert_eq!(size_of::<FileRecord>(), 48);
        assert_eq!(offset_of!(FileRecord, frn), 0);
        assert_eq!(offset_of!(FileRecord, parent_idx), 8);
        assert_eq!(offset_of!(FileRecord, name_offset), 12);
        assert_eq!(offset_of!(FileRecord, name_len), 16);
        assert_eq!(offset_of!(FileRecord, ext_offset), 18);
        assert_eq!(offset_of!(FileRecord, volume_id), 20);
        assert_eq!(offset_of!(FileRecord, flags), 22);
        assert_eq!(offset_of!(FileRecord, size), 24);
        assert_eq!(offset_of!(FileRecord, modified_ts), 32);
        assert_eq!(offset_of!(FileRecord, created_ts), 40);
    }

    #[test]
    fn a_record_round_trips_through_its_encoded_form() {
        let record = sample_record();
        let mut bytes = [0u8; RECORD_SIZE];
        encode_record(&record, &mut bytes);
        assert_eq!(decode_record(&bytes), record);

        // Independent field bytes: the first eight are the little-endian FRN.
        assert_eq!(&bytes[0..8], &0x1122_3344_5566_7788u64.to_le_bytes());
        assert_eq!(&bytes[22..24], &0x0B0Cu16.to_le_bytes());
    }

    #[test]
    fn the_header_round_trips_and_keeps_the_validity_byte_at_thirteen() {
        let header = IndexHeader {
            drive_letter: 'C',
            directory_sizes_valid: true,
            volume_serial: 0x0102_0304_0506_0708,
            journal_id: 42,
            last_usn: -7,
            record_count: 3,
            arena_len: 100,
            folded_len: 100,
            checksum: 0xDEAD_BEEF,
        };
        let bytes = header.encode();
        assert_eq!(bytes[HEADER_DIRECTORY_SIZES_VALID], 1);
        assert!(bytes[14..16].iter().all(|byte| *byte == 0));
        assert!(bytes[56..64].iter().all(|byte| *byte == 0));
        assert_eq!(IndexHeader::decode(&bytes), Ok(header));

        let mut cleared = bytes;
        cleared[HEADER_DIRECTORY_SIZES_VALID] = 0;
        let decoded = IndexHeader::decode(&cleared).expect("legacy byte still decodes");
        assert!(!decoded.directory_sizes_valid);
    }

    #[test]
    fn the_header_rejects_bad_magic_and_versions() {
        let header = IndexHeader {
            drive_letter: 'D',
            directory_sizes_valid: false,
            volume_serial: 0,
            journal_id: 0,
            last_usn: 0,
            record_count: 0,
            arena_len: 0,
            folded_len: 0,
            checksum: 0,
        };

        let mut bad_magic = header.encode();
        bad_magic[0] = b'X';
        assert_eq!(
            IndexHeader::decode(&bad_magic),
            Err(IndexFileError::InvalidMagic)
        );

        let mut newer_version = header.encode();
        newer_version[8..12].copy_from_slice(&9u32.to_le_bytes());
        assert_eq!(
            IndexHeader::decode(&newer_version),
            Err(IndexFileError::UnsupportedVersion(9))
        );
    }
}
