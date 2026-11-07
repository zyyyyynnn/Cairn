// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Category fills for the disk-space Treemap data surface.
//!
//! The shell is grayscale: chrome, controls, text, hover, selection and state colors are neutral
//! roles resolved from the component theme. This module is the one bounded exception, and it serves
//! the Treemap data plane only. A category fill says what a tile is; it never says what a tile
//! does or whether it is selected. Nothing here belongs in [`crate::shell::theme::ShellTokens`],
//! and no control, hover, selected or warning/danger state may read it.
//!
//! The palette is a muted graphite band: every category keeps one hue in both modes, chroma never
//! exceeds `0.040`, and Archive is the single warm accent. Ten near-neutral hues cannot separate on
//! angle alone, so the categories stagger across two value tiers and every pair of fills clears
//! `6.0` CIE76 units ([`cairn_ui::theme::perceptual_distance`]), the distance at which two adjacent
//! tiles read as two surfaces. A directory header is identified by its strip and its name first;
//! its tint stays `3.0` CIE76 units away from every other category's leaf.
//!
//! Nesting depth does not change color: depth is expressed by the 2px gutter, the header tint and
//! the nested rectangle, so one category is one color at every depth.
//!
//! A directory carries its category tint on the header only; the container body stays on the canvas,
//! so every sibling gutter has the same color at every nesting depth while file leaves remain the
//! strongest signal. A directory that inherited no category signal falls back to the Folder color.
//!
//! The canvas and the primary ink are the mode's own `content` and `foreground` roles, so the data
//! plane cannot drift from the shell. The secondary ink is a step of the same neutral ramp, one
//! step lighter than the shell's `muted_foreground` in dark and one darker in light, so size labels
//! clear AA on every fill.
//!
//! Hue is GPUI's turn fraction in `0.0..=1.0`, not degrees.

use cairn_core::treemap::TreemapCategory;
use cairn_ui::theme::mode_palette;
use gpui_kit::gpui::Hsla;

/// A directory header borrows this share of its category fill, mixed into the canvas. It names the
/// directory while staying below a file leaf; the body stays on the canvas so every gutter has one
/// color at every nesting depth. Light headers sit further from the leaf tier because the pale
/// leaves otherwise crowd the tinted strip.
const DIRECTORY_HEADER_TINT_LIGHT: f32 = 0.62;
const DIRECTORY_HEADER_TINT_DARK: f32 = 0.72;

/// Hover and press step the block's own lightness down, keeping hue and chroma. Each mode carries
/// its own pair because equal HSL lightness deltas produce different contrast steps at opposite
/// ends of the luminance curve. Neither state adds a border; selection keeps its 2px ring.
const TREEMAP_HOVER_STEP_LIGHT: f32 = 0.05;
const TREEMAP_ACTIVE_STEP_LIGHT: f32 = 0.10;
const TREEMAP_HOVER_STEP_DARK: f32 = 0.036;
const TREEMAP_ACTIVE_STEP_DARK: f32 = 0.06;

/// How much of a dimmed block's fill is the category itself, per mode.
const DIMMED_FILL_LIGHT: f32 = 0.35;
const DIMMED_FILL_DARK: f32 = 0.50;

const fn hsla(h: f32, s: f32, l: f32) -> Hsla {
    Hsla { h, s, l, a: 1.0 }
}

/// Mixes `tint` into `base` in sRGB space. Interpolating the channels rather than the hue avoids the
/// detour through unrelated hues that an HSL mix takes while a color is being desaturated.
fn mix(base: Hsla, tint: Hsla, amount: f32) -> Hsla {
    let mut tint = tint;
    tint.a = amount.clamp(0.0, 1.0);
    base.blend(tint)
}

// ---- light: low-saturation graphite band on #F7F7F7 ---------------------------

const LIGHT_FOLDER_FILL: Hsla = hsla(0.6111, 0.2069, 0.7725); // #B9C1D1
const LIGHT_SYSTEM_FILL: Hsla = hsla(0.6288, 0.2973, 0.8549); // #CFD4E5
const LIGHT_APPLICATIONS_FILL: Hsla = hsla(0.5000, 0.2371, 0.8098); // #C3DADA
const LIGHT_DOCUMENT_FILL: Hsla = hsla(0.0917, 0.2083, 0.8118); // #D9D0C5
const LIGHT_ARCHIVE_FILL: Hsla = hsla(0.1126, 0.2868, 0.7471); // #D1C5AC
const LIGHT_IMAGE_FILL: Hsla = hsla(0.3824, 0.1828, 0.8176); // #C8D9CD
const LIGHT_CODE_FILL: Hsla = hsla(0.5595, 0.2745, 0.8000); // #BED0DA
const LIGHT_AUDIO_FILL: Hsla = hsla(0.7812, 0.1600, 0.8039); // #D0C5D5
const LIGHT_VIDEO_FILL: Hsla = hsla(0.9500, 0.2381, 0.8353); // #DFCBD1
const LIGHT_OTHER_FILL: Hsla = hsla(0.7222, 0.0361, 0.8373); // #D5D4D7

// ---- dark: the same OKLCH hues on #141414 -------------------------------------

const DARK_FOLDER_FILL: Hsla = hsla(0.5833, 0.0323, 0.2431); // #3C3E40
const DARK_SYSTEM_FILL: Hsla = hsla(0.6288, 0.1667, 0.2588); // #373C4D
const DARK_APPLICATIONS_FILL: Hsla = hsla(0.5000, 0.1311, 0.2392); // #354545
const DARK_DOCUMENT_FILL: Hsla = hsla(0.0882, 0.1429, 0.2333); // #443C33
const DARK_ARCHIVE_FILL: Hsla = hsla(0.1129, 0.2562, 0.2373); // #4C422D
const DARK_IMAGE_FILL: Hsla = hsla(0.3824, 0.1360, 0.2451); // #36473B
const DARK_CODE_FILL: Hsla = hsla(0.5632, 0.2437, 0.2333); // #2D3F4A
const DARK_AUDIO_FILL: Hsla = hsla(0.7843, 0.1382, 0.2412); // #413546
const DARK_VIDEO_FILL: Hsla = hsla(0.9474, 0.1545, 0.2412); // #47343A
const DARK_OTHER_FILL: Hsla = hsla(0.7333, 0.0820, 0.2392); // #3C3842

/// Secondary ink for sizes: `#3A3A3A` in light, `#CECECE` in dark, both steps of the neutral ramp.
const LIGHT_INK_SECONDARY: Hsla = hsla(0.0, 0.0, 0x3A as f32 / 255.0);
const DARK_INK_SECONDARY: Hsla = hsla(0.0, 0.0, 0xCE as f32 / 255.0);

/// Category fills and data-plane ink for one theme mode.
///
/// Copy because a render pass carries it by value, and every value it returns is a constant, a
/// shell role, or one constant with its lightness moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreemapPalette {
    is_dark: bool,
}

impl TreemapPalette {
    pub const fn for_mode(is_dark: bool) -> Self {
        Self { is_dark }
    }

    pub const fn is_dark(&self) -> bool {
        self.is_dark
    }

    /// Fill for a category. Depth does not change it: one category is one color at every level.
    pub fn fill(&self, category: TreemapCategory) -> Hsla {
        if self.is_dark {
            dark_fill(category)
        } else {
            light_fill(category)
        }
    }

    /// Fill for a categorized directory's header strip, and for a compact directory that has no
    /// separate header. The container body itself uses the canvas, so sibling gaps never take on the
    /// parent's tint.
    pub fn directory_header(&self, category: TreemapCategory) -> Hsla {
        let tint = if self.is_dark {
            DIRECTORY_HEADER_TINT_DARK
        } else {
            DIRECTORY_HEADER_TINT_LIGHT
        };
        mix(self.canvas(), self.fill(category), tint)
    }

    /// The 2px selection ring for a category. It keeps the category hue but moves value away from
    /// the fill, so selection reads as an outline rather than another fill. Structural categories
    /// stay quieter than the chromatic ones; every ring clears the 3:1 non-text floor against its
    /// fill and against the canvas.
    pub fn selection(&self, category: TreemapCategory) -> Hsla {
        let structural = matches!(category, TreemapCategory::Folder | TreemapCategory::Other);
        Hsla {
            h: self.fill(category).h,
            s: if structural { 0.16 } else { 0.42 },
            l: if self.is_dark { 0.75 } else { 0.28 },
            a: 1.0,
        }
    }

    /// A fill with a filtered-out category's weight: what the fill itself would paint at
    /// [`Self::dimmed_share`] over the canvas. Dimming a block this way keeps the category hue
    /// readable while its name and size stay fully opaque, so a filter never costs legibility
    /// (WCAG 1.4.3).
    pub fn dimmed_fill(&self, fill: Hsla) -> Hsla {
        mix(self.canvas(), fill, self.dimmed_share())
    }

    /// How much of a dimmed block's fill is the category itself. Dark fills sit close to the canvas,
    /// so they keep a larger share or a filtered tile would read as gutter.
    pub const fn dimmed_share(&self) -> f32 {
        if self.is_dark {
            DIMMED_FILL_DARK
        } else {
            DIMMED_FILL_LIGHT
        }
    }

    /// The canvas the fills sit on: the shell's page role, so a gutter, an empty panel and a tile
    /// body are one color and the data plane cannot drift from the theme.
    pub fn canvas(&self) -> Hsla {
        mode_palette(self.is_dark).content
    }

    /// Primary ink for names on a category fill: the shell's own foreground role.
    pub fn ink(&self) -> Hsla {
        mode_palette(self.is_dark).foreground
    }

    /// Secondary ink for sizes on a category fill. Also the color of the category icon, which is a
    /// non-text indicator and clears 3:1 comfortably.
    pub fn ink_secondary(&self) -> Hsla {
        if self.is_dark {
            DARK_INK_SECONDARY
        } else {
            LIGHT_INK_SECONDARY
        }
    }
}

fn light_fill(category: TreemapCategory) -> Hsla {
    match category {
        TreemapCategory::Folder => LIGHT_FOLDER_FILL,
        TreemapCategory::System => LIGHT_SYSTEM_FILL,
        TreemapCategory::Applications => LIGHT_APPLICATIONS_FILL,
        TreemapCategory::Document => LIGHT_DOCUMENT_FILL,
        TreemapCategory::Archive => LIGHT_ARCHIVE_FILL,
        TreemapCategory::Image => LIGHT_IMAGE_FILL,
        TreemapCategory::Code => LIGHT_CODE_FILL,
        TreemapCategory::Audio => LIGHT_AUDIO_FILL,
        TreemapCategory::Video => LIGHT_VIDEO_FILL,
        TreemapCategory::Other => LIGHT_OTHER_FILL,
    }
}

fn dark_fill(category: TreemapCategory) -> Hsla {
    match category {
        TreemapCategory::Folder => DARK_FOLDER_FILL,
        TreemapCategory::System => DARK_SYSTEM_FILL,
        TreemapCategory::Applications => DARK_APPLICATIONS_FILL,
        TreemapCategory::Document => DARK_DOCUMENT_FILL,
        TreemapCategory::Archive => DARK_ARCHIVE_FILL,
        TreemapCategory::Image => DARK_IMAGE_FILL,
        TreemapCategory::Code => DARK_CODE_FILL,
        TreemapCategory::Audio => DARK_AUDIO_FILL,
        TreemapCategory::Video => DARK_VIDEO_FILL,
        TreemapCategory::Other => DARK_OTHER_FILL,
    }
}

/// Hover and press surfaces for an interactive Treemap block. Both keep the base surface's category
/// hue and chroma while stepping lightness down.
pub(super) fn block_fill_states(base: Hsla, is_dark: bool) -> (Hsla, Hsla) {
    let (hover_step, pressed_step) = if is_dark {
        (TREEMAP_HOVER_STEP_DARK, TREEMAP_ACTIVE_STEP_DARK)
    } else {
        (TREEMAP_HOVER_STEP_LIGHT, TREEMAP_ACTIVE_STEP_LIGHT)
    };
    (
        Hsla {
            l: (base.l - hover_step).clamp(0.0, 1.0),
            ..base
        },
        Hsla {
            l: (base.l - pressed_step).clamp(0.0, 1.0),
            ..base
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_ui::theme::{contrast_ratio, is_neutral_ramp_step, oklch, perceptual_distance};

    const CATEGORIES: [TreemapCategory; 10] = [
        TreemapCategory::Folder,
        TreemapCategory::System,
        TreemapCategory::Applications,
        TreemapCategory::Document,
        TreemapCategory::Archive,
        TreemapCategory::Image,
        TreemapCategory::Code,
        TreemapCategory::Audio,
        TreemapCategory::Video,
        TreemapCategory::Other,
    ];
    /// The two state colors the shell renders inside this view, as OKLCH hue angles: the
    /// component palette's `danger` red and `warning` yellow.
    const STATE_HUES: [(&str, f32); 2] = [("danger red-500", 20.6), ("warning yellow-400", 101.0)];
    /// Two adjacent tiles read as two surfaces from this distance on; below it the graphite band
    /// collapses into one gray field. Dark carries a lower floor because its fills must also keep
    /// headroom for the press step above the canvas.
    const CATEGORY_DISTANCE_FLOOR_LIGHT: f32 = 6.0;
    const CATEGORY_DISTANCE_FLOOR_DARK: f32 = 5.5;
    /// A header strip is named by its label and position; its tint only has to stay clear of every
    /// other category's leaf by this much.
    const HEADER_DISTANCE_FLOOR: f32 = 3.0;

    fn category_floor(is_dark: bool) -> f32 {
        if is_dark {
            CATEGORY_DISTANCE_FLOOR_DARK
        } else {
            CATEGORY_DISTANCE_FLOOR_LIGHT
        }
    }

    /// Data-plane text stays AAA on every category: the fill is the background the label actually
    /// sits on, so the shell surfaces do not apply.
    #[test]
    fn ink_clears_aaa_on_every_category_fill() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                let ratio = contrast_ratio(palette.ink(), palette.fill(category));
                assert!(
                    ratio >= 7.0,
                    "ink on {category:?} (dark={is_dark}) measured {ratio:.2}:1"
                );
            }
        }
    }

    /// A filtered-out category recedes by moving its fill toward the canvas, never by fading the
    /// label: the same inks still clear their floors on the dimmed fill, and the dimmed fill still
    /// reads as a tile rather than as gutter.
    #[test]
    fn dimmed_fills_keep_both_inks_readable() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                let dimmed = palette.dimmed_fill(palette.fill(category));
                let name = contrast_ratio(palette.ink(), dimmed);
                let size = contrast_ratio(palette.ink_secondary(), dimmed);
                let recedes = contrast_ratio(dimmed, palette.canvas());
                assert!(
                    name >= 7.0,
                    "name ink on a dimmed {category:?} (dark={is_dark}) measured {name:.2}:1"
                );
                assert!(
                    size >= 4.5,
                    "size ink on a dimmed {category:?} (dark={is_dark}) measured {size:.2}:1"
                );
                assert!(
                    recedes >= 1.10,
                    "a dimmed {category:?} (dark={is_dark}) measured {recedes:.3}:1 against the canvas"
                );
            }
        }
    }

    /// Sizes may sit one step below the name, but only down to AA.
    #[test]
    fn secondary_ink_clears_aa_on_every_category_fill() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                let ratio = contrast_ratio(palette.ink_secondary(), palette.fill(category));
                assert!(
                    ratio >= 4.5,
                    "secondary ink on {category:?} (dark={is_dark}) measured {ratio:.2}:1"
                );
            }
        }
    }

    /// The data plane's inks are steps of the neutral ramp, and the reason the size ink is not the
    /// shell's muted ink is measured here instead of asserted in prose.
    #[test]
    fn data_plane_inks_are_ramp_steps_and_the_shell_muted_ink_would_fail() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            assert!(
                is_neutral_ramp_step(palette.ink_secondary()),
                "secondary ink (dark={is_dark}) sits between two steps of the neutral ramp"
            );
            assert!(is_neutral_ramp_step(palette.ink()));
            assert!(is_neutral_ramp_step(palette.canvas()));
            let shell_muted = mode_palette(is_dark).muted_foreground;
            let worst = CATEGORIES
                .iter()
                .map(|category| contrast_ratio(shell_muted, palette.fill(*category)))
                .fold(f32::INFINITY, f32::min);
            assert!(
                worst < 4.5,
                "the shell's muted ink measures {worst:.2}:1 on these fills, so the divergence is \
                 documented for nothing"
            );
        }
    }

    /// A fill has to be visible on the surface it covers: gutters show the canvas directly, so this
    /// rejects a tile that disappears into it.
    #[test]
    fn fills_are_distinguishable_from_the_canvas() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                let ratio = contrast_ratio(palette.fill(category), palette.canvas());
                assert!(
                    ratio >= 1.30,
                    "{category:?} (dark={is_dark}) measured {ratio:.3}:1 against the canvas"
                );
            }
        }
    }

    /// Ten muted categories may not collapse into one gray field. A contrast ratio between two mid
    /// tones says nothing about them; the perceptual distance is what a person comparing two
    /// adjacent tiles reads.
    #[test]
    fn categories_are_told_apart_by_color() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            let mut closest = (f32::INFINITY, String::new());
            for (i, left) in CATEGORIES.iter().enumerate() {
                for right in CATEGORIES[i + 1..].iter() {
                    let distance = perceptual_distance(palette.fill(*left), palette.fill(*right));
                    assert_ne!(
                        palette.fill(*left),
                        palette.fill(*right),
                        "{left:?} and {right:?} share a fill (dark={is_dark})"
                    );
                    if distance < closest.0 {
                        closest = (distance, format!("{left:?} and {right:?}"));
                    }
                }
            }
            assert!(
                closest.0 >= category_floor(is_dark),
                "the closest pair, {} (dark={is_dark}), measured {:.1} CIE units apart",
                closest.1,
                closest.0
            );
        }
    }

    /// A header tint is the quieter version of its own category, but it may not read as a different
    /// category's file leaf.
    #[test]
    fn headers_do_not_read_as_another_category() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for left in CATEGORIES {
                let header = palette.directory_header(left);
                for right in CATEGORIES {
                    if left == right {
                        continue;
                    }
                    let distance = perceptual_distance(header, palette.fill(right));
                    assert!(
                        distance >= HEADER_DISTANCE_FLOOR,
                        "{left:?} header reads as {right:?} (dark={is_dark}): {distance:.1} CIE units"
                    );
                }
            }
        }
    }

    /// A hue that carries enough chroma to read as a color may not double as a state hue. Near
    /// neutral fills cannot read as red or yellow, so the rule applies from this chroma up.
    #[test]
    fn category_hues_stay_clear_of_the_state_colors() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                let (chroma, hue) = (
                    oklch(palette.fill(category)).1,
                    oklch(palette.fill(category)).2,
                );
                if chroma < 0.03 {
                    continue;
                }
                for (name, state_hue) in STATE_HUES {
                    let delta = (hue - state_hue).abs();
                    assert!(
                        delta.min(360.0 - delta) >= 15.0,
                        "{category:?} sits {delta:.1} degrees from {name} (dark={is_dark}); a \
                         category color may not double as a state color"
                    );
                }
            }
        }
    }

    /// The band stays muted and Archive remains the single warm accent: no category may exceed the
    /// chroma ceiling, and only Archive may sit at the top of it.
    #[test]
    fn chroma_stays_in_a_muted_band_with_one_accent() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            let mut loudest = (0.0_f32, String::new());
            for category in CATEGORIES {
                let chroma = oklch(palette.fill(category)).1;
                assert!(
                    chroma <= 0.040,
                    "{category:?} chroma {chroma:.3} leaves the muted band (dark={is_dark})"
                );
                if chroma > loudest.0 {
                    loudest = (chroma, format!("{category:?}"));
                }
            }
            assert_eq!(
                loudest.1, "Archive",
                "the warm accent is the loudest category (dark={is_dark}), not {}",
                loudest.1
            );
        }
    }

    /// A category keeps its declared hue across modes within a narrow arc, so the legend reads as
    /// one mapping. The rendered OKLCH angle of a near-neutral fill moves with channel rounding
    /// faster than the eye can use, so the declaration is what holds still.
    #[test]
    fn categories_keep_their_hue_arc_across_modes() {
        let light = TreemapPalette::for_mode(false);
        let dark = TreemapPalette::for_mode(true);
        for category in CATEGORIES {
            let delta = (light.fill(category).h - dark.fill(category).h).abs();
            let turn = delta.min(1.0 - delta);
            assert!(
                turn <= 20.0 / 360.0,
                "{category:?} changed declared hue between modes by {:.1} degrees",
                turn * 360.0
            );
        }
    }

    /// Folder and Other are the hardest pair to keep apart, because both are near-neutral. They
    /// separate on value as well as hue.
    #[test]
    fn folder_and_other_stay_visually_separate() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            let distance = perceptual_distance(
                palette.fill(TreemapCategory::Folder),
                palette.fill(TreemapCategory::Other),
            );
            assert!(
                distance >= category_floor(is_dark),
                "Folder and Other (dark={is_dark}) measured {distance:.1} CIE units"
            );
        }
    }

    /// Selection is a structural outline, but its hue stays attached to the category so a selected
    /// tile remains identifiable. The ring must clear the non-text floor against both the tile it
    /// outlines and the canvas.
    #[test]
    fn selection_rings_are_category_coded_and_legible() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                let ring = palette.selection(category);
                let ring_hue = oklch(ring).2;
                let fill_hue = oklch(palette.fill(category)).2;
                let hue_delta = (ring_hue - fill_hue).abs();
                assert!(
                    hue_delta.min(360.0 - hue_delta) <= 10.0,
                    "{category:?} selection ring sits at {ring_hue:.1} degrees, its fill at \
                     {fill_hue:.1}. The ring keeps the fill's HSL hue, and moving saturation and \
                     lightness in HSL shifts the OKLCH angle slightly."
                );
                if matches!(category, TreemapCategory::Folder | TreemapCategory::Other) {
                    assert!(
                        oklch(ring).1 <= 0.06,
                        "{category:?} is a structural category, so its ring stays near-neutral"
                    );
                }
                let ratio = contrast_ratio(ring, palette.fill(category));
                assert!(
                    ratio >= 3.0,
                    "selection on {category:?} (dark={is_dark}) measured {ratio:.2}:1 against its fill"
                );
                let canvas_ratio = contrast_ratio(ring, palette.canvas());
                assert!(
                    canvas_ratio >= 3.0,
                    "selection on {category:?} (dark={is_dark}) measured {canvas_ratio:.2}:1 against the canvas"
                );
            }
        }
    }

    /// Directory headers mix the category into the canvas, so their labels sit on a different
    /// surface than a file leaf. They still have to clear AAA, and their size text AA, and stay
    /// quieter than the leaf.
    #[test]
    fn directory_headers_are_quieter_and_keep_text_readable() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                let header = palette.directory_header(category);
                let ink_ratio = contrast_ratio(palette.ink(), header);
                let secondary_ratio = contrast_ratio(palette.ink_secondary(), header);
                assert!(
                    ink_ratio >= 7.0,
                    "directory ink on {category:?} (dark={is_dark}) measured {ink_ratio:.2}:1"
                );
                assert!(
                    secondary_ratio >= 4.5,
                    "directory secondary ink on {category:?} (dark={is_dark}) measured {secondary_ratio:.2}:1"
                );
                let fill_ratio = contrast_ratio(palette.fill(category), palette.canvas());
                let header_ratio = contrast_ratio(header, palette.canvas());
                assert!(
                    header_ratio < fill_ratio,
                    "{category:?} header (dark={is_dark}) is not quieter than its file fill"
                );
                assert!(
                    header_ratio >= 1.10,
                    "{category:?} header (dark={is_dark}) measured {header_ratio:.3}:1 against the canvas"
                );
            }
        }
    }

    /// Every interactive surface — file fill, compact leaf and header tint — moves far enough to
    /// read as a state, stays visible against the canvas, and keeps both inks readable. Perceptual
    /// distance and canvas visibility are the floors here: a luminance ratio alone calls a
    /// near-black hover acceptable.
    #[test]
    fn every_interactive_surface_has_a_measured_hover_step() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                let surfaces = [palette.fill(category), palette.directory_header(category)];
                for base in surfaces {
                    let (hover, active) = block_fill_states(base, is_dark);
                    assert_eq!(hover.h, base.h, "{category:?} hover changed hue");
                    assert_eq!(hover.s, base.s, "{category:?} hover changed chroma");

                    let hover_distance = perceptual_distance(base, hover);
                    assert!(
                        hover_distance >= 4.0,
                        "{category:?} (dark={is_dark}) hover measured {hover_distance:.1} CIE units"
                    );
                    let active_distance = perceptual_distance(base, active);
                    assert!(
                        active_distance >= 6.0,
                        "{category:?} (dark={is_dark}) press measured {active_distance:.1} CIE units"
                    );
                    for state in [hover, active] {
                        let canvas_ratio = contrast_ratio(state, palette.canvas());
                        assert!(
                            canvas_ratio >= 1.10,
                            "{category:?} (dark={is_dark}) state measured {canvas_ratio:.3}:1 against the canvas"
                        );
                        assert!(
                            contrast_ratio(palette.ink(), state) >= 7.0,
                            "{category:?} state broke AAA ink (dark={is_dark})"
                        );
                        assert!(
                            contrast_ratio(palette.ink_secondary(), state) >= 4.5,
                            "{category:?} state broke AA secondary ink (dark={is_dark})"
                        );
                    }
                }
            }
        }
    }

    /// GPUI hue is a turn fraction. This test exists because an angle such as `210.0` once slipped
    /// into the render path and painted the wrong color.
    #[test]
    fn hues_stay_inside_the_turn_fraction_domain() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                for color in [palette.fill(category), palette.directory_header(category)] {
                    assert!(
                        (0.0..=1.0).contains(&color.h)
                            && (0.0..=1.0).contains(&color.s)
                            && (0.0..=1.0).contains(&color.l),
                        "{category:?} has a channel outside 0.0..=1.0 (dark={is_dark})"
                    );
                }
            }
        }
    }
}
