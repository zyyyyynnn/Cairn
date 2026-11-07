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
//! The palette uses a unified neutral graphite tone across categories in each mode (#D9D9D9 in
//! light, #3A3A3A in dark), keeping visual hierarchy clean while text labels and icons carry
//! category identity.
//!
//! Nesting depth does not change color: depth is expressed by the 2px gutter, the header tint and
//! the nested rectangle, so one category is one color at every depth.
//!
//! A directory carries its category tint on the header only; the container body stays on the canvas,
//! so every sibling gutter has the same color at every nesting depth while file leaves remain the
//! strongest signal. A directory whose content carries no category signal keeps the
//! [`TreemapCategory::Other`] tint.
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

/// Hover and press step the block's own lightness down, keeping hue and chroma. Each mode carries
/// its own pair because equal HSL lightness deltas produce different contrast steps at opposite
/// ends of the luminance curve. Neither state adds a border; selection keeps full fill brightness
/// while outside regions are dimmed.
const TREEMAP_HOVER_STEP_LIGHT: f32 = 0.035;
const TREEMAP_ACTIVE_STEP_LIGHT: f32 = 0.065;
const TREEMAP_HOVER_STEP_DARK: f32 = 0.03;
const TREEMAP_ACTIVE_STEP_DARK: f32 = 0.055;

/// How much of a dimmed block's fill is the category itself, per mode.
const DIMMED_FILL_LIGHT: f32 = 0.42;
const DIMMED_FILL_DARK: f32 = 0.50;

/// How much of the tile tone a directory body carries, so a container reads as a plate of the
/// same material rather than a hole in the canvas.
const BODY_FILL_LIGHT: f32 = 0.12;
const BODY_FILL_DARK: f32 = 0.16;

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

// ---- light: low-saturation graphite tone on #F7F7F7 ---------------------------

const LIGHT_TILE_FILL: Hsla = hsla(0.0000, 0.0000, 0.8510); // #D9D9D9

// ---- dark: neutral graphite tone on #141414 -----------------------------------

const DARK_TILE_FILL: Hsla = hsla(0.0000, 0.0000, 0.2275); // #3A3A3A

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
        self.fill(category)
    }

    /// The body of a directory container: the same tone at a low share over the canvas.
    pub fn body_fill(&self) -> Hsla {
        let share = if self.is_dark {
            BODY_FILL_DARK
        } else {
            BODY_FILL_LIGHT
        };
        mix(self.canvas(), self.fill(TreemapCategory::Other), share)
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

fn light_fill(_category: TreemapCategory) -> Hsla {
    LIGHT_TILE_FILL
}

fn dark_fill(_category: TreemapCategory) -> Hsla {
    DARK_TILE_FILL
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
    use cairn_ui::theme::{contrast_ratio, is_neutral_ramp_step, perceptual_distance};

    const CATEGORIES: [TreemapCategory; 8] = [
        TreemapCategory::System,
        TreemapCategory::Programs,
        TreemapCategory::Documents,
        TreemapCategory::Image,
        TreemapCategory::Video,
        TreemapCategory::Audio,
        TreemapCategory::Archive,
        TreemapCategory::Other,
    ];

    /// Colour no longer names a category: every tile carries the mode's one tone.
    #[test]
    fn every_category_shares_the_mode_tone() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            let first = palette.fill(CATEGORIES[0]);
            for category in CATEGORIES {
                assert_eq!(
                    palette.fill(category),
                    first,
                    "{category:?} (dark={is_dark})"
                );
            }
        }
    }

    /// Data-plane text stays AAA on the tone, and sizes stay AA.
    #[test]
    fn ink_clears_aaa_on_the_tile() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                let ratio = contrast_ratio(palette.ink(), palette.fill(category));
                assert!(
                    ratio >= 7.0,
                    "{category:?} (dark={is_dark}) measured {ratio:.2}:1"
                );
                let size = contrast_ratio(palette.ink_secondary(), palette.fill(category));
                assert!(
                    size >= 4.5,
                    "{category:?} size (dark={is_dark}) measured {size:.2}:1"
                );
            }
        }
    }

    /// A tile has to be visible on the surface it covers.
    #[test]
    fn the_tile_reads_against_the_canvas() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                let ratio = contrast_ratio(palette.fill(category), palette.canvas());
                assert!(
                    ratio >= 1.30,
                    "{category:?} (dark={is_dark}) measured {ratio:.3}:1"
                );
            }
        }
    }

    /// Filtering and selection share one dimmed style; both inks survive it and the surface still
    /// reads as a tile rather than as gutter.
    #[test]
    fn dimmed_fills_keep_both_inks_readable_and_still_recede() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                let dimmed = palette.dimmed_fill(palette.fill(category));
                let name = contrast_ratio(palette.ink(), dimmed);
                let size = contrast_ratio(palette.ink_secondary(), dimmed);
                let recedes = contrast_ratio(dimmed, palette.canvas());
                assert!(
                    name >= 7.0,
                    "{category:?} name (dark={is_dark}) measured {name:.2}:1"
                );
                assert!(
                    size >= 4.5,
                    "{category:?} size (dark={is_dark}) measured {size:.2}:1"
                );
                assert!(
                    recedes >= 1.10,
                    "{category:?} (dark={is_dark}) measured {recedes:.3}:1"
                );
            }
        }
    }

    /// Directory headers carry the same tone as a leaf and keep their labels readable.
    #[test]
    fn headers_are_quieter_and_keep_text_readable() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                let header = palette.directory_header(category);
                let ink = contrast_ratio(palette.ink(), header);
                let size = contrast_ratio(palette.ink_secondary(), header);
                let ratio = contrast_ratio(header, palette.canvas());
                assert!(
                    ink >= 7.0,
                    "{category:?} header ink (dark={is_dark}) measured {ink:.2}:1"
                );
                assert!(
                    size >= 4.5,
                    "{category:?} header size (dark={is_dark}) measured {size:.2}:1"
                );
                assert!(
                    ratio >= 1.10,
                    "{category:?} header (dark={is_dark}) measured {ratio:.3}:1"
                );
                assert_eq!(
                    header,
                    palette.fill(category),
                    "{category:?} header (dark={is_dark})"
                );
            }
        }
    }

    /// Every interactive surface moves enough to read as a state, stays visible on the canvas and
    /// keeps its inks readable.
    #[test]
    fn every_interactive_surface_has_a_measured_state_step() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in CATEGORIES {
                for base in [palette.fill(category), palette.directory_header(category)] {
                    let (hover, active) = block_fill_states(base, is_dark);
                    let hover_distance = perceptual_distance(base, hover);
                    let active_distance = perceptual_distance(base, active);
                    assert!(
                        hover_distance >= 2.5,
                        "{category:?} (dark={is_dark}) hover measured {hover_distance:.1}"
                    );
                    assert!(
                        active_distance >= 4.5,
                        "{category:?} (dark={is_dark}) press measured {active_distance:.1}"
                    );
                    for state in [hover, active] {
                        let canvas_ratio = contrast_ratio(state, palette.canvas());
                        let size = contrast_ratio(palette.ink_secondary(), state);
                        assert!(
                            canvas_ratio >= 1.10,
                            "{category:?} state {canvas_ratio:.3}:1"
                        );
                        assert!(size >= 4.5, "{category:?} state size {size:.2}:1");
                    }
                    assert!(contrast_ratio(palette.ink(), hover) >= 7.0);
                }
            }
        }
    }

    /// A container body is a plate between the canvas and the tile; dimming moves it back.
    #[test]
    fn the_body_plate_sits_between_canvas_and_tile_and_dims_back() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            let body = palette.body_fill();
            let tile = palette.fill(TreemapCategory::Other);
            let canvas = palette.canvas();
            let to_tile = perceptual_distance(body, tile);
            let to_canvas = perceptual_distance(body, canvas);
            assert!(to_tile > to_canvas, "body is not a plate (dark={is_dark})");
            let dimmed = palette.dimmed_fill(body);
            assert!(perceptual_distance(dimmed, canvas) < to_canvas);
        }
    }

    /// The data plane's inks and canvas are steps of the neutral ramp.
    #[test]
    fn data_plane_inks_are_ramp_steps() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            assert!(is_neutral_ramp_step(palette.ink_secondary()));
            assert!(is_neutral_ramp_step(palette.ink()));
            assert!(is_neutral_ramp_step(palette.canvas()));
        }
    }
}
