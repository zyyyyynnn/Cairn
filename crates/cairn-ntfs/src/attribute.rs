// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! NTFS MFT record attribute structures and parser.

use thiserror::Error;

pub const ATTR_STANDARD_INFORMATION: u32 = 0x10;
pub const ATTR_ATTRIBUTE_LIST: u32 = 0x20;
pub const ATTR_FILE_NAME: u32 = 0x30;
pub const ATTR_DATA: u32 = 0x80;
pub const ATTR_BITMAP: u32 = 0xB0;
pub const ATTR_REPARSE_POINT: u32 = 0xC0;

pub const NAMESPACE_POSIX: u8 = 0x00;
pub const NAMESPACE_WIN32: u8 = 0x01;
pub const NAMESPACE_DOS: u8 = 0x02;
pub const NAMESPACE_WIN32_AND_DOS: u8 = 0x03;

#[derive(Error, Debug)]
pub enum AttributeParseError {
    #[error("Buffer too small for attribute header: {0} bytes")]
    BufferTooSmall(usize),
    #[error("Attribute length is zero")]
    ZeroLength,
}

#[derive(Clone, Debug)]
pub struct ParsedFileName {
    pub parent_frn: u64,
    pub name: String,
    pub created_ts: i64,
    pub modified_ts: i64,
    pub flags: u32,
    pub namespace: u8,
}

#[derive(Clone, Debug, Default)]
pub struct ParsedAttributes {
    pub file_names: Vec<ParsedFileName>,
    pub default_data_size: Option<u64>,
    pub created_ts: Option<i64>,
    pub modified_ts: Option<i64>,
    pub dos_flags: Option<u32>,
    pub has_reparse_point: bool,
    pub data_runlist_offset: Option<usize>,
}

/// Parses all attributes inside an MFT record starting from `first_attr_offset`.
pub fn parse_attributes(
    buf: &[u8],
    first_attr_offset: usize,
) -> Result<ParsedAttributes, AttributeParseError> {
    let mut result = ParsedAttributes::default();
    let mut cursor = first_attr_offset;

    while cursor + 8 <= buf.len() {
        let type_id = u32::from_le_bytes([
            buf[cursor],
            buf[cursor + 1],
            buf[cursor + 2],
            buf[cursor + 3],
        ]);

        if type_id == 0xFFFFFFFF {
            break; // 0xFFFFFFFF marks end of attributes
        }

        let attr_len = u32::from_le_bytes([
            buf[cursor + 4],
            buf[cursor + 5],
            buf[cursor + 6],
            buf[cursor + 7],
        ]) as usize;

        if attr_len == 0 || cursor + attr_len > buf.len() {
            break;
        }

        let non_resident = buf[cursor + 8] != 0;
        let name_len = buf[cursor + 9] as usize;

        match type_id {
            ATTR_STANDARD_INFORMATION => {
                if !non_resident && cursor + 24 <= buf.len() {
                    let val_offset =
                        u16::from_le_bytes([buf[cursor + 20], buf[cursor + 21]]) as usize;
                    let val_start = cursor + val_offset;
                    if val_start + 48 <= cursor + attr_len && val_start + 48 <= buf.len() {
                        let created =
                            i64::from_le_bytes(buf[val_start..val_start + 8].try_into().unwrap());
                        let modified = i64::from_le_bytes(
                            buf[val_start + 8..val_start + 16].try_into().unwrap(),
                        );
                        let dos_flags = u32::from_le_bytes(
                            buf[val_start + 32..val_start + 36].try_into().unwrap(),
                        );

                        result.created_ts = Some(created);
                        result.modified_ts = Some(modified);
                        result.dos_flags = Some(dos_flags);
                    }
                }
            }
            ATTR_FILE_NAME => {
                if !non_resident && cursor + 24 <= buf.len() {
                    let val_offset =
                        u16::from_le_bytes([buf[cursor + 20], buf[cursor + 21]]) as usize;
                    let val_start = cursor + val_offset;
                    if val_start + 66 <= cursor + attr_len && val_start + 66 <= buf.len() {
                        let parent_frn =
                            u64::from_le_bytes(buf[val_start..val_start + 8].try_into().unwrap());
                        let created = i64::from_le_bytes(
                            buf[val_start + 8..val_start + 16].try_into().unwrap(),
                        );
                        let modified = i64::from_le_bytes(
                            buf[val_start + 16..val_start + 24].try_into().unwrap(),
                        );
                        let flags = u32::from_le_bytes(
                            buf[val_start + 56..val_start + 60].try_into().unwrap(),
                        );
                        let fn_len = buf[val_start + 64] as usize;
                        let namespace = buf[val_start + 65];

                        // Skip pure DOS 8.3 short names (NAMESPACE_DOS = 0x02)
                        if namespace != NAMESPACE_DOS {
                            let name_bytes_start = val_start + 66;
                            let name_bytes_end = name_bytes_start + fn_len * 2;

                            if name_bytes_end <= cursor + attr_len && name_bytes_end <= buf.len() {
                                let u16_chars: Vec<u16> = buf[name_bytes_start..name_bytes_end]
                                    .chunks_exact(2)
                                    .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                                    .collect();

                                let name_str = String::from_utf16_lossy(&u16_chars);
                                result.file_names.push(ParsedFileName {
                                    parent_frn,
                                    name: name_str,
                                    created_ts: created,
                                    modified_ts: modified,
                                    flags,
                                    namespace,
                                });
                            }
                        }
                    }
                }
            }
            ATTR_DATA => {
                // Only consider default unnamed stream (name_len == 0)
                if name_len == 0 {
                    if !non_resident {
                        // Resident $DATA
                        if cursor + 20 <= buf.len() {
                            let val_len = u32::from_le_bytes([
                                buf[cursor + 16],
                                buf[cursor + 17],
                                buf[cursor + 18],
                                buf[cursor + 19],
                            ]) as u64;
                            result.default_data_size = Some(val_len);
                        }
                    } else {
                        // Non-resident $DATA
                        if cursor + 56 <= buf.len() {
                            let real_size = u64::from_le_bytes([
                                buf[cursor + 48],
                                buf[cursor + 49],
                                buf[cursor + 50],
                                buf[cursor + 51],
                                buf[cursor + 52],
                                buf[cursor + 53],
                                buf[cursor + 54],
                                buf[cursor + 55],
                            ]);
                            result.default_data_size = Some(real_size);
                            result.data_runlist_offset = Some(cursor);
                        }
                    }
                }
            }
            ATTR_REPARSE_POINT => {
                result.has_reparse_point = true;
            }
            _ => {}
        }

        cursor += attr_len;
    }

    Ok(result)
}
