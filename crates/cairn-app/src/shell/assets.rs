// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Scoped SVG asset source combining GPUI Kit component defaults with only the additional
//! icons referenced by `CairnApp`.

use std::borrow::Cow;

use gpui_kit::assets::{icon_assets, Assets};
use gpui_kit::gpui::{AssetSource, Result, SharedString};

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
        if let Some(bytes) = CairnExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names = Assets.list(path)?;
        names.extend(CairnExtraIcons.list(path)?);
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
    fn unused_catalog_icons_are_excluded_from_bundle() {
        assert!(CairnAssets.load("icons/clapperboard.svg").is_err());
    }
}
