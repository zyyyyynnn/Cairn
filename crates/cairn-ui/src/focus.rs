// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Keyboard focus presentation shared by Cairn's windows.
//!
//! The product's focus frame is one reserved pixel of foreground ink, flush on the control's
//! visible surface. A control that touches the window edge insets its surface instead of the
//! frame, so the line stays clear of the native frame and the rounded corner while every frame
//! keeps the same alignment to its control. The frame is drawn only while the keyboard is driving,
//! so a pointer click focuses a control without adding a second outline. gpui-kit's own 1 px focus
//! line remains the framework components' cue.

use gpui_kit::component::{Icon, IconName};
use gpui_kit::gpui::{
    div, px, rems, AnyElement, App, FocusHandle, Global, Hsla, IntoElement as _,
    ParentElement as _, Pixels, Styled as _, Subscription, Window,
};

/// The focus frame's width. One pixel, flush on the control's visible surface, matching the
/// product's other focus frames.
pub const RING_WIDTH: Pixels = px(1.0);
pub const CAPTION_WIDTH: Pixels = px(34.0);
pub const CAPTION_FOCUS_SIZE: Pixels = px(22.0);

/// Whether the product's last input was a keystroke.
struct KeyboardNavigation(bool);

impl Global for KeyboardNavigation {}

/// Whether the keyboard is currently driving focus in this process.
pub fn keyboard_navigation(cx: &App) -> bool {
    cx.try_global::<KeyboardNavigation>()
        .map(|mode| mode.0)
        .unwrap_or(false)
}

/// Switches the product between keyboard and pointer focus presentation.
pub fn set_keyboard_navigation(enabled: bool, cx: &mut App) {
    if keyboard_navigation(cx) == enabled {
        return;
    }
    if cx.try_global::<KeyboardNavigation>().is_some() {
        cx.global_mut::<KeyboardNavigation>().0 = enabled;
    } else {
        cx.set_global(KeyboardNavigation(enabled));
    }
}

/// Records every keystroke, including the Tab that the focus walk consumes before an element
/// listener can see it. The caller keeps the returned subscription for the window's lifetime.
pub fn observe_keyboard(cx: &mut App) -> Subscription {
    cx.intercept_keystrokes(|_event, _window, cx| set_keyboard_navigation(true, cx))
}

/// Whether `focus` is this window's keyboard-driven focus target.
///
/// `contains_focused` keeps a composite's frame alive while focus moves into one of its children;
/// the modality flag is what keeps a pointer click from painting a frame at all.
pub fn keyboard_focused(
    focus: &FocusHandle,
    keyboard_navigation: bool,
    window: &Window,
    cx: &App,
) -> bool {
    keyboard_navigation && focus.contains_focused(window, cx)
}

/// A one-pixel frame on a control's visible surface. A control that touches the window edge
/// places its surface a little inside the edge, so the frame needs no special offset here.
pub fn edge(
    focus: &FocusHandle,
    ink: Hsla,
    radius: Pixels,
    keyboard_navigation: bool,
    window: &Window,
    cx: &App,
) -> Option<AnyElement> {
    keyboard_focused(focus, keyboard_navigation, window, cx).then(|| {
        div()
            .absolute()
            .inset_0()
            .border(RING_WIDTH)
            .border_color(ink)
            .rounded(radius)
            .into_any_element()
    })
}

/// Centered caption glyph box shared by desktop and installer titlebar window buttons.
pub fn caption_glyph(
    icon: impl Into<IconName>,
    icon_size_rem: f32,
    focus: &FocusHandle,
    ink: Hsla,
    keyboard_navigation: bool,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let mut cell = div()
        .relative()
        .size(CAPTION_FOCUS_SIZE)
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .child(Icon::new(icon.into()).size(rems(icon_size_rem)));
    if let Some(ring) = edge(focus, ink, px(0.0), keyboard_navigation, window, cx) {
        cell = cell.child(ring);
    }
    cell.into_any_element()
}
