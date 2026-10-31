// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Window bootstrap shared by the setup and uninstall binaries.

use gpui_kit::gpui::*;

use crate::app::SetupApp;
use crate::i18n::I18n;
use crate::model::Launch;
use crate::platform;

/// The native window title follows the same bundle as the in-window wordmark, so the taskbar and
/// accessibility tree use the language the person is reading.
fn window_title(language: &str, launch: Launch) -> String {
    let i18n = I18n::new(language);
    let subtitle = match launch {
        Launch::Setup => i18n.get("setup_subtitle"),
        Launch::Uninstall => i18n.get("uninstall_subtitle"),
    };
    format!("Cairn {subtitle}")
}

pub fn run_setup() {
    run(Launch::Setup);
}

pub fn run_uninstall() {
    run(Launch::Uninstall);
}

fn run(launch: Launch) {
    let language = crate::preview::Preview::from_env()
        .and_then(|preview| preview.language)
        .unwrap_or_else(|| cairn_ui::locale::system_language().to_string());
    let setup_title = window_title(&language, Launch::Setup);
    let uninstall_title = window_title(&language, Launch::Uninstall);
    let title = match launch {
        Launch::Setup => setup_title.clone(),
        Launch::Uninstall => uninstall_title.clone(),
    };
    let _instance = match platform::acquire_instance(
        r"Local\CairnSetupInstanceMutex",
        &[setup_title.as_str(), uninstall_title.as_str()],
    ) {
        platform::InstanceLock::Running => return,
        lock => lock,
    };

    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            cairn_ui::brand::init_fonts(cx);
            cairn_ui::brand::init_text_rendering(cx);
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

#[cfg(test)]
mod tests {
    use super::window_title;
    use crate::model::Launch;

    #[test]
    fn the_native_title_follows_the_interface_language() {
        assert_eq!(window_title("en-US", Launch::Setup), "Cairn Setup");
        assert_eq!(window_title("en-US", Launch::Uninstall), "Cairn Uninstall");
        assert_eq!(window_title("zh-CN", Launch::Setup), "Cairn 安装程序");
        assert_eq!(window_title("zh-CN", Launch::Uninstall), "Cairn 卸载程序");
    }
}
