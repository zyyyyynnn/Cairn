// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Drive disk space Treemap: canvas rendering and interaction (`view`), the path
//! band and breadcrumb (`path_band`), navigation and scan lifecycle (`scan`),
//! block context menus (`context_menu`), view state and incremental scan
//! ingestion (`state`), directory history and path semantics (`navigation`),
//! paint geometry and text measurement (`paint`), category palette (`palette`)
//! and viewport zoom (`zoom`).

pub(crate) mod context_menu;
pub(crate) mod navigation;
pub(crate) mod paint;
pub(crate) mod palette;
pub(crate) mod path_band;
pub(crate) mod path_input;
pub(crate) mod scan;
pub(crate) mod state;
pub(crate) mod view;
pub(crate) mod zoom;
