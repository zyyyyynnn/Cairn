// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Reading and changing the `CairnService` registration.
//!
//! The service's start type and its running state are the only machine state the "搜索范围" scope
//! owns, so this module is the only place that reads them and the only place that changes them.
//! Reading is a plain SCM query a standard user may run; changing needs an administrator and goes
//! through one elevation.

use crate::service::elevation::{run_elevated, ElevationError};
use crate::service::search_scope::{ServiceStartType, ServiceState};

/// The name the service is registered under. This is the one place the name is written down.
pub const SERVICE_NAME: &str = "CairnService";

/// `ERROR_SERVICE_ALREADY_RUNNING`: the action raced a service that was already up. That is the
/// outcome the user asked for, not a failure.
const ERROR_SERVICE_ALREADY_RUNNING: u32 = 1056;

/// `ERROR_SERVICE_NOT_ACTIVE`: the action raced a service that had already stopped. Same.
const ERROR_SERVICE_NOT_ACTIVE: u32 = 1062;

/// `ERROR_SERVICE_DOES_NOT_EXIST`: the service is not registered on this machine.
const ERROR_SERVICE_DOES_NOT_EXIST: u32 = 1060;

/// What the scope switch has to change on the machine.
///
/// The steps are one command line because they must happen inside a single elevation: a separate
/// elevation per step would ask the user several times and could leave the service stopped between
/// two of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceChange {
    /// The start type the scope needs.
    pub start_type: ServiceStartType,
    /// Whether the registration's start type has to be rewritten.
    pub reconfigure: bool,
    /// Whether the service has to be stopped as part of the change.
    pub stop: bool,
    /// Whether the service has to be started as part of the change.
    pub start: bool,
}

/// The command line that puts the service into the state `change` describes.
///
/// `sc.exe` runs one command per process, so the steps are chained through `cmd.exe` with `&&`:
/// the chain stops at the first failure, so a step that did not happen cannot leave the next one
/// running against a state the previous one did not establish.
pub fn start_type_change_command(change: ServiceChange) -> String {
    let mut steps = Vec::new();
    if change.reconfigure {
        steps.push(format!(
            "sc config {SERVICE_NAME} start= {}",
            change.start_type.as_sc_argument()
        ));
    }
    if change.stop {
        steps.push(format!("sc stop {SERVICE_NAME}"));
    }
    if change.start {
        steps.push(format!("sc start {SERVICE_NAME}"));
    }
    steps.join(" && ")
}

/// The `cmd.exe` parameters that run `command` as one elevated command.
///
/// The command is passed without surrounding quotes on purpose: `cmd.exe` treats `&&` inside a
/// quoted string as literal text, so a quoted chain would run as one unknown command.
pub fn elevated_parameters(command: &str) -> String {
    format!("/c {command}")
}

/// Whether an elevated command's exit code means the service ended up where it was asked to.
///
/// `sc.exe` answers 1056 when the service was already running and 1062 when it was already
/// stopped. Both are the state the caller asked for.
fn exit_code_is_success(code: u32) -> bool {
    code == 0 || code == ERROR_SERVICE_ALREADY_RUNNING || code == ERROR_SERVICE_NOT_ACTIVE
}

/// Reads the service registration: whether it exists, how it is registered, and whether it runs.
#[cfg(windows)]
pub fn service_state() -> ServiceState {
    use windows::core::HSTRING;
    use windows::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, SC_MANAGER_CONNECT, SERVICE_QUERY_CONFIG,
        SERVICE_QUERY_STATUS,
    };

    // SAFETY: every SCM handle is opened and closed inside this function, and the config buffer
    // is owned here and only read after the call that filled it succeeded.
    unsafe {
        let Ok(scm) = OpenSCManagerW(None, None, SC_MANAGER_CONNECT) else {
            return ServiceState::Unreadable;
        };
        let name = HSTRING::from(SERVICE_NAME);
        let service = OpenServiceW(scm, &name, SERVICE_QUERY_CONFIG | SERVICE_QUERY_STATUS);
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

        let running = service_handle_is_running(service);
        let start_type = read_start_type(service);
        let _ = CloseServiceHandle(service);

        match start_type {
            Some(ServiceStartType::Auto) => ServiceState::Owned {
                start_type: ServiceStartType::Auto,
                running,
            },
            Some(ServiceStartType::Manual) => ServiceState::Owned {
                start_type: ServiceStartType::Manual,
                running,
            },
            // The registration exists but the start type is one this product never sets
            // (disabled, boot, system). It can still be rewritten, so it is not a dead end.
            None => ServiceState::Foreign { running },
        }
    }
}

#[cfg(not(windows))]
pub fn service_state() -> ServiceState {
    ServiceState::NotInstalled
}

/// Whether the machine state says the service is registered on this machine.
pub fn service_is_registered(state: ServiceState) -> bool {
    !matches!(state, ServiceState::NotInstalled)
}

/// Whether the machine state says the service is registered and running right now.
pub fn service_is_running(state: ServiceState) -> bool {
    match state {
        ServiceState::Owned { running, .. } | ServiceState::Foreign { running } => running,
        ServiceState::NotInstalled | ServiceState::Unreadable => false,
    }
}

/// Whether the service is registered on this machine.
pub fn service_is_installed() -> bool {
    service_is_registered(service_state())
}

/// The start type the service is registered with, or `None` when it is registered with one this
/// product does not own.
#[cfg(windows)]
fn read_start_type(
    service: windows::Win32::System::Services::SC_HANDLE,
) -> Option<ServiceStartType> {
    use windows::Win32::System::Services::{QueryServiceConfigW, QUERY_SERVICE_CONFIGW};

    // The answer does not fit in the structure alone: the structure holds pointers to strings that
    // live in the same buffer, so SCM asks for the whole buffer size. One call with a structure
    // sized buffer answers `ERROR_INSUFFICIENT_BUFFER` and reports the size the answer needs; a
    // second call with that buffer returns the configuration.
    let mut probe = QUERY_SERVICE_CONFIGW::default();
    let mut needed = 0u32;
    // SAFETY: `probe` is a live, zero-initialized structure and `needed` is a valid out pointer.
    let first = unsafe {
        QueryServiceConfigW(
            service,
            Some(&mut probe as *mut QUERY_SERVICE_CONFIGW),
            std::mem::size_of::<QUERY_SERVICE_CONFIGW>() as u32,
            &mut needed,
        )
    };
    if first.is_ok() {
        return start_type_of(&probe);
    }
    if needed <= std::mem::size_of::<QUERY_SERVICE_CONFIGW>() as u32 {
        return None;
    }

    let mut buffer = vec![0u8; needed as usize];
    // SAFETY: the buffer is `needed` bytes long, which is the size SCM asked for, and the
    // structure it is read back as lives inside it.
    let second = unsafe {
        QueryServiceConfigW(
            service,
            Some(buffer.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW),
            needed,
            &mut needed,
        )
    };
    if second.is_err() {
        return None;
    }
    // SAFETY: the call above succeeded and filled the buffer with a structure of the requested
    // size, so the pointer is valid and aligned for the structure it now holds.
    let config = unsafe { &*(buffer.as_ptr() as *const QUERY_SERVICE_CONFIGW) };
    start_type_of(config)
}

#[cfg(windows)]
fn start_type_of(
    config: &windows::Win32::System::Services::QUERY_SERVICE_CONFIGW,
) -> Option<ServiceStartType> {
    match config.dwStartType.0 {
        2 => Some(ServiceStartType::Auto),
        3 => Some(ServiceStartType::Manual),
        _ => None,
    }
}

#[cfg(windows)]
fn service_handle_is_running(service: windows::Win32::System::Services::SC_HANDLE) -> bool {
    use windows::Win32::System::Services::{
        QueryServiceStatusEx, SC_STATUS_PROCESS_INFO, SERVICE_RUNNING, SERVICE_STATUS_PROCESS,
    };

    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut needed = 0u32;
    // SAFETY: the buffer is a live, zero-initialized `SERVICE_STATUS_PROCESS` and the byte slice
    // covers exactly that structure.
    let queried = unsafe {
        let bytes = std::slice::from_raw_parts_mut(
            &mut status as *mut SERVICE_STATUS_PROCESS as *mut u8,
            std::mem::size_of::<SERVICE_STATUS_PROCESS>(),
        );
        QueryServiceStatusEx(service, SC_STATUS_PROCESS_INFO, Some(bytes), &mut needed)
    };
    queried.is_ok() && status.dwCurrentState == SERVICE_RUNNING
}

/// Starts the service through one UAC elevation and waits for `sc.exe` to finish.
///
/// The GUI itself stays unprivileged. This is only called from the offline band's explicit button;
/// opening the window never requests elevation.
pub fn start_service_elevated() -> Result<(), ElevationError> {
    let command = format!("sc start {SERVICE_NAME}");
    let code = run_elevated("sc.exe", &command)?;
    if exit_code_is_success(code) {
        Ok(())
    } else {
        Err(ElevationError::Failed {
            os_error: Some(code),
            message: format!("sc.exe exited with code {code}"),
        })
    }
}

/// Applies `change` to the machine through one UAC elevation.
///
/// The steps run as one `cmd.exe` command so the user is asked once. The exit code is checked
/// here; the caller decides whether the machine really ended up in the wanted state.
pub fn apply_change(change: ServiceChange) -> Result<(), ElevationError> {
    let command = start_type_change_command(change);
    let code = run_elevated("cmd.exe", &elevated_parameters(&command))?;
    if exit_code_is_success(code) {
        Ok(())
    } else {
        Err(ElevationError::Failed {
            os_error: Some(code),
            message: format!("the service change exited with code {code}"),
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

    /// The scope switch changes the start type and stops the service, in one elevated command
    /// line. Separate elevations would ask the user several times and could leave the service
    /// stopped in between.
    #[test]
    fn the_start_type_change_is_one_elevated_command() {
        let command = start_type_change_command(ServiceChange {
            start_type: ServiceStartType::Auto,
            reconfigure: true,
            stop: true,
            start: false,
        });
        assert_eq!(
            command,
            "sc config CairnService start= auto && sc stop CairnService"
        );
    }

    /// The scope that reads only the user's own files leaves the service stopped: a service that
    /// keeps running under the new start type would answer the window while the window reads
    /// from the user indexer.
    #[test]
    fn the_scope_that_does_not_need_the_service_leaves_it_stopped() {
        let command = start_type_change_command(ServiceChange {
            start_type: ServiceStartType::Manual,
            reconfigure: true,
            stop: true,
            start: false,
        });
        assert!(command.contains("sc config CairnService start= demand"));
        assert!(command.contains("sc stop CairnService"));
        assert!(
            !command.contains("sc start CairnService"),
            "the service must not be started for a scope that does not read from it"
        );
    }

    /// The manual scope uses `demand`, which is the start type the service must have so it does
    /// not come back on every boot while the user reads only their own files.
    #[test]
    fn the_manual_scope_uses_the_demand_start_type() {
        assert_eq!(ServiceStartType::Manual.as_sc_argument(), "demand");
        assert_eq!(ServiceStartType::Auto.as_sc_argument(), "auto");
    }

    /// `cmd.exe` treats `&&` inside a quoted string as literal text, so the chain must not be
    /// quoted or the whole thing runs as one unknown command.
    #[test]
    fn the_elevated_chain_is_not_quoted() {
        let parameters = elevated_parameters("sc config CairnService start= auto");
        assert_eq!(parameters, "/c sc config CairnService start= auto");
        assert!(!parameters.contains('"'));
    }

    /// The steps that answer "it was already like that" are successes, not failures.
    #[test]
    fn an_already_reached_state_is_not_a_failure() {
        assert!(exit_code_is_success(0));
        assert!(exit_code_is_success(ERROR_SERVICE_ALREADY_RUNNING));
        assert!(exit_code_is_success(ERROR_SERVICE_NOT_ACTIVE));
        assert!(!exit_code_is_success(1060));
        assert!(!exit_code_is_success(1639));
    }

    /// The service name is written down once.
    #[test]
    fn the_service_name_has_one_source() {
        assert_eq!(SERVICE_NAME, "CairnService");
    }
}
