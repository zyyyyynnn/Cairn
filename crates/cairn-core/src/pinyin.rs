// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Mandarin Pinyin lookup and bounded substring matching for CJK filenames.

use crate::pinyin_data::{CJK_SYLLABLE_INDEX, SYLLABLES};
use crate::text_match::{
    TextMatchOptions, is_combining_mark, is_punctuation_char, normalize_for_match,
    strip_diacritics_char,
};

const CJK_START: u32 = 0x4E00;
const CJK_END: u32 = 0x9FFF;

/// Returns the primary Pinyin syllable for a Unicode CJK Unified Ideograph.
#[inline]
pub fn primary_syllable(ch: char) -> Option<&'static str> {
    let code = ch as u32;
    if !(CJK_START..=CJK_END).contains(&code) {
        return None;
    }
    let idx = CJK_SYLLABLE_INDEX[(code - CJK_START) as usize] as usize;
    SYLLABLES.get(idx).copied()
}

/// Alternate readings for common polyphonic characters beyond their primary collation entry.
#[inline]
fn polyphone_readings(ch: char) -> &'static [&'static str] {
    match ch {
        '了' => &["le", "liao"],
        '乐' | '樂' => &["le", "yue"],
        '参' | '參' => &["can", "cen", "shen"],
        '长' | '長' => &["chang", "zhang"],
        '重' => &["zhong", "chong"],
        '行' => &["xing", "hang"],
        '得' => &["de", "dei"],
        '阿' => &["a", "e"],
        '佛' => &["fo", "fu"],
        '都' => &["dou", "du"],
        '读' | '讀' => &["du", "dou"],
        '度' => &["du", "duo"],
        '还' | '還' => &["hai", "huan"],
        '和' => &["he", "huo", "hu"],
        '会' | '會' => &["hui", "kuai"],
        '系' => &["xi", "ji"],
        '解' => &["jie", "xie"],
        '校' => &["xiao", "jiao"],
        '觉' | '覺' => &["jue", "jiao"],
        '卡' => &["ka", "qia"],
        '落' => &["luo", "la"],
        '没' => &["mei", "mo"],
        '模' => &["mo", "mu"],
        '朴' => &["pu", "piao"],
        '区' | '區' => &["qu", "ou"],
        '圈' => &["quan", "juan"],
        '省' => &["sheng", "xing"],
        '盛' => &["sheng", "cheng"],
        '石' => &["shi", "dan"],
        '识' | '識' => &["shi", "zhi"],
        '似' => &["si", "shi"],
        '宿' => &["su", "xiu"],
        '说' | '說' => &["shuo", "shui", "yue"],
        '提' => &["ti", "di"],
        '降' => &["jiang", "xiang"],
        '角' => &["jiao", "jue"],
        '着' | '著' => &["zhao", "zhe", "zhuo"],
        '折' => &["zhe", "she"],
        '露' => &["lu", "lou"],
        '薄' => &["bo", "bao"],
        '差' => &["cha", "chai", "ci"],
        '朝' => &["chao", "zhao"],
        '称' | '稱' => &["cheng", "chen"],
        '乘' => &["cheng", "sheng"],
        '臭' => &["chou", "xiu"],
        '大' => &["da", "dai"],
        '地' => &["di", "de"],
        '便' => &["bian", "pian"],
        '强' | '強' => &["qiang", "jiang"],
        '什' => &["shen", "shi"],
        '数' | '數' => &["shu", "shuo"],
        '熟' => &["shu", "shou"],
        '弹' | '彈' => &["tan", "dan"],
        '血' => &["xue", "xie"],
        '叶' | '葉' => &["ye", "xie"],
        '殷' => &["yin", "yan"],
        '扎' => &["zha", "za"],
        '属' | '屬' => &["shu", "zhu"],
        '龟' | '龜' => &["gui", "jun"],
        '给' | '給' => &["gei", "ji"],
        '奇' => &["qi", "ji"],
        '仇' => &["chou", "qiu"],
        '盖' | '蓋' => &["gai", "ge"],
        '查' => &["cha", "zha"],
        '那' => &["na", "ne"],
        '哪' => &["na", "ne", "nei"],
        '秘' => &["mi", "bi"],
        '贾' | '賈' => &["jia", "gu"],
        '沈' => &["shen", "chen"],
        '俞' => &["yu", "shu"],
        '翟' => &["zhai", "di"],
        '曾' => &["zeng", "ceng"],
        '单' | '單' => &["dan", "shan", "chan"],
        '调' | '調' => &["diao", "tiao"],
        '传' | '傳' => &["chuan", "zhuan"],
        '藏' => &["cang", "zang"],
        '率' => &["lv", "shuai"],
        _ => &[],
    }
}

/// Alternate `v`/`u` spelling accepted by Chinese IME conventions for `ü` finals.
#[inline]
fn umlaut_alias(syl: &str) -> Option<&'static str> {
    match syl {
        "lv" => Some("lu"),
        "nv" => Some("nu"),
        "lue" => Some("lve"),
        "nue" => Some("nve"),
        "lve" => Some("lue"),
        "nve" => Some("nue"),
        _ => None,
    }
}

/// Returns `true` if `haystack` contains a contiguous substring matching `query` when CJK
/// characters expand to their Pinyin full syllables or initials (case-insensitive by default).
pub fn pinyin_matches(haystack: &str, query: &str) -> bool {
    pinyin_matches_with_options(haystack, query, TextMatchOptions::default())
}

/// Variant of [`pinyin_matches`] honoring punctuation, whitespace, diacritics, and case options.
pub fn pinyin_matches_with_options(haystack: &str, query: &str, opts: TextMatchOptions) -> bool {
    let q_norm = normalize_for_match(query, opts);
    if q_norm.is_empty() {
        return true;
    }
    let m = q_norm.len();
    let q_bytes = q_norm.as_bytes();

    let mut stack_curr = [false; 128];
    let mut stack_next = [false; 128];
    let mut heap_curr;
    let mut heap_next;
    let (curr, next): (&mut [bool], &mut [bool]) = if m < 128 {
        (&mut stack_curr[..=m], &mut stack_next[..=m])
    } else {
        heap_curr = vec![false; m + 1];
        heap_next = vec![false; m + 1];
        (&mut heap_curr[..=m], &mut heap_next[..=m])
    };

    for ch in haystack.chars() {
        if opts.ignore_diacritics && is_combining_mark(ch) {
            continue;
        }
        let mapped = if opts.ignore_diacritics {
            strip_diacritics_char(ch)
        } else {
            ch
        };
        if opts.ignore_whitespace && mapped.is_whitespace() {
            continue;
        }
        if opts.ignore_punctuation && is_punctuation_char(mapped) {
            continue;
        }

        let mut lit_buf = [0u8; 16];
        let lit_len = encode_char_for_match(mapped, opts.case_sensitive, &mut lit_buf);
        let lit = &lit_buf[..lit_len];

        curr[0] = true;
        next.fill(false);

        let primary = primary_syllable(mapped);
        let extras = polyphone_readings(mapped);

        for (q_pos, active) in curr[..m].iter().enumerate() {
            if !*active {
                continue;
            }
            let rem = &q_bytes[q_pos..];
            if rem.starts_with(lit) {
                let npos = q_pos + lit.len();
                if npos == m {
                    return true;
                }
                next[npos] = true;
            }

            if let Some(p) = primary {
                if advance_syllable(p, rem, q_pos, next) {
                    return true;
                }
                if let Some(alias) = umlaut_alias(p)
                    && advance_syllable(alias, rem, q_pos, next)
                {
                    return true;
                }
            }
            for &extra in extras {
                if Some(extra) != primary {
                    if advance_syllable(extra, rem, q_pos, next) {
                        return true;
                    }
                    if let Some(alias) = umlaut_alias(extra)
                        && advance_syllable(alias, rem, q_pos, next)
                    {
                        return true;
                    }
                }
            }
        }

        curr.copy_from_slice(next);
    }

    false
}

#[inline]
fn encode_char_for_match(ch: char, case_sensitive: bool, out: &mut [u8; 16]) -> usize {
    if case_sensitive {
        return ch.encode_utf8(out).len();
    }
    let mut len = 0;
    for lower in ch.to_lowercase() {
        let n = lower.encode_utf8(&mut out[len..]).len();
        len += n;
    }
    len
}

#[inline]
fn advance_syllable(syl: &str, rem: &[u8], q_pos: usize, next: &mut [bool]) -> bool {
    let sb = syl.as_bytes();
    if rem.len() <= sb.len() && sb.starts_with(rem) {
        return true;
    }
    if rem.starts_with(sb) {
        next[q_pos + sb.len()] = true;
    }
    if rem.first() == Some(&sb[0]) {
        next[q_pos + 1] = true;
    }
    if sb.len() >= 2
        && matches!(&sb[..2], b"zh" | b"ch" | b"sh")
        && rem.len() > 2
        && rem[..2] == sb[..2]
    {
        next[q_pos + 2] = true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_initial_polyphone_and_mixed_pinyin_matching() {
        assert!(pinyin_matches("文档.txt", "wendang"));
        assert!(pinyin_matches("文档.txt", "wd"));
        assert!(pinyin_matches("設計文檔.docx", "shejiwendang"));
        assert!(pinyin_matches("檔案系統.pdf", "daxt"));
        assert!(pinyin_matches("测试报告.md", "csbg"));
        assert!(pinyin_matches("测试2026报告.md", "cs2026"));
        assert!(pinyin_matches("中国.txt", "zg"));
        assert!(pinyin_matches("中国.txt", "zhg"));
        assert!(pinyin_matches("音乐.mp3", "yinyue"));
        assert!(pinyin_matches("快乐.txt", "kuaile"));
        assert!(pinyin_matches("绿茶.txt", "lvcha"));
        assert!(pinyin_matches("绿茶.txt", "lucha"));
        assert!(pinyin_matches("略微.txt", "luewei"));
        assert!(pinyin_matches("略微.txt", "lvewei"));
        assert!(!pinyin_matches("文档报告.txt", "wenb"));
    }
}
