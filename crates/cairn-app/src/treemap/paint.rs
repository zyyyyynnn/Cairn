// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Paint geometry for Treemap tiles: the shared inset and radius constants,
//! the painted-rect derivation, and device-pixel snapping shared by the
//! canvas, hit testing and the zoom viewport.

use cairn_core::treemap::TreemapRect;

/// Every tile paints inside its layout cell, and the canvas edge is inset by half a gutter, so
/// adjacent cells and the outer edge share one even device-pixel rhythm at every nesting depth.
pub(super) const TREEMAP_TILE_INSET: f32 = 1.0;
/// Corner radius shared by directory containers, file leaves and rendered
/// aggregate summaries. Flat fill patches stay square.
pub(super) const TREEMAP_RADIUS: f32 = 3.0;

/// Snaps a logical pixel coordinate to the window's device-pixel grid using the
/// same half-toward-zero rule GPUI's Taffy layout converter applies.
///
/// The scale arrives already clamped by the view's `canvas_scale`, the one reader of
/// the window's factor: re-guarding it here would only hide a caller that skipped it.
fn snap_to_device_pixel(value: f32, scale_factor: f32) -> f32 {
    let scaled = value * scale_factor;
    if scaled.is_finite() {
        (scaled.signum() * (scaled.abs() + 0.5).next_down().floor()) / scale_factor
    } else {
        value
    }
}

/// The painted gutter between two cells, in device pixels: an even number at every scale.
pub(super) fn painted_gutter_device(scale_factor: f32) -> f32 {
    (2.0 * (TREEMAP_TILE_INSET * scale_factor).round()).max(2.0)
}

/// Insets an already snapped cell by a fixed number of device pixels.
///
/// The gutter between two cells is `round(2 * scale)` device pixels: the leading half comes from
/// this cell's inset and the trailing half from its neighbour's. A logical inset cannot do that —
/// under a squeezed projection 2 logical pixels are 2.85 device pixels, which rounds to 2 at some
/// phases and 3 at others, so one column of the canvas reads thinner than the rest.
pub(super) fn inset_device_pixels(rect: TreemapRect, scale_factor: f32) -> TreemapRect {
    let gutter_device = painted_gutter_device(scale_factor);
    let inset = gutter_device / 2.0 / scale_factor;
    TreemapRect::new(
        rect.x + inset,
        rect.y + inset,
        (rect.width - gutter_device / scale_factor).max(0.0),
        (rect.height - gutter_device / scale_factor).max(0.0),
    )
}

/// Snaps a painted rect to the device-pixel grid by snapping both of its edges.
///
/// Snapping the position and the size separately lets the derived far edge drift from the value a
/// neighbour computes for the same boundary, which turns one gutter into two or four device pixels.
/// Snapping the edges keeps every shared boundary on one pixel, so a 2px logical gutter is exactly
/// 3 device pixels at 150%.
pub(super) fn snap_rect_to_device_pixels(rect: TreemapRect, scale_factor: f32) -> TreemapRect {
    let min_px = if rect.width > 0.0 && rect.height > 0.0 {
        1.0 / scale_factor
    } else {
        0.0
    };
    let left = snap_to_device_pixel(rect.x, scale_factor);
    let top = snap_to_device_pixel(rect.y, scale_factor);
    let right = snap_to_device_pixel(rect.x + rect.width, scale_factor);
    let bottom = snap_to_device_pixel(rect.y + rect.height, scale_factor);
    TreemapRect::new(
        left,
        top,
        (right - left).max(min_px),
        (bottom - top).max(min_px),
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

    /// The painted gutter is a constant even number of device pixels — 2 at 100%, 4 at 150% and
    /// 200% — whatever phase the cell lands on. An odd value would break the even-spacing rule,
    /// and a per-side logical inset lets the gap fall to one device pixel thinner at some phases.
    #[test]
    fn the_painted_gutter_is_an_even_number_of_device_pixels() {
        for (scale, expected) in [(1.0_f32, 2.0_f32), (1.5, 4.0), (2.0, 4.0)] {
            for x in [0.0_f32, 0.3, 0.7, 2.9] {
                let left = inset_device_pixels(
                    snap_rect_to_device_pixels(TreemapRect::new(x, 0.0, 10.0, 10.0), scale),
                    scale,
                );
                let right = inset_device_pixels(
                    snap_rect_to_device_pixels(TreemapRect::new(x + 10.0, 0.0, 10.0, 10.0), scale),
                    scale,
                );
                let gap = (right.x - (left.x + left.width)) * scale;
                assert!(
                    (gap - expected).abs() < 1.0e-4,
                    "scale {scale} x {x}: gap {gap} device px"
                );
            }
        }
    }

    /// A snapped rect must end exactly on the snapped far edge. Snapping the
    /// position and the size separately breaks that: the drawn right edge drifts
    /// from the value a neighbour computes for the same boundary, and one gutter
    /// comes out a device pixel thinner than the rest.
    #[test]
    fn a_snapped_rect_ends_on_the_snapped_far_edge() {
        for (x, width) in [(0.4, 1.066_7), (2.4, 2.4), (10.266_7, 20.733_3)] {
            let rect = TreemapRect::new(x, 0.0, width, 10.0);
            let snapped = snap_rect_to_device_pixels(rect, 1.5);
            let far = snap_to_device_pixel(x + width, 1.5);
            assert!(
                (snapped.x + snapped.width - far).abs() < 1.0e-4,
                "x {x} width {width}: derived {} vs far edge {far}",
                snapped.x + snapped.width
            );
        }
    }
}
