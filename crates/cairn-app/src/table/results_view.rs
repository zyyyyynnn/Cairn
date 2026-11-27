// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Results table region: viewport scrolling containers, virtual row band, and state overlays.

use crate::app::{CairnApp, layout};
use crate::shell::accessibility::{self, ControlElement};
use crate::shell::command_groups::GroupCommand;
use crate::shell::commands::RESULTS_CONTEXT;
use crate::shell::theme::{ShellTokens, overlay_scrollbar};
use crate::table::rows::{RowBand, rows_to_paint};
use crate::table::surface::{ResultSurface, ResultSurfaceInputs, result_surface};
use crate::view::announce_shortcut;
use gpui_kit::component::menu::ContextMenuExt;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::{InteractiveElementExt as _, h_flex, v_flex};
use gpui_kit::gpui::prelude::FluentBuilder as _;
use gpui_kit::gpui::*;
use std::ops::Range;
use std::rc::Rc;

const CONTEXT_MENU_MIN_WIDTH: f32 = 12.0;

impl CairnApp {
    pub(crate) fn render_results(
        &mut self,
        tokens: &ShellTokens,
        visible: Range<usize>,
        row_height: Pixels,
        results_hold_focus: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let search_focused = self.query.search_focus.is_focused(window);
        let header = self.render_table_header(tokens, cx);
        let surface = result_surface(ResultSurfaceInputs {
            total_rows: self.results.table.total_rows(),
            location: self.recycle_bin.surface_location(),
            query_in_flight: self.query.is_searching(),
            index_catching_up: self.service.status.is_any_loading(),
            service_connected: self.service.connection.is_connected(),
            service_issue: self.service.connection.issue().is_some(),
            probing_service: self.service.connection.is_probing(),
            has_query: self.has_query(),
        });
        let rows = (surface == ResultSurface::Rows)
            .then(|| self.render_rows_band(tokens, visible, row_height, search_focused, cx));
        let state = match surface {
            ResultSurface::Empty => Some(self.render_empty_state(tokens, cx)),
            ResultSurface::Offline => Some(self.render_service_offline_state(tokens, cx)),
            ResultSurface::Rows | ResultSurface::Loading => None,
        };

        let has_content = rows.is_some();
        let has_state = state.is_some();
        let rows = rows.map(gpui_kit::IntoElement::into_any_element);
        let state = state.map(gpui_kit::IntoElement::into_any_element);
        let overflows_sideways = self.results.table_scroll.max_offset().x > px(0.0);
        let revealing = self.table_reveal.is_revealing();
        let reveal_epoch = self.table_reveal.epoch();

        let viewport = div()
            .id("result-table-viewport")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_x_scroll()
            .track_scroll(&self.results.table_scroll)
            .lock_scroll_axis()
            .child(
                v_flex()
                    .w_full()
                    .min_w(rems(self.table_min_width()))
                    .flex_1()
                    .min_h_0()
                    .child(header)
                    .when_some(rows, gpui_kit::ParentElement::child),
            );

        let control = accessibility::control("result-table");
        announce_shortcut(accessibility::element(control), control)
            .aria_label(accessibility::name(control, &self.i18n))
            .key_context(RESULTS_CONTEXT)
            .track_focus(&self.results.results_focus)
            .on_action(cx.listener(Self::scroll_table_left))
            .on_action(cx.listener(Self::scroll_table_right))
            .on_action(cx.listener(Self::scroll_table_start))
            .on_action(cx.listener(Self::scroll_table_end))
            .on_action(cx.listener(Self::preview_snippet))
            .flex_1()
            .min_w_0()
            .min_h_0()
            .relative()
            .flex()
            .flex_col()
            .child(
                h_flex()
                    .when(has_content, gpui_kit::Styled::flex_1)
                    .min_w_0()
                    .min_h_0()
                    .items_stretch()
                    .relative()
                    .child(viewport)
                    .child(
                        v_flex()
                            .flex_shrink_0()
                            .h_full()
                            .w(Scrollbar::width())
                            .when(has_content, |this| this.bg(tokens.background))
                            .when(!has_content, |this| this.bg(tokens.muted))
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .h(rems(layout::HEADER_HEIGHT))
                                    .bg(tokens.table_head)
                                    .border_b_1()
                                    .border_color(tokens.border),
                            )
                            .child(div().flex_1().min_h_0().when(
                                has_content && self.results.table.total_rows() > 0,
                                |this| {
                                    this.child(overlay_scrollbar(
                                        Scrollbar::vertical(&self.results.rows_scroll)
                                            .id("result-table-vscroll")
                                            .viewport_from_layout(),
                                    ))
                                },
                            )),
                    ),
            )
            .when(overflows_sideways, |this| {
                this.child(
                    div()
                        .flex_shrink_0()
                        .w_full()
                        .h(Scrollbar::width())
                        .child(overlay_scrollbar(
                            Scrollbar::horizontal(&self.results.table_scroll)
                                .id("result-table-hscroll")
                                .viewport_from_layout(),
                        )),
                )
            })
            .when_some(state, gpui_kit::ParentElement::child)
            .when(surface == ResultSurface::Loading, |this| {
                this.child(
                    div()
                        .id("table-loading-overlay")
                        .absolute()
                        .inset_0()
                        .when(overflows_sideways, |this| this.bottom(Scrollbar::width()))
                        .bg(tokens.muted),
                )
            })
            .when(revealing && (has_content || has_state), |this| {
                this.child(
                    div()
                        .id("table-reveal-fade-overlay")
                        .absolute()
                        .inset_0()
                        .when(overflows_sideways, |this| this.bottom(Scrollbar::width()))
                        .bg(tokens.muted)
                        .with_animation(
                            format!("table-reveal-{reveal_epoch}"),
                            crate::motion::mask_dissolve_animation(),
                            |element, delta| element.opacity(1.0 - delta),
                        ),
                )
            })
            .when(results_hold_focus, |this| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(tokens.radius)
                        .border_1()
                        .border_color(tokens.ring),
                )
            })
    }

    fn render_rows_band(
        &mut self,
        tokens: &ShellTokens,
        visible: Range<usize>,
        row_height: Pixels,
        search_focused: bool,
        cx: &mut Context<'_, Self>,
    ) -> ControlElement {
        let extent = px(self.results.table.scroll_extent(f32::from(row_height)));
        let painted = rows_to_paint(
            visible.clone(),
            self.results.table.loaded_range(),
            self.results.table.scroll_top,
            self.results.table.viewport_height,
            f32::from(row_height),
        );
        let rows = self.collect_visible_rows(
            painted.range.clone(),
            tokens,
            row_height,
            !painted.pinned,
            search_focused,
            cx,
        );
        let band_top = painted.top;
        let pinned = painted.pinned;
        let first = visible.start;

        let control = accessibility::control("result-band");
        accessibility::element(control)
            .aria_label(accessibility::name(control, &self.i18n))
            .aria_orientation(Orientation::Vertical)
            .when(!pinned, |band| band.aria_numeric_value(first as f64))
            .flex_1()
            .min_h_0()
            .relative()
            .bg(tokens.background)
            .child(
                div()
                    .id("result-rows-scroll")
                    .absolute()
                    .inset_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.results.rows_scroll)
                    .lock_scroll_axis()
                    .child(div().absolute().left_0().right_0().top_0().h(extent)),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .overflow_hidden()
                    .on_scroll(cx.listener(|this, event: &ScrollEvent, window, cx| {
                        let delta = event.delta.pixel_delta(window.line_height());
                        if delta.y.is_zero() {
                            return;
                        }
                        let current = this.scrolled_pixels();
                        let max = this
                            .results
                            .table
                            .max_scroll_top(f32::from(layout::row_height(window)));
                        let next = (current - f32::from(delta.y)).clamp(0.0, max);
                        if next != current {
                            this.results.snippet_preview = None;
                            this.results.table.scroll_top = next;
                            this.results
                                .rows_scroll
                                .set_offset(point(px(0.0), -px(next)));
                            this.ensure_visible_rows_loaded(layout::row_height(window), cx);
                            cx.notify();
                        }
                        cx.stop_propagation();
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.dismiss_result_selection(cx)),
                    )
                    .child(self.render_rows_stack(band_top, !pinned, rows, cx)),
            )
    }

    /// Mounts one shared context menu on the visible row stack so scrolling does not recreate a
    /// `ContextMenuState` and `FocusHandle` per row on every frame.
    fn render_rows_stack(
        &self,
        band_top: f32,
        interactive: bool,
        rows: Vec<AnyElement>,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let has_rows = !rows.is_empty();
        let stack = div()
            .id("result-rows-stack")
            .absolute()
            .left_0()
            .right_0()
            .top(px(band_top))
            .flex()
            .flex_col()
            .children(rows);
        if !interactive || !has_rows {
            return stack.into_any_element();
        }
        let profile = self.location_profile();
        let commands = profile.result_context_commands();
        let i18n = Rc::clone(&self.i18n);
        let app = cx.entity();
        stack
            .context_menu(move |menu, window, cx| {
                let menu_entity = cx.entity();
                app.update(cx, |this, cx| {
                    this.tooltip_suppression.suppress();
                    let subscription = cx.subscribe_in(
                        &menu_entity,
                        window,
                        move |this, _menu, _: &DismissEvent, _window, cx| {
                            if this.tooltip_suppression.restore() {
                                cx.notify();
                            }
                        },
                    );
                    this.result_menu_dismiss = Some(subscription);
                    cx.notify();
                });
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
                    let label = i18n.get_text(profile.command_label_key(*command));
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

    fn collect_visible_rows(
        &self,
        visible: Range<usize>,
        tokens: &ShellTokens,
        row_height: Pixels,
        interactive: bool,
        search_focused: bool,
        cx: &mut Context<'_, Self>,
    ) -> Vec<AnyElement> {
        let band = RowBand {
            total: self.results.table.total_rows(),
            tokens,
            row_height,
            interactive,
            search_focused,
        };
        let is_fetching = self.query.is_fetching_more;
        visible
            .map(|ix| match self.results.table.row(ix) {
                Some(row) => self
                    .render_result_row(ix, row, &band, cx)
                    .into_any_element(),
                None => self.render_placeholder_row(ix, is_fetching, tokens, row_height),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::table::rows::{rows_band_local_top, rows_to_paint};

    #[test]
    fn rows_are_positioned_from_a_local_residual_at_any_depth() {
        let row_height = 28.0;
        assert_eq!(rows_band_local_top(0, 0.0, row_height), 0.0);
        assert_eq!(rows_band_local_top(100, 2810.0, row_height), -10.0);
        assert_eq!(rows_band_local_top(97, 2810.0, row_height), -94.0);

        let scroll_top = 1_300_000.0f32 * row_height - 10.0;
        let start_row = (scroll_top / row_height).floor() as usize;
        let first = start_row.saturating_sub(3);
        let local = rows_band_local_top(first, scroll_top, row_height);
        assert!((-4.0 * row_height..=0.0).contains(&local));
        let later = rows_band_local_top(first + 3, scroll_top, row_height);
        assert_eq!(later - local, 3.0 * row_height);
    }

    #[test]
    fn a_viewport_inside_the_window_paints_at_its_scroll_residual() {
        let painted = rows_to_paint(100..120, 0..200, 2810.0, 440.0, 28.0);
        assert_eq!(painted.range, 100..120);
        assert_eq!(painted.top, -10.0);
        assert!(!painted.pinned);
    }

    #[test]
    fn an_unloaded_jump_keeps_the_nearest_loaded_rows_on_screen() {
        let row_height = 28.0;
        let viewport = 440.0;

        let visible = 1000..1016;
        let loaded = 0..200;
        let painted = rows_to_paint(visible.clone(), loaded, 28_000.0, viewport, row_height);
        assert_eq!(painted.range.end, 200);
        assert_eq!(painted.range.len(), visible.len());
        assert!(painted.pinned);
        assert_eq!(
            painted.top + painted.range.len() as f32 * row_height,
            viewport
        );

        let painted = rows_to_paint(0..16, 200..400, 0.0, viewport, row_height);
        assert_eq!(painted.range, 200..216);
        assert_eq!(painted.top, 0.0);
        assert!(painted.pinned);
    }

    #[test]
    fn a_fully_loaded_result_set_is_never_pinned() {
        let painted = rows_to_paint(84..101, 0..101, 2_380.0, 440.0, 28.0);
        assert_eq!(painted.range, 84..101);
        assert_eq!(painted.top, 84.0 * 28.0 - 2_380.0);
        assert!(!painted.pinned);
    }
}
