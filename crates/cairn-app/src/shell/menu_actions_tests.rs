// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use super::*;
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
        let load_gen = state.begin_load().expect("idle subsystem starts load");
        assert!(state.fail_load_if_current(load_gen, "status"));
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
    use super::{CairnApp, ContentIndexState, EscapeQuery, GroupCommand, SelectNextRow};
    use crate::search::ResultSetChange;
    use crate::shell::menu_popup::menu_entries;
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
        use crate::table::columns::{SortColumn, SortDirection, SortSpec};

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
    use crate::shell::command_groups::MenuGroup;
    use crate::shell::menu_popup::{GroupHoverAction, decide_group_hover};

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
