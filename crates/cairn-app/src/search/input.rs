// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Windows Chinese IME-compatible Search Box with cursor tracking,
//! UTF-16 ↔ UTF-8 lossless offset mapping, and duplicate commit prevention.

use gpui_kit::gpui::*;
use std::ops::Range;
use std::time::{Duration, Instant};

/// Bidirectional index converter between UTF-16 code unit offsets and UTF-8 byte offsets.
/// Fixes Zed Issue #64389 (panic on invalid char boundary with non-ASCII or Chinese text).
pub struct Utf16Utf8Mapper<'a> {
    text: &'a str,
}

impl<'a> Utf16Utf8Mapper<'a> {
    pub fn new(text: &'a str) -> Self {
        Self { text }
    }

    /// Converts a UTF-16 code unit offset to the corresponding UTF-8 byte boundary offset.
    pub fn utf16_to_utf8_offset(&self, u16_offset: usize) -> usize {
        let mut curr_u16 = 0;
        for (byte_offset, ch) in self.text.char_indices() {
            if curr_u16 >= u16_offset {
                return byte_offset;
            }
            curr_u16 += ch.len_utf16();
        }
        self.text.len()
    }

    /// Converts a UTF-8 byte offset to the corresponding UTF-16 code unit offset.
    pub fn utf8_to_utf16_offset(&self, u8_offset: usize) -> usize {
        let valid_u8 = u8_offset.min(self.text.len());
        let mut u16_offset = 0;
        for (byte_offset, ch) in self.text.char_indices() {
            if byte_offset >= valid_u8 {
                break;
            }
            u16_offset += ch.len_utf16();
        }
        u16_offset
    }

    /// Converts a UTF-16 range to a UTF-8 range.
    pub fn utf16_to_utf8_range(&self, u16_range: Range<usize>) -> Range<usize> {
        let start = self.utf16_to_utf8_offset(u16_range.start);
        let end = self.utf16_to_utf8_offset(u16_range.end);
        start..end
    }

    /// Converts a UTF-8 range to a UTF-16 range.
    pub fn utf8_to_utf16_range(&self, u8_range: Range<usize>) -> Range<usize> {
        let start = self.utf8_to_utf16_offset(u8_range.start);
        let end = self.utf8_to_utf16_offset(u8_range.end);
        start..end
    }
}

/// A point-in-time snapshot of the query text and cursor/selection range for Undo/Redo operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputSnapshot {
    pub text: String,
    pub selection: Range<usize>,
}

/// State for IME composition tracking, cursor positioning, duplicate insertion suppression,
/// and Undo/Redo history tracking.
pub struct SearchInputState {
    pub text: String,
    pub selection: Range<usize>,
    pub marked_range: Option<Range<usize>>,
    pub last_committed_text: Option<String>,
    pub last_commit_time: Option<Instant>,
    pub is_focused: bool,
    pub undo_stack: Vec<InputSnapshot>,
    pub redo_stack: Vec<InputSnapshot>,
    pub last_snapshot_time: Option<Instant>,
}

impl Default for SearchInputState {
    fn default() -> Self {
        Self {
            text: String::new(),
            selection: 0..0,
            marked_range: None,
            last_committed_text: None,
            last_commit_time: None,
            is_focused: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            last_snapshot_time: None,
        }
    }
}

/// Geometry of the painted query text, read back by pointer and IME hit testing.
///
/// `bounds` is the text area's own paint bounds — the canvas inside the query box, after the search
/// glyph and the box padding — not the query box's bounds. The style is captured at paint time so a
/// click measures with the same font the frame drew; `font_size` is zero before the first paint.
#[derive(Clone)]
pub struct QueryTextMetrics {
    pub bounds: Bounds<Pixels>,
    pub font_size: Pixels,
    pub style: TextStyle,
}

impl Default for QueryTextMetrics {
    fn default() -> Self {
        Self {
            bounds: Bounds::default(),
            font_size: px(0.0),
            style: TextStyle::default(),
        }
    }
}

/// Platform IME geometry, in screen pixels. These must not scale with interface zoom: Windows
/// places the candidate window against the physical caret, not against the application's `rem`.
const IME_FALLBACK_BAR_HEIGHT: Pixels = px(26.0);
const IME_FLIP_ABOVE_Y: Pixels = px(600.0);
const IME_GAP: Pixels = px(4.0);
const IME_CANDIDATE_WIDTH: Pixels = px(200.0);
const IME_CANDIDATE_HEIGHT: Pixels = px(22.0);

/// Where the platform IME should put its candidate window for a caret at `caret_x`.
///
/// `element_bounds` is the painted text area, so the candidate follows the same origin the glyphs
/// were drawn from. The fallback height and the flip above a low window keep the popup on screen.
pub fn candidate_window_bounds(element_bounds: Bounds<Pixels>, caret_x: Pixels) -> Bounds<Pixels> {
    let bar_height = if element_bounds.size.height > px(0.0) {
        element_bounds.size.height
    } else {
        IME_FALLBACK_BAR_HEIGHT
    };
    let origin_y = if element_bounds.origin.y > IME_FLIP_ABOVE_Y {
        element_bounds.origin.y - IME_FALLBACK_BAR_HEIGHT
    } else {
        element_bounds.origin.y + bar_height + IME_GAP
    };

    Bounds {
        origin: Point {
            x: element_bounds.origin.x + caret_x,
            y: origin_y,
        },
        size: Size {
            width: IME_CANDIDATE_WIDTH,
            height: IME_CANDIDATE_HEIGHT,
        },
    }
}

impl SearchInputState {
    /// Records a snapshot into the undo stack, pruning oldest entries if exceeding limit (64 steps).
    /// Clears the redo stack because a new edit branches history.
    pub fn push_undo_snapshot(&mut self) {
        let current = InputSnapshot {
            text: self.text.clone(),
            selection: self.selection.clone(),
        };
        if self.undo_stack.last() == Some(&current) {
            return;
        }
        self.undo_stack.push(current);
        if self.undo_stack.len() > 64 {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
        self.last_snapshot_time = Some(Instant::now());
    }

    /// Conditionally pushes an undo snapshot for normal sequential character typing.
    /// Groups rapid keystrokes within 500ms or after spaces/punctuations.
    pub fn push_typing_undo_snapshot(&mut self, is_replacement_or_paste: bool) {
        if is_replacement_or_paste {
            self.push_undo_snapshot();
            return;
        }
        let now = Instant::now();
        let should_push = match self.last_snapshot_time {
            Some(last) => now.duration_since(last) > Duration::from_millis(500),
            None => true,
        };
        if should_push {
            self.push_undo_snapshot();
        }
    }

    /// Reverts the text and selection to the previous state from the undo stack.
    /// Returns true if an undo operation was performed.
    pub fn undo(&mut self) -> bool {
        if let Some(snapshot) = self.undo_stack.pop() {
            self.redo_stack.push(InputSnapshot {
                text: self.text.clone(),
                selection: self.selection.clone(),
            });
            self.text = snapshot.text;
            self.selection = snapshot.selection;
            self.marked_range = None;
            self.last_snapshot_time = None;
            true
        } else {
            false
        }
    }

    /// Re-applies the next state from the redo stack.
    /// Returns true if a redo operation was performed.
    pub fn redo(&mut self) -> bool {
        if let Some(snapshot) = self.redo_stack.pop() {
            self.undo_stack.push(InputSnapshot {
                text: self.text.clone(),
                selection: self.selection.clone(),
            });
            self.text = snapshot.text;
            self.selection = snapshot.selection;
            self.marked_range = None;
            self.last_snapshot_time = None;
            true
        } else {
            false
        }
    }

    pub fn set_text(&mut self, new_text: &str) {
        if self.text != new_text {
            self.push_undo_snapshot();
            self.text = new_text.to_string();
            self.selection = self.text.len()..self.text.len();
            self.marked_range = None;
        }
    }

    pub fn select_all(&mut self) {
        self.selection = 0..self.text.len();
    }

    pub fn clear(&mut self) {
        if !self.text.is_empty() {
            self.push_undo_snapshot();
            self.text.clear();
            self.selection = 0..0;
            self.marked_range = None;
        }
    }

    /// Handles text replacement in range with duplicate WM_CHAR suppression (Zed Issue #56327).
    pub fn handle_replace_text_in_range(
        &mut self,
        replacement_range: Option<Range<usize>>,
        new_text: &str,
    ) {
        let now = Instant::now();
        // Check for duplicate rapid insertion of the exact same string from an IME commit (Zed Issue #56327).
        // Only suppress if this comes immediately after an active IME composition commit.
        if let Some(ref last_committed) = self.last_committed_text {
            if let Some(last_time) = self.last_commit_time {
                if now.duration_since(last_time) < Duration::from_millis(80)
                    && last_committed == new_text
                    && self.marked_range.is_none()
                {
                    self.last_committed_text = None;
                    return; // Suppress redundant WM_CHAR
                }
            }
        }

        let is_paste_or_multi = new_text.chars().count() > 1
            || replacement_range.is_some()
            || self.selection.start != self.selection.end;
        self.push_typing_undo_snapshot(is_paste_or_multi);

        let had_marked_range = self.marked_range.is_some();

        let mapper = Utf16Utf8Mapper::new(&self.text);
        let target_range = if let Some(r) = replacement_range {
            mapper.utf16_to_utf8_range(r)
        } else if let Some(ref marked) = self.marked_range {
            marked.clone()
        } else {
            self.selection.clone()
        };

        let start = target_range.start.min(self.text.len());
        let end = target_range.end.min(self.text.len());

        let mut updated = String::with_capacity(self.text.len() + new_text.len());
        updated.push_str(&self.text[..start]);
        updated.push_str(new_text);
        updated.push_str(&self.text[end..]);

        let new_cursor = start + new_text.len();
        self.text = updated;
        self.selection = new_cursor..new_cursor;
        self.marked_range = None;

        // Record last commit ONLY if we just committed from an active IME composition
        if had_marked_range && !new_text.is_empty() {
            self.last_committed_text = Some(new_text.to_string());
            self.last_commit_time = Some(now);
        } else {
            self.last_committed_text = None;
            self.last_commit_time = None;
        }
    }

    /// Moves cursor one character left (UTF-8 safe).
    pub fn move_cursor_left(&mut self, select: bool) {
        let cursor = if select {
            self.selection.end
        } else {
            self.selection.start
        };
        let prev_boundary = self.text[..cursor.min(self.text.len())]
            .char_indices()
            .last()
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        if select {
            self.selection = self.selection.start..prev_boundary;
        } else {
            self.selection = prev_boundary..prev_boundary;
        }
        self.marked_range = None;
    }

    /// Moves cursor one character right (UTF-8 safe).
    pub fn move_cursor_right(&mut self, select: bool) {
        let cursor = self.selection.end;
        let next_boundary = if cursor < self.text.len() {
            self.text[cursor..]
                .chars()
                .next()
                .map(|ch| cursor + ch.len_utf8())
                .unwrap_or(self.text.len())
        } else {
            self.text.len()
        };
        if select {
            self.selection = self.selection.start..next_boundary;
        } else {
            self.selection = next_boundary..next_boundary;
        }
        self.marked_range = None;
    }

    /// Moves cursor to start of text.
    pub fn move_cursor_home(&mut self, select: bool) {
        if select {
            self.selection = 0..self.selection.end;
        } else {
            self.selection = 0..0;
        }
        self.marked_range = None;
    }

    /// Moves cursor to end of text.
    pub fn move_cursor_end(&mut self, select: bool) {
        let len = self.text.len();
        if select {
            self.selection = self.selection.start..len;
        } else {
            self.selection = len..len;
        }
        self.marked_range = None;
    }

    /// Deletes text backward (Backspace).
    pub fn delete_backward(&mut self) {
        if self.selection.start < self.selection.end && self.selection.end <= self.text.len() {
            self.push_undo_snapshot();
            let mut updated = self.text.clone();
            updated.drain(self.selection.clone());
            self.text = updated;
            let cursor = self.selection.start;
            self.selection = cursor..cursor;
        } else if self.selection.start > 0 {
            let cursor = self.selection.start.min(self.text.len());
            if let Some((prev_idx, _)) = self.text[..cursor].char_indices().last() {
                self.push_undo_snapshot();
                let mut updated = String::with_capacity(self.text.len());
                updated.push_str(&self.text[..prev_idx]);
                updated.push_str(&self.text[cursor..]);
                self.text = updated;
                self.selection = prev_idx..prev_idx;
            }
        }
        self.marked_range = None;
    }

    /// Deletes text forward (Delete).
    pub fn delete_forward(&mut self) {
        if self.selection.start < self.selection.end && self.selection.end <= self.text.len() {
            self.push_undo_snapshot();
            let mut updated = self.text.clone();
            updated.drain(self.selection.clone());
            self.text = updated;
            let cursor = self.selection.start;
            self.selection = cursor..cursor;
        } else if self.selection.start < self.text.len() {
            let cursor = self.selection.start;
            if let Some(ch) = self.text[cursor..].chars().next() {
                self.push_undo_snapshot();
                let next_idx = cursor + ch.len_utf8();
                let mut updated = String::with_capacity(self.text.len());
                updated.push_str(&self.text[..cursor]);
                updated.push_str(&self.text[next_idx..]);
                self.text = updated;
                self.selection = cursor..cursor;
            }
        }
        self.marked_range = None;
    }

    /// Finds the preceding word boundary for Ctrl+Left or Ctrl+Backspace.
    pub fn find_prev_word_boundary(&self, cursor: usize) -> usize {
        let cursor = cursor.min(self.text.len());
        if cursor == 0 {
            return 0;
        }
        let text_before = &self.text[..cursor];
        let mut chars = text_before.char_indices().rev().peekable();

        // Skip trailing whitespace if any
        while let Some(&(_, ch)) = chars.peek() {
            if ch.is_whitespace() {
                chars.next();
            } else {
                break;
            }
        }

        let Some(&(_, first_ch)) = chars.peek() else {
            return 0;
        };

        let is_alphanumeric_or_cjk = |c: char| c.is_alphanumeric() || (c as u32) >= 0x4E00;
        let is_target_alnum = is_alphanumeric_or_cjk(first_ch);

        let mut boundary = 0;
        while let Some(&(idx, ch)) = chars.peek() {
            if is_alphanumeric_or_cjk(ch) == is_target_alnum && !ch.is_whitespace() {
                boundary = idx;
                chars.next();
            } else {
                break;
            }
        }
        boundary
    }

    /// Finds the following word boundary for Ctrl+Right or Ctrl+Delete.
    pub fn find_next_word_boundary(&self, cursor: usize) -> usize {
        let len = self.text.len();
        if cursor >= len {
            return len;
        }
        let text_after = &self.text[cursor..];
        let mut chars = text_after.char_indices().peekable();

        // Skip leading whitespace if any
        while let Some(&(_, ch)) = chars.peek() {
            if ch.is_whitespace() {
                chars.next();
            } else {
                break;
            }
        }

        let Some(&(_, first_ch)) = chars.peek() else {
            return len;
        };

        let is_alphanumeric_or_cjk = |c: char| c.is_alphanumeric() || (c as u32) >= 0x4E00;
        let is_target_alnum = is_alphanumeric_or_cjk(first_ch);

        let mut boundary = len;
        while let Some(&(idx, ch)) = chars.peek() {
            if is_alphanumeric_or_cjk(ch) == is_target_alnum && !ch.is_whitespace() {
                chars.next();
                boundary = cursor + idx + ch.len_utf8();
            } else {
                break;
            }
        }
        boundary
    }

    /// Deletes the preceding word (Ctrl+Backspace).
    pub fn delete_word_backward(&mut self) {
        if self.selection.start < self.selection.end && self.selection.end <= self.text.len() {
            self.delete_backward();
        } else if self.selection.start > 0 {
            let cursor = self.selection.start.min(self.text.len());
            let prev_boundary = self.find_prev_word_boundary(cursor);
            if prev_boundary < cursor {
                self.push_undo_snapshot();
                let mut updated = String::with_capacity(self.text.len());
                updated.push_str(&self.text[..prev_boundary]);
                updated.push_str(&self.text[cursor..]);
                self.text = updated;
                self.selection = prev_boundary..prev_boundary;
                self.marked_range = None;
            }
        }
    }

    /// Deletes the following word (Ctrl+Delete).
    pub fn delete_word_forward(&mut self) {
        if self.selection.start < self.selection.end && self.selection.end <= self.text.len() {
            self.delete_forward();
        } else if self.selection.start < self.text.len() {
            let cursor = self.selection.start;
            let next_boundary = self.find_next_word_boundary(cursor);
            if next_boundary > cursor {
                self.push_undo_snapshot();
                let mut updated = String::with_capacity(self.text.len());
                updated.push_str(&self.text[..cursor]);
                updated.push_str(&self.text[next_boundary..]);
                self.text = updated;
                self.selection = cursor..cursor;
                self.marked_range = None;
            }
        }
    }

    /// Moves cursor one word left (Ctrl+Left).
    pub fn move_cursor_word_left(&mut self, select: bool) {
        let cursor = if select {
            self.selection.end
        } else {
            self.selection.start
        };
        let prev_boundary = self.find_prev_word_boundary(cursor);
        if select {
            self.selection = self.selection.start..prev_boundary;
        } else {
            self.selection = prev_boundary..prev_boundary;
        }
        self.marked_range = None;
    }

    /// Moves cursor one word right (Ctrl+Right).
    pub fn move_cursor_word_right(&mut self, select: bool) {
        let cursor = self.selection.end;
        let next_boundary = self.find_next_word_boundary(cursor);
        if select {
            self.selection = self.selection.start..next_boundary;
        } else {
            self.selection = next_boundary..next_boundary;
        }
        self.marked_range = None;
    }

    /// Handles IME composition text replacement and marking (Zed Issue #64389).
    pub fn handle_replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
    ) {
        let (target_start, target_end, marked_start_u16) = {
            let mapper = Utf16Utf8Mapper::new(&self.text);
            let target_range = if let Some(r) = range_utf16 {
                mapper.utf16_to_utf8_range(r)
            } else if let Some(ref marked) = self.marked_range {
                marked.clone()
            } else {
                self.selection.clone()
            };
            let start = target_range.start.min(self.text.len());
            let end = target_range.end.min(self.text.len());
            let start_u16 = mapper.utf8_to_utf16_offset(start);
            (start, end, start_u16)
        };

        let mut updated = String::with_capacity(self.text.len() + new_text.len());
        updated.push_str(&self.text[..target_start]);
        updated.push_str(new_text);
        updated.push_str(&self.text[target_end..]);

        let marked_start = target_start;
        let marked_end = target_start + new_text.len();
        self.text = updated;

        if new_text.is_empty() {
            self.marked_range = None;
        } else {
            self.marked_range = Some(marked_start..marked_end);
        }

        if let Some(new_sel) = new_selected_range {
            let mapper_new = Utf16Utf8Mapper::new(&self.text);
            let sel_start = mapper_new.utf16_to_utf8_offset(marked_start_u16 + new_sel.start);
            let sel_end = mapper_new.utf16_to_utf8_offset(marked_start_u16 + new_sel.end);
            self.selection = sel_start..sel_end;
        } else {
            self.selection = marked_end..marked_end;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{candidate_window_bounds, SearchInputState, Utf16Utf8Mapper};
    use core::prelude::rust_2021::test;
    use gpui_kit::gpui::{point, px, size, Bounds};

    #[test]
    fn test_utf16_utf8_mapper() {
        let s = "content:架构设计 v1";
        let mapper = Utf16Utf8Mapper::new(s);

        // "架构设计" has 4 Chinese characters, each 3 bytes in UTF-8 and 1 code unit in UTF-16
        // "content:" is 8 bytes / 8 code units
        let u16_off = 8 + 2; // pointing after '构'
        let u8_off = mapper.utf16_to_utf8_offset(u16_off);
        assert_eq!(u8_off, 8 + 6); // 8 + 2*3 = 14 bytes

        let back_u16 = mapper.utf8_to_utf16_offset(u8_off);
        assert_eq!(back_u16, u16_off);
    }

    #[test]
    fn test_duplicate_insertion_suppression() {
        let mut state = SearchInputState {
            marked_range: Some(0..0),
            ..Default::default()
        };
        state.handle_replace_text_in_range(None, "文件");
        assert_eq!(state.text, "文件");

        // Immediately following redundant WM_CHAR should be suppressed
        state.handle_replace_text_in_range(None, "文件");
        assert_eq!(state.text, "文件");
    }

    #[test]
    fn test_ime_composition_and_marking() {
        let mut state = SearchInputState::default();

        // 1. Simulating IME typing "cairn"
        state.handle_replace_and_mark_text_in_range(None, "cairn", None);
        assert_eq!(state.text, "cairn");
        assert_eq!(state.marked_range, Some(0..5));
        assert_eq!(state.selection, 5..5);

        // 2. Candidate selected, commit "石堆"
        state.handle_replace_text_in_range(None, "石堆");
        assert_eq!(state.text, "石堆");
        assert_eq!(state.marked_range, None);
        assert_eq!(state.selection, 6..6); // "石堆" is 6 UTF-8 bytes

        // 3. Candidate bounds follow the caret's measured x, not a fixed character estimate.
        let bounds = candidate_window_bounds(
            Bounds {
                origin: point(px(100.0), px(20.0)),
                size: size(px(200.0), px(26.0)),
            },
            px(17.5),
        );
        assert_eq!(bounds.origin.x, px(117.5));
        assert_eq!(bounds.origin.y, px(50.0));
    }

    #[test]
    fn test_rapid_typing_does_not_drop_characters() {
        let mut state = SearchInputState::default();
        // Typing "look" with fast double "o"
        state.handle_replace_text_in_range(None, "l");
        state.handle_replace_text_in_range(None, "o");
        // Rapid second 'o' within a few milliseconds
        state.handle_replace_text_in_range(None, "o");
        state.handle_replace_text_in_range(None, "k");
        assert_eq!(
            state.text, "look",
            "Rapid repeated keystrokes must not be dropped in normal typing"
        );
    }

    #[test]
    fn test_cursor_navigation_and_deletion() {
        let mut state = SearchInputState::default();
        state.handle_replace_text_in_range(None, "hello world");
        assert_eq!(state.selection, 11..11);

        // Move left 5 times to position before "world"
        for _ in 0..5 {
            state.move_cursor_left(false);
        }
        assert_eq!(state.selection, 6..6);

        // Backspace should delete space before "world"
        state.delete_backward();
        assert_eq!(state.text, "helloworld");
        assert_eq!(state.selection, 5..5);

        // Delete forward should delete 'w'
        state.delete_forward();
        assert_eq!(state.text, "helloorld");
        assert_eq!(state.selection, 5..5);

        // Home and typing
        state.move_cursor_home(false);
        assert_eq!(state.selection, 0..0);
        state.handle_replace_text_in_range(None, "say ");
        assert_eq!(state.text, "say helloorld");

        // End
        state.move_cursor_end(false);
        assert_eq!(state.selection, state.text.len()..state.text.len());

        // Select all and delete
        state.select_all();
        state.delete_backward();
        assert_eq!(state.text, "");
        assert_eq!(state.selection, 0..0);
    }

    #[test]
    fn test_chinese_cursor_navigation_and_deletion() {
        let mut state = SearchInputState::default();
        state.handle_replace_text_in_range(None, "测试中文搜索");
        assert_eq!(state.text.len(), 18); // 6 Chinese characters * 3 bytes

        // Move left 2 characters (before "搜索", after "中文")
        state.move_cursor_left(false);
        state.move_cursor_left(false);
        assert_eq!(state.selection, 12..12);

        // Backspace should delete '文' (3 bytes before cursor)
        state.delete_backward();
        assert_eq!(state.text, "测试中搜索");
        assert_eq!(state.selection, 9..9);

        // Forward delete should delete '搜' (3 bytes after cursor)
        state.delete_forward();
        assert_eq!(state.text, "测试中索");
        assert_eq!(state.selection, 9..9);
    }

    #[test]
    fn test_undo_and_redo() {
        let mut state = SearchInputState::default();

        // 1. Initial typing
        state.handle_replace_text_in_range(None, "hello");
        assert_eq!(state.text, "hello");

        // 2. Clear query
        state.clear();
        assert_eq!(state.text, "");

        // 3. Undo clear -> restored "hello"
        assert!(state.undo());
        assert_eq!(state.text, "hello");

        // 4. Redo clear -> back to empty
        assert!(state.redo());
        assert_eq!(state.text, "");

        // 5. Undo again
        assert!(state.undo());
        assert_eq!(state.text, "hello");

        // 6. Append " world"
        state.handle_replace_text_in_range(None, " world");
        assert_eq!(state.text, "hello world");

        // 7. Undo append -> "hello"
        assert!(state.undo());
        assert_eq!(state.text, "hello");
    }

    #[test]
    fn test_word_boundary_navigation_and_deletion() {
        let mut state = SearchInputState::default();
        state.handle_replace_text_in_range(None, "cairn ext:rs;toml");
        assert_eq!(state.selection, 17..17);

        // Delete word backward -> deletes "toml"
        state.delete_word_backward();
        assert_eq!(state.text, "cairn ext:rs;");

        // Delete word backward -> deletes ";"
        state.delete_word_backward();
        assert_eq!(state.text, "cairn ext:rs");

        // Delete word backward -> deletes "rs"
        state.delete_word_backward();
        assert_eq!(state.text, "cairn ext:");

        // Undo -> back to "cairn ext:rs"
        assert!(state.undo());
        assert_eq!(state.text, "cairn ext:rs");
    }
}
