// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]

//! `cairn-cli.exe`: Command-line interface for Cairn search engine.

use cairn_client::client::CairnClient;
use cairn_protocol::message::ResponsePayload;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "cairn-cli", about = "Cairn CLI")]
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
    #[arg(short, long, default_value = "name")]
    sort: String,

    /// Sort direction: ascending
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    asc: bool,
}

#[tokio::main]
async fn main() {
    let args = CliArgs::parse();

    let mut client = match CairnClient::connect().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: Cannot connect to Cairn Service: {e}");
            eprintln!("Please ensure cairnd service is running (or start via `cairnd --console`).");
            std::process::exit(2);
        }
    };

    if args.status {
        match client.get_status().await {
            Ok(env) => {
                println!(
                    "{:<6} {:<16} {:<12} {:<10}",
                    "Drive", "Status", "Items", "USN"
                );
                println!("{}", "-".repeat(50));
                for v in env.volumes {
                    println!(
                        "{:<6} {:<16} {:<12} {:<10}",
                        format!("{}:", v.drive_letter),
                        v.status.state_key(),
                        v.indexed_count,
                        if v.usn_enabled { "Enabled" } else { "Disabled" }
                    );
                }
            }
            Err(e) => {
                eprintln!("Error querying status: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    match client
        .query(
            1,
            &args.query,
            cairn_protocol::message::QueryDefaults::default(),
            &args.sort,
            args.asc,
            args.max,
        )
        .await
    {
        Ok(env) => {
            // Check for non-synced volumes
            let has_unsynced = env.volumes.iter().any(|v| !v.status.is_synced());
            if has_unsynced {
                eprintln!(
                    "Warning: One or more volumes are not fully synced. Results may be partial."
                );
            }

            match env.payload {
                ResponsePayload::QueryResult {
                    total_matches,
                    first_page,
                    ..
                } => {
                    if args.json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&first_page)
                                .unwrap_or_else(|_| "[]".to_string())
                        );
                        return;
                    }

                    if let Some(csv_path) = args.export_csv {
                        if let Err(e) = export_to_csv(&csv_path, &first_page) {
                            eprintln!("Failed to export CSV: {e}");
                            std::process::exit(1);
                        }
                        println!("Exported {} results to {:?}", first_page.len(), csv_path);
                        return;
                    }

                    println!(
                        "Showing {} of {} matching objects:\n",
                        first_page.len(),
                        total_matches
                    );
                    println!("{:<40} {:<60} {:>12}", "Name", "Path", "Size");
                    println!("{}", "-".repeat(116));

                    for row in &first_page {
                        let size_str = if row.is_dir {
                            "<DIR>".to_string()
                        } else {
                            format_file_size(row.size)
                        };

                        println!(
                            "{:<40} {:<60} {:>12}",
                            truncate_str(&row.name, 38),
                            truncate_str(&row.path, 58),
                            size_str
                        );
                    }
                }
                ResponsePayload::Error { code, message } => {
                    eprintln!("Search error {code}: {message}");
                    std::process::exit(1);
                }
                _ => {
                    eprintln!("Unexpected service response.");
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("Query failed: {e}");
            std::process::exit(1);
        }
    }
}

fn format_file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars - 3).collect();
        out.push_str("...");
        out
    }
}

fn export_to_csv(
    path: &PathBuf,
    rows: &[cairn_protocol::message::FileRowData],
) -> Result<(), Box<dyn std::error::Error>> {
    let mut writer = csv::Writer::from_path(path)?;
    writer.write_record(["Name", "Path", "Size", "Modified", "Created", "IsDir"])?;

    for r in rows {
        writer.write_record([
            &r.name,
            &r.path,
            &r.size.to_string(),
            &r.modified_ts.to_string(),
            &r.created_ts.to_string(),
            &r.is_dir.to_string(),
        ])?;
    }

    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_args_defaults() {
        let args = CliArgs::try_parse_from(["cairn-cli"]).expect("parse default args");
        assert_eq!(args.query, "");
        assert!(!args.status);
        assert!(!args.json);
        assert!(args.export_csv.is_none());
        assert_eq!(args.max, 100);
        assert_eq!(args.sort, "name");
        assert!(args.asc);
    }

    #[test]
    fn test_cli_args_custom_options() {
        let args = CliArgs::try_parse_from([
            "cairn-cli",
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
        assert_eq!(args.sort, "size");
        assert!(!args.asc);
        assert_eq!(args.export_csv, Some(PathBuf::from("results.csv")));
    }

    #[test]
    fn test_format_file_size() {
        assert_eq!(format_file_size(0), "0 B");
        assert_eq!(format_file_size(512), "512 B");
        assert_eq!(format_file_size(1024), "1.0 KB");
        assert_eq!(format_file_size(1536), "1.5 KB");
        assert_eq!(format_file_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_file_size(1024 * 1024 * 1024), "1.00 GB");
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

        export_to_csv(&csv_path, &rows).expect("export_to_csv");

        let content = std::fs::read_to_string(&csv_path).expect("read csv");
        assert!(content.contains("Name,Path,Size,Modified,Created,IsDir"));
        assert!(content.contains(r"file1.txt,C:\test\file1.txt,2048,1600000000,1500000000,false"));
        assert!(content.contains(r"subfolder,C:\test\subfolder,0,1600000100,1500000100,true"));
    }
}
