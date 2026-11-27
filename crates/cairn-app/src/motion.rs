// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! App-owned motion timing and directional transitions.

use std::time::Duration;

pub use cairn_ui::motion::{MASK_DISSOLVE, MASK_DISSOLVE_CLEANUP, SIDEBAR, TOAST};
use cairn_ui::motion::{flow_enter, flow_exit};
use gpui_kit::base::motion::Transition;

pub const TOAST_INFO_TIMEOUT: Duration = Duration::from_secs(4);
pub const TOAST_ERROR_TIMEOUT: Duration = Duration::from_secs(4);

pub const RESIZE_QUIET: Duration = Duration::from_millis(150);
pub const RESIZE_POLL: Duration = Duration::from_millis(16);
pub const RESIZE_MAX_WAIT: Duration = Duration::from_secs(60);

pub fn sidebar() -> Transition {
    flow_enter(SIDEBAR)
}

pub fn toast_enter() -> Transition {
    flow_enter(TOAST)
}

pub fn toast_exit() -> Transition {
    flow_exit(TOAST)
}

pub fn mask_dissolve() -> Transition {
    flow_exit(MASK_DISSOLVE)
}
