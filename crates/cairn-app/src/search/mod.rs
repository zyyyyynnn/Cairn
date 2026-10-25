// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Search entry: the query input, the search bar view, query filters and the
//! orchestration that turns a submitted query into a service session.

pub(crate) mod bar_view;
pub(crate) mod filters;
pub(crate) mod input;
pub(crate) mod query_orchestration;
