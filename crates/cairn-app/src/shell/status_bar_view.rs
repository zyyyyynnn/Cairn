// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The bottom status band: the number of objects the current query matched.

use crate::app::{layout, CairnApp};
use crate::search::ResultSetChange;
use crate::shell::accessibility::{self, ControlElement};
use crate::shell::status_bar::format_count;
use crate::shell::theme::{self, ShellTokens};
use cairn_protocol::message::QueryDegradedReason;
use fluent_bundle::FluentArgs;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::{h_flex, Icon, Sizable as _};
use gpui_kit::gpui::*;

/// The band marks a loading state only while the current view has nothing to show.
///
/// A volume that is still catching up does not blank a table that already has rows: that
/// progress belongs to the index state, and the band keeps showing the result summary.
fn status_waiting(
    awaiting_results: bool,
    index_loading: bool,
    searching: bool,
    probing: bool,
) -> bool {
    awaiting_results && (index_loading || searching || probing)
}

impl CairnApp {
    pub(crate) fn render_status_line(
        &self,
        tokens: &ShellTokens,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ControlElement {
        let bar = accessibility::control("status-bar");
        let mut summary: Option<AnyElement> = None;
        let mut summary_aria: Option<String> = None;
        let mut summary_loading = false;
        let mut icon_loading = false;
        let mut view_name = "table";

        if self.recycle_bin.error().is_some() {
            view_name = "recycle-bin";
            let text = self.cached_text("status_recycle_bin_unavailable");
            summary_aria = Some(text.to_string());
            summary = Some(
                div()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_features(theme::tabular_figures())
                    .child(text)
                    .into_any_element(),
            );
        } else if self.recycle_bin.is_loading() {
            view_name = "recycle-bin";
            summary_loading = true;
            icon_loading = true;
        } else if let Some(items) = self.recycle_bin.rows() {
            view_name = "recycle-bin";
            let count_text = {
                let mut args = FluentArgs::new();
                args.set("count", items.len() as i64);
                args.set("count_text", format_count(items.len()));
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
        } else {
            let index_loading = self.service.status.is_any_loading();
            let awaiting_results = self.results.table.total_rows() == 0;
            // The band marks only a wait that blocks what the view can show. Once rows are on
            // screen the table is usable while a volume catches up; that volume state is not a
            // result-band wait.
            summary_loading = status_waiting(
                awaiting_results,
                index_loading,
                self.query.is_searching(),
                self.service.connection.is_probing(),
            );
            // Paging keeps the refresh control: rows already on screen are not a first-page wait.
            icon_loading = summary_loading;
            if !summary_loading
                && self.service.connection.is_connected()
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

        // A selection replaces the total summary with the count the next bulk action will
        // actually receive.
        if !summary_loading && !self.results.table.selected_indices.is_empty() {
            let selected = self.results.table.selected_indices.len();
            let mut args = FluentArgs::new();
            args.set("count", selected as i64);
            args.set("count_text", format_count(selected));
            let text = self.i18n.get_text_with_args("status_selected_count", &args);
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

        // The native xsmall body is 20px; a 24px minimum would let the hover surface
        // extend a row above/below the status line.
        //
        // While the current view waits for its own data, the refresh button keeps its footprint
        // and takes the component's own loading state: the RefreshCw glyph becomes the spinner,
        // the button dims and stops answering clicks. A second circular mark beside the refresh
        // arrow would read as two loaders.
        let refresh_label = self.cached_text(if icon_loading {
            "status_refreshing"
        } else {
            "status_refresh"
        });
        let refresh = Button::new("status-refresh")
            .ghost()
            .xsmall()
            .icon(IconName::RefreshCw)
            .loading_icon(IconName::LoaderCircle)
            .loading(icon_loading)
            .tooltip(refresh_label.clone())
            .accessibility_label(refresh_label)
            .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                this.refresh_current_view(cx);
            }));
        let refresh_slot = div()
            .flex_shrink_0()
            // The window root clears selection on every left press; the idle refresh is a control,
            // not a click on the canvas.
            .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| {
                cx.stop_propagation()
            })
            .child(refresh);

        let mut right: Option<AnyElement> = None;
        let in_index_table = !self.recycle_bin.is_open();
        if in_index_table && !summary_loading {
            if let Some(degraded) = &self.query.degraded {
                let notice = match degraded {
                    QueryDegradedReason::PropertyIndexDisabled { .. } => {
                        self.cached_text("query_degraded_property_index")
                    }
                };
                summary_aria = Some(match summary_aria.take() {
                    Some(existing) => format!("{existing} {notice}"),
                    None => notice.to_string(),
                });
                right = Some(
                    h_flex()
                        .items_center()
                        .gap(rems(layout::SPACE_4))
                        .flex_shrink_0()
                        .text_sm()
                        .text_color(tokens.warning_ink)
                        .child(
                            Icon::new(IconName::TriangleAlert)
                                .xsmall()
                                .flex_shrink_0()
                                .text_color(tokens.warning_ink),
                        )
                        .child(notice)
                        .into_any_element(),
                );
            }
        }

        // Opt-in acceptance trace (`CAIRN_PERF_LOG`): the visible summary and the loading state,
        // never a hidden animated value.
        crate::shell::perf::mark(&format!(
            "status frame summary={summary_aria:?} bytes={} summary_loading={summary_loading} icon_loading={icon_loading} view={view_name} qid={}",
            self.service.status.objects_bytes(),
            self.query.query_id(),
        ));

        let aria_summary: SharedString =
            summary_aria.map(SharedString::from).unwrap_or_else(|| {
                if summary_loading {
                    self.cached_text("status_loading")
                } else {
                    self.cached_text("status_refresh")
                }
            });

        let mut left = h_flex()
            .min_w_0()
            .items_center()
            .gap(rems(layout::SPACE_4))
            .child(refresh_slot);
        if let Some(summary) = summary {
            left = left.child(div().min_w_0().flex_1().truncate().child(summary));
        }
        let mut status_bar = StatusBar::new()
            .h(rems(layout::STATUS_HEIGHT))
            .flex_shrink_0()
            .px(rems(layout::CONTENT_INSET))
            .border_t_1()
            .border_color(tokens.border)
            .font_features(theme::tabular_figures())
            .text_sm()
            .bg(tokens.status_bar)
            .text_color(tokens.muted_foreground)
            .left(left);
        if let Some(right_slot) = right {
            status_bar = status_bar.right(right_slot);
        }

        accessibility::element(bar)
            .aria_label(aria_summary)
            // Keep chrome pinned at its fixed height when results overflow taller than the window.
            .flex_shrink_0()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.dismiss_result_selection_on_chrome_press(cx)),
            )
            .child(status_bar)
    }

    pub(crate) fn refresh_current_view(&mut self, cx: &mut Context<Self>) {
        if self.recycle_bin.is_open() {
            self.begin_recycle_bin_load(cx);
            cx.notify();
            return;
        }
        if !self.service.connection.is_connected() {
            self.check_service_connection(cx);
            return;
        }
        self.request_result_set(ResultSetChange::Refresh, cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::status_waiting;

    #[test]
    fn status_waiting_requires_empty_table_and_active_work() {
        assert!(!status_waiting(false, true, true, true));
        assert!(status_waiting(true, true, false, false));
        assert!(status_waiting(true, false, true, false));
        assert!(status_waiting(true, false, false, true));
        assert!(!status_waiting(true, false, false, false));
    }
}
