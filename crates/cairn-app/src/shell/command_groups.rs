// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The Everything-style menu bar the title bar owns (UI-01, UI-03).
//!
//! Six top-level menus sit on the same row as the brand: 文件, 编辑, 查看, 搜索, 工具 and 帮助.
//! Every command a toolbar, a shortcut or the result context menu can run is listed here once, so
//! the menu item and the keyboard binding cannot disagree about what a command is. The 工具 menu is
//! the home of the user preferences (theme, language, autostart) and index status; the index is a
//! status row plus the one service switch this window writes.
//!
//! This module holds no behaviour and no translated text. A [`GroupCommand`] names a window
//! command; the view runs it through the same owner method the keyboard already reaches, and reads
//! its label through Fluent at the call site. A [`GroupCommand::Heading`] is a section label, not a
//! command, and renders as plain text inside the popup.

use crate::shell::commands::{
    CopyFileNames, CopyFullPaths, CutQueryText, DeleteSelected, EscapeQuery, OpenSelected,
    PasteQueryText, RedoQueryText, RestoreSelected, RevealSelectedLocation, SelectAllQueryText,
    ShowProperties, ToggleCaseSensitive, ToggleMatchPath, ToggleRegex, ToggleWholeWord,
    UndoQueryText,
};
use crate::shell::settings::{
    LANGUAGE_EN_US, LANGUAGE_SYSTEM, LANGUAGE_ZH_CN, THEME_DARK, THEME_LIGHT, THEME_SYSTEM,
};
use crate::table::model::SortColumn;
use gpui_kit::gpui::Action;

/// One top-level menu in the title bar. One popup is open at a time, the way a desktop menu bar
/// behaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuGroup {
    File,
    Edit,
    View,
    Search,
    Tools,
    Help,
}

impl MenuGroup {
    /// Stable machine-readable identity: part of the element id of the trigger and its menu.
    pub const fn key(self) -> &'static str {
        match self {
            MenuGroup::File => "file",
            MenuGroup::Edit => "edit",
            MenuGroup::View => "view",
            MenuGroup::Search => "search",
            MenuGroup::Tools => "tools",
            MenuGroup::Help => "help",
        }
    }

    /// Fluent message naming the trigger.
    pub const fn label_key(self) -> &'static str {
        match self {
            MenuGroup::File => "menu_file",
            MenuGroup::Edit => "menu_edit",
            MenuGroup::View => "menu_view",
            MenuGroup::Search => "menu_search",
            MenuGroup::Tools => "menu_tools",
            MenuGroup::Help => "menu_help",
        }
    }

    /// The Alt mnemonic that opens this menu. Every top-level menu has one, so a keyboard user can
    /// reach the whole bar without a pointer.
    #[cfg(test)]
    pub const fn keys(self) -> &'static [&'static str] {
        match self {
            MenuGroup::File => &["alt-f"],
            MenuGroup::Edit => &["alt-e"],
            // Alt+P is the filter-cycle shortcut; it still opens the menu that owns the filters.
            MenuGroup::View => &["alt-v", "alt-p"],
            MenuGroup::Search => &["alt-s"],
            MenuGroup::Tools => &["alt-t"],
            MenuGroup::Help => &["alt-h"],
        }
    }
}

/// The three named interface sizes. The value is the ratio the theme stores; the menu never shows
/// a free number, and the keyboard step lands on the same band.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoomPreset {
    Compact,
    Standard,
    Relaxed,
}

impl ZoomPreset {
    pub const ALL: [ZoomPreset; 3] = [
        ZoomPreset::Compact,
        ZoomPreset::Standard,
        ZoomPreset::Relaxed,
    ];

    pub const fn value(self) -> f32 {
        match self {
            ZoomPreset::Compact => 0.9,
            ZoomPreset::Standard => 1.0,
            ZoomPreset::Relaxed => 1.1,
        }
    }

    pub const fn label_key(self) -> &'static str {
        match self {
            ZoomPreset::Compact => "settings_zoom_compact",
            ZoomPreset::Standard => "settings_zoom_standard",
            ZoomPreset::Relaxed => "settings_zoom_relaxed",
        }
    }

    /// The preset a persisted or stepped zoom points at. An out-of-band value still lands on the
    /// closest allowed size instead of leaving the menu with no checkmark.
    pub fn nearest(zoom: f32) -> ZoomPreset {
        Self::ALL
            .into_iter()
            .min_by(|left, right| {
                (zoom - left.value())
                    .abs()
                    .partial_cmp(&(zoom - right.value()).abs())
                    .unwrap_or(core::cmp::Ordering::Equal)
            })
            .unwrap_or(ZoomPreset::Standard)
    }
}

/// One command a menu lists. Every variant reaches an owner method the keyboard or a table header
/// already drives, so an item is another entry point to one behaviour, never a second
/// implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupCommand {
    // ---- the selected result (文件) -------------------------------------------------
    OpenSelected,
    RestoreSelected,
    RevealLocation,
    ShowProperties,
    DeleteSelected,
    EmptyRecycleBin,
    // ---- the query text and clipboard (编辑) ----------------------------------------
    UndoQueryText,
    RedoQueryText,
    CutQueryText,
    CopyPaths,
    CopyNames,
    PasteQueryText,
    SelectAllQueryText,
    ClearQuery,
    // ---- match flags (搜索) ---------------------------------------------------------
    ToggleCase,
    ToggleWholeWord,
    ToggleMatchPath,
    ToggleRegex,
    // ---- ordering (查看) ------------------------------------------------------------
    SortBy(SortColumn),
    ReverseOrder,
    // ---- appearance (查看) ----------------------------------------------------------
    SetTheme(&'static str),
    SetZoom(ZoomPreset),
    // ---- preferences and index state (工具) ----------------------------------------
    Heading(&'static str),
    /// A labelled submenu and its rows. Everything keeps file types, sorting, theme and zoom in
    /// submenus so the top-level menu stays short enough to fit on one screen.
    Submenu(&'static str, &'static [GroupCommand]),
    SetLanguage(&'static str),
    ToggleAutostart,
    IndexSource,
    /// A read-only status row. It reports what the service says; it is not the switch that changes
    /// it, so clicking it can never resubmit a setting.
    ContentIndexStatus,
    /// A read-only line naming a local action failure that the state row cannot carry.
    ContentIndexNotice,
    ToggleContentIndex,
    CancelContentIndex,
    RetryContentIndex,
    /// Re-reads the status after a status-read failure. Kept apart from the retry that asks the
    /// service to rebuild the index: the two recover different conditions.
    RefreshContentIndexStatus,
    ClearContentIndex,
    // ---- help -----------------------------------------------------------------------
    About,
}

impl GroupCommand {
    /// The Fluent message that names this command, visible and announced alike. A command the result
    /// context menu already speaks reuses that message, so one action never carries two wordings.
    ///
    /// The two index rows are named dynamically by the owner; this is their fallback name for the
    /// coverage checks.
    pub fn label_key(self) -> &'static str {
        match self {
            GroupCommand::OpenSelected => "ctx_open",
            GroupCommand::RestoreSelected => "ctx_restore",
            GroupCommand::RevealLocation => "ctx_open_path",
            GroupCommand::ShowProperties => "ctx_properties",
            GroupCommand::DeleteSelected => "ctx_delete",
            GroupCommand::EmptyRecycleBin => "ctx_empty_recycle_bin",
            GroupCommand::UndoQueryText => "query_undo",
            GroupCommand::RedoQueryText => "query_redo",
            GroupCommand::CutQueryText => "query_cut",
            GroupCommand::CopyPaths => "ctx_copy_path",
            GroupCommand::CopyNames => "ctx_copy_name",
            GroupCommand::PasteQueryText => "query_paste",
            GroupCommand::SelectAllQueryText => "query_select_all_text",
            GroupCommand::ClearQuery => "query_clear",
            GroupCommand::ToggleCase => "toggle_case_label",
            GroupCommand::ToggleWholeWord => "toggle_whole_word_label",
            GroupCommand::ToggleMatchPath => "toggle_match_path_label",
            GroupCommand::ToggleRegex => "toggle_regex_label",
            GroupCommand::SortBy(column) => SortColumn::label_key(column),
            GroupCommand::ReverseOrder => "sort_reverse",
            GroupCommand::SetTheme(mode) => theme_label_key(mode),
            GroupCommand::SetZoom(preset) => preset.label_key(),
            GroupCommand::Heading(key) => key,
            GroupCommand::Submenu(label, _) => label,
            GroupCommand::SetLanguage(code) => language_label_key(code),
            GroupCommand::ToggleAutostart => "settings_autostart",
            GroupCommand::IndexSource => "settings_section_index_source",
            // The status rows are named by the owner, which resolves the current state and its
            // reason; this is the fallback for the coverage checks.
            GroupCommand::ContentIndexStatus | GroupCommand::ContentIndexNotice => {
                "settings_content_index"
            }
            GroupCommand::ToggleContentIndex => "settings_content_index",
            GroupCommand::CancelContentIndex => "menu_content_index_cancel",
            GroupCommand::RetryContentIndex => "menu_content_index_retry",
            GroupCommand::RefreshContentIndexStatus => "menu_content_index_reload",
            GroupCommand::ClearContentIndex => "menu_content_index_clear",
            GroupCommand::About => "menu_about",
        }
    }

    /// The Action this item runs, when a keymap Action already owns the command. Dispatching it lets
    /// the menu read its hint from the keymap instead of repeating a keystroke here, and keeps the
    /// item and its shortcut on one implementation.
    ///
    /// A command with no Action of its own — every choice in a single-owner family such as the sort
    /// columns, the settings choices, the headings, and ClearQuery — returns `None` and the view
    /// wires its click to the owner method directly. ClearQuery's key is the dismissal cascade
    /// rather than this command, so it advertises the key through [`Self::hint_action`].
    pub fn action(self) -> Option<Box<dyn Action>> {
        match self {
            GroupCommand::OpenSelected => Some(Box::new(OpenSelected)),
            GroupCommand::RestoreSelected => Some(Box::new(RestoreSelected)),
            GroupCommand::RevealLocation => Some(Box::new(RevealSelectedLocation)),
            GroupCommand::UndoQueryText => Some(Box::new(UndoQueryText)),
            GroupCommand::RedoQueryText => Some(Box::new(RedoQueryText)),
            GroupCommand::CutQueryText => Some(Box::new(CutQueryText)),
            GroupCommand::CopyPaths => Some(Box::new(CopyFullPaths)),
            GroupCommand::CopyNames => Some(Box::new(CopyFileNames)),
            GroupCommand::PasteQueryText => Some(Box::new(PasteQueryText)),
            GroupCommand::ShowProperties => Some(Box::new(ShowProperties)),
            GroupCommand::DeleteSelected => Some(Box::new(DeleteSelected)),
            GroupCommand::SelectAllQueryText => Some(Box::new(SelectAllQueryText)),
            GroupCommand::ToggleCase => Some(Box::new(ToggleCaseSensitive)),
            GroupCommand::ToggleWholeWord => Some(Box::new(ToggleWholeWord)),
            GroupCommand::ToggleMatchPath => Some(Box::new(ToggleMatchPath)),
            GroupCommand::ToggleRegex => Some(Box::new(ToggleRegex)),
            _ => None,
        }
    }

    /// The Action whose binding supplies this row's hint while the click runs the command.
    ///
    /// ClearQuery is click-only: Escape belongs to the window's dismissal cascade, so dispatching
    /// it from the Edit menu would close the menu (or a toast) and leave the query in place. The
    /// row still advertises Escape through this hint, without dispatching it.
    pub fn hint_action(self) -> Option<Box<dyn Action>> {
        match self {
            GroupCommand::ClearQuery => Some(Box::new(EscapeQuery)),
            _ => None,
        }
    }

    /// The keystroke a person reads beside this item, as the keymap binds it.
    ///
    /// A dropdown item drawn by [`gpui_kit::component::menu::PopupMenu`] resolves its own hint from
    /// the binding of its Action, so the hint can only be checked against the keymap by name. This
    /// is that name, and [`crate::accessibility`] requires every one of them to still be bound: an
    /// item that promises a key nobody bound is a silent failure.
    #[cfg(test)]
    pub const fn shortcut(self) -> Option<&'static str> {
        match self {
            GroupCommand::OpenSelected => Some("enter"),
            GroupCommand::RevealLocation => Some("ctrl-enter"),
            GroupCommand::UndoQueryText => Some("ctrl-z"),
            GroupCommand::RedoQueryText => Some("ctrl-y"),
            GroupCommand::CutQueryText => Some("ctrl-x"),
            GroupCommand::CopyPaths => Some("ctrl-shift-c"),
            GroupCommand::CopyNames => Some("ctrl-shift-n"),
            GroupCommand::PasteQueryText => Some("ctrl-v"),
            GroupCommand::ShowProperties => Some("alt-enter"),
            GroupCommand::DeleteSelected => Some("delete"),
            GroupCommand::SelectAllQueryText => Some("ctrl-a"),
            GroupCommand::ClearQuery => Some("escape"),
            GroupCommand::ToggleCase => Some("ctrl-i"),
            GroupCommand::ToggleWholeWord => Some("ctrl-b"),
            GroupCommand::ToggleMatchPath => Some("ctrl-u"),
            GroupCommand::ToggleRegex => Some("ctrl-r"),
            _ => None,
        }
    }

    /// Headings are section labels, not rows a person can run. The menu builder draws them through
    /// `PopupMenu::label` instead of creating a disabled-looking command.
    pub const fn is_heading(self) -> bool {
        matches!(self, GroupCommand::Heading(_))
    }

    /// The children of a submenu, or an empty slice for every other row.
    pub const fn submenu_children(self) -> &'static [GroupCommand] {
        match self {
            GroupCommand::Submenu(_, children) => children,
            _ => &[],
        }
    }
}

fn theme_label_key(mode: &str) -> &'static str {
    if mode == THEME_SYSTEM {
        "settings_theme_system"
    } else if mode == THEME_LIGHT {
        "settings_theme_light"
    } else {
        "settings_theme_dark"
    }
}

fn language_label_key(code: &str) -> &'static str {
    if code == LANGUAGE_SYSTEM {
        "settings_language_system"
    } else if code == LANGUAGE_ZH_CN {
        "settings_language_zh"
    } else {
        "settings_language_en"
    }
}

/// One top-level menu: its identity and the command sections it lists in order. A section is
/// separated from the one before it, so a long menu still reads as groups rather than one list.
pub struct MenuGroupSpec {
    pub group: MenuGroup,
    pub sections: &'static [&'static [GroupCommand]],
}

pub static DEFAULT_GROUPS: &[MenuGroupSpec] = &[
    MenuGroupSpec {
        group: MenuGroup::File,
        sections: &[
            &[
                GroupCommand::OpenSelected,
                GroupCommand::RestoreSelected,
                GroupCommand::RevealLocation,
                GroupCommand::ShowProperties,
            ],
            &[GroupCommand::DeleteSelected, GroupCommand::EmptyRecycleBin],
        ],
    },
    MenuGroupSpec {
        group: MenuGroup::Edit,
        sections: &[
            &[GroupCommand::UndoQueryText, GroupCommand::RedoQueryText],
            &[
                GroupCommand::CutQueryText,
                GroupCommand::CopyPaths,
                GroupCommand::CopyNames,
                GroupCommand::PasteQueryText,
                GroupCommand::SelectAllQueryText,
            ],
            &[GroupCommand::ClearQuery],
        ],
    },
    MenuGroupSpec {
        group: MenuGroup::View,
        sections: &[
            &[GroupCommand::Submenu(
                "menu_sort",
                &[
                    GroupCommand::SortBy(SortColumn::Name),
                    GroupCommand::SortBy(SortColumn::Path),
                    GroupCommand::SortBy(SortColumn::Size),
                    GroupCommand::SortBy(SortColumn::DateModified),
                    GroupCommand::ReverseOrder,
                ],
            )],
            &[GroupCommand::Submenu(
                "settings_theme",
                &[
                    GroupCommand::SetTheme(THEME_SYSTEM),
                    GroupCommand::SetTheme(THEME_LIGHT),
                    GroupCommand::SetTheme(THEME_DARK),
                ],
            )],
            &[GroupCommand::Submenu(
                "settings_section_zoom",
                &[
                    GroupCommand::SetZoom(ZoomPreset::Compact),
                    GroupCommand::SetZoom(ZoomPreset::Standard),
                    GroupCommand::SetZoom(ZoomPreset::Relaxed),
                ],
            )],
        ],
    },
    MenuGroupSpec {
        group: MenuGroup::Search,
        sections: &[&[
            GroupCommand::ToggleCase,
            GroupCommand::ToggleWholeWord,
            GroupCommand::ToggleMatchPath,
            GroupCommand::ToggleRegex,
        ]],
    },
    MenuGroupSpec {
        group: MenuGroup::Tools,
        sections: &[
            &[GroupCommand::Submenu(
                "settings_section_language",
                &[
                    GroupCommand::SetLanguage(LANGUAGE_SYSTEM),
                    GroupCommand::SetLanguage(LANGUAGE_ZH_CN),
                    GroupCommand::SetLanguage(LANGUAGE_EN_US),
                ],
            )],
            &[
                GroupCommand::Heading("menu_startup"),
                GroupCommand::ToggleAutostart,
            ],
            &[
                GroupCommand::Heading("menu_index"),
                GroupCommand::IndexSource,
                GroupCommand::ContentIndexStatus,
                GroupCommand::ContentIndexNotice,
                GroupCommand::ToggleContentIndex,
                GroupCommand::CancelContentIndex,
                GroupCommand::RetryContentIndex,
                GroupCommand::RefreshContentIndexStatus,
                GroupCommand::ClearContentIndex,
            ],
        ],
    },
    MenuGroupSpec {
        group: MenuGroup::Help,
        sections: &[&[GroupCommand::About]],
    },
];

/// The menu bar groups, in the order the title bar lists their triggers.
pub fn menu_groups() -> &'static [MenuGroupSpec] {
    DEFAULT_GROUPS
}

/// The spec of one group, so the view never searches for the menu it is about to build.
pub fn group_spec(group: MenuGroup) -> &'static MenuGroupSpec {
    menu_groups()
        .iter()
        .find(|spec| spec.group == group)
        .unwrap_or_else(|| panic!("{group:?} has no command group to render"))
}

/// Every command the shell lists, in declaration order. The i18n and registry checks walk this so a
/// command that gained a wording key cannot ship untranslated.
#[cfg(test)]
pub fn all_commands() -> Vec<GroupCommand> {
    fn visit(command: GroupCommand, out: &mut Vec<GroupCommand>) {
        out.push(command);
        for child in command.submenu_children() {
            visit(*child, out);
        }
    }

    let mut commands = Vec::new();
    for spec in menu_groups() {
        for section in spec.sections {
            for command in *section {
                visit(*command, &mut commands);
            }
        }
    }
    commands
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::{all_commands, group_spec, menu_groups, GroupCommand, MenuGroup, ZoomPreset};
    use crate::shell::i18n::I18nManager;
    use crate::shell::theme::{MAX_ZOOM, MIN_ZOOM};
    use crate::table::model::SortColumn;

    #[test]
    fn every_group_label_is_a_localized_message() {
        let i18n = I18nManager::default();
        assert_eq!(menu_groups().len(), 6, "the title bar owns six menus");
        for spec in menu_groups() {
            let text = i18n.get_text(spec.group.label_key());
            assert!(
                !text.trim().is_empty() && text != spec.group.label_key(),
                "{} would render as raw text on the menu bar",
                spec.group.label_key()
            );
            for section in spec.sections {
                assert!(
                    !section.is_empty(),
                    "{} lists an empty section",
                    spec.group.key()
                );
            }
        }
    }

    /// An item that resolves to no text is a silent failure, and a command the user cannot name is a
    /// command they cannot find in a screen reader.
    #[test]
    fn every_command_resolves_to_text_in_both_locales() {
        let commands = all_commands();
        assert!(
            commands.len() >= 30,
            "only {} commands are grouped",
            commands.len()
        );
        for command in &commands {
            for locale in ["zh-CN", "en-US"] {
                let i18n = I18nManager::new(locale);
                let key = command.label_key();
                let text = i18n
                    .try_text(key)
                    .unwrap_or_else(|| panic!("{key:?} ({command:?}) has no {locale} message"));
                assert!(
                    !text.trim().is_empty() && text != key,
                    "{key} would render its raw id in {locale}"
                );
                assert!(
                    !crate::shell::accessibility::label_carries_mnemonic(&text),
                    "{key} would show a mnemonic character as visible text: {text}"
                );
            }
        }
    }

    /// Each group is addressed by exactly one key, and a key names the group the shell opens for it.
    #[test]
    fn group_keys_are_unique_and_each_group_is_reachable() {
        let mut keys: Vec<&str> = Vec::new();
        for spec in menu_groups() {
            keys.push(spec.group.key());
            assert_eq!(group_spec(spec.group).group, spec.group);
        }
        keys.sort_unstable();
        let count = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), count, "two groups share one element key");
    }

    /// A command whose Action and keyboard path exist must not also be handed a second handler by the
    /// view: `PopupMenuItem` runs the handler *instead of* the action. The families that have no
    /// action of their own are the choices and owner methods, so they are the only ones allowed to
    /// return `None`.
    #[test]
    fn action_bearing_commands_are_the_ones_the_keymap_already_owns() {
        for command in all_commands() {
            let has_action = command.action().is_some();
            match command {
                GroupCommand::SortBy(_)
                | GroupCommand::ReverseOrder
                | GroupCommand::SetTheme(_)
                | GroupCommand::SetZoom(_)
                | GroupCommand::Heading(_)
                | GroupCommand::Submenu(_, _)
                | GroupCommand::SetLanguage(_)
                | GroupCommand::ToggleAutostart
                | GroupCommand::IndexSource
                | GroupCommand::ContentIndexStatus
                | GroupCommand::ContentIndexNotice
                | GroupCommand::ToggleContentIndex
                | GroupCommand::CancelContentIndex
                | GroupCommand::RetryContentIndex
                | GroupCommand::RefreshContentIndexStatus
                | GroupCommand::ClearContentIndex
                | GroupCommand::EmptyRecycleBin
                | GroupCommand::About => {
                    assert!(
                        !has_action,
                        "{command:?} is a choice or an owner method, so the view wires its click"
                    );
                }
                // ClearQuery must run as a direct command: its Escape Action is the dismissal
                // cascade, so clicking it through the action path would close the menu and leave
                // the query in place. The row keeps the Escape binding only as its hint.
                GroupCommand::ClearQuery => {
                    assert!(
                        !has_action,
                        "ClearQuery must not dispatch EscapeQuery on click"
                    );
                    assert!(
                        command.hint_action().is_some(),
                        "ClearQuery lost the Escape hint beside its label"
                    );
                }
                _ => assert!(has_action, "{command:?} lost the Action that runs it"),
            }
        }
    }

    /// The Edit menu's 清空搜索 row used to dispatch EscapeQuery: that action closed the menu and
    /// returned before touching the query, so the click looked dead. The row must run the command
    /// itself and only borrow Escape for the hint beside its label.
    #[test]
    fn clear_query_runs_as_a_command_and_borrows_escape_only_as_a_hint() {
        use crate::shell::commands::EscapeQuery;

        let command = GroupCommand::ClearQuery;
        assert!(
            command.action().is_none(),
            "clicking the row must not dispatch the dismissal cascade"
        );
        let hint = command
            .hint_action()
            .expect("the row keeps the Escape hint");
        assert!(
            hint.as_any().is::<EscapeQuery>(),
            "the hint must still be drawn from the Escape binding"
        );
        assert_eq!(command.shortcut(), Some("escape"));
    }

    /// Two menus on one key would open whichever the keymap reaches last, so the mnemonic table has
    /// to stay one-to-one, and every mnemonic has to parse as a keystroke the keymap can bind.
    #[test]
    fn group_and_command_keys_are_unique_and_parseable_keystrokes() {
        use gpui_kit::gpui::Keystroke;
        use std::collections::HashSet;

        let mut seen: HashSet<&str> = HashSet::new();
        for spec in menu_groups() {
            for key in spec.group.keys() {
                assert!(
                    seen.insert(*key),
                    "{} repeats the mnemonic {key}",
                    spec.group.label_key()
                );
                let parsed =
                    Keystroke::parse(key).unwrap_or_else(|_| panic!("{key} is not a keystroke"));
                assert!(parsed.modifiers.alt, "{key} is not an Alt group mnemonic");
            }
        }
        for command in all_commands() {
            let Some(key) = command.shortcut() else {
                continue;
            };
            assert!(
                command.action().is_some() || command.hint_action().is_some(),
                "{command:?} promises {key} but has no Action or hint for it"
            );
            let parsed =
                Keystroke::parse(key).unwrap_or_else(|_| panic!("{key} is not a keystroke"));
            assert!(
                !parsed.modifiers.alt || key == "alt-enter",
                "{command:?} holds an Alt key that belongs to a group, not a command"
            );
        }
    }

    /// Sorting by every column the table can order is a listed command, so the 查看 menu cannot drift
    /// away from the table headings.
    #[test]
    fn every_sortable_column_is_listed_once() {
        let listed: Vec<SortColumn> = all_commands()
            .into_iter()
            .filter_map(|command| match command {
                GroupCommand::SortBy(column) => Some(column),
                _ => None,
            })
            .collect();
        assert_eq!(
            listed,
            vec![
                SortColumn::Name,
                SortColumn::Path,
                SortColumn::Size,
                SortColumn::DateModified
            ]
        );
    }

    #[test]
    fn the_zoom_presets_cover_the_band_and_step_evenly() {
        assert_eq!(ZoomPreset::ALL.len(), 3);
        assert_eq!(ZoomPreset::Compact.value(), MIN_ZOOM);
        assert_eq!(ZoomPreset::Standard.value(), 1.0);
        assert_eq!(ZoomPreset::Relaxed.value(), MAX_ZOOM);
        assert_eq!(ZoomPreset::nearest(0.75), ZoomPreset::Compact);
        assert_eq!(ZoomPreset::nearest(1.25), ZoomPreset::Relaxed);
        assert_eq!(ZoomPreset::nearest(1.0), ZoomPreset::Standard);
    }

    #[test]
    fn every_menu_group_is_in_declaration_order() {
        assert_eq!(
            menu_groups()
                .iter()
                .map(|spec| spec.group)
                .collect::<Vec<_>>(),
            vec![
                MenuGroup::File,
                MenuGroup::Edit,
                MenuGroup::View,
                MenuGroup::Search,
                MenuGroup::Tools,
                MenuGroup::Help,
            ]
        );
    }
}
