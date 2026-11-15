// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Directory tree flattening and the per-request entry budget.

use super::MultiVolumeEngine;
use cairn_core::treemap::DirectorySizeIndex;
use cairn_protocol::message::{
    TreeEntryData, ERROR_CODE_TREE_NOT_DIRECTORY, ERROR_CODE_TREE_NOT_INDEXED,
    ERROR_CODE_TREE_TOO_LARGE,
};

/// Flattens a finished tree pre-order (parent before children) for the wire.
fn flatten_tree(
    node: &cairn_core::treemap::TreemapTreeNode,
    depth: usize,
    out: &mut Vec<TreeEntryData>,
) {
    out.push(TreeEntryData {
        name: node.name.clone(),
        size: node.size,
        is_dir: node.is_dir,
        modified_ts: node.modified_ts,
        descendant_items: node.descendant_items,
        depth,
    });
    for child in &node.children {
        flatten_tree(child, depth + 1, out);
    }
}

/// The deepest level at or below `requested` whose entry count fits `budget`, with that count.
/// Shallower levels are tried in turn, so one oversized subtree degrades its own depth instead of
/// failing the request; `None` means even the immediate children do not fit.
pub(super) fn budgeted_tree_depth(
    mut count_at_depth: impl FnMut(usize) -> usize,
    requested: usize,
    budget: usize,
) -> Option<(usize, usize)> {
    (1..=requested).rev().find_map(|depth| {
        let count = count_at_depth(depth);
        (count <= budget).then_some((depth, count))
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeUnavailable {
    NotIndexed,
    NotDirectory,
    TooLarge,
}

impl TreeUnavailable {
    pub const fn code(self) -> u32 {
        match self {
            Self::NotIndexed => ERROR_CODE_TREE_NOT_INDEXED,
            Self::NotDirectory => ERROR_CODE_TREE_NOT_DIRECTORY,
            Self::TooLarge => ERROR_CODE_TREE_TOO_LARGE,
        }
    }

    pub const fn message(self) -> &'static str {
        match self {
            Self::NotIndexed => "the path is not covered by the volume index",
            Self::NotDirectory => "the path is not a directory",
            Self::TooLarge => "the directory's immediate children exceed the one-response budget",
        }
    }
}

impl MultiVolumeEngine {
    /// Upper bound on entries in one tree answer. Keeps a pathological directory from allocating
    /// an unbounded tree; the frame encoder still enforces the byte ceiling.
    pub(super) const MAX_TREE_ENTRIES: usize = 100_000;
    /// Conservative serialized-size budget for one tree payload. The protocol frame is capped at
    /// 16 MiB; this leaves room for the envelope and the volume list.
    const MAX_TREE_PAYLOAD_BYTES: usize = 15 * 1024 * 1024;
    /// Estimated fixed bytes per entry in the postcard payload.
    const TREE_ENTRY_OVERHEAD: usize = 64;

    /// Reads the directory tree below `path` from the owning volume's index,
    /// flattened pre-order for the wire.
    ///
    /// The size index is built per request: it is an O(records) pass the
    /// Treemap only asks for when it opens a drive, and keeping it resident
    /// for one view would tax every query in between.
    pub fn treemap_tree(
        &self,
        path: &str,
        max_depth: usize,
    ) -> Result<Vec<TreeEntryData>, TreeUnavailable> {
        let trimmed = path.trim_end_matches('\\');
        if let Some(drive) = trimmed
            .chars()
            .next()
            .filter(|c| c.is_ascii_alphabetic() && trimmed.as_bytes().get(1) == Some(&b':'))
            .map(|c| c.to_ascii_uppercase())
        {
            let volumes = self.volumes.read().unwrap();
            if let Some(volume) = volumes.get(&drive) {
                let is_root = trimmed.eq_ignore_ascii_case(&format!("{drive}:"));
                let root_prefix = format!("{drive}:\\");
                let rel_or_abs = if is_root { "" } else { path };
                return build_treemap_entries_for_slice(
                    volume.records(),
                    volume.arena(),
                    is_root,
                    &root_prefix,
                    rel_or_abs,
                    max_depth,
                );
            }
        }

        let norm_req = cairn_core::path_cache::normalize_root(&path.replace('/', "\\"));
        let folders = self.folder_sources.read().unwrap();
        let folder = folders
            .values()
            .filter(|src| {
                norm_req
                    .to_ascii_lowercase()
                    .starts_with(&src.root_prefix.to_ascii_lowercase())
            })
            .max_by_key(|src| src.root_prefix.len())
            .ok_or(TreeUnavailable::NotIndexed)?;
        let relative = norm_req[folder.root_prefix.len()..].trim_matches('\\');
        let is_root = relative.is_empty();
        build_treemap_entries_for_slice(
            folder.records(),
            folder.arena(),
            is_root,
            folder.display_root(),
            relative,
            max_depth,
        )
    }
}

fn build_treemap_entries_for_slice(
    records: &[cairn_core::FileRecord],
    arena: &cairn_core::arena::NamePool,
    is_root: bool,
    root_name_for_root: &str,
    subpath: &str,
    max_depth: usize,
) -> Result<Vec<TreeEntryData>, TreeUnavailable> {
    let dir_index = DirectorySizeIndex::build(records, arena);
    let (dir_idx, root_name) = if is_root {
        (None, root_name_for_root.to_string())
    } else {
        let idx = dir_index
            .resolve_path_to_idx(records, arena, subpath)
            .flatten()
            .ok_or(TreeUnavailable::NotIndexed)?;
        let record = &records[idx as usize];
        if !record.is_dir() {
            return Err(TreeUnavailable::NotDirectory);
        }
        (
            Some(idx),
            arena
                .get_name(record.name_offset, record.name_len)
                .to_string(),
        )
    };

    // The budget picks the depth, not the caller. A depth-capped subtree that
    // cannot fit one response is still served one level shallower: every node
    // carries its exact descendant total, so the Treemap renders the children
    // it gets and its own aggregate blocks absorb the rest. Only a directory
    // whose immediate children overflow the budget has no answer to give.
    let (depth, entry_count) = budgeted_tree_depth(
        |depth| dir_index.subtree_entry_count(dir_idx, depth),
        max_depth,
        MultiVolumeEngine::MAX_TREE_ENTRIES,
    )
    .ok_or(TreeUnavailable::TooLarge)?;
    let tree = dir_index.build_tree_node(records, arena, dir_idx, &root_name, depth);
    let mut entries = Vec::with_capacity(entry_count);
    flatten_tree(&tree, 0, &mut entries);
    // A path above either budget cannot be answered in one response.
    if entries.len() > MultiVolumeEngine::MAX_TREE_ENTRIES {
        return Err(TreeUnavailable::TooLarge);
    }
    let estimated_bytes = entries.iter().fold(0usize, |total, entry| {
        total.saturating_add(
            entry
                .name
                .len()
                .saturating_add(MultiVolumeEngine::TREE_ENTRY_OVERHEAD),
        )
    });
    if estimated_bytes > MultiVolumeEngine::MAX_TREE_PAYLOAD_BYTES {
        return Err(TreeUnavailable::TooLarge);
    }
    Ok(entries)
}
