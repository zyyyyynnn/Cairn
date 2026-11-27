// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Shared motion timing, travel, and easing helpers across Cairn windows.

use std::time::Duration;

use gpui_kit::base::motion::{Easing, Transition};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::gpui::{App, Pixels, px};

/// A pane moving to a new width while its content stays in place.
pub const SIDEBAR: Duration = Duration::from_millis(200);

/// A transient toast entering or leaving.
pub const TOAST: Duration = Duration::from_millis(140);

/// A mask or overlay dissolving into the revealed content.
pub const MASK_DISSOLVE: Duration = Duration::from_millis(150);

/// How long a dissolved mask stays mounted after [`MASK_DISSOLVE`] completes,
/// so the element leaves the tree on a settled frame.
pub const MASK_DISSOLVE_CLEANUP: Duration = Duration::from_millis(200);

/// Tooltip entrance duration.
pub const TOOLTIP: Duration = Duration::from_millis(150);

/// Tooltip upward travel on entrance.
pub const TOOLTIP_SLIDE: Pixels = px(4.0);

/// Builds an entrance transition using the active theme's enter curve.
pub fn enter(duration: Duration, cx: &App) -> Transition {
    Transition::new(duration).easing(cx.theme().motion.easing_enter.clone())
}

/// Directional curve for content leaving a flow.
pub fn flow_exit(duration: Duration) -> Transition {
    Transition::new(duration)
        .easing(Easing::cubic_bezier(0.4, 0.0, 1.0, 1.0).expect("static flow-exit curve is valid"))
}

/// Directional curve for content settling into a flow.
pub fn flow_enter(duration: Duration) -> Transition {
    Transition::new(duration)
        .easing(Easing::cubic_bezier(0.4, 0.0, 0.2, 1.0).expect("static flow-enter curve is valid"))
}
