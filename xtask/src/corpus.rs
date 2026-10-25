// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Synthetic file corpus generator for stress testing and benchmarks.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

pub fn generate_synthetic_corpus<P: AsRef<Path>>(
    path: P,
    total_count: usize,
) -> Result<(), std::io::Error> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    let prefixes = [
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
    let exts = [
        "rs", "txt", "md", "docx", "xlsx", "pptx", "pdf", "exe", "dll", "log", "toml", "json",
    ];

    for i in 0..total_count {
        let prefix = prefixes[i % prefixes.len()];
        let ext = exts[i % exts.len()];
        let name = format!("{}_{:07}.{}", prefix, i, ext);
        let size = (i as u64 * 1024) % (10 * 1024 * 1024);
        let parent = match i % 5 {
            0 => r"C:\Windows\System32",
            1 => r"C:\Users\User\Documents",
            2 => r"C:\Workspace\Projects",
            3 => r"C:\Program Files\Common",
            _ => r"C:\Data\Archives",
        };

        writeln!(
            writer,
            "{}\t{}\t{}\t{}",
            name,
            parent,
            size,
            1600000000 + (i as i64)
        )?;
    }

    writer.flush()?;
    Ok(())
}
