// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Cairn product theme layer.
//!
//! Two jobs, and nothing else:
//!
//! 1. Push the persisted appearance (light or dark, interface zoom) into the GPUI Component theme,
//!    which owns every color in the window. This module constructs no color of its own: the
//!    product's palette is the component palette, so a theme file or a custom theme reaches Cairn
//!    without a code change.
//! 2. Resolve the roles the shell reads into one [`ShellTokens`] snapshot per frame, plus the
//!    product-specific meanings that have no component role name.
//!
//! Why a snapshot instead of `cx.theme()` at each call site: `cx.theme()` borrows the context, and a
//! view that then registers a `cx.listener(...)` closure needs that context free. Reading the roles
//! once per frame keeps every color a semantic role from the live theme, costs one copy, and makes it
//! impossible for a call site to invent a value instead of asking for a role.
//!
//! Interface zoom is the theme's base font, which `Root` publishes as the window's `rem`. Every
//! layout helper in the view therefore scales with it; see [`crate::app::layout`], which holds the
//! rem multiples plus the platform title-bar geometry that must not scale with zoom.

use gpui_kit::component::{ActiveTheme, Colorize as _, Theme, ThemeMode};
use gpui_kit::gpui::{hsla, px, App, FontFeatures, Hsla, Pixels, Window};

use crate::shell::settings::AppSettings;

/// Lowest and highest interface zoom the shell accepts. The three named sizes sit on these edges
/// and the middle, and the zoom commands clamp instead of drifting out of the band.
pub const MIN_ZOOM: f32 = 0.9;
pub const MAX_ZOOM: f32 = 1.1;

/// One device pixel. An audited physical exception, not a layout distance: a hairline must stay
/// hair-thin at every zoom, and multiplying it by `rem` makes quiet separators look heavy.
pub const HAIRLINE: Pixels = px(1.0);

/// The one corner radius every component frame uses. Keeping a single 3px value removes the
/// small/medium/large ladder that had no distinct roles in this shell.
pub const RADIUS: Pixels = px(3.0);

/// The base font of the shell at zoom 1.0, in pixels. It anchors every `rem` helper in the window,
/// which is why it is a pixel value; see the Coding Guides, *Base font is the application zoom
/// control*. It stays at the compact desktop size: the 4px layout ladder is expressed as
/// `4px / BASE_FONT_PX` rem in [`crate::app::layout`], so spacing stays on a whole 4px at 100%
/// without inflating the type scale the way a 16px rem base would.
pub const BASE_FONT_PX: f32 = 14.0;

/// The family the brand wordmark in the title bar is drawn with. This is the theme layer's only say
/// over the window's typeface: the name is handed to the platform text system, which resolves it
/// against the fonts this machine has installed and falls back to its own default when the family is
/// absent. Nothing here substitutes a face or a fallback list, so the wordmark is the one place the
/// product asks for a named typeface and it is asked for exactly once.
pub const WORDMARK_FONT_FAMILY: &str = "Michelangelus";

/// Fixed-width figures, for every value whose digits change while the window is open.
///
/// Proportional digits make a right-aligned column ragged down the rows and make a counter shift
/// horizontally as it updates, which is exactly what the size, date, item-count and catch-up
/// percentage all do. `tnum` is a glyph substitution, so it changes no layout and is safe on a run
/// that also carries highlight colors.
pub fn tabular_figures() -> FontFeatures {
    static FEATURES: std::sync::OnceLock<FontFeatures> = std::sync::OnceLock::new();
    FEATURES
        .get_or_init(|| FontFeatures(std::sync::Arc::new(vec![("tnum".to_string(), 1)])))
        .clone()
}

/// Clamp a persisted or requested zoom into [`MIN_ZOOM`]..=[`MAX_ZOOM`].
pub fn clamp_zoom(zoom: f32) -> f32 {
    zoom.clamp(MIN_ZOOM, MAX_ZOOM)
}

/// The contrast floor the shell requires of an ink against every surface it carries text on: WCAG
/// AA for text below the large-text threshold, which is where every label in this window sits.
const INK_CONTRAST_FLOOR: f32 = 4.5;

/// The floor for a non-text indicator — the keyboard focus ring. Lower than the ink floor because
/// nothing here has to be read, only located, but high enough that the ring survives a glance.
const RING_CONTRAST_FLOOR: f32 = 3.0;

/// WCAG relative luminance of a rendered color.
fn relative_luminance(color: Hsla) -> f32 {
    fn channel(v: f32) -> f32 {
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    // `Hsla` carries h/s/l only, so the sRGB channels have to come from the conversion.
    let rgb = color.to_rgb();
    0.2126 * channel(rgb.r) + 0.7152 * channel(rgb.g) + 0.0722 * channel(rgb.b)
}

/// The contrast ratio between two rendered colors, as a value >= 1.
pub fn contrast_ratio(foreground: Hsla, background: Hsla) -> f32 {
    let a = relative_luminance(foreground);
    let b = relative_luminance(background);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Moves `ink` along its own lightness axis until it clears `floor` against every surface in
/// `surfaces`, and returns it unchanged when it already does.
///
/// The search stays on the lightness channel because contrast responds to lightness and not to hue,
/// so a repaired ink keeps the palette's color. Each step walks away from whichever surface is
/// currently binding, which is what makes the result independent of whether the theme is light or
/// dark: an ink on light surfaces is pushed darker, the same role on dark surfaces is pushed lighter.
fn enforce_contrast_floor(ink: Hsla, surfaces: &[Hsla], floor: f32) -> Hsla {
    let worst = |candidate: Hsla| -> f32 {
        surfaces
            .iter()
            .map(|surface| contrast_ratio(candidate, *surface))
            .fold(f32::INFINITY, f32::min)
    };
    if surfaces.is_empty() || worst(ink) >= floor {
        return ink;
    }
    let mut best = ink;
    for _ in 0..128 {
        // The surface with the lowest contrast is the one constraining the ink; move away from it.
        let binding = surfaces
            .iter()
            .min_by(|a, b| {
                contrast_ratio(best, **a)
                    .partial_cmp(&contrast_ratio(best, **b))
                    .unwrap_or(core::cmp::Ordering::Equal)
            })
            .copied()
            .expect("surfaces is non-empty");
        let step = if relative_luminance(best) < relative_luminance(binding) {
            -0.01
        } else {
            0.01
        };
        let mut candidate = best;
        candidate.l = (best.l + step).clamp(0.0, 1.0);
        if candidate.l == best.l {
            break;
        }
        best = candidate;
        if worst(best) >= floor {
            return best;
        }
    }
    best
}

/// Raises the ink roles that the component palette renders below [`INK_CONTRAST_FLOOR`] on the
/// surfaces this shell actually draws them on.
///
/// Applied to the live [`Theme`] rather than to a [`ShellTokens`] field so the module keeps its
/// contract of minting no color of its own: the palette stays the component palette, a theme file
/// whose pairs already pass is left exactly as it is, and the correction is a floor on the palette
/// instead of a parallel set of values the view could drift from.
fn repair_ink_contrast(theme: &mut Theme) {
    // Enforce pure grayscale tokens (#E6E6E6 ink, #101010, #cccccc) and eliminate non-grayscale tints.
    // The dark ink stays below pure white: on the near-black surface, #F9F9F9 made titles, selected
    // rows and Treemap labels glare. #E6E6E6 still clears the AA text floor with ample headroom.
    if theme.mode == ThemeMode::Dark {
        theme.background = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.0627,
            a: 1.0,
        };
        theme.foreground = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.90,
            a: 1.0,
        };
        theme.selection = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.22,
            a: 1.0,
        };
        theme.list_hover = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.14,
            a: 1.0,
        };
        theme.sidebar_foreground = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.90,
            a: 1.0,
        };
        theme.sidebar_accent_foreground = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.90,
            a: 1.0,
        };
        theme.popover_foreground = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.90,
            a: 1.0,
        };
        theme.skeleton = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.20,
            a: 1.0,
        };
    } else {
        theme.background = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.9765,
            a: 1.0,
        };
        theme.foreground = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.0627,
            a: 1.0,
        };
        theme.selection = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.85,
            a: 1.0,
        };
        theme.list_hover = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.90,
            a: 1.0,
        };
        theme.sidebar_accent_foreground = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.06,
            a: 1.0,
        };
        theme.skeleton = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.80,
            a: 1.0,
        };
    }

    // The recessed fills `muted_foreground` carries text on, gathered here because the view is not
    // allowed to name a color and so cannot state the pair itself.
    let secondary_surfaces = [
        theme.background,
        theme.muted,
        theme.input,
        theme.status_bar,
        theme.table_head,
        theme.sidebar,
    ];
    theme.muted_foreground = enforce_contrast_floor(
        theme.muted_foreground,
        &secondary_surfaces,
        INK_CONTRAST_FLOOR,
    );
    theme.table_head_foreground = enforce_contrast_floor(
        theme.table_head_foreground,
        &[theme.table_head],
        INK_CONTRAST_FLOOR,
    );
    theme.sidebar_foreground = enforce_contrast_floor(
        theme.sidebar_foreground,
        &[theme.sidebar],
        INK_CONTRAST_FLOOR,
    );
    // The focus ring is not ink, but it is the only thing that says where the keyboard is, and the
    // component palette's own value does not clear the non-text floor on the surfaces this shell
    // draws it across: measured 2.52:1 against `background` in the light palette from a real window
    // capture, where the Design Guides ask for a high-contrast ring.
    theme.ring = enforce_contrast_floor(
        theme.ring,
        &[theme.background, theme.table_head],
        RING_CONTRAST_FLOOR,
    );
}

/// Installs the appearance the user persisted. Call once during bootstrap, before the window opens,
/// and again whenever [`set_dark_mode`] or [`set_zoom`] changes a preference.
///
/// Two edits, deliberately. Changing `Theme::mode` makes the component re-resolve every role color
/// from that mode's theme file once the edit returns, which discards any color written inside the
/// same edit; the ink floor therefore has to run in a second edit, against the palette the mode
/// switch just installed.
pub fn apply(settings: &AppSettings, cx: &mut App) {
    let dark = settings.is_dark(system_prefers_dark());
    set_mode(dark, cx);
    settle_overlay(dark, cx);
    Theme::update(cx, |theme| {
        theme.font_size = px(BASE_FONT_PX * clamp_zoom(settings.zoom_level));
        // Every frame in the shell is a 1px hairline. The framework's ring is a 3px outside band;
        // turning it off selects its built-in 1px focus line instead.
        theme.focus_ring = false;
        theme.radius = RADIUS;
        theme.radius_lg = RADIUS;
    });
    raise_failing_ink(cx);
    strengthen_ghost_hover(cx);
}

/// The component palette's accent is only a few levels off the bar surface, which makes a ghost
/// button's hover nearly invisible. Borrow the list row's hover step instead: still quiet, but
/// clearly a target, and consistent with the table's pointer feedback.
fn strengthen_ghost_hover(cx: &mut App) {
    Theme::update(cx, apply_unified_interaction_fill);
}

/// One neutral interaction fill for hover and pressed everywhere. Components that used
/// `selection` or `secondary_active` as a pressed step were visibly darker than the hover surface;
/// keeping both at `list_hover` makes every neutral control answer the pointer with the same light
/// gray. Semantic fills (primary, danger, warning) and the persistent selected row stay distinct.
fn apply_unified_interaction_fill(theme: &mut Theme) {
    let hover = theme.list_hover;
    theme.list_active = hover;
    theme.tokens.accent = hover.into();
    theme.tokens.button_hover = hover.into();
    theme.tokens.button_active = hover.into();
    // Sidebar rows paint their selected and hover fills from this role; both are the same light
    // gray, with weight and foreground carrying the selected state.
    theme.sidebar_accent = hover;
    // The secondary fill (used by the dialog Cancel button and other secondary buttons)
    // must have a distinct optical step on hover and pressed. Setting hover to list_hover
    // caused near-zero contrast against the button's own resting fill. Tint toward the ink
    // with mix_oklab so light mode darkens and dark mode lightens with clear target contrast.
    let secondary = theme.tokens.button_secondary.color;
    let secondary_ink = theme.tokens.button_secondary_foreground.color;
    let sec_hover = secondary_ink.mix_oklab(secondary, 0.15);
    let sec_active = secondary_ink.mix_oklab(secondary, 0.22);
    theme.tokens.button_secondary_hover = sec_hover.into();
    theme.tokens.button_secondary_active = sec_active.into();
    theme.tokens.secondary_hover = sec_hover.into();
    theme.tokens.secondary_active = sec_active.into();
    theme.secondary_active = sec_active;
    // The palette's primary hover is only one neutral step from rest (and nearly invisible on the
    // dark theme's near-white fill). Tint it toward the label ink instead: light mode lightens,
    // dark mode darkens, and the label keeps its contrast in both. `mix_oklab` weights its first
    // color by the factor, so the ink leads and the primary fill remains the majority.
    let primary = theme.tokens.button_primary.color;
    let primary_ink = theme.tokens.button_primary_foreground.color;
    theme.tokens.button_primary_hover = primary_ink.mix_oklab(primary, 0.12).into();
    theme.tokens.button_primary_active = primary_ink.mix_oklab(primary, 0.20).into();
    // The destructive commit uses the same derived step so a danger action answers the pointer
    // with the same clarity as any other filled button.
    let danger = theme.tokens.button_danger.color;
    let danger_ink = theme.tokens.button_danger_foreground.color;
    theme.tokens.button_danger_hover = danger_ink.mix_oklab(danger, 0.12).into();
    theme.tokens.button_danger_active = danger_ink.mix_oklab(danger, 0.20).into();
}

/// Whether Windows is currently using its dark app appearance.
///
/// `AppsUseLightTheme` is the per-user switch the Settings app writes; a missing or unreadable value
/// falls back to the light palette rather than guessing. `system` in the settings is resolved here
/// at startup and whenever the settings dialog opens.
pub fn system_prefers_dark() -> bool {
    #[cfg(windows)]
    {
        use windows::core::w;
        use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

        let mut light_theme = 1u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: the key and value names are static null-terminated strings, the destination is a
        // live u32 and its size is passed with the pointer.
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
                w!("AppsUseLightTheme"),
                RRF_RT_REG_DWORD,
                None,
                Some(&mut light_theme as *mut u32 as *mut core::ffi::c_void),
                Some(&mut size),
            )
        };
        status.is_ok() && light_theme == 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Switches the light or dark palette and returns the value to persist.
pub fn set_dark_mode(dark: bool, cx: &mut App) -> bool {
    set_mode(dark, cx);
    settle_overlay(dark, cx);
    raise_failing_ink(cx);
    strengthen_ghost_hover(cx);
    dark
}

/// The component palette's light overlay is 5% black; on this window's white table that reads as a
/// detached card rather than a modal. Deepen the scrim in both modes without hiding the acted-on row.
fn settle_overlay(dark: bool, cx: &mut App) {
    Theme::update(cx, move |theme| {
        theme.overlay = hsla(0., 0., 0., if dark { 0.38 } else { 0.18 });
    });
}

/// Points the component theme at one palette and settles its base font.
fn set_mode(dark: bool, cx: &mut App) {
    Theme::update(cx, |theme| {
        theme.mode = if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        };
        theme.focus_ring = false;
        theme.radius = RADIUS;
        theme.radius_lg = RADIUS;
    });
}

/// Applies [`repair_ink_contrast`] to the live theme once its palette has settled.
fn raise_failing_ink(cx: &mut App) {
    Theme::update(cx, repair_ink_contrast);
}

/// Rescales the whole shell by moving the theme's base font, so type, spacing, control frames and
/// icons keep the relationships they had at 1.0. Returns the value to persist.
pub fn set_zoom(zoom: f32, cx: &mut App) -> f32 {
    let zoom = clamp_zoom(zoom);
    Theme::update(cx, |theme| theme.font_size = px(BASE_FONT_PX * zoom));
    zoom
}

/// The window's current `rem`, in pixels: the length every helper in [`crate::app::layout`] is a
/// multiple of. Virtualized row math needs a pixel value, so the view reads it here instead of
/// writing a fixed row height.
pub fn rem(window: &Window) -> Pixels {
    window.rem_size()
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

    use super::{
        apply_unified_interaction_fill, clamp_zoom, contrast_ratio, enforce_contrast_floor,
        relative_luminance, repair_ink_contrast, BASE_FONT_PX, HAIRLINE, INK_CONTRAST_FLOOR,
        MAX_ZOOM, MIN_ZOOM,
    };
    use gpui_kit::component::{Theme, ThemeMode};
    use gpui_kit::gpui::{px, Hsla};

    /// A restored or supplied zoom must land inside the band the control frames were designed for.
    #[test]
    fn zoom_stays_inside_the_designed_band() {
        assert_eq!(clamp_zoom(0.1), MIN_ZOOM);
        assert_eq!(clamp_zoom(10.0), MAX_ZOOM);
        for zoom in [0.9, 1.0, 1.1] {
            assert_eq!(clamp_zoom(zoom), zoom, "{zoom} is inside the band");
        }
        assert_eq!(
            clamp_zoom(1.25),
            MAX_ZOOM,
            "the three named sizes are the outside limit"
        );
        assert_eq!(
            clamp_zoom(0.75),
            MIN_ZOOM,
            "a smaller saved zoom lands on the compact size, not below it"
        );
    }

    /// The one literal the shell keeps is a physical boundary, so it has to stay a single pixel
    /// whatever the interface zoom is.
    #[test]
    fn the_hairline_is_a_device_pixel_not_a_rem_step() {
        assert_eq!(HAIRLINE, px(1.0));
        assert_eq!(BASE_FONT_PX, 14.0, "the shell's base font anchors its rem");
    }

    /// The contrast helper has to agree with the published WCAG answer, or every floor computed from
    /// it is meaningless. Black on white is defined as 21:1.
    #[test]
    fn contrast_matches_the_known_reference_pair() {
        let black = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.0,
            a: 1.0,
        };
        let white = Hsla {
            h: 0.0,
            s: 0.0,
            l: 1.0,
            a: 1.0,
        };
        assert!(
            (contrast_ratio(black, white) - 21.0).abs() < 0.01,
            "black on white must be 21:1, measured {}",
            contrast_ratio(black, white)
        );
        // The ratio is symmetric, and a color against itself is 1:1.
        assert!((contrast_ratio(white, black) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(white, white) - 1.0).abs() < 0.001);
    }

    /// A palette whose pairs already pass must come out untouched, so a theme file that ships
    /// compliant ink is never repainted by the floor.
    ///
    /// The fixture sits away from both clamp ends on purpose: an ink at `l = 0.0` cannot move, so
    /// the search would return it unchanged even with the early exit deleted, and the test would
    /// pass while proving nothing.
    #[test]
    fn an_ink_that_already_clears_the_floor_is_left_alone() {
        let ink = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.2,
            a: 1.0,
        };
        let surfaces = [Hsla {
            h: 0.0,
            s: 0.0,
            l: 1.0,
            a: 1.0,
        }];
        assert!(
            contrast_ratio(ink, surfaces[0]) > INK_CONTRAST_FLOOR,
            "the fixture must start out compliant, measured {}",
            contrast_ratio(ink, surfaces[0])
        );
        assert_eq!(
            enforce_contrast_floor(ink, &surfaces, INK_CONTRAST_FLOOR),
            ink
        );
    }

    /// A failing ink is moved until it clears the floor against *every* surface it is drawn on, not
    /// just the one it was measured against.
    #[test]
    fn a_failing_ink_is_moved_until_it_clears_every_surface() {
        let gray = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.5,
            a: 1.0,
        };
        let surfaces = [
            Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.96,
                a: 1.0,
            },
            Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.97,
                a: 1.0,
            },
        ];
        assert!(
            contrast_ratio(gray, surfaces[0]) < INK_CONTRAST_FLOOR,
            "the fixture must start out failing"
        );
        let fixed = enforce_contrast_floor(gray, &surfaces, INK_CONTRAST_FLOOR);
        for (i, surface) in surfaces.iter().enumerate() {
            assert!(
                contrast_ratio(fixed, *surface) >= INK_CONTRAST_FLOOR,
                "surface {i} still measured {} after the repair",
                contrast_ratio(fixed, *surface)
            );
        }
        assert!(
            fixed.l < gray.l,
            "a light surface must push the ink darker, not lighter"
        );
    }

    /// Only lightness moves, so a chromatic ink keeps the palette's hue and saturation.
    #[test]
    fn the_repair_moves_lightness_and_keeps_the_hue() {
        let ink = Hsla {
            h: 0.58,
            s: 0.4,
            l: 0.7,
            a: 1.0,
        };
        let surfaces = [Hsla {
            h: 0.0,
            s: 0.0,
            l: 1.0,
            a: 1.0,
        }];
        let fixed = enforce_contrast_floor(ink, &surfaces, INK_CONTRAST_FLOOR);
        assert_eq!(fixed.h, ink.h, "hue must survive the repair");
        assert_eq!(fixed.s, ink.s, "saturation must survive the repair");
        assert_eq!(fixed.a, ink.a, "alpha must survive the repair");
    }

    /// Hover and pressed neutralize to the same light gray across buttons, menus and the sidebar.
    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn hover_and_pressed_share_the_same_neutral_fill() {
        let mut theme = Theme::default();
        theme.list_hover = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.93,
            a: 1.0,
        };
        apply_unified_interaction_fill(&mut theme);

        assert_eq!(theme.tokens.accent.color, theme.list_hover);
        assert_eq!(theme.tokens.button_hover.color, theme.list_hover);
        assert_eq!(theme.tokens.button_active.color, theme.list_hover);
        assert_eq!(theme.list_active, theme.list_hover);
        assert_eq!(theme.sidebar_accent, theme.list_hover);
    }

    /// The secondary action (e.g. dialog Cancel button) must answer the pointer with a visible step
    /// in both light and dark themes.
    #[test]
    fn secondary_hover_and_pressed_move_toward_the_label_ink() {
        let mut dark = Theme {
            mode: ThemeMode::Dark,
            ..Default::default()
        };
        dark.tokens.button_secondary = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.15,
            a: 1.0,
        }
        .into();
        dark.tokens.button_secondary_foreground = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.90,
            a: 1.0,
        }
        .into();
        apply_unified_interaction_fill(&mut dark);
        let dark_rest = relative_luminance(dark.tokens.button_secondary.color);
        let dark_hover = relative_luminance(dark.tokens.button_secondary_hover.color);
        let dark_active = relative_luminance(dark.tokens.button_secondary_active.color);
        assert!(dark_hover > dark_rest, "dark secondary must lighten on hover");
        assert!(dark_active > dark_hover, "dark secondary must lighten further on active");

        let mut light = Theme {
            mode: ThemeMode::Light,
            ..Default::default()
        };
        light.tokens.button_secondary = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.90,
            a: 1.0,
        }
        .into();
        light.tokens.button_secondary_foreground = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.06,
            a: 1.0,
        }
        .into();
        apply_unified_interaction_fill(&mut light);
        let light_rest = relative_luminance(light.tokens.button_secondary.color);
        let light_hover = relative_luminance(light.tokens.button_secondary_hover.color);
        let light_active = relative_luminance(light.tokens.button_secondary_active.color);
        assert!(light_hover < light_rest, "light secondary must darken on hover");
        assert!(light_active < light_hover, "light secondary must darken further on active");
    }

    /// Verifies that repair_ink_contrast enforces pure grayscale tokens (#E6E6E6, #101010,
    /// #cccccc) without chromatic saturation on skeleton and interactive surfaces in both modes.
    #[test]
    fn repair_ink_contrast_sets_grayscale_tokens() {
        let mut theme = Theme {
            mode: ThemeMode::Light,
            ..Default::default()
        };
        repair_ink_contrast(&mut theme);
        assert_eq!(theme.background.s, 0.0);
        assert_eq!(theme.background.l, 0.9765);
        assert_eq!(theme.foreground.s, 0.0);
        assert_eq!(theme.foreground.l, 0.0627);
        assert_eq!(theme.skeleton.s, 0.0);
        assert_eq!(theme.skeleton.l, 0.80);
        assert_eq!(theme.selection.s, 0.0);
        assert_eq!(theme.list_hover.s, 0.0);

        theme.mode = ThemeMode::Dark;
        repair_ink_contrast(&mut theme);
        assert_eq!(theme.background.s, 0.0);
        assert_eq!(theme.background.l, 0.0627);
        assert_eq!(theme.foreground.s, 0.0);
        assert_eq!(theme.foreground.l, 0.90);
        assert_eq!(theme.sidebar_foreground.l, 0.90);
        assert_eq!(theme.sidebar_accent_foreground.l, 0.90);
        assert_eq!(theme.popover_foreground.l, 0.90);
        assert_eq!(theme.skeleton.s, 0.0);
        assert_eq!(theme.skeleton.l, 0.20);
        assert_eq!(theme.selection.s, 0.0);
        assert_eq!(theme.list_hover.s, 0.0);
    }

    /// The filled primary action must answer the pointer with a real step in both themes; the
    /// palette's own hover is one neutral level away from rest and disappears on the dark fill.
    #[test]
    fn primary_hover_and_pressed_move_toward_the_label_ink() {
        let mut light = Theme::default();
        light.tokens.button_primary = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.09,
            a: 1.0,
        }
        .into();
        light.tokens.button_primary_foreground = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.98,
            a: 1.0,
        }
        .into();
        apply_unified_interaction_fill(&mut light);
        let rest = relative_luminance(light.tokens.button_primary.color);
        let hover = relative_luminance(light.tokens.button_primary_hover.color);
        let active = relative_luminance(light.tokens.button_primary_active.color);
        assert!(
            hover > rest,
            "a dark fill must lighten toward its light ink"
        );
        assert!(active > hover, "pressed must be a further step than hover");

        let mut dark = Theme::default();
        dark.tokens.button_primary = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.98,
            a: 1.0,
        }
        .into();
        dark.tokens.button_primary_foreground = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.09,
            a: 1.0,
        }
        .into();
        apply_unified_interaction_fill(&mut dark);
        let rest = relative_luminance(dark.tokens.button_primary.color);
        let hover = relative_luminance(dark.tokens.button_primary_hover.color);
        let active = relative_luminance(dark.tokens.button_primary_active.color);
        assert!(hover < rest, "a light fill must darken toward its dark ink");
        assert!(active < hover, "pressed must be a further step than hover");
    }

    /// Destructive commits use the same visible pointer step as the primary action.
    #[test]
    fn danger_hover_and_pressed_move_toward_the_label_ink() {
        let red = |l: f32| Hsla {
            h: 0.0,
            s: 0.70,
            l,
            a: 1.0,
        };

        let mut light = Theme::default();
        light.tokens.button_danger = red(0.55).into();
        light.tokens.button_danger_foreground = red(0.98).into();
        apply_unified_interaction_fill(&mut light);
        let rest = relative_luminance(light.tokens.button_danger.color);
        let hover = relative_luminance(light.tokens.button_danger_hover.color);
        let active = relative_luminance(light.tokens.button_danger_active.color);
        assert!(hover > rest, "a dark red must lighten toward its light ink");
        assert!(active > hover, "pressed must be a further step than hover");

        let mut dark = Theme::default();
        dark.tokens.button_danger = red(0.65).into();
        dark.tokens.button_danger_foreground = red(0.30).into();
        apply_unified_interaction_fill(&mut dark);
        let rest = relative_luminance(dark.tokens.button_danger.color);
        let hover = relative_luminance(dark.tokens.button_danger_hover.color);
        let active = relative_luminance(dark.tokens.button_danger_active.color);
        assert!(hover < rest, "a light red must darken toward its dark ink");
        assert!(active < hover, "pressed must be a further step than hover");
    }
}
