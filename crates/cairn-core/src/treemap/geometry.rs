// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Rectangle in the Treemap's base pixel space.

/// A 2D bounding rectangle in abstract float coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct TreemapRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl TreemapRect {
    /// Builds a rectangle; negative extents clamp to zero.
    #[inline]
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width: width.max(0.0),
            height: height.max(0.0),
        }
    }

    #[inline]
    pub fn area(&self) -> f32 {
        self.width * self.height
    }

    #[inline]
    pub fn shorter_side(&self) -> f32 {
        self.width.min(self.height)
    }

    #[inline]
    pub fn longer_side(&self) -> f32 {
        self.width.max(self.height)
    }
}
