// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! NTFS MFT record attribute structures and parser.

use thiserror::Error;

pub const ATTR_STANDARD_INFORMATION: u32 = 0x10;
pub const ATTR_FILE_NAME: u32 = 0x30;
pub const ATTR_DATA: u32 = 0x80;
pub const ATTR_BITMAP: u32 = 0xB0;
pub const ATTR_REPARSE_POINT: u32 = 0xC0;

pub const NAMESPACE_DOS: u8 = 0x02;

#[derive(Error, Debug)]
pub enum AttributeParseError {
    #[error("Buffer too small for attribute header: {0} bytes")]
    BufferTooSmall(usize),
}

#[derive(Clone, Debug)]
pub struct ParsedFileName {
    pub parent_frn: u64,
    pub name: String,
    pub created_ts: i64,
    pub modified_ts: i64,
    pub flags: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NonResidentAttrSpan {
    pub attr_offset: usize,
    pub runlist_offset: usize,
    pub attr_len: usize,
    pub real_size: u64,
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
    pub bitmap_resident: Option<Vec<u8>>,
    pub bitmap_non_resident: Option<NonResidentAttrSpan>,
}

/// Parses all attributes inside an MFT record starting from `first_attr_offset`.
pub fn parse_attributes(
    buf: &[u8],
    first_attr_offset: usize,
) -> Result<ParsedAttributes, AttributeParseError> {
    let mut result = ParsedAttributes::default();
    let mut cursor = first_attr_offset;

    while cursor
        .checked_add(8)
        .is_some_and(|head_end| head_end <= buf.len())
    {
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

        let Some(attr_end) = cursor.checked_add(attr_len) else {
            break;
        };
        if attr_len < 16 || attr_end > buf.len() {
            break;
        }

        let non_resident = buf[cursor + 8] != 0;
        let name_len = buf[cursor + 9] as usize;

        match type_id {
            ATTR_STANDARD_INFORMATION => {
                if !non_resident && cursor + 24 <= attr_end {
                    let val_offset =
                        u16::from_le_bytes([buf[cursor + 20], buf[cursor + 21]]) as usize;
                    if let Some(val_start) = cursor.checked_add(val_offset)
                        && val_start.checked_add(48).is_some_and(|end| end <= attr_end)
                    {
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
                if !non_resident && cursor + 24 <= attr_end {
                    let val_offset =
                        u16::from_le_bytes([buf[cursor + 20], buf[cursor + 21]]) as usize;
                    if let Some(val_start) = cursor.checked_add(val_offset)
                        && val_start.checked_add(66).is_some_and(|end| end <= attr_end)
                    {
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
                        if namespace != NAMESPACE_DOS
                            && let Some(name_bytes_len) = fn_len.checked_mul(2)
                        {
                            let name_bytes_start = val_start + 66;
                            if let Some(name_bytes_end) =
                                name_bytes_start.checked_add(name_bytes_len)
                                && name_bytes_end <= attr_end
                            {
                                let u16_chars: Vec<u16> = buf[name_bytes_start..name_bytes_end]
                                    .as_chunks::<2>()
                                    .0
                                    .iter()
                                    .map(|&pair| u16::from_le_bytes(pair))
                                    .collect();

                                let name_str = String::from_utf16_lossy(&u16_chars);
                                result.file_names.push(ParsedFileName {
                                    parent_frn,
                                    name: name_str,
                                    created_ts: created,
                                    modified_ts: modified,
                                    flags,
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
                        if cursor + 24 <= attr_end {
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
                        if cursor + 56 <= attr_end {
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
            ATTR_BITMAP => {
                if name_len == 0 {
                    if !non_resident && cursor + 24 <= attr_end {
                        let val_len =
                            u32::from_le_bytes(buf[cursor + 16..cursor + 20].try_into().unwrap())
                                as usize;
                        let val_offset =
                            u16::from_le_bytes([buf[cursor + 20], buf[cursor + 21]]) as usize;
                        if let Some(val_start) = cursor.checked_add(val_offset)
                            && let Some(val_end) = val_start.checked_add(val_len)
                            && val_end <= attr_end
                        {
                            result.bitmap_resident = Some(buf[val_start..val_end].to_vec());
                        }
                    } else if non_resident && cursor + 56 <= attr_end {
                        let runlist_offset =
                            u16::from_le_bytes([buf[cursor + 32], buf[cursor + 33]]) as usize;
                        if runlist_offset < attr_len {
                            let real_size = u64::from_le_bytes(
                                buf[cursor + 48..cursor + 56].try_into().unwrap(),
                            );
                            result.bitmap_non_resident = Some(NonResidentAttrSpan {
                                attr_offset: cursor,
                                runlist_offset,
                                attr_len,
                                real_size,
                            });
                        }
                    }
                }
            }
            ATTR_REPARSE_POINT => {
                result.has_reparse_point = true;
            }
            _ => {}
        }

        cursor = attr_end;
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_attributes_ignores_corrupt_length_and_out_of_bounds_value_spans() {
        // Attribute length overflows buf.len() -> stops cleanly without panic.
        let mut buf = vec![0u8; 64];
        buf[0..4].copy_from_slice(&ATTR_STANDARD_INFORMATION.to_le_bytes());
        buf[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_attributes(&buf, 0).is_ok());

        // Resident $FILE_NAME with oversized fn_len that overruns attr_len must be skipped safely.
        let mut fn_buf = vec![0u8; 96];
        fn_buf[0..4].copy_from_slice(&ATTR_FILE_NAME.to_le_bytes());
        fn_buf[4..8].copy_from_slice(&92u32.to_le_bytes());
        fn_buf[20..22].copy_from_slice(&24u16.to_le_bytes());
        fn_buf[24 + 64] = 250; // 250 * 2 = 500 bytes, well past attr_end
        let parsed = parse_attributes(&fn_buf, 0).unwrap();
        assert!(parsed.file_names.is_empty());
    }
}
