// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Pure Treemap viewport transform and pointer-anchored wheel zoom.
//!
//! The squarified layout is computed once in base pixel space. Zooming only
//! changes the projection from base space to the canvas; it never re-lays-out
//! the tree. This module deliberately has no GPUI dependency so the transform,
//! hit testing and auto-enter policy can be tested directly.

use cairn_core::treemap::{NestedTreemapLayoutNode, TreemapRect};
use std::time::{Duration, Instant};

/// Wheel zoom is bounded so a directory tile can always be inspected without
/// losing the surrounding context.
const MIN_SCALE: f32 = 1.0;
const MAX_SCALE: f32 = 4.0;
/// One normalized wheel line. Multiplication keeps repeated steps uniform.
const ZOOM_STEP: f32 = 1.12;
const MAX_STEPS_PER_EVENT: f32 = 3.0;
/// Automatic drill has no transition animation. The cooldown only prevents a
/// trackpad's inertia from crossing a second directory immediately after the
/// first automatic drill.
const AUTO_ENTER_COOLDOWN: Duration = Duration::from_millis(400);

/// The base-to-screen transform: `screen = (base - origin) * scale`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TreemapViewport {
    scale: f32,
    origin: (f32, f32),
}

impl Default for TreemapViewport {
    fn default() -> Self {
        Self {
            scale: MIN_SCALE,
            origin: (0.0, 0.0),
        }
    }
}

impl TreemapViewport {
    pub(super) fn scale(&self) -> f32 {
        self.scale
    }

    pub(super) fn is_zoomed(&self) -> bool {
        self.scale > MIN_SCALE
    }

    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Keeps at least one full canvas of content in view. Resizing the window or
    /// collapsing the sidebar shrinks the canvas under an existing origin, so
    /// the origin has to move back inside the legal range.
    pub(super) fn clamp(&mut self, canvas: (f32, f32)) {
        if self.scale <= MIN_SCALE {
            self.scale = MIN_SCALE;
            self.origin = (0.0, 0.0);
            return;
        }
        let max_x = (canvas.0 - canvas.0 / self.scale).max(0.0);
        let max_y = (canvas.1 - canvas.1 / self.scale).max(0.0);
        self.origin.0 = self.origin.0.clamp(0.0, max_x);
        self.origin.1 = self.origin.1.clamp(0.0, max_y);
    }

    /// Multiplies the scale by `1.12^lines` (at most three steps per event)
    /// while holding the base-space point under `anchor` in place. Returns
    /// whether the scale changed.
    pub(super) fn zoom_by_lines(
        &mut self,
        lines: f32,
        anchor: (f32, f32),
        canvas: (f32, f32),
    ) -> bool {
        if !lines.is_finite() || lines == 0.0 {
            return false;
        }
        let steps = lines.clamp(-MAX_STEPS_PER_EVENT, MAX_STEPS_PER_EVENT);
        let previous = self.scale;
        let next = (previous * ZOOM_STEP.powf(steps)).clamp(MIN_SCALE, MAX_SCALE);
        if next == previous {
            return false;
        }

        let world = self.unproject_point(anchor);
        self.scale = next;
        if next <= MIN_SCALE {
            self.origin = (0.0, 0.0);
        } else {
            self.origin = (world.0 - anchor.0 / next, world.1 - anchor.1 / next);
        }
        self.clamp(canvas);
        self.scale != previous
    }

    pub(super) fn project(&self, rect: TreemapRect) -> TreemapRect {
        let (x, y) = self.project_point((rect.x, rect.y));
        TreemapRect::new(x, y, rect.width * self.scale, rect.height * self.scale)
    }

    /// Moves the visible content by a screen-space delta. Dragging the canvas
    /// right moves the content right, so the origin moves left by the delta in
    /// base space. Returns whether the clamped origin changed.
    pub(super) fn pan_by(&mut self, dx_screen: f32, dy_screen: f32, canvas: (f32, f32)) -> bool {
        if !dx_screen.is_finite() || !dy_screen.is_finite() || self.scale <= MIN_SCALE {
            return false;
        }
        let previous = self.origin;
        self.origin.0 -= dx_screen / self.scale;
        self.origin.1 -= dy_screen / self.scale;
        self.clamp(canvas);
        self.origin != previous
    }

    pub(super) fn project_point(&self, point: (f32, f32)) -> (f32, f32) {
        (
            (point.0 - self.origin.0) * self.scale,
            (point.1 - self.origin.1) * self.scale,
        )
    }

    pub(super) fn unproject_point(&self, point: (f32, f32)) -> (f32, f32) {
        (
            point.0 / self.scale + self.origin.0,
            point.1 / self.scale + self.origin.1,
        )
    }

    /// Scale at which `rect` exactly fills the canvas, without a ceiling.
    /// Callers decide whether the result is reachable: a directory that needs
    /// more than [`MAX_SCALE`] never auto-enters. Uses the base-space layout
    /// cell, whose fixed 1px paint inset is a gutter rather than a size input,
    /// so a cell that exactly fills at 4x stays enterable; hit testing still
    /// requires the pointer inside the inset painted rect.
    pub(super) fn fill_scale(&self, rect: TreemapRect, canvas: (f32, f32)) -> f32 {
        if rect.width <= 0.0 || rect.height <= 0.0 {
            return f32::INFINITY;
        }
        (canvas.0 / rect.width).max(canvas.1 / rect.height)
    }
}

/// Wheel zoom state for one Treemap view, including the auto-enter cooldown.
#[derive(Clone, Copy, Debug)]
pub(super) struct TreemapZoom {
    viewport: TreemapViewport,
    last_auto_enter: Option<Instant>,
    /// Cursor position the next middle-button drag delta is measured from.
    /// It follows the cursor after every step so a missed move cannot replay
    /// the whole drag, which would make the content accelerate.
    pan_anchor: Option<(f32, f32)>,
}

impl Default for TreemapZoom {
    fn default() -> Self {
        Self::new()
    }
}

impl TreemapZoom {
    pub(super) fn new() -> Self {
        Self {
            viewport: TreemapViewport::default(),
            last_auto_enter: None,
            pan_anchor: None,
        }
    }

    pub(super) fn viewport(&self) -> TreemapViewport {
        self.viewport
    }

    pub(super) fn is_zoomed(&self) -> bool {
        self.viewport.is_zoomed()
    }

    pub(super) fn is_panning(&self) -> bool {
        self.pan_anchor.is_some()
    }

    /// Starts a middle-button drag only while there is content outside the
    /// canvas to reveal. An identity view has nothing to pan.
    pub(super) fn begin_pan(&mut self, cursor: (f32, f32)) -> bool {
        if !cursor.0.is_finite() || !cursor.1.is_finite() || !self.is_zoomed() || self.is_panning()
        {
            return false;
        }
        self.pan_anchor = Some(cursor);
        true
    }

    /// Applies the screen-space step since the previous cursor position and
    /// then adopts the new cursor. Adopting the current position, not the
    /// press position, keeps each pointer delta applied exactly once.
    pub(super) fn pan_to(&mut self, cursor: (f32, f32), canvas: (f32, f32)) -> bool {
        let Some(anchor) = self.pan_anchor else {
            return false;
        };
        if !cursor.0.is_finite() || !cursor.1.is_finite() {
            return false;
        }
        if !self.is_zoomed() {
            self.end_pan();
            return false;
        }
        let changed = self
            .viewport
            .pan_by(cursor.0 - anchor.0, cursor.1 - anchor.1, canvas);
        self.pan_anchor = Some(cursor);
        changed
    }

    /// Ends a drag. Returns whether one was active, so callers can notify only
    /// when the rendered pan layer actually goes away.
    pub(super) fn end_pan(&mut self) -> bool {
        self.pan_anchor.take().is_some()
    }

    /// Keyboard pan. A positive X moves the viewport right, which moves the
    /// content left; `pan_by` takes the content-space direction.
    pub(super) fn pan_view(&mut self, dx_screen: f32, dy_screen: f32, canvas: (f32, f32)) -> bool {
        self.viewport.pan_by(-dx_screen, -dy_screen, canvas)
    }

    /// Rounded-up zoom percentage for the status bar. Identity has no badge.
    pub(super) fn zoom_percent(&self) -> Option<u32> {
        if !self.is_zoomed() {
            return None;
        }
        Some(((self.viewport.scale() * 100.0).ceil() as u32).max(101))
    }

    /// Resets the transform. The auto-enter cooldown survives navigation so a
    /// trackpad's inertia cannot immediately cross another directory.
    pub(super) fn reset_view(&mut self) {
        self.viewport.reset();
        self.pan_anchor = None;
    }

    pub(super) fn clamp(&mut self, canvas: (f32, f32)) {
        self.viewport.clamp(canvas);
    }

    /// Applies one wheel event and, only for upward lines, decides whether the
    /// real directory under the pointer now fills the canvas. The decision runs
    /// after the zoom so the current scale is the one being tested. `hit_inset`
    /// is the tile's painted inset, so a pointer on the 1px gutter never hits
    /// the tile that merely owns the layout cell.
    pub(super) fn wheel(
        &mut self,
        nodes: &[NestedTreemapLayoutNode],
        lines: f32,
        cursor: (f32, f32),
        canvas: (f32, f32),
        hit_inset: f32,
        now: Instant,
    ) -> WheelOutcome {
        let changed = self.viewport.zoom_by_lines(lines, cursor, canvas);
        if !self.viewport.is_zoomed() {
            self.end_pan();
        }
        let mut enter_path = None;
        if lines > 0.0 && self.cooldown_elapsed(now) {
            let base = self.viewport.unproject_point(cursor);
            if let Some(node) = hit_test(nodes, base, hit_inset) {
                let required = self.viewport.fill_scale(node.rect, canvas);
                if node.item.is_dir
                    && node.aggregate_count == 0
                    && required <= MAX_SCALE
                    && self.viewport.scale() >= required
                {
                    self.last_auto_enter = Some(now);
                    enter_path = Some(node.path.clone());
                }
            }
        }
        WheelOutcome {
            changed,
            enter_path,
        }
    }

    /// Keyboard zoom keeps the canvas centre fixed and never auto-enters.
    pub(super) fn zoom_center(&mut self, lines: f32, canvas: (f32, f32)) -> bool {
        let anchor = (canvas.0 / 2.0, canvas.1 / 2.0);
        let changed = self.viewport.zoom_by_lines(lines, anchor, canvas);
        if !self.viewport.is_zoomed() {
            self.end_pan();
        }
        changed
    }

    fn cooldown_elapsed(&self, now: Instant) -> bool {
        self.last_auto_enter
            .is_none_or(|last| now.saturating_duration_since(last) >= AUTO_ENTER_COOLDOWN)
    }
}

/// What one wheel event changed. `enter_path` is set only when the pointer has
/// zoomed a real directory until it fills the canvas.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(super) struct WheelOutcome {
    changed: bool,
    enter_path: Option<String>,
}

impl WheelOutcome {
    pub(super) fn changed(&self) -> bool {
        self.changed
    }

    pub(super) fn enter_path(&self) -> Option<&str> {
        self.enter_path.as_deref()
    }
}

/// Deepest node under a base-space point. Children are painted after their
/// container and siblings later in the list are painted later, so both win.
/// `hit_inset` matches the paint inset: a tile only hits inside its painted
/// rect, while the 1px gutter between siblings still belongs to their parent.
/// Rects are half-open: the right and bottom edges do not hit.
pub(super) fn hit_test(
    nodes: &[NestedTreemapLayoutNode],
    point: (f32, f32),
    hit_inset: f32,
) -> Option<&NestedTreemapLayoutNode> {
    nodes
        .iter()
        .rev()
        .find_map(|node| hit_test_node(node, point, hit_inset))
}

fn hit_test_node(
    node: &NestedTreemapLayoutNode,
    point: (f32, f32),
    hit_inset: f32,
) -> Option<&NestedTreemapLayoutNode> {
    let rect = inset_rect(node.rect, hit_inset);
    if rect.width <= 0.0
        || rect.height <= 0.0
        || point.0 < rect.x
        || point.0 >= rect.x + rect.width
        || point.1 < rect.y
        || point.1 >= rect.y + rect.height
    {
        return None;
    }
    node.children
        .iter()
        .rev()
        .find_map(|child| hit_test_node(child, point, hit_inset))
        .or(Some(node))
}

/// Shrinks a base-space rect by `inset` on every side. An inset at or below
/// zero is a no-op so callers can test the raw layout cell.
fn inset_rect(rect: TreemapRect, inset: f32) -> TreemapRect {
    if inset <= 0.0 {
        return rect;
    }
    TreemapRect::new(
        rect.x + inset,
        rect.y + inset,
        (rect.width - 2.0 * inset).max(0.0),
        (rect.height - 2.0 * inset).max(0.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::treemap::{TreemapCategory, TreemapItem};

    fn rect(x: f32, y: f32, width: f32, height: f32) -> TreemapRect {
        TreemapRect::new(x, y, width, height)
    }

    fn node(
        path: &str,
        is_dir: bool,
        bounds: TreemapRect,
        aggregate_count: usize,
        children: Vec<NestedTreemapLayoutNode>,
    ) -> NestedTreemapLayoutNode {
        NestedTreemapLayoutNode {
            item: TreemapItem {
                id: 1,
                name: path.rsplit('\\').next().unwrap_or(path).to_string(),
                size: 1,
                is_dir,
                modified_ts: 0,
                category: TreemapCategory::Folder,
            },
            path: path.to_string(),
            rect: bounds,
            header_rect: None,
            content_rect: None,
            children,
            depth: 0,
            aggregate_count,
        }
    }

    fn directory(path: &str, bounds: TreemapRect) -> NestedTreemapLayoutNode {
        node(path, true, bounds, 0, Vec::new())
    }

    fn assert_canvas_is_covered(viewport: &TreemapViewport, canvas: (f32, f32)) {
        let top_left = viewport.project_point((0.0, 0.0));
        let bottom_right = viewport.project_point(canvas);
        assert!(
            top_left.0 <= 0.0001 && top_left.1 <= 0.0001,
            "canvas top-left shows background: {top_left:?}"
        );
        assert!(
            bottom_right.0 >= canvas.0 - 0.001 && bottom_right.1 >= canvas.1 - 0.001,
            "canvas bottom-right shows background: {bottom_right:?}"
        );
    }

    #[test]
    fn identity_projection_is_a_no_op() {
        let viewport = TreemapViewport::default();
        let bounds = rect(10.0, 20.0, 30.0, 40.0);

        assert_eq!(viewport.scale(), 1.0);
        assert!(!viewport.is_zoomed());
        assert_eq!(viewport.project(bounds), bounds);
        assert_eq!(viewport.project_point((5.0, 6.0)), (5.0, 6.0));
        assert_eq!(viewport.unproject_point((5.0, 6.0)), (5.0, 6.0));
    }

    #[test]
    fn zoom_keeps_the_world_point_under_the_anchor() {
        let canvas = (1000.0, 800.0);
        let mut viewport = TreemapViewport::default();

        assert!(viewport.zoom_by_lines(2.0, (500.0, 400.0), canvas));
        let anchor = viewport.project_point((500.0, 400.0));
        assert!((anchor.0 - 500.0).abs() < 0.001);
        assert!((anchor.1 - 400.0).abs() < 0.001);

        let anchor = (600.0, 300.0);
        let world = viewport.unproject_point(anchor);
        assert!(viewport.zoom_by_lines(-1.0, anchor, canvas));
        let projected = viewport.project_point(world);
        assert!(
            (projected.0 - anchor.0).abs() < 0.001 && (projected.1 - anchor.1).abs() < 0.001,
            "the point under the pointer moved: {anchor:?} -> {projected:?}"
        );
    }

    #[test]
    fn zoom_steps_are_capped_per_event_and_use_linear_scale_steps() {
        let canvas = (800.0, 600.0);
        let mut one_event = TreemapViewport::default();
        let mut three_events = TreemapViewport::default();

        assert!(one_event.zoom_by_lines(10.0, (400.0, 300.0), canvas));
        for _ in 0..3 {
            assert!(three_events.zoom_by_lines(1.0, (400.0, 300.0), canvas));
        }

        assert!((one_event.scale() - three_events.scale()).abs() < 0.0001);
        assert!(
            (one_event.scale() - 1.404_928).abs() < 0.0001,
            "three 1.12x steps should reach 1.404928, got {}",
            one_event.scale()
        );
    }

    #[test]
    fn clamp_keeps_the_canvas_covered_at_every_corner_and_size() {
        let canvases = [(800.0, 600.0), (4000.0, 2000.0), (320.0, 100.0), (1.0, 1.0)];
        for canvas in canvases {
            for anchor in [
                (0.0, 0.0),
                (canvas.0, 0.0),
                (0.0, canvas.1),
                (canvas.0, canvas.1),
            ] {
                let mut viewport = TreemapViewport::default();
                assert!(viewport.zoom_by_lines(3.0, anchor, canvas));
                assert_canvas_is_covered(&viewport, canvas);
            }
        }
    }

    #[test]
    fn clamp_after_a_smaller_window_never_exposes_the_canvas_background() {
        let mut viewport = TreemapViewport::default();
        viewport.zoom_by_lines(3.0, (800.0, 600.0), (800.0, 600.0));

        viewport.clamp((400.0, 300.0));

        assert_canvas_is_covered(&viewport, (400.0, 300.0));
    }

    #[test]
    fn shrinking_back_to_one_resets_the_origin_and_caps_at_four() {
        let canvas = (800.0, 600.0);
        let mut viewport = TreemapViewport::default();

        for _ in 0..12 {
            viewport.zoom_by_lines(3.0, (400.0, 300.0), canvas);
        }
        assert_eq!(viewport.scale(), 4.0);

        for _ in 0..12 {
            viewport.zoom_by_lines(-3.0, (400.0, 300.0), canvas);
        }
        assert_eq!(viewport.scale(), 1.0);
        assert_eq!(viewport.origin, (0.0, 0.0));
        let bounds = rect(7.0, 9.0, 11.0, 13.0);
        assert_eq!(viewport.project(bounds), bounds);
    }

    #[test]
    fn a_zero_line_event_does_not_change_the_viewport() {
        let mut viewport = TreemapViewport::default();
        assert!(!viewport.zoom_by_lines(0.0, (400.0, 300.0), (800.0, 600.0)));
        assert_eq!(viewport, TreemapViewport::default());
    }

    #[test]
    fn fill_scale_reports_the_uncapped_scale_needed_to_fill_the_canvas() {
        let viewport = TreemapViewport::default();
        let canvas = (800.0, 600.0);

        assert_eq!(
            viewport.fill_scale(rect(0.0, 0.0, 400.0, 300.0), canvas),
            2.0
        );
        assert_eq!(
            viewport.fill_scale(rect(0.0, 0.0, 200.0, 150.0), canvas),
            4.0
        );
        assert_eq!(
            viewport.fill_scale(rect(0.0, 0.0, 200.0, 100.0), canvas),
            6.0
        );
        assert_eq!(
            viewport.fill_scale(rect(0.0, 0.0, 800.0, 600.0), canvas),
            1.0
        );
        assert_eq!(viewport.fill_scale(rect(0.0, 0.0, 1.0, 1.0), canvas), 800.0);
    }

    #[test]
    fn hit_test_prefers_the_deepest_node_and_treats_gutters_as_the_parent() {
        let child_file = node(
            r"C:\Root\child.bin",
            false,
            rect(20.0, 40.0, 100.0, 50.0),
            0,
            Vec::new(),
        );
        let mut child_dir = node(
            r"C:\Root\Child",
            true,
            rect(10.0, 30.0, 190.0, 130.0),
            0,
            vec![child_file],
        );
        child_dir.header_rect = Some(rect(10.0, 30.0, 190.0, 20.0));
        let child_leaf = node(
            r"C:\Root\sibling.bin",
            false,
            rect(210.0, 30.0, 180.0, 130.0),
            0,
            Vec::new(),
        );
        let aggregate = node(
            r"C:\Root::<tail:0>",
            false,
            rect(10.0, 170.0, 380.0, 120.0),
            12,
            Vec::new(),
        );
        let mut parent = node(
            r"C:\Root",
            true,
            rect(0.0, 0.0, 400.0, 300.0),
            0,
            vec![child_dir, child_leaf, aggregate],
        );
        parent.header_rect = Some(rect(0.0, 0.0, 400.0, 20.0));
        let nodes = vec![parent];

        assert_eq!(
            hit_test(&nodes, (300.0, 10.0), 1.0).map(|n| n.path.as_str()),
            Some(r"C:\Root"),
            "a parent header belongs to the parent"
        );
        assert_eq!(
            hit_test(&nodes, (50.0, 35.0), 1.0).map(|n| n.path.as_str()),
            Some(r"C:\Root\Child"),
            "a directory header belongs to the deepest directory"
        );
        assert_eq!(
            hit_test(&nodes, (30.0, 50.0), 1.0).map(|n| n.path.as_str()),
            Some(r"C:\Root\child.bin"),
            "a file inside a directory wins"
        );
        assert_eq!(
            hit_test(&nodes, (100.0, 200.0), 1.0).map(|n| n.path.as_str()),
            Some(r"C:\Root::<tail:0>"),
            "an aggregate block is hit as itself"
        );
        assert_eq!(
            hit_test(&nodes, (205.0, 100.0), 1.0).map(|n| n.path.as_str()),
            Some(r"C:\Root"),
            "the gutter between children belongs to the parent"
        );
        assert_eq!(hit_test(&nodes, (400.0, 100.0), 1.0), None);
        assert_eq!(
            hit_test(&nodes, (0.5, 100.0), 1.0),
            None,
            "the outer painted edge is not a hit"
        );
        assert_eq!(
            hit_test(&nodes, (399.5, 100.0), 1.0),
            None,
            "the right painted edge is not a hit"
        );
        assert_eq!(
            hit_test(&nodes, (2.0, 100.0), 1.0).map(|n| n.path.as_str()),
            Some(r"C:\Root")
        );
        assert_eq!(hit_test(&nodes, (410.0, 10.0), 1.0), None);
    }

    #[test]
    fn hit_test_uses_paint_order_between_overlapping_siblings() {
        let nodes = vec![
            node(r"C:\A", false, rect(0.0, 0.0, 100.0, 100.0), 0, Vec::new()),
            node(r"C:\B", false, rect(0.0, 0.0, 100.0, 100.0), 0, Vec::new()),
        ];

        assert_eq!(
            hit_test(&nodes, (50.0, 50.0), 1.0).map(|n| n.path.as_str()),
            Some(r"C:\B")
        );
    }

    #[test]
    fn wheel_enters_a_directory_only_after_it_fills_the_canvas() {
        let canvas = (800.0, 600.0);
        let nodes = vec![directory(r"C:\Data", rect(0.0, 0.0, 400.0, 300.0))];
        let now = Instant::now();
        let mut zoom = TreemapZoom::new();

        let first = zoom.wheel(&nodes, 3.0, (200.0, 150.0), canvas, 1.0, now);
        assert!(first.changed());
        assert_eq!(first.enter_path(), None, "1.40x does not fill a 2x tile");

        let second = zoom.wheel(&nodes, 3.0, (200.0, 150.0), canvas, 1.0, now);
        assert_eq!(second.enter_path(), None, "1.97x still does not fill 2x");

        let third = zoom.wheel(&nodes, 3.0, (200.0, 150.0), canvas, 1.0, now);
        assert_eq!(third.enter_path(), Some(r"C:\Data"));
    }

    #[test]
    fn a_directory_that_needs_more_than_four_times_never_auto_enters() {
        let canvas = (800.0, 600.0);
        let nodes = vec![directory(r"C:\Small", rect(0.0, 0.0, 200.0, 100.0))];
        let mut zoom = TreemapZoom::new();
        for _ in 0..8 {
            zoom.viewport.zoom_by_lines(3.0, (100.0, 50.0), canvas);
        }
        assert_eq!(zoom.viewport().scale(), 4.0);

        let outcome = zoom.wheel(&nodes, 1.0, (100.0, 50.0), canvas, 1.0, Instant::now());
        assert!(!outcome.changed());
        assert_eq!(
            outcome.enter_path(),
            None,
            "a 6x fill requirement must not enter at the 4x ceiling"
        );
    }

    #[test]
    fn a_directory_that_exactly_fills_at_four_times_still_enters() {
        let canvas = (800.0, 600.0);
        let nodes = vec![directory(r"C:\Fit", rect(0.0, 0.0, 200.0, 150.0))];
        let mut zoom = TreemapZoom::new();
        for _ in 0..8 {
            zoom.viewport.zoom_by_lines(3.0, (100.0, 75.0), canvas);
        }
        assert_eq!(zoom.viewport().scale(), 4.0);

        let outcome = zoom.wheel(&nodes, 1.0, (100.0, 75.0), canvas, 1.0, Instant::now());
        assert_eq!(outcome.enter_path(), Some(r"C:\Fit"));
    }

    #[test]
    fn auto_enter_has_a_400ms_cooldown_that_survives_a_reset() {
        let canvas = (800.0, 600.0);
        let nodes = vec![directory(r"C:\Fill", rect(0.0, 0.0, 800.0, 600.0))];
        let start = Instant::now();
        let mut zoom = TreemapZoom::new();

        let first = zoom.wheel(&nodes, 1.0, (400.0, 300.0), canvas, 1.0, start);
        assert_eq!(first.enter_path(), Some(r"C:\Fill"));

        zoom.reset_view();
        assert!(!zoom.is_zoomed());

        let blocked = zoom.wheel(
            &nodes,
            1.0,
            (400.0, 300.0),
            canvas,
            1.0,
            start + Duration::from_millis(399),
        );
        assert_eq!(blocked.enter_path(), None, "cooldown must block inertia");
        assert!(blocked.changed());

        let allowed = zoom.wheel(
            &nodes,
            1.0,
            (400.0, 300.0),
            canvas,
            1.0,
            start + Duration::from_millis(400),
        );
        assert_eq!(allowed.enter_path(), Some(r"C:\Fill"));
    }

    #[test]
    fn files_and_aggregates_never_auto_enter() {
        let canvas = (800.0, 600.0);
        let layouts = [
            vec![node(
                r"C:\file.bin",
                false,
                rect(0.0, 0.0, 800.0, 600.0),
                0,
                Vec::new(),
            )],
            vec![node(
                r"C:\Root::<tail:1>",
                true,
                rect(0.0, 0.0, 800.0, 600.0),
                5,
                Vec::new(),
            )],
        ];

        for nodes in layouts {
            let mut zoom = TreemapZoom::new();
            for _ in 0..8 {
                let outcome = zoom.wheel(&nodes, 3.0, (400.0, 300.0), canvas, 1.0, Instant::now());
                assert_eq!(outcome.enter_path(), None);
            }
            assert_eq!(zoom.viewport().scale(), 4.0);
        }
    }

    #[test]
    fn only_upward_lines_can_enter_and_max_scale_still_enters() {
        let canvas = (800.0, 600.0);
        let nodes = vec![directory(r"C:\Fill", rect(0.0, 0.0, 800.0, 600.0))];
        let now = Instant::now();

        let mut zoom = TreemapZoom::new();
        for _ in 0..8 {
            zoom.zoom_center(3.0, canvas);
        }
        assert_eq!(zoom.viewport().scale(), 4.0);
        assert_eq!(
            zoom.wheel(&nodes, 0.0, (400.0, 300.0), canvas, 1.0, now)
                .enter_path(),
            None
        );
        assert_eq!(
            zoom.wheel(&nodes, -1.0, (400.0, 300.0), canvas, 1.0, now)
                .enter_path(),
            None
        );

        let mut zoom = TreemapZoom::new();
        for _ in 0..8 {
            zoom.zoom_center(3.0, canvas);
        }
        let at_max = zoom.wheel(&nodes, 1.0, (400.0, 300.0), canvas, 1.0, now);
        assert!(!at_max.changed(), "already at the 4.0 ceiling");
        assert_eq!(at_max.enter_path(), Some(r"C:\Fill"));
    }

    #[test]
    fn keyboard_zoom_centers_and_never_auto_enters() {
        let canvas = (800.0, 600.0);
        let mut zoom = TreemapZoom::new();

        assert!(zoom.zoom_center(1.0, canvas));
        assert!((zoom.viewport().scale() - 1.12).abs() < 0.0001);
        assert!(zoom.zoom_center(-1.0, canvas));
        assert_eq!(zoom.viewport().scale(), 1.0);
        assert_eq!(
            zoom.viewport().project(rect(0.0, 0.0, 10.0, 10.0)),
            rect(0.0, 0.0, 10.0, 10.0)
        );

        for _ in 0..8 {
            zoom.zoom_center(3.0, canvas);
        }
        assert!(!zoom.zoom_center(1.0, canvas));
    }

    fn zoomed(scale: f32, origin: (f32, f32)) -> TreemapZoom {
        let mut zoom = TreemapZoom::new();
        zoom.viewport = TreemapViewport { scale, origin };
        zoom
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 0.001,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn an_identity_view_rejects_panning_and_invalid_deltas() {
        let canvas = (800.0, 600.0);
        let mut zoom = TreemapZoom::new();
        assert!(!zoom.begin_pan((300.0, 200.0)));
        assert!(!zoom.is_panning());
        assert!(!zoom.pan_to((320.0, 200.0), canvas));
        assert!(!zoom.pan_view(80.0, 0.0, canvas));

        let mut viewport = TreemapViewport::default();
        assert!(!viewport.pan_by(120.0, 90.0, canvas));
        assert_eq!(viewport, TreemapViewport::default());

        let mut viewport = zoomed(2.0, (100.0, 100.0)).viewport;
        assert!(!viewport.pan_by(f32::NAN, 0.0, canvas));
        assert!(!viewport.pan_by(0.0, f32::INFINITY, canvas));
        assert_eq!(viewport, zoomed(2.0, (100.0, 100.0)).viewport);
    }

    #[test]
    fn dragging_moves_the_projected_content_one_to_one_on_screen() {
        let canvas = (800.0, 600.0);
        let mut zoom = zoomed(2.0, (100.0, 100.0));
        let tile = rect(200.0, 200.0, 100.0, 100.0);
        let before = zoom.viewport().project(tile);

        assert!(zoom.begin_pan((300.0, 300.0)));
        assert!(zoom.pan_to((350.0, 300.0), canvas));
        let after = zoom.viewport().project(tile);

        assert_close(after.x - before.x, 50.0);
        assert_close(after.y - before.y, 0.0);
        assert_close(zoom.viewport().origin.0, 75.0);
    }

    #[test]
    fn each_pan_step_is_measured_from_the_previous_cursor() {
        let canvas = (800.0, 600.0);
        let mut stepwise = zoomed(2.0, (200.0, 200.0));
        assert!(stepwise.begin_pan((100.0, 100.0)));
        assert!(
            !stepwise.begin_pan((500.0, 500.0)),
            "a second press must not move the anchor"
        );
        assert!(stepwise.pan_to((110.0, 100.0), canvas));
        assert!(stepwise.pan_to((120.0, 100.0), canvas));
        let stepped = stepwise.viewport();

        let mut one_jump = zoomed(2.0, (200.0, 200.0));
        assert!(one_jump.begin_pan((100.0, 100.0)));
        assert!(one_jump.pan_to((120.0, 100.0), canvas));

        assert_close(stepped.origin.0, one_jump.viewport().origin.0);
        assert!(!stepwise.pan_to((120.0, 100.0), canvas));
        assert!(!stepwise.pan_to((120.0, 100.0), canvas));
        assert_close(stepwise.viewport().origin.0, stepped.origin.0);
    }

    #[test]
    fn overpanning_in_every_direction_keeps_the_canvas_covered() {
        let canvases = [(800.0, 600.0), (4000.0, 2000.0), (1.0, 1.0), (320.0, 100.0)];
        for canvas in canvases {
            for (dx, dy) in [(9000.0, 0.0), (-9000.0, 0.0), (0.0, 9000.0), (0.0, -9000.0)] {
                let mut zoom = zoomed(2.0, (canvas.0 * 0.25, canvas.1 * 0.25));
                let start = (canvas.0 / 2.0, canvas.1 / 2.0);
                assert!(zoom.begin_pan(start));
                assert!(zoom.pan_to((start.0 + dx, start.1 + dy), canvas));
                assert_canvas_is_covered(&zoom.viewport(), canvas);
            }
        }
    }

    #[test]
    fn a_shrunken_canvas_clamps_an_existing_pan_origin() {
        let mut zoom = zoomed(2.0, (300.0, 200.0));
        assert!(zoom.begin_pan((400.0, 300.0)));
        assert!(zoom.pan_to((500.0, 300.0), (800.0, 600.0)));

        zoom.clamp((320.0, 100.0));

        assert_canvas_is_covered(&zoom.viewport(), (320.0, 100.0));
    }

    #[test]
    fn panning_does_not_move_the_world_point_under_the_wheel() {
        let canvas = (800.0, 600.0);
        let mut zoom = zoomed(2.0, (100.0, 100.0));
        assert!(zoom.begin_pan((300.0, 300.0)));
        assert!(zoom.pan_to((350.0, 320.0), canvas));

        let cursor = (400.0, 250.0);
        let world = zoom.viewport().unproject_point(cursor);
        let outcome = zoom.wheel(&[], 1.0, cursor, canvas, 1.0, Instant::now());
        assert!(outcome.changed());
        let projected = zoom.viewport().project_point(world);
        assert_close(projected.0, cursor.0);
        assert_close(projected.1, cursor.1);
    }

    #[test]
    fn zooming_back_to_one_ends_panning_and_resets_the_origin() {
        let canvas = (800.0, 600.0);
        let mut zoom = zoomed(2.0, (100.0, 100.0));
        assert!(zoom.begin_pan((300.0, 300.0)));
        assert!(zoom.pan_to((360.0, 340.0), canvas));
        assert!(zoom.is_panning());

        for _ in 0..8 {
            zoom.wheel(&[], -3.0, (400.0, 300.0), canvas, 1.0, Instant::now());
        }

        assert_eq!(zoom.viewport().scale(), 1.0);
        assert_eq!(zoom.viewport().origin, (0.0, 0.0));
        assert!(!zoom.is_panning());
    }

    #[test]
    fn reset_ends_panning_and_a_finished_pan_is_idempotent() {
        let canvas = (800.0, 600.0);
        let mut zoom = zoomed(2.0, (100.0, 100.0));
        assert!(zoom.begin_pan((300.0, 300.0)));
        assert!(zoom.pan_to((320.0, 300.0), canvas));
        assert!(zoom.end_pan());
        assert!(!zoom.end_pan());
        assert!(!zoom.pan_to((360.0, 300.0), canvas));

        assert!(zoom.begin_pan((300.0, 300.0)));
        zoom.reset_view();
        assert!(!zoom.is_panning());
        assert!(!zoom.is_zoomed());
        assert!(!zoom.pan_to((360.0, 300.0), canvas));
    }

    #[test]
    fn keyboard_zoom_back_to_identity_also_ends_panning() {
        let canvas = (800.0, 600.0);
        let mut zoom = zoomed(2.0, (100.0, 100.0));
        assert!(zoom.begin_pan((300.0, 300.0)));
        assert!(zoom.pan_to((350.0, 320.0), canvas));
        assert!(zoom.is_panning());

        for _ in 0..8 {
            zoom.zoom_center(-3.0, canvas);
        }

        assert_eq!(zoom.viewport().scale(), 1.0);
        assert_eq!(zoom.viewport().origin, (0.0, 0.0));
        assert!(!zoom.is_panning());
    }

    #[test]
    fn keyboard_pan_moves_the_viewport_right_by_moving_content_left() {
        let canvas = (1000.0, 800.0);
        let mut zoom = zoomed(2.0, (200.0, 200.0));
        let before = zoom.viewport().project_point((300.0, 300.0));

        assert!(zoom.pan_view(canvas.0 * 0.1, 0.0, canvas));
        let after = zoom.viewport().project_point((300.0, 300.0));

        assert_close(before.0 - after.0, canvas.0 * 0.1);
        assert_close(before.1, after.1);
        assert!(zoom.pan_view(0.0, canvas.1 * 0.1, canvas));
        let after_vertical = zoom.viewport().project_point((300.0, 300.0));
        assert_close(after_vertical.0, after.0);
        assert_close(after_vertical.1, before.1 - canvas.1 * 0.1);
    }

    #[test]
    fn zoom_percent_is_ceiled_and_never_reports_identity() {
        let mut zoom = TreemapZoom::new();
        assert_eq!(zoom.zoom_percent(), None);

        zoom.viewport.scale = 1.12;
        assert_eq!(zoom.zoom_percent(), Some(112));

        zoom.viewport.scale = 4.0;
        assert_eq!(zoom.zoom_percent(), Some(400));

        zoom.viewport.scale = 1.0001;
        assert_eq!(zoom.zoom_percent(), Some(101));

        zoom.viewport.scale = 1.125;
        assert_eq!(zoom.zoom_percent(), Some(113));
    }
}
