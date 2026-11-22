// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Cairn's privileged maintenance surface: the GPUI installer and uninstaller.
//!
//! The MSI remains the transaction engine. This crate owns the visible flow, the elevated shell,
//! and the few things an MSI cannot express well: one product-owned entry in Apps & features, a
//! bilingual appearance that follows Windows, a folder picker, and default cleanup of the data the
//! MSI does not own.

#[cfg(feature = "gui")]
pub(crate) mod app;
#[cfg(feature = "gui")]
pub(crate) mod i18n;
#[cfg(feature = "gui")]
pub mod install_path;
#[cfg(feature = "gui")]
pub(crate) mod installed;
#[cfg(feature = "gui")]
pub(crate) mod model;
#[cfg(feature = "gui")]
pub(crate) mod msi;
/// The payload format is a contract between the packaging step that writes it and this crate
/// that reads it, so both ends stay published: `xtask` packaging calls the writer, and the setup
/// binary and the packaging test call the reader.
pub mod payload;
#[cfg(feature = "gui")]
pub(crate) mod platform;
#[cfg(feature = "gui")]
mod preview;
#[cfg(feature = "gui")]
pub mod quiet;
#[cfg(feature = "gui")]
pub(crate) mod registry;
#[cfg(feature = "gui")]
pub mod run;
#[cfg(feature = "gui")]
pub(crate) mod view;

/// The product upgrade code the MSI and the uninstaller agree on.
pub const MSI_UPGRADE_CODE: &str = "4C65A8E1-7F89-4D12-9A4B-3D8F42D8E6C0";

/// The six product executables: the MSI installs five, while `CairnSetup.exe` is the outer
/// installer that packaging stages and the forced cleanup removes if an old install left it.
/// The packaging step and the uninstaller share this list.
pub const PRODUCT_FILES: [&str; 6] = [
    "Cairn.exe",
    "CairnService.exe",
    "CairnCli.exe",
    "CairnWorker.exe",
    "CairnSetup.exe",
    "CairnUninstall.exe",
];

/// The product version this setup binary installs.
#[cfg(feature = "gui")]
pub(crate) const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The license text shown from the consent page and shipped in the repository root.
#[cfg(feature = "gui")]
pub(crate) const LICENSE_TEXT: &str = include_str!("../../../LICENSE");
