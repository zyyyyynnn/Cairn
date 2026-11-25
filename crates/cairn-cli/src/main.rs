// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]

//! `CairnCli.exe`: Command-line interface for Cairn search engine.

use cairn_core::format_size_bytes;
use cairn_core::sort::{SortSpec, UnknownSortColumn};
use cairn_protocol::message::{QueryDefaults, ResponsePayload};
use clap::Parser;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;

mod common;
mod control;
mod format;
#[cfg(test)]
mod tests;

use common::{CLI_PAGE_SIZE, CliCommand, CliTransport, MAX_CLI_SNIPPETS};
use control::{PipeCliTransport, execute_subcommand};
use format::{
    CliStreamError, create_csv_writer, format_property_summary, handle_stream_error,
    report_degraded_notice, sanitize_terminal, stream_query_rows, truncate_str,
    unexpected_response, write_csv_row, write_io_result, write_json_output, write_transport_error,
};
fn parse_sort_spec(value: &str) -> Result<SortSpec, String> {
    SortSpec::parse_clause(value).map_err(|error: UnknownSortColumn| error.to_string())
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

    /// Comma-separated sort clauses, e.g. -size,+name or modified-desc
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
        if args.json {
            return write_json_output(&serde_json::json!({ "volumes": env.volumes }), out, err);
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
            &first_page,
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
            &first_page,
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
        &first_page,
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
