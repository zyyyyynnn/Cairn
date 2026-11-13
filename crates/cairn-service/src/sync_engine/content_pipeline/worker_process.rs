// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Worker process surface: locating, spawning, bounding and collecting CairnWorker batches.

use super::BatchOutcome;
use cairn_protocol::status::ContentIndexTask;
use std::io::{Read, Write};
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::sync_engine::MultiVolumeEngine;

/// Upper bound for one worker batch. Cancellation is checked every 100 ms while waiting.
const CONTENT_WORKER_TIMEOUT: Duration = Duration::from_secs(300);
#[cfg(windows)]
fn lower_worker_priority(child: &std::process::Child) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, SetPriorityClass, PROCESS_MODE_BACKGROUND_BEGIN, PROCESS_SET_INFORMATION,
    };

    // SAFETY: the handle is opened for the child this process just spawned and is closed below.
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_SET_INFORMATION, false, child.id()) }) else {
        return;
    };
    // SAFETY: the handle is live; this only changes the child's scheduling/I/O priority.
    let _ = unsafe { SetPriorityClass(handle, PROCESS_MODE_BACKGROUND_BEGIN) };
    // SAFETY: paired with the successful OpenProcess above.
    let _ = unsafe { CloseHandle(handle) };
}

/// Port abstracting child-process execution and retry backoff for one `CairnWorker` batch.
pub(super) trait WorkerProcessPort {
    fn run_batch(
        &self,
        tasks: &[ContentIndexTask],
        deletions: &[(u64, u64)],
        clear: bool,
    ) -> BatchOutcome;

    /// Waits before retrying a transient sharing conflict; returns `false` if cancelled.
    fn wait_before_retry(&self) -> bool;
}

pub(super) struct SubprocessWorkerPort<'a> {
    engine: &'a MultiVolumeEngine,
}

impl<'a> SubprocessWorkerPort<'a> {
    pub(super) fn new(engine: &'a MultiVolumeEngine) -> Self {
        Self { engine }
    }
}

impl WorkerProcessPort for SubprocessWorkerPort<'_> {
    fn run_batch(
        &self,
        tasks: &[ContentIndexTask],
        deletions: &[(u64, u64)],
        clear: bool,
    ) -> BatchOutcome {
        self.engine.run_worker_process(tasks, deletions, clear)
    }

    fn wait_before_retry(&self) -> bool {
        std::thread::sleep(Duration::from_millis(250));
        !self.engine.content_cancel.load(Ordering::SeqCst)
    }
}

impl MultiVolumeEngine {
    fn worker_path() -> Result<PathBuf, String> {
        let exe = std::env::current_exe()
            .map_err(|err| format!("could not locate the service executable: {err}"))?;
        let dir = exe
            .parent()
            .ok_or_else(|| "service executable has no parent directory".to_string())?;
        // Production ships the worker beside the service only. A test harness may live under
        // cargo's build-script output tree, so tests also walk the enclosing target directories;
        // production never consults a parent, where a planted worker could shadow the packaged one.
        #[cfg(test)]
        let search_dirs: Vec<PathBuf> = dir
            .ancestors()
            .map(|ancestor| ancestor.to_path_buf())
            .collect();
        #[cfg(not(test))]
        let search_dirs = vec![dir.to_path_buf()];
        for base in search_dirs {
            for name in ["CairnWorker.exe", "CairnWorker"] {
                let candidate = base.join(name);
                if candidate.exists() {
                    return Ok(candidate);
                }
            }
        }
        Err(format!(
            "CairnWorker executable not found beside {}",
            exe.display()
        ))
    }

    /// Runs one worker batch, retrying once when the failure looks like a transient sharing
    /// conflict. The previous searcher is dropped first so its mapped index segments cannot keep
    /// the files locked while the retry runs; re-running a batch (including `--clear`) is
    /// idempotent, so no request kind is special-cased.
    pub(super) fn run_worker_batch(
        &self,
        tasks: &[ContentIndexTask],
        deletions: &[(u64, u64)],
        clear: bool,
    ) -> BatchOutcome {
        self.run_worker_batch_with(&SubprocessWorkerPort::new(self), tasks, deletions, clear)
    }

    pub(super) fn run_worker_batch_with(
        &self,
        port: &dyn WorkerProcessPort,
        tasks: &[ContentIndexTask],
        deletions: &[(u64, u64)],
        clear: bool,
    ) -> BatchOutcome {
        let first = port.run_batch(tasks, deletions, clear);
        let err = match &first {
            BatchOutcome::Failed(failure) if failure.transient => failure.message.clone(),
            _ => return first,
        };
        tracing::warn!(
            "CairnWorker hit a transient sharing error; releasing the searcher and retrying once: {err}"
        );
        self.drop_content_searcher();
        if !port.wait_before_retry() || self.content_cancel.load(Ordering::SeqCst) {
            return BatchOutcome::Cancelled;
        }
        port.run_batch(tasks, deletions, clear)
    }

    fn run_worker_process(
        &self,
        tasks: &[ContentIndexTask],
        deletions: &[(u64, u64)],
        clear: bool,
    ) -> BatchOutcome {
        let worker = match Self::worker_path() {
            Ok(path) => path,
            Err(err) => return BatchOutcome::failed(err),
        };
        if let Err(err) = std::fs::create_dir_all(&self.content_dir) {
            return BatchOutcome::failed(format!(
                "could not create content directory {}: {err}",
                self.content_dir.display()
            ));
        }

        let batch_path = self
            .content_dir
            .join(format!("batch-{}.jsonl", std::process::id()));
        let delete_path = (!deletions.is_empty()).then(|| {
            self.content_dir
                .join(format!("deletes-{}.jsonl", std::process::id()))
        });
        let cleanup_paths: Vec<PathBuf> = std::iter::once(batch_path.clone())
            .chain(delete_path.clone())
            .collect();

        if let Err(err) = write_json_lines(&batch_path, tasks) {
            remove_worker_files(&cleanup_paths);
            return BatchOutcome::failed(err);
        }
        if let Some(path) = &delete_path {
            if let Err(err) = write_json_lines(path, deletions) {
                remove_worker_files(&cleanup_paths);
                return BatchOutcome::failed(err);
            }
        }

        let cfg = self.get_config();
        let mut command = Command::new(worker);
        command
            .arg("--index-dir")
            .arg(&self.content_dir)
            .arg("--batch")
            .arg(&batch_path)
            .arg("--max-bytes")
            .arg(cfg.content_index_max_bytes.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use windows::Win32::System::Threading::BELOW_NORMAL_PRIORITY_CLASS;
            command.creation_flags(BELOW_NORMAL_PRIORITY_CLASS.0);
        }
        if let Some(path) = &delete_path {
            command.arg("--delete-batch").arg(path);
        }
        if clear {
            command.arg("--clear");
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(err) => {
                remove_worker_files(&cleanup_paths);
                return BatchOutcome::failed(format!("could not start CairnWorker: {err}"));
            }
        };
        #[cfg(windows)]
        lower_worker_priority(&child);
        // Drain both stdout and stderr while the worker runs, capping kept bytes to 64 KiB and
        // discarding any excess so neither pipe can fill its OS buffer and deadlock the child or
        // exhaust service memory.
        const MAX_WORKER_PIPE_BYTES: u64 = 64 * 1024;
        let stdout_reader = child.stdout.take().map(|pipe| {
            std::thread::spawn(move || {
                let mut capped = pipe.take(MAX_WORKER_PIPE_BYTES);
                let mut buf = Vec::new();
                let _ = capped.read_to_end(&mut buf);
                let mut rest = capped.into_inner();
                let _ = std::io::copy(&mut rest, &mut std::io::sink());
                String::from_utf8_lossy(&buf).into_owned()
            })
        });
        let stderr_reader = child.stderr.take().map(|pipe| {
            std::thread::spawn(move || {
                let mut capped = pipe.take(MAX_WORKER_PIPE_BYTES);
                let mut buf = Vec::new();
                let _ = capped.read_to_end(&mut buf);
                let mut rest = capped.into_inner();
                let _ = std::io::copy(&mut rest, &mut std::io::sink());
                String::from_utf8_lossy(&buf).into_owned()
            })
        });
        let deadline = Instant::now() + CONTENT_WORKER_TIMEOUT;

        loop {
            if self.content_cancel.load(Ordering::SeqCst) {
                let _ = child.kill();
                let _ = child.wait();
                remove_worker_files(&cleanup_paths);
                return BatchOutcome::Cancelled;
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    let output = stdout_reader
                        .and_then(|reader| reader.join().ok())
                        .unwrap_or_default();
                    let errors = stderr_reader
                        .and_then(|reader| reader.join().ok())
                        .unwrap_or_default();
                    remove_worker_files(&cleanup_paths);
                    if !status.success() {
                        return BatchOutcome::worker_failed(
                            format!("CairnWorker exited with {status}: {output} {errors}"),
                            status.code(),
                        );
                    }
                    let summary: serde_json::Value =
                        serde_json::from_str(output.trim()).unwrap_or_default();
                    let processed = summary["processed"].as_u64().unwrap_or(0);
                    let failed = summary["failed"].as_u64().unwrap_or(0);
                    if failed > 0 {
                        // A file can vanish between the USN event and extraction, so a batch of
                        // temporarily created files may legitimately skip everything. The worker
                        // still exited cleanly and committed its deletions; only a non-zero exit
                        // is a batch failure. Treating all-skipped as fatal disabled content
                        // search on ordinary temporary-file churn.
                        tracing::warn!(
                            processed,
                            failed,
                            "CairnWorker skipped content files: {}",
                            errors.trim()
                        );
                    }
                    return BatchOutcome::Completed;
                }
                Ok(None) => {}
                Err(err) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    remove_worker_files(&cleanup_paths);
                    return BatchOutcome::failed(format!("could not wait for CairnWorker: {err}"));
                }
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                remove_worker_files(&cleanup_paths);
                return BatchOutcome::failed("CairnWorker timed out");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Whether the worker's exit code names a handle conflict a retry can clear. Anything else (bad
/// arguments, a crashed worker, a timeout) is not retried at this level.
pub(super) fn worker_exit_is_transient(exit_code: Option<i32>) -> bool {
    matches!(
        exit_code,
        Some(code)
            if code == i32::from(cairn_protocol::worker_exit::ACCESS_DENIED)
                || code == i32::from(cairn_protocol::worker_exit::SHARING_VIOLATION)
    )
}

fn write_json_lines<T: serde::Serialize>(path: &Path, items: &[T]) -> Result<(), String> {
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

fn remove_worker_files(paths: &[PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}
