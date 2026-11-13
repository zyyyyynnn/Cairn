// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Settings, appearance and window commands: sidebar visibility, saved preferences, theme,
//! language, autostart, zoom, the search flag toggles, the location filter, and the result
//! table's sort order and column widths.

use crate::app::{layout, CairnApp};
use crate::search::filters::FilterCategory;
use crate::search::ResultSetChange;
use crate::shell::commands::{
    CycleFilterCategory, ToggleCaseSensitive, ToggleMatchPath, ToggleRegex, ToggleWholeWord,
};
use crate::shell::menu_actions::ToggleFlag;
use crate::shell::settings::THEME_SYSTEM;
use crate::shell::theme;
use crate::table::model::{ColumnBoundary, ColumnWidths, SortColumn};
use gpui_kit::gpui::*;

impl CairnApp {
    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        cx.notify();
    }

    pub fn save_settings(&mut self) {
        if let Err(e) = self.settings.save() {
            tracing::warn!("Failed to save settings: {}", e);
        }
    }

    pub fn toggle_table_sort(&mut self, col: SortColumn, cx: &mut Context<Self>) {
        self.results.table.toggle_sort(col);
        self.settings.columns.sort_column = col.key().to_string();
        self.settings.columns.sort_asc = self.results.table.sort_asc;
        self.save_settings();
        if self.recycle_bin.is_open() {
            // The bin is an in-memory listing, so its order is applied here instead
            // of asking the service for a new page sorted by the same column.
            self.sort_recycle_bin(cx);
        } else if self.block_listing.is_some() {
            // A summary block listing is in memory too: sort it locally.
            self.sort_block_listing(cx);
        } else {
            self.request_result_set(ResultSetChange::InPlace, cx);
        }
        cx.notify();
    }

    /// Applies a drag on one header boundary, measured from the widths the drag started with so a
    /// long drag cannot accumulate rounding or fight the minimum clamps.
    pub(crate) fn resize_columns_to(
        &mut self,
        boundary: ColumnBoundary,
        start: ColumnWidths,
        delta: f32,
        cx: &mut Context<Self>,
    ) {
        let minima = [
            layout::NAME_MIN_COLUMN,
            layout::PATH_MIN_COLUMN,
            layout::SIZE_COLUMN,
            layout::DATE_COLUMN,
        ];
        let widths = start.resized(boundary, delta, minima);
        if widths != self.results.column_widths {
            self.results.column_widths = widths;
            self.store_column_widths();
            cx.notify();
        }
    }

    /// Writes the live widths back to the preferences file once, when the drag ends.
    pub(crate) fn finish_column_resize(&mut self, _cx: &mut Context<Self>) {
        self.store_column_widths();
        self.save_settings();
    }

    fn store_column_widths(&mut self) {
        self.settings.columns.name_width =
            self.results.column_widths.name * crate::app::COLUMN_WIDTH_STORAGE_UNIT;
        self.settings.columns.path_width =
            self.results.column_widths.path * crate::app::COLUMN_WIDTH_STORAGE_UNIT;
        self.settings.columns.size_width =
            self.results.column_widths.size * crate::app::COLUMN_WIDTH_STORAGE_UNIT;
        self.settings.columns.modified_width =
            self.results.column_widths.date * crate::app::COLUMN_WIDTH_STORAGE_UNIT;
    }

    /// Reverses the direction of the column that is already sorting the results.
    pub(super) fn toggle_sort_direction(&mut self, cx: &mut Context<Self>) {
        self.results.table.sort_asc = !self.results.table.sort_asc;
        self.settings.columns.sort_asc = self.results.table.sort_asc;
        self.save_settings();
        if self.recycle_bin.is_open() {
            self.sort_recycle_bin(cx);
        } else if self.block_listing.is_some() {
            self.sort_block_listing(cx);
        } else {
            self.request_result_set(ResultSetChange::InPlace, cx);
        }
        cx.notify();
    }

    fn toggle_flag(&mut self, flag: ToggleFlag, cx: &mut Context<Self>) {
        // Both in-place filters honor only case sensitivity; the other flags
        // have no effect on their rows.
        if (self.recycle_bin.is_open() || self.block_listing.is_some()) && flag != ToggleFlag::Case
        {
            return;
        }
        match flag {
            ToggleFlag::Case => self.query.case_sensitive = !self.query.case_sensitive,
            ToggleFlag::WholeWord => self.query.whole_word = !self.query.whole_word,
            ToggleFlag::MatchPath => self.query.match_path = !self.query.match_path,
            ToggleFlag::Regex => self.query.use_regex = !self.query.use_regex,
        }
        self.request_result_set(ResultSetChange::InPlace, cx);
        cx.notify();
    }

    pub fn toggle_case_sensitive(
        &mut self,
        _: &ToggleCaseSensitive,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_flag(ToggleFlag::Case, cx);
    }

    pub fn toggle_whole_word(
        &mut self,
        _: &ToggleWholeWord,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_flag(ToggleFlag::WholeWord, cx);
    }

    pub fn toggle_match_path(
        &mut self,
        _: &ToggleMatchPath,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_flag(ToggleFlag::MatchPath, cx);
    }

    pub fn toggle_regex(&mut self, _: &ToggleRegex, _w: &mut Window, cx: &mut Context<Self>) {
        self.toggle_flag(ToggleFlag::Regex, cx);
    }

    pub fn set_filter(&mut self, category: FilterCategory, cx: &mut Context<Self>) {
        if self.query.active_filter == category
            && self.drive_view.is_none()
            && !self.recycle_bin.is_open()
            && self.block_listing.is_none()
        {
            // Re-selecting the active location refreshes it, matching the
            // recycle-bin and drive entries.
            self.request_result_set(ResultSetChange::Replace, cx);
            cx.notify();
            return;
        }
        // Leaving the visualization releases its partial tree instead of keeping
        // it alive behind the table, and starts a fresh table entrance.
        // `leave_drive_view` opens the session for the view it just emptied.
        self.leave_drive_view();
        // The bin's and a block listing's filters are location-local searches,
        // not queries: the category's index search and history entry must not
        // inherit the text, and the bin's entry already holds the query that
        // preceded it.
        let leaving_filter = self.recycle_bin.is_open() || self.block_listing.take().is_some();
        if leaving_filter {
            self.query.search_input.clear();
        } else {
            self.update_current_nav_state_text();
        }
        self.recycle_bin.leave();
        self.query.active_filter = category;
        self.record_nav_state();
        self.request_result_set(ResultSetChange::Replace, cx);
        cx.notify();
    }

    pub fn cycle_filter(
        &mut self,
        _: &CycleFilterCategory,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_filter(self.query.active_filter.next(), cx);
    }

    /// Persists a theme choice (`system`, `light` or `dark`) and applies its resolved palette.
    /// Re-selecting `system` also re-reads the OS appearance, so a Windows theme change is picked
    /// up the next time that menu item is chosen.
    pub(super) fn set_theme_mode(&mut self, mode: &str, cx: &mut Context<Self>) {
        if self.settings.theme == mode && mode != THEME_SYSTEM {
            return;
        }
        self.settings.theme = mode.to_string();
        if mode == THEME_SYSTEM {
            self.system_dark = theme::system_prefers_dark();
        }
        self.save_settings();
        theme::set_dark_mode(self.settings.is_dark(self.system_dark), cx);
        cx.notify();
    }

    pub(super) fn set_language(&mut self, language: &str, cx: &mut Context<Self>) {
        if self.settings.language == language {
            return;
        }
        self.settings.language = language.to_string();
        let effective = crate::shell::settings::effective_language(language);
        self.i18n.set_language(&effective);
        #[cfg(windows)]
        crate::shell::tray::set_language_with_i18n(&self.i18n);
        self.save_settings();
        self.repaint_shell_and_overlays(cx);
    }

    pub(super) fn set_autostart(&mut self, enable: bool, cx: &mut Context<Self>) {
        if self.settings.autostart == enable {
            return;
        }
        self.settings.autostart = enable;
        self.save_settings();
        if let Err(err) = crate::shell::settings::set_autostart_registry(enable) {
            tracing::warn!("Failed to update autostart registry entry: {err}");
        }
        self.repaint_shell_and_overlays(cx);
    }

    pub fn repaint_shell_and_overlays(&self, cx: &mut Context<Self>) {
        cx.notify();
        cx.refresh_windows();
    }

    pub(super) fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let zoom = theme::set_zoom(zoom, cx);
        if zoom == self.zoom_level {
            return;
        }
        self.zoom_level = zoom;
        self.settings.zoom_level = zoom;
        self.save_settings();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::block_listing::BlockListing;
    use core::prelude::rust_2021::test;

    fn block_listing() -> BlockListing {
        BlockListing::new(r"C:\Windows".to_string(), 2, Vec::new())
    }

    #[test]
    fn leaving_a_block_listing_for_a_category_does_not_carry_its_filter() {
        gpui_kit::application().run(|cx: &mut gpui_kit::gpui::App| {
            gpui_kit::init(cx);
            let _ = gpui_kit::open_window(
                gpui_kit::gpui::WindowOptions {
                    show: false,
                    focus: false,
                    ..gpui_kit::gpui::WindowOptions::default()
                },
                cx,
                |window, cx| {
                    let app = cx.new(|cx| CairnApp::new(window, cx));

                    app.update(cx, |this, cx| {
                        this.query.search_input.set_text("kept query");
                        this.record_nav_state();
                        let previous = this.nav_history_index;
                        this.block_listing = Some(block_listing());
                        this.query.search_input.set_text("amd64");
                        this.set_filter(FilterCategory::Document, cx);
                        assert!(this.block_listing.is_none());
                        assert!(this.query.search_input.text.is_empty());
                        assert_eq!(this.nav_history[previous].text, "kept query");
                        assert_eq!(this.nav_history[this.nav_history_index].text, "");
                    });

                    cx.quit();
                    app
                },
            );
        });
    }

    #[test]
    fn search_flags_stay_fixed_while_a_block_listing_is_open() {
        gpui_kit::application().run(|cx: &mut gpui_kit::gpui::App| {
            gpui_kit::init(cx);
            let _ = gpui_kit::open_window(
                gpui_kit::gpui::WindowOptions {
                    show: false,
                    focus: false,
                    ..gpui_kit::gpui::WindowOptions::default()
                },
                cx,
                |window, cx| {
                    let app = cx.new(|cx| CairnApp::new(window, cx));

                    app.update(cx, |this, cx| {
                        this.block_listing = Some(block_listing());
                        let flags = (
                            this.query.whole_word,
                            this.query.match_path,
                            this.query.use_regex,
                        );
                        this.toggle_flag(ToggleFlag::WholeWord, cx);
                        this.toggle_flag(ToggleFlag::MatchPath, cx);
                        this.toggle_flag(ToggleFlag::Regex, cx);
                        assert_eq!(
                            (
                                this.query.whole_word,
                                this.query.match_path,
                                this.query.use_regex,
                            ),
                            flags
                        );
                        let case_sensitive = this.query.case_sensitive;
                        this.toggle_flag(ToggleFlag::Case, cx);
                        assert_ne!(this.query.case_sensitive, case_sensitive);
                    });

                    cx.quit();
                    app
                },
            );
        });
    }
}
