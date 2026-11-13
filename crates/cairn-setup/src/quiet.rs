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

pub fn quiet_setup(restart_app: bool, expected_sha256: Option<String>) -> i32 {
    let installed = installed::installed_product();
    let legacy = installed::legacy_burn_uninstallers();

    // The payload is extracted before the up-to-date answer because that answer depends on the
    // build the payload carries: equal versions with different ProductCodes are different builds.
    let (msi, payload_dir) = match msi_for_this_build(expected_sha256.as_deref()) {
        Ok(value) => value,
        Err(_) => return EXIT_ERROR,
    };
    let package_code = msi::package_product_code(&msi);

    if let Some(product) = &installed {
        if legacy.is_empty()
            && (product.is_same_build(package_code.as_deref()) || product.is_newer_than_package())
        {
            let _ = std::fs::remove_dir_all(&payload_dir);
            if restart_app {
                relaunch(&product.install_dir);
            }
            return EXIT_OK;
        }
    }

    let install_dir = installed
        .as_ref()
        .map(|product| product.install_dir.clone())
        .filter(|path| validate_install_path(&path.to_string_lossy()).is_ok())
        .unwrap_or_else(default_install_dir);
    let app_dir = install_dir.clone();
    let log = match platform::install_log_path() {
        Ok(log) => log,
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
    let code = exit_code(result.as_ref().err(), result.as_ref().ok());
    if restart_app {
        relaunch(&app_dir);
    }
    code
}

/// Starts the installed app through the shell so the new process gets the signed-in user's token
/// instead of this elevated installer's. A failed launch leaves the app closed; the user can start
/// it from the Start menu, and the pending-version marker still reports the install result.
fn relaunch(app_dir: &std::path::Path) {
    let exe = app_dir.join("Cairn.exe");
    if exe.is_file() {
        let _ = platform::launch_installed_app(&exe);
    }
}

pub fn quiet_uninstall() -> i32 {
    let installed = installed::installed_product();
    let Some(product) = installed else {
        // An ARP entry can outlive its MSI. Removing the product data is still the useful result.
        let _ = platform::remove_product_data();
        return EXIT_OK;
    };

    let log = match platform::uninstall_log_path() {
        Ok(log) => log,
        Err(_) => return EXIT_ERROR,
    };
    let product_codes = match installed::find_product_codes_checked() {
        Ok(codes) => codes,
        Err(_) => return EXIT_ERROR,
    };
    if product_codes.is_empty() {
        return finish_with_cleanup(&product.install_dir, &[], &log);
    }

    let result = msi::uninstall_blocking(&product_codes, &log);
    if let Err(failure) = &result {
        // A record Windows Installer reports as missing cannot be removed by it again; the file
        // cleanup is the useful result. A damaged cached package is not a reason to delete files
        // unattended: the quiet path reports the failure and leaves the choice to the person.
        if failure.kind == msi::FailureKind::NotInstalled {
            return finish_with_cleanup(&product.install_dir, &product_codes, &log);
        }
    }
    if result.is_ok() {
        let mut failures = platform::remove_product_data();
        if let Err(error) = platform::spawn_install_dir_cleanup(&product.install_dir) {
            failures.push(format!("install folder: {error}"));
        }
        if !failures.is_empty() {
            msi::append_cleanup_failures(&log, &failures);
        }
    }
    exit_code(result.as_ref().err(), result.as_ref().ok())
}

/// Removes the leftover registration after Windows Installer could not. The product set is
/// re-checked under the global installer lock, so a Cairn installed while this process waited is
/// never cleaned up; `expected` is what the original removal started with.
fn finish_with_cleanup(
    install_dir: &std::path::Path,
    expected: &[String],
    log: &std::path::Path,
) -> i32 {
    let cleaned = (|| {
        let _lock = platform::acquire_msi_execution(30_000)
            .ok_or_else(|| "another Windows Installer operation is running".to_string())?;
        let current = installed::find_product_codes_checked()
            .map_err(|error| format!("MSI product enumeration failed: {error}"))?;
        if !installed::all_products_are_expected(&current, expected) {
            return Err("Cairn was installed while the uninstaller was running.".to_string());
        }
        platform::remove_orphan_registration(install_dir)
    })();
    match cleaned {
        Ok(warnings) => {
            if !warnings.is_empty() {
                msi::append_cleanup_failures(log, &warnings);
            }
            EXIT_OK
        }
        Err(critical) => {
            msi::append_cleanup_failures(log, &[critical]);
            EXIT_ERROR
        }
    }
}

pub(crate) fn msi_for_this_build(
    expected_sha256: Option<&str>,
) -> Result<(PathBuf, PathBuf), payload::PayloadError> {
    let payload_dir = platform::create_payload_dir().map_err(payload::PayloadError::Io)?;
    let msi = payload_dir.join("cairn.msi");

    let prepared = (|| -> Result<(), payload::PayloadError> {
        // A debug build is allowed to point at a freshly built MSI for UI and integration work.
        // Release builds always use the payload appended to this executable; an elevated process
        // never installs an arbitrary user-writable path. An update handoff is never overridden.
        #[cfg(debug_assertions)]
        if expected_sha256.is_none() {
            if let Some(path) = std::env::var_os("CAIRN_SETUP_MSI_PATH") {
                let path = PathBuf::from(path);
                if path.is_file() {
                    std::fs::copy(&path, &msi).map_err(payload::PayloadError::Io)?;
                    return Ok(());
                }
            }
        }

        payload::extract_payload_locked(expected_sha256, &msi)?;
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
