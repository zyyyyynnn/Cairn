// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Multi-snippet content match preview popover, state machine, and client-side term highlighting.

use crate::app::{element_id, join_row_path, layout, CairnApp};
use crate::search::query_orchestration::split_name_highlights;
use crate::shell::commands::PreviewSnippet;
use crate::shell::theme::ShellTokens;
use crate::table::rows::styled_highlighted_text;
use cairn_core::syntax::{parse_query_with, QueryDefaults};
use gpui_kit::base::Selectable;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::v_flex;
use gpui_kit::gpui::prelude::FluentBuilder as _;
use gpui_kit::gpui::*;
use std::collections::HashSet;

pub const MAX_SNIPPETS_DISPLAY: usize = 3;
const POPOVER_MIN_WIDTH: f32 = 22.0;
const POPOVER_MAX_WIDTH: f32 = 34.0;
const POPOVER_OFFSET_PX: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnippetPreviewTrigger {
    Pinned,
    Hover,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnippetPreviewState {
    pub row_path: String,
    pub trigger: SnippetPreviewTrigger,
    pub badge_hovered: bool,
    pub card_hovered: bool,
}

#[derive(IntoElement)]
struct SnippetBadgeTrigger {
    inner: Stateful<Div>,
    selected: bool,
}

impl Selectable for SnippetBadgeTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for SnippetBadgeTrigger {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.inner
    }
}

/// Filters blank lines, deduplicates preserving first occurrence, and caps snippets at three.
pub fn prepare_snippets(raw: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(MAX_SNIPPETS_DISPLAY);
    let mut seen = HashSet::new();
    for item in raw {
        let trimmed = item.trim();
        if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
            continue;
        }
        out.push(trimmed.to_string());
        if out.len() >= MAX_SNIPPETS_DISPLAY {
            break;
        }
    }
    out
}

/// Extracts deduplicated positive `content:` query terms from the active query text.
pub fn query_content_highlight_terms(query: &str, defaults: &QueryDefaults) -> Vec<String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let expr = parse_query_with(trimmed, defaults);
    let mut terms = Vec::new();
    let mut seen_lower = HashSet::new();
    for term in expr.extract_positive_content_queries() {
        let cleaned = term.trim();
        if cleaned.is_empty() {
            continue;
        }
        let lower = cleaned.to_lowercase();
        if seen_lower.insert(lower) {
            terms.push(cleaned.to_string());
        }
    }
    terms
}

/// Finds sorted, merged UTF-8 byte ranges for case-insensitive occurrences of `terms` in `snippet`.
pub fn snippet_highlight_ranges(snippet: &str, terms: &[String]) -> Vec<(usize, usize)> {
    if snippet.is_empty() || terms.is_empty() {
        return Vec::new();
    }

    let mut lowered_chars: Vec<(usize, usize, bool, char)> = Vec::with_capacity(snippet.len());
    for (byte_start, ch) in snippet.char_indices() {
        let byte_end = byte_start + ch.len_utf8();
        for (sub_ix, lc) in ch.to_lowercase().enumerate() {
            lowered_chars.push((byte_start, byte_end, sub_ix == 0, lc));
        }
    }

    let mut ranges = Vec::new();
    for term in terms {
        let term_chars: Vec<char> = term.trim().chars().flat_map(char::to_lowercase).collect();
        if term_chars.is_empty() || term_chars.len() > lowered_chars.len() {
            continue;
        }
        let len = term_chars.len();
        for i in 0..=(lowered_chars.len() - len) {
            if !lowered_chars[i].2 {
                continue;
            }
            if lowered_chars[i..i + len]
                .iter()
                .zip(term_chars.iter())
                .all(|(hay, needle)| hay.3 == *needle)
            {
                let end_byte = lowered_chars[i + len - 1].1;
                if snippet.is_char_boundary(lowered_chars[i].0)
                    && snippet.is_char_boundary(end_byte)
                {
                    ranges.push((lowered_chars[i].0, end_byte));
                }
            }
        }
    }

    if ranges.is_empty() {
        return ranges;
    }

    // Reuse interval sorting/merging invariants from `split_name_highlights` then extract highlight spans.
    let mut merged = Vec::new();
    let mut cursor = 0usize;
    for span in split_name_highlights(snippet, &ranges) {
        let start = cursor;
        cursor += span.text.len();
        if span.is_highlight {
            merged.push((start, cursor));
        }
    }
    merged
}

impl CairnApp {
    pub(crate) fn preview_snippet(
        &mut self,
        _: &PreviewSnippet,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.recycle_bin.is_open() || self.block_listing.is_some() || self.drive_view.is_some() {
            self.results.snippet_preview = None;
            return;
        }

        let Some(ix) = self.results.table.focused_idx else {
            return;
        };
        let Some(row) = self.results.table.row(ix) else {
            return;
        };
        if prepare_snippets(&row.snippets).is_empty() {
            return;
        }
        let full_path = join_row_path(&row.path, &row.name);

        let already_pinned = self
            .results
            .snippet_preview
            .as_ref()
            .is_some_and(|preview| {
                preview.row_path == full_path && preview.trigger == SnippetPreviewTrigger::Pinned
            });
        if already_pinned {
            self.results.snippet_preview = None;
        } else {
            let row_height = f32::from(layout::row_height(window));
            let current = self.scrolled_pixels();
            self.results.table.scroll_top = current;
            self.results.table.ensure_row_visible(ix, row_height);
            if self.results.table.scroll_top != current {
                self.results
                    .rows_scroll
                    .set_offset(point(px(0.0), -px(self.results.table.scroll_top)));
            }
            self.results.snippet_preview = Some(SnippetPreviewState {
                row_path: full_path,
                trigger: SnippetPreviewTrigger::Pinned,
                badge_hovered: false,
                card_hovered: false,
            });
        }
        self.results.results_focus.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn click_snippet_badge(
        &mut self,
        ix: usize,
        full_path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.results.table.focused_idx != Some(ix) {
            self.results.table.select_row(ix, false);
        }
        self.results.keyboard_navigation = false;
        let already_pinned = self
            .results
            .snippet_preview
            .as_ref()
            .is_some_and(|preview| {
                preview.row_path == full_path && preview.trigger == SnippetPreviewTrigger::Pinned
            });
        self.results.snippet_preview = if already_pinned {
            None
        } else {
            Some(SnippetPreviewState {
                row_path: full_path.to_string(),
                trigger: SnippetPreviewTrigger::Pinned,
                badge_hovered: true,
                card_hovered: false,
            })
        };
        self.results.results_focus.focus(window, cx);
        cx.notify();
    }

    fn schedule_hover_dismiss(&self, cx: &mut Context<Self>) {
        let view = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = view.update(cx, |this, cx| this.close_unhovered_snippet_preview(cx));
        });
    }

    pub(crate) fn hover_snippet_badge(
        &mut self,
        full_path: &str,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        if hovered {
            match &mut self.results.snippet_preview {
                Some(preview)
                    if preview.row_path == full_path
                        && preview.trigger == SnippetPreviewTrigger::Pinned =>
                {
                    preview.badge_hovered = true;
                }
                _ => {
                    self.results.snippet_preview = Some(SnippetPreviewState {
                        row_path: full_path.to_string(),
                        trigger: SnippetPreviewTrigger::Hover,
                        badge_hovered: true,
                        card_hovered: false,
                    });
                    cx.notify();
                }
            }
        } else if let Some(preview) = &mut self.results.snippet_preview {
            if preview.row_path == full_path {
                preview.badge_hovered = false;
                if preview.trigger == SnippetPreviewTrigger::Hover {
                    self.schedule_hover_dismiss(cx);
                }
            }
        }
    }

    pub(crate) fn hover_snippet_card(
        &mut self,
        full_path: &str,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(preview) = &mut self.results.snippet_preview {
            if preview.row_path == full_path {
                preview.card_hovered = hovered;
                if !hovered && preview.trigger == SnippetPreviewTrigger::Hover {
                    self.schedule_hover_dismiss(cx);
                }
            }
        }
    }

    fn close_unhovered_snippet_preview(&mut self, cx: &mut Context<Self>) {
        if let Some(preview) = &self.results.snippet_preview {
            if preview.trigger == SnippetPreviewTrigger::Hover
                && !preview.badge_hovered
                && !preview.card_hovered
            {
                self.results.snippet_preview = None;
                cx.notify();
            }
        }
    }

    pub(crate) fn dismiss_snippet_preview(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.results.snippet_preview.take().is_some() {
            self.results.results_focus.focus(window, cx);
            cx.notify();
            return true;
        }
        false
    }

    pub(crate) fn render_snippet_badge_popover(
        &self,
        ix: usize,
        full_path: &str,
        snippets: Vec<String>,
        tokens: &ShellTokens,
        search_focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let preview_state = self
            .results
            .snippet_preview
            .as_ref()
            .filter(|preview| preview.row_path == full_path);
        let is_open = preview_state.is_some();
        let is_hover_trigger =
            preview_state.is_some_and(|preview| preview.trigger == SnippetPreviewTrigger::Hover);

        let badge_label = if snippets.len() > 1 {
            format!(
                "{} ({})",
                self.cached_text("result_content_match"),
                snippets.len()
            )
        } else {
            self.cached_text("result_content_match").to_string()
        };

        let click_path = full_path.to_string();
        let hover_path = full_path.to_string();
        let card_path = full_path.to_string();
        let close_path = full_path.to_string();
        let view = cx.entity().downgrade();

        let trigger = SnippetBadgeTrigger {
            selected: is_open,
            inner: div()
                .id(element_id("result-content-match", full_path))
                .flex_shrink_0()
                .px(rems(layout::SPACE_4))
                .rounded(tokens.radius)
                .cursor_pointer()
                .bg(if is_open {
                    tokens.selection
                } else {
                    tokens.muted
                })
                .text_sm()
                .text_color(if is_open {
                    tokens.foreground
                } else {
                    tokens.muted_foreground
                })
                .hover(|s| s.text_color(tokens.foreground))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        this.click_snippet_badge(ix, &click_path, window, cx);
                    }),
                )
                .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                    this.hover_snippet_badge(&hover_path, *hovered, cx);
                }))
                .child(badge_label),
        };

        let track_focus = if is_hover_trigger && search_focused {
            self.query.search_focus.clone()
        } else {
            self.results.results_focus.clone()
        };

        let terms =
            query_content_highlight_terms(&self.query.search_input.text, &self.query_defaults());
        let tokens_copy = *tokens;
        let popover_path = full_path.to_string();
        let view_for_open = view.clone();
        let view_for_card = view;

        Popover::new(element_id("snippet-popover", full_path))
            .anchor(Anchor::TopLeft)
            .offset(px(POPOVER_OFFSET_PX))
            .appearance(true)
            .overlay_closable(true)
            .open(is_open)
            .track_focus(&track_focus)
            .on_open_change(move |next_open, window, cx| {
                if !*next_open {
                    let close_path = close_path.clone();
                    let _ = view_for_open.update(cx, |this, cx| {
                        let matches_current = this
                            .results
                            .snippet_preview
                            .as_ref()
                            .is_some_and(|preview| preview.row_path == close_path);
                        if matches_current {
                            let keep_search_focus = this
                                .results
                                .snippet_preview
                                .as_ref()
                                .is_some_and(|p| p.trigger == SnippetPreviewTrigger::Hover)
                                && this.query.search_focus.is_focused(window);
                            this.results.snippet_preview = None;
                            if !keep_search_focus {
                                this.results.results_focus.focus(window, cx);
                            }
                            cx.notify();
                        }
                    });
                }
            })
            .trigger(trigger)
            .content(move |_, _window, _cx| {
                let card_path = card_path.clone();
                let view_for_hover = view_for_card.clone();
                v_flex()
                    .id(element_id("snippet-popover-body", &popover_path))
                    .min_w(rems(POPOVER_MIN_WIDTH))
                    .max_w(rems(POPOVER_MAX_WIDTH))
                    .gap(rems(layout::SPACE_8))
                    .text_sm()
                    .text_color(tokens_copy.muted_foreground)
                    .on_hover(move |hovered, _window, cx| {
                        let _ = view_for_hover.update(cx, |this, cx| {
                            this.hover_snippet_card(&card_path, *hovered, cx);
                        });
                    })
                    .children(snippets.iter().enumerate().map(|(idx, snippet)| {
                        let ranges = snippet_highlight_ranges(snippet, &terms);
                        let styled = styled_highlighted_text(snippet, &ranges, &tokens_copy);
                        v_flex()
                            .w_full()
                            .gap(rems(layout::SPACE_8))
                            .when(idx > 0, |this| {
                                this.child(div().h(px(0.5)).w_full().bg(tokens_copy.border))
                            })
                            .child(div().w_full().child(styled))
                    }))
            })
            .into_any_element()
    }
}
