// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The main window's shell: title bar, locations sidebar, search band, command bar,
//! result table and status line.
//!
//! This module owns *layout and presentation* only. Every interaction hands the request to an owner
//! method on [`CairnApp`] (in [`crate::app`]), which is the same method the keymap reaches, so a
//! dropdown item, a table heading, a sidebar row and a keystroke cannot drift apart (Coding Guides:
//! *Events, actions, and focus*).
//!
//! Regions come from GPUI Component wherever the component fits the task: `TitleBar`, `Sidebar`,
//! `Toolbar`, `StatusBar` and `PopupMenu` own their geometry, their interaction states
//! and their keyboard model. Two regions stay drawn here, for reasons recorded at each one: the
//! query box, because the platform IME drives it through `impl EntityInputHandler for CairnApp`
//! (SPEC SM-3), and the result band, because the QRY-07 paging contract needs rows the service has
//! not delivered yet to still hold their place on the scrollbar.

use crate::app::{element_id, layout, CairnApp};
use crate::shell::accessibility::{self, ControlElement, KeyControl};
use crate::shell::command_groups::{menu_groups, GroupCommand, MenuGroup};
use crate::shell::commands::{self, WINDOW_CONTEXT, WINDOW_CONTROL_CONTEXT};
use crate::shell::theme::{self, ShellTokens};
use gpui_kit::base::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{
    h_flex, v_flex, Icon, IconName, InteractiveElementExt as _, Sizable as _,
};
use gpui_kit::gpui::prelude::FluentBuilder as _;
use gpui_kit::gpui::*;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

/// The focus frame's box around the caption glyph. It stays clear of the native window frame and
/// the rounded window corner, and leaves the full-cell hover surface untouched. The size is chosen
/// so the frame and the glyph land on the same pixel grid at the common display scales.
const WINDOW_CONTROL_FOCUS_SIZE: Pixels = px(22.0);

/// The fixed identity of one caption button. The action stays a separate argument so each call
/// site keeps its own closure.
struct WindowControl<'a> {
    control: &'static KeyControl,
    label: String,
    icon: IconName,
    area: WindowControlArea,
    focus: &'a FocusHandle,
}

/// One platform caption button. It keeps the square, full-height hit target and the
/// `window_control_area` that snap layouts and the native caption expect, while adding the
/// semantics the hand-drawn div lacked: a registered `Button` role and name, a focus stop, and
/// Enter/Space activation through GPUI's own click path.
fn render_window_control(
    control: WindowControl<'_>,
    keyboard_navigation: bool,
    window: &Window,
    tokens: &ShellTokens,
    on_click: impl Fn(&mut Window) + 'static,
) -> ControlElement {
    let WindowControl {
        control,
        label,
        icon,
        area,
        focus,
    } = control;
    let is_close = matches!(area, WindowControlArea::Close);
    // The caption is a div with a window-control hit target, not a `Button`, so Enter/Space have
    // to run the same action explicitly. `WINDOW_CONTROL_CONTEXT` binds those keys to `NoAction`
    // to keep the window's "open result" command from also firing.
    let action: std::rc::Rc<dyn Fn(&mut Window)> = std::rc::Rc::new(on_click);
    let click_action = action.clone();
    let key_action = action;
    accessibility::element(control)
        .aria_label(label)
        .tab_stop(true)
        .track_focus(focus)
        .key_context(WINDOW_CONTROL_CONTEXT)
        .window_control_area(area)
        .flex()
        .w(layout::WINDOW_CONTROL_WIDTH)
        .h_full()
        .justify_center()
        .content_center()
        .items_center()
        .text_color(tokens.foreground)
        .when(is_close, |this| {
            this.hover(|style| style.bg(tokens.danger).text_color(tokens.danger_foreground))
                .active(|style| style.bg(tokens.danger))
        })
        .when(!is_close, |this| {
            this.hover(|style| style.bg(tokens.row_hover))
                .active(|style| style.bg(tokens.row_hover))
        })
        .on_click(move |_, window, _| click_action(window))
        .on_key_down(move |event, window, _| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                key_action(window);
            }
        })
        // Same 34 px control width and full-height center as the GPUI Kit caption controls;
        // the glyph reads the shared 1rem control-icon token. The focus frame is drawn around the
        // glyph so the window's rounded corner cannot clip it.
        .child(
            div()
                .relative()
                .size(WINDOW_CONTROL_FOCUS_SIZE)
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .child(Icon::new(icon).size(rems(theme::UI_ICON_SIZE)))
                .when(keyboard_navigation && focus.is_focused(window), |this| {
                    this.child(
                        div()
                            .absolute()
                            .inset_0()
                            .border_1()
                            .border_color(tokens.foreground),
                    )
                }),
        )
}

static FIRST_SKELETON_FRAME: AtomicBool = AtomicBool::new(false);
static FIRST_ROWS_FRAME: AtomicBool = AtomicBool::new(false);

/// One row of a command-group menu, already resolved to what this frame shows.
///
/// Labels, checkmarks and availability are read out of the window *before* the menu is built, so the
/// builder closure never borrows the view it renders for, and a menu cannot show a checkmark for a
/// flag that changed after it opened.
pub struct MenuEntry {
    pub label: SharedString,
    pub checked: bool,
    pub disabled: bool,
    /// The Action this row dispatches, when the keymap already owns the row's click behaviour.
    pub action: Option<Box<dyn Action>>,
    /// The command to run when there is no Action of its own, i.e. every choice in a family.
    pub command: Option<GroupCommand>,
    /// The Action whose key binding draws this row's hint while `command` owns the click.
    ///
    /// A menu row whose key is a broader gesture than the command names it here instead of
    /// dispatching it: ClearQuery advertises Escape, but clicking must clear now rather than run
    /// the dismissal cascade and stop at an open menu or toast.
    pub hint: Option<Box<dyn Action>>,
    /// The nested menu a submenu row opens. The entity is built with the bar's menu.
    pub submenu: Option<Entity<PopupMenu>>,
    /// `true` on the first row of a section after the first, where one separator belongs.
    pub separates: bool,
}

/// Builds one group's menu. Rows that carry an Action dispatch it into `action_context`, the query
/// box's focus region: it sits inside the window key context, so the command reaches the same
/// handlers as a keystroke, and dismissing the menu returns focus to the box the user types in.
pub fn build_menu(
    menu: PopupMenu,
    entries: &[MenuEntry],
    action_context: FocusHandle,
    view: Entity<CairnApp>,
) -> PopupMenu {
    let mut menu = menu.action_context(action_context);
    for (ix, entry) in entries.iter().enumerate() {
        if entry.separates && ix > 0 {
            menu = menu.separator();
        }
        let label = entry.label.clone();
        menu = if let Some(submenu) = entry.submenu.clone() {
            menu.item(PopupMenuItem::submenu(label, submenu).disabled(entry.disabled))
        } else {
            match (&entry.action, entry.command) {
                (Some(action), _) => {
                    menu.menu_with_check(label, entry.checked, action.boxed_clone())
                }
                (None, Some(command)) => {
                    let entity = view.clone();
                    let mut item = PopupMenuItem::new(label)
                        .checked(entry.checked)
                        .disabled(entry.disabled);
                    if let Some(hint) = entry.hint.as_ref() {
                        // `PopupMenuItem` runs the click handler before the action, so the action
                        // only contributes the binding that draws the hint.
                        item = item.action(hint.boxed_clone());
                    }
                    menu.item(item.on_click(move |_, window, cx| {
                        entity.update(cx, |this, cx| this.run_group_command(command, window, cx));
                    }))
                }
                (None, None) => menu.label(label),
            }
        };
    }
    menu
}

/// Announce the keystrokes [`accessibility::shortcuts_for`] registers for `control`, when it registers
/// any (UI-03).
///
/// The query box takes its advertised key from the registry table rather than from an argument at the
/// call site: a key written into the chain still reaches AccessKit, but no check can then tell an
/// advertised shortcut from a forgotten one. Commands listed inside a component menu are the
/// exception — that menu reads the keymap for its hint, so the table that binds the key also draws it.
///
/// A region answered by several keys announces them as one list, because AccessKit carries
/// `keyboard_shortcut` as a single string.
pub(crate) fn announce_shortcut(element: ControlElement, control: &KeyControl) -> ControlElement {
    let shortcuts = accessibility::shortcuts_for(control.element);
    if shortcuts.is_empty() {
        return element;
    }
    element.aria_keyshortcuts(shortcuts.join(", "))
}

/// The query box and the command bar act through the same window entity and share its focus state,
/// so the content column is handed one bundle instead of three parameters that never separate.
struct QueryBand {
    is_query_focused: bool,
    search_focus: FocusHandle,
    view: Entity<CairnApp>,
}

/// Table parameters bundled together for rendering the content column.
struct TableBand {
    visible: Range<usize>,
    row_height: Pixels,
    results_hold_focus: bool,
}

impl Render for CairnApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _perf = crate::shell::perf::scope("render");
        let tokens = ShellTokens::current(cx);
        let row_height = layout::row_height(window);
        let toast = self
            .toast
            .clone()
            .map(|toast| self.render_toast(toast, &tokens, window, cx));
        let root_focus = self.root_focus.clone();
        let search_focus = self.query.search_focus.clone();
        let is_query_focused = search_focus.is_focused(window);
        self.query.search_input.is_focused = is_query_focused;
        // The result table ring is drawn from the render pass, so the frame that shows it has to know
        // which region the window actually holds focus in. `contains_focused`, not `is_focused`: Tab
        // walks from the region into the retry button inside it, and a ring that vanishes the moment
        // focus moves one step further in is a keyboard focus with no visible focus. The modifier is
        // input modality: a pointer click focuses the region too, and a second border around the whole
        // table on top of the row selection is noise, not a focus cue.
        let results_hold_focus = self.results.results_focus.contains_focused(window, cx)
            && self.results.keyboard_navigation;
        let view = cx.entity();

        // A header-boundary resize follows the pointer through frame-scoped window listeners.
        // They are registered from a paint callback: `Window::on_mouse_event` only accepts
        // listeners for the next frame while the window is painting, not while the tree is built.
        let drag_paint_view = view.clone();

        // Frame-scoped geometry read from the band's own scroll owner, then used to pick the window.
        // The native scroll area measures the row band itself, so a visible horizontal scrollbar or
        // any other chrome inside the region is excluded from the viewport. Only the first frame,
        // before layout has given the handle bounds, falls back to the window arithmetic.
        let measured_viewport = f32::from(self.results.rows_scroll.bounds().size.height);
        self.results.table.viewport_height = if measured_viewport > 0.0 {
            measured_viewport
        } else {
            f32::from(layout::viewport_height(window))
        };
        // The handle can hold an offset the current result set cannot reach — the list shrank, or
        // the measured viewport differs from the estimate. `visible_range` clamps internally, but
        // `rows_to_paint` and the band's top use the published value directly, so clamp it once
        // here before it decides what this frame renders.
        let row_height_f = f32::from(row_height);
        let max_scroll = self.results.table.max_scroll_top(row_height_f);
        let current_scroll = self.scrolled_pixels();
        let clamped = current_scroll.clamp(0.0, max_scroll);
        if (current_scroll - clamped).abs() > f32::EPSILON {
            let cur_x = self.results.rows_scroll.offset().x;
            self.results
                .rows_scroll
                .set_offset(Point::new(cur_x, px(-clamped)));
        }
        self.results.table.scroll_top = clamped;
        let visible = self.visible_rows(row_height);
        if crate::shell::perf::enabled() {
            if self.results.table.total_rows() == 0
                && !FIRST_SKELETON_FRAME.swap(true, Ordering::Relaxed)
            {
                crate::shell::perf::mark("skeleton first frame");
            }
            if self.results.table.total_rows() > 0
                && !FIRST_ROWS_FRAME.swap(true, Ordering::Relaxed)
            {
                crate::shell::perf::mark("rows first frame");
            }
        }
        // Wheel, pointer and keyboard page directly; a scrollbar drag only moves the scroll handle.
        // Detect the unloaded band here, after the handle's offset became this frame's scroll_top,
        // and schedule one ensure after the frame. The `is_fetching_more` guard makes the callback
        // idempotent, so a second schedule before the first lands cannot spawn a second request.
        if self.needs_visible_rows_page(visible.clone()) {
            cx.defer_in(window, |this, window, cx| {
                this.ensure_visible_rows_loaded(layout::row_height(window), cx);
            });
        }

        let title_bar = self.render_title_bar(view.clone(), window, &tokens, cx);
        let sidebar = self.render_sidebar(&tokens, window, cx);
        let content = self.render_content_column(
            &tokens,
            QueryBand {
                is_query_focused,
                search_focus,
                view,
            },
            TableBand {
                visible,
                row_height,
                results_hold_focus,
            },
            window,
            cx,
        );
        let status_line = self.render_status_line(&tokens, window, cx);

        accessibility::surface("cairn-window")
            .key_context(WINDOW_CONTEXT)
            .track_focus(&root_focus)
            .on_action(cx.listener(Self::select_previous_row))
            .on_action(cx.listener(Self::select_next_row))
            .on_action(cx.listener(Self::open_selected))
            .on_action(cx.listener(Self::reveal_selected_location))
            .on_action(cx.listener(Self::copy_full_paths))
            .on_action(cx.listener(Self::copy_file_names))
            .on_action(cx.listener(Self::copy_text_or_paths))
            .on_action(cx.listener(Self::paste_query_text))
            .on_action(cx.listener(Self::undo_query_text))
            .on_action(cx.listener(Self::redo_query_text))
            .on_action(cx.listener(Self::cut_query_text))
            .on_action(cx.listener(Self::select_all_query_text))
            .on_action(cx.listener(Self::escape_query))
            .on_action(cx.listener(Self::focus_search_box))
            .on_action(cx.listener(Self::toggle_case_sensitive))
            .on_action(cx.listener(Self::toggle_whole_word))
            .on_action(cx.listener(Self::toggle_match_path))
            .on_action(cx.listener(Self::toggle_regex))
            .on_action(cx.listener(Self::cycle_filter))
            .on_action(cx.listener(Self::show_properties))
            .on_action(cx.listener(Self::open_file_menu))
            .on_action(cx.listener(Self::open_edit_menu))
            .on_action(cx.listener(Self::open_view_menu))
            .on_action(cx.listener(Self::open_search_menu))
            .on_action(cx.listener(Self::open_tools_menu))
            .on_action(cx.listener(Self::open_help_menu))
            .on_action(
                cx.listener(|this, _: &commands::NavigateBack, window, cx| {
                    this.go_back(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &commands::NavigateForward, window, cx| {
                    this.go_forward(window, cx)
                }),
            )
            .on_action(cx.listener(Self::delete_selected))
            .on_action(cx.listener(Self::restore_selected))
            // Input modality for focus painting. Any key means the keyboard is driving, and any
            // pointer press means it is not; the result region draws its ring from this rather than
            // from focus alone.
            .on_key_down(cx.listener(|this, _: &KeyDownEvent, _, cx| {
                if !this.results.keyboard_navigation {
                    this.results.keyboard_navigation = true;
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    // A press anywhere outside a Treemap tile reaches this root
                    // handler (tiles stop propagation) and drops the Treemap
                    // selection. The canvas background does the same for its own
                    // presses; this covers the title bar, search band, sidebar
                    // gaps, status bar and other chrome.
                    let had_selection = this
                        .treemap_state
                        .as_mut()
                        .is_some_and(|state| state.clear_selection());
                    if this.results.keyboard_navigation || had_selection {
                        this.results.keyboard_navigation = false;
                        cx.notify();
                    }
                    // The caption focus frame follows the same pointer rule as the result ring.
                    if cairn_ui::focus::keyboard_navigation(cx) {
                        cairn_ui::focus::set_keyboard_navigation(false, cx);
                        cx.notify();
                    }
                }),
            )
            .flex()
            .flex_col()
            .relative()
            .size_full()
            .bg(tokens.background)
            .text_color(tokens.foreground)
            .child(title_bar)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(sidebar)
                    .child(content),
            )
            .child(status_line)
            .when_some(toast, |this, toast| this.child(toast))
            .when(self.results.column_drag.is_some(), |this| {
                let view = drag_paint_view;
                this.child(
                    canvas(
                        |_, _, _| {},
                        move |_bounds, _, window, _cx| {
                            let move_view = view.clone();
                            window.on_mouse_event(
                                move |event: &MouseMoveEvent, phase, _window, cx| {
                                    if phase == DispatchPhase::Capture {
                                        return;
                                    }
                                    move_view.update(cx, |this, cx| {
                                        let Some(drag) = this.results.column_drag else {
                                            return;
                                        };
                                        let table_width = f32::from(
                                            this.results.table_scroll.bounds().size.width,
                                        );
                                        if table_width <= 0.0 {
                                            return;
                                        }
                                        // The lanes are flex-grow ratios, so a pointer delta has to
                                        // be converted into the configured space those ratios are
                                        // written in, or the boundary drifts from the cursor.
                                        let delta_px = f32::from(event.position.x - drag.start_x);
                                        let delta = delta_px * drag.widths.total() / table_width;
                                        this.resize_columns_to(
                                            drag.boundary,
                                            drag.widths,
                                            delta,
                                            cx,
                                        );
                                    });
                                },
                            );

                            let up_view = view.clone();
                            window.on_mouse_event(move |_: &MouseUpEvent, phase, _window, cx| {
                                if phase == DispatchPhase::Capture {
                                    return;
                                }
                                up_view.update(cx, |this, cx| {
                                    if this.results.column_drag.take().is_some() {
                                        this.finish_column_resize(cx);
                                        cx.notify();
                                    }
                                });
                            });
                        },
                    )
                    .absolute()
                    .inset_0(),
                )
            })
    }
}

impl CairnApp {
    // ---- title bar ---------------------------------------------------------------
    // Window chrome and the brand, nothing else. `TitleBar` owns the drag region and the platform
    // controls, and its height is platform geometry, not product spacing. The index state that
    // changes what a result can be trusted for is stated once, in the status line next to the
    // results it qualifies; drawing it here as well spent the emphasis budget twice on one condition
    // and gave the reading order two places to look first.
    /// The Everything-style menu bar. It sits on the title-bar row immediately beside the brand, so
    /// the six menus are the window's first commands rather than a second band below the chrome.
    fn render_menu_bar(
        &self,
        view: &Entity<CairnApp>,
        tokens: &ShellTokens,
        rem_size: Pixels,
    ) -> impl IntoElement {
        h_flex().items_center().gap(rems(layout::SPACE_4)).children(
            menu_groups()
                .iter()
                .map(|spec| self.render_menu_bar_trigger(view, spec.group, tokens, rem_size)),
        )
    }

    /// One top-level menu trigger and the popup it owns. The trigger is a text button, not a glyph;
    /// the popup is anchored to its bottom-left corner, the way a desktop menu bar opens.
    fn render_menu_bar_trigger(
        &self,
        view: &Entity<CairnApp>,
        group: MenuGroup,
        tokens: &ShellTokens,
        rem_size: Pixels,
    ) -> impl IntoElement {
        let control = accessibility::control_at(&element_id("toolbar-menu", group.key()));
        let name = accessibility::name(control, &self.i18n);
        let is_open = self.open_group == Some(group);
        let menu = self.group_menu.clone();
        let mouse_view = view.clone();
        let click_view = view.clone();

        let trigger = Button::new(element_id("toolbar-menu", group.key()))
            .font_weight(FontWeight::MEDIUM)
            .accessibility_label(name)
            .ghost()
            .small()
            // The trigger takes the control step so hover/active fill the menu item, but never
            // exceeds the platform bar: at 110% the control step is taller than the 34px bar.
            .h(rems(layout::CONTROL_HEIGHT))
            .max_h(layout::TITLE_BAR_HEIGHT)
            .label(self.i18n.get_text(group.label_key()))
            .text_color(if is_open {
                tokens.foreground
            } else {
                tokens.muted_foreground
            })
            .when(is_open, |this| this.bg(tokens.row_hover))
            // Toggle on mouse-down and stop propagation. The open PopupMenu treats any click
            // outside itself as dismissal; without this, the menu would close on mouse-down and
            // the click handler would immediately reopen it, so a second click only reopened.
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
                let view = mouse_view.clone();
                view.update(cx, |this, cx| this.toggle_group(group, window, cx));
            })
            .on_click(move |event: &ClickEvent, window, cx| {
                // Mouse activation already ran on mouse-down. Only the keyboard path (Enter/Space
                // on the focused trigger) needs this handler.
                if matches!(event, ClickEvent::Mouse(_)) {
                    return;
                }
                let view = click_view.clone();
                view.update(cx, |this, cx| this.toggle_group(group, window, cx));
            });

        div()
            .relative()
            .flex()
            .items_center()
            .child(trigger)
            .when_some(menu.filter(|_| is_open), |this, menu| {
                // `PopupMenu` paints and dismisses itself with `deferred`; wrapping it in a
                // `Popover` gave its modal surface focus and left the menu's own Up/Down/Enter
                // handling unreachable. Anchor it below the trigger instead, the same shape the
                // framework's app menu bar uses, and focus the menu in `build_group_menu`.
                this.child(deferred(
                    anchored()
                        .anchor(Anchor::TopLeft)
                        .snap_to_window_with_margin(rem_size * layout::SPACE_8)
                        .child(
                            div()
                                .id(element_id("toolbar-menu-pop", group.key()))
                                .occlude()
                                .top(rems(layout::CONTROL_HEIGHT))
                                .child(menu),
                        ),
                ))
            })
    }

    fn render_title_bar(
        &self,
        view: Entity<CairnApp>,
        window: &mut Window,
        tokens: &ShellTokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let toggle_view = view.clone();
        let back_view = view.clone();
        let forward_view = view.clone();
        let can_back = self.can_go_back();
        let can_forward = self.can_go_forward();
        let is_collapsed = self.sidebar_collapsed;
        let is_maximized = window.is_maximized();
        let minimize_focus = window
            .use_keyed_state("window-minimize-focus", cx, |_, cx| {
                cx.focus_handle().tab_stop(true)
            })
            .read(cx)
            .clone();
        let maximize_focus = window
            .use_keyed_state("window-maximize-focus", cx, |_, cx| {
                cx.focus_handle().tab_stop(true)
            })
            .read(cx)
            .clone();
        let close_focus = window
            .use_keyed_state("window-close-focus", cx, |_, cx| {
                cx.focus_handle().tab_stop(true)
            })
            .read(cx)
            .clone();
        let minimize_control = accessibility::control("window-minimize");
        let minimize_label = accessibility::name(minimize_control, &self.i18n);
        let maximize_control = accessibility::control("window-maximize");
        let maximize_label =
            accessibility::name_at(maximize_control, usize::from(is_maximized), &self.i18n);
        let close_control = accessibility::control("window-close");
        let close_label = accessibility::name(close_control, &self.i18n);

        div()
            .id("cairn-title-bar")
            .flex()
            .flex_row()
            .items_center()
            .h(layout::TITLE_BAR_HEIGHT)
            .bg(tokens.chrome)
            // The left half keeps the sidebar's width, so the brand and navigation controls do not
            // move when the tree collapses. The divider belongs to the sidebar below; it stops at
            // the title bar instead of drawing a line through the window chrome.
            .child(
                h_flex()
                    .items_center()
                    .h_full()
                    .gap(rems(layout::SPACE_12))
                    .pl(rems(layout::CONTENT_INSET))
                    .pr(rems(layout::INNER_SPACING))
                    // The title bar's geometry is stable: collapsing the sidebar hides the tree
                    // below but must not slide the menu bar or the navigation controls sideways.
                    .w(rems(layout::SIDEBAR_WIDTH))
                    .flex_shrink_0()
                    .justify_between()
                    .child(
                        h_flex()
                            .items_center()
                            .gap(rems(layout::INNER_SPACING))
                            .child(
                                img(cairn_ui::brand::app_icon())
                                    .w(layout::TITLE_BAR_ICON)
                                    .h(layout::TITLE_BAR_ICON)
                                    .flex_shrink_0(),
                            )
                            .child(
                                div()
                                    .text_size(rems(theme::BRAND_TITLE_SIZE))
                                    .line_height(relative(theme::SERIF_LINE_HEIGHT))
                                    .font(theme::serif_font(FontWeight::SEMIBOLD))
                                    .text_color(tokens.foreground)
                                    .child(self.i18n.get_text("app_title")),
                            ),
                    )
                    .child(
                        h_flex()
                            .items_center()
                            .gap(rems(layout::SPACE_4))
                            .child(
                                Button::new("nav-back")
                                    .ghost()
                                    .small()
                                    .icon(
                                        Icon::new(IconName::ArrowLeft)
                                            .size(rems(theme::UI_ICON_SIZE)),
                                    )
                                    .disabled(!can_back)
                                    .accessibility_label(accessibility::name(
                                        accessibility::control("nav-back"),
                                        &self.i18n,
                                    ))
                                    .on_click(move |_, window, cx| {
                                        back_view.update(cx, |this, cx| this.go_back(window, cx));
                                    }),
                            )
                            .child(
                                Button::new("nav-forward")
                                    .ghost()
                                    .small()
                                    .icon(
                                        Icon::new(IconName::ArrowRight)
                                            .size(rems(theme::UI_ICON_SIZE)),
                                    )
                                    .disabled(!can_forward)
                                    .accessibility_label(accessibility::name(
                                        accessibility::control("nav-forward"),
                                        &self.i18n,
                                    ))
                                    .on_click(move |_, window, cx| {
                                        forward_view
                                            .update(cx, |this, cx| this.go_forward(window, cx));
                                    }),
                            )
                            .child(
                                // The toggle keeps the small control's own body, so its hover
                                // fill cannot extend past the icon row.
                                Button::new("sidebar-toggle")
                                    .ghost()
                                    .small()
                                    .icon(
                                        Icon::new(if is_collapsed {
                                            IconName::PanelLeftOpen
                                        } else {
                                            IconName::PanelLeftClose
                                        })
                                        .size(rems(theme::UI_ICON_SIZE)),
                                    )
                                    .accessibility_label(accessibility::name(
                                        accessibility::control("sidebar-toggle"),
                                        &self.i18n,
                                    ))
                                    .on_click(move |_, _window, cx| {
                                        toggle_view.update(cx, |this, cx| this.toggle_sidebar(cx));
                                    }),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .items_center()
                    .child(
                        h_flex()
                            .items_center()
                            .h_full()
                            // Button's small Ghost padding (0.5rem) is the rest of the lead, so the
                            // first visible character lands on the same 0.75rem baseline as the
                            // search glyph and the Name heading.
                            .pl(rems(layout::MENU_LEAD))
                            .child(self.render_menu_bar(&view, tokens, window.rem_size())),
                    )
                    .child(
                        div()
                            .id("title-bar-drag")
                            .flex_1()
                            .h_full()
                            .window_control_area(WindowControlArea::Drag)
                            .on_double_click(|_, window, _| window.zoom_window()),
                    ),
            )
            .child(
                h_flex()
                    .id("window-controls")
                    .items_center()
                    .flex_shrink_0()
                    .h_full()
                    .child(render_window_control(
                        WindowControl {
                            control: minimize_control,
                            label: minimize_label,
                            icon: IconName::WindowMinimize,
                            area: WindowControlArea::Min,
                            focus: &minimize_focus,
                        },
                        cairn_ui::focus::keyboard_navigation(cx),
                        window,
                        tokens,
                        |window| window.minimize_window(),
                    ))
                    .child(render_window_control(
                        WindowControl {
                            control: maximize_control,
                            label: maximize_label,
                            icon: if is_maximized {
                                IconName::WindowRestore
                            } else {
                                IconName::WindowMaximize
                            },
                            area: WindowControlArea::Max,
                            focus: &maximize_focus,
                        },
                        cairn_ui::focus::keyboard_navigation(cx),
                        window,
                        tokens,
                        |window| window.zoom_window(),
                    ))
                    .child(render_window_control(
                        WindowControl {
                            control: close_control,
                            label: close_label,
                            icon: IconName::WindowClose,
                            area: WindowControlArea::Close,
                            focus: &close_focus,
                        },
                        cairn_ui::focus::keyboard_navigation(cx),
                        window,
                        tokens,
                        |window| window.remove_window(),
                    )),
            )
    }

    // ---- content column ----------------------------------------------------------

    fn render_content_column(
        &mut self,
        tokens: &ShellTokens,
        band: QueryBand,
        table_band: TableBand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        if self.drive_view.is_some() {
            let body = self.render_treemap_body(tokens, window, cx);
            return v_flex()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .bg(tokens.background)
                .child(body);
        }

        let QueryBand {
            is_query_focused,
            search_focus,
            view,
        } = band;
        let query =
            self.render_search_band(tokens, is_query_focused, search_focus, view.clone(), cx);
        let results = self.render_results(
            tokens,
            table_band.visible,
            table_band.row_height,
            table_band.results_hold_focus,
            cx,
        );

        v_flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .bg(tokens.background)
            .child(query)
            .child(results)
    }
}
