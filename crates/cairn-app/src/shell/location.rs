// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Location capabilities and UI/command policy shared across the application shell.

use crate::app::CairnApp;
use crate::search::QueryToggle;
use crate::shell::command_groups::GroupCommand;
use crate::shell::settings::ColumnSettings;
use crate::table::model::{SortColumn, SortColumnLabel, TableColumn};

/// Top-level window location owning the result list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActiveLocation {
    Index,
    RecycleBin,
}

impl ActiveLocation {
    pub(crate) const fn is_index(self) -> bool {
        matches!(self, Self::Index)
    }

    pub(crate) const fn is_recycle_bin(self) -> bool {
        matches!(self, Self::RecycleBin)
    }
}

pub(crate) const INDEX_CONTEXT_COMMANDS: &[GroupCommand] = &[
    GroupCommand::OpenSelected,
    GroupCommand::RenameSelected,
    GroupCommand::RevealLocation,
    GroupCommand::CopyPaths,
    GroupCommand::CopyNames,
    GroupCommand::SelectAll,
    GroupCommand::DeleteSelected,
    GroupCommand::ShowProperties,
];

pub(crate) const RECYCLE_BIN_CONTEXT_COMMANDS: &[GroupCommand] = &[
    GroupCommand::OpenSelected,
    GroupCommand::RestoreSelected,
    GroupCommand::CopyPaths,
    GroupCommand::CopyNames,
    GroupCommand::SelectAll,
    GroupCommand::DeleteSelected,
];

/// Declarative location capabilities replacing scattered `recycle_bin.is_open()` checks.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LocationProfile {
    pub kind: ActiveLocation,
    pub is_local_listing: bool,
    pub allows_column_customization: bool,
    pub preserves_selection_on_chrome_press: bool,
    pub supports_exclusion_filters: bool,
    pub supports_rename: bool,
    pub supports_reveal: bool,
    pub supports_properties_dialog: bool,
    pub supports_snippets: bool,
    pub supports_degraded_notice: bool,
    pub records_nav_history: bool,
    pub search_placeholder_key: &'static str,
    pub search_aria_label_key: Option<&'static str>,
}

impl LocationProfile {
    pub const INDEX: Self = Self {
        kind: ActiveLocation::Index,
        is_local_listing: false,
        allows_column_customization: true,
        preserves_selection_on_chrome_press: false,
        supports_exclusion_filters: true,
        supports_rename: true,
        supports_reveal: true,
        supports_properties_dialog: true,
        supports_snippets: true,
        supports_degraded_notice: true,
        records_nav_history: true,
        search_placeholder_key: "search_placeholder",
        search_aria_label_key: None,
    };

    pub const RECYCLE_BIN: Self = Self {
        kind: ActiveLocation::RecycleBin,
        is_local_listing: true,
        allows_column_customization: false,
        preserves_selection_on_chrome_press: true,
        supports_exclusion_filters: false,
        supports_rename: false,
        supports_reveal: false,
        supports_properties_dialog: false,
        supports_snippets: false,
        supports_degraded_notice: false,
        records_nav_history: false,
        search_placeholder_key: "recycle_bin_filter_placeholder",
        search_aria_label_key: Some("recycle_bin_filter_label"),
    };

    pub const fn for_location(kind: ActiveLocation) -> &'static Self {
        match kind {
            ActiveLocation::Index => &Self::INDEX,
            ActiveLocation::RecycleBin => &Self::RECYCLE_BIN,
        }
    }

    pub fn is_column_visible(&self, col: TableColumn, settings: &ColumnSettings) -> bool {
        if col.is_locked() {
            return true;
        }
        if !self.allows_column_customization {
            return matches!(
                col,
                TableColumn::Name
                    | TableColumn::Path
                    | TableColumn::Size
                    | TableColumn::DateModified
            );
        }
        settings.is_column_shown(col)
    }

    pub const fn supports_sort_column(&self, col: SortColumn) -> bool {
        match self.kind {
            ActiveLocation::Index => true,
            ActiveLocation::RecycleBin => matches!(
                col,
                SortColumn::Name
                    | SortColumn::Path
                    | SortColumn::Size
                    | SortColumn::DateModified
                    | SortColumn::Extension
            ),
        }
    }

    pub fn column_label_key(&self, col: SortColumn) -> &'static str {
        match self.kind {
            ActiveLocation::Index => col.label_key(),
            ActiveLocation::RecycleBin => col.recycle_bin_label_key(),
        }
    }

    pub fn command_is_hidden(&self, command: GroupCommand) -> bool {
        match self.kind {
            ActiveLocation::Index => matches!(
                command,
                GroupCommand::RestoreSelected
                    | GroupCommand::RestoreAll
                    | GroupCommand::EmptyRecycleBin
            ),
            ActiveLocation::RecycleBin => match command {
                GroupCommand::RenameSelected
                | GroupCommand::Submenu("menu_columns", _)
                | GroupCommand::ToggleColumn(_)
                | GroupCommand::RevealLocation
                | GroupCommand::ShowProperties
                | GroupCommand::ToggleWholeWord
                | GroupCommand::ToggleMatchPath
                | GroupCommand::ToggleRegex
                | GroupCommand::TogglePinyin
                | GroupCommand::ToggleIgnorePunctuation
                | GroupCommand::ToggleIgnoreWhitespace
                | GroupCommand::ToggleIgnoreDiacritics
                | GroupCommand::ToggleExcludeHidden
                | GroupCommand::ToggleExcludeSystem => true,
                GroupCommand::SortBy(col) => !self.supports_sort_column(col),
                _ => false,
            },
        }
    }

    pub fn command_label_key(&self, command: GroupCommand) -> &'static str {
        match self.kind {
            ActiveLocation::Index => command.label_key(),
            ActiveLocation::RecycleBin => match command {
                GroupCommand::DeleteSelected => "ctx_delete_permanently",
                GroupCommand::CopyPaths => "ctx_copy_original_location",
                GroupCommand::SortBy(column) => self.column_label_key(column),
                _ => command.label_key(),
            },
        }
    }

    pub const fn supports_query_toggle(&self, toggle: QueryToggle) -> bool {
        match self.kind {
            ActiveLocation::Index => true,
            ActiveLocation::RecycleBin => matches!(toggle, QueryToggle::Case),
        }
    }

    pub const fn result_context_commands(&self) -> &'static [GroupCommand] {
        match self.kind {
            ActiveLocation::Index => INDEX_CONTEXT_COMMANDS,
            ActiveLocation::RecycleBin => RECYCLE_BIN_CONTEXT_COMMANDS,
        }
    }
}

impl CairnApp {
    pub(crate) fn active_location(&self) -> ActiveLocation {
        if self.recycle_bin.is_open() {
            ActiveLocation::RecycleBin
        } else {
            ActiveLocation::Index
        }
    }

    pub(crate) fn location_profile(&self) -> &'static LocationProfile {
        LocationProfile::for_location(self.active_location())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::TextModifier;
    use crate::shell::i18n::I18nManager;

    #[test]
    fn context_commands_match_hidden_policy_and_rename_capability() {
        assert!(INDEX_CONTEXT_COMMANDS.contains(&GroupCommand::RenameSelected));
        assert!(!RECYCLE_BIN_CONTEXT_COMMANDS.contains(&GroupCommand::RenameSelected));

        for profile in [&LocationProfile::INDEX, &LocationProfile::RECYCLE_BIN] {
            for &command in profile.result_context_commands() {
                assert!(
                    !profile.command_is_hidden(command),
                    "context command {command:?} is hidden in {:?}",
                    profile.kind
                );
            }
        }
    }

    #[test]
    fn sort_columns_and_query_toggles_agree_with_menu_visibility() {
        let sort_columns = [
            SortColumn::Name,
            SortColumn::Path,
            SortColumn::Size,
            SortColumn::DateModified,
            SortColumn::DateCreated,
            SortColumn::Extension,
            SortColumn::Length,
            SortColumn::Author,
            SortColumn::Tags,
            SortColumn::Dimensions,
            SortColumn::Framerate,
        ];
        let toggle_pairs = [
            (QueryToggle::Case, GroupCommand::ToggleCase),
            (QueryToggle::WholeWord, GroupCommand::ToggleWholeWord),
            (QueryToggle::MatchPath, GroupCommand::ToggleMatchPath),
            (QueryToggle::Regex, GroupCommand::ToggleRegex),
            (
                QueryToggle::Modifier(TextModifier::Pinyin),
                GroupCommand::TogglePinyin,
            ),
            (
                QueryToggle::Modifier(TextModifier::IgnorePunctuation),
                GroupCommand::ToggleIgnorePunctuation,
            ),
            (
                QueryToggle::Modifier(TextModifier::IgnoreWhitespace),
                GroupCommand::ToggleIgnoreWhitespace,
            ),
            (
                QueryToggle::Modifier(TextModifier::IgnoreDiacritics),
                GroupCommand::ToggleIgnoreDiacritics,
            ),
        ];

        for profile in [&LocationProfile::INDEX, &LocationProfile::RECYCLE_BIN] {
            for col in sort_columns {
                assert_eq!(
                    profile.supports_sort_column(col),
                    !profile.command_is_hidden(GroupCommand::SortBy(col)),
                    "sort column {col:?} mismatch in {:?}",
                    profile.kind
                );
            }
            for (toggle, cmd) in toggle_pairs {
                assert_eq!(
                    profile.supports_query_toggle(toggle),
                    !profile.command_is_hidden(cmd),
                    "toggle {toggle:?} mismatch in {:?}",
                    profile.kind
                );
            }
        }
    }

    #[test]
    fn location_label_keys_exist_in_all_locales() {
        let commands_to_check = [
            GroupCommand::OpenSelected,
            GroupCommand::RenameSelected,
            GroupCommand::RestoreSelected,
            GroupCommand::CopyPaths,
            GroupCommand::CopyNames,
            GroupCommand::DeleteSelected,
            GroupCommand::SortBy(SortColumn::Path),
            GroupCommand::SortBy(SortColumn::DateModified),
        ];
        for locale in ["zh-CN", "en-US"] {
            let i18n = I18nManager::new(locale);
            for profile in [&LocationProfile::INDEX, &LocationProfile::RECYCLE_BIN] {
                let placeholder = i18n.get_text(profile.search_placeholder_key);
                assert!(
                    !placeholder.trim().is_empty() && placeholder != profile.search_placeholder_key,
                    "missing placeholder key `{}` for {locale}",
                    profile.search_placeholder_key
                );
                if let Some(aria_key) = profile.search_aria_label_key {
                    let aria = i18n.get_text(aria_key);
                    assert!(
                        !aria.trim().is_empty() && aria != aria_key,
                        "missing aria key `{aria_key}` for {locale}"
                    );
                }
                for col in [
                    SortColumn::Name,
                    SortColumn::Path,
                    SortColumn::Size,
                    SortColumn::DateModified,
                    SortColumn::DateCreated,
                ] {
                    let key = profile.column_label_key(col);
                    let text = i18n.get_text(key);
                    assert!(
                        !text.trim().is_empty() && text != key,
                        "missing column key `{key}` for {locale}"
                    );
                }
                for cmd in commands_to_check {
                    let key = profile.command_label_key(cmd);
                    let text = i18n.get_text(key);
                    assert!(
                        !text.trim().is_empty() && text != key,
                        "missing command key `{key}` for {locale}"
                    );
                }
            }
        }
    }
}
