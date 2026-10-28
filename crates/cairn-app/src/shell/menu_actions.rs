// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Menu actions and command handlers: toolbar menus, shortcuts, clipboard actions,
//! table keyboard scrolling, flag toggles, theme and zoom controls.

use crate::app::{join_row_path, layout, CairnApp};
use crate::search::filters::FilterCategory;
use crate::service::search_scope::SearchScope;
use crate::shell::command_groups::{group_spec, GroupCommand, MenuGroup, ZoomPreset};
use crate::shell::commands::*;
use crate::shell::menu_rules::IndexMenuState;
use crate::shell::os::{
    delete_permanently_from_recycle_bin, delete_to_recycle_bin, empty_recycle_bin, open_file,
    open_folder_and_select, restore_from_recycle_bin, set_clipboard_text, show_properties_dialog,
    BinOperationError, RecycleBinItem,
};
use crate::shell::settings::THEME_SYSTEM;
use crate::shell::theme;
use crate::table::model::{format_timestamp, ColumnBoundary, ColumnWidths, SortColumn};
use crate::view;
use cairn_core::format_size_bytes;
use cairn_protocol::status::ContentIndexState;
use fluent_bundle::FluentArgs;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::dialog::{AlertDialog, DialogButtonProps, DialogEntrance};
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::component::WindowExt as _;
use gpui_kit::gpui::*;
use std::ops::Range;

/// Which search flag a toggle command flips, so the five toggle Actions share one code path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToggleFlag {
    Case,
    WholeWord,
    MatchPath,
    Regex,
}

/// Which way the keyboard asks the table to move across the axis the window cannot show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableScroll {
    /// One [`layout::TABLE_SCROLL_STEP_FACTOR`] of the visible width towards the left edge.
    Left,
    /// The same step towards the right edge.
    Right,
    /// The table's leftmost column back against the frame's left edge.
    Start,
    /// The table's rightmost column against the frame's right edge.
    End,
}

pub fn copy_lines(lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let _ = set_clipboard_text(&lines.join("\r\n"));
}

/// Diagnostic text from the service can be a whole sentence; a menu row stays one line. Trim it
/// to a readable length on a character boundary so CJK reasons are not cut mid-glyph.
fn short_reason(reason: &str) -> String {
    const MAX_CHARS: usize = 120;
    // One menu row is one line: collapse the diagnostic's own line breaks before truncating.
    let reason = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut short: String = reason.chars().take(MAX_CHARS).collect();
    if short.chars().count() < reason.chars().count() {
        short.push('…');
    }
    short
}

pub fn sort_column_key(col: SortColumn) -> &'static str {
    match col {
        SortColumn::Name => "name",
        SortColumn::Path => "path",
        SortColumn::Size => "size",
        SortColumn::DateModified => "modified",
        SortColumn::DateCreated => "created",
        SortColumn::Extension => "ext",
    }
}

pub fn sort_column_from_key(key: &str) -> SortColumn {
    match key.to_ascii_lowercase().as_str() {
        "path" => SortColumn::Path,
        "size" => SortColumn::Size,
        "modified" | "datemodified" => SortColumn::DateModified,
        "created" | "datecreated" => SortColumn::DateCreated,
        "ext" | "extension" => SortColumn::Extension,
        _ => SortColumn::Name,
    }
}

/// Builds the rows of one menu. A submenu's own popup is created here because its items carry the
/// same actions and checked state as the top-level rows; the caller can then hand the whole list to
/// one `PopupMenu::build`.
fn menu_entries(
    app: &CairnApp,
    view: &Entity<CairnApp>,
    commands: &'static [GroupCommand],
    window: &mut Window,
    cx: &mut Context<CairnApp>,
) -> Vec<view::MenuEntry> {
    commands
        .iter()
        .filter(|command| !app.command_is_hidden(**command))
        .map(|command| {
            let command = *command;
            let heading = command.is_heading();
            let children = command.submenu_children();
            let submenu = if children.is_empty() {
                None
            } else {
                let child_entries = menu_entries(app, view, children, window, cx);
                let action_context = app.root_focus.clone();
                let submenu_view = view.clone();
                Some(PopupMenu::build(window, cx, move |menu, _window, _cx| {
                    view::build_menu(
                        menu,
                        &child_entries,
                        action_context.clone(),
                        submenu_view.clone(),
                    )
                }))
            };
            let has_submenu = submenu.is_some();
            view::MenuEntry {
                label: app.command_label(command).into(),
                checked: !heading && !has_submenu && app.command_is_checked(command),
                disabled: !heading && app.command_is_disabled(command),
                action: command.action(),
                // A heading is drawn by `PopupMenu::label`, and a submenu row owns a nested popup
                // instead of a command.
                command: if heading || has_submenu {
                    None
                } else {
                    Some(command)
                },
                hint: command.hint_action(),
                submenu,
                separates: false,
            }
        })
        .collect()
}

/// Shared confirmation geometry: a short surface sized to its content, centered instead of parked
/// near the top, with the standard footer buttons. The labels travel with the variants so a caller
/// cannot replace the button configuration and lose the danger commit or the secondary cancel:
/// every confirmation on this helper destroys data.
fn style_confirmation_alert(
    alert: AlertDialog,
    window: &Window,
    ok_text: impl Into<SharedString>,
    cancel_text: impl Into<SharedString>,
) -> AlertDialog {
    alert
        .width(window.rem_size() * layout::CONFIRMATION_WIDTH)
        .margin_top(confirmation_top_offset(window))
        .min_h(px(0.))
        // A short settle from just above the resting seat reads calmer than the
        // legacy slide from the window top, and shares the theme's reduced-motion
        // policy. gpui-component 0.8 removes a dialog immediately on close and
        // exposes no closing phase, so only the entrance can be shaped here.
        .entrance(DialogEntrance::FadeSlide)
        // Both footer actions use the same borderless medium frame. The default
        // Cancel border made the filled action read one pixel taller although
        // their outer bounds were equal. The commit button is danger, not a
        // neutral primary: every confirmation on this helper destroys data.
        .button_props(
            DialogButtonProps::default()
                .ok_text(ok_text)
                .cancel_text(cancel_text)
                .cancel_variant(ButtonVariant::Secondary)
                .ok_variant(ButtonVariant::Danger),
        )
}

/// The shell can report a batch as successful while a progress dialog skipped one item. The
/// fresh listing is the authority: an item still present after the operation is a failure.
fn reconcile_bin_items(
    outcome: Result<(), BinOperationError>,
    listing: &Result<Vec<RecycleBinItem>, String>,
    requested: &[(String, String)],
) -> Result<(), BinOperationError> {
    match (outcome, listing) {
        (Ok(()), Ok(items)) => {
            let live: std::collections::HashSet<String> =
                items.iter().map(RecycleBinItem::identity_key).collect();
            let failed: Vec<String> = requested
                .iter()
                .filter(|(key, _)| live.contains(key))
                .map(|(_, name)| name.clone())
                .collect();
            if failed.is_empty() {
                Ok(())
            } else {
                Err(BinOperationError::failed(
                    "some items were not processed",
                    failed,
                ))
            }
        }
        (outcome, _) => outcome,
    }
}

fn reconcile_empty_bin(
    outcome: Result<(), BinOperationError>,
    listing: &Result<Vec<RecycleBinItem>, String>,
) -> Result<(), BinOperationError> {
    match (outcome, listing) {
        (Ok(()), Ok(items)) if !items.is_empty() => Err(BinOperationError::failed(
            "some items still remain",
            items.iter().map(|item| item.name.clone()).collect(),
        )),
        (outcome, _) => outcome,
    }
}

fn confirmation_top_offset(window: &Window) -> Pixels {
    let estimated_height = window.rem_size() * layout::CONFIRMATION_ESTIMATED_HEIGHT;
    confirmation_top_offset_for_height(
        window.viewport_size().height,
        estimated_height,
        window.rem_size() * layout::SPACE_24,
    )
}

/// A stable upper-middle seat for a short alert. The dialog has no pre-layout height to read, so
/// this estimates one; wrapped copy grows downward without moving the top edge.
fn confirmation_top_offset_for_height(
    viewport_height: Pixels,
    estimated_height: Pixels,
    minimum: Pixels,
) -> Pixels {
    ((viewport_height - estimated_height) / 2.).max(minimum)
}

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
        self.settings.columns.sort_column = sort_column_key(col).to_string();
        self.settings.columns.sort_asc = self.results.table.sort_asc;
        self.save_settings();
        if self.recycle_bin.is_open() {
            // The bin is an in-memory listing, so its order is applied here instead
            // of asking the service for a new page sorted by the same column.
            self.sort_recycle_bin(cx);
        } else {
            self.trigger_search(cx);
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
        self.settings.columns.name_width = self.results.column_widths.name * crate::app::PX_PER_REM;
        self.settings.columns.path_width = self.results.column_widths.path * crate::app::PX_PER_REM;
        self.settings.columns.size_width = self.results.column_widths.size * crate::app::PX_PER_REM;
        self.settings.columns.modified_width =
            self.results.column_widths.date * crate::app::PX_PER_REM;
    }

    /// Reverses the direction of the column that is already sorting the results.
    pub fn toggle_sort_direction(&mut self, cx: &mut Context<Self>) {
        self.results.table.sort_asc = !self.results.table.sort_asc;
        self.settings.columns.sort_asc = self.results.table.sort_asc;
        self.save_settings();
        if self.recycle_bin.is_open() {
            self.sort_recycle_bin(cx);
        } else {
            self.trigger_search(cx);
        }
        cx.notify();
    }

    pub fn move_selection(
        &mut self,
        forwards: bool,
        extend: bool,
        row_height: Pixels,
        cx: &mut Context<Self>,
    ) {
        // Arrow keys are the table's keyboard path; moving through rows is what makes the focus
        // ring meaningful, and a pointer press must not suppress keyboard navigation.
        self.results.keyboard_navigation = true;
        let before = self.results.table.focused_idx;
        if forwards {
            self.results.table.move_selection_down(extend);
        } else {
            self.results.table.move_selection_up(extend);
        }
        if self.results.table.focused_idx != before {
            if let Some(ix) = self.results.table.focused_idx {
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
        }
        cx.notify();
    }

    pub fn select_previous_row(
        &mut self,
        _: &SelectPreviousRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.drive_view.is_some() {
            return;
        }
        let extend = window.modifiers().shift;
        self.move_selection(false, extend, layout::row_height(window), cx);
    }

    pub fn select_next_row(
        &mut self,
        _: &SelectNextRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.drive_view.is_some() {
            return;
        }
        let extend = window.modifiers().shift;
        self.move_selection(true, extend, layout::row_height(window), cx);
    }

    pub fn scroll_table(&mut self, request: TableScroll, cx: &mut Context<Self>) {
        self.results.keyboard_navigation = true;
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
        cx: &mut Context<Self>,
    ) {
        self.scroll_table(TableScroll::Left, cx);
    }

    pub fn scroll_table_right(
        &mut self,
        _: &ScrollTableRight,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.scroll_table(TableScroll::Right, cx);
    }

    pub fn scroll_table_start(
        &mut self,
        _: &ScrollTableStart,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.scroll_table(TableScroll::Start, cx);
    }

    pub fn scroll_table_end(
        &mut self,
        _: &ScrollTableEnd,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.scroll_table(TableScroll::End, cx);
    }

    pub fn focus_search_box(
        &mut self,
        _: &FocusSearchBox,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The Treemap replaces the query band; focusing its hidden search box
        // would take the keyboard away from the visualization.
        if self.drive_view.is_some() {
            return;
        }
        self.dismiss_result_selection_on_chrome_press(cx);
        self.close_group();
        self.query.search_input.select_all();
        self.query.search_focus.focus(window, cx);
        cx.notify();
    }

    /// Clears the result selection and its keyboard anchor. The result rows stop the pointer
    /// press that selects them, so every other content surface can call this on its own press
    /// without deselecting the row the person just clicked.
    pub(crate) fn dismiss_result_selection(&mut self, cx: &mut Context<Self>) {
        if self.results.table.selected_indices.is_empty()
            && self.results.table.focused_idx.is_none()
            && self.results.table.selection_anchor.is_none()
        {
            return;
        }
        self.results.table.clear_selection();
        self.results.keyboard_navigation = false;
        cx.notify();
    }

    /// The outside-click dismissal every chrome surface runs on its own press. The Recycle Bin
    /// is the exception: its selection is carried by item identity, and the next filter or sort
    /// merges the row indices into those keys. Clearing the indices on a press that is not a
    /// row would therefore delete the keys and drop the selection instead of preserving it.
    pub(crate) fn dismiss_result_selection_on_chrome_press(&mut self, cx: &mut Context<Self>) {
        if !self.recycle_bin.is_open() {
            self.dismiss_result_selection(cx);
        }
    }

    /// Clears the query text and the scope or filter attached to it, then returns focus to the
    /// search box. Both the clear affordance and Escape land here, so focus never stays on the
    /// clear button as it unmounts.
    pub(crate) fn clear_query_and_scope(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.recycle_bin.is_open() {
            self.clear_bin_filter(window, cx);
            return;
        }
        self.update_current_nav_state_text();
        self.query.search_input.clear();
        self.query.scope_drive = None;
        self.query.active_filter = FilterCategory::All;
        self.record_nav_state();
        self.trigger_search(cx);
        self.query.search_focus.focus(window, cx);
        cx.notify();
    }

    pub fn escape_query(&mut self, _: &EscapeQuery, window: &mut Window, cx: &mut Context<Self>) {
        // While a dropdown is open the keystroke belongs to it: `Popover` gives its surface focus
        // and handles Escape→Cancel in the bubble phase, where actions stop propagation by default,
        // so this handler is not reached. If focus has not moved into the popover yet, closing the
        // group here still keeps the query intact. No dismissal flag survives this dispatch: the
        // next Escape must clear the query even though the menu consumed the one that closed it.
        if self.open_group.take().is_some() {
            self.clear_group_menu();
            self.query.search_focus.focus(window, cx);
            cx.notify();
            return;
        }
        // A transient status is the topmost dismissible layer; Escape closes it
        // before it reaches a location or the window itself.
        if self.toast.is_some() {
            self.dismiss_toast(cx);
            return;
        }
        // The drive visualization only hides the query band; its scope is still
        // set, so the generic escape path would run a hidden search and a second
        // Escape would hide the window mid-browse. Escape clears the Treemap
        // selection and leaves the query untouched.
        if self.drive_view.is_some() {
            if let Some(ref mut st) = self.treemap_state {
                // A drag is the innermost state Escape can end. Selection is
                // untouched; the next Escape clears it.
                if st.is_panning() {
                    st.end_pan();
                    cx.notify();
                    return;
                }
                st.clear_selection();
            }
            cx.notify();
            return;
        }
        // The bin is a location, not an empty query: Escape first clears an
        // in-place bin search, then leaves for the query active before the bin.
        if self.recycle_bin.is_open() {
            if !self.query.search_input.text.trim().is_empty() {
                self.clear_bin_filter(window, cx);
                return;
            }
            self.return_from_recycle_bin(window, cx);
            return;
        }
        // A row context menu owns Escape itself: `PopupMenu` binds it to `Cancel`, focuses the menu
        // when it opens, and an action stops propagation, so this handler is not reached while one
        // is up. Tracking the row here as well left a stale flag after a menu dismissed itself
        // (click outside, or an item), and the next Escape was swallowed doing nothing. The row the
        // menu acted on stays selected in the table, which is the only state that outlives it.
        // A scope the sidebar set is part of the query: clearing only the text would leave the
        // window searching a drive or a file type the person just tried to dismiss.
        if self.has_query() {
            self.clear_query_and_scope(window, cx);
            return;
        }
        // Nothing in scope means the user is done with this window: hide it to the tray.
        crate::app::hide_main_window();
    }

    pub fn show_group(&mut self, group: MenuGroup, window: &mut Window, cx: &mut Context<Self>) {
        if self.open_group == Some(group) {
            return;
        }
        // Opening a window-level menu is leaving the Treemap; its own context
        // menus keep the selection, but the top menu bar does not.
        if let Some(state) = self.treemap_state.as_mut() {
            state.clear_selection();
        }
        self.open_group = Some(group);
        self.build_group_menu(group, window, cx);
        cx.notify();
    }

    pub fn toggle_group(&mut self, group: MenuGroup, window: &mut Window, cx: &mut Context<Self>) {
        if self.open_group == Some(group) {
            self.close_group();
            self.query.search_focus.focus(window, cx);
            cx.notify();
        } else {
            self.show_group(group, window, cx);
        }
    }

    pub fn set_group_open(
        &mut self,
        group: MenuGroup,
        is_open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if is_open {
            self.show_group(group, window, cx);
        } else {
            if self.open_group == Some(group) {
                self.open_group = None;
            }
            self.clear_group_menu();
            // A pointer dismissal still has to leave the keyboard somewhere it can work. The query
            // box is the window's default focus target, and unlike the trigger it is always a valid
            // Tab stop after the menu is gone.
            self.query.search_focus.focus(window, cx);
            cx.notify();
        }
    }

    pub fn close_group(&mut self) {
        self.open_group = None;
        self.clear_group_menu();
    }

    fn clear_group_menu(&mut self) {
        self.group_menu_dismiss = None;
        self.group_menu = None;
    }

    fn build_group_menu(&mut self, group: MenuGroup, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity();
        let this = &*self;
        let mut items = Vec::new();
        for (section_ix, section) in group_spec(group).sections.iter().enumerate() {
            let mut entries = menu_entries(this, &view, section, window, cx);
            if let Some(first) = entries.first_mut() {
                first.separates = section_ix > 0;
            }
            items.extend(entries);
        }
        let action_context = self.root_focus.clone();
        let menu = PopupMenu::build(window, cx, move |menu, _window, _cx| {
            view::build_menu(menu, &items, action_context.clone(), view.clone())
        });
        let subscription = cx.subscribe_in(
            &menu,
            window,
            move |this, _menu, _event: &DismissEvent, window, cx| {
                this.open_group = None;
                this.group_menu_dismiss = None;
                this.group_menu = None;
                this.query.search_focus.focus(window, cx);
                cx.notify();
            },
        );
        // `PopupMenu` owns Up/Down/Enter/Escape once its focus handle is live. Focus it as it is
        // built, before the anchored popup paints; deferring a frame let the window focus stay on
        // the search box, so the menu opened but never received a selection key.
        menu.focus_handle(cx).focus(window, cx);
        self.group_menu = Some(menu);
        self.group_menu_dismiss = Some(subscription);
    }

    pub fn open_file_menu(
        &mut self,
        _: &OpenFileMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_group(MenuGroup::File, window, cx);
    }

    pub fn open_edit_menu(
        &mut self,
        _: &OpenEditMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_group(MenuGroup::Edit, window, cx);
    }

    pub fn open_view_menu(
        &mut self,
        _: &OpenViewMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_group(MenuGroup::View, window, cx);
    }

    pub fn open_search_menu(
        &mut self,
        _: &OpenSearchMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_group(MenuGroup::Search, window, cx);
    }

    pub fn open_tools_menu(
        &mut self,
        _: &OpenToolsMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_group(MenuGroup::Tools, window, cx);
    }

    pub fn open_help_menu(
        &mut self,
        _: &OpenHelpMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_group(MenuGroup::Help, window, cx);
    }

    fn command_label(&self, command: GroupCommand) -> String {
        let index = self.index_menu_state();
        match command {
            // The content-search row names the action it will perform, so the row never reads as
            // a status line the user cannot act on.
            GroupCommand::ToggleContentSearch => self.content_search_toggle_label(),
            // The details row reports what the content index holds, in the user's language.
            GroupCommand::ContentSearchDetails => self.content_search_details_label(),
            // The two facts the popup carries beside that state.
            GroupCommand::ContentSearchUsage => self.content_search_usage_label(),
            GroupCommand::ContentSearchUpdated => self.content_search_updated_label(),
            // "所有文件" needs the background service. Where the service is missing the row is
            // disabled, and it says why here: the menu has no tooltip channel, so the visible
            // label is the only place the reason can reach the reader.
            GroupCommand::SetSearchScope(scope) if index.scope_choice_is_unavailable(scope) => self
                .i18n
                .get_text("menu_search_scope_all_files_unavailable"),
            GroupCommand::DeleteSelected if self.recycle_bin.is_open() => {
                self.i18n.get_text("ctx_delete_permanently")
            }
            GroupCommand::SortBy(column) if self.recycle_bin.is_open() => {
                self.i18n.get_text(column.recycle_bin_label_key())
            }
            GroupCommand::CopyPaths if self.recycle_bin.is_open() => {
                self.i18n.get_text("ctx_copy_original_location")
            }
            _ => self.i18n.get_text(command.label_key()),
        }
    }

    /// What the window knows about the service and the content index right now. The Tools menu
    /// and the result band's empty state both read this one value, so the rows the menu offers and
    /// the reason the band gives cannot drift apart.
    pub(crate) fn index_menu_state(&self) -> IndexMenuState<'_> {
        IndexMenuState {
            scope: self.service.search_scope,
            service_installed: self.service.service_installed,
            service_connected: self.service.is_service_connected,
            switching_scope: self.service.is_switching_scope,
            content_loading: self.service.content_status_loading,
            content_failed: self.service.content_status_error.is_some(),
            content_action_pending: self.service.content_action_pending,
            local_mode: self.service.local_mode,
            content_state: &self.service.content_status.state,
        }
    }

    /// The single content-search switch. Its label names what clicking it will do, so the row is
    /// an action rather than a status line the user cannot act on (UI-10).
    fn content_search_toggle_label(&self) -> String {
        let enabled = self.index_menu_state().content_index_is_on();
        self.i18n.get_text(if enabled {
            "menu_content_search_disable"
        } else {
            "menu_content_search_enable"
        })
    }

    /// What the content index currently holds, as one line the user can read. A state that could
    /// not be read says exactly that; it never falls back to "off", which would claim a fact
    /// nobody knows.
    fn content_search_details_label(&self) -> String {
        if !self.service.is_service_connected {
            return self.i18n.get_text("menu_content_search_unavailable");
        }
        if self.service.content_status_loading {
            return self.i18n.get_text("menu_content_search_loading");
        }
        if let Some(reason) = &self.service.content_status_error {
            return self.reason_label(
                "menu_content_search_unavailable",
                "menu_content_search_unavailable_reason",
                reason,
            );
        }
        match &self.service.content_status.state {
            ContentIndexState::Disabled => self.i18n.get_text("menu_content_search_off"),
            ContentIndexState::Backfilling { processed, total } => {
                let mut args = FluentArgs::new();
                args.set("processed", *processed);
                args.set("total", *total);
                self.i18n
                    .get_text_with_args("menu_content_search_backfilling", &args)
            }
            ContentIndexState::Ready => self.i18n.get_text("menu_content_search_ready"),
            ContentIndexState::Error(reason) => self.reason_label(
                "menu_content_search_failed",
                "menu_content_search_failed_reason",
                reason,
            ),
        }
    }

    /// Appends a localized reason to a status label, keeping the row short enough to stay one
    /// menu line. The service's message is diagnostic text, so it is shown as-is rather than
    /// translated; only the label around it is localized.
    fn reason_label(&self, plain_key: &str, key_with_reason: &str, reason: &str) -> String {
        let reason = short_reason(reason);
        if reason.is_empty() {
            return self.i18n.get_text(plain_key);
        }
        let mut args = FluentArgs::new();
        args.set("reason", reason);
        self.i18n.get_text_with_args(key_with_reason, &args)
    }

    /// How much space the content index takes, as one line the user can read. A size nobody has
    /// reported says exactly that; it never claims an empty index.
    fn content_search_usage_label(&self) -> String {
        if !self.service.is_service_connected || self.service.content_status_error.is_some() {
            return self.i18n.get_text("menu_content_search_usage_unknown");
        }
        let mut args = FluentArgs::new();
        args.set(
            "size",
            format_size_bytes(self.service.content_status.index_bytes),
        );
        self.i18n
            .get_text_with_args("menu_content_search_usage", &args)
    }

    /// When the content index was last committed. Before the first commit there is nothing to
    /// report, and the row says that instead of showing a zero timestamp.
    fn content_search_updated_label(&self) -> String {
        let committed_at = self.service.content_status.last_updated_ts;
        if !self.service.is_service_connected
            || self.service.content_status_error.is_some()
            || committed_at == 0
        {
            return self.i18n.get_text("menu_content_search_never_updated");
        }
        let mut args = FluentArgs::new();
        args.set("time", format_timestamp(committed_at));
        self.i18n
            .get_text_with_args("menu_content_search_updated", &args)
    }

    /// Rows that would describe an action the current state cannot take are omitted rather than
    /// shown disabled row after row. In particular a failed index offers one retry, not a second
    /// "enable" entry that would quietly mean the same request.
    pub fn command_is_hidden(&self, command: GroupCommand) -> bool {
        let index = self.index_menu_state();
        // The content switch and its details only mean something where a content index exists.
        // In the readable-files scope the user indexer has none, so those rows are absent rather
        // than promises the window cannot keep (UI-10). The popup that holds them goes with
        // them: a trigger left behind would open an empty window.
        let content_row = matches!(
            command,
            GroupCommand::ToggleContentSearch
                | GroupCommand::ContentSearchDetailsMenu
                | GroupCommand::ContentSearchDetails
                | GroupCommand::ContentSearchUsage
                | GroupCommand::ContentSearchUpdated
                | GroupCommand::RetryContentSearch
                | GroupCommand::ClearContentSearch
        );
        if content_row && index.content_rows_are_hidden() {
            return true;
        }
        match command {
            // A retry only exists for an index that failed, and a clear only for one that holds
            // something. Both are shown when they apply and absent otherwise, so the popup never
            // lists an action that would do nothing.
            GroupCommand::RetryContentSearch => !matches!(
                self.service.content_status.state,
                ContentIndexState::Error(_)
            ),
            GroupCommand::ClearContentSearch => !matches!(
                self.service.content_status.state,
                ContentIndexState::Ready | ContentIndexState::Error(_)
            ),
            // The bin's own commands exist only where the bin is open; the container's
            // read commands that assume a live filesystem path are hidden there.
            GroupCommand::RestoreSelected | GroupCommand::EmptyRecycleBin => {
                !self.recycle_bin.is_open()
            }
            GroupCommand::RevealLocation | GroupCommand::ShowProperties => {
                self.recycle_bin.is_open()
            }
            // The bin filter is name/original-location substring matching. Whole
            // word, path matching and regex have no effect there, so their rows
            // are absent instead of staying checkable no-ops. Case sensitivity is
            // honored by the filter and remains available.
            GroupCommand::ToggleWholeWord
            | GroupCommand::ToggleMatchPath
            | GroupCommand::ToggleRegex => self.recycle_bin.is_open(),

            _ => false,
        }
    }

    pub fn command_is_checked(&self, command: GroupCommand) -> bool {
        let index = self.index_menu_state();
        match command {
            GroupCommand::ToggleCase => self.query.case_sensitive,
            GroupCommand::ToggleWholeWord => self.query.whole_word,
            GroupCommand::ToggleMatchPath => self.query.match_path,
            GroupCommand::ToggleRegex => self.query.use_regex,
            GroupCommand::SortBy(column) => self.results.table.sort_column == column,
            GroupCommand::ReverseOrder => !self.results.table.sort_asc,
            GroupCommand::SetTheme(mode) => self.settings.theme == mode,
            GroupCommand::SetLanguage(code) => self.settings.language == code,
            GroupCommand::SetZoom(preset) => ZoomPreset::nearest(self.zoom_level) == preset,
            GroupCommand::ToggleAutostart => self.settings.autostart,
            GroupCommand::SetSearchScope(scope) => self.service.search_scope.as_str() == scope,
            // A backfill in progress has its own progress line; the checkmark appears only once
            // the index is ready, so the mark never claims a finished index (UI-10).
            GroupCommand::ToggleContentSearch => index.content_switch_is_checked(),
            _ => false,
        }
    }

    pub fn command_is_disabled(&self, command: GroupCommand) -> bool {
        let index = self.index_menu_state();
        match command {
            GroupCommand::OpenSelected
            | GroupCommand::RevealLocation
            | GroupCommand::ShowProperties => self.target_row_ix().is_none(),
            GroupCommand::DeleteSelected | GroupCommand::RestoreSelected => {
                if self.recycle_bin.is_open() {
                    self.recycle_bin.is_loading()
                        || self.explicit_bin_selection_indices().is_empty()
                } else {
                    self.delete_in_flight || self.selection_indices().is_empty()
                }
            }
            GroupCommand::EmptyRecycleBin => {
                !self.recycle_bin.is_open()
                    || self.recycle_bin.is_loading()
                    || self.recycle_bin.all().is_empty()
            }
            GroupCommand::CopyPaths | GroupCommand::CopyNames => {
                self.selection_indices().is_empty()
            }
            // A scope that is already in force is not a choice the user can make again, and a
            // switch that is running must not be started a second time. "所有文件" needs the
            // background service, so it is out of reach where the service is not installed.
            GroupCommand::SetSearchScope(scope) => index.scope_choice_is_disabled(scope),
            // A scope switch moves the window between two indexers. The content rows belong to
            // the one the window is leaving, so they wait until the switch has settled.
            GroupCommand::ToggleContentSearch => index.content_switch_is_disabled(),
            GroupCommand::ContentSearchDetails
            | GroupCommand::ContentSearchUsage
            | GroupCommand::ContentSearchUpdated => index.content_facts_are_disabled(),
            GroupCommand::RetryContentSearch | GroupCommand::ClearContentSearch => {
                index.content_switch_is_disabled()
            }
            GroupCommand::ClearQuery => !self.can_clear_query(),
            _ => false,
        }
    }

    /// Clearing a search needs something the person can see narrowing the view. The Treemap hides
    /// the query band and gives Escape to its selection, so the Edit row would act on hidden state.
    /// A bin filter and the main query are the two visible cases.
    fn can_clear_query(&self) -> bool {
        if self.drive_view.is_some() {
            return false;
        }
        if self.recycle_bin.is_open() {
            return !self.query.search_input.text.trim().is_empty();
        }
        self.has_query()
    }

    pub fn run_group_command(
        &mut self,
        command: GroupCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            GroupCommand::OpenSelected => self.open_selected(&OpenSelected, window, cx),
            GroupCommand::RevealLocation => {
                self.reveal_selected_location(&RevealSelectedLocation, window, cx)
            }
            GroupCommand::CopyPaths => self.copy_full_paths(&CopyFullPaths, window, cx),
            GroupCommand::CopyNames => self.copy_file_names(&CopyFileNames, window, cx),
            GroupCommand::UndoQueryText => self.undo_query_text(&UndoQueryText, window, cx),
            GroupCommand::RedoQueryText => self.redo_query_text(&RedoQueryText, window, cx),
            GroupCommand::CutQueryText => self.cut_query_text(&CutQueryText, window, cx),
            GroupCommand::PasteQueryText => self.paste_query_text(&PasteQueryText, window, cx),
            GroupCommand::ShowProperties => self.show_properties(&ShowProperties, window, cx),
            GroupCommand::DeleteSelected => self.delete_selected(&DeleteSelected, window, cx),
            GroupCommand::RestoreSelected => self.restore_selected(&RestoreSelected, window, cx),
            GroupCommand::EmptyRecycleBin => self.empty_bin(window, cx),
            GroupCommand::SelectAllQueryText => {
                self.select_all_query_text(&SelectAllQueryText, window, cx)
            }
            GroupCommand::ClearQuery => {
                // A command names one action. Running the Escape cascade here would dismiss an
                // unrelated toast/menu first and leave the query untouched.
                self.close_group();
                self.clear_query_and_scope(window, cx);
            }
            GroupCommand::ToggleCase => {
                self.toggle_case_sensitive(&ToggleCaseSensitive, window, cx)
            }
            GroupCommand::ToggleWholeWord => self.toggle_whole_word(&ToggleWholeWord, window, cx),
            GroupCommand::ToggleMatchPath => self.toggle_match_path(&ToggleMatchPath, window, cx),
            GroupCommand::ToggleRegex => self.toggle_regex(&ToggleRegex, window, cx),
            GroupCommand::SortBy(column) => self.toggle_table_sort(column, cx),
            GroupCommand::ReverseOrder => self.toggle_sort_direction(cx),
            GroupCommand::SetTheme(mode) => self.set_theme_mode(mode, cx),
            GroupCommand::SetZoom(preset) => self.set_zoom(preset.value(), cx),
            GroupCommand::Heading(_) | GroupCommand::Submenu(_, _) => {}
            GroupCommand::SetLanguage(code) => self.set_language(code, cx),
            GroupCommand::ToggleAutostart => {
                let enabled = !self.settings.autostart;
                self.set_autostart(enabled, cx);
            }
            GroupCommand::SetSearchScope(scope) => {
                self.close_group();
                self.switch_search_scope(SearchScope::from_stored(scope), cx);
            }
            GroupCommand::ToggleContentSearch => {
                // The label beside this row promises what the click does, so both read the same
                // fact about the index.
                let enabled = !self.index_menu_state().content_index_is_on();
                self.set_content_index_enabled(enabled, cx);
            }
            GroupCommand::ContentSearchDetailsMenu => {}
            GroupCommand::ContentSearchDetails => {}
            GroupCommand::ContentSearchUsage => {}
            GroupCommand::ContentSearchUpdated => {}
            GroupCommand::RetryContentSearch => self.set_content_index_enabled(true, cx),
            GroupCommand::ClearContentSearch => {
                self.close_group();
                let title = self.i18n.get_text("index_clear");
                let description = self.i18n.get_text("index_clear_description");
                let ok = self.i18n.get_text("index_clear_confirm_action");
                let cancel = self.i18n.get_text("index_clear_cancel");
                let view = cx.entity();
                window.open_alert_dialog(cx, move |alert, window, _| {
                    style_confirmation_alert(alert, window, ok.clone(), cancel.clone())
                        .title(
                            div()
                                .font(theme::serif_font(FontWeight::SEMIBOLD))
                                .child(title.clone()),
                        )
                        .description(description.clone())
                        .confirm()
                        .on_ok({
                            let view = view.clone();
                            move |_, _, cx| {
                                view.update(cx, |this, cx| this.clear_content_index(cx));
                                true
                            }
                        })
                });
            }
            GroupCommand::About => {
                self.close_group();
                let title = format!("Cairn {}", env!("CARGO_PKG_VERSION"));
                let message = self.i18n.get_text("menu_about_body");
                self.show_info_toast(title, message, cx);
            }
        }
    }

    fn toggle_flag(&mut self, flag: ToggleFlag, cx: &mut Context<Self>) {
        if self.recycle_bin.is_open() && flag != ToggleFlag::Case {
            return;
        }
        match flag {
            ToggleFlag::Case => self.query.case_sensitive = !self.query.case_sensitive,
            ToggleFlag::WholeWord => self.query.whole_word = !self.query.whole_word,
            ToggleFlag::MatchPath => self.query.match_path = !self.query.match_path,
            ToggleFlag::Regex => self.query.use_regex = !self.query.use_regex,
        }
        self.trigger_search(cx);
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
        {
            return;
        }
        // Leaving the visualization cancels its progressive scan; otherwise the
        // hidden view keeps a worker and its partial tree alive behind the table.
        if self.drive_view.take().is_some() {
            self.treemap_state = None;
        }
        // The bin's current nav entry already holds the query that preceded it;
        // reading the search box here would overwrite it with the cleared label.
        let leaving_bin = self.recycle_bin.is_open();
        if !leaving_bin {
            self.update_current_nav_state_text();
        } else {
            // The bin filter is not a normal query. Leaving the location must not carry it into
            // the category's index search or its navigation-history entry.
            self.query.search_input.clear();
        }
        self.recycle_bin.leave();
        self.query.active_filter = category;
        self.record_nav_state();
        self.results.rows_scroll.set_offset(Point::default());
        self.results.table.scroll_top = 0.0;
        self.trigger_search(cx);
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
    pub fn set_theme_mode(&mut self, mode: &str, cx: &mut Context<Self>) {
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

    pub fn set_language(&mut self, language: &str, cx: &mut Context<Self>) {
        if self.settings.language == language {
            return;
        }
        self.settings.language = language.to_string();
        self.i18n
            .set_language(&crate::shell::settings::effective_language(language));
        self.save_settings();
        self.repaint_shell_and_overlays(cx);
    }

    pub fn set_autostart(&mut self, enable: bool, cx: &mut Context<Self>) {
        if self.settings.autostart == enable {
            return;
        }
        self.settings.autostart = enable;
        self.save_settings();
        self.repaint_shell_and_overlays(cx);
    }

    pub fn repaint_shell_and_overlays(&self, cx: &mut Context<Self>) {
        cx.notify();
        cx.refresh_windows();
    }

    pub fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        let zoom = theme::set_zoom(zoom, cx);
        if zoom == self.zoom_level {
            return;
        }
        self.zoom_level = zoom;
        self.settings.zoom_level = zoom;
        self.save_settings();
        cx.notify();
    }

    pub fn select_all_query_text(
        &mut self,
        _: &SelectAllQueryText,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.drive_view.is_some() {
            return;
        }
        self.query.search_input.select_all();
        cx.notify();
    }

    pub fn open_selected(&mut self, _: &OpenSelected, _w: &mut Window, cx: &mut Context<Self>) {
        // Inside the Recycle Bin view Enter opens the deleted item straight
        // from the bin payload, exactly like opening it in Explorer.
        if self.recycle_bin.is_open() {
            if let Some(item) = self.target_bin_item() {
                let _ = open_file(&item.fs_path);
            }
            cx.notify();
            return;
        }
        if self.drive_view.is_some() {
            if let Some(ref st) = self.treemap_state {
                if let Some(node) = st.selected_node().cloned() {
                    if node.is_aggregate() {
                        if let Some(parent) = cairn_core::treemap::aggregate_parent_path(&node.path)
                            .map(str::to_string)
                        {
                            self.treemap_drill_down(&parent, cx);
                        }
                    } else if node.item.is_dir {
                        self.treemap_drill_down(&node.path, cx);
                    } else {
                        let _ = open_file(&node.path);
                    }
                }
            }
            cx.notify();
            return;
        }
        if let Some(path) = self.target_full_path() {
            let _ = open_file(&path);
        }
        cx.notify();
    }

    pub fn reveal_selected_location(
        &mut self,
        _: &RevealSelectedLocation,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A binned item has no live original path to reveal; its row hides this
        // command. The guard keeps a direct key dispatch from opening the bin's
        // internal payload directory.
        if self.recycle_bin.is_open() {
            return;
        }
        if self.drive_view.is_some() {
            let path = self
                .treemap_state
                .as_ref()
                .and_then(|state| state.selected_real_path())
                .map(str::to_string);
            if let Some(path) = path {
                let _ = open_folder_and_select(&path);
            }
            cx.notify();
            return;
        }
        let paths = self.collect_selected_paths();
        let targets = crate::shell::reveal::reveal_targets(&paths);
        if targets.is_empty() {
            if let Some(path) = self.target_full_path() {
                let _ = open_folder_and_select(&path);
            }
        } else {
            for path in targets {
                let _ = open_folder_and_select(&path);
            }
        }
        cx.notify();
    }

    pub fn copy_full_paths(&mut self, _: &CopyFullPaths, _w: &mut Window, cx: &mut Context<Self>) {
        let paths = self.collect_selected_paths();
        copy_lines(&paths);
        cx.notify();
    }

    /// Opens the Recycle Bin view: the shell reports the bin's contents and
    /// the table renders them. Any query or drive switch leaves the view.
    pub fn open_recycle_bin(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        // Leaving the visualization cancels its progressive scan; the bin is a
        // location, so a hidden Treemap worker must not keep walking a drive.
        if self.drive_view.take().is_some() {
            self.treemap_state = None;
        }
        self.close_group();
        // Keep the query the person left behind in the current history entry
        // before the bin replaces the search box with a location.
        self.update_current_nav_state_text();
        // Invalidate any in-flight query: its answer belongs to the table and
        // must not replace the listing that is about to be shown.
        self.query.query_id += 1;
        self.query.is_searching = false;
        self.query.is_fetching_more = false;
        self.query.active_session_id = None;
        // The bin is a location, not a search scope. The search box starts
        // empty and filters this listing in place; it must not run an index
        // query that would replace the location.
        if !self.query.search_input.text.is_empty() {
            self.query.search_input.clear();
        }
        self.query.scope_drive = None;
        self.query.active_filter = FilterCategory::All;
        self.results.rows_scroll.set_offset(Point::default());
        self.results.table.scroll_top = 0.0;
        // `begin_recycle_bin_load` opens the view and resets every part of the bin's state, so
        // the location starts from one clean listing instead of a half-cleared one.
        self.begin_recycle_bin_load(cx);
        cx.notify();
    }

    /// The Delete command: moves a table/Treemap selection to the Recycle Bin
    /// through the native file pipeline. Inside the bin the same key means a
    /// permanent delete, confirmed by the same project dialog before the shell
    /// runs without its own prompt.
    pub fn delete_selected(
        &mut self,
        _: &DeleteSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(items) = self.selected_bin_items() {
            self.open_bin_delete_confirmation(items, window, cx);
            return;
        }
        let paths = if self.drive_view.is_some() {
            self.treemap_state
                .as_ref()
                .and_then(|state| state.selected_real_path())
                .map(str::to_string)
                .into_iter()
                .collect()
        } else {
            self.collect_selected_paths()
        };
        self.confirm_delete_paths(paths, window, cx);
    }

    /// Asks before deleting paths. The shell confirmation is suppressed for
    /// this routine action so the question uses Cairn's own dialog; the shell
    /// still owns progress and error UI.
    pub fn confirm_delete_paths(
        &mut self,
        paths: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_delete_confirmation(paths, window, cx);
    }

    /// Asks before permanently deleting Recycle Bin items. The copy names the
    /// irreversible result, then the shell runs with its own confirmation
    /// suppressed; progress and error UI still belong to Windows.
    fn open_bin_delete_confirmation(
        &mut self,
        items: Vec<std::sync::Arc<RecycleBinItem>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if items.is_empty() {
            return;
        }
        self.close_group();
        let title = if items.len() == 1 {
            let mut args = FluentArgs::new();
            args.set("name", items[0].name.as_str());
            self.i18n
                .get_text_with_args("recycle_bin_delete_confirm_title", &args)
        } else {
            let mut args = FluentArgs::new();
            args.set("count", items.len() as i64);
            self.i18n
                .get_text_with_args("recycle_bin_delete_confirm_title_many", &args)
        };
        let description_key = if items.len() == 1 {
            "recycle_bin_delete_confirm_description"
        } else {
            "recycle_bin_delete_confirm_description_many"
        };
        let description = self.i18n.get_text(description_key);
        let ok = self.i18n.get_text("recycle_bin_delete_confirm_action");
        let cancel = self.i18n.get_text("delete_confirm_cancel");
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, window, _| {
            style_confirmation_alert(alert, window, ok.clone(), cancel.clone())
                .title(
                    div()
                        .font(theme::serif_font(FontWeight::SEMIBOLD))
                        .child(title.clone()),
                )
                .description(description.clone())
                .confirm()
                .on_ok({
                    let view = view.clone();
                    let items = items.clone();
                    move |_, _, cx| {
                        view.update(cx, |this, cx| {
                            this.delete_bin_items_permanently(items.clone(), cx)
                        });
                        true
                    }
                })
        });
    }

    /// Runs the confirmed permanent delete on the dedicated COM apartment and
    /// installs the listing that follows it.
    fn delete_bin_items_permanently(
        &mut self,
        items: Vec<std::sync::Arc<RecycleBinItem>>,
        cx: &mut Context<Self>,
    ) {
        if items.is_empty() {
            return;
        }
        let names: Vec<String> = items.iter().map(|item| item.name.clone()).collect();
        let requested: Vec<(String, String)> = items
            .iter()
            .map(|item| (item.identity_key(), item.name.clone()))
            .collect();
        self.spawn_bin_write(
            "recycle_bin_delete_failed",
            names,
            false,
            move |owner| {
                let outcome = delete_permanently_from_recycle_bin(&items, owner);
                let listing = crate::shell::os::list_recycle_bin();
                let outcome = reconcile_bin_items(outcome, &listing, &requested);
                (outcome, listing)
            },
            cx,
        );
    }

    fn open_delete_confirmation(
        &mut self,
        paths: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        self.close_group();
        let title = if paths.len() == 1 {
            let name = std::path::Path::new(&paths[0])
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(paths[0].as_str());
            let mut args = FluentArgs::new();
            args.set("name", name);
            self.i18n.get_text_with_args("delete_confirm_title", &args)
        } else {
            let mut args = FluentArgs::new();
            args.set("count", paths.len() as i64);
            self.i18n
                .get_text_with_args("delete_confirm_title_many", &args)
        };
        let description_key = if paths.len() == 1 {
            "delete_confirm_description"
        } else {
            "delete_confirm_description_many"
        };
        let description = self.i18n.get_text(description_key);
        let ok = self.i18n.get_text("delete_confirm_action");
        let cancel = self.i18n.get_text("delete_confirm_cancel");
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, window, _| {
            style_confirmation_alert(alert, window, ok.clone(), cancel.clone())
                .title(
                    div()
                        .font(theme::serif_font(FontWeight::SEMIBOLD))
                        .child(title.clone()),
                )
                .description(description.clone())
                .confirm()
                .on_ok({
                    let view = view.clone();
                    let paths = paths.clone();
                    move |_, _, cx| {
                        view.update(cx, |this, cx| this.delete_paths(paths.clone(), cx));
                        true
                    }
                })
        });
    }

    /// Runs one bin write on a dedicated COM apartment, then installs the listing that follows it.
    /// The UI thread is only a coordinator: the native progress/confirmation UI stays owned by the
    /// app window, and the generation gate drops a result after the bin has been left.
    fn spawn_bin_write<F>(
        &mut self,
        title_key: &'static str,
        fallback_names: Vec<String>,
        reset_scroll: bool,
        task: F,
        cx: &mut Context<Self>,
    ) where
        F: FnOnce(
                isize,
            ) -> (
                Result<(), BinOperationError>,
                Result<Vec<RecycleBinItem>, String>,
            ) + Send
            + 'static,
    {
        let owner = crate::shell::os::active_window_handle();
        let generation = self.recycle_bin.generation();
        self.sync_bin_selection_keys();
        let anchor = self.explicit_bin_selection_indices().first().copied();
        self.recycle_bin.set_loading(true);
        cx.notify();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let _ = tx.send(task(owner));
        });
        cx.spawn(async move |this, cx| {
            let Ok((outcome, listing)) = rx.await else {
                let _ = this.update(cx, |this, cx| {
                    this.recycle_bin.set_loading(false);
                    cx.notify();
                });
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.finish_recycle_bin_load(generation, listing, reset_scroll, anchor, cx);
                if let Err(failure) = outcome {
                    if !failure.is_cancelled() {
                        this.notify_bin_operation_error(title_key, &fallback_names, &failure, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Puts the selected bin items back through the shell's native restore
    /// pipeline; the list is re-read so a conflict or a missing original
    /// directory cannot leave a row claiming a state the shell does not hold.
    pub fn restore_selected(
        &mut self,
        _: &RestoreSelected,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(items) = self.selected_bin_items() else {
            return;
        };
        if items.is_empty() {
            return;
        }
        let names: Vec<String> = items.iter().map(|item| item.name.clone()).collect();
        let requested: Vec<(String, String)> = items
            .iter()
            .map(|item| (item.identity_key(), item.name.clone()))
            .collect();
        self.spawn_bin_write(
            "recycle_bin_restore_failed",
            names,
            false,
            move |owner| {
                let outcome = restore_from_recycle_bin(&items, owner);
                let listing = crate::shell::os::list_recycle_bin();
                let outcome = reconcile_bin_items(outcome, &listing, &requested);
                (outcome, listing)
            },
            cx,
        );
    }

    /// Confirms emptying the Recycle Bin through Cairn's unified dialog style before
    /// dispatching the operation to the shell.
    pub fn empty_bin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.recycle_bin.is_open()
            || self.recycle_bin.is_loading()
            || self.recycle_bin.all().is_empty()
        {
            return;
        }
        self.close_group();
        let title = self.i18n.get_text("recycle_bin_empty_confirm_title");
        let description = self.i18n.get_text("recycle_bin_empty_confirm_description");
        let ok = self.i18n.get_text("recycle_bin_empty_confirm_action");
        let cancel = self.i18n.get_text("delete_confirm_cancel");
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, window, _| {
            style_confirmation_alert(alert, window, ok.clone(), cancel.clone())
                .title(
                    div()
                        .font(theme::serif_font(FontWeight::SEMIBOLD))
                        .child(title.clone()),
                )
                .description(description.clone())
                .confirm()
                .on_ok({
                    let view = view.clone();
                    move |_, _, cx| {
                        view.update(cx, |this, cx| this.execute_empty_bin(cx));
                        true
                    }
                })
        });
    }

    /// Empties the bin through the shell's own command, then re-reads it. The loading flag is
    /// set before the background operation starts, so it is also the in-flight guard: a second
    /// invocation before the first listing lands must not start a second shell empty.
    pub fn execute_empty_bin(&mut self, cx: &mut Context<Self>) {
        if !self.recycle_bin.is_open() || self.recycle_bin.is_loading() {
            return;
        }
        self.spawn_bin_write(
            "recycle_bin_empty_failed",
            Vec::new(),
            true,
            |owner| {
                let outcome = empty_recycle_bin(owner);
                let listing = crate::shell::os::list_recycle_bin();
                let outcome = reconcile_empty_bin(outcome, &listing);
                (outcome, listing)
            },
            cx,
        );
    }

    /// Deletes the given paths to the Recycle Bin and reflects the result in
    /// whichever surface issued it. Shared by the Delete command and the
    /// Treemap context menu.
    pub fn delete_paths(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        if paths.is_empty() || self.delete_in_flight {
            return;
        }
        let names: Vec<String> = paths
            .iter()
            .map(|path| {
                std::path::Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(path)
                    .to_string()
            })
            .collect();
        let task_paths = paths.clone();
        let task_names = names.clone();
        let owner = crate::shell::os::active_window_handle();
        self.delete_in_flight = true;
        cx.notify();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            struct PendingItem {
                path: String,
                name: String,
            }

            let delete_result = delete_to_recycle_bin(&task_paths, owner);
            // Single pass over the filesystem to determine which paths still exist
            // and which were successfully deleted, avoiding redundant I/O calls.
            let (remaining_items, deleted_items): (Vec<PendingItem>, Vec<PendingItem>) = task_paths
                .into_iter()
                .zip(task_names)
                .map(|(path, name)| PendingItem { path, name })
                .partition(|item| std::path::Path::new(&item.path).exists());

            let deleted_paths: Vec<String> =
                deleted_items.into_iter().map(|item| item.path).collect();
            let remaining_names: Vec<String> =
                remaining_items.into_iter().map(|item| item.name).collect();

            let outcome = match delete_result {
                Ok(()) => {
                    // A shell batch can report success while an item was skipped (for example a
                    // locked file that the progress UI ignored). Reconcile the original paths so
                    // the table/Treemap is not updated for an item that still exists.
                    if remaining_names.is_empty() {
                        Ok(())
                    } else {
                        Err(BinOperationError::failed(
                            "some items could not be deleted",
                            remaining_names,
                        ))
                    }
                }
                Err(failure) => Err(failure),
            };
            let _ = tx.send((outcome, deleted_paths));
        });
        cx.spawn(async move |this, cx| {
            let received = rx.await;
            let _ = this.update(cx, |this, cx| {
                this.delete_in_flight = false;
                if let Ok((outcome, deleted_paths)) = received {
                    if let Err(failure) = &outcome {
                        if !failure.is_cancelled() {
                            this.notify_bin_operation_error(
                                "delete_failed_title",
                                &names,
                                failure,
                                cx,
                            );
                        }
                    }
                    if this.drive_view.is_some() {
                        if let Some(ref mut st) = this.treemap_state {
                            for path in &deleted_paths {
                                st.remove_item(path);
                            }
                        }
                    } else {
                        this.trigger_search(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn copy_file_names(&mut self, _: &CopyFileNames, _w: &mut Window, cx: &mut Context<Self>) {
        let names = self.collect_selected_names();
        copy_lines(&names);
        cx.notify();
    }

    pub fn copy_text_or_paths(
        &mut self,
        _: &CopyTextOrPaths,
        _w: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.selected_query_text() {
            Some(text) => copy_lines(&[text]),
            None => copy_lines(&self.collect_selected_paths()),
        }
        cx.notify();
    }

    pub fn paste_query_text(
        &mut self,
        _: &PasteQueryText,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.drive_view.is_some() {
            return;
        }
        if let Some(text) = crate::shell::os::get_clipboard_text() {
            let text = text.replace("\r\n", " ").replace('\n', " ");
            if !text.is_empty() {
                self.query
                    .search_input
                    .handle_replace_text_in_range(None, &text);
                self.trigger_search(cx);
                self.query.search_focus.focus(window, cx);
                cx.notify();
            }
        }
    }

    pub fn undo_query_text(
        &mut self,
        _: &UndoQueryText,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.drive_view.is_some() {
            return;
        }
        if self.query.search_input.undo() {
            self.trigger_search(cx);
            self.query.search_focus.focus(window, cx);
            cx.notify();
        }
    }

    pub fn redo_query_text(
        &mut self,
        _: &RedoQueryText,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.drive_view.is_some() {
            return;
        }
        if self.query.search_input.redo() {
            self.trigger_search(cx);
            self.query.search_focus.focus(window, cx);
            cx.notify();
        }
    }

    pub fn cut_query_text(
        &mut self,
        _: &CutQueryText,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.drive_view.is_some() {
            return;
        }
        if let Some(text) = self.selected_query_text() {
            let _ = crate::shell::os::set_clipboard_text(&text);
            self.query.search_input.delete_backward();
            self.trigger_search(cx);
            self.query.search_focus.focus(window, cx);
            cx.notify();
        }
    }

    pub fn selected_query_text(&self) -> Option<String> {
        let Range { start, end } = self.query.search_input.selection.clone();
        if start >= end || end > self.query.search_input.text.len() {
            return None;
        }
        Some(self.query.search_input.text[start..end].to_string())
    }

    pub fn target_row_ix(&self) -> Option<usize> {
        let ix = self.results.table.target_row()?;
        self.results.table.is_loaded(ix).then_some(ix)
    }

    pub fn target_full_path(&self) -> Option<String> {
        if self.drive_view.is_some() {
            if let Some(ref st) = self.treemap_state {
                if st.selected_node().is_some() {
                    return st.selected_real_path().map(str::to_string);
                }
                return Some(st.current_dir().to_string());
            }
        }
        let ix = self.target_row_ix()?;
        let row = self.results.table.row(ix)?;
        Some(join_row_path(&row.path, &row.name))
    }

    pub fn collect_selected_paths(&self) -> Vec<String> {
        if self.drive_view.is_some() {
            return self.target_full_path().into_iter().collect();
        }
        self.selection_indices()
            .iter()
            .filter_map(|ix| self.results.table.row(*ix))
            .map(|row| join_row_path(&row.path, &row.name))
            .collect()
    }

    pub fn collect_selected_names(&self) -> Vec<String> {
        if self.drive_view.is_some() {
            if let Some(ref st) = self.treemap_state {
                if st.selected_real_path().is_some() {
                    if let Some(node) = st.selected_node() {
                        return vec![node.item.name.clone()];
                    }
                }
            }
            return Vec::new();
        }
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

    pub fn show_selected_properties(&mut self, cx: &mut Context<Self>) {
        // The properties sheet of a bin payload describes the `$R` file, not
        // the item Explorer shows; the bin hides this command rather than
        // presenting a misleading name and location.
        if self.recycle_bin.is_open() {
            return;
        }
        if self.drive_view.is_some() {
            let path = self
                .treemap_state
                .as_ref()
                .and_then(|state| state.selected_real_path())
                .map(str::to_string);
            if let Some(path) = path {
                let _ = show_properties_dialog(&path);
            }
            cx.notify();
            return;
        }
        if let Some(path) = self.target_full_path() {
            let _ = show_properties_dialog(&path);
        }
        cx.notify();
    }

    pub fn show_properties(&mut self, _: &ShowProperties, _w: &mut Window, cx: &mut Context<Self>) {
        self.show_selected_properties(cx);
    }

    pub fn focus_row_for_context(&mut self, ix: usize) {
        if !self.results.table.selected_indices.contains(&ix) {
            self.results.table.select_row(ix, false);
        }
    }

    pub fn select_row_by_pointer(
        &mut self,
        ix: usize,
        add_to_selection: bool,
        extend_selection: bool,
        is_double_click: bool,
        row_height: Pixels,
        cx: &mut Context<Self>,
    ) {
        self.results.keyboard_navigation = false;
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
        if is_double_click {
            if self.recycle_bin.is_open() {
                if let Some(item) = self.target_bin_item() {
                    let _ = open_file(&item.fs_path);
                }
            } else if let Some(path) = self.target_full_path() {
                let _ = open_file(&path);
            }
        }
        self.ensure_visible_rows_loaded(row_height, cx);
        cx.notify();
    }

    pub fn handle_query_edit_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let shift = event.keystroke.modifiers.shift;
        let ctrl = event.keystroke.modifiers.control;

        if ctrl && !shift && key == "z" {
            if self.query.search_input.undo() {
                self.trigger_search(cx);
                cx.notify();
            }
            return;
        }
        if (ctrl && key == "y") || (ctrl && shift && key == "z") {
            if self.query.search_input.redo() {
                self.trigger_search(cx);
                cx.notify();
            }
            return;
        }
        if (ctrl && key == "x") || (shift && key == "delete") {
            if let Some(text) = self.selected_query_text() {
                let _ = crate::shell::os::set_clipboard_text(&text);
                self.query.search_input.delete_backward();
                self.trigger_search(cx);
                cx.notify();
            }
            return;
        }
        if (ctrl && key == "c") || (ctrl && key == "insert") {
            if let Some(text) = self.selected_query_text() {
                let _ = crate::shell::os::set_clipboard_text(&text);
            }
            return;
        }
        if (ctrl && key == "v") || (shift && key == "insert") {
            if let Some(text) = crate::shell::os::get_clipboard_text() {
                let text = text.replace("\r\n", " ").replace('\n', " ");
                if !text.is_empty() {
                    self.query
                        .search_input
                        .handle_replace_text_in_range(None, &text);
                    self.trigger_search(cx);
                    cx.notify();
                }
            }
            return;
        }
        if ctrl && key == "backspace" {
            self.query.search_input.delete_word_backward();
            self.trigger_search(cx);
            cx.notify();
            return;
        }
        if ctrl && key == "delete" {
            self.query.search_input.delete_word_forward();
            self.trigger_search(cx);
            cx.notify();
            return;
        }
        if ctrl && key == "left" {
            self.query.search_input.move_cursor_word_left(shift);
            cx.notify();
            return;
        }
        if ctrl && key == "right" {
            self.query.search_input.move_cursor_word_right(shift);
            cx.notify();
            return;
        }

        let mut edited = true;
        match key {
            "backspace" => self.query.search_input.delete_backward(),
            "delete" => self.query.search_input.delete_forward(),
            "left" => self.query.search_input.move_cursor_left(shift),
            "right" => self.query.search_input.move_cursor_right(shift),
            "home" => self.query.search_input.move_cursor_home(shift),
            "end" => self.query.search_input.move_cursor_end(shift),
            _ => edited = false,
        }
        if !edited {
            return;
        }
        self.trigger_search(cx);
        cx.notify();
    }
}

#[cfg(test)]
mod confirmation_tests {
    use super::{confirmation_top_offset_for_height, reconcile_bin_items, reconcile_empty_bin};
    use crate::app::layout;
    use crate::shell::i18n::I18nManager;
    use crate::shell::os::{BinOperationError, RecycleBinItem};
    use crate::shell::theme;
    use fluent_bundle::FluentArgs;
    use gpui_kit::gpui::px;

    fn item(name: &str) -> RecycleBinItem {
        RecycleBinItem {
            name: name.to_string(),
            name_lower: name.to_lowercase(),
            extension_lower: crate::shell::os::extract_extension_lower(name),
            original_dir: r"C:\work".to_string(),
            original_dir_lower: r"c:\work".to_string(),
            fs_path: format!(r"C:\$Recycle.Bin\SID\$R{name}"),
            deleted_ts: 1,
            size: 1,
            is_dir: false,
        }
    }

    #[test]
    fn reconciliation_blames_only_items_still_in_the_listing() {
        let requested = vec![
            (item("a.txt").identity_key(), "a.txt".to_string()),
            (item("b.txt").identity_key(), "b.txt".to_string()),
        ];
        let listing = Ok(vec![item("a.txt")]);
        let outcome = reconcile_bin_items(Ok(()), &listing, &requested);
        match outcome {
            Err(BinOperationError::Failed { items, .. }) => {
                assert_eq!(items, vec!["a.txt".to_string()]);
            }
            other => panic!("expected the live item to be blamed, got {other:?}"),
        }
        assert_eq!(
            reconcile_bin_items(Ok(()), &Ok(Vec::new()), &requested),
            Ok(())
        );
        assert_eq!(
            reconcile_bin_items(Err(BinOperationError::Cancelled), &listing, &requested),
            Err(BinOperationError::Cancelled)
        );
        assert!(reconcile_empty_bin(Ok(()), &listing).is_err());
        assert_eq!(reconcile_empty_bin(Ok(()), &Ok(Vec::new())), Ok(()));
    }

    #[test]
    fn confirmation_copy_uses_the_action_verb_and_names_the_recovery_path() {
        for locale in ["zh-CN", "en-US"] {
            let i18n = I18nManager::new(locale);
            let action = i18n.get_text("delete_confirm_action");
            let cancel = i18n.get_text("delete_confirm_cancel");
            let description = i18n.get_text("delete_confirm_description");
            let description_many = i18n.get_text("delete_confirm_description_many");
            let mut name = FluentArgs::new();
            name.set("name", "report.txt");
            let title = i18n.get_text_with_args("delete_confirm_title", &name);
            let mut count = FluentArgs::new();
            count.set("count", 2i64);
            let title_many = i18n.get_text_with_args("delete_confirm_title_many", &count);

            if locale.starts_with("zh") {
                assert_eq!(action, "删除");
                assert_eq!(cancel, "取消");
                assert!(title.contains("删除"));
                assert!(title_many.contains("2"));
                assert!(description.contains("回收站"));
                assert!(description_many.contains("这些项目"));
                assert!(!description.contains("保留"));
            } else {
                assert_eq!(action, "Delete");
                assert_eq!(cancel, "Cancel");
                assert!(title.contains("Delete"));
                assert!(title_many.contains("2"));
                assert!(description.contains("Recycle Bin"));
                assert!(description_many.contains("These items"));
            }
            for key in [
                "delete_confirm_description",
                "delete_confirm_description_many",
                "delete_confirm_action",
                "delete_confirm_cancel",
                "recycle_bin_delete_confirm_description",
                "recycle_bin_delete_confirm_description_many",
                "recycle_bin_delete_confirm_action",
                "recycle_bin_empty_confirm_title",
                "recycle_bin_empty_confirm_description",
                "recycle_bin_empty_confirm_action",
                "index_clear",
                "index_clear_description",
                "index_clear_confirm_action",
                "index_clear_cancel",
            ] {
                let resolved = i18n
                    .try_text(key)
                    .unwrap_or_else(|| panic!("{key} missing in {locale}"));
                assert_ne!(resolved, key, "{key} fell back to raw key in {locale}");
                assert!(
                    !resolved.contains("Are you sure") && !resolved.contains("确定要"),
                    "{key} in {locale} repeats the title question in the body: {resolved}"
                );
            }
        }
    }

    #[test]
    fn confirmation_geometry_is_a_short_centered_surface() {
        assert!((layout::CONFIRMATION_WIDTH * theme::BASE_FONT_PX - 384.0).abs() < 0.001);
        assert!(
            (layout::CONFIRMATION_ESTIMATED_HEIGHT * theme::BASE_FONT_PX - 150.0).abs() < 0.001
        );
        assert_eq!(
            confirmation_top_offset_for_height(px(800.), px(150.), px(24.)),
            px(325.)
        );
        assert_eq!(
            confirmation_top_offset_for_height(px(350.), px(150.), px(24.)),
            px(100.)
        );
        assert_eq!(
            confirmation_top_offset_for_height(px(100.), px(150.), px(24.)),
            px(24.)
        );
    }
}
