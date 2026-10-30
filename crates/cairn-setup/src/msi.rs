// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The MSI transaction behind the installer's progress page.
//!
//! The MSI is installed in-process through `msiexec`'s API rather than as a second `msiexec.exe`
//! process. That keeps the MSI's own external UI silent while giving this window the real action
//! and percentage records: the bar reflects work Windows Installer actually reports, never a timer.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::System::ApplicationInstallationAndServicing::{
    MsiConfigureProductW, MsiEnableLogW, MsiInstallProductW, MsiRecordGetInteger,
    MsiRecordGetStringW, MsiSetExternalUIRecord, MsiSetInternalUI, INSTALLLEVEL_DEFAULT,
    INSTALLLOGMODE_ACTIONSTART, INSTALLLOGMODE_ERROR, INSTALLLOGMODE_PROGRESS,
    INSTALLLOGMODE_VERBOSE, INSTALLMESSAGE_ACTIONSTART, INSTALLMESSAGE_ERROR,
    INSTALLMESSAGE_PROGRESS, INSTALLSTATE_ABSENT, INSTALLUILEVEL_NONE, MSIHANDLE,
};

const MSI_FILTER: u32 = INSTALLMESSAGE_PROGRESS.0 as u32
    | INSTALLMESSAGE_ACTIONSTART.0 as u32
    | INSTALLMESSAGE_ERROR.0 as u32;

const ERROR_SUCCESS: u32 = 0;
const ERROR_SUCCESS_REBOOT_REQUIRED: u32 = 3010;
const ERROR_UNKNOWN_PRODUCT: u32 = 1605;
const ERROR_INSTALL_USEREXIT: u32 = 1602;
const ERROR_INSTALL_FAILURE: u32 = 1603;
const ERROR_INSTALL_ALREADY_RUNNING: u32 = 1618;
const ERROR_INSTALL_PACKAGE_OPEN_FAILED: u32 = 1619;
const ERROR_INSTALL_PACKAGE_INVALID: u32 = 1620;
const ERROR_INSTALL_PLATFORM_UNSUPPORTED: u32 = 1633;
const ERROR_PRODUCT_VERSION: u32 = 1638;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MsiEvent {
    Action(String),
    Progress { current: u32, total: u32 },
    Done(Result<MsiSuccess, MsiFailure>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MsiSuccess {
    pub restart_required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Cancelled,
    Busy,
    InvalidPackage,
    Platform,
    NotInstalled,
    Legacy,
    Fatal,
    Other(u32),
}

impl FailureKind {
    pub fn from_code(code: u32) -> Self {
        match code {
            ERROR_INSTALL_USEREXIT => Self::Cancelled,
            ERROR_INSTALL_ALREADY_RUNNING => Self::Busy,
            ERROR_INSTALL_PACKAGE_OPEN_FAILED | ERROR_INSTALL_PACKAGE_INVALID => {
                Self::InvalidPackage
            }
            ERROR_INSTALL_PLATFORM_UNSUPPORTED => Self::Platform,
            ERROR_UNKNOWN_PRODUCT => Self::NotInstalled,
            ERROR_INSTALL_FAILURE => Self::Fatal,
            ERROR_PRODUCT_VERSION => Self::Other(ERROR_PRODUCT_VERSION),
            other => Self::Other(other),
        }
    }

    /// The Fluent message key for this failure. The raw installer code is deliberately not part
    /// of the message: it belongs to the MSI log, which the failure page links to.
    pub fn message_key(self) -> &'static str {
        match self {
            Self::Cancelled => "error_msi_cancelled",
            Self::Busy => "error_msi_busy",
            Self::InvalidPackage => "error_msi_invalid",
            Self::Platform => "error_msi_platform",
            Self::NotInstalled => "error_not_installed",
            Self::Legacy => "error_legacy_failed",
            Self::Fatal => "error_msi_fatal",
            Self::Other(_) => "error_msi_unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsiFailure {
    pub code: u32,
    pub kind: FailureKind,
    pub log: PathBuf,
}

impl MsiFailure {
    fn new(code: u32, log: PathBuf) -> Self {
        Self {
            code,
            kind: FailureKind::from_code(code),
            log,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyUninstall {
    pub program: PathBuf,
    pub args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct InstallRequest {
    pub msi: PathBuf,
    pub install_dir: PathBuf,
    pub log: PathBuf,
    pub legacy: Vec<LegacyUninstall>,
}

pub struct MsiSession {
    pub events: UnboundedReceiver<MsiEvent>,
}

/// Starts an elevated MSI install on a worker thread. Events arrive in order, ending with `Done`.
pub fn start_install(request: InstallRequest) -> MsiSession {
    let (sender, events) = unbounded_channel();
    std::thread::spawn(move || {
        let result = run_install(&request, Some(&sender));
        let _ = sender.send(MsiEvent::Done(result));
    });
    MsiSession { events }
}

/// Starts a product removal and runs `after_success` before the final `Done` event. The cleanup of
/// data outside the MSI therefore belongs to the same visible operation, not to a detached task
/// that can be interrupted when the window closes.
pub fn start_uninstall_with(
    product_code: String,
    log: PathBuf,
    after_success: impl FnOnce() + Send + 'static,
) -> MsiSession {
    let (sender, events) = unbounded_channel();
    std::thread::spawn(move || {
        let result = run_uninstall(&product_code, &log, Some(&sender));
        if result.is_ok() {
            after_success();
        }
        let _ = sender.send(MsiEvent::Done(result));
    });
    MsiSession { events }
}

/// The quiet path: no window and no progress consumer, but the same transaction and result mapping.
pub fn install_blocking(request: &InstallRequest) -> Result<MsiSuccess, MsiFailure> {
    run_install(request, None)
}

pub fn uninstall_blocking(product_code: &str, log: &Path) -> Result<MsiSuccess, MsiFailure> {
    run_uninstall(product_code, log, None)
}

fn run_install(
    request: &InstallRequest,
    sender: Option<&UnboundedSender<MsiEvent>>,
) -> Result<MsiSuccess, MsiFailure> {
    let mut legacy_restart = false;
    for legacy in &request.legacy {
        emit(
            sender,
            MsiEvent::Action("RemoveExistingProducts".to_string()),
        );
        let mut command = Command::new(&legacy.program);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        command.args(&legacy.args);
        command.args(["/quiet", "/norestart"]);
        let status = command.status();
        let code = match status {
            Ok(status) => status.code().unwrap_or(-1) as u32,
            Err(_) => u32::MAX,
        };
        if !matches!(code, ERROR_SUCCESS | ERROR_SUCCESS_REBOOT_REQUIRED) {
            return Err(MsiFailure {
                code,
                kind: FailureKind::Legacy,
                log: request.log.clone(),
            });
        }
        legacy_restart |= code == ERROR_SUCCESS_REBOOT_REQUIRED;
    }

    let install_dir = request.install_dir.to_string_lossy();
    let command_line = format!("INSTALLFOLDER=\"{install_dir}\" REBOOT=ReallySuppress");
    let result = run_msi(&request.msi, &command_line, &request.log, sender)?;
    Ok(MsiSuccess {
        restart_required: result.restart_required || legacy_restart,
    })
}

fn run_uninstall(
    product_code: &str,
    log: &Path,
    sender: Option<&UnboundedSender<MsiEvent>>,
) -> Result<MsiSuccess, MsiFailure> {
    let log_wide = wide(log.as_os_str());
    let product_wide = wide(std::ffi::OsStr::new(product_code));
    let callback_context = sender.map(|sender| CallbackContext {
        sender: sender.clone(),
        progress: Mutex::new(ProgressState::default()),
    });
    let context_ptr = callback_context
        .as_ref()
        .map(|context| context as *const CallbackContext as *const c_void);

    unsafe {
        let _ = MsiSetInternalUI(INSTALLUILEVEL_NONE, None);
        let _ = MsiEnableLogW(log_filter(), PCWSTR(log_wide.as_ptr()), 0);
        let _ = MsiSetExternalUIRecord(Some(external_ui_handler), MSI_FILTER, context_ptr, None);
        let code = MsiConfigureProductW(
            PCWSTR(product_wide.as_ptr()),
            INSTALLLEVEL_DEFAULT,
            INSTALLSTATE_ABSENT,
        );
        let _ = MsiSetExternalUIRecord(None, 0, None, None);
        finish(code, log)
    }
}

fn run_msi(
    msi: &Path,
    command_line: &str,
    log: &Path,
    sender: Option<&UnboundedSender<MsiEvent>>,
) -> Result<MsiSuccess, MsiFailure> {
    let package_wide = wide(msi.as_os_str());
    let command_wide = wide(std::ffi::OsStr::new(command_line));
    let log_wide = wide(log.as_os_str());
    let callback_context = sender.map(|sender| CallbackContext {
        sender: sender.clone(),
        progress: Mutex::new(ProgressState::default()),
    });
    let context_ptr = callback_context
        .as_ref()
        .map(|context| context as *const CallbackContext as *const c_void);

    unsafe {
        let _ = MsiSetInternalUI(INSTALLUILEVEL_NONE, None);
        let _ = MsiEnableLogW(log_filter(), PCWSTR(log_wide.as_ptr()), 0);
        let _ = MsiSetExternalUIRecord(Some(external_ui_handler), MSI_FILTER, context_ptr, None);
        let code = MsiInstallProductW(PCWSTR(package_wide.as_ptr()), PCWSTR(command_wide.as_ptr()));
        let _ = MsiSetExternalUIRecord(None, 0, None, None);
        finish(code, log)
    }
}

fn finish(code: u32, log: &Path) -> Result<MsiSuccess, MsiFailure> {
    match code {
        ERROR_SUCCESS => Ok(MsiSuccess {
            restart_required: false,
        }),
        ERROR_SUCCESS_REBOOT_REQUIRED => Ok(MsiSuccess {
            restart_required: true,
        }),
        other => Err(MsiFailure::new(other, log.to_path_buf())),
    }
}

fn log_filter() -> windows::Win32::System::ApplicationInstallationAndServicing::INSTALLLOGMODE {
    windows::Win32::System::ApplicationInstallationAndServicing::INSTALLLOGMODE(
        INSTALLLOGMODE_VERBOSE.0
            | INSTALLLOGMODE_ACTIONSTART.0
            | INSTALLLOGMODE_PROGRESS.0
            | INSTALLLOGMODE_ERROR.0,
    )
}

fn emit(sender: Option<&UnboundedSender<MsiEvent>>, event: MsiEvent) {
    if let Some(sender) = sender {
        let _ = sender.send(event);
    }
}

struct CallbackContext {
    sender: UnboundedSender<MsiEvent>,
    progress: Mutex<ProgressState>,
}

#[derive(Default)]
struct ProgressState {
    current: u64,
    total: u64,
}

impl ProgressState {
    /// `kind` is progress record field 1, `value` is field 2. The total comes from a reset and can
    /// grow when an action adds ticks; a tick advances the current value.
    fn apply(&mut self, kind: i32, value: i32) -> Option<(u32, u32)> {
        if value < 0 {
            return None;
        }
        let value = value as u64;
        match kind {
            0 => {
                self.current = 0;
                self.total = value;
                (self.total > 0).then_some((0, self.total.min(u32::MAX as u64) as u32))
            }
            2 if self.total > 0 => {
                self.current = self.current.saturating_add(value).min(self.total);
                Some((self.current as u32, self.total as u32))
            }
            3 => {
                self.total = self.total.saturating_add(value);
                (self.total > 0)
                    .then_some((self.current as u32, self.total.min(u32::MAX as u64) as u32))
            }
            _ => None,
        }
    }
}

/// # Safety
///
/// `context` is the `CallbackContext` pointer supplied to `MsiSetExternalUIRecord`; it outlives the
/// synchronous MSI call. `record` is owned by Windows Installer for the duration of this callback.
unsafe extern "system" fn external_ui_handler(
    context: *mut c_void,
    message: u32,
    record: MSIHANDLE,
) -> i32 {
    let Some(context) = context.cast::<CallbackContext>().as_ref() else {
        return 0;
    };

    if message == INSTALLMESSAGE_PROGRESS.0 as u32 {
        let kind = unsafe { MsiRecordGetInteger(record, 1) };
        let value = unsafe { MsiRecordGetInteger(record, 2) };
        let event = context
            .progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .apply(kind, value);
        if let Some((current, total)) = event {
            let _ = context.sender.send(MsiEvent::Progress { current, total });
        }
    } else if message == INSTALLMESSAGE_ACTIONSTART.0 as u32 {
        if let Some(action) = record_string(record, 1) {
            let _ = context.sender.send(MsiEvent::Action(action));
        }
    } else if message == INSTALLMESSAGE_ERROR.0 as u32 {
        if let Some(action) = record_string(record, 2) {
            let _ = context.sender.send(MsiEvent::Action(action));
        }
    }
    0
}

unsafe fn record_string(record: MSIHANDLE, field: u32) -> Option<String> {
    let mut buffer = vec![0u16; 512];
    let mut length = buffer.len() as u32;
    let result = unsafe {
        MsiRecordGetStringW(
            record,
            field,
            Some(PWSTR(buffer.as_mut_ptr())),
            Some(&mut length),
        )
    };
    if result != ERROR_SUCCESS {
        return None;
    }
    let length = (length as usize).min(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..length]))
}

fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::{FailureKind, MsiFailure, ProgressState};
    use std::path::PathBuf;

    #[test]
    fn progress_records_track_total_and_ticks() {
        let mut state = ProgressState::default();
        assert_eq!(state.apply(0, 100), Some((0, 100)));
        assert_eq!(state.apply(2, 25), Some((25, 100)));
        assert_eq!(state.apply(3, 50), Some((25, 150)));
        assert_eq!(state.apply(2, 200), Some((150, 150)));
        assert_eq!(state.apply(1, 1), None);
    }

    #[test]
    fn installer_exit_codes_map_to_user_facing_failures() {
        assert_eq!(FailureKind::from_code(1602), FailureKind::Cancelled);
        assert_eq!(FailureKind::from_code(1618), FailureKind::Busy);
        assert_eq!(FailureKind::from_code(1620), FailureKind::InvalidPackage);
        assert_eq!(FailureKind::from_code(1605), FailureKind::NotInstalled);
        assert_eq!(FailureKind::from_code(1603), FailureKind::Fatal);
        assert_eq!(FailureKind::from_code(1234), FailureKind::Other(1234));
    }

    #[test]
    fn a_failure_keeps_the_log_path_it_was_written_to() {
        let failure = MsiFailure::new(1603, PathBuf::from(r"C:\Temp\cairn.log"));
        assert_eq!(failure.kind, FailureKind::Fatal);
        assert_eq!(failure.log, PathBuf::from(r"C:\Temp\cairn.log"));
    }
}
