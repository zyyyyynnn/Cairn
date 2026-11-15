// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Win32 Shell Property System fallback for extended file attributes (`IDX-12`).

use cairn_core::property_store::RecordPropertyData;
use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::PropertiesSystem::PSGetPropertyKeyFromName;
use windows::Win32::UI::Shell::{IShellItem2, SHCreateItemFromParsingName};

struct ComApartmentGuard {
    needs_uninit: bool,
}

impl ComApartmentGuard {
    fn init() -> Option<Self> {
        // SAFETY: Initializes COM STA for the calling thread; paired with CoUninitialize on drop
        // only when CoInitializeEx succeeded (S_OK or S_FALSE).
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr.is_ok() {
            Some(Self { needs_uninit: true })
        } else {
            // RPC_E_CHANGED_MODE means MTA/another apartment is already initialized on this thread.
            Some(Self {
                needs_uninit: false,
            })
        }
    }
}

impl Drop for ComApartmentGuard {
    fn drop(&mut self) {
        if self.needs_uninit {
            // SAFETY: Paired with successful CoInitializeEx on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

fn property_key(canonical: &str) -> Option<PROPERTYKEY> {
    let wide: Vec<u16> = canonical.encode_utf16().chain(std::iter::once(0)).collect();
    let mut key = PROPERTYKEY::default();
    // SAFETY: Null-terminated UTF-16 property name and valid out-pointer `key`.
    unsafe { PSGetPropertyKeyFromName(PCWSTR(wide.as_ptr()), &mut key) }.ok()?;
    Some(key)
}

fn read_string_prop(item: &IShellItem2, canonical: &str) -> Option<String> {
    let key = property_key(canonical)?;
    // SAFETY: `key` is valid; `GetString` allocates via CoTaskMemAlloc and we free it below.
    let raw = unsafe { item.GetString(&key) }.ok()?;
    if raw.is_null() {
        return None;
    }
    // SAFETY: `raw` is a valid null-terminated UTF-16 string returned by `GetString`.
    let text = unsafe { raw.to_string() }.ok();
    // SAFETY: Freed with CoTaskMemFree as documented for `IShellItem2::GetString`.
    unsafe { CoTaskMemFree(Some(raw.0.cast())) };
    text.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn read_u64_prop(item: &IShellItem2, canonical: &str) -> Option<u64> {
    let key = property_key(canonical)?;
    // SAFETY: `key` is a valid property key on `item`.
    unsafe { item.GetUInt64(&key) }.ok()
}

fn read_u32_prop(item: &IShellItem2, canonical: &str) -> Option<u32> {
    let key = property_key(canonical)?;
    // SAFETY: `key` is a valid property key on `item`.
    unsafe { item.GetUInt32(&key) }.ok()
}

/// Queries Windows Shell property handlers for metadata missing from pure-Rust parsers.
pub(crate) fn read_shell_properties(path: &Path) -> Option<RecordPropertyData> {
    let _com = ComApartmentGuard::init()?;
    let wide_path: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: Null-terminated UTF-16 path bound for the duration of the call.
    let item: IShellItem2 =
        unsafe { SHCreateItemFromParsingName(PCWSTR(wide_path.as_ptr()), None) }.ok()?;

    let duration_100ns = read_u64_prop(&item, "System.Media.Duration");
    let length_ms = duration_100ns
        .map(|ticks| ticks / 10_000)
        .filter(|ms| *ms > 0);
    let author = read_string_prop(&item, "System.Author");
    let tags = read_string_prop(&item, "System.Keywords");

    let width = read_u32_prop(&item, "System.Video.FrameWidth")
        .or_else(|| read_u32_prop(&item, "System.Image.HorizontalSize"))
        .filter(|v| *v > 0);
    let height = read_u32_prop(&item, "System.Video.FrameHeight")
        .or_else(|| read_u32_prop(&item, "System.Image.VerticalSize"))
        .filter(|v| *v > 0);
    let dimensions = width.zip(height);

    // Windows Property System defines `System.Video.FrameRate` in frames per 1000 seconds.
    let framerate_mfps = read_u32_prop(&item, "System.Video.FrameRate").filter(|v| *v > 0);

    let data = RecordPropertyData {
        length_ms,
        author,
        tags,
        dimensions,
        framerate_mfps,
    };
    (!data.is_empty()).then_some(data)
}
