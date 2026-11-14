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
mod startup;
mod table;
mod treemap;
mod view;
mod win32;

use app::CairnApp;
use gpui_kit::gpui::*;

/// The window title. Single-instance activation and tray hiding match by this exact text.
pub(crate) const WINDOW_TITLE: &str = "Cairn";

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
                    let activated = shell::tray::activate_existing_cairn_window();
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

fn main() {
    // The elevated copy of this image has one job and no window. It answers before the single
    // instance mutex is taken: a copy that found the mutex already held would otherwise return
    // here and exit as though it had started the service.
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    // The exit code is the Win32 code, bit-for-bit: `GetExitCodeProcess` hands it back to the
    // waiting window as `u32`, which is what decides success there.
    if let Some(code) = service::service_control::elevated_start_reply(&raw_args) {
        std::process::exit(code as i32);
    }

    crate::shell::perf::mark("process main");
    let _ = tracing_subscriber::fmt::try_init();

    #[cfg(windows)]
    let _mutex = match acquire_single_instance_mutex() {
        Some(handle) => handle,
        None => return,
    };

    // Prepare settings, OS theme preference, CLI launch flags and shared locale state once before
    // building the tray or window.
    let startup = startup::StartupCoordinator::prepare(raw_args, &startup::SystemBootstrapPorts);
    crate::shell::perf::mark("startup prepared");

    #[cfg(windows)]
    let _tray = shell::tray::init_with_i18n(&startup.i18n);
    crate::shell::perf::mark("tray ready");

    let initial_dark = startup.is_dark();
    let initial_zoom = startup.settings.zoom_level;
    let mut startup_slot = Some(startup);

    gpui_kit::application()
        .with_assets(shell::assets::CairnAssets)
        .run(move |cx: &mut App| {
            // Bootstrap order is fixed by the Coding Guides: initialize the Kit once, install the
            // keymap before any window or menu reads it, then open the window through
            // `gpui_kit::open_window` so `Root` owns the first level of the window.
            gpui_kit::init(cx);
            crate::shell::perf::mark("kit ready");
            cairn_ui::brand::init_fonts(cx);
            crate::shell::perf::mark("fonts ready");
            cairn_ui::brand::init_text_rendering(cx);
            shell::commands::init_keymap(cx);
            crate::shell::perf::mark("keymap ready");

            // The persisted appearance is installed before the first frame, so the window opens at
            // the palette and density the user left it in rather than repainting after the fact.
            shell::theme::apply_resolved(initial_dark, initial_zoom, cx);
            crate::shell::perf::mark("theme ready");

            // `TitleBar` draws the window's own chrome, owns its drag region and paints the platform
            // controls, so the options start from the set the component is built to work with. The
            // title is still given to the platform: it is what the taskbar and Alt+Tab read, and
            // `hide_title` only removes it from the bar we draw ourselves.
            //
            // Both sizes are logical pixels and both are read off the shell's own budgets. One
            // value serves default and minimum: a window cannot be made smaller than what the
            // shell cannot compress — `SIDEBAR_WIDTH`, the four column floors from
            // `crate::app::layout` with their cell padding (about 780px at zoom 1.0) and the bands
            // of `crate::app::layout::CHROME_BANDS`. 1280x800 clears that floor with the two
            // growing lanes, name and path, sitting several hundred pixels above their minimums,
            // and leaves the result band about eighteen rows.
            let base = gpui_kit::component::TitleBar::window_options();
            let window_options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(WINDOW_TITLE.into()),
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
                    width: px(1280.0),
                    height: px(800.0),
                }),
                ..base
            };

            let startup = startup_slot
                .take()
                .expect("startup bundle consumed once by window launch");
            let initial_query = startup.plan.initial_query.clone();
            let sidebar_collapsed = startup.plan.sidebar_collapsed;
            let open_match = startup.plan.open_match;
            let mut app_bundle = Some(startup);

            crate::shell::perf::mark("window open");
            let (_handle, _app) = gpui_kit::open_window(window_options, cx, |window, cx| {
                crate::shell::perf::mark("window factory");
                window.activate_window();
                let startup = app_bundle
                    .take()
                    .expect("window factory runs once per launch");
                let entity = cx.new(|cx| {
                    let mut app = CairnApp::new_with_startup(startup, window, cx);
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
