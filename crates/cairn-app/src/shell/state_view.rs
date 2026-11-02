// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The one centred band every empty, warning and error state shares.

use crate::app::layout;
use crate::shell::accessibility::{self, ControlElement, KeyControl};
use crate::shell::theme::{self, ShellTokens};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{h_flex, Sizable as _};
use gpui_kit::gpui::*;

/// One recovery action in a state surface or toast: a 24px semantic icon whose tooltip and
/// accessible name carry the full phrase, so the action never takes a row of its own.
pub(crate) fn icon_action(
    control: &'static KeyControl,
    icon: IconName,
    label: impl Into<SharedString>,
) -> Button {
    let label = label.into();
    Button::new(control.element)
        .ghost()
        .small()
        .flex_shrink_0()
        .min_w(rems(layout::SPACE_24))
        .min_h(rems(layout::SPACE_24))
        .icon(icon)
        .tooltip(label.clone())
        .accessibility_label(label)
}

/// One centred 32px icon, one sentence, and the optional recovery icon on the sentence line.
/// The sentence carries the whole message; there is no second description paragraph to drift
/// from it.
pub(crate) fn render_state_band(
    tokens: &ShellTokens,
    control: &'static KeyControl,
    message: String,
    icon: AnyElement,
    action: Option<AnyElement>,
) -> ControlElement {
    let aria_message = message.clone();
    let message = div()
        .max_w(rems(layout::MESSAGE_MAX_WIDTH))
        .min_w_0()
        .text_center()
        .text_base()
        .font(theme::serif_font(FontWeight::SEMIBOLD))
        .line_height(relative(1.4))
        .text_color(tokens.foreground)
        .child(theme::heading_text(message, FontWeight::SEMIBOLD));
    let sentence = match action {
        Some(action) => h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            // Equal flexible flanks keep the sentence on the band's centre line.
            // The action starts the trailing flank, so it follows the sentence
            // without pushing the sentence off centre.
            .child(div().flex_1())
            .child(message.flex_shrink(1.0))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .pl(rems(layout::SPACE_4))
                    .child(
                        // Seat the semantic icon on the serif sentence's measured ink
                        // centre instead of the flex line box.
                        div()
                            .relative()
                            .top(rems(layout::STATE_ACTION_OPTICAL_OFFSET))
                            .child(action),
                    ),
            )
            .into_any_element(),
        None => h_flex()
            .w_full()
            .items_center()
            .justify_center()
            .child(message)
            .into_any_element(),
    };
    accessibility::element(control)
        .aria_label(aria_message)
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
        .child(sentence)
}
