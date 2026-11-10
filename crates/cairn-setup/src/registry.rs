// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! One adapter for the HKLM reads shared by setup start-up and the uninstaller.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
    REG_ROUTINE_FLAGS, REG_SAM_FLAGS,
};

pub(crate) fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().chain(std::iter::once(0)).collect()
}

/// The immediate subkeys below `base` under HKLM, read through `view`. Missing keys read as an
/// empty list; the caller decides whether that is an error.
pub(crate) fn subkeys(base: &str, view: REG_SAM_FLAGS) -> Vec<String> {
    let base_wide = wide(std::ffi::OsStr::new(base));
    let mut key = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(base_wide.as_ptr()),
            None,
            KEY_READ | view,
            &mut key,
        )
    };
    if status != ERROR_SUCCESS {
        return Vec::new();
    }

    let mut names = Vec::new();
    let mut index = 0;
    loop {
        let mut name = [0u16; 256];
        let mut length = name.len() as u32;
        // SAFETY: `name` is a stack buffer of `length` UTF-16 units; the call fills it and
        // reports the written length back through `length`.
        let status = unsafe {
            RegEnumKeyExW(
                key,
                index,
                Some(windows::core::PWSTR(name.as_mut_ptr())),
                &mut length,
                None,
                None,
                None,
                None,
            )
        };
        if status == ERROR_NO_MORE_ITEMS || status != ERROR_SUCCESS {
            break;
        }
        names.push(String::from_utf16_lossy(&name[..length as usize]));
        index += 1;
    }
    // SAFETY: `key` was opened by this function and is closed exactly once here.
    unsafe {
        let _ = RegCloseKey(key);
    }
    names
}

/// Reads one HKLM string value. `flags` selects the accepted registry types; callers that read
/// expandable values (profile paths) also accept `RRF_RT_REG_EXPAND_SZ`.
pub(crate) fn read_hklm_string(
    subkey: &str,
    value: &str,
    flags: REG_ROUTINE_FLAGS,
) -> Option<String> {
    let subkey_wide = wide(std::ffi::OsStr::new(subkey));
    let value_wide = wide(std::ffi::OsStr::new(value));
    let mut buffer = [0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: `buffer` holds the reported byte capacity; the call writes at most that many bytes
    // and updates `size` to the written size including the terminator.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey_wide.as_ptr()),
            PCWSTR(value_wide.as_ptr()),
            flags,
            None,
            Some(buffer.as_mut_ptr() as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let length = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(
        &buffer[..length.min(buffer.len())],
    ))
}
