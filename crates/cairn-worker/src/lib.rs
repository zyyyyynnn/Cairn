// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Subprocess property and content extraction library for `CairnWorker.exe`.

pub mod isolation;
pub mod property_extract;
#[cfg(windows)]
mod win_properties;
