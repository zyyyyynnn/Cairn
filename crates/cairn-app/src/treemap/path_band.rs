// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Treemap path band: the breadcrumb above the canvas, the child-folder menus
//! under its segments and the address field that opens a typed directory.

use crate::app::{element_id, layout, CairnApp};
use crate::shell::accessibility;
use crate::shell::status_bar::format_count;
use crate::shell::theme::{self, ShellTokens};
use crate::treemap::dir_menu::{list_menu_directories, PATH_MENU_LIMIT};
use crate::treemap::paint::text_width;
use crate::treemap::path_input::normalize_treemap_path;
use cairn_core::format_size_bytes;
use cairn_core::treemap::path_segments;
use fluent_bundle::FluentArgs;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{h_flex, ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::gpui::prelude::FluentBuilder as _;
use gpui_kit::gpui::*;

/// One breadcrumb label never takes more than a readable share of the path band; the
/// full path remains in the band's tooltip.
const PATH_SEGMENT_MAX_WIDTH: f32 = 24.0;
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

impl CairnApp {
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
        self.treemap_dir_loader.cancel();
        self.treemap_path_menu_dismiss = None;
        self.treemap_path_menu = None;
        self.treemap_path_menu_for = None;
        self.treemap_path_menu_focus = None;
    }

    /// Opens the child-folder menu under one breadcrumb segment. The filesystem scan runs off the
    /// UI thread behind an epoch guard so toggling or navigating drops stale results cleanly.
    pub(crate) fn toggle_treemap_path_menu(
        &mut self,
        segment: &str,
        trigger_focus: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let same_segment = self.treemap_path_menu_for.as_deref() == Some(segment)
            || self.treemap_dir_loader.is_pending_for(segment);
        if same_segment {
            self.clear_treemap_path_menu();
            cx.notify();
            return;
        }
        self.clear_treemap_path_menu();
        let epoch = self.treemap_dir_loader.begin(segment);
        let segment_for_io = segment.to_string();
        let segment_for_ui = segment_for_io.clone();
        let window_handle = window.window_handle();
        let (tx, rx) = tokio::sync::oneshot::channel();
        crate::service::ipc_tasks::get_tokio_runtime().spawn_blocking(move || {
            let _ = tx.send(list_menu_directories(&segment_for_io));
        });
        cx.spawn(async move |this, cx| {
            let Ok(directories) = rx.await else {
                return;
            };
            let _ = cx.update_window(window_handle, |_, window, cx| {
                if let Some(view) = this.upgrade() {
                    view.update(cx, |this, cx| {
                        if !this.treemap_dir_loader.accept(epoch) {
                            return;
                        }
                        this.install_treemap_path_menu(
                            &segment_for_ui,
                            directories,
                            trigger_focus,
                            window,
                            cx,
                        );
                    });
                }
            });
        })
        .detach();
    }

    fn install_treemap_path_menu(
        &mut self,
        segment: &str,
        directories: Vec<(String, String)>,
        trigger_focus: FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        // A canvas root whose folded tail cannot carry a tile label hosts it
        // here instead of painting a sliver in the body.
        let root_tail = self
            .treemap_state
            .as_ref()
            .and_then(|state| state.root_tail.clone());
        let root_tail_chip = root_tail.map(|tail| {
            let mut args = FluentArgs::new();
            args.set("count", tail.count as i64);
            args.set("count_text", format_count(tail.count));
            let label = self.i18n.get_text_with_args("treemap_aggregate", &args);
            let tail_size = format_size_bytes(tail.size);
            let tooltip = SharedString::from(format!("{label}\n{tail_size}"));
            let chip_focus = window
                .use_keyed_state(
                    element_id("treemap-root-tail-focus", &tail.path),
                    cx,
                    |_, cx| cx.focus_handle().tab_stop(true),
                )
                .read(cx)
                .clone();
            let keyboard_navigation = cairn_ui::focus::keyboard_navigation(cx);
            let chip_tail = tail.clone();
            let key_tail = tail.clone();
            let chip_view = view.clone();
            let key_view = view.clone();
            h_flex()
                .id(element_id("treemap-root-tail", &tail.path))
                .flex_shrink_0()
                .items_center()
                .gap_x(rems(layout::SPACE_8))
                .h(rems(layout::SPACE_24))
                .px(rems(layout::SPACE_8))
                .rounded(tokens.radius)
                .bg(tokens.background)
                .text_color(tokens.muted_foreground)
                .text_sm()
                .cursor_pointer()
                .aria_label(SharedString::from(format!("{label} {tail_size}")))
                .track_focus(&chip_focus)
                .hover(|this| this.bg(tokens.row_hover).text_color(tokens.foreground))
                .when(!editing, move |this| {
                    this.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                })
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    let tail = chip_tail.clone();
                    chip_view.update(cx, |this, cx| {
                        this.open_tail_listing(&tail, window, cx);
                    });
                    cx.stop_propagation();
                })
                .on_action(
                    move |_: &crate::shell::commands::OpenSelected,
                          window: &mut Window,
                          cx: &mut App| {
                        cx.stop_propagation();
                        let tail = key_tail.clone();
                        key_view.update(cx, |this, cx| {
                            this.open_tail_listing(&tail, window, cx);
                        });
                    },
                )
                .child(Icon::new(IconName::FileStack).xsmall())
                .child(div().whitespace_nowrap().child(label))
                .children(cairn_ui::focus::edge(
                    &chip_focus,
                    tokens.ring,
                    tokens.radius,
                    keyboard_navigation,
                    window,
                    cx,
                ))
                .into_any_element()
        });

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
            let segments = path_segments(&current_dir);
            let last = segments.len().saturating_sub(1);
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
                    .when_some(root_tail_chip, |this, chip| this.child(chip))
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
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::*;

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
            list_menu_directories(&base),
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
        let listed = list_menu_directories(&base);
        assert_eq!(listed.len(), PATH_MENU_LIMIT + 1);
        assert!(listed.windows(2).all(|pair| pair[0].0 <= pair[1].0));
    }
}
