// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Settings, appearance and window commands: sidebar visibility, saved preferences, theme,
//! language, autostart, zoom, the search flag toggles, the location filter, and the result
//! table's sort order and column widths.

use crate::app::{layout, CairnApp};
use crate::search::filters::FilterCategory;
use crate::search::ResultSetChange;
use crate::shell::command_groups::ZoomPreset;
use crate::shell::commands::{
    CycleFilterCategory, ResetZoom, ToggleCaseSensitive, ToggleMatchPath, ToggleRegex,
    ToggleSidebar, ToggleWholeWord, ZoomIn, ZoomOut,
};
use crate::shell::menu_actions::ToggleFlag;
use crate::shell::settings::THEME_SYSTEM;
use crate::shell::theme;
use crate::table::model::{ColumnBoundary, ColumnWidths, SortColumn, TableColumn};
use gpui_kit::gpui::*;

impl CairnApp {
    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        cx.notify();
    }

    pub(crate) fn toggle_sidebar_action(
        &mut self,
        _: &ToggleSidebar,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_sidebar(cx);
    }

    #[cfg(not(test))]
    pub fn save_settings(&mut self) {
        if let Err(e) = self.settings.save() {
            tracing::warn!("Failed to save settings: {}", e);
        }
    }

    #[cfg(test)]
    pub fn save_settings(&mut self) {}

    fn commit_sort_spec_change(&mut self, cx: &mut Context<Self>) {
        self.settings.columns.sort_spec = self.results.table.sort_spec.to_clause();
        self.save_settings();
        if self.recycle_bin.is_open() {
            // The bin is an in-memory listing, so its order is applied here instead
            // of asking the service for a new page sorted by the same column.
            self.sort_recycle_bin(cx);
        } else {
            self.request_result_set(ResultSetChange::InPlace, cx);
        }
        cx.notify();
    }

    pub fn toggle_table_sort(&mut self, col: SortColumn, cx: &mut Context<Self>) {
        self.results.table.toggle_sort(col);
        self.commit_sort_spec_change(cx);
    }

    pub fn shift_toggle_table_sort(&mut self, col: SortColumn, cx: &mut Context<Self>) {
        self.results.table.shift_toggle_sort(col);
        self.commit_sort_spec_change(cx);
    }

    /// Shows or hides one table column in index queries. Locked columns remain visible, and hiding
    /// a sorted column prunes it from `sort_spec` so the active order stays inspectable.
    pub(crate) fn toggle_table_column(&mut self, col: TableColumn, cx: &mut Context<Self>) {
        if col.is_locked() || self.recycle_bin.is_open() {
            return;
        }
        let shown = !self.settings.columns.is_column_shown(col);
        self.settings.columns.set_column_shown(col, shown);
        if !shown && self.results.table.remove_sort_column(col.sort_column()) {
            self.commit_sort_spec_change(cx);
            return;
        }
        self.save_settings();
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
        self.results.table.reverse_primary_sort();
        self.commit_sort_spec_change(cx);
    }

    fn toggle_flag(&mut self, flag: ToggleFlag, cx: &mut Context<Self>) {
        // The bin filter honors only case sensitivity; the other flags
        // have no effect on its rows.
        if self.recycle_bin.is_open() && flag != ToggleFlag::Case {
            return;
        }
        match flag {
            ToggleFlag::Case => self.query.case_sensitive = !self.query.case_sensitive,
            ToggleFlag::WholeWord => self.query.whole_word = !self.query.whole_word,
            ToggleFlag::MatchPath => self.query.match_path = !self.query.match_path,
            ToggleFlag::Regex => {
                let enabling = !self.query.use_regex;
                self.query.use_regex = enabling;
                if enabling {
                    self.query.pinyin = false;
                    self.query.ignore_punctuation = false;
                    self.query.ignore_whitespace = false;
                    self.query.ignore_diacritics = false;
                }
            }
        }
        self.request_result_set(ResultSetChange::InPlace, cx);
        cx.notify();
    }

    pub(crate) fn toggle_exclude_hidden(&mut self, cx: &mut Context<Self>) {
        self.toggle_exclusion_filter(ExclusionFlag::Hidden, cx);
    }

    pub(crate) fn toggle_exclude_system(&mut self, cx: &mut Context<Self>) {
        self.toggle_exclusion_filter(ExclusionFlag::System, cx);
    }

    pub(crate) fn toggle_pinyin(&mut self, cx: &mut Context<Self>) {
        self.toggle_query_modifier(|this| &mut this.query.pinyin, cx);
    }

    pub(crate) fn toggle_ignore_punctuation(&mut self, cx: &mut Context<Self>) {
        self.toggle_query_modifier(|this| &mut this.query.ignore_punctuation, cx);
    }

    pub(crate) fn toggle_ignore_whitespace(&mut self, cx: &mut Context<Self>) {
        self.toggle_query_modifier(|this| &mut this.query.ignore_whitespace, cx);
    }

    pub(crate) fn toggle_ignore_diacritics(&mut self, cx: &mut Context<Self>) {
        self.toggle_query_modifier(|this| &mut this.query.ignore_diacritics, cx);
    }

    fn toggle_query_modifier(
        &mut self,
        field: impl FnOnce(&mut Self) -> &mut bool,
        cx: &mut Context<Self>,
    ) {
        if self.recycle_bin.is_open() {
            return;
        }
        let value = field(self);
        let enabling = !*value;
        *value = enabling;
        if enabling {
            self.query.use_regex = false;
        }
        self.request_result_set(ResultSetChange::InPlace, cx);
        cx.notify();
    }

    fn toggle_exclusion_filter(&mut self, flag: ExclusionFlag, cx: &mut Context<Self>) {
        if self.recycle_bin.is_open() {
            return;
        }
        match flag {
            ExclusionFlag::Hidden => {
                self.settings.exclude_hidden = !self.settings.exclude_hidden;
            }
            ExclusionFlag::System => {
                self.settings.exclude_system = !self.settings.exclude_system;
            }
        }
        self.save_settings();
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
        // A file-type row, 所有文件 included, is a location: choosing it releases a folder-source
        // scope. Re-selecting the same category while scoped is therefore a place change, not a
        // refresh.
        let had_scope = self.query.source_scope.take().is_some();
        if self.query.active_filter == category && !self.recycle_bin.is_open() && !had_scope {
            // Re-selecting the active location refreshes it, matching the
            // recycle-bin entry.
            self.request_result_set(ResultSetChange::Refresh, cx);
            cx.notify();
            return;
        }
        // The bin's filter is a location-local search, not a query: the
        // category's index search and history entry must not inherit the text,
        // and the bin's entry already holds the query that preceded it.
        let leaving_filter = self.recycle_bin.is_open();
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
        self.repaint_shell_and_overlays(cx);
    }

    pub(crate) fn zoom_in(&mut self, _: &ZoomIn, _w: &mut Window, cx: &mut Context<Self>) {
        self.set_zoom(ZoomPreset::step_up(self.zoom_level).value(), cx);
    }

    pub(crate) fn zoom_out(&mut self, _: &ZoomOut, _w: &mut Window, cx: &mut Context<Self>) {
        self.set_zoom(ZoomPreset::step_down(self.zoom_level).value(), cx);
    }

    pub(crate) fn reset_zoom(&mut self, _: &ResetZoom, _w: &mut Window, cx: &mut Context<Self>) {
        self.set_zoom(ZoomPreset::Standard.value(), cx);
    }
}

#[derive(Clone, Copy)]
enum ExclusionFlag {
    Hidden,
    System,
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::rust_2021::test;

    #[test]
    fn ui_zoom_and_sidebar_actions_update_state_and_theme() {
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
                        let initially_collapsed = this.sidebar_collapsed;
                        this.toggle_sidebar_action(
                            &crate::shell::commands::ToggleSidebar,
                            window,
                            cx,
                        );
                        assert_ne!(this.sidebar_collapsed, initially_collapsed);

                        this.set_zoom(
                            crate::shell::command_groups::ZoomPreset::Standard.value(),
                            cx,
                        );
                        let standard_font = gpui_kit::component::Theme::global(cx).font_size;
                        this.zoom_in(&crate::shell::commands::ZoomIn, window, cx);
                        assert_eq!(
                            this.zoom_level,
                            crate::shell::command_groups::ZoomPreset::Relaxed.value()
                        );
                        assert_eq!(
                            this.settings.zoom_level,
                            crate::shell::command_groups::ZoomPreset::Relaxed.value()
                        );
                        assert!(gpui_kit::component::Theme::global(cx).font_size > standard_font);

                        this.zoom_out(&crate::shell::commands::ZoomOut, window, cx);
                        assert_eq!(
                            this.zoom_level,
                            crate::shell::command_groups::ZoomPreset::Standard.value()
                        );

                        this.zoom_out(&crate::shell::commands::ZoomOut, window, cx);
                        assert_eq!(
                            this.zoom_level,
                            crate::shell::command_groups::ZoomPreset::Compact.value()
                        );
                        assert!(gpui_kit::component::Theme::global(cx).font_size < standard_font);

                        this.reset_zoom(&crate::shell::commands::ResetZoom, window, cx);
                        assert_eq!(
                            this.zoom_level,
                            crate::shell::command_groups::ZoomPreset::Standard.value()
                        );

                        this.settings.exclude_hidden = false;
                        this.settings.exclude_system = false;
                        this.toggle_exclude_hidden(cx);
                        this.toggle_exclude_system(cx);
                        assert!(this.settings.exclude_hidden);
                        assert!(this.settings.exclude_system);

                        use crate::table::model::{OptionalColumn, TableColumn};
                        this.settings.columns = Default::default();
                        this.results.table.sort_spec = cairn_core::sort::SortSpec::default();
                        this.toggle_table_column(TableColumn::Name, cx);
                        assert!(this.settings.columns.is_column_shown(TableColumn::Name));

                        let full_min_width = this.table_min_width();
                        this.toggle_table_sort(SortColumn::Path, cx);
                        this.toggle_table_column(TableColumn::Path, cx);
                        assert!(!this.settings.columns.show_path);
                        assert_eq!(this.settings.columns.sort_spec, "name-asc");
                        assert!(this.table_min_width() < full_min_width);

                        this.toggle_table_column(TableColumn::Optional(OptionalColumn::Length), cx);
                        assert!(this.settings.columns.show_length);

                        this.toggle_table_sort(SortColumn::Size, cx);
                        this.shift_toggle_table_sort(SortColumn::Length, cx);
                        assert_eq!(this.settings.columns.sort_spec, "size-asc,length-asc");

                        this.toggle_table_column(TableColumn::Optional(OptionalColumn::Length), cx);
                        assert!(!this.settings.columns.show_length);
                        assert_eq!(this.settings.columns.sort_spec, "size-asc");
                    });

                    cx.quit();
                    app
                },
            );
        });
    }
}
