// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Window bootstrap shared by the setup and uninstall binaries.

use gpui_kit::gpui::*;

use crate::app::SetupApp;
use crate::model::Launch;
use crate::platform;

const SETUP_TITLE: &str = "Cairn Setup";
const UNINSTALL_TITLE: &str = "Cairn Uninstall";

pub fn run_setup() {
    run(Launch::Setup);
}

pub fn run_uninstall() {
    run(Launch::Uninstall);
}

fn run(launch: Launch) {
    let title = match launch {
        Launch::Setup => SETUP_TITLE,
        Launch::Uninstall => UNINSTALL_TITLE,
    };
    if platform::acquire_instance(r"Local\CairnSetupInstanceMutex", title).is_none() {
        return;
    }

    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            cairn_ui::brand::init_fonts(cx);
            let dark = cairn_ui::locale::system_prefers_dark();
            cairn_ui::theme::apply_appearance(dark, 1.0, cx);

            let base = gpui_kit::component::TitleBar::window_options();
            let window_options = WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some(title.into()),
                    ..base.titlebar.unwrap_or_default()
                }),
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    Size {
                        width: px(520.0),
                        height: px(240.0),
                    },
                    cx,
                ))),
                is_resizable: false,
                is_minimizable: true,
                app_id: Some(match launch {
                    Launch::Setup => "Cairn.Setup".to_string(),
                    Launch::Uninstall => "Cairn.Uninstall".to_string(),
                }),
                ..base
            };

            gpui_kit::open_window(window_options, cx, |window, cx| {
                window.activate_window();
                cx.new(|cx| SetupApp::new(window, cx, launch))
            })
            .expect("failed to open the Cairn setup window");
        });
}
