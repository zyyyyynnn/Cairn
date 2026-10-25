// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Encoding detection supporting UTF-8, UTF-16, GBK, and legacy Windows encodings.

use chardetng::EncodingDetector;

/// Decodes raw file bytes into a UTF-8 String, automatically detecting BOM or legacy encoding.
pub fn decode_bytes_to_string(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }

    // 1. Check for BOM
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        // UTF-8 BOM
        return String::from_utf8_lossy(&bytes[3..]).to_string();
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        // UTF-16LE BOM
        let u16s: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        return String::from_utf16_lossy(&u16s);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        // UTF-16BE BOM
        let u16s: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
            .collect();
        return String::from_utf16_lossy(&u16s);
    }

    // 2. Fast path: check valid UTF-8
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }

    // 3. Fallback: chardetng detector + encoding_rs
    let mut detector = EncodingDetector::new(chardetng::Iso2022JpDetection::Allow);
    detector.feed(bytes, true);
    let encoding = detector.guess(None, chardetng::Utf8Detection::Allow);

    let (cow, _encoding_used, _had_errors) = encoding.decode(bytes);
    cow.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_utf8_and_bom_decoding() {
        let utf8_text = "你好，世界！";
        assert_eq!(decode_bytes_to_string(utf8_text.as_bytes()), utf8_text);

        let mut bom_bytes = vec![0xEF, 0xBB, 0xBF];
        bom_bytes.extend_from_slice(utf8_text.as_bytes());
        assert_eq!(decode_bytes_to_string(&bom_bytes), utf8_text);
    }

    #[test]
    fn test_gbk_decoding() {
        let (gbk_bytes, _, _) = encoding_rs::GBK.encode("中文测试文档");
        let decoded = decode_bytes_to_string(&gbk_bytes);
        assert_eq!(decoded, "中文测试文档");
    }
}
