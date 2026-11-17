// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Read-only modal viewer for the service's bounded in-memory index change journal.

use crate::app::{layout, CairnApp};
use crate::shell::dialogs;
use crate::shell::i18n::I18nManager;
use crate::shell::status_bar::format_count;
use crate::shell::theme::{tabular_figures, ShellTokens, HAIRLINE};
use crate::table::model::format_timestamp;
use cairn_core::format_size_bytes;
use cairn_protocol::status::{IndexJournalEntry, IndexJournalStatus, JournalChangeKind};
use fluent_bundle::FluentArgs;
use gpui_kit::assets::IconName as AssetIconName;
use gpui_kit::base::Disableable;
use gpui_kit::component::button::Button;
use gpui_kit::component::dialog::{DialogClose, DialogEntrance, DialogFooter};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Sizable as _, WindowExt as _};
use gpui_kit::gpui::prelude::FluentBuilder as _;
use gpui_kit::gpui::*;

/// Key for the concise change-type label rendered beside each journal entry.
pub(crate) const fn change_kind_label_key(kind: JournalChangeKind) -> &'static str {
    match kind {
        JournalChangeKind::Created => "journal_kind_created",
        JournalChangeKind::Modified => "journal_kind_modified",
        JournalChangeKind::Renamed => "journal_kind_renamed",
        JournalChangeKind::Deleted => "journal_kind_deleted",
        JournalChangeKind::AttributeChanged => "journal_kind_attribute_changed",
        JournalChangeKind::OverflowRescan => "journal_kind_overflow_rescan",
    }
}

/// One-line recording status summary displayed next to the recording switch.
pub(crate) fn journal_summary_text(
    i18n: &I18nManager,
    connected: bool,
    status: &IndexJournalStatus,
    loading: bool,
    error: Option<&str>,
) -> String {
    if !connected {
        return i18n.get_text("journal_dialog_status_unavailable");
    }
    if error.is_some() {
        return i18n.get_text("journal_dialog_status_error");
    }
    if loading && status.entry_count == 0 {
        return i18n.get_text("journal_dialog_status_loading");
    }
    if !status.enabled {
        return i18n.get_text("journal_dialog_status_off");
    }
    let mut args = FluentArgs::new();
    args.set("count", format_count(status.entry_count));
    args.set("size", format_size_bytes(status.resident_bytes as u64));
    i18n.get_text_with_args("journal_dialog_status_summary", &args)
}

impl CairnApp {
    /// Opens the read-only Index Journal dialog and refreshes recent entries from the service.
    pub(crate) fn open_index_journal_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.service.connection.is_connected() {
            self.refresh_index_journal_dialog(cx);
        }
        let view = cx.entity();
        window.open_dialog(cx, move |dialog, window, cx| {
            render_index_journal_dialog(&view, dialog, window, cx)
        });
    }
}

fn render_index_journal_dialog(
    view: &Entity<CairnApp>,
    dialog: gpui_kit::component::dialog::Dialog,
    window: &mut Window,
    cx: &App,
) -> gpui_kit::component::dialog::Dialog {
    let app = view.read(cx);
    let tokens = ShellTokens::current(cx);
    let surface = cairn_ui::theme::dialog_surface(cx.theme());
    let rem = window.rem_size();
    let list_max_height = (window.viewport_size().height * 0.5).clamp(rem * 12.0, rem * 24.0);

    let connected = app.service.connection.is_connected();
    let status = app.service.journal.value.clone();
    let entries = app.service.journal_entries.value.clone();
    let loading = app.service.journal_entries.is_loading() || app.service.journal.is_loading();
    let entries_error = app.service.journal_entries.load_error().map(str::to_owned);
    let status_error = app.service.journal.load_error().map(str::to_owned);
    let error_text = entries_error.as_deref().or(status_error.as_deref());
    let menu_state = app.index_menu_state();
    let switch_disabled = menu_state.journal_switch_is_disabled();
    let switch_checked = menu_state.journal_is_on();

    let title_text = app.i18n.get_text("journal_dialog_title");
    let switch_label = app.i18n.get_text("journal_dialog_record_toggle");
    let summary = journal_summary_text(&app.i18n, connected, &status, loading, error_text);
    let refresh_label = app.i18n.get_text("journal_dialog_refresh");
    let close_label = app.i18n.get_text("journal_dialog_close");
    let col_time = app.i18n.get_text("journal_dialog_col_time");
    let col_kind = app.i18n.get_text("journal_dialog_col_kind");
    let col_path = app.i18n.get_text("journal_dialog_col_path");
    let loading_text = app.i18n.get_text("journal_dialog_status_loading");
    let unavailable_text = app.i18n.get_text("journal_dialog_empty_unavailable");
    let off_text = app.i18n.get_text("journal_dialog_empty_off");
    let empty_text = app.i18n.get_text("journal_dialog_empty");
    let error_body = app.i18n.get_text("journal_dialog_error_body");

    let switch_view = view.clone();
    let refresh_view = view.clone();

    let controls = h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap(rems(layout::SPACE_8))
        .child(
            h_flex()
                .min_w_0()
                .flex_1()
                .items_center()
                .gap(rems(layout::SPACE_12))
                .child(
                    Switch::new("index-journal-recording-switch")
                        .small()
                        .checked(switch_checked)
                        .disabled(switch_disabled)
                        .label(switch_label)
                        .on_change(move |enabled, _, cx| {
                            switch_view.update(cx, |this, cx| {
                                this.set_index_journal_enabled(*enabled, cx);
                            });
                        }),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .text_sm()
                        .text_color(tokens.muted_foreground)
                        .child(summary),
                ),
        )
        .child(
            Button::new("index-journal-refresh")
                .outline()
                .small()
                .label(refresh_label)
                .disabled(!connected || loading)
                .loading(loading)
                .on_click(move |_, _, cx| {
                    refresh_view.update(cx, |this, cx| {
                        this.refresh_index_journal_dialog(cx);
                    });
                }),
        );

    let list_body = if !connected {
        empty_panel(unavailable_text, &tokens)
    } else if error_text.is_some() {
        empty_panel(error_body, &tokens)
    } else if !status.enabled {
        empty_panel(off_text, &tokens)
    } else if entries.is_empty() && loading {
        h_flex()
            .w_full()
            .min_h(rems(10.0))
            .items_center()
            .justify_center()
            .gap(rems(layout::SPACE_8))
            .text_sm()
            .text_color(tokens.muted_foreground)
            .child(
                Spinner::new()
                    .icon(AssetIconName::LoaderCircle)
                    .small()
                    .color(tokens.muted_foreground),
            )
            .child(loading_text)
            .into_any_element()
    } else if entries.is_empty() {
        empty_panel(empty_text, &tokens)
    } else {
        let rows = render_journal_rows(
            &entries,
            &app.i18n,
            &col_time,
            &col_kind,
            &col_path,
            list_max_height,
            &tokens,
        );
        rows.into_any_element()
    };

    let surface_box = div()
        .w_full()
        .rounded(tokens.radius)
        .border(HAIRLINE)
        .border_color(tokens.border)
        .bg(tokens.background)
        .overflow_hidden()
        .child(list_body);

    let content = v_flex()
        .w_full()
        .gap(rems(layout::SPACE_12))
        .child(controls)
        .child(surface_box);

    dialog
        .width(rem * layout::DIALOG_WIDTH)
        .centered()
        .bg(surface)
        .entrance(DialogEntrance::FadeSlide)
        .title(dialogs::title(title_text, window))
        .child(content)
        .footer(
            DialogFooter::new().child(
                DialogClose::new().child(
                    Button::new("index-journal-close")
                        .outline()
                        .label(close_label),
                ),
            ),
        )
}

fn empty_panel(message: String, tokens: &ShellTokens) -> AnyElement {
    div()
        .w_full()
        .min_h(rems(10.0))
        .p(rems(layout::SPACE_12))
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .text_center()
        .text_color(tokens.muted_foreground)
        .child(message)
        .into_any_element()
}

fn render_journal_rows(
    entries: &[IndexJournalEntry],
    i18n: &I18nManager,
    col_time: &str,
    col_kind: &str,
    col_path: &str,
    list_max_height: Pixels,
    tokens: &ShellTokens,
) -> impl IntoElement {
    let time_col_w = rems(8.75);
    let kind_col_w = rems(6.0);

    let header = h_flex()
        .w_full()
        .h(rems(layout::ROW_HEIGHT))
        .px(rems(layout::SPACE_8))
        .gap(rems(layout::SPACE_8))
        .items_center()
        .bg(tokens.table_head)
        .border_b(HAIRLINE)
        .border_color(tokens.border)
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .text_color(tokens.table_head_foreground)
        .child(
            div()
                .w(time_col_w)
                .flex_shrink_0()
                .child(col_time.to_string()),
        )
        .child(
            div()
                .w(kind_col_w)
                .flex_shrink_0()
                .child(col_kind.to_string()),
        )
        .child(div().min_w_0().flex_1().child(col_path.to_string()));

    let scroll_list = v_flex()
        .id("index-journal-entries")
        .w_full()
        .max_h(list_max_height)
        .overflow_y_scroll()
        .children(entries.iter().enumerate().map(|(ix, entry)| {
            let timestamp = format_timestamp(entry.timestamp_ts);
            let kind_label = i18n.get_text(change_kind_label_key(entry.kind));
            let old_path_line = entry.old_path.as_ref().map(|old| {
                let mut args = FluentArgs::new();
                args.set("path", old.as_str());
                i18n.get_text_with_args("journal_dialog_renamed_from", &args)
            });

            h_flex()
                .id(("index-journal-row", ix))
                .w_full()
                .min_h(rems(layout::ROW_HEIGHT))
                .px(rems(layout::SPACE_8))
                .py(rems(layout::TIGHT_LEAD))
                .gap(rems(layout::SPACE_4))
                .items_start()
                .border_b(HAIRLINE)
                .border_color(tokens.border)
                .hover(|this| this.bg(tokens.row_hover))
                .child(
                    h_flex()
                        .w_full()
                        .gap(rems(layout::SPACE_8))
                        .items_center()
                        .child(
                            div()
                                .w(time_col_w)
                                .flex_shrink_0()
                                .font_features(tabular_figures())
                                .text_sm()
                                .text_color(tokens.muted_foreground)
                                .child(timestamp),
                        )
                        .child(
                            div()
                                .w(kind_col_w)
                                .flex_shrink_0()
                                .text_sm()
                                .text_color(tokens.foreground)
                                .child(kind_label),
                        ),
                )
                .child(
                    v_flex()
                        .w_full()
                        .gap(rems(layout::SPACE_4))
                        .child(
                            div()
                                .w_full()
                                .whitespace_normal()
                                .text_sm()
                                .text_color(tokens.foreground)
                                .child(entry.path.clone()),
                        )
                        .when_some(old_path_line, |this, old| {
                            this.child(
                                div()
                                    .w_full()
                                    .whitespace_normal()
                                    .text_sm()
                                    .text_color(tokens.muted_foreground)
                                    .child(old),
                            )
                        }),
                )
        }));

    v_flex().w_full().child(header).child(scroll_list)
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::{change_kind_label_key, journal_summary_text};
    use crate::shell::i18n::I18nManager;
    use cairn_protocol::status::{IndexJournalStatus, JournalChangeKind};

    #[test]
    fn change_kind_labels_resolve_without_mid_dots_in_both_locales() {
        for kind in [
            JournalChangeKind::Created,
            JournalChangeKind::Modified,
            JournalChangeKind::Renamed,
            JournalChangeKind::Deleted,
            JournalChangeKind::AttributeChanged,
            JournalChangeKind::OverflowRescan,
        ] {
            let key = change_kind_label_key(kind);
            for locale in ["zh-CN", "en-US"] {
                let i18n = I18nManager::new(locale);
                let text = i18n
                    .try_text(key)
                    .unwrap_or_else(|| panic!("{key} missing in {locale}"));
                assert!(!text.trim().is_empty());
                assert!(!text.contains('·'), "mid-dot forbidden in {key}: {text}");
            }
        }
    }

    #[test]
    fn summary_text_reports_connection_off_error_and_counts_honestly() {
        let i18n = I18nManager::new("en-US");
        let off = IndexJournalStatus {
            enabled: false,
            ..Default::default()
        };
        let on = IndexJournalStatus {
            enabled: true,
            entry_count: 12,
            resident_bytes: 2048,
            ..Default::default()
        };

        assert_eq!(
            journal_summary_text(&i18n, false, &on, false, None),
            i18n.get_text("journal_dialog_status_unavailable")
        );
        assert_eq!(
            journal_summary_text(&i18n, true, &on, false, Some("pipe failed")),
            i18n.get_text("journal_dialog_status_error")
        );
        assert_eq!(
            journal_summary_text(&i18n, true, &off, false, None),
            i18n.get_text("journal_dialog_status_off")
        );
        let ready = journal_summary_text(&i18n, true, &on, false, None);
        assert!(ready.contains("12"), "expected entry count in {ready}");
        assert!(!ready.contains('·'));
    }
}
