// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Menu construction, command predicates and dispatch: the rows and groups the window menus are
//! built from, the label and state each command reports, the dispatch table behind them, and the
//! table keyboard selection, scrolling and focus actions.

use crate::app::{CairnApp, join_row_path, layout};
use crate::search::ResultSetChange;
use crate::search::filters::FilterCategory;
use crate::shell::command_groups::{GroupCommand, MenuGroup, ZoomPreset, group_spec};
use crate::shell::commands::*;
use crate::shell::dialogs;
use crate::shell::location::ActiveLocation;
use crate::shell::menu_rules::{IndexMenuState, SubsystemMenuAccess};
use crate::shell::os::{
    open_file, open_folder_and_select, open_url, set_clipboard_text, show_properties_dialog,
};
use crate::shell::reveal::{MAX_REVEAL_TARGETS, reveal_targets};
use crate::shell::status_bar::format_count;
use crate::table::model::{SELECT_ALL_LIMIT, format_timestamp};
use crate::view;
use cairn_core::format_size_bytes;
use cairn_protocol::status::{ContentIndexState, SourceKind};
use fluent_bundle::FluentArgs;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::gpui::*;
use std::rc::Rc;

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

fn build_submenu_popup(
    app: &CairnApp,
    view: &Entity<CairnApp>,
    child_entries: Vec<view::MenuEntry>,
    window: &mut Window,
    cx: &mut Context<'_, CairnApp>,
) -> Option<Entity<PopupMenu>> {
    if child_entries.is_empty() {
        return None;
    }
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
}

fn rescan_submenu_entries(
    app: &CairnApp,
    view: &Entity<CairnApp>,
    window: &mut Window,
    cx: &mut Context<'_, CairnApp>,
) -> Vec<view::MenuEntry> {
    let mut entries = menu_entries_from_sections(
        app,
        view,
        GroupCommand::RescanMenu.submenu_sections(),
        window,
        cx,
    );
    let disabled = app.index_menu_state().service_action_is_disabled();
    for (ix, source) in app.service.sources.iter().enumerate() {
        let source_id = source.source_id;
        let label = match source.kind {
            SourceKind::NtfsVolume => source
                .root_path
                .chars()
                .next()
                .filter(char::is_ascii_alphabetic)
                .map_or_else(
                    || source.root_path.clone(),
                    |ch| {
                        let mut args = FluentArgs::new();
                        args.set("drive", ch.to_ascii_uppercase().to_string());
                        app.i18n.get_text_with_args("drive_label", &args)
                    },
                ),
            SourceKind::Folder => source.root_path.clone(),
        };
        let toast_label = label.clone();
        entries.push(view::MenuEntry {
            label: label.into(),
            checked: false,
            radio: false,
            disabled,
            action: None,
            command: None,
            on_select: Some(Rc::new(move |this, _, cx| {
                this.close_group();
                this.rescan_source(source_id, toast_label.clone(), cx);
            })),
            hint: None,
            submenu: None,
            trailing: view::MenuTrailing::None,
            separates: ix == 0,
        });
    }
    entries
}

/// Builds the rows of one sectioned menu or submenu, omitting empty trailing/leading separators.
fn menu_entries_from_sections(
    app: &CairnApp,
    view: &Entity<CairnApp>,
    sections: &'static [&'static [GroupCommand]],
    window: &mut Window,
    cx: &mut Context<'_, CairnApp>,
) -> Vec<view::MenuEntry> {
    let mut items = Vec::new();
    let mut had_visible_section = false;
    for section in sections {
        let mut entries = menu_entries(app, view, section, window, cx);
        if entries.is_empty() {
            continue;
        }
        if had_visible_section {
            entries[0].separates = true;
        }
        had_visible_section = true;
        items.extend(entries);
    }
    items
}

/// Builds the rows of one menu. A submenu's own popup is created here because its items carry the
/// same actions and checked state as the top-level rows; the caller can then hand the whole list to
/// one `PopupMenu::build`.
fn menu_entries(
    app: &CairnApp,
    view: &Entity<CairnApp>,
    commands: &'static [GroupCommand],
    window: &mut Window,
    cx: &mut Context<'_, CairnApp>,
) -> Vec<view::MenuEntry> {
    commands
        .iter()
        .filter(|command| !app.command_is_hidden(**command))
        .map(|command| {
            let command = *command;
            let submenu = if command == GroupCommand::RescanMenu {
                let child_entries = rescan_submenu_entries(app, view, window, cx);
                build_submenu_popup(app, view, child_entries, window, cx)
            } else {
                let sections = command.submenu_sections();
                if sections.is_empty() {
                    None
                } else {
                    let child_entries = menu_entries_from_sections(app, view, sections, window, cx);
                    build_submenu_popup(app, view, child_entries, window, cx)
                }
            };
            let has_submenu = submenu.is_some();
            let (label, trailing) = if command == GroupCommand::CheckForUpdates {
                use crate::shell::update::UpdateState;
                match &app.update_state {
                    UpdateState::Idle => (app.command_label(command), view::MenuTrailing::None),
                    UpdateState::Checking => {
                        (app.command_label(command), view::MenuTrailing::Loading)
                    }
                    UpdateState::Available(_) => (
                        app.i18n.get_text("menu_download_and_install"),
                        view::MenuTrailing::Download,
                    ),
                    UpdateState::Downloading(_) => (
                        app.i18n.get_text("menu_download_and_install"),
                        view::MenuTrailing::Loading,
                    ),
                }
            } else {
                (app.command_label(command), view::MenuTrailing::None)
            };
            view::MenuEntry {
                label: label.into(),
                checked: !has_submenu && app.command_is_checked(command),
                radio: !has_submenu && command.is_radio_choice(),
                disabled: app.command_is_disabled(command),
                action: command.action(),
                // A submenu row owns a nested popup instead of dispatching a command.
                command: if has_submenu { None } else { Some(command) },
                on_select: None,
                hint: command.hint_action(),
                submenu,
                trailing,
                separates: false,
            }
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupHoverAction {
    Noop,
    RebuildOpenGroup(MenuGroup),
    SwitchGroup(MenuGroup),
}

pub(crate) fn decide_group_hover(
    open_group: Option<MenuGroup>,
    hovered: MenuGroup,
    menu_entered: bool,
    keyboard_driven: bool,
) -> GroupHoverAction {
    if keyboard_driven {
        return GroupHoverAction::Noop;
    }
    match open_group {
        Some(open) if open == hovered => {
            if menu_entered {
                GroupHoverAction::RebuildOpenGroup(hovered)
            } else {
                GroupHoverAction::Noop
            }
        }
        Some(_) => GroupHoverAction::SwitchGroup(hovered),
        None => GroupHoverAction::Noop,
    }
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
    fn focus_table_after_arrow(&self, window: &mut Window, cx: &mut Context<'_, Self>) {
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
        if self.toast.is_some() {
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

    pub fn show_group(
        &mut self,
        group: MenuGroup,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self
            .open_group_menu
            .as_ref()
            .is_some_and(|menu| menu.group == group)
        {
            return;
        }
        self.build_group_menu(group, window, cx);
        cx.notify();
    }

    pub fn toggle_group(
        &mut self,
        group: MenuGroup,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self
            .open_group_menu
            .as_ref()
            .is_some_and(|menu| menu.group == group)
        {
            self.close_group();
            self.query.search_focus.focus(window, cx);
            cx.notify();
        } else {
            self.show_group(group, window, cx);
        }
    }

    /// Pointer arrival on a menu-bar trigger while a group is open.
    pub fn hover_group(
        &mut self,
        group: MenuGroup,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let open_group = self.open_group_menu.as_ref().map(|menu| menu.group);
        let menu_entered = self
            .open_group_menu
            .as_ref()
            .is_some_and(|menu| menu.entered);
        match decide_group_hover(
            open_group,
            group,
            menu_entered,
            window.last_input_was_keyboard(),
        ) {
            GroupHoverAction::Noop => {}
            GroupHoverAction::RebuildOpenGroup(group) => self.build_group_menu(group, window, cx),
            GroupHoverAction::SwitchGroup(group) => self.show_group(group, window, cx),
        }
    }

    pub fn mark_group_menu_entered(&mut self) {
        if let Some(menu) = self.open_group_menu.as_mut() {
            menu.entered = true;
        }
    }

    pub fn set_group_open(
        &mut self,
        group: MenuGroup,
        is_open: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if is_open {
            self.show_group(group, window, cx);
        } else {
            if self
                .open_group_menu
                .as_ref()
                .is_some_and(|menu| menu.group == group)
            {
                self.close_group();
            }
            self.query.search_focus.focus(window, cx);
            cx.notify();
        }
    }

    pub fn close_group(&mut self) {
        self.open_group_menu = None;
    }

    fn build_group_menu(
        &mut self,
        group: MenuGroup,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if group == MenuGroup::Index
            && self.service.connection.is_connected()
            && self.service.sources.is_empty()
        {
            self.refresh_sources(cx);
        }
        let view = cx.entity();
        let items = menu_entries_from_sections(self, &view, group_spec(group).sections, window, cx);
        let action_context = self.root_focus.clone();
        let menu = PopupMenu::build(window, cx, move |menu, _window, _cx| {
            view::build_menu(menu, &items, action_context.clone(), view.clone())
        });
        let subscription = cx.subscribe_in(
            &menu,
            window,
            move |this, _menu, _event: &DismissEvent, window, cx| {
                this.open_group_menu = None;
                if this.rename.draft.is_none() {
                    this.query.search_focus.focus(window, cx);
                }
                cx.notify();
            },
        );
        menu.focus_handle(cx).focus(window, cx);
        self.open_group_menu = Some(crate::app::OpenGroupMenu {
            group,
            popup: menu,
            _dismiss: subscription,
            entered: false,
        });
    }

    pub fn open_file_menu(
        &mut self,
        _: &OpenFileMenu,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.toggle_group(MenuGroup::File, window, cx);
    }

    pub fn open_edit_menu(
        &mut self,
        _: &OpenEditMenu,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.toggle_group(MenuGroup::Edit, window, cx);
    }

    pub fn open_view_menu(
        &mut self,
        _: &OpenViewMenu,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.toggle_group(MenuGroup::View, window, cx);
    }

    pub fn open_search_menu(
        &mut self,
        _: &OpenSearchMenu,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.toggle_group(MenuGroup::Search, window, cx);
    }

    pub fn open_index_menu(
        &mut self,
        _: &OpenIndexMenu,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.toggle_group(MenuGroup::Index, window, cx);
    }

    pub fn open_tools_menu(
        &mut self,
        _: &OpenToolsMenu,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.toggle_group(MenuGroup::Tools, window, cx);
    }

    pub fn open_help_menu(
        &mut self,
        _: &OpenHelpMenu,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
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
            GroupCommand::PropertyIndexStatus => self.property_index_status_label(),
            GroupCommand::PropertyIndexUsage => self.property_index_usage_label(),
            _ => self
                .i18n
                .get_text(self.location_profile().command_label_key(command)),
        }
    }

    /// What the window knows about the service and the content index right now. The Tools menu
    /// and the result band's empty state both read this one value, so the rows the menu offers and
    /// the reason the band gives cannot drift apart.
    pub(crate) fn index_menu_state(&self) -> IndexMenuState<'_> {
        IndexMenuState {
            service_connected: self.service.connection.is_connected(),
            service_control_pending: self.service.connection.is_control_pending(),
            content: SubsystemMenuAccess::from_subsystem(&self.service.content),
            content_action_failed: self.service.content.action_error.is_some(),
            content_state: &self.service.content.value.state,
            property: SubsystemMenuAccess::from_subsystem(&self.service.property),
            property_state: &self.service.property.value,
            journal: SubsystemMenuAccess::from_subsystem(&self.service.journal),
            journal_status: &self.service.journal.value,
        }
    }

    fn format_backfill_label(&self, key: &str, processed: u64, total: u64) -> String {
        let mut args = FluentArgs::new();
        args.set("processed", processed);
        args.set("total", total);
        self.i18n.get_text_with_args(key, &args)
    }

    fn format_usage_label<S>(
        &self,
        subsystem: &crate::service::SubsystemState<S>,
        bytes: u64,
        unknown_key: &str,
        known_key: &str,
    ) -> String {
        if !self.service.connection.is_connected() || subsystem.load_error().is_some() {
            return self.i18n.get_text(unknown_key);
        }
        let mut args = FluentArgs::new();
        args.set("size", format_size_bytes(bytes));
        self.i18n.get_text_with_args(known_key, &args)
    }

    /// What the content index currently holds, as one line the user can read.
    fn content_search_details_label(&self) -> String {
        if !self.service.connection.is_connected() {
            return self.i18n.get_text("menu_content_search_unavailable");
        }
        if self.service.content.action_error.is_some() {
            return self.i18n.get_text("menu_content_search_failed");
        }
        if self.service.content.is_loading() {
            return self.i18n.get_text("menu_content_search_loading");
        }
        if self.service.content.load_error().is_some() {
            return self.i18n.get_text("menu_content_search_unavailable");
        }
        match &self.service.content.value.state {
            ContentIndexState::Disabled => self.i18n.get_text("menu_content_search_off"),
            ContentIndexState::Backfilling { processed, total } => {
                self.format_backfill_label("menu_content_search_backfilling", *processed, *total)
            }
            ContentIndexState::Ready => self.i18n.get_text("menu_content_search_ready"),
            ContentIndexState::Error(_) => self.i18n.get_text("menu_content_search_failed"),
        }
    }

    fn content_search_usage_label(&self) -> String {
        self.format_usage_label(
            &self.service.content,
            self.service.content.value.index_bytes,
            "menu_content_search_usage_unknown",
            "menu_content_search_usage",
        )
    }

    /// When the content index was last committed.
    fn content_search_updated_label(&self) -> String {
        let committed_at = self.service.content.value.last_updated_ts;
        if !self.service.connection.is_connected()
            || self.service.content.load_error().is_some()
            || committed_at == 0
        {
            return self.i18n.get_text("menu_content_search_never_updated");
        }
        let mut args = FluentArgs::new();
        args.set("time", format_timestamp(committed_at));
        self.i18n
            .get_text_with_args("menu_content_search_updated", &args)
    }

    fn property_index_status_label(&self) -> String {
        if !self.service.connection.is_connected() || self.service.property.load_error().is_some() {
            return self.i18n.get_text("menu_property_index_unavailable");
        }
        if self.service.property.is_loading() {
            return self.i18n.get_text("menu_property_index_loading");
        }
        match &self.service.property.value.state {
            ContentIndexState::Disabled if !self.service.property.value.enabled => {
                self.i18n.get_text("menu_property_index_off")
            }
            ContentIndexState::Backfilling { processed, total } => {
                self.format_backfill_label("menu_property_index_backfilling", *processed, *total)
            }
            ContentIndexState::Ready | ContentIndexState::Disabled => {
                let mut args = FluentArgs::new();
                args.set(
                    "count",
                    format_count(self.service.property.value.indexed_records as usize),
                );
                self.i18n
                    .get_text_with_args("menu_property_index_ready", &args)
            }
            ContentIndexState::Error(_) => self.i18n.get_text("menu_property_index_failed"),
        }
    }

    fn property_index_usage_label(&self) -> String {
        self.format_usage_label(
            &self.service.property,
            self.service.property.value.side_table_bytes as u64,
            "menu_property_index_usage_unknown",
            "menu_property_index_usage",
        )
    }

    /// Rows that would describe an action the current state cannot take are omitted rather than
    /// shown disabled row after row. In particular a failed index offers one retry, not a second
    /// "enable" entry that would quietly mean the same request.
    pub fn command_is_hidden(&self, command: GroupCommand) -> bool {
        if self.location_profile().command_is_hidden(command) {
            return true;
        }
        match command {
            GroupCommand::RetryContentSearch => !matches!(
                self.service.content.value.state,
                ContentIndexState::Error(_)
            ),
            GroupCommand::ClearContentSearch => {
                let content = &self.service.content.value;
                let has_content = content.indexed_documents > 0 || content.index_bytes > 0;
                let failed = matches!(content.state, ContentIndexState::Error(_));
                matches!(content.state, ContentIndexState::Backfilling { .. })
                    || !(has_content || failed)
            }
            _ => false,
        }
    }

    pub fn command_is_checked(&self, command: GroupCommand) -> bool {
        if let Some(toggle) = command.query_toggle() {
            return self.query.is_toggle_checked(toggle);
        }
        let index = self.index_menu_state();
        match command {
            GroupCommand::ToggleExcludeHidden => self.settings.exclude_hidden,
            GroupCommand::ToggleExcludeSystem => self.settings.exclude_system,
            GroupCommand::SortBy(column) => self.results.table.primary_sort().0 == column,
            GroupCommand::ReverseOrder => !self.results.table.primary_sort().1,
            GroupCommand::ToggleColumn(col) => self.settings.columns.is_column_shown(col),
            GroupCommand::SetTheme(mode) => self.settings.theme == mode,
            GroupCommand::SetLanguage(code) => self.settings.language == code,
            GroupCommand::SetZoom(preset) => ZoomPreset::nearest(self.zoom_level) == preset,
            GroupCommand::ToggleAutostart => self.settings.autostart,
            // The checkmark means the switch is on, including while the index is still filling;
            // the details row carries progress and readiness.
            GroupCommand::ToggleContentSearch => index.content_index_is_on(),
            GroupCommand::TogglePropertyIndex => index.property_index_is_on(),
            _ => false,
        }
    }

    pub fn command_is_disabled(&self, command: GroupCommand) -> bool {
        let index = self.index_menu_state();
        match command {
            GroupCommand::OpenSelected
            | GroupCommand::RevealLocation
            | GroupCommand::ShowProperties => self.target_row_ix().is_none(),
            GroupCommand::RenameSelected => {
                self.rename.in_flight || self.rename_target_ix().is_none()
            }
            GroupCommand::DeleteSelected | GroupCommand::RestoreSelected => {
                match self.active_location() {
                    ActiveLocation::RecycleBin => {
                        self.recycle_bin.is_loading()
                            || self.explicit_bin_selection_indices().is_empty()
                    }
                    ActiveLocation::Index => {
                        self.delete_in_flight || self.selection_indices().is_empty()
                    }
                }
            }
            GroupCommand::RestoreAll | GroupCommand::EmptyRecycleBin => {
                !self.active_location().is_recycle_bin()
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
            GroupCommand::TogglePropertyIndex => index.property_switch_is_disabled(),
            GroupCommand::PropertyIndexStatus | GroupCommand::PropertyIndexUsage => {
                index.property_facts_are_disabled()
            }
            GroupCommand::RescanMenu | GroupCommand::RescanAll => {
                index.service_action_is_disabled()
            }
            GroupCommand::ClearQuery => !self.can_clear_query(),
            GroupCommand::ToggleColumn(col) => col.is_locked(),
            GroupCommand::CheckForUpdates => self.update_state.is_busy(),
            _ => false,
        }
    }

    /// Clearing a search needs something the person can see narrowing the view.
    /// A bin filter and the main query are the visible cases.
    fn can_clear_query(&self) -> bool {
        match self.active_location() {
            ActiveLocation::RecycleBin => !self.query.search_input.text.trim().is_empty(),
            ActiveLocation::Index => self.has_query(),
        }
    }

    pub fn run_group_command(
        &mut self,
        command: GroupCommand,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        // A command the keymap already owns runs through its Action, so the menu item and its
        // shortcut reach one implementation.
        if let Some(action) = command.action() {
            window.dispatch_action(action, cx);
            return;
        }
        if let Some(toggle) = command.query_toggle() {
            self.toggle_query_option(toggle, cx);
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
            GroupCommand::ToggleColumn(col) => self.toggle_table_column(col, cx),
            GroupCommand::SetTheme(mode) => self.set_theme_mode(mode, cx),
            GroupCommand::SetZoom(preset) => self.set_zoom(preset.value(), cx),
            GroupCommand::Submenu(_, _)
            | GroupCommand::RescanMenu
            | GroupCommand::ContentIndexMenu
            | GroupCommand::PropertyIndexMenu => {}
            GroupCommand::SetLanguage(code) => self.set_language(code, cx),
            GroupCommand::ToggleExcludeHidden => self.toggle_exclude_hidden(cx),
            GroupCommand::ToggleExcludeSystem => self.toggle_exclude_system(cx),
            GroupCommand::ToggleAutostart => {
                let enabled = !self.settings.autostart;
                self.set_autostart(enabled, cx);
            }
            GroupCommand::OpenIndexJournal => {
                self.close_group();
                self.open_index_journal_dialog(window, cx);
            }
            GroupCommand::ManageFolderSources => {
                self.close_group();
                self.open_folder_sources_dialog(window, cx);
            }
            GroupCommand::RescanAll => {
                self.close_group();
                self.rescan_all(cx);
            }
            GroupCommand::ToggleContentSearch => {
                // The stable row label names the feature; the checkmark carries whether it is
                // on, and the click always flips that same fact.
                let enabled = !self.index_menu_state().content_index_is_on();
                self.set_content_index_enabled(enabled, cx);
            }
            GroupCommand::ContentSearchDetails => {}
            GroupCommand::ContentSearchUsage => {}
            GroupCommand::ContentSearchUpdated => {}
            GroupCommand::RetryContentSearch => self.set_content_index_enabled(true, cx),
            GroupCommand::TogglePropertyIndex => {
                let enabled = !self.index_menu_state().property_index_is_on();
                self.set_property_index_enabled(enabled, cx);
            }
            GroupCommand::PropertyIndexStatus | GroupCommand::PropertyIndexUsage => {}
            GroupCommand::ClearContentSearch => {
                self.close_group();
                let title = self.i18n.get_text("index_clear");
                let description = self.i18n.get_text("index_clear_description");
                let ok = self.i18n.get_text("index_clear_confirm_action");
                let cancel = self.i18n.get_text("index_clear_cancel");
                let view = cx.entity();
                window.open_alert_dialog(cx, move |alert, window, app| {
                    dialogs::confirmation_alert(alert, window, app, ok.clone(), cancel.clone())
                        .child(dialogs::body(title.clone(), description.clone(), window))
                        .confirm()
                        .on_ok({
                            let view = view.clone();
                            move |_, _, cx| {
                                view.update(cx, Self::clear_content_index);
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
                if let Err(err) = open_url(crate::shell::update::REPOSITORY_URL) {
                    tracing::warn!("Failed to open repository URL: {err}");
                }
            }
            // Every variant below owns an Action or query toggle and dispatched above. Listing
            // them keeps this match exhaustive, so a new command must be classified cleanly.
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
            | GroupCommand::RenameSelected
            | GroupCommand::RestoreSelected
            | GroupCommand::RestoreAll
            | GroupCommand::SelectAll
            | GroupCommand::ToggleCase
            | GroupCommand::ToggleWholeWord
            | GroupCommand::ToggleMatchPath
            | GroupCommand::ToggleRegex
            | GroupCommand::TogglePinyin
            | GroupCommand::ToggleIgnorePunctuation
            | GroupCommand::ToggleIgnoreWhitespace
            | GroupCommand::ToggleIgnoreDiacritics => {
                unreachable!("action- or toggle-bearing commands dispatch above")
            }
        }
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

    fn open_target_row(&self) {
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
            for path in targets {
                let _ = open_folder_and_select(&path);
            }
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

#[cfg(test)]
mod confirmation_tests {
    use crate::service::SubsystemState;
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
        let mut state: SubsystemState<()> = SubsystemState::default();
        assert!(!state.has_failure());
        state.fail_load("status");
        assert!(state.has_failure());
        state.finish_load(());
        assert!(!state.has_failure());
        state.finish_action_err("action");
        assert!(state.has_failure());
        state.finish_action_ok();
        assert!(!state.has_failure());
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

#[cfg(test)]
mod visibility_tests {
    use super::{
        CairnApp, ContentIndexState, EscapeQuery, GroupCommand, ResultSetChange, SelectNextRow,
        menu_entries,
    };
    use gpui_kit::gpui::AppContext as _;

    #[test]
    fn clear_content_follows_content_or_failure_not_only_state() {
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
                    app.update(cx, |this, _cx| {
                        let mut status = cairn_protocol::status::ContentIndexStatus {
                            state: ContentIndexState::Ready,
                            indexed_documents: 0,
                            index_bytes: 0,
                            last_updated_ts: 0,
                        };
                        this.service.content.value = status.clone();
                        assert!(this.command_is_hidden(GroupCommand::ClearContentSearch));

                        status.state = ContentIndexState::Disabled;
                        status.indexed_documents = 10;
                        status.index_bytes = 0;
                        this.service.content.value = status.clone();
                        assert!(!this.command_is_hidden(GroupCommand::ClearContentSearch));

                        status.indexed_documents = 0;
                        status.index_bytes = 10;
                        this.service.content.value = status.clone();
                        assert!(!this.command_is_hidden(GroupCommand::ClearContentSearch));

                        status.state = ContentIndexState::Backfilling {
                            processed: 0,
                            total: 10,
                        };
                        this.service.content.value = status.clone();
                        assert!(this.command_is_hidden(GroupCommand::ClearContentSearch));

                        status.state = ContentIndexState::Error("failed".to_string());
                        status.indexed_documents = 0;
                        this.service.content.value = status;
                        assert!(!this.command_is_hidden(GroupCommand::ClearContentSearch));
                    });
                    cx.quit();
                    app
                },
            );
        });
    }

    #[test]
    fn local_listing_replaces_hidden_created_sort() {
        use crate::table::model::{SortColumn, SortDirection, SortSpec};

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
                    app.update(cx, |this, _cx| {
                        this.results.table.sort_spec =
                            SortSpec::single(SortColumn::Length, SortDirection::Descending);
                        this.normalize_local_listing_sort();
                        assert_eq!(
                            this.results.table.primary_sort(),
                            (SortColumn::DateModified, false)
                        );
                        assert_eq!(this.settings.columns.sort_spec, "modified-desc");
                    });
                    cx.quit();
                    app
                },
            );
        });
    }

    #[test]
    fn check_updates_menu_row_tracks_all_four_update_states() {
        use crate::shell::update::{ReleaseInfo, UpdateState};
        use crate::view::MenuTrailing;

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
                        let view = cx.entity();
                        let release = ReleaseInfo {
                            version: "9.9.9".to_string(),
                            setup_url: "https://example.invalid/CairnSetup.exe".to_string(),
                            sums_url: "https://example.invalid/SHA256SUMS.txt".to_string(),
                        };

                        this.update_state = UpdateState::Idle;
                        let entries =
                            menu_entries(this, &view, &[GroupCommand::CheckForUpdates], window, cx);
                        assert_eq!(
                            entries[0].label.as_ref(),
                            this.i18n.get_text("menu_check_updates")
                        );
                        assert_eq!(entries[0].trailing, MenuTrailing::None);
                        assert!(!entries[0].disabled);

                        this.update_state = UpdateState::Checking;
                        let entries =
                            menu_entries(this, &view, &[GroupCommand::CheckForUpdates], window, cx);
                        assert_eq!(
                            entries[0].label.as_ref(),
                            this.i18n.get_text("menu_check_updates")
                        );
                        assert_eq!(entries[0].trailing, MenuTrailing::Loading);
                        assert!(entries[0].disabled);

                        this.update_state = UpdateState::Available(release.clone());
                        let entries =
                            menu_entries(this, &view, &[GroupCommand::CheckForUpdates], window, cx);
                        assert_eq!(
                            entries[0].label.as_ref(),
                            this.i18n.get_text("menu_download_and_install")
                        );
                        assert_eq!(entries[0].trailing, MenuTrailing::Download);
                        assert!(!entries[0].disabled);

                        this.update_state = UpdateState::Downloading(release);
                        let entries =
                            menu_entries(this, &view, &[GroupCommand::CheckForUpdates], window, cx);
                        assert_eq!(
                            entries[0].label.as_ref(),
                            this.i18n.get_text("menu_download_and_install")
                        );
                        assert_eq!(entries[0].trailing, MenuTrailing::Loading);
                        assert!(entries[0].disabled);
                    });
                    cx.quit();
                    app
                },
            );
        });
    }

    #[test]
    fn escape_stops_only_a_narrowed_query() {
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
                        this.query.search_input.set_text("report");
                        this.request_result_set(ResultSetChange::InPlace, cx);
                        assert!(this.query.is_searching(), "the narrowed query is running");
                        this.escape_query(&EscapeQuery, window, cx);
                        assert!(!this.query.is_searching());
                        assert!(this.query.is_cancelled());

                        // The implicit all-files view is not a stoppable query: an
                        // empty box never leaves a stopped band behind. Clearing
                        // the text is an explicit trigger that clears the stop.
                        this.query.search_input.clear();
                        this.request_result_set(ResultSetChange::Replace, cx);
                        assert!(!this.query.is_cancelled());
                        this.escape_query(&EscapeQuery, window, cx);
                        assert!(this.query.is_searching());
                        assert!(!this.query.is_cancelled());
                    });

                    cx.quit();
                    app
                },
            );
        });
    }

    #[test]
    fn arrow_selection_moves_the_keyboard_into_the_table() {
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
                        this.results.table.set_rows(
                            vec![cairn_protocol::message::FileRowData {
                                frn: 0,
                                name: "a.txt".to_string(),
                                path: r"C:\work".to_string(),
                                size: 1,
                                modified_ts: 0,
                                created_ts: 0,
                                is_dir: false,
                                flags: 0,
                                name_highlights: Vec::new(),
                                path_highlights: Vec::new(),
                                snippets: Vec::new(),
                                properties: None,
                            }],
                            1,
                        );
                        this.query.search_focus.focus(window, cx);
                        this.select_next_row(&SelectNextRow, window, cx);
                        assert_eq!(this.results.table.focused_idx, Some(0));
                        assert!(this.results.results_focus.is_focused(window));
                    });

                    cx.quit();
                    app
                },
            );
        });
    }

    #[test]
    fn service_menu_commands_reflect_property_and_journal_state() {
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

                    app.update(cx, |this, _cx| {
                        this.service.connection.mark_connected();
                        this.service.property.value = cairn_protocol::status::PropertyIndexStatus {
                            state: ContentIndexState::Ready,
                            enabled: true,
                            indexed_records: 42,
                            side_table_bytes: 2048,
                            last_updated_ts: 100,
                        };
                        this.service.journal.value = cairn_protocol::status::IndexJournalStatus {
                            enabled: true,
                            entry_count: 7,
                            resident_bytes: 512,
                            max_entries: 1024,
                            max_bytes: 65536,
                            oldest_seq: 1,
                            newest_seq: 7,
                        };
                        assert!(this.command_is_checked(GroupCommand::TogglePropertyIndex));
                        assert!(!this.command_is_disabled(GroupCommand::RescanMenu));
                        assert!(!this.command_is_disabled(GroupCommand::RescanAll));
                        assert!(!this.command_is_disabled(GroupCommand::OpenIndexJournal));

                        let prop_label = this.command_label(GroupCommand::PropertyIndexStatus);
                        let prop_usage = this.command_label(GroupCommand::PropertyIndexUsage);
                        assert!(prop_label.contains("42"));
                        assert!(prop_usage.contains("2"));

                        this.service.connection.set_control_pending(true);
                        assert!(this.command_is_disabled(GroupCommand::RescanMenu));
                        assert!(this.command_is_disabled(GroupCommand::RescanAll));

                        this.service.connection.set_control_pending(false);
                        this.service.connection.mark_lost_running();
                        assert!(this.command_is_disabled(GroupCommand::TogglePropertyIndex));
                        assert!(this.command_is_disabled(GroupCommand::RescanMenu));
                        assert!(this.command_is_disabled(GroupCommand::RescanAll));
                        assert!(!this.command_is_disabled(GroupCommand::OpenIndexJournal));
                    });

                    cx.quit();
                    app
                },
            );
        });
    }
}

#[cfg(test)]
mod hover_group_tests {
    use super::{GroupHoverAction, decide_group_hover};
    use crate::shell::command_groups::MenuGroup;

    #[test]
    fn hover_decisions_cover_open_popup_switch_and_keyboard_states() {
        let cases = [
            (
                Some(MenuGroup::File),
                MenuGroup::File,
                false,
                false,
                GroupHoverAction::Noop,
            ),
            (
                Some(MenuGroup::File),
                MenuGroup::File,
                true,
                false,
                GroupHoverAction::RebuildOpenGroup(MenuGroup::File),
            ),
            (
                Some(MenuGroup::File),
                MenuGroup::View,
                false,
                false,
                GroupHoverAction::SwitchGroup(MenuGroup::View),
            ),
            (
                Some(MenuGroup::File),
                MenuGroup::File,
                true,
                true,
                GroupHoverAction::Noop,
            ),
            (None, MenuGroup::File, false, false, GroupHoverAction::Noop),
        ];
        for (open_group, hovered, inside_popup, keyboard_nav, expected) in cases {
            assert_eq!(
                decide_group_hover(open_group, hovered, inside_popup, keyboard_nav),
                expected
            );
        }
    }
}
