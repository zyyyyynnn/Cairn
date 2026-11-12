// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! A summary block's members as a table location.
//!
//! The block folds the children that cannot carry a readable canvas tile. Its
//! details belong in the table: virtualised rows, sorting and the normal row
//! actions, with no second canvas layer.

use crate::app::{layout, CairnApp};
use cairn_protocol::message::FileRowData;
use gpui_kit::gpui::{px, Context, Pixels, Point, Window};

/// One summary block's members: the unfiltered listing and the filtered rows
/// the table renders.
pub(crate) struct BlockListing {
    /// The directory that owns the block the listing came from.
    directory: String,
    /// The block's member count, independent of the in-place filter.
    count: usize,
    all: Vec<FileRowData>,
    rows: Vec<FileRowData>,
    total_bytes: u64,
}

impl BlockListing {
    pub(crate) fn new(directory: String, count: usize, rows: Vec<FileRowData>) -> Self {
        let total_bytes = rows.iter().map(|row| row.size).sum();
        Self {
            directory,
            count,
            all: rows.clone(),
            rows,
            total_bytes,
        }
    }

    pub(crate) fn directory(&self) -> &str {
        &self.directory
    }

    pub(crate) fn count(&self) -> usize {
        self.count
    }

    pub(crate) fn rows(&self) -> &[FileRowData] {
        &self.rows
    }

    pub(crate) fn total(&self) -> usize {
        self.rows.len()
    }

    pub(crate) fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Rebuilds the visible rows from the unfiltered listing and the current
    /// search text, the same in-place filter the Recycle Bin uses.
    pub(crate) fn apply_filter(&mut self, needle: &str, case_sensitive: bool) {
        let needle = needle.trim();
        self.rows = if needle.is_empty() {
            self.all.clone()
        } else {
            let lowered = needle.to_lowercase();
            self.all
                .iter()
                .filter(|row| {
                    listing_matches(&row.name, needle, &lowered, case_sensitive)
                        || listing_matches(&row.path, needle, &lowered, case_sensitive)
                })
                .cloned()
                .collect()
        };
        self.total_bytes = self.rows.iter().map(|row| row.size).sum();
    }
}

fn listing_matches(text: &str, needle: &str, lowered: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        text.contains(needle)
    } else {
        text.to_lowercase().contains(lowered)
    }
}

impl CairnApp {
    /// Opens the summary block at `block_path` as a table listing of exactly
    /// its members. The canvas keeps the block; the details are rows.
    pub(crate) fn open_block_listing(
        &mut self,
        block_path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (parent, count, rows) = {
            let Some(state) = self.treemap_state.as_ref() else {
                return;
            };
            let Some(node) = state.layout_node(block_path) else {
                return;
            };
            let Some(parent) = cairn_core::treemap::aggregate_parent_path(&node.path) else {
                return;
            };
            let parent = parent.to_string();
            let rows = block_listing_rows(
                &parent,
                &node.aggregate_member_indices,
                state.children_at(&parent),
            );
            (parent, node.aggregate_count, rows)
        };
        self.record_nav_state();
        self.leave_drive_view();
        self.block_listing = Some(BlockListing::new(parent, count, rows));
        self.query.search_input.set_text("");
        self.install_block_listing_rows(true, cx);
        self.query.search_focus.focus(window, cx);
        cx.notify();
    }

    /// Installs the first page of the current listing.
    pub(crate) fn install_block_listing_rows(
        &mut self,
        reset_scroll: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(listing) = self.block_listing.as_ref() else {
            return;
        };
        let total = listing.total();
        let first_len = total.min(layout::RESULT_PAGE_SIZE);
        let rows = listing.rows()[..first_len].to_vec();
        let saved_scroll = self.results.table.scroll_top;
        self.results.table.set_rows(rows, total);
        self.note_table_content(cx);
        if total == 0 {
            self.note_table_empty(cx);
        }
        if reset_scroll {
            self.results.rows_scroll.set_offset(Point::default());
            self.results.table.scroll_top = 0.0;
        } else {
            self.results.table.scroll_top = saved_scroll;
            self.results
                .rows_scroll
                .set_offset(Point::new(px(0.0), -px(saved_scroll)));
        }
        cx.notify();
    }

    /// Installs the page the viewport needs from the in-memory listing, with no
    /// IPC request.
    pub(crate) fn fill_block_listing_page(&mut self, row_height: Pixels, cx: &mut Context<Self>) {
        let Some(listing) = self.block_listing.as_ref() else {
            return;
        };
        self.results
            .table
            .set_scroll_top(self.scrolled_pixels(), f32::from(row_height));
        let Some((offset, limit)) = self
            .results
            .table
            .page_request(self.visible_rows(row_height), layout::RESULT_PAGE_SIZE)
        else {
            return;
        };
        if offset >= listing.total() {
            return;
        }
        let end = offset.saturating_add(limit).min(listing.total());
        let rows = listing.rows()[offset..end].to_vec();
        self.results.table.apply_page(offset, rows);
        cx.notify();
    }

    /// Rebuilds the listing's visible rows from the search text; the search
    /// band filters this location in place instead of launching an index query.
    pub(crate) fn filter_block_listing(&mut self, cx: &mut Context<Self>) {
        let needle = self.query.search_input.text.trim().to_string();
        let case_sensitive = self.query.case_sensitive;
        if let Some(listing) = self.block_listing.as_mut() {
            listing.apply_filter(&needle, case_sensitive);
        }
        self.install_block_listing_rows(true, cx);
    }

    /// Leaves the listing; called by every location entry point.
    pub(crate) fn clear_block_listing(&mut self) {
        self.block_listing = None;
    }
}

/// The rows of one summary block, in the block's member order (size desc).
pub(crate) fn block_listing_rows(
    parent: &str,
    members: &[u32],
    children: &[cairn_core::treemap::TreemapTreeNode],
) -> Vec<FileRowData> {
    members
        .iter()
        .filter_map(|&index| children.get(index as usize))
        .map(|child| FileRowData {
            frn: 0,
            name: child.name.clone(),
            path: join_listing_path(parent, &child.name),
            size: child.size,
            modified_ts: child.modified_ts,
            created_ts: 0,
            is_dir: child.is_dir,
            flags: 0,
            name_highlights: Vec::new(),
            path_highlights: Vec::new(),
            snippet: None,
        })
        .collect()
}

fn join_listing_path(parent: &str, name: &str) -> String {
    if parent.ends_with('\\') {
        format!("{parent}{name}")
    } else {
        format!("{parent}\\{name}")
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn child(name: &str, size: u64, is_dir: bool) -> cairn_core::treemap::TreemapTreeNode {
        let mut node = if is_dir {
            cairn_core::treemap::TreemapTreeNode::new_dir(0, name.to_string(), 7)
        } else {
            cairn_core::treemap::TreemapTreeNode::new_file(0, name.to_string(), size, 7)
        };
        node.size = size;
        node
    }

    #[test]
    fn block_rows_keep_member_order_and_join_paths() {
        let children = vec![child("Windows", 300, true), child("boot.ini", 100, false)];
        let rows = block_listing_rows(r"C:\", &[0, 1], &children);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].path, r"C:\Windows");
        assert_eq!(rows[0].size, 300);
        assert!(rows[0].is_dir);
        assert_eq!(rows[1].path, r"C:\boot.ini");
        assert_eq!(rows[1].modified_ts, 7);

        let rows = block_listing_rows(r"C:\Program Files", &[1], &children);
        assert_eq!(rows[0].path, r"C:\Program Files\boot.ini");
    }

    #[test]
    fn the_listing_filter_matches_name_and_path() {
        let children = vec![child("Windows", 300, true), child("boot.ini", 100, false)];
        let rows = block_listing_rows(r"C:\", &[0, 1], &children);
        let mut listing = BlockListing::new(r"C:\".to_string(), 2, rows);
        assert_eq!(listing.total(), 2);

        listing.apply_filter("boot", false);
        assert_eq!(listing.total(), 1);
        assert_eq!(listing.rows()[0].name, "boot.ini");
        assert_eq!(listing.total_bytes(), 100);

        listing.apply_filter("WINDOWS", false);
        assert_eq!(listing.total(), 1);
        assert_eq!(listing.rows()[0].name, "Windows");

        listing.apply_filter("WINDOWS", true);
        assert_eq!(listing.total(), 0);

        listing.apply_filter("", false);
        assert_eq!(listing.total(), 2);
        assert_eq!(listing.total_bytes(), 400);
    }
}
