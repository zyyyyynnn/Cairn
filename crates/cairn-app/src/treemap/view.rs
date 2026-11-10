// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! GPUI canvas, tile rendering and pointer/keyboard interaction for the
//! drive Treemap view.

use crate::app::{element_id, layout, CairnApp};
use crate::shell::accessibility;
use crate::shell::file_icon::file_kind_icon;
use crate::shell::os::{open_file, show_properties_dialog};
use crate::shell::state_view::{icon_action, render_state_band};
use crate::shell::status_bar::format_count;
use crate::shell::theme::{self, ShellTokens};
use crate::treemap::context_menu::{build_block_context_menu, build_context_menu};
use crate::treemap::paint::{
    inset_device_pixels, painted_gutter_device, snap_rect_to_device_pixels, text_width,
    TREEMAP_RADIUS, TREEMAP_TILE_INSET,
};
use crate::treemap::palette::{block_fill_states, TreemapPalette};
use crate::treemap::state::TreemapUnavailable;
use crate::treemap::zoom::TreemapViewport;
use cairn_core::format_size_bytes;
use cairn_core::treemap::{
    aggregate_parent_path, path_is_within, FileKind, NestedTreemapLayoutNode, TreemapCategory,
    TreemapRect,
};
use fluent_bundle::FluentArgs;
use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt;
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex, Icon, Sizable as _};
use gpui_kit::gpui::prelude::FluentBuilder as _;
use gpui_kit::gpui::*;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

/// A readable block uses one standard: a leading 12px icon lane plus a name,
/// with the size either trailing (headers) or on the second line (blocks).
/// Anything smaller is a flat category cell. There is no middle state with a
/// title but no icon, or a size without a title.
const TREEMAP_LABEL_MIN_WIDTH: f32 = 44.0;
const TREEMAP_LABEL_MIN_HEIGHT: f32 = 28.0;

/// Horizontal inset used by headers, leaves and summary labels.
const TREEMAP_TEXT_PADDING: f32 = 4.0;
/// The one gutter used between a block's own parts (icon and name, name and size) and between
/// header elements, so no block mixes two spacing steps.
const TREEMAP_BLOCK_GAP: f32 = 2.0;
/// Leading icon (12px) plus its 2px gap.
const TREEMAP_ICON_SPACE: f32 = 14.0;
/// The header hides its size rather than squeezing the name below this width.
const TREEMAP_MIN_HEADER_NAME_WIDTH: f32 = 28.0;

/// The device scale the canvas paints at, clamped once at the read: every gutter, snap
/// and hit test divides by it, so the helpers take it as given.
fn canvas_scale(window: &Window) -> f32 {
    window.scale_factor().max(1.0)
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
    pub fn render_treemap_body(
        &mut self,
        tokens: &ShellTokens,
        sidebar_width: f32,
        sidebar_settled_width: f32,
        sidebar_animating: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Read the shell's resize cover before `state` borrows the app.
        let resize_covered = self.window_resize.is_covered();
        let Some(ref mut state) = self.treemap_state else {
            return div().into_any_element();
        };

        // Measure available content bounds
        let window_bounds = window.bounds();
        // The shell lays its boxes out border-box, so the 1px hairlines are
        // already inside these band widths; counting them again would shift
        // every pointer-to-base projection by a pixel or two.
        // The canvas is the content column's panel, below the path band. It keeps the
        // panel's own margin on the sides and bottom, while the band's bottom inset is
        // its top gap, mirroring the table panel under the search band. Pointer
        // coordinates and the tile layout both read this rect.
        let panel_margin = f32::from(theme::rem(window) * layout::SPACE_4);
        let band_height = f32::from(theme::rem(window) * layout::SEARCH_BAND_HEIGHT);
        let avail_w =
            (f32::from(window_bounds.size.width) - sidebar_width - 2.0 * panel_margin).max(100.0);
        let chrome_h = f32::from(layout::TITLE_BAR_HEIGHT)
            + f32::from(theme::rem(window) * layout::STATUS_HEIGHT)
            + band_height;
        let avail_h = (f32::from(window_bounds.size.height) - chrome_h - panel_margin).max(100.0);
        let fallback_origin = (
            sidebar_width + panel_margin,
            f32::from(layout::TITLE_BAR_HEIGHT) + band_height,
        );
        let canvas_bounds = state.painted_canvas_bounds.clone();
        let canvas_size = (avail_w, avail_h);
        let window_size = (
            f32::from(window_bounds.size.width),
            f32::from(window_bounds.size.height),
        );
        state.last_scale_factor = canvas_scale(window);

        // The squarified layout is tied to the window, not to the sidebar. A
        // sidebar motion projects the existing cells onto the new canvas width,
        // so the topology never reshuffles and no frame exposes the background.
        // While the shell covers a live resize the layout stays put and reflows
        // once, on the first frame after the cover lifts.
        let is_initial = state.last_window_size.0 <= 1.0 || state.last_window_size.1 <= 1.0;
        let window_changed = (state.last_window_size.0 - window_size.0).abs() > 0.5
            || (state.last_window_size.1 - window_size.1).abs() > 0.5;
        if !resize_covered && (is_initial || (window_changed && !sidebar_animating)) {
            state.last_window_size = window_size;
            state.last_bounds = canvas_size;
            state.recompute_layout();
        }

        // A sidebar shift only changes the width the same cells cover. The
        // projection absorbs the delta, so the final frame has no layout jump.
        let stretch_x = if state.last_bounds.0 > 1.0 {
            canvas_size.0 / state.last_bounds.0
        } else {
            1.0
        };
        state.zoom.set_stretch_x(stretch_x, canvas_size);

        // Labels are sized by the width the canvas keeps once the sidebar stops moving: a collapse
        // widens every cell without reshaping the squarified layout, so a title has to appear on
        // the settled width instead of flipping with the animated one.
        let settled_canvas_width =
            (f32::from(window_bounds.size.width) - sidebar_settled_width - 2.0 * panel_margin)
                .max(100.0);
        let label_stretch_x = if state.last_bounds.0 > 1.0 {
            settled_canvas_width / state.last_bounds.0
        } else {
            1.0
        };

        let is_dark = self.settings.is_dark(self.system_dark);
        let viewport = state.zoom.viewport();
        let panning = state.zoom.is_panning();
        let view = cx.entity();
        let pan_view = view.clone();
        let focus = state.focus_handle().cloned();

        // Scan progress is reported by the status bar, not here.
        let is_scanning = state.is_scanning;

        let render_cx = TreemapRenderContext {
            palette: TreemapPalette::for_mode(is_dark),
            selected_path: state.selected_path.as_deref(),
            category_filter: state.category_filter,
            view: &view,
            i18n: &self.i18n,
            window,
            label_stretch_x,
            panning,
            suppress_tooltip: state.suppress_tooltip,
            selection_active: state.selected_path.is_some(),
        };

        // An empty directory is content, not a separate surface: it must keep
        // the canvas focus, Backspace/Enter and wheel zoom.
        // A scan waits on the panel's own muted surface: the canvas container is already
        // painted in it, so a waiting overlay would add no pixels and one more id to keep.
        let content = if is_scanning {
            div().into_any_element()
        } else if state.layout_nodes.is_empty() {
            let control = accessibility::control("treemap-state");
            let unavailable = state.unavailable_reason();
            let message = self.i18n.get_text(
                unavailable
                    .map(TreemapUnavailable::message_key)
                    .unwrap_or("treemap_empty_directory"),
            );
            let icon = match unavailable {
                Some(TreemapUnavailable::NotDirectory | TreemapUnavailable::TooLarge) => {
                    Icon::new(IconName::TriangleAlert)
                        .size_8()
                        .text_color(tokens.warning_ink)
                        .into_any_element()
                }
                Some(_) => Icon::new(IconName::CircleAlert)
                    .size_8()
                    .text_color(tokens.danger)
                    .into_any_element(),
                None => Icon::new(IconName::Folder)
                    .size_8()
                    .text_color(tokens.muted_foreground)
                    .into_any_element(),
            };
            let retryable = unavailable.is_some_and(TreemapUnavailable::retryable);
            let action = retryable.then(|| {
                let retry = accessibility::control("treemap-retry");
                let label = accessibility::name(retry, &self.i18n);
                icon_action(retry, IconName::RotateCcw, label)
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(dir) = this
                            .treemap_state
                            .as_ref()
                            .map(|state| state.current_dir().to_string())
                        {
                            this.start_treemap_scan(PathBuf::from(dir), cx);
                            if let Some(state) = this.treemap_state.as_ref() {
                                state.request_focus(window, cx);
                            }
                        }
                    }))
                    .into_any_element()
            });
            render_state_band(tokens, control, message, icon, action).into_any_element()
        } else {
            let mut block_elements = Vec::with_capacity(state.layout_nodes.len() * 4);
            for node in &state.layout_nodes {
                render_nested_node(node, &render_cx, viewport, canvas_size, &mut block_elements);
            }
            let mut tiles_layer = div()
                .absolute()
                .left(px(0.0))
                .top(px(0.0))
                .size_full()
                .children(block_elements);

            if state.scan_fade_epoch > 0 {
                let fade_key = format!("treemap-scan-fade-{}", state.scan_fade_epoch);
                let reduce_motion = cx.reduce_motion();
                tiles_layer = tiles_layer.child(
                    div()
                        .id("treemap-scan-fade-overlay")
                        .absolute()
                        .inset_0()
                        .bg(tokens.muted)
                        .with_animation(
                            fade_key,
                            Animation::new(crate::shell::motion::MASK_DISSOLVE)
                                .with_easing(ease_out_quint()),
                            move |el, delta| {
                                if reduce_motion {
                                    el.opacity(0.0)
                                } else {
                                    el.opacity(1.0 - delta)
                                }
                            },
                        ),
                );
            }

            tiles_layer.into_any_element()
        };

        let mut canvas_element = v_flex()
            .id("treemap-canvas-container")
            .relative()
            .flex_1()
            .w_full()
            .h_full()
            .bg(tokens.muted)
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
                    if let Some(ref mut st) = this.treemap_state {
                        let origin = st.canvas_origin(fallback_origin);
                        let cursor = (
                            f32::from(event.position.x) - origin.0,
                            f32::from(event.position.y) - origin.1,
                        );
                        if st.zoom.begin_pan(cursor) {
                            st.request_focus(window, cx);
                            cx.notify();
                        }
                    }
                    cx.stop_propagation();
                }),
            )
            .on_scroll(cx.listener(move |this, event: &ScrollEvent, window, cx| {
                let lines = wheel_zoom_lines(event.delta);
                let enter_path = if let Some(ref mut st) = this.treemap_state {
                    if lines == 0.0 {
                        None
                    } else {
                        let origin = st.canvas_origin(fallback_origin);
                        let cursor = (
                            f32::from(event.position.x) - origin.0,
                            f32::from(event.position.y) - origin.1,
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
                    this.treemap_drill_down(&path, window, cx);
                }
                cx.stop_propagation();
            }))
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
                        this.treemap_drill_back(window, cx);
                    }
                    "enter" => {
                        this.treemap_open_selected(window, cx);
                    }
                    "+" | "=" => {
                        if let Some(ref mut st) = this.treemap_state {
                            let lvl = (st.detail_level + 1).min(5);
                            st.set_detail_level(lvl);
                            this.arm_treemap_scan_fade_cleanup(cx);
                            cx.notify();
                        }
                    }
                    "-" | "_" => {
                        if let Some(ref mut st) = this.treemap_state {
                            let lvl = st.detail_level.saturating_sub(1).max(1);
                            st.set_detail_level(lvl);
                            this.arm_treemap_scan_fade_cleanup(cx);
                            cx.notify();
                        }
                    }
                    _ => {}
                }
            }))
            .child({
                let bounds_cell = canvas_bounds.clone();
                canvas(
                    |_, _, _| {},
                    move |bounds, _, _, _| {
                        bounds_cell.set(Some(bounds));
                    },
                )
                .absolute()
                .inset_0()
            })
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
                            move |bounds, _, window, _cx| {
                                let pan_origin =
                                    (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
                                let move_view = entity.clone();
                                window.on_mouse_event(
                                    move |event: &MouseMoveEvent, phase, _window, cx| {
                                        if phase == DispatchPhase::Capture {
                                            return;
                                        }
                                        let cursor = (
                                            f32::from(event.position.x) - pan_origin.0,
                                            f32::from(event.position.y) - pan_origin.1,
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
    /// How much wider or narrower the canvas is settled to be than the layout was built for, so a
    /// sidebar motion changes which labels fit without changing which cells exist.
    label_stretch_x: f32,
    /// A middle-button drag owns the pointer; hover tooltips would otherwise
    /// pop up under the cursor while the content moves.
    panning: bool,
    suppress_tooltip: bool,
    selection_active: bool,
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
/// its category hue and saturation. Selection keeps full fill brightness while outside
/// regions are dimmed, so neither state is installed on a selected block. Directory
/// containers themselves never use this, so hovering a child cannot light up its ancestors.
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
fn push_flat_fill(
    elements: &mut Vec<AnyElement>,
    rect: TreemapRect,
    fill: Hsla,
    palette: TreemapPalette,
    dimmed: bool,
) {
    if rect.width < 1.0 || rect.height < 1.0 {
        return;
    }
    let background = if dimmed {
        palette.dimmed_fill(fill)
    } else {
        fill
    };
    elements.push(
        div()
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.width))
            .h(px(rect.height))
            .bg(background)
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
        .gap_x(px(TREEMAP_BLOCK_GAP))
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
        .gap_y(px(TREEMAP_BLOCK_GAP))
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
    // and selection highlight. The project step is the only difference between
    // the identity view and a zoomed one.
    let scale = canvas_scale(rcx.window);
    let paint = inset_device_pixels(
        snap_rect_to_device_pixels(viewport.project(node.rect), scale),
        scale,
    );
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
    let display = snap_rect_to_device_pixels(paint, canvas_scale(rcx.window));
    // Label decisions read the settled cell, never the per-frame projection: `stretch_x` moves
    // every frame while the panel collapses, and device-pixel rounding flips a size threshold back
    // and forth, which makes titles appear and disappear mid-animation. The settled stretch is
    // fixed for the whole motion, so a collapse shows exactly the labels its final width carries.
    let stable = TreemapRect::new(
        0.0,
        0.0,
        node.rect.width * viewport.scale() * rcx.label_stretch_x,
        node.rect.height * viewport.scale(),
    );

    let category = node.item.category;
    // Filtered categories dim in place; the layout and selection path never
    // move. A selected node stays at full strength so it remains findable.
    let filtered = !rcx.category_filter[category as usize] && !is_selected;
    // Selecting a container focuses its whole subtree: the container's own header and every
    // descendant stay at full tone, and only what lies outside dims.
    let in_selected = rcx
        .selected_path
        .is_some_and(|selected| path_is_within(&node.path, selected));
    let unfocused = rcx.selection_active && !in_selected;
    let dimmed = filtered || unfocused;
    // Every directory uses its resolved category color; a directory whose content carries no
    // signal keeps the Other tint.
    let base_header = rcx.palette.directory_header(category);
    let header_bg = if dimmed {
        rcx.palette.dimmed_fill(base_header)
    } else {
        base_header
    };
    let texture_fill = if is_dir || node.is_aggregate() {
        base_header
    } else {
        rcx.palette.fill(category)
    };
    let has_header = is_dir && node.header_rect.is_some();
    let is_label_sized =
        stable.width >= TREEMAP_LABEL_MIN_WIDTH && stable.height >= TREEMAP_LABEL_MIN_HEIGHT;
    if !has_header && !is_label_sized {
        // Sub-label cells are texture, not blocks with missing content. The
        // core keeps these rare by folding anything this small into a summary.
        push_flat_fill(elements, display, texture_fill, rcx.palette, dimmed);
        return;
    }

    // Labels allocate; do it only after culling and flat-fill early return.
    let name = node.item.name.clone();
    let size_str = format_size_bytes(node.item.size);
    let full_path = node.path.clone();

    if node.is_aggregate() {
        let mut args = FluentArgs::new();
        args.set("count", node.aggregate_count as i64);
        args.set("count_text", format_count(node.aggregate_count));
        let label = rcx.i18n.get_text_with_args("treemap_aggregate", &args);
        let aggregate_label_text = label.clone();
        let tooltip = SharedString::from(format!(
            "{label}
{size_str}"
        ));
        let content = compact_block_content(
            IconName::FileStack,
            label,
            &size_str,
            stable,
            rcx.window,
            rcx.palette.ink(),
            rcx.palette.ink_secondary(),
        );
        let aggregate_view = rcx.view.clone();
        let aggregate_path = full_path.clone();
        let block_bg = header_bg;
        let menu_parent = aggregate_parent_path(&full_path).map(str::to_string);
        let block = div()
            .id(element_id("treemap-aggregate", &node.path))
            .rounded(px(TREEMAP_RADIUS))
            .bg(block_bg)
            .overflow_hidden()
            .cursor_pointer()
            .when(!rcx.panning && !rcx.suppress_tooltip, move |this| {
                this.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            })
            .on_mouse_down(MouseButton::Right, {
                let v = aggregate_view.clone();
                move |_, _, cx| {
                    v.update(cx, |this, cx| {
                        if let Some(st) = this.treemap_state.as_mut() {
                            st.suppress_tooltip = true;
                        }
                        cx.notify();
                    });
                }
            })
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                aggregate_view.update(cx, |this, cx| {
                    if let Some(ref mut st) = this.treemap_state {
                        st.selected_path = Some(aggregate_path.clone());
                        st.suppress_tooltip = false;
                        st.request_focus(window, cx);
                        // A block is a container: the same double click that enters a directory
                        // opens its folded items as a canvas layer.
                        if event.click_count == 2 {
                            st.open_block(&aggregate_path, aggregate_label_text.clone());
                        }
                    }
                    cx.notify();
                    cx.stop_propagation();
                });
            });
        let block =
            with_block_fill_interaction(block, block_bg, rcx.palette.is_dark(), is_selected)
                .context_menu(build_block_context_menu(
                    rcx.view.clone(),
                    menu_parent,
                    rcx.i18n,
                ));
        elements.push(place_treemap_node(block.child(content), paint));
        return;
    }
    if is_dir {
        if let Some(header) = node.header_rect {
            // The layout band is 20px; the header stops one painted gutter above the first content
            // row, so the band's gap is the same even device-pixel gutter as every other seam.
            // The gutter is a device quantity, so it is converted back to view pixels here.
            let scale = canvas_scale(rcx.window);
            let header_height = (header.height * viewport.scale()
                - painted_gutter_device(scale) / scale)
                .max(12.0 * viewport.scale());
            let header_view = rcx.view.clone();
            let full_path_drill = full_path.clone();
            let header_size_label = size_str.clone();
            let inner_width = (stable.width - TREEMAP_TEXT_PADDING).max(0.0);
            let size_width = text_width(rcx.window, &header_size_label, FontWeight::NORMAL);
            // The name is the header's identity; the size yields instead of
            // squeezing the name below a readable width. The name ellipsizes
            // at the end rather than disappearing.
            let show_header_size = inner_width
                >= TREEMAP_ICON_SPACE + TREEMAP_MIN_HEADER_NAME_WIDTH + 2.0 + size_width;
            let header_tooltip = SharedString::from(format!("{full_path_drill}\n{size_str}"));

            // A container body is a low-share plate of the same tone, so nesting reads as tonal
            // plates instead of white boxes; it dims with its header when the node is out of focus.
            let body = rcx.palette.body_fill();
            let container_bg = if dimmed {
                rcx.palette.dimmed_fill(body)
            } else {
                body
            };
            let container = div()
                .id(element_id("treemap-dir-box", &full_path))
                .rounded(px(TREEMAP_RADIUS))
                .bg(container_bg)
                .overflow_hidden()
                .on_mouse_down(MouseButton::Left, {
                    let fp = full_path_drill.clone();
                    let cv = header_view.clone();
                    move |event: &MouseDownEvent, window, cx| {
                        cv.update(cx, |this, cx| {
                            if let Some(ref mut st) = this.treemap_state {
                                st.selected_path = Some(fp.clone());
                                st.request_focus(window, cx);
                                if event.click_count == 2 {
                                    this.treemap_drill_down(&fp, window, cx);
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
                .gap_x(px(TREEMAP_BLOCK_GAP))
                .bg(header_bg)
                .overflow_hidden()
                .cursor_pointer()
                .when(!rcx.panning && !rcx.suppress_tooltip, move |this| {
                    this.tooltip(move |window, cx| {
                        Tooltip::new(header_tooltip.clone()).build(window, cx)
                    })
                })
                .on_mouse_down(MouseButton::Right, {
                    let v = header_view.clone();
                    move |_, _, cx| {
                        v.update(cx, |this, cx| {
                            if let Some(st) = this.treemap_state.as_mut() {
                                st.suppress_tooltip = true;
                            }
                            cx.notify();
                        });
                    }
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
            return;
        }
    }

    // Leaf block (file or compact directory). A file carries its category fill; a
    // compact directory uses the quieter header share of the same category, so a
    // directory without a header strip still reads as what it holds. A directory
    // whose content carries no category signal keeps the Other tint.
    let leaf_path = full_path.clone();
    let leaf_view = rcx.view.clone();
    let leaf_tooltip = SharedString::from(format!("{full_path}\n{size_str}"));
    let leaf_fill = if is_dir {
        base_header
    } else {
        rcx.palette.fill(node.item.category)
    };
    let leaf_icon = if is_dir {
        IconName::Folder
    } else {
        file_kind_icon(FileKind::classify_file(&node.item.name))
    };

    let leaf_bg = if dimmed {
        rcx.palette.dimmed_fill(leaf_fill)
    } else {
        leaf_fill
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
        .bg(leaf_bg)
        .overflow_hidden()
        .cursor_pointer()
        // A leaf shows a single clipped line; hovering names the whole item.
        .when(!rcx.panning && !rcx.suppress_tooltip, move |this| {
            this.tooltip(move |window, cx| Tooltip::new(leaf_tooltip.clone()).build(window, cx))
        })
        .on_mouse_down(MouseButton::Right, {
            let v = leaf_view.clone();
            move |_, _, cx| {
                v.update(cx, |this, cx| {
                    if let Some(st) = this.treemap_state.as_mut() {
                        st.suppress_tooltip = true;
                    }
                    cx.notify();
                });
            }
        });
    let block = with_block_fill_interaction(block, leaf_bg, rcx.palette.is_dark(), is_selected)
        .on_mouse_down(MouseButton::Left, {
            let fp = leaf_path.clone();
            let lv = leaf_view.clone();
            move |event: &MouseDownEvent, window, cx| {
                lv.update(cx, |this, cx| {
                    if let Some(ref mut st) = this.treemap_state {
                        st.selected_path = Some(fp.clone());
                        st.suppress_tooltip = false;
                        st.request_focus(window, cx);
                        if event.click_count == 2 {
                            if is_dir {
                                this.treemap_drill_down(&fp, window, cx);
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
        stable,
        rcx.window,
        rcx.palette.ink(),
        rcx.palette.ink_secondary(),
    );
    elements.push(place_treemap_node(block.child(content), paint));
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::*;

    #[test]
    fn dimmed_directory_and_file_leaves_match() {
        for is_dark in [false, true] {
            let palette = TreemapPalette::for_mode(is_dark);
            for category in [
                cairn_core::treemap::TreemapCategory::System,
                cairn_core::treemap::TreemapCategory::Programs,
                cairn_core::treemap::TreemapCategory::Documents,
                cairn_core::treemap::TreemapCategory::Image,
                cairn_core::treemap::TreemapCategory::Video,
                cairn_core::treemap::TreemapCategory::Audio,
                cairn_core::treemap::TreemapCategory::Archive,
                cairn_core::treemap::TreemapCategory::Other,
            ] {
                let base_header = palette.directory_header(category);
                let dir_leaf_bg_dimmed = palette.dimmed_fill(base_header);
                let file_leaf_bg_dimmed = palette.dimmed_fill(palette.fill(category));

                assert_eq!(
                    dir_leaf_bg_dimmed, file_leaf_bg_dimmed,
                    "compact directory leaf and file leaf must match when dimmed (category {category:?}, is_dark={is_dark})"
                );
            }
        }
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
