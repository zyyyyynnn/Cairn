// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The hierarchical size tree every Treemap producer yields, and the item
//! payload a laid-out tile carries.

use super::category::{CategoryBytes, TreemapCategory};

/// One item to display inside the Treemap.
#[derive(Clone, Debug, PartialEq)]
pub struct TreemapItem {
    /// Producer-assigned identifier (record index or scan serial). Not stable
    /// across scans; the path is the stable identity.
    pub id: u64,
    /// Display name of the file or folder.
    pub name: String,
    /// Size in bytes (actual size for files, aggregated recursive size for directories).
    pub size: u64,
    /// Whether this item is a directory.
    pub is_dir: bool,
    /// Last modification timestamp.
    pub modified_ts: i64,
    /// Data category resolved when the item was built: a file's from its name,
    /// a directory's from its content.
    pub category: TreemapCategory,
}

impl From<&TreemapTreeNode> for TreemapItem {
    fn from(node: &TreemapTreeNode) -> Self {
        Self {
            id: node.id,
            name: node.name.clone(),
            size: node.size,
            is_dir: node.is_dir,
            modified_ts: node.modified_ts,
            category: node.category,
        }
    }
}

/// A directory or file in the hierarchical size tree.
///
/// Nodes carry only their own name: storing the full path on every node would
/// duplicate every ancestor prefix once per descendant, which is the dominant
/// memory cost of a whole-drive tree. Paths are materialized only for the
/// nodes the layout actually paints, and lookups walk name segments.
///
/// Producers hand out finished trees: directory sizes are aggregated, children
/// are sorted by size descending and directories carry the category of their
/// dominant content. A directory without children may still carry a size — a
/// depth-capped leaf whose recursive size was already measured.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct TreemapTreeNode {
    pub id: u64,
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub modified_ts: i64,
    pub category: TreemapCategory,
    pub children: Vec<TreemapTreeNode>,
    /// Number of items in this subtree, excluding this node. A depth-capped
    /// leaf carries the exact recursive count measured by its producer; a
    /// node with children may leave it zero and let aggregation fill it.
    pub descendant_items: usize,
}

impl TreemapTreeNode {
    pub fn new_file(id: u64, name: String, size: u64, modified_ts: i64) -> Self {
        let category = TreemapCategory::classify(&name, false);
        Self {
            id,
            name,
            size,
            is_dir: false,
            modified_ts,
            category,
            children: Vec::new(),
            descendant_items: 0,
        }
    }

    pub fn new_dir(id: u64, name: String, modified_ts: i64) -> Self {
        let category = TreemapCategory::classify(&name, true);
        Self {
            id,
            name,
            size: 0,
            is_dir: true,
            modified_ts,
            category,
            children: Vec::new(),
            descendant_items: 0,
        }
    }

    /// Number of items in this subtree, excluding the node itself. A producer
    /// that measured a depth-capped leaf stores its exact count here; otherwise
    /// the tree's children provide the count.
    pub fn descendant_count(&self) -> usize {
        if self.descendant_items > 0 || self.children.is_empty() {
            self.descendant_items
        } else {
            self.children
                .iter()
                .map(|child| 1 + child.descendant_count())
                .sum()
        }
    }

    /// Stores the aggregate count without re-walking sizes. `false` leaves the
    /// count untouched (a depth-capped leaf already carries its exact count).
    fn aggregate_descendant_items(&mut self) {
        if self.is_dir && !self.children.is_empty() && self.descendant_items == 0 {
            self.descendant_items = self
                .children
                .iter()
                .map(|child| 1 + child.descendant_count())
                .sum();
        }
    }

    /// Sorts direct children and all descendants by size descending.
    pub fn sort_recursive(&mut self) {
        self.children.sort_by_key(|a| std::cmp::Reverse(a.size));
        for child in &mut self.children {
            if child.is_dir {
                child.sort_recursive();
            }
        }
    }

    /// Recursively recomputes aggregated sizes for directory nodes bottom-up.
    ///
    /// A directory without children keeps its existing size instead of being
    /// reset to zero. That size is either a genuine empty directory (zero) or a
    /// depth-capped leaf whose recursive size was already computed; zeroing it
    /// would silently under-report every ancestor and make the Treemap lie
    /// about area.
    pub fn compute_aggregated_sizes(&mut self) -> u64 {
        if !self.is_dir || self.children.is_empty() {
            return self.size;
        }
        let mut total = 0u64;
        for child in &mut self.children {
            total = total.saturating_add(child.compute_aggregated_sizes());
        }
        self.size = total;
        self.aggregate_descendant_items();
        total
    }

    /// Resolves the category of every directory from its content, bottom-up,
    /// after sizes are final. Returns this node's final category.
    ///
    /// A file keeps its classified category. A directory accumulates the whole
    /// final size of each child under that child's final category — a nested
    /// directory contributes its own resolved category, not a per-file
    /// breakdown. A directory whose category is still [`TreemapCategory::Other`]
    /// adopts the category holding the most descendant bytes when that category
    /// is not `Other`; a directory whose bytes are mostly unclassified stays
    /// `Other`.
    pub fn propagate_content_categories(&mut self) -> TreemapCategory {
        if !self.is_dir {
            return self.category;
        }

        let mut totals = CategoryBytes::default();
        for child in &mut self.children {
            let category = child.propagate_content_categories();
            totals.add(category, child.size);
        }

        if self.category == TreemapCategory::Other {
            if let Some(dominant) = totals.dominant() {
                if dominant != TreemapCategory::Other {
                    self.category = dominant;
                }
            }
        }
        self.category
    }

    /// Recursively searches for a directory by walking name segments from this
    /// root. The path's first segment names the root itself; comparison
    /// ignores empty segments, trailing separators and ASCII case.
    pub fn find_by_path(&self, target_path: &str) -> Option<&TreemapTreeNode> {
        let mut node = self;
        let mut segments = target_path
            .split(['\\', '/'])
            .filter(|segment| !segment.is_empty());
        // The drive segment ("C:") stands for this root; a tree whose root is
        // a deeper directory is only ever looked up by paths below it, where
        // the first segment names that root's directory.
        if let Some(first) = segments.next() {
            #[cfg(windows)]
            let root_matches = self
                .name
                .trim_end_matches(['\\', '/', ':'])
                .eq_ignore_ascii_case(first.trim_end_matches(':'));
            #[cfg(not(windows))]
            let root_matches = {
                let _ = first;
                self.name.trim_end_matches(['\\', '/', ':']) == first.trim_end_matches(':')
            };
            if !root_matches {
                node = find_child_by_name(node, first)?;
            }
        }
        segments.try_fold(node, |node, segment| find_child_by_name(node, segment))
    }
}

fn find_child_by_name<'a>(node: &'a TreemapTreeNode, segment: &str) -> Option<&'a TreemapTreeNode> {
    node.children
        .iter()
        .find(|child| child.is_dir && child.name.eq_ignore_ascii_case(segment))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_node(name: &str, size: u64) -> TreemapTreeNode {
        TreemapTreeNode::new_file(0, name.to_string(), size, 0)
    }

    fn dir_with_children(name: &str, children: Vec<TreemapTreeNode>) -> TreemapTreeNode {
        let mut dir = TreemapTreeNode::new_dir(0, name.to_string(), 0);
        dir.children = children;
        dir.compute_aggregated_sizes();
        dir
    }

    #[test]
    fn aggregation_counts_each_descendant_once() {
        let system32 = dir_with_children(
            "System32",
            vec![file_node("kernel32.dll", 2), file_node("ntdll.dll", 1)],
        );
        let windows = dir_with_children("Windows", vec![system32, file_node("notepad.exe", 1)]);
        let root = dir_with_children(r"C:\\", vec![windows, file_node("pagefile.sys", 1)]);

        assert_eq!(root.descendant_count(), 6);
        let windows = &root.children[0];
        assert_eq!(windows.descendant_count(), 4);
        assert_eq!(windows.children[0].descendant_count(), 2);
        assert_eq!(windows.children[1].descendant_count(), 0);
    }

    #[test]
    fn content_propagation_assigns_a_directory_category() {
        let mut windows = dir_with_children(
            "Windows",
            vec![
                file_node("pagefile.sys", 900),
                file_node("wallpaper.jpg", 100),
            ],
        );
        assert_eq!(
            windows.propagate_content_categories(),
            TreemapCategory::System
        );
        assert_eq!(windows.category, TreemapCategory::System);

        let mut program_files = dir_with_children(
            "Program Files",
            vec![file_node("manual.pdf", 500), file_node("app.exe", 100)],
        );
        assert_eq!(
            program_files.propagate_content_categories(),
            TreemapCategory::Documents
        );

        let mut users = dir_with_children("Users", vec![file_node("main.rs", 400)]);
        assert_eq!(
            users.propagate_content_categories(),
            TreemapCategory::Documents
        );
    }

    #[test]
    fn unknown_bytes_can_outweigh_a_content_category() {
        let mut folder = dir_with_children(
            "Temp",
            vec![file_node("data.bin", 900), file_node("notes.txt", 100)],
        );
        assert_eq!(
            folder.propagate_content_categories(),
            TreemapCategory::Other,
            "a directory whose bytes are mostly unclassified stays Other"
        );
    }

    #[test]
    fn a_generic_directory_inherits_its_files_category() {
        let mut folder = dir_with_children(
            "ZYN",
            vec![file_node("main.rs", 300), file_node("lib.rs", 200)],
        );
        assert_eq!(folder.category, TreemapCategory::Other);
        assert_eq!(
            folder.propagate_content_categories(),
            TreemapCategory::Documents
        );
    }

    #[test]
    fn a_mixed_directory_picks_the_largest_category() {
        let mut folder = dir_with_children(
            "ZYN",
            vec![
                file_node("clip.mp4", 100),
                file_node("notes.txt", 300),
                file_node("backup.zip", 500),
            ],
        );
        assert_eq!(
            folder.propagate_content_categories(),
            TreemapCategory::Archive
        );
    }

    #[test]
    fn a_tie_between_content_categories_keeps_the_earlier_variant() {
        let mut folder = dir_with_children(
            "ZYN",
            vec![file_node("clip.mp4", 500), file_node("backup.zip", 500)],
        );
        assert_eq!(
            folder.propagate_content_categories(),
            TreemapCategory::Video,
            "Video precedes Archive in the category order"
        );
    }

    #[test]
    fn a_generic_directory_with_only_unknown_files_stays_other() {
        let mut folder = dir_with_children(
            "ZYN",
            vec![file_node("data.bin", 100), file_node("data.xyz", 200)],
        );
        assert_eq!(
            folder.propagate_content_categories(),
            TreemapCategory::Other
        );
    }

    #[test]
    fn a_nested_directory_contributes_its_whole_size_under_its_resolved_category() {
        // Workspace holds more image bytes than source bytes, so its whole
        // subtree counts as Image in the parent.
        let workspace = dir_with_children(
            "Workspace",
            vec![file_node("main.rs", 100), file_node("photo.jpg", 1_000)],
        );
        let mut parent = dir_with_children("ZYN", vec![workspace, file_node("notes.txt", 500)]);

        assert_eq!(
            parent.propagate_content_categories(),
            TreemapCategory::Image,
            "the whole Workspace subtree counts as Image, not its source files"
        );
        assert_eq!(parent.children[0].category, TreemapCategory::Image);
    }

    #[test]
    fn propagation_resolves_nested_generic_directories_bottom_up() {
        let inner = dir_with_children("Alpha", vec![file_node("deep.rs", 800)]);
        let mut outer = dir_with_children("ZYN", vec![inner, file_node("notes.txt", 200)]);

        assert_eq!(
            outer.propagate_content_categories(),
            TreemapCategory::Documents
        );
        assert_eq!(outer.children[0].category, TreemapCategory::Documents);
    }

    #[test]
    fn test_treemap_tree_node_size_aggregation_and_sorting() {
        let mut root = TreemapTreeNode::new_dir(0, "C:".to_string(), 0);

        let mut win = TreemapTreeNode::new_dir(1, "Windows".to_string(), 0);
        let mut sys32 = TreemapTreeNode::new_dir(2, "System32".to_string(), 0);
        sys32.children.push(TreemapTreeNode::new_file(
            3,
            "kernel32.dll".to_string(),
            3_000_000,
            0,
        ));
        win.children.push(sys32);
        win.children.push(TreemapTreeNode::new_file(
            4,
            "explorer.exe".to_string(),
            1_000_000,
            0,
        ));

        let mut users = TreemapTreeNode::new_dir(5, "Users".to_string(), 0);
        let mut user_zyn = TreemapTreeNode::new_dir(6, "ZYN".to_string(), 0);
        user_zyn.children.push(TreemapTreeNode::new_file(
            7,
            "data.bin".to_string(),
            8_000_000,
            0,
        ));
        users.children.push(user_zyn);

        root.children.push(win);
        root.children.push(users);

        let total = root.compute_aggregated_sizes();
        assert_eq!(total, 12_000_000);
        assert_eq!(root.children[0].size + root.children[1].size, 12_000_000);

        root.sort_recursive();
        // Users (8MB) should come before Windows (4MB)
        assert_eq!(root.children[0].name, "Users");
        assert_eq!(root.children[0].size, 8_000_000);
        assert_eq!(root.children[1].name, "Windows");
        assert_eq!(root.children[1].size, 4_000_000);
    }

    #[test]
    fn test_tree_node_find_by_path() {
        let mut root = TreemapTreeNode::new_dir(0, "C:".to_string(), 0);
        let mut win = TreemapTreeNode::new_dir(1, "Windows".to_string(), 0);
        let sys32 = TreemapTreeNode::new_dir(2, "System32".to_string(), 0);
        win.children.push(sys32);
        root.children.push(win);

        assert!(root.find_by_path("C:\\").is_some());
        assert!(root.find_by_path("C:\\Windows").is_some());
        assert_eq!(root.find_by_path("c:\\windows").unwrap().name, "Windows");
        assert!(root.find_by_path("C:\\Windows\\System32").is_some());
        assert_eq!(
            root.find_by_path("c:\\windows\\system32").unwrap().name,
            "System32"
        );
        assert!(root.find_by_path("C:\\NonExistent").is_none());
    }

    #[test]
    fn compute_aggregated_sizes_keeps_depth_capped_leaf_size() {
        // Deeper than any scan depth: the leaf already carries its recursive
        // size and must survive the bottom-up aggregation.
        let mut leaf = TreemapTreeNode::new_dir(3, "Leaf".to_string(), 0);
        leaf.size = 4_096;
        let mut middle = TreemapTreeNode::new_dir(2, "B".to_string(), 0);
        middle.children.push(leaf);
        let mut parent = TreemapTreeNode::new_dir(1, "A".to_string(), 0);
        parent.children.push(middle);
        parent
            .children
            .push(TreemapTreeNode::new_file(4, "a.bin".to_string(), 1_024, 0));

        assert_eq!(parent.compute_aggregated_sizes(), 5_120);
        assert_eq!(parent.children[0].size, 4_096);
        assert_eq!(parent.children[1].size, 1_024);
    }
}
