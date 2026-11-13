// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! A summary block's members as a table location.
//!
//! The block folds the children that cannot carry a readable canvas tile. Its
//! details belong in the table: virtualised rows, sorting and the normal row
//! actions, with no second canvas layer.

use crate::app::{layout, CairnApp};
use crate::search::filters::FilterCategory;
use cairn_core::sort::SortColumn;
use cairn_protocol::message::FileRowData;
use gpui_kit::gpui::{Context, Pixels, Point, Window};

/// One summary block's members: the unfiltered listing and the filtered rows
/// the table renders.
pub(crate) struct BlockListing {
    /// The directory that owns the block the listing came from.
    directory: String,
    /// The block's member count, independent of the in-place filter.
    count: usize,
    all: Vec<FileRowData>,
    /// Populated only while a filter query is active, avoiding a second full copy when unfiltered.
    filtered: Option<Vec<FileRowData>>,
    total_bytes: u64,
}

impl BlockListing {
    pub(crate) fn new(directory: String, count: usize, rows: Vec<FileRowData>) -> Self {
        let total_bytes = rows.iter().map(|row| row.size).sum();
        Self {
            directory,
            count,
            all: rows,
            filtered: None,
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
        self.filtered.as_deref().unwrap_or(&self.all)
    }

    pub(crate) fn total(&self) -> usize {
        self.rows().len()
    }

    pub(crate) fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Reorders the listing to the table's current sort.
    pub(crate) fn sort(&mut self, column: SortColumn, ascending: bool) {
        sort_rows(&mut self.all, column, ascending);
        if let Some(filtered) = self.filtered.as_mut() {
            sort_rows(filtered, column, ascending);
        }
    }

    /// Drops the given full paths from the listing after a successful delete.
    pub(crate) fn remove_paths(&mut self, paths: &[String]) {
        self.all.retain(|row| {
            let full = join_listing_path(&self.directory, &row.name);
            !paths.iter().any(|path| path.eq_ignore_ascii_case(&full))
        });
        if let Some(filtered) = self.filtered.as_mut() {
            filtered.retain(|row| {
                let full = join_listing_path(&self.directory, &row.name);
                !paths.iter().any(|path| path.eq_ignore_ascii_case(&full))
            });
        }
        self.count = self.all.len();
    }

    /// Applies a successful rename to the rows a later filter, sort or page
    /// reinstall reads from.
    pub(crate) fn rename_path(&mut self, old_path: &str, new_name: &str) {
        for row in self.all.iter_mut() {
            let full = join_listing_path(&self.directory, &row.name);
            if full.eq_ignore_ascii_case(old_path) {
                row.name = new_name.to_string();
            }
        }
        if let Some(filtered) = self.filtered.as_mut() {
            for row in filtered.iter_mut() {
                let full = join_listing_path(&self.directory, &row.name);
                if full.eq_ignore_ascii_case(old_path) {
                    row.name = new_name.to_string();
                }
            }
        }
    }

    /// Rebuilds the visible rows from the unfiltered listing and the current
    /// search text, the same in-place filter the Recycle Bin uses.
    pub(crate) fn apply_filter(&mut self, needle: &str, case_sensitive: bool) {
        let needle = needle.trim();
        self.filtered = if needle.is_empty() {
            None
        } else {
            let lowered = needle.to_lowercase();
            Some(
                self.all
                    .iter()
                    .filter(|row| {
                        listing_matches(&row.name, needle, &lowered, case_sensitive)
                            || listing_matches(&row.path, needle, &lowered, case_sensitive)
                    })
                    .cloned()
                    .collect(),
            )
        };
        self.total_bytes = self.rows().iter().map(|row| row.size).sum();
    }

    #[cfg(test)]
    pub(crate) fn has_filtered_buffer(&self) -> bool {
        self.filtered.is_some()
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
        self.discard_rename(cx);
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
        self.open_listing(parent, count, rows, window, cx);
    }

    /// Opens the member list of a folded tail that moved out of the body into
    /// its directory header. The tail is resolved from the live layout by its
    /// path, so a click after a resize or rescan lists the current members
    /// rather than the set captured at render time.
    pub(crate) fn open_tail_listing(
        &mut self,
        tail_path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.discard_rename(cx);
        let (parent, count, rows) = {
            let Some(state) = self.treemap_state.as_ref() else {
                return;
            };
            let Some(tail) = state.tail_by_path(tail_path) else {
                return;
            };
            let Some(parent) = cairn_core::treemap::aggregate_parent_path(&tail.path) else {
                return;
            };
            let parent = parent.to_string();
            let rows =
                block_listing_rows(&parent, &tail.member_indices, state.children_at(&parent));
            (parent, tail.count, rows)
        };
        self.open_listing(parent, count, rows, window, cx);
    }

    fn open_listing(
        &mut self,
        parent: String,
        count: usize,
        rows: Vec<FileRowData>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The listing owns the table now: an index answer in flight is
        // cancelled and its session released before these rows are installed.
        self.take_table_for_local_rows();
        // A refusal or stop of the query that preceded the block does not
        // describe these rows.
        self.query.rejection = None;
        self.query.cancelled = false;
        self.leave_drive_view();
        self.query.scope_drive = None;
        self.query.active_filter = FilterCategory::All;
        let mut listing = BlockListing::new(parent, count, rows);
        // The header arrow persists across the hand-off, so the rows arrive in
        // the order the table claims.
        listing.sort(self.results.table.sort_column, self.results.table.sort_asc);
        self.block_listing = Some(listing);
        self.query.search_input.set_text("");
        self.install_block_listing_rows(cx);
        self.query.search_focus.focus(window, cx);
        cx.notify();
    }

    /// Installs the first page of the current listing, from the top.
    pub(crate) fn install_block_listing_rows(&mut self, cx: &mut Context<Self>) {
        let Some(listing) = self.block_listing.as_ref() else {
            return;
        };
        let total = listing.total();
        let first_len = total.min(layout::RESULT_PAGE_SIZE);
        let rows = listing.rows()[..first_len].to_vec();
        self.results.table.set_rows(rows, total);
        self.note_table_content(cx);
        if total == 0 {
            self.note_table_empty(cx);
        }
        self.results.rows_scroll.set_offset(Point::default());
        self.results.table.scroll_top = 0.0;
        cx.notify();
    }

    /// Reorders the listing to the table's current sort and restarts at its top.
    pub(crate) fn sort_block_listing(&mut self, cx: &mut Context<Self>) {
        let (column, ascending) = (self.results.table.sort_column, self.results.table.sort_asc);
        let Some(listing) = self.block_listing.as_mut() else {
            return;
        };
        listing.sort(column, ascending);
        self.install_block_listing_rows(cx);
    }

    /// Drops rows the shell has deleted from the listing and re-applies the
    /// current search text.
    pub(crate) fn remove_block_listing_paths(&mut self, paths: &[String], cx: &mut Context<Self>) {
        let needle = self.query.search_input.text.trim().to_string();
        let case_sensitive = self.query.case_sensitive;
        let Some(listing) = self.block_listing.as_mut() else {
            return;
        };
        listing.remove_paths(paths);
        listing.apply_filter(&needle, case_sensitive);
        self.install_block_listing_rows(cx);
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
        self.install_block_listing_rows(cx);
    }

    /// Leaves the listing.
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
            path: parent.to_string(),
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

fn sort_rows(rows: &mut [FileRowData], column: SortColumn, ascending: bool) {
    match column {
        SortColumn::Name => rows.sort_by(|a, b| {
            cairn_core::sort::natural_cmp(&a.name, &b.name)
                .then_with(|| cairn_core::sort::natural_cmp(&a.path, &b.path))
        }),
        SortColumn::Path => rows.sort_by(|a, b| {
            cairn_core::sort::natural_cmp(&a.path, &b.path)
                .then_with(|| cairn_core::sort::natural_cmp(&a.name, &b.name))
        }),
        SortColumn::Size => rows.sort_by(|a, b| {
            a.size
                .cmp(&b.size)
                .then_with(|| cairn_core::sort::natural_cmp(&a.name, &b.name))
        }),
        SortColumn::DateModified => rows.sort_by(|a, b| {
            a.modified_ts
                .cmp(&b.modified_ts)
                .then_with(|| cairn_core::sort::natural_cmp(&a.name, &b.name))
        }),
        SortColumn::DateCreated => rows.sort_by(|a, b| {
            a.created_ts
                .cmp(&b.created_ts)
                .then_with(|| cairn_core::sort::natural_cmp(&a.name, &b.name))
        }),
        SortColumn::Extension => rows.sort_by(|a, b| {
            let ext = |row: &FileRowData| {
                row.name
                    .rsplit_once('.')
                    .map_or(String::new(), |(_, e)| e.to_lowercase())
            };
            ext(a)
                .cmp(&ext(b))
                .then_with(|| cairn_core::sort::natural_cmp(&a.name, &b.name))
        }),
    }
    if !ascending {
        rows.reverse();
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
        assert_eq!(rows[0].path, r"C:\");
        assert_eq!(rows[0].size, 300);
        assert!(rows[0].is_dir);
        assert_eq!(rows[1].path, r"C:\");
        assert_eq!(rows[1].modified_ts, 7);

        let rows = block_listing_rows(r"C:\Program Files", &[1], &children);
        assert_eq!(rows[0].path, r"C:\Program Files");
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

    #[test]
    fn a_rename_reaches_the_rows_a_later_reinstall_uses() {
        let children = vec![child("Windows", 300, true), child("boot.ini", 100, false)];
        let rows = block_listing_rows(r"C:\", &[0, 1], &children);
        let mut listing = BlockListing::new(r"C:\".to_string(), 2, rows);

        listing.rename_path(r"C:\boot.ini", "loader.ini");
        listing.apply_filter("loader", false);
        assert_eq!(listing.total(), 1);
        assert_eq!(listing.rows()[0].name, "loader.ini");

        listing.apply_filter("", false);
        assert!(
            listing.rows().iter().all(|row| row.name != "boot.ini"),
            "the old name must not come back through a reinstall"
        );
    }

    #[test]
    fn unfiltered_listing_shares_single_buffer_until_filtered() {
        let children = vec![child("Windows", 300, true), child("boot.ini", 100, false)];
        let rows = block_listing_rows(r"C:\", &[0, 1], &children);
        let mut listing = BlockListing::new(r"C:\".to_string(), 2, rows);
        assert!(!listing.has_filtered_buffer());
        assert_eq!(listing.rows().len(), 2);

        listing.apply_filter("boot", false);
        assert!(listing.has_filtered_buffer());
        assert_eq!(listing.rows().len(), 1);

        listing.apply_filter("", false);
        assert!(!listing.has_filtered_buffer());
        assert_eq!(listing.rows().len(), 2);
    }
}
