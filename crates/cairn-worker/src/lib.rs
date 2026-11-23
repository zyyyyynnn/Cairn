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
