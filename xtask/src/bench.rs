// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Automated benchmark and regression check suite comparing Cairn against SM-1 and SM-2 budgets.

use cairn_core::engine::CancellationToken;
use cairn_core::frn_index::IndexMemoryFootprint;
use cairn_core::model::{FileRecord, FLAG_DIRECTORY};
use cairn_core::sort::SortColumn;
use cairn_core::syntax::QueryDefaults;
use cairn_ntfs::mft::ExtractedItem;
use cairn_service::sync_engine::{MultiVolumeEngine, VolumeIndex};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::{Duration, Instant};

const CHECK_FIXTURE_COUNT: usize = 100_000;
const MEMORY_BUDGET_BYTES_PER_RECORD: f64 = 125.0;
const CHECK_P95_BUDGET_MS: u64 = 250;
const ROOT_FRN: u64 = 5;

const BENCHMARK_PREFIXES: [&str; 15] = [
    "cairn",
    "system",
    "kernel",
    "build",
    "debug",
    "release",
    "doc",
    "架构",
    "设计",
    "报告",
    "test",
    "benchmark",
    "everything",
    "windows",
    "index",
];

const BENCHMARK_EXTS: [&str; 12] = [
    "rs", "txt", "md", "docx", "xlsx", "pptx", "pdf", "exe", "dll", "log", "toml", "json",
];

const BENCHMARK_PARENTS: [&str; 5] = [
    r"C:\Windows\System32\drivers",
    r"C:\Users\User\Documents\Work\2026",
    r"C:\Workspace\Projects\Cairn\crates",
    r"C:\Program Files\Common Files\System",
    r"C:\Data\Archives\Logs\Monthly",
];

struct QueryMeasurement {
    query: &'static str,
    matches: usize,
    p95: Duration,
}

fn ensure_dir_chain(
    dirs: &mut HashMap<String, u64>,
    items: &mut Vec<ExtractedItem>,
    next_frn: &mut u64,
    parent_path: &str,
) -> u64 {
    if let Some(&frn) = dirs.get(parent_path) {
        return frn;
    }

    let trimmed = parent_path.trim_end_matches(['\\', '/']);
    let without_drive = trimmed
        .strip_prefix("C:\\")
        .or_else(|| trimmed.strip_prefix("C:/"))
        .or_else(|| trimmed.strip_prefix("C:"))
        .unwrap_or(trimmed);

    if without_drive.is_empty() {
        return ROOT_FRN;
    }

    let mut current_path = String::from("C:");
    let mut parent_frn = ROOT_FRN;

    for segment in without_drive.split(['\\', '/']).filter(|s| !s.is_empty()) {
        current_path.push('\\');
        current_path.push_str(segment);
        if let Some(&frn) = dirs.get(&current_path) {
            parent_frn = frn;
            continue;
        }

        let frn = *next_frn;
        *next_frn += 1;
        dirs.insert(current_path.clone(), frn);
        items.push(ExtractedItem {
            frn,
            parent_frn,
            name: segment.to_string(),
            is_dir: true,
            size: 0,
            created_ts: 1_600_000_000,
            modified_ts: 1_600_000_000,
            flags: FLAG_DIRECTORY,
            is_reparse_point: false,
        });
        parent_frn = frn;
    }

    parent_frn
}

fn build_fixture_items(
    corpus_path: Option<&Path>,
    fallback_count: usize,
) -> Result<Vec<ExtractedItem>, Box<dyn std::error::Error>> {
    let mut items = Vec::with_capacity(fallback_count + 32);
    let mut dirs = HashMap::from([(String::from("C:"), ROOT_FRN)]);
    let mut next_frn = 100u64;

    if let Some(path) = corpus_path.filter(|p| p.exists()) {
        let reader = BufReader::new(File::open(path)?);
        for line in reader.lines() {
            let line = line?;
            let mut parts = line.split('\t');
            let Some(name) = parts.next() else { continue };
            let Some(parent) = parts.next() else { continue };
            let Some(size_str) = parts.next() else {
                continue;
            };
            let Some(ts_str) = parts.next() else { continue };

            let size: u64 = size_str.parse().unwrap_or(0);
            let ts: i64 = ts_str.parse().unwrap_or(0);
            let parent_frn = ensure_dir_chain(&mut dirs, &mut items, &mut next_frn, parent);
            let frn = next_frn;
            next_frn += 1;

            items.push(ExtractedItem {
                frn,
                parent_frn,
                name: name.to_string(),
                is_dir: false,
                size,
                created_ts: ts,
                modified_ts: ts,
                flags: 0,
                is_reparse_point: false,
            });
        }
        return Ok(items);
    }

    for parent in BENCHMARK_PARENTS {
        ensure_dir_chain(&mut dirs, &mut items, &mut next_frn, parent);
    }

    let file_target = fallback_count.saturating_sub(items.len());
    for i in 0..file_target {
        let prefix = BENCHMARK_PREFIXES[i % BENCHMARK_PREFIXES.len()];
        let ext = BENCHMARK_EXTS[i % BENCHMARK_EXTS.len()];
        let parent = BENCHMARK_PARENTS[i % BENCHMARK_PARENTS.len()];
        let parent_frn = dirs[parent];
        let frn = next_frn;
        next_frn += 1;

        items.push(ExtractedItem {
            frn,
            parent_frn,
            name: format!("{prefix}_{i:07}.{ext}"),
            is_dir: false,
            size: (i as u64 * 1024) % (10 * 1024 * 1024),
            created_ts: 1_600_000_000 + i as i64,
            modified_ts: 1_600_000_000 + i as i64,
            flags: 0,
            is_reparse_point: false,
        });
    }

    Ok(items)
}

fn build_benchmark_engine(items: Vec<ExtractedItem>, index_dir: &Path) -> MultiVolumeEngine {
    let engine = MultiVolumeEngine::with_index_dir_and_drives(index_dir, Vec::new());
    let mut vol = VolumeIndex::new('C');
    vol.ingest_items(items);
    engine.add_volume(vol);
    engine
}

fn print_footprint(label: &str, fp: &IndexMemoryFootprint) {
    let mb = |bytes: usize| bytes as f64 / (1024.0 * 1024.0);
    println!("\n--- [SM-2] {label} Memory Ledger ---");
    println!(
        "Records: {} (live {}), FileRecord={} B, Total={:.2} MB ({:.2} B/record)",
        fp.record_count,
        fp.live_count,
        std::mem::size_of::<FileRecord>(),
        mb(fp.total_bytes()),
        fp.bytes_per_record()
    );
    println!(
        "  records={:.2} MB | arena={:.2} MB | folded={:.2} MB | frn_lookup={:.2} MB",
        mb(fp.records_bytes),
        mb(fp.arena_bytes),
        mb(fp.folded_arena_bytes),
        mb(fp.frn_lookup_bytes),
    );
    println!(
        "  presorted={:.2} MB | dir_sizes={:.2} MB | path_cache={:.2} MB | scratch={:.2} MB",
        mb(fp.presorted_bytes),
        mb(fp.directory_sizes_bytes),
        mb(fp.path_cache_bytes),
        mb(fp.scratch_bytes),
    );
}

fn validate_check_thresholds(
    idle: &IndexMemoryFootprint,
    warm: &IndexMemoryFootprint,
    measurements: &[QueryMeasurement],
) -> Result<(), String> {
    if idle.record_count == 0 {
        return Err("SM-2 check failed: benchmark fixture is empty".to_string());
    }
    if idle.presorted_bytes != 0 {
        return Err(format!(
            "SM-2 check failed: idle presorted_bytes should be 0 (lazy), got {}",
            idle.presorted_bytes
        ));
    }
    if idle.bytes_per_record() > MEMORY_BUDGET_BYTES_PER_RECORD {
        return Err(format!(
            "SM-2 check failed: idle footprint {:.2} B/record exceeds {:.2} B/record ({idle:?})",
            idle.bytes_per_record(),
            MEMORY_BUDGET_BYTES_PER_RECORD
        ));
    }
    if warm.bytes_per_record() > MEMORY_BUDGET_BYTES_PER_RECORD {
        return Err(format!(
            "SM-2 check failed: single-column warm footprint {:.2} B/record exceeds {:.2} B/record ({warm:?})",
            warm.bytes_per_record(),
            MEMORY_BUDGET_BYTES_PER_RECORD
        ));
    }

    let budget = Duration::from_millis(CHECK_P95_BUDGET_MS);
    for m in measurements {
        if m.matches == 0 {
            return Err(format!(
                "SM-1 check failed: benchmark query {:?} returned 0 matches",
                m.query
            ));
        }
        if m.p95 > budget {
            return Err(format!(
                "SM-1 check failed: query {:?} P95 {:?} exceeded regression ceiling {:?}",
                m.query, m.p95, budget
            ));
        }
    }

    Ok(())
}

pub fn run_benchmark(
    corpus_path: &Path,
    runs: usize,
    check: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let effective_runs = runs.max(1);
    let use_corpus = if check { None } else { Some(corpus_path) };

    println!("=== Cairn Engine Benchmark (SM-1, SM-2) ===");
    match use_corpus.filter(|p| p.exists()) {
        Some(p) => println!("Loading synthetic corpus from {:?}...", p),
        None => println!(
            "Generating {} deterministic multi-level records in memory...",
            CHECK_FIXTURE_COUNT
        ),
    }

    let start_load = Instant::now();
    let items = build_fixture_items(use_corpus, CHECK_FIXTURE_COUNT)?;
    let temp_dir = tempfile::tempdir()?;
    let engine = build_benchmark_engine(items, temp_dir.path());
    let load_dur = start_load.elapsed();

    let idle_footprint = engine.memory_footprint();
    println!(
        "Loaded {} records in {:?}",
        idle_footprint.record_count, load_dur
    );
    print_footprint("Idle (Lazy Presort)", &idle_footprint);

    let benchmark_queries = [
        "cairn",
        "system",
        "*.rs",
        "ext:rs;txt",
        "size:>1mb",
        "^:build",
        "$:log",
        "架构",
        "cairn|system",
        "ext:rs size:<500kb",
        "Documents\\",
        "",
    ];

    println!(
        "\n--- [SM-1] Query Latency Benchmark (Warmup 2x, Samples {}x) ---",
        effective_runs
    );
    println!(
        "{:<25} {:<12} {:<12} {:<10}",
        "Query", "Matches", "Median", "P95"
    );
    println!("{}", "-".repeat(62));

    let defaults = QueryDefaults::default();
    let cancel = CancellationToken::new();
    let mut measurements = Vec::with_capacity(benchmark_queries.len());

    for &q in &benchmark_queries {
        for _ in 0..2 {
            let _ = engine.execute_search_multi(q, &defaults, SortColumn::Name, true, &cancel);
        }

        let mut latencies = Vec::with_capacity(effective_runs);
        let mut match_count = 0usize;

        for _ in 0..effective_runs {
            let t0 = Instant::now();
            let outcome =
                engine.execute_search_multi(q, &defaults, SortColumn::Name, true, &cancel);
            latencies.push(t0.elapsed());
            match_count = outcome.total;
        }

        latencies.sort();
        let median = latencies[latencies.len() / 2];
        let p95_idx = ((latencies.len() as f64 * 0.95) as usize).min(latencies.len() - 1);
        let p95 = latencies[p95_idx];

        let label = if q.is_empty() { "<all>" } else { q };
        println!(
            "{:<25} {:<12} {:<12?} {:<10?}",
            label, match_count, median, p95
        );
        measurements.push(QueryMeasurement {
            query: label,
            matches: match_count,
            p95,
        });
    }

    let warm_footprint = engine.memory_footprint();
    print_footprint("Warm (Post-Query + SortColumn::Name)", &warm_footprint);

    if check {
        validate_check_thresholds(&idle_footprint, &warm_footprint, &measurements)
            .map_err(|err| -> Box<dyn std::error::Error> { err.into() })?;
    } else if idle_footprint.bytes_per_record() > MEMORY_BUDGET_BYTES_PER_RECORD {
        return Err(format!(
            "SM-2 violation: idle full-ledger {:.2} B/record exceeds {:.2} B/record",
            idle_footprint.bytes_per_record(),
            MEMORY_BUDGET_BYTES_PER_RECORD
        )
        .into());
    }

    println!("\nBenchmark completed successfully.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_fixture_builds_multi_level_directory_paths_without_flattening() {
        let items = build_fixture_items(None, 200).expect("fixture");
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let engine = build_benchmark_engine(items, temp_dir.path());

        // Directory hits resolve to their own parent, so pick a file below the
        // deepest fixture directory: its resolved parent proves the chain kept
        // every intermediate level instead of flattening to one component.
        let resolved_parent = engine.with_volumes(|vols| {
            let vol = &vols[&'C'];
            assert!(!vol.records().is_empty(), "the fixture must carry records");
            vol.records()
                .iter()
                .filter(|rec| rec.flags & FLAG_DIRECTORY == 0)
                .find_map(|rec| {
                    let path = vol.path_cache().get_full_path(
                        vol.records(),
                        vol.arena(),
                        'C',
                        rec.parent_idx,
                    );
                    path.contains(r"\Users\User\Documents\Work\2026")
                        .then_some(path)
                })
        });
        assert!(
            resolved_parent.is_some(),
            "expected a file whose parent resolves to the full multi-level path"
        );
    }

    #[test]
    fn validate_check_thresholds_rejects_unaccounted_lookup_bloat_and_slow_queries() {
        let base_idle = IndexMemoryFootprint {
            record_count: 10_000,
            live_count: 10_000,
            records_bytes: 480_000,
            arena_bytes: 160_000,
            folded_arena_bytes: 160_000,
            frn_lookup_bytes: 180_000,
            presorted_bytes: 0,
            directory_sizes_bytes: 0,
            path_cache_bytes: 8_000,
            scratch_bytes: 0,
        };
        let base_warm = IndexMemoryFootprint {
            presorted_bytes: 40_000,
            ..base_idle
        };
        let fast = vec![QueryMeasurement {
            query: "cairn",
            matches: 100,
            p95: Duration::from_millis(5),
        }];
        assert!(validate_check_thresholds(&base_idle, &base_warm, &fast).is_ok());

        // Even though records + arena alone look small, bloated frn_lookup pushes total > 125 B/rec.
        let bloated_idle = IndexMemoryFootprint {
            frn_lookup_bytes: 500_000,
            ..base_idle
        };
        let err = validate_check_thresholds(&bloated_idle, &base_warm, &fast).unwrap_err();
        assert!(err.contains("exceeds 125.00 B/record"), "{err}");

        let slow = vec![QueryMeasurement {
            query: "*.rs",
            matches: 100,
            p95: Duration::from_millis(300),
        }];
        let slow_err = validate_check_thresholds(&base_idle, &base_warm, &slow).unwrap_err();
        assert!(slow_err.contains("regression ceiling"), "{slow_err}");
    }
}
