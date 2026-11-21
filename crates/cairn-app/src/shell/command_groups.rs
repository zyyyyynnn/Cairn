// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The Everything-style menu bar the title bar owns (UI-01, UI-03).
//!
//! Seven top-level menus sit on the same row as the brand: 文件, 编辑, 查看, 搜索, 索引, 工具 and 帮助.
//! Every command a toolbar, a shortcut or the result context menu can run is listed here once, so
//! the menu item and the keyboard binding cannot disagree about what a command is.
//!
//! This module holds no behaviour and no translated text. A [`GroupCommand`] names a window
//! command; the view runs it through the same owner method the keyboard already reaches, and reads
//! its label through Fluent at the call site.

use crate::shell::commands::{
    CopyFileNames, CopyFullPaths, CutQueryText, DeleteSelected, EscapeQuery, OpenSelected,
    PasteQueryText, RedoQueryText, RenameSelected, RestoreAll, RestoreSelected,
    RevealSelectedLocation, SelectAll, ShowProperties, ToggleCaseSensitive, ToggleMatchPath,
    ToggleRegex, ToggleWholeWord, UndoQueryText,
};
use crate::shell::settings::{
    LANGUAGE_EN_US, LANGUAGE_SYSTEM, LANGUAGE_ZH_CN, THEME_DARK, THEME_LIGHT, THEME_SYSTEM,
};
use crate::table::model::{SortColumn, SortColumnLabel, TableColumn};
use gpui_kit::gpui::Action;

/// One top-level menu in the title bar. One popup is open at a time, the way a desktop menu bar
/// behaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuGroup {
    File,
    Edit,
    View,
    Search,
    Index,
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
            MenuGroup::Index => "index",
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
            MenuGroup::Index => "menu_index",
            MenuGroup::Tools => "menu_tools",
            MenuGroup::Help => "menu_help",
        }
    }

    /// The Action that toggles this menu from the keyboard.
    #[cfg(test)]
    pub fn open_action(self) -> Box<dyn Action> {
        use crate::shell::commands::{
            OpenEditMenu, OpenFileMenu, OpenHelpMenu, OpenIndexMenu, OpenSearchMenu, OpenToolsMenu,
            OpenViewMenu,
        };
        match self {
            MenuGroup::File => Box::new(OpenFileMenu),
            MenuGroup::Edit => Box::new(OpenEditMenu),
            MenuGroup::View => Box::new(OpenViewMenu),
            MenuGroup::Search => Box::new(OpenSearchMenu),
            MenuGroup::Index => Box::new(OpenIndexMenu),
            MenuGroup::Tools => Box::new(OpenToolsMenu),
            MenuGroup::Help => Box::new(OpenHelpMenu),
        }
    }

    /// The Alt mnemonic that opens this menu, derived from [`crate::shell::commands::key_bindings`].
    #[cfg(test)]
    pub fn keys(self) -> Vec<&'static str> {
        let action_name = self.open_action().name();
        crate::shell::commands::key_bindings()
            .into_iter()
            .filter(|entry| entry.action == action_name)
            .map(|entry| entry.shortcut)
            .collect()
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

    pub fn step_up(zoom: f32) -> ZoomPreset {
        match Self::nearest(zoom) {
            ZoomPreset::Compact => ZoomPreset::Standard,
            ZoomPreset::Standard | ZoomPreset::Relaxed => ZoomPreset::Relaxed,
        }
    }

    pub fn step_down(zoom: f32) -> ZoomPreset {
        match Self::nearest(zoom) {
            ZoomPreset::Relaxed => ZoomPreset::Standard,
            ZoomPreset::Standard | ZoomPreset::Compact => ZoomPreset::Compact,
        }
    }
}

/// One command a menu lists. Every variant reaches an owner method the keyboard or a table header
/// already drives, so an item is another entry point to one behaviour, never a second
/// implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupCommand {
    OpenSelected,
    RenameSelected,
    RestoreSelected,
    RestoreAll,
    RevealLocation,
    ShowProperties,
    DeleteSelected,
    EmptyRecycleBin,
    UndoQueryText,
    RedoQueryText,
    CutQueryText,
    CopyPaths,
    CopyNames,
    PasteQueryText,
    SelectAll,
    ClearQuery,
    ToggleCase,
    ToggleWholeWord,
    ToggleMatchPath,
    ToggleRegex,
    TogglePinyin,
    ToggleIgnorePunctuation,
    ToggleIgnoreWhitespace,
    ToggleIgnoreDiacritics,
    ToggleExcludeHidden,
    ToggleExcludeSystem,
    SortBy(SortColumn),
    ReverseOrder,
    ToggleColumn(TableColumn),
    SetTheme(&'static str),
    SetZoom(ZoomPreset),
    /// A labelled submenu and its sectioned rows. Sections are separated inside the nested popup.
    Submenu(&'static str, &'static [&'static [GroupCommand]]),
    SetLanguage(&'static str),
    ToggleAutostart,
    /// Opens the read-only Index Journal viewer dialog.
    OpenIndexJournal,
    /// Opens the folder-source manager even when no source exists yet.
    ManageFolderSources,
    /// Dynamic rescan submenu listing all locations plus individual indexed sources.
    RescanMenu,
    /// Triggers a full rescan across all indexed sources.
    RescanAll,
    /// Submenu grouping the content-search switch, status facts, and recovery actions.
    ContentIndexMenu,
    /// The single "Search file contents" switch.
    ToggleContentSearch,
    /// Whether the content index is off, backfilling, ready or failed.
    ContentSearchDetails,
    /// How much space the content index takes on disk.
    ContentSearchUsage,
    /// When the content index was last committed.
    ContentSearchUpdated,
    /// Restarts a content index that failed.
    RetryContentSearch,
    /// Removes the indexed contents without touching the file-name index.
    ClearContentSearch,
    /// Submenu grouping extended-property indexing switch and status facts.
    PropertyIndexMenu,
    /// Master switch for extended-property indexing.
    TogglePropertyIndex,
    PropertyIndexStatus,
    PropertyIndexUsage,
    /// Checks the latest stable release, downloads and verifies the installer, then hands the
    /// verified installer to the elevated setup flow.
    CheckForUpdates,
    About,
}

/// Static children for the Rescan submenu; individual indexed sources follow dynamically in the view.
pub(crate) const RESCAN_SECTIONS: &[&[GroupCommand]] = &[&[GroupCommand::RescanAll]];

/// Sections inside the Content Index submenu: toggle, live facts, and conditional recovery actions.
pub(crate) const CONTENT_INDEX_SECTIONS: &[&[GroupCommand]] = &[
    &[GroupCommand::ToggleContentSearch],
    &[
        GroupCommand::ContentSearchDetails,
        GroupCommand::ContentSearchUsage,
        GroupCommand::ContentSearchUpdated,
    ],
    &[
        GroupCommand::RetryContentSearch,
        GroupCommand::ClearContentSearch,
    ],
];

/// Sections inside the Property Index submenu: toggle and live status/storage facts.
pub(crate) const PROPERTY_INDEX_SECTIONS: &[&[GroupCommand]] = &[
    &[GroupCommand::TogglePropertyIndex],
    &[
        GroupCommand::PropertyIndexStatus,
        GroupCommand::PropertyIndexUsage,
    ],
];

impl GroupCommand {
    /// The Fluent message that names this command, visible and announced alike. A command the result
    /// context menu already speaks reuses that message, so one action never carries two wordings.
    ///
    /// Live index fact rows are formatted dynamically by the owner; this is their fallback name for
    /// coverage checks.
    pub fn label_key(self) -> &'static str {
        match self {
            GroupCommand::OpenSelected => "ctx_open",
            GroupCommand::RenameSelected => "ctx_rename",
            GroupCommand::RestoreSelected => "ctx_restore",
            GroupCommand::RestoreAll => "ctx_restore_all",
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
            GroupCommand::SelectAll => "edit_select_all",
            GroupCommand::ClearQuery => "query_clear",
            GroupCommand::ToggleCase => "toggle_case_label",
            GroupCommand::ToggleWholeWord => "toggle_whole_word_label",
            GroupCommand::ToggleMatchPath => "toggle_match_path_label",
            GroupCommand::ToggleRegex => "toggle_regex_label",
            GroupCommand::TogglePinyin => "toggle_pinyin_label",
            GroupCommand::ToggleIgnorePunctuation => "toggle_ignore_punctuation_label",
            GroupCommand::ToggleIgnoreWhitespace => "toggle_ignore_whitespace_label",
            GroupCommand::ToggleIgnoreDiacritics => "toggle_ignore_diacritics_label",
            GroupCommand::ToggleExcludeHidden => "toggle_exclude_hidden_label",
            GroupCommand::ToggleExcludeSystem => "toggle_exclude_system_label",
            GroupCommand::SortBy(column) => column.label_key(),
            GroupCommand::ReverseOrder => "sort_reverse",
            GroupCommand::ToggleColumn(col) => col.label_key(),
            GroupCommand::SetTheme(mode) => theme_label_key(mode),
            GroupCommand::SetZoom(preset) => preset.label_key(),
            GroupCommand::Submenu(label, _) => label,
            GroupCommand::SetLanguage(code) => language_label_key(code),
            GroupCommand::ToggleAutostart => "settings_autostart",
            GroupCommand::OpenIndexJournal => "menu_index_journal_open",
            GroupCommand::ManageFolderSources => "menu_folder_sources",
            GroupCommand::RescanMenu => "menu_rescan",
            GroupCommand::RescanAll => "menu_rescan_all",
            GroupCommand::ContentIndexMenu => "menu_content_index",
            GroupCommand::ToggleContentSearch => "menu_content_search",
            GroupCommand::ContentSearchDetails => "menu_content_search_state",
            GroupCommand::ContentSearchUsage => "menu_content_search_usage_unknown",
            GroupCommand::ContentSearchUpdated => "menu_content_search_never_updated",
            GroupCommand::RetryContentSearch => "menu_content_search_retry",
            GroupCommand::ClearContentSearch => "menu_content_search_clear",
            GroupCommand::PropertyIndexMenu => "menu_property_index_menu",
            GroupCommand::TogglePropertyIndex => "menu_property_index",
            GroupCommand::PropertyIndexStatus => "menu_property_index_status",
            GroupCommand::PropertyIndexUsage => "menu_property_index_usage_unknown",
            GroupCommand::CheckForUpdates => "menu_check_updates",
            GroupCommand::About => "menu_about",
        }
    }

    /// The Fluent message that names this command in the active location. Inside the Recycle Bin,
    /// deletion is permanent, copying paths copies the original location, and sort columns use the
    /// bin's column captions.
    pub fn label_key_in(self, in_recycle_bin: bool) -> &'static str {
        if in_recycle_bin {
            match self {
                GroupCommand::DeleteSelected => "ctx_delete_permanently",
                GroupCommand::CopyPaths => "ctx_copy_original_location",
                GroupCommand::SortBy(column) => column.label_key_in(true),
                _ => self.label_key(),
            }
        } else {
            self.label_key()
        }
    }

    /// The Action this item runs, when a keymap Action already owns the command. Dispatching it lets
    /// the menu read its hint from the keymap instead of repeating a keystroke here, and keeps the
    /// item and its shortcut on one implementation.
    ///
    /// A command with no Action of its own — every choice in a single-owner family such as the sort
    /// columns, the settings choices, the headings, and `ClearQuery` — returns `None` and the view
    /// wires its click to the owner method directly. `ClearQuery`'s key is the dismissal cascade
    /// rather than this command, so it advertises the key through [`Self::hint_action`].
    pub fn action(self) -> Option<Box<dyn Action>> {
        match self {
            GroupCommand::OpenSelected => Some(Box::new(OpenSelected)),
            GroupCommand::RenameSelected => Some(Box::new(RenameSelected)),
            GroupCommand::RestoreSelected => Some(Box::new(RestoreSelected)),
            GroupCommand::RestoreAll => Some(Box::new(RestoreAll)),
            GroupCommand::RevealLocation => Some(Box::new(RevealSelectedLocation)),
            GroupCommand::UndoQueryText => Some(Box::new(UndoQueryText)),
            GroupCommand::RedoQueryText => Some(Box::new(RedoQueryText)),
            GroupCommand::CutQueryText => Some(Box::new(CutQueryText)),
            GroupCommand::CopyPaths => Some(Box::new(CopyFullPaths)),
            GroupCommand::CopyNames => Some(Box::new(CopyFileNames)),
            GroupCommand::PasteQueryText => Some(Box::new(PasteQueryText)),
            GroupCommand::ShowProperties => Some(Box::new(ShowProperties)),
            GroupCommand::DeleteSelected => Some(Box::new(DeleteSelected)),
            GroupCommand::SelectAll => Some(Box::new(SelectAll)),
            GroupCommand::ToggleCase => Some(Box::new(ToggleCaseSensitive)),
            GroupCommand::ToggleWholeWord => Some(Box::new(ToggleWholeWord)),
            GroupCommand::ToggleMatchPath => Some(Box::new(ToggleMatchPath)),
            GroupCommand::ToggleRegex => Some(Box::new(ToggleRegex)),
            _ => None,
        }
    }

    /// The Action whose binding supplies this row's hint while the click runs the command.
    ///
    /// `ClearQuery` is click-only: Escape belongs to the window's dismissal cascade, so dispatching
    /// it from the Edit menu would close the menu (or a toast) and leave the query in place. The
    /// row still advertises Escape through this hint, without dispatching it.
    pub fn hint_action(self) -> Option<Box<dyn Action>> {
        match self {
            GroupCommand::ClearQuery => Some(Box::new(EscapeQuery)),
            _ => None,
        }
    }

    /// The primary keystroke the keymap binds for this item's Action (or hint Action).
    ///
    /// A dropdown item drawn by [`gpui_kit::component::menu::PopupMenu`] resolves its own hint from
    /// the binding of its Action, so deriving the shortcut from [`crate::shell::commands::key_bindings`]
    /// keeps the menu model and the keymap on one definition.
    #[cfg(test)]
    pub fn shortcut(self) -> Option<&'static str> {
        let action = self.action().or_else(|| self.hint_action())?;
        let action_name = action.name();
        crate::shell::commands::key_bindings()
            .into_iter()
            .find(|entry| entry.action == action_name)
            .map(|entry| entry.shortcut)
    }

    /// `true` when this command is a 1-of-N mutually exclusive selection rendered with a radio dot.
    pub const fn is_radio_choice(self) -> bool {
        matches!(
            self,
            GroupCommand::SortBy(_)
                | GroupCommand::SetTheme(_)
                | GroupCommand::SetZoom(_)
                | GroupCommand::SetLanguage(_)
        )
    }

    /// Sectioned rows of a static submenu, or an empty slice for non-submenu commands.
    pub const fn submenu_sections(self) -> &'static [&'static [GroupCommand]] {
        match self {
            GroupCommand::Submenu(_, sections) => sections,
            GroupCommand::RescanMenu => RESCAN_SECTIONS,
            GroupCommand::ContentIndexMenu => CONTENT_INDEX_SECTIONS,
            GroupCommand::PropertyIndexMenu => PROPERTY_INDEX_SECTIONS,
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
                GroupCommand::RenameSelected,
                GroupCommand::RestoreSelected,
                GroupCommand::RestoreAll,
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
                GroupCommand::PasteQueryText,
                GroupCommand::CopyPaths,
                GroupCommand::CopyNames,
                GroupCommand::SelectAll,
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
                    &[
                        GroupCommand::SortBy(SortColumn::Name),
                        GroupCommand::SortBy(SortColumn::Path),
                        GroupCommand::SortBy(SortColumn::Size),
                        GroupCommand::SortBy(SortColumn::DateModified),
                        GroupCommand::SortBy(SortColumn::DateCreated),
                        GroupCommand::SortBy(SortColumn::Extension),
                    ],
                    &[GroupCommand::ReverseOrder],
                ],
            )],
            &[GroupCommand::Submenu(
                "menu_columns",
                &[
                    &[
                        GroupCommand::ToggleColumn(TableColumn::Name),
                        GroupCommand::ToggleColumn(TableColumn::Path),
                        GroupCommand::ToggleColumn(TableColumn::Size),
                        GroupCommand::ToggleColumn(TableColumn::DateModified),
                    ],
                    &[
                        GroupCommand::ToggleColumn(TableColumn::Optional(
                            crate::table::model::OptionalColumn::DateCreated,
                        )),
                        GroupCommand::ToggleColumn(TableColumn::Optional(
                            crate::table::model::OptionalColumn::Length,
                        )),
                        GroupCommand::ToggleColumn(TableColumn::Optional(
                            crate::table::model::OptionalColumn::Author,
                        )),
                        GroupCommand::ToggleColumn(TableColumn::Optional(
                            crate::table::model::OptionalColumn::Tags,
                        )),
                        GroupCommand::ToggleColumn(TableColumn::Optional(
                            crate::table::model::OptionalColumn::Dimensions,
                        )),
                        GroupCommand::ToggleColumn(TableColumn::Optional(
                            crate::table::model::OptionalColumn::Framerate,
                        )),
                    ],
                ],
            )],
            &[GroupCommand::Submenu(
                "settings_theme",
                &[&[
                    GroupCommand::SetTheme(THEME_SYSTEM),
                    GroupCommand::SetTheme(THEME_LIGHT),
                    GroupCommand::SetTheme(THEME_DARK),
                ]],
            )],
            &[GroupCommand::Submenu(
                "settings_section_zoom",
                &[&[
                    GroupCommand::SetZoom(ZoomPreset::Compact),
                    GroupCommand::SetZoom(ZoomPreset::Standard),
                    GroupCommand::SetZoom(ZoomPreset::Relaxed),
                ]],
            )],
        ],
    },
    MenuGroupSpec {
        group: MenuGroup::Search,
        sections: &[
            &[
                GroupCommand::ToggleCase,
                GroupCommand::ToggleWholeWord,
                GroupCommand::ToggleMatchPath,
                GroupCommand::ToggleRegex,
            ],
            &[
                GroupCommand::TogglePinyin,
                GroupCommand::ToggleIgnorePunctuation,
                GroupCommand::ToggleIgnoreWhitespace,
                GroupCommand::ToggleIgnoreDiacritics,
            ],
            &[
                GroupCommand::ToggleExcludeHidden,
                GroupCommand::ToggleExcludeSystem,
            ],
        ],
    },
    MenuGroupSpec {
        group: MenuGroup::Index,
        sections: &[
            &[
                GroupCommand::OpenIndexJournal,
                GroupCommand::ManageFolderSources,
            ],
            &[GroupCommand::RescanMenu],
            &[
                GroupCommand::ContentIndexMenu,
                GroupCommand::PropertyIndexMenu,
            ],
        ],
    },
    MenuGroupSpec {
        group: MenuGroup::Tools,
        sections: &[
            &[GroupCommand::Submenu(
                "settings_section_language",
                &[&[
                    GroupCommand::SetLanguage(LANGUAGE_SYSTEM),
                    GroupCommand::SetLanguage(LANGUAGE_ZH_CN),
                    GroupCommand::SetLanguage(LANGUAGE_EN_US),
                ]],
            )],
            &[GroupCommand::ToggleAutostart],
        ],
    },
    MenuGroupSpec {
        group: MenuGroup::Help,
        sections: &[&[GroupCommand::CheckForUpdates, GroupCommand::About]],
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
        for section in command.submenu_sections() {
            for child in *section {
                visit(*child, out);
            }
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
    use super::{
        CONTENT_INDEX_SECTIONS, GroupCommand, MenuGroup, PROPERTY_INDEX_SECTIONS, RESCAN_SECTIONS,
        ZoomPreset, all_commands, group_spec, menu_groups,
    };
    use crate::shell::i18n::I18nManager;
    use crate::table::model::{SortColumn, TableColumn};
    use cairn_ui::theme::{MAX_ZOOM, MIN_ZOOM};

    #[test]
    fn every_group_label_is_a_localized_message() {
        let i18n = I18nManager::default();
        assert_eq!(menu_groups().len(), 7, "the title bar owns seven menus");
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

    /// Rename is a read-modify command on one row, so it sits with the other row commands in the
    /// File menu rather than in the destructive section.
    #[test]
    fn the_file_menu_offers_rename_beside_the_row_commands() {
        let file = group_spec(MenuGroup::File);
        let row_section = file
            .sections
            .first()
            .expect("the File menu has a row section");
        assert!(row_section.contains(&GroupCommand::RenameSelected));
        assert!(
            !file.sections[1].contains(&GroupCommand::RenameSelected),
            "rename is not a destructive command"
        );
    }

    /// The Help menu keeps the update entry and About in one group, update first.
    #[test]
    fn the_help_menu_offers_updates_before_about() {
        let help = group_spec(MenuGroup::Help);
        assert_eq!(help.sections.len(), 1);
        assert_eq!(
            help.sections[0],
            &[GroupCommand::CheckForUpdates, GroupCommand::About][..]
        );
    }

    #[test]
    fn index_and_tools_menus_separate_index_operations_from_preferences() {
        let tools = group_spec(MenuGroup::Tools);
        let tool_commands: Vec<GroupCommand> = tools
            .sections
            .iter()
            .flat_map(|section| section.iter().copied())
            .collect();
        assert_eq!(
            tool_commands.len(),
            2,
            "Tools keeps only language and autostart"
        );
        assert!(tool_commands.contains(&GroupCommand::ToggleAutostart));
        assert!(
            tool_commands
                .iter()
                .any(|cmd| matches!(cmd, GroupCommand::Submenu("settings_section_language", _)))
        );

        let index = group_spec(MenuGroup::Index);
        let index_commands: Vec<GroupCommand> = index
            .sections
            .iter()
            .flat_map(|section| section.iter().copied())
            .collect();
        assert_eq!(
            index_commands,
            vec![
                GroupCommand::OpenIndexJournal,
                GroupCommand::ManageFolderSources,
                GroupCommand::RescanMenu,
                GroupCommand::ContentIndexMenu,
                GroupCommand::PropertyIndexMenu,
            ]
        );
    }

    #[test]
    fn index_submenus_carry_their_documented_switches_facts_and_actions() {
        let rescan_children: Vec<GroupCommand> = GroupCommand::RescanMenu
            .submenu_sections()
            .iter()
            .flat_map(|section| section.iter().copied())
            .collect();
        assert_eq!(GroupCommand::RescanMenu.submenu_sections(), RESCAN_SECTIONS);
        assert_eq!(rescan_children, vec![GroupCommand::RescanAll]);

        let content_children: Vec<GroupCommand> = GroupCommand::ContentIndexMenu
            .submenu_sections()
            .iter()
            .flat_map(|section| section.iter().copied())
            .collect();
        assert_eq!(
            GroupCommand::ContentIndexMenu.submenu_sections(),
            CONTENT_INDEX_SECTIONS
        );
        assert_eq!(
            content_children,
            vec![
                GroupCommand::ToggleContentSearch,
                GroupCommand::ContentSearchDetails,
                GroupCommand::ContentSearchUsage,
                GroupCommand::ContentSearchUpdated,
                GroupCommand::RetryContentSearch,
                GroupCommand::ClearContentSearch,
            ]
        );

        let property_children: Vec<GroupCommand> = GroupCommand::PropertyIndexMenu
            .submenu_sections()
            .iter()
            .flat_map(|section| section.iter().copied())
            .collect();
        assert_eq!(
            GroupCommand::PropertyIndexMenu.submenu_sections(),
            PROPERTY_INDEX_SECTIONS
        );
        assert_eq!(
            property_children,
            vec![
                GroupCommand::TogglePropertyIndex,
                GroupCommand::PropertyIndexStatus,
                GroupCommand::PropertyIndexUsage,
            ]
        );
    }

    #[test]
    fn radio_choices_match_only_mutually_exclusive_option_families() {
        assert!(GroupCommand::SortBy(SortColumn::Name).is_radio_choice());
        assert!(GroupCommand::SetTheme(crate::shell::settings::THEME_SYSTEM).is_radio_choice());
        assert!(GroupCommand::SetZoom(ZoomPreset::Standard).is_radio_choice());
        assert!(
            GroupCommand::SetLanguage(crate::shell::settings::LANGUAGE_SYSTEM).is_radio_choice()
        );

        assert!(!GroupCommand::ReverseOrder.is_radio_choice());
        assert!(!GroupCommand::ToggleColumn(TableColumn::Path).is_radio_choice());
        assert!(!GroupCommand::ToggleCase.is_radio_choice());
        assert!(!GroupCommand::ToggleAutostart.is_radio_choice());
        assert!(!GroupCommand::ToggleContentSearch.is_radio_choice());
        assert!(!GroupCommand::TogglePropertyIndex.is_radio_choice());
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
                | GroupCommand::ToggleColumn(_)
                | GroupCommand::SetTheme(_)
                | GroupCommand::SetZoom(_)
                | GroupCommand::Submenu(_, _)
                | GroupCommand::SetLanguage(_)
                | GroupCommand::ToggleAutostart
                | GroupCommand::TogglePinyin
                | GroupCommand::ToggleIgnorePunctuation
                | GroupCommand::ToggleIgnoreWhitespace
                | GroupCommand::ToggleIgnoreDiacritics
                | GroupCommand::ToggleExcludeHidden
                | GroupCommand::ToggleExcludeSystem
                | GroupCommand::OpenIndexJournal
                | GroupCommand::ManageFolderSources
                | GroupCommand::RescanMenu
                | GroupCommand::RescanAll
                | GroupCommand::ContentIndexMenu
                | GroupCommand::ToggleContentSearch
                | GroupCommand::ContentSearchDetails
                | GroupCommand::ContentSearchUsage
                | GroupCommand::ContentSearchUpdated
                | GroupCommand::RetryContentSearch
                | GroupCommand::ClearContentSearch
                | GroupCommand::PropertyIndexMenu
                | GroupCommand::TogglePropertyIndex
                | GroupCommand::PropertyIndexStatus
                | GroupCommand::PropertyIndexUsage
                | GroupCommand::EmptyRecycleBin
                | GroupCommand::CheckForUpdates
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

    /// `run_group_command` dispatches a command through its Action when it has one, so its
    /// retained match may only cover the commands without an Action. This lists the arms that
    /// match really keeps: a command whose Action appears here, or a command without one that is
    /// missing, would reach the fallback `unreachable!` instead of its owner method.
    #[test]
    fn the_directly_dispatched_commands_are_exactly_the_ones_without_an_action() {
        let direct = [
            GroupCommand::ClearQuery,
            GroupCommand::EmptyRecycleBin,
            GroupCommand::SortBy(SortColumn::Name),
            GroupCommand::ReverseOrder,
            GroupCommand::ToggleColumn(TableColumn::Name),
            GroupCommand::SetTheme(crate::shell::settings::THEME_SYSTEM),
            GroupCommand::SetZoom(ZoomPreset::Standard),
            GroupCommand::Submenu("menu_sort", &[]),
            GroupCommand::SetLanguage(crate::shell::settings::LANGUAGE_SYSTEM),
            GroupCommand::ToggleAutostart,
            GroupCommand::TogglePinyin,
            GroupCommand::ToggleIgnorePunctuation,
            GroupCommand::ToggleIgnoreWhitespace,
            GroupCommand::ToggleIgnoreDiacritics,
            GroupCommand::ToggleExcludeHidden,
            GroupCommand::ToggleExcludeSystem,
            GroupCommand::OpenIndexJournal,
            GroupCommand::ManageFolderSources,
            GroupCommand::RescanMenu,
            GroupCommand::RescanAll,
            GroupCommand::ContentIndexMenu,
            GroupCommand::ToggleContentSearch,
            GroupCommand::ContentSearchDetails,
            GroupCommand::ContentSearchUsage,
            GroupCommand::ContentSearchUpdated,
            GroupCommand::RetryContentSearch,
            GroupCommand::ClearContentSearch,
            GroupCommand::PropertyIndexMenu,
            GroupCommand::TogglePropertyIndex,
            GroupCommand::PropertyIndexStatus,
            GroupCommand::PropertyIndexUsage,
            GroupCommand::CheckForUpdates,
            GroupCommand::About,
        ];
        for command in all_commands() {
            let arm_matches = direct.iter().any(|candidate| {
                core::mem::discriminant(candidate) == core::mem::discriminant(&command)
            });
            assert_eq!(
                arm_matches,
                command.action().is_none(),
                "{command:?} does not agree with the arms run_group_command keeps"
            );
        }
    }

    /// The Edit menu's 清空搜索 row dispatches the command itself; Escape is only the hint beside
    /// its label. Dispatching `EscapeQuery` would close the menu and return before touching the query.
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
                    seen.insert(key),
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

    /// The base sort commands cover every QRY-05 column, so the 查看 menu cannot drift from the
    /// required sort order.
    #[test]
    fn base_sort_columns_are_listed_once() {
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
                SortColumn::DateModified,
                SortColumn::DateCreated,
                SortColumn::Extension
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
        assert_eq!(
            ZoomPreset::step_up(ZoomPreset::Compact.value()),
            ZoomPreset::Standard
        );
        assert_eq!(
            ZoomPreset::step_up(ZoomPreset::Standard.value()),
            ZoomPreset::Relaxed
        );
        assert_eq!(
            ZoomPreset::step_up(ZoomPreset::Relaxed.value()),
            ZoomPreset::Relaxed
        );
        assert_eq!(
            ZoomPreset::step_down(ZoomPreset::Relaxed.value()),
            ZoomPreset::Standard
        );
        assert_eq!(
            ZoomPreset::step_down(ZoomPreset::Standard.value()),
            ZoomPreset::Compact
        );
        assert_eq!(
            ZoomPreset::step_down(ZoomPreset::Compact.value()),
            ZoomPreset::Compact
        );
    }

    #[test]
    fn search_menu_includes_hidden_and_system_exclusion_toggles() {
        let search = group_spec(MenuGroup::Search);
        let commands: Vec<GroupCommand> = search
            .sections
            .iter()
            .flat_map(|section| section.iter().copied())
            .collect();
        assert!(commands.contains(&GroupCommand::ToggleExcludeHidden));
        assert!(commands.contains(&GroupCommand::ToggleExcludeSystem));
    }

    #[test]
    fn view_menu_offers_all_table_columns_with_name_locked() {
        use crate::table::model::TableColumn;

        let view = group_spec(MenuGroup::View);
        let commands: Vec<GroupCommand> = view
            .sections
            .iter()
            .flat_map(|section| section.iter().copied())
            .flat_map(|cmd| {
                let mut flat = vec![cmd];
                for sub_section in cmd.submenu_sections() {
                    flat.extend(sub_section.iter().copied());
                }
                flat
            })
            .collect();
        for col in TableColumn::ALL {
            assert!(commands.contains(&GroupCommand::ToggleColumn(col)));
        }
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
                MenuGroup::Index,
                MenuGroup::Tools,
                MenuGroup::Help,
            ]
        );
    }
}
