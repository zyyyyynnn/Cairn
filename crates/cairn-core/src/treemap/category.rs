// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! What a Treemap tile holds: name-based classification of files and
//! directories, and the dominant category of a set of sized children.

/// The data category a Treemap tile encodes.
///
/// This is a data-model value, not a color: the app decides how a category is
/// painted. Known directory names carry the category of what they usually hold
/// (`Windows` is system, `Program Files` is applications, `Users` is documents,
/// and so on); an unrecognized directory starts as [`TreemapCategory::Folder`]
/// and later inherits its dominant content category. Files are classified from
/// their name once, when the item is built, so the render path only reads the
/// stored value instead of parsing names every frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TreemapCategory {
    /// Structural category for a directory whose content carries no signal yet.
    Folder,
    /// Operating-system files and directories (`Windows`, page and hibernation
    /// files, drivers, EFI binaries).
    System,
    /// Installed application trees (`Program Files`, `Applications`).
    Applications,
    Document,
    Archive,
    Image,
    Code,
    Audio,
    Video,
    /// Unrecognized extension or no extension at all. It still receives a quiet
    /// tint so no data tile collapses into neutral gray.
    #[default]
    Other,
}

/// Number of [`TreemapCategory`] variants. Content propagation sizes one stack
/// array with it and indexes it by the enum discriminant, so the order of
/// [`TreemapCategory::ALL`] matters.
const CATEGORY_COUNT: usize = 10;

/// Build files whose extension would otherwise point at another category
/// (`CMakeLists.txt` would read as a document, `Cargo.toml`/`package.json` as
/// unknown configuration).
const CODE_FILE_NAMES: &[&str] = &[
    "Makefile",
    "Dockerfile",
    "CMakeLists.txt",
    "Cargo.toml",
    "package.json",
];

/// Files named after their role rather than their format; matched on the stem so
/// `README`, `README.md` and `LICENSE.txt` all land in the same category.
const DOCUMENT_FILE_STEMS: &[&str] = &["README", "LICENSE", "NOTICE"];

/// Well-known directory names, grouped by what the directory usually contains.
/// Matching is ASCII case-insensitive and applies at any depth, so the same
/// `System32`, `Pictures` or `target` keeps its meaning wherever it appears.
/// A directory that is not listed here stays [`TreemapCategory::Folder`], which
/// is the neutral structural category — not a generic catch-all color. Its
/// category can later be inherited from its content.
const SYSTEM_DIRECTORY_NAMES: &[&str] = &[
    "Windows",
    "System32",
    "SysWOW64",
    "WinSxS",
    "DriverStore",
    "Drivers",
    "Recovery",
    "$SysReset",
    "System Volume Information",
];

const APPLICATION_DIRECTORY_NAMES: &[&str] = &[
    "Program Files",
    "Program Files (x86)",
    "Applications",
    "Apps",
];

const CODE_DIRECTORY_NAMES: &[&str] = &[
    "src",
    "tools",
    "node_modules",
    ".git",
    "target",
    ".cargo",
    "Workspace",
    "Projects",
    "repos",
];

const DOCUMENT_DIRECTORY_NAMES: &[&str] = &[
    "Users",
    "Documents",
    "Desktop",
    "Downloads",
    "Public",
    "Books",
    "Notes",
];

const IMAGE_DIRECTORY_NAMES: &[&str] = &["Pictures", "Photos", "Images"];
const AUDIO_DIRECTORY_NAMES: &[&str] = &["Music"];
const VIDEO_DIRECTORY_NAMES: &[&str] = &["Videos", "Movies"];
const ARCHIVE_DIRECTORY_NAMES: &[&str] = &["Archive", "Backups", "Backup"];
const OTHER_DIRECTORY_NAMES: &[&str] = &["ProgramData", "AppData", "Temp", "Cache", "$Recycle.Bin"];

const DOCUMENT_EXTENSIONS: &[&str] = &[
    "doc", "docx", "odt", "rtf", "txt", "md", "pdf", "epub", "mobi", "xls", "xlsx", "ppt", "pptx",
    "csv", "tsv",
];

const IMAGE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "svg", "heic", "avif", "ico", "psd",
    "cr2", "nef", "arw", "dng",
];

const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "wav", "aac", "ogg", "opus", "m4a", "wma", "aiff", "midi",
];

const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "wmv", "webm", "m4v", "flv", "mpg", "mpeg", "m2ts", "3gp",
];

const ARCHIVE_EXTENSIONS: &[&str] = &[
    "zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "zst", "lz4", "cab", "iso", "dmg", "jar",
];

/// Operating-system binaries: drivers, kernel modules, EFI images and paging
/// files. Checked before [`CODE_EXTENSIONS`] so `.sys` is system, not code.
const SYSTEM_EXTENSIONS: &[&str] = &["sys", "drv", "efi"];

/// Source, configuration and program assets. `.ts` is TypeScript here, not an
/// MPEG transport stream; `m2ts` stays the video container.
const CODE_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "c", "cpp", "h", "hpp", "cs", "rb", "php",
    "swift", "kt", "sh", "html", "css", "scss", "json", "toml", "yaml", "yml", "xml", "sql", "exe",
    "dll", "msi", "msix", "com", "bat", "cmd", "ps1",
];

impl TreemapCategory {
    /// Every category in discriminant order. Content propagation indexes its
    /// per-directory byte totals with the discriminant, so variants must not be
    /// reordered without updating `CATEGORY_COUNT`.
    pub const ALL: [Self; CATEGORY_COUNT] = [
        Self::Folder,
        Self::System,
        Self::Applications,
        Self::Document,
        Self::Archive,
        Self::Image,
        Self::Code,
        Self::Audio,
        Self::Video,
        Self::Other,
    ];

    /// Number of categories, for fixed-size category filters in the view.
    pub const COUNT: usize = CATEGORY_COUNT;

    /// Classifies one directory entry.
    ///
    /// `is_dir` wins over the name, so a folder called `photos.jpg` is still
    /// classified by its directory name (unknown names remain
    /// [`Self::Folder`]). For files the last dot-separated segment wins
    /// (`archive.tar.gz` is an archive), comparison is ASCII case-insensitive,
    /// and a name whose only dot is the leading one (`.gitignore`, `.env`) is
    /// treated as a tool configuration file.
    pub fn classify(name: &str, is_dir: bool) -> Self {
        if is_dir {
            return Self::from_directory_name(name).unwrap_or(Self::Folder);
        }

        if CODE_FILE_NAMES
            .iter()
            .any(|known| name.eq_ignore_ascii_case(known))
        {
            return Self::Code;
        }

        // A bare dotfile is configuration, not an unknown binary. A dotted name
        // with a real extension still goes through the extension table below.
        if let Some(rest) = name.strip_prefix('.') {
            if !rest.is_empty() && !rest.contains('.') {
                return Self::Code;
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
            Some((_, extension)) if !extension.is_empty() => {
                Self::from_extension(extension).unwrap_or_default()
            }
            _ => Self::Other,
        }
    }

    fn from_directory_name(name: &str) -> Option<Self> {
        let matches = |table: &[&str]| table.iter().any(|known| name.eq_ignore_ascii_case(known));

        if matches(SYSTEM_DIRECTORY_NAMES) {
            Some(Self::System)
        } else if matches(APPLICATION_DIRECTORY_NAMES) {
            Some(Self::Applications)
        } else if matches(CODE_DIRECTORY_NAMES) {
            Some(Self::Code)
        } else if matches(DOCUMENT_DIRECTORY_NAMES) {
            Some(Self::Document)
        } else if matches(IMAGE_DIRECTORY_NAMES) {
            Some(Self::Image)
        } else if matches(AUDIO_DIRECTORY_NAMES) {
            Some(Self::Audio)
        } else if matches(VIDEO_DIRECTORY_NAMES) {
            Some(Self::Video)
        } else if matches(ARCHIVE_DIRECTORY_NAMES) {
            Some(Self::Archive)
        } else if matches(OTHER_DIRECTORY_NAMES) {
            Some(Self::Other)
        } else {
            None
        }
    }

    fn from_extension(extension: &str) -> Option<Self> {
        let matches = |table: &[&str]| {
            table
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        };

        if matches(SYSTEM_EXTENSIONS) {
            Some(Self::System)
        } else if matches(DOCUMENT_EXTENSIONS) {
            Some(Self::Document)
        } else if matches(IMAGE_EXTENSIONS) {
            Some(Self::Image)
        } else if matches(AUDIO_EXTENSIONS) {
            Some(Self::Audio)
        } else if matches(VIDEO_EXTENSIONS) {
            Some(Self::Video)
        } else if matches(ARCHIVE_EXTENSIONS) {
            Some(Self::Archive)
        } else if matches(CODE_EXTENSIONS) {
            Some(Self::Code)
        } else {
            None
        }
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

    /// The content category holding the most bytes. Structural
    /// [`TreemapCategory::Folder`] and [`TreemapCategory::Other`] never win; a
    /// tie keeps the category listed first in [`TreemapCategory::ALL`]. `None`
    /// when no content category holds a byte.
    pub(super) fn dominant(&self) -> Option<TreemapCategory> {
        let mut dominant = None;
        let mut dominant_bytes = 0u64;
        for category in TreemapCategory::ALL {
            if matches!(category, TreemapCategory::Folder | TreemapCategory::Other) {
                continue;
            }
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
    fn directory_names_win_over_file_extensions() {
        assert_eq!(
            TreemapCategory::classify("photos.jpg", true),
            TreemapCategory::Folder
        );
        // `.git` is a known directory name, not a dotfile extension.
        assert_eq!(
            TreemapCategory::classify(".git", true),
            TreemapCategory::Code
        );
        assert_eq!(
            TreemapCategory::classify("archive.tar.gz", true),
            TreemapCategory::Folder
        );
    }

    #[test]
    fn known_directory_names_carry_their_category() {
        for (name, expected) in [
            ("Program Files", TreemapCategory::Applications),
            ("program files (x86)", TreemapCategory::Applications),
            ("Applications", TreemapCategory::Applications),
            ("Apps", TreemapCategory::Applications),
            ("Windows", TreemapCategory::System),
            ("System32", TreemapCategory::System),
            ("SysWOW64", TreemapCategory::System),
            ("WinSxS", TreemapCategory::System),
            ("DriverStore", TreemapCategory::System),
            ("Drivers", TreemapCategory::System),
            ("Recovery", TreemapCategory::System),
            ("$SysReset", TreemapCategory::System),
            ("System Volume Information", TreemapCategory::System),
            ("src", TreemapCategory::Code),
            ("tools", TreemapCategory::Code),
            ("node_modules", TreemapCategory::Code),
            (".git", TreemapCategory::Code),
            ("target", TreemapCategory::Code),
            (".cargo", TreemapCategory::Code),
            ("Workspace", TreemapCategory::Code),
            ("workspace", TreemapCategory::Code),
            ("Projects", TreemapCategory::Code),
            ("repos", TreemapCategory::Code),
            ("Users", TreemapCategory::Document),
            ("Documents", TreemapCategory::Document),
            ("Desktop", TreemapCategory::Document),
            ("Downloads", TreemapCategory::Document),
            ("Public", TreemapCategory::Document),
            ("Books", TreemapCategory::Document),
            ("Notes", TreemapCategory::Document),
            ("Pictures", TreemapCategory::Image),
            ("Photos", TreemapCategory::Image),
            ("Images", TreemapCategory::Image),
            ("Music", TreemapCategory::Audio),
            ("Videos", TreemapCategory::Video),
            ("Movies", TreemapCategory::Video),
            ("Archive", TreemapCategory::Archive),
            ("Backups", TreemapCategory::Archive),
            ("Backup", TreemapCategory::Archive),
            ("ProgramData", TreemapCategory::Other),
            ("AppData", TreemapCategory::Other),
            ("Temp", TreemapCategory::Other),
            ("Cache", TreemapCategory::Other),
            ("$Recycle.Bin", TreemapCategory::Other),
        ] {
            assert_eq!(
                TreemapCategory::classify(name, true),
                expected,
                "{name} must classify as {expected:?}"
            );
        }

        // An unrecognized directory stays structural: it must not borrow a
        // category color just because every directory has to have one.
        for name in ["C:", "C:\\", "ZYN", "New Folder"] {
            assert_eq!(
                TreemapCategory::classify(name, true),
                TreemapCategory::Folder,
                "{name} must stay a neutral folder"
            );
        }
    }

    #[test]
    fn extension_match_ignores_ascii_case() {
        assert_eq!(
            TreemapCategory::classify("PHOTO.JPG", false),
            TreemapCategory::Image
        );
        assert_eq!(
            TreemapCategory::classify("Track.FLAC", false),
            TreemapCategory::Audio
        );
        assert_eq!(
            TreemapCategory::classify("KERNEL32.DLL", false),
            TreemapCategory::Code
        );
        for name in ["pagefile.sys", "hiberfil.sys", "swapfile.sys"] {
            assert_eq!(
                TreemapCategory::classify(name, false),
                TreemapCategory::System,
                "{name} must be a system file"
            );
        }
        assert_eq!(
            TreemapCategory::classify("PAGEFILE.SYS", false),
            TreemapCategory::System
        );
        assert_eq!(
            TreemapCategory::classify("nvlddmkm.drv", false),
            TreemapCategory::System
        );
        assert_eq!(
            TreemapCategory::classify("boot.efi", false),
            TreemapCategory::System
        );
    }

    #[test]
    fn multiple_dots_use_the_last_extension() {
        assert_eq!(
            TreemapCategory::classify("archive.tar.gz", false),
            TreemapCategory::Archive
        );
        assert_eq!(
            TreemapCategory::classify("notes.final.txt", false),
            TreemapCategory::Document
        );
        assert_eq!(
            TreemapCategory::classify("trailing.", false),
            TreemapCategory::Other
        );
    }

    #[test]
    fn known_file_names_beat_their_extension() {
        for name in [
            "Makefile",
            "dockerfile",
            "CMakeLists.txt",
            "Cargo.toml",
            "package.json",
        ] {
            assert_eq!(
                TreemapCategory::classify(name, false),
                TreemapCategory::Code,
                "{name} must be code"
            );
        }
        for name in ["README", "readme", "README.md", "LICENSE.txt", "NOTICE"] {
            assert_eq!(
                TreemapCategory::classify(name, false),
                TreemapCategory::Document,
                "{name} must be a document"
            );
        }
        // A name whose only dot is the leading one is tool configuration.
        assert_eq!(
            TreemapCategory::classify(".gitignore", false),
            TreemapCategory::Code
        );
        assert_eq!(
            TreemapCategory::classify(".editorconfig", false),
            TreemapCategory::Code
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
    fn dominant_category_ignores_structural_bytes_and_keeps_the_first_tie() {
        let mut bytes = CategoryBytes::default();
        assert_eq!(bytes.dominant(), None);

        bytes.add(TreemapCategory::Folder, 10_000);
        bytes.add(TreemapCategory::Other, 10_000);
        assert_eq!(bytes.dominant(), None, "structural bytes never dominate");

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
}
