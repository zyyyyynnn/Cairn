// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Directory history of one drive view: the directory the canvas shows, the
//! directories Back returns to, and the directories Forward returns to.

use cairn_core::treemap::parent_directory;
use std::path::{Component, Path};

/// Browser-style history for one drive view. Drilling, a breadcrumb jump and
/// Up are all navigation steps: each records the directory it left, and
/// Forward follows those steps back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TreemapNavigation {
    current_dir: String,
    /// Directories Back returns to, oldest first.
    previous: Vec<String>,
    /// Directories left by Back and Up, newest last.
    forward: Vec<String>,
}

impl TreemapNavigation {
    pub(super) fn at_drive_root(drive: char) -> Self {
        Self {
            current_dir: format!("{}:\\", drive.to_ascii_uppercase()),
            previous: Vec::new(),
            forward: Vec::new(),
        }
    }

    /// Opens the view already inside `path` with no drill history of its own, the way navigation
    /// history restores a recorded directory: Back from there leaves the view rather than replaying
    /// steps nobody walked. `path` is a directory this view has already shown, so it is used as-is.
    pub(super) fn at_directory(drive: char, path: &str) -> Self {
        if parent_directory(path).is_none() {
            return Self::at_drive_root(drive);
        }
        Self {
            current_dir: path.to_string(),
            previous: Vec::new(),
            forward: Vec::new(),
        }
    }

    pub(super) fn current_dir(&self) -> &str {
        &self.current_dir
    }

    pub(super) fn can_go_back(&self) -> bool {
        !self.previous.is_empty()
    }

    pub(super) fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    pub(super) fn can_go_up(&self) -> bool {
        parent_directory(&self.current_dir).is_some()
    }

    /// Enters `path` as a new branch. The previous branch is no longer
    /// reachable, so its Forward targets are dropped.
    pub(super) fn enter(&mut self, path: &str) {
        if path == self.current_dir {
            return;
        }
        let left = std::mem::replace(&mut self.current_dir, path.to_string());
        self.previous.push(left);
        self.forward.clear();
    }

    /// Moves to the parent directory as a navigation step. At the drive root
    /// there is no parent and nothing changes. Returns whether the directory
    /// changed.
    pub(super) fn up(&mut self) -> bool {
        let Some(parent) = parent_directory(&self.current_dir) else {
            return false;
        };
        let left = std::mem::replace(&mut self.current_dir, parent);
        self.previous.push(left);
        self.forward.clear();
        true
    }

    /// Returns to the directory most recently left by any navigation step.
    /// Returns whether the directory changed.
    pub(super) fn back(&mut self) -> bool {
        let Some(path) = self.previous.pop() else {
            return false;
        };
        let left = std::mem::replace(&mut self.current_dir, path);
        self.forward.push(left);
        true
    }

    /// Returns to the directory most recently left by Back, keeping older
    /// departures for the Forward after it. Returns whether the directory
    /// changed.
    pub(super) fn forward(&mut self) -> bool {
        let Some(path) = self.forward.pop() else {
            return false;
        };
        let left = std::mem::replace(&mut self.current_dir, path);
        self.previous.push(left);
        true
    }
}

/// The breadcrumb segments of `path`: each label with the directory it opens.
///
/// The drive root keeps its trailing separator (`C:\`); every deeper segment
/// opens the path that reaches it without one, the form the Treemap scan and
/// history already use.
pub(super) fn path_segments(path: &str) -> Vec<(String, String)> {
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
pub(super) fn path_is_within(candidate: &str, ancestor: &str) -> bool {
    let ancestor = ancestor.trim_end_matches('\\');
    if candidate.eq_ignore_ascii_case(ancestor) {
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

pub(super) fn same_directory(left: &str, right: &str) -> bool {
    left.trim_end_matches(['\\', '/'])
        .eq_ignore_ascii_case(right.trim_end_matches(['\\', '/']))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walking_into_directories_records_each_step() {
        let mut navigation = TreemapNavigation::at_drive_root('c');
        assert_eq!(navigation.current_dir(), r"C:\");
        assert!(!navigation.can_go_back());
        assert!(!navigation.can_go_forward());
        assert!(!navigation.can_go_up());
        assert!(!navigation.back());
        assert!(!navigation.up());

        navigation.enter(r"C:\Windows");
        navigation.enter(r"C:\Windows\System32");
        assert!(navigation.can_go_back());
        assert!(navigation.can_go_up());

        assert!(navigation.back());
        assert_eq!(navigation.current_dir(), r"C:\Windows");
        assert!(navigation.back());
        assert_eq!(navigation.current_dir(), r"C:\");
        assert!(!navigation.can_go_back());
        assert!(!navigation.can_go_up());

        assert!(navigation.forward());
        assert_eq!(navigation.current_dir(), r"C:\Windows");
        assert!(navigation.forward());
        assert_eq!(navigation.current_dir(), r"C:\Windows\System32");
        assert!(!navigation.can_go_forward());
        assert!(!navigation.forward());
    }

    #[test]
    fn up_is_a_history_step_and_back_returns_to_the_directory_it_left() {
        let mut navigation = TreemapNavigation::at_drive_root('C');
        navigation.enter(r"C:\Users\Alice");

        assert!(navigation.up());
        assert_eq!(navigation.current_dir(), r"C:\Users");
        assert!(navigation.back());
        assert_eq!(navigation.current_dir(), r"C:\Users\Alice");
        assert!(navigation.forward());
        assert_eq!(navigation.current_dir(), r"C:\Users");
    }

    #[test]
    fn a_breadcrumb_jump_keeps_its_own_back_target() {
        let mut navigation = TreemapNavigation::at_drive_root('C');
        navigation.enter(r"C:\Users\Alice");
        navigation.enter(r"C:\Windows");

        assert!(navigation.back());
        assert_eq!(navigation.current_dir(), r"C:\Users\Alice");
        assert!(navigation.forward());
        assert_eq!(navigation.current_dir(), r"C:\Windows");
    }

    #[test]
    fn entering_the_current_directory_does_not_grow_history() {
        let mut navigation = TreemapNavigation::at_drive_root('C');
        navigation.enter(r"C:\Users");
        navigation.enter(r"C:\Users");

        assert!(navigation.back());
        assert_eq!(navigation.current_dir(), r"C:\");
        assert!(!navigation.can_go_back());
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

    /// Selecting a directory keeps its whole subtree in focus; only the separator boundary
    /// decides, so a sibling whose name shares a prefix is outside.
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
}
