// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]

//! `CairnWorker.exe`: Isolated content and extended-property extraction subprocess.
//!
//! The service owns the MFT and the decision of which files are eligible. This process owns the
//! risky half: decoding a document, extracting its text or its extended properties, and writing
//! the Tantivy index. A malformed file can crash or stall this process without touching the
//! `LocalSystem` service.

use cairn_content::indexer::ContentWriter;
use cairn_protocol::{ContentIndexTask, PropertyExtractResult, PropertyExtractTask};
use cairn_worker::extractor::CompositeExtractor;
use cairn_worker::property_extract::extract_file_properties;
use clap::Parser;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(name = "CairnWorker", about = "Cairn Service Worker")]
struct WorkerCli {
    /// Directory where Tantivy content index is stored
    #[arg(short, long, required_unless_present = "extract_properties")]
    index_dir: Option<PathBuf>,

    /// Extract extended file properties instead of writing the content index
    #[arg(long)]
    extract_properties: bool,

    /// JSON Lines output path for property extraction mode
    #[arg(long)]
    out: Option<PathBuf>,

    /// JSON Lines batch produced by the service
    #[arg(short, long)]
    batch: Option<PathBuf>,

    #[arg(
        long,
        help = "JSON Lines deletions produced by the service: one [volume_id, frn] pair per line"
    )]
    delete_batch: Option<PathBuf>,

    /// Maximum document bytes to extract per file
    #[arg(long, default_value_t = 2 * 1024 * 1024)]
    max_bytes: usize,

    /// Remove every document before applying this batch
    #[arg(long)]
    clear: bool,
}

fn extract_and_index(
    extractor: &CompositeExtractor,
    writer: &ContentWriter,
    task: &ContentIndexTask,
    max_bytes: usize,
) -> Result<(), String> {
    let body = extractor
        .extract_file(Path::new(&task.path), max_bytes)
        .map_err(|err| err.to_string())?;
    writer
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

fn for_each_non_empty_line<R: BufRead, E>(
    mut reader: R,
    mut on_line: impl FnMut(&str) -> Result<(), E>,
) -> Result<(), Box<dyn std::error::Error>>
where
    E: Into<Box<dyn std::error::Error>>,
{
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            on_line(trimmed).map_err(Into::into)?;
        }
    }
    Ok(())
}

/// Applies clear, deletions and documents to one writer generation and commits them together.
/// The caller waits for Tantivy's merges before this process exits.
fn apply_batches<RB: BufRead, RD: BufRead>(
    extractor: &CompositeExtractor,
    writer: &ContentWriter,
    batch: Option<RB>,
    delete_batch: Option<RD>,
    clear: bool,
    max_bytes: usize,
) -> Result<(u64, u64), Box<dyn std::error::Error>> {
    if clear {
        writer.clear_deferred()?;
    }

    let mut processed = 0u64;
    let mut failed = 0u64;
    let mut deletions = Vec::new();
    if let Some(delete_reader) = delete_batch {
        for_each_non_empty_line(delete_reader, |line| {
            match serde_json::from_str::<(u64, u64)>(line) {
                Ok(pair) => deletions.push(pair),
                Err(err) => {
                    // Deletion lines share the summary's failure counter so a corrupt batch
                    // cannot be mistaken for a clean run.
                    failed += 1;
                    error!("Malformed deletion task: {err}");
                }
            }
            Ok::<(), Box<dyn std::error::Error>>(())
        })?;
        writer.delete_documents_deferred(&deletions)?;
    }

    if let Some(batch_reader) = batch {
        for_each_non_empty_line(batch_reader, |line| {
            let task: ContentIndexTask = match serde_json::from_str(line) {
                Ok(task) => task,
                Err(err) => {
                    failed += 1;
                    error!("Malformed batch task: {err}");
                    return Ok::<(), Box<dyn std::error::Error>>(());
                }
            };
            match extract_and_index(extractor, writer, &task, max_bytes) {
                Ok(()) => processed += 1,
                Err(err) => {
                    failed += 1;
                    info!("Skipped {}: {err}", task.path);
                }
            }
            Ok(())
        })?;
    }

    if clear || !deletions.is_empty() || processed > 0 {
        writer.commit()?;
    }

    Ok((processed, failed))
}

fn main() -> std::process::ExitCode {
    let args = WorkerCli::parse();
    cairn_worker::enter_background_mode();

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    match run(args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            error!("{error}");
            std::process::ExitCode::from(worker_exit_code(error.as_ref()))
        }
    }
}

/// The dominant failure kind the service uses to decide whether a retry can help. The search
/// covers wrapped `io::Error`s and the OS codes Tantivy keeps inside its own error variants.
fn worker_exit_code(error: &(dyn std::error::Error + 'static)) -> u8 {
    use cairn_protocol::worker_exit;
    match cairn_content::raw_os_error(error) {
        Some(5) => worker_exit::ACCESS_DENIED,
        Some(32) => worker_exit::SHARING_VIOLATION,
        _ => worker_exit::FAILED,
    }
}

fn open_buf_reader(path: &Path) -> std::io::Result<BufReader<File>> {
    Ok(BufReader::new(File::open(path)?))
}

fn run_worker<RB: BufRead, RD: BufRead, W: Write>(
    index_dir: &Path,
    batch: Option<RB>,
    delete_batch: Option<RD>,
    clear: bool,
    max_bytes: usize,
    out: &mut W,
) -> Result<(), Box<dyn std::error::Error>> {
    let extractor = CompositeExtractor::new();
    let writer = ContentWriter::open_or_create(index_dir)?;

    if batch.is_some() || delete_batch.is_some() {
        let (processed, failed) =
            apply_batches(&extractor, &writer, batch, delete_batch, clear, max_bytes)?;
        // Merges run asynchronously; exiting now would abort them and leave one segment per
        // commit. The service starts a new worker for every batch, so the merge must finish here.
        writer.finish()?;
        // The service reads this line; keep it the only stdout output.
        writeln!(
            out,
            "{}",
            serde_json::json!({ "processed": processed, "failed": failed })
        )?;
        out.flush()?;
        return Ok(());
    }

    if clear {
        writer.clear()?;
    } else {
        info!("Nothing to do: pass --batch, --delete-batch or --clear");
    }

    writer.finish()?;
    Ok(())
}

fn write_property_output_file(
    out_path: &Path,
    batch: Option<BufReader<File>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out_path)?;
    let mut writer = BufWriter::new(file);
    run_property_worker(batch, &mut writer)?;
    writer.flush()?;
    Ok(())
}

fn run_property_worker<RB: BufRead, W: Write>(
    batch: Option<RB>,
    out: &mut W,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(reader) = batch {
        let mut malformed = 0u64;
        for_each_non_empty_line(reader, |line| {
            let task: PropertyExtractTask = match serde_json::from_str(line) {
                Ok(task) => task,
                Err(err) => {
                    malformed += 1;
                    error!("Malformed property task: {err}");
                    return Ok::<(), Box<dyn std::error::Error>>(());
                }
            };
            let properties = extract_file_properties(Path::new(&task.path));
            let result = PropertyExtractResult {
                source_id: task.source_id,
                record_key: task.record_key,
                modified_ts: task.modified_ts,
                properties,
            };
            let encoded = serde_json::to_string(&result)?;
            writeln!(out, "{encoded}")?;
            Ok(())
        })?;
        if malformed > 0 {
            return Err(format!("{malformed} malformed property task(s)").into());
        }
        out.flush()?;
    }
    Ok(())
}

fn run(args: WorkerCli) -> Result<(), Box<dyn std::error::Error>> {
    let batch = args.batch.as_deref().map(open_buf_reader).transpose()?;
    if args.extract_properties {
        if let Some(out_path) = &args.out {
            write_property_output_file(out_path, batch)?;
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            writeln!(lock, "{{}}")?;
            lock.flush()?;
        } else {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            run_property_worker(batch, &mut lock)?;
        }
        return Ok(());
    }

    let index_dir = args
        .index_dir
        .as_deref()
        .ok_or("--index-dir is required unless --extract-properties is set")?;
    let delete_batch = args
        .delete_batch
        .as_deref()
        .map(open_buf_reader)
        .transpose()?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    run_worker(
        index_dir,
        batch,
        delete_batch,
        args.clear,
        args.max_bytes,
        &mut out,
    )
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

        assert_eq!(args.index_dir, Some(PathBuf::from(r"C:\test\idx")));
        assert_eq!(args.batch, Some(PathBuf::from(r"C:\test\batch.jsonl")));
        assert_eq!(args.delete_batch, Some(PathBuf::from(r"C:\test\del.jsonl")));
        assert_eq!(args.max_bytes, 4096);
        assert!(args.clear);
    }

    #[test]
    fn malformed_property_task_fails_the_worker() {
        use std::io::Cursor;

        let mut out = Vec::new();
        assert!(run_property_worker(Some(Cursor::new(b"not json\n")), &mut out).is_err());
        assert!(out.is_empty());
    }

    #[test]
    fn test_apply_batches_empty_and_malformed() {
        use std::io::Cursor;

        let temp = tempfile::tempdir().expect("tempdir");
        let idx_dir = temp.path().join("idx");
        let writer = ContentWriter::open_or_create(&idx_dir).expect("create writer");
        let extractor = CompositeExtractor::new();

        let (processed, failed) = apply_batches(
            &extractor,
            &writer,
            None::<Cursor<&[u8]>>,
            None::<Cursor<&[u8]>>,
            false,
            1024 * 1024,
        )
        .expect("empty batch");
        assert_eq!(processed, 0);
        assert_eq!(failed, 0);

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
            &writer,
            Some(open_buf_reader(&batch_file).expect("open batch")),
            Some(open_buf_reader(&del_file).expect("open dels")),
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

    #[test]
    fn run_worker_writes_summary_and_persists_content_writer_index() {
        use cairn_content::ContentSearcher;
        use std::io::Cursor;

        let temp = tempfile::tempdir().expect("tempdir");
        let idx_dir = temp.path().join("idx");
        let doc_a = temp.path().join("alpha.txt");
        let doc_b = temp.path().join("beta.txt");
        std::fs::write(&doc_a, "Rust 全文检索架构").expect("write alpha");
        std::fs::write(&doc_b, "将被删除的旧文档").expect("write beta");

        let seed_task = ContentIndexTask {
            volume_id: 1,
            frn: 200,
            size: 24,
            modified_ts: 100,
            path: doc_b.to_string_lossy().into_owned(),
        };
        let seed_jsonl = format!("{}\n", serde_json::to_string(&seed_task).unwrap());
        let mut seed_out = Vec::new();
        run_worker(
            &idx_dir,
            Some(Cursor::new(seed_jsonl.as_bytes())),
            None::<Cursor<&[u8]>>,
            false,
            1024 * 1024,
            &mut seed_out,
        )
        .expect("seed run_worker");

        let new_task = ContentIndexTask {
            volume_id: 1,
            frn: 100,
            size: 23,
            modified_ts: 200,
            path: doc_a.to_string_lossy().into_owned(),
        };
        let batch_jsonl = format!("{}\n", serde_json::to_string(&new_task).unwrap());
        let delete_jsonl = "[1, 200]\n";
        let mut out = Vec::new();
        run_worker(
            &idx_dir,
            Some(Cursor::new(batch_jsonl.as_bytes())),
            Some(Cursor::new(delete_jsonl.as_bytes())),
            false,
            1024 * 1024,
            &mut out,
        )
        .expect("update+delete run_worker");

        let summary: serde_json::Value = serde_json::from_slice(&out).expect("stdout JSON summary");
        assert_eq!(summary["processed"], 1);
        assert_eq!(summary["failed"], 0);

        let searcher = ContentSearcher::open(&idx_dir).expect("open searcher");
        assert_eq!(searcher.document_count().unwrap(), 1);
        let hits = searcher.search("全文检索", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].volume_id, hits[0].frn), (1, 100));
        assert!(searcher.search("旧文档", 10).unwrap().is_empty());
    }

    #[test]
    fn write_property_output_file_propagates_errors_and_uses_create_new() {
        use std::io::Cursor;

        struct FailingWriter;
        impl Write for FailingWriter {
            fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "injected write error",
                ))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let existing_out = temp.path().join("existing.jsonl");
        std::fs::write(&existing_out, "prior").unwrap();
        assert!(write_property_output_file(&existing_out, None).is_err());

        let task = PropertyExtractTask {
            source_id: cairn_core::property_store::SourceId::for_drive('C').unwrap(),
            record_key: 1,
            modified_ts: 10,
            path: temp
                .path()
                .join("missing.txt")
                .to_string_lossy()
                .into_owned(),
        };
        let batch = format!("{}\n", serde_json::to_string(&task).unwrap());
        let err = run_property_worker(Some(Cursor::new(batch.as_bytes())), &mut FailingWriter)
            .expect_err("write errors must propagate instead of being discarded");
        assert!(err.to_string().contains("injected write error"));
    }
}
