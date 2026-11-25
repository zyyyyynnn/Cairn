// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The notification-area icon and its two-row menu.
//!
//! The tray belongs to the thread that created it and its menu handles are neither `Send` nor
//! `Sync`, so the handles live in a thread-local slot: [`init_with_i18n`] fills it, [`set_language_with_i18n`]
//! rewrites the rows in place. A process the shell gives no icon (or a test thread) runs every
//! operation as a no-op.

use crate::shell::i18n::I18nManager;
use std::cell::RefCell;
use std::sync::OnceLock;
use tray_icon::menu::{Menu, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// The two rows the tray menu offers, in menu order.
const MENU_KEYS: [&str; 2] = ["tray_show", "tray_exit"];

thread_local! {
    static MENU_ITEMS: RefCell<Option<[MenuItem; 2]>> = const { RefCell::new(None) };
}

static EXIT_WATCH: OnceLock<tokio::sync::watch::Sender<bool>> = OnceLock::new();

fn exit_sender() -> &'static tokio::sync::watch::Sender<bool> {
    EXIT_WATCH.get_or_init(|| tokio::sync::watch::channel(false).0)
}

pub(crate) fn subscribe_exit() -> tokio::sync::watch::Receiver<bool> {
    exit_sender().subscribe()
}

/// The words the two rows show using an already-built `I18nManager`.
pub(crate) fn labels_with_i18n(i18n: &I18nManager) -> [String; 2] {
    MENU_KEYS.map(|key| i18n.get_text(key))
}

fn process_matches_cairn(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::core::PWSTR;

    struct OwnedHandle(HANDLE);
    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: `self.0` came from `OpenProcess` in this function.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    // SAFETY: queries the executable path for an existing PID with least privilege.
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return false;
    };
    let handle = OwnedHandle(handle);
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` is writable for `len` UTF-16 code units.
    if unsafe {
        QueryFullProcessImageNameW(
            handle.0,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    }
    .is_err()
    {
        return false;
    }
    let candidate = std::path::PathBuf::from(String::from_utf16_lossy(&buf[..len as usize]));
    std::env::current_exe().is_ok_and(|current| {
        candidate
            .to_string_lossy()
            .eq_ignore_ascii_case(&current.to_string_lossy())
    })
}

fn find_cairn_window(match_current_process: bool) -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId,
    };
    use windows::core::BOOL;

    struct EnumCtx {
        current_pid: u32,
        match_current_process: bool,
        found: Option<HWND>,
    }

    /// # Safety
    /// `EnumWindows` must invoke this callback synchronously with a valid top-level `hwnd` and a
    /// non-null `lparam` pointing to a live `EnumCtx` for the duration of enumeration.
    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` is `&mut EnumCtx` valid for the synchronous `EnumWindows` call, and `hwnd` is a Win32 enumerated window handle.
        unsafe {
            let ctx = &mut *(lparam.0 as *mut EnumCtx);
            let mut pid = 0u32;
            let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
            let pid_ok = if ctx.match_current_process {
                pid == ctx.current_pid
            } else {
                pid != 0 && pid != ctx.current_pid
            };
            if !pid_ok {
                return BOOL(1);
            }
            let mut title_buf = [0u16; 64];
            let title_len = GetWindowTextW(hwnd, &mut title_buf);
            if title_len > 0 {
                let title = String::from_utf16_lossy(&title_buf[..title_len as usize]);
                if title == crate::WINDOW_TITLE
                    && (ctx.match_current_process || process_matches_cairn(pid))
                {
                    ctx.found = Some(hwnd);
                    return BOOL(0);
                }
            }
            BOOL(1)
        }
    }

    let mut ctx = EnumCtx {
        current_pid: std::process::id(),
        match_current_process,
        found: None,
    };
    // SAFETY: `EnumWindows` invokes `enum_proc` synchronously while `ctx` stays alive on this stack frame.
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut _ as isize));
    }
    ctx.found
}

/// Finds this process's main Cairn window by PID and exact window title.
pub(crate) fn find_main_window() -> Option<windows::Win32::Foundation::HWND> {
    find_cairn_window(true)
}

fn activate_window(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, SW_RESTORE, SetForegroundWindow, ShowWindow,
    };

    // SAFETY: `hwnd` belongs to a matched Cairn window.
    unsafe {
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
    }
}

pub(crate) fn activate_existing_cairn_window() -> bool {
    let Some(hwnd) = find_cairn_window(false) else {
        return false;
    };
    activate_window(hwnd);
    true
}

fn activate_current_window() -> bool {
    let Some(hwnd) = find_main_window() else {
        return false;
    };
    activate_window(hwnd);
    true
}

/// The global hotkey's window action: a foreground window goes to the tray, anything else is
/// brought back and focused.
pub(crate) fn toggle_main_window() {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, IsWindowVisible};

    let Some(hwnd) = find_main_window() else {
        return;
    };
    // SAFETY: `hwnd` is a top-level HWND matched by `find_main_window`.
    let visible = unsafe { IsWindowVisible(hwnd).as_bool() };
    // SAFETY: `GetForegroundWindow` reads the current foreground window handle from User32.
    let foreground = unsafe { GetForegroundWindow() } == hwnd;
    if visible && foreground {
        crate::app::hide_main_window();
    } else {
        let _ = activate_current_window();
    }
}

/// Creates the tray icon with its menu named via `i18n` and keeps the row handles for later
/// language changes. `None` when the shell offers no icon, so the window still opens.
pub(crate) fn init_with_i18n(i18n: &I18nManager) -> Option<TrayIcon> {
    let menu = Menu::new();
    let [show_text, exit_text] = labels_with_i18n(i18n);
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
                let _ = activate_current_window();
            } else if event.id == exit_id {
                let sender = exit_sender();
                if sender.receiver_count() > 0 {
                    let _ = sender.send(true);
                } else {
                    std::process::exit(0);
                }
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
            let _ = activate_current_window();
        }
    }));

    MENU_ITEMS.with(|items| *items.borrow_mut() = Some([show, exit]));
    Some(tray)
}

/// Rewrites the two rows via `i18n`. Without a tray this does nothing.
pub(crate) fn set_language_with_i18n(i18n: &I18nManager) {
    let [show_text, exit_text] = labels_with_i18n(i18n);
    MENU_ITEMS.with(|items| {
        if let Some([show, exit]) = &*items.borrow() {
            show.set_text(show_text);
            exit.set_text(exit_text);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{MENU_KEYS, labels_with_i18n};
    use crate::shell::i18n::I18nManager;
    use crate::shell::settings::{LANGUAGE_EN_US, LANGUAGE_ZH_CN};

    /// Both rows resolve in both locales, so a missing message would show its raw id in the tray.
    #[test]
    fn every_tray_row_has_a_message_in_both_locales() {
        let zh_i18n = I18nManager::new(LANGUAGE_ZH_CN);
        let en_i18n = I18nManager::new(LANGUAGE_EN_US);
        let zh = labels_with_i18n(&zh_i18n);
        let en = labels_with_i18n(&en_i18n);
        for (index, key) in MENU_KEYS.iter().enumerate() {
            assert_ne!(zh[index], *key, "{key} falls back to its id in zh-CN");
            assert_ne!(en[index], *key, "{key} falls back to its id in en-US");
            assert!(!zh[index].trim().is_empty(), "{key} is blank in zh-CN");
            assert!(!en[index].trim().is_empty(), "{key} is blank in en-US");
        }
        assert_ne!(zh, en, "the tray would read the same in every language");
    }
}
