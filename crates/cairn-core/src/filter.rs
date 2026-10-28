// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Size and Date filter evaluation against file records.

use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq)]
pub enum SizeOp {
    Lt(u64),
    Lte(u64),
    Gt(u64),
    Gte(u64),
    Eq(u64),
    Neq(u64),
    Range(u64, u64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SizeFilter {
    pub op: SizeOp,
}

impl SizeFilter {
    pub fn matches(&self, size: u64) -> bool {
        match self.op {
            SizeOp::Lt(v) => size < v,
            SizeOp::Lte(v) => size <= v,
            SizeOp::Gt(v) => size > v,
            SizeOp::Gte(v) => size >= v,
            SizeOp::Eq(v) => size == v,
            SizeOp::Neq(v) => size != v,
            SizeOp::Range(min, max) => size >= min && size <= max,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }

        // Check for range `a..b` or `a-b`
        if let Some((left, right)) = trimmed.split_once("..") {
            let min = parse_size_val(left)?;
            let max = parse_size_val(right)?;
            return Some(Self {
                op: SizeOp::Range(min, max),
            });
        }
        if let Some((left, right)) = trimmed.split_once('-') {
            if !left.is_empty() && !right.is_empty() {
                if let (Some(min), Some(max)) = (parse_size_val(left), parse_size_val(right)) {
                    return Some(Self {
                        op: SizeOp::Range(min, max),
                    });
                }
            }
        }

        if let Some(rest) = trimmed.strip_prefix(">=") {
            let val = parse_size_val(rest)?;
            Some(Self {
                op: SizeOp::Gte(val),
            })
        } else if let Some(rest) = trimmed.strip_prefix('>') {
            let val = parse_size_val(rest)?;
            Some(Self {
                op: SizeOp::Gt(val),
            })
        } else if let Some(rest) = trimmed.strip_prefix("<=") {
            let val = parse_size_val(rest)?;
            Some(Self {
                op: SizeOp::Lte(val),
            })
        } else if let Some(rest) = trimmed.strip_prefix('<') {
            let val = parse_size_val(rest)?;
            Some(Self {
                op: SizeOp::Lt(val),
            })
        } else if let Some(rest) = trimmed.strip_prefix("!=") {
            let val = parse_size_val(rest)?;
            Some(Self {
                op: SizeOp::Neq(val),
            })
        } else if let Some(rest) = trimmed.strip_prefix("==") {
            let val = parse_size_val(rest)?;
            Some(Self {
                op: SizeOp::Eq(val),
            })
        } else if let Some(rest) = trimmed.strip_prefix('=') {
            let val = parse_size_val(rest)?;
            Some(Self {
                op: SizeOp::Eq(val),
            })
        } else {
            let val = parse_size_val(trimmed)?;
            Some(Self {
                op: SizeOp::Eq(val),
            })
        }
    }
}

fn parse_size_val(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }

    let (num_part, multiplier) = if s.ends_with("tb") {
        (&s[..s.len() - 2], 1024u64 * 1024 * 1024 * 1024)
    } else if s.ends_with("gb") {
        (&s[..s.len() - 2], 1024u64 * 1024 * 1024)
    } else if s.ends_with("mb") {
        (&s[..s.len() - 2], 1024u64 * 1024)
    } else if s.ends_with("kb") {
        (&s[..s.len() - 2], 1024u64)
    } else if s.ends_with('t') {
        (&s[..s.len() - 1], 1024u64 * 1024 * 1024 * 1024)
    } else if s.ends_with('g') {
        (&s[..s.len() - 1], 1024u64 * 1024 * 1024)
    } else if s.ends_with('m') {
        (&s[..s.len() - 1], 1024u64 * 1024)
    } else if s.ends_with('k') {
        (&s[..s.len() - 1], 1024u64)
    } else if s.ends_with('b') {
        (&s[..s.len() - 1], 1u64)
    } else {
        (s.as_str(), 1u64)
    };

    let num: f64 = num_part.trim().parse().ok()?;
    Some((num * multiplier as f64) as u64)
}

#[derive(Clone, Debug, PartialEq)]
pub enum DateOp {
    Lt(i64),
    Lte(i64),
    Gt(i64),
    Gte(i64),
    Eq(i64),
    Range(i64, i64),
    Neq(i64, i64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct DateFilter {
    pub op: DateOp,
}

impl DateFilter {
    pub fn matches(&self, ts: i64) -> bool {
        let ts = crate::time::timestamp_to_unix_seconds(ts);
        match self.op {
            DateOp::Lt(v) => ts < v,
            DateOp::Lte(v) => ts < v,
            DateOp::Gt(v) => ts >= v,
            DateOp::Gte(v) => ts >= v,
            DateOp::Eq(v) => ts == v,
            DateOp::Range(min, max) => ts >= min && ts < max,
            DateOp::Neq(min, max) => ts < min || ts >= max,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }

        let now_sec = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        if let Some((left, right)) = trimmed.split_once("..") {
            let (min, _) = parse_date_span(left, now_sec)?;
            let (_, max) = parse_date_span(right, now_sec)?;
            return Some(Self {
                op: DateOp::Range(min, max),
            });
        }

        if let Some((left, right)) = trimmed.split_once('-') {
            if !left.is_empty() && !right.is_empty() {
                if let (Some((min, _)), Some((_, max))) = (
                    parse_date_span(left, now_sec),
                    parse_date_span(right, now_sec),
                ) {
                    return Some(Self {
                        op: DateOp::Range(min, max),
                    });
                }
            }
        }

        if let Some(rest) = trimmed.strip_prefix("!=") {
            let (start, end) = parse_date_span(rest, now_sec)?;
            Some(Self {
                op: DateOp::Neq(start, end),
            })
        } else if let Some(rest) = trimmed.strip_prefix(">=") {
            let (val, _) = parse_date_span(rest, now_sec)?;
            Some(Self {
                op: DateOp::Gte(val),
            })
        } else if let Some(rest) = trimmed.strip_prefix('>') {
            let (_, val) = parse_date_span(rest, now_sec)?;
            Some(Self {
                op: DateOp::Gt(val),
            })
        } else if let Some(rest) = trimmed.strip_prefix("<=") {
            let (_, val) = parse_date_span(rest, now_sec)?;
            Some(Self {
                op: DateOp::Lte(val),
            })
        } else if let Some(rest) = trimmed.strip_prefix('<') {
            let (val, _) = parse_date_span(rest, now_sec)?;
            Some(Self {
                op: DateOp::Lt(val),
            })
        } else if let Some(rest) = trimmed
            .strip_prefix("==")
            .or_else(|| trimmed.strip_prefix('='))
        {
            let (start, end) = parse_date_span(rest, now_sec)?;
            Some(Self {
                op: DateOp::Range(start, end),
            })
        } else {
            let (start, end) = parse_date_span(trimmed, now_sec)?;
            Some(Self {
                op: DateOp::Range(start, end),
            })
        }
    }
}

pub fn parse_date_point(s: &str, now_sec: i64) -> Option<i64> {
    parse_date_span(s, now_sec).map(|(start, _)| start)
}

pub fn parse_date_span(s: &str, now_sec: i64) -> Option<(i64, i64)> {
    let s = s.trim().to_ascii_lowercase();
    let cur_days = now_sec.div_euclid(86400);
    let start_of_today = cur_days * 86400;

    match s.as_str() {
        "today" => return Some((start_of_today, start_of_today + 86400)),
        "yesterday" => return Some((start_of_today - 86400, start_of_today)),
        "mtd" => {
            let (cur_y, cur_m, _) = days_to_ymd(cur_days);
            let start = ymd_to_timestamp(cur_y, cur_m, 1);
            return Some((start, start_of_today + 86400));
        }
        "ytd" => {
            let (cur_y, _, _) = days_to_ymd(cur_days);
            let start = ymd_to_timestamp(cur_y, 1, 1);
            return Some((start, start_of_today + 86400));
        }
        "qtd" => {
            let (cur_y, cur_m, _) = days_to_ymd(cur_days);
            let q_month = match cur_m {
                1..=3 => 1,
                4..=6 => 4,
                7..=9 => 7,
                _ => 10,
            };
            let start = ymd_to_timestamp(cur_y, q_month, 1);
            return Some((start, start_of_today + 86400));
        }
        _ => {}
    }

    // 1. Month names (e.g. january, feb)
    let month_idx = match s.as_str() {
        "january" | "jan" => Some(1),
        "february" | "feb" => Some(2),
        "march" | "mar" => Some(3),
        "april" | "apr" => Some(4),
        "may" => Some(5),
        "june" | "jun" => Some(6),
        "july" | "jul" => Some(7),
        "august" | "aug" => Some(8),
        "september" | "sep" | "sept" => Some(9),
        "october" | "oct" => Some(10),
        "november" | "nov" => Some(11),
        "december" | "dec" => Some(12),
        _ => None,
    };
    if let Some(m) = month_idx {
        let (cur_y, _, _) = days_to_ymd(cur_days);
        let start = ymd_to_timestamp(cur_y, m, 1);
        let end = if m == 12 {
            ymd_to_timestamp(cur_y + 1, 1, 1)
        } else {
            ymd_to_timestamp(cur_y, m + 1, 1)
        };
        return Some((start, end));
    }

    // 2. Weekday names (e.g. monday, fri)
    let weekday_idx = match s.as_str() {
        "monday" | "mon" => Some(1),
        "tuesday" | "tue" => Some(2),
        "wednesday" | "wed" => Some(3),
        "thursday" | "thu" => Some(4),
        "friday" | "fri" => Some(5),
        "saturday" | "sat" => Some(6),
        "sunday" | "sun" => Some(7),
        _ => None,
    };
    if let Some(target_wd) = weekday_idx {
        // 1970-01-01 was Thursday (wd=4)
        let cur_wd = (cur_days + 3).rem_euclid(7) + 1; // 1 = Monday
        let diff = target_wd - cur_wd;
        let start = start_of_today + diff * 86400;
        return Some((start, start + 86400));
    }

    // 3. Relative durations: e.g. 2years, 3months, 4weeks, 5days, 6hours, 7minutes, 8seconds
    if let Some(n_str) = s.strip_suffix("years").or_else(|| s.strip_suffix("year")) {
        let n: i64 = n_str.trim().parse().ok()?;
        return Some((now_sec - n * 365 * 86400, now_sec));
    }
    if let Some(n_str) = s.strip_suffix("months").or_else(|| s.strip_suffix("month")) {
        let n: i64 = n_str.trim().parse().ok()?;
        return Some((now_sec - n * 30 * 86400, now_sec));
    }
    if let Some(n_str) = s.strip_suffix("weeks").or_else(|| s.strip_suffix("week")) {
        let n: i64 = n_str.trim().parse().ok()?;
        return Some((now_sec - n * 7 * 86400, now_sec));
    }
    if let Some(n_str) = s.strip_suffix("days").or_else(|| s.strip_suffix("day")) {
        let n: i64 = n_str.trim().parse().ok()?;
        return Some((now_sec - n * 86400, now_sec));
    }
    if let Some(n_str) = s.strip_suffix("hours").or_else(|| s.strip_suffix("hour")) {
        let n: i64 = n_str.trim().parse().ok()?;
        return Some((now_sec - n * 3600, now_sec));
    }
    if let Some(n_str) = s
        .strip_suffix("minutes")
        .or_else(|| s.strip_suffix("minute"))
        .or_else(|| s.strip_suffix("mins"))
        .or_else(|| s.strip_suffix("min"))
    {
        let n: i64 = n_str.trim().parse().ok()?;
        return Some((now_sec - n * 60, now_sec));
    }
    if let Some(n_str) = s
        .strip_suffix("seconds")
        .or_else(|| s.strip_suffix("second"))
        .or_else(|| s.strip_suffix("secs"))
        .or_else(|| s.strip_suffix("sec"))
    {
        let n: i64 = n_str.trim().parse().ok()?;
        return Some((now_sec - n, now_sec));
    }

    // 4. MM/YYYY format (e.g. 07/2026 or 7/2026)
    if let Some((m_str, y_str)) = s.split_once('/') {
        if let (Ok(m), Ok(y)) = (m_str.trim().parse::<i64>(), y_str.trim().parse::<i64>()) {
            if (1..=12).contains(&m) && y > 0 {
                let start = ymd_to_timestamp(y, m, 1);
                let end = if m == 12 {
                    ymd_to_timestamp(y + 1, 1, 1)
                } else {
                    ymd_to_timestamp(y, m + 1, 1)
                };
                return Some((start, end));
            }
        }
    }

    // 5. YYYY-MM-DD
    if s.len() == 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-' {
        let year: i64 = s[0..4].parse().ok()?;
        let month: i64 = s[5..7].parse().ok()?;
        let day: i64 = s[8..10].parse().ok()?;
        if (1..=12).contains(&month) && (1..=31).contains(&day) {
            let start = ymd_to_timestamp(year, month, day);
            return Some((start, start + 86400));
        }
    }

    // 6. YYMMDD (6 digits, e.g. 260702 -> 2026-07-02, 990702 -> 1999-07-02)
    if s.len() == 6 && s.chars().all(|c| c.is_ascii_digit()) {
        let yy: i64 = s[0..2].parse().ok()?;
        let month: i64 = s[2..4].parse().ok()?;
        let day: i64 = s[4..6].parse().ok()?;
        if (1..=12).contains(&month) && (1..=31).contains(&day) {
            let year = if yy >= 70 { 1900 + yy } else { 2000 + yy };
            let start = ymd_to_timestamp(year, month, day);
            return Some((start, start + 86400));
        }
    }

    // 7. YYYY (4 digits, e.g. 2026)
    if s.len() == 4 && s.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(year) = s.parse::<i64>() {
            let start = ymd_to_timestamp(year, 1, 1);
            let end = ymd_to_timestamp(year + 1, 1, 1);
            return Some((start, end));
        }
    }

    // 8. YY (2 digits, e.g. 26 -> 2026, 99 -> 1999)
    if s.len() == 2 && s.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(yy) = s.parse::<i64>() {
            let year = if yy >= 70 { 1900 + yy } else { 2000 + yy };
            let start = ymd_to_timestamp(year, 1, 1);
            let end = ymd_to_timestamp(year + 1, 1, 1);
            return Some((start, end));
        }
    }

    None
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

fn days_to_ymd(mut days: i64) -> (i64, i64, i64) {
    let mut year = 1970;
    loop {
        let leap = is_leap_year(year);
        let days_in_year = if leap { 366 } else { 365 };
        if days >= days_in_year {
            days -= days_in_year;
            year += 1;
        } else if days < 0 {
            year -= 1;
            let leap_prev = is_leap_year(year);
            days += if leap_prev { 366 } else { 365 };
        } else {
            break;
        }
    }
    let leap = is_leap_year(year);
    let days_in_months = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1;
    for &dim in &days_in_months {
        if days >= dim {
            days -= dim;
            month += 1;
        } else {
            break;
        }
    }
    let day = days + 1;
    (year, month, day)
}

fn ymd_to_timestamp(year: i64, month: i64, day: i64) -> i64 {
    let mut total_days =
        (year - 1970) * 365 + (year - 1969) / 4 - (year - 1901) / 100 + (year - 1601) / 400;
    let days_in_month = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let is_leap = is_leap_year(year);

    for m in 1..month {
        total_days += days_in_month[(m - 1) as usize];
        if m == 2 && is_leap {
            total_days += 1;
        }
    }
    total_days += day - 1;
    total_days * 86400
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_size_filter_parsing() {
        let f1 = SizeFilter::parse(">10mb").unwrap();
        assert!(f1.matches(11 * 1024 * 1024));
        assert!(!f1.matches(9 * 1024 * 1024));

        let f2 = SizeFilter::parse("1kb..10kb").unwrap();
        assert!(f2.matches(5 * 1024));
        assert!(!f2.matches(15 * 1024));
    }

    #[test]
    fn test_date_filter_parsing() {
        let f = DateFilter::parse("today");
        assert!(f.is_some());

        // Relative durations
        assert!(DateFilter::parse("2years").is_some());
        assert!(DateFilter::parse("3months").is_some());
        assert!(DateFilter::parse("4weeks").is_some());
        assert!(DateFilter::parse("5days").is_some());
        assert!(DateFilter::parse("6hours").is_some());
        assert!(DateFilter::parse("10minutes").is_some());
        assert!(DateFilter::parse("30seconds").is_some());

        // Date formats
        let f_my = DateFilter::parse("07/2026").unwrap();
        let ts_july = ymd_to_timestamp(2026, 7, 15);
        let ts_aug = ymd_to_timestamp(2026, 8, 15);
        assert!(f_my.matches(ts_july));
        assert!(!f_my.matches(ts_aug));

        let f_yymmdd = DateFilter::parse("260702").unwrap();
        let ts_target = ymd_to_timestamp(2026, 7, 2) + 3600;
        let ts_other = ymd_to_timestamp(2026, 7, 3) + 3600;
        assert!(f_yymmdd.matches(ts_target));
        assert!(!f_yymmdd.matches(ts_other));

        // Month and Weekday constants
        assert!(DateFilter::parse("january").is_some());
        assert!(DateFilter::parse("december").is_some());
        assert!(DateFilter::parse("monday").is_some());
        assert!(DateFilter::parse("friday").is_some());

        // mtd, ytd, qtd
        assert!(DateFilter::parse("mtd").is_some());
        assert!(DateFilter::parse("ytd").is_some());
        assert!(DateFilter::parse("qtd").is_some());

        // Operators
        let f_gte = DateFilter::parse(">=07/2026").unwrap();
        assert!(f_gte.matches(ymd_to_timestamp(2026, 7, 1)));
        assert!(f_gte.matches(ymd_to_timestamp(2026, 8, 1)));
        assert!(!f_gte.matches(ymd_to_timestamp(2026, 6, 30)));

        // > operator must strictly match after the entire date span
        let f_gt = DateFilter::parse(">2026").unwrap();
        assert!(!f_gt.matches(ymd_to_timestamp(2026, 5, 1)));
        assert!(!f_gt.matches(ymd_to_timestamp(2026, 12, 31)));
        assert!(f_gt.matches(ymd_to_timestamp(2027, 1, 1)));

        // != operator matches timestamps outside the span
        let f_neq = DateFilter::parse("!=07/2026").unwrap();
        assert!(f_neq.matches(ymd_to_timestamp(2026, 6, 30)));
        assert!(!f_neq.matches(ymd_to_timestamp(2026, 7, 15)));
        assert!(f_neq.matches(ymd_to_timestamp(2026, 8, 1)));

        // Range boundary: July 2026 must not include August 1 00:00:00
        let f_month_exact = DateFilter::parse("07/2026").unwrap();
        assert!(f_month_exact.matches(ymd_to_timestamp(2026, 7, 31) + 86399));
        assert!(!f_month_exact.matches(ymd_to_timestamp(2026, 8, 1)));

        // a-b range syntax
        let f_range_dash = DateFilter::parse("2025-2026").unwrap();
        assert!(f_range_dash.matches(ymd_to_timestamp(2025, 6, 1)));
        assert!(f_range_dash.matches(ymd_to_timestamp(2026, 6, 1)));
        assert!(!f_range_dash.matches(ymd_to_timestamp(2027, 1, 1)));

        // 2-digit year pivot: 99 -> 1999, 26 -> 2026
        let (s99, _) = parse_date_span("990702", 0).unwrap();
        assert_eq!(s99, ymd_to_timestamp(1999, 7, 2));

        // Windows FILETIME conversion in matches
        let unix_sec = ymd_to_timestamp(2025, 7, 1) + 3600;
        let ft = unix_sec * 10_000_000 + 116444736000000000;
        let f_filetime = DateFilter::parse("2025-07-01").unwrap();
        assert!(f_filetime.matches(unix_sec));
        assert!(f_filetime.matches(ft));
    }
}
