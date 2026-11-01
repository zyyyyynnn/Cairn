// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Reading and starting the `CairnService` registration.
//!
//! The GUI never installs or reconfigures the service. It asks whether the registration exists,
//! whether it is running, and can request one elevated start when the offline band offers it.

use crate::service::elevation::{run_elevated, ElevationError};

/// The name the service is registered under. This is the one place the name is written down.
pub const SERVICE_NAME: &str = "CairnService";

/// `ERROR_SERVICE_ALREADY_RUNNING`: the action raced a service that was already up. That is the
/// outcome the user asked for, not a failure.
const ERROR_SERVICE_ALREADY_RUNNING: u32 = 1056;

/// `ERROR_SERVICE_DOES_NOT_EXIST`: the service is not registered on this machine.
const ERROR_SERVICE_DOES_NOT_EXIST: u32 = 1060;

/// What the machine knows about the service registration right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceState {
    /// The service is not registered on this machine.
    NotInstalled,
    /// The service is registered on this machine.
    Registered,
    /// The registration could not be read, so the window cannot claim the service is absent.
    Unreadable,
}

/// Reads the service registration: whether it exists and whether it runs.
#[cfg(windows)]
pub fn service_state() -> ServiceState {
    use windows::core::HSTRING;
    use windows::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS,
    };

    // SAFETY: every SCM handle is opened and closed inside this function.
    unsafe {
        let Ok(scm) = OpenSCManagerW(None, None, SC_MANAGER_CONNECT) else {
            return ServiceState::Unreadable;
        };
        let name = HSTRING::from(SERVICE_NAME);
        let service = OpenServiceW(scm, &name, SERVICE_QUERY_STATUS);
        // The error is read before the manager handle is closed: closing a handle is another call
        // that may touch the thread's last error.
        let open_error = windows::Win32::Foundation::GetLastError().0;
        let _ = CloseServiceHandle(scm);
        let Ok(service) = service else {
            // The service is simply not there. Anything else (an access-denied SCM, a machine
            // policy) is a registration this window cannot reason about.
            return match open_error {
                ERROR_SERVICE_DOES_NOT_EXIST => ServiceState::NotInstalled,
                _ => ServiceState::Unreadable,
            };
        };
        let _ = CloseServiceHandle(service);
        ServiceState::Registered
    }
}

#[cfg(not(windows))]
pub fn service_state() -> ServiceState {
    ServiceState::NotInstalled
}

/// Whether the machine state says the service is registered on this machine.
pub fn service_is_installed() -> bool {
    matches!(service_state(), ServiceState::Registered)
}

/// The arguments `sc.exe` receives for the explicit start action.
fn service_start_parameters() -> String {
    format!("start {SERVICE_NAME}")
}

/// Whether a start command's exit code means the service ended up running.
fn start_exit_code_is_success(code: u32) -> bool {
    code == 0 || code == ERROR_SERVICE_ALREADY_RUNNING
}

/// Starts the service through one UAC elevation and waits for `sc.exe` to finish.
///
/// The GUI itself stays unprivileged. This is only called from the offline band's explicit button;
/// opening the window never requests elevation.
pub fn start_service_elevated() -> Result<(), ElevationError> {
    let command = service_start_parameters();
    let code = run_elevated("sc.exe", &command)?;
    if start_exit_code_is_success(code) {
        Ok(())
    } else {
        Err(ElevationError::Failed {
            os_error: Some(code),
            message: format!("sc.exe exited with code {code}"),
        })
    }
}

/// Best-effort start with the caller's own token, without a UAC prompt. The installed service's
/// default DACL usually lets only administrators start it, so an access-denied answer is expected
/// for a standard user and simply hands the decision to the explicit button.
pub fn try_start_installed_service() -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows::core::HSTRING;
        use windows::Win32::System::Services::{
            CloseServiceHandle, OpenSCManagerW, OpenServiceW, StartServiceW, SC_MANAGER_CONNECT,
            SERVICE_QUERY_STATUS, SERVICE_START,
        };

        // SAFETY: every SCM handle is closed before returning; StartServiceW only asks SCM to
        // transition the already-registered service.
        unsafe {
            let scm = OpenSCManagerW(None, None, SC_MANAGER_CONNECT)
                .map_err(|err| format!("OpenSCManager: {err}"))?;
            let name = HSTRING::from(SERVICE_NAME);
            let service = OpenServiceW(scm, &name, SERVICE_START | SERVICE_QUERY_STATUS);
            let _ = CloseServiceHandle(scm);
            let service = service.map_err(|err| format!("OpenService: {err}"))?;
            let result = StartServiceW(service, None);
            // The error is read before the handle is closed: closing a handle is another call
            // that may touch the thread's last error.
            let start_error = windows::Win32::Foundation::GetLastError().0;
            let _ = CloseServiceHandle(service);
            match result {
                Ok(()) => Ok(()),
                Err(_) if start_error == ERROR_SERVICE_ALREADY_RUNNING => Ok(()),
                Err(_) => Err("StartService was refused".to_string()),
            }
        }
    }

    #[cfg(not(windows))]
    {
        Err("starting the Windows service is only available on Windows".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_elevated_start_passes_only_the_sc_command() {
        assert_eq!(service_start_parameters(), "start CairnService");
    }

    /// The steps that answer "it was already like that" are successes, not failures.
    #[test]
    fn an_already_reached_state_is_not_a_failure() {
        assert!(start_exit_code_is_success(0));
        assert!(start_exit_code_is_success(ERROR_SERVICE_ALREADY_RUNNING));
        assert!(!start_exit_code_is_success(1062));
        assert!(!start_exit_code_is_success(1639));
    }

    /// The service name is written down once.
    #[test]
    fn the_service_name_has_one_source() {
        assert_eq!(SERVICE_NAME, "CairnService");
    }
}
