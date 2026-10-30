// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Shared product surface for Cairn's windows.
//!
//! The desktop shell and the installer are separate binaries but one product. This crate owns the
//! values that must not drift between them: the grayscale palette repair, the base font and radius,
//! the brand mark, system appearance and system language. It contains no application workflow.

pub mod brand;
pub mod locale;
pub mod theme;
