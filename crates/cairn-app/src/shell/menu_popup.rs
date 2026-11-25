// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Popup-menu construction and command dispatch for the window menu bar.

use crate::app::CairnApp;
use crate::shell::command_groups::{GroupCommand, MenuGroup};
use crate::view;
use cairn_protocol::status::SourceKind;
use fluent_bundle::FluentArgs;
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::gpui::*;
use std::rc::Rc;

pub(super) fn build_submenu_popup(
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

pub(super) fn rescan_submenu_entries(
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
pub(crate) fn menu_entries_from_sections(
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
pub(crate) fn menu_entries(
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
