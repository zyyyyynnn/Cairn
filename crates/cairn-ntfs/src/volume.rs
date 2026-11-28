// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Volume access, sector reading, and MFT stream parsing.

use crate::mft::ExtractedItem;
use crate::mft_reader::{BlockReader, MftScanOptions, scan_mft_from_reader};
use thiserror::Error;

#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE};
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_BEGIN, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile,
    SetFilePointerEx,
};
#[cfg(windows)]
use windows::core::PCWSTR;

#[derive(Error, Debug)]
pub enum VolumeError {
    #[error("Failed to open volume {path}: Win32 error {code:#x}")]
    OpenFailed { path: String, code: u32 },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Boot sector error: {0}")]
    BootSector(#[from] crate::boot_sector::BootSectorError),
    #[error("Runlist error: {0}")]
    Runlist(#[from] crate::runlist::RunlistError),
    #[error("MFT error: {0}")]
    Mft(#[from] crate::mft::MftError),
    #[error("Fixup error: {0}")]
    Fixup(#[from] crate::fixup::FixupError),
    #[error("Attribute parse error: {0}")]
    Attribute(#[from] crate::attribute::AttributeParseError),
}

/// Enumerates all local fixed NTFS drive letters (e.g. ['C', 'D']).
pub fn enumerate_fixed_drives() -> Vec<char> {
    crate::volume_identity::enumerate_volumes()
        .into_iter()
        .filter_map(|vol| vol.drive_letter)
        .collect()
}

/// An open raw volume device handle; the stored path has no trailing separator.
#[cfg(windows)]
pub struct RawVolumeHandle {
    handle: HANDLE,
    pub path: String,
}

#[cfg(windows)]
impl RawVolumeHandle {
    /// Opens a drive root handle such as `\\.\C:`.
    pub fn open(drive_letter: char) -> Result<Self, VolumeError> {
        let dev_path = format!(r"\\.\{}:", drive_letter.to_ascii_uppercase());
        Self::open_volume(&dev_path)
    }

    /// Opens a raw volume handle from a device path or Win32 Volume GUID (`\\?\Volume{...}`).
    pub fn open_volume(volume_path: &str) -> Result<Self, VolumeError> {
        // Raw volume CreateFileW handles reject trailing backslashes on `\\?\Volume{...}\`.
        let trimmed = volume_path.trim_end_matches(['\\', '/']);
        let dev_path = trimmed.to_string();
        let wide_path: Vec<u16> = dev_path.encode_utf16().chain(std::iter::once(0)).collect();

        // SAFETY: CreateFileW called with explicitly permitted sharing flags and valid null-terminated string.
        let handle_res = unsafe {
            CreateFileW(
                PCWSTR(wide_path.as_ptr()),
                GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(FILE_FLAG_BACKUP_SEMANTICS.0),
                None,
            )
        };

        match handle_res {
            Ok(handle) if handle != INVALID_HANDLE_VALUE => Ok(Self {
                handle,
                path: dev_path,
            }),
            Ok(_) | Err(_) => {
                // SAFETY: GetLastError retrieves thread's last Win32 error code.
                let err = unsafe { windows::Win32::Foundation::GetLastError() };
                Err(VolumeError::OpenFailed {
                    path: dev_path,
                    code: err.0,
                })
            }
        }
    }

    /// Reads raw bytes at `offset` using `SetFilePointerEx` and `ReadFile`.
    pub fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        let signed_offset = i64::try_from(offset).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "raw volume read offset exceeds i64::MAX",
            )
        })?;
        let mut moved_offset = 0i64;
        // SAFETY: SetFilePointerEx moves file pointer to specified 64-bit physical offset.
        let seek_ok = unsafe {
            SetFilePointerEx(
                self.handle,
                signed_offset,
                Some(&mut moved_offset),
                FILE_BEGIN,
            )
        };
        if seek_ok.is_err() {
            return Err(std::io::Error::last_os_error());
        }

        let mut bytes_read = 0u32;
        // SAFETY: ReadFile takes a writable slice and a pointer to the byte counter.
        let read_ok = unsafe { ReadFile(self.handle, Some(buf), Some(&mut bytes_read), None) };
        if read_ok.is_err() {
            return Err(std::io::Error::last_os_error());
        }

        Ok(bytes_read as usize)
    }

    /// Queries the volume's USN journal metadata (`FSCTL_QUERY_USN_JOURNAL`).
    pub fn query_usn_journal(&self) -> Result<crate::usn::UsnJournalData, crate::usn::UsnError> {
        let mut out_buf = [0u8; 80];
        let mut bytes_returned = 0u32;
        // SAFETY: DeviceIoControl with FSCTL_QUERY_USN_JOURNAL on opened volume handle.
        let res = unsafe {
            windows::Win32::System::IO::DeviceIoControl(
                self.handle,
                crate::usn::FSCTL_QUERY_USN_JOURNAL,
                None,
                0,
                Some(out_buf.as_mut_ptr() as *mut _),
                out_buf.len() as u32,
                Some(&mut bytes_returned),
                None,
            )
        };
        if res.is_err() {
            // SAFETY: GetLastError reads thread-local error state set by the failing DeviceIoControl call.
            let err = unsafe { windows::Win32::Foundation::GetLastError() };
            return Err(crate::usn::UsnError::QueryFailed(err.0));
        }
        crate::usn::UsnJournalData::parse(&out_buf[..bytes_returned as usize])
    }

    /// Reads USN change journal records starting from `start_usn` (`FSCTL_READ_USN_JOURNAL`).
    pub fn read_usn_journal(
        &self,
        journal_id: u64,
        start_usn: i64,
        out_buf: &mut [u8],
    ) -> Result<(i64, Vec<crate::usn::UsnChange>), crate::usn::UsnError> {
        let in_data = crate::usn::ReadUsnJournalDataV0 {
            start_usn,
            reason_mask: 0xFFFFFFFF,
            return_only_on_close: 0,
            timeout: 0,
            bytes_to_wait_for: 0,
            usn_journal_id: journal_id,
        };
        let mut bytes_returned = 0u32;
        // SAFETY: DeviceIoControl with FSCTL_READ_USN_JOURNAL on opened volume handle.
        let res = unsafe {
            windows::Win32::System::IO::DeviceIoControl(
                self.handle,
                crate::usn::FSCTL_READ_USN_JOURNAL,
                Some(&in_data as *const _ as *const _),
                std::mem::size_of::<crate::usn::ReadUsnJournalDataV0>() as u32,
                Some(out_buf.as_mut_ptr() as *mut _),
                out_buf.len() as u32,
                Some(&mut bytes_returned),
                None,
            )
        };
        if res.is_err() {
            // SAFETY: GetLastError reads thread-local error state set by the failing DeviceIoControl call.
            let err = unsafe { windows::Win32::Foundation::GetLastError() };
            return Err(crate::usn::UsnError::ReadFailed(err.0));
        }
        crate::usn::parse_usn_records(&out_buf[..bytes_returned as usize])
    }
}

#[cfg(windows)]
impl Drop for RawVolumeHandle {
    fn drop(&mut self) {
        if self.handle != INVALID_HANDLE_VALUE {
            // SAFETY: CloseHandle closes the opened device handle.
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(windows)]
impl BlockReader for RawVolumeHandle {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        RawVolumeHandle::read_at(self, offset, buf)
    }
}

/// Scans a live NTFS volume by reading raw MFT clusters via `RawVolumeHandle`.
#[cfg(windows)]
pub fn scan_live_volume_mft(
    drive_letter: char,
    chunk_records: usize,
) -> Result<Vec<ExtractedItem>, VolumeError> {
    let mut raw_vol = RawVolumeHandle::open(drive_letter)?;
    let res = scan_mft_from_reader(
        &mut raw_vol,
        MftScanOptions::default().with_chunk_records(chunk_records),
    )?;
    Ok(res.items)
}

#[cfg(not(windows))]
pub fn scan_live_volume_mft(
    _drive_letter: char,
    _chunk_records: usize,
) -> Result<Vec<ExtractedItem>, VolumeError> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mft_reader::locate_mft_clusters_from_reader;

    struct MockReader<F>(F);

    impl<F> BlockReader for MockReader<F>
    where
        F: FnMut(u64, &mut [u8]) -> Result<usize, std::io::Error>,
    {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, std::io::Error> {
            (self.0)(offset, buf)
        }
    }

    #[test]
    fn test_locate_mft_clusters_synthetic() {
        // Synthetic NTFS Boot Sector (512 bytes/sector, 8 sectors/cluster, 1024-byte MFT records, LCN 4).
        let mut boot_sector = vec![0u8; 512];
        boot_sector[3..11].copy_from_slice(b"NTFS    ");
        boot_sector[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
        boot_sector[0x0D] = 8;
        boot_sector[0x40] = 0xF6;
        boot_sector[0x30..0x38].copy_from_slice(&4i64.to_le_bytes());
        boot_sector[0x48..0x50].copy_from_slice(&0x1122334455667788u64.to_le_bytes());

        // Synthetic Record 0 (1024 bytes) with USA fixup at 0x30 and non-resident $DATA runlist.
        let mut rec0 = vec![0u8; 1024];
        rec0[0..4].copy_from_slice(b"FILE");
        rec0[4..6].copy_from_slice(&48u16.to_le_bytes());
        rec0[6..8].copy_from_slice(&3u16.to_le_bytes());
        rec0[0x14..0x16].copy_from_slice(&56u16.to_le_bytes());
        rec0[0x16..0x18].copy_from_slice(&1u16.to_le_bytes());

        // USA check word at offset 48
        rec0[48..50].copy_from_slice(&[0xAA, 0x55]);
        // USA replacement words at offset 50..54
        rec0[50..52].copy_from_slice(&[0x11, 0x22]);
        rec0[52..54].copy_from_slice(&[0x33, 0x44]);

        // Place check word at sector boundaries: 510..512 and 1022..1024
        rec0[510..512].copy_from_slice(&[0xAA, 0x55]);
        rec0[1022..1024].copy_from_slice(&[0xAA, 0x55]);

        // At offset 56: Non-resident $DATA attribute
        // Attribute header (non-resident):
        // 0..4: Type (0x80)
        // 4..8: Length (72 = 0x48)
        // 8: Non-resident flag (1)
        // 9: Name length (0)
        // 10..12: Name offset (0)
        // 12..14: Flags (0)
        // 14..16: Attribute ID (1)
        // 16..24: Starting VCN (0)
        // 24..32: Ending VCN (3)
        // 32..34: Runlist offset (64 = 0x40)
        // 48..56: Allocated/Real size (16384 bytes)
        // 64..: Runlist [0x21, 0x04, 0x10, 0x00, 0x00] -> length 4, LCN 16
        let attr_start = 56;
        rec0[attr_start..attr_start + 4].copy_from_slice(&0x80u32.to_le_bytes());
        rec0[attr_start + 4..attr_start + 8].copy_from_slice(&72u32.to_le_bytes());
        rec0[attr_start + 8] = 1; // non-resident
        rec0[attr_start + 32..attr_start + 34].copy_from_slice(&64u16.to_le_bytes());
        rec0[attr_start + 48..attr_start + 56].copy_from_slice(&16384u64.to_le_bytes());
        let runlist = [0x21, 0x04, 0x10, 0x00, 0x00];
        rec0[attr_start + 64..attr_start + 64 + runlist.len()].copy_from_slice(&runlist);

        // Terminating attribute marker (0xFFFFFFFF)
        let term_offset = attr_start + 72;
        rec0[term_offset..term_offset + 4].copy_from_slice(&0xFFFFFFFFu32.to_le_bytes());

        let mock_read = |offset: u64, buf: &mut [u8]| -> Result<usize, std::io::Error> {
            if offset == 0 {
                let to_copy = buf.len().min(boot_sector.len());
                buf[..to_copy].copy_from_slice(&boot_sector[..to_copy]);
                Ok(to_copy)
            } else if offset == 16384 {
                let to_copy = buf.len().min(rec0.len());
                buf[..to_copy].copy_from_slice(&rec0[..to_copy]);
                Ok(to_copy)
            } else {
                Ok(0)
            }
        };

        let mut reader = MockReader(mock_read);
        let (bs, runs) = locate_mft_clusters_from_reader(&mut reader)
            .expect("locate_mft_clusters should succeed");
        assert_eq!(bs.bytes_per_sector, 512);
        assert_eq!(bs.bytes_per_cluster(), 4096);
        assert_eq!(bs.mft_start_lcn, 4);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].lcn, Some(16));
        assert_eq!(runs[0].cluster_count, 4);
    }

    #[test]
    fn test_enumerate_fixed_drives() {
        let drives = enumerate_fixed_drives();
        #[cfg(windows)]
        {
            assert!(!drives.is_empty());
            assert!(drives.contains(&'C'));
        }
        #[cfg(not(windows))]
        {
            assert_eq!(drives, vec!['C']);
        }
    }
}
