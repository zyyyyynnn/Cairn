// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The bottom status band: the number of objects the current query matched,
//! or selected node information in drive visualization mode.

use crate::app::{layout, CairnApp};
use crate::shell::accessibility::{self, ControlElement};
use crate::shell::theme::{self, ShellTokens};
use crate::treemap::palette::TreemapPalette;
use cairn_core::format_size_bytes;
use cairn_core::treemap::TreemapCategory;
use fluent_bundle::FluentArgs;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, Sizable as _};
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ControlElement {
        let bar = accessibility::control("status-bar");
        let mut zoom_percent = None;
        let mut summary: Option<AnyElement> = None;
        let mut summary_aria: Option<String> = None;
        let mut summary_danger = false;
        let mut summary_loading = false;
        let mut icon_loading = false;
        let mut view_name = "table";

        if self.recycle_bin.error().is_some() {
            view_name = "recycle-bin";
            let text = self.i18n.get_text("status_recycle_bin_unavailable");
            summary_aria = Some(text.clone());
            summary = Some(
                div()
                    .min_w_0()
                    .truncate()
                    .font_features(theme::tabular_figures())
                    .child(text)
                    .into_any_element(),
            );
        } else if let Some(items) = self.recycle_bin.rows() {
            view_name = "recycle-bin";
            if self.recycle_bin.is_loading() {
                summary_loading = true;
                icon_loading = true;
            } else {
                let count_text = {
                    let mut args = FluentArgs::new();
                    args.set("count", items.len() as i64);
                    self.i18n.get_text_with_args("status_objects_count", &args)
                };
                summary_aria = Some(count_text.clone());
                summary = Some(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_features(theme::tabular_figures())
                        .child(count_text)
                        .into_any_element(),
                );
            }
        } else if self.drive_view.is_some() {
            view_name = "treemap";
            if let Some(ref st) = self.treemap_state {
                if st.is_scanning() {
                    // No counts or bytes are published until the scan has a complete tree.
                    summary_loading = true;
                    icon_loading = true;
                } else if st.is_unreadable() {
                    let text = self.i18n.get_text("treemap_unreadable");
                    summary_aria = Some(text.clone());
                    summary = Some(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(tokens.danger)
                            .child(text)
                            .into_any_element(),
                    );
                    summary_danger = true;
                } else {
                    zoom_percent = st.zoom_percent();
                    let mut args = FluentArgs::new();
                    args.set("count", st.total_items() as i64);
                    args.set("size", format_size_bytes(st.total_size()));
                    let totals_text = self
                        .i18n
                        .get_text_with_args("treemap_status_summary", &args);
                    summary_aria = Some(totals_text.clone());
                    summary = if let Some(node) = st.selected_node() {
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
                            summary_aria = Some(format!("{name} {size}"));
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
                            summary_aria = Some(
                                self.i18n
                                    .get_text_with_args("treemap_status_selected", &args),
                            );
                        }
                        Some(
                            h_flex()
                                .min_w_0()
                                .items_center()
                                .gap(rems(layout::SPACE_4))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .font_features(theme::tabular_figures())
                                        .child(name),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .font_features(theme::tabular_figures())
                                        .child(size),
                                )
                                .into_any_element(),
                        )
                    } else {
                        Some(
                            div()
                                .min_w_0()
                                .truncate()
                                .font_features(theme::tabular_figures())
                                .child(totals_text)
                                .into_any_element(),
                        )
                    };
                }
            } else {
                summary_loading = true;
                icon_loading = true;
            }
        } else {
            let index_loading = self.service.status.is_any_loading();
            let awaiting_results = self.results.table.total_rows() == 0;
            // A total can only be final once the index has settled. While volumes are still
            // building, a completed query describes a partial index, so its number would climb in
            // steps as batches land. The band shows the loading indicator alone until the status
            // poll asks for the result set of the completed index.
            summary_loading = index_loading
                || (awaiting_results
                    && (self.query.is_searching || self.service.is_probing_service));
            // Paging lazy rows during scrollbar dragging must not flicker the status refresh control.
            // Only genuine searches, index builds, or connection probes show the loading indicator.
            icon_loading = summary_loading;
            if !summary_loading
                && self.service.is_service_connected
                && self.service.status.has_objects_summary()
            {
                let text = self.service.status.objects_summary_text(&self.i18n);
                summary_aria = Some(text.clone());
                summary = Some(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_features(theme::tabular_figures())
                        .child(text)
                        .into_any_element(),
                );
            }
        }

        // Keep the native xsmall button body (20px). A forced 24px minimum made the hover
        // surface extend a row above/below the status line.
        //
        // While the window waits, the control is replaced by the single loading indicator: a
        // second circular mark beside the refresh arrow reads as two loaders. The indicator turns
        // at 30 FPS, and it is not interactive; refreshing is the idle control's job.
        let refresh_control: AnyElement = if icon_loading {
            div()
                .flex()
                .items_center()
                .justify_center()
                // The slot is the xsmall icon-only button's own footprint (size_5 = 1.25rem),
                // so swapping the refresh glyph for the loader cannot move or resize the band.
                .w(rems(1.25))
                .h(rems(1.25))
                .child(crate::shell::loading_indicator::loading_indicator(
                    !cx.reduce_motion(),
                ))
                .into_any_element()
        } else {
            let refresh_label = self.i18n.get_text("status_refresh");
            Button::new("status-refresh")
                .ghost()
                .xsmall()
                .icon(IconName::RefreshCw)
                .tooltip(refresh_label.clone())
                .accessibility_label(refresh_label)
                .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                    this.refresh_current_view(cx);
                }))
                .into_any_element()
        };
        let refresh_slot = div()
            .flex_shrink_0()
            // The window root clears selection on every left press; the idle refresh is a control,
            // not a click on the canvas.
            .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| {
                cx.stop_propagation()
            })
            .child(refresh_control);

        // Opt-in acceptance trace (`CAIRN_PERF_LOG`): the visible summary and the loading state,
        // never a hidden animated value.
        crate::shell::perf::mark(&format!(
            "status frame summary={summary_aria:?} bytes={} summary_loading={summary_loading} icon_loading={icon_loading} view={view_name} qid={}",
            self.service.status.objects_bytes(),
            self.query.query_id,
        ));

        let aria_summary = summary_aria.unwrap_or_else(|| {
            if summary_loading {
                self.i18n.get_text("status_loading")
            } else {
                self.i18n.get_text("status_refresh")
            }
        });

        let mut left = h_flex()
            .min_w_0()
            .items_center()
            .gap(rems(layout::SPACE_4))
            .child(refresh_slot);
        // The summary is only rendered once there is a completed result set, so the band does not
        // shift when it appears.
        if let Some(summary) = summary {
            left = left.child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .when(summary_danger, |this| this.text_color(tokens.danger))
                    .child(summary),
            );
        }
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
                    .font_features(theme::tabular_figures())
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
            .render_treemap_legend(tokens, window, cx)
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
        if !self.recycle_bin.is_open()
            || self.recycle_bin.is_loading()
            || self.recycle_bin.error().is_some()
            || self.recycle_bin.all().is_empty()
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let state = self.treemap_state.as_ref()?;
        let palette = TreemapPalette::for_mode(self.settings.is_dark(self.system_dark));
        let mut row = h_flex()
            .flex_shrink_0()
            .items_center()
            .gap(rems(layout::SPACE_8));
        let legend_control = accessibility::control("treemap-legend");
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
            let legend_accessible_label = tooltip.clone();
            let cat_ix = category as usize;
            let focus = window
                .use_keyed_state(("treemap-legend", cat_ix), cx, |_, cx| {
                    cx.focus_handle().tab_stop(true)
                })
                .read(cx)
                .clone();
            let click_focus = focus.clone();

            row = row.child(
                accessibility::element_at(legend_control, cat_ix)
                    .flex()
                    .flex_row()
                    .tab_stop(true)
                    .track_focus(&focus)
                    .aria_label(legend_accessible_label)
                    .aria_toggled(if visible {
                        gpui_kit::gpui::accesskit::Toggled::True
                    } else {
                        gpui_kit::gpui::accesskit::Toggled::False
                    })
                    .flex_shrink_0()
                    .items_center()
                    .gap(rems(layout::SPACE_4))
                    .h(rems(layout::LEGEND_ROW))
                    .px(rems(layout::LEGEND_INSET))
                    .rounded(tokens.radius)
                    // Reserve the ring's geometry so a keyboard focus never moves the legend,
                    // and paint it only for keyboard focus: a mouse click may focus the row for
                    // activation, but it must not draw a control frame.
                    .border_1()
                    .border_color(tokens.border.opacity(0.0))
                    .focus_visible(move |style| style.border_color(tokens.ring))
                    .text_color(tokens.muted_foreground)
                    .hover(move |this| this.bg(tokens.row_hover).text_color(tokens.foreground))
                    .when(!visible, |this| this.opacity(0.35))
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            click_focus.focus(window, cx);
                            if let Some(state) = this.treemap_state.as_mut() {
                                state.toggle_category(category);
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            if let Some(state) = this.treemap_state.as_mut() {
                                state.toggle_category(category);
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }
                    }))
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
        // The reset is permanent at the end of the legend, whether or not a category is
        // currently hidden: the legend's trailing control must not appear and disappear with
        // the filter state.
        let reset = self.i18n.get_text("treemap_filter_all");
        row = row.child(
            Button::new("treemap-filter-reset")
                .ghost()
                .xsmall()
                .icon(IconName::ListFilter)
                .tooltip(reset.clone())
                .accessibility_label(reset)
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(state) = this.treemap_state.as_mut() {
                        state.show_all_categories();
                    }
                    cx.notify();
                })),
        );
        Some(row.into_any_element())
    }

    /// Refreshes whichever data surface the status band is describing. The loading/refresh
    /// control is the same in the table and Treemap; it starts a new query, a new Treemap scan,
    /// a new bin listing, or a service probe instead of animating a value.
    pub(crate) fn refresh_current_view(&mut self, cx: &mut Context<Self>) {
        if self.recycle_bin.is_open() {
            self.begin_recycle_bin_load(cx);
            return;
        }
        if self.drive_view.is_some() {
            if let Some(path) = self
                .treemap_state
                .as_ref()
                .map(|state| state.current_dir().to_string())
            {
                self.start_treemap_scan(std::path::PathBuf::from(path), cx);
            }
            return;
        }
        if !self.service.is_service_connected {
            self.check_service_connection(cx);
            return;
        }
        self.trigger_search(cx);
    }
}
