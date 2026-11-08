// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! GPUI canvas, tile rendering and pointer/keyboard interaction for the
//! drive Treemap view.

use crate::app::{element_id, layout, CairnApp};
use crate::search::filters::FilterCategory;
use crate::service::ipc_tasks::get_tokio_runtime;
use crate::shell::accessibility;
use crate::shell::file_icon::file_kind_icon;
use crate::shell::os::{open_file, open_folder_and_select, show_properties_dialog};
use crate::shell::state_view::{icon_action, render_state_band};
use crate::shell::status_bar::format_count;
use crate::shell::theme::{self, ShellTokens};
use crate::treemap::navigation::path_segments;
use crate::treemap::paint::{
    inset_device_pixels, painted_gutter_device, snap_rect_to_device_pixels, TREEMAP_RADIUS,
    TREEMAP_TILE_INSET,
};
use crate::treemap::palette::{block_fill_states, TreemapPalette};
use crate::treemap::path_input::normalize_treemap_path;
use crate::treemap::state::{
    next_tree_request_epoch, tree_unavailable_from_code, TreemapUnavailable, TreemapViewState,
};
use crate::treemap::zoom::TreemapViewport;
use cairn_core::format_size_bytes;
use cairn_core::treemap::{
    aggregate_parent_path, FileKind, NestedTreemapLayoutNode, TreemapCategory, TreemapRect,
};
use cairn_protocol::message::{ResponsePayload, MAX_TREE_DEPTH};
use fluent_bundle::FluentArgs;
use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt;
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::gpui::prelude::FluentBuilder as _;
use gpui_kit::gpui::*;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

/// One breadcrumb label never takes more than a readable share of the path band; the
/// full path remains in the band's tooltip.
const PATH_SEGMENT_MAX_WIDTH: f32 = 24.0;
/// One dropdown row never builds an unbounded menu; the Treemap remains the
/// route to folders past this cap.
const PATH_MENU_LIMIT: usize = 100;
/// A path menu is as wide as its longest readable name, inside this band: the
/// floor keeps a short list from collapsing, the ceiling keeps one long folder
/// from stretching the menu across the window.
const MENU_MIN_WIDTH: f32 = 16.0;
const MENU_MAX_WIDTH: f32 = 24.0;
/// The share of the widest menu a single folder name may take. A label is given
/// this as a definite width, because `truncate` only draws an ellipsis when the
/// text box is narrower than the text: left to size itself the label pushes the
/// row past the menu's ceiling and the menu clips it without one. The tooltip
/// carries the rest of the path.
const MENU_LABEL_MAX_WIDTH: f32 = 20.0;
/// One screen-half cap for every path menu, so a long list scrolls instead of
/// running past the window.
const MENU_MAX_HEIGHT: f32 = 20.0;

/// Child folders of `directory` as the filesystem holds them now. The Treemap tree can lag
/// deletions and a breadcrumb segment can name an ancestor the current tree does not hold, so
/// the menu reads the directory itself.
///
/// The scan stops once the menu holds more than its cap: a directory can carry tens of
/// thousands of children, and enumerating, stat-ing and sorting all of them to display a
/// hundred costs about a second on the UI thread. The listed set is therefore the eligible
/// folders found first in directory order, sorted for display afterwards.
fn path_menu_directories(directory: &str) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let base = directory.trim_end_matches(['\\', '/']);
    let mut directories = Vec::with_capacity(PATH_MENU_LIMIT + 1);
    for entry in entries.flatten() {
        if directories.len() > PATH_MENU_LIMIT {
            break;
        }
        if !menu_directory(&entry) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        directories.push((name.clone(), format!("{base}\\{name}")));
    }
    directories.sort_by(|a, b| cairn_core::sort::natural_cmp(&a.0, &b.0));
    directories
}

/// A path menu offers real switch destinations only: the entry must be a directory, and
/// Explorer's default hides the system attribute pair. The attributes come from the
/// enumeration's own find data, so no second filesystem call is made and the link itself is
/// read — a junction such as "Documents and Settings" points at an ordinary directory but is
/// still a system entry.
#[cfg(windows)]
fn menu_directory(entry: &std::fs::DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
    entry
        .metadata()
        .map(|metadata| {
            let attributes = metadata.file_attributes();
            attributes & FILE_ATTRIBUTE_DIRECTORY != 0
                && attributes & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) == 0
        })
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn menu_directory(entry: &std::fs::DirEntry) -> bool {
    entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false)
}

/// One folder option built here rather than by the menu component's intrinsic
/// row because the name has to ellipsize at the menu's width and carry the
/// whole path in its tooltip. The row takes the shared menu step and the shared
/// icon-to-label distance, and it names itself to assistive technology by the
/// folder it opens.
fn path_menu_option(
    name: String,
    accessible_name: String,
    path: String,
    label_width: Pixels,
    view: Entity<CairnApp>,
) -> PopupMenuItem {
    let control = accessibility::control("treemap-path-menu-row");
    let row_path = path.clone();
    let tooltip_path = path.clone();
    let item_path = path;
    PopupMenuItem::element(move |_, _| {
        let tooltip_text = tooltip_path.clone();
        accessibility::element_at_key(control, &row_path)
            .aria_label(accessible_name.clone())
            .h(layout::MENU_ROW_HEIGHT)
            .w_full()
            .flex()
            .items_center()
            .gap(rems(layout::INNER_SPACING))
            .cursor_pointer()
            .child(Icon::new(IconName::Folder).xsmall().flex_shrink_0())
            .child(div().w(label_width).truncate().child(name.clone()))
            .tooltip(move |window, cx| Tooltip::new(tooltip_text.clone()).build(window, cx))
    })
    .on_click(move |_, window, cx| {
        view.update(cx, |this, cx| {
            this.clear_treemap_path_menu();
            this.treemap_drill_down(&item_path, window, cx);
        });
    })
}

/// The menu's foot note: the same row step as the options above it, flush with
/// the menu's leading edge instead of the icon lane its siblings reserve.
fn path_menu_note(key: &'static str, label: String) -> PopupMenuItem {
    let control = accessibility::control("treemap-path-menu-note");
    PopupMenuItem::element(move |_, cx| {
        let text = label.clone();
        accessibility::element_at_key(control, key)
            .aria_label(label.clone())
            .h(layout::MENU_ROW_HEIGHT)
            .w_full()
            .flex()
            .items_center()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(text)
    })
    .disabled(true)
}

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
    /// Switches to the drive disk space Treemap visualization and records it as a history step, so
    /// Back leaves the canvas again instead of stopping at its root.
    pub fn open_drive_visualization(
        &mut self,
        drive: char,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The query and directory being left behind stay in the entry they belong to.
        self.sync_visualization_dir();
        self.update_current_nav_state_text();
        self.open_drive_at(
            drive,
            &format!("{}:\\", drive.to_ascii_uppercase()),
            window,
            cx,
        );
        self.record_nav_state();
    }

    /// Opens the visualization on `dir` without touching navigation history: Back and Forward
    /// restore a recorded directory through here, and pushing an entry would move the cursor.
    pub(crate) fn open_drive_at(
        &mut self,
        drive: char,
        dir: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let drive_upper = drive.to_ascii_uppercase();
        if self.recycle_bin.leave() {
            // A bin filter is a location-local search, not a query to carry into the drive view.
            self.query.search_input.clear();
        }
        self.clear_treemap_path_menu();
        self.drive_view = Some(drive_upper);
        self.query.scope_drive = Some(drive_upper);
        let (state, scan_target) = TreemapViewState::new(drive_upper, dir, cx.focus_handle());
        state.request_focus(window, cx);
        self.treemap_state = Some(state);
        if let Some(path) = scan_target {
            self.start_treemap_scan(path, cx);
        } else {
            self.arm_treemap_scan_fade_cleanup(cx);
        }
        cx.notify();
    }

    /// Returns keyboard ownership to the canvas after a navigation, commit or
    /// retry. One method so every path that changes the tree ends the same way.
    fn focus_treemap_canvas(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(state) = self.treemap_state.as_ref() {
            state.request_focus(window, cx);
        }
    }

    /// Turns the path field into an editable, selected copy of the current
    /// directory. The path field is the Treemap's address bar.
    pub(crate) fn begin_treemap_path_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.drive_view.is_none() {
            return;
        }
        self.clear_treemap_path_menu();
        let input = self.treemap_path.input.clone();
        if self.treemap_path.editing {
            // A second request lands on the text already in the field; replacing
            // it with the current directory would discard what the person typed.
            input.update(cx, |input, cx| input.select_all(window, cx));
            input.read(cx).focus_handle(cx).focus(window, cx);
            return;
        }
        let Some(current) = self
            .treemap_state
            .as_ref()
            .map(|state| state.current_dir().to_string())
        else {
            return;
        };
        self.treemap_path.editing = true;
        self.treemap_path.error = false;
        input.update(cx, |input, cx| {
            input.set_value(current, window, cx);
            input.select_all(window, cx);
        });
        input.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// Leaves path editing without navigating. The breadcrumb returns and the
    /// canvas takes the focus, so the next key still reaches the Treemap.
    pub(crate) fn cancel_treemap_path_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.treemap_path.editing {
            return;
        }
        self.treemap_path.editing = false;
        self.treemap_path.error = false;
        let title = self.i18n.get_text("treemap_path_invalid_title");
        self.dismiss_toast_titled(&title, cx);
        self.focus_treemap_canvas(window, cx);
        cx.notify();
    }

    pub(crate) fn treemap_path_changed(&mut self, cx: &mut Context<Self>) {
        self.treemap_path.error = false;
        let title = self.i18n.get_text("treemap_path_invalid_title");
        self.dismiss_toast_titled(&title, cx);
        // The clear control appears and disappears with the text, so the
        // enclosing view must rebuild on each change.
        cx.notify();
    }

    /// Empties the address field and keeps it in edit mode. The button itself
    /// disappears because the text is now empty.
    pub(crate) fn clear_treemap_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.treemap_path.input.clone();
        input.update(cx, |input, cx| input.set_value("", window, cx));
        input.read(cx).focus_handle(cx).focus(window, cx);
        self.treemap_path.error = false;
        cx.notify();
    }

    pub(crate) fn clear_treemap_path_menu(&mut self) {
        self.treemap_path_menu_dismiss = None;
        self.treemap_path_menu = None;
        self.treemap_path_menu_for = None;
        self.treemap_path_menu_focus = None;
    }

    /// Opens the child-folder menu under one breadcrumb segment. The menu
    /// belongs to the segment it was opened from; asking for it again closes
    /// it, and asking for another segment replaces it.
    pub(crate) fn toggle_treemap_path_menu(
        &mut self,
        segment: &str,
        trigger_focus: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let same_segment = self.treemap_path_menu_for.as_deref() == Some(segment);
        if same_segment {
            self.clear_treemap_path_menu();
            cx.notify();
            return;
        }
        self.clear_treemap_path_menu();
        let directories = path_menu_directories(segment);
        let view = cx.entity();
        let empty_label = self.i18n.get_text("treemap_path_menu_empty");
        let truncated = directories.len() > PATH_MENU_LIMIT;
        let truncated_label = (truncated && !directories.is_empty()).then(|| {
            let mut args = FluentArgs::new();
            args.set("count", directories.len().min(PATH_MENU_LIMIT) as i64);
            self.i18n
                .get_text_with_args("treemap_path_menu_truncated", &args)
        });
        let menu_width = window.rem_size() * MENU_MIN_WIDTH;
        let menu_max_width = window.rem_size() * MENU_MAX_WIDTH;
        // The widest name the menu will show, shaped rather than estimated, so
        // the label gets a definite width and the menu sizes to its content.
        let label_width = directories
            .iter()
            .take(PATH_MENU_LIMIT)
            .map(|(name, _)| px(text_width(window, name, FontWeight::default())))
            .fold(px(0.0), |widest, width| widest.max(width))
            .min(window.rem_size() * MENU_LABEL_MAX_WIDTH);
        // Named here, where the locale is reachable: an accessible name belongs to
        // the row, and the menu builder's closure owns no application context.
        let rows: Vec<(String, String, String)> = directories
            .iter()
            .take(PATH_MENU_LIMIT)
            .map(|(name, path)| {
                let mut args = FluentArgs::new();
                args.set("label", name.clone());
                let accessible_name = self.i18n.get_text_with_args("treemap_path_menu_row", &args);
                (name.clone(), accessible_name, path.clone())
            })
            .collect();
        let menu = PopupMenu::build(window, cx, move |menu, window, _cx| {
            let mut menu = menu
                .min_w(menu_width)
                .max_w(menu_max_width)
                .max_h(window.rem_size() * MENU_MAX_HEIGHT)
                .scrollable(true);
            if directories.is_empty() {
                return menu.item(path_menu_note("empty", empty_label));
            }
            for (name, accessible_name, path) in rows {
                menu = menu.item(path_menu_option(
                    name,
                    accessible_name,
                    path,
                    label_width,
                    view.clone(),
                ));
            }
            if let Some(label) = truncated_label {
                menu = menu.item(path_menu_note("truncated", label));
            }
            menu
        });
        let subscription = cx.subscribe_in(
            &menu,
            window,
            |this, _menu, _: &DismissEvent, window, cx| {
                this.treemap_path_menu_dismiss = None;
                this.treemap_path_menu = None;
                this.treemap_path_menu_for = None;
                // Dismissal returns focus to the arrow that opened the menu; the canvas only
                // catches the case where the segment is no longer on screen.
                match this.treemap_path_menu_focus.take() {
                    Some(trigger) => trigger.focus(window, cx),
                    None => {
                        if let Some(state) = this.treemap_state.as_ref() {
                            state.request_focus(window, cx);
                        }
                    }
                }
                cx.notify();
            },
        );
        menu.focus_handle(cx).focus(window, cx);
        self.treemap_path_menu = Some(menu);
        self.treemap_path_menu_focus = Some(trigger_focus);
        self.treemap_path_menu_for = Some(segment.to_string());
        self.treemap_path_menu_dismiss = Some(subscription);
        cx.notify();
    }

    /// Opens the directory typed into the address field. Text that cannot name a
    /// drive path stays in the field with the error frame and a toast; a
    /// well-formed path the index does not cover opens and shows the canvas's
    /// unavailable state.
    pub(crate) fn commit_treemap_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.treemap_path.editing {
            return;
        }
        let typed = self.treemap_path.input.read(cx).value().to_string();
        let Some(current) = self
            .treemap_state
            .as_ref()
            .map(|state| state.current_dir().to_string())
        else {
            self.cancel_treemap_path_edit(window, cx);
            return;
        };
        let Some(target) = normalize_treemap_path(&typed, &current) else {
            // The field action and the input's own Enter can both report the
            // same rejection; one toast is enough.
            let first_rejection = !self.treemap_path.error;
            self.treemap_path.error = true;
            if first_rejection {
                let title = self.i18n.get_text("treemap_path_invalid_title");
                let hint = self.i18n.get_text("treemap_path_invalid_hint");
                self.show_error_toast(title, hint, cx);
            }
            cx.notify();
            return;
        };
        self.treemap_path.editing = false;
        self.treemap_path.error = false;
        let rejection = self.i18n.get_text("treemap_path_invalid_title");
        self.dismiss_toast_titled(&rejection, cx);
        let drive = target
            .chars()
            .next()
            .map(|letter| letter.to_ascii_uppercase());
        if let Some(drive) = drive {
            if self.drive_view != Some(drive) {
                self.open_drive_visualization(drive, window, cx);
            }
        }
        self.treemap_drill_down(&target, window, cx);
        self.focus_treemap_canvas(window, cx);
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

    /// Loads the Treemap for `scan_path` from the service's live index.
    ///
    /// The cached volume index and the stored drive tree are consulted before
    /// this call. A path they do not hold is answered by one pipe round-trip;
    /// when the service cannot answer, the canvas reports the directory as
    /// unavailable rather than scanning the disk itself.
    pub fn start_treemap_scan(&mut self, scan_path: PathBuf, cx: &mut Context<Self>) {
        self.clear_treemap_path_menu();
        let Some(ref mut state) = self.treemap_state else {
            return;
        };
        state.request_epoch = next_tree_request_epoch();
        let epoch = state.request_epoch;
        state.is_scanning = true;
        state.scan_fade_epoch = 0;
        state.content_revealed = false;
        state.unavailable = None;
        state.layout_nodes.clear();
        state.root_tree = None;
        cx.notify();

        let path_for_service = scan_path.to_string_lossy().into_owned();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            get_tokio_runtime().spawn(async move {
                let result = async {
                    let mut client = crate::service::ipc_channel::connect().await?;
                    client.get_tree(&path_for_service, MAX_TREE_DEPTH).await
                }
                .await;
                let _ = tx.send(result);
            });

            let outcome = match rx.await {
                Ok(Ok(envelope)) => match envelope.payload {
                    ResponsePayload::TreeResult { entries, .. } if !entries.is_empty() => {
                        Ok(entries)
                    }
                    ResponsePayload::TreeResult { .. } => Err(TreemapUnavailable::NotIndexed),
                    ResponsePayload::Error { code, .. } => Err(tree_unavailable_from_code(code)),
                    _ => Err(TreemapUnavailable::ServiceUnavailable),
                },
                _ => Err(TreemapUnavailable::ServiceUnavailable),
            };
            let _ = this.update(cx, |this, cx| {
                let Some(ref mut state) = this.treemap_state else {
                    return;
                };
                if state.request_epoch != epoch {
                    return;
                }
                match outcome {
                    Ok(entries) => {
                        crate::shell::perf::mark(&format!(
                            "treemap tree service entries={}",
                            entries.len()
                        ));
                        if !state.adopt_service_tree(entries) {
                            state.mark_unavailable(TreemapUnavailable::ServiceUnavailable);
                        }
                    }
                    Err(reason) => state.mark_unavailable(reason),
                }
                this.arm_treemap_scan_fade_cleanup(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Arms the Treemap scan fade cleanup timer if a new scan fade epoch is active.
    ///
    /// The timer runs on the background executor and cleans up the fade epoch
    /// after `MASK_DISSOLVE_CLEANUP`, ensuring `render()` remains pure and never
    /// spawns redundant tasks across animation frames.
    pub(crate) fn arm_treemap_scan_fade_cleanup(&mut self, cx: &mut Context<Self>) {
        let Some(ref mut state) = self.treemap_state else {
            return;
        };
        let epoch = state.scan_fade_epoch;
        if epoch == 0 || state.scan_fade_timer_armed >= epoch {
            return;
        }
        state.scan_fade_timer_armed = epoch;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(crate::shell::motion::MASK_DISSOLVE_CLEANUP)
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(ref mut state) = this.treemap_state {
                    if state.scan_fade_epoch == epoch {
                        state.scan_fade_epoch = 0;
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    pub fn treemap_drill_down(
        &mut self,
        target_path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_treemap_path_menu();
        // A navigation step ends address editing: the field describes the
        // directory the canvas is leaving, not the one it just opened.
        self.cancel_treemap_path_edit(window, cx);
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
        } else {
            self.arm_treemap_scan_fade_cleanup(cx);
        }
        self.focus_treemap_canvas(window, cx);
        cx.notify();
    }

    /// Moves to the parent directory as a history step. At the drive root the
    /// control is disabled and this does nothing.
    pub fn treemap_drill_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_treemap_path_edit(window, cx);
        let scan_target = if let Some(ref mut st) = self.treemap_state {
            st.drill_up()
        } else {
            None
        };
        if let Some(path) = scan_target {
            self.start_treemap_scan(path, cx);
        } else {
            self.arm_treemap_scan_fade_cleanup(cx);
        }
        self.focus_treemap_canvas(window, cx);
        cx.notify();
    }

    /// Returns to the directory most recently left by any drill step.
    pub fn treemap_drill_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_treemap_path_edit(window, cx);
        let scan_target = if let Some(ref mut st) = self.treemap_state {
            st.drill_back()
        } else {
            None
        };
        if let Some(path) = scan_target {
            self.start_treemap_scan(path, cx);
        } else {
            self.arm_treemap_scan_fade_cleanup(cx);
        }
        self.focus_treemap_canvas(window, cx);
        cx.notify();
    }

    pub fn treemap_drill_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_treemap_path_edit(window, cx);
        let scan_target = if let Some(ref mut st) = self.treemap_state {
            st.drill_forward()
        } else {
            None
        };
        if let Some(path) = scan_target {
            self.start_treemap_scan(path, cx);
        } else {
            self.arm_treemap_scan_fade_cleanup(cx);
        }
        self.focus_treemap_canvas(window, cx);
        cx.notify();
    }

    /// Opens the canvas selection the way a double click does: a summary block becomes a
    /// view-only layer, a directory drills down, a file opens in its associated application.
    pub fn treemap_open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(node) = self
            .treemap_state
            .as_ref()
            .and_then(|st| st.selected_node().cloned())
        else {
            return;
        };
        if node.is_aggregate() {
            let mut args = FluentArgs::new();
            args.set("count", node.aggregate_count as i64);
            args.set("count_text", format_count(node.aggregate_count));
            let label = self.i18n.get_text_with_args("treemap_aggregate", &args);
            if let Some(st) = self.treemap_state.as_mut() {
                st.open_block(&node.path, label);
            }
            cx.notify();
        } else if node.item.is_dir {
            self.treemap_drill_down(&node.path, window, cx);
        } else {
            let _ = open_file(&node.path);
        }
    }

    /// Switches back to classic "All files" table filtered by `path`.
    pub fn switch_to_table_with_path(
        &mut self,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Leaving the visualization releases the partial tree instead of keeping
        // it alive behind the table, and starts a fresh table entrance.
        self.leave_drive_view();
        self.query.active_filter = FilterCategory::All;
        self.query.search_input.set_text(&Self::parent_query(path));
        self.record_nav_state();
        self.trigger_search(cx);
        self.query.search_focus.focus(window, cx);
        cx.notify();
    }

    /// The search that lists a directory's own children. A category narrowing is not written here:
    /// it travels as query state and is spliced when the request is sent, so the box stays a path a
    /// person can read and edit.
    fn parent_query(path: &str) -> String {
        let trimmed = path.trim_end_matches(['\\', '/']);
        // A drive root must keep its separator: `parent:"C"` matches nothing, `parent:"C:\"`
        // lists the root's children. Both `C:` and `C:\` name the same root.
        let is_drive_root = (trimmed.len() == 2 && trimmed.ends_with(':')) || trimmed.len() == 1;
        if is_drive_root {
            format!("parent:\"{}:\\\"", trimmed.trim_end_matches(':'))
        } else {
            format!("parent:\"{trimmed}\"")
        }
    }

    /// The path band above the Treemap panel: the current directory and its
    /// ancestors inside the same field frame as the table's search box.
    /// Clicking an ancestor enters that directory; clicking the field (or the
    /// current directory label) turns it into a path input. Enter opens the
    /// typed directory; Escape or leaving the field restores the breadcrumb.
    pub(crate) fn render_treemap_path_band(
        &self,
        tokens: &ShellTokens,
        view: Entity<CairnApp>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let current_dir = self
            .treemap_state
            .as_ref()
            .map(|state| state.current_dir().to_string())
            .unwrap_or_default();
        let editing = self.treemap_path.editing;
        let error = self.treemap_path.error;
        let show_clear = editing && !self.treemap_path.input.read(cx).value().is_empty();
        let snap_margin = window.rem_size() * layout::SPACE_8;

        let content = if editing {
            Input::new(&self.treemap_path.input)
                .id(accessibility::control("treemap-path").element)
                .xsmall()
                .appearance(false)
                .bordered(false)
                .focus_bordered(false)
                // Both states of this field are one text: the 14px body step inside the
                // shell's default line box. The component's own 1.25rem box is shorter, and
                // a shorter box centred in the same field rounds its baseline to a different
                // device pixel, so editing sat one pixel above the address it replaced. The
                // frame padding is removed too, so both states share the field's left edge.
                .text_size(rems(1.0))
                .line_height(phi())
                .p(rems(0.0))
                .h_full()
                .flex_1()
                .min_w_0()
                .aria_label(accessibility::component_name(
                    accessibility::control("treemap-path"),
                    &self.i18n,
                ))
                .into_any_element()
        } else {
            // A virtual layer sits under the directory that owns the block, not under the canvas's
            // own current directory: the crumbs keep every real level and the layer takes the last
            // position as the current step.
            let virtual_label = self
                .treemap_state
                .as_ref()
                .and_then(|state| state.virtual_layer.as_ref())
                .map(|layer| (layer.parent_path.clone(), layer.label.clone()));
            let crumbs_root = virtual_label
                .as_ref()
                .map_or_else(|| current_dir.clone(), |(parent, _)| parent.clone());
            let segments = path_segments(&crumbs_root);
            // With a layer open no real segment is the current one: they all stay navigable, and
            // the layer itself is the level the canvas is showing.
            let last = if virtual_label.is_some() {
                usize::MAX
            } else {
                segments.len().saturating_sub(1)
            };
            let menu_for = self.treemap_path_menu_for.clone();
            let menu = self.treemap_path_menu.clone();
            // The product's focus frame belongs to the keyboard. A pointer click
            // lands on these controls without painting a second outline.
            let keyboard_navigation = cairn_ui::focus::keyboard_navigation(cx);
            // A tooltip that appears over the open menu reads as a stray label,
            // so the band stays quiet while one of its menus is up.
            let menu_open = self.treemap_path_menu.is_some();
            let mut breadcrumb = h_flex()
                .min_w_0()
                .overflow_hidden()
                .justify_start()
                .items_center()
                // No gap of its own: the arrow's 24px plate centres a 14px
                // glyph, and that padding alone is the distance to the label on
                // either side, so the separator's ink sits equidistant from both
                // by construction.
                .text_color(tokens.muted_foreground)
                // The path reads at the search box's body step, so punctuation
                // and Latin text use the same face and size in both fields.
                .text_size(rems(1.0));
            for (ix, (label, path)) in segments.into_iter().enumerate() {
                let text = div()
                    .max_w(rems(PATH_SEGMENT_MAX_WIDTH))
                    .truncate()
                    .child(label.clone());
                let is_current = ix == last;
                let segment: AnyElement = if is_current {
                    let tip = path.clone();
                    div()
                        .id(element_id("treemap-path-current", &path))
                        .flex_shrink_0()
                        .text_color(tokens.foreground)
                        // The address the truncated label stands for, anchored to
                        // the label the pointer is on rather than to the whole
                        // field, which is a thousand pixels wide.
                        .when(!menu_open, move |this| {
                            this.tooltip(move |window, cx| {
                                Tooltip::new(tip.clone()).build(window, cx)
                            })
                        })
                        .child(text)
                        .into_any_element()
                } else {
                    // Ancestors are real controls: each is a tab stop, activated
                    // by Enter, Space or a click, and framed only while the
                    // keyboard drives focus.
                    let focus = window
                        .use_keyed_state(element_id("treemap-crumb-focus", &path), cx, |_, cx| {
                            cx.focus_handle().tab_stop(true)
                        })
                        .read(cx)
                        .clone();
                    let target = path.clone();
                    let click_view = view.clone();
                    let key_view = view.clone();
                    let click_focus = focus.clone();
                    let crumb_control = accessibility::control("treemap-crumb");
                    let mut crumb_args = FluentArgs::new();
                    crumb_args.set("label", label.as_str());
                    crumb_args.set("path", path.as_str());
                    let crumb_label = self
                        .i18n
                        .get_text_with_args("treemap_crumb_label", &crumb_args);
                    accessibility::element_at_key(crumb_control, &path)
                        .aria_label(crumb_label)
                        .relative()
                        .flex_shrink(1.0)
                        .min_w_0()
                        .overflow_hidden()
                        .cursor_pointer()
                        .rounded(tokens.radius)
                        .track_focus(&focus)
                        .hover(|this| this.text_color(tokens.foreground))
                        .on_click(move |_, window, cx| {
                            click_focus.focus(window, cx);
                            click_view.update(cx, |this, cx| {
                                this.treemap_drill_down(&target, window, cx)
                            });
                            cx.stop_propagation();
                        })
                        .on_action({
                            let target = path.clone();
                            move |_: &crate::shell::commands::OpenSelected,
                                  window: &mut Window,
                                  cx: &mut App| {
                                cx.stop_propagation();
                                key_view.update(cx, |this, cx| {
                                    this.treemap_drill_down(&target, window, cx)
                                });
                            }
                        })
                        .children(cairn_ui::focus::edge(
                            &focus,
                            tokens.ring,
                            tokens.radius,
                            keyboard_navigation,
                            window,
                            cx,
                        ))
                        .when(!menu_open, {
                            let tip = path.clone();
                            move |this| {
                                this.tooltip(move |window, cx| {
                                    Tooltip::new(tip.clone()).build(window, cx)
                                })
                            }
                        })
                        .child(text)
                        .into_any_element()
                };

                // Each segment owns the menu of its own child folders. The arrow
                // is a tab stop of its own, so the crumb still only navigates.
                let menu_focus = window
                    .use_keyed_state(element_id("treemap-path-menu-focus", &path), cx, |_, cx| {
                        cx.focus_handle().tab_stop(true)
                    })
                    .read(cx)
                    .clone();
                let open = menu_for.as_deref() == Some(path.as_str());
                let menu_control = accessibility::control("treemap-path-menu");
                let mut menu_args = FluentArgs::new();
                menu_args.set("label", label.as_str());
                let menu_label = self
                    .i18n
                    .get_text_with_args("treemap_path_menu_for", &menu_args);
                let down_focus = menu_focus.clone();
                let down_view = view.clone();
                let down_path = path.clone();
                let click_view = view.clone();
                let click_path = path.clone();
                let click_focus = menu_focus.clone();
                let key_view = view.clone();
                let key_path = path.clone();
                let key_focus = menu_focus.clone();
                let toggle_label = menu_label.clone();
                let open_menu = menu.clone().filter(|_| open);
                let arrow = div()
                    .relative()
                    .flex_shrink_0()
                    .child(
                        accessibility::element_at_key(menu_control, &path)
                            .aria_label(menu_label)
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(rems(layout::SPACE_24))
                            .h(rems(layout::SPACE_24))
                            .rounded(tokens.radius)
                            .cursor_pointer()
                            .track_focus(&menu_focus)
                            .text_color(if open {
                                tokens.foreground
                            } else {
                                tokens.muted_foreground
                            })
                            .hover(|this| this.bg(tokens.row_hover).text_color(tokens.foreground))
                            .when(open, |this| this.bg(tokens.row_hover))
                            .children(cairn_ui::focus::edge(
                                &menu_focus,
                                tokens.ring,
                                tokens.radius,
                                keyboard_navigation,
                                window,
                                cx,
                            ))
                            // Toggling on mouse-down lets the open menu close
                            // before its own outside-press dismissal clears the
                            // state; a click handler would reopen it instead.
                            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                                window.prevent_default();
                                cx.stop_propagation();
                                down_focus.focus(window, cx);
                                down_view.update(cx, |this, cx| {
                                    this.toggle_treemap_path_menu(
                                        &down_path,
                                        down_focus.clone(),
                                        window,
                                        cx,
                                    )
                                });
                            })
                            .on_click(move |event: &ClickEvent, window, cx| {
                                cx.stop_propagation();
                                // Mouse activation already ran on mouse-down.
                                if matches!(event, ClickEvent::Mouse(_)) {
                                    return;
                                }
                                click_view.update(cx, |this, cx| {
                                    this.toggle_treemap_path_menu(
                                        &click_path,
                                        click_focus.clone(),
                                        window,
                                        cx,
                                    )
                                });
                            })
                            .on_action(
                                move |_: &crate::shell::commands::OpenSelected,
                                      window: &mut Window,
                                      cx: &mut App| {
                                    cx.stop_propagation();
                                    key_view.update(cx, |this, cx| {
                                        this.toggle_treemap_path_menu(
                                            &key_path,
                                            key_focus.clone(),
                                            window,
                                            cx,
                                        )
                                    });
                                },
                            )
                            // The tooltip would sit on top of the menu the same
                            // press just opened, so it belongs to the quiet state.
                            .when(!menu_open, |this| {
                                this.tooltip(move |window, cx| {
                                    Tooltip::new(toggle_label.clone()).build(window, cx)
                                })
                            })
                            // The separator and the menu trigger are one control:
                            // it points at the next path step while closed and
                            // turns down while its menu is open.
                            .child(
                                Icon::new(if open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .size(rems(theme::UI_ICON_SIZE)),
                            ),
                    )
                    .when_some(open_menu, |this, menu| {
                        this.child(deferred(
                            anchored()
                                .anchor(Anchor::TopLeft)
                                .snap_to_window_with_margin(snap_margin)
                                .child(
                                    div()
                                        .id(element_id("treemap-path-menu-pop", &path))
                                        .occlude()
                                        .top(rems(layout::TIGHT_LEAD))
                                        .child(menu),
                                ),
                        ))
                    });
                breadcrumb = breadcrumb.child(segment).child(arrow);
            }
            // The layer is a view-only level: it takes the last crumb position but is not a path,
            // so it renders as the current label with no menu, focus or editing of its own.
            if let Some((_, label)) = virtual_label {
                // The segment before it already ends with its own arrow, which is the separator
                // every crumb uses; a second one would read as another level.
                breadcrumb = breadcrumb.child(
                    div()
                        .max_w(rems(PATH_SEGMENT_MAX_WIDTH))
                        .truncate()
                        .flex_shrink_0()
                        .text_color(tokens.foreground)
                        .child(label),
                );
            }
            breadcrumb.into_any_element()
        };

        let band = h_flex()
            .h(rems(layout::SEARCH_BAND_HEIGHT))
            .px(rems(layout::TIGHT_LEAD))
            .py(rems(layout::TIGHT_LEAD))
            .bg(tokens.chrome)
            .child(
                h_flex()
                    .id("treemap-path-field")
                    .flex_1()
                    .min_w_0()
                    .h(rems(layout::ROW_HEIGHT))
                    .px(rems(layout::SEARCH_INPUT_LEAD))
                    .gap(rems(layout::INNER_SPACING))
                    .items_center()
                    .justify_start()
                    .rounded(tokens.radius)
                    .bg(if editing {
                        tokens.background
                    } else {
                        tokens.muted
                    })
                    .border_1()
                    .border_color(if error {
                        tokens.danger
                    } else if editing {
                        tokens.ring
                    } else {
                        tokens.control_stroke
                    })
                    .text_color(tokens.foreground)
                    .cursor(CursorStyle::IBeam)
                    // The field owns Enter and Escape only while it is an editor.
                    // Outside editing, Enter belongs to the focused crumb or the
                    // canvas, so the address context must not shadow it.
                    .when(editing, |this| {
                        this.key_context(crate::shell::commands::TREEMAP_PATH_CONTEXT)
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        if !this.treemap_path.editing {
                            this.begin_treemap_path_edit(window, cx);
                        }
                    }))
                    // The field's own context resolves Enter and Escape before the
                    // window's OpenSelected/EscapeQuery, which the focused input's
                    // text handling would otherwise leave unclaimed.
                    .on_action(cx.listener(
                        |this, _: &crate::shell::commands::CommitTreemapPath, window, cx| {
                            this.commit_treemap_path(window, cx);
                        },
                    ))
                    .on_action(cx.listener(
                        |this, _: &crate::shell::commands::CancelTreemapPath, window, cx| {
                            this.cancel_treemap_path_edit(window, cx);
                        },
                    ))
                    .child(
                        Icon::new(IconName::Folder)
                            .small()
                            .flex_shrink_0()
                            .text_color(tokens.muted_foreground),
                    )
                    // The breadcrumb occupies an explicit full-height lane that starts
                    // at the icon's text edge and packs its crumbs from the start; the
                    // field only places the icon, this lane and the clear control.
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .items_center()
                            .justify_start()
                            .child(content),
                    )
                    .when(show_clear, |this| {
                        this.child(crate::view::treemap_path_clear_button(
                            &self.i18n,
                            cx.listener(|this, _, window, cx| {
                                this.clear_treemap_path(window, cx);
                            }),
                        ))
                    }),
            );
        band
    }

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
        let canvas_origin = (
            sidebar_width + panel_margin,
            f32::from(layout::TITLE_BAR_HEIGHT) + band_height,
        );
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
            .on_scroll(cx.listener(move |this, event: &ScrollEvent, window, cx| {
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

    // The cell this path painted in the view being left, if the block survives
    // the navigation. New blocks fall back to their inset growth.
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

/// Whether `candidate` is `ancestor` itself or a path beneath it. Comparison is ASCII
/// case-insensitive and requires a separator boundary, so `C:\Windows` does not contain
/// `C:\WindowsApps`.
fn path_is_within(candidate: &str, ancestor: &str) -> bool {
    let ancestor = ancestor.trim_end_matches('\\');
    if candidate.eq_ignore_ascii_case(ancestor) {
        return true;
    }
    if candidate.len() <= ancestor.len() {
        return false;
    }
    let Some(prefix) = candidate.get(..ancestor.len()) else {
        return false;
    };
    if !prefix.eq_ignore_ascii_case(ancestor) {
        return false;
    }
    let rest = &candidate[ancestor.len()..];
    rest.starts_with('\\') || rest.starts_with("::")
}

fn same_directory(left: &str, right: &str) -> bool {
    left.trim_end_matches('\\')
        .eq_ignore_ascii_case(right.trim_end_matches('\\'))
}

/// The menu a summary block can honour: it lists its owning directory in the table.
fn build_block_context_menu(
    view: Entity<CairnApp>,
    parent_path: Option<String>,
    i18n: &Rc<crate::shell::i18n::I18nManager>,
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
    let i18n = i18n.clone();
    move |menu: PopupMenu, _window: &mut Window, cx: &mut Context<PopupMenu>| {
        let dismiss_view = view.clone();
        cx.subscribe_self::<DismissEvent>(move |_menu, _event, cx| {
            dismiss_view.update(cx, |this, cx| {
                if let Some(st) = this.treemap_state.as_mut() {
                    st.suppress_tooltip = false;
                }
                cx.notify();
            });
        })
        .detach();
        let release_view = view.clone();
        cx.on_release(move |_menu, cx| {
            release_view.update(cx, |this, cx| {
                if let Some(st) = this.treemap_state.as_mut() {
                    st.suppress_tooltip = false;
                }
                cx.notify();
            });
        })
        .detach();
        let table_view = view.clone();
        let table_parent = parent_path.clone();
        menu.item(PopupMenuItem::new(i18n.get_text("view_in_table")).on_click(
            move |_, window, cx| {
                if let Some(ref dir) = table_parent {
                    let dir = dir.clone();
                    table_view.update(cx, |this, cx| {
                        this.switch_to_table_with_path(&dir, window, cx);
                    });
                }
            },
        ))
    }
}

fn build_context_menu(
    view: Entity<CairnApp>,
    full_path: String,
    is_dir: bool,
    i18n: &Rc<crate::shell::i18n::I18nManager>,
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
    let i18n = i18n.clone();
    move |menu: PopupMenu, _window: &mut Window, cx: &mut Context<PopupMenu>| {
        let dismiss_view = view.clone();
        cx.subscribe_self::<DismissEvent>(move |_menu, _event, cx| {
            dismiss_view.update(cx, |this, cx| {
                if let Some(st) = this.treemap_state.as_mut() {
                    st.suppress_tooltip = false;
                }
                cx.notify();
            });
        })
        .detach();
        let release_view = view.clone();
        cx.on_release(move |_menu, cx| {
            release_view.update(cx, |this, cx| {
                if let Some(st) = this.treemap_state.as_mut() {
                    st.suppress_tooltip = false;
                }
                cx.notify();
            });
        })
        .detach();

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

    /// Selecting a directory keeps its whole subtree in focus; only the separator boundary
    /// decides, so a sibling whose name shares a prefix is outside.
    #[test]
    fn a_path_contains_itself_and_its_descendants_only() {
        assert!(path_is_within(r"C:\Windows", r"C:\Windows"));
        assert!(path_is_within(r"C:\Windows\System32", r"C:\Windows"));
        assert!(path_is_within(r"C:\Windows::<tail:Other>", r"C:\Windows"));
        assert!(!path_is_within(r"C:\Windows", r"C:\Windows\System32"));
        assert!(path_is_within(
            r"C:\Users\Alice\AppData",
            r"C:\Users\Alice\AppData\"
        ));
        assert!(!path_is_within(r"C:\WindowsApps", r"C:\Windows"));
        assert!(!path_is_within(r"C:\Win", r"C:\Windows"));
    }

    /// The table hand-off must search the directory's own children. A drive root keeps the
    /// separator the query syntax needs; a nested path is used as written.
    #[test]
    fn the_table_hand_off_queries_the_directory_children() {
        assert_eq!(
            CairnApp::parent_query("C:\\Windows\\WinSxS"),
            "parent:\"C:\\Windows\\WinSxS\""
        );
        assert_eq!(
            CairnApp::parent_query("C:\\Windows\\"),
            "parent:\"C:\\Windows\""
        );
        assert_eq!(CairnApp::parent_query("C:"), "parent:\"C:\\\"");
        assert_eq!(CairnApp::parent_query("C:\\"), "parent:\"C:\\\"");
    }

    /// The path menu reads the directory as it stands on disk: child folders
    /// only, in display order, each joined to an absolute path the Treemap can
    /// enter. A sibling file never becomes a row.
    #[test]
    fn the_path_menu_lists_child_directories_in_display_order() {
        // One temp root per test: it removes itself on drop, including when an assertion fails.
        let temp = tempfile::tempdir().expect("a temp root for the menu fixture");
        let root = temp.path().join("menu");
        std::fs::create_dir_all(&root).expect("the temp root exists");
        for name in ["Windows", "Users", "Program Files"] {
            std::fs::create_dir(root.join(name)).expect("a child directory exists");
        }
        std::fs::write(root.join("notes.md"), "").expect("a sibling file exists");

        let base = root
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_string();
        assert_eq!(
            path_menu_directories(&base),
            vec![
                (
                    "Program Files".to_string(),
                    format!(r"{base}\Program Files")
                ),
                ("Users".to_string(), format!(r"{base}\Users")),
                ("Windows".to_string(), format!(r"{base}\Windows")),
            ]
        );
    }

    /// The scan stops at the menu's cap rather than materializing a whole directory: a folder
    /// holding more children than the menu can show still yields cap-plus-one rows, and that
    /// one extra row is what tells the menu to offer the truncation note.
    #[test]
    fn the_path_menu_scan_stops_once_it_passes_the_cap() {
        let temp = tempfile::tempdir().expect("a temp root for the cap fixture");
        let root = temp.path().join("cap");
        std::fs::create_dir_all(&root).expect("the temp root exists");
        for index in 0..PATH_MENU_LIMIT + 5 {
            std::fs::create_dir(root.join(format!("folder-{index:03}")))
                .expect("a child directory exists");
        }

        let base = root
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_string();
        let listed = path_menu_directories(&base);
        assert_eq!(listed.len(), PATH_MENU_LIMIT + 1);
        assert!(listed.windows(2).all(|pair| pair[0].0 <= pair[1].0));
    }

    #[test]
    fn dimmed_leaf_and_header_fills_match_for_directory_and_file() {
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
                let header_bg_dimmed = palette.dimmed_fill(base_header);
                let dir_leaf_bg_dimmed = palette.dimmed_fill(base_header);
                let file_leaf_bg_dimmed = palette.dimmed_fill(palette.fill(category));

                assert_eq!(
                    dir_leaf_bg_dimmed, header_bg_dimmed,
                    "compact directory leaf and header must match when dimmed (category {category:?}, is_dark={is_dark})"
                );
                assert_eq!(
                    dir_leaf_bg_dimmed, file_leaf_bg_dimmed,
                    "compact directory leaf and file leaf must match when dimmed (category {category:?}, is_dark={is_dark})"
                );
            }
        }
    }

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

    #[test]
    fn revealing_a_finished_scan_keys_the_dissolve_to_that_generation() {
        let mut state = crate::treemap::state::TreemapViewState::new_for_test('C');
        state.is_scanning = true;
        state.reveal_content();
        assert!(
            state.scan_fade_epoch > 0,
            "a revealed scan starts the dissolve"
        );
        assert_eq!(
            state.scan_fade_epoch, state.reveal_generation,
            "the dissolve belongs to the generation it reveals"
        );
    }
}
