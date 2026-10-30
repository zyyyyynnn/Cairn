// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! How a size is written for a person to read.
//!
//! The same file is shown by the table, the status bar, the treemap and the command line, so
//! the wording of a size is one decision rather than one per surface.

/// Formats a raw byte count into a human-readable string with units (B, KB, MB, GB, TB).
pub fn format_size_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;
    let value = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if value < MB {
        format!("{:.1} KB", value / KB)
    } else if value < GB {
        format!("{:.1} MB", value / MB)
    } else if value < TB {
        format!("{:.2} GB", value / GB)
    } else {
        format!("{:.2} TB", value / TB)
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::format_size_bytes;

    #[test]
    fn sizes_format_with_binary_units() {
        assert_eq!(format_size_bytes(0), "0 B");
        assert_eq!(format_size_bytes(1023), "1023 B");
        assert_eq!(format_size_bytes(1536), "1.5 KB");
        assert_eq!(format_size_bytes(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_size_bytes(3 * 1024 * 1024 * 1024), "3.00 GB");
        assert_eq!(format_size_bytes(2 * 1024 * 1024 * 1024 * 1024), "2.00 TB");
    }
}
