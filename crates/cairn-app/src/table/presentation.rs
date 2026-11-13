// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Per-frame result presentation helpers: bounded minute-bucket local timestamp cache so virtual
//! row and status rendering avoid redundant Win32 FILETIME->SYSTEMTIME conversions and string
//! allocations.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};

use gpui_kit::gpui::SharedString;

const MAX_MINUTE_CACHE_ENTRIES: usize = 512;

/// Local wall-clock calendar breakdown for a timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LocalDateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
}

/// Port converting a UTC Unix timestamp (seconds) into local wall-clock calendar components.
pub(crate) trait LocalTimePort {
    fn to_local(&self, unix_seconds: i64) -> Option<LocalDateTime>;
}

/// Pure UTC calendar conversion used as cross-platform fallback and deterministic test seam.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct UtcLocalTimePort;

impl LocalTimePort for UtcLocalTimePort {
    fn to_local(&self, unix_seconds: i64) -> Option<LocalDateTime> {
        let (year, month, day) = civil_from_days(unix_seconds.div_euclid(86_400));
        let rem_sec = unix_seconds.rem_euclid(86_400);
        Some(LocalDateTime {
            year: u16::try_from(year).ok()?,
            month: month as u8,
            day: day as u8,
            hour: (rem_sec / 3_600) as u8,
            minute: ((rem_sec % 3_600) / 60) as u8,
        })
    }
}

/// Production Windows/system local time converter.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SystemLocalTimePort;

impl LocalTimePort for SystemLocalTimePort {
    fn to_local(&self, unix_seconds: i64) -> Option<LocalDateTime> {
        #[cfg(windows)]
        if let Some(local) = windows_local_datetime(unix_seconds) {
            return Some(local);
        }
        UtcLocalTimePort.to_local(unix_seconds)
    }
}

#[cfg(windows)]
fn windows_local_datetime(sec: i64) -> Option<LocalDateTime> {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
    use windows::Win32::System::Time::FileTimeToSystemTime;

    let ticks = (sec as i128 + 11_644_473_600).checked_mul(10_000_000)?;
    if !(0..=u64::MAX as i128).contains(&ticks) {
        return None;
    }
    let ticks = ticks as u64;
    let utc = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut local = FILETIME::default();
    // SAFETY: both pointers reference live FILETIME values; the API only writes the output.
    unsafe { FileTimeToLocalFileTime(&utc, &mut local) }.ok()?;
    let mut system = SYSTEMTIME::default();
    // SAFETY: the local FILETIME is initialized and the output points at a live SYSTEMTIME.
    unsafe { FileTimeToSystemTime(&local, &mut system) }.ok()?;
    Some(LocalDateTime {
        year: system.wYear,
        month: system.wMonth as u8,
        day: system.wDay as u8,
        hour: system.wHour as u8,
        minute: system.wMinute as u8,
    })
}

/// Howard Hinnant's civil-from-days conversion for Unix-epoch day counts.
fn civil_from_days(days_since_1970: i64) -> (i64, u32, u32) {
    let shifted = days_since_1970 + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_part + 2) / 5 + 1) as u32;
    let month = (if month_part < 10 {
        month_part + 3
    } else {
        month_part - 9
    }) as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Bounded minute-bucket timestamp formatter cache for per-frame row rendering.
pub(crate) struct RowPresentationCache<P: LocalTimePort> {
    port: P,
    capacity: usize,
    entries: HashMap<i64, SharedString>,
    order: VecDeque<i64>,
}

impl<P: LocalTimePort> RowPresentationCache<P> {
    pub fn with_capacity(port: P, capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            port,
            capacity,
            entries: HashMap::with_capacity(capacity.min(64)),
            order: VecDeque::with_capacity(capacity.min(64)),
        }
    }

    pub fn format_timestamp(&mut self, filetime_ts: i64) -> SharedString {
        if filetime_ts == 0 {
            return SharedString::default();
        }
        let unix_seconds = cairn_core::time::timestamp_to_unix_seconds(filetime_ts);
        let minute_bucket = unix_seconds.div_euclid(60);
        if let Some(cached) = self.entries.get(&minute_bucket) {
            return cached.clone();
        }
        let Some(local) = self.port.to_local(unix_seconds) else {
            return SharedString::default();
        };
        let formatted = SharedString::from(format!(
            "{:04}.{:02}.{:02} {:02}:{:02}",
            local.year, local.month, local.day, local.hour, local.minute
        ));
        while self.entries.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            } else {
                break;
            }
        }
        self.order.push_back(minute_bucket);
        self.entries.insert(minute_bucket, formatted.clone());
        formatted
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

thread_local! {
    static UI_ROW_PRESENTATION_CACHE: RefCell<RowPresentationCache<SystemLocalTimePort>> =
        RefCell::new(RowPresentationCache::with_capacity(
            SystemLocalTimePort,
            MAX_MINUTE_CACHE_ENTRIES,
        ));
}

/// Formats a Windows FILETIME timestamp using the UI thread's bounded minute-bucket cache.
pub(crate) fn format_timestamp_cached(ts: i64) -> SharedString {
    UI_ROW_PRESENTATION_CACHE.with(|cache| cache.borrow_mut().format_timestamp(ts))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    struct CountingUtcPort {
        calls: Rc<Cell<usize>>,
    }

    impl LocalTimePort for CountingUtcPort {
        fn to_local(&self, unix_seconds: i64) -> Option<LocalDateTime> {
            self.calls.set(self.calls.get() + 1);
            UtcLocalTimePort.to_local(unix_seconds)
        }
    }

    #[test]
    fn minute_bucket_cache_reuses_same_minute_across_frames_and_bounds_entries() {
        let calls = Rc::new(Cell::new(0));
        let mut cache = RowPresentationCache::with_capacity(
            CountingUtcPort {
                calls: Rc::clone(&calls),
            },
            2,
        );

        let ts_zero = cache.format_timestamp(0);
        assert_eq!(ts_zero.as_ref(), "");
        assert_eq!(calls.get(), 0);

        // Two distinct FILETIME values in the same UTC minute (1970.01.01 00:00).
        let base: i64 = 116_444_736_000_000_000;
        let first = cache.format_timestamp(base);
        let second = cache.format_timestamp(base + 59 * 10_000_000);
        assert_eq!(first.as_ref(), "1970.01.01 00:00");
        assert_eq!(second, first);
        assert_eq!(calls.get(), 1);

        // Next minute hits port once, then stays cached.
        let next_min = cache.format_timestamp(base + 60 * 10_000_000);
        assert_eq!(next_min.as_ref(), "1970.01.01 00:01");
        assert_eq!(calls.get(), 2);
        let _ = cache.format_timestamp(base + 60 * 10_000_000);
        assert_eq!(calls.get(), 2);

        // Third distinct minute evicts the oldest bucket because capacity is 2.
        let third_min = cache.format_timestamp(base + 120 * 10_000_000);
        assert_eq!(third_min.as_ref(), "1970.01.01 00:02");
        assert_eq!(cache.len(), 2);
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn utc_port_matches_known_filetime_calendar_dates() {
        let mut cache = RowPresentationCache::with_capacity(UtcLocalTimePort, 16);
        for (filetime, expected) in [
            (116_444_736_000_000_000, "1970.01.01 00:00"),
            (130_103_999_900_000_000, "2013.04.14 07:59"),
            (132_000_000_000_000_000, "2019.04.17 18:40"),
            (133_000_000_000_000_000, "2022.06.18 04:26"),
            (133_484_544_000_000_000, "2023.12.31 00:00"),
            (133_485_408_000_000_000, "2024.01.01 00:00"),
            (133_536_384_000_000_000, "2024.02.29 00:00"),
            (133_564_628_421_147_572, "2024.04.01 16:34"),
        ] {
            assert_eq!(cache.format_timestamp(filetime).as_ref(), expected);
        }
    }
}
