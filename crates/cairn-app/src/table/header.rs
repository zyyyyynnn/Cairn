// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Result table column headings, lane sizing, and column resize handles.

use crate::app::{element_id, layout, CairnApp};
use crate::shell::accessibility;
use crate::shell::settings::ColumnSettings;
use crate::shell::theme::{self, ShellTokens};
use crate::table::model::{
    ColumnBoundary, OptionalColumn, SortColumn, SortColumnLabel, TableColumn,
};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_kit::component::{Icon, IconName, Sizable as _};
use gpui_kit::gpui::*;

const RESIZE_HIT_WIDTH: Pixels = px(8.0);
const RESIZE_LINE_OFFSET: Pixels = px(3.0);

/// Proportional column lane whose `flex_basis(0)` makes flex-grow divide surplus width evenly
/// between matching header and row cells.
pub(super) trait ColumnLane {
    fn lane_grow(self, grow: f32) -> Self;
}

impl ColumnLane for Div {
    fn lane_grow(self, grow: f32) -> Self {
        self.flex_grow(grow).flex_basis(px(0.0))
    }
}

impl<E> ColumnLane for Stateful<E>
where
    Stateful<E>: Styled,
{
    fn lane_grow(self, grow: f32) -> Self {
        self.flex_grow(grow).flex_basis(px(0.0))
    }
}

/// Column lane identity shared by header cells and row cells.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum HeaderLane {
    Name,
    Path,
    Size,
    Date,
    Optional(OptionalColumn),
}

pub(super) fn boundary_after_visible_lane(
    lane: HeaderLane,
    show_path: bool,
    show_size: bool,
    show_modified: bool,
) -> Option<ColumnBoundary> {
    let left = match lane {
        HeaderLane::Name => 0,
        HeaderLane::Path => 1,
        HeaderLane::Size => 2,
        HeaderLane::Date | HeaderLane::Optional(_) => return None,
    };
    [(1, show_path), (2, show_size), (3, show_modified)]
        .into_iter()
        .find(|&(idx, visible)| idx > left && visible)
        .map(|(right, _)| ColumnBoundary { left, right })
}

#[cfg(test)]
pub(super) fn boundary_after_lane(
    lane: HeaderLane,
    cols: &ColumnSettings,
) -> Option<ColumnBoundary> {
    boundary_after_visible_lane(lane, cols.show_path, cols.show_size, cols.show_modified)
}

pub(super) fn header_context_menu_entries(cols: &ColumnSettings) -> Vec<(TableColumn, bool, bool)> {
    TableColumn::ALL
        .into_iter()
        .map(|col| (col, cols.is_column_shown(col), col.is_locked()))
        .collect()
}

impl CairnApp {
    pub(super) fn show_table_column(&self, col: TableColumn) -> bool {
        if col.is_locked() {
            return true;
        }
        if self.recycle_bin.is_open() || self.block_listing.is_some() {
            return matches!(
                col,
                TableColumn::Name
                    | TableColumn::Path
                    | TableColumn::Size
                    | TableColumn::DateModified
            );
        }
        self.settings.columns.is_column_shown(col)
    }

    pub(super) fn show_optional_column(&self, col: OptionalColumn) -> bool {
        self.show_table_column(TableColumn::Optional(col))
    }

    pub(super) fn optional_columns_width(&self) -> f32 {
        OptionalColumn::ALL
            .into_iter()
            .filter(|col| self.show_optional_column(*col))
            .map(OptionalColumn::width_rem)
            .sum()
    }

    pub(crate) fn table_min_width(&self) -> f32 {
        let mut width = layout::NAME_MIN_COLUMN;
        if self.show_table_column(TableColumn::Path) {
            width += layout::PATH_MIN_COLUMN;
        }
        if self.show_table_column(TableColumn::Size) {
            width += layout::SIZE_COLUMN;
        }
        if self.show_table_column(TableColumn::DateModified) {
            width += layout::DATE_COLUMN;
        }
        width + self.optional_columns_width()
    }

    pub(super) fn render_table_header(
        &self,
        tokens: &ShellTokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let control = accessibility::control("table-header");
        let mut header = accessibility::element(control)
            .aria_label(accessibility::name(control, &self.i18n))
            .h(rems(layout::HEADER_HEIGHT))
            .flex()
            .items_center()
            .flex_shrink_0()
            .bg(tokens.table_head)
            .text_color(tokens.table_head_foreground)
            .border_b_1()
            .border_color(tokens.border)
            .text_sm()
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.finish_column_resize(cx)),
            )
            .child(self.render_column_header(SortColumn::Name, HeaderLane::Name, tokens, cx));
        if self.show_table_column(TableColumn::Path) {
            header = header.child(self.render_column_header(
                SortColumn::Path,
                HeaderLane::Path,
                tokens,
                cx,
            ));
        }
        if self.show_table_column(TableColumn::Size) {
            header = header.child(self.render_column_header(
                SortColumn::Size,
                HeaderLane::Size,
                tokens,
                cx,
            ));
        }
        if self.show_table_column(TableColumn::DateModified) {
            header = header.child(self.render_column_header(
                SortColumn::DateModified,
                HeaderLane::Date,
                tokens,
                cx,
            ));
        }
        for col in OptionalColumn::ALL {
            if self.show_optional_column(col) {
                header = header.child(self.render_column_header(
                    col.sort_column(),
                    HeaderLane::Optional(col),
                    tokens,
                    cx,
                ));
            }
        }
        header.into_any_element()
    }

    fn render_column_resize_handle(
        &self,
        boundary: ColumnBoundary,
        tokens: &ShellTokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = element_id("column-resize", boundary.key());
        let group = SharedString::from(id.clone());

        div()
            .id(id)
            .group(group.clone())
            .occlude()
            .absolute()
            .right_0()
            .top_0()
            .h_full()
            .w(RESIZE_HIT_WIDTH)
            .cursor_col_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.results.column_drag = Some(crate::app::ColumnDrag {
                        boundary,
                        widths: this.results.column_widths,
                        start_x: event.position.x,
                    });
                    this.results.keyboard_navigation = false;
                    cx.notify();
                }),
            )
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .absolute()
                    .right(RESIZE_LINE_OFFSET)
                    .top_0()
                    .h_full()
                    .w(theme::HAIRLINE)
                    .bg(tokens.border.opacity(0.0))
                    .group_hover(group, |style| style.bg(tokens.border)),
            )
            .into_any_element()
    }

    fn render_column_header(
        &self,
        column: SortColumn,
        lane: HeaderLane,
        tokens: &ShellTokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label_key = column.label_key_in(self.recycle_bin.is_open());
        let control = accessibility::control_at(&element_id("column-header", label_key));
        let label = accessibility::name(control, &self.i18n);
        let sort_rank = self.results.table.sort_rank(column);
        let multi_sort = self.results.table.sort_spec.keys.len() > 1;
        let is_active = sort_rank.is_some();

        let boundary = boundary_after_visible_lane(
            lane,
            self.show_table_column(TableColumn::Path),
            self.show_table_column(TableColumn::Size),
            self.show_table_column(TableColumn::DateModified),
        );

        let cell = accessibility::element(control)
            .aria_label(label.clone())
            .aria_selected(is_active)
            .relative()
            .h(rems(layout::HEADER_HEIGHT))
            .flex()
            .items_center()
            .gap(rems(layout::SPACE_4))
            .text_color(tokens.table_head_foreground)
            .hover(|s| s.bg(tokens.row_hover))
            .active(|s| s.bg(tokens.row_hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                if !this.recycle_bin.is_open() {
                    this.dismiss_result_selection(cx);
                }
                if window.modifiers().shift {
                    this.shift_toggle_table_sort(column, cx);
                } else {
                    this.toggle_table_sort(column, cx);
                }
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(label),
            )
            .children(sort_rank.map(|(rank, asc)| sort_indicator(rank, asc, multi_sort, tokens)));

        let cell = match lane {
            HeaderLane::Name => cell
                .lane_grow(self.results.column_widths.name)
                .min_w(rems(layout::NAME_MIN_COLUMN))
                .pl(rems(layout::CONTENT_INSET - layout::SPACE_4))
                .pr(rems(layout::INNER_SPACING)),
            HeaderLane::Path => cell
                .lane_grow(self.results.column_widths.path)
                .min_w(rems(layout::PATH_MIN_COLUMN))
                .px(rems(layout::INNER_SPACING)),
            HeaderLane::Size => cell
                .lane_grow(self.results.column_widths.size)
                .min_w(rems(layout::SIZE_COLUMN))
                .px(rems(layout::INNER_SPACING)),
            HeaderLane::Date => cell
                .lane_grow(self.results.column_widths.date)
                .min_w(rems(layout::DATE_COLUMN))
                .px(rems(layout::INNER_SPACING)),
            HeaderLane::Optional(col) => {
                let width = col.width_rem();
                cell.w(rems(width))
                    .min_w(rems(width))
                    .flex_shrink_0()
                    .px(rems(layout::INNER_SPACING))
            }
        };

        let cell = match boundary {
            Some(boundary) => cell.child(self.render_column_resize_handle(boundary, tokens, cx)),
            None => cell,
        };
        if self.recycle_bin.is_open() || self.block_listing.is_some() {
            return cell.into_any_element();
        }
        let app = cx.entity();
        let column_items: Vec<(TableColumn, String, bool, bool)> =
            header_context_menu_entries(&self.settings.columns)
                .into_iter()
                .map(|(col, checked, disabled)| {
                    (col, self.i18n.get_text(col.label_key()), checked, disabled)
                })
                .collect();
        cell.context_menu(move |mut menu, _, _| {
            for (ix, (col, item_label, checked, disabled)) in column_items.iter().enumerate() {
                if ix == TableColumn::BASE.len() {
                    menu = menu.separator();
                }
                let col = *col;
                let item = PopupMenuItem::new(item_label.clone())
                    .checked(*checked)
                    .disabled(*disabled);
                let item = if *disabled {
                    item
                } else {
                    let item_app = app.clone();
                    item.on_click(move |_, _, cx| {
                        item_app.update(cx, |this, cx| this.toggle_table_column(col, cx));
                    })
                };
                menu = menu.item(item);
            }
            menu
        })
        .into_any_element()
    }
}

fn sort_indicator(
    rank: usize,
    ascending: bool,
    multi_sort: bool,
    tokens: &ShellTokens,
) -> impl IntoElement {
    let mut row = div().flex().items_center().gap(px(2.0)).flex_shrink_0();
    if multi_sort {
        row = row.child(
            div()
                .text_size(rems(theme::SMALL_TEXT_SIZE))
                .font_features(theme::tabular_figures())
                .text_color(tokens.muted_foreground)
                .child(rank.to_string()),
        );
    }
    row.child(
        Icon::new(if ascending {
            IconName::ChevronUp
        } else {
            IconName::ChevronDown
        })
        .xsmall()
        .text_color(tokens.table_head_foreground),
    )
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use crate::table::model::{SortColumn, SortColumnLabel};

    #[test]
    fn the_recycle_bin_headers_name_the_values_they_hold() {
        assert_eq!(SortColumn::Name.label_key_in(true), "col_name");
        assert_eq!(SortColumn::Path.label_key_in(true), "col_original_location");
        assert_eq!(SortColumn::DateModified.label_key_in(true), "col_deleted");
        assert_eq!(SortColumn::Path.label_key_in(false), "col_path");
        assert_eq!(SortColumn::DateModified.label_key_in(false), "col_modified");
    }

    #[test]
    fn header_context_menu_starts_with_locked_name_and_base_columns() {
        use crate::shell::settings::ColumnSettings;
        use crate::table::model::{ColumnBoundary, TableColumn};

        let mut settings = ColumnSettings {
            show_size: false,
            ..Default::default()
        };
        let entries = super::header_context_menu_entries(&settings);
        assert_eq!(entries[0], (TableColumn::Name, true, true));
        assert_eq!(entries[1], (TableColumn::Path, true, false));
        assert_eq!(entries[2], (TableColumn::Size, false, false));
        assert_eq!(entries[3], (TableColumn::DateModified, true, false));
        assert_eq!(entries.len(), TableColumn::ALL.len());

        assert_eq!(
            super::boundary_after_lane(super::HeaderLane::Path, &ColumnSettings::default()),
            Some(ColumnBoundary::PATH_SIZE)
        );
        assert_eq!(
            super::boundary_after_lane(super::HeaderLane::Name, &settings),
            Some(ColumnBoundary::NAME_PATH)
        );
        assert_eq!(
            super::boundary_after_lane(super::HeaderLane::Path, &settings),
            Some(ColumnBoundary { left: 1, right: 3 })
        );
        assert_eq!(
            super::boundary_after_lane(super::HeaderLane::Date, &settings),
            None
        );

        settings.show_path = false;
        settings.show_modified = false;
        assert_eq!(
            super::boundary_after_lane(super::HeaderLane::Name, &settings),
            None
        );
    }
}
