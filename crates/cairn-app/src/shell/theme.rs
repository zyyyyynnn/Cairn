// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The search window's view of the shared Cairn theme.
//!
//! Palette policy lives in `cairn-ui`, which the installer also uses. This module adds
//! [`ShellTokens`], the semantic role snapshot a shell view reads once per frame.

use gpui_kit::component::{ActiveTheme, Theme};
use gpui_kit::gpui::{App, Global, Hsla, Pixels, Window};

pub use cairn_ui::brand::{
    serif_font, tabular_figures, BASE_FONT_PX, BRAND_TITLE_SIZE, HAIRLINE, RADIUS,
    SERIF_LINE_HEIGHT, SMALL_TEXT_SIZE, UI_ICON_SIZE,
};
pub use cairn_ui::locale::system_prefers_dark;
pub use cairn_ui::theme::{clamp_zoom, control_stroke, warning_ink};

/// Installs the resolved appearance before the window opens. Call once during bootstrap and update
/// via [`set_dark_mode`] or [`set_zoom`] when preferences change.
pub fn apply_resolved(is_dark: bool, zoom_level: f32, cx: &mut App) {
    cairn_ui::theme::apply_appearance(is_dark, clamp_zoom(zoom_level), cx);
    refresh_shell_tokens(cx);
}

/// Switches the light or dark palette and returns the value to persist.
pub fn set_dark_mode(dark: bool, cx: &mut App) -> bool {
    let resolved = cairn_ui::theme::set_dark_mode(dark, cx);
    refresh_shell_tokens(cx);
    resolved
}

/// Rescales the whole shell by moving the theme's base font, so type, spacing, control frames and
/// icons keep the relationships they had at 1.0. Returns the value to persist.
pub fn set_zoom(zoom: f32, cx: &mut App) -> f32 {
    let resolved = cairn_ui::theme::set_zoom(zoom, cx);
    refresh_shell_tokens(cx);
    resolved
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
    pub chrome: Hsla,
    pub sidebar_accent: Hsla,
    pub sidebar_accent_foreground: Hsla,
    pub status_bar: Hsla,
    pub table_head: Hsla,
    pub table_head_foreground: Hsla,
    /// Editable control outlines, held at 3:1 against the page and muted surfaces.
    pub control_stroke: Hsla,

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
    pub warning_ink: Hsla,
    pub danger: Hsla,
    pub danger_foreground: Hsla,
    // ---- shape --------------------------------------------------------------------
    /// Every app-owned corner: search box, sidebar rows, focus frames.
    pub radius: Pixels,
}

impl ShellTokens {
    /// Reads every role the shell uses out of the cached global when the theme key matches, or
    /// computes once as a fallback when no global cache has been primed yet.
    pub fn current(cx: &App) -> Self {
        let key = ThemeKey::from_theme(cx.theme());
        if let Some(cached) = cx.try_global::<CachedShellTokens>() {
            if cached.key == key {
                return cached.tokens;
            }
        }
        Self::compute(cx)
    }

    fn compute(cx: &App) -> Self {
        let theme = cx.theme();
        Self {
            background: theme.background,
            muted: theme.muted,
            sidebar: theme.sidebar,
            chrome: theme.title_bar,
            sidebar_accent: theme.sidebar_accent,
            sidebar_accent_foreground: theme.sidebar_accent_foreground,
            status_bar: theme.status_bar,
            table_head: theme.table_head,
            table_head_foreground: theme.table_head_foreground,
            control_stroke: control_stroke(theme),

            foreground: theme.foreground,
            muted_foreground: theme.muted_foreground,
            border: theme.border,
            ring: theme.ring,
            caret: theme.caret,
            selection: theme.selection,
            row_hover: theme.list_hover,
            warning_ink: warning_ink(theme),
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

#[derive(Clone, Copy, Debug, PartialEq)]
struct ThemeKey {
    is_dark: bool,
    background: Hsla,
    muted: Hsla,
    warning: Hsla,
    border: Hsla,
}

impl ThemeKey {
    fn from_theme(theme: &Theme) -> Self {
        Self {
            is_dark: theme.mode.is_dark(),
            background: theme.background,
            muted: theme.muted,
            warning: theme.warning,
            border: theme.border,
        }
    }
}

#[derive(Clone, Copy)]
struct CachedShellTokens {
    key: ThemeKey,
    tokens: ShellTokens,
}

impl Global for CachedShellTokens {}

fn resolve_cached_tokens<K: PartialEq + Copy, V: Copy>(
    cached: &mut Option<(K, V)>,
    key: K,
    compute: impl FnOnce() -> V,
) -> V {
    if let Some((cached_key, cached_value)) = *cached {
        if cached_key == key {
            return cached_value;
        }
    }
    let value = compute();
    *cached = Some((key, value));
    value
}

/// Refreshes the cached frame tokens after any appearance/palette change so `render` never runs
/// `contrast_ratio` searches per frame.
pub fn refresh_shell_tokens(cx: &mut App) {
    let key = ThemeKey::from_theme(cx.theme());
    let mut slot = cx
        .try_global::<CachedShellTokens>()
        .map(|cached| (cached.key, cached.tokens));
    let tokens = resolve_cached_tokens(&mut slot, key, || ShellTokens::compute(cx));
    cx.set_global(CachedShellTokens { key, tokens });
}

/// Primes the cached frame tokens once if no global snapshot exists yet.
pub fn ensure_shell_tokens(cx: &mut App) {
    let key = ThemeKey::from_theme(cx.theme());
    let already_valid = cx
        .try_global::<CachedShellTokens>()
        .is_some_and(|cached| cached.key == key);
    if !already_valid {
        let tokens = ShellTokens::compute(cx);
        cx.set_global(CachedShellTokens { key, tokens });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_tokens_recompute_only_when_theme_key_changes() {
        let mut slot: Option<(u8, u32)> = None;
        let mut computes = 0usize;

        let first = resolve_cached_tokens(&mut slot, 1, || {
            computes += 1;
            100
        });
        let second = resolve_cached_tokens(&mut slot, 1, || {
            computes += 1;
            200
        });
        assert_eq!(first, 100);
        assert_eq!(second, 100);
        assert_eq!(computes, 1);

        let third = resolve_cached_tokens(&mut slot, 2, || {
            computes += 1;
            300
        });
        assert_eq!(third, 300);
        assert_eq!(computes, 2);
    }
}
