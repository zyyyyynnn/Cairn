// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The notification-area icon and its two-row menu.
//!
//! The tray belongs to the thread that created it and its menu handles are neither `Send` nor
//! `Sync`, so the handles live in a thread-local slot: [`init`] fills it, [`set_language`]
//! rewrites the rows in place. A process the shell gives no icon (or a test thread) runs every
//! operation as a no-op.

use crate::shell::i18n::I18nManager;
use std::cell::RefCell;
use tray_icon::menu::{Menu, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// The two rows the tray menu offers, in menu order.
const MENU_KEYS: [&str; 2] = ["tray_show", "tray_exit"];

thread_local! {
    static MENU_ITEMS: RefCell<Option<[MenuItem; 2]>> = const { RefCell::new(None) };
}

/// The words the two rows show in `language`.
fn labels(language: &str) -> [String; 2] {
    let i18n = I18nManager::new(language);
    MENU_KEYS.map(|key| i18n.get_text(key))
}

/// Finds the running Cairn window by its exact window title.
///
/// GPUI's window class is shared with other applications, so matching the class could focus an
/// unrelated window.
pub(crate) fn find_main_window() -> Option<windows::Win32::Foundation::HWND> {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW};

    struct EnumCtx {
        found: Option<HWND>,
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut EnumCtx);
        let mut title_buf = [0u16; 64];
        let title_len = GetWindowTextW(hwnd, &mut title_buf);
        if title_len > 0 {
            let title = String::from_utf16_lossy(&title_buf[..title_len as usize]);
            if title == crate::WINDOW_TITLE {
                ctx.found = Some(hwnd);
                return BOOL(0);
            }
        }
        BOOL(1)
    }

    let mut ctx = EnumCtx { found: None };
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut _ as isize));
    }
    ctx.found
}

pub(crate) fn activate_existing_cairn_window() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, SetForegroundWindow, ShowWindow, SW_RESTORE,
    };

    let Some(hwnd) = find_main_window() else {
        return false;
    };
    unsafe {
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
    }
    true
}

/// The global hotkey's window action: a foreground window goes to the tray, anything else is
/// brought back and focused.
pub(crate) fn toggle_main_window() {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, IsWindowVisible, ShowWindow, SW_HIDE,
    };

    let Some(hwnd) = find_main_window() else {
        return;
    };
    let visible = unsafe { IsWindowVisible(hwnd).as_bool() };
    let foreground = unsafe { GetForegroundWindow() } == hwnd;
    if visible && foreground {
        unsafe {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    } else {
        activate_existing_cairn_window();
    }
}

/// Creates the tray icon with its menu named in `language` and keeps the row handles for later
/// language changes. `None` when the shell offers no icon, so the window still opens.
pub fn init(language: &str) -> Option<TrayIcon> {
    let menu = Menu::new();
    let [show_text, exit_text] = labels(language);
    let show = MenuItem::new(show_text, true, None);
    let exit = MenuItem::new(exit_text, true, None);
    let _ = menu.append(&show);
    let _ = menu.append(&exit);

    const ICON_BYTES: &[u8] = include_bytes!("../../../../assets/icons/cairn-32.rgba");
    let icon = Icon::from_rgba(ICON_BYTES.to_vec(), 32, 32).ok()?;
    let tray = TrayIconBuilder::new()
        .with_tooltip("Cairn")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .build()
        .ok()?;

    let show_id = show.id().clone();
    let exit_id = exit.id().clone();
    tray_icon::menu::MenuEvent::set_event_handler(Some(
        move |event: tray_icon::menu::MenuEvent| {
            if event.id == show_id {
                activate_existing_cairn_window();
            } else if event.id == exit_id {
                std::process::exit(0);
            }
        },
    ));
    tray_icon::TrayIconEvent::set_event_handler(Some(|event: tray_icon::TrayIconEvent| {
        if matches!(
            event,
            tray_icon::TrayIconEvent::Click {
                button: tray_icon::MouseButton::Left,
                ..
            } | tray_icon::TrayIconEvent::DoubleClick { .. }
        ) {
            activate_existing_cairn_window();
        }
    }));

    MENU_ITEMS.with(|items| *items.borrow_mut() = Some([show, exit]));
    Some(tray)
}

/// Rewrites the two rows in `language`. Without a tray this does nothing.
pub fn set_language(language: &str) {
    let [show_text, exit_text] = labels(language);
    MENU_ITEMS.with(|items| {
        if let Some([show, exit]) = &*items.borrow() {
            show.set_text(show_text);
            exit.set_text(exit_text);
        }
    });
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::{labels, set_language, MENU_KEYS};
    use crate::shell::settings::{LANGUAGE_EN_US, LANGUAGE_ZH_CN};

    /// Both rows resolve in both locales, so a missing message would show its raw id in the tray.
    #[test]
    fn every_tray_row_has_a_message_in_both_locales() {
        let zh = labels(LANGUAGE_ZH_CN);
        let en = labels(LANGUAGE_EN_US);
        for (index, key) in MENU_KEYS.iter().enumerate() {
            assert_ne!(zh[index], *key, "{key} falls back to its id in zh-CN");
            assert_ne!(en[index], *key, "{key} falls back to its id in en-US");
            assert!(!zh[index].trim().is_empty(), "{key} is blank in zh-CN");
            assert!(!en[index].trim().is_empty(), "{key} is blank in en-US");
        }
        assert_ne!(zh, en, "the tray would read the same in every language");
    }

    /// Tests, and a desktop whose shell refused the icon, have no row handles; rewriting them
    /// must still be safe.
    #[test]
    fn changing_the_language_without_a_tray_is_a_no_op() {
        set_language(LANGUAGE_ZH_CN);
        set_language(LANGUAGE_EN_US);
    }
}
