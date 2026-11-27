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
use crate::motion::{TOAST, TOAST_ERROR_TIMEOUT, TOAST_INFO_TIMEOUT, toast_enter, toast_exit};
use crate::shell::accessibility;
use crate::shell::theme::{self, ShellTokens};
use fluent_bundle::FluentArgs;
use gpui_kit::assets::IconName as AssetIconName;
use gpui_kit::base::motion::Presence;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::gpui::*;
use gpui_kit::prelude::FluentBuilder as _;
use std::time::{Duration, Instant};

/// The toast is a reading surface, not a panel: one short measure, independent
/// of the result columns underneath it.
const TOAST_WIDTH_REM: f32 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToastKind {
    Error,
    Info,
}

impl ToastKind {
    fn timeout(self) -> Duration {
        match self {
            Self::Error => TOAST_ERROR_TIMEOUT,
            Self::Info => TOAST_INFO_TIMEOUT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToastId {
    Update,
    Message(u64),
}

impl ToastId {
    fn key(self) -> u64 {
        match self {
            Self::Update => 0,
            Self::Message(id) => id,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ToastContent {
    id: ToastId,
    kind: ToastKind,
    title: String,
    message: String,
    shown_at: Instant,
}

impl ToastContent {
    fn new(id: ToastId, kind: ToastKind, title: String, message: String) -> Self {
        Self {
            id,
            kind,
            title,
            message,
            shown_at: Instant::now(),
        }
    }
}

#[derive(Clone)]
pub(crate) enum ToastState {
    Hidden,
    Visible(ToastContent),
    Exiting(ToastId, ToastContent),
}

impl ToastState {
    pub(crate) fn content(&self) -> Option<&ToastContent> {
        match self {
            Self::Hidden => None,
            Self::Visible(content) | Self::Exiting(_, content) => Some(content),
        }
    }

    pub(crate) fn is_hidden(&self) -> bool {
        matches!(self, Self::Hidden)
    }

    fn is_current(&self, id: ToastId, shown_at: Instant) -> bool {
        self.content()
            .is_some_and(|content| content.id == id && content.shown_at == shown_at)
    }

    fn replace(&mut self, content: ToastContent) -> bool {
        let same_id = self
            .content()
            .is_some_and(|current| current.id == content.id);
        *self = Self::Visible(content);
        same_id
    }

    fn begin_exit(&mut self, id: ToastId, shown_at: Instant) -> bool {
        let Self::Visible(content) = self else {
            return false;
        };
        if content.id != id || content.shown_at != shown_at {
            return false;
        }
        *self = Self::Exiting(id, content.clone());
        true
    }

    fn finish_exit(&mut self, id: ToastId, shown_at: Instant) -> bool {
        let Self::Exiting(exiting_id, content) = self else {
            return false;
        };
        if *exiting_id != id || content.shown_at != shown_at {
            return false;
        }
        *self = Self::Hidden;
        true
    }
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
        self.upsert_toast(
            ToastId::Update,
            ToastKind::Info,
            title.into(),
            message.into(),
            false,
            cx,
        );
    }

    fn show_toast(
        &mut self,
        kind: ToastKind,
        title: String,
        message: String,
        auto_dismiss: bool,
        cx: &mut Context<'_, Self>,
    ) {
        self.next_toast_id = self.next_toast_id.wrapping_add(1);
        let id = ToastId::Message(self.next_toast_id);
        self.upsert_toast(id, kind, title, message, auto_dismiss, cx);
    }

    fn upsert_toast(
        &mut self,
        id: ToastId,
        kind: ToastKind,
        title: String,
        message: String,
        auto_dismiss: bool,
        cx: &mut Context<'_, Self>,
    ) {
        let content = ToastContent::new(id, kind, title, message);
        let shown_at = content.shown_at;
        self.toast.replace(content);

        if auto_dismiss {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(kind.timeout()).await;
                let _ = this.update(cx, |this, cx| {
                    if this.toast.is_current(id, shown_at) {
                        this.begin_toast_exit(id, shown_at, cx);
                    }
                });
            })
            .detach();
        }
        cx.notify();
    }

    pub(crate) fn dismiss_toast(&mut self, cx: &mut Context<'_, Self>) {
        let Some((id, shown_at)) = self
            .toast
            .content()
            .map(|content| (content.id, content.shown_at))
        else {
            return;
        };
        self.begin_toast_exit(id, shown_at, cx);
    }

    /// Removes the current toast only when it is the one `title` names. A
    /// corrected field must not leave its stale validation card behind, and
    /// an unrelated operation's error must stay readable.
    pub(crate) fn dismiss_toast_titled(&mut self, title: &str, cx: &mut Context<'_, Self>) {
        let matches = self
            .toast
            .content()
            .is_some_and(|toast| toast.title == title);
        if matches {
            self.dismiss_toast(cx);
        }
    }

    /// Keeps the card mounted for one short fade-out, then drops it. The id and
    /// content instance make every superseded timer a no-op.
    fn begin_toast_exit(&mut self, id: ToastId, shown_at: Instant, cx: &mut Context<'_, Self>) {
        if !self.toast.begin_exit(id, shown_at) {
            return;
        }
        if cx.reduce_motion() {
            self.toast.finish_exit(id, shown_at);
            cx.notify();
            return;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST).await;
            let _ = this.update(cx, |this, cx| {
                if this.toast.finish_exit(id, shown_at) {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(crate) fn render_toast(
        &self,
        toast: ToastContent,
        tokens: &ShellTokens,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let present = matches!(self.toast, ToastState::Visible(_));
        let toast_key = toast.id.key();
        let sample = Presence::new(
            ElementId::NamedInteger("cairn-toast-presence".into(), toast_key),
            present,
        )
        .transition(if present { toast_enter() } else { toast_exit() })
        .sample(window, cx);
        let is_error = toast.kind == ToastKind::Error;
        let control = if is_error {
            accessibility::control("toast")
        } else {
            accessibility::control("toast-info")
        };
        let dismiss = accessibility::control("toast-dismiss");
        let dismiss_label = accessibility::name(dismiss, &self.i18n);
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
            .opacity(sample.progress)
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
                            .id(("toast-message", toast_key))
                            .w_full()
                            .min_w_0()
                            .truncate()
                            .text_size(rems(theme::SMALL_TEXT_SIZE))
                            .line_height(relative(1.5))
                            .text_color(tokens.muted_foreground)
                            .when(!self.tooltip_suppression.is_suppressed(), |this| {
                                this.tooltip(cairn_ui::tooltip::text(toast.message.clone()))
                            })
                            .child(toast.message),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{ToastContent, ToastId, ToastKind, ToastState};
    use crate::app::CairnApp;
    use gpui_kit::AppContext as _;

    fn content(id: ToastId, title: &str) -> ToastContent {
        ToastContent::new(id, ToastKind::Info, title.to_string(), String::new())
    }

    #[test]
    fn replacing_a_visible_toast_with_the_same_id_keeps_its_identity() {
        let id = ToastId::Update;
        let mut state = ToastState::Hidden;
        state.replace(content(id, "Installing"));
        assert!(state.replace(content(id, "Installing 50%")));

        let current = state.content().expect("updated toast stays visible");
        assert_eq!(current.id, id);
        assert_eq!(current.title, "Installing 50%");
        assert!(matches!(state, ToastState::Visible(_)));
    }

    #[test]
    fn a_new_id_replaces_the_old_card_without_an_exiting_state() {
        let mut state = ToastState::Hidden;
        state.replace(content(ToastId::Message(1), "Old"));
        assert!(!state.replace(content(ToastId::Message(2), "New")));

        let current = state.content().expect("replacement is visible");
        assert_eq!(current.id, ToastId::Message(2));
        assert_eq!(current.title, "New");
        assert!(matches!(state, ToastState::Visible(_)));
    }

    #[test]
    fn exit_and_cleanup_reject_stale_timers() {
        let id = ToastId::Message(1);
        let first = content(id, "First");
        let first_shown_at = first.shown_at;
        let mut state = ToastState::Hidden;
        state.replace(first);
        assert!(state.begin_exit(id, first_shown_at));
        assert!(matches!(state, ToastState::Exiting(_, _)));

        assert!(!state.finish_exit(ToastId::Message(2), first_shown_at));
        let second = content(ToastId::Message(2), "Second");
        let second_shown_at = second.shown_at;
        state.replace(second);
        assert!(!state.finish_exit(id, first_shown_at));
        assert!(state.is_current(ToastId::Message(2), second_shown_at));

        assert!(state.begin_exit(ToastId::Message(2), second_shown_at));
        assert!(state.finish_exit(ToastId::Message(2), second_shown_at));
        assert!(state.is_hidden());
    }

    #[test]
    fn updating_the_same_id_invalidates_its_previous_auto_dismiss_timer() {
        let id = ToastId::Update;
        let first = content(id, "First");
        let first_shown_at = first.shown_at;
        let mut state = ToastState::Hidden;
        state.replace(first);
        state.begin_exit(id, first_shown_at);

        let second = content(id, "Second");
        let second_shown_at = second.shown_at;
        state.replace(second);
        assert!(!state.finish_exit(id, first_shown_at));
        assert!(state.is_current(id, second_shown_at));
    }

    #[test]
    fn reduced_motion_dismisses_a_toast_without_an_exit_timer() {
        gpui_kit::application().run(|cx: &mut gpui_kit::gpui::App| {
            gpui_kit::init(cx);
            let _ = gpui_kit::open_window(
                gpui_kit::gpui::WindowOptions {
                    show: false,
                    focus: false,
                    ..gpui_kit::gpui::WindowOptions::default()
                },
                cx,
                |window, cx| {
                    let app = cx.new(|cx| CairnApp::new(window, cx));
                    app.update(cx, |this, cx| {
                        cx.set_reduce_motion(true);
                        this.show_info_toast("Title", "Message", cx);
                        assert!(!this.toast.is_hidden());
                        this.dismiss_toast(cx);
                        assert!(this.toast.is_hidden());
                    });
                    cx.quit();
                    app
                },
            );
        });
    }

    #[test]
    fn toast_timeouts_share_the_same_transient_retention() {
        let timeout = ToastKind::Info.timeout();
        assert_eq!(ToastKind::Error.timeout(), timeout);
        assert!(!timeout.is_zero());
    }
}
