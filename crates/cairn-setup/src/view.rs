// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The installer window. One column, one decision per page, a fixed footer, and the same chrome
//! and grayscale palette as the product it installs.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable as _};
use gpui_kit::gpui::prelude::FluentBuilder as _;
use gpui_kit::gpui::*;

use cairn_ui::brand::{
    app_icon, serif_font, tabular_figures, BRAND_DESCRIPTOR_SIZE, BRAND_TITLE_SIZE, RADIUS,
    SERIF_LINE_HEIGHT, UI_ICON_SIZE,
};

use crate::app::{visible_progress, SetupApp};
use crate::model::Screen;

/// One physical platform surface: the caption row is 34 px at every zoom, like the product shell.
const TITLE_BAR_HEIGHT: Pixels = px(34.0);
const CAPTION_WIDTH: Pixels = px(34.0);
/// The focus frame's box around the caption glyph. It stays clear of the window's native frame
/// and rounded corner, and leaves the full-cell hover surface untouched. The size is chosen so the
/// frame and the glyph land on the same pixel grid at the common display scales.
const CAPTION_FOCUS_SIZE: Pixels = px(22.0);
/// Chrome horizontal inset (12 px), matching the product shell's title bar and footer margins.
const PAGE_INSET: Pixels = px(12.0);
/// Single vertical spacing token (16 px) for the main body area between title, description,
/// inputs, cards, and checkboxes.
const MAIN_GAP: Pixels = px(16.0);
/// Inline tight group spacing (8 px) for related controls on the same row.
const GROUP_GAP: Pixels = px(8.0);
const CARD_PAD: Pixels = px(12.0);
/// Action bar height: 40 px, leaving 6 px around the 28 px medium buttons.
const FOOTER_HEIGHT: Pixels = px(40.0);
/// The title-bar app mark token, separate from the 16 px wordmark.
const TITLE_BAR_ICON: Pixels = cairn_ui::brand::APP_ICON_SIZE;

/// Page titles use the shared serif set at the body size plus one step; the status icon matches
/// that 16 px measure so icon and title read as one unit.
const PAGE_TITLE_SIZE: f32 = 16.0 / 14.0;
const PAGE_ICON: Pixels = px(18.0);
const CHECK_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 14 14" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3.5 7.5 6 10l4.5-5.5"/></svg>"#;

/// The path field's accessible name carries its current validation message because the input
/// component exposes no description attribute. The visible Alert and the focus returned to the
/// field remain the primary error treatment.
fn path_accessible_label(label: &str, error: Option<&str>) -> String {
    match error {
        Some(error) => format!("{label}: {error}"),
        None => label.to_string(),
    }
}

/// Whether the failure page offers Try again. A missing product, a damaged payload or package,
/// and an unsupported package cannot be retried from this window.
fn failure_offers_retry(error_key: &str) -> bool {
    !matches!(
        error_key,
        "error_not_installed"
            | "error_no_payload"
            | "error_msi_platform"
            | "error_msi_invalid"
            | "error_msi_cleanup_failed"
    )
}

/// The line box and baseline offset the text system actually uses for one shaped line.
/// The title-bar lockup positions both runs from these values, so their baselines coincide
/// in layout coordinates instead of relying on centered line boxes or a hand-tuned offset.
fn line_metrics(
    window: &Window,
    text: &str,
    font: Font,
    font_size: Pixels,
    color: Hsla,
) -> (Pixels, Pixels) {
    let run = TextRun {
        len: text.len(),
        font,
        color,
        ..Default::default()
    };
    let layout = window
        .text_system()
        .layout_line(text, font_size, &[run], None);
    let line_height = (layout.ascent + layout.descent).round();
    let baseline = (line_height - layout.ascent - layout.descent) / 2.0 + layout.ascent;
    (line_height, baseline)
}

#[derive(Clone, Copy)]
struct Tokens {
    background: Hsla,
    foreground: Hsla,
    muted_foreground: Hsla,
    border: Hsla,
    control_stroke: Hsla,
    danger: Hsla,
    danger_fill: Hsla,
    danger_hover: Hsla,
    danger_active: Hsla,
    danger_label: Hsla,
    muted: Hsla,
    row_hover: Hsla,
    ring: Hsla,
}

/// One destructive control. The action's identity, label and activation travel together, so the
/// places that render one cannot pair an id with the wrong sentence.
struct DangerAction<'a> {
    id: &'static str,
    label_key: &'static str,
    focus: &'a FocusHandle,
    on_activate: fn(&mut SetupApp, &mut Context<SetupApp>),
}

impl Tokens {
    fn new(cx: &App) -> Self {
        let theme = cx.theme();
        Self {
            background: theme.background,
            foreground: theme.foreground,
            muted_foreground: theme.muted_foreground,
            border: theme.border,
            control_stroke: cairn_ui::theme::control_stroke(theme),
            danger: theme.danger,
            danger_fill: theme.tokens.button_danger.color,
            danger_hover: theme.tokens.button_danger_hover.color,
            danger_active: theme.tokens.button_danger_active.color,
            danger_label: theme.tokens.button_danger_foreground.color,
            muted: theme.muted,
            row_hover: theme.list_hover,
            ring: theme.ring,
        }
    }
}

#[derive(Clone, Copy)]
enum CaptionAction {
    Minimize,
    Close,
}

/// One title-bar control: its stable id, accessible label, glyph and action.
struct CaptionButton {
    id: &'static str,
    label: String,
    icon: IconName,
    action: CaptionAction,
}

impl SetupApp {
    fn caption_button(
        &self,
        window: &mut Window,
        button: CaptionButton,
        tokens: &Tokens,
        cx: &mut Context<SetupApp>,
    ) -> AnyElement {
        let CaptionButton {
            id,
            label,
            icon,
            action,
        } = button;
        let is_close = matches!(action, CaptionAction::Close);
        // The caption buttons are the window's only minimize and close, so they are Tab stops like
        // any other control: a handle passed to `track_focus` carries its own tab-stop flag.
        let focus = window
            .use_keyed_state(id, cx, |_, cx| cx.focus_handle().tab_stop(true))
            .read(cx)
            .clone();

        div()
            .id(id)
            .role(Role::Button)
            .aria_label(label)
            .track_focus(&focus)
            .flex()
            .w(CAPTION_WIDTH)
            .h_full()
            .items_center()
            .justify_center()
            .text_color(tokens.foreground)
            .when(is_close, |this| {
                this.hover(|style| style.bg(tokens.danger).text_color(tokens.background))
                    .active(|style| style.bg(tokens.danger).text_color(tokens.background))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.close_window(window, cx);
                    }))
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            this.close_window(window, cx);
                        }
                    }))
            })
            .when(!is_close, |this| {
                this.hover(|style| style.bg(tokens.row_hover))
                    .active(|style| style.bg(tokens.row_hover))
                    .on_click(|_, window, _| window.minimize_window())
                    .on_key_down(|event, window, _| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            window.minimize_window();
                        }
                    })
            })
            .child(
                div()
                    .relative()
                    .size(CAPTION_FOCUS_SIZE)
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(icon).size(rems(UI_ICON_SIZE)))
                    .when_some(
                        cairn_ui::focus::edge(
                            &focus,
                            tokens.foreground,
                            px(0.0),
                            cairn_ui::focus::keyboard_navigation(cx),
                            window,
                            cx,
                        ),
                        |this, ring| this.child(ring),
                    ),
            )
            .into_any_element()
    }
}

impl Render for SetupApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = Tokens::new(cx);
        if self.focused_screen != Some(self.screen) {
            self.focused_screen = Some(self.screen);
            window.focus(&self.screen_focus, cx);
        }
        v_flex()
            .id("setup-root")
            .size_full()
            .bg(tokens.background)
            .text_color(tokens.foreground)
            // The window's own key context carries the Escape gesture, so it answers wherever
            // focus happens to be. It is separate from the title bar's control context, which
            // belongs to the platform window buttons. The root tracks the window's focus handle:
            // the keymap reads a binding's context from the dispatch path of the focused node,
            // and a window with nothing focused resolves to the window chrome's own node, which
            // does not carry this context. Holding the window's focus keeps it on the path.
            .key_context(crate::app::WINDOW_CONTEXT)
            .track_focus(&self.root_focus)
            .on_action(
                cx.listener(|this, _: &crate::app::CancelSetup, window, cx| {
                    this.on_escape(window, cx)
                }),
            )
            // Input modality for focus painting. The app-level keystroke interceptor records the
            // keyboard side, including the Tab the focus walk consumes; a pointer press clears it
            // before the next frame, so a click never paints the ring.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_this, _: &MouseDownEvent, _, cx| {
                    if cairn_ui::focus::keyboard_navigation(cx) {
                        cairn_ui::focus::set_keyboard_navigation(false, cx);
                        cx.notify();
                    }
                }),
            )
            .child(self.render_titlebar(&tokens, window, cx))
            .child(self.render_body(&tokens, window, cx))
            .child(self.render_footer(&tokens, window, cx))
    }
}

impl SetupApp {
    fn render_titlebar(
        &self,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let subtitle = if self.is_uninstall() {
            "uninstall_subtitle"
        } else {
            "setup_subtitle"
        };
        let subtitle_text = self.i18n.get(subtitle);
        let title_text = self.i18n.get("app_title");
        let wordmark_font = serif_font(FontWeight::SEMIBOLD);
        let descriptor_font = serif_font(FontWeight::NORMAL);
        let rem = window.rem_size();
        let (wordmark_line, wordmark_baseline) = line_metrics(
            window,
            &title_text,
            wordmark_font.clone(),
            rem * BRAND_TITLE_SIZE,
            tokens.foreground,
        );
        let (descriptor_line, descriptor_baseline) = line_metrics(
            window,
            &subtitle_text,
            descriptor_font.clone(),
            rem * BRAND_DESCRIPTOR_SIZE,
            tokens.muted_foreground,
        );
        let descriptor_top = wordmark_baseline - descriptor_baseline;
        let wordmark = div()
            .text_size(rems(BRAND_TITLE_SIZE))
            .line_height(wordmark_line)
            .font(wordmark_font)
            .child(title_text);
        let descriptor = div()
            .text_size(rems(BRAND_DESCRIPTOR_SIZE))
            .line_height(descriptor_line)
            .mt(descriptor_top)
            .font(descriptor_font)
            .text_color(tokens.muted_foreground)
            .child(subtitle_text);
        let lockup = h_flex()
            .items_start()
            .gap(GROUP_GAP)
            .child(wordmark)
            .child(descriptor);
        h_flex()
            .id("setup-titlebar")
            .h(TITLE_BAR_HEIGHT)
            .flex_shrink_0()
            .items_center()
            .border_b_1()
            .border_color(tokens.border)
            .bg(tokens.background)
            .child(
                h_flex()
                    .h_full()
                    .items_center()
                    .gap(GROUP_GAP)
                    .pl(PAGE_INSET)
                    .child(
                        img(app_icon())
                            .w(TITLE_BAR_ICON)
                            .h(TITLE_BAR_ICON)
                            .flex_shrink_0(),
                    )
                    .child(lockup),
            )
            .child(
                div()
                    .id("setup-titlebar-drag")
                    .flex_1()
                    .h_full()
                    .window_control_area(WindowControlArea::Drag),
            )
            .child(self.caption_button(
                window,
                CaptionButton {
                    id: "setup-minimize",
                    label: self.i18n.get("window_minimize"),
                    icon: IconName::WindowMinimize,
                    action: CaptionAction::Minimize,
                },
                tokens,
                cx,
            ))
            .when(!self.is_busy(), |this| {
                this.child(self.caption_button(
                    window,
                    CaptionButton {
                        id: "setup-close",
                        label: self.i18n.get("window_close"),
                        icon: IconName::WindowClose,
                        action: CaptionAction::Close,
                    },
                    tokens,
                    cx,
                ))
            })
    }

    fn render_body(
        &self,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let page = match self.screen {
            Screen::License => self.render_license(tokens, window, cx),
            Screen::Location => self.render_location(tokens, window, cx),
            Screen::Installing => self.render_installing(tokens),
            Screen::Finished => self.render_installed(tokens, true),
            Screen::Failed => self.render_failed(tokens),
            Screen::Maintenance => self.render_installed(tokens, false),
            Screen::Uninstall => self.render_uninstall_confirm(tokens, cx),
            Screen::Uninstalling => self.render_uninstalling(tokens),
            Screen::Uninstalled => self.render_uninstalled(tokens),
        };
        div()
            .id("setup-body")
            .role(Role::Main)
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(v_flex().w_full().px(PAGE_INSET).py(MAIN_GAP).child(page))
    }

    fn render_license(
        &self,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let license_card = h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap(GROUP_GAP)
            .px(CARD_PAD)
            .py(GROUP_GAP)
            .border_1()
            .border_color(tokens.border)
            .rounded(RADIUS)
            .bg(tokens.muted)
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.i18n.get("license_name")),
            )
            .child(
                Button::new("setup-view-license")
                    .small()
                    .label(self.i18n.get("action_view_license"))
                    .border_color(tokens.control_stroke)
                    .on_click(cx.listener(|this, _, _, cx| this.view_license(cx))),
            );

        v_flex()
            .gap(MAIN_GAP)
            .child(self.heading("license_title"))
            .child(license_card)
            // The agreement and the sentence that explains it belong together: the error hangs
            // under the row it belongs to instead of taking a block of its own, so the page keeps
            // inside the window's body without scrolling.
            .child(
                v_flex()
                    .child(
                        self.license_checkbox(tokens, window, cx, |this, checked, _, cx| {
                            this.set_accepted(checked, cx);
                        }),
                    )
                    .when_some(self.notice, |this, key| {
                        this.child(
                            div()
                                .id("setup-license-notice")
                                .role(Role::Alert)
                                .text_sm()
                                .line_height(relative(1.5))
                                .text_color(tokens.danger)
                                .child(self.i18n.get(key)),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_location(
        &self,
        tokens: &Tokens,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = self.i18n.get("install_path_label");
        let path_error = self
            .path_error
            .map(|problem| self.i18n.get(problem.message_key()));
        let accessible_label = path_accessible_label(&label, path_error.as_deref());
        let path_focus = self.path_input.read(cx).focus_handle(cx);
        let path_focused = path_focus.is_focused(window);
        v_flex()
            .gap(MAIN_GAP)
            .child(self.heading("location_title"))
            .child(
                div()
                    .text_sm()
                    .line_height(relative(1.0))
                    .text_color(tokens.muted_foreground)
                    .child(label.clone()),
            )
            // The path field and the sentence that explains it belong together: the error hangs
            // under the field it belongs to instead of taking a block of its own, so a message
            // that wraps to a second line still keeps the page inside the window's body.
            .child(
                v_flex()
                    .child(
                        h_flex()
                            .gap(GROUP_GAP)
                            .items_center()
                            .child(
                                div().flex_1().min_w_0().child(
                                    Input::new(&self.path_input)
                                        .aria_label(accessible_label)
                                        // The Input's own focus style recolours the border with
                                        // `theme.ring`, which is the resting stroke; the wrapper's
                                        // foreground border below is the visible focus indicator.
                                        .focus_bordered(false)
                                        .border_color(if self.path_error.is_some() {
                                            tokens.danger
                                        } else if path_focused {
                                            tokens.foreground
                                        } else {
                                            tokens.control_stroke
                                        })
                                        .w_full(),
                                ),
                            )
                            .child(
                                Button::new("setup-browse")
                                    .label(self.i18n.get("action_browse"))
                                    .border_color(tokens.control_stroke)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.browse(window, cx);
                                    })),
                            ),
                    )
                    .when_some(path_error, |this, message| {
                        this.child(
                            div()
                                .id("setup-path-error")
                                .role(Role::Alert)
                                .text_sm()
                                .line_height(relative(1.5))
                                .text_color(tokens.danger)
                                .child(message),
                        )
                    }),
            )
            .when(self.path_error.is_none(), |this| {
                this.child(
                    h_flex()
                        .gap(GROUP_GAP)
                        .text_sm()
                        .text_color(tokens.muted_foreground)
                        .child(self.i18n.get("install_scope"))
                        .child("·")
                        .child(self.i18n.get("install_space")),
                )
            })
            .into_any_element()
    }

    fn render_installing(&self, tokens: &Tokens) -> AnyElement {
        let progress = visible_progress(self.progress);
        v_flex()
            .gap(MAIN_GAP)
            .child(self.heading("progress_title"))
            .child(
                v_flex()
                    .gap(GROUP_GAP)
                    .child(
                        h_flex()
                            .justify_between()
                            .text_sm()
                            .text_color(tokens.muted_foreground)
                            .child(self.i18n.get(self.stage.message_key()))
                            .child(div().font_features(tabular_figures()).child(
                                if self.progress.is_some() {
                                    self.i18n.format(
                                        "progress_percent",
                                        &[("percent", &format!("{progress:.0}"))],
                                    )
                                } else {
                                    String::new()
                                },
                            )),
                    )
                    .child(
                        Progress::new("setup-progress")
                            .value(progress)
                            .loading(self.progress.is_none())
                            .accessibility_label(self.i18n.get(self.stage.message_key())),
                    ),
            )
            .into_any_element()
    }

    /// One installed surface for both contexts: the success page after a fresh install and the
    /// maintenance page shown when setup is launched on an installed machine. The header, icon and
    /// actions are identical; only the supporting line changes.
    fn render_installed(&self, tokens: &Tokens, fresh: bool) -> AnyElement {
        let icon = Icon::new(IconName::CircleCheck)
            .size(PAGE_ICON)
            .text_color(tokens.foreground)
            .into_any_element();
        let mut page = v_flex()
            .gap(MAIN_GAP)
            .child(self.title_row(icon, "finish_title"));
        if fresh {
            page = page.child(self.paragraph("finish_body", tokens));
            if self.restart_required {
                page = page.child(
                    div()
                        .text_sm()
                        .text_color(tokens.muted_foreground)
                        .child(self.i18n.get("finish_restart")),
                );
            }
        } else {
            let version = self
                .installed
                .as_ref()
                .map(|product| product.version.as_str())
                .unwrap_or_default();
            let version_line = if self
                .installed
                .as_ref()
                .is_some_and(|product| product.is_newer_than_package())
            {
                self.i18n
                    .format("maintenance_newer", &[("version", version)])
            } else {
                self.i18n
                    .format("maintenance_installed", &[("version", version)])
            };
            page = page.child(
                div()
                    .text_sm()
                    .text_color(tokens.foreground)
                    .child(version_line),
            );
        }
        if let Some(notice) = self.notice {
            page = page.child(
                div()
                    .id("setup-installed-notice")
                    .role(Role::Alert)
                    .text_sm()
                    .text_color(tokens.danger)
                    .child(self.i18n.get(notice)),
            );
        }
        page.into_any_element()
    }

    fn render_failed(&self, tokens: &Tokens) -> AnyElement {
        let uninstall = self.is_uninstall() || self.uninstall_attempted;
        let missing_install = uninstall
            && self
                .error
                .as_ref()
                .is_some_and(|error| error.key == "error_not_installed");
        let title = if missing_install {
            "uninstall_missing_title"
        } else if uninstall {
            "error_uninstall_title"
        } else {
            "error_title"
        };
        let detail = if missing_install {
            Some(self.i18n.get("uninstall_missing_body"))
        } else {
            self.error.as_ref().map(|error| {
                // The uninstall flow names the same failures in uninstall terms: a person who
                // came to remove Cairn must not read the installer's wording.
                let key = if uninstall {
                    crate::model::uninstall_error_key(error.key)
                } else {
                    error.key
                };
                // The raw MSI code is an internal token: it belongs in the log, not on the page.
                // The page says what happened and points at the log entry.
                self.i18n.get(key)
            })
        };

        let icon = Icon::new(IconName::CircleAlert)
            .size(PAGE_ICON)
            .text_color(tokens.danger)
            .into_any_element();
        let mut page = v_flex().gap(MAIN_GAP).child(self.title_row(icon, title));

        if let Some(detail) = detail {
            page = page.child(
                div()
                    .id("setup-failed-detail")
                    .role(Role::Alert)
                    .text_sm()
                    .line_height(relative(1.5))
                    .text_color(tokens.foreground)
                    .child(detail),
            );
        }
        if let Some(notice) = self.notice {
            page = page.child(
                div()
                    .id("setup-failed-notice")
                    .role(Role::Alert)
                    .text_sm()
                    .text_color(tokens.danger)
                    .child(self.i18n.get(notice)),
            );
        }
        page.into_any_element()
    }

    fn render_uninstall_confirm(&self, tokens: &Tokens, _cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .gap(MAIN_GAP)
            .child(self.heading("uninstall_title"))
            .child(self.paragraph("uninstall_body", tokens))
            .when_some(self.notice, |this, key| {
                this.child(
                    div()
                        .id("setup-uninstall-notice")
                        .role(Role::Alert)
                        .text_sm()
                        .text_color(tokens.danger)
                        .child(self.i18n.get(key)),
                )
            })
            .into_any_element()
    }

    fn render_uninstalling(&self, tokens: &Tokens) -> AnyElement {
        let progress = visible_progress(self.progress);
        let working = self.i18n.get("uninstall_progress_working");
        v_flex()
            .gap(MAIN_GAP)
            .child(self.heading("uninstall_progress_title"))
            .child(
                v_flex()
                    .gap(GROUP_GAP)
                    .child(
                        h_flex()
                            .justify_between()
                            .text_sm()
                            .text_color(tokens.muted_foreground)
                            .child(working.clone())
                            .child(div().font_features(tabular_figures()).child(
                                if self.progress.is_some() {
                                    self.i18n.format(
                                        "progress_percent",
                                        &[("percent", &format!("{progress:.0}"))],
                                    )
                                } else {
                                    String::new()
                                },
                            )),
                    )
                    .child(
                        Progress::new("setup-uninstall-progress")
                            .value(progress)
                            .loading(self.progress.is_none())
                            .accessibility_label(working),
                    ),
            )
            .into_any_element()
    }

    fn render_uninstalled(&self, tokens: &Tokens) -> AnyElement {
        let icon = Icon::new(IconName::CircleCheck)
            .size(PAGE_ICON)
            .text_color(tokens.foreground)
            .into_any_element();
        // A forced cleanup can finish with the Windows Installer record still in place. The page
        // says what was removed and what remains instead of claiming a complete uninstall.
        let title = if self.record_remaining {
            "uninstall_files_removed_title"
        } else {
            "uninstall_done_title"
        };
        v_flex()
            .gap(MAIN_GAP)
            .child(self.title_row(icon, title))
            .when(self.record_remaining, |this| {
                this.child(self.paragraph("uninstall_record_remaining", tokens))
            })
            .when(self.restart_required, |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(tokens.muted_foreground)
                        .child(self.i18n.get("uninstall_restart")),
                )
            })
            .when_some(self.notice, |this, key| {
                this.child(
                    div()
                        .id("setup-uninstalled-notice")
                        .role(Role::Alert)
                        .text_sm()
                        .text_color(tokens.danger)
                        .child(self.i18n.get(key)),
                )
            })
            .when(self.cleanup_incomplete, |this| {
                this.child(
                    div()
                        .id("setup-cleanup-notice")
                        .role(Role::Alert)
                        .text_sm()
                        .line_height(relative(1.5))
                        .text_color(tokens.danger)
                        .child(self.i18n.get("uninstall_cleanup_incomplete")),
                )
            })
            .into_any_element()
    }

    fn render_footer(
        &self,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Only progress/status text stays on the leading edge. Every action, including the way
        // out, is grouped at the trailing edge in reading order.
        let mut leading: Vec<AnyElement> = Vec::new();
        let mut actions: Vec<AnyElement> = Vec::new();

        match self.screen {
            Screen::License => {
                actions.push(self.cancel_button(tokens, cx));
                actions.push(
                    Button::new("setup-continue")
                        .primary()
                        .label(self.i18n.get("action_continue"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.continue_from_license(window, cx);
                        }))
                        .into_any_element(),
                );
            }
            Screen::Location => {
                actions.push(
                    Button::new("setup-back")
                        .ghost()
                        .label(self.i18n.get("action_back"))
                        .on_click(cx.listener(|this, _, _, cx| this.back(cx)))
                        .into_any_element(),
                );
                actions.push(self.cancel_button(tokens, cx));
                let label = if self.installed.is_some() {
                    "action_update"
                } else {
                    "action_install"
                };
                actions.push(
                    Button::new("setup-install")
                        .primary()
                        .label(self.i18n.get(label))
                        .on_click(cx.listener(|this, _, window, cx| this.start_install(window, cx)))
                        .into_any_element(),
                );
            }
            Screen::Installing | Screen::Uninstalling => {
                let key = if self.screen == Screen::Uninstalling {
                    "progress_do_not_close_uninstall"
                } else {
                    "progress_do_not_close"
                };
                leading.push(
                    div()
                        .text_sm()
                        .text_color(tokens.muted_foreground)
                        .child(self.i18n.get(key))
                        .into_any_element(),
                );
            }
            Screen::Finished | Screen::Maintenance => {
                if self.log.is_some() {
                    actions.push(self.log_button(cx));
                }
                actions.push(
                    Button::new("setup-done")
                        .ghost()
                        .label(self.i18n.get("action_close"))
                        .on_click(cx.listener(|this, _, window, cx| this.close_window(window, cx)))
                        .into_any_element(),
                );
                if !self.restart_required {
                    actions.push(
                        Button::new("setup-launch-app")
                            .primary()
                            .label(self.i18n.get("action_open_app"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_installed(window, cx);
                            }))
                            .into_any_element(),
                    );
                }
            }
            Screen::Failed => {
                if self.log.is_some() {
                    actions.push(self.log_button(cx));
                }
                let uninstall = self.is_uninstall() || self.uninstall_attempted;
                let uninstall_invalid = uninstall
                    && self
                        .error
                        .as_ref()
                        .is_some_and(|error| error.key == "error_msi_invalid");
                if uninstall_invalid {
                    // A damaged saved package cannot be retried into working, so the page's one
                    // recovery is the file cleanup. It keeps the log beside it and leaves the way
                    // out to the title bar and Escape.
                    actions.push(self.danger_action(
                        DangerAction {
                            id: "setup-force-cleanup",
                            label_key: "action_remove_remaining",
                            focus: &self.force_cleanup_focus,
                            on_activate: SetupApp::start_force_cleanup,
                        },
                        tokens,
                        window,
                        cx,
                    ));
                } else {
                    actions.push(
                        Button::new("setup-failed-close")
                            .ghost()
                            .label(self.i18n.get("action_close"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close_window(window, cx);
                            }))
                            .into_any_element(),
                    );
                    // A damaged payload or package, a missing product and an unsupported package
                    // cannot be retried from this window. A failed forced cleanup cannot either:
                    // it may have stopped because a newer install appeared, and retrying would
                    // remove that install. Transient MSI failures can be retried.
                    if !self.forced_cleanup
                        && failure_offers_retry(
                            self.error
                                .as_ref()
                                .map(|error| error.key)
                                .unwrap_or_default(),
                        )
                    {
                        actions.push(
                            Button::new("setup-retry")
                                .primary()
                                .label(self.i18n.get("action_retry"))
                                .on_click(cx.listener(|this, _, window, cx| this.retry(window, cx)))
                                .into_any_element(),
                        );
                    }
                }
            }
            Screen::Uninstall => {
                actions.push(self.cancel_button(tokens, cx));
                actions.push(self.uninstall_button(tokens, window, cx));
            }
            Screen::Uninstalled => {
                if self.log.is_some() {
                    actions.push(self.log_button(cx));
                }
                actions.push(
                    Button::new("setup-uninstalled-done")
                        .primary()
                        .label(self.i18n.get("action_finish"))
                        .on_click(cx.listener(|this, _, window, cx| this.close_window(window, cx)))
                        .into_any_element(),
                );
            }
        }

        h_flex()
            .id("setup-footer")
            .h(FOOTER_HEIGHT)
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(tokens.border)
            .px(PAGE_INSET)
            .gap(GROUP_GAP)
            .child(h_flex().gap(GROUP_GAP).items_center().children(leading))
            .child(h_flex().gap(GROUP_GAP).items_center().children(actions))
    }

    fn cancel_button(&self, _tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        Button::new("setup-cancel")
            .ghost()
            .label(self.i18n.get("action_cancel"))
            .on_click(cx.listener(|this, _, window, cx| this.close_window(window, cx)))
            .into_any_element()
    }

    fn log_button(&self, cx: &mut Context<Self>) -> AnyElement {
        Button::new("setup-log")
            .ghost()
            .label(self.i18n.get("action_view_log"))
            .on_click(cx.listener(|this, _, _, cx| this.open_log(cx)))
            .into_any_element()
    }

    /// The destructive action owns its focus treatment: the framework draws the filled-button
    /// focus line at 60% of the label ink inside the fill, which measures 2.51:1 on the light
    /// danger fill. The reserved 1 px foreground frame sits on the component edge, matching the
    /// product's other focus frames without a gap.
    fn danger_action(
        &self,
        action: DangerAction<'_>,
        tokens: &Tokens,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let DangerAction {
            id,
            label_key,
            focus,
            on_activate,
        } = action;
        let label = self.i18n.get(label_key);
        h_flex()
            .id(id)
            .role(Role::Button)
            .aria_label(label.clone())
            .track_focus(focus)
            .h(px(28.0))
            .px(rems(0.625))
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .whitespace_nowrap()
            .rounded(RADIUS)
            .border_1()
            .border_color(
                if cairn_ui::focus::keyboard_navigation(cx) && focus.is_focused(window) {
                    tokens.foreground
                } else {
                    tokens.foreground.opacity(0.0)
                },
            )
            .bg(tokens.danger_fill)
            .text_color(tokens.danger_label)
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .hover(|style| style.bg(tokens.danger_hover))
            .active(|style| style.bg(tokens.danger_active))
            .on_click(cx.listener(move |this, _, _, cx| on_activate(this, cx)))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    on_activate(this, cx);
                }
            }))
            .child(label)
            .into_any_element()
    }

    fn uninstall_button(
        &self,
        tokens: &Tokens,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.danger_action(
            DangerAction {
                id: "setup-uninstall-now",
                label_key: "action_uninstall",
                focus: &self.uninstall_focus,
                on_activate: SetupApp::start_uninstall,
            },
            tokens,
            window,
            cx,
        )
    }

    fn title_row(&self, icon: AnyElement, key: &'static str) -> Div {
        h_flex()
            .items_center()
            .gap(GROUP_GAP)
            .child(div().relative().top(px(1.0)).child(icon))
            .child(self.heading(key))
    }

    fn license_checkbox(
        &self,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<SetupApp>,
        on_click: impl Fn(&mut SetupApp, bool, &mut Window, &mut Context<SetupApp>) + 'static,
    ) -> AnyElement {
        let checked = self.accepted;
        let label = self.i18n.get("license_accept");
        let description = (self.notice == Some("error_license_required"))
            .then(|| self.i18n.get("error_license_required"));
        let focus = &self.license_focus;
        let is_focused = cairn_ui::focus::keyboard_navigation(cx) && focus.is_focused(window);

        let on_click = std::rc::Rc::new(on_click);
        let on_click_mouse = on_click.clone();
        let on_click_key = on_click.clone();

        let indicator_size = px(14.0);
        // The frame sits on the indicator edge with no gap. A checked box is filled with the
        // foreground ink, so its frame uses the ring ink instead and stays visible against both the
        // fill and the page.
        let focus_frame = if checked {
            tokens.ring
        } else {
            tokens.foreground
        };

        h_flex()
            .id("setup-accept")
            .role(Role::CheckBox)
            .aria_label(label.clone())
            .when_some(description, |this, text| this.aria_description(text))
            .aria_toggled(if checked {
                gpui_kit::gpui::accesskit::Toggled::True
            } else {
                gpui_kit::gpui::accesskit::Toggled::False
            })
            .track_focus(focus)
            .min_h(px(24.0))
            .py(px(4.0))
            .items_center()
            .gap(GROUP_GAP)
            .group("checkbox")
            .on_click(cx.listener(move |this, _, window, cx| {
                on_click_mouse(this, !checked, window, cx);
            }))
            // The agreement flips on the key release, not on the key press. A held key sends one
            // press and one release, so the box cannot flip again and again while the key is
            // down, and the mark appears exactly once per press.
            .on_key_up(cx.listener(move |this, event: &KeyUpEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    on_click_key(this, !checked, window, cx);
                }
            }))
            .child(
                div()
                    .rounded(RADIUS)
                    .border_1()
                    // A reserved 1px frame sits on the indicator edge, not around a padded gap.
                    .border_color(focus_frame.opacity(if is_focused { 1.0 } else { 0.0 }))
                    .child(
                        div()
                            .size(indicator_size)
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(RADIUS)
                            .border_1()
                            .when(!checked, |this| {
                                this.border_color(tokens.control_stroke)
                                    .bg(tokens.background)
                                    .group_hover("checkbox", |style| {
                                        style.border_color(tokens.foreground)
                                    })
                            })
                            .when(checked, |this| {
                                this.border_color(tokens.foreground)
                                    .bg(tokens.foreground)
                                    .group_hover("checkbox", |style| style.opacity(0.85))
                                    .child(
                                        Icon::default()
                                            .data(CHECK_SVG)
                                            .size(indicator_size)
                                            .text_color(tokens.background),
                                    )
                            }),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .line_height(relative(1.0))
                    .text_color(tokens.foreground)
                    .child(label),
            )
            .into_any_element()
    }

    /// The page title. It carries the heading role so a screen reader can name the page it is
    /// on, and its own id so the role reaches the accessibility tree.
    fn heading(&self, key: &'static str) -> Stateful<Div> {
        div()
            .id(key)
            .role(Role::Heading)
            .aria_level(1)
            .track_focus(&self.screen_focus)
            .text_size(rems(PAGE_TITLE_SIZE))
            .line_height(relative(SERIF_LINE_HEIGHT))
            .font(serif_font(FontWeight::SEMIBOLD))
            .child(self.i18n.get(key))
    }

    fn paragraph(&self, key: &str, tokens: &Tokens) -> Div {
        div()
            .text_sm()
            .line_height(relative(1.5))
            .text_color(tokens.muted_foreground)
            .child(self.i18n.get(key))
    }
}

#[cfg(test)]
mod tests {
    use super::{failure_offers_retry, path_accessible_label};

    #[test]
    fn the_path_label_carries_the_validation_message() {
        assert_eq!(
            path_accessible_label("Install folder", None),
            "Install folder"
        );
        assert_eq!(
            path_accessible_label("Install folder", Some("Enter a full local path.")),
            "Install folder: Enter a full local path."
        );
    }

    #[test]
    fn the_failure_page_only_retries_transient_failures() {
        for key in [
            "error_not_installed",
            "error_no_payload",
            "error_msi_platform",
            "error_msi_invalid",
            "error_msi_cleanup_failed",
        ] {
            assert!(!failure_offers_retry(key), "{key} must not offer a retry");
        }
        for key in [
            "error_msi_busy",
            "error_msi_cancelled",
            "error_msi_fatal",
            "error_msi_unknown",
        ] {
            assert!(failure_offers_retry(key), "{key} must offer a retry");
        }
    }
}
