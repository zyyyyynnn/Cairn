// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Stable file identities for paths on a live volume.
//!
//! The MFT scanner and the change journal both describe a file by its 64-bit file reference
//! number (MFT record number plus sequence number). Resolving a directory path to the same value
//! lets the index exclude a whole subtree by identity instead of comparing paths for every record.

use std::path::Path;

/// Returns the NTFS file reference number of `path` when it is a directory on a local volume.
///
/// The low eight bytes of `FILE_ID_INFO.FileId` are the 64-bit file reference number: low 48 bits
/// are the MFT record number and high 16 bits the sequence number, exactly the value carried by
/// `$MFT` records and USN journal records. `None` means the path does not exist, cannot be opened,
/// or the platform has no NTFS identity concept.
#[cfg(windows)]
pub fn directory_file_id(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FileIdInfo, GetFileInformationByHandleEx, FILE_FLAGS_AND_ATTRIBUTES,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let wide_path: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: `wide_path` is a valid null-terminated UTF-16 string. No access rights are requested
    // because the identity does not require reading the directory, and `FILE_FLAG_BACKUP_SEMANTICS`
    // is what makes opening a directory handle legal.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide_path.as_ptr()),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(FILE_FLAG_BACKUP_SEMANTICS.0),
            None,
        )
    }
    .ok()?;
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }

    let mut info = FILE_ID_INFO::default();
    // SAFETY: `handle` is an open directory handle and `info` is a correctly sized, writable
    // `FILE_ID_INFO` buffer for `FileIdInfo`.
    let result = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };

    // SAFETY: the handle was opened above and is owned by this function. Every return path after
    // this point goes through this single close, so no handle can leak on an error.
    unsafe {
        let _ = CloseHandle(handle);
    }

    if result.is_err() {
        return None;
    }

    let identifier: [u8; 8] = info.FileId.Identifier[..8].try_into().ok()?;
    Some(u64::from_le_bytes(identifier))
}

/// Non-Windows platforms have no NTFS file reference numbers.
#[cfg(not(windows))]
pub fn directory_file_id(path: &Path) -> Option<u64> {
    let _ = path;
    None
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        std::env::temp_dir().join(format!("cairn-file-id-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn existing_directory_has_a_stable_nonzero_id_and_missing_path_has_none() {
        let dir = unique_temp_dir();
        std::fs::create_dir_all(&dir).expect("create temp directory");
        let id = directory_file_id(&dir).expect("existing directory must expose a file id");
        assert_ne!(id, 0, "a live NTFS directory cannot have a zero file id");
        assert_eq!(
            directory_file_id(&dir),
            Some(id),
            "resolving the same directory twice must yield the same identity"
        );
        assert_eq!(directory_file_id(&dir.join("missing")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
