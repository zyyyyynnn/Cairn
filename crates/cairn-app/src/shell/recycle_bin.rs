// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Recycle Bin view state: shell listing to table rows, local paging, sorting and selection.
//!
//! The bin is a location, not a search result set. Its rows come from the Windows shell and
//! live in [`RecycleBinView`]; the result table renders a window of them exactly like it
//! renders service pages, so a large bin stays virtualized without asking the service for rows
//! the index does not hold.

use crate::app::{CairnApp, layout};
use crate::shell::os::{BinErrorKind, RecycleBinItem};
use crate::table::model::SortColumn;
use cairn_protocol::message::FileRowData;
use gpui_kit::gpui::{Context, Pixels, Point, Window, px};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

enum BinStage {
    Loading,
    Ready {
        all: Vec<Arc<RecycleBinItem>>,
        rows: Vec<Arc<RecycleBinItem>>,
        icon_extensions: HashMap<u64, String>,
        mutating: bool,
    },
    Failed(String),
}

enum RecycleBinState {
    Closed,
    Open {
        stage: BinStage,
        selected_keys: HashSet<String>,
    },
}

/// The Recycle Bin view's own state.
///
/// The bin is a location, not a search scope, so every path that leaves it releases the whole
/// view at once.
pub(crate) struct RecycleBinView {
    state: RecycleBinState,
    /// Invalidates a background bin read when the location is left or reloaded again.
    generation: u64,
}

impl RecycleBinView {
    /// A bin view that is not the window's current location.
    pub(crate) fn closed() -> Self {
        Self {
            state: RecycleBinState::Closed,
            generation: 0,
        }
    }

    /// Whether the bin is the window's current location.
    pub(crate) fn is_open(&self) -> bool {
        matches!(self.state, RecycleBinState::Open { .. })
    }

    pub(crate) fn surface_location(&self) -> crate::table::surface::SurfaceLocation {
        use crate::table::surface::{LocalListingStage, SurfaceLocation};
        match &self.state {
            RecycleBinState::Closed => SurfaceLocation::Index,
            RecycleBinState::Open {
                stage: BinStage::Loading | BinStage::Ready { mutating: true, .. },
                ..
            } => SurfaceLocation::LocalListing(LocalListingStage::Loading),
            RecycleBinState::Open {
                stage: BinStage::Ready {
                    mutating: false, ..
                },
                ..
            } => SurfaceLocation::LocalListing(LocalListingStage::Ready),
            RecycleBinState::Open {
                stage: BinStage::Failed(_),
                ..
            } => SurfaceLocation::LocalListing(LocalListingStage::Error),
        }
    }

    /// The rows the table renders when ready, or `None` when closed, loading, or failed.
    pub(crate) fn rows(&self) -> Option<&[Arc<RecycleBinItem>]> {
        match &self.state {
            RecycleBinState::Open {
                stage: BinStage::Ready { rows, .. },
                ..
            } => Some(rows.as_slice()),
            _ => None,
        }
    }

    /// The unfiltered shell listing behind the rendered rows.
    pub(crate) fn all(&self) -> &[Arc<RecycleBinItem>] {
        match &self.state {
            RecycleBinState::Open {
                stage: BinStage::Ready { all, .. },
                ..
            } => all.as_slice(),
            _ => &[],
        }
    }

    /// The payload extension of a row, keyed by its display identity.
    pub(crate) fn icon_extension(&self, key: u64) -> Option<&str> {
        match &self.state {
            RecycleBinState::Open {
                stage: BinStage::Ready {
                    icon_extensions, ..
                },
                ..
            } => icon_extensions.get(&key).map(String::as_str),
            _ => None,
        }
    }

    /// The identity keys of the rows the person selected.
    pub(crate) fn selected_keys(&self) -> &HashSet<String> {
        static EMPTY: std::sync::LazyLock<HashSet<String>> = std::sync::LazyLock::new(HashSet::new);
        match &self.state {
            RecycleBinState::Open { selected_keys, .. } => selected_keys,
            RecycleBinState::Closed => &EMPTY,
        }
    }

    /// The reason the listing could not be read.
    pub(crate) fn error(&self) -> Option<&str> {
        match &self.state {
            RecycleBinState::Open {
                stage: BinStage::Failed(reason),
                ..
            } => Some(reason.as_str()),
            _ => None,
        }
    }

    /// True while a listing read or bin mutation is in flight.
    pub(crate) fn is_loading(&self) -> bool {
        matches!(
            self.state,
            RecycleBinState::Open {
                stage: BinStage::Loading | BinStage::Ready { mutating: true, .. },
                ..
            }
        )
    }

    /// Marks whether a write operation on an already-loaded bin is in flight.
    pub(crate) fn set_mutating(&mut self, next: bool) {
        if let RecycleBinState::Open {
            stage: BinStage::Ready { mutating, .. },
            ..
        } = &mut self.state
        {
            *mutating = next;
        }
    }

    /// The token a background read must still match to be accepted.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// Releases the view. Returns whether the bin was open.
    pub(crate) fn leave(&mut self) -> bool {
        let was_open = self.is_open();
        self.state = RecycleBinState::Closed;
        was_open
    }

    /// Starts a background shell read. Returns the token the read must match to be accepted.
    pub(crate) fn begin_load(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.state = RecycleBinState::Open {
            stage: BinStage::Loading,
            selected_keys: HashSet::new(),
        };
        self.generation
    }

    /// Installs the listing a shell read returned, dropping the selection it no longer covers.
    pub(crate) fn install_listing(&mut self, items: Vec<Arc<RecycleBinItem>>) {
        let icon_extensions = bin_icon_extensions(&items);
        let live: HashSet<String> = items.iter().map(|item| item.identity_key()).collect();
        let selected_keys = match &mut self.state {
            RecycleBinState::Open { selected_keys, .. } => {
                selected_keys.retain(|key| live.contains(key));
                std::mem::take(selected_keys)
            }
            RecycleBinState::Closed => HashSet::new(),
        };
        self.state = RecycleBinState::Open {
            stage: BinStage::Ready {
                all: items,
                rows: Vec::new(),
                icon_extensions,
                mutating: false,
            },
            selected_keys,
        };
    }

    /// Records that the listing could not be read.
    pub(crate) fn fail(&mut self, reason: String) {
        self.state = RecycleBinState::Open {
            stage: BinStage::Failed(reason),
            selected_keys: HashSet::new(),
        };
    }

    /// Shows the filtered rows of the current listing.
    pub(crate) fn set_rows(&mut self, next_rows: Vec<Arc<RecycleBinItem>>) {
        if let RecycleBinState::Open {
            stage: BinStage::Ready { rows, .. },
            ..
        } = &mut self.state
        {
            *rows = next_rows;
        }
    }

    /// Merges the visible table selection into the identity-keyed selection before the visible
    /// set changes.
    pub(crate) fn merge_visible_selection(
        &mut self,
        selected_indices: &HashSet<usize>,
        selection_cleared: bool,
    ) {
        if let RecycleBinState::Open {
            stage: BinStage::Ready { rows, .. },
            selected_keys,
        } = &mut self.state
        {
            merge_bin_selection_keys(rows, selected_indices, selection_cleared, selected_keys);
        }
    }

    /// Reorders the full listing to the table's current sort.
    pub(crate) fn sort(&mut self, column: SortColumn, ascending: bool) {
        if let RecycleBinState::Open {
            stage: BinStage::Ready { all, .. },
            ..
        } = &mut self.state
        {
            sort_items(all, column, ascending);
        }
    }
}

/// Stable key for the icon-extension side table. It mirrors the fields a bin row exposes to the
/// table, so the name-hidden `.lnk` suffix can be recovered without changing the IPC row format.
pub(crate) fn bin_icon_key(
    name: &str,
    original_dir: &str,
    deleted_ts: i64,
    size: u64,
    is_dir: bool,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hasher);
    original_dir.hash(&mut hasher);
    deleted_ts.hash(&mut hasher);
    size.hash(&mut hasher);
    is_dir.hash(&mut hasher);
    hasher.finish()
}

pub(crate) fn bin_icon_extensions(items: &[Arc<RecycleBinItem>]) -> HashMap<u64, String> {
    items
        .iter()
        .map(|item| {
            (
                bin_icon_key(
                    &item.name,
                    &item.original_dir,
                    item.deleted_ts,
                    item.size,
                    item.is_dir,
                ),
                item.extension_lower.clone(),
            )
        })
        .collect()
}

/// One bin item in the shape the result table renders. The table's date lane is the deletion
/// time here, and the path lane is the original location, both stated by the bin's headers.
pub(crate) fn row_for(item: &RecycleBinItem) -> FileRowData {
    FileRowData {
        frn: 0,
        name: item.name.clone(),
        path: item.original_dir.clone(),
        size: item.size,
        modified_ts: item.deleted_ts,
        created_ts: 0,
        is_dir: item.is_dir,
        flags: 0,
        name_highlights: Vec::new(),
        path_highlights: Vec::new(),
        snippets: Vec::new(),
        properties: None,
    }
}

/// Byte ranges of every case-sensitive or case-insensitive occurrence of the bin filter.
/// Case folding that changes byte length is skipped rather than mapped to a wrong range;
/// the row still matches, it simply carries no highlight.
fn filter_highlight_ranges(text: &str, needle: &str, case_sensitive: bool) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    if case_sensitive {
        return text
            .match_indices(needle)
            .map(|(start, matched)| (start, start + matched.len()))
            .collect();
    }
    let lower = text.to_lowercase();
    if lower.len() != text.len() {
        return Vec::new();
    }
    let needle = needle.to_lowercase();
    lower
        .match_indices(&needle)
        .map(|(start, matched)| (start, start + matched.len()))
        .collect()
}

/// A bin row with the current local filter's matches marked for the table's text runs.
pub(crate) fn row_for_filtered(
    item: &RecycleBinItem,
    needle: &str,
    case_sensitive: bool,
) -> FileRowData {
    let mut row = row_for(item);
    row.name_highlights = filter_highlight_ranges(&item.name, needle, case_sensitive);
    row.path_highlights = filter_highlight_ranges(&item.original_dir, needle, case_sensitive);
    row
}

/// Orders bin items by a table column. The comparison is independent of the shell's listing
/// order, so the header the person clicked always describes the order they see. Name and path
/// sort keys are lowercased once per item instead of twice per comparison.
pub(crate) fn sort_items(
    items: &mut [std::sync::Arc<RecycleBinItem>],
    column: SortColumn,
    ascending: bool,
) {
    match column {
        SortColumn::Name => {
            items.sort_by(|a, b| {
                cairn_core::sort::natural_cmp(&a.name, &b.name)
                    .then_with(|| cairn_core::sort::natural_cmp(&a.original_dir, &b.original_dir))
                    .then_with(|| a.deleted_ts.cmp(&b.deleted_ts))
                    .then_with(|| a.fs_path.cmp(&b.fs_path))
            });
        }
        SortColumn::Extension => {
            items.sort_by(|a, b| {
                a.extension_lower
                    .as_str()
                    .cmp(b.extension_lower.as_str())
                    .then_with(|| cairn_core::sort::natural_cmp(&a.name, &b.name))
                    .then_with(|| cairn_core::sort::natural_cmp(&a.original_dir, &b.original_dir))
                    .then_with(|| a.deleted_ts.cmp(&b.deleted_ts))
                    .then_with(|| a.fs_path.cmp(&b.fs_path))
            });
        }
        SortColumn::Path => {
            items.sort_by(|a, b| {
                cairn_core::sort::natural_cmp(&a.original_dir, &b.original_dir)
                    .then_with(|| cairn_core::sort::natural_cmp(&a.name, &b.name))
                    .then_with(|| a.deleted_ts.cmp(&b.deleted_ts))
                    .then_with(|| a.fs_path.cmp(&b.fs_path))
            });
        }
        SortColumn::Size => {
            items.sort_by(|a, b| {
                a.size
                    .cmp(&b.size)
                    .then_with(|| cairn_core::sort::natural_cmp(&a.name, &b.name))
                    .then_with(|| cairn_core::sort::natural_cmp(&a.original_dir, &b.original_dir))
                    .then_with(|| a.deleted_ts.cmp(&b.deleted_ts))
                    .then_with(|| a.fs_path.cmp(&b.fs_path))
            });
        }
        SortColumn::DateModified
        | SortColumn::DateCreated
        | SortColumn::Length
        | SortColumn::Author
        | SortColumn::Tags
        | SortColumn::Dimensions
        | SortColumn::Framerate => {
            items.sort_by(|a, b| {
                a.deleted_ts
                    .cmp(&b.deleted_ts)
                    .then_with(|| cairn_core::sort::natural_cmp(&a.name, &b.name))
                    .then_with(|| cairn_core::sort::natural_cmp(&a.original_dir, &b.original_dir))
                    .then_with(|| a.fs_path.cmp(&b.fs_path))
            });
        }
    }
    if !ascending {
        items.reverse();
    }
}

/// The rows the bin's search box keeps. Matching the name and the original location makes the
/// search useful for both "what did I delete" and "where did it come from"; an empty needle keeps
/// the whole listing.
pub(crate) fn filter_items(
    items: &[std::sync::Arc<RecycleBinItem>],
    needle: &str,
    case_sensitive: bool,
) -> Vec<std::sync::Arc<RecycleBinItem>> {
    let needle = needle.trim();
    if needle.is_empty() {
        return items.to_vec();
    }
    if case_sensitive {
        return items
            .iter()
            .filter(|item| item.name.contains(needle) || item.original_dir.contains(needle))
            .cloned()
            .collect();
    }
    let needle = needle.to_lowercase();
    items
        .iter()
        .filter(|item| {
            item.name_lower.contains(&needle) || item.original_dir_lower.contains(&needle)
        })
        .cloned()
        .collect()
}

/// Merges the visible row indices into the identity-keyed selection. `selection_cleared` marks
/// the snapshot a press on shell chrome left behind — no indices, no focus and no anchor. That
/// is a transient clear, not a decision to deselect visible items, so the keys must survive it.
/// A row interaction always leaves focus or the anchor behind, so its empty index list still
/// removes the keys of the rows the person deselected.
fn merge_bin_selection_keys(
    items: &[std::sync::Arc<RecycleBinItem>],
    selected_indices: &std::collections::HashSet<usize>,
    selection_cleared: bool,
    keys: &mut std::collections::HashSet<String>,
) {
    if selection_cleared {
        return;
    }
    for (index, item) in items.iter().enumerate() {
        let key = item.identity_key();
        if selected_indices.contains(&index) {
            keys.insert(key);
        } else {
            keys.remove(&key);
        }
    }
}

impl CairnApp {
    /// Rebuilds the visible bin rows from the full listing and the current search text. The
    /// search box filters this location in place instead of launching an index query.
    pub(crate) fn apply_recycle_bin_filter(
        &mut self,
        reset_scroll: bool,
        cx: &mut Context<'_, Self>,
    ) {
        if self.recycle_bin.error().is_some() {
            return;
        }
        let needle = self.query.search_input.text.trim().to_string();
        let filtered = filter_items(self.recycle_bin.all(), &needle, self.query.case_sensitive);
        self.recycle_bin.set_rows(filtered);
        self.install_recycle_bin_rows(reset_scroll, cx);
        self.restore_bin_selection();
    }

    /// Merges the visible table selection into the identity-keyed selection before the visible
    /// set changes. A key that is temporarily hidden by a filter stays in the set; a visible
    /// row the person deselected is removed. A wholly absent table selection is not a decision
    /// to deselect anything: a chrome press clears the indices, focus and anchor together, and
    /// the keys are then the only record of what the person selected.
    pub(crate) fn sync_bin_selection_keys(&mut self) {
        let selection_cleared = self.results.table.selected_indices.is_empty()
            && self.results.table.focused_idx.is_none()
            && self.results.table.selection_anchor.is_none();
        self.recycle_bin
            .merge_visible_selection(&self.results.table.selected_indices, selection_cleared);
    }

    fn restore_bin_selection(&mut self) {
        let Some(items) = self.recycle_bin.rows() else {
            return;
        };
        let selected_keys = self.recycle_bin.selected_keys();
        let indices: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| selected_keys.contains(&item.identity_key()))
            .map(|(index, _)| index)
            .collect();
        self.results.table.selected_indices = indices.iter().copied().collect();
        let focused = indices
            .iter()
            .copied()
            .find(|index| self.results.table.is_loaded(*index));
        self.results.table.focused_idx = focused;
        self.results.table.selection_anchor = focused;
    }

    /// Re-filters after a text edit while the bin is open. This is the search band's path in the
    /// bin, so typing never replaces the location with an index query.
    pub(crate) fn filter_recycle_bin(&mut self, cx: &mut Context<'_, Self>) {
        if !self.recycle_bin.is_open() || self.recycle_bin.error().is_some() {
            return;
        }
        self.sync_bin_selection_keys();
        self.apply_recycle_bin_filter(true, cx);
        cx.notify();
    }

    /// Clears the bin's in-place search and keeps the location open.
    pub(crate) fn clear_bin_filter(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if !self.recycle_bin.is_open() {
            return;
        }
        self.query.search_input.clear();
        if self.recycle_bin.error().is_some() {
            cx.notify();
            return;
        }
        self.sync_bin_selection_keys();
        self.apply_recycle_bin_filter(true, cx);
        self.query.search_focus.focus(window, cx);
        cx.notify();
    }

    /// Rebuilds the table's fetched window from the first bin page. `reset_scroll` is false when
    /// an operation removed or restored a row and the person should stay where they were reading.
    pub(crate) fn install_recycle_bin_rows(
        &mut self,
        reset_scroll: bool,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(items) = self.recycle_bin.rows() else {
            return;
        };
        let total = items.len();
        let first_len = total.min(layout::RESULT_PAGE_SIZE);
        let needle = self.query.search_input.text.trim();
        let case_sensitive = self.query.case_sensitive;
        let rows: Vec<FileRowData> = items[..first_len]
            .iter()
            .map(|item| row_for_filtered(item, needle, case_sensitive))
            .collect();
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
    }

    /// Installs the page the viewport needs from the in-memory bin, with no IPC request.
    pub(crate) fn fill_recycle_bin_page(&mut self, row_height: Pixels, cx: &mut Context<'_, Self>) {
        if !self.recycle_bin.is_open() {
            return;
        }
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
        let Some(items) = self.recycle_bin.rows() else {
            return;
        };
        if offset >= items.len() {
            return;
        }
        let end = offset.saturating_add(limit).min(items.len());
        let needle = self.query.search_input.text.trim();
        let case_sensitive = self.query.case_sensitive;
        let rows: Vec<FileRowData> = items[offset..end]
            .iter()
            .map(|item| row_for_filtered(item, needle, case_sensitive))
            .collect();
        self.results.table.apply_page(offset, rows);
        cx.notify();
    }

    /// `true` when the bin is showing rows the fetched table window does not hold.
    pub(crate) fn needs_recycle_bin_page(&self, visible: std::ops::Range<usize>) -> bool {
        self.recycle_bin.is_open()
            && self
                .results
                .table
                .page_request(visible, layout::RESULT_PAGE_SIZE)
                .is_some()
    }

    /// Reorders the full in-memory listing to the table's current sort and re-applies the
    /// in-place search from the top.
    pub(crate) fn sort_recycle_bin(&mut self, cx: &mut Context<'_, Self>) {
        if self.recycle_bin.error().is_some() || self.recycle_bin.is_loading() {
            return;
        }
        let (column, ascending) = self.results.table.primary_sort();
        self.sync_bin_selection_keys();
        self.recycle_bin.sort(column, ascending);
        self.apply_recycle_bin_filter(true, cx);
        cx.notify();
    }

    /// Starts a background shell read for the current bin location. The GPUI thread remains responsive
    /// while a dedicated COM apartment walks the shell; the generation token discards
    /// a result that arrives after the person has left or reloaded the location.
    pub(crate) fn begin_recycle_bin_load(&mut self, cx: &mut Context<'_, Self>) {
        let generation = self.recycle_bin.begin_load();
        // The bin owns the table; an index answer still in flight must not
        // land on top of its rows.
        self.take_table_for_local_rows();
        self.begin_table_replacement();
        cx.notify();

        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let _ = tx.send(crate::shell::os::list_recycle_bin());
        });
        cx.spawn(async move |this, cx| {
            let Ok(result) = rx.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.finish_recycle_bin_load(generation, result, true, None, cx);
            });
        })
        .detach();
    }

    /// Installs the completed background read. Shared by the initial load and by write operations
    /// that return a fresh listing, so both go through the same generation and error gates.
    pub(crate) fn finish_recycle_bin_load(
        &mut self,
        generation: u64,
        result: Result<Vec<RecycleBinItem>, String>,
        reset_scroll: bool,
        anchor: Option<usize>,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.recycle_bin.is_open() || self.recycle_bin.generation() != generation {
            return;
        }
        match result {
            Ok(items) => {
                let mut items: Vec<std::sync::Arc<RecycleBinItem>> =
                    items.into_iter().map(std::sync::Arc::new).collect();
                let (sort_col, sort_asc) = self.results.table.primary_sort();
                sort_items(&mut items, sort_col, sort_asc);
                self.recycle_bin.install_listing(items);
                if self.recycle_bin.all().is_empty() && !self.query.search_input.text.is_empty() {
                    self.query.search_input.clear();
                }
                self.apply_recycle_bin_filter(reset_scroll, cx);
                if let Some(anchor) = anchor
                    && self.recycle_bin.selected_keys().is_empty()
                    && self.results.table.selected_indices.is_empty()
                    && let Some(rows) = self.recycle_bin.rows()
                    && !rows.is_empty()
                {
                    let index = anchor.min(rows.len() - 1);
                    if self.results.table.is_loaded(index) {
                        self.results.table.select_row(index, false);
                    }
                }
            }
            Err(reason) => {
                tracing::warn!("recycle bin load failed: {reason}");
                self.recycle_bin.fail(reason);
                self.install_recycle_bin_rows(true, cx);
            }
        }
        cx.notify();
    }

    /// The selected bin items, in table order. `None` when the bin view is not open.
    /// A focused row that was explicitly deselected is not a selection here:
    /// permanent delete must never act on a row the person just removed from the
    /// selection.
    pub(crate) fn selected_bin_items(&self) -> Option<Vec<std::sync::Arc<RecycleBinItem>>> {
        let items = self.recycle_bin.rows()?;
        Some(
            self.explicit_bin_selection_indices()
                .iter()
                .filter_map(|index| items.get(*index).cloned())
                .collect(),
        )
    }

    /// Selection indices without the focused-row fallback. The search table uses
    /// that fallback because a focused row is the row Enter would open; the bin's
    /// irreversible commands must require an actual selection. The whole listing
    /// is in memory, so a selection outside the fetched page is still actionable.
    pub(crate) fn explicit_bin_selection_indices(&self) -> Vec<usize> {
        let mut indices: Vec<usize> = self
            .results
            .table
            .selected_indices
            .iter()
            .copied()
            .collect();
        indices.sort_unstable();
        indices
    }

    pub(crate) fn target_bin_item(&self) -> Option<std::sync::Arc<RecycleBinItem>> {
        let items = self.recycle_bin.rows()?;
        items.get(self.target_row_ix()?).cloned()
    }

    /// A failed shell operation is reported where the operation was started. Cancellation is not
    /// a failure: the shell returns `COPYENGINE_E_USER_CANCELLED` when the native warning was
    /// dismissed, and a toast for that decision would be noise. Real failures name the items the
    /// action covered and replace the raw HRESULT with a readable reason.
    /// A structured native outcome is reported once: cancellation stays silent,
    /// and a failure names the items the shell actually could not process
    /// (falling back to the selected set only when the shell reported none).
    pub(crate) fn notify_bin_operation_error(
        &mut self,
        title_key: &str,
        fallback_names: &[String],
        failure: &crate::shell::os::BinOperationError,
        cx: &mut Context<'_, Self>,
    ) {
        if failure.is_cancelled() {
            tracing::info!("recycle bin operation cancelled: {}", failure.reason());
            return;
        }
        tracing::warn!("recycle bin operation failed: {}", failure.reason());
        let item_names = if failure.items().is_empty() {
            fallback_names
        } else {
            failure.items()
        };
        let title = self.i18n.get_text(title_key);
        let reason = self.shell_reason_text(failure);
        let message = if item_names.is_empty() {
            reason
        } else {
            let mut args = fluent_bundle::FluentArgs::new();
            args.set("reason", reason);
            args.set("items", item_names_summary(&self.i18n, item_names));
            self.i18n.get_text_with_args("bin_error_items", &args)
        };
        self.show_error_toast(title, message, cx);
    }

    fn shell_reason_text(&self, failure: &crate::shell::os::BinOperationError) -> String {
        let kind = failure.kind().unwrap_or(BinErrorKind::Generic);
        self.i18n.get_text(shell_reason_key(kind))
    }
}

/// One localized sentence per failure class. The raw diagnostic stays in the log; the toast never
/// shows an HRESULT to a person.
fn shell_reason_key(kind: BinErrorKind) -> &'static str {
    match kind {
        BinErrorKind::AccessDenied => "bin_error_access_denied",
        BinErrorKind::ReadOnly => "bin_error_read_only",
        BinErrorKind::Conflict => "bin_error_conflict",
        BinErrorKind::PathTooDeep => "bin_error_path_too_deep",
        BinErrorKind::PathTooLong => "bin_error_path_too_long",
        BinErrorKind::MissingItem => "bin_error_missing_item",
        BinErrorKind::OriginalLocation => "bin_error_original_location",
        BinErrorKind::NoItems => "bin_error_no_items",
        BinErrorKind::Generic => "bin_error_generic",
    }
}

/// At most three names keep a toast one glance long; the total count still tells the scale.
/// Each shown name is clipped by characters, not bytes, so a long CJK name cannot stretch the
/// card across the helper's fixed measure.
fn item_names_summary(i18n: &crate::shell::i18n::I18nManager, names: &[String]) -> String {
    const SHOWN: usize = 3;
    const NAME_LIMIT: usize = 48;
    let separator = i18n.get_text("bin_list_separator");
    let mut summary = names
        .iter()
        .take(SHOWN)
        .map(|name| {
            if name.chars().count() > NAME_LIMIT {
                let mut short: String = name.chars().take(NAME_LIMIT - 1).collect();
                short.push('…');
                short
            } else {
                name.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(&separator);
    if names.len() > SHOWN {
        let mut args = fluent_bundle::FluentArgs::new();
        args.set("count", (names.len() - SHOWN) as i64);
        summary.push_str(&i18n.get_text_with_args("bin_error_more_items", &args));
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::{
        RecycleBinView, bin_icon_extensions, bin_icon_key, filter_items, item_names_summary,
        merge_bin_selection_keys, row_for, row_for_filtered, shell_reason_key, sort_items,
    };
    use crate::shell::i18n::I18nManager;
    use crate::shell::os::{BinErrorKind, RecycleBinItem};
    use crate::table::model::SortColumn;

    fn item(name: &str, dir: &str, ts: i64, size: u64) -> RecycleBinItem {
        RecycleBinItem {
            name: name.to_string(),
            name_lower: name.to_lowercase(),
            extension_lower: crate::shell::os::extract_extension_lower(name),
            original_dir: dir.to_string(),
            original_dir_lower: dir.to_lowercase(),
            fs_path: format!(r"C:\$Recycle.Bin\SID\$R{name}"),
            deleted_ts: ts,
            size,
            is_dir: false,
        }
    }

    #[test]
    fn sorting_uses_the_column_the_header_shows() {
        let mut items = vec![
            std::sync::Arc::new(item("beta.txt", r"C:\work", 30, 10)),
            std::sync::Arc::new(item("Alpha.txt", r"C:\archive", 10, 30)),
            std::sync::Arc::new(item("gamma.txt", r"C:\work", 20, 20)),
        ];
        sort_items(&mut items, SortColumn::Name, true);
        assert_eq!(
            items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Alpha.txt", "beta.txt", "gamma.txt"]
        );
        sort_items(&mut items, SortColumn::Size, false);
        assert_eq!(
            items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Alpha.txt", "gamma.txt", "beta.txt"]
        );
        sort_items(&mut items, SortColumn::DateModified, true);
        assert_eq!(
            items.iter().map(|item| item.deleted_ts).collect::<Vec<_>>(),
            vec![10, 20, 30]
        );
        sort_items(&mut items, SortColumn::Path, true);
        assert_eq!(items[0].original_dir, r"C:\archive");

        let mut ext_items = vec![
            std::sync::Arc::new(item("beta.txt", r"C:\work", 30, 10)),
            std::sync::Arc::new(item("alpha.png", r"C:\archive", 10, 30)),
            std::sync::Arc::new(item("gamma.doc", r"C:\work", 20, 20)),
            std::sync::Arc::new(item("delta.TXT", r"C:\work", 40, 40)),
            std::sync::Arc::new(item("noext", r"C:\work", 50, 50)),
        ];
        sort_items(&mut ext_items, SortColumn::Extension, true);
        assert_eq!(
            ext_items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["noext", "gamma.doc", "alpha.png", "beta.txt", "delta.TXT"]
        );
        sort_items(&mut ext_items, SortColumn::Extension, false);
        assert_eq!(
            ext_items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["delta.TXT", "beta.txt", "alpha.png", "gamma.doc", "noext"]
        );

        let mut nat_items = vec![
            std::sync::Arc::new(item("file10.txt", r"C:\work\dir10", 10, 10)),
            std::sync::Arc::new(item("file2.txt", r"C:\work\dir2", 20, 20)),
        ];
        sort_items(&mut nat_items, SortColumn::Name, true);
        assert_eq!(nat_items[0].name, "file2.txt");
        assert_eq!(nat_items[1].name, "file10.txt");

        sort_items(&mut nat_items, SortColumn::Path, true);
        assert_eq!(nat_items[0].original_dir, r"C:\work\dir2");
        assert_eq!(nat_items[1].original_dir, r"C:\work\dir10");
    }

    #[test]
    fn extension_sorting_handles_dotfiles_empty_extensions_and_casing_stably() {
        let mut items = vec![
            std::sync::Arc::new(item("b_noext", r"C:\work", 10, 10)),
            std::sync::Arc::new(item(".gitignore", r"C:\work", 20, 20)),
            std::sync::Arc::new(item("a_noext", r"C:\work", 30, 30)),
            std::sync::Arc::new(item("a.txt", r"C:\work", 40, 40)),
            std::sync::Arc::new(item(".env", r"C:\work", 50, 50)),
            std::sync::Arc::new(item("A.TXT", r"C:\work", 60, 60)),
        ];
        sort_items(&mut items, SortColumn::Extension, true);
        assert_eq!(
            items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["a_noext", "b_noext", ".env", ".gitignore", "a.txt", "A.TXT"]
        );
        sort_items(&mut items, SortColumn::Extension, false);
        assert_eq!(
            items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["A.TXT", "a.txt", ".gitignore", ".env", "b_noext", "a_noext"]
        );
    }

    #[test]
    fn identity_selection_survives_a_chrome_press_but_not_a_row_deselect() {
        let items = vec![
            std::sync::Arc::new(item("alpha.txt", r"C:\work", 10, 1)),
            std::sync::Arc::new(item("beta.txt", r"C:\archive", 20, 2)),
        ];
        let selected = |indices: &[usize]| {
            indices
                .iter()
                .copied()
                .collect::<std::collections::HashSet<usize>>()
        };
        let mut keys = std::collections::HashSet::from([items[0].identity_key()]);

        // A press on shell chrome clears indices, focus and anchor together. That snapshot is
        // not a deselect decision: the keys are the only remaining record of the selection.
        merge_bin_selection_keys(&items, &selected(&[]), true, &mut keys);
        assert_eq!(
            keys,
            std::collections::HashSet::from([items[0].identity_key()])
        );

        // A row interaction leaves focus behind, so an empty index list is authoritative and
        // removes the key of the row the person just deselected.
        merge_bin_selection_keys(&items, &selected(&[]), false, &mut keys);
        assert!(keys.is_empty());

        // A filter only merges the rows it shows; a key hidden by the filter stays selected.
        keys.insert(items[0].identity_key());
        merge_bin_selection_keys(&items[1..], &selected(&[0]), false, &mut keys);
        assert_eq!(
            keys,
            std::collections::HashSet::from([items[0].identity_key(), items[1].identity_key()])
        );
    }

    #[test]
    fn the_bin_filter_matches_names_and_original_locations() {
        let items = vec![
            std::sync::Arc::new(item("report.txt", r"C:\work", 30, 10)),
            std::sync::Arc::new(item("photo.png", r"C:\pictures", 20, 20)),
            std::sync::Arc::new(item("Report-final.txt", r"D:\archive", 10, 30)),
        ];
        let by_name = filter_items(&items, "report", false);
        assert_eq!(by_name.len(), 2);
        assert!(
            by_name
                .iter()
                .all(|item| item.name.to_lowercase().contains("report"))
        );
        let by_path = filter_items(&items, "pictures", false);
        assert_eq!(by_path.len(), 1);
        assert_eq!(by_path[0].name, "photo.png");
        assert_eq!(filter_items(&items, "", false).len(), 3);
        assert_eq!(filter_items(&items, "REPORT", true).len(), 0);
    }

    #[test]
    fn the_bin_filter_marks_matching_name_and_location_ranges() {
        let report = item("Report-final.txt", r"C:\work\reports", 30, 10);
        let case_insensitive = row_for_filtered(&report, "report", false);
        assert_eq!(case_insensitive.name_highlights, vec![(0, 6)]);
        assert_eq!(case_insensitive.path_highlights, vec![(8, 14)]);
        assert!(
            row_for_filtered(&report, "report", true)
                .name_highlights
                .is_empty()
        );

        let chinese = item("屏幕录制 2026.mp4", r"C:\视频\屏幕录制", 40, 20);
        let row = row_for_filtered(&chinese, "屏幕", false);
        let (start, end) = row.name_highlights[0];
        assert_eq!(&row.name[start..end], "屏幕");
        let (start, end) = row.path_highlights[0];
        assert_eq!(&row.path[start..end], "屏幕");
    }

    #[test]
    fn every_failure_class_has_a_localized_sentence() {
        let i18n = I18nManager::new("zh-CN");
        for kind in [
            BinErrorKind::AccessDenied,
            BinErrorKind::ReadOnly,
            BinErrorKind::Conflict,
            BinErrorKind::PathTooDeep,
            BinErrorKind::PathTooLong,
            BinErrorKind::MissingItem,
            BinErrorKind::OriginalLocation,
            BinErrorKind::NoItems,
            BinErrorKind::Generic,
        ] {
            let key = shell_reason_key(kind);
            assert!(i18n.has_key(key), "no zh-CN/en-US message for {key}");
        }
    }

    #[test]
    fn a_toast_lists_the_items_and_only_the_hidden_remainder() {
        let i18n = I18nManager::new("zh-CN");
        let names: Vec<String> = ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"]
            .iter()
            .map(ToString::to_string)
            .collect();
        let mut more_args = fluent_bundle::FluentArgs::new();
        more_args.set("count", 2i64);
        let more = i18n.get_text_with_args("bin_error_more_items", &more_args);
        let separator = i18n.get_text("bin_list_separator");
        assert_eq!(
            item_names_summary(&i18n, &names),
            format!("a.txt{separator}b.txt{separator}c.txt{more}")
        );
        assert_eq!(
            item_names_summary(&i18n, &names[..2]),
            format!("a.txt{separator}b.txt")
        );
    }

    #[test]
    fn a_long_item_name_is_clipped_by_characters_not_bytes() {
        let i18n = I18nManager::new("zh-CN");
        let long = "很长的项目名称".repeat(12);
        let summary = item_names_summary(&i18n, &[long]);
        assert!(summary.ends_with('…'));
        assert_eq!(summary.chars().count(), 48);
    }

    #[test]
    fn a_bin_row_carries_the_deletion_time_as_the_table_date() {
        let row = row_for(&item("gone.txt", r"C:\work", 1_700_000_000, 42));
        assert_eq!(row.name, "gone.txt");
        assert_eq!(row.path, r"C:\work");
        assert_eq!(row.modified_ts, 1_700_000_000);
        assert_eq!(row.size, 42);
        assert!(!row.is_dir);
    }

    #[test]
    fn a_shortcut_extension_survives_the_hidden_display_suffix() {
        let mut shortcut = item("Grok Bot", r"C:\Users\Alice\Desktop", 1_700_000_000, 42);
        shortcut.extension_lower = "lnk".into();
        let shortcut = std::sync::Arc::new(shortcut);
        let map = bin_icon_extensions(std::slice::from_ref(&shortcut));
        let row = row_for(&shortcut);
        let key = bin_icon_key(&row.name, &row.path, row.modified_ts, row.size, row.is_dir);
        assert_eq!(map.get(&key).map(String::as_str), Some("lnk"));
    }

    /// A bin that holds a listing, a filtered view, a selection and an icon side table.
    fn open_view() -> RecycleBinView {
        let mut view = RecycleBinView::closed();
        view.begin_load();
        view.install_listing(vec![
            std::sync::Arc::new(item("alpha.txt", r"C:\work", 10, 1)),
            std::sync::Arc::new(item("beta.txt", r"C:\work", 20, 2)),
        ]);
        // The search box filters this location in place, so the table shows one of the two rows.
        view.set_rows(vec![std::sync::Arc::new(item(
            "alpha.txt",
            r"C:\work",
            10,
            1,
        ))]);
        view.merge_visible_selection(&std::collections::HashSet::from([0usize]), false);
        view
    }

    #[test]
    fn leaving_the_bin_releases_the_whole_view() {
        let mut view = open_view();
        assert!(view.is_open());
        assert_eq!(view.all().len(), 2);
        assert_eq!(view.rows().expect("open bin has rows").len(), 1);
        assert_eq!(view.selected_keys().len(), 1);

        assert!(view.leave());

        assert!(!view.is_open());
        assert!(view.rows().is_none());
        assert!(view.all().is_empty());
        assert!(view.selected_keys().is_empty());
        assert!(view.error().is_none());
        assert!(!view.is_loading());
        assert!(
            view.icon_extension(bin_icon_key("alpha.txt", r"C:\work", 10, 1, false))
                .is_none()
        );
    }

    #[test]
    fn leaving_a_bin_that_was_never_open_reports_nothing_to_release() {
        let mut view = RecycleBinView::closed();
        assert!(!view.leave());
    }

    #[test]
    fn a_reload_starts_from_a_clean_listing_under_a_fresh_token() {
        let mut view = open_view();
        let first = view.generation();

        let second = view.begin_load();

        assert!(second > first);
        assert!(view.is_open());
        assert!(view.all().is_empty());
        assert!(view.selected_keys().is_empty());
        assert!(view.error().is_none());
        assert!(view.is_loading());
    }

    #[test]
    fn a_failed_listing_keeps_the_location_open_for_the_recovery_band() {
        let mut view = open_view();

        view.fail("access denied".to_string());

        // The bin stays the current location so the error band can offer the retry path.
        assert!(view.is_open());
        assert_eq!(view.error(), Some("access denied"));
        assert!(view.all().is_empty());
        assert!(view.selected_keys().is_empty());
        assert!(view.rows().is_none());
    }

    #[test]
    fn the_selection_drops_the_keys_a_fresh_listing_no_longer_holds() {
        let mut view = open_view();
        assert_eq!(view.selected_keys().len(), 1);

        view.install_listing(vec![std::sync::Arc::new(item(
            "beta.txt", r"C:\work", 20, 2,
        ))]);

        assert!(view.selected_keys().is_empty());
    }
}
