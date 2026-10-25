// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! USN (Update Sequence Number) Change Journal monitoring and event parser.

use cairn_core::{FLAG_DIRECTORY, FLAG_REPARSE_POINT};
use thiserror::Error;

pub const FSCTL_QUERY_USN_JOURNAL: u32 = 0x000900F4;
pub const FSCTL_READ_USN_JOURNAL: u32 = 0x000900BB;

pub const USN_REASON_DATA_OVERWRITE: u32 = 0x00000001;
pub const USN_REASON_DATA_EXTEND: u32 = 0x00000002;
pub const USN_REASON_DATA_TRUNCATION: u32 = 0x00000004;
pub const USN_REASON_FILE_CREATE: u32 = 0x00000100;
pub const USN_REASON_FILE_DELETE: u32 = 0x00000200;
pub const USN_REASON_RENAME_OLD_NAME: u32 = 0x00001000;
pub const USN_REASON_RENAME_NEW_NAME: u32 = 0x00002000;
pub const USN_REASON_BASIC_INFO_CHANGE: u32 = 0x00008000;
pub const USN_REASON_CLOSE: u32 = 0x80000000;

pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x00000010;
pub const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x00000400;

/// Translates the Win32 attributes carried by a USN record into the directory/reparse flag bits an
/// indexed record keeps. Other attributes are not part of the compact record.
pub fn file_record_flags(attributes: u32) -> u16 {
    let mut flags = 0u16;
    if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        flags |= FLAG_DIRECTORY;
    }
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        flags |= FLAG_REPARSE_POINT;
    }
    flags
}

#[derive(Error, Debug)]
pub enum UsnError {
    #[error("USN Journal query failed: Win32 error {0:#x}")]
    QueryFailed(u32),
    #[error("USN Journal read failed: Win32 error {0:#x}")]
    ReadFailed(u32),
    #[error("Buffer too small: {0} bytes")]
    BufferTooSmall(usize),
}

/// Metadata queried from the volume's USN journal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsnJournalData {
    pub journal_id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
    pub lowest_valid_usn: i64,
}

impl UsnJournalData {
    pub fn parse(buf: &[u8]) -> Result<Self, UsnError> {
        if buf.len() < 56 {
            return Err(UsnError::BufferTooSmall(buf.len()));
        }

        let journal_id = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let first_usn = i64::from_le_bytes(buf[8..16].try_into().unwrap());
        let next_usn = i64::from_le_bytes(buf[16..24].try_into().unwrap());
        let lowest_valid_usn = i64::from_le_bytes(buf[24..32].try_into().unwrap());

        Ok(Self {
            journal_id,
            first_usn,
            next_usn,
            lowest_valid_usn,
        })
    }
}

/// Win32 input parameter structure for `FSCTL_READ_USN_JOURNAL`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadUsnJournalDataV0 {
    pub start_usn: i64,
    pub reason_mask: u32,
    pub return_only_on_close: u32,
    pub timeout: u64,
    pub bytes_to_wait_for: u64,
    pub usn_journal_id: u64,
}

/// High-level parsed event emitted from a USN journal record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsnChange {
    Created {
        frn: u64,
        parent_frn: u64,
        name: String,
        is_dir: bool,
        attributes: u32,
        ts: i64,
    },
    Deleted {
        frn: u64,
    },
    Renamed {
        frn: u64,
        parent_frn: u64,
        name: String,
        is_dir: bool,
        is_new_name: bool,
    },
    BasicInfoChanged {
        frn: u64,
        ts: i64,
        attributes: u32,
    },
    DataChanged {
        frn: u64,
    },
}

/// Parses raw USN records from `FSCTL_READ_USN_JOURNAL` output buffer.
///
/// Returns `(next_usn_to_query, list_of_parsed_changes)`.
pub fn parse_usn_records(buf: &[u8]) -> (i64, Vec<UsnChange>) {
    if buf.len() < 8 {
        return (0, Vec::new());
    }

    // The first 8 bytes of the buffer contain the NextUsn to use for subsequent queries
    let next_usn = i64::from_le_bytes(buf[0..8].try_into().unwrap());
    let mut changes = Vec::new();
    let mut cursor = 8;

    while cursor + 60 <= buf.len() {
        let record_len = u32::from_le_bytes(buf[cursor..cursor + 4].try_into().unwrap()) as usize;
        if record_len == 0 || cursor + record_len > buf.len() {
            break;
        }

        let major_ver = u16::from_le_bytes(buf[cursor + 4..cursor + 6].try_into().unwrap());
        if major_ver == 2 || major_ver == 3 {
            let frn = u64::from_le_bytes(buf[cursor + 8..cursor + 16].try_into().unwrap());
            let parent_frn = u64::from_le_bytes(buf[cursor + 16..cursor + 24].try_into().unwrap());
            let timestamp = i64::from_le_bytes(buf[cursor + 32..cursor + 40].try_into().unwrap());
            let reason = u32::from_le_bytes(buf[cursor + 40..cursor + 44].try_into().unwrap());
            let file_attrs = u32::from_le_bytes(buf[cursor + 52..cursor + 56].try_into().unwrap());
            let fn_len =
                u16::from_le_bytes(buf[cursor + 56..cursor + 58].try_into().unwrap()) as usize;
            let fn_offset =
                u16::from_le_bytes(buf[cursor + 58..cursor + 60].try_into().unwrap()) as usize;

            let is_dir = (file_attrs & FILE_ATTRIBUTE_DIRECTORY) != 0;

            let name = if cursor + fn_offset + fn_len <= cursor + record_len {
                let name_bytes = &buf[cursor + fn_offset..cursor + fn_offset + fn_len];
                let u16_chars: Vec<u16> = name_bytes
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                String::from_utf16_lossy(&u16_chars)
            } else {
                String::new()
            };

            // SPEC IDX-03: Process only final CLOSE records or rename pair records
            let is_close = (reason & USN_REASON_CLOSE) != 0;
            let is_rename =
                (reason & (USN_REASON_RENAME_NEW_NAME | USN_REASON_RENAME_OLD_NAME)) != 0;
            if !is_close && !is_rename {
                cursor += record_len;
                continue;
            }

            // MUST prioritize USN_REASON_FILE_DELETE
            if (reason & USN_REASON_FILE_DELETE) != 0 {
                changes.push(UsnChange::Deleted { frn });
            } else if (reason & USN_REASON_FILE_CREATE) != 0 {
                changes.push(UsnChange::Created {
                    frn,
                    parent_frn,
                    name,
                    is_dir,
                    attributes: file_attrs,
                    ts: timestamp,
                });
            } else if (reason & USN_REASON_RENAME_NEW_NAME) != 0 {
                changes.push(UsnChange::Renamed {
                    frn,
                    parent_frn,
                    name,
                    is_dir,
                    is_new_name: true,
                });
            } else if (reason & USN_REASON_RENAME_OLD_NAME) != 0 {
                changes.push(UsnChange::Renamed {
                    frn,
                    parent_frn,
                    name,
                    is_dir,
                    is_new_name: false,
                });
            } else if (reason
                & (USN_REASON_DATA_EXTEND | USN_REASON_DATA_TRUNCATION | USN_REASON_DATA_OVERWRITE))
                != 0
            {
                changes.push(UsnChange::DataChanged { frn });
            } else if (reason & USN_REASON_BASIC_INFO_CHANGE) != 0 {
                changes.push(UsnChange::BasicInfoChanged {
                    frn,
                    ts: timestamp,
                    attributes: file_attrs,
                });
            }
        }

        cursor += record_len;
    }

    (next_usn, changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_delete_priority() {
        // When both FILE_CREATE and FILE_DELETE are present, DELETE must be chosen
        let mut buf = vec![0u8; 80];
        buf[0..8].copy_from_slice(&1000i64.to_le_bytes()); // NextUsn
        buf[8..12].copy_from_slice(&72u32.to_le_bytes()); // RecordLength
        buf[12..14].copy_from_slice(&2u16.to_le_bytes()); // MajorVersion = 2
        buf[16..24].copy_from_slice(&42u64.to_le_bytes()); // FRN
                                                           // Reason: CREATE | DELETE
        let reason = USN_REASON_FILE_CREATE | USN_REASON_FILE_DELETE | USN_REASON_CLOSE;
        buf[48..52].copy_from_slice(&reason.to_le_bytes());

        let (next, changes) = parse_usn_records(&buf);
        assert_eq!(next, 1000);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0], UsnChange::Deleted { frn: 42 });
    }

    #[test]
    fn test_created_record_keeps_file_attributes() {
        let name: Vec<u16> = "link.txt".encode_utf16().collect();
        let name_bytes = name.len() * 2;
        let record_len = (60 + name_bytes + 7) & !7;
        let mut buf = vec![0u8; 8 + record_len];
        buf[0..8].copy_from_slice(&2000i64.to_le_bytes()); // NextUsn
        buf[8..12].copy_from_slice(&(record_len as u32).to_le_bytes());
        buf[12..14].copy_from_slice(&2u16.to_le_bytes()); // MajorVersion = 2
        buf[16..24].copy_from_slice(&77u64.to_le_bytes()); // FRN
        buf[24..32].copy_from_slice(&42u64.to_le_bytes()); // Parent FRN
        buf[40..48].copy_from_slice(&1234i64.to_le_bytes()); // Timestamp
        let reason = USN_REASON_FILE_CREATE | USN_REASON_CLOSE;
        buf[48..52].copy_from_slice(&reason.to_le_bytes());
        let attributes = FILE_ATTRIBUTE_REPARSE_POINT | 0x20; // REPARSE_POINT | ARCHIVE
        buf[60..64].copy_from_slice(&attributes.to_le_bytes());
        buf[64..66].copy_from_slice(&(name_bytes as u16).to_le_bytes());
        buf[66..68].copy_from_slice(&60u16.to_le_bytes()); // NameOffset
        for (i, unit) in name.iter().enumerate() {
            let at = 8 + 60 + i * 2;
            buf[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        }

        let (next, changes) = parse_usn_records(&buf);
        assert_eq!(next, 2000);
        assert_eq!(
            changes,
            vec![UsnChange::Created {
                frn: 77,
                parent_frn: 42,
                name: "link.txt".to_string(),
                is_dir: false,
                attributes,
                ts: 1234,
            }]
        );
        assert_eq!(
            file_record_flags(attributes) & FLAG_REPARSE_POINT,
            FLAG_REPARSE_POINT
        );
        assert_eq!(
            file_record_flags(FILE_ATTRIBUTE_DIRECTORY) & FLAG_DIRECTORY,
            FLAG_DIRECTORY
        );
    }

    #[test]
    fn test_read_usn_journal_data_v0_layout() {
        assert_eq!(std::mem::size_of::<ReadUsnJournalDataV0>(), 40);
        let data = ReadUsnJournalDataV0 {
            start_usn: 12345,
            reason_mask: 0xFFFFFFFF,
            return_only_on_close: 1,
            timeout: 0,
            bytes_to_wait_for: 0,
            usn_journal_id: 99999,
        };
        assert_eq!(data.start_usn, 12345);
        assert_eq!(data.usn_journal_id, 99999);
    }

    #[test]
    fn test_usn_journal_data_parse() {
        let mut buf = [0u8; 56];
        buf[0..8].copy_from_slice(&0x0102030405060708u64.to_le_bytes());
        buf[8..16].copy_from_slice(&100i64.to_le_bytes());
        buf[16..24].copy_from_slice(&500i64.to_le_bytes());
        buf[24..32].copy_from_slice(&50i64.to_le_bytes());

        let parsed = UsnJournalData::parse(&buf).expect("should parse");
        assert_eq!(parsed.journal_id, 0x0102030405060708);
        assert_eq!(parsed.first_usn, 100);
        assert_eq!(parsed.next_usn, 500);
        assert_eq!(parsed.lowest_valid_usn, 50);

        // Buffer too small should error
        assert!(UsnJournalData::parse(&buf[..40]).is_err());
    }
}
