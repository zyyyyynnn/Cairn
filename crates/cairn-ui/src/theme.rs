// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The product palette, shared by the search window and the installer.
//!
//! The component theme owns every color. This module pushes a mode and zoom into it and raises the
//! roles this product renders below their contrast floor. It mints one palette of its own — the
//! danger button's — because the component's own red pairs cannot carry a label in either mode.
//! Every other color still comes from the theme file, so a theme file or a custom theme reaches
//! both windows without a code change.

use gpui_kit::component::{
    neutral_950, red_300, red_400, red_500, red_600, red_700, red_800, white, Colorize as _, Theme,
    ThemeMode,
};
use gpui_kit::gpui::{hsla, px, App, Hsla, Pixels};

use crate::brand::{BASE_FONT_PX, RADIUS};

/// Lowest and highest interface zoom a Cairn window accepts.
pub const MIN_ZOOM: f32 = 0.9;
pub const MAX_ZOOM: f32 = 1.1;

/// The contrast floor the product requires of an ink against every surface it carries text on:
/// WCAG AA for text below the large-text threshold.
const INK_CONTRAST_FLOOR: f32 = 4.5;

/// The floor for a non-text indicator — the keyboard focus ring.
const RING_CONTRAST_FLOOR: f32 = 3.0;

/// The floor for a control's own boundary against the page (WCAG 1.4.11).
const CONTROL_CONTRAST_FLOOR: f32 = 3.0;

/// A neutral stroke for form-control boundaries that clears 3:1 against the page.
/// Structural hairlines keep the lighter `border` token; only the outline of an editable
/// or checkable control uses this role.
pub fn control_stroke(theme: &Theme) -> Hsla {
    enforce_contrast_floor(
        theme.border,
        &[theme.background, theme.muted],
        CONTROL_CONTRAST_FLOOR,
    )
}

/// The floor for a filled control against the surface it sits on — the danger button's fill
/// against the page and the status bar.
const SURFACE_CONTRAST_FLOOR: f32 = 3.0;

/// Clamp a persisted or requested zoom into [`MIN_ZOOM`]..=[`MAX_ZOOM`].
pub fn clamp_zoom(zoom: f32) -> f32 {
    zoom.clamp(MIN_ZOOM, MAX_ZOOM)
}

/// WCAG relative luminance of a rendered color.
fn relative_luminance(color: Hsla) -> f32 {
    fn channel(v: f32) -> f32 {
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
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

/// Enforces pure grayscale tokens and raises the ink roles that render below [`INK_CONTRAST_FLOOR`]
/// on the surfaces this product actually draws them on.
fn repair_ink_contrast(theme: &mut Theme) {
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
    theme.ring = enforce_contrast_floor(
        theme.ring,
        &[theme.background, theme.table_head, theme.muted],
        RING_CONTRAST_FLOOR,
    );
    theme.danger = enforce_contrast_floor(
        theme.danger,
        &[theme.background, theme.status_bar],
        INK_CONTRAST_FLOOR,
    );
}

/// The danger button's own palette: the red fill and the label that sits on it.
///
/// The component palette pairs light mode with `red-500` on `neutral-50` and dark mode with
/// `red-400` on `red-600`. Those pairs measure 3.6:1 and 1.8:1, which leaves the label of a
/// destructive action as the least legible text in the product. These pairs are the product's
/// own, and every value is a step of the component palette's red and neutral scales.
struct DangerPalette {
    /// The fill of the resting button.
    fill: Hsla,
    /// The fill under the pointer.
    hover: Hsla,
    /// The fill while the button is held down.
    active: Hsla,
    /// The label, the same on all three fills.
    label: Hsla,
}

/// The danger palette for one mode.
///
/// Light mode carries a white label, so its fill has to stay deep enough for that label to clear
/// the text floor; the fill deepens under the pointer and the press, which only widens the gap.
/// Dark mode has the opposite constraint — its fill has to stay light enough to hold its own 3:1
/// against the near-black page — so the label turns near-black and the fill lightens under the
/// pointer and the press instead.
fn danger_palette(dark: bool) -> DangerPalette {
    if dark {
        DangerPalette {
            fill: red_500(),
            hover: red_400(),
            active: red_300(),
            label: neutral_950(),
        }
    } else {
        DangerPalette {
            fill: red_600(),
            hover: red_700(),
            active: red_800(),
            label: white(),
        }
    }
}

/// Paints the danger button on both sides of the theme.
///
/// The button reads its fill from the tokens and its label from the colors, so both sides are
/// written here, to one value per field.
///
/// Every fill is raised until it clears the text floor against the label and the surface floor
/// against the page and the status bar, and every state after the first is raised until it
/// measures at least as much against the label as the state before it. The palette already
/// clears all three, so the raises are the guarantee a later palette edit cannot break.
fn apply_danger_button_palette(theme: &mut Theme) {
    let DangerPalette {
        fill,
        hover,
        active,
        label,
    } = danger_palette(theme.is_dark());
    let surfaces = [theme.background, theme.status_bar];
    let mut fills = [fill, hover, active];
    let mut floor = INK_CONTRAST_FLOOR;
    for fill in &mut fills {
        *fill = enforce_contrast_floor(*fill, &[label], floor);
        *fill = enforce_contrast_floor(*fill, &surfaces, SURFACE_CONTRAST_FLOOR);
        floor = contrast_ratio(label, *fill);
    }
    let [fill, hover, active] = fills;

    theme.colors.button_danger = fill;
    theme.colors.button_danger_hover = hover;
    theme.colors.button_danger_active = active;
    theme.colors.button_danger_foreground = label;
    theme.tokens.button_danger = fill.into();
    theme.tokens.button_danger_hover = hover.into();
    theme.tokens.button_danger_active = active.into();
    theme.tokens.button_danger_foreground = label.into();
}

/// Installs an appearance before the first frame and whenever a preference changes.
///
/// Separate edits, deliberately: changing `Theme::mode` re-resolves every role color from that
/// mode's theme file and discards colors written inside the same edit, so the ink floor has to run
/// in a second edit against the palette the mode switch just installed.
pub fn apply_appearance(dark: bool, zoom: f32, cx: &mut App) {
    crate::brand::init_fonts(cx);
    set_mode(dark, cx);
    settle_overlay(dark, cx);
    Theme::update(cx, |theme| {
        theme.font_size = px(BASE_FONT_PX * clamp_zoom(zoom));
        theme.focus_ring = false;
        theme.radius = RADIUS;
        theme.radius_lg = RADIUS;
    });
    raise_failing_ink(cx);
    strengthen_ghost_hover(cx);
    paint_danger_button(cx);
}

/// Switches the light or dark palette and returns the value to persist.
pub fn set_dark_mode(dark: bool, cx: &mut App) -> bool {
    set_mode(dark, cx);
    settle_overlay(dark, cx);
    raise_failing_ink(cx);
    strengthen_ghost_hover(cx);
    paint_danger_button(cx);
    dark
}

/// The component palette's light overlay is 5% black; on this product's surfaces that reads as a
/// detached card rather than a modal. Deepen the scrim in both modes.
fn settle_overlay(dark: bool, cx: &mut App) {
    Theme::update(cx, move |theme| {
        theme.overlay = hsla(0., 0., 0., if dark { 0.38 } else { 0.18 });
    });
}

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

fn raise_failing_ink(cx: &mut App) {
    Theme::update(cx, repair_ink_contrast);
}

fn strengthen_ghost_hover(cx: &mut App) {
    Theme::update(cx, apply_unified_interaction_fill);
}

fn paint_danger_button(cx: &mut App) {
    Theme::update(cx, apply_danger_button_palette);
}

/// One neutral interaction fill for hover and pressed everywhere.
fn apply_unified_interaction_fill(theme: &mut Theme) {
    let hover = theme.list_hover;
    theme.list_active = hover;
    theme.tokens.accent = hover.into();
    theme.tokens.button_hover = hover.into();
    theme.tokens.button_active = hover.into();
    theme.sidebar_accent = hover;

    let secondary = theme.tokens.button_secondary.color;
    let secondary_ink = theme.tokens.button_secondary_foreground.color;
    let sec_hover = secondary_ink.mix_oklab(secondary, 0.15);
    let sec_active = secondary_ink.mix_oklab(secondary, 0.22);
    theme.tokens.button_secondary_hover = sec_hover.into();
    theme.tokens.button_secondary_active = sec_active.into();
    theme.tokens.secondary_hover = sec_hover.into();
    theme.tokens.secondary_active = sec_active.into();
    theme.secondary_active = sec_active;

    let primary = theme.tokens.button_primary.color;
    let primary_ink = theme.tokens.button_primary_foreground.color;
    theme.tokens.button_primary_hover = primary_ink.mix_oklab(primary, 0.12).into();
    theme.tokens.button_primary_active = primary_ink.mix_oklab(primary, 0.20).into();
}

/// Rescales a window by moving the theme's base font, so type, spacing, control frames and icons
/// keep the relationships they had at 1.0. Returns the value to persist.
pub fn set_zoom(zoom: f32, cx: &mut App) -> f32 {
    let zoom = clamp_zoom(zoom);
    Theme::update(cx, |theme| theme.font_size = px(BASE_FONT_PX * zoom));
    zoom
}

/// The current `rem`, in pixels: the length every product layout helper is a multiple of.
pub fn rem(window: &gpui_kit::gpui::Window) -> Pixels {
    window.rem_size()
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::{
        apply_danger_button_palette, apply_unified_interaction_fill, clamp_zoom, contrast_ratio,
        enforce_contrast_floor, relative_luminance, repair_ink_contrast, INK_CONTRAST_FLOOR,
        MAX_ZOOM, MIN_ZOOM, SURFACE_CONTRAST_FLOOR,
    };
    use crate::brand::{BASE_FONT_PX, HAIRLINE};
    use gpui_kit::component::{Theme, ThemeMode};
    use gpui_kit::gpui::{px, rgb, Hsla};

    /// A theme built the way the product builds it: the mode's surfaces, the ink floor, the
    /// interaction fills, and the danger button's own palette.
    fn product_theme(dark: bool) -> Theme {
        let mut theme = Theme {
            mode: if dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
            ..Default::default()
        };
        theme.colors.status_bar = if dark {
            rgb(0x171717).into()
        } else {
            rgb(0xF8F8F8).into()
        };
        repair_ink_contrast(&mut theme);
        apply_unified_interaction_fill(&mut theme);
        apply_danger_button_palette(&mut theme);
        theme
    }

    /// The bytes a color renders as, so a palette step can be named in a test.
    fn rgb_bytes(color: Hsla) -> (u8, u8, u8) {
        let rgb = color.to_rgb();
        (
            (rgb.r * 255.0).round() as u8,
            (rgb.g * 255.0).round() as u8,
            (rgb.b * 255.0).round() as u8,
        )
    }

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

    #[test]
    fn the_hairline_is_a_device_pixel_not_a_rem_step() {
        assert_eq!(HAIRLINE, px(1.0));
        assert_eq!(BASE_FONT_PX, 14.0);
    }

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
        assert!((contrast_ratio(black, white) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(white, black) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(white, white) - 1.0).abs() < 0.001);
    }

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
        assert!(contrast_ratio(ink, surfaces[0]) > INK_CONTRAST_FLOOR);
        assert_eq!(
            enforce_contrast_floor(ink, &surfaces, INK_CONTRAST_FLOOR),
            ink
        );
    }

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
        assert!(contrast_ratio(gray, surfaces[0]) < INK_CONTRAST_FLOOR);
        let fixed = enforce_contrast_floor(gray, &surfaces, INK_CONTRAST_FLOOR);
        for (i, surface) in surfaces.iter().enumerate() {
            assert!(
                contrast_ratio(fixed, *surface) >= INK_CONTRAST_FLOOR,
                "surface {i} still measured {}",
                contrast_ratio(fixed, *surface)
            );
        }
        assert!(fixed.l < gray.l);
    }

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
        assert_eq!(fixed.h, ink.h);
        assert_eq!(fixed.s, ink.s);
        assert_eq!(fixed.a, ink.a);
    }

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
        let rest = relative_luminance(dark.tokens.button_secondary.color);
        let hover = relative_luminance(dark.tokens.button_secondary_hover.color);
        let active = relative_luminance(dark.tokens.button_secondary_active.color);
        assert!(hover > rest);
        assert!(active > hover);

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
        let rest = relative_luminance(light.tokens.button_secondary.color);
        let hover = relative_luminance(light.tokens.button_secondary_hover.color);
        let active = relative_luminance(light.tokens.button_secondary_active.color);
        assert!(hover < rest);
        assert!(active < hover);
    }

    #[test]
    fn ring_clears_non_text_contrast_on_card_surfaces() {
        for (mode, background, muted, ring) in [
            (ThemeMode::Light, 0.9765, 0.961, 0.639),
            (ThemeMode::Dark, 0.0627, 0.149, 0.451),
        ] {
            let bg = Hsla {
                h: 0.0,
                s: 0.0,
                l: background,
                a: 1.0,
            };
            let card = Hsla {
                h: 0.0,
                s: 0.0,
                l: muted,
                a: 1.0,
            };
            let ink = Hsla {
                h: 0.0,
                s: 0.0,
                l: ring,
                a: 1.0,
            };
            let stroke = enforce_contrast_floor(ink, &[bg, card], super::RING_CONTRAST_FLOOR);
            let ratio = contrast_ratio(stroke, bg).min(contrast_ratio(stroke, card));
            assert!(ratio >= 3.0, "{mode:?} ring contrast {ratio} is below 3:1");
        }
    }

    #[test]
    fn control_stroke_clears_non_text_contrast() {
        for (mode, background, muted, border) in [
            (ThemeMode::Light, 0.9765, 0.961, 0.898),
            (ThemeMode::Dark, 0.0627, 0.149, 0.149),
        ] {
            let mut theme = Theme {
                mode,
                ..Default::default()
            };
            theme.background = Hsla {
                h: 0.0,
                s: 0.0,
                l: background,
                a: 1.0,
            };
            theme.border = Hsla {
                h: 0.0,
                s: 0.0,
                l: border,
                a: 1.0,
            };
            theme.muted = Hsla {
                h: 0.0,
                s: 0.0,
                l: muted,
                a: 1.0,
            };
            let stroke = super::control_stroke(&theme);
            let ratio =
                contrast_ratio(stroke, theme.background).min(contrast_ratio(stroke, theme.muted));
            assert!(
                ratio >= 3.0,
                "{mode:?} control stroke contrast {ratio} is below 3:1"
            );
        }
    }

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
        assert!(hover > rest);
        assert!(active > hover);

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
        assert!(hover < rest);
        assert!(active < hover);
    }

    #[test]
    fn the_danger_button_carries_a_label_that_clears_the_text_floor() {
        for dark in [false, true] {
            let theme = product_theme(dark);
            let label = theme.colors.button_danger_foreground;
            let fills = [
                ("rest", theme.tokens.button_danger.color),
                ("hover", theme.tokens.button_danger_hover.color),
                ("active", theme.tokens.button_danger_active.color),
            ];
            let mut previous = 0.0;
            for (state, fill) in fills {
                let ratio = contrast_ratio(label, fill);
                assert!(
                    ratio >= INK_CONTRAST_FLOOR,
                    "the {state} fill measures {ratio} against its label"
                );
                assert!(
                    ratio >= previous,
                    "the {state} fill measures {ratio}, less than the state before it"
                );
                previous = ratio;
            }
            println!(
                "danger button ({}): rest {:.2}, hover {:.2}, active {:.2}",
                if dark { "dark" } else { "light" },
                contrast_ratio(label, theme.tokens.button_danger.color),
                contrast_ratio(label, theme.tokens.button_danger_hover.color),
                contrast_ratio(label, theme.tokens.button_danger_active.color),
            );
        }
    }

    #[test]
    fn the_danger_fill_holds_its_own_against_the_page() {
        for dark in [false, true] {
            let theme = product_theme(dark);
            for fill in [
                theme.tokens.button_danger.color,
                theme.tokens.button_danger_hover.color,
                theme.tokens.button_danger_active.color,
            ] {
                let page = contrast_ratio(fill, theme.background);
                assert!(
                    page >= SURFACE_CONTRAST_FLOOR,
                    "the fill measures {page} against the page"
                );
                let bar = contrast_ratio(fill, theme.status_bar);
                assert!(
                    bar >= SURFACE_CONTRAST_FLOOR,
                    "the fill measures {bar} against the status bar"
                );
            }
        }
    }

    #[test]
    fn the_danger_button_keeps_the_products_red() {
        let light = product_theme(false);
        assert_eq!(
            rgb_bytes(light.tokens.button_danger.color),
            (0xdc, 0x26, 0x26)
        );
        assert_eq!(
            rgb_bytes(light.tokens.button_danger_hover.color),
            (0xb9, 0x1c, 0x1c)
        );
        assert_eq!(
            rgb_bytes(light.tokens.button_danger_active.color),
            (0x99, 0x1b, 0x1b)
        );
        assert_eq!(
            rgb_bytes(light.colors.button_danger_foreground),
            (0xff, 0xff, 0xff)
        );

        let dark = product_theme(true);
        assert_eq!(
            rgb_bytes(dark.tokens.button_danger.color),
            (0xef, 0x44, 0x44)
        );
        assert_eq!(
            rgb_bytes(dark.tokens.button_danger_hover.color),
            (0xf8, 0x71, 0x71)
        );
        assert_eq!(
            rgb_bytes(dark.tokens.button_danger_active.color),
            (0xfc, 0xa5, 0xa5)
        );
        assert_eq!(
            rgb_bytes(dark.colors.button_danger_foreground),
            (0x0a, 0x0a, 0x0a)
        );

        for theme in [light, dark] {
            assert!(
                theme.tokens.button_danger.color.s >= 0.7,
                "the fill is not a red"
            );
        }
    }

    #[test]
    fn the_danger_button_reads_one_value_per_field() {
        for dark in [false, true] {
            let theme = product_theme(dark);
            assert_eq!(theme.colors.button_danger, theme.tokens.button_danger.color);
            assert_eq!(
                theme.colors.button_danger_hover,
                theme.tokens.button_danger_hover.color
            );
            assert_eq!(
                theme.colors.button_danger_active,
                theme.tokens.button_danger_active.color
            );
            assert_eq!(
                theme.colors.button_danger_foreground,
                theme.tokens.button_danger_foreground.color
            );
        }
    }
}
