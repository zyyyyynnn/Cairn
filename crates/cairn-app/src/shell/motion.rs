// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Shared motion tokens. A duration names the interaction family, so one
//! surface cannot drift from the others that replace or reveal the same kind
//! of content.

use std::time::Duration;

/// A pane moving to a new width while its content stays in place.
pub(crate) const SIDEBAR: Duration = Duration::from_millis(200);

/// A content panel replacing or revealing the result surface.
pub(crate) const PANEL_REVEAL: Duration = Duration::from_millis(260);

/// A transient toast entering or leaving.
pub(crate) const TOAST: Duration = Duration::from_millis(140);
