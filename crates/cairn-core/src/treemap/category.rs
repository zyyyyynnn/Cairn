// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! What a Treemap tile holds: the category a tile encodes, the file kind behind
//! a file's name, and the dominant category of a set of sized children.

/// The data category a Treemap tile encodes.
///
/// This is a data-model value, not a color: the app decides how a category is
/// painted. The variant order defines the palette slots and the legend order.
///
/// Only files are classified from their name. A directory starts as
/// [`TreemapCategory::Other`] and later inherits the category holding most of
/// its bytes, so a folder of photos is images whatever the folder is called.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TreemapCategory {
    /// Operating-system binaries: drivers, system resources and paging files.
    System,
    /// Executables, installers and shared libraries.
    Programs,
    /// Text, office files, source code, configuration and scripts.
    Documents,
    Image,
    Video,
    Audio,
    /// Compressed archives and disk images.
    Archive,
    /// Unrecognized extension or no extension at all. A directory whose content
    /// carries no category signal stays here; it still receives a quiet tint so
    /// no data tile collapses into neutral gray.
    #[default]
    Other,
}

/// Number of [`TreemapCategory`] variants. Content propagation sizes one stack
/// array with it and indexes it by the enum discriminant, so the order of
/// [`TreemapCategory::ALL`] matters.
const CATEGORY_COUNT: usize = 8;

/// The concrete kind of a file, resolved from its name. A kind is finer than a
/// category: the app paints a row or tile with the kind's icon, while the
/// Treemap tallies bytes by category.
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
    /// The Treemap category this kind is tallied under.
    pub fn category(self) -> TreemapCategory {
        match self {
            Self::Driver | Self::SystemFile => TreemapCategory::System,
            Self::Application | Self::Installer | Self::Library => TreemapCategory::Programs,
            Self::Document
            | Self::Presentation
            | Self::Spreadsheet
            | Self::SourceCode
            | Self::Config
            | Self::Script => TreemapCategory::Documents,
            Self::Archive | Self::DiscImage => TreemapCategory::Archive,
            Self::Image | Self::Design => TreemapCategory::Image,
            Self::Video => TreemapCategory::Video,
            Self::Audio => TreemapCategory::Audio,
            Self::Font | Self::Database | Self::Shortcut | Self::Log | Self::Other => {
                TreemapCategory::Other
            }
        }
    }

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

impl TreemapCategory {
    /// Every category in discriminant order. Content propagation indexes its
    /// per-directory byte totals with the discriminant, and the palette and the
    /// legend follow the same order.
    pub const ALL: [Self; CATEGORY_COUNT] = [
        Self::System,
        Self::Programs,
        Self::Documents,
        Self::Image,
        Self::Video,
        Self::Audio,
        Self::Archive,
        Self::Other,
    ];

    /// Number of categories, for fixed-size category filters in the view.
    pub const COUNT: usize = CATEGORY_COUNT;

    /// Locale-independent identity of a category, used by a summary block's path so the same
    /// category keeps one identity across canvas sizes. It is never translated and never derived
    /// from the display label.
    pub fn machine_key(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Programs => "programs",
            Self::Documents => "documents",
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Archive => "archive",
            Self::Other => "other",
        }
    }

    /// Classifies one directory entry.
    ///
    /// A directory carries no category from its name: it starts at
    /// [`Self::Other`] and content propagation later assigns the category
    /// holding most of its bytes. A file's kind is resolved from its name by
    /// [`FileKind::classify_file`] and then mapped through
    /// [`FileKind::category`].
    pub fn classify(name: &str, is_dir: bool) -> Self {
        if is_dir {
            return Self::Other;
        }
        FileKind::classify_file(name).category()
    }
}

/// Bytes per category, accumulated in one fixed-size stack array so content
/// propagation and summary blocks add no heap allocation per node.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct CategoryBytes([u64; CATEGORY_COUNT]);

impl CategoryBytes {
    pub(super) fn add(&mut self, category: TreemapCategory, bytes: u64) {
        let slot = &mut self.0[category as usize];
        *slot = slot.saturating_add(bytes);
    }

    /// The category holding the most bytes, [`TreemapCategory::Other`] included.
    /// A tie keeps the category listed first in [`TreemapCategory::ALL`].
    /// `None` when no category holds a byte.
    pub(super) fn dominant(&self) -> Option<TreemapCategory> {
        let mut dominant = None;
        let mut dominant_bytes = 0u64;
        for category in TreemapCategory::ALL {
            let bytes = self.0[category as usize];
            if bytes > dominant_bytes {
                dominant = Some(category);
                dominant_bytes = bytes;
            }
        }
        dominant
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_name_never_decides_its_category() {
        for name in [
            "Windows",
            "Program Files",
            "Users",
            "Pictures",
            "Music",
            "Videos",
            "Backups",
            ".git",
            "photos.jpg",
            "archive.tar.gz",
        ] {
            assert_eq!(
                TreemapCategory::classify(name, true),
                TreemapCategory::Other,
                "{name} must start uncategorized"
            );
        }
    }

    #[test]
    fn extension_tables_cover_every_kind() {
        for (kind, extensions) in EXTENSION_TABLE {
            for extension in *extensions {
                let name = format!("sample.{extension}");
                assert_eq!(
                    FileKind::classify_file(&name),
                    *kind,
                    "{name} must classify as {kind:?}"
                );
                let upper = format!("SAMPLE.{}", extension.to_ascii_uppercase());
                assert_eq!(
                    FileKind::classify_file(&upper),
                    *kind,
                    "{upper} must classify as {kind:?}"
                );
            }
        }
    }

    #[test]
    fn file_kinds_map_to_the_eight_categories() {
        for (kind, category) in [
            (FileKind::Driver, TreemapCategory::System),
            (FileKind::SystemFile, TreemapCategory::System),
            (FileKind::Application, TreemapCategory::Programs),
            (FileKind::Installer, TreemapCategory::Programs),
            (FileKind::Library, TreemapCategory::Programs),
            (FileKind::Document, TreemapCategory::Documents),
            (FileKind::Presentation, TreemapCategory::Documents),
            (FileKind::Spreadsheet, TreemapCategory::Documents),
            (FileKind::SourceCode, TreemapCategory::Documents),
            (FileKind::Config, TreemapCategory::Documents),
            (FileKind::Script, TreemapCategory::Documents),
            (FileKind::Archive, TreemapCategory::Archive),
            (FileKind::DiscImage, TreemapCategory::Archive),
            (FileKind::Image, TreemapCategory::Image),
            (FileKind::Design, TreemapCategory::Image),
            (FileKind::Video, TreemapCategory::Video),
            (FileKind::Audio, TreemapCategory::Audio),
            (FileKind::Font, TreemapCategory::Other),
            (FileKind::Database, TreemapCategory::Other),
            (FileKind::Shortcut, TreemapCategory::Other),
            (FileKind::Log, TreemapCategory::Other),
            (FileKind::Other, TreemapCategory::Other),
        ] {
            assert_eq!(kind.category(), category, "{kind:?} maps to {category:?}");
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
            assert_eq!(
                FileKind::classify_file(name),
                kind,
                "{name} must classify as {kind:?}"
            );
            assert_eq!(
                TreemapCategory::classify(name, false),
                kind.category(),
                "{name} must take its kind's category"
            );
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
                "{name} must be source code"
            );
            assert_eq!(
                TreemapCategory::classify(name, false),
                TreemapCategory::Documents
            );
        }
        for name in ["README", "readme", "README.md", "LICENSE.txt", "NOTICE"] {
            assert_eq!(
                FileKind::classify_file(name),
                FileKind::Document,
                "{name} must be a document"
            );
        }
        for name in [".gitignore", ".editorconfig", ".env"] {
            assert_eq!(
                FileKind::classify_file(name),
                FileKind::Config,
                "{name} must be configuration"
            );
        }
    }

    #[test]
    fn multiple_dots_use_the_last_extension() {
        assert_eq!(
            TreemapCategory::classify("archive.tar.gz", false),
            TreemapCategory::Archive
        );
        assert_eq!(
            TreemapCategory::classify("notes.final.txt", false),
            TreemapCategory::Documents
        );
        assert_eq!(
            TreemapCategory::classify("trailing.", false),
            TreemapCategory::Other
        );
    }

    #[test]
    fn unknown_and_missing_extensions_stay_other() {
        for name in ["data.bin", "data.xyz", "plainname", "", ".env.local"] {
            assert_eq!(
                TreemapCategory::classify(name, false),
                TreemapCategory::Other,
                "{name} must stay uncategorized"
            );
        }
        assert_eq!(TreemapCategory::default(), TreemapCategory::Other);
    }

    #[test]
    fn dominant_considers_every_category_and_keeps_the_first_tie() {
        let mut bytes = CategoryBytes::default();
        assert_eq!(bytes.dominant(), None);

        bytes.add(TreemapCategory::Other, 100);
        assert_eq!(
            bytes.dominant(),
            Some(TreemapCategory::Other),
            "unknown bytes are still a measured total"
        );

        bytes.add(TreemapCategory::Video, 300);
        bytes.add(TreemapCategory::Image, 300);
        assert_eq!(
            bytes.dominant(),
            Some(TreemapCategory::Image),
            "a tie keeps the category listed first"
        );

        bytes.add(TreemapCategory::Video, 1);
        assert_eq!(bytes.dominant(), Some(TreemapCategory::Video));
    }

    #[test]
    fn category_order_is_the_documented_one() {
        assert_eq!(TreemapCategory::COUNT, 8);
        assert_eq!(
            TreemapCategory::ALL,
            [
                TreemapCategory::System,
                TreemapCategory::Programs,
                TreemapCategory::Documents,
                TreemapCategory::Image,
                TreemapCategory::Video,
                TreemapCategory::Audio,
                TreemapCategory::Archive,
                TreemapCategory::Other,
            ]
        );
        let keys: Vec<&str> = TreemapCategory::ALL
            .iter()
            .map(|category| category.machine_key())
            .collect();
        assert_eq!(
            keys,
            [
                "system",
                "programs",
                "documents",
                "image",
                "video",
                "audio",
                "archive",
                "other",
            ]
        );
    }
}
