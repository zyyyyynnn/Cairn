// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Menu construction, command predicates and dispatch: the rows and groups the window menus are
//! built from, the label and state each command reports, the dispatch table behind them, and the
//! table keyboard selection, scrolling and focus actions.

use crate::app::CairnApp;
use crate::shell::command_groups::{GroupCommand, MenuGroup, ZoomPreset, group_spec};
use crate::shell::commands::*;
use crate::shell::dialogs;
use crate::shell::location::ActiveLocation;
use crate::shell::menu_rules::{IndexMenuState, SubsystemMenuAccess};
use crate::shell::os::open_url;
use crate::shell::status_bar::format_count;
use crate::table::model::format_timestamp;
use crate::view;
use cairn_core::format_size_bytes;
use cairn_protocol::status::ContentIndexState;
use fluent_bundle::FluentArgs;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::gpui::*;

use crate::shell::menu_popup::{GroupHoverAction, decide_group_hover, menu_entries_from_sections};

impl CairnApp {
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

    pub(crate) fn command_label(&self, command: GroupCommand) -> String {
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
            GroupCommand::SortBy(_) | GroupCommand::ReverseOrder => self.local_listing_is_busy(),
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
}

#[cfg(test)]
#[path = "menu_actions_tests.rs"]
mod tests;
