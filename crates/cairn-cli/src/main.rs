// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]

//! `CairnCli.exe`: Command-line interface for Cairn search engine.

use cairn_client::client::{CairnClient, ClientError};
use cairn_core::format_size_bytes;
use cairn_core::property_store::{RecordPropertyData, SourceId};
use cairn_core::query::QueryDegradedReason;
use cairn_core::sort::{SortSpec, UnknownSortColumn};
use cairn_protocol::message::{Envelope, FileRowData, QueryDefaults, ResponsePayload};
use clap::{Parser, Subcommand};
use std::cell::{Ref, RefCell, RefMut};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const CLI_PAGE_SIZE: usize = 500;
const MAX_CLI_SNIPPETS: usize = 3;

fn parse_sort_spec(value: &str) -> Result<SortSpec, String> {
    SortSpec::parse_clause(value).map_err(|error: UnknownSortColumn| error.to_string())
}

fn parse_volume_letter(value: &str) -> Result<char, String> {
    let mut chars = value.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) if ch.is_ascii_alphabetic() => Ok(ch.to_ascii_uppercase()),
        _ => Err(format!(
            "invalid volume letter `{value}`; expected a single ASCII drive letter such as C"
        )),
    }
}

#[derive(Subcommand, Debug)]
enum CliCommand {
    /// List indexed NTFS volumes and folder sources
    Sources,
    /// Register and scan a folder source
    AddFolder { path: String },
    /// Stop watching and remove a folder source
    RemoveFolder { path: String },
    /// Trigger an asynchronous index rescan
    Rescan {
        /// Drive letter to rescan; rescans all sources when omitted
        #[arg(long, value_parser = parse_volume_letter)]
        volume: Option<char>,
    },
    /// Trigger a rescan for one source by numeric ID
    RescanSource { source_id: u32 },
    /// Reload service configuration from service.toml
    ReloadConfig,
    /// Query or toggle the optional extended-property index
    PropertyIndex {
        /// Enable extended-property indexing
        #[arg(long, conflicts_with = "disable")]
        enable: bool,
        /// Disable extended-property indexing and release its side tables
        #[arg(long)]
        disable: bool,
    },
    /// Inspect the bounded index change journal or toggle recording
    Journal {
        /// Return entries with sequence strictly greater than this cursor
        #[arg(long, default_value_t = 0)]
        after_seq: u64,
        /// Maximum entries to return
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Enable index journal recording
        #[arg(long, conflicts_with = "disable")]
        enable: bool,
        /// Disable index journal recording
        #[arg(long)]
        disable: bool,
        /// Write the journal window after `after_seq` to a JSON file (all pages)
        #[arg(long, value_name = "PATH", conflicts_with_all = ["enable", "disable", "json"])]
        export: Option<PathBuf>,
    },
}

#[derive(Parser, Debug)]
#[command(name = "CairnCli", about = "Cairn CLI")]
struct CliArgs {
    #[command(subcommand)]
    command: Option<CliCommand>,

    /// Search query string
    #[arg(default_value = "")]
    query: String,

    /// Show volume and indexing status
    #[arg(long)]
    status: bool,

    /// Output results in JSON format
    #[arg(long, global = true)]
    json: bool,

    /// Export results to a CSV file
    #[arg(long)]
    export_csv: Option<PathBuf>,

    /// Maximum number of results to retrieve
    #[arg(short, long, default_value_t = 100)]
    max: usize,

    /// Comma-separated sort clauses, e.g. `-size,+name` or `modified-desc`
    #[arg(
        short,
        long,
        default_value = "name",
        allow_hyphen_values = true,
        value_parser = parse_sort_spec
    )]
    sort: SortSpec,

    /// Exclude hidden entries unless overridden per term
    #[arg(long)]
    exclude_hidden: bool,

    /// Exclude system entries unless overridden per term
    #[arg(long)]
    exclude_system: bool,
}

#[derive(Clone, Debug)]
enum TransportError {
    Connect(String),
    Request(String),
}

trait CliTransport {
    fn get_status(&self) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn query(
        &self,
        query_id: u64,
        raw_query: &str,
        defaults: QueryDefaults,
        sort_spec: &SortSpec,
        first_page_limit: usize,
    ) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn get_rows(
        &self,
        session_id: u64,
        offset: usize,
        limit: usize,
    ) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn get_sources(&self) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn add_folder_source(&self, path: &str) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn remove_folder_source(&self, path: &str)
        -> Result<Envelope<ResponsePayload>, TransportError>;

    fn rescan(&self, volume_id: Option<u16>) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn rescan_source(
        &self,
        source_id: SourceId,
    ) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn reload_service_config(&self) -> Result<(bool, String), TransportError>;

    fn get_property_index_status(&self) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn set_property_index_enabled(
        &self,
        enabled: bool,
    ) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn get_index_journal(
        &self,
        after_seq: u64,
        limit: usize,
    ) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn set_index_journal_enabled(
        &self,
        enabled: bool,
    ) -> Result<Envelope<ResponsePayload>, TransportError>;
}

/// Lazily creates a single-thread Tokio runtime and pipe client only when a command executes.
#[derive(Default)]
struct PipeCliTransport {
    runtime: RefCell<Option<tokio::runtime::Runtime>>,
    client: RefCell<Option<CairnClient>>,
}

impl PipeCliTransport {
    fn ensure_ready(
        &self,
    ) -> Result<(Ref<'_, tokio::runtime::Runtime>, RefMut<'_, CairnClient>), TransportError> {
        if self.runtime.borrow().is_none() {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
                .map_err(|e| TransportError::Connect(e.to_string()))?;
            *self.runtime.borrow_mut() = Some(rt);
        }

        let rt = Ref::map(self.runtime.borrow(), |opt| opt.as_ref().unwrap());
        if self.client.borrow().is_none() {
            let client = rt
                .block_on(CairnClient::connect())
                .map_err(|e| TransportError::Connect(e.to_string()))?;
            *self.client.borrow_mut() = Some(client);
        }
        let client = RefMut::map(self.client.borrow_mut(), |opt| opt.as_mut().unwrap());
        Ok((rt, client))
    }

    fn call_pipe<T>(
        &self,
        op: impl FnOnce(&tokio::runtime::Runtime, &mut CairnClient) -> Result<T, ClientError>,
    ) -> Result<T, TransportError> {
        let (rt, mut client) = self.ensure_ready()?;
        map_request_result(op(&rt, &mut client))
    }
}

fn map_request_result<T>(res: Result<T, ClientError>) -> Result<T, TransportError> {
    res.map_err(|e| TransportError::Request(e.to_string()))
}

impl CliTransport for PipeCliTransport {
    fn get_status(&self) -> Result<Envelope<ResponsePayload>, TransportError> {
        self.call_pipe(|rt, c| rt.block_on(c.get_status()))
    }

    fn query(
        &self,
        query_id: u64,
        raw_query: &str,
        defaults: QueryDefaults,
        sort_spec: &SortSpec,
        first_page_limit: usize,
    ) -> Result<Envelope<ResponsePayload>, TransportError> {
        self.call_pipe(|rt, c| {
            rt.block_on(c.query(
                query_id,
                raw_query,
                defaults,
                sort_spec,
                first_page_limit,
                None,
            ))
        })
    }

    fn get_rows(
        &self,
        session_id: u64,
        offset: usize,
        limit: usize,
    ) -> Result<Envelope<ResponsePayload>, TransportError> {
        self.call_pipe(|rt, c| rt.block_on(c.get_rows(session_id, offset, limit)))
    }

    fn get_sources(&self) -> Result<Envelope<ResponsePayload>, TransportError> {
        self.call_pipe(|rt, c| rt.block_on(c.get_sources()))
    }

    fn add_folder_source(&self, path: &str) -> Result<Envelope<ResponsePayload>, TransportError> {
        self.call_pipe(|rt, c| rt.block_on(c.add_folder_source(path)))
    }

    fn remove_folder_source(
        &self,
        path: &str,
    ) -> Result<Envelope<ResponsePayload>, TransportError> {
        self.call_pipe(|rt, c| rt.block_on(c.remove_folder_source(path)))
    }

    fn rescan(&self, volume_id: Option<u16>) -> Result<Envelope<ResponsePayload>, TransportError> {
        self.call_pipe(|rt, c| rt.block_on(c.rescan(volume_id)))
    }

    fn rescan_source(
        &self,
        source_id: SourceId,
    ) -> Result<Envelope<ResponsePayload>, TransportError> {
        self.call_pipe(|rt, c| rt.block_on(c.rescan_source(source_id)))
    }

    fn reload_service_config(&self) -> Result<(bool, String), TransportError> {
        self.call_pipe(|rt, c| rt.block_on(c.reload_service_config()))
    }

    fn get_property_index_status(&self) -> Result<Envelope<ResponsePayload>, TransportError> {
        self.call_pipe(|rt, c| rt.block_on(c.get_property_index_status()))
    }

    fn set_property_index_enabled(
        &self,
        enabled: bool,
    ) -> Result<Envelope<ResponsePayload>, TransportError> {
        self.call_pipe(|rt, c| rt.block_on(c.set_property_index_enabled(enabled)))
    }

    fn get_index_journal(
        &self,
        after_seq: u64,
        limit: usize,
    ) -> Result<Envelope<ResponsePayload>, TransportError> {
        self.call_pipe(|rt, c| rt.block_on(c.get_index_journal(after_seq, limit)))
    }

    fn set_index_journal_enabled(
        &self,
        enabled: bool,
    ) -> Result<Envelope<ResponsePayload>, TransportError> {
        self.call_pipe(|rt, c| rt.block_on(c.set_index_journal_enabled(enabled)))
    }
}

enum CliStreamError {
    Transport {
        context: &'static str,
        error: TransportError,
    },
    Service {
        code: u32,
        message: String,
    },
    UnexpectedResponse,
    Io(Box<dyn std::error::Error>),
}

fn write_transport_error(err: &mut impl Write, context: &str, error: TransportError) -> ExitCode {
    match error {
        TransportError::Connect(e) => {
            let _ = writeln!(err, "Error: Cannot connect to Cairn Service: {e}");
            let _ = writeln!(
                err,
                "Please ensure Cairn Service is running (or start via `CairnService --console`)."
            );
            ExitCode::from(2)
        }
        TransportError::Request(e) => {
            let _ = writeln!(err, "{context}: {e}");
            ExitCode::from(1)
        }
    }
}

fn handle_stream_error(err: &mut impl Write, error: CliStreamError) -> ExitCode {
    match error {
        CliStreamError::Transport { context, error } => write_transport_error(err, context, error),
        CliStreamError::Service { code, message } => {
            let _ = writeln!(err, "Search error {code}: {}", sanitize_terminal(&message));
            ExitCode::from(1)
        }
        CliStreamError::UnexpectedResponse => {
            let _ = writeln!(err, "Unexpected service response.");
            ExitCode::from(1)
        }
        CliStreamError::Io(error) => {
            let _ = writeln!(err, "{error}");
            ExitCode::from(1)
        }
    }
}

fn stream_query_rows<F>(
    transport: &impl CliTransport,
    session_id: u64,
    total_matches: usize,
    max_rows: usize,
    first_page: Vec<FileRowData>,
    mut on_row: F,
) -> Result<usize, CliStreamError>
where
    F: FnMut(&FileRowData) -> Result<(), Box<dyn std::error::Error>>,
{
    let target = max_rows.min(total_matches);
    if target == 0 {
        return Ok(0);
    }

    let mut emitted = 0usize;
    for row in first_page.iter().take(target) {
        on_row(row).map_err(CliStreamError::Io)?;
        emitted += 1;
    }

    // The service pages over the match list by offset, which can be wider than the rows it can
    // materialize; advancing by the requested limit keeps a sparse page from being re-read.
    let mut offset = first_page.len();
    while emitted < target {
        let limit = (target - emitted).min(CLI_PAGE_SIZE);
        let env = transport
            .get_rows(session_id, offset, limit)
            .map_err(|error| CliStreamError::Transport {
                context: "Query failed",
                error,
            })?;
        offset = offset.saturating_add(limit);
        match env.payload {
            ResponsePayload::RowsResult { rows, .. } => {
                if rows.is_empty() {
                    break;
                }
                for row in rows.iter().take(target - emitted) {
                    on_row(row).map_err(CliStreamError::Io)?;
                    emitted += 1;
                }
            }
            ResponsePayload::Error { code, message } => {
                return Err(CliStreamError::Service { code, message });
            }
            _ => return Err(CliStreamError::UnexpectedResponse),
        }
    }

    Ok(emitted)
}

fn expect_control_payload(
    res: Result<Envelope<ResponsePayload>, TransportError>,
    context: &str,
    err: &mut impl Write,
) -> Result<Envelope<ResponsePayload>, ExitCode> {
    let env = match res {
        Ok(env) => env,
        Err(error) => return Err(write_transport_error(err, context, error)),
    };
    if let ResponsePayload::Error { code, message } = &env.payload {
        let _ = writeln!(
            err,
            "{context} error {code}: {}",
            sanitize_terminal(message)
        );
        return Err(ExitCode::from(1));
    }
    Ok(env)
}

fn write_json_output(
    value: &serde_json::Value,
    out: &mut impl Write,
    err: &mut impl Write,
) -> ExitCode {
    let mut buf = BufWriter::new(out);
    let result = serde_json::to_writer_pretty(&mut buf, value)
        .map_err(std::io::Error::other)
        .and_then(|()| buf.write_all(b"\n"))
        .and_then(|()| buf.flush());
    if let Err(error) = result {
        let _ = writeln!(err, "Failed to write output: {error}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn unexpected_response(err: &mut impl Write) -> ExitCode {
    let _ = writeln!(err, "Unexpected service response.");
    ExitCode::from(1)
}

fn write_io_result(result: std::io::Result<()>, err: &mut impl Write) -> ExitCode {
    if let Err(error) = result {
        let _ = writeln!(err, "Failed to write output: {error}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn execute_subcommand(
    command: CliCommand,
    json: bool,
    transport: &impl CliTransport,
    out: &mut impl Write,
    err: &mut impl Write,
) -> ExitCode {
    match command {
        CliCommand::Sources | CliCommand::AddFolder { .. } | CliCommand::RemoveFolder { .. } => {
            let (context, res) = match &command {
                CliCommand::Sources => ("Error listing sources", transport.get_sources()),
                CliCommand::AddFolder { path } => (
                    "Error adding folder source",
                    transport.add_folder_source(path),
                ),
                CliCommand::RemoveFolder { path } => (
                    "Error removing folder source",
                    transport.remove_folder_source(path),
                ),
                _ => unreachable!(),
            };
            let env = match expect_control_payload(res, context, err) {
                Ok(env) => env,
                Err(code) => return code,
            };
            render_sources_response(env.payload, json, out, err)
        }
        CliCommand::Rescan { volume } => {
            let volume_id = volume.map(|letter| (letter as u8 - b'A') as u16);
            let env = match expect_control_payload(
                transport.rescan(volume_id),
                "Error requesting rescan",
                err,
            ) {
                Ok(env) => env,
                Err(code) => return code,
            };
            let ResponsePayload::RescanAck { volume_id, success } = env.payload else {
                return unexpected_response(err);
            };
            if json {
                return write_json_output(
                    &serde_json::json!({ "volume_id": volume_id, "success": success }),
                    out,
                    err,
                );
            }
            if !success {
                let _ = writeln!(err, "Rescan request was refused or volume not found.");
                return ExitCode::from(1);
            }
            write_io_result(
                writeln!(out, "Rescan scheduled (volume_id={volume_id:?})."),
                err,
            )
        }
        CliCommand::RescanSource { source_id } => {
            let env = match expect_control_payload(
                transport.rescan_source(SourceId(source_id)),
                "Error requesting source rescan",
                err,
            ) {
                Ok(env) => env,
                Err(code) => return code,
            };
            let ResponsePayload::RescanSourceAck { source_id, success } = env.payload else {
                return unexpected_response(err);
            };
            if json {
                return write_json_output(
                    &serde_json::json!({ "source_id": source_id.0, "success": success }),
                    out,
                    err,
                );
            }
            if !success {
                let _ = writeln!(
                    err,
                    "Source {} was not found or cannot rescan.",
                    source_id.0
                );
                return ExitCode::from(1);
            }
            write_io_result(
                writeln!(out, "Rescan scheduled for source {}.", source_id.0),
                err,
            )
        }
        CliCommand::ReloadConfig => {
            let (success, message) = match transport.reload_service_config() {
                Ok(pair) => pair,
                Err(error) => return write_transport_error(err, "Error reloading config", error),
            };
            if json {
                let code = write_json_output(
                    &serde_json::json!({ "success": success, "message": message }),
                    out,
                    err,
                );
                return if success { code } else { ExitCode::from(1) };
            }
            if !success {
                let _ = writeln!(err, "Reload config failed: {}", sanitize_terminal(&message));
                return ExitCode::from(1);
            }
            write_io_result(writeln!(out, "{message}"), err)
        }
        CliCommand::PropertyIndex { enable, disable } => {
            let res = if enable {
                transport.set_property_index_enabled(true)
            } else if disable {
                transport.set_property_index_enabled(false)
            } else {
                transport.get_property_index_status()
            };
            let env = match expect_control_payload(res, "Error reading property index", err) {
                Ok(env) => env,
                Err(code) => return code,
            };
            let ResponsePayload::PropertyIndexStatus { status } = env.payload else {
                return unexpected_response(err);
            };
            if json {
                return write_json_output(&serde_json::json!(status), out, err);
            }
            let mut buf = BufWriter::new(out);
            let res = (|| -> std::io::Result<()> {
                writeln!(
                    buf,
                    "enabled={} state={} indexed_records={} side_table_bytes={} last_updated_ts={}",
                    status.enabled,
                    status.state.state_key(),
                    status.indexed_records,
                    status.side_table_bytes,
                    status.last_updated_ts
                )?;
                buf.flush()
            })();
            write_io_result(res, err)
        }
        CliCommand::Journal {
            after_seq,
            limit,
            enable,
            disable,
            export,
        } => {
            if enable || disable {
                let env = match expect_control_payload(
                    transport.set_index_journal_enabled(enable),
                    "Error updating index journal",
                    err,
                ) {
                    Ok(env) => env,
                    Err(code) => return code,
                };
                let ResponsePayload::IndexJournalStatus { status } = env.payload else {
                    return unexpected_response(err);
                };
                if json {
                    return write_json_output(&serde_json::json!(status), out, err);
                }
                return write_io_result(
                    writeln!(
                        out,
                        "enabled={} entry_count={} resident_bytes={} oldest_seq={} newest_seq={}",
                        status.enabled,
                        status.entry_count,
                        status.resident_bytes,
                        status.oldest_seq,
                        status.newest_seq
                    ),
                    err,
                );
            }
            if let Some(path) = export {
                let page_size = 10_000usize;
                let mut cursor = after_seq;
                let mut all_entries = Vec::new();
                let status = loop {
                    let env = match expect_control_payload(
                        transport.get_index_journal(cursor, page_size),
                        "Error reading index journal",
                        err,
                    ) {
                        Ok(env) => env,
                        Err(code) => return code,
                    };
                    let ResponsePayload::IndexJournalResult {
                        status: page_status,
                        entries,
                    } = env.payload
                    else {
                        return unexpected_response(err);
                    };
                    let fetched = entries.len();
                    let last_seq = entries.last().map(|entry| entry.seq);
                    all_entries.extend(entries);
                    if fetched < page_size || last_seq.is_none_or(|last_seq| last_seq <= cursor) {
                        break page_status;
                    }
                    cursor = last_seq.unwrap_or(cursor);
                };
                let document = serde_json::json!({ "status": status, "entries": all_entries });
                let encoded = match serde_json::to_vec_pretty(&document) {
                    Ok(encoded) => encoded,
                    Err(error) => {
                        let _ = writeln!(
                            err,
                            "Failed to encode journal export: {}",
                            sanitize_terminal(&error.to_string())
                        );
                        return ExitCode::from(1);
                    }
                };
                if let Err(error) = std::fs::write(&path, encoded) {
                    let _ = writeln!(
                        err,
                        "Failed to write journal export {}: {}",
                        sanitize_terminal(&path.display().to_string()),
                        sanitize_terminal(&error.to_string())
                    );
                    return ExitCode::from(1);
                }
                return write_io_result(
                    writeln!(
                        out,
                        "Exported {} journal entries to {}.",
                        all_entries.len(),
                        sanitize_terminal(&path.display().to_string())
                    ),
                    err,
                );
            }
            let env = match expect_control_payload(
                transport.get_index_journal(after_seq, limit),
                "Error reading index journal",
                err,
            ) {
                Ok(env) => env,
                Err(code) => return code,
            };
            let ResponsePayload::IndexJournalResult { status, entries } = env.payload else {
                return unexpected_response(err);
            };
            if json {
                return write_json_output(
                    &serde_json::json!({ "status": status, "entries": entries }),
                    out,
                    err,
                );
            }
            let mut buf = BufWriter::new(out);
            let res = (|| -> std::io::Result<()> {
                writeln!(
                    buf,
                    "Journal enabled={} entries={} (shown={}) seq={}..={}",
                    status.enabled,
                    status.entry_count,
                    entries.len(),
                    status.oldest_seq,
                    status.newest_seq
                )?;
                for entry in entries {
                    let path = sanitize_terminal(&entry.path);
                    let old_path = entry.old_path.as_deref().map(sanitize_terminal);
                    if let Some(old_path) = old_path {
                        writeln!(
                            buf,
                            "{:>8} {:<18} src={} frn={} {} <- {}",
                            entry.seq,
                            entry.kind.key(),
                            entry.source_id.0,
                            entry.frn,
                            path,
                            old_path
                        )?;
                    } else {
                        writeln!(
                            buf,
                            "{:>8} {:<18} src={} frn={} {}",
                            entry.seq,
                            entry.kind.key(),
                            entry.source_id.0,
                            entry.frn,
                            path
                        )?;
                    }
                }
                buf.flush()
            })();
            write_io_result(res, err)
        }
    }
}

fn render_sources_response(
    payload: ResponsePayload,
    json: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> ExitCode {
    let ResponsePayload::SourcesResult { sources } = payload else {
        return unexpected_response(err);
    };
    if json {
        return write_json_output(&serde_json::json!(sources), out, err);
    }
    let mut buf = BufWriter::new(out);
    let res = (|| -> std::io::Result<()> {
        writeln!(
            buf,
            "{:<8} {:<12} {:<14} {:<10} {:<6} Root",
            "ID", "Kind", "Status", "Items", "Watch"
        )?;
        writeln!(buf, "{}", "-".repeat(72))?;
        for s in sources {
            let root = sanitize_terminal(&s.root_path);
            writeln!(
                buf,
                "{:<8} {:<12} {:<14} {:<10} {:<6} {}",
                s.source_id.0,
                s.kind.key(),
                s.status.state_key(),
                s.indexed_count,
                if s.live_watch_enabled { "on" } else { "off" },
                root
            )?;
        }
        buf.flush()
    })();
    write_io_result(res, err)
}

fn report_degraded_notice(err: &mut impl Write, degraded: Option<&QueryDegradedReason>) {
    if let Some(QueryDegradedReason::PropertyIndexDisabled { requested }) = degraded {
        let fields = requested
            .iter()
            .map(|kind| kind.key())
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            err,
            "Notice: Extended property index is disabled; requested properties ({fields}) were not evaluated and sorting fell back."
        );
    }
}

fn sanitize_terminal(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

fn format_property_summary(props: &RecordPropertyData) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(ms) = props.length_ms {
        let total_secs = ms / 1000;
        let rem_ms = ms % 1000;
        let mins = total_secs / 60;
        let secs = total_secs % 60;
        if mins > 0 {
            parts.push(format!("length={mins}m{secs:02}s"));
        } else if rem_ms > 0 {
            parts.push(format!("length={secs}.{rem_ms:03}s"));
        } else {
            parts.push(format!("length={secs}s"));
        }
    }
    if let Some(author) = props.author.as_deref().filter(|s| !s.is_empty()) {
        parts.push(format!("author={}", sanitize_terminal(author)));
    }
    if let Some(tags) = props.tags.as_deref().filter(|s| !s.is_empty()) {
        parts.push(format!("tags={}", sanitize_terminal(tags)));
    }
    if let Some((w, h)) = props.dimensions {
        parts.push(format!("dimensions={w}x{h}"));
    }
    if let Some(mfps) = props.framerate_mfps {
        let whole = mfps / 1000;
        let frac = (mfps % 1000) / 10;
        parts.push(if frac == 0 {
            format!("framerate={whole}fps")
        } else {
            format!("framerate={whole}.{frac:02}fps")
        });
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" | "))
    }
}

fn execute_cli(
    args: CliArgs,
    transport: &impl CliTransport,
    out: &mut impl Write,
    err: &mut impl Write,
) -> ExitCode {
    if let Some(command) = args.command {
        return execute_subcommand(command, args.json, transport, out, err);
    }

    if args.status {
        let env = match transport.get_status() {
            Ok(env) => env,
            Err(error) => return write_transport_error(err, "Error querying status", error),
        };
        match &env.payload {
            ResponsePayload::StatusAck => {}
            ResponsePayload::Error { code, message } => {
                let _ = writeln!(err, "Status error {code}: {}", sanitize_terminal(message));
                return ExitCode::from(1);
            }
            _ => return unexpected_response(err),
        }
        let mut buf = BufWriter::new(out);
        let write_result = (|| -> std::io::Result<()> {
            writeln!(
                buf,
                "{:<6} {:<16} {:<12} {:<10}",
                "Drive", "Status", "Items", "USN"
            )?;
            writeln!(buf, "{}", "-".repeat(50))?;
            for v in env.volumes {
                writeln!(
                    buf,
                    "{:<6} {:<16} {:<12} {:<10}",
                    format!("{}:", v.drive_letter),
                    v.status.state_key(),
                    v.indexed_count,
                    if v.usn_enabled { "Enabled" } else { "Disabled" }
                )?;
            }
            buf.flush()
        })();
        return write_io_result(write_result, err);
    }

    let defaults = QueryDefaults {
        exclude_hidden: args.exclude_hidden,
        exclude_system: args.exclude_system,
        ..Default::default()
    };
    let env = match transport.query(
        1,
        &args.query,
        defaults,
        &args.sort,
        args.max.min(CLI_PAGE_SIZE),
    ) {
        Ok(env) => env,
        Err(error) => return write_transport_error(err, "Query failed", error),
    };

    if env.volumes.iter().any(|v| !v.status.is_synced()) {
        let _ = writeln!(
            err,
            "Warning: One or more volumes are not fully synced. Results may be partial."
        );
    }

    let ResponsePayload::QueryResult {
        session_id,
        total_matches,
        total_bytes,
        first_page,
        degraded,
        ..
    } = env.payload
    else {
        return match env.payload {
            ResponsePayload::Error { code, message } => {
                handle_stream_error(err, CliStreamError::Service { code, message })
            }
            _ => handle_stream_error(err, CliStreamError::UnexpectedResponse),
        };
    };
    report_degraded_notice(err, degraded.as_ref());

    if args.json {
        let mut buf = BufWriter::new(out);
        if let Err(error) = buf
            .write_all(b"[")
            .map_err(|e| CliStreamError::Io(e.into()))
        {
            return handle_stream_error(err, error);
        }
        let mut wrote_any = false;
        let result = stream_query_rows(
            transport,
            session_id,
            total_matches,
            args.max,
            first_page,
            |row| {
                if wrote_any {
                    buf.write_all(b",\n")?;
                } else {
                    buf.write_all(b"\n")?;
                    wrote_any = true;
                }
                serde_json::to_writer_pretty(&mut buf, row)?;
                Ok(())
            },
        );
        return match result {
            Ok(_) => {
                let tail = if wrote_any {
                    b"\n]\n".as_slice()
                } else {
                    b"]\n".as_slice()
                };
                if buf.write_all(tail).and_then(|()| buf.flush()).is_err() {
                    return ExitCode::from(1);
                }
                ExitCode::SUCCESS
            }
            Err(error) => handle_stream_error(err, error),
        };
    }

    if let Some(csv_path) = args.export_csv {
        let mut writer = match create_csv_writer(&csv_path) {
            Ok(writer) => writer,
            Err(error) => {
                let _ = writeln!(
                    err,
                    "Failed to export CSV: {}",
                    sanitize_terminal(&error.to_string())
                );
                return ExitCode::from(1);
            }
        };
        let count = match stream_query_rows(
            transport,
            session_id,
            total_matches,
            args.max,
            first_page,
            |row| write_csv_row(&mut writer, row),
        ) {
            Ok(count) => count,
            Err(CliStreamError::Io(error)) => {
                let _ = writeln!(
                    err,
                    "Failed to export CSV: {}",
                    sanitize_terminal(&error.to_string())
                );
                return ExitCode::from(1);
            }
            Err(error) => return handle_stream_error(err, error),
        };
        if let Err(error) = writer.flush() {
            let _ = writeln!(err, "Failed to export CSV: {error}");
            return ExitCode::from(1);
        }
        let _ = writeln!(out, "Exported {count} results to {csv_path:?}");
        return ExitCode::SUCCESS;
    }

    let displayed = args.max.min(total_matches);
    let mut buf = BufWriter::new(out);
    if let Err(error) = (|| -> std::io::Result<()> {
        writeln!(
            buf,
            "Showing {} of {} matching objects ({}):\n",
            displayed,
            total_matches,
            format_size_bytes(total_bytes)
        )?;
        writeln!(buf, "{:<40} {:<60} {:>12}", "Name", "Path", "Size")?;
        writeln!(buf, "{}", "-".repeat(116))
    })() {
        let _ = writeln!(err, "Failed to write output: {error}");
        return ExitCode::from(1);
    }

    match stream_query_rows(
        transport,
        session_id,
        total_matches,
        args.max,
        first_page,
        |row| {
            let size_str = if row.is_dir {
                "<DIR>".to_string()
            } else {
                format_size_bytes(row.size)
            };
            let name = sanitize_terminal(&row.name);
            let path = sanitize_terminal(&row.path);
            writeln!(
                buf,
                "{:<40} {:<60} {:>12}",
                truncate_str(&name, 38),
                truncate_str(&path, 58),
                size_str
            )?;
            if let Some(summary) = row.properties.as_ref().and_then(format_property_summary) {
                writeln!(buf, "  [{summary}]")?;
            }
            for snippet in row.snippets.iter().take(MAX_CLI_SNIPPETS) {
                writeln!(buf, "  snippet: {}", sanitize_terminal(snippet))?;
            }
            Ok(())
        },
    ) {
        Ok(_) => {
            let _ = buf.flush();
            ExitCode::SUCCESS
        }
        Err(error) => handle_stream_error(err, error),
    }
}

fn main() -> ExitCode {
    let args = CliArgs::parse();
    let transport = PipeCliTransport::default();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    execute_cli(args, &transport, &mut out, &mut err)
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars.saturating_sub(3)).collect();
        out.push_str("...");
        out
    }
}

fn create_csv_writer(
    path: &Path,
) -> Result<csv::Writer<BufWriter<File>>, Box<dyn std::error::Error>> {
    let mut writer = csv::Writer::from_writer(BufWriter::new(File::create(path)?));
    writer.write_record([
        "Name",
        "Path",
        "Size",
        "Modified",
        "Created",
        "IsDir",
        "Snippets",
        "LengthMs",
        "Author",
        "Tags",
        "Dimensions",
        "FramerateMfps",
    ])?;
    Ok(writer)
}

fn write_csv_row(
    writer: &mut csv::Writer<impl Write>,
    r: &FileRowData,
) -> Result<(), Box<dyn std::error::Error>> {
    let snippets = sanitize_terminal(&r.snippets.join(" | "));
    let name = sanitize_terminal(&r.name);
    let path = sanitize_terminal(&r.path);
    let props = r.properties.as_ref();
    let length_ms = props
        .and_then(|p| p.length_ms)
        .map(|v| v.to_string())
        .unwrap_or_default();
    let author = sanitize_terminal(props.and_then(|p| p.author.as_deref()).unwrap_or(""));
    let tags = sanitize_terminal(props.and_then(|p| p.tags.as_deref()).unwrap_or(""));
    let dimensions = props
        .and_then(|p| p.dimensions)
        .map(|(w, h)| format!("{w}x{h}"))
        .unwrap_or_default();
    let framerate_mfps = props
        .and_then(|p| p.framerate_mfps)
        .map(|v| v.to_string())
        .unwrap_or_default();

    writer.write_record([
        name.as_str(),
        path.as_str(),
        &r.size.to_string(),
        &r.modified_ts.to_string(),
        &r.created_ts.to_string(),
        &r.is_dir.to_string(),
        snippets.as_str(),
        length_ms.as_str(),
        author.as_str(),
        tags.as_str(),
        dimensions.as_str(),
        framerate_mfps.as_str(),
    ])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::sort::{SortColumn, SortDirection, SortKey};

    #[test]
    fn test_cli_args_custom_options() {
        let args = CliArgs::try_parse_from([
            "CairnCli",
            "my_query",
            "--status",
            "--json",
            "--max",
            "50",
            "--sort",
            "-size,+name",
            "--exclude-hidden",
            "--exclude-system",
            "--export-csv",
            "results.csv",
        ])
        .expect("parse custom args");

        assert_eq!(args.query, "my_query");
        assert!(args.status);
        assert!(args.json);
        assert_eq!(args.max, 50);
        assert_eq!(
            args.sort,
            SortSpec {
                keys: vec![
                    SortKey::new(SortColumn::Size, SortDirection::Descending),
                    SortKey::new(SortColumn::Name, SortDirection::Ascending),
                ]
            }
        );
        assert!(args.exclude_hidden);
        assert!(args.exclude_system);
        assert_eq!(args.export_csv, Some(PathBuf::from("results.csv")));
    }

    #[test]
    fn cli_parses_platform_subcommands_and_rejects_invalid_volume_letter() {
        let args = CliArgs::try_parse_from(["CairnCli", "sources", "--json"]).expect("sources");
        assert!(args.json);
        assert!(matches!(args.command, Some(CliCommand::Sources)));

        let args =
            CliArgs::try_parse_from(["CairnCli", "add-folder", r"D:\Work"]).expect("add-folder");
        assert!(matches!(
            args.command,
            Some(CliCommand::AddFolder { ref path }) if path == r"D:\Work"
        ));

        let args = CliArgs::try_parse_from(["CairnCli", "remove-folder", r"D:\Work"])
            .expect("remove-folder");
        assert!(matches!(
            args.command,
            Some(CliCommand::RemoveFolder { ref path }) if path == r"D:\Work"
        ));

        let args =
            CliArgs::try_parse_from(["CairnCli", "rescan", "--volume", "c"]).expect("rescan");
        assert!(matches!(
            args.command,
            Some(CliCommand::Rescan { volume: Some('C') })
        ));
        assert!(
            CliArgs::try_parse_from(["CairnCli", "rescan", "--volume", "1"]).is_err(),
            "non-ASCII-letter volume must be refused at parse time"
        );

        let args =
            CliArgs::try_parse_from(["CairnCli", "rescan-source", "1000"]).expect("rescan-source");
        assert!(matches!(
            args.command,
            Some(CliCommand::RescanSource { source_id: 1000 })
        ));

        let args = CliArgs::try_parse_from(["CairnCli", "reload-config"]).expect("reload-config");
        assert!(matches!(args.command, Some(CliCommand::ReloadConfig)));

        let args = CliArgs::try_parse_from(["CairnCli", "property-index", "--enable"])
            .expect("property-index");
        assert!(matches!(
            args.command,
            Some(CliCommand::PropertyIndex {
                enable: true,
                disable: false
            })
        ));
        assert!(
            CliArgs::try_parse_from(["CairnCli", "property-index", "--enable", "--disable"])
                .is_err(),
            "--enable and --disable must conflict"
        );

        let args = CliArgs::try_parse_from([
            "CairnCli",
            "journal",
            "--after-seq",
            "42",
            "--limit",
            "25",
            "--json",
        ])
        .expect("journal");
        assert!(args.json);
        assert!(matches!(
            args.command,
            Some(CliCommand::Journal {
                after_seq: 42,
                limit: 25,
                enable: false,
                disable: false,
                export: None,
            })
        ));
    }

    #[test]
    fn an_unknown_sort_column_is_refused_at_the_command_line() {
        assert!(
            CliArgs::try_parse_from(["CairnCli", "--sort", "bogus"]).is_err(),
            "an unknown column must be refused, not silently answered as name"
        );
    }

    #[test]
    fn truncate_str_respects_utf8_boundaries_and_max_chars() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello", 5), "hello");
        assert_eq!(truncate_str("hello world", 8), "hello...");
        assert_eq!(truncate_str("中文文档检索结果", 6), "中文文...");
        assert_eq!(truncate_str("abc", 2), "...");
    }

    #[test]
    fn test_export_to_csv() {
        let temp = tempfile::tempdir().expect("tempdir");
        let csv_path = temp.path().join("out.csv");

        let rows = vec![
            cairn_protocol::message::FileRowData {
                flags: 0,
                name_highlights: vec![],
                path_highlights: vec![],
                snippets: vec!["line one".to_string(), "line two".to_string()],
                properties: Some(cairn_protocol::message::RecordPropertyData {
                    length_ms: Some(12_500),
                    author: Some("Alice".to_string()),
                    tags: Some("finance;q1".to_string()),
                    dimensions: Some((1920, 1080)),
                    framerate_mfps: Some(29_970),
                }),
                frn: 100,
                name: "file1.txt".to_string(),
                path: r"C:\test\file1.txt".to_string(),
                size: 2048,
                modified_ts: 1600000000,
                created_ts: 1500000000,
                is_dir: false,
            },
            cairn_protocol::message::FileRowData {
                flags: 0,
                name_highlights: vec![],
                path_highlights: vec![],
                snippets: vec![],
                properties: None,
                frn: 101,
                name: "subfolder".to_string(),
                path: r"C:\test\subfolder".to_string(),
                size: 0,
                modified_ts: 1600000100,
                created_ts: 1500000100,
                is_dir: true,
            },
        ];

        let mut writer = create_csv_writer(&csv_path).expect("create csv writer");
        for row in &rows {
            write_csv_row(&mut writer, row).expect("write csv row");
        }
        writer.flush().expect("flush csv writer");

        let content = std::fs::read_to_string(&csv_path).expect("read csv");
        assert!(content.contains("Name,Path,Size,Modified,Created,IsDir,Snippets,LengthMs,Author,Tags,Dimensions,FramerateMfps"));
        assert!(content.contains(r"file1.txt,C:\test\file1.txt,2048,1600000000,1500000000,false,line one | line two,12500,Alice,finance;q1,1920x1080,29970"));
        assert!(content.contains(r"subfolder,C:\test\subfolder,0,1600000100,1500000100,true,,,,,,"));
    }

    fn make_row(
        frn: u64,
        name: &str,
        path: &str,
        size: u64,
    ) -> cairn_protocol::message::FileRowData {
        cairn_protocol::message::FileRowData {
            flags: 0,
            name_highlights: vec![],
            path_highlights: vec![],
            snippets: vec![],
            properties: None,
            frn,
            name: name.to_string(),
            path: path.to_string(),
            size,
            modified_ts: 1700000000,
            created_ts: 1600000000,
            is_dir: false,
        }
    }

    #[derive(Default)]
    struct StubTransport {
        connect_error: Option<String>,
        query_error: Option<String>,
        query_response: Option<cairn_protocol::message::Envelope<ResponsePayload>>,
        control_response: Option<cairn_protocol::message::Envelope<ResponsePayload>>,
        reload_response: Option<Result<(bool, String), TransportError>>,
        pages: Vec<(usize, Vec<cairn_protocol::message::FileRowData>)>,
        recorded_offsets: std::cell::RefCell<Vec<(usize, usize)>>,
        recorded_queries: std::cell::RefCell<Vec<(String, QueryDefaults, SortSpec)>>,
        recorded_controls: std::cell::RefCell<Vec<String>>,
    }

    impl StubTransport {
        fn check_err(&self) -> Result<(), TransportError> {
            if let Some(msg) = &self.connect_error {
                return Err(TransportError::Connect(msg.clone()));
            }
            if let Some(msg) = &self.query_error {
                return Err(TransportError::Request(msg.clone()));
            }
            Ok(())
        }
    }

    impl CliTransport for StubTransport {
        fn get_status(
            &self,
        ) -> Result<cairn_protocol::message::Envelope<ResponsePayload>, TransportError> {
            self.check_err()?;
            unreachable!("status unused in stub")
        }

        fn query(
            &self,
            _query_id: u64,
            raw_query: &str,
            defaults: QueryDefaults,
            sort_spec: &SortSpec,
            _first_page_limit: usize,
        ) -> Result<cairn_protocol::message::Envelope<ResponsePayload>, TransportError> {
            self.check_err()?;
            self.recorded_queries.borrow_mut().push((
                raw_query.to_string(),
                defaults,
                sort_spec.clone(),
            ));
            Ok(self.query_response.clone().expect("query_response"))
        }

        fn get_rows(
            &self,
            session_id: u64,
            offset: usize,
            limit: usize,
        ) -> Result<cairn_protocol::message::Envelope<ResponsePayload>, TransportError> {
            self.recorded_offsets.borrow_mut().push((offset, limit));
            let rows = self
                .pages
                .iter()
                .find(|(page_off, _)| *page_off == offset)
                .map(|(_, rows)| rows.clone())
                .unwrap_or_default();
            Ok(cairn_protocol::message::Envelope {
                volumes: vec![],
                payload: ResponsePayload::RowsResult {
                    session_id,
                    offset,
                    rows,
                },
            })
        }

        fn get_sources(
            &self,
        ) -> Result<cairn_protocol::message::Envelope<ResponsePayload>, TransportError> {
            self.check_err()?;
            self.recorded_controls
                .borrow_mut()
                .push("get_sources".into());
            Ok(self.control_response.clone().expect("control_response"))
        }

        fn add_folder_source(
            &self,
            path: &str,
        ) -> Result<cairn_protocol::message::Envelope<ResponsePayload>, TransportError> {
            self.check_err()?;
            self.recorded_controls
                .borrow_mut()
                .push(format!("add_folder:{path}"));
            Ok(self.control_response.clone().expect("control_response"))
        }

        fn remove_folder_source(
            &self,
            path: &str,
        ) -> Result<cairn_protocol::message::Envelope<ResponsePayload>, TransportError> {
            self.check_err()?;
            self.recorded_controls
                .borrow_mut()
                .push(format!("remove_folder:{path}"));
            Ok(self.control_response.clone().expect("control_response"))
        }

        fn rescan(
            &self,
            volume_id: Option<u16>,
        ) -> Result<cairn_protocol::message::Envelope<ResponsePayload>, TransportError> {
            self.check_err()?;
            self.recorded_controls
                .borrow_mut()
                .push(format!("rescan:{volume_id:?}"));
            Ok(self.control_response.clone().expect("control_response"))
        }

        fn rescan_source(
            &self,
            source_id: cairn_protocol::message::SourceId,
        ) -> Result<cairn_protocol::message::Envelope<ResponsePayload>, TransportError> {
            self.check_err()?;
            self.recorded_controls
                .borrow_mut()
                .push(format!("rescan_source:{}", source_id.0));
            Ok(self.control_response.clone().expect("control_response"))
        }

        fn reload_service_config(&self) -> Result<(bool, String), TransportError> {
            self.check_err()?;
            self.recorded_controls
                .borrow_mut()
                .push("reload_config".into());
            self.reload_response.clone().expect("reload_response")
        }

        fn get_property_index_status(
            &self,
        ) -> Result<cairn_protocol::message::Envelope<ResponsePayload>, TransportError> {
            self.check_err()?;
            self.recorded_controls
                .borrow_mut()
                .push("get_property_index_status".into());
            Ok(self.control_response.clone().expect("control_response"))
        }

        fn set_property_index_enabled(
            &self,
            enabled: bool,
        ) -> Result<cairn_protocol::message::Envelope<ResponsePayload>, TransportError> {
            self.check_err()?;
            self.recorded_controls
                .borrow_mut()
                .push(format!("set_property_index:{enabled}"));
            Ok(self.control_response.clone().expect("control_response"))
        }

        fn get_index_journal(
            &self,
            after_seq: u64,
            limit: usize,
        ) -> Result<cairn_protocol::message::Envelope<ResponsePayload>, TransportError> {
            self.check_err()?;
            self.recorded_controls
                .borrow_mut()
                .push(format!("get_index_journal:{after_seq}:{limit}"));
            Ok(self.control_response.clone().expect("control_response"))
        }

        fn set_index_journal_enabled(
            &self,
            enabled: bool,
        ) -> Result<cairn_protocol::message::Envelope<ResponsePayload>, TransportError> {
            self.check_err()?;
            self.recorded_controls
                .borrow_mut()
                .push(format!("set_index_journal:{enabled}"));
            Ok(self.control_response.clone().expect("control_response"))
        }
    }

    #[test]
    fn execute_cli_returns_exit_code_2_on_connect_failure() {
        let args = CliArgs::try_parse_from(["CairnCli", "hello"]).unwrap();
        let transport = StubTransport {
            connect_error: Some("pipe not found".to_string()),
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = execute_cli(args, &transport, &mut out, &mut err);
        assert_eq!(code, std::process::ExitCode::from(2));
        let err_text = String::from_utf8(err).unwrap();
        assert!(err_text.contains("Cannot connect to Cairn Service"));
        assert!(out.is_empty());
    }

    #[test]
    fn execute_cli_returns_exit_code_1_on_request_or_payload_error() {
        let args = CliArgs::try_parse_from(["CairnCli", "hello"]).unwrap();
        let transport_err = StubTransport {
            query_error: Some("timeout".to_string()),
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(
            execute_cli(args, &transport_err, &mut out, &mut err),
            std::process::ExitCode::from(1)
        );
        assert!(String::from_utf8(err)
            .unwrap()
            .contains("Query failed: timeout"));

        let args = CliArgs::try_parse_from(["CairnCli", "hello"]).unwrap();
        let service_err = StubTransport {
            query_response: Some(cairn_protocol::message::Envelope {
                volumes: vec![],
                payload: ResponsePayload::Error {
                    code: 500,
                    message: "index busy".to_string(),
                },
            }),
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(
            execute_cli(args, &service_err, &mut out, &mut err),
            std::process::ExitCode::from(1)
        );
        assert!(String::from_utf8(err).unwrap().contains("Search error 500"));
    }

    #[test]
    fn execute_cli_streams_paginated_json_and_csv_with_cjk_escaping() {
        let page1 = vec![
            make_row(1, "alpha.txt", r"C:\dir\alpha.txt", 10),
            make_row(
                2,
                "报告, \"第一版\".docx",
                r"C:\中文 目录\报告, 第一版.docx",
                2048,
            ),
        ];
        let page2 = vec![
            make_row(3, "beta.rs", r"C:\dir\beta.rs", 30),
            make_row(4, "gamma.md", r"C:\dir\gamma.md", 40),
        ];
        let page3 = vec![make_row(5, "delta.log", r"C:\dir\delta.log", 50)];

        let mk_transport = || StubTransport {
            query_response: Some(cairn_protocol::message::Envelope {
                volumes: vec![],
                payload: ResponsePayload::QueryResult {
                    query_id: 1,
                    session_id: 77,
                    total_matches: 5,
                    total_bytes: 2178,
                    first_page: page1.clone(),
                    applied_sort: SortSpec::default(),
                    degraded: None,
                },
            }),
            pages: vec![(2, page2.clone()), (5, page3.clone())],
            ..Default::default()
        };

        let json_args =
            CliArgs::try_parse_from(["CairnCli", "--json", "--max", "5", "report"]).unwrap();
        let transport = mk_transport();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = execute_cli(json_args, &transport, &mut out, &mut err);
        assert_eq!(code, std::process::ExitCode::SUCCESS);
        let decoded: Vec<cairn_protocol::message::FileRowData> =
            serde_json::from_slice(&out).expect("valid streamed JSON");
        assert_eq!(decoded.len(), 5);
        assert_eq!(decoded[1].name, "报告, \"第一版\".docx");
        assert_eq!(decoded[4].frn, 5);
        assert_eq!(*transport.recorded_offsets.borrow(), vec![(2, 3), (5, 1)]);

        let temp = tempfile::tempdir().unwrap();
        let csv_path = temp.path().join("paginated.csv");
        let csv_args = CliArgs::try_parse_from([
            "CairnCli",
            "--max",
            "5",
            "--export-csv",
            csv_path.to_str().unwrap(),
            "report",
        ])
        .unwrap();
        let transport = mk_transport();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = execute_cli(csv_args, &transport, &mut out, &mut err);
        assert_eq!(code, std::process::ExitCode::SUCCESS);
        assert!(String::from_utf8(out)
            .unwrap()
            .contains("Exported 5 results"));

        let mut reader = csv::Reader::from_path(&csv_path).unwrap();
        let records: Vec<csv::StringRecord> = reader.records().map(Result::unwrap).collect();
        assert_eq!(records.len(), 5);
        assert_eq!(&records[1][0], "报告, \"第一版\".docx");
        assert_eq!(&records[1][1], r"C:\中文 目录\报告, 第一版.docx");
        assert_eq!(&records[4][0], "delta.log");
    }

    #[test]
    fn execute_cli_prints_snippets_properties_and_degraded_notice() {
        let mut row = make_row(9, "clip.mp4", r"D:\Media\clip.mp4", 10_240);
        row.snippets = vec!["matched transcript".to_string()];
        row.properties = Some(cairn_protocol::message::RecordPropertyData {
            length_ms: Some(95_000),
            author: Some("Director".to_string()),
            tags: Some("demo".to_string()),
            dimensions: Some((3840, 2160)),
            framerate_mfps: Some(60_000),
        });

        let args = CliArgs::try_parse_from([
            "CairnCli",
            "--sort",
            "-author,+name",
            "--exclude-hidden",
            "--exclude-system",
            "clip",
        ])
        .unwrap();
        let transport = StubTransport {
            query_response: Some(Envelope {
                volumes: vec![],
                payload: ResponsePayload::QueryResult {
                    query_id: 1,
                    session_id: 5,
                    total_matches: 1,
                    total_bytes: 10_240,
                    first_page: vec![row],
                    applied_sort: SortSpec::default(),
                    degraded: Some(
                        cairn_protocol::message::QueryDegradedReason::PropertyIndexDisabled {
                            requested: vec![cairn_protocol::message::PropertyKind::Author],
                        },
                    ),
                },
            }),
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(
            execute_cli(args, &transport, &mut out, &mut err),
            ExitCode::SUCCESS
        );

        let recorded = transport.recorded_queries.borrow();
        assert_eq!(recorded.len(), 1);
        assert!(recorded[0].1.exclude_hidden);
        assert!(recorded[0].1.exclude_system);
        assert_eq!(recorded[0].2.keys.len(), 2);

        let err_text = String::from_utf8(err).unwrap();
        assert!(
            err_text.contains("author"),
            "degraded notice must mention requested property: {err_text}"
        );
        let out_text = String::from_utf8(out).unwrap();
        assert!(out_text.contains("matched transcript"));
        assert!(out_text.contains("Director"));
        assert!(out_text.contains("3840x2160"));
    }

    #[test]
    fn execute_cli_dispatches_platform_subcommands_and_reports_refusals() {
        use cairn_protocol::message::SourceId;
        use cairn_protocol::status::{
            ContentIndexState, IndexJournalEntry, IndexJournalStatus, JournalChangeKind,
            PropertyIndexStatus, SourceKind, SourceStatusSummary, VolumeState,
        };

        let sample_source = SourceStatusSummary {
            source_id: SourceId(1_000),
            kind: SourceKind::Folder,
            root_path: r"D:\Work".to_string(),
            volume_guid: String::new(),
            status: VolumeState::Synced,
            indexed_count: 3,
            live_watch_enabled: true,
        };
        let transport = StubTransport {
            control_response: Some(Envelope {
                volumes: vec![],
                payload: ResponsePayload::SourcesResult {
                    sources: vec![sample_source],
                },
            }),
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = execute_cli(
            CliArgs::try_parse_from(["CairnCli", "sources", "--json"]).unwrap(),
            &transport,
            &mut out,
            &mut err,
        );
        assert_eq!(code, ExitCode::SUCCESS);
        let decoded: Vec<SourceStatusSummary> = serde_json::from_slice(&out).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].root_path, r"D:\Work");

        let denied_reload = StubTransport {
            reload_response: Some(Ok((false, "admin_only_query active".to_string()))),
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = execute_cli(
            CliArgs::try_parse_from(["CairnCli", "reload-config"]).unwrap(),
            &denied_reload,
            &mut out,
            &mut err,
        );
        assert_eq!(code, ExitCode::from(1));
        assert!(String::from_utf8(err)
            .unwrap()
            .contains("admin_only_query active"));

        let journal_transport = StubTransport {
            control_response: Some(Envelope {
                volumes: vec![],
                payload: ResponsePayload::IndexJournalResult {
                    status: IndexJournalStatus {
                        enabled: true,
                        entry_count: 1,
                        resident_bytes: 64,
                        max_entries: 200_000,
                        max_bytes: 33_554_432,
                        oldest_seq: 43,
                        newest_seq: 43,
                    },
                    entries: vec![IndexJournalEntry {
                        seq: 43,
                        timestamp_ts: 1_700_000_000,
                        source_id: SourceId(1_000),
                        frn: 8,
                        kind: JournalChangeKind::Modified,
                        path: r"D:\Work\notes.md".to_string(),
                        old_path: None,
                    }],
                },
            }),
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = execute_cli(
            CliArgs::try_parse_from(["CairnCli", "journal", "--after-seq", "42", "--limit", "20"])
                .unwrap(),
            &journal_transport,
            &mut out,
            &mut err,
        );
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(String::from_utf8(out)
            .unwrap()
            .contains(r"D:\Work\notes.md"));

        let export_dir = tempfile::tempdir().unwrap();
        let export_path = export_dir.path().join("journal.json");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = execute_cli(
            CliArgs::try_parse_from([
                "CairnCli",
                "journal",
                "--export",
                export_path.to_str().unwrap(),
            ])
            .unwrap(),
            &journal_transport,
            &mut out,
            &mut err,
        );
        assert_eq!(code, ExitCode::SUCCESS);
        let exported: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&export_path).unwrap()).unwrap();
        assert_eq!(exported["entries"][0]["path"], r"D:\Work\notes.md");

        let prop_transport = StubTransport {
            control_response: Some(Envelope {
                volumes: vec![],
                payload: ResponsePayload::PropertyIndexStatus {
                    status: PropertyIndexStatus {
                        state: ContentIndexState::Ready,
                        enabled: true,
                        indexed_records: 5,
                        side_table_bytes: 256,
                        last_updated_ts: 1_700_000_000,
                    },
                },
            }),
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = execute_cli(
            CliArgs::try_parse_from(["CairnCli", "property-index", "--enable"]).unwrap(),
            &prop_transport,
            &mut out,
            &mut err,
        );
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(String::from_utf8(out).unwrap().contains("ready"));
        assert_eq!(
            prop_transport.recorded_controls.borrow().as_slice(),
            &["set_property_index:true"]
        );
    }
}
