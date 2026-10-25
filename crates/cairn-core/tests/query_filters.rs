// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! End-to-end regressions for SPEC QRY-02 (operators), QRY-03 (modifiers) and QRY-04 (functions).
//!
//! Every case runs the real pipeline `parse_query` -> `execute_query`. The filter parsers
//! have their own unit tests, but those call `SizeFilter::parse(">10mb")` directly and so
//! bypass the lexer; the defects covered here (a bare `>` truncating the search item, and
//! `ext:` rejecting wildcards) are only visible through the full chain. Each case asserts an
//! exact hit count so a query that silently degrades to "the whole library" cannot pass.

use cairn_core::arena::NamePool;
use cairn_core::engine::{execute_query, CancellationToken};
use cairn_core::model::{FileRecord, FLAG_DIRECTORY};
use cairn_core::path_cache::DirectoryPathCache;
use cairn_core::syntax::{parse_query_with, QueryDefaults};

const KB: u64 = 1024;
const MB: u64 = 1024 * KB;
const GB: u64 = 1024 * MB;

/// One indexed entry: name, logical size, and the day (UTC) of its modification/creation time.
struct Item {
    name: &'static str,
    size: u64,
    modified: (i64, i64, i64),
    created: (i64, i64, i64),
    dir: bool,
}

/// Fixture deliberately built so that "match all" (13) is never a plausible answer for the
/// asserted counts, and so that boundary values exist on purpose:
/// `架构图.md` is exactly 1 MB (excluded by `size:>1mb`, included by `size:>=1mb`),
/// `notes.txt` is exactly 1 KB, and `build.rs`/`orange.rs`/`apple.txt` sit on both sides of
/// the 2026/2027 year boundary. `apple banana.txt` contains a space, which only a quoted
/// literal can express as one search item.
const ITEMS: &[Item] = &[
    Item {
        name: "tiny.txt",
        size: 10,
        modified: (2025, 3, 1),
        created: (2025, 3, 1),
        dir: false,
    },
    Item {
        name: "notes.txt",
        size: KB,
        modified: (2026, 1, 15),
        created: (2024, 6, 1),
        dir: false,
    },
    Item {
        name: "guide.md",
        size: 500 * KB,
        modified: (2026, 8, 1),
        created: (2026, 8, 1),
        dir: false,
    },
    Item {
        name: "report.log",
        size: 2 * MB,
        modified: (2027, 2, 1),
        created: (2025, 1, 1),
        dir: false,
    },
    Item {
        name: "video.mkv",
        size: 5 * MB,
        modified: (2027, 5, 5),
        created: (2027, 5, 5),
        dir: false,
    },
    Item {
        name: "archive.iso",
        size: 2 * GB,
        modified: (2020, 1, 1),
        created: (2020, 1, 1),
        dir: false,
    },
    Item {
        name: "build.rs",
        size: 3 * MB,
        modified: (2026, 12, 31),
        created: (2027, 3, 1),
        dir: false,
    },
    Item {
        name: "架构图.md",
        size: MB,
        modified: (2026, 7, 2),
        created: (2026, 7, 2),
        dir: false,
    },
    Item {
        name: "apple.txt",
        size: 0,
        modified: (2027, 6, 1),
        created: (2027, 6, 1),
        dir: false,
    },
    Item {
        name: "apple banana.txt",
        size: 4 * MB,
        modified: (2026, 6, 1),
        created: (2026, 6, 1),
        dir: false,
    },
    Item {
        name: "banana.md",
        size: 600 * KB,
        modified: (2027, 1, 2),
        created: (2027, 1, 2),
        dir: false,
    },
    Item {
        name: "orange.rs",
        size: 0,
        modified: (2026, 5, 1),
        created: (2026, 5, 1),
        dir: false,
    },
    Item {
        name: "Docs",
        size: 0,
        modified: (2026, 7, 2),
        created: (2026, 7, 2),
        dir: true,
    },
];

/// Total fixture size, i.e. the number a silently-degraded "match everything" query returns.
const TOTAL: usize = ITEMS.len();

/// Unix seconds of midnight UTC on the given day, computed independently of `filter.rs`.
fn utc_day(y: i64, m: i64, d: i64) -> i64 {
    let era_year = if m <= 2 { y - 1 } else { y };
    let era = era_year.div_euclid(400);
    let yoe = era_year - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146097 + doe - 719468) * 86400
}

struct Fixture {
    pool: NamePool,
    records: Vec<FileRecord>,
    path_cache: DirectoryPathCache,
    cancel: CancellationToken,
}

impl Fixture {
    fn new() -> Self {
        let mut pool = NamePool::with_capacity(4096);
        let mut records = Vec::with_capacity(TOTAL);
        for (i, item) in ITEMS.iter().enumerate() {
            let (name_offset, name_len, ext_offset) = pool.insert(item.name);
            records.push(FileRecord {
                frn: 100 + i as u64,
                parent_idx: FileRecord::ROOT_PARENT,
                name_offset,
                name_len,
                ext_offset,
                volume_id: 0,
                flags: if item.dir { FLAG_DIRECTORY } else { 0 },
                size: item.size,
                modified_ts: utc_day(item.modified.0, item.modified.1, item.modified.2),
                created_ts: utc_day(item.created.0, item.created.1, item.created.2),
            });
        }
        Self {
            pool,
            records,
            path_cache: DirectoryPathCache::new(),
            cancel: CancellationToken::new(),
        }
    }

    fn names_for(&self, query: &str) -> Vec<&'static str> {
        self.names_for_with(query, &QueryDefaults::default())
    }

    fn names_for_with(&self, query: &str, defaults: &QueryDefaults) -> Vec<&'static str> {
        let expr = parse_query_with(query, defaults);
        let hits = execute_query(
            &self.records,
            &self.pool,
            &self.path_cache,
            'C',
            &expr,
            &self.cancel,
        );
        hits.iter().map(|idx| ITEMS[*idx as usize].name).collect()
    }

    /// Asserts the exact hit set (in index order) for a query.
    fn expect(&self, query: &str, expected: &[&'static str]) {
        let actual = self.names_for(query);
        assert_eq!(
            actual, expected,
            "query {query:?} matched {actual:?}, expected {expected:?}"
        );
    }

    /// Asserts the exact hit set of a query executed with query-wide defaults.
    fn expect_with(&self, query: &str, defaults: &QueryDefaults, expected: &[&'static str]) {
        let actual = self.names_for_with(query, defaults);
        assert_eq!(
            actual, expected,
            "query {query:?} with defaults {defaults:?} matched {actual:?}, expected {expected:?}"
        );
    }
}

/// Guards the fixture's own date arithmetic, so the exact counts below mean something.
#[test]
fn fixture_date_helper_matches_known_unix_timestamps() {
    assert_eq!(utc_day(1970, 1, 1), 0);
    assert_eq!(utc_day(2020, 1, 1), 1577836800);
    assert_eq!(utc_day(2026, 1, 1), 1767225600);
    assert_eq!(utc_day(2027, 1, 1), 1798761600);
    assert_eq!(utc_day(2026, 7, 2), 1782950400);
    assert_eq!(TOTAL, 13);
}

#[test]
fn size_operators_are_lexed_and_executed() {
    let f = Fixture::new();

    // SPEC QRY-04: bare `>` and `>=` inside numeric filters stay inside the search item.
    f.expect(
        "size:>1mb",
        &[
            "report.log",
            "video.mkv",
            "archive.iso",
            "build.rs",
            "apple banana.txt",
        ],
    );
    f.expect(
        "size:>=1mb",
        &[
            "report.log",
            "video.mkv",
            "archive.iso",
            "build.rs",
            "架构图.md",
            "apple banana.txt",
        ],
    );
    f.expect("size:>10mb", &["archive.iso"]);
    f.expect(
        "size:<1mb",
        &[
            "tiny.txt",
            "notes.txt",
            "guide.md",
            "apple.txt",
            "banana.md",
            "orange.rs",
            "Docs",
        ],
    );
    f.expect(
        "size:<=1kb",
        &["tiny.txt", "notes.txt", "apple.txt", "orange.rs", "Docs"],
    );
    f.expect("size:=0", &["apple.txt", "orange.rs", "Docs"]);

    // `<` and `<=` worked before the fix; the cases above pin them so the fix cannot
    // regress them. A degraded "match everything" answer is the specific failure mode.
    for query in [
        "size:>1mb",
        "size:>=1mb",
        "size:>10mb",
        "size:<1mb",
        "size:<=1kb",
        "size:=0",
    ] {
        assert!(
            f.names_for(query).len() < TOTAL,
            "{query} must not return the whole library ({TOTAL} records)"
        );
    }
}

#[test]
fn size_ranges_and_negation_operators() {
    let f = Fixture::new();

    // Range forms `a..b` and `a-b` are both inclusive.
    f.expect(
        "size:1kb..1mb",
        &["notes.txt", "guide.md", "架构图.md", "banana.md"],
    );
    f.expect(
        "size:1mb-5mb",
        &[
            "report.log",
            "video.mkv",
            "build.rs",
            "架构图.md",
            "apple banana.txt",
        ],
    );
    f.expect(
        "size:!=1kb",
        &[
            "tiny.txt",
            "guide.md",
            "report.log",
            "video.mkv",
            "archive.iso",
            "build.rs",
            "架构图.md",
            "apple.txt",
            "apple banana.txt",
            "banana.md",
            "orange.rs",
            "Docs",
        ],
    );
}

#[test]
fn date_operators_are_lexed_and_executed() {
    let f = Fixture::new();

    // `>` on a date value spans everything after the whole 2026 year.
    f.expect(
        "dm:>2026",
        &["report.log", "video.mkv", "apple.txt", "banana.md"],
    );
    f.expect(
        "dc:>2026",
        &["video.mkv", "build.rs", "apple.txt", "banana.md"],
    );
    f.expect("dm:<2026", &["tiny.txt", "archive.iso"]);
    f.expect(
        "dm:2026",
        &[
            "notes.txt",
            "guide.md",
            "build.rs",
            "架构图.md",
            "apple banana.txt",
            "orange.rs",
            "Docs",
        ],
    );
    assert_eq!(f.names_for("dm:>=2026").len(), 11);
    assert_eq!(f.names_for("date-modified:>2026").len(), 4);
    assert_eq!(f.names_for("date-created:>2026").len(), 4);

    // Long form and a day-precision range must agree with the year operator's boundary.
    f.expect(
        "dm:2027-01-01..2027-06-30",
        &["report.log", "video.mkv", "apple.txt", "banana.md"],
    );
}

#[test]
fn ext_filter_supports_wildcards_and_lists() {
    let f = Fixture::new();

    f.expect(
        "ext:txt",
        &["tiny.txt", "notes.txt", "apple.txt", "apple banana.txt"],
    );
    f.expect(
        "ext:t*",
        &["tiny.txt", "notes.txt", "apple.txt", "apple banana.txt"],
    );
    f.expect(
        "ext:rs;md",
        &[
            "guide.md",
            "build.rs",
            "架构图.md",
            "banana.md",
            "orange.rs",
        ],
    );
    f.expect(
        "ext:?xt",
        &["tiny.txt", "notes.txt", "apple.txt", "apple banana.txt"],
    );
    f.expect(
        "ext:m*",
        &["guide.md", "video.mkv", "架构图.md", "banana.md"],
    );
    // Leading `.`, `*` and `*.` are ignored (SPEC QRY-04), so `*.md` is the same as `md`.
    f.expect("ext:*.md", &["guide.md", "架构图.md", "banana.md"]);
    f.expect("ext:.md", &["guide.md", "架构图.md", "banana.md"]);
    // `*` matches any extension; the extension-less directory stays out, proving the count
    // is 12 of 13 rather than the whole library.
    assert_eq!(f.names_for("ext:*").len(), 12);
    assert_eq!(f.names_for("ext:md").len(), 3);
}

#[test]
fn operators_and_grouping_survive_the_full_chain() {
    let f = Fixture::new();

    // SPEC QRY-02: OR binds tighter than AND, so this is `apple <banana|orange>`.
    f.expect("apple banana|orange", &["apple banana.txt"]);
    f.expect("apple <banana|orange>", &["apple banana.txt"]);
    f.expect("<apple|orange> ext:txt", &["apple.txt", "apple banana.txt"]);
    f.expect("!orange ext:rs", &["build.rs"]);
    f.expect("size:>1mb ext:md|ext:iso", &["archive.iso"]);
    // Everything inside quotes is literal, operators included.
    f.expect("\"apple banana\"", &["apple banana.txt"]);
    f.expect(
        "ext:txt file:",
        &["tiny.txt", "notes.txt", "apple.txt", "apple banana.txt"],
    );
    f.expect("ext:txt folder:", &[]);
    f.expect("folder:", &["Docs"]);
    f.expect(
        "file:",
        &[
            "tiny.txt",
            "notes.txt",
            "guide.md",
            "report.log",
            "video.mkv",
            "archive.iso",
            "build.rs",
            "架构图.md",
            "apple.txt",
            "apple banana.txt",
            "banana.md",
            "orange.rs",
        ],
    );
}

#[test]
fn unconstrained_items_match_nothing_instead_of_everything() {
    let f = Fixture::new();

    // A function whose value is empty or unparsable carries no constraint. Returning all
    // records for it would be the silent-degradation defect.
    for broken in ["size:", "size:notanumber", "ext:", "dm:", "dc:not-a-date"] {
        assert!(
            f.names_for(broken).is_empty(),
            "{broken} must match nothing, got {} of {TOTAL} records",
            f.names_for(broken).len()
        );
    }

    // A broken sibling must not erase its partners from the AND chain either.
    f.expect("apple size:", &[]);
    f.expect("size: ext:txt", &[]);
    // ... but it must not widen an OR chain either: the empty operand is unsatisfiable.
    f.expect("apple|size:", &["apple.txt", "apple banana.txt"]);
    f.expect("apple|", &["apple.txt", "apple banana.txt"]);

    // Documented consequence of "unsatisfiable": negating such an item is the complement of
    // the empty set, i.e. every record. This is deliberate, not a fallback.
    assert_eq!(f.names_for("!size:").len(), TOTAL);

    // Truly empty input keeps the "no query" behaviour: everything that is not a tombstone.
    assert_eq!(f.names_for("").len(), TOTAL);
    assert_eq!(f.names_for("   ").len(), TOTAL);

    // Deliberate boundary of the chosen semantics: a lone modifier is not a failed filter,
    // so `case:` stays vacuous (matching Everything) rather than becoming unsatisfiable.
    assert_eq!(f.names_for("case:").len(), TOTAL);
}

/// The match toggles are query-wide defaults, so every word they are sent with behaves the
/// same way. Each case below is chosen so that "the default only reached the first word"
/// gives a different, wrong answer, which is printed in each assertion comment below.
/// Verifies `case_sensitive`, `whole_word`, `match_path`, and `regex` defaults across multi-word queries.
#[test]
fn query_defaults_reach_every_word_of_the_query() {
    let f = Fixture::new();

    let case = QueryDefaults {
        case_sensitive: true,
        ..Default::default()
    };
    // First word matches, second word only matches case-insensitively: with the default on
    // both words are case-sensitive, so nothing survives the AND.
    // First-word-only would answer ["report.log"].
    f.expect_with("report LOG", &case, &[]);
    f.expect_with("report log", &case, &["report.log"]);
    // Same query, default off: unchanged, and it proves the difference above is the default.
    f.expect("report LOG", &["report.log"]);

    // A written `no-case:` still wins over the default for its own item, so this matches.
    f.expect_with("report no-case:LOG", &case, &["report.log"]);
    // ... and a written `case:` next to a default-off query behaves as before.
    f.expect("report case:LOG", &[]);

    let ww = QueryDefaults {
        whole_words: true,
        ..Default::default()
    };
    // `txt` is a whole word, `note` is not one anywhere: the second word must be whole-word
    // too. First-word-only would answer ["notes.txt"].
    f.expect_with("txt note", &ww, &[]);
    f.expect_with("txt notes", &ww, &["notes.txt"]);
    f.expect("txt note", &["notes.txt"]);

    let regex = QueryDefaults {
        is_regex: true,
        ..Default::default()
    };
    // Both operands are patterns; `archi.e` only matches as a regex. First-word-only would
    // answer ["report.log"].
    f.expect_with("rep.rt|archi.e", &regex, &["report.log", "archive.iso"]);
    // Same query with the default off: both words are literal substrings, so nothing matches.
    f.expect("rep.rt|archi.e", &[]);
}
