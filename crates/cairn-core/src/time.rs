// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! How a stored timestamp becomes a Unix second.
//!
//! A file time reaches the index in two shapes: Unix seconds, or a Windows FILETIME counting
//! 100-ns intervals since 1601-01-01. The index, the filter and the table all have to read the
//! same value, so the epoch offset and the tick rate live here rather than in each of them.

/// The 100 ns ticks between the FILETIME epoch (1601-01-01) and the Unix epoch (1970-01-01).
pub const FILETIME_UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;

/// The 100 ns ticks in one second.
pub const FILETIME_TICKS_PER_SECOND: u64 = 10_000_000;

/// A stored timestamp above this many seconds is a Windows FILETIME, not a Unix second count.
/// Ten billion seconds is the year 2286, so every Unix second count stays below it.
const UNIX_SECONDS_CEILING: i64 = 10_000_000_000;

/// Converts a stored timestamp to Unix seconds. A timestamp may carry either Unix seconds or a
/// Windows FILETIME (100-ns intervals since Jan 1, 1601).
pub fn timestamp_to_unix_seconds(ts: i64) -> i64 {
    if ts > UNIX_SECONDS_CEILING {
        // Both constants fit an i64 exactly, so the narrowing is lossless.
        (ts - FILETIME_UNIX_EPOCH_TICKS as i64) / FILETIME_TICKS_PER_SECOND as i64
    } else {
        ts
    }
}

/// Converts Windows FILETIME ticks to the Unix seconds a timestamp carries. A tick count before
/// the Unix epoch names no second the calendar can show, so it reads as the epoch itself.
pub fn filetime_ticks_to_unix_seconds(ticks: u64) -> i64 {
    let delta = ticks.saturating_sub(FILETIME_UNIX_EPOCH_TICKS);
    i64::try_from(delta / FILETIME_TICKS_PER_SECOND).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::{filetime_ticks_to_unix_seconds, timestamp_to_unix_seconds};

    #[test]
    fn a_unix_second_count_stays_as_it_is() {
        assert_eq!(timestamp_to_unix_seconds(0), 0);
        assert_eq!(timestamp_to_unix_seconds(1_700_000_000), 1_700_000_000);
    }

    #[test]
    fn a_filetime_becomes_the_unix_second_it_names() {
        assert_eq!(timestamp_to_unix_seconds(116_444_736_000_000_000), 0);
        assert_eq!(
            timestamp_to_unix_seconds(116_444_736_010_000_000),
            1,
            "one second past the epoch"
        );
        assert_eq!(
            timestamp_to_unix_seconds(1_700_000_000 * 10_000_000 + 116_444_736_000_000_000),
            1_700_000_000
        );
    }

    #[test]
    fn a_tick_count_before_the_epoch_reads_as_the_epoch() {
        assert_eq!(filetime_ticks_to_unix_seconds(0), 0);
        assert_eq!(filetime_ticks_to_unix_seconds(116_444_736_000_000_000), 0);
        assert_eq!(filetime_ticks_to_unix_seconds(116_444_736_010_000_000), 1);
    }
}
