// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Result table: the fetched-row model with windowed rendering state, and the
//! results view that renders heading, rows and placeholders.

pub(crate) mod columns;
pub(crate) mod header;
pub(crate) mod model;
pub(crate) mod presentation;
pub(crate) mod results_view;
pub(crate) mod rows;
pub(crate) mod snippets;
pub(crate) mod states;
pub(crate) mod surface;
