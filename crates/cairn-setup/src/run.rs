// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Window bootstrap shared by the setup and uninstall binaries.

use gpui_kit::gpui::*;

use crate::app::SetupApp;
use crate::i18n::I18n;
use crate::model::Launch;
use crate::platform;
use crate::preview::Preview;

gpui_kit::assets::icon_assets!(
    SetupIcons,
    [WindowMinimize, WindowClose, CircleCheck, CircleAlert]
);

struct SetupAssets;

impl AssetSource for SetupAssets {
    fn load(&self, path: &str) -> gpui_kit::gpui::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        if let Some(bytes) = SetupIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> gpui_kit::gpui::Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(SetupIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

/// The native window title follows the same bundle as the in-window wordmark, so the taskbar and
/// accessibility tree use the language the person is reading.
fn window_title(i18n: &I18n, launch: Launch) -> String {
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
    let preview = Preview::from_env();
    let language = preview
        .as_ref()
        .and_then(|preview| preview.language.clone())
        .unwrap_or_else(|| cairn_ui::locale::system_language().to_string());
    let i18n = I18n::new(&language);
    let setup_title = window_title(&i18n, Launch::Setup);
    let uninstall_title = window_title(&i18n, Launch::Uninstall);
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
        .with_assets(SetupAssets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            cairn_ui::brand::init_fonts(cx);
            cairn_ui::brand::init_text_rendering(cx);
            let dark = preview
                .as_ref()
                .and_then(|preview| preview.dark)
                .unwrap_or_else(cairn_ui::locale::system_prefers_dark);
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

            gpui_kit::open_window(window_options, cx, move |window, cx| {
                window.activate_window();
                cx.new(|cx| SetupApp::new(window, cx, launch, i18n, preview, dark))
            })
            .expect("failed to open the Cairn setup window");
        });
}

#[cfg(test)]
mod tests {
    use super::window_title;
    use crate::i18n::I18n;
    use crate::model::Launch;

    #[test]
    fn the_native_title_follows_the_interface_language() {
        let en = I18n::new("en-US");
        assert_eq!(window_title(&en, Launch::Setup), "Cairn Setup");
        assert_eq!(window_title(&en, Launch::Uninstall), "Cairn Uninstall");
        let zh = I18n::new("zh-CN");
        assert_eq!(window_title(&zh, Launch::Setup), "Cairn 安装程序");
        assert_eq!(window_title(&zh, Launch::Uninstall), "Cairn 卸载程序");
    }
}
