// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The `--quiet` path used by unattended installers. It shares the same extraction,
//! legacy removal, MSI transaction, and cleanup as the window; only the presentation is absent.

use std::path::PathBuf;

use crate::installed;
use crate::model::{default_install_dir, validate_install_path};
use crate::msi::{self, InstallRequest, MsiFailure, MsiSuccess};
use crate::payload;
use crate::platform;

const EXIT_OK: i32 = 0;
const EXIT_ERROR: i32 = 1;
const EXIT_REBOOT: i32 = 3010;

/// Handles the cleanup-helper mode before GPUI, the mutex, or any MSI work starts.
pub fn handle_cleanup_helper(args: &[String]) -> bool {
    let Some(dir) = platform::cleanup_dir_from_args(args) else {
        return false;
    };
    let wait_pid = platform::wait_pid_from_args(args).unwrap_or(0);
    platform::run_install_dir_cleanup(&dir, wait_pid);
    true
}

pub fn quiet_setup() -> i32 {
    let installed = installed::installed_product();
    let legacy = installed::legacy_burn_uninstallers();

    if let Some(product) = &installed {
        if legacy.is_empty() && (product.is_same_as_package() || product.is_newer_than_package()) {
            return EXIT_OK;
        }
    }

    let install_dir = installed
        .as_ref()
        .map(|product| product.install_dir.clone())
        .filter(|path| validate_install_path(&path.to_string_lossy()).is_ok())
        .unwrap_or_else(default_install_dir);
    let log = platform::install_log_path();
    let (msi, payload_dir) = match msi_for_this_build() {
        Ok(value) => value,
        Err(_) => return EXIT_ERROR,
    };

    let request = InstallRequest {
        msi,
        install_dir,
        log: log.clone(),
        legacy,
    };
    let result = msi::install_blocking(&request);
    let _ = std::fs::remove_dir_all(&payload_dir);
    exit_code(result.as_ref().err(), result.as_ref().ok())
}

pub fn quiet_uninstall() -> i32 {
    let installed = installed::installed_product();
    let Some(product) = installed else {
        // An ARP entry can outlive its MSI. Removing the product data is still the useful result.
        let _ = platform::remove_product_data();
        return EXIT_OK;
    };

    let log = platform::uninstall_log_path();
    let cleanup_helper = platform::prepare_install_dir_cleanup();
    let product_codes = match installed::find_product_codes_checked() {
        Ok(codes) => codes,
        Err(_) => return EXIT_ERROR,
    };
    if product_codes.is_empty() {
        return match platform::remove_orphan_registration(&product.install_dir, cleanup_helper) {
            Ok(warnings) => {
                if !warnings.is_empty() {
                    msi::append_cleanup_failures(&log, &warnings);
                }
                EXIT_OK
            }
            Err(critical) => {
                msi::append_cleanup_failures(&log, &[critical]);
                EXIT_ERROR
            }
        };
    }

    let result = msi::uninstall_blocking(&product_codes, &log);
    if result.is_ok() {
        let mut failures = platform::remove_product_data();
        if let Err(error) =
            platform::spawn_install_dir_cleanup(&product.install_dir, cleanup_helper)
        {
            failures.push(format!("install folder: {error}"));
        }
        if !failures.is_empty() {
            msi::append_cleanup_failures(&log, &failures);
        }
    }
    exit_code(result.as_ref().err(), result.as_ref().ok())
}

pub(crate) fn msi_for_this_build() -> Result<(PathBuf, PathBuf), payload::PayloadError> {
    let payload_dir = platform::create_payload_dir().map_err(payload::PayloadError::Io)?;
    let msi = payload_dir.join("cairn.msi");

    let prepared = (|| -> Result<(), payload::PayloadError> {
        // A debug build is allowed to point at a freshly built MSI for UI and integration work.
        // Release builds always use the payload appended to this executable; an elevated process
        // never installs an arbitrary user-writable path.
        #[cfg(debug_assertions)]
        if let Some(path) = std::env::var_os("CAIRN_SETUP_MSI_PATH") {
            let path = PathBuf::from(path);
            if path.is_file() {
                std::fs::copy(&path, &msi).map_err(payload::PayloadError::Io)?;
                return Ok(());
            }
        }

        let current = std::env::current_exe().map_err(payload::PayloadError::Io)?;
        payload::extract_payload(&current, &msi)?;
        Ok(())
    })();

    if let Err(error) = prepared {
        let _ = std::fs::remove_dir_all(&payload_dir);
        return Err(error);
    }
    Ok((msi, payload_dir))
}

fn exit_code(error: Option<&MsiFailure>, success: Option<&MsiSuccess>) -> i32 {
    if error.is_some() {
        return EXIT_ERROR;
    }
    if success.is_some_and(|success| success.restart_required) {
        return EXIT_REBOOT;
    }
    EXIT_OK
}
