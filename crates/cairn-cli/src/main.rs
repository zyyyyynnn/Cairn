// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]

//! `CairnCli.exe`: Command-line interface for Cairn search engine.

use cairn_client::client::{CairnClient, ClientError};
use cairn_core::format_size_bytes;
use cairn_core::sort::{SortColumn, UnknownSortColumn};
use cairn_protocol::message::{Envelope, FileRowData, QueryDefaults, ResponsePayload};
use clap::Parser;
use std::cell::{Ref, RefCell, RefMut};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const CLI_PAGE_SIZE: usize = 500;

fn parse_sort_column(value: &str) -> Result<SortColumn, String> {
    value
        .parse()
        .map_err(|error: UnknownSortColumn| error.to_string())
}

#[derive(Parser, Debug)]
#[command(name = "CairnCli", about = "Cairn CLI")]
struct CliArgs {
    /// Search query string
    #[arg(default_value = "")]
    query: String,

    /// Show volume and indexing status
    #[arg(long)]
    status: bool,

    /// Output results in JSON format
    #[arg(long)]
    json: bool,

    /// Export results to a CSV file
    #[arg(long)]
    export_csv: Option<PathBuf>,

    /// Maximum number of results to retrieve
    #[arg(short, long, default_value_t = 100)]
    max: usize,

    /// Column to sort results by: name, path, size, modified, created, ext
    #[arg(short, long, default_value = "name", value_parser = parse_sort_column)]
    sort: SortColumn,

    /// Sort direction: ascending
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    asc: bool,
}

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
        sort_column: SortColumn,
        ascending: bool,
        first_page_limit: usize,
    ) -> Result<Envelope<ResponsePayload>, TransportError>;

    fn get_rows(
        &self,
        session_id: u64,
        offset: usize,
        limit: usize,
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
}

fn map_request_result(
    res: Result<Envelope<ResponsePayload>, ClientError>,
) -> Result<Envelope<ResponsePayload>, TransportError> {
    res.map_err(|e| TransportError::Request(e.to_string()))
}

impl CliTransport for PipeCliTransport {
    fn get_status(&self) -> Result<Envelope<ResponsePayload>, TransportError> {
        let (rt, mut client) = self.ensure_ready()?;
        map_request_result(rt.block_on(client.get_status()))
    }

    fn query(
        &self,
        query_id: u64,
        raw_query: &str,
        defaults: QueryDefaults,
        sort_column: SortColumn,
        ascending: bool,
        first_page_limit: usize,
    ) -> Result<Envelope<ResponsePayload>, TransportError> {
        let (rt, mut client) = self.ensure_ready()?;
        map_request_result(rt.block_on(client.query(
            query_id,
            raw_query,
            defaults,
            sort_column,
            ascending,
            first_page_limit,
        )))
    }

    fn get_rows(
        &self,
        session_id: u64,
        offset: usize,
        limit: usize,
    ) -> Result<Envelope<ResponsePayload>, TransportError> {
        let (rt, mut client) = self.ensure_ready()?;
        map_request_result(rt.block_on(client.get_rows(session_id, offset, limit)))
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
            let _ = writeln!(err, "Search error {code}: {message}");
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

    while emitted < target {
        let limit = (target - emitted).min(CLI_PAGE_SIZE);
        let env = transport
            .get_rows(session_id, emitted, limit)
            .map_err(|error| CliStreamError::Transport {
                context: "Query failed",
                error,
            })?;
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

fn execute_cli(
    args: CliArgs,
    transport: &impl CliTransport,
    out: &mut impl Write,
    err: &mut impl Write,
) -> ExitCode {
    if args.status {
        let env = match transport.get_status() {
            Ok(env) => env,
            Err(error) => return write_transport_error(err, "Error querying status", error),
        };
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
        if let Err(error) = write_result {
            let _ = writeln!(err, "Failed to write output: {error}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    let env = match transport.query(
        1,
        &args.query,
        QueryDefaults::default(),
        args.sort,
        args.asc,
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
                let _ = writeln!(err, "Failed to export CSV: {error}");
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
                let _ = writeln!(err, "Failed to export CSV: {error}");
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
            writeln!(
                buf,
                "{:<40} {:<60} {:>12}",
                truncate_str(&row.name, 38),
                truncate_str(&row.path, 58),
                size_str
            )?;
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
    writer.write_record(["Name", "Path", "Size", "Modified", "Created", "IsDir"])?;
    Ok(writer)
}

fn write_csv_row(
    writer: &mut csv::Writer<impl Write>,
    r: &FileRowData,
) -> Result<(), Box<dyn std::error::Error>> {
    writer.write_record([
        r.name.as_str(),
        r.path.as_str(),
        &r.size.to_string(),
        &r.modified_ts.to_string(),
        &r.created_ts.to_string(),
        &r.is_dir.to_string(),
    ])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            "size",
            "--asc=false",
            "--export-csv",
            "results.csv",
        ])
        .expect("parse custom args");

        assert_eq!(args.query, "my_query");
        assert!(args.status);
        assert!(args.json);
        assert_eq!(args.max, 50);
        assert_eq!(args.sort, SortColumn::Size);
        assert!(!args.asc);
        assert_eq!(args.export_csv, Some(PathBuf::from("results.csv")));
    }

    #[test]
    fn an_unknown_sort_column_is_refused_at_the_command_line() {
        assert!(
            CliArgs::try_parse_from(["CairnCli", "--sort", "bogus"]).is_err(),
            "an unknown column must be refused, not silently answered as name"
        );
    }

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello", 5), "hello");
        assert_eq!(truncate_str("hello world", 8), "hello...");
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
                snippet: None,
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
                snippet: None,
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
        assert!(content.contains("Name,Path,Size,Modified,Created,IsDir"));
        assert!(content.contains(r"file1.txt,C:\test\file1.txt,2048,1600000000,1500000000,false"));
        assert!(content.contains(r"subfolder,C:\test\subfolder,0,1600000100,1500000100,true"));
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
            snippet: None,
            frn,
            name: name.to_string(),
            path: path.to_string(),
            size,
            modified_ts: 1700000000,
            created_ts: 1600000000,
            is_dir: false,
        }
    }

    struct StubTransport {
        connect_error: Option<String>,
        query_error: Option<String>,
        query_response: Option<cairn_protocol::message::Envelope<ResponsePayload>>,
        pages: Vec<(usize, Vec<cairn_protocol::message::FileRowData>)>,
        recorded_offsets: std::cell::RefCell<Vec<(usize, usize)>>,
    }

    impl CliTransport for StubTransport {
        fn get_status(
            &self,
        ) -> Result<cairn_protocol::message::Envelope<ResponsePayload>, TransportError> {
            if let Some(msg) = &self.connect_error {
                return Err(TransportError::Connect(msg.clone()));
            }
            if let Some(msg) = &self.query_error {
                return Err(TransportError::Request(msg.clone()));
            }
            unreachable!("status unused in stub")
        }

        fn query(
            &self,
            _query_id: u64,
            _raw_query: &str,
            _defaults: cairn_protocol::message::QueryDefaults,
            _sort_column: SortColumn,
            _ascending: bool,
            _first_page_limit: usize,
        ) -> Result<cairn_protocol::message::Envelope<ResponsePayload>, TransportError> {
            if let Some(msg) = &self.connect_error {
                return Err(TransportError::Connect(msg.clone()));
            }
            if let Some(msg) = &self.query_error {
                return Err(TransportError::Request(msg.clone()));
            }
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
    }

    #[test]
    fn execute_cli_returns_exit_code_2_on_connect_failure() {
        let args = CliArgs::try_parse_from(["CairnCli", "hello"]).unwrap();
        let transport = StubTransport {
            connect_error: Some("pipe not found".to_string()),
            query_error: None,
            query_response: None,
            pages: vec![],
            recorded_offsets: std::cell::RefCell::new(vec![]),
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
            connect_error: None,
            query_error: Some("timeout".to_string()),
            query_response: None,
            pages: vec![],
            recorded_offsets: std::cell::RefCell::new(vec![]),
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
            connect_error: None,
            query_error: None,
            query_response: Some(cairn_protocol::message::Envelope {
                volumes: vec![],
                payload: ResponsePayload::Error {
                    code: 500,
                    message: "index busy".to_string(),
                },
            }),
            pages: vec![],
            recorded_offsets: std::cell::RefCell::new(vec![]),
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
            connect_error: None,
            query_error: None,
            query_response: Some(cairn_protocol::message::Envelope {
                volumes: vec![],
                payload: ResponsePayload::QueryResult {
                    query_id: 1,
                    session_id: 77,
                    total_matches: 5,
                    total_bytes: 2178,
                    first_page: page1.clone(),
                },
            }),
            pages: vec![(2, page2.clone()), (4, page3.clone())],
            recorded_offsets: std::cell::RefCell::new(vec![]),
        };

        // 1. Paginated streaming JSON output
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
        assert_eq!(*transport.recorded_offsets.borrow(), vec![(2, 3), (4, 1)]);

        // 2. Paginated streaming CSV export with CJK, commas, and quotes
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
}
