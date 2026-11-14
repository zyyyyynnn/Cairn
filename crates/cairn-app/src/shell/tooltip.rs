// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! One tooltip entrance for every trigger.
//!
//! Component controls get their slide-and-fade from the managed tooltip system. Plain
//! elements attach tooltips through GPUI's own path, which shows them without an entrance;
//! routing those through this builder keeps every tooltip moving the same way.

use gpui_kit::component::animation::{ease_out_cubic, EffectTransition};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::gpui::*;
use std::hash::{Hash as _, Hasher as _};
use std::time::Duration;

/// The managed tooltip entrance: a 4 px upward slide under a fade, 150 ms.
const ENTER: Duration = Duration::from_millis(150);

struct AppTooltip {
    content: AnyView,
    animation_id: u64,
}

impl Render for AppTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = div().child(self.content.clone());
        if cx.reduce_motion() {
            return content.into_any_element();
        }
        EffectTransition::new(ENTER)
            .ease(ease_out_cubic)
            .slide_y(px(4.), px(0.))
            .fade(0.0, 1.0)
            .apply(
                content,
                ElementId::NamedInteger("app-tooltip".into(), self.animation_id),
            )
            .into_any_element()
    }
}

/// Builds a tooltip with the same entrance a component control's tooltip uses.
pub fn text(text: impl Into<SharedString> + 'static) -> impl Fn(&mut Window, &mut App) -> AnyView {
    let text = text.into();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    let animation_id = hasher.finish();
    move |window, cx| {
        let content = Tooltip::new(text.clone()).build(window, cx);
        cx.new(|_| AppTooltip {
            content,
            animation_id,
        })
        .into()
    }
}
