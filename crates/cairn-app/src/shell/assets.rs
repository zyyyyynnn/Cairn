// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Scoped SVG asset source combining GPUI Kit component defaults with only the additional
//! icons referenced by `CairnApp`.

use std::borrow::Cow;

use gpui_kit::assets::{icon_assets, Assets};
use gpui_kit::gpui::{AssetSource, Result, SharedString};

pub(crate) const RADIO_DOT_ICON_PATH: &str = "icons/cairn-radio-dot.svg";
const RADIO_DOT_SVG: &[u8] =
    br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><circle cx="12" cy="12" r="5" fill="currentColor"/></svg>"#;

icon_assets!(
    CairnExtraIcons,
    [
        AppWindow,
        Database,
        Disc,
        FileArchive,
        FileBox,
        FileBraces,
        FileCode,
        FileCog,
        FileImage,
        FileMusic,
        FileSpreadsheet,
        FileStack,
        FileSymlink,
        FileTerminal,
        FileType,
        FileVideoCamera,
        Logs,
        Presentation,
        Recycle,
        RotateCcw,
        Timer,
        X,
    ]
);

/// Application asset bundle: serves GPUI Kit's default component icons (`Assets`) plus the 22
/// Cairn-specific icons via static `icon_assets!`, avoiding embedding unused catalog SVGs.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CairnAssets;

impl AssetSource for CairnAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == RADIO_DOT_ICON_PATH {
            return Ok(Some(Cow::Borrowed(RADIO_DOT_SVG)));
        }
        if let Some(bytes) = CairnExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names = Assets.list(path)?;
        names.extend(CairnExtraIcons.list(path)?);
        if path.is_empty() || path == "icons" {
            names.push(RADIO_DOT_ICON_PATH.into());
        }
        names.sort();
        names.dedup();
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::CairnAssets;
    use gpui_kit::assets::IconName;
    use gpui_kit::gpui::AssetSource as _;

    const USED_ICONS: &[IconName] = &[
        IconName::AppWindow,
        IconName::ArrowLeft,
        IconName::ArrowRight,
        IconName::ArrowUp,
        IconName::ChevronDown,
        IconName::ChevronRight,
        IconName::ChevronUp,
        IconName::CircleAlert,
        IconName::Cpu,
        IconName::Database,
        IconName::Disc,
        IconName::File,
        IconName::FileArchive,
        IconName::FileBox,
        IconName::FileBraces,
        IconName::FileCode,
        IconName::FileCog,
        IconName::FileImage,
        IconName::FileMusic,
        IconName::FileSpreadsheet,
        IconName::FileStack,
        IconName::FileSymlink,
        IconName::FileTerminal,
        IconName::FileText,
        IconName::FileType,
        IconName::FileVideoCamera,
        IconName::Folder,
        IconName::HardDrive,
        IconName::Info,
        IconName::LayoutDashboard,
        IconName::LoaderCircle,
        IconName::Logs,
        IconName::Palette,
        IconName::PanelLeftClose,
        IconName::PanelLeftOpen,
        IconName::Play,
        IconName::Presentation,
        IconName::Recycle,
        IconName::RefreshCw,
        IconName::RotateCcw,
        IconName::Search,
        IconName::Timer,
        IconName::TriangleAlert,
        IconName::WindowClose,
        IconName::WindowMaximize,
        IconName::WindowMinimize,
        IconName::WindowRestore,
        IconName::X,
    ];

    #[test]
    fn every_cairn_icon_resolves_as_valid_svg() {
        for &icon in USED_ICONS {
            let path = icon.path();
            let bytes = CairnAssets
                .load(path.as_ref())
                .unwrap_or_else(|err| panic!("failed to load {icon:?} ({path}): {err}"))
                .unwrap_or_else(|| panic!("missing icon asset {icon:?} ({path})"));
            let text = std::str::from_utf8(&bytes)
                .unwrap_or_else(|err| panic!("non-UTF8 svg for {icon:?}: {err}"));
            assert!(
                text.contains("<svg"),
                "asset {icon:?} ({path}) is not an SVG document"
            );
        }
    }

    #[test]
    fn radio_dot_icon_resolves_as_circle_svg() {
        let bytes = CairnAssets
            .load(super::RADIO_DOT_ICON_PATH)
            .expect("radio dot icon query should succeed")
            .expect("radio dot icon asset should exist");
        let text = std::str::from_utf8(&bytes).expect("radio dot svg should be UTF-8");
        assert!(text.contains("<svg") && text.contains("<circle"));
    }

    #[test]
    fn unused_catalog_icons_are_excluded_from_bundle() {
        assert!(CairnAssets.load("icons/clapperboard.svg").is_err());
    }
}
