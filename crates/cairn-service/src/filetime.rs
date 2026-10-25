// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Windows FILETIME, the timestamp unit the index stores (`$MFT` and the file APIs differ only in
//! how they hand it over).

use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

/// Converts a filesystem timestamp to 100 ns intervals since 1601. A clock earlier than the Unix
/// epoch (or an unavailable timestamp) yields `0`, which the UI renders as an empty cell.
pub fn system_time_to_filetime(time: io::Result<SystemTime>) -> i64 {
    let Ok(time) = time else {
        return 0;
    };
    match time.duration_since(UNIX_EPOCH) {
        Ok(elapsed) => {
            const EPOCH_OFFSET_SECS: u64 = 11_644_473_600;
            let intervals = elapsed
                .as_secs()
                .saturating_add(EPOCH_OFFSET_SECS)
                .saturating_mul(10_000_000)
                .saturating_add(u64::from(elapsed.subsec_nanos()) / 100);
            i64::try_from(intervals).unwrap_or(i64::MAX)
        }
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_known_instant_becomes_its_filetime() {
        // 2026-10-20T00:00:00Z, computed independently of this function.
        let time = UNIX_EPOCH + Duration::from_secs(1_792_454_400);
        assert_eq!(system_time_to_filetime(Ok(time)), 134_369_280_000_000_000);
    }

    #[test]
    fn an_absent_or_pre_epoch_time_is_zero() {
        assert_eq!(
            system_time_to_filetime(Err(io::Error::from(io::ErrorKind::NotFound))),
            0
        );
        assert_eq!(
            system_time_to_filetime(Ok(UNIX_EPOCH - Duration::from_secs(1))),
            0
        );
    }
}
