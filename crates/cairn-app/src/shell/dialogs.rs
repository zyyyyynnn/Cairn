// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Shared surface for every modal alert: one fixed size, one scrollable body.

use crate::app::layout;
use crate::shell::accessibility;
use cairn_ui::brand::HAIRLINE;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dialog::{
    AlertDialog, Cancel, Confirm, DialogDescription, DialogEntrance, DialogFooter,
};
use gpui_kit::component::v_flex;
use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::gpui::*;

/// Seats the fixed surface in the vertical middle of the window, below the caption bar.
pub(crate) fn top_offset(window: &Window) -> Pixels {
    top_offset_for_height(
        window.viewport_size().height,
        window.rem_size() * layout::DIALOG_SEAT_HEIGHT,
        window.rem_size() * layout::SPACE_24,
    )
}

pub(crate) fn top_offset_for_height(
    viewport_height: Pixels,
    dialog_height: Pixels,
    minimum: Pixels,
) -> Pixels {
    ((viewport_height - dialog_height) / 2.).max(minimum)
}

/// The two footer actions of an alert, with their labels and treatments. Both keep the shared
/// medium frame; the lighter neutral plate is painted one device pixel inside it so the two
/// actions read as the same optical size.
pub(crate) struct DialogActions {
    ok: SharedString,
    ok_variant: ButtonVariant,
    cancel: Option<SharedString>,
}

impl DialogActions {
    pub(crate) fn new(ok: impl Into<SharedString>, ok_variant: ButtonVariant) -> Self {
        Self {
            ok: ok.into(),
            ok_variant,
            cancel: None,
        }
    }

    pub(crate) fn with_cancel(mut self, cancel: impl Into<SharedString>) -> Self {
        self.cancel = Some(cancel.into());
        self
    }
}

/// The one frame every alert shares: fixed width, seated below the caption bar, with the common
/// entrance. The caller supplies the action labels and treatments.
pub(crate) fn alert_surface(
    alert: AlertDialog,
    window: &mut Window,
    cx: &mut App,
    surface: Hsla,
    actions: DialogActions,
) -> AlertDialog {
    let footer = dialog_footer(window, cx, surface, actions);
    alert
        .width(window.rem_size() * layout::DIALOG_WIDTH)
        .margin_top(top_offset(window))
        .bg(surface)
        .entrance(DialogEntrance::FadeSlide)
        .footer(footer)
}

/// The footer order matches the stock alert: cancel on the left, commit on the right. A pair has
/// an optical inset only when both actions are present, so a lone commit stays at full size.
fn dialog_footer(
    window: &mut Window,
    cx: &mut App,
    surface: Hsla,
    actions: DialogActions,
) -> impl IntoElement {
    let inset = {
        let theme = cx.theme();
        let cancel_fill = theme.tokens.button_secondary.color;
        if actions.cancel.is_some() {
            inset_target(action_fill(theme, actions.ok_variant), cancel_fill)
        } else {
            None
        }
    };

    let mut footer = DialogFooter::new();
    if let Some(cancel) = actions.cancel {
        let button = action_button(
            "cairn-dialog-cancel",
            cancel,
            ButtonVariant::Secondary,
            false,
            window,
            cx,
        );
        let inset = matches!(inset, Some(InsetTarget::Cancel));
        footer = footer.child(inset_action(button, inset, surface, cx));
    }
    let button = action_button(
        "cairn-dialog-ok",
        actions.ok,
        actions.ok_variant,
        true,
        window,
        cx,
    );
    let inset = matches!(inset, Some(InsetTarget::Ok));
    footer.child(inset_action(button, inset, surface, cx))
}

/// A standard medium button that dispatches the dialog's own Confirm/Cancel action from a
/// rendered anchor, so the alert's callbacks and key bindings stay in charge.
fn action_button(
    id: &'static str,
    label: SharedString,
    variant: ButtonVariant,
    confirm: bool,
    window: &mut Window,
    cx: &mut App,
) -> Button {
    let anchor = window
        .use_keyed_state(id, cx, |_, cx| cx.focus_handle())
        .read(cx)
        .clone();
    let dispatch = anchor.clone();
    Button::new(id)
        .label(label)
        .with_variant(variant)
        .child(div().absolute().size_0().track_focus(&anchor))
        .on_click(move |_, window, cx| {
            if confirm {
                dispatch.dispatch_action(&Confirm { secondary: false }, window, cx);
            } else {
                dispatch.dispatch_action(&Cancel, window, cx);
            }
        })
}

/// Which action of a pair carries the optical inset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InsetTarget {
    Ok,
    Cancel,
}

/// The fill an action variant paints, when it is one of the footer's filled roles.
fn action_fill(theme: &Theme, variant: ButtonVariant) -> Option<Hsla> {
    match variant {
        ButtonVariant::Primary => Some(theme.tokens.button_primary.color),
        ButtonVariant::Danger => Some(theme.tokens.button_danger.color),
        _ => None,
    }
}

/// A light plate reads larger than a dark plate of the same size. The lighter neutral action is
/// therefore painted one device pixel inside its frame; a chromatic action keeps its full plate,
/// because its fill is not the flat bright surface that blooms. The comparison uses the fills'
/// own lightness, so each theme picks the pair's lighter neutral action independently.
fn inset_target(ok_fill: Option<Hsla>, cancel_fill: Hsla) -> Option<InsetTarget> {
    let ok = ok_fill?;
    let ok_neutral = ok.s <= f32::EPSILON;
    let cancel_neutral = cancel_fill.s <= f32::EPSILON;
    if ok_neutral && ok.l > cancel_fill.l {
        Some(InsetTarget::Ok)
    } else if cancel_neutral && cancel_fill.l > ok.l {
        Some(InsetTarget::Cancel)
    } else {
        None
    }
}

/// Masks the outer device pixel of the lighter action with the dialog's own surface, so its
/// painted plate shrinks by one pixel per side while the frame, hit area and hover fill stay
/// unchanged. No stroke is drawn: the mask color is the surface behind the footer.
fn inset_action(button: Button, inset: bool, surface: Hsla, cx: &App) -> AnyElement {
    let button = button.into_any_element();
    if !inset {
        return button;
    }
    let radius = cx.theme().radius;
    div()
        .relative()
        .flex_none()
        .child(button)
        .child(
            div()
                .absolute()
                .top(px(0.))
                .left(px(0.))
                .right(px(0.))
                .bottom(px(0.))
                .rounded(radius)
                .border(HAIRLINE)
                .border_color(surface),
        )
        .into_any_element()
}

/// One title recipe: the window's UI face at heading weight, shaped as one line.
pub(crate) fn title(title: String, window: &Window) -> AnyElement {
    let color = window.text_style().color;
    accessibility::heading(div().id("dialog-title"), title.clone())
        .text_base()
        .font_weight(FontWeight::SEMIBOLD)
        .line_height(relative(1.25))
        .text_color(color)
        .child(title)
        .into_any_element()
}

/// The title and explanation share one body. The dialog wraps this body in its scroll area, so a
/// long filename or a two-line explanation never clips inside the fixed surface.
pub(crate) fn body(title: String, description: String, window: &Window) -> AnyElement {
    v_flex()
        .w_full()
        .gap(rems(layout::SPACE_4))
        .child(self::title(title, window))
        .child(DialogDescription::new().child(description))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{inset_target, top_offset_for_height, InsetTarget};
    use crate::app::layout;
    use crate::shell::theme;
    use gpui_kit::gpui::{px, Hsla};

    fn gray(l: f32) -> Hsla {
        Hsla {
            h: 0.,
            s: 0.,
            l,
            a: 1.,
        }
    }

    fn red(l: f32) -> Hsla {
        Hsla {
            h: 0.,
            s: 0.8,
            l,
            a: 1.,
        }
    }

    #[test]
    fn dialogs_share_a_compact_seat() {
        assert!((layout::DIALOG_WIDTH * theme::BASE_FONT_PX - 384.0).abs() < 0.001);
        assert!((layout::DIALOG_SEAT_HEIGHT * theme::BASE_FONT_PX - 128.0).abs() < 0.001);
        assert_eq!(top_offset_for_height(px(800.), px(128.), px(24.)), px(336.));
        assert_eq!(top_offset_for_height(px(350.), px(128.), px(24.)), px(111.));
        assert_eq!(top_offset_for_height(px(100.), px(128.), px(24.)), px(24.));
    }

    #[test]
    fn the_lighter_neutral_action_takes_the_optical_inset() {
        assert_eq!(
            inset_target(Some(gray(0.98)), gray(0.227)),
            Some(InsetTarget::Ok)
        );
        assert_eq!(
            inset_target(Some(gray(0.09)), gray(0.898)),
            Some(InsetTarget::Cancel)
        );
        assert_eq!(inset_target(Some(red(0.6)), gray(0.227)), None);
        assert_eq!(
            inset_target(Some(red(0.6)), gray(0.898)),
            Some(InsetTarget::Cancel)
        );
        assert_eq!(inset_target(Some(gray(0.5)), gray(0.5)), None);
        assert_eq!(inset_target(None, gray(0.5)), None);
    }
}
