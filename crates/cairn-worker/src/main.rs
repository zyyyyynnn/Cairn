// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]

//! `cairn-worker.exe`: Isolated content extraction and indexing subprocess.
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
#[command(name = "cairn-worker", about = "Cairn Service Worker")]
struct WorkerCli {
    /// Directory where Tantivy content index is stored
    #[arg(short, long, default_value = r"C:\ProgramData\Cairn\content")]
    index_dir: PathBuf,

    /// JSON Lines batch produced by the service: one ContentIndexTask per line
    #[arg(short, long)]
    batch: Option<PathBuf>,

    /// JSON Lines deletions produced by the service: one [volume_id, frn] pair per line
    #[arg(long)]
    delete_batch: Option<PathBuf>,

    /// Single file path to extract and index (manual use)
    #[arg(short, long)]
    file: Option<PathBuf>,

    /// Maximum document bytes to extract per file
    #[arg(long, default_value_t = 10 * 1024 * 1024)]
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
        let contents = std::fs::read_to_string(delete_path)?;
        for line in contents.lines().filter(|line| !line.trim().is_empty()) {
            match serde_json::from_str::<(u64, u64)>(line) {
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
        let contents = std::fs::read_to_string(batch_path)?;
        for line in contents.lines().filter(|line| !line.trim().is_empty()) {
            let task: ContentIndexTask = match serde_json::from_str(line) {
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    let args = WorkerCli::parse();
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
    }

    if let Some(file_path) = args.file {
        let metadata = std::fs::metadata(&file_path)?;
        let task = ContentIndexTask {
            volume_id: 0,
            frn: 0,
            size: metadata.len(),
            modified_ts: metadata
                .modified()
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            path: file_path.to_string_lossy().into_owned(),
        };
        extract_and_index(&extractor, &index, &task, args.max_bytes)?;
        index.commit()?;
        info!("Indexed {}", task.path);
    } else if !args.clear {
        info!("Nothing to do: pass --batch, --delete-batch or --file");
    }

    index.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worker_cli_defaults() {
        let args = WorkerCli::try_parse_from(["cairn-worker"]).expect("parse defaults");
        assert_eq!(
            args.index_dir,
            PathBuf::from(r"C:\ProgramData\Cairn\content")
        );
        assert!(args.batch.is_none());
        assert!(args.delete_batch.is_none());
        assert!(args.file.is_none());
        assert_eq!(args.max_bytes, 10 * 1024 * 1024);
        assert!(!args.clear);
    }

    #[test]
    fn test_worker_cli_custom_flags() {
        let args = WorkerCli::try_parse_from([
            "cairn-worker",
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
}
