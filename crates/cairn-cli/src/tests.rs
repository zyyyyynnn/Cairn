// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

use super::common::TransportError;
use super::*;
use cairn_core::sort::{SortColumn, SortDirection, SortKey};
use cairn_protocol::message::Envelope;

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

    let args = CliArgs::try_parse_from(["CairnCli", "add-folder", r"D:\Work"]).expect("add-folder");
    assert!(matches!(
        args.command,
        Some(CliCommand::AddFolder { ref path }) if path == r"D:\Work"
    ));

    let args =
        CliArgs::try_parse_from(["CairnCli", "remove-folder", r"D:\Work"]).expect("remove-folder");
    assert!(matches!(
        args.command,
        Some(CliCommand::RemoveFolder { ref path }) if path == r"D:\Work"
    ));

    let args = CliArgs::try_parse_from(["CairnCli", "rescan", "--volume", "c"]).expect("rescan");
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
        CliArgs::try_parse_from(["CairnCli", "property-index", "--enable", "--disable"]).is_err(),
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

fn make_row(frn: u64, name: &str, path: &str, size: u64) -> cairn_protocol::message::FileRowData {
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
        Ok(self
            .control_response
            .clone()
            .unwrap_or(cairn_protocol::message::Envelope {
                volumes: Vec::new(),
                payload: ResponsePayload::StatusAck,
            }))
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
    assert!(
        String::from_utf8(err)
            .unwrap()
            .contains("Query failed: timeout")
    );

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
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("Exported 5 results")
    );

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
    assert!(
        String::from_utf8(err)
            .unwrap()
            .contains("admin_only_query active")
    );

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
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains(r"D:\Work\notes.md")
    );

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

    let status_transport = StubTransport {
        control_response: Some(Envelope {
            volumes: vec![cairn_protocol::status::VolumeStatusSummary {
                drive_letter: 'C',
                status: cairn_protocol::status::VolumeState::Synced,
                indexed_count: 42,
                usn_enabled: true,
                volume_guid: String::new(),
            }],
            payload: ResponsePayload::StatusAck,
        }),
        ..Default::default()
    };
    let mut status_out = Vec::new();
    let mut status_err = Vec::new();
    let code = execute_cli(
        CliArgs::try_parse_from(["CairnCli", "--status", "--json"]).unwrap(),
        &status_transport,
        &mut status_out,
        &mut status_err,
    );
    assert_eq!(code, ExitCode::SUCCESS);
    let status_json: serde_json::Value = serde_json::from_slice(&status_out).unwrap();
    assert_eq!(status_json["volumes"][0]["drive_letter"], "C");
    assert_eq!(status_json["volumes"][0]["indexed_count"], 42);
}
