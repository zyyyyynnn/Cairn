// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Reading and starting the `CairnService` registration.
//!
//! The GUI never installs or reconfigures the service. It asks whether the registration exists,
//! whether it is running, and can request one elevated start when the offline band offers it. That
//! start is a second copy of this same executable, launched elevated with `--start-service`, so the
//! consent dialog names Cairn and carries its icon instead of naming a system tool.

use crate::service::elevation::{run_elevated_self, ElevationError};

/// The name the service is registered under. This is the one place the name is written down.
pub const SERVICE_NAME: &str = "CairnService";

/// The single argument that turns this process into the elevated start command.
pub const START_SERVICE_FLAG: &str = "--start-service";

/// `ERROR_INVALID_ARGUMENT`: the start flag arrived alongside something else, so this process was
/// not asked for exactly the one action it may perform.
const ERROR_INVALID_ARGUMENT: u32 = 87;

/// Whether `args` is exactly the elevated start command. Anything else alongside the flag is
/// refused, so an elevated copy of the GUI can never be aimed at a query, a path or another action.
fn is_start_service_request(args: &[String]) -> bool {
    args.len() == 1 && args[0] == START_SERVICE_FLAG
}

/// The exit code for a process launched as the elevated start command, or `None` when this is an
/// ordinary window. The flag claims the process as soon as it appears: with any other argument
/// beside it the process refuses instead of falling through, because a fall-through would open the
/// GUI with the administrator token it was asked to spend on one service.
pub fn elevated_start_reply(args: &[String]) -> Option<u32> {
    if !args.iter().any(|arg| arg == START_SERVICE_FLAG) {
        return None;
    }
    Some(if is_start_service_request(args) {
        start_service_exit_code()
    } else {
        ERROR_INVALID_ARGUMENT
    })
}

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

/// Whether a start command's exit code means the service ended up running.
fn start_exit_code_is_success(code: u32) -> bool {
    code == 0 || code == ERROR_SERVICE_ALREADY_RUNNING
}

/// Starts the service through one UAC elevation of this same executable and waits for it to exit.
///
/// The window itself stays unprivileged. This is only called from the offline band's explicit
/// button; opening the window never requests elevation.
pub fn start_service_elevated() -> Result<(), ElevationError> {
    let code = run_elevated_self(START_SERVICE_FLAG)?;
    if start_exit_code_is_success(code) {
        Ok(())
    } else {
        Err(ElevationError::Failed {
            os_error: Some(code),
            message: format!("the elevated start exited with code {code}"),
        })
    }
}

/// Asks SCM to start the service with this process's own token and answers with the Win32 code: 0
/// when it started, `ERROR_SERVICE_ALREADY_RUNNING` when it was already up, the refusal otherwise.
/// The service name is never an argument, so an elevated caller cannot reach any other
/// registration on the machine.
fn start_service_exit_code() -> u32 {
    #[cfg(windows)]
    // SAFETY: every handle opened here is closed before returning, and StartServiceW only asks SCM
    // to transition the already-registered service.
    unsafe {
        use windows::core::HSTRING;
        use windows::Win32::Foundation::GetLastError;
        use windows::Win32::System::Services::{
            CloseServiceHandle, OpenSCManagerW, OpenServiceW, StartServiceW, SC_MANAGER_CONNECT,
            SERVICE_START,
        };

        let Ok(scm) = OpenSCManagerW(None, None, SC_MANAGER_CONNECT) else {
            return GetLastError().0;
        };
        let name = HSTRING::from(SERVICE_NAME);
        let Ok(service) = OpenServiceW(scm, &name, SERVICE_START) else {
            let error = GetLastError().0;
            let _ = CloseServiceHandle(scm);
            return error;
        };
        // The code is read before either handle is closed: closing a handle is another call that
        // may touch the thread's last error.
        let code = match StartServiceW(service, None) {
            Ok(()) => 0,
            Err(_) => GetLastError().0,
        };
        let _ = CloseServiceHandle(service);
        let _ = CloseServiceHandle(scm);
        code
    }

    #[cfg(not(windows))]
    {
        ERROR_SERVICE_DOES_NOT_EXIST
    }
}

/// Best-effort start with the caller's own token, without a UAC prompt. The installed service's
/// default DACL usually lets only administrators start it, so an access-denied answer is expected
/// for a standard user and simply hands the decision to the explicit button.
pub fn try_start_installed_service() -> Result<(), String> {
    match start_service_exit_code() {
        0 | ERROR_SERVICE_ALREADY_RUNNING => Ok(()),
        code => Err(format!("StartService was refused with code {code}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The elevated copy acts on the flag and nothing else: one extra argument of any kind is
    /// refused, so it can never be turned into a query or a path-opening run.
    #[test]
    fn only_the_bare_start_flag_is_the_elevated_command() {
        let argv = |values: &[&str]| -> Vec<String> {
            values.iter().map(|value| (*value).to_string()).collect()
        };
        assert!(is_start_service_request(&argv(&[START_SERVICE_FLAG])));
        assert!(!is_start_service_request(&argv(&[])));
        assert!(!is_start_service_request(&argv(&[
            START_SERVICE_FLAG,
            r"C:\Windows"
        ])));
        assert!(!is_start_service_request(&argv(&[
            "--collapsed",
            START_SERVICE_FLAG
        ])));
    }

    /// A stray argument beside the flag is refused rather than dropped: falling through would spend
    /// the administrator token on an ordinary window.
    #[test]
    fn the_flag_claims_the_process_and_refuses_any_mixture() {
        let argv = |values: &[&str]| -> Vec<String> {
            values.iter().map(|value| (*value).to_string()).collect()
        };
        assert_eq!(elevated_start_reply(&argv(&[])), None);
        assert_eq!(elevated_start_reply(&argv(&["--collapsed"])), None);
        assert_eq!(
            elevated_start_reply(&argv(&[START_SERVICE_FLAG, "C:\\Windows"])),
            Some(ERROR_INVALID_ARGUMENT)
        );
    }

    /// The steps that answer "it was already like that" are successes, not failures.
    #[test]
    fn an_already_reached_state_is_not_a_failure() {
        assert!(start_exit_code_is_success(0));
        assert!(start_exit_code_is_success(ERROR_SERVICE_ALREADY_RUNNING));
        assert!(!start_exit_code_is_success(1062));
        assert!(!start_exit_code_is_success(1639));
    }

    /// The installed service identity is pinned to the name `ipc-security.md` documents.
    #[test]
    fn the_service_name_has_one_source() {
        assert_eq!(SERVICE_NAME, "CairnService");
    }
}
