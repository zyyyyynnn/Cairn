// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Selection-driven file actions, table scrolling, and clipboard operations for the result list.

use crate::app::{CairnApp, join_row_path, layout};
use crate::search::ResultSetChange;
use crate::search::filters::FilterCategory;
use crate::shell::commands::*;
use crate::shell::location::ActiveLocation;
use crate::shell::os::{
    copy_lines, open_file, open_folder_and_select, open_folders_in_tabs, show_properties_dialog,
};
use crate::shell::reveal::{MAX_REVEAL_TARGETS, reveal_targets};
use crate::shell::status_bar::format_count;
use crate::table::model::SELECT_ALL_LIMIT;
use fluent_bundle::FluentArgs;
use gpui_kit::gpui::*;

/// Which way the keyboard asks the table to move across the axis the window cannot show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableScroll {
    /// One [`crate::app::layout::TABLE_SCROLL_STEP_FACTOR`] of the visible width towards the left edge.
    Left,
    /// The same step towards the right edge.
    Right,
    /// The table's leftmost column back against the frame's left edge.
    Start,
    /// The table's rightmost column against the frame's right edge.
    End,
}

impl CairnApp {
    pub fn move_selection(
        &mut self,
        forwards: bool,
        extend: bool,
        row_height: Pixels,
        cx: &mut Context<'_, Self>,
    ) {
        // Arrow keys are the table's keyboard path; moving through rows is what makes the focus
        // ring meaningful, and a pointer press must not suppress keyboard navigation. Focus
        // follows the selection so F2 and the row actions act on the chosen row.
        cairn_ui::focus::set_keyboard_navigation(true, cx);
        self.results.snippet_preview = None;
        let before = self.results.table.focused_idx;
        if forwards {
            self.results.table.move_selection_down(extend);
        } else {
            self.results.table.move_selection_up(extend);
        }
        if self.results.table.focused_idx != before
            && let Some(ix) = self.results.table.focused_idx
        {
            let current = self.scrolled_pixels();
            self.results.table.scroll_top = current;
            self.results
                .table
                .ensure_row_visible(ix, f32::from(row_height));
            if self.results.table.scroll_top != current {
                self.results
                    .rows_scroll
                    .set_offset(point(px(0.0), -px(self.results.table.scroll_top)));
            }
            self.ensure_visible_rows_loaded(row_height, cx);
        }
        self.discard_rename_off_row(cx);
        cx.notify();
    }

    pub fn select_previous_row(
        &mut self,
        _: &SelectPreviousRow,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let extend = window.modifiers().shift;
        self.move_selection(false, extend, layout::row_height(window), cx);
        self.focus_table_after_arrow(window, cx);
    }

    pub fn select_next_row(
        &mut self,
        _: &SelectNextRow,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let extend = window.modifiers().shift;
        self.move_selection(true, extend, layout::row_height(window), cx);
        self.focus_table_after_arrow(window, cx);
    }

    /// Arrow-key selection moves the keyboard into the table, where F2 and the
    /// row commands live; the search box keeps its text.
    pub(super) fn focus_table_after_arrow(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.results.table.focused_idx.is_some() {
            self.results.results_focus.focus(window, cx);
        }
    }

    pub fn scroll_table(&mut self, request: TableScroll, cx: &mut Context<'_, Self>) {
        cairn_ui::focus::set_keyboard_navigation(true, cx);
        self.results.snippet_preview = None;
        let offset = self.results.table_scroll.offset();
        let furthest = self.results.table_scroll.max_offset().x;
        let step = self.results.table_scroll.bounds().size.width * layout::TABLE_SCROLL_STEP_FACTOR;
        let target = match request {
            TableScroll::Left => (offset.x + step).min(px(0.)),
            TableScroll::Right => (offset.x - step).max(-furthest),
            TableScroll::Start => px(0.),
            TableScroll::End => -furthest,
        };
        self.results
            .table_scroll
            .set_offset(point(target, offset.y));
        cx.notify();
    }

    pub fn scroll_table_left(
        &mut self,
        _: &ScrollTableLeft,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.scroll_table(TableScroll::Left, cx);
    }

    pub fn scroll_table_right(
        &mut self,
        _: &ScrollTableRight,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.scroll_table(TableScroll::Right, cx);
    }

    pub fn scroll_table_start(
        &mut self,
        _: &ScrollTableStart,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.scroll_table(TableScroll::Start, cx);
    }

    pub fn scroll_table_end(
        &mut self,
        _: &ScrollTableEnd,
        _window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.scroll_table(TableScroll::End, cx);
    }

    pub fn focus_search_box(
        &mut self,
        _: &FocusSearchBox,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.dismiss_result_selection_on_chrome_press(cx);
        self.close_group();
        self.query.search_input.select_all();
        self.query.search_focus.focus(window, cx);
        cx.notify();
    }

    /// Clears the result selection and its keyboard anchor. The result rows stop the pointer
    /// press that selects them, so every other content surface can call this on its own press
    /// without deselecting the row the person just clicked.
    pub(crate) fn dismiss_result_selection(&mut self, cx: &mut Context<'_, Self>) {
        if self.results.table.selected_indices.is_empty()
            && self.results.table.focused_idx.is_none()
            && self.results.table.selection_anchor.is_none()
        {
            return;
        }
        self.results.table.clear_selection();
        self.results.snippet_preview = None;
        cairn_ui::focus::set_keyboard_navigation(false, cx);
        cx.notify();
    }

    /// The outside-click dismissal every chrome surface runs on its own press. The Recycle Bin
    /// is the exception: its selection is carried by item identity, and the next filter or sort
    /// merges the row indices into those keys. Clearing the indices on a press that is not a
    /// row would therefore delete the keys and drop the selection instead of preserving it.
    pub(crate) fn dismiss_result_selection_on_chrome_press(&mut self, cx: &mut Context<'_, Self>) {
        if !self.location_profile().preserves_selection_on_chrome_press {
            self.dismiss_result_selection(cx);
        }
    }

    /// Clears the query text and the filter attached to it, then returns focus to the search box.
    /// Both the clear affordance and Escape land here, so focus never stays on the clear button as
    /// it unmounts.
    pub(crate) fn clear_query_and_scope(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.active_location().is_recycle_bin() {
            self.clear_bin_filter(window, cx);
            return;
        }
        self.update_current_nav_state_text();
        self.query.search_input.clear();
        self.query.active_filter = FilterCategory::All;
        self.query.source_scope = None;
        self.record_nav_state();
        self.request_result_set(ResultSetChange::Replace, cx);
        self.query.search_focus.focus(window, cx);
        cx.notify();
    }

    pub fn escape_query(
        &mut self,
        _: &EscapeQuery,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.rename.draft.is_some() {
            self.cancel_rename(window, cx);
            return;
        }
        if self.open_group_menu.take().is_some() {
            self.query.search_focus.focus(window, cx);
            cx.notify();
            return;
        }
        if !self.toast.is_hidden() {
            self.dismiss_toast(cx);
            return;
        }
        if self.dismiss_snippet_preview(window, cx) {
            return;
        }
        if self.active_location().is_recycle_bin() {
            if !self.query.search_input.text.trim().is_empty() {
                self.clear_bin_filter(window, cx);
                return;
            }
            self.return_from_recycle_bin(window, cx);
            return;
        }
        if self.query.is_searching() && self.has_query() {
            self.cancel_in_flight_query(cx);
            return;
        }
        if self.has_query() {
            self.clear_query_and_scope(window, cx);
            return;
        }
        crate::app::hide_main_window();
    }
    pub fn select_all(&mut self, _: &SelectAll, window: &mut Window, cx: &mut Context<'_, Self>) {
        // A row click keeps the search box focused so typing continues there.
        // An existing row selection is the signal that the result list, not the
        // query text, is the surface Ctrl+A should act on.
        let results_active = !self.results.table.selected_indices.is_empty()
            || self.results.results_focus.contains_focused(window, cx);
        if self.query.search_focus.is_focused(window) && !results_active {
            self.query.search_input.select_all();
            cx.notify();
            return;
        }
        if self.results.table.total_rows() == 0 {
            return;
        }

        let loaded = self.results.table.loaded_range().len();
        let selected = self.results.table.select_all_loaded(SELECT_ALL_LIMIT);
        if selected == 0 {
            return;
        }
        if loaded > SELECT_ALL_LIMIT {
            let title = self.i18n.get_text("selection_limit_title");
            let mut args = FluentArgs::new();
            args.set("count", SELECT_ALL_LIMIT as i64);
            args.set("count_text", format_count(SELECT_ALL_LIMIT));
            let message = self
                .i18n
                .get_text_with_args("selection_limit_message", &args);
            self.show_info_toast(title, message, cx);
        }
        cx.notify();
    }

    pub(super) fn open_target_row(&self) {
        match self.active_location() {
            ActiveLocation::RecycleBin => {
                if let Some(item) = self.target_bin_item() {
                    let _ = open_file(&item.fs_path);
                }
            }
            ActiveLocation::Index => {
                if let Some(path) = self.target_full_path() {
                    let _ = open_file(&path);
                }
            }
        }
    }

    pub fn open_selected(
        &mut self,
        _: &OpenSelected,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        // Open is a read action: let a focused inline rename handle Enter itself,
        // otherwise dismiss the unfocused editor before opening the row.
        if self.rename.draft.is_some() {
            if self
                .rename
                .input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
            {
                return;
            }
            self.discard_rename(cx);
        }
        self.open_target_row();
        cx.notify();
    }

    pub fn reveal_selected_location(
        &mut self,
        _: &RevealSelectedLocation,
        _w: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if !self.location_profile().supports_reveal {
            return;
        }
        let paths = self.collect_selected_paths();
        let targets = reveal_targets(&paths);
        if targets.len() > MAX_REVEAL_TARGETS {
            let title = self.i18n.get_text("reveal_limit_title");
            let mut args = FluentArgs::new();
            args.set("count", targets.len() as i64);
            args.set("count_text", format_count(targets.len()));
            args.set("limit", MAX_REVEAL_TARGETS as i64);
            args.set("limit_text", format_count(MAX_REVEAL_TARGETS));
            let message = self.i18n.get_text_with_args("reveal_limit_message", &args);
            self.show_info_toast(title, message, cx);
            cx.notify();
            return;
        }
        if targets.is_empty() {
            if let Some(path) = self.target_full_path() {
                let _ = open_folder_and_select(&path);
            }
        } else {
            let targets = targets.clone();
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move { open_folders_in_tabs(&targets) })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if let Err(error) = result {
                        tracing::warn!("Explorer tab reveal failed: {error}");
                        let title = this.i18n.get_text("reveal_failed_title");
                        let message = this.i18n.get_text("reveal_failed_message");
                        this.show_error_toast(title, message, cx);
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    pub fn copy_full_paths(
        &mut self,
        _: &CopyFullPaths,
        _w: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let paths = self.collect_selected_paths();
        copy_lines(&paths);
        cx.notify();
    }

    pub fn target_row_ix(&self) -> Option<usize> {
        let ix = self.results.table.target_row()?;
        self.results.table.is_loaded(ix).then_some(ix)
    }

    /// The one row an in-place rename may edit. The bin is a location with its
    /// own surface, and a row outside the fetched window has no name to load into the editor.
    pub fn rename_target_ix(&self) -> Option<usize> {
        if !self.location_profile().supports_rename {
            return None;
        }
        self.results.table.sole_selected_row()
    }

    pub fn target_full_path(&self) -> Option<String> {
        let ix = self.target_row_ix()?;
        let row = self.results.table.row(ix)?;
        Some(join_row_path(&row.path, &row.name))
    }

    pub fn collect_selected_paths(&self) -> Vec<String> {
        self.selection_indices()
            .iter()
            .filter_map(|ix| self.results.table.row(*ix))
            .map(|row| join_row_path(&row.path, &row.name))
            .collect()
    }

    pub fn collect_selected_names(&self) -> Vec<String> {
        self.selection_indices()
            .iter()
            .filter_map(|ix| self.results.table.row(*ix))
            .map(|row| row.name.clone())
            .collect()
    }

    pub fn selection_indices(&self) -> Vec<usize> {
        let mut indices: Vec<usize> = if self.results.table.selected_indices.is_empty() {
            self.target_row_ix().into_iter().collect()
        } else {
            self.results
                .table
                .selected_indices
                .iter()
                .copied()
                .filter(|ix| self.results.table.is_loaded(*ix))
                .collect()
        };
        indices.sort_unstable();
        indices
    }

    pub fn show_selected_properties(&mut self, cx: &mut Context<'_, Self>) {
        // The properties sheet of a bin payload describes the `$R` file, not
        // the item Explorer shows; the bin hides this command rather than
        // presenting a misleading name and location.
        if !self.location_profile().supports_properties_dialog {
            return;
        }
        if let Some(path) = self.target_full_path() {
            let _ = show_properties_dialog(&path);
        }
        cx.notify();
    }

    pub fn show_properties(
        &mut self,
        _: &ShowProperties,
        _w: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.show_selected_properties(cx);
    }

    pub(crate) fn restore_result_tooltips(&mut self, cx: &mut Context<'_, Self>) {
        if self.tooltip_suppression.restore() {
            cx.notify();
        }
    }

    pub fn focus_row_for_context(&mut self, ix: usize, cx: &mut Context<'_, Self>) {
        self.results.snippet_preview = None;
        if !self.results.table.selected_indices.contains(&ix) {
            self.results.table.select_row(ix, false);
        }
        // The editor on another row closes; the menu describes this row.
        self.discard_rename_off_row(cx);
    }

    pub fn select_row_by_pointer(
        &mut self,
        ix: usize,
        add_to_selection: bool,
        extend_selection: bool,
        is_double_click: bool,
        row_height: Pixels,
        cx: &mut Context<'_, Self>,
    ) {
        cairn_ui::focus::set_keyboard_navigation(false, cx);
        self.results.snippet_preview = None;
        if extend_selection {
            let anchor = self
                .results
                .table
                .selection_anchor
                .or(self.results.table.focused_idx)
                .unwrap_or(ix);
            self.results
                .table
                .select_range(anchor, ix, add_to_selection);
        } else if add_to_selection {
            self.results.table.toggle_row(ix);
        } else {
            self.results.table.select_row(ix, false);
        }
        self.discard_rename_off_row(cx);
        if is_double_click {
            self.open_target_row();
        }
        self.ensure_visible_rows_loaded(row_height, cx);
        cx.notify();
    }
}
