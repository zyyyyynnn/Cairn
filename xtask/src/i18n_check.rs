// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Static i18n validator ensuring 100% key parity between zh-CN and en-US locales.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

pub fn check_i18n_parity<P: AsRef<Path>>(locales_dir: P) -> Result<(), String> {
    let dir = locales_dir.as_ref();
    let zh_path = dir.join("zh-CN.ftl");
    let en_path = dir.join("en-US.ftl");

    if !zh_path.exists() || !en_path.exists() {
        return Err(format!(
            "Missing locale files in {:?}: zh-CN.ftl or en-US.ftl not found",
            dir
        ));
    }

    let zh_keys = extract_keys(&fs::read_to_string(&zh_path).map_err(|e| e.to_string())?);
    let en_keys = extract_keys(&fs::read_to_string(&en_path).map_err(|e| e.to_string())?);

    let missing_in_en: Vec<_> = zh_keys.difference(&en_keys).collect();
    let missing_in_zh: Vec<_> = en_keys.difference(&zh_keys).collect();

    if !missing_in_en.is_empty() || !missing_in_zh.is_empty() {
        return Err(format!(
            "Locale parity check failed!\nKeys in zh-CN but missing in en-US: {:?}\nKeys in en-US but missing in zh-CN: {:?}",
            missing_in_en, missing_in_zh
        ));
    }

    println!(
        "i18n check passed: {} keys perfectly aligned between zh-CN and en-US.",
        zh_keys.len()
    );
    Ok(())
}

fn extract_keys(content: &str) -> HashSet<String> {
    let mut keys = HashSet::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if let Some((key, _)) = trimmed.split_once('=') {
            keys.insert(key.trim().to_string());
        }
    }
    keys
}
