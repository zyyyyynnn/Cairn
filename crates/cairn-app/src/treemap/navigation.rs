// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Directory history of one drive view: the directory the canvas shows, the
//! directories Back returns to, and the directories Forward returns to.

use cairn_core::treemap::parent_directory;

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
}
