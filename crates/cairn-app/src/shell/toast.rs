// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! App-owned transient status: one quiet toast in the shell's own visual language.
//!
//! The component library's notification is a generic surface with its own
//! radius, density, timing and dismissal model. This app already has a dense
//! 1px/3px system, so the toast is composed from the same `ShellTokens`, `Button`
//! and `Icon` primitives as the rest of the shell. Informational toasts expire;
//! failures stay until the person dismisses them because they carry an outcome
//! that should not disappear before it is read.

use crate::app::{layout, CairnApp};
use crate::shell::accessibility;
use crate::shell::theme::ShellTokens;
use gpui_kit::assets::IconName as AssetIconName;
use gpui_kit::base::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{h_flex, Icon, Sizable as _};
use gpui_kit::gpui::*;
use gpui_kit::gpui::{ease_out_quint, Animation, AnimationExt};
use std::time::Duration;

/// Informational status is allowed to leave on its own; a failure is not.
const TOAST_INFO_DURATION: Duration = Duration::from_secs(6);
/// The toast is a reading surface, not a panel: one short measure, independent
/// of the result columns underneath it.
const TOAST_WIDTH_REM: f32 = 24.0;
/// Enter and exit share one short settle. Reduced motion skips the frames and
/// removes the card without delay.
const TOAST_MOTION: Duration = Duration::from_millis(140);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToastKind {
    Error,
    Info,
}

#[derive(Clone, Debug)]
pub(crate) struct ToastState {
    kind: ToastKind,
    title: String,
    message: String,
    closing: bool,
}

impl CairnApp {
    pub(crate) fn show_error_toast(
        &mut self,
        title: impl Into<String>,
        message: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.show_toast(ToastKind::Error, title.into(), message.into(), cx);
    }

    pub(crate) fn show_info_toast(
        &mut self,
        title: impl Into<String>,
        message: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.show_toast(ToastKind::Info, title.into(), message.into(), cx);
    }

    fn show_toast(
        &mut self,
        kind: ToastKind,
        title: String,
        message: String,
        cx: &mut Context<Self>,
    ) {
        self.toast_serial = self.toast_serial.wrapping_add(1);
        let serial = self.toast_serial;
        self.toast = Some(ToastState {
            kind,
            title,
            message,
            closing: false,
        });

        // A newer toast replaces this one before its timer fires; the serial
        // makes the stale timer a no-op instead of clearing the newer state.
        if kind == ToastKind::Info {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(TOAST_INFO_DURATION).await;
                let _ = this.update(cx, |this, cx| {
                    let still_current = this.toast_serial == serial
                        && this
                            .toast
                            .as_ref()
                            .is_some_and(|toast| toast.kind == ToastKind::Info);
                    if still_current {
                        this.begin_toast_exit(serial, cx);
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    pub(crate) fn dismiss_toast(&mut self, cx: &mut Context<Self>) {
        let serial = self.toast_serial;
        self.begin_toast_exit(serial, cx);
    }

    /// Keeps the card mounted for one short fade-out, then drops it. A newer toast bumps the
    /// serial, so a stale exit timer can never remove the replacement.
    fn begin_toast_exit(&mut self, serial: u64, cx: &mut Context<Self>) {
        let Some(toast) = self.toast.as_mut() else {
            return;
        };
        if self.toast_serial != serial || toast.closing {
            return;
        }
        toast.closing = true;
        if cx.reduce_motion() {
            self.toast = None;
            cx.notify();
            return;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_MOTION).await;
            let _ = this.update(cx, |this, cx| {
                let still_closing = this.toast_serial == serial
                    && this.toast.as_ref().is_some_and(|toast| toast.closing);
                if still_closing {
                    this.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(crate) fn render_toast(
        &self,
        toast: ToastState,
        tokens: &ShellTokens,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let control = accessibility::control("toast");
        let dismiss = accessibility::control("toast-dismiss");
        let dismiss_label = accessibility::name(dismiss, &self.i18n);
        let serial = self.toast_serial;
        let closing = toast.closing;
        let is_error = toast.kind == ToastKind::Error;
        let icon = if is_error {
            AssetIconName::CircleAlert
        } else {
            AssetIconName::Info
        };
        let icon_color = if is_error {
            tokens.danger
        } else {
            tokens.muted_foreground
        };
        let close = Button::new(dismiss.element)
            .ghost()
            .small()
            .min_w(rems(layout::SPACE_24))
            .min_h(rems(layout::SPACE_24))
            .icon(AssetIconName::X)
            .accessibility_label(dismiss_label)
            .on_click(cx.listener(|this, _, _, cx| this.dismiss_toast(cx)));

        let bottom = rems(layout::STATUS_HEIGHT + layout::SPACE_8);
        let travel = rems(layout::SPACE_8);
        accessibility::element(control)
            .aria_label(format!("{}. {}", toast.title, toast.message))
            // The shell root paints this after its content, so a click on the
            // card and its close control never falls through to the table.
            .occlude()
            .absolute()
            // Above the status bar, not over the query band and table header: the
            // notification must never occlude the primary input or its clear control.
            .bottom(bottom)
            .right(rems(layout::CONTENT_INSET))
            .w(rems(TOAST_WIDTH_REM))
            .p(rems(layout::SPACE_12))
            .flex()
            .flex_col()
            .rounded(tokens.radius)
            .bg(tokens.background)
            .border_1()
            .border_color(tokens.border)
            .child(
                // Icon, title and close share one centre line. The earlier
                // start-aligned row put a 16px icon, a 20px title line and a
                // 24px close target on three different vertical centres.
                h_flex()
                    .w_full()
                    .items_center()
                    .gap(rems(layout::INNER_SPACING))
                    .child(
                        Icon::new(icon)
                            .small()
                            .flex_shrink_0()
                            .text_color(icon_color),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_medium()
                            .text_color(tokens.foreground)
                            .child(toast.title),
                    )
                    .child(
                        close
                            // An icon-only 24px target keeps a 16px glyph
                            // inset by 4px on each side; pull the target toward
                            // the card edge so the glyph, not the target, sits
                            // on the same 12px padding spine as the left icon.
                            .mr(rems(-layout::SPACE_4)),
                    ),
            )
            .child(
                div()
                    .pl(rems(layout::SPACE_24))
                    .pt(rems(layout::SPACE_4))
                    .text_xs()
                    .text_color(tokens.muted_foreground)
                    .whitespace_normal()
                    .child(toast.message),
            )
            .with_animation(
                if closing {
                    format!("toast-out-{serial}")
                } else {
                    format!("toast-in-{serial}")
                },
                Animation::new(TOAST_MOTION).with_easing(ease_out_quint()),
                move |card, delta| {
                    let progress = if closing { 1.0 - delta } else { delta };
                    card.bottom(bottom - travel * (1.0 - progress))
                        .opacity(progress)
                },
            )
            .into_any_element()
    }
}
