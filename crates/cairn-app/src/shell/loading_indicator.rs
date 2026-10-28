// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The status band's loading indicator.
//!
//! A long wait must still look alive: a frozen mark reads as a broken control. The mark turns at
//! a bounded frame rate, because every animated frame rebuilds and repaints the whole window; the
//! rest of a long loading surface is still, so the mark is the only frame driver while a build
//! runs.

use gpui_kit::component::{Icon, IconName};
use gpui_kit::gpui::*;
use gpui_kit::gpui::{Animation, AnimationExt};
use std::time::Duration;

/// One turn of the indicator.
const SPIN_PERIOD: Duration = Duration::from_millis(1000);
/// The indicator's repaint ceiling. Thirty frames per second is the point where a one-second turn
/// reads as motion rather than a slideshow; the rest of the loading surface is still, so this is
/// the only frame driver while a build runs.
const SPIN_MAX_FPS: f32 = 30.0;

/// The status band's loading mark.
///
/// It is the size of the refresh control's own icon, so the band does not grow while it is shown.
pub fn loading_indicator(animate: bool) -> AnyElement {
    let icon = Icon::new(IconName::LoaderCircle).size_3();
    if !animate {
        return icon.into_any_element();
    }
    icon.with_animation(
        "status-loading",
        Animation::new(SPIN_PERIOD)
            .repeat_synced()
            .with_max_fps(SPIN_MAX_FPS),
        |icon, delta| icon.transform(Transformation::rotate(percentage(delta))),
    )
    .into_any_element()
}
