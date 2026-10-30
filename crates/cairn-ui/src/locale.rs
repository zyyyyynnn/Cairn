// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The two operating-system preferences every Cairn window follows.

pub const LANGUAGE_ZH_CN: &str = "zh-CN";
pub const LANGUAGE_EN_US: &str = "en-US";

/// The OS UI language, reduced to the two locales Cairn ships. Any non-Chinese UI language uses
/// the English bundle rather than showing a half-translated third locale.
pub fn system_language() -> &'static str {
    #[cfg(windows)]
    {
        use windows::Win32::Globalization::GetUserDefaultUILanguage;
        // LANGID: the low 10 bits are the primary language. 0x04 is Chinese.
        let langid = unsafe { GetUserDefaultUILanguage() };
        if langid & 0x03ff == 0x04 {
            LANGUAGE_ZH_CN
        } else {
            LANGUAGE_EN_US
        }
    }
    #[cfg(not(windows))]
    {
        LANGUAGE_EN_US
    }
}

/// Whether Windows is currently using its dark app appearance.
///
/// `AppsUseLightTheme` is the per-user switch the Settings app writes; a missing or unreadable value
/// falls back to the light palette rather than guessing.
pub fn system_prefers_dark() -> bool {
    #[cfg(windows)]
    {
        use windows::core::w;
        use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

        let mut light_theme = 1u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: the key and value names are static null-terminated strings, the destination is a
        // live u32 and its size is passed with the pointer.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
                w!("AppsUseLightTheme"),
                RRF_RT_REG_DWORD,
                None,
                Some(&mut light_theme as *mut u32 as *mut core::ffi::c_void),
                Some(&mut size),
            )
        };
        status.is_ok() && light_theme == 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}
