// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Cairn's privileged maintenance surface: the GPUI installer and uninstaller.
//!
//! The MSI remains the transaction engine. This crate owns the visible flow, the elevated shell,
//! and the few things an MSI cannot express well: one product-owned entry in Apps & features, a
//! bilingual appearance that follows Windows, a folder picker, and default cleanup of the data the
//! MSI does not own.

pub(crate) mod app;
pub(crate) mod i18n;
pub mod install_path;
pub(crate) mod installed;
pub(crate) mod model;
pub(crate) mod msi;
/// The payload format is a contract between the packaging step that writes it and this crate
/// that reads it, so both ends stay published: the writer half is what builds the fixture the
/// crate's own round-trip test checks the reader against.
pub mod payload;
pub(crate) mod platform;
mod preview;
pub mod quiet;
pub mod run;
pub(crate) mod view;

/// The product upgrade code the MSI and the uninstaller agree on.
pub(crate) const MSI_UPGRADE_CODE: &str = "4C65A8E1-7F89-4D12-9A4B-3D8F42D8E6C0";

/// The product version this setup binary installs.
pub(crate) const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The license text shown from the consent page and shipped in the repository root.
pub(crate) const LICENSE_TEXT: &str = include_str!("../../../LICENSE");
