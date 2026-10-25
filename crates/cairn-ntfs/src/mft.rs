// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! MFT Record parsing and entry generation.

use crate::attribute::{parse_attributes, ParsedAttributes};
use crate::fixup::{apply_fixup, RECORD_FLAG_IN_USE, RECORD_FLAG_IS_DIR};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum MftError {
    #[error("Fixup error: {0}")]
    Fixup(#[from] crate::fixup::FixupError),
    #[error("Attribute parse error: {0}")]
    Attribute(#[from] crate::attribute::AttributeParseError),
}

/// A parsed file or directory item extracted from an MFT record.
#[derive(Clone, Debug)]
pub struct ExtractedItem {
    pub frn: u64,
    pub parent_frn: u64,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub created_ts: i64,
    pub modified_ts: i64,
    pub flags: u16,
    pub is_reparse_point: bool,
}

/// Parses an MFT record buffer (e.g. 1024 bytes), applying USA fixup and returning
/// all extracted items (one per valid non-DOS $FILE_NAME, supporting hard links).
pub fn parse_mft_record(
    record_buf: &mut [u8],
    sector_size: usize,
    default_record_num: u64,
) -> Result<Vec<ExtractedItem>, MftError> {
    // 1. Apply USA fixup
    apply_fixup(record_buf, sector_size)?;

    let flags = u16::from_le_bytes([record_buf[0x16], record_buf[0x17]]);
    let in_use = (flags & RECORD_FLAG_IN_USE) != 0;
    if !in_use {
        return Ok(Vec::new()); // Skip unallocated records
    }

    let is_dir = (flags & RECORD_FLAG_IS_DIR) != 0;

    // Record number: offset 0x2C in NTFS 3.1, or fall back to default_record_num
    let record_num = if record_buf.len() >= 48 {
        let rec_num_32 = u32::from_le_bytes([
            record_buf[0x2C],
            record_buf[0x2D],
            record_buf[0x2E],
            record_buf[0x2F],
        ]);
        if rec_num_32 != 0 {
            rec_num_32 as u64
        } else {
            default_record_num
        }
    } else {
        default_record_num
    };

    let sequence_number = u16::from_le_bytes([record_buf[0x10], record_buf[0x11]]) as u64;
    let frn = (sequence_number << 48) | (record_num & 0x0000FFFFFFFFFFFF);

    let first_attr_offset = u16::from_le_bytes([record_buf[0x14], record_buf[0x15]]) as usize;
    let attrs: ParsedAttributes = parse_attributes(record_buf, first_attr_offset)?;

    let size = if is_dir {
        0
    } else {
        attrs.default_data_size.unwrap_or(0)
    };

    let mut items = Vec::with_capacity(attrs.file_names.len());

    for fn_attr in attrs.file_names {
        let mut core_flags = 0u16;
        if is_dir {
            core_flags |= cairn_core::FLAG_DIRECTORY;
        }
        if attrs.has_reparse_point {
            core_flags |= cairn_core::FLAG_REPARSE_POINT;
        }

        // Prefer timestamps from $STANDARD_INFORMATION, falling back to $FILE_NAME
        let created_ts = attrs.created_ts.unwrap_or(fn_attr.created_ts);
        let modified_ts = attrs.modified_ts.unwrap_or(fn_attr.modified_ts);

        items.push(ExtractedItem {
            frn,
            parent_frn: fn_attr.parent_frn,
            name: fn_attr.name,
            is_dir,
            size,
            created_ts,
            modified_ts,
            flags: core_flags,
            is_reparse_point: attrs.has_reparse_point,
        });
    }

    Ok(items)
}
