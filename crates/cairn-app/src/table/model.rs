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

use cairn_protocol::message::FileRowData;
use std::collections::{HashSet, VecDeque};
use std::ops::Range;

/// Rows rendered beyond the viewport edges, so a wheel or arrow key does not expose a blank band
/// before the next frame lands.
const OVERSCAN_ROWS: usize = 3;

/// Lane widths the table grows its four columns from, in rem. They are ratios, not fixed extents:
/// a wider window makes every lane wider in proportion, and a narrower one shrinks them down to
/// their minima before the band scrolls sideways. Dragging a boundary trades width between the two
/// neighbours, so the total — and therefore every other lane — stays where it was.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColumnWidths {
    pub name: f32,
    pub path: f32,
    pub size: f32,
    pub date: f32,
}

/// The draggable boundary between two neighbouring lanes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnBoundary {
    NamePath,
    PathSize,
    SizeDate,
}

impl ColumnBoundary {
    /// Stable element-id suffix, one per boundary.
    pub const fn key(self) -> &'static str {
        match self {
            Self::NamePath => "name-path",
            Self::PathSize => "path-size",
            Self::SizeDate => "size-date",
        }
    }

    fn indices(self) -> (usize, usize) {
        match self {
            Self::NamePath => (0, 1),
            Self::PathSize => (1, 2),
            Self::SizeDate => (2, 3),
        }
    }
}

impl ColumnWidths {
    /// Moves `boundary` by `delta` rem, taking width from the lane on its right and giving it to
    /// the lane on its left. Either lane may stop at its own minimum; the other then keeps the
    /// remainder, so a drag can never squeeze a column out of existence.
    pub fn resized(self, boundary: ColumnBoundary, delta: f32, minima: [f32; 4]) -> Self {
        let (left, right) = boundary.indices();
        let mut widths = [self.name, self.path, self.size, self.date];
        debug_assert!(
            widths[left] + widths[right] >= minima[left] + minima[right],
            "column minima exceed their combined width"
        );
        let delta = delta.clamp(
            -(widths[left] - minima[left]),
            widths[right] - minima[right],
        );
        widths[left] += delta;
        widths[right] -= delta;
        Self {
            name: widths[0],
            path: widths[1],
            size: widths[2],
            date: widths[3],
        }
    }

    pub fn total(self) -> f32 {
        self.name + self.path + self.size + self.date
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SortColumn {
    #[default]
    Name,
    Path,
    Size,
    DateModified,
    DateCreated,
    Extension,
}

impl SortColumn {
    /// The Fluent message naming this column, shared by its table header and its 排序 item so one
    /// column never carries two wordings.
    pub const fn label_key(self) -> &'static str {
        match self {
            SortColumn::Name => "col_name",
            SortColumn::Path => "col_path",
            SortColumn::Size => "col_size",
            SortColumn::DateModified => "col_modified",
            SortColumn::DateCreated => "col_created",
            SortColumn::Extension => "col_ext",
        }
    }

    /// The same column inside the Recycle Bin holds the original location and the deletion time,
    /// so its header and its 排序 item have to carry those names instead.
    pub const fn recycle_bin_label_key(self) -> &'static str {
        match self {
            SortColumn::Path => "col_original_location",
            SortColumn::DateModified => "col_deleted",
            _ => self.label_key(),
        }
    }
}

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

/// Maximum number of rows retained in memory by TableState to prevent unbounded allocation.
pub const MAX_LOADED_ROWS: usize = 2000;

/// Row model plus selection, sort and viewport state for the result table.
#[derive(Debug, Default)]
pub struct TableState {
    /// Fetched window; `rows[i]` is the absolute row `rows_offset + i`.
    rows: VecDeque<FileRowData>,
    rows_offset: usize,
    total_rows: usize,
    /// Offsets already requested that produced no row. The service skips rows whose volume or
    /// record is gone and can still answer an in-range page with an empty list, so without this set
    /// the same viewport keeps asking for the same offset every frame and never stops.
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
    pub sort_column: SortColumn,
    pub sort_asc: bool,
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
        // A result set that shrank below the window still owns the display; clip only the part
        // that no longer exists. A deep window stays where it is and its page is fetched by the
        // viewport when the list itself changes.
        let keep = total_rows
            .saturating_sub(self.rows_offset)
            .min(self.rows.len());
        self.rows.truncate(keep);
        if self.rows.is_empty() {
            self.rows_offset = 0;
        }
        self.clear_selection();
        // A deep window is wholly old, and even a top-anchored one stops being trustworthy below
        // the rows the refresh actually delivered.
        self.revalidate = self.rows_offset != 0 || total_rows > self.fresh_page_end;
    }

    /// Installs a fetched page at its absolute offset, extending the window when it is contiguous
    /// or overlapping, with bounded LRU eviction to prevent runaway heap growth.
    ///
    /// The page requested to revalidate a window after a background refresh replaces that window
    /// outright: the old rows must not survive just because they still cover the requested range.
    /// An empty page records its offset so the render-driven paging cannot ask for it again on every
    /// frame.
    pub fn apply_page(&mut self, offset: usize, page: Vec<FileRowData>) {
        if self.revalidate {
            if self.revalidate_offset != Some(offset) {
                // A page requested before the refresh is still arriving. It carries the
                // generation that was just invalidated, so merging it would put stale rows (or
                // their old order) back into the window. The pending revalidation request fetches
                // the visible range again.
                return;
            }
            // The response the view asked for after the refresh replaces the stale generation;
            // containment against the old window must not turn it into a no-op.
            self.revalidate = false;
            self.revalidate_offset = None;
            if page.is_empty() {
                // Retaining the old rows would leave the stale generation on screen for as long
                // as the viewport stays here. Drop them so the band can show its loading state,
                // and retire the offset so the same empty page is not requested every frame.
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

        // A non-empty page clears any empty marker that covered its range.
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
            // Overlapping or exact prepend when scrolling backwards
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
            // Overlapping or exact append when scrolling forwards
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
            // The received page is already completely contained within loaded_range - no op!
        } else {
            // Disjoint page jump (e.g. scrollbar jump to far position)
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
            // The rows inside the window predate the refresh, so their presence says nothing
            // about the visible range still being current. Fetch from the page that covers the
            // viewport and far enough to replace every row it shows; the old rows stay on screen
            // until this page lands. `empty_page_offsets` was cleared by the refresh, so a fresh
            // generation never inherits the previous one's retired offsets.
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
    ///
    /// [`Self::apply_page`] records empty and non-extending pages; repeating one would keep the
    /// frame's placeholder band alive forever, because the fetched window never advances past it.
    fn requestable_page(&self, offset: usize, limit: usize) -> Option<(usize, usize)> {
        if limit == 0 || self.empty_page_offsets.contains(&offset) {
            None
        } else {
            Some((offset, limit))
        }
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

    /// Scrolls by `delta` pixels; positive moves towards later rows. The
    /// windowed-rendering tests drive this instead of a scroll handle.
    #[cfg(test)]
    pub fn scroll_by(&mut self, delta: f32, row_height: f32) {
        self.set_scroll_top(self.scroll_top + delta, row_height);
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

    /// Selects every loaded row. Only the model tests exercise it today; the
    /// window's Ctrl+A selects the query text instead.
    #[cfg(test)]
    pub fn select_all(&mut self) {
        self.selected_indices.clear();
        for ix in self.loaded_range() {
            self.selected_indices.insert(ix);
        }
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

    /// The row a keyboard command acts on: the focused row, else the lowest selected one.
    pub fn target_row(&self) -> Option<usize> {
        self.focused_idx
            .or_else(|| self.selected_indices.iter().copied().min())
    }

    pub fn toggle_sort(&mut self, col: SortColumn) {
        if self.sort_column == col {
            self.sort_asc = !self.sort_asc;
        } else {
            self.sort_column = col;
            self.sort_asc = true;
        }
    }
}

/// Civil year, month and day for a count of days since the Unix epoch.
///
/// Howard Hinnant's `civil_from_days`: it moves across the 400-year Gregorian cycle with integer
/// arithmetic only, so it needs no lookup table and no dependency.
fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    // The algorithm runs March..February, so January and February belong to the year just counted.
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn format_utc_seconds(sec: i64) -> String {
    let (year, month, day) = civil_from_days(sec.div_euclid(86400));
    let rem_sec = sec.rem_euclid(86400);
    format!(
        "{:04}.{:02}.{:02} {:02}:{:02}",
        year,
        month,
        day,
        rem_sec / 3600,
        (rem_sec % 3600) / 60
    )
}

/// The deterministic UTC rendering used by the calendar tests; production rows display local time.
#[cfg(test)]
pub fn format_timestamp_utc(ts: i64) -> String {
    if ts == 0 {
        return String::new();
    }
    format_utc_seconds(cairn_core::time::timestamp_to_unix_seconds(ts))
}

/// Renders a row's timestamp as `YYYY.MM.DD HH:mm` in the user's local time. The index and the
/// Recycle Bin store UTC; Explorer and the native shell warnings show the local clock, so the table
/// must present the same wall time.
pub fn format_timestamp(ts: i64) -> String {
    if ts == 0 {
        return String::new();
    }
    let sec = cairn_core::time::timestamp_to_unix_seconds(ts);
    #[cfg(windows)]
    if let Some(local) = format_local_seconds(sec) {
        return local;
    }
    format_utc_seconds(sec)
}

#[cfg(windows)]
fn format_local_seconds(sec: i64) -> Option<String> {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
    use windows::Win32::System::Time::FileTimeToSystemTime;

    let ticks = (sec as i128 + 11_644_473_600).checked_mul(10_000_000)?;
    if !(0..=u64::MAX as i128).contains(&ticks) {
        return None;
    }
    let ticks = ticks as u64;
    let utc = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut local = FILETIME::default();
    // SAFETY: both pointers reference live FILETIME values; the API only writes the output.
    unsafe { FileTimeToLocalFileTime(&utc, &mut local) }.ok()?;
    let mut system = SYSTEMTIME::default();
    // SAFETY: the local FILETIME is initialized and the output points at a live SYSTEMTIME.
    unsafe { FileTimeToSystemTime(&local, &mut system) }.ok()?;
    Some(format!(
        "{:04}.{:02}.{:02} {:02}:{:02}",
        system.wYear, system.wMonth, system.wDay, system.wHour, system.wMinute
    ))
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::{
        format_timestamp, format_timestamp_utc, visible_range, ColumnBoundary, ColumnWidths,
        SortColumn, TableState, OVERSCAN_ROWS,
    };
    use cairn_protocol::message::FileRowData;

    const ROW_HEIGHT: f32 = 22.0;
    const VIEWPORT: f32 = 440.0;
    const PAGE: usize = 200;

    fn row(name: &str) -> FileRowData {
        FileRowData {
            name: name.to_string(),
            path: "C:\\".to_string(),
            ..Default::default()
        }
    }

    /// A table whose server-reported total is `total` but whose fetched window is `loaded` rows.
    fn table(loaded: usize, total: usize) -> TableState {
        let mut table = TableState {
            total_rows: total,
            viewport_height: VIEWPORT,
            ..Default::default()
        };
        table.set_rows(
            (0..loaded).map(|i| row(&format!("file{i}.rs"))).collect(),
            total,
        );
        table
    }

    #[test]
    fn visible_range_size_depends_only_on_the_viewport() {
        let small = visible_range(0.0, VIEWPORT, ROW_HEIGHT, 100);
        let huge = visible_range(0.0, VIEWPORT, ROW_HEIGHT, 100_000);
        assert_eq!(
            small, huge,
            "result-set size must not change the rendered row count"
        );

        let rows_in_viewport = (VIEWPORT / ROW_HEIGHT) as usize;
        assert!(
            huge.len() <= rows_in_viewport + OVERSCAN_ROWS * 2 + 1,
            "rendered {} rows for a {rows_in_viewport} row viewport",
            huge.len()
        );
        assert!(
            huge.len() >= rows_in_viewport,
            "the viewport must be fully covered"
        );
    }

    #[test]
    fn visible_range_is_clamped_to_the_result_set() {
        assert_eq!(visible_range(0.0, VIEWPORT, ROW_HEIGHT, 0), 0..0);
        assert_eq!(visible_range(0.0, VIEWPORT, ROW_HEIGHT, 4), 0..4);
        assert_eq!(
            visible_range(4.0 * ROW_HEIGHT, VIEWPORT, ROW_HEIGHT, 5),
            0..5
        );
    }

    #[test]
    fn visible_range_tracks_the_scroll_offset() {
        let top = visible_range(0.0, VIEWPORT, ROW_HEIGHT, 100_000);
        let deep = visible_range(2_000.0, VIEWPORT, ROW_HEIGHT, 100_000);
        assert!(deep.start > top.start + OVERSCAN_ROWS);
        let bound = (VIEWPORT / ROW_HEIGHT) as usize + OVERSCAN_ROWS * 2 + 1;
        assert!(deep.len() <= bound && top.len() <= bound);
        assert_eq!(
            deep.start + OVERSCAN_ROWS,
            (2_000.0 / ROW_HEIGHT).floor() as usize
        );
    }

    #[test]
    fn visible_range_rejects_degenerate_geometry() {
        assert_eq!(visible_range(0.0, 0.0, ROW_HEIGHT, 10), 0..0);
        assert_eq!(visible_range(0.0, VIEWPORT, 0.0, 10), 0..0);
        // Overscroll bounces off the end of the result set rather than panicking.
        assert_eq!(visible_range(1.0e9, VIEWPORT, ROW_HEIGHT, 10), 0..10);
    }

    #[test]
    fn a_million_rows_render_a_constant_number_of_slots() {
        let table = table(200, 1_000_000);
        let range = table.visible_range(ROW_HEIGHT);
        assert!(
            range.len() < 64,
            "{} slots for one million matches",
            range.len()
        );
    }

    #[test]
    fn scroll_top_is_clamped_to_the_band() {
        let mut table = table(100, 100_000);
        // The band is 100,000 rows * 22px tall, so only a delta beyond that extent can reach the
        // far clamp.
        table.scroll_by(5_000_000.0, ROW_HEIGHT);
        assert_eq!(table.scroll_top, table.max_scroll_top(ROW_HEIGHT));
        table.scroll_by(-1.0e9, ROW_HEIGHT);
        assert_eq!(table.scroll_top, 0.0);
    }

    #[test]
    fn unfetched_rows_still_extend_the_band() {
        let table = table(1, 1_000_000);
        assert_eq!(table.loaded_range(), 0..1);
        assert_eq!(table.scroll_extent(ROW_HEIGHT), 1_000_000.0 * ROW_HEIGHT);
        assert!(!table.is_loaded(500));
        assert!(table.is_loaded(0));
        assert!(table.row(500).is_none());
        assert_eq!(table.row(0).map(|r| r.name.as_str()), Some("file0.rs"));
    }

    #[test]
    fn ensure_row_visible_moves_the_minimum_amount() {
        let mut table = table(10_000, 10_000);
        table.ensure_row_visible(3, ROW_HEIGHT);
        assert_eq!(table.scroll_top, 0.0, "row 3 is already visible");

        table.ensure_row_visible(500, ROW_HEIGHT);
        assert!(table.scroll_top > 0.0);
        let range = table.visible_range(ROW_HEIGHT);
        assert!(range.contains(&500), "row 500 must be inside {range:?}");
        assert!(
            range.len() <= (VIEWPORT / ROW_HEIGHT) as usize + OVERSCAN_ROWS * 2 + 1,
            "row 500 rendered {} slots",
            range.len()
        );
    }

    #[test]
    fn overlapping_viewport_extends_the_window_without_dropping_visible_rows() {
        let before = table(200, 100_000);
        let visible = 180..220;
        assert_eq!(before.page_request(visible, PAGE), Some((200, PAGE)));

        let mut after = table(200, 100_000);
        after.apply_page(
            200,
            (200..400).map(|i| row(&format!("file{i}.rs"))).collect(),
        );
        assert_eq!(after.loaded_range(), 0..400, "contiguous pages append");
        assert!(after.row(180).is_some(), "already visible rows survive");
    }

    #[test]
    fn a_scrollback_jump_pages_to_the_viewport() {
        let mut table = table(200, 1_000_000);
        table.viewport_height = VIEWPORT;
        table.scroll_by(44_000.0, ROW_HEIGHT); // row 2000
        let visible = table.visible_range(ROW_HEIGHT);
        let (offset, limit) = table
            .page_request(visible.clone(), PAGE)
            .expect("the jumped range needs a page");
        assert_eq!(offset % PAGE, 0, "a jump lands on a page boundary");
        assert!(
            offset <= visible.start && offset + limit >= visible.end,
            "the requested page must cover the viewport {visible:?}"
        );

        let mut after = table;
        after.apply_page(offset, (offset..offset + limit).map(|_| row("x")).collect());
        assert_eq!(
            after.loaded_range(),
            offset..offset + limit,
            "a jump replaces the window"
        );
        assert!(visible.clone().all(|ix| after.is_loaded(ix)));
        assert!(
            after.page_request(visible, PAGE).is_none(),
            "no repeat request"
        );
    }

    #[test]
    fn no_page_is_requested_when_everything_is_loaded() {
        let table = table(10, 10);
        assert_eq!(table.page_request(0..10, PAGE), None);
        assert_eq!(table.page_request(0..100_000, PAGE), None);
        assert_eq!(TableState::default().page_request(0..10, PAGE), None);
    }

    /// The service skips rows whose volume or record vanished and still returns `RowsResult`; an
    /// empty page must still advance `loaded_range` so the render pass does not ask for the same
    /// offset again every frame, forever.
    #[test]
    fn an_empty_page_is_not_requested_again() {
        let mut table = table(200, 1_000);
        let visible = 180..400;
        assert_eq!(table.page_request(visible.clone(), PAGE), Some((200, PAGE)));

        table.apply_page(200, Vec::new());
        assert_eq!(table.loaded_range(), 0..200, "an empty page adds no rows");
        assert_eq!(
            table.page_request(visible, PAGE),
            None,
            "the same offset must not be fetched again"
        );
        assert_eq!(table.page_request(200..400, PAGE), None);
        assert!(
            table.page_request(600..800, PAGE).is_some(),
            "a different page is still reachable after one empty page"
        );
    }

    /// A page that is already inside the fetched window cannot advance it either. Recording it is
    /// what keeps a duplicate or overlapping response from becoming the offset the next viewport
    /// jump asks for again and again.
    /// A page that is already inside the fetched window does not widen it, but contains valid
    /// rows and must NOT be marked as an empty page. If the window later evicts or jumps away,
    /// scrolling back to that range must request it again rather than permanently refusing it.
    #[test]
    fn a_contained_non_empty_page_is_not_marked_empty() {
        let mut table = table(0, 1_000);
        table.apply_page(200, (200..400).map(|i| row(&format!("f{i}"))).collect());
        assert_eq!(table.loaded_range(), 200..400);

        table.apply_page(200, (200..250).map(|i| row(&format!("f{i}"))).collect());
        assert_eq!(
            table.loaded_range(),
            200..400,
            "a contained page adds nothing"
        );

        table.apply_page(600, (600..800).map(|i| row(&format!("f{i}"))).collect());
        assert_eq!(table.loaded_range(), 600..800);
        assert_eq!(
            table.page_request(200..300, PAGE),
            Some((200, PAGE)),
            "valid rows can be fetched again after jumping away"
        );
    }

    #[test]
    fn lru_eviction_prunes_empty_page_offsets() {
        let mut table = table(0, 10_000);

        // Prepend scrolling: loaded range starts at 1000..3000
        table.apply_page(1000, (1000..3000).map(|i| row(&format!("f{i}"))).collect());
        assert_eq!(table.loaded_range(), 1000..3000);

        // Empty page recorded at 4000
        table.apply_page(4000, Vec::new());
        assert!(table.empty_page_offsets.contains(&4000));

        // Prepend another 200 rows at 800: window becomes 800..2800 (3000 truncated)
        table.apply_page(800, (800..1000).map(|i| row(&format!("f{i}"))).collect());
        assert_eq!(table.loaded_range(), 800..2800);
        assert!(
            !table.empty_page_offsets.contains(&4000),
            "offsets beyond the truncated tail must be pruned"
        );

        // Record an empty page at 500 (before head)
        table.apply_page(500, Vec::new());
        assert!(table.empty_page_offsets.contains(&500));

        // Append 200 rows at 2800: window expands to 3000, drains 200 rows from head to 1000..3000
        table.apply_page(2800, (2800..3000).map(|i| row(&format!("f{i}"))).collect());
        assert_eq!(table.loaded_range(), 1000..3000);
        assert!(
            !table.empty_page_offsets.contains(&500),
            "offsets before the drained head must be pruned"
        );
    }

    #[test]
    fn non_empty_page_clears_prior_empty_page_offset() {
        let mut table = table(0, 10_000);
        // Initially offset 400 answers with empty
        table.apply_page(400, Vec::new());
        assert!(table.empty_page_offsets.contains(&400));
        assert_eq!(table.page_request(400..600, PAGE), None);

        // Later offset 400 returns valid rows
        table.apply_page(400, (400..600).map(|i| row(&format!("f{i}"))).collect());
        assert!(
            !table.empty_page_offsets.contains(&400),
            "landing non-empty rows must remove offset from empty_page_offsets"
        );

        // Disjoint jump far away
        table.apply_page(5000, (5000..5200).map(|i| row(&format!("f{i}"))).collect());
        // Requesting 400..600 again should succeed
        assert_eq!(table.page_request(400..600, PAGE), Some((400, PAGE)));
    }

    #[test]
    fn a_new_query_clears_recorded_empty_pages() {
        let mut table = table(200, 1_000);
        table.apply_page(200, Vec::new());
        assert_eq!(table.page_request(200..400, PAGE), None);

        table.set_rows(
            (0..200).map(|i| row(&format!("file{i}.rs"))).collect(),
            1_000,
        );
        assert_eq!(
            table.page_request(200..400, PAGE),
            Some((200, PAGE)),
            "a fresh result set may retry an offset the old one could not fill"
        );
    }

    /// A short page must still advance the window: the next request starts after the rows that did
    /// arrive, so one offset is never fetched twice and a row's absolute index stays its position.
    #[test]
    fn a_short_page_advances_the_next_request_without_misaligning_rows() {
        let mut table = table(200, 1_000);
        table.apply_page(
            200,
            (200..250).map(|i| row(&format!("file{i}.rs"))).collect(),
        );

        assert_eq!(table.loaded_range(), 0..250);
        assert_eq!(
            table.row(200).map(|r| r.name.as_str()),
            Some("file200.rs"),
            "the first row of a short page keeps its absolute index"
        );
        assert_eq!(
            table.page_request(180..400, PAGE),
            Some((250, PAGE)),
            "the hole between 250 and total starts a new request, not the old one"
        );
    }

    #[test]
    fn selection_uses_absolute_indices_and_survives_a_moving_window() {
        let mut table = table(2, 10_000);
        table.select_row(1, false);
        table.apply_page(2, (2..202).map(|_| row("x")).collect());

        assert!(table.selected_indices.contains(&1));
        table.move_selection_down(false);
        assert_eq!(table.focused_idx, Some(2));
        table.move_selection_down(false);
        assert_eq!(table.focused_idx, Some(3));
        table.select_all();
        assert_eq!(table.selected_indices.len(), table.loaded_range().len());
    }

    #[test]
    fn selection_cannot_move_past_the_loaded_window() {
        let mut table = table(2, 1_000_000);
        table.select_row(1, false);
        for _ in 0..10 {
            table.move_selection_down(false);
        }
        assert_eq!(table.focused_idx, Some(1));
    }

    #[test]
    fn shift_ranges_from_a_fixed_anchor_and_ctrl_toggles_one_row() {
        let mut table = table(2, 100);
        table.select_row(2, false);
        table.select_row(6, true);

        assert_eq!(table.selection_anchor, Some(2));
        assert_eq!(table.focused_idx, Some(6));
        assert_eq!(
            table.selected_indices,
            std::collections::HashSet::from([2, 3, 4, 5, 6])
        );

        table.toggle_row(4);
        assert!(!table.selected_indices.contains(&4));
        assert_eq!(table.selection_anchor, Some(4));

        table.select_range(1, 3, true);
        assert!(table.selected_indices.contains(&1));
        assert_eq!(table.selection_anchor, Some(1));
        assert_eq!(
            table.selected_indices,
            std::collections::HashSet::from([1, 2, 3, 5, 6])
        );

        table.clear_selection();
        assert_eq!(table.selection_anchor, None);
    }

    #[test]
    fn set_rows_resets_the_viewport_and_selection() {
        let mut table = table(10, 10);
        table.select_row(7, false);
        table.set_scroll_top(900.0, ROW_HEIGHT);

        table.set_rows(vec![row("a.rs")], 1);
        assert_eq!(table.scroll_top, 0.0);
        assert!(table.selected_indices.is_empty());
        assert_eq!(table.focused_idx, None);
        assert_eq!(table.loaded_range(), 0..1);
    }

    fn widths() -> ColumnWidths {
        ColumnWidths {
            name: 20.0,
            path: 22.5,
            size: 6.875,
            date: 10.0,
        }
    }

    const MINIMA: [f32; 4] = [12.0, 8.0, 6.0, 9.5];

    #[test]
    fn dragging_a_boundary_trades_width_between_its_two_lanes() {
        let before = widths();
        let after = before.resized(ColumnBoundary::NamePath, 3.0, MINIMA);
        assert_eq!(after.name, 23.0);
        assert_eq!(after.path, 19.5);
        assert_eq!(after.size, before.size, "a drag touches only two lanes");
        assert_eq!(after.date, before.date);
        assert!((after.total() - before.total()).abs() < f32::EPSILON);
    }

    #[test]
    fn a_drag_stops_when_either_neighbour_reaches_its_minimum() {
        let collapsed_name = widths().resized(ColumnBoundary::NamePath, -100.0, MINIMA);
        assert_eq!(collapsed_name.name, MINIMA[0]);
        assert_eq!(collapsed_name.path, 30.5);

        let collapsed_path = widths().resized(ColumnBoundary::NamePath, 100.0, MINIMA);
        assert_eq!(collapsed_path.path, MINIMA[1]);
        assert_eq!(collapsed_path.name, 34.5);
    }

    #[test]
    fn the_numeric_boundaries_move_their_own_lanes() {
        let after = widths().resized(ColumnBoundary::SizeDate, -0.5, MINIMA);
        assert_eq!(after.size, 6.375);
        assert_eq!(after.date, 10.5);
        assert_eq!(after.name, 20.0);
        assert_eq!(after.path, 22.5);
    }

    #[test]
    fn a_background_refresh_keeps_the_viewport_and_drops_dead_selection() {
        let mut table = table(10, 100);
        table.select_row(7, false);
        table.set_scroll_top(400.0, ROW_HEIGHT);

        table.refresh_rows((0..5).map(|i| row(&format!("r{i}.rs"))).collect(), 5);

        assert_eq!(
            table.scroll_top, 400.0,
            "an index that grew must not pull the reader back to the top"
        );
        assert!(
            table.selected_indices.is_empty(),
            "row 7 no longer names the row it was selected on"
        );
        assert_eq!(table.total_rows(), 5);
        assert_eq!(table.loaded_range(), 0..5);
    }

    /// A status refresh only carries the first page. Splicing it into the window replaces every
    /// row the reader paged to with an unloaded index, and the table turns into placeholders until
    /// the viewport's own page is fetched again.
    #[test]
    fn a_background_refresh_does_not_replace_a_deep_window() {
        let mut table = table(0, 1_000_000);
        table.apply_page(
            70_000,
            (70_000..70_200)
                .map(|i| row(&format!("old{i}.rs")))
                .collect(),
        );

        table.refresh_rows(
            (0..200).map(|i| row(&format!("new{i}.rs"))).collect(),
            1_000_100,
        );

        assert_eq!(table.total_rows(), 1_000_100);
        assert_eq!(
            table.loaded_range(),
            70_000..70_200,
            "the page the reader is looking at must survive a first-page refresh"
        );
        assert!(table.row(70_000).is_some());
        assert!(
            table.row(0).is_none(),
            "the first page must not be spliced into a disjoint window"
        );
    }

    /// The refreshed first page is newer than the rows below it, but dropping those rows would
    /// leave a viewport that is still inside the fetched window without any rows to draw.
    #[test]
    fn a_top_anchored_refresh_keeps_rows_below_its_first_page() {
        let mut table = table(400, 1_000);
        table.refresh_rows(
            (0..200).map(|i| row(&format!("new{i}.rs"))).collect(),
            1_010,
        );

        assert_eq!(table.loaded_range(), 0..400);
        assert_eq!(table.row(0).map(|r| r.name.as_str()), Some("new0.rs"));
        assert_eq!(
            table.row(200).map(|r| r.name.as_str()),
            Some("file200.rs"),
            "a page shorter than the window updates only its own rows"
        );
    }

    /// A refresh that arrives without a first page cannot update anything; it also must not clear
    /// the only rows the table has to show.
    #[test]
    fn an_empty_background_refresh_keeps_the_window() {
        let mut table = table(200, 1_000);
        table.refresh_rows(Vec::new(), 1_200);
        assert_eq!(table.loaded_range(), 0..200);
        assert_eq!(table.total_rows(), 1_200);
    }

    /// A deep window cannot be rewritten by a first-page refresh, so its rows stay on screen. They
    /// are of the old generation, though: the next viewport request must fetch that region again
    /// and replace the window instead of being suppressed by the rows still loaded there.
    #[test]
    fn a_deep_window_refresh_revalidates_the_visible_rows() {
        let mut table = table(0, 1_000_000);
        table.apply_page(
            70_000,
            (70_000..70_200)
                .map(|i| row(&format!("old{i}.rs")))
                .collect(),
        );
        table.set_scroll_top(70_000.0 * ROW_HEIGHT, ROW_HEIGHT);
        let visible = table.visible_range(ROW_HEIGHT);
        assert!(visible.start < 70_000 && visible.end > 70_000);

        table.refresh_rows(
            (0..200).map(|i| row(&format!("new{i}.rs"))).collect(),
            1_000_100,
        );
        assert_eq!(
            table.row(70_000).map(|r| r.name.as_str()),
            Some("old70000.rs"),
            "the rows already on screen survive until their replacement lands"
        );

        let (offset, limit) = table
            .page_request(visible.clone(), PAGE)
            .expect("the refreshed generation must fetch the visible region");
        assert_eq!(offset % PAGE, 0, "a revalidation page is page-aligned");
        assert!(
            offset <= visible.start && offset + limit >= visible.end,
            "the requested page {offset}..{} must cover {visible:?}",
            offset + limit
        );

        table.note_page_request(offset);
        table.apply_page(
            offset,
            (offset..offset + limit)
                .map(|i| row(&format!("fresh{i}.rs")))
                .collect(),
        );

        assert_eq!(table.loaded_range(), offset..offset + limit);
        assert!(
            visible.clone().all(|ix| table
                .row(ix)
                .is_some_and(|row| row.name.starts_with("fresh"))),
            "every visible row must come from the refreshed generation"
        );
        assert_eq!(
            table.page_request(visible, PAGE),
            None,
            "the replaced window already holds the visible rows"
        );
    }

    /// A page that was in flight before the refresh landed carries the invalidated generation.
    /// Installing it would put stale rows or their old order back into the window before the
    /// revalidation request returns.
    #[test]
    fn a_page_from_before_the_refresh_cannot_consume_the_revalidation() {
        let mut table = table(0, 1_000_000);
        table.apply_page(
            70_000,
            (70_000..70_200)
                .map(|i| row(&format!("old{i}.rs")))
                .collect(),
        );
        table.set_scroll_top(70_000.0 * ROW_HEIGHT, ROW_HEIGHT);
        let visible = table.visible_range(ROW_HEIGHT);

        table.refresh_rows(
            (0..200).map(|i| row(&format!("new{i}.rs"))).collect(),
            1_000_100,
        );

        // A pre-refresh page for another offset lands before the revalidation page is fetched.
        table.apply_page(0, (0..200).map(|i| row(&format!("stale{i}.rs"))).collect());
        assert_eq!(
            table.loaded_range(),
            70_000..70_200,
            "the stale in-flight page must be ignored"
        );

        let (offset, limit) = table
            .page_request(visible.clone(), PAGE)
            .expect("revalidation request must still be issued");
        table.note_page_request(offset);
        table.apply_page(
            offset,
            (offset..offset + limit)
                .map(|i| row(&format!("fresh{i}.rs")))
                .collect(),
        );

        assert_eq!(table.loaded_range(), offset..offset + limit);
        assert!(visible
            .clone()
            .all(|ix| table.row(ix).is_some_and(|r| r.name.starts_with("fresh"))));
    }

    /// A top-anchored refresh only invalidates the rows below its first page. A viewport that stays
    /// within that page needs no fetch, and the refresh drops the selection it can no longer name —
    /// including rows that still exist, since the new order can have moved them.
    #[test]
    fn a_top_window_inside_the_fresh_page_does_not_refetch() {
        let mut table = table(400, 1_000);
        table.select_row(7, false);
        table.refresh_rows(
            (0..200).map(|i| row(&format!("new{i}.rs"))).collect(),
            1_010,
        );

        assert!(table.selected_indices.is_empty());
        assert_eq!(table.focused_idx, None);
        assert_eq!(table.selection_anchor, None);
        assert_eq!(table.page_request(0..40, PAGE), None);
        assert_eq!(
            table.page_request(0..200, PAGE),
            None,
            "a viewport inside the fresh page is already current"
        );
    }

    /// An empty revalidation page means the service cannot fill the visible region. Keeping the old
    /// generation there would show stale rows indefinitely, so the window is dropped and the band
    /// can show its loading state; the offset is retired rather than requested every frame.
    #[test]
    fn an_empty_revalidation_page_drops_the_stale_window() {
        let mut table = table(0, 1_000_000);
        table.apply_page(
            70_000,
            (70_000..70_200)
                .map(|i| row(&format!("old{i}.rs")))
                .collect(),
        );
        table.set_scroll_top(70_000.0 * ROW_HEIGHT, ROW_HEIGHT);
        let visible = table.visible_range(ROW_HEIGHT);
        table.refresh_rows(
            (0..200).map(|i| row(&format!("new{i}.rs"))).collect(),
            1_000_100,
        );

        let (offset, _) = table
            .page_request(visible.clone(), PAGE)
            .expect("the visible region needs a page");
        table.note_page_request(offset);
        table.apply_page(offset, Vec::new());

        assert_eq!(table.loaded_range(), 0..0);
        assert!(
            table.row(70_000).is_none(),
            "the stale rows must not outlive their replacement"
        );
        assert_eq!(
            table.page_request(visible, PAGE),
            None,
            "the empty offset is retired instead of requested every frame"
        );
    }

    #[test]
    fn set_rows_empty_clears_ranges_and_total() {
        let mut table = table(10, 100);
        table.set_rows(Vec::new(), 0);
        assert_eq!(table.total_rows(), 0);
        assert_eq!(table.loaded_range(), 0..0);
        assert_eq!(table.scroll_top, 0.0);
        assert!(table.selected_indices.is_empty());
    }

    #[test]
    fn sort_toggles_direction_only_on_the_active_column() {
        let mut table = TableState::default();
        table.toggle_sort(SortColumn::Size);
        assert_eq!(table.sort_column, SortColumn::Size);
        assert!(table.sort_asc);
        table.toggle_sort(SortColumn::Size);
        assert!(!table.sort_asc);
        table.toggle_sort(SortColumn::Name);
        assert!(table.sort_asc);
    }

    /// Every expected string here was produced by .NET's `DateTimeOffset.FromUnixTimeSeconds` over the
    /// same FILETIME, not by the function under test — the previous formula divided the day count by
    /// 365 and the remainder by 30, and would have been perfectly happy asserting its own answer.
    /// The leap day and the two New Year rows are the cases that arithmetic got wrong.
    #[test]
    fn timestamps_render_the_real_calendar_date() {
        for (filetime, expected) in [
            (116_444_736_000_000_000, "1970.01.01 00:00"),
            (130_103_999_900_000_000, "2013.04.14 07:59"),
            (132_000_000_000_000_000, "2019.04.17 18:40"),
            (133_000_000_000_000_000, "2022.06.18 04:26"),
            (133_484_544_000_000_000, "2023.12.31 00:00"),
            (133_485_408_000_000_000, "2024.01.01 00:00"),
            (133_536_384_000_000_000, "2024.02.29 00:00"),
            // A row read out of the live index while this arithmetic was being fixed.
            (133_564_628_421_147_572, "2024.04.01 16:34"),
        ] {
            assert_eq!(format_timestamp_utc(filetime), expected, "{filetime}");
        }
    }

    /// A row with no recorded time stays blank rather than becoming the epoch.
    #[test]
    fn an_absent_timestamp_stays_blank() {
        assert_eq!(format_timestamp(0), "");
    }

    #[test]
    fn test_apply_page_bidirectional_and_overlapping() {
        let mut table = TableState::default();
        // Initial page: 100..200
        let page1 = (100..200).map(|i| row(&format!("f{i}"))).collect();
        table.apply_page(100, page1);
        assert_eq!(table.loaded_range(), 100..200);

        // Overlapping append: 180..250 (overlap 180..200, adds 200..250)
        let page2 = (180..250).map(|i| row(&format!("f{i}"))).collect();
        table.apply_page(180, page2);
        assert_eq!(table.loaded_range(), 100..250);
        assert_eq!(table.row(100).unwrap().name, "f100");
        assert_eq!(table.row(249).unwrap().name, "f249");

        // Overlapping prepend: 50..120 (overlap 100..120, prepends 50..100)
        let page3 = (50..120).map(|i| row(&format!("f{i}"))).collect();
        table.apply_page(50, page3);
        assert_eq!(table.loaded_range(), 50..250);
        assert_eq!(table.row(50).unwrap().name, "f50");
        assert_eq!(table.row(249).unwrap().name, "f249");

        // Contained subset page: 80..150 (completely contained in 50..250, no-op)
        let page4 = (80..150).map(|i| row(&format!("f{i}"))).collect();
        table.apply_page(80, page4);
        assert_eq!(table.loaded_range(), 50..250);

        // Disjoint far jump: 1000..1050
        let page5 = (1000..1050).map(|i| row(&format!("f{i}"))).collect();
        table.apply_page(1000, page5);
        assert_eq!(table.loaded_range(), 1000..1050);
    }
}
