// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The bottom status band: the number of objects the current query matched,
//! or selected node information in drive visualization mode.

use crate::app::{element_id, layout, CairnApp};
use crate::shell::accessibility::{self, ControlElement};
use crate::shell::theme::{self, ShellTokens};
use crate::treemap::palette::TreemapPalette;
use cairn_core::treemap::{format_size_bytes, TreemapCategory};
use fluent_bundle::FluentArgs;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, Icon, Sizable as _};
use gpui_kit::gpui::prelude::FluentBuilder as _;
use gpui_kit::gpui::*;

fn treemap_category_key(category: TreemapCategory) -> &'static str {
    match category {
        TreemapCategory::Folder => "treemap_category_folder",
        TreemapCategory::System => "treemap_category_system",
        TreemapCategory::Applications => "treemap_category_applications",
        TreemapCategory::Document => "treemap_category_document",
        TreemapCategory::Archive => "treemap_category_archive",
        TreemapCategory::Image => "treemap_category_image",
        TreemapCategory::Code => "treemap_category_code",
        TreemapCategory::Audio => "treemap_category_audio",
        TreemapCategory::Video => "treemap_category_video",
        TreemapCategory::Other => "treemap_category_other",
    }
}

impl CairnApp {
    pub(crate) fn render_status_line(
        &self,
        tokens: &ShellTokens,
        cx: &mut Context<Self>,
    ) -> ControlElement {
        let bar = accessibility::control("status-bar");
        let mut zoom_percent = None;
        let mut aria_summary = if self.recycle_bin_error.is_some() {
            self.i18n.get_text("status_recycle_bin_unavailable")
        } else if let Some(items) = &self.recycle_bin {
            let mut args = FluentArgs::new();
            args.set("count", items.len() as i64);
            self.i18n.get_text_with_args("status_objects_count", &args)
        } else {
            self.service.status.objects_count_text(&self.i18n)
        };
        let mut visible_summary = div()
            .min_w_0()
            .truncate()
            .child(aria_summary.clone())
            .into_any_element();

        // The table uses the same live marker as the Treemap: a spinner
        // prefixes the count while the first page or a page fetch is running,
        // so the two data surfaces read as one system.
        let table_loading = self.drive_view.is_none()
            && (self.recycle_bin_loading
                || (self.recycle_bin.is_none()
                    && (self.query.is_searching
                        || self.query.is_fetching_more
                        || (self.results.table.total_rows() == 0
                            && self.service.status.is_any_loading()))));
        if table_loading {
            visible_summary = h_flex()
                .min_w_0()
                .items_center()
                .gap(rems(layout::SPACE_4))
                .child(
                    Spinner::new()
                        .icon(Icon::new(IconName::LoaderCircle))
                        .xsmall(),
                )
                .child(div().min_w_0().truncate().child(aria_summary.clone()))
                .into_any_element();
            // The visual count is the useful progressive feedback; the accessible name says the
            // result set is still loading instead of repeating a stale count.
            aria_summary = self.i18n.get_text("status_loading");
        }

        if self.drive_view.is_some() {
            if let Some(ref st) = self.treemap_state {
                zoom_percent = st.zoom_percent();
                // One summary format for every state, the table side's
                // `N 个项目` convention carried over with the byte total:
                // loading only adds the animated icon that marks the count
                // as still growing.
                let mut args = FluentArgs::new();
                args.set("count", st.total_items() as i64);
                args.set("size", format_size_bytes(st.total_size()));
                aria_summary = self
                    .i18n
                    .get_text_with_args("treemap_status_summary", &args);
                visible_summary = if st.is_scanning() {
                    h_flex()
                        .min_w_0()
                        .items_center()
                        .gap(rems(layout::SPACE_4))
                        .child(
                            Spinner::new()
                                .icon(Icon::new(IconName::LoaderCircle))
                                .xsmall(),
                        )
                        .child(div().min_w_0().truncate().child(aria_summary.clone()))
                        .into_any_element()
                } else if let Some(node) = st.selected_node() {
                    let is_aggregate = node.is_aggregate();
                    let name = if is_aggregate {
                        let mut args = FluentArgs::new();
                        args.set("count", node.aggregate_count as i64);
                        self.i18n.get_text_with_args("treemap_aggregate", &args)
                    } else {
                        node.item.name.clone()
                    };
                    let size = format_size_bytes(node.item.size);
                    if is_aggregate {
                        // A summary has no concrete path; naming one would
                        // point at an invented location.
                        aria_summary = format!("{name} {size}");
                    } else {
                        let mut args = FluentArgs::new();
                        args.set("name", name.clone());
                        args.set("path", node.path.clone());
                        args.set("size", size.clone());
                        let pct = if st.total_size() > 0 {
                            (node.item.size as f64 / st.total_size() as f64) * 100.0
                        } else {
                            0.0
                        };
                        args.set("percent", format!("{:.1}", pct));
                        args.set(
                            "date",
                            crate::table::model::format_timestamp(node.item.modified_ts),
                        );
                        // The full path/date stay available to assistive technology;
                        // the visible bar only carries the two values the tile shows.
                        aria_summary = self
                            .i18n
                            .get_text_with_args("treemap_status_selected", &args);
                    }
                    h_flex()
                        .min_w_0()
                        .items_center()
                        .gap(rems(layout::SPACE_4))
                        .child(div().min_w_0().truncate().child(name))
                        .child(div().flex_shrink_0().child(size))
                        .into_any_element()
                } else {
                    div()
                        .min_w_0()
                        .truncate()
                        .child(aria_summary.clone())
                        .into_any_element()
                }
            }
        }

        let mut left = h_flex()
            .min_w_0()
            .items_center()
            .gap(rems(layout::SPACE_4))
            .child(div().min_w_0().flex_1().truncate().child(visible_summary));
        if let Some(percent) = zoom_percent {
            let reset = accessibility::control("treemap-zoom-reset");
            let reset_name = accessibility::name(reset, &self.i18n);
            let mut args = FluentArgs::new();
            args.set("percent", percent);
            let reset_tooltip = self
                .i18n
                .get_text_with_args("treemap_zoom_reset_tooltip", &args);
            left = left.child(
                div()
                    .flex_shrink_0()
                    // The window root clears Treemap selection on every left
                    // press; the reset is a control, not a click on the canvas.
                    .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| {
                        cx.stop_propagation()
                    })
                    .child(
                        Button::new("treemap-zoom-reset")
                            .ghost()
                            .xsmall()
                            .min_h(rems(layout::SPACE_24))
                            .label(format!("{percent}%"))
                            .tooltip(reset_tooltip)
                            .accessibility_label(reset_name)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.reset_treemap_zoom(window, cx);
                            })),
                    ),
            );
        }

        let mut status_bar = StatusBar::new()
            .h(rems(layout::STATUS_HEIGHT))
            .flex_shrink_0()
            .px(rems(layout::CONTENT_INSET))
            .border_t_1()
            .border_color(tokens.border)
            // The count is a live value; its digits must not reflow as results arrive.
            .font_features(theme::tabular_figures())
            .bg(tokens.status_bar)
            .text_color(tokens.muted_foreground)
            // The summary can carry a full path; let it ellipsize before it
            // pushes the category legend out of the bar.
            .left(left);
        let right = self
            .render_treemap_legend(tokens, cx)
            .or_else(|| self.render_empty_bin_action(cx));
        if let Some(right) = right {
            status_bar = status_bar.right(right);
        }

        accessibility::element(bar)
            .aria_label(aria_summary)
            // The status band is fixed-height chrome. Without this it is the
            // first child the column can shrink when the result band's content
            // is taller than the window, and the count disappears entirely.
            .flex_shrink_0()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.dismiss_result_selection_on_chrome_press(cx)),
            )
            .child(status_bar)
    }

    /// The Recycle Bin's one location-wide action. It lives in the status band
    /// because the bin has no toolbar, and it is absent while the listing is
    /// loading, unreadable or genuinely empty.
    fn render_empty_bin_action(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.recycle_bin.is_none()
            || self.recycle_bin_loading
            || self.recycle_bin_error.is_some()
            || self.recycle_bin_all.is_empty()
        {
            return None;
        }
        let control = accessibility::control("empty-recycle-bin");
        let label = accessibility::name(control, &self.i18n);
        Some(
            Button::new(control.element)
                .xsmall()
                .ghost()
                .min_h(rems(layout::SPACE_24))
                .label(label.clone())
                .accessibility_label(label)
                .on_click(cx.listener(|this, _, window, cx| this.empty_bin(window, cx)))
                .into_any_element(),
        )
    }

    /// A compact labeled legend doubles as the category filter: clicking an
    /// entry dims that category in place, preserving tile geometry and selection.
    fn render_treemap_legend(
        &self,
        tokens: &ShellTokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let state = self.treemap_state.as_ref()?;
        let palette = TreemapPalette::for_mode(self.settings.is_dark(self.system_dark));
        let mut row = h_flex()
            .flex_shrink_0()
            .items_center()
            .gap(rems(layout::SPACE_8));
        for category in TreemapCategory::ALL {
            let visible = state.category_visible(category);
            let label = SharedString::from(self.i18n.get_text(treemap_category_key(category)));
            let tooltip = {
                let mut args = FluentArgs::new();
                args.set("category", label.to_string());
                SharedString::from(self.i18n.get_text_with_args(
                    if visible {
                        "treemap_filter_hide"
                    } else {
                        "treemap_filter_show"
                    },
                    &args,
                ))
            };
            row = row.child(
                h_flex()
                    .id(element_id(
                        "treemap-legend",
                        &format!("{}", category as usize),
                    ))
                    .flex_shrink_0()
                    .items_center()
                    .gap(rems(layout::SPACE_4))
                    .h(rems(layout::LEGEND_ROW))
                    .px(rems(layout::LEGEND_INSET))
                    .rounded(tokens.radius)
                    .cursor_pointer()
                    .text_color(tokens.muted_foreground)
                    .hover(move |this| this.bg(tokens.row_hover).text_color(tokens.foreground))
                    .when(!visible, |this| this.opacity(0.35))
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            if let Some(state) = this.treemap_state.as_mut() {
                                state.toggle_category(category);
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .w(rems(layout::LEGEND_SWATCH))
                            .h(rems(layout::LEGEND_SWATCH))
                            .flex_shrink_0()
                            .rounded(tokens.radius)
                            .border_1()
                            .border_color(tokens.border)
                            .bg(palette.fill(category, 0)),
                    )
                    .child(div().whitespace_nowrap().child(label)),
            );
        }
        let reset = SharedString::from(self.i18n.get_text("treemap_filter_all"));
        let reset_tooltip = reset.clone();
        row = row.child(
            div()
                .id("treemap-legend-all")
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .w(rems(layout::LEGEND_ROW))
                .h(rems(layout::LEGEND_ROW))
                .rounded(tokens.radius)
                .cursor_pointer()
                .text_color(tokens.muted_foreground)
                .hover(move |this| this.bg(tokens.row_hover).text_color(tokens.foreground))
                .tooltip(move |window, cx| Tooltip::new(reset_tooltip.clone()).build(window, cx))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        if let Some(state) = this.treemap_state.as_mut() {
                            state.show_all_categories();
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
                .child(Icon::new(IconName::RotateCcw).xsmall()),
        );
        Some(row.into_any_element())
    }
}
