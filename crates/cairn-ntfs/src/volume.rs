// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Volume access, sector reading, and MFT stream parsing.

use crate::mft::{parse_mft_record, ExtractedItem};
use rayon::prelude::*;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use thiserror::Error;

#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE};
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, SetFilePointerEx, FILE_BEGIN, FILE_FLAGS_AND_ATTRIBUTES,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};

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

/// Enumerates all local fixed drive letters (e.g. ['C', 'D']).
pub fn enumerate_fixed_drives() -> Vec<char> {
    #[cfg(windows)]
    {
        use windows::Win32::Storage::FileSystem::{
            GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
        };
        let mut drives = Vec::new();
        // SAFETY: GetLogicalDrives has no parameters and simply returns a 32-bit drive bitmask.
        let mask = unsafe { GetLogicalDrives() };
        for i in 0..26 {
            if (mask & (1 << i)) != 0 {
                let letter = (b'A' + i as u8) as char;
                let root_path: Vec<u16> = format!("{}:\\\0", letter).encode_utf16().collect();
                // SAFETY: root_path is a valid null-terminated UTF-16 drive root string.
                let drive_type = unsafe { GetDriveTypeW(PCWSTR(root_path.as_ptr())) };
                // 3 corresponds to DRIVE_FIXED
                if drive_type == 3 {
                    let mut fs_name_buf = [0u16; 260];
                    let res = unsafe {
                        GetVolumeInformationW(
                            PCWSTR(root_path.as_ptr()),
                            None,
                            None,
                            None,
                            None,
                            Some(&mut fs_name_buf),
                        )
                    };
                    if res.is_ok() {
                        let fs_name = String::from_utf16_lossy(
                            &fs_name_buf[..fs_name_buf
                                .iter()
                                .position(|&c| c == 0)
                                .unwrap_or(fs_name_buf.len())],
                        );
                        if fs_name.eq_ignore_ascii_case("NTFS") {
                            drives.push(letter);
                        }
                    }
                }
            }
        }
        drives
    }
    #[cfg(not(windows))]
    {
        vec!['C']
    }
}

/// Opens a raw volume device handle.
/// Device path format MUST be `\\.\C:` strictly without any trailing backslash.
#[cfg(windows)]
pub struct RawVolumeHandle {
    pub handle: HANDLE,
    pub path: String,
}

#[cfg(windows)]
impl RawVolumeHandle {
    pub fn open(drive_letter: char) -> Result<Self, VolumeError> {
        let dev_path = format!(r"\\.\{}:", drive_letter.to_ascii_uppercase());
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

    /// Reads raw bytes at `offset` using SetFilePointerEx and ReadFile.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        let mut moved_offset = 0i64;
        // SAFETY: SetFilePointerEx moves file pointer to specified 64-bit physical offset.
        let seek_ok = unsafe {
            SetFilePointerEx(
                self.handle,
                offset as i64,
                Some(&mut moved_offset),
                FILE_BEGIN,
            )
        };
        if seek_ok.is_err() {
            return Err(std::io::Error::last_os_error());
        }

        let mut bytes_read = 0u32;
        // SAFETY: ReadFile in windows 0.58 takes safe slice Some(buf) and pointer to bytes_read.
        let read_ok = unsafe { ReadFile(self.handle, Some(buf), Some(&mut bytes_read), None) };
        if read_ok.is_err() {
            return Err(std::io::Error::last_os_error());
        }

        Ok(bytes_read as usize)
    }

    /// Queries the volume's USN journal metadata (FSCTL_QUERY_USN_JOURNAL).
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
            let err = unsafe { windows::Win32::Foundation::GetLastError() };
            return Err(crate::usn::UsnError::QueryFailed(err.0));
        }
        crate::usn::UsnJournalData::parse(&out_buf[..bytes_returned as usize])
    }

    /// Reads USN change journal records starting from `start_usn` (FSCTL_READ_USN_JOURNAL).
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
            let err = unsafe { windows::Win32::Foundation::GetLastError() };
            return Err(crate::usn::UsnError::ReadFailed(err.0));
        }
        Ok(crate::usn::parse_usn_records(
            &out_buf[..bytes_returned as usize],
        ))
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

/// Locates the NTFS $MFT cluster runs and boot sector using a generic block reader function.
pub fn locate_mft_clusters_with_read_at<F>(
    mut read_at: F,
) -> Result<
    (
        crate::boot_sector::NtfsBootSector,
        Vec<crate::runlist::DataRun>,
    ),
    VolumeError,
>
where
    F: FnMut(u64, &mut [u8]) -> Result<usize, std::io::Error>,
{
    // Read up to 4096 bytes for sector 0 to accommodate both 512e and 4Kn native sector drives
    let mut boot_sector_buf = [0u8; 4096];
    read_at(0, &mut boot_sector_buf)?;
    let boot_sector = crate::boot_sector::NtfsBootSector::parse(&boot_sector_buf[..512])?;

    let bytes_per_cluster = boot_sector.bytes_per_cluster();
    let mft_start_offset = (boot_sector.mft_start_lcn as u64) * bytes_per_cluster;
    let rec_size = boot_sector.mft_record_size as usize;
    let sector_size = boot_sector.bytes_per_sector as usize;

    let read_size = rec_size.max(sector_size);
    let mut record0_buf = vec![0u8; read_size];
    read_at(mft_start_offset, &mut record0_buf)?;

    crate::fixup::apply_fixup(&mut record0_buf[..rec_size], sector_size)?;

    let first_attr_offset = u16::from_le_bytes([record0_buf[0x14], record0_buf[0x15]]) as usize;
    let attrs = crate::attribute::parse_attributes(&record0_buf[..rec_size], first_attr_offset)?;

    let data_offset =
        attrs
            .data_runlist_offset
            .ok_or(VolumeError::Mft(crate::mft::MftError::Attribute(
                crate::attribute::AttributeParseError::BufferTooSmall(0),
            )))?;

    let runlist_offset =
        u16::from_le_bytes([record0_buf[data_offset + 32], record0_buf[data_offset + 33]]) as usize;
    let attr_len = u32::from_le_bytes([
        record0_buf[data_offset + 4],
        record0_buf[data_offset + 5],
        record0_buf[data_offset + 6],
        record0_buf[data_offset + 7],
    ]) as usize;

    if data_offset + runlist_offset >= rec_size
        || data_offset + attr_len > rec_size
        || runlist_offset >= attr_len
    {
        return Err(VolumeError::Runlist(
            crate::runlist::RunlistError::UnexpectedEnd(0),
        ));
    }

    let runlist_bytes = &record0_buf[data_offset + runlist_offset..data_offset + attr_len];
    let runs = crate::runlist::parse_runlist(runlist_bytes, 0)?;

    Ok((boot_sector, runs))
}

/// Locates the NTFS $MFT cluster runs and boot sector from Sector 0 and Record 0.
#[cfg(windows)]
pub fn locate_mft_clusters(
    raw_vol: &RawVolumeHandle,
) -> Result<
    (
        crate::boot_sector::NtfsBootSector,
        Vec<crate::runlist::DataRun>,
    ),
    VolumeError,
> {
    locate_mft_clusters_with_read_at(|offset, buf| raw_vol.read_at(offset, buf))
}

/// Scans a live NTFS volume by reading raw MFT clusters via `RawVolumeHandle`.
#[cfg(windows)]
pub fn scan_live_volume_mft(
    drive_letter: char,
    chunk_records: usize,
) -> Result<Vec<ExtractedItem>, VolumeError> {
    let raw_vol = RawVolumeHandle::open(drive_letter)?;
    let (boot_sector, runs) = locate_mft_clusters(&raw_vol)?;

    let bytes_per_cluster = boot_sector.bytes_per_cluster();
    let record_size = boot_sector.mft_record_size as usize;
    let sector_size = boot_sector.bytes_per_sector as usize;
    let mut all_items = Vec::new();

    let chunk_size = chunk_records * record_size;
    let mut buffer = vec![0u8; chunk_size];
    let mut current_record_num = 0u64;

    for run in runs {
        let Some(lcn) = run.lcn else {
            let sparse_records = (run.cluster_count * bytes_per_cluster) / (record_size as u64);
            current_record_num += sparse_records;
            continue;
        };

        let mut run_bytes_remaining = run.cluster_count * bytes_per_cluster;
        let mut run_offset = (lcn as u64) * bytes_per_cluster;

        while run_bytes_remaining > 0 {
            let to_read = (chunk_size as u64).min(run_bytes_remaining) as usize;
            let read_slice = &mut buffer[..to_read];
            let bytes_read = raw_vol.read_at(run_offset, read_slice)?;
            if bytes_read == 0 {
                break;
            }

            let records_in_buf = bytes_read / record_size;
            let mut chunks: Vec<(u64, Vec<u8>)> = Vec::with_capacity(records_in_buf);

            for i in 0..records_in_buf {
                let rec_num = current_record_num + i as u64;
                if rec_num < 24 {
                    continue;
                }
                let slice = &read_slice[i * record_size..(i + 1) * record_size];
                chunks.push((rec_num, slice.to_vec()));
            }

            let parsed_items: Vec<ExtractedItem> = chunks
                .into_par_iter()
                .flat_map(|(rec_num, mut rec_bytes)| {
                    parse_mft_record(&mut rec_bytes, sector_size, rec_num).unwrap_or_default()
                })
                .collect();

            all_items.extend(parsed_items);
            current_record_num += records_in_buf as u64;
            run_offset += bytes_read as u64;
            run_bytes_remaining = run_bytes_remaining.saturating_sub(bytes_read as u64);
        }
    }

    Ok(all_items)
}

#[cfg(not(windows))]
pub fn scan_live_volume_mft(
    _drive_letter: char,
    _chunk_records: usize,
) -> Result<Vec<ExtractedItem>, VolumeError> {
    Ok(Vec::new())
}

/// Loads MFT records from an offline MFT image file (IDX-10).
pub fn read_offline_mft_file<P: AsRef<Path>>(
    path: P,
    chunk_records: usize,
) -> Result<Vec<ExtractedItem>, VolumeError> {
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    const RECORD_SIZE: usize = 1024;
    const SECTOR_SIZE: usize = 512;

    let total_records = file_len as usize / RECORD_SIZE;
    let mut all_items = Vec::new();
    let chunk_size = chunk_records * RECORD_SIZE;
    let mut buffer = vec![0u8; chunk_size];

    let mut current_record_num = 0u64;

    while current_record_num < total_records as u64 {
        let read_bytes = file.read(&mut buffer)?;
        if read_bytes == 0 {
            break;
        }

        let records_in_buf = read_bytes / RECORD_SIZE;
        let mut chunks: Vec<(u64, Vec<u8>)> = Vec::with_capacity(records_in_buf);

        for i in 0..records_in_buf {
            let rec_num = current_record_num + i as u64;
            // Skip first 24 system metadata records by default (IDX-09)
            if rec_num < 24 {
                continue;
            }
            let slice = &buffer[i * RECORD_SIZE..(i + 1) * RECORD_SIZE];
            chunks.push((rec_num, slice.to_vec()));
        }

        let parsed_items: Vec<ExtractedItem> = chunks
            .into_par_iter()
            .flat_map(|(rec_num, mut rec_bytes)| {
                parse_mft_record(&mut rec_bytes, SECTOR_SIZE, rec_num).unwrap_or_default()
            })
            .collect();

        all_items.extend(parsed_items);
        current_record_num += records_in_buf as u64;
    }

    Ok(all_items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_locate_mft_clusters_synthetic() {
        // 1. Build a synthetic NTFS Sector 0 (Boot Sector)
        let mut boot_sector = vec![0u8; 512];
        boot_sector[3..11].copy_from_slice(b"NTFS    ");
        boot_sector[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes()); // 512 bytes / sector
        boot_sector[0x0D] = 8; // 8 sectors / cluster -> 4096 bytes / cluster
        boot_sector[0x40] = 0xF6; // -10 -> 1024 bytes per MFT record
        boot_sector[0x30..0x38].copy_from_slice(&4i64.to_le_bytes()); // MFT start LCN = 4 (offset 16384)
        boot_sector[0x48..0x50].copy_from_slice(&0x1122334455667788u64.to_le_bytes());

        // 2. Build a synthetic Record 0 (1024 bytes) with non-resident $DATA runlist
        let mut rec0 = vec![0u8; 1024];
        rec0[0..4].copy_from_slice(b"FILE");
        rec0[4..6].copy_from_slice(&48u16.to_le_bytes()); // Update sequence offset = 48 (0x30)
        rec0[6..8].copy_from_slice(&3u16.to_le_bytes()); // Update sequence size = 3 words
        rec0[0x14..0x16].copy_from_slice(&56u16.to_le_bytes()); // First attribute offset = 56 (0x38)
        rec0[0x16..0x18].copy_from_slice(&1u16.to_le_bytes()); // Flags: in use (1)

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

        let (bs, runs) = locate_mft_clusters_with_read_at(mock_read)
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
