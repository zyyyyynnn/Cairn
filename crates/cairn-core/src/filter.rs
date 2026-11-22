// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Size and Date filter evaluation against file records.

use crate::model::{
    FLAG_ARCHIVE, FLAG_COMPRESSED, FLAG_DIRECTORY, FLAG_HIDDEN, FLAG_READONLY, FLAG_REPARSE_POINT,
    FLAG_SPARSE, FLAG_SYSTEM,
};
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
        Self::parse_with(s, parse_size_val)
    }

    /// Parses a comparison or range expression using `parse_val` for each scalar operand.
    pub fn parse_with(s: &str, parse_val: fn(&str) -> Option<u64>) -> Option<Self> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return None;
        }

        if let Some((left, right)) = trimmed.split_once("..") {
            let min = parse_val(left)?;
            let max = parse_val(right)?;
            return Some(Self {
                op: SizeOp::Range(min, max),
            });
        }
        if let Some((left, right)) = trimmed.split_once('-')
            && !left.is_empty()
            && !right.is_empty()
            && let (Some(min), Some(max)) = (parse_val(left), parse_val(right))
        {
            return Some(Self {
                op: SizeOp::Range(min, max),
            });
        }

        if let Some(rest) = trimmed.strip_prefix(">=") {
            Some(Self {
                op: SizeOp::Gte(parse_val(rest)?),
            })
        } else if let Some(rest) = trimmed.strip_prefix('>') {
            Some(Self {
                op: SizeOp::Gt(parse_val(rest)?),
            })
        } else if let Some(rest) = trimmed.strip_prefix("<=") {
            Some(Self {
                op: SizeOp::Lte(parse_val(rest)?),
            })
        } else if let Some(rest) = trimmed.strip_prefix('<') {
            Some(Self {
                op: SizeOp::Lt(parse_val(rest)?),
            })
        } else if let Some(rest) = trimmed.strip_prefix("!=") {
            Some(Self {
                op: SizeOp::Neq(parse_val(rest)?),
            })
        } else if let Some(rest) = trimmed.strip_prefix("==") {
            Some(Self {
                op: SizeOp::Eq(parse_val(rest)?),
            })
        } else if let Some(rest) = trimmed.strip_prefix('=') {
            Some(Self {
                op: SizeOp::Eq(parse_val(rest)?),
            })
        } else {
            Some(Self {
                op: SizeOp::Eq(parse_val(trimmed)?),
            })
        }
    }
}

/// Parses an unsigned integer comparison or range filter (`len:`, `depth:`, `childcount:`).
pub fn parse_u64_filter(s: &str) -> Option<SizeFilter> {
    SizeFilter::parse_with(s, parse_plain_u64)
}

fn parse_plain_u64(s: &str) -> Option<u64> {
    s.trim().parse::<u64>().ok()
}

/// Parses duration values (`length:`, `duration:`) into milliseconds.
pub fn parse_duration_filter(s: &str) -> Option<SizeFilter> {
    SizeFilter::parse_with(s, parse_duration_ms)
}

fn parse_duration_ms(s: &str) -> Option<u64> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.contains(':') {
        let parts: Vec<&str> = trimmed.split(':').collect();
        if !(2..=3).contains(&parts.len()) {
            return None;
        }
        let secs_f: f64 = parts.last()?.trim().parse().ok()?;
        let mut total_secs = secs_f;
        if parts.len() == 2 {
            let mins: f64 = parts[0].trim().parse().ok()?;
            total_secs += mins * 60.0;
        } else {
            let hrs: f64 = parts[0].trim().parse().ok()?;
            let mins: f64 = parts[1].trim().parse().ok()?;
            total_secs += hrs * 3600.0 + mins * 60.0;
        }
        if total_secs < 0.0 || !total_secs.is_finite() {
            return None;
        }
        return Some((total_secs * 1000.0).round() as u64);
    }

    let lower = trimmed.to_ascii_lowercase();
    if let Some(num) = lower.strip_suffix("ms") {
        let v: f64 = num.trim().parse().ok()?;
        return (v >= 0.0 && v.is_finite()).then_some(v.round() as u64);
    }

    // Support compound forms such as `1m30s` or `1h2m3s`.
    if lower.contains(['h', 'm', 's']) {
        let mut total_ms = 0.0f64;
        let mut num_start = 0usize;
        let bytes = lower.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            let ch = bytes[i];
            if matches!(ch, b'h' | b'm' | b's') {
                let num_slice = lower[num_start..i].trim();
                let val: f64 = num_slice.parse().ok()?;
                if val < 0.0 || !val.is_finite() {
                    return None;
                }
                let unit_ms = match ch {
                    b'h' => 3_600_000.0,
                    b'm' => 60_000.0,
                    b's' => 1_000.0,
                    _ => unreachable!(),
                };
                total_ms += val * unit_ms;
                i += 1;
                // Skip optional "hr"/"min"/"sec" suffix tails.
                while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                    i += 1;
                }
                num_start = i;
            } else {
                i += 1;
            }
        }
        if num_start == bytes.len() && total_ms.is_finite() {
            return Some(total_ms.round() as u64);
        }
        return None;
    }

    let bare_secs: f64 = lower.parse().ok()?;
    (bare_secs >= 0.0 && bare_secs.is_finite()).then_some((bare_secs * 1000.0).round() as u64)
}

/// Parses framerate filters (`framerate:`, `fps:`) scaled by 1000 (`milli-fps`).
pub fn parse_framerate_filter(s: &str) -> Option<SizeFilter> {
    SizeFilter::parse_with(s, parse_framerate_mfps)
}

fn parse_framerate_mfps(s: &str) -> Option<u64> {
    let lower = s.trim().to_ascii_lowercase();
    let num = lower.strip_suffix("fps").unwrap_or(&lower).trim();
    let fps: f64 = num.parse().ok()?;
    if fps < 0.0 || !fps.is_finite() {
        return None;
    }
    Some((fps * 1000.0).round() as u64)
}

/// Bitmask attribute matcher for `attrib:` queries (`R`, `H`, `S`, `D`, `A`, `C`, `P`, `L`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttribFilter {
    pub required_mask: u16,
    pub forbidden_mask: u16,
}

impl AttribFilter {
    pub fn parse(raw: &str) -> Option<Self> {
        let s = raw.trim();
        if s.is_empty() {
            return None;
        }
        let mut required_mask = 0u16;
        let mut forbidden_mask = 0u16;
        let mut negate_next = false;

        for ch in s.chars() {
            match ch {
                '-' | '!' => negate_next = true,
                '+' => negate_next = false,
                _ => {
                    let bit = match ch.to_ascii_uppercase() {
                        'R' => FLAG_READONLY,
                        'H' => FLAG_HIDDEN,
                        'S' => FLAG_SYSTEM,
                        'D' => FLAG_DIRECTORY,
                        'A' => FLAG_ARCHIVE,
                        'C' => FLAG_COMPRESSED,
                        'P' => FLAG_SPARSE,
                        'L' => FLAG_REPARSE_POINT,
                        _ => return None,
                    };
                    if negate_next {
                        forbidden_mask |= bit;
                    } else {
                        required_mask |= bit;
                    }
                }
            }
        }

        if required_mask == 0 && forbidden_mask == 0 {
            return None;
        }
        Some(Self {
            required_mask,
            forbidden_mask,
        })
    }

    #[inline]
    pub const fn matches(self, flags: u16) -> bool {
        (flags & self.required_mask) == self.required_mask && (flags & self.forbidden_mask) == 0
    }
}

/// Resolution filter for `dimensions:`, `width:`, and `height:`.
#[derive(Clone, Debug, PartialEq)]
pub struct DimensionsFilter {
    pub width: Option<SizeFilter>,
    pub height: Option<SizeFilter>,
}

impl DimensionsFilter {
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        match trimmed.to_ascii_lowercase().as_str() {
            "4k" | "uhd" => {
                return Some(Self {
                    width: Some(SizeFilter {
                        op: SizeOp::Gte(3840),
                    }),
                    height: Some(SizeFilter {
                        op: SizeOp::Gte(2160),
                    }),
                });
            }
            "1080p" | "fhd" => {
                return Some(Self {
                    width: Some(SizeFilter {
                        op: SizeOp::Eq(1920),
                    }),
                    height: Some(SizeFilter {
                        op: SizeOp::Eq(1080),
                    }),
                });
            }
            "720p" | "hd" => {
                return Some(Self {
                    width: Some(SizeFilter {
                        op: SizeOp::Eq(1280),
                    }),
                    height: Some(SizeFilter {
                        op: SizeOp::Eq(720),
                    }),
                });
            }
            _ => {}
        }

        let (op_prefix, body) = if let Some(rest) = trimmed.strip_prefix(">=") {
            (">=", rest)
        } else if let Some(rest) = trimmed.strip_prefix("<=") {
            ("<=", rest)
        } else if let Some(rest) = trimmed.strip_prefix("!=") {
            ("!=", rest)
        } else if let Some(rest) = trimmed.strip_prefix("==") {
            ("==", rest)
        } else if let Some(rest) = trimmed.strip_prefix('>') {
            (">", rest)
        } else if let Some(rest) = trimmed.strip_prefix('<') {
            ("<", rest)
        } else if let Some(rest) = trimmed.strip_prefix('=') {
            ("=", rest)
        } else {
            ("", trimmed)
        };

        let sep = ['x', 'X', '*', '×'];
        let (w_raw, h_raw) = body.split_once(sep)?;
        let w_trim = w_raw.trim();
        let h_trim = h_raw.trim();

        let width = if w_trim == "*" || w_trim.is_empty() {
            None
        } else {
            Some(parse_u64_filter(&format!("{op_prefix}{w_trim}"))?)
        };
        let height = if h_trim == "*" || h_trim.is_empty() {
            None
        } else {
            Some(parse_u64_filter(&format!("{op_prefix}{h_trim}"))?)
        };
        if width.is_none() && height.is_none() {
            return None;
        }
        Some(Self { width, height })
    }

    #[inline]
    pub fn matches(&self, w: u32, h: u32) -> bool {
        let w_ok = self.width.as_ref().is_none_or(|f| f.matches(w as u64));
        let h_ok = self.height.as_ref().is_none_or(|f| f.matches(h as u64));
        w_ok && h_ok
    }
}

/// Duplicate / uniqueness mode (`QRY-10`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DupeMode {
    Name,
    Size,
    NamePart,
    NameAndSize,
    UniqueName,
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
    /// Matches `ts < v`. The parser puts `v` at the span start for `<` and at the span end for
    /// `<=`, so the operator decides which edge of the written date is compared.
    Lt(i64),
    /// Matches `ts >= v`. The parser puts `v` at the span start for `>=` and at the span end for
    /// `>`, mirroring [`DateOp::Lt`].
    Gt(i64),
    /// Matches the half-open span `min <= ts < max` of an equality date.
    Range(i64, i64),
    /// Matches outside the half-open span.
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
            DateOp::Gt(v) => ts >= v,
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

        if let Some((left, right)) = trimmed.split_once('-')
            && !left.is_empty()
            && !right.is_empty()
            && let (Some((min, _)), Some((_, max))) = (
                parse_date_span(left, now_sec),
                parse_date_span(right, now_sec),
            )
        {
            return Some(Self {
                op: DateOp::Range(min, max),
            });
        }

        if let Some(rest) = trimmed.strip_prefix("!=") {
            let (start, end) = parse_date_span(rest, now_sec)?;
            Some(Self {
                op: DateOp::Neq(start, end),
            })
        } else if let Some(rest) = trimmed.strip_prefix(">=") {
            let (val, _) = parse_date_span(rest, now_sec)?;
            Some(Self {
                op: DateOp::Gt(val),
            })
        } else if let Some(rest) = trimmed.strip_prefix('>') {
            let (_, val) = parse_date_span(rest, now_sec)?;
            Some(Self {
                op: DateOp::Gt(val),
            })
        } else if let Some(rest) = trimmed.strip_prefix("<=") {
            let (_, val) = parse_date_span(rest, now_sec)?;
            Some(Self {
                op: DateOp::Lt(val),
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

    if let Some((m_str, y_str)) = s.split_once('/')
        && let (Ok(m), Ok(y)) = (m_str.trim().parse::<i64>(), y_str.trim().parse::<i64>())
        && (1..=12).contains(&m)
        && y > 0
    {
        let start = ymd_to_timestamp(y, m, 1);
        let end = if m == 12 {
            ymd_to_timestamp(y + 1, 1, 1)
        } else {
            ymd_to_timestamp(y, m + 1, 1)
        };
        return Some((start, end));
    }

    if s.len() == 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-' {
        let year: i64 = s[0..4].parse().ok()?;
        let month: i64 = s[5..7].parse().ok()?;
        let day: i64 = s[8..10].parse().ok()?;
        if (1..=12).contains(&month) && (1..=31).contains(&day) {
            let start = ymd_to_timestamp(year, month, day);
            return Some((start, start + 86400));
        }
    }

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

    if s.len() == 4
        && s.chars().all(|c| c.is_ascii_digit())
        && let Ok(year) = s.parse::<i64>()
    {
        let start = ymd_to_timestamp(year, 1, 1);
        let end = ymd_to_timestamp(year + 1, 1, 1);
        return Some((start, end));
    }

    if s.len() == 2
        && s.chars().all(|c| c.is_ascii_digit())
        && let Ok(yy) = s.parse::<i64>()
    {
        let year = if yy >= 70 { 1900 + yy } else { 2000 + yy };
        let start = ymd_to_timestamp(year, 1, 1);
        let end = ymd_to_timestamp(year + 1, 1, 1);
        return Some((start, end));
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

        assert!(DateFilter::parse("2years").is_some());
        assert!(DateFilter::parse("3months").is_some());
        assert!(DateFilter::parse("4weeks").is_some());
        assert!(DateFilter::parse("5days").is_some());
        assert!(DateFilter::parse("6hours").is_some());
        assert!(DateFilter::parse("10minutes").is_some());
        assert!(DateFilter::parse("30seconds").is_some());

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

        assert!(DateFilter::parse("january").is_some());
        assert!(DateFilter::parse("december").is_some());
        assert!(DateFilter::parse("monday").is_some());
        assert!(DateFilter::parse("friday").is_some());

        assert!(DateFilter::parse("mtd").is_some());
        assert!(DateFilter::parse("ytd").is_some());
        assert!(DateFilter::parse("qtd").is_some());

        let f_gte = DateFilter::parse(">=07/2026").unwrap();
        assert!(f_gte.matches(ymd_to_timestamp(2026, 7, 1)));
        assert!(f_gte.matches(ymd_to_timestamp(2026, 8, 1)));
        assert!(!f_gte.matches(ymd_to_timestamp(2026, 6, 30)));

        // > operator must strictly match after the entire date span
        let f_gt = DateFilter::parse(">2026").unwrap();
        assert!(!f_gt.matches(ymd_to_timestamp(2026, 5, 1)));
        assert!(!f_gt.matches(ymd_to_timestamp(2026, 12, 31)));
        assert!(f_gt.matches(ymd_to_timestamp(2027, 1, 1)));

        // `<` stops before the span and `<=` runs to its trailing boundary; the operator
        // decides which span edge the comparison uses.
        let f_lt = DateFilter::parse("<07/2026").unwrap();
        let f_lte = DateFilter::parse("<=07/2026").unwrap();
        let last_second = ymd_to_timestamp(2026, 8, 1) - 1;
        assert!(!f_lt.matches(ymd_to_timestamp(2026, 7, 1)));
        assert!(f_lt.matches(ymd_to_timestamp(2026, 6, 30)));
        assert!(!f_lt.matches(last_second));
        assert!(f_lte.matches(last_second));
        assert!(!f_lte.matches(ymd_to_timestamp(2026, 8, 1)));

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
