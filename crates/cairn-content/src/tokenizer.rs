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
    type TokenStream<'a> = CjkBigramLatinTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        let tokens = tokenize_text(text);
        CjkBigramLatinTokenStream {
            tokens,
            current_idx: 0,
        }
    }
}

pub struct CjkBigramLatinTokenStream {
    tokens: Vec<Token>,
    current_idx: usize,
}

impl TokenStream for CjkBigramLatinTokenStream {
    fn advance(&mut self) -> bool {
        if self.current_idx < self.tokens.len() {
            self.current_idx += 1;
            true
        } else {
            false
        }
    }

    fn token(&self) -> &Token {
        &self.tokens[self.current_idx - 1]
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.tokens[self.current_idx - 1]
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

fn tokenize_text(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut position = 0;

    let mut i = 0;
    while i < chars.len() {
        let (start_byte, ch) = chars[i];

        if is_cjk(ch) {
            // 1. Single character token (unigram fallback)
            let end_byte = start_byte + ch.len_utf8();
            tokens.push(Token {
                offset_from: start_byte,
                offset_to: end_byte,
                position,
                text: ch.to_string(),
                position_length: 1,
            });
            position += 1;

            // 2. Bigram token if next char is also CJK
            if i + 1 < chars.len() {
                let (next_start, next_ch) = chars[i + 1];
                if is_cjk(next_ch) {
                    let next_end = next_start + next_ch.len_utf8();
                    let bigram = format!("{}{}", ch, next_ch);
                    tokens.push(Token {
                        offset_from: start_byte,
                        offset_to: next_end,
                        position,
                        text: bigram,
                        position_length: 1,
                    });
                    position += 1;
                }
            }
            i += 1;
        } else if ch.is_alphanumeric() {
            // Latin or alphanumeric word run
            let word_start = start_byte;
            let mut word = String::new();
            while i < chars.len() && chars[i].1.is_alphanumeric() && !is_cjk(chars[i].1) {
                word.push(chars[i].1);
                i += 1;
            }
            let word_end = if i < chars.len() {
                chars[i].0
            } else {
                text.len()
            };

            tokens.push(Token {
                offset_from: word_start,
                offset_to: word_end,
                position,
                text: word.to_lowercase(),
                position_length: 1,
            });
            position += 1;
        } else {
            // Skip whitespace and punctuation
            i += 1;
        }
    }

    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
