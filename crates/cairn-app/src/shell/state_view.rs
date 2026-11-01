// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The one centred band every empty, warning and error state shares.

use crate::app::layout;
use crate::shell::accessibility::{self, ControlElement, KeyControl};
use crate::shell::theme::{self, ShellTokens};
use gpui_kit::gpui::*;

/// One centred 32px icon, one sentence, and the optional recovery action.
/// The sentence carries the whole message; there is no second description
/// paragraph to drift from it.
pub(crate) fn render_state_band(
    tokens: &ShellTokens,
    control: &'static KeyControl,
    message: String,
    icon: AnyElement,
    action: Option<AnyElement>,
) -> ControlElement {
    let band = accessibility::element(control)
        .aria_label(message.clone())
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .gap(rems(layout::SPACE_12))
        .py(rems(layout::SPACE_48))
        .bg(tokens.background)
        .child(icon)
        .child(
            div()
                .max_w(rems(layout::MESSAGE_MAX_WIDTH))
                .text_center()
                .text_base()
                .font(theme::serif_font(FontWeight::SEMIBOLD))
                .line_height(relative(1.4))
                .text_color(tokens.foreground)
                .child(theme::heading_text(message, FontWeight::SEMIBOLD)),
        );
    match action {
        Some(action) => band.child(action),
        None => band,
    }
}
