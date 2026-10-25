// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Drive-path helpers for Treemap navigation. Paths are Windows drive paths
//! (`C:\`, `C:\Windows`) and are parsed as text, so the rules are identical on
//! every host.

/// Whether `path` is a drive root (`C:\` or `C:`).
pub fn is_drive_root(path: &str) -> bool {
    let trimmed = path.trim_end_matches('\\');
    trimmed.len() == 2 && trimmed.ends_with(':')
}

/// The directory that contains `path`, or `None` at a drive root.
///
/// The parent of a top-level directory is the drive root with its trailing
/// separator (`C:\Users` → `C:\`), the form every Treemap root uses.
pub fn parent_directory(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches('\\');
    if trimmed.is_empty() || is_drive_root(trimmed) {
        return None;
    }
    let (parent, _) = trimmed.rsplit_once('\\')?;
    if is_drive_root(parent) {
        Some(format!("{parent}\\"))
    } else if parent.is_empty() {
        None
    } else {
        Some(parent.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_roots_are_recognized_with_and_without_a_separator() {
        assert!(is_drive_root("C:\\"));
        assert!(is_drive_root("C:"));
        assert!(is_drive_root("d:\\"));
        assert!(is_drive_root("D:"));
        assert!(!is_drive_root("C:\\Windows"));
        assert!(!is_drive_root("C:\\Windows\\System32"));
        assert!(!is_drive_root(""));
    }

    #[test]
    fn a_drive_root_has_no_parent() {
        assert_eq!(parent_directory("C:\\"), None);
        assert_eq!(parent_directory("C:"), None);
        assert_eq!(parent_directory(""), None);
    }

    #[test]
    fn a_top_level_directory_returns_to_the_drive_root() {
        assert_eq!(parent_directory("C:\\Users"), Some("C:\\".to_string()));
        assert_eq!(parent_directory("C:\\Users\\"), Some("C:\\".to_string()));
    }

    #[test]
    fn nested_directories_drop_their_last_segment() {
        assert_eq!(
            parent_directory("C:\\Windows\\System32"),
            Some("C:\\Windows".to_string())
        );
        assert_eq!(
            parent_directory("c:\\Users\\ZYN\\AppData\\Local"),
            Some("c:\\Users\\ZYN\\AppData".to_string())
        );
    }

    #[test]
    fn a_relative_name_has_no_drive_parent() {
        assert_eq!(parent_directory("Windows"), None);
        assert_eq!(parent_directory("\\Windows"), None);
    }
}
