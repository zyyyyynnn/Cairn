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

    let expected = path
        .to_string_lossy()
        .replace('/', "\\")
        .trim_start_matches(r"\\?\")
        .to_lowercase();
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
            break String::from_utf16_lossy(&buf[..len as usize])
                .trim_start_matches(r"\\?\")
                .to_lowercase();
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
