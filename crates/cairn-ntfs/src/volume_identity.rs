// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Real Win32 Volume GUID enumeration and mount-path parsing for local NTFS volumes.

/// Persistent identity metadata for a mounted fixed NTFS volume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeIdentity {
    pub drive_letter: Option<char>,
    pub mount_point: String,
    pub volume_guid: String,
    pub volume_serial: u64,
}

use cairn_core::path_cache::drive_letter_of_mount;

/// Splits a Win32 `MULTI_SZ` UTF-16 buffer into mount path strings, stopping at the double-null terminator.
pub fn parse_volume_path_names(multi_sz: &[u16]) -> Vec<String> {
    multi_sz
        .split(|&ch| ch == 0)
        .take_while(|slice| !slice.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

#[cfg(windows)]
mod win {
    use super::{VolumeIdentity, drive_letter_of_mount, parse_volume_path_names};
    use std::collections::HashSet;
    use windows::Win32::Foundation::{ERROR_MORE_DATA, HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Storage::FileSystem::{
        FindFirstVolumeW, FindNextVolumeW, FindVolumeClose, GetDriveTypeW, GetLogicalDrives,
        GetVolumeInformationW, GetVolumeNameForVolumeMountPointW, GetVolumePathNamesForVolumeNameW,
    };
    use windows::core::PCWSTR;

    const DRIVE_FIXED: u32 = 3;

    struct VolumeFindGuard(HANDLE);

    impl Drop for VolumeFindGuard {
        fn drop(&mut self) {
            if self.0 != INVALID_HANDLE_VALUE && !self.0.is_invalid() {
                // SAFETY: Handle came from FindFirstVolumeW and is closed exactly once on drop.
                unsafe {
                    let _ = FindVolumeClose(self.0);
                }
            }
        }
    }

    fn wide_slice_to_string(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    fn query_ntfs_serial(root_wide: &[u16]) -> Option<u64> {
        let mut serial = 0u32;
        let mut fs_name_buf = [0u16; 64];
        // SAFETY: root_wide is null-terminated UTF-16; output buffers are valid stack arrays.
        let ok = unsafe {
            GetVolumeInformationW(
                PCWSTR(root_wide.as_ptr()),
                None,
                Some(&mut serial),
                None,
                None,
                Some(&mut fs_name_buf),
            )
        };
        if ok.is_err() {
            return None;
        }
        let fs_name = wide_slice_to_string(&fs_name_buf);
        fs_name
            .eq_ignore_ascii_case("NTFS")
            .then_some(serial as u64)
    }

    fn query_volume_mount_paths(volume_guid_wide: &[u16]) -> Vec<String> {
        let mut buf = vec![0u16; 512];
        let mut return_len = 0u32;
        // SAFETY: volume_guid_wide is null-terminated; buf is writable for buf.len() u16s.
        let res = unsafe {
            GetVolumePathNamesForVolumeNameW(
                PCWSTR(volume_guid_wide.as_ptr()),
                Some(&mut buf),
                &mut return_len,
            )
        };
        if let Err(err) = res {
            if err.code() == ERROR_MORE_DATA.to_hresult() && return_len > 0 {
                buf.resize(return_len as usize, 0);
                // SAFETY: resized buffer holds at least `return_len` UTF-16 units.
                let retry = unsafe {
                    GetVolumePathNamesForVolumeNameW(
                        PCWSTR(volume_guid_wide.as_ptr()),
                        Some(&mut buf),
                        &mut return_len,
                    )
                };
                if retry.is_err() {
                    return Vec::new();
                }
            } else {
                return Vec::new();
            }
        }
        parse_volume_path_names(&buf)
    }

    pub fn enumerate_volumes_impl() -> Vec<VolumeIdentity> {
        let mut volumes = Vec::new();
        let mut seen_guids = HashSet::new();
        let mut seen_letters = HashSet::new();

        let mut name_buf = [0u16; 512];
        // SAFETY: name_buf is valid for receiving a null-terminated Win32 volume GUID path.
        if let Ok(handle) = unsafe { FindFirstVolumeW(&mut name_buf) } {
            let _guard = VolumeFindGuard(handle);
            loop {
                let volume_guid = wide_slice_to_string(&name_buf);
                let guid_wide: Vec<u16> = volume_guid
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();

                // SAFETY: guid_wide is a valid null-terminated UTF-16 volume path.
                let drive_type = unsafe { GetDriveTypeW(PCWSTR(guid_wide.as_ptr())) };
                if drive_type == DRIVE_FIXED
                    && let Some(serial) = query_ntfs_serial(&guid_wide)
                {
                    let mount_paths = query_volume_mount_paths(&guid_wide);
                    if !mount_paths.is_empty() {
                        let drive_mount = mount_paths
                            .iter()
                            .find_map(|p| drive_letter_of_mount(p).map(|ch| (ch, p.clone())));
                        let (drive_letter, mount_point) = match drive_mount {
                            Some((ch, p)) => (Some(ch), p),
                            None => (None, mount_paths[0].clone()),
                        };
                        let guid_key = volume_guid.to_ascii_lowercase();
                        if seen_guids.insert(guid_key) {
                            if let Some(ch) = drive_letter {
                                seen_letters.insert(ch);
                            }
                            volumes.push(VolumeIdentity {
                                drive_letter,
                                mount_point,
                                volume_guid,
                                volume_serial: serial,
                            });
                        }
                    }
                }

                name_buf.fill(0);
                // SAFETY: FindNextVolumeW uses the live find handle owned by `_guard`.
                if unsafe { FindNextVolumeW(handle, &mut name_buf) }.is_err() {
                    break;
                }
            }
        }

        // Supplement with drive-letter enumeration in case a fixed virtual NTFS volume
        // was excluded from `FindFirstVolumeW` output.
        // SAFETY: GetLogicalDrives takes no pointers and returns a 26-bit mask.
        let mask = unsafe { GetLogicalDrives() };
        for i in 0..26u8 {
            if (mask & (1 << i)) == 0 {
                continue;
            }
            let letter = (b'A' + i) as char;
            if seen_letters.contains(&letter) {
                continue;
            }
            let root = format!("{letter}:\\");
            let root_wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
            // SAFETY: root_wide is a valid null-terminated UTF-16 drive root.
            if unsafe { GetDriveTypeW(PCWSTR(root_wide.as_ptr())) } != DRIVE_FIXED {
                continue;
            }
            let Some(serial) = query_ntfs_serial(&root_wide) else {
                continue;
            };
            let mut guid_buf = [0u16; 128];
            // SAFETY: root_wide is null-terminated; guid_buf receives `\\?\Volume{...}\`.
            if unsafe {
                GetVolumeNameForVolumeMountPointW(PCWSTR(root_wide.as_ptr()), &mut guid_buf)
            }
            .is_err()
            {
                continue;
            }
            let volume_guid = wide_slice_to_string(&guid_buf);
            let guid_key = volume_guid.to_ascii_lowercase();
            if seen_guids.insert(guid_key) {
                seen_letters.insert(letter);
                volumes.push(VolumeIdentity {
                    drive_letter: Some(letter),
                    mount_point: root,
                    volume_guid,
                    volume_serial: serial,
                });
            }
        }

        volumes.sort_by(|a, b| match (a.drive_letter, b.drive_letter) {
            (Some(la), Some(lb)) => la.cmp(&lb),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a
                .mount_point
                .to_ascii_lowercase()
                .cmp(&b.mount_point.to_ascii_lowercase()),
        });
        volumes
    }
}

/// Enumerates all mounted local fixed NTFS volumes with their real Win32 Volume GUIDs.
pub fn enumerate_volumes() -> Vec<VolumeIdentity> {
    #[cfg(windows)]
    {
        win::enumerate_volumes_impl()
    }
    #[cfg(not(windows))]
    {
        vec![VolumeIdentity {
            drive_letter: Some('C'),
            mount_point: "C:\\".to_string(),
            volume_guid: r"\\?\Volume{00000000-0000-0000-0000-000000000043}\".to_string(),
            volume_serial: 0,
        }]
    }
}
