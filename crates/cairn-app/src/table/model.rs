// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Result table model: the fetched row window, selection, sort, and the viewport state that drives
//! windowed rendering (UI-02).
//!
//! The table owns viewport state only. Rendering a row stays with the view, and the view asks
//! [`visible_range`] which absolute row indices the current scroll position actually requires. The
//! scroll extent is the server-reported `total_rows`, so the scrollbar reflects the whole result set
//! even while a smaller window has arrived; indices outside [`TableState::loaded_range`] render as
//! loading placeholders and are fetched through the QRY-07 `(offset, limit)` contract.
//!
//! Row indices handed to this type are always *absolute* positions in the result set, never
//! positions inside `rows`, so selection and keyboard navigation keep working when the fetched
//! window moves.

use super::columns::{SortColumn, SortDirection, SortSpec};
use cairn_protocol::message::{FileRowData, MAX_SORT_KEYS};
use std::collections::{HashSet, VecDeque};
use std::ops::Range;

/// Rows rendered beyond the viewport edges, so a wheel or arrow key does not expose a blank band
/// before the next frame lands.
const OVERSCAN_ROWS: usize = 3;

/// Ctrl+A selects the fetched window, never the whole virtual result set.
/// This cap keeps one bulk action from collecting an unbounded number of rows;
/// when fewer rows are loaded, the selection is naturally smaller than the cap.
pub const SELECT_ALL_LIMIT: usize = 1000;

/// Computes the absolute row indices a viewport needs.
///
/// `scroll_top` is the distance scrolled down from the top of the row band, `viewport_height` the
/// visible band height, and `row_height` the fixed row height. The result is clamped to `total` and
/// widened by [`OVERSCAN_ROWS`] on both sides, so its length depends on the viewport and never on
/// the size of the result set.
pub fn visible_range(
    scroll_top: f32,
    viewport_height: f32,
    row_height: f32,
    total: usize,
) -> Range<usize> {
    if total == 0 || row_height <= 0.0 || viewport_height <= 0.0 {
        return 0..0;
    }
    let row_h = row_height as f64;
    let vp_h = viewport_height as f64;
    let max_scroll_top = ((total as f64) * row_h - vp_h).max(0.0);
    let scroll_top = (scroll_top as f64).max(0.0).min(max_scroll_top);

    let start_row = (scroll_top / row_h).floor() as usize;
    let visible_rows = (vp_h / row_h).ceil() as usize + 1;

    let start = start_row.saturating_sub(OVERSCAN_ROWS).min(total);
    let end = start_row
        .saturating_add(visible_rows)
        .saturating_add(OVERSCAN_ROWS)
        .min(total);
    start..end
}

/// Maximum number of rows retained in memory by `TableState` to prevent unbounded allocation.
pub const MAX_LOADED_ROWS: usize = 2000;

/// Row model plus selection, sort and viewport state for the result table.
#[derive(Debug, Default)]
pub struct TableState {
    /// Fetched window; `rows[i]` is the absolute row `rows_offset + i`.
    rows: VecDeque<FileRowData>,
    rows_offset: usize,
    total_rows: usize,
    empty_page_offsets: HashSet<usize>,
    /// Absolute end (exclusive) of the first page received from the most recent background
    /// refresh. Only `0..fresh_page_end` is known to carry the refreshed order.
    fresh_page_end: usize,
    /// `true` while the fetched window predates the most recent refresh. Its rows and their order
    /// cannot be trusted for the visible range until a page fetched after the refresh replaces
    /// them; they stay on screen meanwhile.
    revalidate: bool,
    /// Offset of the page the view requested to replace a stale window. It tells that response
    /// apart from a page that was already in flight when the refresh landed.
    revalidate_offset: Option<usize>,
    pub selected_indices: HashSet<usize>,
    pub focused_idx: Option<usize>,
    /// The row a Shift range extends from. Kept apart from `focused_idx`, which moves with every
    /// extension, so extending out and back still covers the same range.
    pub selection_anchor: Option<usize>,
    pub sort_spec: SortSpec,
    /// Pixels scrolled down from the first row of the band.
    pub scroll_top: f32,
    /// Measured height of the scrollable row band, in pixels.
    pub viewport_height: f32,
}

impl TableState {
    /// Replaces the fetched window with a fresh first page and resets viewport and selection.
    pub fn set_rows(&mut self, mut rows: Vec<FileRowData>, total_rows: usize) {
        if rows.len() > MAX_LOADED_ROWS {
            rows.truncate(MAX_LOADED_ROWS);
        }
        self.rows = VecDeque::from(rows);
        self.rows_offset = 0;
        self.total_rows = total_rows;
        self.empty_page_offsets.clear();
        self.empty_page_offsets.shrink_to_fit();
        self.fresh_page_end = self.rows.len();
        self.revalidate = false;
        self.revalidate_offset = None;
        self.selected_indices.clear();
        self.selected_indices.shrink_to_fit();
        self.focused_idx = None;
        self.selection_anchor = None;
        self.scroll_top = 0.0;
    }

    /// Clears any cached empty page offsets so the table can re-request pages after a session reset.
    pub fn clear_empty_pages(&mut self) {
        self.empty_page_offsets.clear();
        self.empty_page_offsets.shrink_to_fit();
    }

    /// Merges a newer first page into the fetched window without moving the viewport.
    ///
    /// The refresh carries only the first page, so a window the reader has paged away from cannot
    /// be replaced by it: doing so would leave every visible index unloaded and swap the table for
    /// placeholders until the viewport's own page came back. A top-anchored window adopts the new
    /// rows in place and keeps any rows it already held beyond the first page.
    ///
    /// The first page cannot vouch for the rows and order already on screen — an index that grew
    /// reorders the whole result set under any sort — so the window is marked for revalidation.
    /// The old rows stay visible until the visible range is fetched again and that page replaces
    /// the window ([`Self::page_request`], [`Self::apply_page`]).
    ///
    /// An old selection no longer names the rows it was made on, and a row that still exists could
    /// now sit somewhere else, so selection, focus and the range anchor are dropped outright. The
    /// scroll position is kept: a background refresh must not yank the user back to the top of a
    /// table they are reading.
    pub fn refresh_rows(&mut self, mut rows: Vec<FileRowData>, total_rows: usize) {
        if rows.len() > MAX_LOADED_ROWS {
            rows.truncate(MAX_LOADED_ROWS);
        }
        self.total_rows = total_rows;
        self.empty_page_offsets.clear();
        self.empty_page_offsets.shrink_to_fit();
        self.fresh_page_end = rows.len().min(total_rows);
        self.revalidate_offset = None;

        if self.rows_offset == 0 {
            if rows.len() >= self.rows.len() {
                self.rows = VecDeque::from(rows);
            } else {
                for (slot, fresh) in self.rows.iter_mut().zip(rows) {
                    *slot = fresh;
                }
            }
        }
        // See docs/engineering/performance.md#21-流式与虚拟化.
        let keep = total_rows
            .saturating_sub(self.rows_offset)
            .min(self.rows.len());
        self.rows.truncate(keep);
        if self.rows.is_empty() {
            self.rows_offset = 0;
        }
        self.clear_selection();
        self.revalidate = self.rows_offset != 0 || total_rows > self.fresh_page_end;
    }

    /// Installs a fetched page at its absolute offset, extending the window when it is contiguous or
    /// overlapping; the window is truncated to [`MAX_LOADED_ROWS`] from the edge the page grew toward.
    ///
    /// The page requested to revalidate a window after a background refresh replaces that window
    /// outright: the old rows must not survive just because they still cover the requested range.
    /// An empty page records its offset so the render-driven paging cannot ask for it again on every
    /// frame.
    pub fn apply_page(&mut self, offset: usize, page: Vec<FileRowData>) {
        if self.revalidate {
            if self.revalidate_offset != Some(offset) {
                return;
            }
            self.revalidate = false;
            self.revalidate_offset = None;
            if page.is_empty() {
                self.rows.clear();
                self.rows_offset = 0;
                self.empty_page_offsets.insert(offset);
                return;
            }
            let mut page = page;
            if page.len() > MAX_LOADED_ROWS {
                page.truncate(MAX_LOADED_ROWS);
            }
            self.empty_page_offsets
                .retain(|&off| off < offset || off >= offset + page.len());
            self.rows = VecDeque::from(page);
            self.rows_offset = offset;
            return;
        }

        if page.is_empty() {
            self.empty_page_offsets.insert(offset);
            return;
        }

        self.empty_page_offsets
            .retain(|&off| off < offset || off >= offset + page.len());

        let current_start = self.rows_offset;
        let current_end = self.rows_offset + self.rows.len();

        if self.rows.is_empty() {
            let mut page = page;
            if page.len() > MAX_LOADED_ROWS {
                page.truncate(MAX_LOADED_ROWS);
            }
            self.rows = VecDeque::from(page);
            self.rows_offset = offset;
        } else if offset <= current_start && offset + page.len() >= current_start {
            let overlap = (offset + page.len()) - current_start;
            let take_len = page.len() - overlap.min(page.len());
            if take_len > 0 {
                for item in page.into_iter().take(take_len).rev() {
                    self.rows.push_front(item);
                }
                self.rows_offset = offset;
            }
            if self.rows.len() > MAX_LOADED_ROWS {
                self.rows.truncate(MAX_LOADED_ROWS);
                let max_offset = self.rows_offset + self.rows.len();
                self.empty_page_offsets.retain(|&off| off <= max_offset);
            }
        } else if offset <= current_end && offset + page.len() >= current_end {
            let overlap = current_end - offset;
            if overlap < page.len() {
                self.rows.extend(page.into_iter().skip(overlap));
            }
            if self.rows.len() > MAX_LOADED_ROWS {
                let excess = self.rows.len() - MAX_LOADED_ROWS;
                self.rows.drain(0..excess);
                self.rows_offset += excess;
                let min_offset = self.rows_offset;
                self.empty_page_offsets.retain(|&off| off >= min_offset);
            }
        } else if offset >= current_start && offset + page.len() <= current_end {
        } else {
            let mut page = page;
            if page.len() > MAX_LOADED_ROWS {
                page.truncate(MAX_LOADED_ROWS);
            }
            self.rows = VecDeque::from(page);
            self.rows_offset = offset;
            let min_keep = offset.saturating_sub(MAX_LOADED_ROWS);
            let max_keep = offset.saturating_add(MAX_LOADED_ROWS * 2);
            self.empty_page_offsets
                .retain(|&off| off >= min_keep && off <= max_keep);
        }
    }

    /// Absolute row, when it is inside the fetched window.
    pub fn row(&self, ix: usize) -> Option<&FileRowData> {
        let local = ix.checked_sub(self.rows_offset)?;
        self.rows.get(local)
    }

    /// Absolute indices covered by the fetched window.
    pub fn loaded_range(&self) -> Range<usize> {
        self.rows_offset..self.rows_offset + self.rows.len()
    }

    pub fn is_loaded(&self, ix: usize) -> bool {
        self.loaded_range().contains(&ix)
    }

    pub fn total_rows(&self) -> usize {
        self.total_rows
    }

    /// Absolute indices the current frame must render.
    pub fn visible_range(&self, row_height: f32) -> Range<usize> {
        visible_range(
            self.scroll_top,
            self.viewport_height,
            row_height,
            self.total_rows,
        )
    }

    /// `(offset, limit)` to request so `visible` becomes available, or `None` when it already is.
    ///
    /// A viewport that still overlaps the fetched window extends it from the front, keeping already
    /// visible rows. A viewport fully outside it (a scrollbar jump) is page-aligned and replaces it.
    /// While a background refresh is being revalidated, the visible range is fetched even when the
    /// stale window still contains it.
    pub fn page_request(&self, visible: Range<usize>, page_size: usize) -> Option<(usize, usize)> {
        if self.total_rows == 0 || visible.is_empty() {
            return None;
        }
        if self.revalidate && (self.rows_offset != 0 || visible.end > self.fresh_page_end) {
            let offset = (visible.start / page_size * page_size).min(self.total_rows);
            let limit = visible
                .end
                .saturating_sub(offset)
                .max(page_size)
                .min(self.total_rows.saturating_sub(offset));
            return (limit > 0).then_some((offset, limit));
        }
        let loaded = self.loaded_range();
        if loaded.start == 0 && loaded.end >= self.total_rows {
            return None;
        }
        if visible.start >= loaded.start && visible.end <= loaded.end {
            return None;
        }

        let outside = visible.start >= loaded.end || visible.end <= loaded.start;
        if outside {
            let offset = (visible.start / page_size * page_size).min(self.total_rows);
            let limit = (visible.end - offset)
                .max(page_size)
                .min(self.total_rows.saturating_sub(offset));
            return self.requestable_page(offset, limit);
        }

        if visible.end > loaded.end {
            let offset = loaded.end;
            if offset >= self.total_rows {
                return None;
            }
            let limit = (visible.end - offset)
                .max(page_size)
                .min(self.total_rows - offset);
            return self.requestable_page(offset, limit);
        }

        if visible.start < loaded.start {
            let target_start = visible.start / page_size * page_size;
            let limit = (loaded.start - target_start).max(page_size);
            return self.requestable_page(target_start, limit);
        }

        None
    }

    /// Records that the view issued the page request at `offset`.
    ///
    /// While a refresh is being revalidated the response must replace the stale window instead of
    /// merging into it. Remembering the requested offset also keeps a page that was already in
    /// flight when the refresh landed from being mistaken for that replacement.
    pub fn note_page_request(&mut self, offset: usize) {
        if self.revalidate {
            self.revalidate_offset = Some(offset);
        }
    }

    /// A page request for `offset`, unless that offset already answered with nothing usable.
    fn requestable_page(&self, offset: usize, limit: usize) -> Option<(usize, usize)> {
        if limit == 0 || self.empty_page_offsets.contains(&offset) {
            None
        } else {
            Some((offset, limit))
        }
    }

    /// Renames the loaded row whose full path is `old_path`, keeping its position, selection and
    /// the table's order. The new-name highlights are cleared because they describe the old text;
    /// the background refresh reconciles the row with the index afterwards.
    pub fn rename_row(&mut self, old_path: &str, new_name: &str) -> bool {
        let old = std::path::Path::new(old_path);
        let (Some(parent), Some(name)) =
            (old.parent(), old.file_name().and_then(|name| name.to_str()))
        else {
            return false;
        };
        let Some(row) = self
            .rows
            .iter_mut()
            .find(|row| row.name == name && std::path::Path::new(&row.path) == parent)
        else {
            return false;
        };
        row.name.clear();
        row.name.push_str(new_name);
        row.name_highlights.clear();
        true
    }

    /// Full scrollable height of the band, driven by the server-reported match count.
    pub fn scroll_extent(&self, row_height: f32) -> f32 {
        self.total_rows as f32 * row_height
    }

    /// Largest `scroll_top` the band can reach for the given viewport.
    pub fn max_scroll_top(&self, row_height: f32) -> f32 {
        (self.scroll_extent(row_height) - self.viewport_height).max(0.0)
    }

    /// Applies a new scroll position, clamped to the band.
    pub fn set_scroll_top(&mut self, scroll_top: f32, row_height: f32) {
        self.scroll_top = scroll_top.clamp(0.0, self.max_scroll_top(row_height));
    }

    /// Scrolls the minimum amount that brings `ix` fully into the band.
    pub fn ensure_row_visible(&mut self, ix: usize, row_height: f32) {
        if self.viewport_height <= 0.0 {
            return;
        }
        let row_top = ix as f32 * row_height;
        let row_bottom = row_top + row_height;
        if row_top < self.scroll_top {
            self.set_scroll_top(row_top, row_height);
        } else if row_bottom > self.scroll_top + self.viewport_height {
            self.set_scroll_top(row_bottom - self.viewport_height, row_height);
        }
    }

    pub fn select_row(&mut self, ix: usize, extend: bool) {
        if extend {
            let anchor = self.selection_anchor.or(self.focused_idx).unwrap_or(ix);
            self.select_range(anchor, ix, true);
            return;
        }
        self.selected_indices.clear();
        self.selected_indices.insert(ix);
        self.focused_idx = Some(ix);
        self.selection_anchor = Some(ix);
    }

    /// Selects every row between `anchor` and `ix`. Shift+click replaces the selection unless
    /// Ctrl is also held; the anchor itself stays fixed so further extensions do not creep.
    pub fn select_range(&mut self, anchor: usize, ix: usize, additive: bool) {
        if !additive {
            self.selected_indices.clear();
        }
        let (start, end) = if anchor <= ix {
            (anchor, ix)
        } else {
            (ix, anchor)
        };
        self.selected_indices.extend(start..=end);
        self.focused_idx = Some(ix);
        self.selection_anchor = Some(anchor);
    }

    /// Ctrl+click switches one row without disturbing the rest of the selection.
    pub fn toggle_row(&mut self, ix: usize) {
        if !self.selected_indices.remove(&ix) {
            self.selected_indices.insert(ix);
        }
        self.focused_idx = Some(ix);
        self.selection_anchor = Some(ix);
    }

    /// Selects the loaded rows up to `limit` and returns how many were taken.
    /// The selection never reaches rows that have not been fetched.
    pub fn select_all_loaded(&mut self, limit: usize) -> usize {
        let loaded = self.loaded_range();
        let end = loaded.start.saturating_add(limit.min(loaded.len()));
        self.selected_indices.clear();
        self.selected_indices.extend(loaded.start..end);
        self.focused_idx = self.selected_indices.iter().copied().min();
        self.selection_anchor = self.focused_idx;
        self.selected_indices.len()
    }

    /// Drops the whole selection and focus. The shell calls this when a press lands outside the
    /// result rows, so a selected row never survives a click into the search box or sidebar.
    pub fn clear_selection(&mut self) {
        self.selected_indices.clear();
        self.focused_idx = None;
        self.selection_anchor = None;
    }

    pub fn move_selection_up(&mut self, extend: bool) {
        if let Some(curr) = self.focused_idx {
            if curr > 0 {
                self.select_row(curr - 1, extend);
            }
        } else if !self.loaded_range().is_empty() {
            self.select_row(self.loaded_range().start, false);
        }
    }

    pub fn move_selection_down(&mut self, extend: bool) {
        let last = self.loaded_range().end.saturating_sub(1);
        if let Some(curr) = self.focused_idx {
            if curr < last {
                self.select_row(curr + 1, extend);
            }
        } else if !self.loaded_range().is_empty() {
            self.select_row(self.loaded_range().start, false);
        }
    }

    /// The row a single-row action such as rename may target: exactly one selected row, and that row
    /// must be inside the fetched window so its name and path are known.
    pub fn sole_selected_row(&self) -> Option<usize> {
        let mut selected = self.selected_indices.iter();
        let ix = *selected.next()?;
        if selected.next().is_some() || !self.is_loaded(ix) {
            return None;
        }
        Some(ix)
    }

    /// The row a keyboard command acts on: the focused row, else the lowest selected one.
    pub fn target_row(&self) -> Option<usize> {
        self.focused_idx
            .or_else(|| self.selected_indices.iter().copied().min())
    }

    pub fn primary_sort(&self) -> (SortColumn, bool) {
        let primary = self.sort_spec.primary();
        (
            primary.column,
            primary.direction == SortDirection::Ascending,
        )
    }

    pub fn toggle_sort(&mut self, col: SortColumn) {
        self.sort_spec
            .set_or_toggle_primary(col, SortDirection::Ascending);
    }

    /// Replaces the sort with a single primary key, for local listings that only support the
    /// columns their table shows.
    pub fn set_primary_sort(&mut self, column: SortColumn, ascending: bool) {
        let direction = if ascending {
            SortDirection::Ascending
        } else {
            SortDirection::Descending
        };
        self.sort_spec = SortSpec::single(column, direction);
    }

    pub fn shift_toggle_sort(&mut self, col: SortColumn) {
        let already_present = self.sort_spec.keys.iter().any(|key| key.column == col);
        if !already_present && self.sort_spec.keys.len() >= MAX_SORT_KEYS {
            return;
        }
        self.sort_spec
            .shift_toggle_column(col, SortDirection::Ascending);
    }

    pub fn reverse_primary_sort(&mut self) {
        if let Some(primary) = self.sort_spec.keys.first_mut() {
            primary.direction = primary.direction.flipped();
        }
    }

    /// 1-based sort priority rank and ascending flag when `col` participates in `sort_spec`.
    pub fn sort_rank(&self, col: SortColumn) -> Option<(usize, bool)> {
        self.sort_spec
            .keys
            .iter()
            .enumerate()
            .find(|(_, key)| key.column == col)
            .map(|(ix, key)| (ix + 1, key.direction == SortDirection::Ascending))
    }

    /// Removes `col` from `sort_spec`, restoring default `name-asc` when it was the sole key.
    pub fn remove_sort_column(&mut self, col: SortColumn) -> bool {
        let before = self.sort_spec.keys.len();
        self.sort_spec.keys.retain(|key| key.column != col);
        if self.sort_spec.keys.len() == before {
            return false;
        }
        if self.sort_spec.keys.is_empty() {
            self.sort_spec = SortSpec::default();
        }
        true
    }
}

/// Renders a row's timestamp as `YYYY.MM.DD HH:mm` in the user's local time via the bounded
/// minute-bucket cache.
pub fn format_timestamp(ts: i64) -> String {
    crate::table::presentation::format_timestamp_cached(ts).to_string()
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
