// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Drive-path helpers for Treemap navigation. Paths are Windows drive paths
//! (`C:\`, `C:\Windows`) and are parsed as text, so the rules are identical on
//! every host.

use std::path::{Component, Path};

/// Whether `path` is a drive root (`C:\` or `C:`).
pub fn is_drive_root(path: &str) -> bool {
    let trimmed = path.trim_end_matches(['\\', '/']);
    trimmed.len() == 2
        && trimmed.as_bytes()[0].is_ascii_alphabetic()
        && trimmed.as_bytes()[1] == b':'
}

/// The directory that contains `path`, or `None` at a drive root.
///
/// The parent of a top-level directory is the drive root with its trailing
/// separator (`C:\Users` → `C:\`), the form every Treemap root uses.
pub fn parent_directory(path: &str) -> Option<String> {
    let trimmed = path.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() || is_drive_root(trimmed) {
        return None;
    }
    let (parent, _) = trimmed.rsplit_once(['\\', '/'])?;
    if is_drive_root(parent) {
        Some(format!("{parent}\\"))
    } else if parent.is_empty() {
        None
    } else {
        Some(parent.to_string())
    }
}

/// The breadcrumb segments of `path`: each label with the directory it opens.
///
/// The drive root keeps its trailing separator (`C:\`); every deeper segment
/// opens the path that reaches it without one, the form the Treemap scan and
/// history already use.
pub fn path_segments(path: &str) -> Vec<(String, String)> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut components = Path::new(path).components().peekable();

    if let Some(Component::Prefix(prefix)) = components.peek() {
        let label = prefix.as_os_str().to_string_lossy().into_owned();
        current.push_str(&label);
        components.next();
        if matches!(components.peek(), Some(Component::RootDir)) {
            current.push('\\');
            components.next();
        }
        segments.push((label, current.clone()));
    }

    for component in components {
        if let Component::Normal(name) = component {
            if !current.ends_with('\\') {
                current.push('\\');
            }
            current.push_str(&name.to_string_lossy());
            segments.push((name.to_string_lossy().into_owned(), current.clone()));
        }
    }
    segments
}

/// Whether `candidate` is `ancestor` itself or a path beneath it. Comparison is ASCII
/// case-insensitive and requires a separator boundary, so `C:\Windows` does not contain
/// `C:\WindowsApps`.
pub fn path_is_within(candidate: &str, ancestor: &str) -> bool {
    let ancestor = ancestor.trim_end_matches(['\\', '/']);
    if candidate
        .trim_end_matches(['\\', '/'])
        .eq_ignore_ascii_case(ancestor)
    {
        return true;
    }
    if candidate.len() <= ancestor.len() {
        return false;
    }
    let Some(prefix) = candidate.get(..ancestor.len()) else {
        return false;
    };
    if !prefix.eq_ignore_ascii_case(ancestor) {
        return false;
    }
    let rest = &candidate[ancestor.len()..];
    rest.starts_with('\\') || rest.starts_with('/') || rest.starts_with("::")
}

/// Whether `left` and `right` name the same directory, ignoring trailing separators and ASCII case.
pub fn same_directory(left: &str, right: &str) -> bool {
    left.trim_end_matches(['\\', '/'])
        .eq_ignore_ascii_case(right.trim_end_matches(['\\', '/']))
}

/// Returns the path segments of `target_path` relative to `base_dir` (empty when `target_path`
/// equals `base_dir`), or `None` when `target_path` is not at or below `base_dir`.
pub fn relative_segments<'a>(base_dir: &str, target_path: &'a str) -> Option<Vec<&'a str>> {
    let base_parts: Vec<&str> = base_dir
        .split(['\\', '/'])
        .filter(|s| !s.is_empty())
        .collect();
    let target_parts: Vec<&str> = target_path
        .split(['\\', '/'])
        .filter(|s| !s.is_empty())
        .collect();

    if target_parts.len() < base_parts.len() {
        return None;
    }

    for (i, (b, t)) in base_parts.iter().zip(target_parts.iter()).enumerate() {
        if i == 0 {
            let b_norm = b.trim_end_matches(':');
            let t_norm = t.trim_end_matches(':');
            if !b_norm.eq_ignore_ascii_case(t_norm) {
                return None;
            }
        } else if !b.eq_ignore_ascii_case(t) {
            return None;
        }
    }

    Some(target_parts[base_parts.len()..].to_vec())
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
        assert!(is_drive_root("C:/"));
        assert!(!is_drive_root("::"));
        assert!(!is_drive_root("1:"));
        assert!(!is_drive_root("?:\\"));
        assert!(!is_drive_root("C:\\Windows"));
        assert!(!is_drive_root("C:\\Windows\\System32"));
        assert!(!is_drive_root(""));
    }

    #[test]
    fn a_drive_root_has_no_parent() {
        assert_eq!(parent_directory("C:\\"), None);
        assert_eq!(parent_directory("C:"), None);
        assert_eq!(parent_directory("C:/"), None);
        assert_eq!(parent_directory("::"), None);
        assert_eq!(parent_directory(""), None);
    }

    #[test]
    fn a_top_level_directory_returns_to_the_drive_root() {
        assert_eq!(parent_directory("C:\\Users"), Some("C:\\".to_string()));
        assert_eq!(parent_directory("C:\\Users\\"), Some("C:\\".to_string()));
        assert_eq!(parent_directory("C:/Users"), Some("C:\\".to_string()));
    }

    #[test]
    fn nested_directories_drop_their_last_segment() {
        assert_eq!(
            parent_directory("C:\\Windows\\System32"),
            Some("C:\\Windows".to_string())
        );
        assert_eq!(
            parent_directory("c:\\Users\\Alice\\AppData\\Local"),
            Some("c:\\Users\\Alice\\AppData".to_string())
        );
    }

    #[test]
    fn a_relative_name_has_no_drive_parent() {
        assert_eq!(parent_directory("Windows"), None);
        assert_eq!(parent_directory("\\Windows"), None);
    }

    #[test]
    fn path_segments_name_each_ancestor_and_the_directory_it_opens() {
        assert_eq!(
            path_segments(r"C:\"),
            vec![("C:".to_string(), r"C:\".to_string())]
        );
        assert_eq!(
            path_segments(r"C:\Users\Alice"),
            vec![
                ("C:".to_string(), r"C:\".to_string()),
                ("Users".to_string(), r"C:\Users".to_string()),
                ("Alice".to_string(), r"C:\Users\Alice".to_string()),
            ]
        );
        assert_eq!(
            path_segments(r"C:\Users\"),
            vec![
                ("C:".to_string(), r"C:\".to_string()),
                ("Users".to_string(), r"C:\Users".to_string()),
            ]
        );
    }

    #[test]
    fn a_path_contains_itself_and_its_descendants_only() {
        assert!(path_is_within(r"C:\Windows", r"C:\Windows"));
        assert!(path_is_within(r"C:\Windows\System32", r"C:\Windows"));
        assert!(path_is_within(r"C:\Windows::<tail:Other>", r"C:\Windows"));
        assert!(!path_is_within(r"C:\Windows", r"C:\Windows\System32"));
        assert!(path_is_within(
            r"C:\Users\Alice\AppData",
            r"C:\Users\Alice\AppData\"
        ));
        assert!(!path_is_within(r"C:\WindowsApps", r"C:\Windows"));
        assert!(!path_is_within(r"C:\Win", r"C:\Windows"));
    }

    #[test]
    fn same_directory_compares_drive_roots_and_trailing_separators() {
        assert!(same_directory(r"C:\", r"C:\"));
        assert!(same_directory(r"C:\users", r"C:\Users\\"));
        assert!(!same_directory(r"C:\", r"C:\Windows"));
        assert!(!same_directory(r"C:\a", r"D:\a"));
    }

    #[test]
    fn path_is_within_handles_both_separators_and_aggregates() {
        assert!(path_is_within("C:\\Users\\Alice", "C:\\Users"));
        assert!(path_is_within("C:/Users/Alice", "C:/Users"));
        assert!(path_is_within(
            "C:/Users/Alice::<tail:documents>",
            "C:/Users/Alice"
        ));
        assert!(path_is_within(
            "C:\\Users\\Alice::<tail:documents>",
            "C:\\Users\\Alice"
        ));
        assert!(!path_is_within("C:\\UsersExtended", "C:\\Users"));
        assert!(!path_is_within("C:/UsersExtended", "C:/Users"));
    }

    #[test]
    fn relative_segments_extracts_strict_descendants_and_self() {
        assert_eq!(relative_segments(r"C:\", r"C:\"), Some(Vec::<&str>::new()));
        assert_eq!(relative_segments(r"c:\", r"C:"), Some(Vec::<&str>::new()));
        assert_eq!(
            relative_segments(r"C:\Users", r"C:\Users\Alice\Documents"),
            Some(vec!["Alice", "Documents"])
        );
        assert_eq!(relative_segments(r"C:\Users", r"D:\Users\Alice"), None);
        assert_eq!(relative_segments(r"C:\Users\Alice", r"C:\Users"), None);
    }
}
