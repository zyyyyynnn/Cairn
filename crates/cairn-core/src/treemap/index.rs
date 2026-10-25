// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Directory sizes and parent-child structure over a compact volume index.

use super::category::TreemapCategory;
use super::tree::TreemapTreeNode;
use crate::arena::NamePool;
use crate::model::FileRecord;

/// Precomputed directory sizes and child lists for fast Treemap trees over
/// compact `FileRecord`s.
///
/// Child lists are stored once in compressed form (one offset per record plus
/// one flat index array), so the index costs 20 bytes per record and no
/// per-directory allocation. Only records reachable from the volume root take
/// part: tombstones, records under a tombstone and records whose parent chain
/// never reaches the root are left out of both the structure and the totals.
#[derive(Clone, Debug, Default)]
pub struct DirectorySizeIndex {
    /// Recursive byte total of each record (a file's own size).
    sizes: Vec<u64>,
    /// Records below each record, at every depth, excluding the record itself.
    counts: Vec<u32>,
    /// Children of record `i` are `child_records[child_starts[i]..child_starts[i + 1]]`.
    child_starts: Vec<u32>,
    child_records: Vec<u32>,
    /// Direct children of the volume root.
    root_children: Vec<u32>,
}

/// Where one record sits in the hierarchy.
enum Placement {
    Root,
    Child(u32),
    Excluded,
}

impl DirectorySizeIndex {
    /// Builds the hierarchy and its totals in linear time.
    ///
    /// Two index shapes are accepted: one with an explicit volume-root record
    /// (MFT record 5, named `.`, `\`, `$Root`, `C:` or `C:\`) and one without,
    /// where top-level entries point at [`FileRecord::ROOT_PARENT`]. A record
    /// that is its own parent is treated as top level.
    pub fn build(records: &[FileRecord], arena: &NamePool) -> Self {
        let n = records.len();
        let root_record = find_root_record(records, arena);
        let placement = |index: usize, record: &FileRecord| -> Placement {
            let idx = index as u32;
            if record.is_tombstone() || Some(idx) == root_record {
                return Placement::Excluded;
            }
            let parent = record.parent_idx;
            if parent == FileRecord::ROOT_PARENT || parent == idx || Some(parent) == root_record {
                Placement::Root
            } else if (parent as usize) < n {
                Placement::Child(parent)
            } else {
                Placement::Excluded
            }
        };

        // Count children per parent, prefix-sum into offsets, then fill in
        // record order so every child list keeps the index order.
        let mut child_starts = vec![0u32; n + 1];
        let mut root_children = Vec::new();
        for (index, record) in records.iter().enumerate() {
            match placement(index, record) {
                Placement::Root => root_children.push(index as u32),
                Placement::Child(parent) => child_starts[parent as usize + 1] += 1,
                Placement::Excluded => {}
            }
        }
        for index in 0..n {
            child_starts[index + 1] += child_starts[index];
        }
        let mut cursor = child_starts[..n].to_vec();
        let mut child_records = vec![0u32; child_starts[n] as usize];
        for (index, record) in records.iter().enumerate() {
            if let Placement::Child(parent) = placement(index, record) {
                let slot = &mut cursor[parent as usize];
                child_records[*slot as usize] = index as u32;
                *slot += 1;
            }
        }
        drop(cursor);

        let mut index = Self {
            sizes: vec![0; n],
            counts: vec![0; n],
            child_starts,
            child_records,
            root_children,
        };
        index.aggregate(records);

        // The explicit root record is not part of the tree, but it reports
        // the volume totals so its own entry never reads as empty.
        if let Some(root) = root_record {
            let (size, count) = index.totals_of(&index.root_children);
            index.sizes[root as usize] = size;
            index.counts[root as usize] = count;
        }
        index
    }

    /// Computes sizes and counts bottom-up. Every reachable record sits in
    /// exactly one child list, so a pre-order walk from the root visits it
    /// once, and its reverse finishes every child before its parent.
    fn aggregate(&mut self, records: &[FileRecord]) {
        let mut order = Vec::with_capacity(records.len());
        let mut stack = self.root_children.clone();
        while let Some(idx) = stack.pop() {
            order.push(idx);
            stack.extend_from_slice(self.children_of(Some(idx)));
        }
        for &idx in order.iter().rev() {
            let record = &records[idx as usize];
            let own = if record.is_file() { record.size } else { 0 };
            let (children_size, count) = self.totals_of(self.children_of(Some(idx)));
            self.sizes[idx as usize] = own.saturating_add(children_size);
            self.counts[idx as usize] = count;
        }
    }

    /// Byte total and item count of `children` and everything below them.
    fn totals_of(&self, children: &[u32]) -> (u64, u32) {
        children.iter().fold((0u64, 0u32), |(size, count), &child| {
            (
                size.saturating_add(self.sizes[child as usize]),
                count
                    .saturating_add(1)
                    .saturating_add(self.counts[child as usize]),
            )
        })
    }

    /// Direct children of a directory (`None` = volume root).
    fn children_of(&self, dir_idx: Option<u32>) -> &[u32] {
        match dir_idx {
            None => &self.root_children,
            Some(idx) => {
                let idx = idx as usize;
                match (self.child_starts.get(idx), self.child_starts.get(idx + 1)) {
                    (Some(&start), Some(&end)) => &self.child_records[start as usize..end as usize],
                    _ => &[],
                }
            }
        }
    }

    /// Writes every live directory's recursive indexed size back into its record. The service
    /// keeps this value in `FileRecord.size`, so the table can display and sort folders without a
    /// second size structure; files keep their own size.
    pub fn apply_directory_sizes(records: &mut [FileRecord], arena: &NamePool) {
        let index = Self::build(records, arena);
        for (record, size) in records.iter_mut().zip(index.sizes.iter()) {
            if record.is_dir() && !record.is_tombstone() {
                record.size = *size;
            }
        }
    }

    /// Number of items in a directory's subtree, excluding the directory
    /// itself. `None` counts the whole volume root without its synthetic
    /// root record.
    pub fn descendant_count(&self, dir_idx: Option<u32>) -> usize {
        match dir_idx {
            Some(idx) => self.counts.get(idx as usize).copied().unwrap_or(0) as usize,
            None => self.totals_of(&self.root_children).1 as usize,
        }
    }

    /// Resolves an absolute path (e.g. `C:\Windows\System32`) to a record index.
    /// Returns `Some(None)` for the volume root (e.g. `C:\`), `Some(Some(idx))`
    /// for a subdirectory, or `None` if any segment could not be resolved.
    /// Segments match ASCII case-insensitively.
    pub fn resolve_path_to_idx(
        &self,
        records: &[FileRecord],
        arena: &NamePool,
        path: &str,
    ) -> Option<Option<u32>> {
        let trimmed = path.trim_end_matches('\\');
        let relative_path =
            if let Some(stripped) = trimmed.strip_prefix(|c: char| c.is_ascii_alphabetic()) {
                if let Some(rest) = stripped.strip_prefix(':') {
                    rest.trim_start_matches('\\')
                } else {
                    trimmed
                }
            } else {
                trimmed.trim_start_matches('\\')
            };

        let mut current = None;
        for segment in relative_path.split('\\').filter(|s| !s.is_empty()) {
            let child = self.children_of(current).iter().copied().find(|&child| {
                let record = &records[child as usize];
                arena
                    .get_name(record.name_offset, record.name_len)
                    .eq_ignore_ascii_case(segment)
            })?;
            current = Some(child);
        }
        Some(current)
    }

    /// Builds a finished tree for a directory (`None` = volume root), down to
    /// `max_depth` levels below it. Directories at the depth limit are leaves
    /// that still carry their recursive size; children are sorted by size and
    /// content categories are propagated once on the whole tree. Nodes carry
    /// names only; the caller materializes paths for what it paints.
    pub fn build_tree_node(
        &self,
        records: &[FileRecord],
        arena: &NamePool,
        dir_idx: Option<u32>,
        name: &str,
        max_depth: usize,
    ) -> TreemapTreeNode {
        let mut node = self.build_subtree(records, arena, dir_idx, name, max_depth);
        node.propagate_content_categories();
        node
    }

    fn build_subtree(
        &self,
        records: &[FileRecord],
        arena: &NamePool,
        dir_idx: Option<u32>,
        name: &str,
        max_depth: usize,
    ) -> TreemapTreeNode {
        let child_indices = self.children_of(dir_idx);
        let mut children = Vec::with_capacity(child_indices.len());
        for &child in child_indices {
            let record = &records[child as usize];
            let child_name = arena.get_name(record.name_offset, record.name_len);
            if record.is_dir() && max_depth > 0 {
                let mut child_node =
                    self.build_subtree(records, arena, Some(child), child_name, max_depth - 1);
                // The index knows every descendant even where the tree is
                // depth-capped, so every node carries the exact total.
                child_node.descendant_items = self.descendant_count(Some(child));
                children.push(child_node);
            } else {
                children.push(TreemapTreeNode {
                    id: child as u64,
                    name: child_name.to_string(),
                    size: self.sizes[child as usize],
                    is_dir: record.is_dir(),
                    modified_ts: record.modified_ts,
                    category: TreemapCategory::classify(child_name, record.is_dir()),
                    children: Vec::new(),
                    descendant_items: if record.is_dir() {
                        self.descendant_count(Some(child))
                    } else {
                        0
                    },
                });
            }
        }
        children.sort_by_key(|child| std::cmp::Reverse(child.size));

        let (size, modified_ts) = match dir_idx {
            Some(idx) => (self.sizes[idx as usize], records[idx as usize].modified_ts),
            None => (children.iter().map(|child| child.size).sum(), 0),
        };
        TreemapTreeNode {
            id: dir_idx.map(u64::from).unwrap_or(0),
            name: name.to_string(),
            size,
            is_dir: true,
            modified_ts,
            category: TreemapCategory::classify(name, true),
            children,
            descendant_items: self.descendant_count(dir_idx),
        }
    }
}

/// The explicit volume-root record, if the index has one.
fn find_root_record(records: &[FileRecord], arena: &NamePool) -> Option<u32> {
    records.iter().enumerate().find_map(|(index, record)| {
        let is_top =
            record.parent_idx == FileRecord::ROOT_PARENT || record.parent_idx == index as u32;
        if record.is_tombstone() || !record.is_dir() || !is_top {
            return None;
        }
        let name = arena.get_name(record.name_offset, record.name_len);
        let is_root_name = name == "."
            || name.is_empty()
            || name == "\\"
            || name == "$Root"
            || name.ends_with(":\\")
            || (name.len() == 2 && name.ends_with(':'));
        is_root_name.then_some(index as u32)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::FLAG_DIRECTORY;

    /// Appends one record and returns its index.
    fn push(
        records: &mut Vec<FileRecord>,
        pool: &mut NamePool,
        name: &str,
        parent_idx: u32,
        flags: u16,
        size: u64,
    ) -> u32 {
        let (name_offset, name_len, ext_offset) = pool.insert(name);
        records.push(FileRecord {
            frn: records.len() as u64 + 1,
            parent_idx,
            name_offset,
            name_len,
            ext_offset,
            volume_id: 2,
            flags,
            size,
            modified_ts: 100 + records.len() as i64,
            created_ts: 0,
        });
        records.len() as u32 - 1
    }

    fn child<'a>(node: &'a TreemapTreeNode, name: &str) -> &'a TreemapTreeNode {
        node.children
            .iter()
            .find(|child| child.name == name)
            .unwrap_or_else(|| panic!("{name} is a child of {}", node.name))
    }

    #[test]
    fn an_explicit_root_record_aggregates_sizes_counts_and_categories() {
        let mut pool = NamePool::default();
        let mut records = Vec::new();
        let root = push(
            &mut records,
            &mut pool,
            "C:\\",
            FileRecord::ROOT_PARENT,
            FLAG_DIRECTORY,
            0,
        );
        let windows = push(&mut records, &mut pool, "Windows", root, FLAG_DIRECTORY, 0);
        let system32 = push(
            &mut records,
            &mut pool,
            "System32",
            windows,
            FLAG_DIRECTORY,
            0,
        );
        push(&mut records, &mut pool, "kernel32.dll", system32, 0, 3000);
        let notepad = push(&mut records, &mut pool, "notepad.exe", windows, 0, 1000);
        push(&mut records, &mut pool, "pagefile.sys", root, 0, 10_000);

        let index = DirectorySizeIndex::build(&records, &pool);

        // Every item is counted once at every ancestor. A directory's count
        // excludes itself; the volume total excludes the synthetic root record.
        assert_eq!(index.descendant_count(None), 5);
        assert_eq!(index.descendant_count(Some(windows)), 3);
        assert_eq!(index.descendant_count(Some(system32)), 1);
        assert_eq!(index.descendant_count(Some(notepad)), 0);

        let tree = index.build_tree_node(&records, &pool, None, "C:\\", 6);
        assert_eq!(tree.size, 14_000);
        assert_eq!(
            tree.children
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["pagefile.sys", "Windows"],
            "the root record itself is not a child, and children sort by size"
        );
        assert_eq!(
            child(&tree, "pagefile.sys").category,
            TreemapCategory::System
        );

        assert_eq!(tree.descendant_count(), 5);
        assert_eq!(child(&tree, "pagefile.sys").descendant_count(), 0);

        let windows_node = child(&tree, "Windows");
        assert_eq!(windows_node.size, 4000);
        assert_eq!(windows_node.descendant_count(), 3);
        let system32_node = child(windows_node, "System32");
        assert_eq!(system32_node.descendant_count(), 1);
        assert_eq!(system32_node.size, 3000);
        assert!(system32_node.is_dir);
        assert_eq!(system32_node.category, TreemapCategory::System);
        let notepad_node = child(windows_node, "notepad.exe");
        assert_eq!(notepad_node.size, 1000);
        assert!(!notepad_node.is_dir);
        assert_eq!(notepad_node.category, TreemapCategory::Code);
    }

    #[test]
    fn an_index_without_a_root_record_keeps_every_top_level_entry() {
        let mut pool = NamePool::default();
        let mut records = Vec::new();
        let windows = push(
            &mut records,
            &mut pool,
            "Windows",
            FileRecord::ROOT_PARENT,
            FLAG_DIRECTORY,
            0,
        );
        push(
            &mut records,
            &mut pool,
            "Program Files",
            FileRecord::ROOT_PARENT,
            FLAG_DIRECTORY,
            0,
        );
        let system32 = push(
            &mut records,
            &mut pool,
            "System32",
            windows,
            FLAG_DIRECTORY,
            0,
        );
        push(&mut records, &mut pool, "cmd.exe", system32, 0, 50_000);

        let index = DirectorySizeIndex::build(&records, &pool);
        let tree = index.build_tree_node(&records, &pool, None, "C:\\", 6);

        assert_eq!(
            tree.children.len(),
            2,
            "Windows must not be omitted from the root"
        );
        assert_eq!(child(&tree, "Windows").size, 50_000);
        assert_eq!(child(&tree, "Program Files").size, 0);
        assert_eq!(child(child(&tree, "Windows"), "System32").size, 50_000);

        assert_eq!(
            index.resolve_path_to_idx(&records, &pool, "C:\\"),
            Some(None)
        );
        assert_eq!(index.resolve_path_to_idx(&records, &pool, "C:"), Some(None));
        assert_eq!(
            index.resolve_path_to_idx(&records, &pool, "C:\\Windows"),
            Some(Some(windows))
        );
        assert_eq!(
            index.resolve_path_to_idx(&records, &pool, "c:\\windows\\SYSTEM32\\"),
            Some(Some(system32))
        );
        assert_eq!(
            index.resolve_path_to_idx(&records, &pool, "C:\\NonExistent"),
            None
        );
    }

    #[test]
    fn tombstones_and_orphans_stay_out_of_the_tree_and_its_totals() {
        let mut pool = NamePool::default();
        let mut records = Vec::new();
        let kept = push(
            &mut records,
            &mut pool,
            "Kept",
            FileRecord::ROOT_PARENT,
            FLAG_DIRECTORY,
            0,
        );
        push(&mut records, &mut pool, "live.bin", kept, 0, 100);
        let deleted = push(&mut records, &mut pool, "Deleted", kept, FLAG_DIRECTORY, 0);
        records[deleted as usize].flags |= crate::model::FLAG_TOMBSTONE;
        push(
            &mut records,
            &mut pool,
            "under-deleted.bin",
            deleted,
            0,
            7_000,
        );
        push(&mut records, &mut pool, "orphan.bin", 9_999, 0, 5_000);

        let index = DirectorySizeIndex::build(&records, &pool);
        let tree = index.build_tree_node(&records, &pool, None, "C:\\", 6);

        assert_eq!(tree.size, 100);
        assert_eq!(index.descendant_count(None), 2);
        let kept_node = child(&tree, "Kept");
        assert_eq!(kept_node.children.len(), 1);
        assert_eq!(kept_node.size, 100);
    }

    #[test]
    fn the_depth_limit_keeps_recursive_sizes_on_leaf_directories() {
        let mut pool = NamePool::default();
        let mut records = Vec::new();
        let top = push(
            &mut records,
            &mut pool,
            "Top",
            FileRecord::ROOT_PARENT,
            FLAG_DIRECTORY,
            0,
        );
        let mid = push(&mut records, &mut pool, "Mid", top, FLAG_DIRECTORY, 0);
        push(&mut records, &mut pool, "deep.bin", mid, 0, 4_096);

        let index = DirectorySizeIndex::build(&records, &pool);
        let tree = index.build_tree_node(&records, &pool, Some(top), "Top", 0);

        assert_eq!(tree.size, 4_096);
        let mid_node = child(&tree, "Mid");
        assert!(mid_node.is_dir);
        assert!(mid_node.children.is_empty());
        assert_eq!(mid_node.size, 4_096);
    }
}
