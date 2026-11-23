// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Bounded low-priority subprocess runner shared by content and property extraction pipelines.
//!
//! All document and media parsing happens in `CairnWorker.exe` under `BELOW_NORMAL_PRIORITY_CLASS`
//! and background scheduling/I/O mode (SPEC IDX-03) with hard wall-clock timeouts, cooperative
//! cancellation, and 64 KiB capped pipe drains.

use cairn_protocol::{PropertyExtractResult, PropertyExtractTask};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const MAX_CAPTURED_PIPE_BYTES: u64 = 64 * 1024;
static BATCH_COUNTER: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_batch_seq() -> u64 {
    BATCH_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// RAII cleanup guard for per-batch worker scratch files across all exit paths.
pub(crate) struct TempFileGuard {
    paths: Vec<PathBuf>,
}

impl TempFileGuard {
    pub(crate) fn new(paths: Vec<PathBuf>) -> Self {
        Self { paths }
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Low-level outcome of running a single bounded subprocess.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkerProcessOutcome {
    Completed {
        stdout: String,
        stderr: String,
    },
    TimedOut {
        reason: String,
    },
    Cancelled,
    Failed {
        exit_code: Option<i32>,
        reason: String,
    },
}

/// Outcome of running an isolated property extraction batch in `CairnWorker.exe`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PropertyIsolationOutcome {
    Completed(Vec<PropertyExtractResult>),
    TimedOut {
        reason: String,
    },
    Cancelled,
    WorkerFailed {
        exit_code: Option<i32>,
        reason: String,
    },
}

#[cfg(windows)]
fn lower_worker_priority(child: &Child) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_MODE_BACKGROUND_BEGIN, PROCESS_SET_INFORMATION, SetPriorityClass,
    };

    // SAFETY: the handle is opened for the child this process just spawned and is closed below.
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_SET_INFORMATION, false, child.id()) }) else {
        return;
    };
    // SAFETY: the handle is live; this only lowers the child's scheduling and I/O priority.
    let _ = unsafe { SetPriorityClass(handle, PROCESS_MODE_BACKGROUND_BEGIN) };
    // SAFETY: paired with the successful OpenProcess above.
    let _ = unsafe { CloseHandle(handle) };
}

/// Spawns `program` at background priority with capped pipe drains until exit, timeout, or cancel.
pub(crate) fn run_bounded_worker(
    program: &Path,
    args: &[OsString],
    timeout: Duration,
    poll_interval: Duration,
    cancel: &AtomicBool,
) -> WorkerProcessOutcome {
    if cancel.load(Ordering::SeqCst) {
        return WorkerProcessOutcome::Cancelled;
    }

    let mut command = Command::new(program);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::BELOW_NORMAL_PRIORITY_CLASS;
        command.creation_flags(BELOW_NORMAL_PRIORITY_CLASS.0);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return WorkerProcessOutcome::Failed {
                exit_code: None,
                reason: format!("could not start {}: {err}", program.display()),
            };
        }
    };
    #[cfg(windows)]
    lower_worker_priority(&child);

    // Drain stdout and stderr concurrently up to 64 KiB, discarding excess so OS pipe buffers
    // cannot fill and deadlock the child or exhaust service memory.
    let stdout_reader = child.stdout.take().map(spawn_pipe_drain);
    let stderr_reader = child.stderr.take().map(spawn_pipe_drain);
    let deadline = Instant::now() + timeout;

    loop {
        if cancel.load(Ordering::SeqCst) {
            terminate_child(&mut child);
            join_reader(stdout_reader);
            join_reader(stderr_reader);
            return WorkerProcessOutcome::Cancelled;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = join_reader(stdout_reader);
                let stderr = join_reader(stderr_reader);
                if !status.success() {
                    return WorkerProcessOutcome::Failed {
                        exit_code: status.code(),
                        reason: format!(
                            "worker exited with {status}: {} {}",
                            stdout.trim(),
                            stderr.trim()
                        ),
                    };
                }
                return WorkerProcessOutcome::Completed { stdout, stderr };
            }
            Ok(None) => {}
            Err(err) => {
                terminate_child(&mut child);
                join_reader(stdout_reader);
                join_reader(stderr_reader);
                return WorkerProcessOutcome::Failed {
                    exit_code: None,
                    reason: format!("failed polling worker process: {err}"),
                };
            }
        }
        if Instant::now() >= deadline {
            terminate_child(&mut child);
            join_reader(stdout_reader);
            join_reader(stderr_reader);
            return WorkerProcessOutcome::TimedOut {
                reason: format!(
                    "worker {} exceeded timeout of {} ms",
                    program.display(),
                    timeout.as_millis()
                ),
            };
        }
        std::thread::sleep(poll_interval);
    }
}

/// Extracts extended properties for `tasks` via `CairnWorker --extract-properties`.
pub(crate) fn extract_properties_isolated(
    worker_binary: &Path,
    tasks: &[PropertyExtractTask],
    work_dir: &Path,
    timeout: Duration,
    cancel: &AtomicBool,
) -> PropertyIsolationOutcome {
    if tasks.is_empty() {
        return PropertyIsolationOutcome::Completed(Vec::new());
    }
    if cancel.load(Ordering::SeqCst) {
        return PropertyIsolationOutcome::Cancelled;
    }
    if let Err(err) = std::fs::create_dir_all(work_dir) {
        return PropertyIsolationOutcome::WorkerFailed {
            exit_code: None,
            reason: format!(
                "could not create worker directory {}: {err}",
                work_dir.display()
            ),
        };
    }

    let seq = next_batch_seq();
    let batch_path = work_dir.join(format!("prop-batch-{}-{seq}.jsonl", std::process::id()));
    let out_path = work_dir.join(format!("prop-out-{}-{seq}.jsonl", std::process::id()));
    let _cleanup = TempFileGuard::new(vec![batch_path.clone(), out_path.clone()]);

    if let Err(err) = write_jsonl(&batch_path, tasks) {
        return PropertyIsolationOutcome::WorkerFailed {
            exit_code: None,
            reason: err,
        };
    }

    let args = vec![
        OsString::from("--extract-properties"),
        OsString::from("--batch"),
        batch_path.into_os_string(),
        OsString::from("--out"),
        out_path.clone().into_os_string(),
    ];

    match run_bounded_worker(
        worker_binary,
        &args,
        timeout,
        Duration::from_millis(25),
        cancel,
    ) {
        WorkerProcessOutcome::Completed { .. } => match read_jsonl(&out_path) {
            Ok(results) => PropertyIsolationOutcome::Completed(results),
            Err(err) => PropertyIsolationOutcome::WorkerFailed {
                exit_code: Some(0),
                reason: err,
            },
        },
        WorkerProcessOutcome::TimedOut { reason } => PropertyIsolationOutcome::TimedOut { reason },
        WorkerProcessOutcome::Cancelled => PropertyIsolationOutcome::Cancelled,
        WorkerProcessOutcome::Failed { exit_code, reason } => {
            PropertyIsolationOutcome::WorkerFailed { exit_code, reason }
        }
    }
}

pub(crate) fn write_jsonl<T: serde::Serialize>(path: &Path, items: &[T]) -> Result<(), String> {
    let mut file = std::fs::File::create(path)
        .map_err(|err| format!("could not create {}: {err}", path.display()))?;
    for item in items {
        let line = serde_json::to_string(item)
            .map_err(|err| format!("could not encode worker task: {err}"))?;
        writeln!(file, "{line}")
            .map_err(|err| format!("could not write {}: {err}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>, String> {
    let file = std::fs::File::open(path)
        .map_err(|err| format!("worker output missing at {}: {err}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut items = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|err| format!("could not read {}: {err}", path.display()))?;
        if read == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let item = serde_json::from_str(trimmed)
            .map_err(|err| format!("malformed worker output line: {err}"))?;
        items.push(item);
    }
    Ok(items)
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn spawn_pipe_drain<R: Read + Send + 'static>(pipe: R) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut capped = pipe.take(MAX_CAPTURED_PIPE_BYTES);
        let mut buf = Vec::new();
        let _ = capped.read_to_end(&mut buf);
        let mut rest = capped.into_inner();
        let _ = std::io::copy(&mut rest, &mut std::io::sink());
        String::from_utf8_lossy(&buf).into_owned()
    })
}

fn join_reader(handle: Option<std::thread::JoinHandle<String>>) -> String {
    handle.and_then(|h| h.join().ok()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn hung_worker_is_killed_on_timeout_cancelled_and_crash_recovers_cleanly() {
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let timeout_outcome = run_bounded_worker(
            Path::new("powershell"),
            &[
                OsString::from("-NoProfile"),
                OsString::from("-Command"),
                OsString::from("Start-Sleep -Seconds 20"),
            ],
            Duration::from_millis(250),
            Duration::from_millis(25),
            &cancel,
        );
        assert!(
            matches!(timeout_outcome, WorkerProcessOutcome::TimedOut { .. }),
            "hung worker must time out cleanly: {timeout_outcome:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "timeout must kill hung child promptly"
        );

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_trigger = Arc::clone(&cancel_flag);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            cancel_trigger.store(true, Ordering::SeqCst);
        });
        let cancelled = run_bounded_worker(
            Path::new("powershell"),
            &[
                OsString::from("-NoProfile"),
                OsString::from("-Command"),
                OsString::from("Start-Sleep -Seconds 20"),
            ],
            Duration::from_secs(10),
            Duration::from_millis(25),
            &cancel_flag,
        );
        assert!(
            matches!(cancelled, WorkerProcessOutcome::Cancelled),
            "cancellation must terminate hung worker: {cancelled:?}"
        );

        let crashed = run_bounded_worker(
            Path::new("cmd"),
            &[OsString::from("/C"), OsString::from("exit 42")],
            Duration::from_secs(5),
            Duration::from_millis(25),
            &AtomicBool::new(false),
        );
        match crashed {
            WorkerProcessOutcome::Failed { exit_code, .. } => {
                assert_eq!(exit_code, Some(42));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }
}
