// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The search window's view of the shared Cairn theme.
//!
//! Palette policy lives in `cairn-ui`, which the installer also uses. This module adds the two
//! things only the search window needs: the persisted [`AppSettings`] entry point and
//! [`ShellTokens`], the semantic role snapshot a shell view reads once per frame.

use gpui_kit::component::ActiveTheme;
use gpui_kit::gpui::{App, Hsla, Pixels, Window};

pub use cairn_ui::brand::{
    heading_text, serif_font, tabular_figures, BASE_FONT_PX, BRAND_TITLE_SIZE, HAIRLINE, RADIUS,
    SERIF_LINE_HEIGHT, SMALL_TEXT_SIZE, UI_ICON_SIZE,
};
pub use cairn_ui::locale::system_prefers_dark;
pub use cairn_ui::theme::clamp_zoom;

use crate::shell::settings::AppSettings;

/// Installs the appearance the user persisted. Call once during bootstrap, before the window opens,
/// and again whenever [`set_dark_mode`] or [`set_zoom`] changes a preference.
pub fn apply(settings: &AppSettings, cx: &mut App) {
    cairn_ui::theme::apply_appearance(
        settings.is_dark(system_prefers_dark()),
        clamp_zoom(settings.zoom_level),
        cx,
    );
}

/// Switches the light or dark palette and returns the value to persist.
pub fn set_dark_mode(dark: bool, cx: &mut App) -> bool {
    cairn_ui::theme::set_dark_mode(dark, cx)
}

/// Rescales the whole shell by moving the theme's base font, so type, spacing, control frames and
/// icons keep the relationships they had at 1.0. Returns the value to persist.
pub fn set_zoom(zoom: f32, cx: &mut App) -> f32 {
    cairn_ui::theme::set_zoom(zoom, cx)
}

/// The window's current `rem`, in pixels: the length every helper in [`crate::app::layout`] is a
/// multiple of.
pub fn rem(window: &Window) -> Pixels {
    cairn_ui::theme::rem(window)
}

/// The semantic roles the shell reads, taken from the live GPUI Component theme. Every field is a
/// role, never a palette position.
#[derive(Clone, Copy, Debug)]
pub struct ShellTokens {
    // ---- surfaces ---------------------------------------------------------------
    pub background: Hsla,
    pub muted: Hsla,
    pub sidebar: Hsla,
    pub sidebar_accent: Hsla,
    pub sidebar_accent_foreground: Hsla,
    pub status_bar: Hsla,
    pub table_head: Hsla,
    pub table_head_foreground: Hsla,
    pub input: Hsla,
    pub skeleton: Hsla,
    // ---- ink ----------------------------------------------------------------------
    pub foreground: Hsla,
    pub muted_foreground: Hsla,
    // ---- structure and interaction -------------------------------------------------
    pub border: Hsla,
    pub ring: Hsla,
    pub caret: Hsla,
    pub selection: Hsla,
    pub row_hover: Hsla,
    // ---- emphasis, spent sparingly -------------------------------------------------
    pub danger: Hsla,
    pub danger_foreground: Hsla,
    // ---- shape --------------------------------------------------------------------
    /// Every app-owned corner: search box, sidebar rows, skeleton bars, focus frames.
    pub radius: Pixels,
}

impl ShellTokens {
    /// Reads every role the shell uses out of the active theme. Call once per rendered frame.
    pub fn current(cx: &App) -> Self {
        let theme = cx.theme();
        Self {
            background: theme.background,
            muted: theme.muted,
            sidebar: theme.sidebar,
            sidebar_accent: theme.sidebar_accent,
            sidebar_accent_foreground: theme.sidebar_accent_foreground,
            status_bar: theme.status_bar,
            table_head: theme.table_head,
            table_head_foreground: theme.table_head_foreground,
            input: theme.input,
            skeleton: theme.skeleton,
            foreground: theme.foreground,
            muted_foreground: theme.muted_foreground,
            border: theme.border,
            ring: theme.ring,
            caret: theme.caret,
            selection: theme.selection,
            row_hover: theme.list_hover,
            danger: theme.danger,
            danger_foreground: theme.danger_foreground,
            radius: RADIUS,
        }
    }

    /// Row background for the static selected state. Hover and pressed are painted by the same
    /// `row_hover` fill, so the selected row keeps a single uniform light gray and relies on its
    /// foreground and `aria-selected` state to announce selection.
    pub fn row_background(&self, is_selected: bool) -> Hsla {
        if is_selected {
            self.row_hover
        } else {
            self.background
        }
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::{clamp_zoom, BASE_FONT_PX, HAIRLINE};
    use cairn_ui::theme::{MAX_ZOOM, MIN_ZOOM};
    use gpui_kit::gpui::px;

    /// A restored or supplied zoom must land inside the band the control frames were designed for.
    #[test]
    fn zoom_stays_inside_the_designed_band() {
        assert_eq!(clamp_zoom(0.1), MIN_ZOOM);
        assert_eq!(clamp_zoom(10.0), MAX_ZOOM);
        for zoom in [0.9, 1.0, 1.1] {
            assert_eq!(clamp_zoom(zoom), zoom, "{zoom} is inside the band");
        }
        assert_eq!(clamp_zoom(1.25), MAX_ZOOM);
        assert_eq!(clamp_zoom(0.75), MIN_ZOOM);
    }

    /// The one literal the shell keeps is a physical boundary, so it has to stay a single pixel
    /// whatever the interface zoom is.
    #[test]
    fn the_hairline_is_a_device_pixel_not_a_rem_step() {
        assert_eq!(HAIRLINE, px(1.0));
        assert_eq!(BASE_FONT_PX, 14.0, "the shell's base font anchors its rem");
    }
}
