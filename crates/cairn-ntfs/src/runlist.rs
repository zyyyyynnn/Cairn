// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! NTFS Non-Resident Data Runs (Runlist) decoder.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum RunlistError {
    #[error("Unexpected end of runlist at offset {0}")]
    UnexpectedEnd(usize),
    #[error("Invalid field size: len_bytes {len_bytes}, offset_bytes {offset_bytes}")]
    InvalidFieldSize { len_bytes: u8, offset_bytes: u8 },
    #[error("Invalid cluster count or overflowing LCN offset in runlist")]
    InvalidOffset,
}

/// A contiguous chunk of clusters on the physical volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataRun {
    /// Starting Logical Cluster Number (LCN), or None if sparse run.
    pub lcn: Option<i64>,
    /// Number of contiguous clusters in this run.
    pub cluster_count: u64,
}

/// Parses an NTFS Data Runlist from raw attribute bytes.
pub fn parse_runlist(bytes: &[u8], mut current_lcn: i64) -> Result<Vec<DataRun>, RunlistError> {
    let mut runs = Vec::new();
    let mut cursor = 0usize;

    while cursor < bytes.len() {
        let header = bytes[cursor];
        cursor += 1;

        if header == 0 {
            break; // 0x00 marks end of runlist
        }

        let len_bytes = (header & 0x0F) as usize;
        let offset_bytes = ((header >> 4) & 0x0F) as usize;

        if len_bytes == 0 || len_bytes > 8 || offset_bytes > 8 {
            return Err(RunlistError::InvalidFieldSize {
                len_bytes: len_bytes as u8,
                offset_bytes: offset_bytes as u8,
            });
        }

        let Some(end) = cursor
            .checked_add(len_bytes)
            .and_then(|pos| pos.checked_add(offset_bytes))
        else {
            return Err(RunlistError::UnexpectedEnd(cursor));
        };
        if end > bytes.len() {
            return Err(RunlistError::UnexpectedEnd(cursor));
        }

        // Read cluster count (unsigned LE)
        let mut cluster_count = 0u64;
        for i in 0..len_bytes {
            cluster_count |= (bytes[cursor + i] as u64) << (i * 8);
        }
        if cluster_count == 0 {
            return Err(RunlistError::InvalidOffset);
        }
        cursor += len_bytes;

        // Read LCN delta (signed LE, two's complement)
        let run_lcn = if offset_bytes == 0 {
            None // Sparse run
        } else {
            let mut delta = 0i64;
            for i in 0..offset_bytes {
                delta |= (bytes[cursor + i] as i64) << (i * 8);
            }
            let shift = offset_bytes * 8;
            if shift < 64 {
                let sign_bit = 1i64 << (shift - 1);
                if (delta & sign_bit) != 0 {
                    delta |= !0i64 << shift;
                }
            }
            cursor += offset_bytes;

            current_lcn = current_lcn
                .checked_add(delta)
                .ok_or(RunlistError::InvalidOffset)?;
            if current_lcn < 0 {
                return Err(RunlistError::InvalidOffset);
            }
            Some(current_lcn)
        };

        runs.push(DataRun {
            lcn: run_lcn,
            cluster_count,
        });
    }

    Ok(runs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_runlist() {
        // Run 1: len_bytes=1, offset_bytes=2 (0x21). Length=4, offset=16 -> LCN 16
        // Run 2: len_bytes=1, offset_bytes=2 (0x21). Length=8, offset=32 -> LCN 48
        // End: 0x00
        let bytes = vec![0x21, 0x04, 0x10, 0x00, 0x21, 0x08, 0x20, 0x00, 0x00];
        let runs = parse_runlist(&bytes, 0).expect("runlist should parse");
        assert_eq!(runs.len(), 2);
        assert_eq!(
            runs[0],
            DataRun {
                lcn: Some(16),
                cluster_count: 4
            }
        );
        assert_eq!(
            runs[1],
            DataRun {
                lcn: Some(48),
                cluster_count: 8
            }
        );
    }

    #[test]
    fn parse_runlist_rejects_zero_clusters_8byte_overflow_and_negative_lcn() {
        // Zero cluster count
        assert!(parse_runlist(&[0x11, 0x00, 0x04, 0x00], 0).is_err());
        // 8-byte offset shift must not panic; negative LCN result must return Err
        let neg_delta = [
            0x81, 0x04, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00,
        ];
        assert!(parse_runlist(&neg_delta, 0).is_err());
        // Signed overflow past i64::MAX
        let pos_overflow = [
            0x81, 0x02, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert!(parse_runlist(&pos_overflow, i64::MAX).is_err());
        // Oversized field count (> 8)
        assert!(parse_runlist(&[0x91, 0x01, 0x01], 0).is_err());
    }
}
