// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Menu construction, command predicates and dispatch: the rows and groups the window menus are
//! built from, the label and state each command reports, the dispatch table behind them, and the
//! table keyboard selection, scrolling and focus actions.

use crate::app::{join_row_path, layout, CairnApp};
use crate::search::filters::FilterCategory;
use crate::shell::command_groups::{group_spec, GroupCommand, MenuGroup, ZoomPreset};
use crate::shell::commands::*;
use crate::shell::dialogs;
use crate::shell::menu_rules::IndexMenuState;
use crate::shell::os::{
    open_file, open_folder_and_select, set_clipboard_text, show_properties_dialog,
};
use crate::shell::reveal::{reveal_targets, MAX_REVEAL_TARGETS};
use crate::shell::status_bar::format_count;
use crate::table::model::{format_timestamp, SELECT_ALL_LIMIT};
use crate::view;
use cairn_core::format_size_bytes;
use cairn_protocol::status::ContentIndexState;
use fluent_bundle::FluentArgs;
use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::dialog::AlertDialog;
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::gpui::*;

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
                loading: command == GroupCommand::CheckForUpdates && app.is_checking_update,
                separates: false,
            }
        })
        .collect()
}

/// Shared confirmation geometry: a short surface sized to its content, centered instead of parked
/// near the top, with the standard footer buttons. The labels travel with the variants so a caller
/// cannot replace the button configuration and lose the danger commit or the secondary cancel:
/// every confirmation on this helper destroys data.
pub(super) fn style_confirmation_alert(
    alert: AlertDialog,
    window: &mut Window,
    cx: &mut App,
    ok_text: impl Into<SharedString>,
    cancel_text: impl Into<SharedString>,
) -> AlertDialog {
    style_confirmation_alert_with_variant(
        alert,
        window,
        cx,
        ok_text,
        cancel_text,
        ButtonVariant::Danger,
    )
}

pub(super) fn style_confirmation_alert_with_variant(
    alert: AlertDialog,
    window: &mut Window,
    cx: &mut App,
    ok_text: impl Into<SharedString>,
    cancel_text: impl Into<SharedString>,
    variant: ButtonVariant,
) -> AlertDialog {
    // Both footer actions keep the same medium frame; the lighter neutral plate is painted
    // one pixel inside it so the pair reads as one optical size. Destructive actions pass Danger,
    // while non-destructive recoveries (like restoring all bin items) pass Primary.
    let surface = cairn_ui::theme::dialog_surface(cx.theme());
    let actions = dialogs::DialogActions::new(ok_text, variant).with_cancel(cancel_text);
    dialogs::alert_surface(alert, window, cx, surface, actions)
}

impl CairnApp {
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
        // The Treemap replaces the query band with the path field, which is its
        // address bar; the same desktop shortcut reaches it.
        if self.drive_view.is_some() {
            self.begin_treemap_path_edit(window, cx);
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
        self.clear_block_listing();
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
        // The address field is the innermost editing surface while it is open.
        if self.treemap_path.editing {
            self.cancel_treemap_path_edit(window, cx);
            return;
        }
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

    /// Pointer arrival on a menu-bar trigger while a group is open. The trigger lives outside the
    /// PopupMenu element, so the popup's hover-out never clears a submenu item when the pointer
    /// leaves for the bar: returning to the open trigger rebuilds its popup without a selection,
    /// and arriving on a different trigger switches the open menu to it. A keyboard-driven
    /// selection is left intact.
    pub fn hover_group(&mut self, group: MenuGroup, window: &mut Window, cx: &mut Context<Self>) {
        if window.last_input_was_keyboard() {
            return;
        }
        match self.open_group {
            Some(open) if open == group => self.build_group_menu(group, window, cx),
            Some(_) => self.show_group(group, window, cx),
            None => {}
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
        match command {
            // The details row reports what the content index holds, in the user's language.
            GroupCommand::ContentSearchDetails => self.content_search_details_label(),
            // The two facts the popup carries beside that state.
            GroupCommand::ContentSearchUsage => self.content_search_usage_label(),
            GroupCommand::ContentSearchUpdated => self.content_search_updated_label(),
            _ => self
                .i18n
                .get_text(command.label_key_in(self.recycle_bin.is_open())),
        }
    }

    /// What the window knows about the service and the content index right now. The Tools menu
    /// and the result band's empty state both read this one value, so the rows the menu offers and
    /// the reason the band gives cannot drift apart.
    pub(crate) fn index_menu_state(&self) -> IndexMenuState<'_> {
        IndexMenuState {
            service_connected: self.service.is_service_connected,
            content_loading: self.service.content_status_loading,
            content_failed: content_failed(
                self.service.content_status_error.as_deref(),
                self.service.content_action_error.as_deref(),
            ),
            content_action_failed: self.service.content_action_error.is_some(),
            content_action_pending: self.service.content_action_pending,
            content_state: &self.service.content_status.state,
        }
    }

    /// What the content index currently holds, as one line the user can read. A state that could
    /// not be read says exactly that; it never falls back to "off", which would claim a fact
    /// nobody knows.
    fn content_search_details_label(&self) -> String {
        if !self.service.is_service_connected {
            return self.i18n.get_text("menu_content_search_unavailable");
        }
        if self.service.content_action_error.is_some() {
            return self.i18n.get_text("menu_content_search_failed");
        }
        if self.service.content_status_loading {
            return self.i18n.get_text("menu_content_search_loading");
        }
        if self.service.content_status_error.is_some() {
            return self.i18n.get_text("menu_content_search_unavailable");
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
            ContentIndexState::Error(_) => self.i18n.get_text("menu_content_search_failed"),
        }
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
            GroupCommand::RestoreSelected
            | GroupCommand::RestoreAll
            | GroupCommand::EmptyRecycleBin => !self.recycle_bin.is_open(),
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
            // The checkmark means the switch is on, including while the index is still filling;
            // the details row carries progress and readiness.
            GroupCommand::ToggleContentSearch => index.content_index_is_on(),
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
            GroupCommand::RestoreAll | GroupCommand::EmptyRecycleBin => {
                !self.recycle_bin.is_open()
                    || self.recycle_bin.is_loading()
                    || self.recycle_bin.all().is_empty()
            }
            GroupCommand::CopyPaths | GroupCommand::CopyNames => {
                self.selection_indices().is_empty()
            }
            GroupCommand::ToggleContentSearch => index.content_switch_is_disabled(),
            GroupCommand::ContentSearchDetails
            | GroupCommand::ContentSearchUsage
            | GroupCommand::ContentSearchUpdated => index.content_facts_are_disabled(),
            GroupCommand::RetryContentSearch | GroupCommand::ClearContentSearch => {
                index.content_switch_is_disabled()
            }
            GroupCommand::ClearQuery => !self.can_clear_query(),
            GroupCommand::CheckForUpdates => self.is_checking_update,
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
        // A command the keymap already owns runs through its Action, so the menu item and its
        // shortcut reach one implementation.
        if let Some(action) = command.action() {
            window.dispatch_action(action, cx);
            return;
        }
        match command {
            GroupCommand::ClearQuery => {
                // A command names one action. Running the Escape cascade here would dismiss an
                // unrelated toast/menu first and leave the query untouched.
                self.close_group();
                self.clear_query_and_scope(window, cx);
            }
            GroupCommand::EmptyRecycleBin => self.empty_bin(window, cx),
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
            GroupCommand::ToggleContentSearch => {
                // The stable row label names the feature; the checkmark carries whether it is
                // on, and the click always flips that same fact.
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
                window.open_alert_dialog(cx, move |alert, window, app| {
                    style_confirmation_alert(alert, window, app, ok.clone(), cancel.clone())
                        .child(dialogs::body(title.clone(), description.clone(), window))
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
            GroupCommand::CheckForUpdates => {
                self.close_group();
                self.start_update_check(window, cx);
            }
            GroupCommand::About => {
                self.close_group();
                let title = format!("Cairn {}", env!("CARGO_PKG_VERSION"));
                let message = self.i18n.get_text("menu_about_body");
                self.show_info_toast(title, message, cx);
            }
            // Every variant below owns an Action and dispatched above. Listing them keeps this
            // match exhaustive, so a new command must be classified instead of failing at click.
            GroupCommand::OpenSelected
            | GroupCommand::RevealLocation
            | GroupCommand::CopyPaths
            | GroupCommand::CopyNames
            | GroupCommand::UndoQueryText
            | GroupCommand::RedoQueryText
            | GroupCommand::CutQueryText
            | GroupCommand::PasteQueryText
            | GroupCommand::ShowProperties
            | GroupCommand::DeleteSelected
            | GroupCommand::RestoreSelected
            | GroupCommand::RestoreAll
            | GroupCommand::SelectAll
            | GroupCommand::ToggleCase
            | GroupCommand::ToggleWholeWord
            | GroupCommand::ToggleMatchPath
            | GroupCommand::ToggleRegex => {
                unreachable!("action-bearing commands dispatch through their Action")
            }
        }
    }

    pub fn select_all(&mut self, _: &SelectAll, window: &mut Window, cx: &mut Context<Self>) {
        let path_input_focused = self.treemap_path.editing
            && self
                .treemap_path
                .input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window);
        if path_input_focused {
            let input = self.treemap_path.input.clone();
            input.update(cx, |input, cx| input.select_all(window, cx));
            return;
        }
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
        if self.drive_view.is_some() || self.results.table.total_rows() == 0 {
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

    pub fn open_selected(&mut self, _: &OpenSelected, window: &mut Window, cx: &mut Context<Self>) {
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
            self.treemap_open_selected(window, cx);
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
}

/// A content-index action failed or the last status read failed. Both make the menu row speak a
/// failure instead of a healthy state.
fn content_failed(status_error: Option<&str>, action_error: Option<&str>) -> bool {
    status_error.is_some() || action_error.is_some()
}

#[cfg(test)]
mod confirmation_tests {
    use super::content_failed;
    use crate::shell::i18n::I18nManager;
    use fluent_bundle::FluentArgs;
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
    fn a_content_action_failure_marks_the_menu_failed() {
        assert!(!content_failed(None, None));
        assert!(content_failed(Some("status"), None));
        assert!(content_failed(None, Some("action")));
        assert!(content_failed(Some("status"), Some("action")));
    }

    #[test]
    fn delete_and_recycle_bin_actions_have_no_redundant_ellipsis() {
        for (locale, zh) in [("zh-CN", true), ("en-US", false)] {
            let i18n = I18nManager::new(locale);
            let delete = i18n.get_text("ctx_delete");
            let perm_delete = i18n.get_text("ctx_delete_permanently");
            let empty_bin = i18n.get_text("ctx_empty_recycle_bin");
            let restore_all = i18n.get_text("ctx_restore_all");
            let clear_content = i18n.get_text("menu_content_search_clear");

            for (text, name) in [
                (&delete, "ctx_delete"),
                (&perm_delete, "ctx_delete_permanently"),
                (&empty_bin, "ctx_empty_recycle_bin"),
                (&restore_all, "ctx_restore_all"),
                (&clear_content, "menu_content_search_clear"),
            ] {
                assert!(
                    !text.ends_with('…') && !text.ends_with("..."),
                    "{name} in {locale} should not have ellipsis suffix: {text}"
                );
            }

            if zh {
                assert_eq!(delete, "删除");
                assert_eq!(perm_delete, "永久删除");
                assert_eq!(empty_bin, "清空回收站");
                assert_eq!(restore_all, "还原所有项目");
                assert_eq!(clear_content, "清空内容索引");
            } else {
                assert_eq!(delete, "Delete");
                assert_eq!(perm_delete, "Delete permanently");
                assert_eq!(empty_bin, "Empty Recycle Bin");
                assert_eq!(restore_all, "Restore all items");
                assert_eq!(clear_content, "Clear the content index");
            }
        }
    }

    #[test]
    fn restore_all_confirmation_dialog_strings_are_localized() {
        for locale in ["zh-CN", "en-US"] {
            let i18n = I18nManager::new(locale);
            for key in [
                "recycle_bin_restore_all_confirm_title",
                "recycle_bin_restore_all_confirm_description",
                "recycle_bin_restore_all_confirm_action",
            ] {
                let resolved = i18n
                    .try_text(key)
                    .unwrap_or_else(|| panic!("{key} missing in {locale}"));
                assert_ne!(resolved, key, "{key} fell back to raw key in {locale}");
            }
        }
    }
}
