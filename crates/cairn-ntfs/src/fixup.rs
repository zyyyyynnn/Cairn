// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Update Sequence Array (USA) Fixup validation and restoration for MFT records.

use thiserror::Error;

pub const FILE_SIGNATURE: &[u8; 4] = b"FILE";
pub const RECORD_FLAG_IN_USE: u16 = 0x0001;
pub const RECORD_FLAG_IS_DIR: u16 = 0x0002;

#[derive(Error, Debug)]
pub enum FixupError {
    #[error("Buffer too small for record header: {0} bytes")]
    BufferTooSmall(usize),
    #[error("Invalid record magic signature: expected FILE, found {0:?}")]
    InvalidSignature([u8; 4]),
    #[error("USA array out of bounds (offset {offset}, count {count})")]
    UsaOutOfBounds { offset: usize, count: usize },
    #[error(
        "Sector {sector_idx} fixup mismatch: expected USN {expected:#06x}, found {actual:#06x}"
    )]
    FixupMismatch {
        sector_idx: usize,
        expected: u16,
        actual: u16,
    },
}

/// Validates the `FILE` signature and restores sector-end bytes from the Update Sequence Array (USA).
///
/// Modifies `buf` in-place to restore original bytes before disk write.
pub fn apply_fixup(buf: &mut [u8], sector_size: usize) -> Result<(), FixupError> {
    if buf.len() < 48 {
        return Err(FixupError::BufferTooSmall(buf.len()));
    }

    if &buf[0..4] != FILE_SIGNATURE {
        let mut sig = [0u8; 4];
        sig.copy_from_slice(&buf[0..4]);
        return Err(FixupError::InvalidSignature(sig));
    }

    let usa_offset = u16::from_le_bytes([buf[0x04], buf[0x05]]) as usize;
    let usa_count = u16::from_le_bytes([buf[0x06], buf[0x07]]) as usize;

    if usa_offset + usa_count * 2 > buf.len() || usa_count < 1 {
        return Err(FixupError::UsaOutOfBounds {
            offset: usa_offset,
            count: usa_count,
        });
    }

    let expected_usn = u16::from_le_bytes([buf[usa_offset], buf[usa_offset + 1]]);
    let num_sectors = usa_count - 1;

    for i in 1..=num_sectors {
        let sector_end = i * sector_size;
        if sector_end > buf.len() {
            break;
        }

        let fixup_pos = sector_end - 2;
        let actual_usn = u16::from_le_bytes([buf[fixup_pos], buf[fixup_pos + 1]]);

        if actual_usn != expected_usn {
            return Err(FixupError::FixupMismatch {
                sector_idx: i,
                expected: expected_usn,
                actual: actual_usn,
            });
        }

        // Restore original bytes from USA
        let saved_pos = usa_offset + i * 2;
        buf[fixup_pos] = buf[saved_pos];
        buf[fixup_pos + 1] = buf[saved_pos + 1];
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_fixup_success() {
        let mut record = vec![0u8; 1024];
        record[0..4].copy_from_slice(b"FILE");
        record[4..6].copy_from_slice(&48u16.to_le_bytes()); // usa_offset = 48
        record[6..8].copy_from_slice(&3u16.to_le_bytes()); // usa_count = 3 (1 USN + 2 sectors)

        let usn: u16 = 0x4242;
        record[48..50].copy_from_slice(&usn.to_le_bytes()); // Update sequence number
        record[50..52].copy_from_slice(&[0x11, 0x22]); // original sector 1 end
        record[52..54].copy_from_slice(&[0x33, 0x44]); // original sector 2 end

        // Put USN at end of sector 1 (510..512) and sector 2 (1022..1024)
        record[510..512].copy_from_slice(&usn.to_le_bytes());
        record[1022..1024].copy_from_slice(&usn.to_le_bytes());

        apply_fixup(&mut record, 512).expect("fixup should succeed");

        assert_eq!(&record[510..512], &[0x11, 0x22]);
        assert_eq!(&record[1022..1024], &[0x33, 0x44]);
    }
}
