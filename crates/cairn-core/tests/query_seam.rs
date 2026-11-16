// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for `cairn-core` Seams 3 & 4: query execution across `QRY-10`,
//! `IDX-12` properties, folder sources, multi-key sorting, and `PropertySideTables`.

use cairn_core::engine::CancellationToken;
use cairn_core::model::{FLAG_DIRECTORY, FLAG_HIDDEN, FLAG_READONLY, FLAG_SYSTEM, FLAG_TOMBSTONE};
use cairn_core::property_store::{PropertyKind, PropertySideTables, RecordPropertyData, SourceId};
use cairn_core::query::{execute_query, QueryDegradedReason, QueryPolicy, QueryRows, SourceIndex};
use cairn_core::sort::{PresortedIndices, SortColumn, SortDirection, SortKey, SortSpec};
use cairn_core::syntax::{parse_query, parse_query_with, QueryDefaults};
use cairn_core::{DirectoryPathCache, FileRecord, NamePool};

struct FixtureBuilder {
    arena: NamePool,
    records: Vec<FileRecord>,
}

impl FixtureBuilder {
    fn new() -> Self {
        Self {
            arena: NamePool::default(),
            records: Vec::new(),
        }
    }

    fn push_entry(
        &mut self,
        name: &str,
        parent_idx: u32,
        flags: u16,
        size: u64,
        modified_ts: i64,
        created_ts: i64,
    ) -> u32 {
        let idx = self.records.len() as u32;
        let (name_offset, name_len, ext_offset) = self.arena.insert(name);
        self.records.push(FileRecord {
            frn: idx as u64 + 1,
            parent_idx,
            name_offset,
            name_len,
            ext_offset,
            volume_id: 2,
            flags,
            size,
            modified_ts,
            created_ts,
        });
        idx
    }

    fn push_dir(&mut self, name: &str, parent_idx: u32) -> u32 {
        self.push_entry(
            name,
            parent_idx,
            FLAG_DIRECTORY,
            0,
            1_700_000_000,
            1_600_000_000,
        )
    }

    fn push_file(
        &mut self,
        name: &str,
        parent_idx: u32,
        flags: u16,
        size: u64,
        modified_ts: i64,
        created_ts: i64,
    ) -> u32 {
        self.push_entry(name, parent_idx, flags, size, modified_ts, created_ts)
    }

    fn names_for(&self, indices: &[u32]) -> Vec<String> {
        indices
            .iter()
            .map(|&idx| {
                let rec = &self.records[idx as usize];
                self.arena
                    .get_name(rec.name_offset, rec.name_len)
                    .to_string()
            })
            .collect()
    }
}

fn materialize(rows: QueryRows, presorted: Option<&PresortedIndices>) -> Vec<u32> {
    match rows {
        QueryRows::Materialized(v) => v,
        QueryRows::PresortedAll {
            column,
            descending,
            len,
        } => {
            let ps = presorted.expect("PresortedAll requires presorted index");
            let slice = match column {
                SortColumn::Name => &ps.by_name,
                SortColumn::Path => &ps.by_path,
                SortColumn::Size => &ps.by_size,
                SortColumn::DateModified => &ps.by_date_modified,
                other => panic!("unexpected presorted column {other:?}"),
            };
            assert_eq!(slice.len(), len);
            if descending {
                slice.iter().copied().rev().collect()
            } else {
                slice.clone()
            }
        }
    }
}

#[test]
fn property_side_tables_zero_bytes_when_disabled_and_tracks_sparse_updates() {
    let mut store = PropertySideTables::disabled();
    assert!(!store.is_enabled());
    assert_eq!(store.allocated_bytes(), 0);

    let sample = RecordPropertyData {
        length_ms: Some(185_000),
        author: Some("Alice".to_string()),
        tags: Some("rust;search".to_string()),
        dimensions: Some((1920, 1080)),
        framerate_mfps: Some(59_940),
    };
    assert!(!store.upsert(SourceId(1), 10, &sample));
    assert_eq!(store.allocated_bytes(), 0);
    assert!(store.get(SourceId(1), 10).is_none());

    store.set_enabled(true);
    assert!(store.upsert(SourceId(1), 10, &sample));
    assert!(store.upsert(
        SourceId(2),
        99,
        &RecordPropertyData {
            author: Some("Bob".to_string()),
            ..Default::default()
        }
    ));
    assert!(store.allocated_bytes() > 0);
    assert_eq!(store.record_count(), 2);

    let view = store.get(SourceId(1), 10).expect("present");
    assert_eq!(view.length_ms, Some(185_000));
    assert_eq!(view.author, Some("Alice"));
    assert_eq!(view.tags, Some("rust;search"));
    assert_eq!(view.dimensions, Some((1920, 1080)));
    assert_eq!(view.framerate_mfps, Some(59_940));

    store.clear_source(SourceId(1));
    assert!(store.get(SourceId(1), 10).is_none());
    assert_eq!(
        store.get(SourceId(2), 99).and_then(|v| v.author),
        Some("Bob")
    );

    store.set_enabled(false);
    assert_eq!(store.allocated_bytes(), 0);
    assert_eq!(store.record_count(), 0);
}

#[test]
fn query_supports_pinyin_diacritics_punctuation_and_whitespace_modifiers() {
    let mut fx = FixtureBuilder::new();
    fx.push_file("测试报告.md", FileRecord::ROOT_PARENT, 0, 100, 10, 10);
    fx.push_file("設計文檔.docx", FileRecord::ROOT_PARENT, 0, 100, 10, 10);
    fx.push_file("测试2026报告.txt", FileRecord::ROOT_PARENT, 0, 100, 10, 10);
    fx.push_file("绿茶.txt", FileRecord::ROOT_PARENT, 0, 100, 10, 10);
    fx.push_file("音乐.mp3", FileRecord::ROOT_PARENT, 0, 100, 10, 10);
    fx.push_file("快乐.txt", FileRecord::ROOT_PARENT, 0, 100, 10, 10);
    fx.push_file("文档报告.txt", FileRecord::ROOT_PARENT, 0, 100, 10, 10);
    fx.push_file("cafe-foo_bar.txt", FileRecord::ROOT_PARENT, 0, 100, 10, 10);
    fx.push_file("hello world.txt", FileRecord::ROOT_PARENT, 0, 100, 10, 10);
    fx.push_file("Résumé.pdf", FileRecord::ROOT_PARENT, 0, 100, 10, 10);
    fx.push_file("Resume.pdf", FileRecord::ROOT_PARENT, 0, 100, 10, 10);

    let cache = DirectoryPathCache::new();
    let props = PropertySideTables::disabled();
    let idx = SourceIndex {
        source_id: SourceId(0),
        root_prefix: "C:\\",
        records: &fx.records,
        arena: &fx.arena,
        path_cache: &cache,
        presorted: None,
        properties: &props,
        has_tombstones: false,
    };
    let cancel = CancellationToken::new();
    let policy = QueryPolicy::default();

    let run = |q: &str, defaults: &QueryDefaults| {
        let expr = parse_query_with(q, defaults);
        let res = execute_query(&idx, &expr, &policy, &cancel);
        fx.names_for(&materialize(res.rows, None))
    };

    assert_eq!(
        run("py:csbg", &QueryDefaults::default()),
        vec!["测试报告.md"]
    );
    assert_eq!(
        run("pinyin:sheji", &QueryDefaults::default()),
        vec!["設計文檔.docx"]
    );
    assert_eq!(
        run("py:cs2026", &QueryDefaults::default()),
        vec!["测试2026报告.txt"]
    );
    assert_eq!(run("py:lvcha", &QueryDefaults::default()), vec!["绿茶.txt"]);
    assert_eq!(run("py:lucha", &QueryDefaults::default()), vec!["绿茶.txt"]);
    assert_eq!(
        run("py:yinyue", &QueryDefaults::default()),
        vec!["音乐.mp3"]
    );
    assert_eq!(
        run("py:kuaile", &QueryDefaults::default()),
        vec!["快乐.txt"]
    );
    assert!(run("py:wenb", &QueryDefaults::default()).is_empty());

    assert_eq!(
        run("nopunc:cafefoobar", &QueryDefaults::default()),
        vec!["cafe-foo_bar.txt"]
    );
    assert_eq!(
        run("nows:helloworld", &QueryDefaults::default()),
        vec!["hello world.txt"]
    );
    assert_eq!(
        run("no-diac:resume", &QueryDefaults::default()),
        vec!["Resume.pdf", "Résumé.pdf"]
    );
    let ignore_diac_defaults = QueryDefaults {
        ignore_diacritics: true,
        ..Default::default()
    };
    assert_eq!(
        run("diac:Resume", &ignore_diac_defaults),
        vec!["Resume.pdf"]
    );
}

#[test]
fn query_evaluates_len_attrib_depth_children_and_duplicates() {
    let mut fx = FixtureBuilder::new();
    let src_dir = fx.push_dir("src", FileRecord::ROOT_PARENT);
    let empty_dir = fx.push_dir("empty_dir", FileRecord::ROOT_PARENT);
    let _hidden_sys = fx.push_file(
        "sys.bin",
        FileRecord::ROOT_PARENT,
        FLAG_READONLY | FLAG_HIDDEN | FLAG_SYSTEM,
        512,
        10,
        10,
    );
    let _main_rs = fx.push_file("main.rs", src_dir, FLAG_READONLY, 256, 20, 20);
    let _lib_rs = fx.push_file("shared.txt", src_dir, 0, 512, 30, 30);
    let _dup_name = fx.push_file("shared.txt", FileRecord::ROOT_PARENT, 0, 128, 40, 40);
    let _dup_part = fx.push_file("shared.md", FileRecord::ROOT_PARENT, 0, 999, 50, 50);
    let _cjk_name = fx.push_file("四字中文.txt", FileRecord::ROOT_PARENT, 0, 42, 60, 60);

    let cache = DirectoryPathCache::new();
    let props = PropertySideTables::disabled();
    // Root as a non-drive FolderSource (`D:\Projects\Cairn`)
    let idx = SourceIndex {
        source_id: SourceId(1),
        root_prefix: r"D:\Projects\Cairn",
        records: &fx.records,
        arena: &fx.arena,
        path_cache: &cache,
        presorted: None,
        properties: &props,
        has_tombstones: false,
    };
    let cancel = CancellationToken::new();
    let policy = QueryPolicy::default();

    let run = |q: &str| {
        let expr = parse_query(q);
        let res = execute_query(&idx, &expr, &policy, &cancel);
        fx.names_for(&materialize(res.rows, None))
    };

    // `D:\Projects\Cairn` has 3 path segments; `src\main.rs` sits at depth 4.
    assert_eq!(run("depth:4"), vec!["main.rs", "shared.txt"]);
    assert_eq!(
        run(r"parent:D:\Projects\Cairn\src"),
        vec!["main.rs", "shared.txt"]
    );
    // "四字中文.txt" has 8 Unicode chars (`len:8`) while UTF-8 byte length is 16; "main.rs" and "sys.bin" are 7 chars.
    assert_eq!(run("len:8"), vec!["四字中文.txt"]);
    assert_eq!(run("len:7"), vec!["main.rs", "sys.bin"]);
    assert_eq!(run("attrib:rhs"), vec!["sys.bin"]);
    assert_eq!(run("attrib:r-h"), vec!["main.rs"]);

    assert_eq!(run("child:*.rs"), vec!["src"]);
    assert_eq!(run("childfilecount:2"), vec!["src"]);
    assert_eq!(run("childfoldercount:0"), vec!["empty_dir", "src"]);
    assert_eq!(run("empty:"), vec!["empty_dir"]);

    assert_eq!(run("dupe:"), vec!["shared.txt", "shared.txt"]);
    assert_eq!(run("sizedupe:"), vec!["shared.txt", "sys.bin"]);
    assert_eq!(
        run("namepartdupe:"),
        vec!["shared.md", "shared.txt", "shared.txt"]
    );
    let unique = run("unique:");
    assert!(!unique.contains(&"shared.txt".to_string()));
    assert!(unique.contains(&"main.rs".to_string()));
    let _ = empty_dir;
}

#[test]
fn property_queries_work_when_enabled_and_report_explicit_degradation_when_disabled() {
    let mut fx = FixtureBuilder::new();
    let clip_hd = fx.push_file("clip_hd.mp4", FileRecord::ROOT_PARENT, 0, 1000, 10, 10);
    let clip_4k = fx.push_file("clip_4k.mp4", FileRecord::ROOT_PARENT, 0, 5000, 20, 20);
    let song = fx.push_file("track.flac", FileRecord::ROOT_PARENT, 0, 3000, 30, 30);
    let plain = fx.push_file("notes.txt", FileRecord::ROOT_PARENT, 0, 100, 40, 40);

    let mut props = PropertySideTables::new(true);
    props.upsert(
        SourceId(3),
        fx.records[clip_hd as usize].frn,
        &RecordPropertyData {
            length_ms: Some(45_000),
            author: Some("Alice Studio".to_string()),
            tags: Some("demo;hd".to_string()),
            dimensions: Some((1280, 720)),
            framerate_mfps: Some(30_000),
        },
    );
    props.upsert(
        SourceId(3),
        fx.records[clip_4k as usize].frn,
        &RecordPropertyData {
            length_ms: Some(180_000),
            author: Some("Alice Studio".to_string()),
            tags: Some("demo;4k;hdr".to_string()),
            dimensions: Some((3840, 2160)),
            framerate_mfps: Some(60_000),
        },
    );
    props.upsert(
        SourceId(3),
        fx.records[song as usize].frn,
        &RecordPropertyData {
            length_ms: Some(210_000),
            author: Some("Clara".to_string()),
            tags: Some("ost;flac".to_string()),
            ..Default::default()
        },
    );
    let _ = plain;

    let cache = DirectoryPathCache::new();
    let cancel = CancellationToken::new();
    let enabled_idx = SourceIndex {
        source_id: SourceId(3),
        root_prefix: "D:\\",
        records: &fx.records,
        arena: &fx.arena,
        path_cache: &cache,
        presorted: None,
        properties: &props,
        has_tombstones: false,
    };

    let run_enabled = |q: &str, sort: SortSpec| {
        let expr = parse_query(q);
        let res = execute_query(
            &enabled_idx,
            &expr,
            &QueryPolicy {
                sort,
                content_hit: None,
            },
            &cancel,
        );
        assert!(res.degraded.is_none());
        fx.names_for(&materialize(res.rows, None))
    };

    assert_eq!(
        run_enabled(
            "length:>2m",
            SortSpec::single(SortColumn::Length, SortDirection::Descending)
        ),
        vec!["track.flac", "clip_4k.mp4"]
    );
    assert_eq!(
        run_enabled("author:alice", SortSpec::default()),
        vec!["clip_4k.mp4", "clip_hd.mp4"]
    );
    assert_eq!(
        run_enabled("tags:hdr", SortSpec::default()),
        vec!["clip_4k.mp4"]
    );
    assert_eq!(
        run_enabled("dimensions:>=1920x1080", SortSpec::default()),
        vec!["clip_4k.mp4"]
    );
    assert_eq!(
        run_enabled("framerate:>30", SortSpec::default()),
        vec!["clip_4k.mp4"]
    );

    // NULLS LAST: populated lengths first descending, `notes.txt` (None) last.
    let all_by_len = run_enabled(
        "",
        SortSpec::single(SortColumn::Length, SortDirection::Descending),
    );
    assert_eq!(all_by_len.last().map(String::as_str), Some("notes.txt"));

    // Disabled store must return empty results AND explicit degraded reason.
    let disabled_props = PropertySideTables::disabled();
    let disabled_idx = SourceIndex {
        source_id: SourceId(3),
        root_prefix: "D:\\",
        records: &fx.records,
        arena: &fx.arena,
        path_cache: &cache,
        presorted: None,
        properties: &disabled_props,
        has_tombstones: false,
    };
    let expr = parse_query("length:>1m tags:hdr");
    let degraded = execute_query(&disabled_idx, &expr, &QueryPolicy::default(), &cancel);
    assert!(materialize(degraded.rows, None).is_empty());
    assert_eq!(
        degraded.degraded,
        Some(QueryDegradedReason::PropertyIndexDisabled {
            requested: vec![PropertyKind::Length, PropertyKind::Tags],
        })
    );
}

#[test]
fn multi_key_sort_presorted_all_and_hidden_system_opt_in_contract() {
    let mut fx = FixtureBuilder::new();
    fx.push_file("b.txt", FileRecord::ROOT_PARENT, 0, 200, 10, 300);
    fx.push_file("a.txt", FileRecord::ROOT_PARENT, 0, 200, 20, 100);
    fx.push_file("c.log", FileRecord::ROOT_PARENT, FLAG_HIDDEN, 50, 30, 200);
    // Deliberate tombstone to verify normal queries exclude tombstones even on zero-filter paths.
    fx.push_file(
        "gone.txt",
        FileRecord::ROOT_PARENT,
        FLAG_TOMBSTONE,
        10,
        40,
        400,
    );

    let cache = DirectoryPathCache::new();
    let props = PropertySideTables::disabled();
    let cancel = CancellationToken::new();

    // Multi-key inline sort: equal sizes break by Name ascending without reversing Name.
    let idx_with_tombstone = SourceIndex {
        source_id: SourceId(0),
        root_prefix: "C:\\",
        records: &fx.records,
        arena: &fx.arena,
        path_cache: &cache,
        presorted: None,
        properties: &props,
        has_tombstones: false,
    };
    let expr = parse_query("sort:size-desc,name-asc");
    let res = execute_query(&idx_with_tombstone, &expr, &QueryPolicy::default(), &cancel);
    assert_eq!(
        res.applied_sort,
        SortSpec {
            keys: vec![
                SortKey::new(SortColumn::Size, SortDirection::Descending),
                SortKey::new(SortColumn::Name, SortDirection::Ascending),
            ]
        }
    );
    assert_eq!(
        fx.names_for(&materialize(res.rows, None)),
        vec!["a.txt", "b.txt", "c.log"]
    );

    // DateCreated & Extension sorting.
    let res_created = execute_query(
        &idx_with_tombstone,
        &parse_query(""),
        &QueryPolicy {
            sort: SortSpec::single(SortColumn::DateCreated, SortDirection::Ascending),
            content_hit: None,
        },
        &cancel,
    );
    assert_eq!(
        fx.names_for(&materialize(res_created.rows, None)),
        vec!["a.txt", "c.log", "b.txt"]
    );

    // Clean fixture without tombstones tests zero-materialization `PresortedAll` path.
    fx.records.pop();
    let presorted = PresortedIndices::build(&fx.records, &fx.arena, &cache, 'C');
    let clean_idx = SourceIndex {
        source_id: SourceId(0),
        root_prefix: "C:\\",
        records: &fx.records,
        arena: &fx.arena,
        path_cache: &cache,
        presorted: Some(&presorted),
        properties: &props,
        has_tombstones: false,
    };
    let unfiltered = execute_query(
        &clean_idx,
        &parse_query_with("", &QueryDefaults::default()),
        &QueryPolicy::default(),
        &cancel,
    );
    assert!(matches!(
        unfiltered.rows,
        QueryRows::PresortedAll {
            column: SortColumn::Name,
            descending: false,
            len: 3
        }
    ));

    // Enabling `exclude_hidden` narrows via Materialized even on empty query text.
    let hidden_filtered = execute_query(
        &clean_idx,
        &parse_query_with(
            "",
            &QueryDefaults {
                exclude_hidden: true,
                ..Default::default()
            },
        ),
        &QueryPolicy::default(),
        &cancel,
    );
    assert!(matches!(hidden_filtered.rows, QueryRows::Materialized(_)));
    assert_eq!(
        fx.names_for(&materialize(hidden_filtered.rows, Some(&presorted))),
        vec!["a.txt", "b.txt"]
    );
}
