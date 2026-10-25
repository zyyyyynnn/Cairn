// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Query orchestration: search triggering, query lifecycle, paging requests, and highlight slicing.

use crate::app::{layout, CairnApp, HighlightSpan};
use crate::service::ipc_tasks::{get_tokio_runtime, ServiceIssue};
use crate::shell::menu_actions::sort_column_key;
use gpui_kit::gpui::{AsyncApp, Context, Pixels, Point};
use std::ops::Range;

/// Slices a file name into alternating highlighted and plain spans based on byte offsets.
pub fn split_name_highlights<'a>(
    name: &'a str,
    highlights: &[(usize, usize)],
) -> Vec<HighlightSpan<'a>> {
    if name.is_empty() {
        return Vec::new();
    }
    if highlights.is_empty() {
        return vec![HighlightSpan {
            text: name,
            is_highlight: false,
        }];
    }

    // Fast path: for small highlight arrays (>=99% of searches), avoid all heap allocations
    // for filtering and merging highlight intervals.
    let (merged_buf, merged_len): ([(usize, usize); 8], usize) = if highlights.len() <= 8 {
        let mut valid = [(0usize, 0usize); 8];
        let mut valid_count = 0;
        for &(s, e) in highlights {
            if s < e && e <= name.len() && name.is_char_boundary(s) && name.is_char_boundary(e) {
                valid[valid_count] = (s, e);
                valid_count += 1;
            }
        }
        if valid_count == 0 {
            return vec![HighlightSpan {
                text: name,
                is_highlight: false,
            }];
        }
        valid[..valid_count].sort_unstable_by_key(|&(s, _)| s);
        let mut merged = [(0usize, 0usize); 8];
        let mut m_count = 0;
        for &(s, e) in &valid[..valid_count] {
            if m_count > 0 && s <= merged[m_count - 1].1 {
                merged[m_count - 1].1 = merged[m_count - 1].1.max(e);
            } else {
                merged[m_count] = (s, e);
                m_count += 1;
            }
        }
        (merged, m_count)
    } else {
        let mut valid: Vec<(usize, usize)> = highlights
            .iter()
            .copied()
            .filter(|&(s, e)| {
                s < e && e <= name.len() && name.is_char_boundary(s) && name.is_char_boundary(e)
            })
            .collect();

        if valid.is_empty() {
            return vec![HighlightSpan {
                text: name,
                is_highlight: false,
            }];
        }

        valid.sort_unstable_by_key(|&(s, _)| s);

        let mut merged_vec: Vec<(usize, usize)> = Vec::new();
        for (s, e) in valid {
            if let Some(last) = merged_vec.last_mut() {
                if s <= last.1 {
                    last.1 = last.1.max(e);
                } else {
                    merged_vec.push((s, e));
                }
            } else {
                merged_vec.push((s, e));
            }
        }
        let mut spans = Vec::with_capacity(merged_vec.len() * 2 + 1);
        let mut pos = 0;
        for (s, e) in merged_vec {
            if s > pos {
                spans.push(HighlightSpan {
                    text: &name[pos..s],
                    is_highlight: false,
                });
            }
            spans.push(HighlightSpan {
                text: &name[s..e],
                is_highlight: true,
            });
            pos = e;
        }
        if pos < name.len() {
            spans.push(HighlightSpan {
                text: &name[pos..],
                is_highlight: false,
            });
        }
        return spans;
    };

    let mut spans = Vec::with_capacity(merged_len * 2 + 1);
    let mut pos = 0;
    for &(s, e) in &merged_buf[..merged_len] {
        if s > pos {
            spans.push(HighlightSpan {
                text: &name[pos..s],
                is_highlight: false,
            });
        }
        spans.push(HighlightSpan {
            text: &name[s..e],
            is_highlight: true,
        });
        pos = e;
    }
    if pos < name.len() {
        spans.push(HighlightSpan {
            text: &name[pos..],
            is_highlight: false,
        });
    }

    spans
}

/// Whether anything at all narrows the result set.
pub fn narrows_the_result_set(text: &str, drive: Option<char>, filter_macro: Option<&str>) -> bool {
    !text.trim().is_empty() || drive.is_some() || filter_macro.is_some()
}

/// Whether the typed text carries a `content:` term. The service's own parser decides, so quoted
/// literals and grouped terms follow the same grammar as the executed query.
pub fn query_has_content(text: &str) -> bool {
    !cairn_core::syntax::parse_query(text.trim())
        .extract_content_queries()
        .is_empty()
}

/// Whether a page response still belongs to the query and session that requested it.
///
/// A new query can arrive while a page is in flight. Rows from the superseded session describe a
/// result set the table no longer shows, so they must not be spliced into the new window even when
/// the query id happens to match.
fn page_response_is_current(
    requested_query: u64,
    current_query: u64,
    requested_session: u64,
    current_session: Option<u64>,
) -> bool {
    requested_query == current_query && current_session == Some(requested_session)
}

impl CairnApp {
    /// `true` when there is something the service can answer: typed text, or a drive the sidebar
    /// scoped. A bare flag with no query is not a search, and running one would show an empty table
    /// under a query box the user can see is empty.
    pub fn has_query(&self) -> bool {
        narrows_the_result_set(
            &self.query.search_input.text,
            self.query.scope_drive,
            self.query.active_filter.query_macro(),
        )
    }

    /// `true` when the typed query asks for a full-text term.
    pub fn has_content_query(&self) -> bool {
        query_has_content(&self.query.search_input.text)
    }

    /// Pixels already scrolled into the row band, read from the band's own scroll owner. GPUI
    /// stores the offset as a negative translation, so scrolling down raises this value.
    pub fn scrolled_pixels(&self) -> f32 {
        f32::from(-self.results.rows_scroll.offset().y).max(0.0)
    }

    pub fn visible_rows(&self, row_height: Pixels) -> Range<usize> {
        self.results.table.visible_range(f32::from(row_height))
    }

    /// Requests the page that makes the visible band available (QRY-07). Driven by wheel, pointer,
    /// keyboard and scrollbar offsets; the render pass watches the latter through
    /// [`Self::needs_visible_rows_page`] and schedules this after the frame.
    pub fn ensure_visible_rows_loaded(&mut self, row_height: Pixels, cx: &mut Context<Self>) {
        // The Recycle Bin holds its whole listing in memory; a page comes from that
        // listing, never from the service, so even a stopped service can show it.
        if self.recycle_bin.is_some() {
            self.fill_recycle_bin_page(row_height, cx);
            return;
        }
        if self.query.is_searching
            || self.query.is_fetching_more
            || self.query.active_session_id.is_none()
        {
            return;
        }
        // Read the scroll owner now instead of the value the previous frame published: a wheel event
        // moves the handle before this runs, and a page request based on the old offset would fetch
        // rows the reader already scrolled past.
        self.results
            .table
            .set_scroll_top(self.scrolled_pixels(), f32::from(row_height));
        let Some((offset, limit)) = self
            .results
            .table
            .page_request(self.visible_rows(row_height), layout::RESULT_PAGE_SIZE)
        else {
            return;
        };
        self.fetch_rows(offset, limit, cx);
    }

    /// `true` when this frame shows rows the fetched window does not hold and no page is in flight.
    ///
    /// Only the offset owners can know when to page: wheel and keyboard call
    /// [`Self::ensure_visible_rows_loaded`] directly, while a scrollbar drag writes the scroll
    /// handle without an event the app sees. The render pass uses this predicate to schedule one
    /// post-frame ensure, so every scroll source — pointer drag, wheel, keyboard, first page — is
    /// covered without fetching from `render` itself.
    pub fn needs_visible_rows_page(&self, visible: Range<usize>) -> bool {
        if self.recycle_bin.is_some() {
            return self.needs_recycle_bin_page(visible);
        }
        self.service.is_service_connected
            && !self.query.is_searching
            && !self.service.is_probing_service
            && !self.query.is_fetching_more
            && self.query.active_session_id.is_some()
            && self
                .results
                .table
                .page_request(visible, layout::RESULT_PAGE_SIZE)
                .is_some()
    }

    pub fn trigger_search(&mut self, cx: &mut Context<Self>) {
        self.run_query(cx, true);
    }

    /// Re-asks the active query while a growing index adds rows, keeping what the table already
    /// shows on screen. Used by the local scan's status refresh: a refresh that cleared the table
    /// would replace live rows with the skeleton once the query takes longer than the poll
    /// interval, which is how a progressive index turns into a loading animation that never ends.
    /// Skipped while a query is already in flight; that answer is newer than the request would be.
    pub fn refresh_results(&mut self, cx: &mut Context<Self>) {
        // A background index refresh never replaces the location the person is
        // reading; only an explicit query leaves the Recycle Bin view.
        if self.query.is_searching || self.recycle_bin.is_some() {
            return;
        }
        self.run_query(cx, false);
    }

    fn run_query(&mut self, cx: &mut Context<Self>, clear_rows: bool) {
        // While the bin is open the search box is an in-place filter over the
        // shell listing, not a request for an index page. Typing, paste, undo
        // and IME commit all arrive here, so this one branch keeps the location
        // stable for every text path.
        if self.recycle_bin.is_some() {
            self.query.query_id += 1;
            self.query.is_searching = false;
            self.filter_recycle_bin(cx);
            return;
        }
        self.query.query_id += 1;
        let qid = self.query.query_id;

        if clear_rows {
            // A new result set starts at its first row. The view republishes the scroll handle's
            // offset into `table.scroll_top` every frame, so resetting the table alone would be
            // overwritten by the previous result's offset on the next render; the handle has to
            // return to the top too.
            self.results.rows_scroll.set_offset(Point::default());
            self.results.table.scroll_top = 0.0;
            self.results.table.set_rows(Vec::new(), 0);
            self.query.active_session_id = None;
        }

        // An empty query is not an idle state: it is the "所有文件" view. The service answers it
        // with the whole index, so startup, service reconnect and Escape all land here and show
        // the table rather than a "start typing" placeholder. The skeleton covers the first page.
        self.query.is_searching = true;
        // Any index query leaves the Recycle Bin view: the bin is a location,
        // not a search scope. Its filtered listing is released with the view.
        self.recycle_bin = None;
        self.recycle_bin_loading = false;
        self.recycle_bin_all = Vec::new();
        self.recycle_bin_icon_extensions.clear();
        self.recycle_bin_selected_keys = std::collections::HashSet::new();
        self.recycle_bin_error = None;
        cx.notify();

        let raw_text = self.query.search_input.text.trim();
        let query_defaults = cairn_protocol::message::QueryDefaults {
            case_sensitive: self.query.case_sensitive,
            whole_words: self.query.whole_word,
            match_path: self.query.match_path,
            is_regex: self.query.use_regex,
            // Content is a query syntax (`content:`), not a modal toggle. The service gates it on
            // the master switch; the UI no longer carries a second content control.
            content: false,
            exclude_hidden: false,
            exclude_system: false,
        };
        let mut q = raw_text.to_string();
        if let Some(mac) = self.query.active_filter.query_macro() {
            q = format!("{mac} {q}");
        }
        if let Some(drive) = self.query.scope_drive {
            q = format!("{}: {q}", drive.to_ascii_uppercase());
        }

        let sort_by = sort_column_key(self.results.table.sort_column).to_string();
        let sort_asc = self.results.table.sort_asc;
        let page_size = layout::RESULT_PAGE_SIZE;

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::shell::perf::mark("query start");
            get_tokio_runtime().spawn(async move {
                let started = std::time::Instant::now();
                let res = match crate::service::ipc_channel::connect().await {
                    Ok(mut client) => {
                        client
                            .query(qid, &q, query_defaults, &sort_by, sort_asc, page_size)
                            .await
                    }
                    Err(e) => Err(e),
                };
                crate::shell::perf::mark(&format!(
                    "query ipc {:.3}ms",
                    started.elapsed().as_secs_f64() * 1000.0
                ));
                let _ = tx.send(res);
            });

            if let Ok(res) = rx.await {
                this.update(cx, |this: &mut CairnApp, cx| {
                    if this.query.query_id != qid {
                        return;
                    }
                    this.query.is_searching = false;
                    match res {
                        Ok(envelope) => {
                            this.service.is_service_connected = true;
                            this.service.service_issue = None;
                            this.service.status.volumes = envelope.volumes;
                            this.start_status_refresh(cx);
                            if let cairn_protocol::message::ResponsePayload::QueryResult {
                                session_id,
                                total_matches,
                                first_page,
                                ..
                            } = envelope.payload
                            {
                                this.query.active_session_id = Some(session_id);
                                if clear_rows {
                                    this.results.table.set_rows(first_page, total_matches);
                                } else {
                                    this.results.table.refresh_rows(first_page, total_matches);
                                }
                                crate::shell::perf::mark(&format!(
                                    "query applied id={qid} total={total_matches}"
                                ));
                                this.service.status.total_objects = total_matches;
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Cairn IPC query error: {}", e);
                            this.service.is_service_connected = false;
                            this.service.service_issue = Some(ServiceIssue::from_client_error(&e));
                        }
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// Fetches one `(offset, limit)` window of the active session and installs it.
    pub fn fetch_rows(&mut self, offset: usize, limit: usize, cx: &mut Context<Self>) {
        let Some(session_id) = self.query.active_session_id else {
            return;
        };
        self.query.is_fetching_more = true;
        let current_qid = self.query.query_id;
        // A page fetched after a background refresh replaces the stale window; remember which
        // offset that is so a response already in flight cannot be mistaken for it.
        self.results.table.note_page_request(offset);

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            get_tokio_runtime().spawn(async move {
                let res = match crate::service::ipc_channel::connect().await {
                    Ok(mut client) => client.get_rows(session_id, offset, limit).await,
                    Err(e) => Err(e),
                };
                let _ = tx.send(res);
            });

            let res = rx.await;
            this.update(cx, |this: &mut CairnApp, cx| {
                this.query.is_fetching_more = false;
                // A new query replaces the session. Rows from the old session belong to a result
                // set this table no longer shows, even when the query id still matches.
                if !page_response_is_current(
                    current_qid,
                    this.query.query_id,
                    session_id,
                    this.query.active_session_id,
                ) {
                    return;
                }
                match res {
                    Ok(Ok(envelope)) => match envelope.payload {
                        cairn_protocol::message::ResponsePayload::RowsResult { rows, .. } => {
                            this.results.table.apply_page(offset, rows);
                        }
                        cairn_protocol::message::ResponsePayload::Error { code, .. }
                            if code == cairn_protocol::message::ERROR_CODE_SESSION_EXPIRED =>
                        {
                            this.results.table.clear_empty_pages();
                            this.refresh_results(cx);
                            return;
                        }
                        _ => {}
                    },
                    Ok(Err(e)) => {
                        tracing::warn!("Cairn page fetch error: {e}");
                        if !matches!(e, cairn_client::ClientError::Timeout) {
                            this.service.is_service_connected = false;
                            this.service.service_issue = Some(ServiceIssue::from_client_error(&e));
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Cairn page fetch task lost: {e}");
                        this.service.is_service_connected = false;
                        this.service.service_issue = Some(ServiceIssue::NotRunning);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_name_highlights_empty_and_no_match() {
        assert_eq!(split_name_highlights("", &[]), Vec::new());
        assert_eq!(split_name_highlights("", &[(0, 1)]), Vec::new());

        let spans = split_name_highlights("test.rs", &[]);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "test.rs");
        assert!(!spans[0].is_highlight);
    }

    #[test]
    fn test_split_name_highlights_single_and_overlapping() {
        let spans = split_name_highlights("hello world", &[(0, 5)]);
        assert_eq!(
            spans,
            vec![
                HighlightSpan {
                    text: "hello",
                    is_highlight: true,
                },
                HighlightSpan {
                    text: " world",
                    is_highlight: false,
                },
            ]
        );

        let spans = split_name_highlights("hello world", &[(0, 4), (2, 7)]);
        assert_eq!(
            spans,
            vec![
                HighlightSpan {
                    text: "hello w",
                    is_highlight: true,
                },
                HighlightSpan {
                    text: "orld",
                    is_highlight: false,
                },
            ]
        );
    }

    const CJK_FILE_NAME: &str = "\u{9879}\u{76EE}\u{67B6}\u{6784}\u{8BBE}\u{8BA1}.pdf";
    const CJK_PREFIX: &str = "\u{9879}\u{76EE}";
    const CJK_MATCH: &str = "\u{67B6}\u{6784}";
    const CJK_SUFFIX: &str = "\u{8BBE}\u{8BA1}.pdf";

    #[test]
    fn test_split_name_highlights_utf8_chinese() {
        let spans = split_name_highlights(CJK_FILE_NAME, &[(6, 12)]);
        assert_eq!(
            spans,
            vec![
                HighlightSpan {
                    text: CJK_PREFIX,
                    is_highlight: false,
                },
                HighlightSpan {
                    text: CJK_MATCH,
                    is_highlight: true,
                },
                HighlightSpan {
                    text: CJK_SUFFIX,
                    is_highlight: false,
                },
            ]
        );
    }

    const CJK_BOUNDARY_NAME: &str = "\u{4E2D}\u{6587}\u{6D4B}\u{8BD5}";

    #[test]
    fn test_split_name_highlights_invalid_boundaries() {
        let spans = split_name_highlights(CJK_BOUNDARY_NAME, &[(1, 4), (0, 999)]);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, CJK_BOUNDARY_NAME);
        assert!(!spans[0].is_highlight);
    }

    #[test]
    fn only_a_real_content_term_asks_for_the_content_index() {
        use super::query_has_content;

        assert!(query_has_content("content:架构"));
        assert!(query_has_content("ext:md content:<two words>"));
        assert!(
            !query_has_content("\"content:架构\""),
            "a quoted literal is a file-name term, not a content constraint"
        );
        assert!(!query_has_content("notes"));
        assert!(!query_has_content(""));
    }

    #[test]
    fn a_type_filter_counts_as_a_query_without_any_text() {
        assert!(
            narrows_the_result_set("", None, Some("exe:")),
            "可执行文件 with an empty box is still a constraint"
        );
        assert!(narrows_the_result_set("report", None, None));
        assert!(narrows_the_result_set("", Some('c'), None));
        assert!(
            !narrows_the_result_set("", None, None),
            "no text, no scope and 所有文件 is the idle state"
        );
        assert!(
            !narrows_the_result_set("   ", None, None),
            "whitespace alone is not a query"
        );
    }

    #[test]
    fn a_page_from_a_superseded_session_is_not_current() {
        assert!(page_response_is_current(7, 7, 11, Some(11)));
        assert!(
            !page_response_is_current(7, 7, 11, Some(12)),
            "a new query's session owns the table, not the session that was paged before it"
        );
        assert!(!page_response_is_current(7, 8, 11, Some(11)));
        assert!(!page_response_is_current(7, 7, 11, None));
    }

    /// A new query resets the result scroll position: `set_rows` zeroes
    /// `table.scroll_top`, but the next render republishes the scroll handle's offset into it, so
    /// both owners have to return to the top before the new query is spawned.
    #[test]
    fn a_new_query_resets_the_result_band() {
        let source = include_str!("query_orchestration.rs");
        let trigger = source
            .split("pub fn trigger_search")
            .nth(1)
            .expect("trigger_search must exist");
        let body = trigger
            .split("cx.spawn")
            .next()
            .expect("the synchronous part precedes the spawn");
        assert!(
            body.contains("rows_scroll.set_offset(Point::default())"),
            "a new query must reset the scroll handle the render pass reads"
        );
        assert!(
            body.contains("table.scroll_top = 0.0"),
            "a new query must reset the table's published scroll position"
        );
        assert!(
            body.contains("active_session_id = None"),
            "a new query must clear the old session id before the response lands"
        );
    }

    #[test]
    fn page_fetch_error_handling_covers_session_expired() {
        let source = include_str!("query_orchestration.rs");
        assert!(
            source.contains("ERROR_CODE_SESSION_EXPIRED"),
            "query orchestration must handle session expiration"
        );
        assert!(
            source.contains("this.results.table.clear_empty_pages()"),
            "session expiration must clear empty pages"
        );
    }
}
