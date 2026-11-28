// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! `Cairn.exe`: Non-elevated GPU-accelerated desktop GUI for Cairn.
#![recursion_limit = "512"]
#![windows_subsystem = "windows"]

mod app;
mod motion;
mod search;
mod service;
mod shell;
mod startup;
mod table;
mod view;
mod win32;

use app::CairnApp;
use gpui_kit::gpui::*;

/// The window title. Single-instance activation and tray hiding match by this exact text.
pub(crate) const WINDOW_TITLE: &str = "Cairn";

#[cfg(windows)]
fn acquire_single_instance_mutex() -> Option<windows::Win32::Foundation::HANDLE> {
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::core::w;

    // SAFETY: `w!(...)` is a static null-terminated wide string and the returned mutex handle
    // is closed on duplicate-instance exit or kept alive until process termination.
    unsafe {
        let handle = CreateMutexW(None, true, w!(r"Local\CairnAppInstanceMutex"));
        match handle {
            Ok(h) => {
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    let activated = shell::tray::activate_existing_cairn_window();
                    if activated {
                        let _ = windows::Win32::Foundation::CloseHandle(h);
                        return None;
                    }
                    tracing::warn!(
                        "Existing Cairn mutex detected but no active window found; launching new instance."
                    );
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
    let Some(_mutex) = acquire_single_instance_mutex() else {
        return;
    };

    let startup = startup::StartupCoordinator::prepare(&raw_args, &startup::SystemBootstrapPorts);
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
            crate::shell::perf::mark("app loop start");
            gpui_kit::init(cx);
            cairn_ui::locale::init_layout_direction(cx);
            crate::shell::perf::mark("kit ready");
            cairn_ui::brand::init_fonts(cx);
            crate::shell::perf::mark("fonts ready");
            cairn_ui::brand::init_text_rendering(cx);
            shell::commands::init_keymap(cx);
            crate::shell::perf::mark("keymap ready");

            shell::theme::apply_resolved(initial_dark, initial_zoom, cx);
            crate::shell::perf::mark("theme ready");

            let base = gpui_kit::component::TitleBar::window_options();
            let window_options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(WINDOW_TITLE.into()),
                    ..base.titlebar.unwrap_or_default()
                }),
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
            #[cfg(windows)]
            {
                let mut exit_rx = shell::tray::subscribe_exit();
                cx.spawn(async move |cx: &mut AsyncApp| {
                    while exit_rx.changed().await.is_ok() {
                        if *exit_rx.borrow_and_update() {
                            cx.update(|cx| cx.quit());
                            break;
                        }
                    }
                })
                .detach();
            }
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
