// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Flow state shared by the installer and uninstaller. Pure decisions live here so they can be
//! tested without a window or an MSI.

use std::cmp::Ordering;
use std::path::PathBuf;

pub use crate::install_path::{validate_install_path_under, PathProblem};
use crate::PRODUCT_VERSION;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Launch {
    Setup,
    Uninstall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    License,
    Location,
    Installing,
    Finished,
    Failed,
    Maintenance,
    Uninstall,
    Uninstalling,
    Uninstalled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    RemovingOld,
    Preparing,
    Copying,
    Service,
    Finishing,
}

impl Screen {
    /// Whether this page offers a Cancel action. The pages that are running the MSI do not:
    /// closing the window mid-transaction would leave the machine half-installed.
    pub fn can_cancel(self) -> bool {
        !matches!(self, Screen::Installing | Screen::Uninstalling)
    }
}

impl Stage {
    pub fn message_key(self) -> &'static str {
        match self {
            Stage::RemovingOld => "progress_removing_old",
            Stage::Preparing => "progress_preparing",
            Stage::Copying => "progress_copying",
            Stage::Service => "progress_service",
            Stage::Finishing => "progress_finishing",
        }
    }

    /// Maps an MSI action to the one sentence a person needs. Unknown actions keep the current
    /// stage instead of exposing installer internals.
    pub fn from_action(action: &str) -> Option<Self> {
        let action = action.to_ascii_lowercase();
        if action.contains("removeexistingproducts") {
            Some(Stage::RemovingOld)
        } else if action.contains("installfiles") || action.contains("copy") {
            Some(Stage::Copying)
        } else if action.contains("service") || action.contains("startservices") {
            Some(Stage::Service)
        } else if action.contains("registerproduct")
            || action.contains("publishproduct")
            || action.contains("installfinalize")
        {
            Some(Stage::Finishing)
        } else if action.contains("installvalidate")
            || action.contains("installinitialize")
            || action.contains("costinitialize")
        {
            Some(Stage::Preparing)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledProduct {
    pub version: String,
    pub install_dir: PathBuf,
    pub product_code: Option<String>,
}

impl InstalledProduct {
    pub fn is_newer_than_package(&self) -> bool {
        compare_versions(&self.version, PRODUCT_VERSION) == Ordering::Greater
    }

    pub fn is_same_as_package(&self) -> bool {
        compare_versions(&self.version, PRODUCT_VERSION) == Ordering::Equal
    }
}

/// What the first screen should be, given what the machine already has.
pub fn initial_screen(
    launch: Launch,
    installed: Option<&InstalledProduct>,
    legacy_burn: bool,
) -> Screen {
    if launch == Launch::Uninstall {
        return if installed.is_some() {
            Screen::Uninstall
        } else {
            Screen::Failed
        };
    }

    match installed {
        None => Screen::License,
        // The 0.1.x Burn package predates this installer and must be removed before the new MSI
        // can own the machine. Its version is allowed to look newer than this work line.
        Some(_) if legacy_burn => Screen::License,
        Some(product)
            if product
                .product_code
                .as_deref()
                .is_some_and(|code| !code.trim().is_empty())
                && (product.is_newer_than_package() || product.is_same_as_package()) =>
        {
            Screen::Maintenance
        }
        Some(_) => Screen::License,
    }
}

/// The uninstall flow names the same failures in uninstall terms. A person who came to remove
/// Cairn must not read the installer's wording, and the wording must not claim that an
/// installation was attempted. The install-side key is the input; the uninstall-side key is what
/// the failure page shows.
pub fn uninstall_error_key(install_key: &str) -> &'static str {
    match install_key {
        "error_msi_cancelled" => "error_uninstall_msi_cancelled",
        "error_msi_busy" => "error_uninstall_msi_busy",
        "error_msi_platform" => "error_uninstall_msi_platform",
        "error_msi_fatal" => "error_uninstall_msi_fatal",
        "error_not_installed" => "error_uninstall_not_installed",
        _ => "error_uninstall_msi_unknown",
    }
}

/// Validates a folder typed or picked by the user. It rejects only paths that cannot be a sensible
/// machine-wide product folder; an existing non-empty folder is allowed because upgrades reuse it.
pub fn validate_install_path(raw: &str) -> Result<PathBuf, PathProblem> {
    validate_install_path_under(raw, &program_files_root())
}

/// The machine-wide Program Files root, from the OS rather than a user environment variable.
pub fn program_files_root() -> PathBuf {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Shell::{SHGetFolderPathW, CSIDL_PROGRAM_FILES};

        let mut buffer = [0u16; 260];
        if unsafe { SHGetFolderPathW(None, CSIDL_PROGRAM_FILES as i32, None, 0, &mut buffer) }
            .is_ok()
        {
            let length = buffer
                .iter()
                .position(|&unit| unit == 0)
                .unwrap_or(buffer.len());
            if length > 0 {
                return PathBuf::from(String::from_utf16_lossy(&buffer[..length]));
            }
        }
    }
    PathBuf::from(r"C:\Program Files")
}

/// The machine-wide default. The MSI's `ProgramFiles64Folder` and this value both come from the
/// OS-known folder, not from an environment variable a standard user can set.
pub fn default_install_dir() -> PathBuf {
    program_files_root().join("Cairn")
}

/// Numeric version ordering for the `a.b.c` versions the MSI accepts. Missing components count as
/// zero; a non-numeric suffix is ignored for ordering.
pub fn compare_versions(left: &str, right: &str) -> Ordering {
    let left = version_parts(left);
    let right = version_parts(right);
    let len = left.len().max(right.len());
    for ix in 0..len {
        let a = left.get(ix).copied().unwrap_or(0);
        let b = right.get(ix).copied().unwrap_or(0);
        match a.cmp(&b) {
            Ordering::Equal => continue,
            other => return other,
        }
    }
    Ordering::Equal
}

fn version_parts(version: &str) -> Vec<u64> {
    version
        .split(['.', '-', '+'])
        .map(|part| {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().unwrap_or(0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        compare_versions, initial_screen, uninstall_error_key, validate_install_path,
        InstalledProduct, Launch, PathProblem, Screen,
    };
    use std::cmp::Ordering;
    use std::path::PathBuf;

    fn product(version: &str) -> InstalledProduct {
        InstalledProduct {
            version: version.to_string(),
            install_dir: PathBuf::from(r"C:\Program Files\Cairn"),
            product_code: Some("{00000000-0000-0000-0000-000000000000}".to_string()),
        }
    }

    #[test]
    fn a_registration_without_a_product_code_is_repaired() {
        let mut damaged = product(env!("CARGO_PKG_VERSION"));
        damaged.product_code = None;
        assert_eq!(
            initial_screen(Launch::Setup, Some(&damaged), false),
            Screen::License
        );
    }

    #[test]
    fn a_fresh_machine_starts_at_the_license() {
        assert_eq!(initial_screen(Launch::Setup, None, false), Screen::License);
    }

    #[test]
    fn a_newer_or_equal_install_opens_maintenance() {
        assert_eq!(
            initial_screen(Launch::Setup, Some(&product("9.0.0")), false),
            Screen::Maintenance
        );
        assert_eq!(
            initial_screen(
                Launch::Setup,
                Some(&product(env!("CARGO_PKG_VERSION"))),
                false
            ),
            Screen::Maintenance
        );
    }

    #[test]
    fn an_older_install_updates_and_a_legacy_burn_install_is_migrated() {
        assert_eq!(
            initial_screen(Launch::Setup, Some(&product("0.0.1")), false),
            Screen::License
        );
        assert_eq!(
            initial_screen(Launch::Setup, Some(&product("0.1.2")), true),
            Screen::License,
            "the legacy Burn product must be removable even when its version is higher"
        );
    }

    #[test]
    fn uninstall_without_a_product_reports_the_missing_install() {
        assert_eq!(
            initial_screen(Launch::Uninstall, None, false),
            Screen::Failed
        );
        assert_eq!(
            initial_screen(Launch::Uninstall, Some(&product("0.1.0")), false),
            Screen::Uninstall
        );
    }

    #[test]
    fn version_order_is_numeric_not_lexical() {
        assert_eq!(compare_versions("0.1.2", "0.1.10"), Ordering::Less);
        assert_eq!(compare_versions("1.0", "1.0.0"), Ordering::Equal);
        assert_eq!(compare_versions("2.0.0", "1.9.9"), Ordering::Greater);
    }

    #[test]
    fn invalid_install_paths_are_rejected_with_a_specific_reason() {
        assert_eq!(validate_install_path("  "), Err(PathProblem::Empty));
        assert_eq!(
            validate_install_path(r"Program Files\Cairn"),
            Err(PathProblem::NotAbsolute)
        );
        assert_eq!(
            validate_install_path(r"\\server\share\Cairn"),
            Err(PathProblem::Network)
        );
        assert_eq!(validate_install_path(r"C:\"), Err(PathProblem::Root));
        assert_eq!(
            validate_install_path(r"C:\Windows\Cairn"),
            Err(PathProblem::System)
        );
        assert_eq!(
            validate_install_path(r"C:\Program Files"),
            Err(PathProblem::System)
        );
        assert_eq!(
            validate_install_path(r"C:\Cairn\bad?name"),
            Err(PathProblem::Invalid)
        );
        assert_eq!(
            validate_install_path(r"C:\Program Files\..\Windows"),
            Err(PathProblem::Invalid)
        );
        assert_eq!(
            validate_install_path(r"C:\Program Files\Cairn;C:\Tools"),
            Err(PathProblem::Invalid)
        );
        assert_eq!(
            validate_install_path(r"C:\Program Files\Cairn."),
            Err(PathProblem::Invalid)
        );
        assert_eq!(
            validate_install_path(r"C:\Users\Public\Cairn"),
            Err(PathProblem::OutsideProgramFiles)
        );
    }

    #[test]
    fn a_normal_program_files_folder_is_accepted_and_normalized() {
        assert_eq!(
            validate_install_path(r"C:\Program Files\Cairn\"),
            Ok(PathBuf::from(r"C:\Program Files\Cairn"))
        );
        assert_eq!(
            validate_install_path(r"C:\Program Files\Vendor\Cairn"),
            Ok(PathBuf::from(r"C:\Program Files\Vendor\Cairn"))
        );
    }

    /// The uninstall page never borrows the installer's sentences: a person who came to remove
    /// Cairn must read about the removal, and every failure the uninstall can hit has its own key.
    #[test]
    fn the_uninstall_flow_names_its_failures_in_uninstall_terms() {
        for (install_key, uninstall_key) in [
            ("error_msi_cancelled", "error_uninstall_msi_cancelled"),
            ("error_msi_busy", "error_uninstall_msi_busy"),
            ("error_msi_platform", "error_uninstall_msi_platform"),
            ("error_msi_fatal", "error_uninstall_msi_fatal"),
            ("error_not_installed", "error_uninstall_not_installed"),
            ("error_msi_unknown", "error_uninstall_msi_unknown"),
        ] {
            assert_eq!(uninstall_error_key(install_key), uninstall_key);
        }
    }

    /// An install-only failure has no uninstall wording of its own; it falls back to the generic
    /// uninstall sentence rather than leaking the installer's one.
    #[test]
    fn an_install_only_failure_falls_back_to_the_uninstall_sentence() {
        assert_eq!(
            uninstall_error_key("error_legacy_failed"),
            "error_uninstall_msi_unknown"
        );
        assert_eq!(
            uninstall_error_key("error_no_payload"),
            "error_uninstall_msi_unknown"
        );
    }
}
