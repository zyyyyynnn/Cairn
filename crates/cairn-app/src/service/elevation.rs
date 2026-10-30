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

use std::path::PathBuf;

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

/// Runs `program` with `parameters` through one UAC elevation and waits for it to finish.
///
/// The program is resolved inside the system directory, so the elevated command is the one Windows
/// ships and not whatever happens to be on the current path. The exit code is returned as it came
/// back: this module does not decide what a non-zero code means, because only the caller knows
/// which command it asked for.
#[cfg(windows)]
pub fn run_elevated(program: &str, parameters: &str) -> Result<u32, ElevationError> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, GetLastError, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let helper_failed =
        |os_error: Option<u32>, message: String| ElevationError::Failed { os_error, message };

    let executable = system32(program);
    if !executable.is_file() {
        return Err(helper_failed(
            None,
            format!("{} was not found at {}", program, executable.display()),
        ));
    }

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
pub fn run_elevated(_program: &str, _parameters: &str) -> Result<u32, ElevationError> {
    Err(ElevationError::Failed {
        os_error: None,
        message: "elevating a command is only available on Windows".to_string(),
    })
}

/// Resolves an executable in the system directory.
///
/// The system directory comes from the OS, not from `SystemRoot`: a user-writable environment
/// variable must not be able to redirect an elevated command to another executable.
pub fn system32(executable: &str) -> PathBuf {
    #[cfg(windows)]
    {
        use windows::Win32::System::SystemInformation::GetSystemDirectoryW;

        let mut buffer = [0u16; 260];
        let length = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
        if length > 0 && length < buffer.len() {
            return PathBuf::from(String::from_utf16_lossy(&buffer[..length])).join(executable);
        }
        PathBuf::from(r"C:\Windows\System32").join(executable)
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(r"C:\Windows\System32").join(executable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_executable_is_resolved_inside_the_system_directory() {
        let path = system32("cmd.exe");
        assert!(
            path.to_string_lossy()
                .to_ascii_lowercase()
                .ends_with("system32\\cmd.exe"),
            "{path:?}"
        );
        assert!(path.is_absolute());
        assert!(path.is_file(), "{path:?}");
    }
}
