// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Search input band and matching options trigger view.

use crate::app::{CairnApp, layout};
use crate::shell::accessibility::{self, ControlElement};
use crate::shell::theme::{self, ShellTokens};
use crate::view::announce_shortcut;
use gpui_kit::assets::IconName as AssetIconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, IconName, Sizable as _, h_flex};
use gpui_kit::gpui::prelude::FluentBuilder as _;
use gpui_kit::gpui::*;

/// One pixel: the caret and the hairline edges the shell draws itself. Not a rem token, so it does
/// not scale with interface zoom.
const HAIRLINE: Pixels = theme::HAIRLINE;

impl CairnApp {
    pub(crate) fn render_search_band(
        &self,
        tokens: &ShellTokens,
        is_focused: bool,
        focus_handle: FocusHandle,
        view: Entity<CairnApp>,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Div {
        h_flex()
            .h(rems(layout::SEARCH_BAND_HEIGHT))
            .items_center()
            .pl(rems(layout::SEARCH_BAND_LEAD))
            .pr(rems(layout::SEARCH_BAND_LEAD))
            .bg(tokens.chrome)
            .child(self.render_query_box(tokens, is_focused, focus_handle, view, window, cx))
    }

    pub(crate) fn render_query_box(
        &self,
        tokens: &ShellTokens,
        is_focused: bool,
        focus_handle: FocusHandle,
        view: Entity<CairnApp>,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> ControlElement {
        let control = accessibility::control("search-input");
        let clear = accessibility::control("search-clear");
        let metrics = self.query.query_text_metrics.clone();
        let accessible_name = match self.location_profile().search_aria_label_key {
            Some(key) => self.cached_text(key),
            None => self.cached_control_name(control),
        };

        let clear_label = self.cached_control_name(clear);
        let show_clear = !self.query.search_input.text.is_empty();
        announce_shortcut(accessibility::element(control), control)
            .aria_label(accessible_name)
            .aria_placeholder(self.placeholder_text())
            .track_focus(&focus_handle)
            .relative()
            .flex_1()
            .min_w_0()
            .h(rems(layout::ROW_HEIGHT))
            .pl(rems(layout::SEARCH_INPUT_LEAD))
            .pr(rems(layout::SEARCH_INPUT_LEAD))
            .gap(rems(layout::INNER_SPACING))
            .flex()
            .items_center()
            .rounded(tokens.radius)
            .bg(if is_focused {
                tokens.background
            } else {
                tokens.muted
            })
            .border_1()
            .border_color(if is_focused {
                tokens.ring
            } else {
                tokens.control_stroke
            })
            .text_color(tokens.foreground)
            .cursor(CursorStyle::IBeam)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.dismiss_result_selection_on_chrome_press(cx);
                    this.query.search_focus.focus(window, cx);
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                this.handle_query_edit_key(event, cx);
            }))
            .child(
                Icon::new(IconName::Search)
                    .small()
                    .flex_shrink_0()
                    .text_color(tokens.muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .relative()
                    .h_full()
                    .flex()
                    .items_center()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            this.dismiss_result_selection_on_chrome_press(cx);
                            this.query.search_focus.focus(window, cx);
                            let byte_idx = this.query_byte_index_for_point(event.position, window);
                            this.handle_search_bar_mouse_down(
                                byte_idx,
                                event.click_count,
                                event.modifiers.shift,
                            );
                            cx.notify();
                        }),
                    )
                    .child(
                        canvas(
                            |_bounds, _window, _cx| {},
                            move |bounds, _, window, cx| {
                                {
                                    let style = window.text_style();
                                    let mut metrics = metrics.borrow_mut();
                                    metrics.bounds = bounds;
                                    metrics.font_size =
                                        style.font_size.to_pixels(window.rem_size());
                                    metrics.style = style;
                                }
                                window.handle_input(
                                    &focus_handle,
                                    ElementInputHandler::new(bounds, view.clone()),
                                    cx,
                                );
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(self.render_query_text(tokens, is_focused, window)),
            )
            .when(show_clear, |this| {
                this.child(
                    Button::new(clear.element)
                        .ghost()
                        .small()
                        .min_w(rems(24.0 / theme::BASE_FONT_PX))
                        .min_h(rems(24.0 / theme::BASE_FONT_PX))
                        .flex_shrink_0()
                        .icon(AssetIconName::X)
                        .accessibility_label(clear_label)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.dismiss_result_selection_on_chrome_press(cx);
                            this.clear_query_and_scope(window, cx);
                        })),
                )
            })
    }

    pub(crate) fn render_query_text(
        &self,
        tokens: &ShellTokens,
        is_focused: bool,
        window: &Window,
    ) -> Div {
        if self.query.search_input.text.is_empty() {
            let placeholder = div()
                .text_color(tokens.muted_foreground)
                .child(self.placeholder_text());
            return div()
                .relative()
                .flex()
                .items_center()
                .min_w_0()
                .when(is_focused, |this| {
                    this.child(div().absolute().left_0().w(HAIRLINE).h_5().bg(tokens.caret))
                })
                .child(placeholder);
        }
        let text = &self.query.search_input.text;
        let (sel_start, sel_end) = self.query.search_input.normalized_selection();
        let has_selection = sel_start < sel_end;

        if !is_focused && !has_selection && self.query.search_input.marked_range.is_none() {
            return div()
                .flex()
                .items_center()
                .min_w_0()
                .overflow_hidden()
                .child(div().whitespace_nowrap().child(text.clone()));
        }

        let layout = self.query_text_layout(window);

        let mut container = div()
            .relative()
            .flex()
            .items_center()
            .min_w_0()
            .overflow_hidden();

        if has_selection {
            let sel_x1 = layout.x_for_index(sel_start);
            let sel_x2 = layout.x_for_index(sel_end);
            let (sel_left, sel_width) = if sel_x2 >= sel_x1 {
                (sel_x1, sel_x2 - sel_x1)
            } else {
                (sel_x2, sel_x1 - sel_x2)
            };
            container = container.child(
                div()
                    .absolute()
                    .left(sel_left)
                    .w(sel_width)
                    .h_5()
                    .bg(tokens.selection),
            );
        }

        if let Some(ref range) = self.query.search_input.marked_range {
            let mark_start = range.start.min(range.end).min(text.len());
            let mark_end = range.start.max(range.end).min(text.len());
            if mark_start < mark_end {
                let mark_x1 = layout.x_for_index(mark_start);
                let mark_x2 = layout.x_for_index(mark_end);
                let (mark_left, mark_width) = if mark_x2 >= mark_x1 {
                    (mark_x1, mark_x2 - mark_x1)
                } else {
                    (mark_x2, mark_x1 - mark_x2)
                };
                container = container.child(
                    div()
                        .absolute()
                        .left(mark_left)
                        .w(mark_width)
                        .bottom_0()
                        .h(HAIRLINE)
                        .bg(tokens.ring),
                );
            }
        }

        container = container.child(div().whitespace_nowrap().child(text.clone()));

        if is_focused && !has_selection {
            let caret_x = layout.x_for_index(sel_start);
            container = container.child(
                div()
                    .absolute()
                    .left(caret_x)
                    .w(HAIRLINE)
                    .h_5()
                    .bg(tokens.caret),
            );
        }

        container
    }

    /// Normal queries match names and paths; body search is the explicit `content:` term, so the
    /// placeholder does not promise content results for text that never asked for them.
    pub(crate) fn placeholder_text(&self) -> SharedString {
        self.cached_text(self.location_profile().search_placeholder_key)
    }

    pub(crate) fn handle_search_bar_mouse_down(
        &mut self,
        byte_idx: usize,
        click_count: usize,
        shift: bool,
    ) {
        if click_count == 1 {
            if shift {
                let anchor = self
                    .query
                    .text_selection_anchor
                    .unwrap_or(self.query.search_input.selection.start);
                self.query.text_selection_anchor = Some(anchor);
                self.query.search_input.selection = anchor..byte_idx;
            } else {
                self.query.text_selection_anchor = Some(byte_idx);
                self.query.search_input.selection = byte_idx..byte_idx;
            }
        } else if click_count == 2 {
            self.query.search_input.select_word_at(byte_idx);
            self.query.text_selection_anchor = None;
        } else if click_count >= 3 {
            self.query.search_input.select_all();
            self.query.text_selection_anchor = None;
        }
    }
}
