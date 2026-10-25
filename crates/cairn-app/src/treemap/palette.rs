// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Category fills for the disk-space Treemap data surface.
//!
//! The shell is grayscale: chrome, controls, text, hover, selection and state
//! colors are neutral roles resolved from the component theme. This module is the
//! one bounded exception, and it serves the Treemap data plane only. A category
//! fill says *what a tile is*; it never says *what a tile does* or *whether it is
//! selected*. Nothing here belongs in [`crate::shell::theme::ShellTokens`], and no
//! control, hover, selected or warning/danger state may read it.
//!
//! The graphite palette keeps one hue per category inside a quiet band. Archive
//! alone carries warmth and extra chroma, so it reads as the one accent without
//! competing with the shell's warning/danger colors. Folder and Other are the
//! structural neutrals: Folder is the darker cool slate, Other the lightest
//! lilac-gray, and the two stay clearly apart by value as well as hue. Nesting
//! depth moves lightness away from the canvas by one fixed step and never
//! changes hue or saturation. A directory carries its category tint on the
//! header only; the container body stays on the canvas, so every sibling gutter
//! has the same color at every nesting depth while file leaves remain the
//! strongest signal. A directory that inherited no category signal falls back
//! to the Folder color. Ink ([`TreemapPalette::ink`] and
//! [`TreemapPalette::ink_secondary`]) is grayscale and verified against every
//! fill and every directory surface at every depth.
//!
//! Hue is GPUI's turn fraction in `0.0..=1.0`, not degrees. `h: 210.0` is not a
//! blue; it leaves the valid domain and wraps to an unrelated color.

use cairn_core::treemap::TreemapCategory;
use gpui_kit::gpui::Hsla;

/// Deepest nesting step the fill ramp moves; deeper tiles reuse the last step so
/// a fill never drifts toward black or white.
const MAX_DEPTH_STEPS: usize = 3;

/// How far one nesting level moves the fill away from the canvas.
const DEPTH_STEP: f32 = 0.012;

/// A directory header borrows this share of its category fill, mixed into the
/// canvas. It names the directory while staying below a file leaf; the body
/// stays on the canvas so every gutter has one color at every nesting depth.
const DIRECTORY_HEADER_TINT: f32 = 0.72;

/// Hover and press darken the block's own surface by a fixed lightness step.
/// A fixed neutral blend was measured to fail on header tints: a header starts
/// already close to `row_hover`, so light headers moved only 1.00–1.03:1 while
/// file leaves moved 1.01–1.06:1. Keeping hue and saturation and moving
/// lightness gives every interactive surface the same measured step. Light and
/// dark use different steps because the same `L` delta buys less contrast near
/// white. Neither state adds a border; selection keeps its 2px `ring`.
const TREEMAP_HOVER_STEP_LIGHT: f32 = 0.035;
const TREEMAP_ACTIVE_STEP_LIGHT: f32 = 0.070;
const TREEMAP_HOVER_STEP_DARK: f32 = 0.025;
const TREEMAP_ACTIVE_STEP_DARK: f32 = 0.052;

const fn hsla(h: f32, s: f32, l: f32) -> Hsla {
    Hsla { h, s, l, a: 1.0 }
}

/// Mixes `tint` into `base` in sRGB space. Interpolating the channels rather
/// than the hue avoids the detour through unrelated hues that an HSL mix takes
/// while a saturated color is being desaturated.
fn mix(base: Hsla, tint: Hsla, amount: f32) -> Hsla {
    let mut tint = tint;
    tint.a = amount.clamp(0.0, 1.0);
    base.blend(tint)
}

// ---- light: canvas #F9F9F9 ----------------------------------------------------

const LIGHT_FOLDER_FILL: Hsla = hsla(0.5972, 0.1091, 0.7843); // #C2C7CE
const LIGHT_SYSTEM_FILL: Hsla = hsla(0.6296, 0.0989, 0.8216); // #CDCFD6
const LIGHT_APPLICATIONS_FILL: Hsla = hsla(0.5000, 0.1158, 0.8137); // #CAD5D5
const LIGHT_DOCUMENT_FILL: Hsla = hsla(0.0897, 0.1368, 0.8137); // #D6D0C9
const LIGHT_ARCHIVE_FILL: Hsla = hsla(0.1111, 0.3971, 0.7333); // #D6C4A0
const LIGHT_IMAGE_FILL: Hsla = hsla(0.3788, 0.1209, 0.8216); // #CCD7CF
const LIGHT_CODE_FILL: Hsla = hsla(0.5625, 0.1600, 0.8039); // #C5CFD5
const LIGHT_AUDIO_FILL: Hsla = hsla(0.7727, 0.1209, 0.8216); // #D3CCD7
const LIGHT_VIDEO_FILL: Hsla = hsla(0.9242, 0.1209, 0.8216); // #D7CCD1
const LIGHT_OTHER_FILL: Hsla = hsla(0.7500, 0.0600, 0.8800); // #E0DFE2

// ---- dark: canvas #101010 -----------------------------------------------------

const DARK_FOLDER_FILL: Hsla = hsla(0.5909, 0.0803, 0.2486); // #3A3F44
const DARK_SYSTEM_FILL: Hsla = hsla(0.6250, 0.1212, 0.2388); // #363944
const DARK_APPLICATIONS_FILL: Hsla = hsla(0.5000, 0.1385, 0.2349); // #344444
const DARK_DOCUMENT_FILL: Hsla = hsla(0.0833, 0.1538, 0.2349); // #453C33
const DARK_ARCHIVE_FILL: Hsla = hsla(0.1100, 0.3623, 0.2300); // #504125
const DARK_IMAGE_FILL: Hsla = hsla(0.3860, 0.1429, 0.2408); // #35463A
const DARK_CODE_FILL: Hsla = hsla(0.5694, 0.1846, 0.2349); // #313E47
const DARK_AUDIO_FILL: Hsla = hsla(0.7719, 0.1429, 0.2408); // #403546
const DARK_VIDEO_FILL: Hsla = hsla(0.9211, 0.1429, 0.2408); // #46353D
const DARK_OTHER_FILL: Hsla = hsla(0.7167, 0.0943, 0.1878); // #2E2B34

// ---- ink and canvas -----------------------------------------------------------

/// The shell foreground the palette pins as data-plane ink: `#101010` in light,
/// `#F0F0F0` in dark. Every category fill clears AAA against it; the dark fills
/// sit two steps deeper and the ink stops short of white, so dense tile labels
/// do not glare on near-black fills.
const LIGHT_INK: Hsla = hsla(0.0, 0.0, 0.0627); // #101010
const DARK_INK: Hsla = hsla(0.0, 0.0, 0.9412); // #F0F0F0

/// Secondary ink for sizes: `#3A3A3A` in light, `#D0D0D0` in dark. The shell's
/// `muted_foreground` role is deliberately not used here — it does not reach AA
/// on most low-saturation fills.
const LIGHT_INK_SECONDARY: Hsla = hsla(0.0, 0.0, 0.2275); // #3A3A3A
const DARK_INK_SECONDARY: Hsla = hsla(0.0, 0.0, 0.8157); // #D0D0D0

/// The canvas each mode's fills sit on, i.e. the shell `background` role. Used to
/// mix the quieter directory surfaces and to verify that a fill is at least
/// distinguishable from the surface it covers.
const LIGHT_BACKGROUND: Hsla = hsla(0.0, 0.0, 0.9765); // #F9F9F9
const DARK_BACKGROUND: Hsla = hsla(0.0, 0.0, 0.0627); // #101010

/// Category fills and data-plane ink for one theme mode.
///
/// Copy because a render pass carries it by value, and every value it returns is
/// a constant or one constant with its lightness moved.
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

    /// Fill for a category at a nesting depth. Depth moves lightness only, by at
    /// most three steps, and never changes hue or saturation.
    pub fn fill(&self, category: TreemapCategory, depth: usize) -> Hsla {
        let base = if self.is_dark {
            dark_fill(category)
        } else {
            light_fill(category)
        };
        let shift = DEPTH_STEP * depth.min(MAX_DEPTH_STEPS) as f32;
        Hsla {
            l: if self.is_dark {
                base.l + shift
            } else {
                base.l - shift
            },
            ..base
        }
    }

    /// Fill for a categorized directory's header strip, and for a compact
    /// directory that has no separate header. The container body itself uses
    /// the canvas, so sibling gaps never take on the parent's tint.
    pub fn directory_header(&self, category: TreemapCategory, depth: usize) -> Hsla {
        self.directory_surface(category, depth, DIRECTORY_HEADER_TINT)
    }

    /// The 2px selection ring for a category. It keeps the category hue but
    /// moves value away from the fill so selection reads as an outline rather
    /// than another fill. Structural categories stay quieter than the chromatic
    /// ones; every ring clears the 3:1 non-text floor against its fill and the
    /// canvas.
    pub fn selection(&self, category: TreemapCategory) -> Hsla {
        let structural = matches!(category, TreemapCategory::Folder | TreemapCategory::Other);
        Hsla {
            h: self.fill(category, 0).h,
            s: if structural { 0.18 } else { 0.45 },
            l: if self.is_dark { 0.72 } else { 0.32 },
            a: 1.0,
        }
    }

    fn directory_surface(&self, category: TreemapCategory, depth: usize, amount: f32) -> Hsla {
        mix(self.canvas(), self.fill(category, depth), amount)
    }

    fn canvas(&self) -> Hsla {
        if self.is_dark {
            DARK_BACKGROUND
        } else {
            LIGHT_BACKGROUND
        }
    }

    /// Primary ink for names on a category fill.
    pub fn ink(&self) -> Hsla {
        if self.is_dark {
            DARK_INK
        } else {
            LIGHT_INK
        }
    }

    /// Secondary ink for sizes on a category fill. Also the color of the category
    /// icon, which is a non-text indicator and clears 3:1 comfortably.
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

/// Hover and press surfaces for an interactive Treemap block. Both keep the
/// base surface's category hue and saturation while stepping lightness down.
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
    use crate::shell::theme::contrast_ratio;

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
    const CHROMATIC: [TreemapCategory; 8] = [
        TreemapCategory::System,
        TreemapCategory::Applications,
        TreemapCategory::Document,
        TreemapCategory::Archive,
        TreemapCategory::Image,
        TreemapCategory::Code,
        TreemapCategory::Audio,
        TreemapCategory::Video,
    ];
    const DEPTHS: [usize; 4] = [0, 1, 2, 3];

    fn background(is_dark: bool) -> Hsla {
        if is_dark {
            DARK_BACKGROUND
        } else {
            LIGHT_BACKGROUND
        }
    }

    /// Data-plane text must stay AAA on every category at every depth: the fill is
    /// the background the label sits on, so the usual shell surfaces do not apply.
    #[test]
    fn ink_clears_aaa_on_every_category_fill() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                for depth in DEPTHS {
                    let fill = palette.fill(category, depth);
                    let ratio = contrast_ratio(palette.ink(), fill);
                    assert!(
                        ratio >= 7.0,
                        "ink on {category:?} at depth {depth} (dark={is_dark}) measured {ratio:.2}:1"
                    );
                }
            }
        }
    }

    /// Sizes may sit one step below the name, but only down to AA.
    #[test]
    fn secondary_ink_clears_aa_on_every_category_fill() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                for depth in DEPTHS {
                    let fill = palette.fill(category, depth);
                    let ratio = contrast_ratio(palette.ink_secondary(), fill);
                    assert!(
                        ratio >= 4.5,
                        "secondary ink on {category:?} at depth {depth} (dark={is_dark}) measured {ratio:.2}:1"
                    );
                }
            }
        }
    }

    /// A low-saturation fill still has to be visible against the canvas; gutters
    /// show the canvas directly, so this rejects a fill that disappears into it.
    #[test]
    fn fills_are_distinguishable_from_the_canvas() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                for depth in DEPTHS {
                    let fill = palette.fill(category, depth);
                    let ratio = contrast_ratio(fill, background(is_dark));
                    assert!(
                        ratio >= 1.10,
                        "{category:?} at depth {depth} (dark={is_dark}) measured {ratio:.3}:1 against the canvas"
                    );
                }
            }
        }
    }

    /// Selection is a structural outline, but its hue stays attached to the
    /// category so a selected tile remains identifiable. The ring must clear
    /// the non-text floor against both the tile it outlines and the canvas.
    #[test]
    fn selection_rings_are_category_coded_and_legible() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                let ring = palette.selection(category);
                assert_eq!(ring.h, palette.fill(category, 0).h);
                if matches!(category, TreemapCategory::Folder | TreemapCategory::Other) {
                    assert!(ring.s <= 0.2, "{category:?} selection is too loud");
                } else {
                    assert!(
                        ring.s >= 0.4,
                        "{category:?} selection lost its category hue"
                    );
                }
                for depth in DEPTHS {
                    let fill = palette.fill(category, depth);
                    let ratio = contrast_ratio(ring, fill);
                    assert!(
                        ratio >= 3.0,
                        "selection on {category:?} at depth {depth} (dark={is_dark}) measured {ratio:.2}:1 against its fill"
                    );
                }
                let canvas_ratio = contrast_ratio(ring, background(is_dark));
                assert!(
                    canvas_ratio >= 3.0,
                    "selection on {category:?} (dark={is_dark}) measured {canvas_ratio:.2}:1 against the canvas"
                );
            }
        }
    }

    /// Directory headers mix the category into the canvas, so their labels sit
    /// on a different surface than a file leaf. They still have to clear AAA,
    /// and their size text AA.
    #[test]
    fn directory_headers_keep_data_plane_text_readable() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                for depth in DEPTHS {
                    let surface = palette.directory_header(category, depth);
                    let ink_ratio = contrast_ratio(palette.ink(), surface);
                    assert!(
                        ink_ratio >= 7.0,
                        "directory ink on {category:?} at depth {depth} (dark={is_dark}) measured {ink_ratio:.2}:1"
                    );
                    let secondary_ratio = contrast_ratio(palette.ink_secondary(), surface);
                    assert!(
                        secondary_ratio >= 4.5,
                        "directory secondary ink on {category:?} at depth {depth} (dark={is_dark}) measured {secondary_ratio:.2}:1"
                    );
                    assert!(
                        (0.0..=1.0).contains(&surface.h),
                        "{category:?} directory hue {} is outside the turn fraction",
                        surface.h
                    );
                }
            }
        }
    }

    /// A directory header borrows less of its category than a file leaf, so
    /// the fill remains the strongest category signal while the header still
    /// reads as its category.
    #[test]
    fn directory_headers_are_quieter_than_file_fills() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                for depth in DEPTHS {
                    let canvas = background(is_dark);
                    let fill_ratio = contrast_ratio(palette.fill(category, depth), canvas);
                    let header_ratio =
                        contrast_ratio(palette.directory_header(category, depth), canvas);
                    assert!(
                        header_ratio < fill_ratio,
                        "{category:?} header at depth {depth} (dark={is_dark}) is not quieter than its file fill"
                    );
                    assert!(
                        header_ratio >= 1.08,
                        "{category:?} header at depth {depth} (dark={is_dark}) measured {header_ratio:.3}:1 against the canvas"
                    );
                }
            }
        }
    }

    /// The Treemap has no neutral category tiles: even the structural Folder and
    /// Other neutrals carry enough tint to read as data rather than chrome.
    #[test]
    fn every_category_fill_is_tinted() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                let fill = palette.fill(category, 0);
                let floor = match category {
                    TreemapCategory::Folder | TreemapCategory::Other => 0.05,
                    _ => 0.09,
                };
                assert!(
                    fill.s >= floor,
                    "{category:?} fill is neutral gray (dark={is_dark})"
                );
            }
        }
    }

    /// Category color stays a quiet encoding; Archive alone carries more chroma,
    /// but no fill may become a saturated block.
    #[test]
    fn fills_stay_on_a_muted_saturation_scale() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                let fill = palette.fill(category, 0);
                assert!(
                    fill.s <= 0.50,
                    "{category:?} fill saturation {} is too strong (dark={is_dark})",
                    fill.s
                );
            }
        }
    }

    /// The graphite categories stay in one quiet band; Archive is the single
    /// warm accent, so it is the only fill allowed extra chroma. Folder and
    /// Other sit on their own structural bases.
    #[test]
    fn graphite_band_keeps_one_amber_accent() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            let (min_l, max_l) = if is_dark {
                (0.225, 0.245)
            } else {
                (0.800, 0.830)
            };
            for category in CHROMATIC {
                let fill = palette.fill(category, 0);
                if category == TreemapCategory::Archive {
                    assert!(
                        (0.34..=0.41).contains(&fill.s),
                        "Archive chroma {} left its accent band (dark={is_dark})",
                        fill.s
                    );
                    let (archive_min_l, archive_max_l) = if is_dark {
                        (0.220, 0.240)
                    } else {
                        (0.720, 0.750)
                    };
                    assert!(
                        (archive_min_l..=archive_max_l).contains(&fill.l),
                        "Archive lightness {} left its tonal band (dark={is_dark})",
                        fill.l
                    );
                } else {
                    assert!(
                        (0.09..=0.21).contains(&fill.s),
                        "{category:?} chroma {} left the graphite band (dark={is_dark})",
                        fill.s
                    );
                    assert!(
                        (min_l..=max_l).contains(&fill.l),
                        "{category:?} lightness {} left the graphite band (dark={is_dark})",
                        fill.l
                    );
                }
            }
            let tinted = CATEGORIES
                .iter()
                .filter(|category| palette.fill(**category, 0).s >= 0.30)
                .count();
            assert_eq!(
                tinted, 1,
                "exactly one category may carry the warm accent (dark={is_dark})"
            );
            assert_eq!(
                palette.fill(TreemapCategory::Folder, 0).s,
                if is_dark { 0.0803 } else { 0.1091 }
            );
            assert_eq!(
                palette.fill(TreemapCategory::Folder, 0).l,
                if is_dark { 0.2486 } else { 0.7843 }
            );
            assert_eq!(
                palette.fill(TreemapCategory::Other, 0).s,
                if is_dark { 0.0943 } else { 0.0600 }
            );
            assert_eq!(
                palette.fill(TreemapCategory::Other, 0).l,
                if is_dark { 0.1878 } else { 0.8800 }
            );
        }
    }

    /// Folder and Other split on both temperature and value, and each depth keeps that separation.
    /// Verifies that hue distance or lightness delta stays above the perceptual threshold.
    #[test]
    fn folder_and_other_stay_visually_separate() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            let folder = palette.fill(TreemapCategory::Folder, 0);
            let other = palette.fill(TreemapCategory::Other, 0);
            let hue_delta = (folder.h - other.h).abs();
            let hue_turn = hue_delta.min(1.0 - hue_delta);
            assert!(
                hue_turn >= 45.0 / 360.0,
                "Folder and Other hues are only {:.1} degrees apart (dark={is_dark})",
                hue_turn * 360.0
            );
            for depth in DEPTHS {
                let ratio = contrast_ratio(
                    palette.fill(TreemapCategory::Folder, depth),
                    palette.fill(TreemapCategory::Other, depth),
                );
                assert!(
                    ratio >= 1.25,
                    "Folder and Other at depth {depth} (dark={is_dark}) measured {ratio:.2}:1"
                );
            }
        }
    }

    /// Ten categories may not collapse into fewer visible colors.
    #[test]
    fn categories_do_not_share_a_fill() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for depth in DEPTHS {
                for (i, left) in CATEGORIES.iter().enumerate() {
                    for right in &CATEGORIES[i + 1..] {
                        assert_ne!(
                            palette.fill(*left, depth),
                            palette.fill(*right, depth),
                            "{left:?} and {right:?} share a fill at depth {depth} (dark={is_dark})"
                        );
                    }
                }
            }
        }
    }

    /// The chromatic categories keep one distinct hue each.
    #[test]
    fn chromatic_categories_keep_distinct_hues() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for (i, left) in CHROMATIC.iter().enumerate() {
                for right in &CHROMATIC[i + 1..] {
                    assert_ne!(
                        palette.fill(*left, 0).h,
                        palette.fill(*right, 0).h,
                        "{left:?} and {right:?} share a hue (dark={is_dark})"
                    );
                }
            }
        }
    }

    /// Close hues must be carried apart by chroma instead: Document and Archive
    /// sit eight degrees apart, and only Archive's stronger tint keeps them
    /// readable as two categories.
    #[test]
    fn close_hues_are_separated_by_chroma() {
        const MIN_TURN: f32 = 15.0 / 360.0;
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for (i, left) in CHROMATIC.iter().enumerate() {
                for right in &CHROMATIC[i + 1..] {
                    let a = palette.fill(*left, 0);
                    let b = palette.fill(*right, 0);
                    let delta = (a.h - b.h).abs();
                    let turn = delta.min(1.0 - delta);
                    if turn < MIN_TURN {
                        assert!(
                            (a.s - b.s).abs() >= 0.15,
                            "{left:?} and {right:?} are {:.1} degrees apart with only {:.2} chroma between them (dark={is_dark})",
                            turn * 360.0,
                            (a.s - b.s).abs()
                        );
                    }
                }
            }
        }
    }

    /// GPUI hue is a turn fraction. This test exists because an angle such as
    /// `210.0` once slipped into the render path and painted the wrong color.
    #[test]
    fn hues_stay_inside_the_turn_fraction_domain() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                for depth in DEPTHS {
                    let fill = palette.fill(category, depth);
                    assert!(
                        (0.0..=1.0).contains(&fill.h),
                        "{category:?} fill hue {}",
                        fill.h
                    );
                }
            }
        }
    }

    /// Depth moves lightness only, and stops after three steps.
    #[test]
    fn depth_only_steps_lightness_and_clamps() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                let base = palette.fill(category, 0);
                for depth in 1..=MAX_DEPTH_STEPS {
                    let fill = palette.fill(category, depth);
                    assert_eq!(fill.h, base.h, "{category:?} hue must not move with depth");
                    assert_eq!(
                        fill.s, base.s,
                        "{category:?} saturation must not move with depth"
                    );
                }
                let last = palette.fill(category, MAX_DEPTH_STEPS);
                assert_eq!(
                    palette.fill(category, MAX_DEPTH_STEPS + 5),
                    last,
                    "{category:?} must clamp past the last step"
                );
                let direction = palette.fill(category, 1).l - base.l;
                assert_eq!(
                    direction > 0.0,
                    is_dark,
                    "dark fills move lighter with depth, light fills darker"
                );
            }
        }
    }

    /// The old neutral blend left some header tints almost unchanged on hover:
    /// in light mode Other measured 1.000:1 and other headers only 1.01–1.03:1,
    /// because their base already sat next to the neutral. Hover and press now
    /// move the surface's own lightness by a fixed step, so every interactive
    /// surface — file fill, compact leaf and header tint — has a measured,
    /// uniform delta and keeps its category hue.
    #[test]
    fn every_interactive_surface_has_a_measured_hover_step() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in TreemapCategory::ALL {
                for depth in DEPTHS {
                    let surfaces = [
                        palette.fill(category, depth),
                        palette.directory_header(category, depth),
                    ];
                    for base in surfaces {
                        let (hover, active) = block_fill_states(base, is_dark);
                        assert_eq!(hover.h, base.h, "{category:?} hover changed hue");
                        assert_eq!(hover.s, base.s, "{category:?} hover changed chroma");

                        let hover_step = contrast_ratio(base, hover);
                        assert!(
                            hover_step >= 1.07,
                            "{category:?} at depth {depth} (dark={is_dark}) hover moved only {hover_step:.3}:1"
                        );
                        let active_step = contrast_ratio(base, active);
                        assert!(
                            active_step >= 1.15,
                            "{category:?} at depth {depth} (dark={is_dark}) press moved only {active_step:.3}:1"
                        );
                        assert!(
                            contrast_ratio(palette.ink(), hover) >= 7.0,
                            "{category:?} hover broke AAA ink (dark={is_dark})"
                        );
                        assert!(
                            contrast_ratio(palette.ink_secondary(), hover) >= 4.5,
                            "{category:?} hover broke AA secondary ink (dark={is_dark})"
                        );
                    }
                }
            }
        }
    }
}
