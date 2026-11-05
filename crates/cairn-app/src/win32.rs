// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Win32 primitives shared by the shell and the service layer.
//!
//! These live in their own leaf module so neither layer has to depend on the other for a string
//! conversion or an error-code unpacking. The module depends on nothing but `windows`.

/// Null-terminated UTF-16 without the `\\?\` prefix a path conversion adds. The command line of a
/// child process is *not* a path, so the prefix would be handed to the argument parser verbatim.
#[cfg(windows)]
pub(crate) fn to_wide_literal(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `ERROR_CANCELLED`: the user dismissed a consent prompt.
#[cfg(windows)]
pub(crate) const WIN32_ERROR_USER_CANCELLED: u32 = 1223;

/// Recovers the Win32 error from an `HRESULT_FROM_WIN32` value (facility 7).
#[cfg(windows)]
pub(crate) const fn win32_code_from_hresult(code: i32) -> Option<u32> {
    let bits = code as u32;
    let is_failure = bits & 0x8000_0000 != 0;
    let facility = (bits >> 16) & 0x1FFF;
    if is_failure && facility == 7 {
        Some(bits & 0xFFFF)
    } else {
        None
    }
}

/// Whether the left mouse button is physically down anywhere on the desktop.
/// A window resize runs inside the system's modal loop, so the window itself
/// receives no release until the drag ends; the Treemap mask polls this state
/// to stay up through a slow drag instead of settling on a pause.
#[cfg(windows)]
pub(crate) fn is_left_mouse_button_down() -> bool {
    #[link(name = "user32")]
    extern "system" {
        fn GetAsyncKeyState(v_key: i32) -> i16;
    }
    const VK_LBUTTON: i32 = 0x01;
    // SAFETY: GetAsyncKeyState only reads the asynchronous key state.
    let state = unsafe { GetAsyncKeyState(VK_LBUTTON) };
    (state as u16 & 0x8000) != 0
}

#[cfg(not(windows))]
pub(crate) fn is_left_mouse_button_down() -> bool {
    false
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn an_hresult_from_win32_carries_the_win32_code() {
        // HRESULT_FROM_WIN32(ERROR_ACCESS_DENIED)
        assert_eq!(win32_code_from_hresult(0x8007_0005u32 as i32), Some(5));
        // A success HRESULT carries no error code.
        assert_eq!(win32_code_from_hresult(0), None);
        // A failure from another facility is not a Win32 error.
        assert_eq!(win32_code_from_hresult(0x8004_0000u32 as i32), None);
    }
}
