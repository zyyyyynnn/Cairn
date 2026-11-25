// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use super::common::{CLI_PAGE_SIZE, CliTransport, TransportError};
use cairn_core::property_store::RecordPropertyData;
use cairn_core::query::QueryDegradedReason;
use cairn_protocol::message::{FileRowData, ResponsePayload};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::process::ExitCode;
pub(super) enum CliStreamError {
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

pub(super) fn write_transport_error(
    err: &mut impl Write,
    context: &str,
    error: TransportError,
) -> ExitCode {
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

pub(super) fn write_json_output(
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

pub(super) fn unexpected_response(err: &mut impl Write) -> ExitCode {
    let _ = writeln!(err, "Unexpected service response.");
    ExitCode::from(1)
}

pub(super) fn write_io_result(result: std::io::Result<()>, err: &mut impl Write) -> ExitCode {
    if let Err(error) = result {
        let _ = writeln!(err, "Failed to write output: {error}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

pub(super) fn handle_stream_error(err: &mut impl Write, error: CliStreamError) -> ExitCode {
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

pub(super) fn stream_query_rows<F>(
    transport: &impl CliTransport,
    session_id: u64,
    total_matches: usize,
    max_rows: usize,
    first_page: &[FileRowData],
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

pub(super) fn render_sources_response(
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

pub(super) fn report_degraded_notice(err: &mut impl Write, degraded: Option<&QueryDegradedReason>) {
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

pub(super) fn sanitize_terminal(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

pub(super) fn format_property_summary(props: &RecordPropertyData) -> Option<String> {
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
pub(super) fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars.saturating_sub(3)).collect();
        out.push_str("...");
        out
    }
}

pub(super) fn create_csv_writer(
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

pub(super) fn write_csv_row(
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
