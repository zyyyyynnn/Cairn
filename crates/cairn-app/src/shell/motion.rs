// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Shared motion tokens. A duration names the interaction family, so one
//! surface cannot drift from the others that replace or reveal the same kind
//! of content.

use std::time::Duration;

/// A pane moving to a new width while its content stays in place.
pub(crate) const SIDEBAR: Duration = Duration::from_millis(200);

/// A transient toast entering or leaving.
pub(crate) const TOAST: Duration = Duration::from_millis(140);

/// A mask or overlay dissolving into the revealed content.
pub(crate) const MASK_DISSOLVE: Duration = Duration::from_millis(150);

/// How long a dissolved mask stays mounted after [`MASK_DISSOLVE`] has run out,
/// so the element leaves the tree on a frame the animation cannot be seen on.
pub(crate) const MASK_DISSOLVE_CLEANUP: Duration = Duration::from_millis(160);
