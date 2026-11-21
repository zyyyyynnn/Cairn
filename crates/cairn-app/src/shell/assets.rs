// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Scoped SVG asset source combining GPUI Kit component defaults with only the additional
//! icons referenced by `CairnApp`.

use std::borrow::Cow;

use gpui_kit::assets::{Assets, icon_assets};
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
        FolderCog,
        FolderX,
        Logs,
        Presentation,
        Recycle,
        RotateCcw,
        Timer,
        X,
    ]
);

/// Application asset bundle: serves GPUI Kit's default component icons (`Assets`) plus the
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
        IconName::FolderCog,
        IconName::FolderX,
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
        IconName::RotateCw,
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

    fn collect_rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read src dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                collect_rs_files(&path, out);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    fn extract_icon_variants_from_code(source: &str) -> Vec<String> {
        let mut variants = Vec::new();
        let bytes = source.as_bytes();
        let mut i = 0;
        let mut code = String::with_capacity(source.len());

        // Strip comments, string literals, raw strings, and char literals so only code is scanned.
        while i < bytes.len() {
            if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                code.push(' ');
            } else if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                let mut depth = 1usize;
                i += 2;
                while i < bytes.len() && depth > 0 {
                    if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                        depth += 1;
                        i += 2;
                    } else if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                code.push(' ');
            } else if let Some((after_open, hashes)) = match_raw_string_bytes(bytes, i) {
                i = after_open;
                while i < bytes.len() {
                    if bytes[i] == b'"' {
                        let mut matched = 0usize;
                        while matched < hashes
                            && i + 1 + matched < bytes.len()
                            && bytes[i + 1 + matched] == b'#'
                        {
                            matched += 1;
                        }
                        if matched == hashes {
                            i += 1 + hashes;
                            break;
                        }
                    }
                    i += 1;
                }
                code.push(' ');
            } else if bytes[i] == b'"'
                || (bytes[i] == b'b'
                    && i + 1 < bytes.len()
                    && bytes[i + 1] == b'"'
                    && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_')))
            {
                i += if bytes[i] == b'b' { 2 } else { 1 };
                while i < bytes.len() {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 2;
                    } else if bytes[i] == b'"' {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
                code.push(' ');
            } else if bytes[i] == b'\'' {
                if let Some(after_char) = skip_char_literal_bytes(source, i) {
                    i = after_char;
                    code.push(' ');
                } else {
                    code.push('\'');
                    i += 1;
                }
            } else {
                let ch = source[i..].chars().next().expect("valid utf8");
                code.push(ch);
                i += ch.len_utf8();
            }
        }

        for prefix in ["IconName::", "AssetIconName::"] {
            let mut rest = code.as_str();
            let mut offset = 0usize;
            while let Some(pos) = rest.find(prefix) {
                let abs = offset + pos;
                let prev_ok = abs == 0
                    || !code[..abs]
                        .chars()
                        .next_back()
                        .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_');
                let after = &rest[pos + prefix.len()..];
                let ident: String = after
                    .chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .collect();
                if prev_ok
                    && ident != "ALL"
                    && ident
                        .chars()
                        .next()
                        .is_some_and(|ch| ch.is_ascii_uppercase())
                {
                    variants.push(ident);
                }
                let step = pos + prefix.len();
                offset += step;
                rest = &rest[step..];
            }
        }
        variants
    }

    fn match_raw_string_bytes(bytes: &[u8], idx: usize) -> Option<(usize, usize)> {
        if idx > 0 && (bytes[idx - 1].is_ascii_alphanumeric() || bytes[idx - 1] == b'_') {
            return None;
        }
        let mut pos = idx;
        if pos < bytes.len() && bytes[pos] == b'b' {
            pos += 1;
        }
        if pos >= bytes.len() || bytes[pos] != b'r' {
            return None;
        }
        pos += 1;
        let mut hashes = 0usize;
        while pos < bytes.len() && bytes[pos] == b'#' {
            hashes += 1;
            pos += 1;
        }
        (pos < bytes.len() && bytes[pos] == b'"').then_some((pos + 1, hashes))
    }

    fn skip_char_literal_bytes(source: &str, idx: usize) -> Option<usize> {
        let rest = &source[idx + 1..];
        let mut chars = rest.char_indices();
        let (_, first) = chars.next()?;
        if first == '\\' {
            let (_, escaped) = chars.next()?;
            if escaped == '\n' {
                return None;
            }
            for (offset, ch) in chars {
                if ch == '\'' {
                    return Some(idx + 1 + offset + 1);
                }
                if ch == '\n' {
                    return None;
                }
            }
            None
        } else if first != '\n' && first != '\'' {
            let (offset, second) = chars.next()?;
            (second == '\'').then_some(idx + 1 + offset + 1)
        } else {
            None
        }
    }

    #[test]
    fn every_referenced_icon_in_cairn_app_resolves_as_valid_svg() {
        let by_name: std::collections::HashMap<String, IconName> = IconName::ALL
            .iter()
            .copied()
            .map(|icon| (format!("{icon:?}"), icon))
            .collect();

        let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rs_files(&src_dir, &mut files);
        files.sort();

        let mut referenced = std::collections::BTreeSet::new();
        for file in files {
            let text = std::fs::read_to_string(&file)
                .unwrap_or_else(|err| panic!("failed to read {}: {err}", file.display()));
            for variant in extract_icon_variants_from_code(&text) {
                referenced.insert((variant, file.display().to_string()));
            }
        }

        assert!(
            !referenced.is_empty(),
            "expected IconName references under {}",
            src_dir.display()
        );
        for (variant, file) in referenced {
            let icon = by_name
                .get(&variant)
                .unwrap_or_else(|| panic!("unknown icon variant `{variant}` referenced in {file}"));
            let path = icon.path();
            let bytes = CairnAssets
                .load(path.as_ref())
                .unwrap_or_else(|err| {
                    panic!("failed to load {variant} ({path}) referenced in {file}: {err}")
                })
                .unwrap_or_else(|| {
                    panic!("missing icon asset {variant} ({path}) referenced in {file}")
                });
            let text = std::str::from_utf8(&bytes)
                .unwrap_or_else(|err| panic!("non-UTF8 svg for {variant}: {err}"));
            assert!(
                text.contains("<svg"),
                "asset {variant} ({path}) referenced in {file} is not an SVG document"
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
