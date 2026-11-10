// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Process-exit contract between the service and the content worker. A non-zero exit carries the
//! dominant failure kind so the service can decide whether a retry can help; the human-readable
//! reason still travels on stderr for the log.

/// Any failure without a more specific kind.
pub const FAILED: u8 = 1;
/// Opening a file or the index was denied (`ERROR_ACCESS_DENIED`).
pub const ACCESS_DENIED: u8 = 5;
/// Reading a file hit `ERROR_SHARING_VIOLATION`.
pub const SHARING_VIOLATION: u8 = 32;
