// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use super::common::{CliCommand, CliTransport, TransportError};
use super::format::{
    render_sources_response, sanitize_terminal, unexpected_response, write_io_result,
    write_json_output, write_transport_error,
};
use cairn_client::client::{CairnClient, ClientError};
use cairn_core::property_store::SourceId;
use cairn_core::sort::SortSpec;
use cairn_protocol::message::{Envelope, QueryDefaults, ResponsePayload};
use std::cell::{Ref, RefCell, RefMut};
use std::io::{BufWriter, Write};
use std::process::ExitCode;

/// Lazily creates a single-thread Tokio runtime and pipe client only when a command executes.
#[derive(Default)]
pub(super) struct PipeCliTransport {
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

pub(super) fn map_request_result<T>(res: Result<T, ClientError>) -> Result<T, TransportError> {
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

pub(super) fn expect_control_payload(
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

pub(super) fn execute_subcommand(
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
