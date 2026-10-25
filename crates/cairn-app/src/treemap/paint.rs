// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Paint geometry for Treemap tiles: the shared inset and radius constants,
//! the painted-rect derivation, and device-pixel snapping shared by the
//! canvas, hit testing and the zoom viewport.

use cairn_core::treemap::TreemapRect;

/// The canvas keeps the same visual gutter around the outermost blocks that
/// nested blocks keep between themselves: this inset plus the 1px tile paint
/// inset read as a 2px outer gutter.
pub(super) const TREEMAP_CANVAS_INSET: f32 = 1.0;
/// Every tile paints 1px inside its layout cell. Adjacent cells therefore show
/// a uniform 2px gutter at every nesting depth, without compounding on
/// recursive layouts.
pub(super) const TREEMAP_TILE_INSET: f32 = 1.0;
/// Corner radius shared by directory containers, file leaves and rendered
/// aggregate summaries. Flat fill patches stay square.
pub(super) const TREEMAP_RADIUS: f32 = 3.0;

/// The painted rect of a tile: its layout cell inset by one pixel on every
/// side. Children keep their own layout cells, so the inset never compounds
/// recursively; shared cell edges are always exactly 2px apart.
pub(super) fn paint_rect(rect: TreemapRect) -> TreemapRect {
    TreemapRect::new(
        rect.x + TREEMAP_TILE_INSET,
        rect.y + TREEMAP_TILE_INSET,
        (rect.width - 2.0 * TREEMAP_TILE_INSET).max(0.0),
        (rect.height - 2.0 * TREEMAP_TILE_INSET).max(0.0),
    )
}

/// Snaps a logical pixel coordinate to the window's device-pixel grid using the
/// same half-toward-zero rule GPUI's Taffy layout converter applies.
fn snap_to_device_pixel(value: f32, scale_factor: f32) -> f32 {
    let sf = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let scaled = value * sf;
    if scaled.is_finite() {
        (scaled.signum() * (scaled.abs() + 0.5).next_down().floor()) / sf
    } else {
        value
    }
}

/// Snaps all four coordinates of a painted rect to the device-pixel grid. The
/// snapped rect is what the canvas actually paints, so label thresholds, the
/// selection ring and hit testing all agree with the pixels on screen.
pub(super) fn snap_rect_to_device_pixels(rect: TreemapRect, scale_factor: f32) -> TreemapRect {
    let sf = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let min_px = if rect.width > 0.0 && rect.height > 0.0 {
        1.0 / sf
    } else {
        0.0
    };
    TreemapRect::new(
        snap_to_device_pixel(rect.x, sf),
        snap_to_device_pixel(rect.y, sf),
        snap_to_device_pixel(rect.width, sf).max(min_px),
        snap_to_device_pixel(rect.height, sf).max(min_px),
    )
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::*;

    /// Every painted rect snaps to exact integer device pixels so a block and
    /// the gutter beside it never sit on a `.5` rounding boundary.
    #[test]
    fn device_pixel_snapping_eliminates_half_pixel_boundary_targets() {
        for scale in [1.0_f32, 1.25, 1.5, 2.0] {
            let raw = TreemapRect::new(394.439, 148.037, 107.668, 43.755);
            let snapped = snap_rect_to_device_pixels(raw, scale);
            for coord in [snapped.x, snapped.y, snapped.width, snapped.height] {
                let dp = coord * scale;
                let nearest = dp.round();
                assert!(
                    (dp - nearest).abs() < 1.0e-4,
                    "snapped coord {coord} at scale {scale} is not an integer device pixel ({dp})"
                );
            }
        }
    }

    /// The tile inset keeps shared cell edges exactly 2px apart at every
    /// nesting depth, and never produces a negative size.
    #[test]
    fn paint_rect_insets_every_side_and_clamps_to_zero() {
        let rect = TreemapRect::new(10.0, 20.0, 100.0, 50.0);
        let painted = paint_rect(rect);
        assert_eq!(painted, TreemapRect::new(11.0, 21.0, 98.0, 48.0));

        let tiny = paint_rect(TreemapRect::new(0.0, 0.0, 1.0, 1.0));
        assert_eq!(tiny.width, 0.0);
        assert_eq!(tiny.height, 0.0);
    }
}
