// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Tantivy custom CJK Bigram + Latin Word mixed tokenizer.

use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

pub const CJK_BIGRAM_LATIN_TOKENIZER: &str = "cjk_bigram_latin";

/// Tokenizer that produces CJK bigrams (with unigram fallback) and Latin lowercase words.
#[derive(Clone, Default)]
pub struct CjkBigramLatinTokenizer;

impl Tokenizer for CjkBigramLatinTokenizer {
    type TokenStream<'a> = CjkBigramLatinTokenStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        CjkBigramLatinTokenStream::new(text)
    }
}

/// Streaming zero-vector token stream reusing a single `Token` buffer across `advance()` calls.
pub struct CjkBigramLatinTokenStream<'a> {
    text: &'a str,
    byte_pos: usize,
    next_position: usize,
    pending_bigram: Option<(usize, usize, char, char)>,
    token: Token,
}

impl<'a> CjkBigramLatinTokenStream<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            byte_pos: 0,
            next_position: 0,
            pending_bigram: None,
            token: Token {
                position_length: 1,
                ..Token::default()
            },
        }
    }

    fn advance_token_meta(&mut self, offset_from: usize, offset_to: usize) {
        self.token.offset_from = offset_from;
        self.token.offset_to = offset_to;
        self.token.position = self.next_position;
        self.token.position_length = 1;
        self.next_position += 1;
    }
}

impl TokenStream for CjkBigramLatinTokenStream<'_> {
    fn advance(&mut self) -> bool {
        if let Some((offset_from, offset_to, first, second)) = self.pending_bigram.take() {
            self.advance_token_meta(offset_from, offset_to);
            self.token.text.clear();
            self.token.text.push(first);
            self.token.text.push(second);
            return true;
        }

        while self.byte_pos < self.text.len() {
            let rest = &self.text[self.byte_pos..];
            let Some(ch) = rest.chars().next() else {
                break;
            };

            if is_cjk(ch) {
                let start_byte = self.byte_pos;
                let end_byte = start_byte + ch.len_utf8();
                self.byte_pos = end_byte;

                if let Some(next_ch) = self.text[end_byte..].chars().next().filter(|c| is_cjk(*c)) {
                    self.pending_bigram =
                        Some((start_byte, end_byte + next_ch.len_utf8(), ch, next_ch));
                }

                self.advance_token_meta(start_byte, end_byte);
                self.token.text.clear();
                self.token.text.push(ch);
                return true;
            } else if ch.is_alphanumeric() {
                let word_start = self.byte_pos;
                let mut consumed = 0usize;
                self.token.text.clear();
                for c in rest.chars() {
                    if c.is_alphanumeric() && !is_cjk(c) {
                        if c.is_ascii() {
                            self.token.text.push(c.to_ascii_lowercase());
                        } else {
                            for lc in c.to_lowercase() {
                                self.token.text.push(lc);
                            }
                        }
                        consumed += c.len_utf8();
                    } else {
                        break;
                    }
                }
                self.byte_pos = word_start + consumed;
                self.advance_token_meta(word_start, self.byte_pos);
                return true;
            } else {
                self.byte_pos += ch.len_utf8();
            }
        }

        false
    }

    fn token(&self) -> &Token {
        &self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.token
    }
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{4E00}'..='\u{9FFF}' |   // CJK Unified Ideographs
        '\u{3400}'..='\u{4DBF}' |   // CJK Extension A
        '\u{20000}'..='\u{2A6DF}' | // CJK Extension B
        '\u{F900}'..='\u{FAFF}'     // CJK Compatibility Ideographs
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    struct ExpectedToken<'a> {
        text: &'a str,
        offset_from: usize,
        offset_to: usize,
        position: usize,
        position_length: usize,
    }

    fn collect_tokens(text: &str) -> Vec<(String, usize, usize, usize, usize)> {
        let mut tokenizer = CjkBigramLatinTokenizer;
        let mut stream = tokenizer.token_stream(text);
        let mut out = Vec::new();
        while stream.advance() {
            let t = stream.token();
            out.push((
                t.text.clone(),
                t.offset_from,
                t.offset_to,
                t.position,
                t.position_length,
            ));
        }
        out
    }

    fn tok<'a>(
        text: &'a str,
        offset_from: usize,
        offset_to: usize,
        position: usize,
    ) -> ExpectedToken<'a> {
        ExpectedToken {
            text,
            offset_from,
            offset_to,
            position,
            position_length: 1,
        }
    }

    #[test]
    fn test_cjk_bigram_latin_tokenizer() {
        let mut tokenizer = CjkBigramLatinTokenizer;
        let mut stream = tokenizer.token_stream("Cairn 架构设计 v1.0");
        let mut texts = Vec::new();
        while stream.advance() {
            texts.push(stream.token().text.clone());
        }

        assert!(texts.contains(&"cairn".to_string()));
        assert!(texts.contains(&"架构".to_string()));
        assert!(texts.contains(&"构设".to_string()));
        assert!(texts.contains(&"设计".to_string()));
        assert!(texts.contains(&"v1".to_string()));
    }

    #[test]
    fn streaming_tokenizer_matches_reference_offsets_positions_and_text() {
        let cases: &[(&str, Vec<ExpectedToken<'_>>)] = &[
            ("", vec![]),
            ("   \t\n.,!?;:——  ", vec![]),
            ("汉", vec![tok("汉", 0, 3, 0)]),
            (
                "Rust编译器2026版!",
                vec![
                    tok("rust", 0, 4, 0),
                    tok("编", 4, 7, 1),
                    tok("编译", 4, 10, 2),
                    tok("译", 7, 10, 3),
                    tok("译器", 7, 13, 4),
                    tok("器", 10, 13, 5),
                    tok("2026", 13, 17, 6),
                    tok("版", 17, 20, 7),
                ],
            ),
            (
                "你好，世界！",
                vec![
                    tok("你", 0, 3, 0),
                    tok("你好", 0, 6, 1),
                    tok("好", 3, 6, 2),
                    // Full-width comma '，' is 3 bytes at 6..9
                    tok("世", 9, 12, 3),
                    tok("世界", 9, 15, 4),
                    tok("界", 12, 15, 5),
                ],
            ),
            (
                // Ext-A (3B), Ext-B surrogate-outside BMP (4B), Compat (3B), accented Latin
                "\u{3400}\u{20000}\u{F900} CAFÉ_alpha-42",
                vec![
                    tok("\u{3400}", 0, 3, 0),
                    tok("\u{3400}\u{20000}", 0, 7, 1),
                    tok("\u{20000}", 3, 7, 2),
                    tok("\u{20000}\u{F900}", 3, 10, 3),
                    tok("\u{F900}", 7, 10, 4),
                    tok("café", 11, 16, 5),
                    tok("alpha", 17, 22, 6),
                    tok("42", 23, 25, 7),
                ],
            ),
        ];

        for (input, expected) in cases {
            let actual = collect_tokens(input);
            let actual_view: Vec<ExpectedToken<'_>> = actual
                .iter()
                .map(|(text, from, to, pos, pos_len)| ExpectedToken {
                    text: text.as_str(),
                    offset_from: *from,
                    offset_to: *to,
                    position: *pos,
                    position_length: *pos_len,
                })
                .collect();
            assert_eq!(actual_view, *expected, "mismatch for input {input:?}");
        }
    }

    #[test]
    fn streaming_tokenizer_reuses_single_token_buffer_on_large_input() {
        let large = "Cairn全文检索架构设计_benchmark_2026 ".repeat(100_000);
        let mut tokenizer = CjkBigramLatinTokenizer;
        let mut stream = tokenizer.token_stream(&large);

        // Prime longest token capacity from first cycle so buffer does not need to grow later.
        let mut warm_tokens = 0usize;
        while warm_tokens < 40 && stream.advance() {
            warm_tokens += 1;
        }
        assert_eq!(warm_tokens, 40);

        let stable_token_addr = stream.token() as *const Token;
        let stable_text_ptr = stream.token().text.as_ptr();
        let stable_text_cap = stream.token().text.capacity();

        let mut remaining_tokens = 0usize;
        while stream.advance() {
            let t = stream.token();
            assert_eq!(
                t as *const Token, stable_token_addr,
                "Token struct must be reused in place across advance()"
            );
            assert_eq!(
                t.text.as_ptr(),
                stable_text_ptr,
                "Token.text String buffer must be reused in place across advance()"
            );
            assert_eq!(t.text.capacity(), stable_text_cap);
            remaining_tokens += 1;
        }
        assert!(remaining_tokens > 1_000_000);
    }
}
