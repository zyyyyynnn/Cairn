// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Windows FILETIME, the timestamp unit the index stores (`$MFT` and the file APIs differ only in
//! how they hand it over).

use cairn_core::time::{FILETIME_TICKS_PER_SECOND, FILETIME_UNIX_EPOCH_TICKS};
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
            let intervals = elapsed
                .as_secs()
                .saturating_mul(FILETIME_TICKS_PER_SECOND)
                .saturating_add(u64::from(elapsed.subsec_nanos()) / 100)
                .saturating_add(FILETIME_UNIX_EPOCH_TICKS);
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
