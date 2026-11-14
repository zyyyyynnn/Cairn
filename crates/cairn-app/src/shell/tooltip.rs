// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! One tooltip entrance for every trigger.
//!
//! gpui-kit component tooltips animate through the managed overlay. A GPUI-native
//! `.tooltip()` builder, the form the result table, path band and Treemap use, shows its
//! view without one; this wrapper gives those views the same 150 ms, 4 px entrance.

use gpui_kit::component::animation::{ease_out_cubic, EffectTransition};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::gpui::*;
use std::time::Duration;

/// Matches the managed tooltip entrance: a 4 px upward slide under a fade, 150 ms.
const ENTER_DURATION: Duration = Duration::from_millis(150);

struct AppTooltip {
    content: AnyView,
}

impl Render for AppTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let content = div().child(self.content.clone());
        EffectTransition::new(ENTER_DURATION)
            .ease(ease_out_cubic)
            .slide_y(px(4.), px(0.))
            .fade(0.0, 1.0)
            .apply(content, ElementId::NamedInteger("app-tooltip".into(), 0))
            .into_any_element()
    }
}

/// Builds a tooltip whose entrance matches a managed component tooltip.
pub fn text(text: impl Into<SharedString> + 'static) -> impl Fn(&mut Window, &mut App) -> AnyView {
    let text = text.into();
    move |window, cx| {
        let content = Tooltip::new(text.clone()).build(window, cx);
        cx.new(|_| AppTooltip { content }).into()
    }
}
