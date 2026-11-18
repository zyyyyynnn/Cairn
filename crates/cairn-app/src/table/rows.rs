// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Result table rows, cells, placeholders, virtual painting window, and row context menu.

use crate::app::{element_id, join_row_path, layout, split_name_highlights, CairnApp};
use crate::shell::accessibility;
use crate::shell::command_groups::GroupCommand;
use crate::shell::file_icon::file_kind_icon;
use crate::shell::theme::{self, ShellTokens};
use crate::table::header::ColumnLane as _;
use crate::table::model::{OptionalColumn, TableColumn};
use crate::table::presentation::{format_optional_cell, format_timestamp_cached};
use cairn_core::format_size_bytes;
use cairn_core::FileKind;
use cairn_protocol::message::FileRowData;
use gpui_kit::assets::IconName as AssetIconName;
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::gpui::prelude::FluentBuilder as _;
use gpui_kit::gpui::*;
use std::ops::Range;
use std::rc::Rc;

const PLACEHOLDER_BAR_HEIGHT: f32 = 0.75;
const PLACEHOLDER_DATE_WIDTH: f32 = 7.0;
const CONTEXT_MENU_MIN_WIDTH: f32 = 12.0;

pub(super) struct PaintedRows {
    pub range: Range<usize>,
    pub top: f32,
    pub pinned: bool,
}

pub(super) struct RowBand<'a> {
    pub total: usize,
    pub tokens: &'a ShellTokens,
    pub row_height: Pixels,
    pub interactive: bool,
    pub search_focused: bool,
}

/// Computes viewport-local `top` in `f64` before casting so deep scroll offsets keep exact row pitch.
pub(super) fn rows_band_local_top(first: usize, scroll_top: f32, row_height: f32) -> f32 {
    (first as f64 * row_height as f64 - scroll_top as f64) as f32
}

/// Resolves which loaded rows to paint and whether they are a non-interactive edge preview.
pub(super) fn rows_to_paint(
    visible: Range<usize>,
    loaded: Range<usize>,
    scroll_top: f32,
    viewport_height: f32,
    row_height: f32,
) -> PaintedRows {
    let overlaps = visible.start < loaded.end && visible.end > loaded.start;
    if overlaps || loaded.is_empty() {
        let top = rows_band_local_top(visible.start, scroll_top, row_height);
        return PaintedRows {
            range: visible,
            top,
            pinned: false,
        };
    }

    let count = visible.len().max(1);
    if visible.start >= loaded.end {
        let start = loaded.end.saturating_sub(count).max(loaded.start);
        let top = viewport_height - (loaded.end - start) as f32 * row_height;
        PaintedRows {
            range: start..loaded.end,
            top,
            pinned: true,
        }
    } else {
        let end = loaded.start.saturating_add(count).min(loaded.end);
        PaintedRows {
            range: loaded.start..end,
            top: 0.0,
            pinned: true,
        }
    }
}

fn row_file_kind(name: &str, resolved_extension: &str) -> FileKind {
    let name_extension = name.rsplit_once('.').map(|(_, extension)| extension);
    let name_names_the_suffix =
        name_extension.is_some_and(|extension| extension.eq_ignore_ascii_case(resolved_extension));
    if resolved_extension.is_empty() || name_names_the_suffix {
        return FileKind::classify_file(name);
    }
    let kind = FileKind::classify_file(&format!("file.{resolved_extension}"));
    if kind == FileKind::Other {
        FileKind::classify_file(name)
    } else {
        kind
    }
}

fn result_row_icon(name: &str, resolved_extension: &str, is_dir: bool) -> AssetIconName {
    if is_dir {
        return AssetIconName::Folder;
    }
    file_kind_icon(row_file_kind(name, resolved_extension))
}

pub(super) const RESULT_COMMANDS: &[GroupCommand] = &[
    GroupCommand::OpenSelected,
    GroupCommand::RenameSelected,
    GroupCommand::RevealLocation,
    GroupCommand::CopyPaths,
    GroupCommand::CopyNames,
    GroupCommand::SelectAll,
    GroupCommand::DeleteSelected,
    GroupCommand::ShowProperties,
];

pub(super) const BIN_COMMANDS: &[GroupCommand] = &[
    GroupCommand::OpenSelected,
    GroupCommand::RestoreSelected,
    GroupCommand::CopyPaths,
    GroupCommand::CopyNames,
    GroupCommand::SelectAll,
    GroupCommand::DeleteSelected,
];

struct RowRender<'a> {
    ix: usize,
    row: &'a FileRowData,
    full_path: &'a str,
    is_selected: bool,
    band: &'a RowBand<'a>,
}

fn cell_text_color(tokens: &ShellTokens, is_selected: bool) -> gpui::Hsla {
    if is_selected {
        tokens.foreground
    } else {
        tokens.muted_foreground
    }
}

impl CairnApp {
    pub(super) fn render_result_row(
        &self,
        ix: usize,
        row: &FileRowData,
        band: &RowBand<'_>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let total = band.total;
        let tokens = band.tokens;
        let row_height = band.row_height;
        let is_selected = self.results.table.selected_indices.contains(&ix);
        let is_focused = self.results.table.focused_idx == Some(ix);
        let full_path = join_row_path(&row.path, &row.name);
        let i18n = Rc::clone(&self.i18n);

        let snippets = crate::table::snippets::prepare_snippets(&row.snippets);
        let aria_snippets = (!snippets.is_empty()).then(|| snippets.join(" "));
        let cell_ctx = RowRender {
            ix,
            row,
            full_path: &full_path,
            is_selected,
            band,
        };

        let mut row_el = accessibility::element_at(accessibility::control("result-row"), ix)
            .aria_label(full_path.clone())
            .when_some(aria_snippets, |this, snippet| {
                this.aria_description(snippet)
            })
            .aria_selected(is_selected)
            .aria_row_index(ix + 1)
            .aria_position_in_set(ix + 1)
            .aria_size_of_set(total)
            .flex()
            .items_center()
            .h(row_height)
            .bg(tokens.row_background(is_selected))
            .text_color(cell_text_color(tokens, is_selected))
            .text_sm()
            .child(self.render_name_cell(&cell_ctx, snippets, cx));

        if self.show_table_column(TableColumn::Path) {
            row_el = row_el.child(self.render_path_cell(&cell_ctx));
        }
        if self.show_table_column(TableColumn::Size) {
            row_el = row_el.child(self.render_metric_cell(
                &cell_ctx,
                self.results.column_widths.size,
                layout::SIZE_COLUMN,
                format_size_bytes(row.size),
            ));
        }
        if self.show_table_column(TableColumn::DateModified) {
            row_el = row_el.child(self.render_metric_cell(
                &cell_ctx,
                self.results.column_widths.date,
                layout::DATE_COLUMN,
                format_timestamp_cached(row.modified_ts),
            ));
        }

        for col in OptionalColumn::ALL {
            if self.show_optional_column(col) {
                row_el = row_el.child(self.render_optional_cell(&cell_ctx, col));
            }
        }

        if band.interactive {
            row_el = row_el
                .hover(|s| s.bg(tokens.row_hover))
                .active(|s| s.bg(tokens.row_hover))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        if this
                            .rename
                            .draft
                            .as_ref()
                            .is_some_and(|draft| draft.path == full_path)
                        {
                            cx.stop_propagation();
                            return;
                        }
                        this.select_row_by_pointer(
                            ix,
                            event.modifiers.control,
                            event.modifiers.shift,
                            event.click_count == 2,
                            layout::row_height(window),
                            cx,
                        );
                        this.results.results_focus.focus(window, cx);
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        this.focus_row_for_context(ix, cx);
                        cx.notify();
                    }),
                );
        }

        if is_focused {
            row_el = row_el.aria_active_descendant();
        }
        if !band.interactive {
            return row_el.into_any_element();
        }
        let in_bin = self.recycle_bin.is_open();
        let commands = if in_bin {
            BIN_COMMANDS
        } else {
            RESULT_COMMANDS
        };
        let app = cx.entity();
        row_el
            .context_menu(move |menu, window, cx| {
                let mut menu = menu.min_w(window.rem_size() * CONTEXT_MENU_MIN_WIDTH);
                let rename_available = app.read(cx).rename_target_ix().is_some();
                for (ix, command) in commands.iter().enumerate() {
                    if ix > 0
                        && matches!(
                            command,
                            GroupCommand::DeleteSelected | GroupCommand::ShowProperties
                        )
                    {
                        menu = menu.separator();
                    }
                    let label = i18n.get_text(command.label_key_in(in_bin));
                    let action = command
                        .action()
                        .expect("a result command is an Action the root already handles");
                    menu = if *command == GroupCommand::RenameSelected {
                        menu.menu_with_disabled(label, action, !rename_available)
                    } else {
                        menu.menu(label, action)
                    };
                }
                menu
            })
            .into_any_element()
    }

    fn render_name_cell(
        &self,
        ctx: &RowRender<'_>,
        snippets: Vec<String>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let RowRender {
            ix,
            row,
            full_path,
            is_selected,
            band,
        } = *ctx;
        let tokens = band.tokens;
        let search_focused = band.search_focused;
        let renaming = self
            .rename
            .draft
            .as_ref()
            .is_some_and(|draft| draft.path == full_path);
        let tooltip = SharedString::from(row.name.as_str());
        let extension = if self.recycle_bin.is_open() {
            self.recycle_bin
                .icon_extension(crate::shell::recycle_bin::bin_icon_key(
                    &row.name,
                    &row.path,
                    row.modified_ts,
                    row.size,
                    row.is_dir,
                ))
                .unwrap_or("")
        } else {
            ""
        };

        let cell = div()
            .id(element_id("result-name", full_path))
            .lane_grow(self.results.column_widths.name)
            .min_w(rems(layout::NAME_MIN_COLUMN))
            .pl(rems(layout::CONTENT_INSET - layout::SPACE_4))
            .pr(rems(layout::INNER_SPACING))
            .flex()
            .items_center()
            .gap(rems(layout::INNER_SPACING));
        let cell = if renaming {
            cell
        } else {
            cell.tooltip(crate::shell::tooltip::text(tooltip))
        };
        let cell = cell.child(
            Icon::new(result_row_icon(&row.name, extension, row.is_dir))
                .small()
                .text_color(cell_text_color(tokens, is_selected)),
        );
        let cell = if renaming {
            let control = accessibility::control("rename-input");
            cell.child(
                Input::new(&self.rename.input)
                    .id(control.element)
                    .xsmall()
                    .appearance(false)
                    .bordered(false)
                    .focus_bordered(false)
                    .text_size(rems(0.875))
                    .line_height(rems(1.25))
                    .p(rems(0.0))
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .aria_label(accessibility::component_name(control, &self.i18n)),
            )
        } else {
            cell.child(result_text(&row.name, &row.name_highlights, tokens))
        };
        cell.when(!renaming && !snippets.is_empty(), |this| {
            this.child(self.render_snippet_badge_popover(
                ix,
                full_path,
                snippets,
                tokens,
                search_focused,
                cx,
            ))
        })
    }

    fn render_path_cell(&self, ctx: &RowRender<'_>) -> Stateful<Div> {
        let tooltip = SharedString::from(ctx.row.path.as_str());

        div()
            .id(element_id("result-path", ctx.full_path))
            .tooltip(crate::shell::tooltip::text(tooltip))
            .lane_grow(self.results.column_widths.path)
            .min_w(rems(layout::PATH_MIN_COLUMN))
            .px(rems(layout::INNER_SPACING))
            .flex()
            .items_center()
            .overflow_hidden()
            .text_color(cell_text_color(ctx.band.tokens, ctx.is_selected))
            .child(result_text(
                &ctx.row.path,
                &ctx.row.path_highlights,
                ctx.band.tokens,
            ))
    }

    fn render_metric_cell(
        &self,
        ctx: &RowRender<'_>,
        width_grow: f32,
        min_width: f32,
        text: impl Into<SharedString>,
    ) -> Div {
        div()
            .lane_grow(width_grow)
            .min_w(rems(min_width))
            .px(rems(layout::INNER_SPACING))
            .flex()
            .items_center()
            .font_features(theme::tabular_figures())
            .text_color(cell_text_color(ctx.band.tokens, ctx.is_selected))
            .child(text.into())
    }

    fn render_optional_cell(&self, ctx: &RowRender<'_>, col: OptionalColumn) -> Div {
        let mut cell = div()
            .w(rems(col.width_rem()))
            .flex_shrink_0()
            .px(rems(layout::INNER_SPACING))
            .flex()
            .items_center()
            .overflow_hidden()
            .text_color(cell_text_color(ctx.band.tokens, ctx.is_selected));
        if col.uses_tabular_figures() {
            cell = cell.font_features(theme::tabular_figures());
        }
        cell.child(
            div()
                .min_w_0()
                .truncate()
                .child(format_optional_cell(ctx.row, col)),
        )
    }

    pub(super) fn render_placeholder_row(
        &self,
        ix: usize,
        is_fetching: bool,
        tokens: &ShellTokens,
        row_height: Pixels,
    ) -> AnyElement {
        let control = accessibility::control("result-placeholder");
        let message = if is_fetching {
            accessibility::name_at(control, 0, &self.i18n)
        } else {
            accessibility::name_at(control, 1, &self.i18n)
        };

        const PLACEHOLDER_NAME_WIDTHS: [f32; 8] = [8.0, 11.0, 6.5, 9.5, 12.0, 7.5, 10.0, 8.5];
        const PLACEHOLDER_PATH_WIDTHS: [f32; 8] = [14.0, 18.0, 12.0, 16.5, 20.0, 13.5, 17.0, 15.0];
        const PLACEHOLDER_SIZE_WIDTHS: [f32; 4] = [3.2, 2.5, 4.0, 2.8];

        let name_w = rems(PLACEHOLDER_NAME_WIDTHS[ix % PLACEHOLDER_NAME_WIDTHS.len()]);
        let path_w = rems(PLACEHOLDER_PATH_WIDTHS[ix % PLACEHOLDER_PATH_WIDTHS.len()]);
        let size_w = rems(PLACEHOLDER_SIZE_WIDTHS[ix % PLACEHOLDER_SIZE_WIDTHS.len()]);
        let bar_bg = tokens.muted;

        let mut row_el = accessibility::element_at(control, ix)
            .aria_label(message)
            .h(row_height)
            .flex()
            .items_center()
            .w_full()
            .child(
                div()
                    .lane_grow(self.results.column_widths.name)
                    .min_w(rems(layout::NAME_MIN_COLUMN))
                    .pl(rems(layout::CONTENT_INSET - layout::SPACE_4))
                    .pr(rems(layout::INNER_SPACING))
                    .flex()
                    .items_center()
                    .gap(rems(layout::INNER_SPACING))
                    .child(
                        div()
                            .size_4()
                            .flex_shrink_0()
                            .rounded(tokens.radius)
                            .bg(bar_bg),
                    )
                    .child(
                        div()
                            .h(rems(PLACEHOLDER_BAR_HEIGHT))
                            .w(name_w)
                            .max_w_full()
                            .rounded(tokens.radius)
                            .bg(bar_bg),
                    ),
            );

        if self.show_table_column(TableColumn::Path) {
            row_el = row_el.child(
                div()
                    .lane_grow(self.results.column_widths.path)
                    .min_w(rems(layout::PATH_MIN_COLUMN))
                    .px(rems(layout::INNER_SPACING))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .h(rems(PLACEHOLDER_BAR_HEIGHT))
                            .w(path_w)
                            .max_w_full()
                            .rounded(tokens.radius)
                            .bg(bar_bg),
                    ),
            );
        }
        if self.show_table_column(TableColumn::Size) {
            row_el = row_el.child(
                div()
                    .lane_grow(self.results.column_widths.size)
                    .min_w(rems(layout::SIZE_COLUMN))
                    .px(rems(layout::INNER_SPACING))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .h(rems(PLACEHOLDER_BAR_HEIGHT))
                            .w(size_w)
                            .rounded(tokens.radius)
                            .bg(bar_bg),
                    ),
            );
        }
        if self.show_table_column(TableColumn::DateModified) {
            row_el = row_el.child(
                div()
                    .lane_grow(self.results.column_widths.date)
                    .min_w(rems(layout::DATE_COLUMN))
                    .px(rems(layout::INNER_SPACING))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .h(rems(PLACEHOLDER_BAR_HEIGHT))
                            .w(rems(PLACEHOLDER_DATE_WIDTH))
                            .rounded(tokens.radius)
                            .bg(bar_bg),
                    ),
            );
        }

        for col in OptionalColumn::ALL {
            if self.show_optional_column(col) {
                row_el = row_el.child(
                    div()
                        .w(rems(col.width_rem()))
                        .flex_shrink_0()
                        .px(rems(layout::INNER_SPACING))
                        .flex()
                        .items_center()
                        .child(
                            div()
                                .h(rems(PLACEHOLDER_BAR_HEIGHT))
                                .w(rems(PLACEHOLDER_DATE_WIDTH))
                                .rounded(tokens.radius)
                                .bg(bar_bg),
                        ),
                );
            }
        }
        row_el.into_any_element()
    }
}

pub(super) fn styled_highlighted_text(
    text: &str,
    highlights: &[(usize, usize)],
    tokens: &ShellTokens,
) -> StyledText {
    let value = StyledText::new(SharedString::from(text));
    if highlights.is_empty() {
        return value;
    }
    let mut offset = 0;
    let runs = split_name_highlights(text, highlights)
        .into_iter()
        .filter_map(|span| {
            let start = offset;
            offset += span.text.len();
            span.is_highlight.then_some(start..offset)
        })
        .map(|range| {
            (
                range,
                HighlightStyle {
                    color: Some(tokens.foreground),
                    font_weight: Some(FontWeight::SEMIBOLD),
                    ..Default::default()
                },
            )
        });
    value.with_highlights(runs)
}

fn result_text(text: &str, highlights: &[(usize, usize)], tokens: &ShellTokens) -> Div {
    let value = styled_highlighted_text(text, highlights, tokens);

    div()
        .flex_1()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .child(value)
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::{result_row_icon, BIN_COMMANDS, RESULT_COMMANDS};
    use crate::shell::command_groups::GroupCommand;
    use gpui_kit::assets::IconName as AssetIconName;

    #[test]
    fn result_rows_use_the_file_semantics_they_can_show() {
        assert_eq!(
            result_row_icon("Deploy.lnk", "", false),
            AssetIconName::FileSymlink
        );
        assert_eq!(
            result_row_icon("BOOKMARKS.URL", "", false),
            AssetIconName::FileSymlink
        );
        assert_eq!(
            result_row_icon("budget.xlsx", "", false),
            AssetIconName::FileSpreadsheet
        );
        assert_eq!(
            result_row_icon("photo.PNG", "", false),
            AssetIconName::FileImage
        );
        assert_eq!(
            result_row_icon("Cairn.exe", "", false),
            AssetIconName::AppWindow
        );
        assert_eq!(
            result_row_icon("build.ps1", "", false),
            AssetIconName::FileTerminal
        );
        assert_eq!(
            result_row_icon("notes.unknown", "", false),
            AssetIconName::File
        );
        assert_eq!(result_row_icon("Reports", "", true), AssetIconName::Folder);
        assert_eq!(
            result_row_icon("Grok Bot", "lnk", false),
            AssetIconName::FileSymlink
        );
        assert_eq!(
            result_row_icon("Screenshot", "png", false),
            AssetIconName::FileImage
        );
        assert_eq!(
            result_row_icon(".gitignore", "gitignore", false),
            AssetIconName::FileBraces
        );
        assert_eq!(
            result_row_icon("CMakeLists.txt", "txt", false),
            AssetIconName::FileCode
        );
    }

    #[test]
    fn the_result_menu_offers_in_place_rename_but_the_bin_does_not() {
        assert!(RESULT_COMMANDS.contains(&GroupCommand::RenameSelected));
        assert!(!BIN_COMMANDS.contains(&GroupCommand::RenameSelected));
    }

    #[test]
    fn snippet_preparation_deduplicates_highlights_and_caps_at_three() {
        use crate::table::snippets::{
            prepare_snippets, query_content_highlight_terms, snippet_highlight_ranges,
        };
        use cairn_core::syntax::QueryDefaults;

        let prepared = prepare_snippets(&[
            "   ".into(),
            "first match line".into(),
            "first match line".into(),
            "second match".into(),
            "third 架构 line".into(),
            "fourth ignored".into(),
        ]);
        assert_eq!(
            prepared,
            vec!["first match line", "second match", "third 架构 line"]
        );

        let terms = query_content_highlight_terms(
            "content:Alpha !content:Beta content:架构",
            &QueryDefaults::default(),
        );
        assert_eq!(terms, vec!["Alpha", "架构"]);

        let ranges = snippet_highlight_ranges("pre ALPHA and alpha and 架构 post", &terms);
        assert_eq!(ranges, vec![(4, 9), (14, 19), (24, 30)]);
    }

    #[test]
    fn snippet_preview_opens_via_alt_space_click_hover_and_dismisses_cleanly() {
        use crate::app::{test_row, CairnApp};
        use crate::shell::commands::{EscapeQuery, PreviewSnippet};
        use crate::table::snippets::SnippetPreviewTrigger;
        use gpui_kit::gpui::AppContext as _;

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
                        let mut with_snippets = test_row("doc.md");
                        with_snippets.snippets = vec!["hello world".into(), "second match".into()];
                        let plain = test_row("plain.txt");
                        this.results.table.set_rows(vec![with_snippets, plain], 2);

                        this.results.table.select_row(1, false);
                        this.results.results_focus.focus(window, cx);
                        this.preview_snippet(&PreviewSnippet, window, cx);
                        assert!(this.results.snippet_preview.is_none());

                        this.results.table.select_row(0, false);
                        this.preview_snippet(&PreviewSnippet, window, cx);
                        assert_eq!(
                            this.results
                                .snippet_preview
                                .as_ref()
                                .map(|s| (s.row_path.as_str(), s.trigger)),
                            Some(("C:\\doc.md", SnippetPreviewTrigger::Pinned))
                        );
                        assert!(this.results.results_focus.is_focused(window));

                        this.query.search_input.set_text("content:hello");
                        this.escape_query(&EscapeQuery, window, cx);
                        assert!(this.results.snippet_preview.is_none());
                        assert_eq!(this.query.search_input.text, "content:hello");
                        assert!(this.results.results_focus.is_focused(window));

                        this.query.search_focus.focus(window, cx);
                        this.hover_snippet_badge("C:\\doc.md", true, cx);
                        assert_eq!(
                            this.results
                                .snippet_preview
                                .as_ref()
                                .map(|s| (s.row_path.as_str(), s.trigger)),
                            Some(("C:\\doc.md", SnippetPreviewTrigger::Hover))
                        );
                        assert!(this.query.search_focus.is_focused(window));

                        this.click_snippet_badge(0, "C:\\doc.md", window, cx);
                        assert_eq!(
                            this.results
                                .snippet_preview
                                .as_ref()
                                .map(|s| (s.row_path.as_str(), s.trigger)),
                            Some(("C:\\doc.md", SnippetPreviewTrigger::Pinned))
                        );
                        assert!(this.results.results_focus.is_focused(window));

                        this.preview_snippet(&PreviewSnippet, window, cx);
                        assert!(this.results.snippet_preview.is_none());
                        assert!(this.results.results_focus.is_focused(window));

                        this.preview_snippet(&PreviewSnippet, window, cx);
                        assert!(this.results.snippet_preview.is_some());
                        this.move_selection(
                            true,
                            false,
                            crate::app::layout::row_height(window),
                            cx,
                        );
                        assert!(this.results.snippet_preview.is_none());
                    });

                    cx.quit();
                    app
                },
            );
        });
    }
}
