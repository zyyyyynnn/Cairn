// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The product palette, shared by the search window and the installer.
//!
//! One neutral primitive ramp feeds a semantic [`ModePalette`] per appearance; the palette is
//! written into the component theme together with the mode and zoom, the roles this product renders
//! are held at their contrast floor, and every neutral role a used component reads is repointed at
//! the ramp, because an unassigned role keeps the component theme file's own Tailwind gray. The
//! danger button takes the component's red ramp with the palette's solid label; the chromatic state
//! roles keep the component palette.

use gpui_kit::component::{
    red_300, red_400, red_500, red_600, red_700, red_800, Colorize as _, Theme, ThemeMode,
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
        &[theme.background, theme.muted, theme.title_bar],
        CONTROL_CONTRAST_FLOOR,
    )
}

/// The warning ink used for a state graphic (not a filled surface). The component's warning
/// yellow is too light for a 32px icon on white, so this role holds the same hue at 3:1.
pub fn warning_ink(theme: &Theme) -> Hsla {
    enforce_contrast_floor(
        theme.warning,
        &[theme.background, theme.muted, theme.title_bar],
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

/// A rendered sRGB channel on the linear scale every colorimetric calculation below works on.
fn into_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG relative luminance of a rendered color.
fn relative_luminance(color: Hsla) -> f32 {
    let rgb = color.to_rgb();
    0.2126 * into_linear(rgb.r) + 0.7152 * into_linear(rgb.g) + 0.0722 * into_linear(rgb.b)
}

/// The contrast ratio between two rendered colors, as a value >= 1.
pub fn contrast_ratio(foreground: Hsla, background: Hsla) -> f32 {
    let a = relative_luminance(foreground);
    let b = relative_luminance(background);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The CIE L\* difference between two rendered colors: how far a flat surface moves when it changes
/// state, independent of which direction it moves. A WCAG ratio compares two inks and calls a
/// 1.089:1 hover acceptable; on a fill that lights a whole row, menu item or control, L\* is the
/// scale the change is actually perceived on.
pub fn lightness_step(left: Hsla, right: Hsla) -> f32 {
    fn l_star(y: f32) -> f32 {
        if y > 0.008856 {
            116.0 * y.cbrt() - 16.0
        } else {
            903.3 * y
        }
    }
    (l_star(relative_luminance(left)) - l_star(relative_luminance(right))).abs()
}

/// The color's position in OKLCH: perceived lightness in `0.0..=1.0`, chroma, and hue angle in
/// degrees. A data plane chooses its hues on this scale, because equal numbers here are equal
/// perceived steps in a way sRGB and HSL numbers are not.
pub fn oklch(color: Hsla) -> (f32, f32, f32) {
    let rgb = color.to_rgb();
    let (r, g, b) = (into_linear(rgb.r), into_linear(rgb.g), into_linear(rgb.b));
    // The OKLab matrices, carried to the seven significant digits an f32 can hold.
    let l = (0.4122215 * r + 0.5363325 * g + 0.05144385 * b).cbrt();
    let m = (0.2119035 * r + 0.6806995 * g + 0.107397 * b).cbrt();
    let s = (0.08830246 * r + 0.2817188 * g + 0.6299787 * b).cbrt();
    let a = 1.977998 * l - 2.428592 * m + 0.4505937 * s;
    let c = 0.02590404 * l + 0.7827718 * m - 0.8086758 * s;
    let lightness = 0.2104543 * l + 0.7936178 * m - 0.00407205 * s;
    let chroma = a.hypot(c);
    let hue = if chroma < 1e-5 {
        0.0
    } else {
        (c.atan2(a).to_degrees() + 360.0) % 360.0
    };
    (lightness, chroma, hue)
}

/// The CIE 76 difference between two rendered colors: the scale on which two surfaces placed side
/// by side read as one color or as two. Around 2 is invisible at a glance, around 10 is obvious.
pub fn perceptual_distance(left: Hsla, right: Hsla) -> f32 {
    fn coordinates(color: Hsla) -> (f32, f32, f32) {
        let rgb = color.to_rgb();
        let x = into_linear(rgb.r) * 0.4124564
            + into_linear(rgb.g) * 0.3575761
            + into_linear(rgb.b) * 0.1804375;
        let y = into_linear(rgb.r) * 0.2126729
            + into_linear(rgb.g) * 0.7151522
            + into_linear(rgb.b) * 0.072175;
        let z = into_linear(rgb.r) * 0.0193339
            + into_linear(rgb.g) * 0.119192
            + into_linear(rgb.b) * 0.9503041;
        let curve = |t: f32| {
            if t > 0.008856 {
                t.cbrt()
            } else {
                7.787 * t + 16.0 / 116.0
            }
        };
        let (fx, fy, fz) = (curve(x / 0.95047), curve(y), curve(z / 1.08883));
        (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
    }
    let (l1, a1, b1) = coordinates(left);
    let (l2, a2, b2) = coordinates(right);
    ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt()
}

/// Every step of the neutral ramp, light to dark.
pub const NEUTRAL_RAMP: [Hsla; 16] = gray::ALL;

/// Whether an achromatic color is one of the ramp's steps. A data plane may add hues of its own; no
/// neutral in the product may land between two steps.
pub fn is_neutral_ramp_step(color: Hsla) -> bool {
    let rgb = color.to_rgb();
    let near = |other: Hsla| {
        let other = other.to_rgb();
        (other.r - rgb.r).abs() < 0.5 / 255.0
            && (other.g - rgb.g).abs() < 0.5 / 255.0
            && (other.b - rgb.b).abs() < 0.5 / 255.0
    };
    NEUTRAL_RAMP.iter().copied().any(near)
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

/// The neutral primitive ramp: achromatic steps ordered light to dark, numbered by the CIE L\*
/// darkness each carries. Sixteen steps, every one consumed by a role below; the two ends sit at
/// least eight bytes from white and black, so no surface in the product is near-pure. Within a mode
/// the steps that can appear next to each other — a line, a hover fill, a selection — are at least
/// three L\* apart, so no step is decoration. The anchors are `GRAY_3` (`#F7F7F7`, light canvas),
/// `GRAY_91` (`#1A1A1A`, light ink and accent), `GRAY_94` (`#141414`, dark canvas) and `GRAY_9`
/// (`#E4E4E4`, dark ink and accent). Dark is not the light ramp read backwards: only one of its
/// steps is the mirror of a light step, because a near-black ground needs its own spacing.
mod gray {
    use gpui_kit::gpui::Hsla;

    /// An achromatic color from its sRGB channel byte.
    const fn step(byte: u8) -> Hsla {
        Hsla {
            h: 0.0,
            s: 0.0,
            l: byte as f32 / 255.0,
            a: 1.0,
        }
    }

    /// A step named by the darkness it carries: `GRAY_76` sits at CIE L\* 24, so the number says
    /// how far the step is from white and a reader can compare two steps without converting them.
    pub const GRAY_3: Hsla = step(0xF7);
    pub const GRAY_5: Hsla = step(0xF2);
    pub const GRAY_7: Hsla = step(0xEC);
    pub const GRAY_9: Hsla = step(0xE4);
    pub const GRAY_13: Hsla = step(0xD9);
    pub const GRAY_17: Hsla = step(0xCE);
    pub const GRAY_33: Hsla = step(0xA3);
    pub const GRAY_44: Hsla = step(0x86);
    pub const GRAY_53: Hsla = step(0x70);
    pub const GRAY_61: Hsla = step(0x5C);
    pub const GRAY_72: Hsla = step(0x42);
    pub const GRAY_76: Hsla = step(0x3A);
    pub const GRAY_81: Hsla = step(0x2E);
    pub const GRAY_88: Hsla = step(0x1F);
    pub const GRAY_91: Hsla = step(0x1A);
    pub const GRAY_94: Hsla = step(0x14);

    /// Every step, light to dark. The ramp is walked for monotonicity in tests and published as
    /// [`super::NEUTRAL_RAMP`] so a data plane can check a neutral against it.
    pub const ALL: [Hsla; 16] = [
        GRAY_3, GRAY_5, GRAY_7, GRAY_9, GRAY_13, GRAY_17, GRAY_33, GRAY_44, GRAY_53, GRAY_61,
        GRAY_72, GRAY_76, GRAY_81, GRAY_88, GRAY_91, GRAY_94,
    ];
}

/// The semantic color roles of one appearance. Each field names a job and points at a step of the
/// neutral ramp. The content, inset and frame steps carry the window hierarchy instead of heavier
/// borders.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModePalette {
    /// The page: result body, table head and the light dialog.
    pub content: Hsla,
    /// Sunken surfaces: cards, toasts and the unfocused search box.
    pub inset: Hsla,
    /// Window chrome: title bar, sidebar, status bar and the dark dialog.
    pub frame: Hsla,
    /// Hover, pressed and selected-row fill, and the highlight of a ghost control or menu item.
    /// One step serves both because the component paints this role at half strength over a dark
    /// ground, and the step is chosen so the halved composite still moves its ground visibly.
    pub interaction: Hsla,
    /// Structural hairlines, the outline of an editable control, and the fill of a neutral action:
    /// one step, because all three are the quiet line-and-fill tier between the frame and the
    /// interaction fill.
    pub border: Hsla,
    /// The outline of an editable control.
    pub input: Hsla,
    /// Text selection behind ink.
    pub selection: Hsla,
    /// Keyboard focus indicator, 3:1 against every surface.
    pub ring: Hsla,
    /// Secondary ink, 4.5:1 against every surface and the interaction fill.
    pub muted_foreground: Hsla,
    /// Primary ink.
    pub foreground: Hsla,
    /// The theme color: the fill of the one primary action.
    pub accent: Hsla,
    /// The fill of a neutral action beside the primary one.
    pub secondary: Hsla,
    /// The label on every solid fill: the accent and the danger button.
    pub solid_label: Hsla,
}

/// Light mode: three surface steps under a near-white page, then the line tier, the interaction
/// fill and the selection, each at least three L\* from the one before it.
pub const LIGHT_PALETTE: ModePalette = ModePalette {
    content: gray::GRAY_3,
    inset: gray::GRAY_5,
    frame: gray::GRAY_7,
    interaction: gray::GRAY_13,
    border: gray::GRAY_9,
    input: gray::GRAY_9,
    selection: gray::GRAY_17,
    ring: gray::GRAY_44,
    muted_foreground: gray::GRAY_61,
    foreground: gray::GRAY_91,
    accent: gray::GRAY_91,
    secondary: gray::GRAY_9,
    solid_label: gray::GRAY_3,
};

/// Dark mode takes its own steps from the dark end of the ramp instead of mirroring light mode: the
/// page, inset and frame steps are compressed, and the interaction step is the one the component
/// halves over a dark ground — `#3A3A3A` composites to `#2A2A2A`, which moves the `#1F1F1F` frame by
/// 6.5 L\*, where the `#262626` this palette used to carry moved it by 3.4 and read as no change.
/// Ink stops at the `#E4E4E4` anchor, short of white.
pub const DARK_PALETTE: ModePalette = ModePalette {
    content: gray::GRAY_94,
    inset: gray::GRAY_91,
    frame: gray::GRAY_88,
    interaction: gray::GRAY_76,
    border: gray::GRAY_81,
    input: gray::GRAY_81,
    selection: gray::GRAY_72,
    ring: gray::GRAY_53,
    muted_foreground: gray::GRAY_33,
    foreground: gray::GRAY_9,
    accent: gray::GRAY_9,
    secondary: gray::GRAY_81,
    solid_label: gray::GRAY_94,
};

/// The palette for one appearance.
pub const fn mode_palette(dark: bool) -> &'static ModePalette {
    if dark {
        &DARK_PALETTE
    } else {
        &LIGHT_PALETTE
    }
}

/// Writes the mode's roles into the component theme. Button fields are written on both the colors
/// and the tokens, because components read them from the tokens.
fn apply_mode_palette(theme: &mut Theme) {
    let palette = mode_palette(theme.mode == ThemeMode::Dark);
    theme.background = palette.content;
    theme.muted = palette.inset;
    theme.table_head = palette.content;
    theme.sidebar = palette.frame;
    theme.status_bar = palette.frame;
    theme.title_bar = palette.frame;
    theme.border = palette.border;
    theme.title_bar_border = palette.border;
    theme.sidebar_border = palette.border;
    theme.status_bar_border = palette.border;
    theme.input = palette.input;
    theme.list_hover = palette.interaction;
    theme.list_active = palette.interaction;
    theme.sidebar_accent = palette.interaction;
    theme.selection = palette.selection;
    theme.foreground = palette.foreground;
    theme.caret = palette.foreground;
    theme.muted_foreground = palette.muted_foreground;
    theme.table_head_foreground = palette.muted_foreground;
    theme.sidebar_foreground = palette.foreground;
    theme.sidebar_accent_foreground = palette.foreground;
    theme.popover_foreground = palette.foreground;
    theme.ring = palette.ring;

    theme.primary = palette.accent;
    theme.primary_foreground = palette.solid_label;
    theme.colors.button_primary = palette.accent;
    theme.colors.button_primary_foreground = palette.solid_label;
    theme.tokens.button_primary = palette.accent.into();
    theme.tokens.button_primary_foreground = palette.solid_label.into();
    theme.colors.button_secondary = palette.secondary;
    theme.colors.button_secondary_foreground = palette.foreground;
    theme.tokens.button_secondary = palette.secondary.into();
    theme.tokens.button_secondary_foreground = palette.foreground.into();

    bind_component_neutrals(theme, palette);
}

/// Writes the neutral roles the components read but this product never assigned.
///
/// An unassigned role keeps the component theme file's own Tailwind gray, and those grays are not
/// this ramp: the dark file's `secondary.foreground` is `neutral-50` (`#FAFAFA`), so every ghost
/// icon in the title bar lit brighter than the `#E4E4E4` brand beside it, while the light file's
/// `neutral-900` (`#171717`) sits three bytes from the ramp and the same leak was invisible. The
/// popover, progress and scrollbar roles leak the same way (`#0A0A0A`, `#F5F5F5`, `#525252`).
///
/// Only roles a used component actually reads are written here; the chromatic state roles keep
/// the component palette.
fn bind_component_neutrals(theme: &mut Theme, palette: &ModePalette) {
    // Text and glyphs.
    theme.secondary_foreground = palette.foreground;
    theme.accent_foreground = palette.foreground;
    theme.button_foreground = palette.foreground;
    theme.link = palette.foreground;
    theme.link_hover = palette.muted_foreground;
    theme.link_active = palette.foreground;

    // Surfaces that float over the page.
    let popover = dialog_surface(theme);
    theme.popover = popover;
    theme.tokens.popover = popover.into();
    theme.tokens.background = palette.content.into();
    theme.tokens.muted = palette.inset.into();
    theme.tokens.status_bar = palette.frame.into();

    // Fills.
    theme.secondary = palette.secondary;
    theme.tokens.secondary = palette.secondary.into();
    theme.progress_bar = palette.accent;
    theme.tokens.progress_bar = palette.accent.into();

    // The scroll track and thumb: the thumb is a non-text control, so it takes the ring step,
    // and its hover state takes the step that moves further from the page in both modes.
    theme.scrollbar = palette.inset;
    theme.scrollbar_thumb = palette.ring;
    theme.scrollbar_thumb_hover = palette.muted_foreground;
}

/// Applies the mode palette and raises the ink roles that render below [`INK_CONTRAST_FLOOR`] on
/// the surfaces this product actually draws them on.
fn repair_ink_contrast(theme: &mut Theme) {
    apply_mode_palette(theme);

    let secondary_surfaces = [
        theme.background,
        theme.muted,
        theme.table_head,
        theme.sidebar,
        theme.status_bar,
        theme.title_bar,
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
        &[
            theme.background,
            theme.table_head,
            theme.muted,
            theme.sidebar,
            theme.status_bar,
            theme.title_bar,
        ],
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
/// destructive action as the least legible text in the product. These fills are steps of the
/// component palette's red ramp, and the label is the mode's solid label.
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
    let label = mode_palette(dark).solid_label;
    if dark {
        DangerPalette {
            fill: red_500(),
            hover: red_400(),
            active: red_300(),
            label,
        }
    } else {
        DangerPalette {
            fill: red_600(),
            hover: red_700(),
            active: red_800(),
            label,
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

/// The dialog's raised surface. Light keeps the page white; dark uses the frame step so the modal
/// is not the same color as the page it covers.
pub fn dialog_surface(theme: &Theme) -> Hsla {
    if theme.mode == ThemeMode::Dark {
        theme.title_bar
    } else {
        theme.background
    }
}

/// The modal scrim.
///
/// The component palette's light overlay is 5% black, which reads as a detached card rather than a
/// modal, so the scrim is deepened in both modes. Dark stops at 35%: pure black at the deeper alpha
/// composited the `#141414` page down to `#090909`, within nine bytes of black, and the dialog's own
/// raised surface and border already carry the separation.
fn overlay_color(dark: bool) -> Hsla {
    hsla(0., 0., 0., if dark { 0.35 } else { 0.18 })
}
/// Writes the scrim, so a modal separates from the page it covers.
fn settle_overlay(dark: bool, cx: &mut App) {
    Theme::update(cx, move |theme| {
        theme.overlay = overlay_color(dark);
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
///
/// Rows, list items, the sidebar, ghost controls and menu items all take
/// [`ModePalette::interaction`]. The step is chosen on the weakest of those readings: the component
/// paints the `accent` role at half strength over a dark ground, so the dark step has to clear the
/// floor after that composite, not before it.
fn apply_unified_interaction_fill(theme: &mut Theme) {
    let palette = mode_palette(theme.is_dark());
    let hover = palette.interaction;
    theme.list_hover = hover;
    theme.list_active = hover;
    theme.tokens.button_hover = hover.into();
    theme.tokens.button_active = hover.into();
    theme.sidebar_accent = hover;
    theme.accent = hover;
    theme.tokens.accent = hover.into();

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
        dialog_surface, enforce_contrast_floor, gray, is_neutral_ramp_step, lightness_step,
        mode_palette, oklch, overlay_color, perceptual_distance, relative_luminance,
        repair_ink_contrast, ModePalette, DARK_PALETTE, INK_CONTRAST_FLOOR, LIGHT_PALETTE,
        MAX_ZOOM, MIN_ZOOM, NEUTRAL_RAMP, RING_CONTRAST_FLOOR, SURFACE_CONTRAST_FLOOR,
    };
    use crate::brand::{BASE_FONT_PX, HAIRLINE};
    use gpui_kit::component::{Theme, ThemeMode};
    use gpui_kit::gpui::{px, Hsla};

    /// A theme built the way the product builds it: the mode palette, the ink floor, the
    /// interaction fills, and the danger button's own palette.
    fn product_theme(dark: bool) -> Theme {
        let mut theme = Theme::default();
        theme.mode = if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
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

    /// The L\* step a state fill has to take against the surface it lights before the pointer reads
    /// as hovering. Light mode's own row hover moves its frame by 5.3, which is the bar.
    const INTERACTION_LIGHTNESS_FLOOR: f32 = 5.0;

    /// The fill a ghost control or a menu item ends up showing: the component paints the `accent`
    /// role at half strength over its own ground on dark and takes it whole on light, so the floor
    /// is met by this composite rather than by the role value.
    fn control_hover_fill(accent: Hsla, ground: Hsla, dark: bool) -> Hsla {
        if dark {
            let mut half = accent;
            half.a = 0.5;
            ground.blend(half)
        } else {
            accent
        }
    }

    #[test]
    fn the_perceptual_helpers_agree_with_their_reference_points() {
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
        let gray = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.5,
            a: 1.0,
        };
        assert!((perceptual_distance(black, white) - 100.0).abs() < 0.2);
        assert_eq!(perceptual_distance(gray, gray), 0.0);
        let (lightness, chroma, _) = oklch(gray);
        assert!(
            (lightness - 0.5999).abs() < 0.002,
            "mid gray sits at OKLCH L {lightness}"
        );
        assert!(chroma.abs() < 1e-4, "a neutral carries no chroma");
        // A pure red sits at OKLCH hue 29 degrees, the angle the one-color-one-meaning rule compares.
        let red = Hsla {
            h: 0.0,
            s: 1.0,
            l: 0.5,
            a: 1.0,
        };
        let (_, _, red_hue) = oklch(red);
        assert!(
            (red_hue - 29.2).abs() < 1.0,
            "red landed at hue {red_hue} degrees"
        );
    }

    #[test]
    fn the_ramp_is_monotonic_and_a_step_is_recognised() {
        let mut previous = None;
        for step in NEUTRAL_RAMP {
            let (lightness, chroma, _) = oklch(step);
            assert!(chroma.abs() < 0.002, "a ramp step carries chroma {chroma}");
            if let Some(before) = previous {
                assert!(lightness < before, "the ramp stopped getting darker");
            }
            previous = Some(lightness);
        }
        assert!(is_neutral_ramp_step(mode_palette(true).frame));
        assert!(is_neutral_ramp_step(mode_palette(false).content));
        assert!(!is_neutral_ramp_step(Hsla {
            h: 0.0,
            s: 1.0,
            l: 0.5,
            a: 1.0,
        }));
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
    fn one_interaction_fill_lights_rows_controls_and_menu_items() {
        for dark in [false, true] {
            let theme = product_theme(dark);
            let palette = mode_palette(dark);
            assert_eq!(theme.list_hover, palette.interaction);
            assert_eq!(theme.list_active, theme.list_hover);
            assert_eq!(theme.sidebar_accent, theme.list_hover);
            assert_eq!(theme.tokens.button_hover.color, theme.list_hover);
            assert_eq!(theme.tokens.button_active.color, theme.list_hover);
            assert_eq!(theme.accent, palette.interaction);
            assert_eq!(theme.tokens.accent.color, theme.accent);
        }
    }

    #[test]
    fn secondary_hover_and_pressed_move_toward_the_label_ink() {
        let mut dark = Theme::default();
        dark.mode = ThemeMode::Dark;
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

        let mut light = Theme::default();
        light.mode = ThemeMode::Light;
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
    fn the_dark_secondary_action_clears_the_dialog_surface() {
        let theme = product_theme(true);
        let dialog = dialog_surface(&theme);
        let fill = theme.tokens.button_secondary.color;
        assert_eq!(
            fill, DARK_PALETTE.secondary,
            "the filled action should take the palette's secondary step"
        );
        assert_ne!(
            fill, dialog,
            "the filled action is painted in the dialog's own surface color"
        );
        println!(
            "dark secondary action on the dialog: {:.2}",
            contrast_ratio(fill, dialog)
        );
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
    fn the_dark_dialog_uses_a_raised_surface_and_a_stronger_scrim() {
        let mut dark = Theme::default();
        dark.mode = ThemeMode::Dark;
        dark.background = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.078431,
            a: 1.0,
        };
        dark.title_bar = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0x1F as f32 / 255.0,
            a: 1.0,
        };
        assert_eq!(dialog_surface(&dark), dark.title_bar);

        let scrim = overlay_color(true);
        assert_eq!(scrim.h, 0.0);
        assert_eq!(scrim.s, 0.0);
        assert_eq!(scrim.l, 0.0);
        assert!((scrim.a - 0.35).abs() < f32::EPSILON);

        let mut light = Theme::default();
        light.mode = ThemeMode::Light;
        assert_eq!(dialog_surface(&light), light.background);
        assert!((overlay_color(false).a - 0.18).abs() < f32::EPSILON);
    }

    #[test]
    fn control_stroke_clears_non_text_contrast() {
        for (mode, background, muted, border) in [
            (ThemeMode::Light, 0.9765, 0.961, 0.898),
            (ThemeMode::Dark, 0.0627, 0.149, 0.149),
        ] {
            let mut theme = Theme::default();
            theme.mode = mode;
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
            let ratio = [theme.background, theme.muted, theme.title_bar]
                .into_iter()
                .map(|surface| contrast_ratio(stroke, surface))
                .fold(f32::INFINITY, f32::min);
            assert!(
                ratio >= 3.0,
                "{mode:?} control stroke contrast {ratio} is below 3:1"
            );
        }
    }

    #[test]
    fn warning_ink_clears_non_text_contrast() {
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let mut theme = Theme::default();
            theme.mode = mode;
            let ink = super::warning_ink(&theme);
            let ratio = [theme.background, theme.muted, theme.title_bar]
                .into_iter()
                .map(|surface| contrast_ratio(ink, surface))
                .fold(f32::INFINITY, f32::min);
            assert!(
                ratio >= 3.0,
                "{mode:?} warning ink contrast {ratio} is below 3:1"
            );
        }
    }

    #[test]
    fn the_product_ring_clears_every_ramp_surface() {
        for dark in [false, true] {
            let theme = product_theme(dark);
            for surface in [
                theme.background,
                theme.muted,
                theme.table_head,
                theme.sidebar,
                theme.status_bar,
                theme.title_bar,
            ] {
                let ratio = contrast_ratio(theme.ring, surface);
                assert!(
                    ratio >= super::RING_CONTRAST_FLOOR,
                    "{dark:?} ring contrast {ratio} on {surface:?} is below 3:1"
                );
            }
        }
    }

    #[test]
    fn the_theme_colors_anchor_ink_and_accent() {
        let light = product_theme(false);
        assert_eq!(rgb_bytes(light.foreground), (0x1a, 0x1a, 0x1a));
        assert_eq!(
            rgb_bytes(light.tokens.button_primary.color),
            (0x1a, 0x1a, 0x1a)
        );
        assert_eq!(rgb_bytes(light.background), (0xf7, 0xf7, 0xf7));

        let dark = product_theme(true);
        assert_eq!(rgb_bytes(dark.foreground), (0xe4, 0xe4, 0xe4));
        assert_eq!(
            rgb_bytes(dark.tokens.button_primary.color),
            (0xe4, 0xe4, 0xe4)
        );
        assert_eq!(rgb_bytes(dark.background), (0x14, 0x14, 0x14));

        for theme in [light, dark] {
            let palette = mode_palette(theme.mode == ThemeMode::Dark);
            assert_eq!(theme.caret, palette.foreground);
            assert_eq!(theme.primary, palette.accent);
            assert_eq!(
                theme.tokens.button_primary_foreground.color,
                palette.solid_label
            );
            assert_eq!(theme.sidebar_foreground, palette.foreground);
            assert_eq!(theme.popover_foreground, palette.foreground);
        }
    }

    #[test]
    fn every_palette_pair_clears_its_floor_without_repair() {
        let worst = |ink: Hsla, surfaces: &[Hsla]| {
            surfaces
                .iter()
                .map(|surface| contrast_ratio(ink, *surface))
                .fold(f32::INFINITY, f32::min)
        };
        for (name, palette) in [("light", LIGHT_PALETTE), ("dark", DARK_PALETTE)] {
            let ModePalette {
                content,
                inset,
                frame,
                interaction,
                selection,
                ..
            } = palette;
            let surfaces = [content, inset, frame];
            let checks = [
                (
                    "foreground",
                    worst(
                        palette.foreground,
                        &[content, inset, frame, interaction, selection],
                    ),
                    INK_CONTRAST_FLOOR,
                ),
                (
                    "muted foreground",
                    worst(
                        palette.muted_foreground,
                        &[content, inset, frame, interaction],
                    ),
                    INK_CONTRAST_FLOOR,
                ),
                ("ring", worst(palette.ring, &surfaces), RING_CONTRAST_FLOOR),
                (
                    "accent label",
                    contrast_ratio(palette.solid_label, palette.accent),
                    INK_CONTRAST_FLOOR,
                ),
                (
                    "secondary label",
                    contrast_ratio(palette.foreground, palette.secondary),
                    INK_CONTRAST_FLOOR,
                ),
            ];
            for (role, ratio, floor) in checks {
                assert!(
                    ratio >= floor,
                    "{name} {role} measured {ratio:.2}, below {floor}"
                );
            }
        }
    }

    #[test]
    fn the_surface_ramp_separates_content_inset_and_frame() {
        let light = product_theme(false);
        assert!(light.background.l > light.muted.l);
        assert!(light.muted.l > light.sidebar.l);
        assert_eq!(light.title_bar.l, light.sidebar.l);
        assert_eq!(light.status_bar.l, light.sidebar.l);
        assert!(light.list_hover.l < light.sidebar.l);

        let dark = product_theme(true);
        assert!(dark.background.l < dark.muted.l);
        assert!(dark.muted.l < dark.sidebar.l);
        assert_eq!(dark.title_bar.l, dark.sidebar.l);
        assert_eq!(dark.status_bar.l, dark.sidebar.l);
        assert!(dark.list_hover.l > dark.sidebar.l);
    }

    #[test]
    fn muted_ink_stays_readable_on_the_interaction_fill() {
        for dark in [false, true] {
            let theme = product_theme(dark);
            for surface in [theme.list_hover, theme.sidebar, theme.title_bar] {
                let ratio = contrast_ratio(theme.muted_foreground, surface);
                assert!(
                    ratio >= INK_CONTRAST_FLOOR,
                    "{dark:?} muted ink contrast {ratio} on {surface:?} is below 4.5:1"
                );
            }
        }
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
            (0xf7, 0xf7, 0xf7)
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
            (0x14, 0x14, 0x14)
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

    #[test]
    fn the_interaction_fill_moves_every_ground_it_lights() {
        for dark in [false, true] {
            let palette = mode_palette(dark);
            let grounds = [
                ("content", palette.content),
                ("inset", palette.inset),
                ("frame", palette.frame),
            ];
            for (name, ground) in grounds {
                let row = lightness_step(palette.interaction, ground);
                assert!(
                    row >= INTERACTION_LIGHTNESS_FLOOR,
                    "{dark:?} row fill on {name} moved {row:.1} L\\*, below \
                     {INTERACTION_LIGHTNESS_FLOOR}"
                );
                let control = lightness_step(
                    control_hover_fill(palette.interaction, ground, dark),
                    ground,
                );
                assert!(
                    control >= INTERACTION_LIGHTNESS_FLOOR,
                    "{dark:?} control hover on {name} composited to {control:.1} L\\*, below \
                     {INTERACTION_LIGHTNESS_FLOOR}"
                );
            }

            // The halved composite is what sets the step. The line tier sits one step below it,
            // and borrowing that step would leave a ghost control's hover under the floor, which is
            // exactly the state this rule replaced.
            let line_as_control = lightness_step(
                control_hover_fill(palette.border, palette.frame, dark),
                palette.frame,
            );
            assert!(
                line_as_control < INTERACTION_LIGHTNESS_FLOOR,
                "the interaction step would be the line step if halving were not the constraint \
                 ({line_as_control:.1} L\\*)"
            );
        }
    }

    /// The component theme file's own grays are not this ramp. Every neutral role a used component
    /// reads has to resolve to a ramp step, or the window renders two inks at once: the dark file's
    /// `secondary.foreground` lit ghost icons at `#FAFAFA` beside a `#E4E4E4` brand, and its
    /// `popover.background` floated menus on `#0A0A0A`, below the page they cover.
    #[test]
    fn no_component_neutral_lives_outside_the_ramp() {
        let ramp = gray::ALL;
        for dark in [false, true] {
            let theme = product_theme(dark);
            let roles = [
                ("foreground", theme.foreground),
                ("muted_foreground", theme.muted_foreground),
                ("secondary", theme.secondary),
                ("secondary_foreground", theme.secondary_foreground),
                ("accent", theme.accent),
                ("accent_foreground", theme.accent_foreground),
                ("button_foreground", theme.button_foreground),
                ("link", theme.link),
                ("link_hover", theme.link_hover),
                ("link_active", theme.link_active),
                ("background", theme.background),
                ("muted", theme.muted),
                ("popover", theme.popover),
                ("popover_foreground", theme.popover_foreground),
                ("progress_bar", theme.progress_bar),
                ("scrollbar", theme.scrollbar),
                ("scrollbar_thumb", theme.scrollbar_thumb),
                ("scrollbar_thumb_hover", theme.scrollbar_thumb_hover),
                ("selection", theme.selection),
                ("caret", theme.caret),
                ("border", theme.border),
                ("input", theme.input),
                ("ring", theme.ring),
                ("sidebar", theme.sidebar),
                ("sidebar_foreground", theme.sidebar_foreground),
                ("sidebar_accent", theme.sidebar_accent),
                ("sidebar_accent_foreground", theme.sidebar_accent_foreground),
                ("title_bar", theme.title_bar),
                ("status_bar", theme.status_bar),
                ("table_head", theme.table_head),
                ("table_head_foreground", theme.table_head_foreground),
                ("list_hover", theme.list_hover),
                ("list_active", theme.list_active),
            ];
            for (name, color) in roles {
                assert!(
                    ramp.iter().any(|step| rgb_bytes(*step) == rgb_bytes(color)),
                    "{name} resolved to {:?}, outside the ramp (dark={dark})",
                    rgb_bytes(color)
                );
            }

            // Components read several of these roles from the legacy tokens rather than the
            // colors, so both sides have to name the same step.
            let token_roles = [
                ("tokens.accent", theme.tokens.accent.color),
                ("tokens.button_hover", theme.tokens.button_hover.color),
                ("tokens.button_active", theme.tokens.button_active.color),
                ("tokens.background", theme.tokens.background.color),
                ("tokens.muted", theme.tokens.muted.color),
                ("tokens.popover", theme.tokens.popover.color),
                ("tokens.status_bar", theme.tokens.status_bar.color),
                ("tokens.secondary", theme.tokens.secondary.color),
                ("tokens.progress_bar", theme.tokens.progress_bar.color),
            ];
            for (name, color) in token_roles {
                assert!(
                    ramp.iter().any(|step| rgb_bytes(*step) == rgb_bytes(color)),
                    "{name} resolved to {:?}, outside the ramp (dark={dark})",
                    rgb_bytes(color)
                );
            }
        }
    }
}
