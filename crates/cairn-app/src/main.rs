// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! `Cairn.exe`: Non-elevated GPU-accelerated desktop GUI for Cairn.
#![recursion_limit = "512"]
#![windows_subsystem = "windows"]

mod app;
mod search;
mod service;
mod shell;
mod table;
mod treemap;
mod view;
mod win32;

use app::CairnApp;
use gpui_kit::gpui::*;

#[cfg(windows)]
fn activate_existing_cairn_window() -> bool {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, EnumWindows, GetWindowTextW, SetForegroundWindow, ShowWindow, SW_RESTORE,
    };

    struct EnumCtx {
        found: Option<HWND>,
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let ctx = &mut *(lparam.0 as *mut EnumCtx);
        // Match only this product's window title. GPUI's window class is shared
        // with other applications, so matching it could focus an unrelated window.
        let mut title_buf = [0u16; 64];
        let title_len = GetWindowTextW(hwnd, &mut title_buf);
        if title_len > 0 {
            let title = String::from_utf16_lossy(&title_buf[..title_len as usize]);
            if title == "Cairn" {
                ctx.found = Some(hwnd);
                return BOOL(0);
            }
        }
        BOOL(1) // Continue enumeration
    }

    let mut ctx = EnumCtx { found: None };
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut _ as isize));
    }

    if let Some(hwnd) = ctx.found {
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = BringWindowToTop(hwnd);
            let _ = SetForegroundWindow(hwnd);
        }
        true
    } else {
        false
    }
}

#[cfg(windows)]
fn acquire_single_instance_mutex() -> Option<windows::Win32::Foundation::HANDLE> {
    use windows::core::w;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;

    unsafe {
        let handle = CreateMutexW(None, true, w!(r"Local\CairnAppInstanceMutex"));
        match handle {
            Ok(h) => {
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    // Another instance is already running; restore its window and terminate current process (UI-08)
                    let activated = activate_existing_cairn_window();
                    if activated {
                        let _ = windows::Win32::Foundation::CloseHandle(h);
                        return None;
                    }
                    // If no existing window could be found or activated (e.g. headless orphan process),
                    // proceed to launch rather than trapping the user in an unresponsive state.
                    tracing::warn!("Existing Cairn mutex detected but no active window found; launching new instance.");
                    Some(h)
                } else {
                    Some(h)
                }
            }
            Err(_) => None,
        }
    }
}

#[cfg(windows)]
fn init_system_tray() -> Option<tray_icon::TrayIcon> {
    use tray_icon::menu::{Menu, MenuItem};
    use tray_icon::{Icon, TrayIconBuilder};

    let i18n = crate::shell::i18n::I18nManager::default();
    let menu = Menu::new();
    let item_show = MenuItem::new(i18n.get_text("tray_show"), true, None);
    let item_exit = MenuItem::new(i18n.get_text("tray_exit"), true, None);
    let _ = menu.append(&item_show);
    let _ = menu.append(&item_exit);

    // Load official 32x32 RGBA Cairn icon
    const ICON_BYTES: &[u8] = include_bytes!("../../../assets/icons/cairn-32.rgba");
    let icon = Icon::from_rgba(ICON_BYTES.to_vec(), 32, 32).ok()?;
    let tray = TrayIconBuilder::new()
        .with_tooltip("Cairn")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .build()
        .ok()?;

    let show_id = item_show.id().clone();
    let exit_id = item_exit.id().clone();

    std::thread::spawn(move || {
        let menu_rx = tray_icon::menu::MenuEvent::receiver();
        let tray_rx = tray_icon::TrayIconEvent::receiver();

        loop {
            while let Ok(event) = menu_rx.try_recv() {
                if event.id == show_id {
                    activate_existing_cairn_window();
                } else if event.id == exit_id {
                    std::process::exit(0);
                }
            }

            while let Ok(event) = tray_rx.try_recv() {
                if matches!(
                    event,
                    tray_icon::TrayIconEvent::Click {
                        button: tray_icon::MouseButton::Left,
                        ..
                    } | tray_icon::TrayIconEvent::DoubleClick { .. }
                ) {
                    activate_existing_cairn_window();
                }
            }

            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    });

    Some(tray)
}

fn main() {
    crate::shell::perf::mark("process main");
    let _ = tracing_subscriber::fmt::try_init();

    #[cfg(windows)]
    let _mutex = match acquire_single_instance_mutex() {
        Some(handle) => handle,
        None => return,
    };

    #[cfg(windows)]
    let _tray = init_system_tray();

    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(|cx: &mut App| {
            // Bootstrap order is fixed by the Coding Guides: initialize the Kit once, install the
            // keymap before any window or menu reads it, then open the window through
            // `gpui_kit::open_window` so `Root` owns the first level of the window.
            gpui_kit::init(cx);
            cairn_ui::brand::init_fonts(cx);
            cairn_ui::brand::init_text_rendering(cx);
            shell::commands::init_keymap(cx);

            // The persisted appearance is installed before the first frame, so the window opens at
            // the palette and density the user left it in rather than repainting after the fact.
            let settings = shell::settings::AppSettings::load();
            shell::theme::apply(&settings, cx);

            // `TitleBar` draws the window's own chrome, owns its drag region and paints the platform
            // controls, so the options start from the set the component is built to work with. The
            // title is still given to the platform: it is what the taskbar and Alt+Tab read, and
            // `hide_title` only removes it from the bar we draw ourselves.
            //
            // Both sizes are logical pixels and both are read off the shell's own budgets. 1024x640
            // is the smallest standard desktop rung (1024 / 1280 / 1440 / 1920) whose width still
            // contains what the shell cannot compress: `SIDEBAR_MIN_WIDTH`, the four column floors
            // from `crate::app::layout` and their cell padding, about 780px at zoom 1.0. Below that
            // rung a lane stops narrowing and starts being cut off. Its height leaves the result
            // band about eighteen rows once the title bar and the three bands of
            // `crate::app::layout::CHROME_BANDS` have been placed. 1280x800 is the next rung up, and
            // it is the default because that is where the two growing lanes, name and path, sit
            // several hundred pixels clear of their floors instead of barely above them.
            let base = gpui_kit::component::TitleBar::window_options();
            let window_options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("Cairn".into()),
                    ..base.titlebar.unwrap_or_default()
                }),
                // Centered rather than at (0, 0): a top-left origin pushes the status line under
                // the taskbar on a short display, and the shell's own bottom band is part of the task.
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    Size {
                        width: px(1280.0),
                        height: px(800.0),
                    },
                    cx,
                ))),
                window_min_size: Some(Size {
                    width: px(1024.0),
                    height: px(640.0),
                }),
                ..base
            };

            let args: Vec<String> = std::env::args().skip(1).collect();
            let mut initial_query: Option<String> = None;
            let mut sidebar_collapsed = false;
            let mut open_match = false;

            for arg in &args {
                if arg == "--collapsed" || arg == "--sidebar-collapsed" {
                    sidebar_collapsed = true;
                } else if arg == "--open-match" {
                    open_match = true;
                } else if !arg.starts_with('-') && initial_query.is_none() {
                    initial_query = Some(arg.clone());
                }
            }

            let (_handle, _app) = gpui_kit::open_window(window_options, cx, |window, cx| {
                crate::shell::perf::mark("window factory");
                window.activate_window();
                let initial_query = initial_query.clone();
                let entity = cx.new(|cx| {
                    let mut app = CairnApp::new(window, cx);
                    if sidebar_collapsed {
                        app.sidebar_collapsed = true;
                    }
                    app
                });

                if let Some(ref text) = initial_query {
                    let text = text.clone();
                    entity.update(cx, |this, cx| {
                        this.set_initial_query(&text, cx);
                    });
                }
                if open_match {
                    entity.update(cx, |this, cx| {
                        this.set_group_open(
                            shell::command_groups::MenuGroup::Search,
                            true,
                            window,
                            cx,
                        );
                    });
                }
                entity
            })
            .expect("Failed to open main Cairn window");
        });
}
