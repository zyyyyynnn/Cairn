// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Off-thread directory menu enumeration and generation-guarded cancellation for Treemap
//! breadcrumbs.

/// Maximum directory entries shown in one breadcrumb dropdown before displaying a truncation note.
pub(crate) const PATH_MENU_LIMIT: usize = 100;

/// Tracks the currently pending asynchronous breadcrumb directory read so stale scans or repeated
/// clicks cancel cleanly without blocking or corrupting UI state.
#[derive(Default, Debug)]
pub(crate) struct DirectoryMenuLoader {
    epoch: u64,
    pending_segment: Option<String>,
}

impl DirectoryMenuLoader {
    pub(crate) fn is_pending_for(&self, segment: &str) -> bool {
        self.pending_segment.as_deref() == Some(segment)
    }

    pub(crate) fn begin(&mut self, segment: &str) -> u64 {
        self.epoch = self.epoch.wrapping_add(1);
        self.pending_segment = Some(segment.to_string());
        self.epoch
    }

    pub(crate) fn cancel(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.pending_segment = None;
    }

    pub(crate) fn accept(&mut self, epoch: u64) -> bool {
        if self.epoch == epoch {
            self.pending_segment = None;
            true
        } else {
            false
        }
    }
}

/// Enumerates eligible non-hidden subdirectories for `directory` off the UI thread, capped at
/// [`PATH_MENU_LIMIT`] + 1 entries so truncation can be detected without reading unbounded trees.
pub(crate) fn list_menu_directories(directory: &str) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let base = directory.trim_end_matches(['\\', '/']);
    let mut directories = Vec::with_capacity(PATH_MENU_LIMIT + 1);
    for entry in entries.flatten() {
        if directories.len() > PATH_MENU_LIMIT {
            break;
        }
        if !menu_directory(&entry) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        directories.push((name.clone(), format!("{base}\\{name}")));
    }
    directories.sort_by(|a, b| cairn_core::sort::natural_cmp(&a.0, &b.0));
    directories
}

#[cfg(windows)]
fn menu_directory(entry: &std::fs::DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
    entry
        .metadata()
        .map(|metadata| {
            let attributes = metadata.file_attributes();
            attributes & FILE_ATTRIBUTE_DIRECTORY != 0
                && attributes & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) == 0
        })
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn menu_directory(entry: &std::fs::DirEntry) -> bool {
    entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::{list_menu_directories, DirectoryMenuLoader};

    #[test]
    fn stale_epoch_directory_listing_is_ignored_after_toggle_or_navigation() {
        let mut loader = DirectoryMenuLoader::default();
        let epoch_a = loader.begin(r"C:\Windows");
        assert!(loader.is_pending_for(r"C:\Windows"));

        let epoch_b = loader.begin(r"C:\Users");
        assert!(!loader.is_pending_for(r"C:\Windows"));
        assert!(loader.is_pending_for(r"C:\Users"));

        assert!(!loader.accept(epoch_a));
        assert!(loader.accept(epoch_b));
        assert!(!loader.is_pending_for(r"C:\Users"));

        let epoch_c = loader.begin(r"C:\Program Files");
        loader.cancel();
        assert!(!loader.is_pending_for(r"C:\Program Files"));
        assert!(!loader.accept(epoch_c));
    }

    #[test]
    fn list_menu_directories_returns_naturally_sorted_subdirectories_without_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("menu");
        std::fs::create_dir_all(root.join("dir10")).unwrap();
        std::fs::create_dir_all(root.join("dir2")).unwrap();
        std::fs::write(root.join("plain.txt"), b"hello").unwrap();

        let listed = list_menu_directories(root.to_str().unwrap());
        let names: Vec<&str> = listed.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, vec!["dir2", "dir10"]);
    }
}
