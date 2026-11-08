// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The one file-kind icon map shared by result rows and Treemap tiles.

use cairn_core::treemap::FileKind;
use gpui_kit::assets::IconName;

/// The glyph for a file kind. The installed icon catalog carries every name, so
/// a kind always renders a glyph rather than a blank slot.
pub fn file_kind_icon(kind: FileKind) -> IconName {
    match kind {
        FileKind::Driver => IconName::Cpu,
        FileKind::SystemFile => IconName::FileCog,
        FileKind::Application => IconName::AppWindow,
        FileKind::Installer => IconName::FileBox,
        FileKind::Library => IconName::FileCog,
        FileKind::Document => IconName::FileText,
        FileKind::Presentation => IconName::Presentation,
        FileKind::Spreadsheet => IconName::FileSpreadsheet,
        FileKind::SourceCode => IconName::FileCode,
        FileKind::Config => IconName::FileBraces,
        FileKind::Script => IconName::FileTerminal,
        FileKind::Archive => IconName::FileArchive,
        FileKind::DiscImage => IconName::Disc,
        FileKind::Image => IconName::FileImage,
        FileKind::Design => IconName::Palette,
        FileKind::Video => IconName::FileVideoCamera,
        FileKind::Audio => IconName::FileMusic,
        FileKind::Font => IconName::FileType,
        FileKind::Database => IconName::Database,
        FileKind::Shortcut => IconName::FileSymlink,
        FileKind::Log => IconName::Logs,
        FileKind::Other => IconName::File,
    }
}
