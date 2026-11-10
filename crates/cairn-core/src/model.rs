// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Compact memory representation for file records.

pub const FLAG_DIRECTORY: u16 = 1 << 0;
pub const FLAG_TOMBSTONE: u16 = 1 << 1;
pub const FLAG_HIDDEN: u16 = 1 << 3;
pub const FLAG_SYSTEM: u16 = 1 << 4;
pub const FLAG_ARCHIVE: u16 = 1 << 5;
pub const FLAG_COMPRESSED: u16 = 1 << 6;
pub const FLAG_SPARSE: u16 = 1 << 7;
pub const FLAG_REPARSE_POINT: u16 = 1 << 8;

use serde::{Deserialize, Serialize};

/// Fixed-size compact record for an indexed file or directory.
/// Exactly 48 bytes with 8-byte alignment.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    /// File Reference Number: low 48 bits MFT Record Number, high 16 bits Sequence Number.
    pub frn: u64,
    /// Parent directory index in the volume's records array.
    /// Sentinel `u32::MAX` represents root or detached item.
    pub parent_idx: u32,
    /// Byte offset within the NamePool arena.
    pub name_offset: u32,
    /// Byte length of the UTF-8 file name.
    pub name_len: u16,
    /// Relative offset of the file extension (after the last '.') or `u16::MAX` if none.
    pub ext_offset: u16,
    /// Zero-based offset of the drive letter that hosts this record (0 = A:, 2 = C:, 3 = D:).
    pub volume_id: u16,
    /// Attributes and status flags (directory, tombstone, hidden, etc.).
    pub flags: u16,
    /// Logical size of the default data stream for a file; for a directory, the recursive size of
    /// its live indexed descendants.
    pub size: u64,
    /// Last modification timestamp: Windows FILETIME ticks or Unix seconds; read it through
    /// [`crate::time::timestamp_to_unix_seconds`].
    pub modified_ts: i64,
    /// Creation timestamp: Windows FILETIME ticks or Unix seconds; read it through
    /// [`crate::time::timestamp_to_unix_seconds`].
    pub created_ts: i64,
}

impl FileRecord {
    pub const ROOT_PARENT: u32 = u32::MAX;
    pub const NO_EXT: u16 = u16::MAX;

    #[inline(always)]
    pub fn is_dir(&self) -> bool {
        (self.flags & FLAG_DIRECTORY) != 0
    }

    #[inline(always)]
    pub fn is_file(&self) -> bool {
        (self.flags & FLAG_DIRECTORY) == 0
    }

    #[inline(always)]
    pub fn is_tombstone(&self) -> bool {
        (self.flags & FLAG_TOMBSTONE) != 0
    }

    #[inline(always)]
    pub fn is_hidden(&self) -> bool {
        (self.flags & FLAG_HIDDEN) != 0
    }

    #[inline(always)]
    pub fn is_system(&self) -> bool {
        (self.flags & FLAG_SYSTEM) != 0
    }

    #[inline(always)]
    pub fn is_reparse_point(&self) -> bool {
        (self.flags & FLAG_REPARSE_POINT) != 0
    }

    #[inline(always)]
    pub fn has_ext(&self) -> bool {
        self.ext_offset != Self::NO_EXT && (self.ext_offset as usize) < (self.name_len as usize)
    }

    #[inline(always)]
    pub fn mark_tombstone(&mut self) {
        self.flags |= FLAG_TOMBSTONE;
    }

    /// Encodes a drive letter (`A`..=`Z`, case-insensitive) into a zero-based `volume_id` (`0`..`26`).
    #[inline(always)]
    pub const fn volume_id_from_drive(drive: char) -> Option<u16> {
        let upper = drive.to_ascii_uppercase();
        if upper.is_ascii_uppercase() {
            Some((upper as u8 - b'A') as u16)
        } else {
            None
        }
    }

    /// Decodes a zero-based `volume_id` (`0`..`26`) back into an uppercase ASCII drive letter (`A`..=`Z`).
    #[inline(always)]
    pub const fn drive_from_volume_id(volume_id: u16) -> Option<char> {
        if volume_id < 26 {
            Some((b'A' + volume_id as u8) as char)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FileRecord;

    #[test]
    fn volume_id_roundtrips_every_ascii_drive_letter() {
        for (idx, letter) in ('A'..='Z').enumerate() {
            let id = idx as u16;
            assert_eq!(FileRecord::volume_id_from_drive(letter), Some(id));
            assert_eq!(
                FileRecord::volume_id_from_drive(letter.to_ascii_lowercase()),
                Some(id)
            );
            assert_eq!(FileRecord::drive_from_volume_id(id), Some(letter));
        }
        assert_eq!(FileRecord::volume_id_from_drive('1'), None);
        assert_eq!(FileRecord::volume_id_from_drive('?'), None);
        assert_eq!(FileRecord::drive_from_volume_id(26), None);
        assert_eq!(FileRecord::drive_from_volume_id(u16::MAX), None);
    }
}
