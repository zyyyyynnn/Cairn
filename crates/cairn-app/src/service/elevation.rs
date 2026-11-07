// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! One elevation, one place.
//!
//! The window itself stays unprivileged. The only thing that ever asks for an administrator is a
//! change to the machine's service registration, and that change has to happen as one unit: one
//! prompt per step would ask the user three times and could leave the service stopped between two
//! of them. Every elevated command therefore goes through this one function, so the UAC prompt,
//! the wait and the exit-code reading behave identically every time.

use std::path::Path;

#[cfg(windows)]
use crate::win32::{to_wide_literal, win32_code_from_hresult, WIN32_ERROR_USER_CANCELLED};

/// Why an explicit elevation request did not complete.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ElevationError {
    /// The user dismissed the UAC consent dialog. Nothing was started.
    #[error("the elevation request was cancelled at the UAC prompt")]
    Cancelled,
    /// The elevated process could not be started, or its result could not be read.
    #[error("the elevated command did not complete: {message}")]
    Failed {
        /// Win32 last-error code behind the failure, when the failure is one.
        os_error: Option<u32>,
        /// Technical reason for logs and for the status band's detail line.
        message: String,
    },
}

/// Runs this same executable once, elevated, with `parameters`, and waits for it to finish.
///
/// The image is the one this window is already running from, so the consent dialog names Cairn and
/// shows its icon. The path comes from the process itself, never from `PATH` or from an argument,
/// and the elevated copy accepts exactly one action and refuses anything else.
#[cfg(windows)]
pub fn run_elevated_self(parameters: &str) -> Result<u32, ElevationError> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, GetLastError, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let helper_failed =
        |os_error: Option<u32>, message: String| ElevationError::Failed { os_error, message };

    let Ok(executable) = std::env::current_exe() else {
        return Err(helper_failed(
            None,
            "this executable could not be located".to_string(),
        ));
    };

    let wide_file = to_wide_literal(&executable.to_string_lossy());
    let wide_parameters = to_wide_literal(parameters);
    let wide_verb = to_wide_literal("runas");

    let mut exec_info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        // Without the process handle the wait below would be the optimistic "started, therefore
        // running" answer instead of the observed one.
        fMask: SEE_MASK_NOCLOSEPROCESS,
        hwnd: windows::Win32::Foundation::HWND(std::ptr::null_mut()),
        lpVerb: PCWSTR(wide_verb.as_ptr()),
        lpFile: PCWSTR(wide_file.as_ptr()),
        lpParameters: PCWSTR(wide_parameters.as_ptr()),
        lpDirectory: PCWSTR(std::ptr::null()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };

    // SAFETY: the three PCWSTR buffers are live until ShellExecuteExW returns, cbSize is the size
    // of the struct, and every other field is either null or a plain value.
    if let Err(err) = unsafe { ShellExecuteExW(&mut exec_info) } {
        let os_error = win32_code_from_hresult(err.code().0);
        if os_error == Some(WIN32_ERROR_USER_CANCELLED) {
            return Err(ElevationError::Cancelled);
        }
        return Err(helper_failed(os_error, err.message()));
    }

    let process = exec_info.hProcess;
    if process.is_invalid() {
        return Err(helper_failed(
            None,
            "the elevated command returned no process handle".to_string(),
        ));
    }

    // SAFETY: process is the handle ShellExecuteExW returned and is closed after the wait and the
    // exit-code read below.
    let wait = unsafe { WaitForSingleObject(process, INFINITE) };
    if wait != WAIT_OBJECT_0 {
        // SAFETY: GetLastError reads the calling thread's last error set by the wait, and the
        // process handle is paired with the successful ShellExecuteExW above.
        let last_error = unsafe { GetLastError().0 };
        unsafe {
            let _ = CloseHandle(process);
        }
        return Err(helper_failed(
            Some(last_error),
            format!("waiting for the elevated command did not complete (wait code {wait:?})"),
        ));
    }

    let mut exit_code = 0u32;
    // SAFETY: process is an exited process handle and exit_code is a valid out pointer.
    let read = unsafe { GetExitCodeProcess(process, &mut exit_code) };
    // SAFETY: paired with the successful ShellExecuteExW that produced this handle.
    unsafe {
        let _ = CloseHandle(process);
    }
    read.map_err(|err| {
        helper_failed(
            win32_code_from_hresult(err.code().0),
            format!("the elevated command exited, but its exit code could not be read: {err}"),
        )
    })?;

    Ok(exit_code)
}

#[cfg(not(windows))]
pub fn run_elevated_self(_parameters: &str) -> Result<u32, ElevationError> {
    Err(ElevationError::Failed {
        os_error: None,
        message: "elevating a command is only available on Windows".to_string(),
    })
}

/// Holds the verified installer with write and delete denied, so the same-user process that
/// requested the update cannot replace it between the checksum and the UAC prompt.
#[cfg(windows)]
pub fn lock_verified_installer(
    installer: &Path,
    expected_sha256: &str,
) -> Result<std::fs::File, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(installer)
        .map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err("the installer changed after verification".to_string());
    }
    Ok(file)
}

#[cfg(not(windows))]
pub fn lock_verified_installer(
    _installer: &Path,
    _expected_sha256: &str,
) -> Result<std::fs::File, String> {
    Err("locking an installer is only available on Windows".to_string())
}

fn installer_parameters(expected_sha256: &str) -> String {
    format!("/quiet /restart-app /expected-sha256 {expected_sha256}")
}

/// Starts a downloaded Cairn installer with the silent, restart-on-finish contract. The caller
/// holds the verified file handle through this call, so the UAC prompt cannot be raced. The setup
/// then verifies its own hash and extracts from a handle that denies replacement.
#[cfg(windows)]
pub fn launch_elevated_installer(
    installer: &Path,
    expected_sha256: &str,
) -> Result<(), ElevationError> {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    if !installer.is_file() {
        return Err(ElevationError::Failed {
            os_error: None,
            message: format!(
                "the downloaded installer is missing: {}",
                installer.display()
            ),
        });
    }

    let wide_file = to_wide_literal(&installer.to_string_lossy());
    let wide_parameters = to_wide_literal(&installer_parameters(expected_sha256));
    let wide_verb = to_wide_literal("runas");
    let mut exec_info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: 0,
        hwnd: windows::Win32::Foundation::HWND(std::ptr::null_mut()),
        lpVerb: PCWSTR(wide_verb.as_ptr()),
        lpFile: PCWSTR(wide_file.as_ptr()),
        lpParameters: PCWSTR(wide_parameters.as_ptr()),
        lpDirectory: PCWSTR(std::ptr::null()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };

    // SAFETY: every PCWSTR points at a live null-terminated buffer for the duration of the call.
    if let Err(err) = unsafe { ShellExecuteExW(&mut exec_info) } {
        let os_error = win32_code_from_hresult(err.code().0);
        if os_error == Some(WIN32_ERROR_USER_CANCELLED) {
            return Err(ElevationError::Cancelled);
        }
        return Err(ElevationError::Failed {
            os_error,
            message: err.message(),
        });
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn launch_elevated_installer(
    _installer: &Path,
    _expected_sha256: &str,
) -> Result<(), ElevationError> {
    Err(ElevationError::Failed {
        os_error: None,
        message: "starting the installer is only available on Windows".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_installer_handoff_passes_the_verified_hash() {
        assert_eq!(
            installer_parameters("abc123"),
            "/quiet /restart-app /expected-sha256 abc123"
        );
    }

    #[test]
    fn a_locked_installer_must_still_match_its_hash() {
        let temp = tempfile::tempdir().expect("temp dir");
        let installer = temp.path().join("CairnSetup.exe");
        std::fs::write(&installer, b"abc").expect("installer");
        let known = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let file = lock_verified_installer(&installer, known).expect("matching hash");
        drop(file);
        assert!(lock_verified_installer(&installer, &"0".repeat(64)).is_err());
    }
}
