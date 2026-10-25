// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! `cairn-client`: Async Named Pipe client library for Cairn GUI and CLI frontends.

pub mod client;

pub use cairn_protocol::*;
pub use client::{CairnClient, ClientError};
