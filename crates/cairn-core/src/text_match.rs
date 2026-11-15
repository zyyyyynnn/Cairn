// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Text folding, diacritic stripping, and configurable matching primitives.

/// Options governing character normalization during text comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextMatchOptions {
    pub case_sensitive: bool,
    pub ignore_punctuation: bool,
    pub ignore_whitespace: bool,
    pub ignore_diacritics: bool,
}

impl TextMatchOptions {
    /// Returns `true` when normalization transforms are needed beyond plain case-folded matching.
    #[inline]
    pub const fn needs_custom_normalization(self) -> bool {
        self.ignore_punctuation || self.ignore_whitespace || self.ignore_diacritics
    }
}

/// Appends the case-folded form of `text`. A character whose lowercase form changes the UTF-8
/// byte length keeps its original bytes, so arena offsets remain aligned between original and folded copies.
pub fn fold_append(text: &str, folded: &mut Vec<u8>) {
    if text.is_ascii() {
        let start = folded.len();
        folded.extend_from_slice(text.as_bytes());
        folded[start..].make_ascii_lowercase();
        return;
    }
    for ch in text.chars() {
        if ch.is_ascii() {
            folded.push(ch.to_ascii_lowercase() as u8);
        } else {
            let ch_len = ch.len_utf8();
            let lower_count: usize = ch.to_lowercase().map(|c| c.len_utf8()).sum();
            if lower_count == ch_len {
                for lower_ch in ch.to_lowercase() {
                    let mut buf = [0u8; 4];
                    folded.extend_from_slice(lower_ch.encode_utf8(&mut buf).as_bytes());
                }
            } else {
                let mut buf = [0u8; 4];
                folded.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
}

/// Returns the byte-length-preserving case-folded bytes of `query`.
pub fn fold_query_bytes(query: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(query.len());
    fold_append(query, &mut out);
    out
}

/// Returns `true` for Unicode combining diacritical marks.
#[inline]
pub fn is_combining_mark(ch: char) -> bool {
    matches!(
        ch as u32,
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F
    )
}

/// Returns `true` for ASCII and common Unicode/CJK punctuation characters.
#[inline]
pub fn is_punctuation_char(ch: char) -> bool {
    if ch.is_ascii() {
        return ch.is_ascii_punctuation();
    }
    matches!(
        ch as u32,
        0x00A1..=0x00BF
            | 0x2010..=0x2027
            | 0x2030..=0x205E
            | 0x3001..=0x303F
            | 0xFE10..=0xFE19
            | 0xFE30..=0xFE6B
            | 0xFF01..=0xFF0F
            | 0xFF1A..=0xFF20
            | 0xFF3B..=0xFF40
            | 0xFF5B..=0xFF65
    )
}

/// Strips Latin diacritical accents from common precomposed Unicode characters.
#[inline]
pub fn strip_diacritics_char(ch: char) -> char {
    match ch {
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' | 'Ā' | 'Ă' | 'Ą' | 'Ǎ' | 'Ǟ' | 'Ǡ' | 'Ǻ' | 'Ȁ' | 'Ȃ'
        | 'Ȧ' | 'Ạ' | 'Ả' | 'Ấ' | 'Ầ' | 'Ẩ' | 'Ẫ' | 'Ậ' | 'Ắ' | 'Ằ' | 'Ẳ' | 'Ẵ' | 'Ặ' => {
            'A'
        }
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'ǎ' | 'ǟ' | 'ǡ' | 'ǻ' | 'ȁ' | 'ȃ'
        | 'ȧ' | 'ạ' | 'ả' | 'ấ' | 'ầ' | 'ẩ' | 'ẫ' | 'ậ' | 'ắ' | 'ằ' | 'ẳ' | 'ẵ' | 'ặ' => {
            'a'
        }
        'Ç' | 'Ć' | 'Ĉ' | 'Ċ' | 'Č' => 'C',
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
        'Ď' | 'Đ' => 'D',
        'ď' | 'đ' => 'd',
        'È' | 'É' | 'Ê' | 'Ë' | 'Ē' | 'Ĕ' | 'Ė' | 'Ę' | 'Ě' | 'Ȅ' | 'Ȇ' | 'Ȩ' | 'Ẹ' | 'Ẻ' | 'Ẽ'
        | 'Ế' | 'Ề' | 'Ể' | 'Ễ' | 'Ệ' => 'E',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' | 'ȅ' | 'ȇ' | 'ȩ' | 'ẹ' | 'ẻ' | 'ẽ'
        | 'ế' | 'ề' | 'ể' | 'ễ' | 'ệ' => 'e',
        'Ĝ' | 'Ğ' | 'Ġ' | 'Ģ' | 'Ǧ' => 'G',
        'ĝ' | 'ğ' | 'ġ' | 'ģ' | 'ǧ' => 'g',
        'Ĥ' | 'Ħ' => 'H',
        'ĥ' | 'ħ' => 'h',
        'Ì' | 'Í' | 'Î' | 'Ï' | 'Ĩ' | 'Ī' | 'Ĭ' | 'Į' | 'İ' | 'Ǐ' | 'Ȉ' | 'Ȋ' | 'Ỉ' | 'Ị' => {
            'I'
        }
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' | 'ǐ' | 'ȉ' | 'ȋ' | 'ỉ' | 'ị' => {
            'i'
        }
        'Ĵ' => 'J',
        'ĵ' => 'j',
        'Ķ' | 'Ǩ' => 'K',
        'ķ' | 'ǩ' => 'k',
        'Ĺ' | 'Ļ' | 'Ľ' | 'Ŀ' | 'Ł' => 'L',
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => 'l',
        'Ñ' | 'Ń' | 'Ņ' | 'Ň' | 'Ǹ' => 'N',
        'ñ' | 'ń' | 'ņ' | 'ň' | 'ǹ' => 'n',
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' | 'Ø' | 'Ō' | 'Ŏ' | 'Ő' | 'Ơ' | 'Ǒ' | 'Ǫ' | 'Ǭ' | 'Ǿ' | 'Ȍ'
        | 'Ȏ' | 'Ȫ' | 'Ȭ' | 'Ȯ' | 'Ȱ' | 'Ọ' | 'Ỏ' | 'Ố' | 'Ồ' | 'Ổ' | 'Ỗ' | 'Ộ' | 'Ớ' | 'Ờ'
        | 'Ở' | 'Ỡ' | 'Ợ' => 'O',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' | 'ơ' | 'ǒ' | 'ǫ' | 'ǭ' | 'ǿ' | 'ȍ'
        | 'ȏ' | 'ȫ' | 'ȭ' | 'ȯ' | 'ȱ' | 'ọ' | 'ỏ' | 'ố' | 'ồ' | 'ổ' | 'ỗ' | 'ộ' | 'ớ' | 'ờ'
        | 'ở' | 'ỡ' | 'ợ' => 'o',
        'Ŕ' | 'Ŗ' | 'Ř' | 'Ȑ' | 'Ȓ' => 'R',
        'ŕ' | 'ŗ' | 'ř' | 'ȑ' | 'ȓ' => 'r',
        'Ś' | 'Ŝ' | 'Ş' | 'Š' | 'Ș' => 'S',
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => 's',
        'Ţ' | 'Ť' | 'Ŧ' | 'Ț' => 'T',
        'ţ' | 'ť' | 'ŧ' | 'ț' => 't',
        'Ù' | 'Ú' | 'Û' | 'Ü' | 'Ũ' | 'Ū' | 'Ŭ' | 'Ů' | 'Ű' | 'Ų' | 'Ư' | 'Ǔ' | 'Ǖ' | 'Ǘ' | 'Ǚ'
        | 'Ǜ' | 'Ȕ' | 'Ȗ' | 'Ụ' | 'Ủ' | 'Ứ' | 'Ừ' | 'Ử' | 'Ữ' | 'Ự' => 'U',
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' | 'ư' | 'ǔ' | 'ǖ' | 'ǘ' | 'ǚ'
        | 'ǜ' | 'ȕ' | 'ȗ' | 'ụ' | 'ủ' | 'ứ' | 'ừ' | 'ử' | 'ữ' | 'ự' => 'u',
        'Ŵ' => 'W',
        'ŵ' => 'w',
        'Ý' | 'Ŷ' | 'Ÿ' | 'Ỳ' | 'Ỵ' | 'Ỷ' | 'Ỹ' => 'Y',
        'ý' | 'ÿ' | 'ŷ' | 'ỳ' | 'ỵ' | 'ỷ' | 'ỹ' => 'y',
        'Ź' | 'Ż' | 'Ž' => 'Z',
        'ź' | 'ż' | 'ž' => 'z',
        other => other,
    }
}

/// Pushes a single character into `out` after applying `opts`, returning `false` when skipped.
#[inline]
pub fn push_normalized_char(ch: char, opts: TextMatchOptions, out: &mut String) -> bool {
    if opts.ignore_diacritics && is_combining_mark(ch) {
        return false;
    }
    let mapped = if opts.ignore_diacritics {
        strip_diacritics_char(ch)
    } else {
        ch
    };
    if opts.ignore_whitespace && mapped.is_whitespace() {
        return false;
    }
    if opts.ignore_punctuation && is_punctuation_char(mapped) {
        return false;
    }
    if opts.case_sensitive {
        out.push(mapped);
    } else {
        for lower in mapped.to_lowercase() {
            out.push(lower);
        }
    }
    true
}

/// Normalizes `input` according to `opts`.
pub fn normalize_for_match(input: &str, opts: TextMatchOptions) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        push_normalized_char(ch, opts, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diacritics_and_punctuation_folding() {
        let opts = TextMatchOptions {
            case_sensitive: false,
            ignore_punctuation: true,
            ignore_whitespace: true,
            ignore_diacritics: true,
        };
        assert_eq!(
            normalize_for_match("Café — Résumé_01", opts),
            "caferesume01"
        );
        assert_eq!(normalize_for_match("c\u{0301}a\u{0302}fe", opts), "cafe");

        let keep_diac = TextMatchOptions {
            ignore_diacritics: false,
            ..Default::default()
        };
        assert_ne!(
            normalize_for_match("Café", keep_diac),
            normalize_for_match("Cafe", keep_diac)
        );
    }
}
