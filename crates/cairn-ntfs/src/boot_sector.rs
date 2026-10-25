// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! NTFS Boot Sector parser (Sector 0 of the NTFS volume).

use thiserror::Error;

#[derive(Error, Debug)]
pub enum BootSectorError {
    #[error("Sector too small: expected at least 512 bytes, got {0}")]
    SectorTooSmall(usize),
    #[error("Invalid NTFS OEM ID signature: {0:?}")]
    InvalidOemId([u8; 8]),
}

/// Key geometry and layout parameters parsed from Sector 0 of an NTFS volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NtfsBootSector {
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub mft_record_size: u32,
    pub mft_start_lcn: i64,
    pub volume_serial_number: u64,
}

impl NtfsBootSector {
    /// Parses the NTFS Boot Sector from sector 0 raw bytes.
    pub fn parse(buf: &[u8]) -> Result<Self, BootSectorError> {
        if buf.len() < 512 {
            return Err(BootSectorError::SectorTooSmall(buf.len()));
        }

        // OEM ID must be "NTFS    " (offset 0x03)
        let oem_id = &buf[3..11];
        if oem_id != b"NTFS    " {
            let mut arr = [0u8; 8];
            arr.copy_from_slice(oem_id);
            return Err(BootSectorError::InvalidOemId(arr));
        }

        let bytes_per_sector = u16::from_le_bytes([buf[0x0B], buf[0x0C]]);
        let sectors_per_cluster = buf[0x0D];

        let mft_start_lcn = i64::from_le_bytes([
            buf[0x30], buf[0x31], buf[0x32], buf[0x33], buf[0x34], buf[0x35], buf[0x36], buf[0x37],
        ]);

        let clusters_per_record = buf[0x40] as i8;
        let mft_record_size = if clusters_per_record > 0 {
            (clusters_per_record as u32) * (sectors_per_cluster as u32) * (bytes_per_sector as u32)
        } else {
            // Negative value represents 2^|n| bytes (e.g. -10 -> 2^10 = 1024 bytes)
            1u32 << (-clusters_per_record as u32)
        };

        let volume_serial_number = u64::from_le_bytes([
            buf[0x48], buf[0x49], buf[0x4A], buf[0x4B], buf[0x4C], buf[0x4D], buf[0x4E], buf[0x4F],
        ]);

        Ok(Self {
            bytes_per_sector,
            sectors_per_cluster,
            mft_record_size,
            mft_start_lcn,
            volume_serial_number,
        })
    }

    #[inline(always)]
    pub fn bytes_per_cluster(&self) -> u64 {
        (self.bytes_per_sector as u64) * (self.sectors_per_cluster as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ntfs_boot_sector() {
        let mut buf = vec![0u8; 512];
        buf[3..11].copy_from_slice(b"NTFS    ");
        buf[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
        buf[0x0D] = 8; // 8 * 512 = 4096 bytes per cluster
        buf[0x40] = 0xF6; // -10 in i8 -> 1024 bytes per record
        buf[0x30..0x38].copy_from_slice(&786432i64.to_le_bytes());
        buf[0x48..0x50].copy_from_slice(&0x1234567890ABCDEFu64.to_le_bytes());

        let bs = NtfsBootSector::parse(&buf).expect("should parse successfully");
        assert_eq!(bs.bytes_per_sector, 512);
        assert_eq!(bs.sectors_per_cluster, 8);
        assert_eq!(bs.mft_record_size, 1024);
        assert_eq!(bs.mft_start_lcn, 786432);
        assert_eq!(bs.volume_serial_number, 0x1234567890ABCDEF);
        assert_eq!(bs.bytes_per_cluster(), 4096);
    }
}
