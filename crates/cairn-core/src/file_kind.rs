// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! File-kind classification from a name, shared by table icons and filters.

/// The concrete kind of a file, resolved from its name. The app paints a row
/// with the kind's icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileKind {
    /// Kernel drivers and EFI images.
    Driver,
    /// System resources: message tables, catalogs and setup information.
    SystemFile,
    /// Executables that run on their own.
    Application,
    /// Installation packages for an application.
    Installer,
    /// Shared libraries and their registrable controls.
    Library,
    /// Text and general office documents.
    Document,
    /// Slide decks.
    Presentation,
    /// Row-and-column data files.
    Spreadsheet,
    /// Source code and build files.
    SourceCode,
    /// Structured or key/value configuration files.
    Config,
    /// Shell and scripting-language programs.
    Script,
    /// Compressed archives.
    Archive,
    /// Whole-disk and whole-volume images.
    DiscImage,
    /// Photographs and general-purpose graphics.
    Image,
    /// Design and layout source files.
    Design,
    Video,
    Audio,
    /// Typeface files.
    Font,
    /// Database files.
    Database,
    /// Links to another file, folder or site.
    Shortcut,
    /// Recorded runtime logs.
    Log,
    /// Unrecognized extension or no extension at all.
    Other,
}

/// Build files whose extension would otherwise point at another kind
/// (`CMakeLists.txt` would read as a document, `Cargo.toml`/`package.json` as
/// unknown configuration).
const SOURCE_FILE_NAMES: &[&str] = &[
    "Makefile",
    "Dockerfile",
    "CMakeLists.txt",
    "Cargo.toml",
    "package.json",
];

/// Files named after their role rather than their format; matched on the stem so
/// `README`, `README.md` and `LICENSE.txt` all resolve to the same kind.
const DOCUMENT_FILE_STEMS: &[&str] = &["README", "LICENSE", "NOTICE"];

/// The extension table is the only extension source: the last dot-separated
/// segment of a name selects the first kind whose list contains it, compared
/// ASCII case-insensitively.
const EXTENSION_TABLE: &[(FileKind, &[&str])] = &[
    (FileKind::Driver, &["sys", "drv", "efi"]),
    (FileKind::SystemFile, &["mui", "cat", "inf", "pnf"]),
    (FileKind::Application, &["exe", "com", "cpl", "scr"]),
    (
        FileKind::Installer,
        &["msi", "msix", "msixbundle", "appx", "appxbundle"],
    ),
    (FileKind::Library, &["dll", "ocx", "ax", "tlb"]),
    (
        FileKind::Document,
        &[
            "doc", "docx", "odt", "rtf", "txt", "md", "pdf", "epub", "mobi", "xps", "one",
            "onenote", "pages", "tex",
        ],
    ),
    (FileKind::Presentation, &["ppt", "pptx", "odp", "key"]),
    (
        FileKind::Spreadsheet,
        &["xls", "xlsx", "xlsm", "ods", "csv", "tsv", "numbers"],
    ),
    (
        FileKind::SourceCode,
        &[
            "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "vue", "svelte", "py", "pyi", "go",
            "java", "kt", "kts", "c", "cc", "cpp", "cxx", "h", "hpp", "cs", "rb", "php", "swift",
            "m", "mm", "scala", "dart", "lua", "r", "jl", "pl", "ex", "exs", "erl", "hs", "clj",
            "fs", "html", "htm", "css", "scss", "sass", "less", "sql", "proto", "graphql", "ipynb",
        ],
    ),
    (
        FileKind::Config,
        &[
            "json",
            "toml",
            "yaml",
            "yml",
            "ini",
            "cfg",
            "conf",
            "xml",
            "plist",
            "env",
            "properties",
        ],
    ),
    (
        FileKind::Script,
        &[
            "sh", "bash", "zsh", "fish", "bat", "cmd", "ps1", "psm1", "vbs",
        ],
    ),
    (
        FileKind::Archive,
        &[
            "zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "zst", "lz4", "cab", "arj", "lzh",
            "war", "jar",
        ],
    ),
    (
        FileKind::DiscImage,
        &["iso", "dmg", "vhd", "vhdx", "vmdk", "img", "wim", "esd"],
    ),
    (
        FileKind::Image,
        &[
            "jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "svg", "heic", "heif",
            "avif", "ico", "jfif",
        ],
    ),
    (
        FileKind::Design,
        &["psd", "psb", "ai", "fig", "sketch", "xd", "indd", "eps"],
    ),
    (
        FileKind::Video,
        &[
            "mp4", "mkv", "avi", "mov", "wmv", "webm", "m4v", "flv", "mpg", "mpeg", "m2ts", "3gp",
            "vob", "rmvb",
        ],
    ),
    (
        FileKind::Audio,
        &[
            "mp3", "flac", "wav", "aac", "ogg", "opus", "m4a", "wma", "aiff", "mid", "midi", "ape",
        ],
    ),
    (
        FileKind::Font,
        &["ttf", "otf", "woff", "woff2", "fon", "fnt"],
    ),
    (
        FileKind::Database,
        &["db", "sqlite", "sqlite3", "mdb", "accdb", "dbf"],
    ),
    (
        FileKind::Shortcut,
        &["lnk", "url", "appref-ms", "website", "webloc"],
    ),
    (FileKind::Log, &["log"]),
];

impl FileKind {
    /// Classifies a file name.
    ///
    /// The last dot-separated segment wins (`archive.tar.gz` is an archive) and
    /// comparison is ASCII case-insensitive. A known build file name outranks
    /// its extension; a name whose only dot is the leading one (`.gitignore`,
    /// `.env`) is configuration; and `README`, `LICENSE` and `NOTICE` resolve by
    /// stem, so their extension never decides.
    pub fn classify_file(name: &str) -> Self {
        if SOURCE_FILE_NAMES
            .iter()
            .any(|known| name.eq_ignore_ascii_case(known))
        {
            return Self::SourceCode;
        }

        if let Some(rest) = name.strip_prefix('.') {
            if !rest.is_empty() && !rest.contains('.') {
                return Self::Config;
            }
        }

        let stem = match name.rsplit_once('.') {
            Some((stem, _)) => stem,
            None => name,
        };
        if DOCUMENT_FILE_STEMS
            .iter()
            .any(|known| stem.eq_ignore_ascii_case(known))
        {
            return Self::Document;
        }

        match name.rsplit_once('.') {
            Some((_, extension)) if !extension.is_empty() => Self::from_extension(extension),
            _ => Self::Other,
        }
    }

    fn from_extension(extension: &str) -> Self {
        EXTENSION_TABLE
            .iter()
            .find(|(_, extensions)| {
                extensions
                    .iter()
                    .any(|known| extension.eq_ignore_ascii_case(known))
            })
            .map(|(kind, _)| *kind)
            .unwrap_or(Self::Other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_tables_cover_every_kind() {
        for (kind, extensions) in EXTENSION_TABLE {
            for extension in *extensions {
                let name = format!("sample.{extension}");
                assert_eq!(FileKind::classify_file(&name), *kind, "{name}");
                let upper = format!("SAMPLE.{}", extension.to_ascii_uppercase());
                assert_eq!(FileKind::classify_file(&upper), *kind, "{upper}");
            }
        }
    }

    #[test]
    fn known_kinds_classify_from_names() {
        for (name, kind) in [
            ("pagefile.sys", FileKind::Driver),
            ("hiberfil.sys", FileKind::Driver),
            ("nvlddmkm.drv", FileKind::Driver),
            ("boot.efi", FileKind::Driver),
            ("explorer.exe", FileKind::Application),
            ("setup.msix", FileKind::Installer),
            ("KERNEL32.DLL", FileKind::Library),
            ("report.xlsx", FileKind::Spreadsheet),
            ("deck.pptx", FileKind::Presentation),
            ("notes.final.txt", FileKind::Document),
            ("main.rs", FileKind::SourceCode),
            ("settings.json", FileKind::Config),
            ("build.ps1", FileKind::Script),
            ("archive.tar.gz", FileKind::Archive),
            ("disc.vhdx", FileKind::DiscImage),
            ("photo.PSD", FileKind::Design),
            ("scan.PNG", FileKind::Image),
            ("clip.mkv", FileKind::Video),
            ("Track.FLAC", FileKind::Audio),
            ("text.ttf", FileKind::Font),
            ("app.sqlite3", FileKind::Database),
            ("Deploy.lnk", FileKind::Shortcut),
            ("build.log", FileKind::Log),
        ] {
            assert_eq!(FileKind::classify_file(name), kind, "{name}");
        }
    }

    #[test]
    fn build_files_documents_and_dotfiles_beat_their_extension() {
        for name in [
            "Makefile",
            "dockerfile",
            "CMakeLists.txt",
            "Cargo.toml",
            "package.json",
        ] {
            assert_eq!(
                FileKind::classify_file(name),
                FileKind::SourceCode,
                "{name}"
            );
        }
        for name in ["README", "readme", "README.md", "LICENSE.txt", "NOTICE"] {
            assert_eq!(FileKind::classify_file(name), FileKind::Document, "{name}");
        }
        for name in [".gitignore", ".editorconfig", ".env"] {
            assert_eq!(FileKind::classify_file(name), FileKind::Config, "{name}");
        }
    }

    #[test]
    fn multiple_dots_use_the_last_extension() {
        assert_eq!(FileKind::classify_file("archive.tar.gz"), FileKind::Archive);
        assert_eq!(
            FileKind::classify_file("notes.final.txt"),
            FileKind::Document
        );
        assert_eq!(FileKind::classify_file("trailing."), FileKind::Other);
    }

    #[test]
    fn unknown_and_missing_extensions_stay_other() {
        for name in ["data.bin", "data.xyz", "plainname", "", ".env.local"] {
            assert_eq!(FileKind::classify_file(name), FileKind::Other, "{name}");
        }
    }
}
