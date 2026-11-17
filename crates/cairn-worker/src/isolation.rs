// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Subprocess isolation for extended property extraction (`IDX-12`).
//!
//! Spawns `CairnWorker.exe` with hard wall-clock timeouts and cooperative cancellation so hung or
//! crashing extractors never block or crash the host service.

#![forbid(unsafe_code)]

use cairn_protocol::{PropertyExtractResult, PropertyExtractTask};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const MAX_CAPTURED_PIPE_BYTES: u64 = 64 * 1024;
static BATCH_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Result of running one isolated property-extraction batch in a child process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IsolationOutcome {
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

struct TempFileGuard {
    paths: Vec<PathBuf>,
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Extracts properties for `tasks` using `CairnWorker` in a timeout-bounded child process.
pub fn extract_with_isolation(
    worker_binary: &Path,
    tasks: &[PropertyExtractTask],
    work_dir: &Path,
    timeout: Duration,
    cancel: &AtomicBool,
) -> IsolationOutcome {
    run_command_with_isolation(
        worker_binary,
        &["--extract-properties".to_string()],
        tasks,
        work_dir,
        timeout,
        cancel,
    )
}

/// Runs `program` with `extra_args` plus `--batch <tasks.jsonl> --out <results.jsonl>`,
/// enforcing `timeout` and `cancel` so a hung or crashing child degrades cleanly.
pub fn run_command_with_isolation(
    program: &Path,
    extra_args: &[String],
    tasks: &[PropertyExtractTask],
    work_dir: &Path,
    timeout: Duration,
    cancel: &AtomicBool,
) -> IsolationOutcome {
    if tasks.is_empty() {
        return IsolationOutcome::Completed(Vec::new());
    }
    if cancel.load(Ordering::SeqCst) {
        return IsolationOutcome::Cancelled;
    }
    if let Err(err) = std::fs::create_dir_all(work_dir) {
        return IsolationOutcome::WorkerFailed {
            exit_code: None,
            reason: format!(
                "could not create worker directory {}: {err}",
                work_dir.display()
            ),
        };
    }

    let seq = BATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let batch_path = work_dir.join(format!("prop-batch-{}-{seq}.jsonl", std::process::id()));
    let out_path = work_dir.join(format!("prop-out-{}-{seq}.jsonl", std::process::id()));
    let _cleanup = TempFileGuard {
        paths: vec![batch_path.clone(), out_path.clone()],
    };

    if let Err(err) = write_jsonl(&batch_path, tasks) {
        return IsolationOutcome::WorkerFailed {
            exit_code: None,
            reason: err,
        };
    }

    let mut command = Command::new(program);
    command
        .args(extra_args)
        .arg("--batch")
        .arg(&batch_path)
        .arg("--out")
        .arg(&out_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return IsolationOutcome::WorkerFailed {
                exit_code: None,
                reason: format!("could not start {}: {err}", program.display()),
            };
        }
    };

    let stdout_reader = child.stdout.take().map(spawn_pipe_drain);
    let stderr_reader = child.stderr.take().map(spawn_pipe_drain);
    let deadline = Instant::now() + timeout;

    loop {
        if cancel.load(Ordering::SeqCst) {
            terminate_child(&mut child);
            join_reader(stdout_reader);
            join_reader(stderr_reader);
            return IsolationOutcome::Cancelled;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = join_reader(stdout_reader);
                let errors = join_reader(stderr_reader);
                if !status.success() {
                    return IsolationOutcome::WorkerFailed {
                        exit_code: status.code(),
                        reason: format!(
                            "worker exited with {status}: {} {}",
                            output.trim(),
                            errors.trim()
                        ),
                    };
                }
                return match read_jsonl(&out_path) {
                    Ok(results) => IsolationOutcome::Completed(results),
                    Err(err) => IsolationOutcome::WorkerFailed {
                        exit_code: status.code(),
                        reason: err,
                    },
                };
            }
            Ok(None) => {}
            Err(err) => {
                terminate_child(&mut child);
                join_reader(stdout_reader);
                join_reader(stderr_reader);
                return IsolationOutcome::WorkerFailed {
                    exit_code: None,
                    reason: format!("failed polling worker process: {err}"),
                };
            }
        }
        if Instant::now() >= deadline {
            terminate_child(&mut child);
            join_reader(stdout_reader);
            join_reader(stderr_reader);
            return IsolationOutcome::TimedOut {
                reason: format!(
                    "worker {} exceeded timeout of {} ms",
                    program.display(),
                    timeout.as_millis()
                ),
            };
        }
        std::thread::sleep(Duration::from_millis(25));
    }
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

fn write_jsonl<T: serde::Serialize>(path: &Path, items: &[T]) -> Result<(), String> {
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
