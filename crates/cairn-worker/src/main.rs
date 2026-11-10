// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]

//! `CairnWorker.exe`: Isolated content extraction and indexing subprocess.
//!
//! The service owns the MFT and the decision of which files are eligible. This process owns the
//! risky half: decoding a document, extracting its text and writing the Tantivy index. A malformed
//! file can crash or stall this process without touching the LocalSystem service.

use cairn_content::extractor::CompositeExtractor;
use cairn_content::indexer::ContentIndex;
use cairn_protocol::ContentIndexTask;
use clap::Parser;
use std::path::{Path, PathBuf};
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(name = "CairnWorker", about = "Cairn Service Worker")]
struct WorkerCli {
    /// Directory where Tantivy content index is stored
    #[arg(short, long)]
    index_dir: PathBuf,

    /// JSON Lines batch produced by the service: one ContentIndexTask per line
    #[arg(short, long)]
    batch: Option<PathBuf>,

    /// JSON Lines deletions produced by the service: one [volume_id, frn] pair per line
    #[arg(long)]
    delete_batch: Option<PathBuf>,

    /// Maximum document bytes to extract per file
    #[arg(long)]
    max_bytes: usize,

    /// Remove every document before applying this batch
    #[arg(long)]
    clear: bool,
}

fn extract_and_index(
    extractor: &CompositeExtractor,
    index: &ContentIndex,
    task: &ContentIndexTask,
    max_bytes: usize,
) -> Result<(), String> {
    let body = extractor
        .extract_file(std::path::Path::new(&task.path), max_bytes)
        .map_err(|err| err.to_string())?;
    index
        .index_document_deferred(
            task.volume_id,
            task.frn,
            task.size,
            task.modified_ts,
            &task.path,
            &body,
        )
        .map_err(|err| err.to_string())
}

/// Applies clear, deletions and documents to one writer generation and commits them together.
/// The caller waits for Tantivy's merges before this process exits.
fn apply_batches(
    extractor: &CompositeExtractor,
    index: &ContentIndex,
    batch: Option<&Path>,
    delete_batch: Option<&Path>,
    clear: bool,
    max_bytes: usize,
) -> Result<(u64, u64), Box<dyn std::error::Error>> {
    if clear {
        index.clear_deferred()?;
    }

    let mut processed = 0u64;
    let mut failed = 0u64;
    let mut deletions = Vec::new();
    if let Some(delete_path) = delete_batch {
        use std::io::BufRead;
        let file = std::fs::File::open(delete_path)?;
        let reader = std::io::BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<(u64, u64)>(&line) {
                Ok(pair) => deletions.push(pair),
                Err(err) => {
                    // Deletion lines share the summary's failure counter so a corrupt batch
                    // cannot be mistaken for a clean run.
                    failed += 1;
                    error!("Malformed deletion task: {err}");
                }
            }
        }
        index.delete_documents_deferred(&deletions)?;
    }

    if let Some(batch_path) = batch {
        use std::io::BufRead;
        let file = std::fs::File::open(batch_path)?;
        let reader = std::io::BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let task: ContentIndexTask = match serde_json::from_str(&line) {
                Ok(task) => task,
                Err(err) => {
                    failed += 1;
                    error!("Malformed batch task: {err}");
                    continue;
                }
            };
            match extract_and_index(extractor, index, &task, max_bytes) {
                Ok(()) => processed += 1,
                Err(err) => {
                    failed += 1;
                    info!("Skipped {}: {err}", task.path);
                }
            }
        }
    }

    if clear || !deletions.is_empty() || processed > 0 {
        index.commit()?;
    }

    Ok((processed, failed))
}

fn main() -> std::process::ExitCode {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    match run(WorkerCli::parse()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            error!("{error}");
            std::process::ExitCode::from(worker_exit_code(error.as_ref()))
        }
    }
}

/// The dominant failure kind the service uses to decide whether a retry can help. Errors from
/// Tantivy or the extractor wrap the OS error, so the whole source chain is searched.
fn worker_exit_code(error: &(dyn std::error::Error + 'static)) -> u8 {
    use cairn_protocol::worker_exit;
    match raw_os_error(error) {
        Some(5) => worker_exit::ACCESS_DENIED,
        Some(32) => worker_exit::SHARING_VIOLATION,
        _ => worker_exit::FAILED,
    }
}

fn raw_os_error(error: &(dyn std::error::Error + 'static)) -> Option<i32> {
    let mut current = Some(error);
    while let Some(err) = current {
        if let Some(code) = err
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::raw_os_error)
        {
            return Some(code);
        }
        current = err.source();
    }
    None
}

fn run(args: WorkerCli) -> Result<(), Box<dyn std::error::Error>> {
    let extractor = CompositeExtractor::new();
    let index = ContentIndex::open_or_create(&args.index_dir)?;

    if args.batch.is_some() || args.delete_batch.is_some() {
        let (processed, failed) = apply_batches(
            &extractor,
            &index,
            args.batch.as_deref(),
            args.delete_batch.as_deref(),
            args.clear,
            args.max_bytes,
        )?;
        // Merges run asynchronously; exiting now would abort them and leave one segment per
        // commit. The service starts a new worker for every batch, so the merge must finish here.
        index.finish()?;
        // The service reads this line; keep it the only stdout output.
        println!(
            "{}",
            serde_json::json!({ "processed": processed, "failed": failed })
        );
        return Ok(());
    }

    if args.clear {
        index.clear()?;
    } else {
        info!("Nothing to do: pass --batch, --delete-batch or --clear");
    }

    index.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worker_cli_custom_flags() {
        let args = WorkerCli::try_parse_from([
            "CairnWorker",
            "--index-dir",
            r"C:\test\idx",
            "--batch",
            r"C:\test\batch.jsonl",
            "--delete-batch",
            r"C:\test\del.jsonl",
            "--max-bytes",
            "4096",
            "--clear",
        ])
        .expect("parse custom");

        assert_eq!(args.index_dir, PathBuf::from(r"C:\test\idx"));
        assert_eq!(args.batch, Some(PathBuf::from(r"C:\test\batch.jsonl")));
        assert_eq!(args.delete_batch, Some(PathBuf::from(r"C:\test\del.jsonl")));
        assert_eq!(args.max_bytes, 4096);
        assert!(args.clear);
    }

    #[test]
    fn test_apply_batches_empty_and_malformed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let idx_dir = temp.path().join("idx");
        let index = ContentIndex::open_or_create(&idx_dir).expect("create index");
        let extractor = CompositeExtractor::new();

        // 1. Empty runs
        let (processed, failed) =
            apply_batches(&extractor, &index, None, None, false, 1024 * 1024).expect("empty batch");
        assert_eq!(processed, 0);
        assert_eq!(failed, 0);

        // 2. Malformed deletions and tasks
        let del_file = temp.path().join("dels.jsonl");
        std::fs::write(&del_file, "not a json\n[1, 2]\n").expect("write dels");

        let batch_file = temp.path().join("batch.jsonl");
        let valid_doc = temp.path().join("doc.txt");
        std::fs::write(&valid_doc, "Searchable text content here").expect("write doc");

        let valid_task = ContentIndexTask {
            volume_id: 1,
            frn: 10,
            size: 27,
            modified_ts: 1000,
            path: valid_doc.to_string_lossy().into_owned(),
        };

        let batch_contents = format!(
            "invalid task line\n{}\n",
            serde_json::to_string(&valid_task).expect("json")
        );
        std::fs::write(&batch_file, batch_contents).expect("write batch");

        let (processed, failed) = apply_batches(
            &extractor,
            &index,
            Some(&batch_file),
            Some(&del_file),
            false,
            1024 * 1024,
        )
        .expect("apply batches");

        assert_eq!(processed, 1);
        assert_eq!(failed, 2); // 1 malformed delete + 1 malformed task
    }

    #[derive(Debug)]
    struct WrappedError(std::io::Error);

    impl std::fmt::Display for WrappedError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "wrapped: {}", self.0)
        }
    }

    impl std::error::Error for WrappedError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn exit_codes_name_the_os_failure_even_when_wrapped() {
        use cairn_protocol::worker_exit;

        assert_eq!(
            worker_exit_code(&std::io::Error::from_raw_os_error(5)),
            worker_exit::ACCESS_DENIED
        );
        assert_eq!(
            worker_exit_code(&std::io::Error::from_raw_os_error(32)),
            worker_exit::SHARING_VIOLATION
        );
        assert_eq!(
            worker_exit_code(&std::io::Error::from_raw_os_error(2)),
            worker_exit::FAILED
        );
        assert_eq!(
            worker_exit_code(&WrappedError(std::io::Error::from_raw_os_error(32))),
            worker_exit::SHARING_VIOLATION,
            "a Tantivy/extractor wrapper must not hide the retryable OS error"
        );
        assert_eq!(
            worker_exit_code(&WrappedError(std::io::Error::from_raw_os_error(5))),
            worker_exit::ACCESS_DENIED
        );
    }
}
