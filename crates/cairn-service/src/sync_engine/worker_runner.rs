// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Bounded low-priority subprocess runner shared by content and property extraction pipelines.
//!
//! The host spawns `CairnWorker.exe` with `BELOW_NORMAL_PRIORITY_CLASS`, while the worker
//! itself enters `PROCESS_MODE_BACKGROUND_BEGIN` on startup. Runs are bounded by wall-clock
//! timeouts, cooperative cancellation, and 64 KiB capped pipe drains.

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

pub(super) fn next_batch_seq() -> u64 {
    BATCH_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// RAII cleanup guard for per-batch worker scratch files across all exit paths.
pub(super) struct TempFileGuard {
    paths: Vec<PathBuf>,
}

impl TempFileGuard {
    pub(super) fn new(paths: Vec<PathBuf>) -> Self {
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
pub(super) enum WorkerProcessOutcome {
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
pub(super) enum PropertyIsolationOutcome {
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

/// Spawns `program` at below-normal priority with capped pipe drains until exit, timeout, or cancel.
pub(super) fn run_bounded_worker(
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
pub(super) fn extract_properties_isolated(
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

pub(super) fn write_jsonl<T: serde::Serialize>(path: &Path, items: &[T]) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|err| format!("could not create {}: {err}", path.display()))?;
    for item in items {
        let line = serde_json::to_string(item)
            .map_err(|err| format!("could not encode worker task: {err}"))?;
        writeln!(file, "{line}")
            .map_err(|err| format!("could not write {}: {err}", path.display()))?;
    }
    Ok(())
}

fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>, String> {
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
    use cairn_core::property_store::{RecordPropertyData, SourceId};
    use std::sync::Arc;

    #[cfg(windows)]
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

    #[cfg(windows)]
    #[test]
    fn excessive_pipe_output_is_drained_without_deadlock_and_capped_at_limit() {
        let outcome = run_bounded_worker(
            Path::new("powershell"),
            &[
                OsString::from("-NoProfile"),
                OsString::from("-Command"),
                OsString::from("[Console]::Out.Write('x' * 131072)"),
            ],
            Duration::from_secs(30),
            Duration::from_millis(25),
            &AtomicBool::new(false),
        );
        match outcome {
            WorkerProcessOutcome::Completed { stdout, .. } => {
                assert_eq!(stdout.len(), MAX_CAPTURED_PIPE_BYTES as usize);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn extract_properties_isolated_short_circuits_empty_and_pre_cancelled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let work_dir = dir.path().join("missing-work");

        let empty = extract_properties_isolated(
            Path::new("nonexistent-worker.exe"),
            &[],
            &work_dir,
            Duration::from_secs(1),
            &AtomicBool::new(false),
        );
        assert_eq!(empty, PropertyIsolationOutcome::Completed(Vec::new()));
        assert!(
            !work_dir.exists(),
            "empty task batch must not create work directory"
        );

        let task = PropertyExtractTask {
            source_id: SourceId::for_drive('C').expect("C:"),
            record_key: 1,
            modified_ts: 100,
            path: r"C:\sample.png".to_string(),
        };
        let cancelled = extract_properties_isolated(
            Path::new("nonexistent-worker.exe"),
            &[task],
            &work_dir,
            Duration::from_secs(1),
            &AtomicBool::new(true),
        );
        assert_eq!(cancelled, PropertyIsolationOutcome::Cancelled);
        assert!(
            !work_dir.exists(),
            "pre-cancelled batch must not create work directory"
        );
    }

    #[cfg(windows)]
    #[test]
    fn extract_properties_isolated_handles_missing_malformed_and_valid_output_and_cleans_temp_files()
     {
        let dir = tempfile::tempdir().expect("tempdir");
        let work_dir = dir.path().join("work");
        let task = PropertyExtractTask {
            source_id: SourceId::for_drive('C').expect("C:"),
            record_key: 42,
            modified_ts: 900,
            path: r"C:\clip.mp4".to_string(),
        };

        let missing_cmd = dir.path().join("missing_out.cmd");
        std::fs::write(&missing_cmd, "@echo off\r\nexit /b 0\r\n").expect("write missing_out");
        match extract_properties_isolated(
            &missing_cmd,
            std::slice::from_ref(&task),
            &work_dir,
            Duration::from_secs(5),
            &AtomicBool::new(false),
        ) {
            PropertyIsolationOutcome::WorkerFailed { exit_code, reason } => {
                assert_eq!(exit_code, Some(0));
                assert!(reason.contains("worker output missing"), "{reason}");
            }
            other => panic!("expected WorkerFailed for missing output, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_dir(&work_dir).expect("read work_dir").count(),
            0,
            "TempFileGuard must clean batch files after missing output"
        );

        let malformed_cmd = dir.path().join("malformed_out.cmd");
        std::fs::write(&malformed_cmd, "@echo off\r\necho not-json> \"%~5\"\r\n")
            .expect("write malformed_out");
        match extract_properties_isolated(
            &malformed_cmd,
            std::slice::from_ref(&task),
            &work_dir,
            Duration::from_secs(5),
            &AtomicBool::new(false),
        ) {
            PropertyIsolationOutcome::WorkerFailed { exit_code, reason } => {
                assert_eq!(exit_code, Some(0));
                assert!(reason.contains("malformed worker output line"), "{reason}");
            }
            other => panic!("expected WorkerFailed for malformed output, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_dir(&work_dir).expect("read work_dir").count(),
            0,
            "TempFileGuard must clean batch files after malformed output"
        );

        let expected = PropertyExtractResult {
            source_id: task.source_id,
            record_key: task.record_key,
            modified_ts: task.modified_ts,
            properties: Some(RecordPropertyData {
                length_ms: Some(1500),
                author: Some("tester".to_string()),
                tags: None,
                dimensions: Some((1920, 1080)),
                framerate_mfps: Some(30_000),
            }),
        };
        let fixture = dir.path().join("valid_fixture.jsonl");
        std::fs::write(
            &fixture,
            format!("{}\n", serde_json::to_string(&expected).expect("json")),
        )
        .expect("write fixture");
        let valid_cmd = dir.path().join("valid_out.cmd");
        std::fs::write(
            &valid_cmd,
            format!(
                "@echo off\r\ncopy /Y \"{}\" \"%~5\" >nul\r\n",
                fixture.display()
            ),
        )
        .expect("write valid_out");

        let ok = extract_properties_isolated(
            &valid_cmd,
            std::slice::from_ref(&task),
            &work_dir,
            Duration::from_secs(5),
            &AtomicBool::new(false),
        );
        assert_eq!(ok, PropertyIsolationOutcome::Completed(vec![expected]));
        assert_eq!(
            std::fs::read_dir(&work_dir).expect("read work_dir").count(),
            0,
            "TempFileGuard must clean batch files after success"
        );
    }
}
