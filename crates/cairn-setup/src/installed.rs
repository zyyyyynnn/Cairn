// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! What this PC already has.
//!
//! The MSI's own registration is authoritative for version and product code. The registry values
//! are the fast path and the place the legacy Burn entry is found.

use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
use windows::Win32::System::ApplicationInstallationAndServicing::{
    MsiEnumRelatedProductsW, MsiGetProductInfoW, INSTALLPROPERTY_INSTALLLOCATION,
    INSTALLPROPERTY_VERSIONSTRING,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
    KEY_WOW64_32KEY, KEY_WOW64_64KEY, RRF_RT_REG_SZ,
};

use crate::model::{default_install_dir, InstalledProduct};
use crate::msi::LegacyUninstall;

const PRODUCT_KEY: &str = r"Software\Cairn";
const UNINSTALL_KEYS: [&str; 2] = [
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
    r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
];

/// Reads the installed product from the MSI registration, falling back to the registry values the
/// MSI writes for the custom Apps & features entry.
pub fn installed_product() -> Option<InstalledProduct> {
    if let Some(product_code) = find_product_code() {
        let version = product_info(&product_code, INSTALLPROPERTY_VERSIONSTRING)
            .filter(|value| !value.is_empty())
            .or_else(|| read_hklm_string(PRODUCT_KEY, "Version"))
            .unwrap_or_default();
        let install_dir = product_info(&product_code, INSTALLPROPERTY_INSTALLLOCATION)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| read_hklm_string(PRODUCT_KEY, "InstallFolder").map(PathBuf::from))
            .unwrap_or_else(default_install_dir);
        return Some(InstalledProduct {
            version,
            install_dir,
            product_code: Some(product_code),
        });
    }

    let version = read_hklm_string(PRODUCT_KEY, "Version")?;
    Some(InstalledProduct {
        version,
        install_dir: read_hklm_string(PRODUCT_KEY, "InstallFolder")
            .map(PathBuf::from)
            .unwrap_or_else(default_install_dir),
        product_code: read_hklm_string(PRODUCT_KEY, "ProductCode"),
    })
}

/// The upgrade code in the braced form `MsiEnumRelatedProductsW` requires.
pub(crate) fn upgrade_code_guid() -> String {
    format!("{{{}}}", crate::MSI_UPGRADE_CODE)
}

/// The ProductCode of the installed Cairn MSI, if any. This is what the uninstaller passes to
/// `MsiConfigureProductW`; it is not guessed from a version string.
pub fn find_product_code() -> Option<String> {
    let upgrade = wide(std::ffi::OsStr::new(&upgrade_code_guid()));
    let mut product = [0u16; 39];
    let result = unsafe {
        MsiEnumRelatedProductsW(
            PCWSTR(upgrade.as_ptr()),
            None,
            0,
            windows::core::PWSTR(product.as_mut_ptr()),
        )
    };
    if result != ERROR_SUCCESS.0 {
        return None;
    }
    Some(utf16_until_nul(&product))
}

/// The old Burn bundles registered their own Apps & features entries and launched `CairnSetup.exe`.
/// Those entries cannot be upgraded by the new MSI, so setup removes every one before installing.
/// More than one can exist after a failed migration or a side-by-side 0.1.x install.
pub fn legacy_burn_uninstallers() -> Vec<LegacyUninstall> {
    let mut found = Vec::new();
    for (base, view) in UNINSTALL_KEYS
        .iter()
        .zip([KEY_WOW64_64KEY, KEY_WOW64_32KEY])
    {
        for subkey in subkeys(base, view) {
            let path = format!(r"{base}\{subkey}");
            let display = read_hklm_string(&path, "DisplayName").unwrap_or_default();
            if !display.eq_ignore_ascii_case("Cairn") {
                continue;
            }
            let Some(command) = read_hklm_string(&path, "QuietUninstallString")
                .or_else(|| read_hklm_string(&path, "UninstallString"))
            else {
                continue;
            };
            let lower = command.to_ascii_lowercase();
            if !lower.contains("cairnsetup.exe")
                || lower.contains("cairnuninstall.exe")
                || lower.contains("cairn-uninstall.exe")
            {
                continue;
            }
            let mut parts = split_command_line(&command);
            if parts.is_empty() {
                continue;
            }
            let program = PathBuf::from(parts.remove(0));
            let Some(parent) = program.parent() else {
                continue;
            };
            // The legacy command is about to run with the installer's administrator token. Only
            // accept an executable that still sits in an administrator-owned product folder.
            if crate::model::validate_install_path(&parent.to_string_lossy()).is_err()
                || !program.is_file()
            {
                continue;
            }
            let candidate = LegacyUninstall {
                program,
                args: parts,
            };
            if !found.contains(&candidate) {
                found.push(candidate);
            }
        }
    }
    found
}

fn product_info(product_code: &str, attribute: PCWSTR) -> Option<String> {
    let product = wide(std::ffi::OsStr::new(product_code));
    let mut length = 0u32;
    let status =
        unsafe { MsiGetProductInfoW(PCWSTR(product.as_ptr()), attribute, None, Some(&mut length)) };
    if status != ERROR_SUCCESS.0 && status != 234 {
        // 234 is ERROR_MORE_DATA: the first call reports the required size.
        return None;
    }
    let mut buffer = vec![0u16; length as usize + 1];
    let mut length = buffer.len() as u32;
    let status = unsafe {
        MsiGetProductInfoW(
            PCWSTR(product.as_ptr()),
            attribute,
            Some(windows::core::PWSTR(buffer.as_mut_ptr())),
            Some(&mut length),
        )
    };
    if status != ERROR_SUCCESS.0 {
        return None;
    }
    let length = (length as usize).min(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..length]))
}

fn read_hklm_string(subkey: &str, value: &str) -> Option<String> {
    let subkey = wide(std::ffi::OsStr::new(subkey));
    let value = wide(std::ffi::OsStr::new(value));
    let mut buffer = [0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr() as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let length = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(
        &buffer[..length.min(buffer.len())],
    ))
}

fn subkeys(base: &str, view: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Vec<String> {
    let base_wide = wide(std::ffi::OsStr::new(base));
    let mut key = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(base_wide.as_ptr()),
            None,
            KEY_READ | view,
            &mut key,
        )
    };
    if status != ERROR_SUCCESS {
        return Vec::new();
    }

    let mut names = Vec::new();
    let mut index = 0;
    loop {
        let mut name = [0u16; 256];
        let mut length = name.len() as u32;
        let status = unsafe {
            RegEnumKeyExW(
                key,
                index,
                Some(windows::core::PWSTR(name.as_mut_ptr())),
                &mut length,
                None,
                None,
                None,
                None,
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            break;
        }
        if status != ERROR_SUCCESS {
            break;
        }
        names.push(String::from_utf16_lossy(&name[..length as usize]));
        index += 1;
    }
    unsafe {
        let _ = RegCloseKey(key);
    }
    names
}

fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn utf16_until_nul(buffer: &[u16]) -> String {
    let length = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..length])
}

/// Splits the command string recorded in the old bundle's uninstall entry into a program and its
/// arguments. The syntax is the Windows CreateProcess form, not a shell script.
pub fn split_command_line(input: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for ch in input.chars() {
        match ch {
            '"' => quoted = !quoted,
            ch if ch.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            ch => current.push(ch),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::upgrade_code_guid;

    #[test]
    fn the_upgrade_code_is_braced_for_the_msi_api() {
        let guid = upgrade_code_guid();
        assert!(guid.starts_with('{') && guid.ends_with('}'), "{guid}");
        assert_eq!(guid.len(), 38, "{guid}");
    }

    use super::split_command_line;

    #[test]
    fn an_old_burn_uninstall_command_keeps_a_quoted_program_path() {
        let parts =
            split_command_line(r#""C:\Program Files\Cairn\CairnSetup.exe" /uninstall /quiet"#);
        assert_eq!(parts[0], r"C:\Program Files\Cairn\CairnSetup.exe");
        assert_eq!(&parts[1..], ["/uninstall", "/quiet"]);
    }

    #[test]
    fn an_unquoted_program_path_is_split_at_whitespace() {
        let parts = split_command_line(r"C:\Cairn\CairnSetup.exe /uninstall");
        assert_eq!(parts, [r"C:\Cairn\CairnSetup.exe", "/uninstall"]);
    }
}
