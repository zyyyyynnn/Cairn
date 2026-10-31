// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! GPUI canvas, tile rendering, progressive scan worker orchestration and
//! pointer/keyboard interaction for the drive Treemap view.

use crate::app::{element_id, layout, CairnApp};
use crate::search::filters::FilterCategory;
use crate::service::ipc_tasks::get_tokio_runtime;
use crate::shell::os::{open_file, open_folder_and_select, show_properties_dialog};
use crate::shell::theme::{self, ShellTokens};
use crate::treemap::paint::{
    paint_rect, snap_rect_to_device_pixels, TREEMAP_RADIUS, TREEMAP_TILE_INSET,
};
use crate::treemap::palette::{block_fill_states, TreemapPalette};
use crate::treemap::state::{next_scan_epoch, TreemapViewState};
use crate::treemap::zoom::TreemapViewport;
use cairn_core::format_size_bytes;
use cairn_core::treemap::{
    aggregate_parent_path, scan_directory_tree_progressive, NestedTreemapLayoutNode,
    TreemapCategory, TreemapRect, TreemapScanEvent,
};
use cairn_protocol::message::ResponsePayload;
use fluent_bundle::FluentArgs;
use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex, Icon, Sizable as _};
use gpui_kit::gpui::prelude::FluentBuilder as _;
use gpui_kit::gpui::*;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// A readable block uses one standard: a leading 12px icon lane plus a name,
/// with the size either trailing (headers) or on the second line (blocks).
/// Anything smaller is a flat category cell. There is no middle state with a
/// title but no icon, or a size without a title.
const TREEMAP_LABEL_MIN_WIDTH: f32 = 44.0;
const TREEMAP_LABEL_MIN_HEIGHT: f32 = 28.0;

/// Depth cap for a tree served by the index. Matches the on-disk volume-index
/// loader, so both service paths present the same hierarchy.
// The response must fit the protocol's frame budget. Depth 2 is ~47k entries on a 1.4M-index
// drive and answers in ~160 ms; depth 6 overflows the 16 MiB frame and drops the connection,
// which forced the six-thread filesystem fallback. Deeper levels are requested per directory as
// the reader drills in.
const TREEMAP_SERVICE_DEPTH: usize = 2;

/// Horizontal inset used by headers, leaves and summary labels.
const TREEMAP_TEXT_PADDING: f32 = 4.0;
/// Leading icon (12px) plus its 2px gap.
const TREEMAP_ICON_SPACE: f32 = 14.0;
/// The header hides its size rather than squeezing the name below this width.
const TREEMAP_MIN_HEADER_NAME_WIDTH: f32 = 28.0;

/// Shapes a label at the size and weight it will be painted, so fit decisions
/// use real glyph advances (CJK, emoji and zoom changes included) instead of a
/// per-character estimate.
fn text_width(window: &Window, text: &str, weight: FontWeight) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    let mut style = window.text_style();
    style.font_size = rems(theme::SMALL_TEXT_SIZE).into();
    style.font_weight = weight;
    style.font_features = theme::tabular_figures();
    let font_size = style.font_size.to_pixels(window.rem_size());
    f32::from(
        window
            .text_system()
            .layout_line(text, font_size, &[style.to_run(text.len())], None)
            .width,
    )
}

/// One wheel line is one zoom step. Windows reports a mouse notch as the
/// system's wheel lines (three by default) and a precision touchpad as pixels;
/// both are normalized here. The horizontal component is ignored because the
/// Treemap has no horizontal pan.
fn wheel_zoom_lines(delta: ScrollDelta) -> f32 {
    match delta {
        ScrollDelta::Pixels(delta) => f32::from(delta.y) / 120.0,
        ScrollDelta::Lines(delta) => delta.y / 3.0,
    }
}

impl CairnApp {
    /// Switches to the drive disk space Treemap visualization.
    pub fn open_drive_visualization(
        &mut self,
        drive: char,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let drive_upper = drive.to_ascii_uppercase();
        if self.recycle_bin.leave() {
            // A bin filter is a location-local search, not a query to carry into the drive view.
            self.query.search_input.clear();
        }
        self.drive_view = Some(drive_upper);
        self.query.scope_drive = Some(drive_upper);
        let (state, scan_target) = TreemapViewState::new(drive_upper, cx.focus_handle());
        state.request_focus(window, cx);
        self.treemap_state = Some(state);
        if let Some(path) = scan_target {
            self.start_treemap_scan(path, cx);
        }
        cx.notify();
    }

    /// Returns the Treemap to 100% without touching the selected node. The
    /// canvas keeps keyboard focus so a following wheel, arrow or Escape still
    /// drives the visualization.
    pub fn reset_treemap_zoom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ref mut st) = self.treemap_state {
            st.zoom.reset_view();
            st.request_focus(window, cx);
            cx.notify();
        }
    }

    /// Starts loading the Treemap for `scan_path`: the live index answers one
    /// pipe round-trip when it covers the path, and the progressive
    /// filesystem scan is the fallback for a closed pipe or an unindexed
    /// drive.
    pub fn start_treemap_scan(&mut self, scan_path: PathBuf, cx: &mut Context<Self>) {
        let Some(ref mut state) = self.treemap_state else {
            return;
        };

        if let Some(cancel) = state.scan_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }

        state.scan_generation = next_scan_epoch();
        let gen = state.scan_generation;
        state.is_scanning = true;
        state.content_revealed = false;
        state.scan_unreadable = false;
        state.layout_nodes.clear();
        state.root_tree = None;
        cx.notify();

        let path_for_service = scan_path.to_string_lossy().into_owned();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            // One round-trip against whichever indexer is selected. Empty
            // entries mean "not indexed here"; any other failure (no pipe,
            // old protocol) lands on the same fallback.
            let (tx, rx) = tokio::sync::oneshot::channel();
            get_tokio_runtime().spawn(async move {
                let result = async {
                    let mut client = crate::service::ipc_channel::connect().await?;
                    client
                        .get_tree(&path_for_service, TREEMAP_SERVICE_DEPTH)
                        .await
                }
                .await;
                let _ = tx.send(result);
            });

            let served = match rx.await {
                Ok(Ok(envelope)) => match envelope.payload {
                    ResponsePayload::TreeResult { entries, .. } if !entries.is_empty() => {
                        crate::shell::perf::mark(&format!(
                            "treemap tree service entries={}",
                            entries.len()
                        ));
                        let applied = this.update(cx, |this, cx| {
                            if let Some(ref mut st) = this.treemap_state {
                                if st.scan_generation != gen {
                                    return false;
                                }
                                let ok = st.adopt_service_tree(entries);
                                cx.notify();
                                ok
                            } else {
                                false
                            }
                        });
                        applied.unwrap_or(false)
                    }
                    _ => false,
                },
                _ => false,
            };
            if served {
                return;
            }

            crate::shell::perf::mark("treemap tree filesystem scan");
            // Fall back to the progressive filesystem scan.
            let fallback = this.update(cx, |this, cx| {
                this.run_treemap_filesystem_scan(scan_path, gen, cx);
            });
            if fallback.is_err() {
                // The view died before the answer arrived; nothing to load.
            }
        })
        .detach();
    }

    /// The std::fs progressive scan fallback: streams Listing/Progress events
    /// through a channel and folds them into the view state.
    fn run_treemap_filesystem_scan(
        &mut self,
        scan_path: PathBuf,
        gen: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(ref mut state) = self.treemap_state else {
            return;
        };
        if state.scan_generation != gen {
            return;
        }

        let cancel = Arc::new(AtomicBool::new(false));
        state.scan_cancel = Some(cancel.clone());
        state.is_scanning = true;
        state.content_revealed = false;
        state.scan_unreadable = false;
        state.layout_nodes.clear();
        cx.notify();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<TreemapScanEvent>();
        let cancel_worker = cancel.clone();
        let target_path = scan_path.clone();

        // Spawn heavy recursive scan on dedicated OS worker thread to avoid blocking UI or Tokio
        std::thread::Builder::new()
            .name("cairn-treemap-scanner".into())
            .spawn(move || {
                scan_directory_tree_progressive(&target_path, 5, cancel_worker, move |evt| {
                    let _ = tx.send(evt);
                });
            })
            .expect("failed to spawn treemap scanner thread");

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            while let Some(event) = rx.recv().await {
                let update_res = this.update(cx, |this: &mut CairnApp, cx| {
                    if let Some(ref mut st) = this.treemap_state {
                        if st.scan_generation != gen {
                            return;
                        }
                        // Progress folds the tree in memory; the canvas shows the skeleton
                        // until the finished event reveals it, and the status band shows no
                        // counters during the scan. Repainting for each batch would cost a
                        // full frame per batch for nothing the reader can see.
                        let finished = event.is_finished;
                        st.apply_scan_event(event);
                        if finished {
                            cx.notify();
                        }
                    }
                });
                if update_res.is_err() {
                    break;
                }
            }

            // The channel closes either after the terminal event or because the
            // scanner died. Never leave the current generation stuck on the
            // loading skeleton: stop the spinner and reveal whatever arrived.
            let _ = this.update(cx, |this: &mut CairnApp, cx| {
                if let Some(ref mut st) = this.treemap_state {
                    if st.scan_generation == gen {
                        if st.is_scanning {
                            st.is_scanning = false;
                            if let Some(tree) = st.root_tree.as_mut() {
                                let tree = Arc::make_mut(tree);
                                tree.compute_aggregated_sizes();
                                tree.sort_recursive();
                                st.recompute_layout();
                            }
                            st.reveal_content();
                        }
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub fn treemap_drill_down(&mut self, target_path: &str, cx: &mut Context<Self>) {
        // A summary block owned by the current root cannot drill anywhere; the
        // old code reloaded the same directory and flashed the empty state.
        if self
            .treemap_state
            .as_ref()
            .is_some_and(|st| same_directory(st.current_dir(), target_path))
        {
            return;
        }
        let scan_target = if let Some(ref mut st) = self.treemap_state {
            st.drill_down(target_path)
        } else {
            None
        };
        if let Some(path) = scan_target {
            self.start_treemap_scan(path, cx);
        }
        cx.notify();
    }

    pub fn treemap_drill_up(&mut self, cx: &mut Context<Self>) {
        let scan_target = if let Some(ref mut st) = self.treemap_state {
            st.drill_up()
        } else {
            None
        };
        if let Some(path) = scan_target {
            self.start_treemap_scan(path, cx);
        }
        cx.notify();
    }

    pub fn treemap_drill_forward(&mut self, cx: &mut Context<Self>) {
        let scan_target = if let Some(ref mut st) = self.treemap_state {
            st.drill_forward()
        } else {
            None
        };
        if let Some(path) = scan_target {
            self.start_treemap_scan(path, cx);
        }
        cx.notify();
    }

    /// Switches back to classic "All files" table filtered by `path`.
    pub fn switch_to_table_with_path(
        &mut self,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Leaving the visualization cancels its progressive scan and releases
        // the partial tree instead of keeping a hidden worker alive.
        self.drive_view = None;
        self.treemap_state = None;
        self.query.active_filter = FilterCategory::All;
        let trimmed = path.trim_end_matches('\\');
        let query_path = if trimmed.len() <= 2 && trimmed.ends_with(':') {
            format!("parent:\"{}:\\\"", trimmed.chars().next().unwrap())
        } else {
            format!("parent:\"{}\"", trimmed)
        };
        self.query.search_input.set_text(&query_path);
        self.trigger_search(cx);
        self.query.search_focus.focus(window, cx);
        cx.notify();
    }

    /// Renders the main Hierarchical Nested Treemap surface.
    pub fn render_treemap_body(
        &mut self,
        tokens: &ShellTokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(ref mut state) = self.treemap_state else {
            return div().into_any_element();
        };

        // Measure available content bounds
        let window_bounds = window.bounds();
        // The shell lays its boxes out border-box, so the 1px hairlines are
        // already inside these band widths; counting them again would shift
        // every pointer-to-base projection by a pixel or two.
        let sidebar_w = if self.sidebar_collapsed {
            0.0
        } else {
            f32::from(theme::rem(window) * layout::SIDEBAR_WIDTH)
        };
        let avail_w = (f32::from(window_bounds.size.width) - sidebar_w).max(100.0);
        let chrome_h = f32::from(layout::TITLE_BAR_HEIGHT)
            + f32::from(theme::rem(window) * layout::STATUS_HEIGHT);
        let avail_h = (f32::from(window_bounds.size.height) - chrome_h).max(100.0);
        // Canvas-space pointer coordinates start below the title bar and right
        // of the sidebar; the layout bounds above already use the same box.
        let canvas_origin = (sidebar_w, f32::from(layout::TITLE_BAR_HEIGHT));
        let canvas_size = (avail_w, avail_h);
        state.last_scale_factor = window.scale_factor().max(1.0);

        if (state.last_bounds.0 - canvas_size.0).abs() > 0.5
            || (state.last_bounds.1 - canvas_size.1).abs() > 0.5
        {
            state.last_bounds = canvas_size;
            state.recompute_layout();
        }

        let is_dark = self.settings.is_dark(self.system_dark);
        let viewport = state.zoom.viewport();
        let panning = state.zoom.is_panning();
        let view = cx.entity();
        let pan_view = view.clone();
        let focus = state.focus_handle().cloned();

        // The skeleton covers the whole scan: the canvas has no intermediate
        // state. Scan progress is reported by the status bar, not here; a
        // later navigation also clears the old layout, and until the new
        // listing arrives the same skeleton stands in for a false "empty
        // folder".
        //
        // The blocks are still. A scan can last minutes, and the design system's Skeleton
        // animates at the display rate; the status band's mark is the one moving element,
        // capped so a long build does not repaint the window at the display rate.
        if shows_loading_skeleton(state.content_revealed, state.is_scanning) {
            return v_flex()
                .id("treemap-loading-skeleton")
                .size_full()
                .p(rems(layout::CONTENT_INSET))
                .child(
                    h_flex()
                        .w_full()
                        .h_full()
                        .gap_x(rems(layout::SPACE_4))
                        .child(
                            div()
                                .h_full()
                                .w(relative(0.65))
                                .flex_shrink_0()
                                .rounded(px(4.0))
                                .bg(tokens.skeleton),
                        )
                        .child(
                            v_flex()
                                .h_full()
                                .flex_1()
                                .gap_y(rems(layout::SPACE_4))
                                .child(
                                    div()
                                        .flex_1()
                                        .w_full()
                                        .rounded(px(4.0))
                                        .bg(tokens.skeleton.opacity(0.5)),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .w_full()
                                        .rounded(px(4.0))
                                        .bg(tokens.skeleton.opacity(0.5)),
                                ),
                        ),
                )
                .into_any_element();
        }

        let render_cx = TreemapRenderContext {
            tokens,
            palette: TreemapPalette::for_mode(is_dark),
            selected_path: state.selected_path.as_deref(),
            category_filter: state.category_filter,
            view: &view,
            i18n: &self.i18n,
            window,
            panning,
        };

        // An empty directory is content, not a separate surface: it must keep
        // the canvas focus, Backspace/Enter and wheel zoom.
        let content = if state.layout_nodes.is_empty() {
            let empty_text_key = if state.is_unreadable() {
                "treemap_unreadable"
            } else {
                "treemap_empty_directory"
            };
            v_flex()
                .absolute()
                .left(px(0.0))
                .top(px(0.0))
                .size_full()
                .items_center()
                .justify_center()
                .text_color(tokens.muted_foreground)
                .text_sm()
                .child(self.i18n.get_text(empty_text_key))
                .into_any_element()
        } else {
            let mut block_elements = Vec::with_capacity(state.layout_nodes.len() * 4);
            for node in &state.layout_nodes {
                render_nested_node(node, &render_cx, viewport, canvas_size, &mut block_elements);
            }
            div()
                .absolute()
                .left(px(0.0))
                .top(px(0.0))
                .size_full()
                .children(block_elements)
                .into_any_element()
        };

        let mut canvas_element = v_flex()
            .id("treemap-canvas-container")
            .relative()
            .flex_1()
            .w_full()
            .h_full()
            .overflow_hidden();
        if let Some(ref focus) = focus {
            canvas_element = canvas_element.track_focus(focus);
        }
        let canvas_element = canvas_element
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _event: &MouseDownEvent, window, cx| {
                    if let Some(ref mut st) = this.treemap_state {
                        st.clear_selection();
                        st.request_focus(window, cx);
                        cx.notify();
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    let cursor = (
                        f32::from(event.position.x) - canvas_origin.0,
                        f32::from(event.position.y) - canvas_origin.1,
                    );
                    if let Some(ref mut st) = this.treemap_state {
                        if st.zoom.begin_pan(cursor) {
                            st.request_focus(window, cx);
                            cx.notify();
                        }
                    }
                    cx.stop_propagation();
                }),
            )
            .on_scroll_wheel(
                cx.listener(move |this, event: &ScrollWheelEvent, window, cx| {
                    let lines = wheel_zoom_lines(event.delta);
                    let enter_path = if let Some(ref mut st) = this.treemap_state {
                        if lines == 0.0 {
                            None
                        } else {
                            let cursor = (
                                f32::from(event.position.x) - canvas_origin.0,
                                f32::from(event.position.y) - canvas_origin.1,
                            );
                            let outcome = st.zoom.wheel(
                                &st.layout_nodes,
                                lines,
                                cursor,
                                canvas_size,
                                TREEMAP_TILE_INSET,
                                Instant::now(),
                            );
                            // Any vertical wheel claims the canvas focus so a
                            // following Backspace/Enter acts on what was just
                            // inspected.
                            st.request_focus(window, cx);
                            let enter_path = outcome.enter_path().map(str::to_string);
                            if enter_path.is_none() && outcome.changed() {
                                cx.notify();
                            }
                            enter_path
                        }
                    } else {
                        None
                    };
                    if let Some(path) = enter_path {
                        this.treemap_drill_down(&path, cx);
                    }
                    cx.stop_propagation();
                }),
            )
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.modifiers.control {
                    match event.keystroke.key.as_str() {
                        "0" => {
                            if let Some(ref mut st) = this.treemap_state {
                                st.zoom.reset_view();
                                st.request_focus(window, cx);
                                cx.notify();
                            }
                        }
                        "=" | "+" => {
                            if let Some(ref mut st) = this.treemap_state {
                                if st.zoom.zoom_center(1.0, canvas_size) {
                                    st.request_focus(window, cx);
                                    cx.notify();
                                }
                            }
                        }
                        "-" | "_" => {
                            if let Some(ref mut st) = this.treemap_state {
                                if st.zoom.zoom_center(-1.0, canvas_size) {
                                    st.request_focus(window, cx);
                                    cx.notify();
                                }
                            }
                        }
                        _ => {}
                    }
                    return;
                }
                if event.keystroke.modifiers.alt && event.keystroke.key.as_str() == "enter" {
                    if let Some(ref st) = this.treemap_state {
                        if let Some(path) = st.selected_real_path() {
                            let _ = show_properties_dialog(path);
                        }
                    }
                    return;
                }
                match event.keystroke.key.as_str() {
                    // Plain arrows pan the viewport by one tenth. At identity
                    // they are consumed without moving, so they can never fall
                    // through to a hidden table. Alt+Left/Right stay navigation.
                    "left" | "right" | "up" | "down" if !event.keystroke.modifiers.alt => {
                        if let Some(ref mut st) = this.treemap_state {
                            let step_x = canvas_size.0 * 0.1;
                            let step_y = canvas_size.1 * 0.1;
                            let (dx, dy) = match event.keystroke.key.as_str() {
                                "left" => (-step_x, 0.0),
                                "right" => (step_x, 0.0),
                                "up" => (0.0, -step_y),
                                _ => (0.0, step_y),
                            };
                            if st.zoom.pan_view(dx, dy, canvas_size) {
                                cx.notify();
                            }
                        }
                        cx.stop_propagation();
                    }
                    "backspace" => {
                        this.treemap_drill_up(cx);
                    }
                    "enter" => {
                        let mut drill = None;
                        if let Some(ref st) = this.treemap_state {
                            if let Some(node) = st.selected_node().cloned() {
                                if node.is_aggregate() {
                                    drill = aggregate_parent_path(&node.path).map(str::to_string);
                                } else if node.item.is_dir {
                                    this.treemap_drill_down(&node.path, cx);
                                } else {
                                    let _ = open_file(&node.path);
                                }
                            }
                        }
                        if let Some(parent) = drill {
                            this.treemap_drill_down(&parent, cx);
                        }
                    }
                    "+" | "=" => {
                        if let Some(ref mut st) = this.treemap_state {
                            let lvl = (st.detail_level + 1).min(5);
                            st.set_detail_level(lvl);
                            cx.notify();
                        }
                    }
                    "-" | "_" => {
                        if let Some(ref mut st) = this.treemap_state {
                            let lvl = st.detail_level.saturating_sub(1).max(1);
                            st.set_detail_level(lvl);
                            cx.notify();
                        }
                    }
                    _ => {}
                }
            }))
            .child(content)
            // A drag registers frame-scoped window listeners from this paint
            // callback. They exist only while the pan layer is rendered, so a
            // finished drag cannot leave a stale listener behind.
            .when(panning, |this| {
                let entity = pan_view.clone();
                this.child(
                    div().absolute().size_full().child(
                        canvas(
                            |_, _, _| {},
                            move |_bounds, _, window, _cx| {
                                let move_view = entity.clone();
                                window.on_mouse_event(
                                    move |event: &MouseMoveEvent, phase, _window, cx| {
                                        if phase == DispatchPhase::Capture {
                                            return;
                                        }
                                        let cursor = (
                                            f32::from(event.position.x) - canvas_origin.0,
                                            f32::from(event.position.y) - canvas_origin.1,
                                        );
                                        if event.pressed_button != Some(MouseButton::Middle) {
                                            move_view.update(cx, |this, cx| {
                                                if let Some(ref mut st) = this.treemap_state {
                                                    if st.end_pan() {
                                                        cx.notify();
                                                    }
                                                }
                                            });
                                            return;
                                        }
                                        move_view.update(cx, |this, cx| {
                                            if let Some(ref mut st) = this.treemap_state {
                                                if st.zoom.pan_to(cursor, canvas_size) {
                                                    cx.notify();
                                                }
                                            }
                                        });
                                    },
                                );
                                let up_view = entity.clone();
                                window.on_mouse_event(
                                    move |event: &MouseUpEvent, phase, _window, cx| {
                                        if phase == DispatchPhase::Capture
                                            || event.button != MouseButton::Middle
                                        {
                                            return;
                                        }
                                        up_view.update(cx, |this, cx| {
                                            if let Some(ref mut st) = this.treemap_state {
                                                if st.end_pan() {
                                                    cx.notify();
                                                }
                                            }
                                        });
                                    },
                                );
                            },
                        )
                        .absolute()
                        .inset_0(),
                    ),
                )
            });

        canvas_element.into_any_element()
    }
}

struct TreemapRenderContext<'a> {
    tokens: &'a ShellTokens,
    /// Data-plane category fills. Resolved once per frame from the theme mode;
    /// the shell tokens never carry a category color.
    palette: TreemapPalette,
    selected_path: Option<&'a str>,
    category_filter: [bool; TreemapCategory::COUNT],
    view: &'a Entity<CairnApp>,
    i18n: &'a Rc<crate::shell::i18n::I18nManager>,
    /// Text is shaped at render time so labels are shown only when their real
    /// advance fits the painted block.
    window: &'a Window,
    /// A middle-button drag owns the pointer; hover tooltips would otherwise
    /// pop up under the cursor while the content moves.
    panning: bool,
}

/// Places a rendered node at its exact projected layout rect. Blocks appear
/// at their final coordinates the moment a scan batch lands.
fn place_treemap_node<E>(element: E, rect: TreemapRect) -> AnyElement
where
    E: Styled + IntoElement + 'static,
{
    element
        .absolute()
        .left(px(rect.x))
        .top(px(rect.y))
        .w(px(rect.width))
        .h(px(rect.height))
        .into_any_element()
}

/// The one interaction response shared by header surfaces, file leaves and
/// compact directory leaves: hover and press darken the surface itself, keeping
/// its category hue and saturation. Selection owns the 2px `ring`, so neither
/// state is installed on a selected block. Directory containers themselves never
/// use this, so hovering a child cannot light up its ancestors.
fn with_block_fill_interaction<E>(element: E, base: Hsla, is_dark: bool, is_selected: bool) -> E
where
    E: StatefulInteractiveElement,
{
    if is_selected {
        return element;
    }
    let (hover, pressed) = block_fill_states(base, is_dark);
    element
        .hover(move |this| this.bg(hover))
        .active(move |this| this.bg(pressed))
}

/// Paints a culled tile as a flat category surface instead of leaving the
/// parent color showing through. The patch has no id, radius, text or
/// interaction, so a press inside it falls through to the containing
/// directory's selection handler.
fn push_flat_fill(elements: &mut Vec<AnyElement>, rect: TreemapRect, fill: Hsla, dimmed: bool) {
    if rect.width < 1.0 || rect.height < 1.0 {
        return;
    }
    let mut patch = div()
        .absolute()
        .left(px(rect.x))
        .top(px(rect.y))
        .w(px(rect.width))
        .h(px(rect.height))
        .bg(fill);
    if dimmed {
        patch = patch.opacity(0.35);
    }
    elements.push(patch.into_any_element());
}

/// Paints the selection ring above the tile and its children. A border on the
/// tile itself would inset the header and content by 2px, and nested child
/// rects would clip its inner edge; an overlay keeps the tile geometry exact
/// and the ring uniform on all four sides.
fn push_selection_ring(elements: &mut Vec<AnyElement>, rect: TreemapRect, color: Hsla) {
    if rect.width < 1.0 || rect.height < 1.0 {
        return;
    }
    // The ring is a selection state: paint it directly at the block's snapped
    // paint rect, above the tile and every child.
    elements.push(
        div()
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.width))
            .h(px(rect.height))
            .rounded(px(TREEMAP_RADIUS))
            .border_2()
            .border_color(color)
            .into_any_element(),
    );
}

/// One standard label row for every labeled block: a fixed 12px icon lane, a
/// 2px gap, and a single-line name that ellipsizes at the end. The icon never
/// disappears with available width, so names share one leading spine.
fn label_row(
    icon: IconName,
    name: impl Into<SharedString>,
    ink: Hsla,
    semibold: bool,
) -> AnyElement {
    let text = div()
        .flex_1()
        .min_w_0()
        .truncate()
        .text_sm()
        .text_color(ink);
    let text = if semibold {
        text.font_semibold()
    } else {
        text.font_medium()
    };
    h_flex()
        .w_full()
        .gap_x(px(2.0))
        .items_center()
        .overflow_hidden()
        // The glyph is part of the title, not secondary metadata: both read at
        // the same ink so filtering, selection and category state never brighten
        // one without the other.
        .child(Icon::new(icon).xsmall().text_color(ink))
        .child(text.child(name.into()))
        .into_any_element()
}

/// Compact blocks (files, directory leaves and summaries) share one structure:
/// icon + name on the first line, size aligned under the name on the second.
/// The size only appears when the block is tall and wide enough for it.
fn compact_block_content(
    icon: IconName,
    name: impl Into<SharedString>,
    size: &str,
    paint: TreemapRect,
    window: &Window,
    ink: Hsla,
    secondary: Hsla,
) -> Div {
    // Every folder title is semibold, whether it owns a header band or is a
    // compact leaf. Files and summaries keep the medium weight.
    let semibold = matches!(icon, IconName::Folder);
    let mut content = v_flex()
        .size_full()
        .p(px(2.0))
        .gap_y(px(1.0))
        .justify_between()
        .overflow_hidden()
        .child(label_row(icon, name, ink, semibold));
    let inner_width = (paint.width - TREEMAP_TEXT_PADDING).max(0.0);
    let size_width = text_width(window, size, FontWeight::NORMAL);
    if paint.height >= 40.0 && size_width <= inner_width {
        content = content.child(
            div()
                .whitespace_nowrap()
                .text_sm()
                .font_features(theme::tabular_figures())
                .text_color(secondary)
                .child(size.to_string()),
        );
    }
    content
}

/// Recursively renders a nested layout node:
/// If the directory has a header_rect, it renders the container box and header bar,
/// then recurses into child layout nodes.
/// Otherwise, it renders as a leaf card (file or compact directory).
fn render_nested_node(
    node: &NestedTreemapLayoutNode,
    rcx: &TreemapRenderContext<'_>,
    viewport: TreemapViewport,
    canvas: (f32, f32),
    elements: &mut Vec<AnyElement>,
) {
    let is_selected = rcx.selected_path == Some(node.path.as_str());
    let is_dir = node.item.is_dir;

    // Paint geometry is the single source for painting, hit surfaces, tooltips
    // and the selection ring. The project step is the only difference between
    // the identity view and a zoomed one.
    let paint = viewport.project(paint_rect(node.rect));
    if paint.width < 1.0 || paint.height < 1.0 {
        return;
    }
    if paint.x + paint.width <= 0.0
        || paint.y + paint.height <= 0.0
        || paint.x >= canvas.0
        || paint.y >= canvas.1
    {
        // Children live inside their container, so the whole subtree is off
        // screen and can be skipped.
        return;
    }
    // Content decisions run on the rect that will actually be painted, not on
    // the fractional layout value; otherwise a label/size threshold can flip
    // on a device-pixel boundary.
    let display = snap_rect_to_device_pixels(paint, rcx.window.scale_factor().max(1.0));

    let category = node.item.category;
    // Filtered categories dim in place; the layout and selection path never
    // move. A selected node stays at full strength so it remains findable.
    let dimmed = !rcx.category_filter[category as usize] && !is_selected;
    // Every directory uses its category color, including Folder and Other;
    // an unknown folder falls back to the Folder tint, never a neutral ladder.
    let header_bg = rcx.palette.directory_header(category, node.depth);
    let texture_fill = if is_dir || node.is_aggregate() {
        header_bg
    } else {
        rcx.palette.fill(category, node.depth)
    };
    let has_header = is_dir && node.header_rect.is_some();
    let is_label_sized =
        display.width >= TREEMAP_LABEL_MIN_WIDTH && display.height >= TREEMAP_LABEL_MIN_HEIGHT;
    if !has_header && !is_label_sized {
        // Sub-label cells are texture, not blocks with missing content. The
        // core keeps these rare by folding anything this small into a summary.
        push_flat_fill(elements, display, texture_fill, dimmed);
        return;
    }

    // The cell this path painted in the view being left, if the block survives
    // the navigation. New blocks fall back to their inset growth.
    // Labels allocate; do it only after culling and flat-fill early return.
    let name = node.item.name.clone();
    let size_str = format_size_bytes(node.item.size);
    let full_path = node.path.clone();

    if node.is_aggregate() {
        let mut args = FluentArgs::new();
        args.set("count", node.aggregate_count as i64);
        let label = rcx.i18n.get_text_with_args("treemap_aggregate", &args);
        let tooltip = SharedString::from(format!("{label}\n{size_str}"));
        let content = compact_block_content(
            IconName::FileStack,
            label,
            &size_str,
            display,
            rcx.window,
            rcx.palette.ink(),
            rcx.palette.ink_secondary(),
        );
        let aggregate_view = rcx.view.clone();
        let aggregate_path = full_path.clone();
        let aggregate_parent = aggregate_parent_path(&full_path).map(str::to_string);
        let block = div()
            .id(element_id("treemap-aggregate", &node.path))
            .rounded(px(TREEMAP_RADIUS))
            .bg(header_bg)
            .overflow_hidden()
            .cursor_pointer()
            .when(dimmed, |this| this.opacity(0.35))
            .when(!rcx.panning, move |this| {
                this.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            })
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                aggregate_view.update(cx, |this, cx| {
                    let mut drill = None;
                    if let Some(ref mut st) = this.treemap_state {
                        st.selected_path = Some(aggregate_path.clone());
                        st.request_focus(window, cx);
                        if event.click_count == 2 {
                            drill = aggregate_parent.clone();
                        }
                    }
                    if let Some(parent) = drill {
                        this.treemap_drill_down(&parent, cx);
                    }
                    cx.notify();
                    cx.stop_propagation();
                });
            });
        let block =
            with_block_fill_interaction(block, header_bg, rcx.palette.is_dark(), is_selected);
        elements.push(place_treemap_node(block.child(content), paint));
        if is_selected {
            push_selection_ring(elements, display, rcx.palette.selection(category));
        }
        return;
    }
    if is_dir {
        if let Some(header) = node.header_rect {
            // The layout band is 20px; the visible header is 18px and the
            // remaining 2px becomes the gap above the first content row. The
            // inset is applied in base space, then the band is projected.
            let header_height = (header.height - 2.0).max(12.0) * viewport.scale();
            let header_view = rcx.view.clone();
            let full_path_drill = full_path.clone();
            let header_size_label = size_str.clone();
            let inner_width = (display.width - TREEMAP_TEXT_PADDING).max(0.0);
            let size_width = text_width(rcx.window, &header_size_label, FontWeight::NORMAL);
            // The name is the header's identity; the size yields instead of
            // squeezing the name below a readable width. The name ellipsizes
            // at the end rather than disappearing.
            let show_header_size = inner_width
                >= TREEMAP_ICON_SPACE + TREEMAP_MIN_HEADER_NAME_WIDTH + 2.0 + size_width;
            let header_tooltip = SharedString::from(format!("{full_path_drill}\n{size_str}"));

            // Directory containers are flat: the header and the quieter fill
            // carry the grouping, so no nesting level adds another frame. The
            // selection ring is painted last, above every child, so no nested
            // tile edge can clip or shift it.
            let container = div()
                .id(element_id("treemap-dir-box", &full_path))
                .rounded(px(TREEMAP_RADIUS))
                .bg(rcx.tokens.background)
                .overflow_hidden()
                .when(dimmed, |this| this.opacity(0.35))
                .on_mouse_down(MouseButton::Left, {
                    let fp = full_path_drill.clone();
                    let cv = header_view.clone();
                    move |event: &MouseDownEvent, window, cx| {
                        cv.update(cx, |this, cx| {
                            if let Some(ref mut st) = this.treemap_state {
                                st.selected_path = Some(fp.clone());
                                st.request_focus(window, cx);
                                if event.click_count == 2 {
                                    this.treemap_drill_down(&fp, cx);
                                }
                                cx.notify();
                            }
                            // The canvas below clears the selection on every
                            // press; a tile press owns the selection instead.
                            cx.stop_propagation();
                        });
                    }
                })
                .context_menu(build_context_menu(
                    rcx.view.clone(),
                    full_path.clone(),
                    true,
                    rcx.i18n,
                ));

            // The header sits at the painted container origin, so it spans the
            // full painted width and leaves a 2px gap before the children.
            let header_bar = h_flex()
                .id(element_id("treemap-dir-hdr", &full_path))
                .w_full()
                .h(px(header_height))
                .px(px(2.0))
                .items_center()
                .gap_x(px(2.0))
                .bg(header_bg)
                .overflow_hidden()
                .cursor_pointer()
                .when(!rcx.panning, move |this| {
                    this.tooltip(move |window, cx| {
                        Tooltip::new(header_tooltip.clone()).build(window, cx)
                    })
                })
                .child(
                    Icon::new(IconName::Folder)
                        .xsmall()
                        .text_color(rcx.palette.ink()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .font_semibold()
                        .text_color(rcx.palette.ink())
                        .child(name),
                )
                .when(show_header_size, |this| {
                    this.child(
                        div()
                            .flex_shrink_0()
                            .whitespace_nowrap()
                            .text_sm()
                            .font_features(theme::tabular_figures())
                            .text_color(rcx.palette.ink_secondary())
                            .child(header_size_label),
                    )
                });
            // The container owns the press: it selects, drills on double click,
            // and stops propagation so the canvas does not clear the selection.
            // The header tints itself on hover/press; no ancestor frame reacts.
            let header_bar = with_block_fill_interaction(
                header_bar,
                header_bg,
                rcx.palette.is_dark(),
                is_selected,
            )
            .context_menu(build_context_menu(
                rcx.view.clone(),
                full_path.clone(),
                true,
                rcx.i18n,
            ));

            elements.push(place_treemap_node(container.child(header_bar), paint));

            // Recursively render child layout nodes
            for child in &node.children {
                render_nested_node(child, rcx, viewport, canvas, elements);
            }
            if is_selected {
                push_selection_ring(elements, display, rcx.palette.selection(category));
            }
            return;
        }
    }

    // Leaf block (file or compact directory). A file carries its category fill; a
    // compact directory uses the quieter header share of the same category, so a
    // directory without a header strip still reads as what it holds. Directories
    // without a category signal stay on the neutral header surface.
    let leaf_path = full_path.clone();
    let leaf_view = rcx.view.clone();
    let leaf_tooltip = SharedString::from(format!("{full_path}\n{size_str}"));
    let leaf_fill = if is_dir {
        header_bg
    } else {
        rcx.palette.fill(node.item.category, node.depth)
    };
    let leaf_icon = if is_dir {
        IconName::Folder
    } else {
        category_icon(node.item.category)
    };

    let block = div()
        .id(element_id(
            if is_dir {
                "treemap-leaf-dir"
            } else {
                "treemap-leaf-file"
            },
            &full_path,
        ))
        .rounded(px(TREEMAP_RADIUS))
        .bg(leaf_fill)
        .overflow_hidden()
        .when(dimmed, |this| this.opacity(0.35))
        .cursor_pointer()
        // A leaf shows a single clipped line; hovering names the whole item.
        .when(!rcx.panning, move |this| {
            this.tooltip(move |window, cx| Tooltip::new(leaf_tooltip.clone()).build(window, cx))
        });
    let block = with_block_fill_interaction(block, leaf_fill, rcx.palette.is_dark(), is_selected)
        .on_mouse_down(MouseButton::Left, {
            let fp = leaf_path.clone();
            let lv = leaf_view.clone();
            move |event: &MouseDownEvent, window, cx| {
                lv.update(cx, |this, cx| {
                    if let Some(ref mut st) = this.treemap_state {
                        st.selected_path = Some(fp.clone());
                        st.request_focus(window, cx);
                        if event.click_count == 2 {
                            if is_dir {
                                this.treemap_drill_down(&fp, cx);
                            } else {
                                let _ = open_file(&fp);
                            }
                        }
                        cx.notify();
                    }
                    cx.stop_propagation();
                });
            }
        })
        .context_menu(build_context_menu(
            rcx.view.clone(),
            full_path.clone(),
            is_dir,
            rcx.i18n,
        ));

    let content = compact_block_content(
        leaf_icon,
        name,
        &size_str,
        display,
        rcx.window,
        rcx.palette.ink(),
        rcx.palette.ink_secondary(),
    );

    elements.push(place_treemap_node(block.child(content), paint));
    if is_selected {
        push_selection_ring(elements, display, rcx.palette.selection(category));
    }
}

/// The file shape that repeats the category already carried by the fill, label
/// and tooltip. `main.rs` installs the complete icon catalog, so this mapping
/// never renders a zero-width icon.
fn category_icon(category: TreemapCategory) -> IconName {
    match category {
        TreemapCategory::Folder => IconName::Folder,
        TreemapCategory::System => IconName::HardDrive,
        TreemapCategory::Applications => IconName::Package,
        TreemapCategory::Document => IconName::FileText,
        TreemapCategory::Archive => IconName::FileArchive,
        TreemapCategory::Image => IconName::FileImage,
        TreemapCategory::Code => IconName::FileCode,
        TreemapCategory::Audio => IconName::FileMusic,
        TreemapCategory::Video => IconName::FileVideoCamera,
        TreemapCategory::Other => IconName::File,
    }
}

fn shows_loading_skeleton(content_revealed: bool, is_scanning: bool) -> bool {
    !content_revealed || is_scanning
}

fn same_directory(left: &str, right: &str) -> bool {
    left.trim_end_matches('\\')
        .eq_ignore_ascii_case(right.trim_end_matches('\\'))
}

fn build_context_menu(
    view: Entity<CairnApp>,
    full_path: String,
    is_dir: bool,
    i18n: &Rc<crate::shell::i18n::I18nManager>,
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
    let i18n = i18n.clone();
    move |menu: PopupMenu, _window: &mut Window, _cx: &mut Context<PopupMenu>| {
        let file_name = Path::new(&full_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&full_path)
            .to_string();

        let table_dir = if is_dir {
            full_path.clone()
        } else {
            Path::new(&full_path)
                .parent()
                .and_then(|p| p.to_str())
                .unwrap_or(&full_path)
                .to_string()
        };

        let open_view = view.clone();
        let reveal_view = view.clone();
        let copy_path_view = view.clone();
        let copy_name_view = view.clone();
        let delete_view = view.clone();
        let properties_view = view.clone();
        let table_view = view.clone();
        let open_path = full_path.clone();
        let reveal_path = full_path.clone();
        let copy_path = full_path.clone();
        let delete_path = full_path.clone();
        let properties_path = full_path.clone();

        menu.item(
            PopupMenuItem::new(i18n.get_text("ctx_open")).on_click(move |_, _, cx| {
                open_view.update(cx, |_, _| {
                    let _ = open_file(&open_path);
                });
            }),
        )
        .item(
            PopupMenuItem::new(i18n.get_text("ctx_open_path")).on_click(move |_, _, cx| {
                reveal_view.update(cx, |_, _| {
                    let _ = open_folder_and_select(&reveal_path);
                });
            }),
        )
        .item(
            PopupMenuItem::new(i18n.get_text("ctx_copy_path")).on_click(move |_, _, cx| {
                copy_path_view.update(cx, |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy_path.clone()));
                });
            }),
        )
        .item(
            PopupMenuItem::new(i18n.get_text("ctx_copy_name")).on_click(move |_, _, cx| {
                copy_name_view.update(cx, |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(file_name.clone()));
                });
            }),
        )
        .separator()
        .item(
            PopupMenuItem::new(i18n.get_text("ctx_delete")).on_click(move |_, window, cx| {
                delete_view.update(cx, |this, cx| {
                    this.confirm_delete_paths(vec![delete_path.clone()], window, cx);
                });
            }),
        )
        .separator()
        .item(
            PopupMenuItem::new(i18n.get_text("view_in_table")).on_click(move |_, window, cx| {
                table_view.update(cx, |this, cx| {
                    this.switch_to_table_with_path(&table_dir, window, cx);
                });
            }),
        )
        .separator()
        .item(
            PopupMenuItem::new(i18n.get_text("ctx_properties")).on_click(move |_, _, cx| {
                properties_view.update(cx, |_, _| {
                    let _ = show_properties_dialog(&properties_path);
                });
            }),
        )
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::*;

    /// A wheel event is normalized to upward-positive lines: pixels use 120px
    /// per line, and Windows reports one notch as three lines, so Lines is
    /// divided by three. Horizontal movement never zooms.
    /// A directory that is still scanning without a listing must keep the
    /// skeleton; only a finished, empty directory shows the empty message.
    #[test]
    fn loading_skeleton_wins_over_a_false_empty_directory() {
        assert!(shows_loading_skeleton(false, true));
        assert!(shows_loading_skeleton(true, true));
        assert!(shows_loading_skeleton(false, false));
        assert!(!shows_loading_skeleton(true, false));
    }

    /// A root-owned summary has no parent to enter; drilling would reload the
    /// same directory and trigger the loading flash.
    #[test]
    fn same_directory_compares_drive_roots_and_trailing_separators() {
        assert!(same_directory(r"C:\", r"C:\"));
        assert!(same_directory(r"C:\users", r"C:\Users\\"));
        assert!(!same_directory(r"C:\", r"C:\Windows"));
        assert!(!same_directory(r"C:\a", r"D:\a"));
    }

    #[test]
    fn wheel_delta_normalizes_lines_and_ignores_the_horizontal_axis() {
        assert_eq!(
            wheel_zoom_lines(ScrollDelta::Pixels(point(px(0.0), px(120.0)))),
            1.0
        );
        assert_eq!(
            wheel_zoom_lines(ScrollDelta::Pixels(point(px(0.0), px(-240.0)))),
            -2.0
        );
        assert_eq!(wheel_zoom_lines(ScrollDelta::Lines(point(0.0, 3.0))), 1.0);
        assert_eq!(
            wheel_zoom_lines(ScrollDelta::Lines(point(0.0, 1.0))),
            1.0 / 3.0
        );
        assert_eq!(wheel_zoom_lines(ScrollDelta::Lines(point(0.0, -3.0))), -1.0);
        assert_eq!(wheel_zoom_lines(ScrollDelta::Lines(point(3.0, 0.0))), 0.0);
        assert_eq!(
            wheel_zoom_lines(ScrollDelta::Pixels(point(px(240.0), px(0.0)))),
            0.0
        );
    }
}
