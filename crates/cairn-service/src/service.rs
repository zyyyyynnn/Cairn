// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Windows Service dispatcher and background lifecycle controller.

use crate::startup::ServiceRuntime;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tracing::{error, info};
use windows_service::{
    define_windows_service,
    service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
    },
    service_control_handler::{self, ServiceControlHandlerResult, ServiceStatusHandle},
    service_dispatcher,
};

pub const SERVICE_NAME: &str = "CairnService";

/// Time allowed by the SCM for the service to report the next checkpoint.
const START_WAIT_HINT: Duration = Duration::from_secs(3);
/// Checkpoints are reported more often than [`START_WAIT_HINT`] so a long cold start (the initial
/// `$MFT` scan) is not mistaken for a hung service that the SCM may terminate.
const START_CHECKPOINT_INTERVAL: Duration = Duration::from_secs(2);

define_windows_service!(ffi_service_main, cairn_service_main);

pub fn run_service() -> Result<(), windows_service::Error> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
}

pub fn cairn_service_main(_arguments: Vec<std::ffi::OsString>) {
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);

    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                let _ = shutdown_tx.blocking_send(());
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = match service_control_handler::register(SERVICE_NAME, event_handler) {
        Ok(handle) => handle,
        Err(e) => {
            error!("Failed to register service control handler: {}", e);
            return;
        }
    };

    // StartPending is reported before the fallible work below. Only a later Running report tells
    // the SCM (and therefore MSI's StartServices action) that the service actually started; calling
    // Running here would turn a failed build into an apparent success.
    let _ = status_handle.set_service_status(pending_status(1));
    let (stop_reporter, reporter) = spawn_start_checkpoint_reporter(status_handle);

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            error!("Cairn service did not start: tokio runtime could not be created: {err}");
            report_start_failure(status_handle, stop_reporter, reporter);
            return;
        }
    };

    // The data locations are secured before anything is read from them. If that cannot be done --
    // for example because an unprivileged account pre-created them and their ownership cannot be
    // restored here -- the service refuses to index instead of running on an uncontrolled store.
    let runtime = match ServiceRuntime::build() {
        Ok(runtime) => runtime,
        Err(err) => {
            error!("Cairn service did not start: {err}");
            report_start_failure(status_handle, stop_reporter, reporter);
            return;
        }
    };

    // A stop requested while the runtime was being built must not be overwritten with Running.
    let stop_requested = shutdown_rx.try_recv().is_ok();
    stop_start_checkpoint_reporter(stop_reporter, reporter);
    if stop_requested {
        info!("Stop was requested during startup; exiting before serving requests");
        let _ = status_handle.set_service_status(stopped_status(0));
        return;
    }

    let _ = status_handle.set_service_status(state_after_startup(true).service_status());

    rt.block_on(async move {
        let shutdown = async move {
            let _ = shutdown_rx.recv().await;
        };
        runtime.run(shutdown).await;
    });

    let _ = status_handle.set_service_status(stopped_status(0));
}

/// The SCM phases of a service start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupState {
    /// Work is in progress; `checkpoint` advances while it runs.
    Pending { checkpoint: u32 },
    /// The runtime was built; this is the SCM's signal of a successful start.
    Running,
    /// Startup failed. The start must fail visibly instead of succeeding and stopping immediately.
    Failed { exit_code: u32 },
}

impl StartupState {
    fn service_status(self) -> ServiceStatus {
        match self {
            StartupState::Pending { checkpoint } => ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: ServiceState::StartPending,
                controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
                exit_code: ServiceExitCode::Win32(0),
                checkpoint,
                wait_hint: START_WAIT_HINT,
                process_id: None,
            },
            StartupState::Running => running_status(),
            StartupState::Failed { exit_code } => stopped_status(exit_code),
        }
    }
}

/// The status a finished startup attempt must report: Running only once the runtime exists,
/// otherwise Stopped(1). Kept pure so both outcomes are testable without a service control manager.
fn state_after_startup(runtime_built: bool) -> StartupState {
    if runtime_built {
        StartupState::Running
    } else {
        StartupState::Failed { exit_code: 1 }
    }
}

fn pending_status(checkpoint: u32) -> ServiceStatus {
    StartupState::Pending { checkpoint }.service_status()
}

fn running_status() -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    }
}

fn stopped_status(exit_code: u32) -> ServiceStatus {
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(exit_code),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    }
}

/// Keeps the SCM informed while the data tree is secured and the first index is built: the initial
/// pending status is followed by increasing checkpoints until the reporter is stopped.
fn spawn_start_checkpoint_reporter(
    status_handle: ServiceStatusHandle,
) -> (mpsc::Sender<()>, JoinHandle<()>) {
    let (stop_tx, stop_rx) = mpsc::channel();
    let reporter = thread::spawn(move || {
        let mut checkpoint = 1u32;
        loop {
            match stop_rx.recv_timeout(START_CHECKPOINT_INTERVAL) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    checkpoint = checkpoint.checked_add(1).unwrap_or(1);
                    if status_handle
                        .set_service_status(pending_status(checkpoint))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    });
    (stop_tx, reporter)
}

/// Stops the checkpoint reporter before the final status is reported, so no stale StartPending can
/// overwrite Running or Stopped.
fn stop_start_checkpoint_reporter(sender: mpsc::Sender<()>, reporter: JoinHandle<()>) {
    let _ = sender.send(());
    let _ = reporter.join();
}

fn report_start_failure(
    status_handle: ServiceStatusHandle,
    stop_reporter: mpsc::Sender<()>,
    reporter: JoinHandle<()>,
) {
    stop_start_checkpoint_reporter(stop_reporter, reporter);
    let _ = status_handle.set_service_status(state_after_startup(false).service_status());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_status_keeps_the_scm_waiting() {
        let status = state_after_startup(true);
        assert_eq!(status, StartupState::Running);
        let running = status.service_status();
        assert_eq!(running.current_state, ServiceState::Running);
        assert_eq!(running.checkpoint, 0);

        let pending = pending_status(1);
        assert_eq!(pending.current_state, ServiceState::StartPending);
        assert_eq!(pending.checkpoint, 1);
        assert_eq!(pending.wait_hint, START_WAIT_HINT);
        assert!(
            pending
                .controls_accepted
                .contains(ServiceControlAccept::STOP),
            "StartPending must accept STOP"
        );
        assert!(
            pending
                .controls_accepted
                .contains(ServiceControlAccept::SHUTDOWN),
            "StartPending must accept SHUTDOWN"
        );
    }

    #[test]
    fn failed_startup_reports_a_failed_start() {
        let state = state_after_startup(false);
        assert_eq!(state, StartupState::Failed { exit_code: 1 });

        let stopped = state.service_status();
        assert_eq!(stopped.current_state, ServiceState::Stopped);
        assert_eq!(stopped.exit_code, ServiceExitCode::Win32(1));
        assert_eq!(stopped.checkpoint, 0);
        assert_eq!(stopped.wait_hint, Duration::default());
    }

    #[test]
    fn shutdown_still_reports_a_clean_stop() {
        let stopped = stopped_status(0);
        assert_eq!(stopped.current_state, ServiceState::Stopped);
        assert_eq!(stopped.exit_code, ServiceExitCode::Win32(0));
        assert_eq!(stopped.checkpoint, 0);
    }
}
