// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! One tooltip entrance shared by the desktop shell and installer.

use gpui_kit::base::motion::Presence;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::gpui::*;
use std::rc::Rc;

struct CairnTooltip {
    content: AnyView,
}

impl Render for CairnTooltip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let sample = Presence::new(
            ElementId::NamedInteger("cairn-tooltip".into(), cx.entity_id().as_u64()),
            true,
        )
        .transition(crate::motion::enter(crate::motion::TOOLTIP, cx))
        .sample(window, cx);
        let progress = sample.progress.clamp(0.0, 1.0);
        let offset_y = crate::motion::TOOLTIP_SLIDE * (1.0 - progress);
        div()
            .relative()
            .top(offset_y)
            .opacity(progress)
            .child(self.content.clone())
            .into_any_element()
    }
}

pub fn wrap(content: AnyView, cx: &mut App) -> AnyView {
    cx.new(|_| CairnTooltip { content }).into()
}

pub fn text(text: impl Into<SharedString> + 'static) -> impl Fn(&mut Window, &mut App) -> AnyView {
    let text = text.into();
    move |window, cx| wrap(Tooltip::new(text.clone()).build(window, cx), cx)
}

pub fn element<E, F>(builder: F) -> impl Fn(&mut Window, &mut App) -> AnyView
where
    E: IntoElement,
    F: Fn(&mut Window, &mut App) -> E + 'static,
{
    let builder = Rc::new(builder);
    move |window, cx| {
        let builder = builder.clone();
        wrap(
            Tooltip::element(move |window, cx| builder(window, cx)).build(window, cx),
            cx,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{element, text, wrap};
    use gpui_kit::gpui::{
        AnyView, AppContext as _, Context, IntoElement, ParentElement, Render, Window,
        WindowOptions, div,
    };

    struct Harness {
        child: AnyView,
    }

    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<'_, Self>) -> impl IntoElement {
            div().child(self.child.clone())
        }
    }

    #[test]
    fn text_element_and_wrap_builders_return_views() {
        gpui_kit::application().run(|cx| {
            gpui_kit::init(cx);
            crate::theme::apply_appearance(false, 1.0, cx);
            let (handle, _) = gpui_kit::open_window(
                WindowOptions {
                    show: false,
                    focus: false,
                    ..WindowOptions::default()
                },
                cx,
                |window, cx| {
                    let content = text("tooltip")(window, cx);
                    let wrapped = wrap(content, cx);
                    let custom = element(|_, _| div().child("tooltip"))(window, cx);
                    let _ = custom;
                    cx.new(|_| Harness { child: wrapped })
                },
            )
            .expect("tooltip test window opens");
            handle
                .update(cx, |_, window, cx| {
                    cx.set_reduce_motion(false);
                    window.draw(cx).clear(cx);
                    cx.set_reduce_motion(true);
                    window.draw(cx).clear(cx);
                })
                .expect("tooltip window draws");
            cx.quit();
        });
    }
}
