// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Directory history of one drive view: the directory the canvas shows and
//! the directories Forward returns to.

use cairn_core::treemap::parent_directory;

/// Back goes to the parent directory; Forward replays the directories Back
/// left, newest first. Entering a directory starts a new branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TreemapNavigation {
    current_dir: String,
    /// Directories left by Back, newest last.
    forward: Vec<String>,
}

impl TreemapNavigation {
    pub(super) fn at_drive_root(drive: char) -> Self {
        Self {
            current_dir: format!("{}:\\", drive.to_ascii_uppercase()),
            forward: Vec::new(),
        }
    }

    pub(super) fn current_dir(&self) -> &str {
        &self.current_dir
    }

    pub(super) fn can_go_back(&self) -> bool {
        parent_directory(&self.current_dir).is_some()
    }

    pub(super) fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// Enters `path` as a new branch. The previous branch is no longer
    /// reachable, so its Forward targets are dropped.
    pub(super) fn enter(&mut self, path: &str) {
        self.forward.clear();
        self.current_dir = path.to_string();
    }

    /// Moves to the parent directory and remembers the one being left. At the
    /// drive root there is no parent and nothing changes. Returns whether the
    /// directory changed.
    pub(super) fn back(&mut self) -> bool {
        let Some(parent) = parent_directory(&self.current_dir) else {
            return false;
        };
        let left = std::mem::replace(&mut self.current_dir, parent);
        self.forward.push(left);
        true
    }

    /// Returns to the directory most recently left by [`Self::back`], keeping
    /// older departures for the Forward after it. Returns whether the
    /// directory changed.
    pub(super) fn forward(&mut self) -> bool {
        let Some(path) = self.forward.pop() else {
            return false;
        };
        self.current_dir = path;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Back at the drive root has no parent: it neither navigates nor records a
    /// forward target, while every nested level records the directory it leaves.
    #[test]
    fn back_records_forward_only_below_the_drive_root() {
        let mut navigation = TreemapNavigation::at_drive_root('c');
        assert_eq!(navigation.current_dir(), r"C:\");
        assert!(!navigation.can_go_back());
        assert!(!navigation.back());
        assert!(!navigation.can_go_forward());

        navigation.enter(r"C:\Windows\System32");
        assert!(navigation.back());
        assert_eq!(navigation.current_dir(), r"C:\Windows");
        assert!(navigation.can_go_forward());
    }

    /// Forward replays departures newest-first and leaves the older ones on the
    /// stack, so backing out twice needs two Forward steps to return.
    #[test]
    fn forward_replays_backed_out_directories_in_reverse_order() {
        let mut navigation = TreemapNavigation::at_drive_root('C');
        navigation.enter(r"C:\Users\Alice");

        assert!(navigation.back());
        assert_eq!(navigation.current_dir(), r"C:\Users");
        assert!(navigation.back());
        assert_eq!(navigation.current_dir(), r"C:\");

        assert!(navigation.forward());
        assert_eq!(navigation.current_dir(), r"C:\Users");
        assert!(navigation.forward());
        assert_eq!(navigation.current_dir(), r"C:\Users\Alice");
        assert!(!navigation.can_go_forward());
        assert!(!navigation.forward());
    }

    /// Choosing a directory is a new branch; the old forward targets must not
    /// survive, or Forward would cross between unrelated branches.
    #[test]
    fn entering_a_new_directory_discards_the_forward_branch() {
        let mut navigation = TreemapNavigation::at_drive_root('C');
        navigation.enter(r"C:\Windows\System32");
        assert!(navigation.back());
        assert!(navigation.can_go_forward());

        navigation.enter(r"C:\Data");
        assert!(!navigation.can_go_forward());
        assert_eq!(navigation.current_dir(), r"C:\Data");
    }
}
