// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Subprocess property and content extraction library for `CairnWorker.exe`.

pub mod detector;
pub mod extractor;
pub mod property_extract;

pub use detector::decode_bytes_to_string;
pub use extractor::{
    CompositeExtractor, ExtractError, OfficeExtractor, PdfExtractor, PlainTextExtractor,
    TextExtractor, open_no_follow,
};

#[cfg(windows)]
fn normalized_dos_path(raw: &str) -> String {
    raw.replace('/', "\\")
        .trim_start_matches(r"\\?\")
        .to_lowercase()
}

#[cfg(windows)]
fn expand_expected_dos_path(path: &std::path::Path) -> std::io::Result<String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{GetFullPathNameW, GetLongPathNameW};
    use windows::core::PCWSTR;

    let raw_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut full_buf = vec![0u16; 32_768];
    let full_len = loop {
        // SAFETY: `raw_wide` is NUL-terminated and `full_buf` is writable for its full length.
        let len = unsafe { GetFullPathNameW(PCWSTR(raw_wide.as_ptr()), Some(&mut full_buf), None) };
        if len == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if (len as usize) < full_buf.len() {
            break len as usize;
        }
        full_buf.resize(len as usize + 1, 0);
    };
    full_buf[full_len] = 0;

    let mut long_buf = vec![0u16; full_len.max(32_768) + 1];
    let long_len = loop {
        // SAFETY: `full_buf` is NUL-terminated at `full_len` and `long_buf` is writable.
        let len = unsafe { GetLongPathNameW(PCWSTR(full_buf.as_ptr()), Some(&mut long_buf)) };
        if len == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if (len as usize) < long_buf.len() {
            break len as usize;
        }
        long_buf.resize(len as usize + 1, 0);
    };

    Ok(normalized_dos_path(&String::from_utf16_lossy(
        &long_buf[..long_len],
    )))
}

#[cfg(windows)]
pub(crate) fn validate_opened_path(
    file: &std::fs::File,
    path: &std::path::Path,
) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GETFINALPATHNAMEBYHANDLE_FLAGS, GetFinalPathNameByHandleW,
        VOLUME_NAME_DOS,
    };

    let expected = expand_expected_dos_path(path)?;
    let mut buf = vec![0u16; 32_768];
    let actual = loop {
        // SAFETY: the handle remains owned by `file`, and `buf` is writable for its full length.
        let len = unsafe {
            GetFinalPathNameByHandleW(
                HANDLE(file.as_raw_handle()),
                &mut buf,
                GETFINALPATHNAMEBYHANDLE_FLAGS(FILE_NAME_NORMALIZED.0 | VOLUME_NAME_DOS.0),
            )
        };
        if len == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if len < buf.len() as u32 {
            break normalized_dos_path(&String::from_utf16_lossy(&buf[..len as usize]));
        }
        buf.resize(len as usize + 1, 0);
    };
    if actual != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("opened path {actual} does not match expected {expected}"),
        ));
    }
    Ok(())
}

#[cfg(all(windows, test))]
pub(crate) fn short_path_for_test(path: &std::path::Path) -> std::path::PathBuf {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::GetShortPathNameW;
    use windows::core::PCWSTR;

    let raw_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut short_buf = vec![0u16; 32_768];
    // SAFETY: `raw_wide` is NUL-terminated and `short_buf` is writable.
    let len = unsafe { GetShortPathNameW(PCWSTR(raw_wide.as_ptr()), Some(&mut short_buf)) };
    if len == 0 || (len as usize) >= short_buf.len() {
        return path.to_path_buf();
    }
    std::path::PathBuf::from(String::from_utf16_lossy(&short_buf[..len as usize]))
}

/// Lowers scheduling and I/O priority of the current worker process to background mode.
pub fn enter_background_mode() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::{
            GetCurrentProcess, PROCESS_MODE_BACKGROUND_BEGIN, SetPriorityClass,
        };
        // SAFETY: `GetCurrentProcess()` returns the current process pseudo-handle, which is always
        // valid for `SetPriorityClass` with `PROCESS_MODE_BACKGROUND_BEGIN`.
        let _ = unsafe { SetPriorityClass(GetCurrentProcess(), PROCESS_MODE_BACKGROUND_BEGIN) };
    }
}
