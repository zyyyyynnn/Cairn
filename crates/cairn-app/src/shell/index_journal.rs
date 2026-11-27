// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Read-only modal viewer for the service's bounded in-memory index change journal.

use crate::app::{CairnApp, layout};
use crate::shell::dialogs;
use crate::shell::i18n::I18nManager;
use crate::shell::status_bar::format_count;
use crate::shell::theme::{HAIRLINE, ShellTokens, overlay_scrollbar, tabular_figures};
use crate::table::model::format_timestamp;
use cairn_core::format_size_bytes;
use cairn_protocol::status::{IndexJournalEntry, IndexJournalStatus, JournalChangeKind};
use fluent_bundle::FluentArgs;
use gpui_kit::assets::IconName as AssetIconName;
use gpui_kit::base::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
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
        cx: &mut Context<'_, Self>,
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
    cx: &mut App,
) -> gpui_kit::component::dialog::Dialog {
    let scroll_handle = window
        .use_keyed_state("index-journal-scroll", cx, |_, _| ScrollHandle::new())
        .read(cx)
        .clone();
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
    let close_label = app.i18n.get_text("window_close");
    let col_time = app.i18n.get_text("journal_dialog_col_time");
    let col_kind = app.i18n.get_text("journal_dialog_col_kind");
    let col_path = app.i18n.get_text("journal_dialog_col_path");
    let loading_text = app.i18n.get_text("journal_dialog_status_loading");
    let unavailable_text = app.i18n.get_text("journal_dialog_empty_unavailable");
    let off_text = app.i18n.get_text("journal_dialog_empty_off");
    let empty_text = app.i18n.get_text("journal_dialog_empty");
    let error_body = app.i18n.get_text("journal_dialog_error_body");

    let title_row = render_journal_title_row(
        view,
        JournalTitleRow {
            title_text,
            switch_label,
            switch_checked,
            switch_disabled,
            refresh_label,
            close_label,
            refresh_disabled: !connected || loading,
            refresh_loading: loading,
        },
        window,
    );
    let status_line = h_flex()
        .w_full()
        .h(rems(layout::ROW_HEIGHT))
        .items_center()
        .px(rems(layout::SPACE_8))
        .text_sm()
        .font_features(tabular_figures())
        .text_color(tokens.muted_foreground)
        .child(summary);

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
        let columns = JournalColumns {
            time: &col_time,
            kind: &col_kind,
            path: &col_path,
        };
        let list_height = (rem * (entries.len() as f32 * layout::ROW_HEIGHT)).min(list_max_height);
        let rows = render_journal_rows(
            &entries,
            &app.i18n,
            &columns,
            list_height,
            &scroll_handle,
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
        .child(list_body)
        .child(status_line);

    let content = v_flex()
        .w_full()
        .pt(rems(layout::SPACE_8))
        .child(surface_box);

    dialog
        .width(rem * layout::DIALOG_WIDE_WIDTH)
        .centered()
        .bg(surface)
        .entrance(DialogEntrance::FadeSlide)
        .close_button(false)
        .title(title_row)
        .child(content)
}

struct JournalTitleRow {
    title_text: String,
    switch_label: String,
    switch_checked: bool,
    switch_disabled: bool,
    refresh_label: String,
    close_label: String,
    refresh_disabled: bool,
    refresh_loading: bool,
}

fn build_journal_header_parts(
    view: &Entity<CairnApp>,
    props: JournalTitleRow,
    window: &Window,
) -> (Div, AnyElement) {
    let JournalTitleRow {
        title_text,
        switch_label,
        switch_checked,
        switch_disabled,
        refresh_label,
        close_label,
        refresh_disabled,
        refresh_loading,
    } = props;
    let switch_view = view.clone();
    let refresh_view = view.clone();

    let leading = h_flex()
        .items_center()
        .gap(rems(layout::SPACE_8))
        .child(dialogs::title(title_text, window))
        .child(
            Switch::new("index-journal-recording-switch")
                .small()
                .checked(switch_checked)
                .disabled(switch_disabled)
                .accessibility_label(switch_label.clone())
                .tooltip(switch_label)
                .on_change(move |enabled, _, cx| {
                    switch_view.update(cx, |this, cx| {
                        this.set_index_journal_enabled(*enabled, cx);
                    });
                }),
        )
        .child(
            Button::new("index-journal-refresh")
                .ghost()
                .small()
                .icon(AssetIconName::RefreshCw)
                .loading_icon(AssetIconName::LoaderCircle)
                .accessibility_label(refresh_label.clone())
                .tooltip(refresh_label)
                .disabled(refresh_disabled)
                .loading(refresh_loading)
                .on_click(move |_, _, cx| {
                    refresh_view.update(cx, |this, cx| {
                        this.refresh_index_journal_dialog(cx);
                    });
                }),
        );

    let close = Button::new("index-journal-close")
        .ghost()
        .small()
        .icon(AssetIconName::X)
        .accessibility_label(close_label.clone())
        .tooltip(close_label)
        .on_click(|_, window, cx| window.close_dialog(cx))
        .into_any_element();

    (leading, close)
}

fn build_journal_header_row(leading: Div, close: AnyElement) -> Div {
    h_flex()
        .w_full()
        .pl(rems(layout::SPACE_8))
        .items_center()
        .justify_between()
        .gap(rems(layout::SPACE_8))
        .child(leading)
        .child(close)
}

fn render_journal_title_row(
    view: &Entity<CairnApp>,
    props: JournalTitleRow,
    window: &Window,
) -> AnyElement {
    let (leading, close) = build_journal_header_parts(view, props, window);
    build_journal_header_row(leading, close).into_any_element()
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

fn render_journal_row(
    ix: usize,
    entry: &IndexJournalEntry,
    i18n: &I18nManager,
    time_col_w: Rems,
    kind_col_w: Rems,
    tokens: &ShellTokens,
) -> AnyElement {
    let timestamp = format_timestamp(entry.timestamp_ts);
    let kind_label = i18n.get_text(change_kind_label_key(entry.kind));
    let old_path_line = entry.old_path.as_ref().map(|old| {
        let mut args = FluentArgs::new();
        args.set("path", old.as_str());
        i18n.get_text_with_args("journal_dialog_renamed_from", &args)
    });
    let tooltip = match &old_path_line {
        Some(old) => format!("{}\n{}", entry.path, old),
        None => entry.path.clone(),
    };

    h_flex()
        .id(("index-journal-row", ix))
        .w_full()
        .h(rems(layout::ROW_HEIGHT))
        .flex_shrink_0()
        .items_center()
        .gap(rems(layout::SPACE_8))
        .px(rems(layout::SPACE_8))
        .border_b(HAIRLINE)
        .border_color(tokens.border)
        .hover(|this| this.bg(tokens.row_hover))
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
        )
        .child(
            div()
                .id(("index-journal-path", ix))
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .text_color(tokens.foreground)
                .tooltip(cairn_ui::tooltip::text(tooltip))
                .child(entry.path.clone()),
        )
        .into_any_element()
}

struct JournalColumns<'a> {
    time: &'a str,
    kind: &'a str,
    path: &'a str,
}

fn build_journal_rows_list(
    entries: &[IndexJournalEntry],
    i18n: &I18nManager,
    time_col_w: Rems,
    kind_col_w: Rems,
    tokens: &ShellTokens,
) -> Div {
    v_flex().w_full().children(
        entries
            .iter()
            .enumerate()
            .map(|(ix, entry)| render_journal_row(ix, entry, i18n, time_col_w, kind_col_w, tokens)),
    )
}

fn render_journal_rows(
    entries: &[IndexJournalEntry],
    i18n: &I18nManager,
    columns: &JournalColumns<'_>,
    list_height: Pixels,
    scroll_handle: &ScrollHandle,
    tokens: &ShellTokens,
) -> impl IntoElement {
    let time_col_w = rems(8.75);
    let kind_col_w = rems(6.0);

    let header = h_flex()
        .w_full()
        .flex_shrink_0()
        .h(rems(layout::ROW_HEIGHT))
        .items_center()
        .gap(rems(layout::SPACE_8))
        .px(rems(layout::SPACE_8))
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
                .child(columns.time.to_string()),
        )
        .child(
            div()
                .w(kind_col_w)
                .flex_shrink_0()
                .child(columns.kind.to_string()),
        )
        .child(div().flex_1().min_w_0().child(columns.path.to_string()));

    let scroll_list = build_journal_rows_list(entries, i18n, time_col_w, kind_col_w, tokens)
        .id("index-journal-entries")
        .h(list_height)
        .overflow_y_scroll()
        .track_scroll(scroll_handle);

    let scroller = h_flex()
        .w_full()
        .items_stretch()
        .child(scroll_list.flex_1().min_w_0())
        .child(
            div()
                .flex_shrink_0()
                .w(Scrollbar::width())
                .h(list_height)
                .child(overlay_scrollbar(
                    Scrollbar::vertical(scroll_handle)
                        .id("index-journal-scrollbar")
                        .viewport_from_layout(),
                )),
        );

    v_flex().w_full().child(header).child(scroller)
}

#[cfg(test)]
mod tests {
    use super::{
        JournalTitleRow, build_journal_header_parts, build_journal_header_row,
        change_kind_label_key, journal_summary_text, render_journal_row,
    };
    use crate::app::{CairnApp, layout};
    use crate::shell::i18n::I18nManager;
    use crate::shell::theme::ShellTokens;
    use cairn_protocol::message::SourceId;
    use cairn_protocol::status::{IndexJournalEntry, IndexJournalStatus, JournalChangeKind};
    use gpui_kit::component::v_flex;
    use gpui_kit::gpui::{
        AppContext as _, Bounds, Context, Entity, IntoElement, ParentElement as _, Pixels, Render,
        Styled, Window, div, px, rems,
    };
    use std::cell::RefCell;

    thread_local! {
        static LAST_JOURNAL_ROW_BOUNDS: RefCell<Vec<Bounds<Pixels>>> = const { RefCell::new(Vec::new()) };
        static LAST_HEADER_ROW_BOUNDS: RefCell<Vec<Bounds<Pixels>>> = const { RefCell::new(Vec::new()) };
        static LAST_HEADER_LEADING_BOUNDS: RefCell<Vec<Bounds<Pixels>>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn record_journal_row_bounds(bounds: Vec<Bounds<Pixels>>) {
        LAST_JOURNAL_ROW_BOUNDS.with(|slot| {
            *slot.borrow_mut() = bounds;
        });
    }

    fn take_journal_row_bounds() -> Vec<Bounds<Pixels>> {
        LAST_JOURNAL_ROW_BOUNDS.with(|slot| std::mem::take(&mut *slot.borrow_mut()))
    }

    pub(super) fn record_journal_header_row_bounds(bounds: Vec<Bounds<Pixels>>) {
        LAST_HEADER_ROW_BOUNDS.with(|slot| {
            *slot.borrow_mut() = bounds;
        });
    }

    pub(super) fn record_journal_header_leading_bounds(bounds: Vec<Bounds<Pixels>>) {
        LAST_HEADER_LEADING_BOUNDS.with(|slot| {
            *slot.borrow_mut() = bounds;
        });
    }

    fn take_journal_header_bounds() -> (Vec<Bounds<Pixels>>, Vec<Bounds<Pixels>>) {
        (
            LAST_HEADER_ROW_BOUNDS.with(|slot| std::mem::take(&mut *slot.borrow_mut())),
            LAST_HEADER_LEADING_BOUNDS.with(|slot| std::mem::take(&mut *slot.borrow_mut())),
        )
    }

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

    struct JournalRowsHarness {
        entries: Vec<IndexJournalEntry>,
        i18n: I18nManager,
    }

    impl Render for JournalRowsHarness {
        fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
            window.set_rem_size(gpui_kit::gpui::px(crate::shell::theme::BASE_FONT_PX));
            let rem = window.rem_size();
            let tokens = ShellTokens::current(cx);
            let rows = v_flex().w_full().on_children_prepainted(|bounds, _, _| {
                record_journal_row_bounds(bounds);
            });
            div()
                .w(rem * layout::DIALOG_WIDE_WIDTH)
                .child(
                    rows.children(self.entries.iter().enumerate().map(|(ix, entry)| {
                        render_journal_row(ix, entry, &self.i18n, rems(8.75), rems(6.0), &tokens)
                    })),
                )
        }
    }

    #[test]
    fn journal_rows_keep_a_uniform_height_without_overlapping() {
        gpui_kit::application().run(|cx: &mut gpui_kit::gpui::App| {
            gpui_kit::init(cx);
            let entries: Vec<IndexJournalEntry> = (0..20)
                .map(|ix| IndexJournalEntry {
                    seq: (ix + 1) as u64,
                    timestamp_ts: 1_760_000_000 + ix as i64,
                    source_id: SourceId(1),
                    kind: if ix == 0 {
                        JournalChangeKind::Renamed
                    } else {
                        JournalChangeKind::Modified
                    },
                    path: format!(
                        r"C:\Users\Administrator\Documents\Workspace\Cairn\crates\cairn-service\src\indexer\journal\deeply_nested_module_{ix:02}\very_long_filename_that_wraps_across_multiple_lines_in_narrow_dialog_{ix:02}.rs"
                    ),
                    old_path: (ix == 0).then(|| {
                        r"C:\Users\Administrator\Documents\Workspace\Cairn\crates\cairn-service\src\indexer\journal\deeply_nested_module_00\previous_very_long_filename_before_rename_operation_00.rs"
                            .to_string()
                    }),
                    frn: 100 + ix as u64,
                })
                .collect();

            let (handle, app) = gpui_kit::open_window(
                gpui_kit::gpui::WindowOptions {
                    show: false,
                    focus: false,
                    ..gpui_kit::gpui::WindowOptions::default()
                },
                cx,
                |window, cx| cx.new(|cx| CairnApp::new(window, cx)),
            )
            .expect("test window opens");

            let entries_for_dialog = entries.clone();
            handle
                .update(cx, |_, window, cx| {
                    app.update(cx, |this, cx| {
                        this.open_index_journal_dialog(window, cx);
                        this.service.connection.mark_connected();
                        this.service.journal.finish_load(IndexJournalStatus {
                            enabled: true,
                            entry_count: entries_for_dialog.len(),
                            resident_bytes: 475_648,
                            max_entries: 2_000,
                            max_bytes: 1_048_576,
                            oldest_seq: 1,
                            newest_seq: entries_for_dialog.len() as u64,
                        });
                        this.service.journal_entries.finish_load(entries_for_dialog);
                        cx.notify();
                    });
                    window.draw(cx).clear(cx);
                })
                .expect("dialog window draw succeeds");

            let (harness_handle, _harness) = gpui_kit::open_window(
                gpui_kit::gpui::WindowOptions {
                    show: false,
                    focus: false,
                    ..gpui_kit::gpui::WindowOptions::default()
                },
                cx,
                |_, cx| {
                    cx.new(|_| JournalRowsHarness {
                        entries,
                        i18n: I18nManager::new("zh-CN"),
                    })
                },
            )
            .expect("harness window opens");

            harness_handle
                .update(cx, |_, window, cx| {
                    take_journal_row_bounds();
                    window.draw(cx).clear(cx);
                    let row_bounds = take_journal_row_bounds();
                    assert_eq!(row_bounds.len(), 20, "all 20 journal rows must be laid out");

                    let expected_row_height = window.rem_size() * layout::ROW_HEIGHT;
                    let subpixel_epsilon = gpui_kit::gpui::px(0.01);
                    for (ix, bounds) in row_bounds.iter().enumerate() {
                        assert!(
                            (bounds.size.height - expected_row_height).abs()
                                <= subpixel_epsilon,
                            "row {ix} height ({:?}) must equal the uniform row height ({:?})",
                            bounds.size.height,
                            expected_row_height
                        );
                    }
                    for (ix, pair) in row_bounds.windows(2).enumerate() {
                        assert!(
                            pair[1].top() + subpixel_epsilon >= pair[0].bottom(),
                            "row {} top ({:?}) must not overlap row {} bottom ({:?})",
                            ix + 1,
                            pair[1].top(),
                            ix,
                            pair[0].bottom()
                        );
                    }
                })
                .expect("harness draw succeeds");
            cx.quit();
        });
    }

    struct JournalHeaderHarness {
        app: Entity<CairnApp>,
    }

    impl Render for JournalHeaderHarness {
        fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
            window.set_rem_size(gpui_kit::gpui::px(crate::shell::theme::BASE_FONT_PX));
            let rem = window.rem_size();
            let app_ref = self.app.read(cx);
            let props = JournalTitleRow {
                title_text: app_ref.i18n.get_text("journal_dialog_title"),
                switch_label: app_ref.i18n.get_text("journal_dialog_record_toggle"),
                switch_checked: true,
                switch_disabled: false,
                refresh_label: app_ref.i18n.get_text("journal_dialog_refresh"),
                close_label: app_ref.i18n.get_text("window_close"),
                refresh_disabled: false,
                refresh_loading: false,
            };
            let (leading, close) = build_journal_header_parts(&self.app, props, window);
            let leading = leading.on_children_prepainted(|bounds, _, _| {
                record_journal_header_leading_bounds(bounds);
            });
            let row =
                build_journal_header_row(leading, close).on_children_prepainted(|bounds, _, _| {
                    record_journal_header_row_bounds(bounds);
                });
            div().w(rem * layout::DIALOG_WIDE_WIDTH).child(row)
        }
    }

    #[test]
    fn journal_header_keeps_title_switch_refresh_and_close_on_one_center_line() {
        gpui_kit::application().run(|cx: &mut gpui_kit::gpui::App| {
            gpui_kit::init(cx);
            let entries = vec![IndexJournalEntry {
                seq: 1,
                timestamp_ts: 1_760_000_000,
                source_id: SourceId(1),
                kind: JournalChangeKind::Modified,
                path: r"C:\Users\Administrator\Documents\report.txt".to_string(),
                old_path: None,
                frn: 100,
            }];

            let (handle, app) = gpui_kit::open_window(
                gpui_kit::gpui::WindowOptions {
                    show: false,
                    focus: false,
                    ..gpui_kit::gpui::WindowOptions::default()
                },
                cx,
                |window, cx| cx.new(|cx| CairnApp::new(window, cx)),
            )
            .expect("test window opens");

            handle
                .update(cx, |_, window, cx| {
                    app.update(cx, |this, cx| {
                        this.open_index_journal_dialog(window, cx);
                        this.service.connection.mark_connected();
                        this.service.journal.finish_load(IndexJournalStatus {
                            enabled: true,
                            entry_count: entries.len(),
                            resident_bytes: 1024,
                            max_entries: 2_000,
                            max_bytes: 1_048_576,
                            oldest_seq: 1,
                            newest_seq: 1,
                        });
                        this.service.journal_entries.finish_load(entries);
                        cx.notify();
                    });
                    window.draw(cx).clear(cx);
                })
                .expect("dialog draw succeeds");

            let (header_handle, _harness) = gpui_kit::open_window(
                gpui_kit::gpui::WindowOptions {
                    show: false,
                    focus: false,
                    ..gpui_kit::gpui::WindowOptions::default()
                },
                cx,
                |_, cx| cx.new(|_| JournalHeaderHarness { app }),
            )
            .expect("header harness opens");

            header_handle
                .update(cx, |_, window, cx| {
                    take_journal_header_bounds();
                    window.draw(cx).clear(cx);
                    let (row, leading) = take_journal_header_bounds();

                    assert_eq!(
                        row.len(),
                        2,
                        "header must place the leading group and the close control in one row"
                    );
                    assert_eq!(
                        leading.len(),
                        3,
                        "leading group must hold title, switch and refresh"
                    );

                    let tolerance = px(1.0);
                    let cluster_center = leading[0].center().y;
                    for (ix, bounds) in leading.iter().enumerate() {
                        assert!(
                            (bounds.center().y - cluster_center).abs() <= tolerance,
                            "leading element {ix} center ({:?}) must share the title center ({:?})",
                            bounds.center().y,
                            cluster_center
                        );
                    }
                    assert!(
                        (row[1].center().y - cluster_center).abs() <= tolerance,
                        "close control center ({:?}) must share the title center ({:?})",
                        row[1].center().y,
                        cluster_center
                    );
                    assert!(leading[0].right() <= leading[1].left());
                    assert!(leading[1].right() <= leading[2].left());
                    assert!(
                        row[1].left() > leading[2].right(),
                        "close control must trail the leading group"
                    );
                })
                .expect("header draw succeeds");
            cx.quit();
        });
    }
}
