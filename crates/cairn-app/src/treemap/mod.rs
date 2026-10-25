// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Drive disk space Treemap: view rendering and interaction, view state and
//! incremental scan ingestion, directory history, paint geometry, category
//! palette, viewport zoom, and the pre-built volume index loader.

pub(crate) mod navigation;
pub(crate) mod paint;
pub(crate) mod palette;
pub(crate) mod state;
pub(crate) mod view;
pub(crate) mod volume_index;
pub(crate) mod zoom;
