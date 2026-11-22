// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! App-owned transient status toast composed from `ShellTokens`, `dialog_surface`, `Button`,
//! and shell typography.
//!
//! Informational and error toasts auto-dismiss after a short timeout, while an active update
//! installation stays persistent. Each card presents a title, a close button, and a
//! single-line truncated description with a full-text tooltip.

use crate::app::{CairnApp, layout};
use crate::shell::accessibility;
use crate::shell::theme::{self, ShellTokens};
use fluent_bundle::FluentArgs;
use gpui_kit::assets::IconName as AssetIconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::gpui::*;
use gpui_kit::gpui::{Animation, AnimationExt, ease_out_quint};
use std::time::Duration;

const TOAST_INFO_DURATION: Duration = Duration::from_secs(6);
const TOAST_ERROR_DURATION: Duration = Duration::from_secs(8);
/// The toast is a reading surface, not a panel: one short measure, independent
/// of the result columns underneath it.
const TOAST_WIDTH_REM: f32 = 24.0;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToastKind {
    Error,
    Info,
}

#[derive(Clone)]
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
        cx: &mut Context<'_, Self>,
    ) {
        self.show_toast(ToastKind::Error, title.into(), message.into(), true, cx);
    }

    pub(crate) fn show_info_toast(
        &mut self,
        title: impl Into<String>,
        message: impl Into<String>,
        cx: &mut Context<'_, Self>,
    ) {
        self.show_toast(ToastKind::Info, title.into(), message.into(), true, cx);
    }

    pub(crate) fn show_persistent_info_toast(
        &mut self,
        title: impl Into<String>,
        message: impl Into<String>,
        cx: &mut Context<'_, Self>,
    ) {
        self.show_toast(ToastKind::Info, title.into(), message.into(), false, cx);
    }

    fn show_toast(
        &mut self,
        kind: ToastKind,
        title: String,
        message: String,
        auto_dismiss: bool,
        cx: &mut Context<'_, Self>,
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
        if auto_dismiss {
            let duration = if kind == ToastKind::Error {
                TOAST_ERROR_DURATION
            } else {
                TOAST_INFO_DURATION
            };
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(duration).await;
                let _ = this.update(cx, |this, cx| {
                    let still_current = this.toast_serial == serial
                        && this.toast.as_ref().is_some_and(|toast| !toast.closing);
                    if still_current {
                        this.begin_toast_exit(serial, cx);
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    pub(crate) fn dismiss_toast(&mut self, cx: &mut Context<'_, Self>) {
        let serial = self.toast_serial;
        self.begin_toast_exit(serial, cx);
    }

    /// Removes the current toast only when it is the one `title` names. A
    /// corrected field must not leave its stale validation card behind, and
    /// an unrelated operation's error must stay readable.
    pub(crate) fn dismiss_toast_titled(&mut self, title: &str, cx: &mut Context<'_, Self>) {
        let matches = self
            .toast
            .as_ref()
            .is_some_and(|toast| toast.title == title);
        if matches {
            self.dismiss_toast(cx);
        }
    }

    /// Keeps the card mounted for one short fade-out, then drops it. A newer toast bumps the
    /// serial, so a stale exit timer can never remove the replacement.
    fn begin_toast_exit(&mut self, serial: u64, cx: &mut Context<'_, Self>) {
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
            cx.background_executor()
                .timer(crate::shell::motion::TOAST)
                .await;
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
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let is_error = toast.kind == ToastKind::Error;
        let control = if is_error {
            accessibility::control("toast")
        } else {
            accessibility::control("toast-info")
        };
        let dismiss = accessibility::control("toast-dismiss");
        let dismiss_label = accessibility::name(dismiss, &self.i18n);
        let serial = self.toast_serial;
        let closing = toast.closing;
        let surface = cairn_ui::theme::dialog_surface(cx.theme());
        let close = Button::new(dismiss.element)
            .ghost()
            .small()
            .min_w(rems(layout::SPACE_24))
            .min_h(rems(layout::SPACE_24))
            .icon(AssetIconName::X)
            .accessibility_label(dismiss_label)
            .on_click(cx.listener(|this, _, _, cx| this.dismiss_toast(cx)));

        let bottom = rems(layout::STATUS_HEIGHT + layout::SPACE_8);
        let reduce_motion = cx.reduce_motion();
        let accessible_name = if toast.message.is_empty() {
            toast.title.clone()
        } else {
            let mut args = FluentArgs::new();
            args.set("title", toast.title.as_str());
            args.set("message", toast.message.as_str());
            self.i18n.get_text_with_args("toast_accessible_name", &args)
        };
        accessibility::element(control)
            .aria_label(accessible_name)
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
            .bg(surface)
            .border_1()
            .border_color(tokens.border)
            .shadow_md()
            .child(
                v_flex()
                    .w_full()
                    .gap(rems(layout::SPACE_4))
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap(rems(layout::INNER_SPACING))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(tokens.foreground)
                                    .child(toast.title),
                            )
                            .child(
                                close
                                    // An icon-only 24px target keeps a 16px glyph
                                    // inset by 4px on each side; pull the target
                                    // toward the card edge so the glyph sits on the
                                    // same 12px padding spine.
                                    .mr(rems(-layout::SPACE_4)),
                            ),
                    )
                    .child(
                        div()
                            .id(("toast-message", serial))
                            .w_full()
                            .min_w_0()
                            .truncate()
                            .text_size(rems(theme::SMALL_TEXT_SIZE))
                            .line_height(relative(1.5))
                            .text_color(tokens.muted_foreground)
                            .tooltip(crate::shell::tooltip::text(toast.message.clone()))
                            .child(toast.message),
                    ),
            )
            .with_animation(
                if closing {
                    format!("toast-out-{serial}")
                } else {
                    format!("toast-in-{serial}")
                },
                Animation::new(crate::shell::motion::TOAST).with_easing(ease_out_quint()),
                move |card, delta| {
                    let progress = if reduce_motion {
                        1.0
                    } else if closing {
                        1.0 - delta
                    } else {
                        delta
                    };
                    card.opacity(progress)
                },
            )
            .into_any_element()
    }
}
