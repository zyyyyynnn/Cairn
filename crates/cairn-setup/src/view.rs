// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The installer window. One column, one decision per page, a fixed footer, and the same chrome
//! and grayscale palette as the product it installs.

use cairn_ui::brand::{
    BODY_LINE_HEIGHT, BRAND_TITLE_SIZE, RADIUS, SERIF_LINE_HEIGHT, SMALL_TEXT_SIZE, UI_ICON_SIZE,
    app_icon, cjk_heading_font, has_cjk, heading_text, serif_font,
};
use gpui_kit::base::{Disableable as _, Progress as BaseProgress};
use gpui_kit::component::animation::cubic_bezier;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::gpui::prelude::FluentBuilder as _;
use gpui_kit::gpui::*;
use gpui_kit::gpui::{Animation, AnimationExt};

use crate::app::{SetupApp, visible_progress};
use crate::model::{InstalledProduct, Screen};
use crate::motion::PagePhase;

/// One physical platform surface: the caption row is 34 px at every zoom, like the product shell.
const TITLE_BAR_HEIGHT: Pixels = px(34.0);
/// Chrome horizontal inset (12 px), matching the product shell's title bar and footer margins.
const PAGE_INSET: Pixels = px(12.0);
/// Single vertical spacing token (16 px) for the main body area between title, description,
/// and inputs.
const MAIN_GAP: Pixels = px(16.0);
/// Inline tight group spacing (8 px) for related controls on the same row.
const GROUP_GAP: Pixels = px(8.0);
const PROGRESS_TRACK_HEIGHT: Pixels = px(10.0);
const PROGRESS_TRACK_RADIUS: Pixels = px(2.0);
const PROGRESS_TRACK_BORDER: Pixels = px(0.5);
/// Width of the indeterminate segment, as a fraction of the track.
const PROGRESS_INDETERMINATE_WIDTH: f32 = 0.28;
/// Action bar height: 40 px, leaving 6 px around the 28 px medium buttons.
const FOOTER_HEIGHT: Pixels = px(40.0);
/// The title-bar app mark token, separate from the 16 px wordmark.
const TITLE_BAR_ICON: Pixels = cairn_ui::brand::APP_ICON_SIZE;

const PAGE_ICON: Pixels = px(18.0);
const LOG_ACTION_MARKER: &str = "\u{E000}";

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
            | "error_staging"
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
    chrome: Hsla,
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
    on_activate: fn(&mut SetupApp, &mut Context<'_, SetupApp>),
}

impl Tokens {
    fn new(cx: &App) -> Self {
        let theme = cx.theme();
        Self {
            background: theme.background,
            chrome: theme.title_bar,
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

/// Returns the normalized leading offset of the indeterminate progress segment.
/// Under reduced motion (`phase = 0.0`), the segment stays centred in the track rather than
/// starting clipped outside the leading edge at `-PROGRESS_INDETERMINATE_WIDTH`.
fn indeterminate_left(phase: f32, reduce_motion: bool) -> f32 {
    if reduce_motion {
        (1.0 - PROGRESS_INDETERMINATE_WIDTH) / 2.0
    } else {
        phase * (1.0 + PROGRESS_INDETERMINATE_WIDTH) - PROGRESS_INDETERMINATE_WIDTH
    }
}

fn progress_fraction(progress: f32) -> f32 {
    (progress / 100.0).clamp(0.0, 1.0)
}

fn progress_segment(tokens: &Tokens, phase: f32, reduce_motion: bool) -> AnyElement {
    let left = indeterminate_left(phase, reduce_motion);
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left(relative(left))
        .w(relative(PROGRESS_INDETERMINATE_WIDTH))
        .rounded(PROGRESS_TRACK_RADIUS)
        .bg(tokens.foreground)
        .into_any_element()
}

fn progress_fill(tokens: &Tokens, progress: f32) -> AnyElement {
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left_0()
        .w(relative(progress_fraction(progress)))
        .rounded(PROGRESS_TRACK_RADIUS)
        .bg(tokens.foreground)
        .into_any_element()
}

/// The track carries determinate progress when MSI provides it and a travelling segment while
/// the total is unknown. Completion crossfades either state into a solid full track.
fn progress_bar(
    tokens: &Tokens,
    progress: Option<f32>,
    phase: f32,
    reduce_motion: bool,
    completion: Option<f32>,
) -> AnyElement {
    let track = div()
        .relative()
        .w_full()
        .h(PROGRESS_TRACK_HEIGHT)
        .rounded(PROGRESS_TRACK_RADIUS)
        .border(PROGRESS_TRACK_BORDER)
        .border_color(tokens.border)
        .bg(tokens.muted)
        .overflow_hidden();
    let running = match progress {
        Some(value) => progress_fill(tokens, value),
        None => progress_segment(tokens, phase, reduce_motion),
    };
    if let Some(opacity) = completion {
        track
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .opacity(1.0 - opacity)
                    .child(running),
            )
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .opacity(opacity)
                    .child(
                        div()
                            .w_full()
                            .h_full()
                            .rounded(PROGRESS_TRACK_RADIUS)
                            .bg(tokens.foreground),
                    ),
            )
            .into_any_element()
    } else {
        track.child(running).into_any_element()
    }
}

fn progress_element(
    tokens: &Tokens,
    id: &'static str,
    progress: Option<f32>,
    reduce_motion: bool,
    completion: Option<f32>,
) -> AnyElement {
    let tokens = *tokens;
    if completion.is_some() || progress.is_some() || reduce_motion {
        return div()
            .w_full()
            .child(progress_bar(
                &tokens,
                progress,
                0.0,
                reduce_motion,
                completion,
            ))
            .into_any_element();
    }
    div()
        .w_full()
        .with_animation(
            id,
            Animation::new(crate::motion::PROGRESS_SWEEP)
                .with_easing(cubic_bezier(0.4, 0.0, 0.6, 1.0))
                .repeat(),
            move |track, phase| {
                track.child(progress_bar(&tokens, progress, phase, reduce_motion, None))
            },
        )
        .into_any_element()
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
    disabled: bool,
}

impl SetupApp {
    fn caption_button(
        &self,
        window: &mut Window,
        button: CaptionButton,
        tokens: &Tokens,
        cx: &mut Context<'_, SetupApp>,
    ) -> AnyElement {
        let CaptionButton {
            id,
            label,
            icon,
            action,
            disabled,
        } = button;
        let is_close = matches!(action, CaptionAction::Close);
        // The caption buttons are the window's only minimize and close, so they are Tab stops like
        // any other control: a handle passed to `track_focus` carries its own tab-stop flag.
        let focus = window
            .use_keyed_state((id, disabled as usize), cx, move |_, cx| {
                cx.focus_handle().tab_stop(!disabled)
            })
            .read(cx)
            .clone();

        div()
            .id(id)
            .role(Role::Button)
            .aria_label(label)
            .track_focus(&focus)
            .flex()
            .w(cairn_ui::focus::CAPTION_WIDTH)
            .h_full()
            .items_center()
            .justify_center()
            .text_color(if disabled {
                tokens.muted_foreground
            } else {
                tokens.foreground
            })
            .when(is_close && !disabled, |this| {
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
            .child(cairn_ui::focus::caption_glyph(
                icon,
                UI_ICON_SIZE,
                &focus,
                tokens.foreground,
                cairn_ui::focus::keyboard_navigation(cx),
                window,
                cx,
            ))
            .into_any_element()
    }
}

impl Render for SetupApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let tokens = Tokens::new(cx);
        let reduce_motion = cx.reduce_motion();
        if self.motion.is_transitioning() {
            if self.focused_screen.take().is_some() {
                window.focus(&self.root_focus, cx);
            }
        } else if self.focused_screen != Some(self.screen) {
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
                    this.on_escape(window, cx);
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
            .child(self.render_body(&tokens, reduce_motion, window, cx))
            .child(self.render_footer(&tokens, window, cx))
    }
}

impl SetupApp {
    fn render_titlebar(
        &self,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let subtitle = if self.is_uninstall() {
            "uninstall_subtitle"
        } else {
            "setup_subtitle"
        };
        let subtitle_text = self.i18n.get(subtitle);
        let title_text = self.i18n.get("app_title");
        let wordmark_font = serif_font(FontWeight::SEMIBOLD);
        let descriptor_font = if has_cjk(&subtitle_text) {
            cjk_heading_font()
        } else {
            serif_font(FontWeight::NORMAL)
        };
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
            rem * SMALL_TEXT_SIZE,
            tokens.muted_foreground,
        );
        let descriptor_top = wordmark_baseline - descriptor_baseline;
        let wordmark = div()
            .text_size(rems(BRAND_TITLE_SIZE))
            .line_height(wordmark_line)
            .font(wordmark_font)
            .child(title_text);
        let descriptor = div()
            .text_size(rems(SMALL_TEXT_SIZE))
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
            .bg(tokens.chrome)
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
                    disabled: false,
                },
                tokens,
                cx,
            ))
            .child(self.caption_button(
                window,
                CaptionButton {
                    id: "setup-close",
                    label: self.i18n.get("window_close"),
                    icon: IconName::WindowClose,
                    action: CaptionAction::Close,
                    disabled: self.is_busy(),
                },
                tokens,
                cx,
            ))
    }

    fn render_body(
        &self,
        tokens: &Tokens,
        reduce_motion: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let page = match self.screen {
            Screen::Location => self.render_location(tokens, window, cx),
            Screen::Installing => self.render_installing(tokens, reduce_motion, window, cx),
            Screen::Finished => self.render_finished(tokens),
            Screen::Failed => self.render_failed(tokens, cx),
            Screen::Maintenance => self.render_maintenance(tokens),
            Screen::Uninstall => self.render_uninstall_confirm(tokens, cx),
            Screen::Uninstalling => self.render_uninstalling(tokens, reduce_motion, window, cx),
            Screen::Uninstalled => self.render_uninstalled(tokens, cx),
        };
        let page_opacity = match self.motion.page_phase() {
            PagePhase::Settled => 1.0,
            PagePhase::Exiting => {
                1.0 - crate::motion::sample_progress(
                    "setup-page-exit",
                    self.motion.transition_epoch(),
                    crate::motion::fade_exit(crate::motion::PAGE_EXIT),
                    window,
                    cx,
                )
            }
            PagePhase::Entering => crate::motion::sample_progress(
                "setup-page-enter",
                self.motion.transition_epoch(),
                crate::motion::fade_enter(crate::motion::PAGE_ENTER),
                window,
                cx,
            ),
        };
        div()
            .id("setup-body")
            .when(self.motion.is_settled(), |this| this.role(Role::Main))
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .w_full()
                    .px(PAGE_INSET)
                    .py(MAIN_GAP)
                    .opacity(page_opacity)
                    .child(page),
            )
    }

    fn render_location(
        &self,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let Some(path_input) = &self.path_input else {
            return div().into_any_element();
        };
        let path_error = self
            .path_error
            .map(|problem| self.i18n.get(problem.message_key()));
        // The page heading already names the task; the path field reuses it as its accessible
        // name instead of repeating a visible "Install folder" label under the heading.
        let accessible_label =
            path_accessible_label(&self.i18n.get("location_title"), path_error.as_deref());
        let path_focus = path_input.read(cx).focus_handle(cx);
        let path_focused = path_focus.is_focused(window);
        let notice_opacity = crate::motion::sample_fade(
            "setup-path-notice",
            self.motion.path_notice_epoch(),
            crate::motion::NOTICE_ENTER,
            window,
            cx,
        );
        v_flex()
            .gap(MAIN_GAP)
            .child(self.heading("location_title"))
            // The field and its message use the same compact gap as the progress label and bar.
            .child(
                v_flex()
                    .gap(GROUP_GAP)
                    .child(
                        h_flex()
                            .gap(GROUP_GAP)
                            .items_center()
                            .child(
                                div().flex_1().min_w_0().child(
                                    Input::new(path_input)
                                        .disabled(!self.motion.is_settled())
                                        .aria_label(accessible_label)
                                        // The field carries its own resting, focused and error
                                        // strokes, so the component's focus border would paint a
                                        // second frame on top of them.
                                        .focus_bordered(false)
                                        .border_color(if self.path_error.is_some() {
                                            tokens.danger
                                        } else if path_focused {
                                            tokens.ring
                                        } else {
                                            tokens.control_stroke
                                        })
                                        .w_full(),
                                ),
                            )
                            .child(
                                Button::new("setup-browse")
                                    .disabled(!self.motion.is_settled())
                                    .font_weight(FontWeight::MEDIUM)
                                    .line_height(relative(BODY_LINE_HEIGHT))
                                    .label(self.i18n.get("action_browse"))
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
                                .line_height(relative(BODY_LINE_HEIGHT))
                                .text_color(tokens.danger)
                                .opacity(notice_opacity)
                                .child(message),
                        )
                    })
                    .when(self.path_error.is_none(), |this| {
                        this.child(
                            div()
                                .text_sm()
                                .line_height(relative(BODY_LINE_HEIGHT))
                                .text_color(tokens.muted_foreground)
                                .opacity(notice_opacity)
                                .child(self.i18n.get("install_summary")),
                        )
                    }),
            )
            .into_any_element()
    }

    /// The progress page's shared body: current stage, percentage, and the progress track.
    /// Setup and uninstall use the same block so their two long-running pages cannot drift.
    fn progress_block(
        &self,
        tokens: &Tokens,
        id: &'static str,
        reduce_motion: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let completing = self.is_completing();
        let progress = if completing {
            100.0
        } else {
            visible_progress(self.progress)
        };
        let completion = completing.then(|| {
            crate::motion::sample_fade(
                "setup-progress-complete",
                self.motion.completion_epoch(),
                crate::motion::COMPLETION_FADE,
                window,
                cx,
            )
        });
        let line_height = (window.rem_size() * SMALL_TEXT_SIZE * BODY_LINE_HEIGHT).ceil();
        let percent_pattern = self.i18n.format(
            "progress_percent",
            &[("percent", crate::motion::PERCENT_DIGITS_MARKER)],
        );
        v_flex()
            .gap(GROUP_GAP)
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap(GROUP_GAP)
                    .child(crate::motion::render_stage_caption(
                        self.motion.caption(),
                        tokens.muted_foreground,
                        line_height,
                        window,
                        cx,
                    ))
                    .children(crate::motion::render_percent_reel(
                        self.motion.percent(),
                        &percent_pattern,
                        tokens.muted_foreground,
                        line_height,
                        window,
                        cx,
                    )),
            )
            .child(
                BaseProgress::new(id)
                    .value(progress)
                    .indeterminate(self.progress.is_none() && !completing)
                    .accessibility_label(self.motion.caption().current().to_string())
                    .w_full()
                    .child(progress_element(
                        tokens,
                        id,
                        self.progress,
                        reduce_motion,
                        completion,
                    )),
            )
            .into_any_element()
    }

    fn render_installing(
        &self,
        tokens: &Tokens,
        reduce_motion: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        v_flex()
            .gap(MAIN_GAP)
            .child(self.heading("progress_title"))
            .child(self.progress_block(tokens, "setup-progress", reduce_motion, window, cx))
            .into_any_element()
    }

    fn render_finished(&self, tokens: &Tokens) -> AnyElement {
        let mut details = vec![self.paragraph("finish_body", tokens).into_any_element()];
        if self.restart_required {
            details.push(
                div()
                    .text_sm()
                    .text_color(tokens.muted_foreground)
                    .child(self.i18n.get("finish_restart"))
                    .into_any_element(),
            );
        }
        self.render_installed_page(tokens, details)
    }

    fn render_maintenance(&self, tokens: &Tokens) -> AnyElement {
        let version = self
            .installed
            .as_ref()
            .map(|product| product.version.as_str())
            .unwrap_or_default();
        let version_line = if self
            .installed
            .as_ref()
            .is_some_and(InstalledProduct::is_newer_than_package)
        {
            self.i18n
                .format("maintenance_newer", &[("version", version)])
        } else {
            self.i18n
                .format("maintenance_installed", &[("version", version)])
        };
        let detail = div()
            .text_sm()
            .text_color(tokens.muted_foreground)
            .child(version_line)
            .into_any_element();
        self.render_installed_page(tokens, vec![detail])
    }

    fn render_installed_page(&self, tokens: &Tokens, details: Vec<AnyElement>) -> AnyElement {
        let icon = Icon::new(IconName::CircleCheck)
            .size(PAGE_ICON)
            .text_color(tokens.foreground)
            .into_any_element();
        let mut page = v_flex()
            .gap(MAIN_GAP)
            .child(self.title_row(icon, "finish_title"))
            .children(details);
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

    fn render_failed(&self, tokens: &Tokens, cx: &mut Context<'_, Self>) -> AnyElement {
        let uninstall = self.is_uninstall() || self.uninstall_attempted;
        let missing_install = uninstall && self.error == Some("error_not_installed");
        let title = if missing_install {
            "uninstall_missing_title"
        } else if uninstall {
            "error_uninstall_title"
        } else {
            "error_title"
        };
        let detail_key = if missing_install {
            Some("uninstall_missing_body")
        } else {
            self.error.map(|key| {
                if uninstall {
                    crate::model::uninstall_error_key(key)
                } else {
                    key
                }
            })
        };

        let icon = Icon::new(IconName::CircleAlert)
            .size(PAGE_ICON)
            .text_color(tokens.danger)
            .into_any_element();
        let mut page = v_flex().gap(MAIN_GAP).child(self.title_row(icon, title));

        if let Some(key) = detail_key {
            let detail = self.inline_log_detail(key, tokens.muted_foreground, cx);
            page = page.child(
                div()
                    .id("setup-failed-detail")
                    .role(Role::Alert)
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

    fn render_uninstall_confirm(&self, tokens: &Tokens, _cx: &mut Context<'_, Self>) -> AnyElement {
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

    fn render_uninstalling(
        &self,
        tokens: &Tokens,
        reduce_motion: bool,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        v_flex()
            .gap(MAIN_GAP)
            .child(self.heading("uninstall_progress_title"))
            .child(self.progress_block(
                tokens,
                "setup-uninstall-progress",
                reduce_motion,
                window,
                cx,
            ))
            .into_any_element()
    }

    fn render_uninstalled(&self, tokens: &Tokens, cx: &mut Context<'_, Self>) -> AnyElement {
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
            .when(!self.record_remaining, |this| {
                this.child(self.paragraph("uninstall_done_body", tokens))
            })
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
                let detail =
                    self.inline_log_detail("uninstall_cleanup_incomplete", tokens.danger, cx);
                this.child(
                    div()
                        .id("setup-cleanup-notice")
                        .role(Role::Alert)
                        .child(detail),
                )
            })
            .into_any_element()
    }

    fn render_footer(
        &self,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        // Only progress/status text stays on the leading edge. Footer actions sit at the trailing
        // edge. The uninstall confirmation also carries a secondary Cancel, because its primary
        // action is destructive; other pages leave closing to the title-bar control.
        let mut leading: Vec<AnyElement> = Vec::new();
        let mut actions: Vec<AnyElement> = Vec::new();

        match self.screen {
            Screen::Location => {
                actions.push(
                    Button::new("setup-install")
                        .disabled(!self.motion.is_settled())
                        .font_weight(FontWeight::MEDIUM)
                        .primary()
                        .label(self.i18n.get("action_install"))
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
                        .disabled(!self.motion.is_settled())
                        .font_weight(FontWeight::MEDIUM)
                        .ghost()
                        .label(self.i18n.get("action_close"))
                        .on_click(cx.listener(|this, _, window, cx| this.close_window(window, cx)))
                        .into_any_element(),
                );
                if !self.restart_required {
                    actions.push(
                        Button::new("setup-launch-app")
                            .disabled(!self.motion.is_settled())
                            .font_weight(FontWeight::MEDIUM)
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
                let uninstall = self.is_uninstall() || self.uninstall_attempted;
                let uninstall_invalid = uninstall && self.error == Some("error_msi_invalid");
                if uninstall_invalid {
                    // A damaged saved package cannot be retried into working, so the page's one
                    // recovery is the file cleanup. Closing stays in the title bar and Escape.
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
                            .disabled(!self.motion.is_settled())
                            .font_weight(FontWeight::MEDIUM)
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
                    if !self.forced_cleanup && failure_offers_retry(self.error.unwrap_or_default())
                    {
                        actions.push(
                            Button::new("setup-retry")
                                .disabled(!self.motion.is_settled())
                                .font_weight(FontWeight::MEDIUM)
                                .primary()
                                .label(self.i18n.get("action_retry"))
                                .on_click(cx.listener(|this, _, window, cx| this.retry(window, cx)))
                                .into_any_element(),
                        );
                    }
                }
            }
            Screen::Uninstall => {
                actions.push(self.cancel_button(cx));
                actions.push(self.uninstall_button(tokens, window, cx));
            }
            Screen::Uninstalled => {
                actions.push(
                    Button::new("setup-uninstalled-done")
                        .disabled(!self.motion.is_settled())
                        .font_weight(FontWeight::MEDIUM)
                        .primary()
                        .label(self.i18n.get("action_finish"))
                        .on_click(cx.listener(|this, _, window, cx| this.close_window(window, cx)))
                        .into_any_element(),
                );
            }
        }

        let footer_opacity = match self.motion.page_phase() {
            PagePhase::Settled => 1.0,
            PagePhase::Exiting => {
                1.0 - crate::motion::sample_progress(
                    "setup-footer-exit",
                    self.motion.transition_epoch(),
                    crate::motion::fade_exit(crate::motion::FOOTER_EXIT),
                    window,
                    cx,
                )
            }
            PagePhase::Entering => crate::motion::sample_progress(
                "setup-footer-enter",
                self.motion.transition_epoch(),
                crate::motion::fade_enter(crate::motion::FOOTER_ENTER)
                    .delay(crate::motion::FOOTER_ENTER_DELAY),
                window,
                cx,
            ),
        };
        h_flex()
            .id("setup-footer")
            .h(FOOTER_HEIGHT)
            .flex_shrink_0()
            .items_center()
            .justify_between()
            .border_t_1()
            .border_color(tokens.border)
            .bg(tokens.chrome)
            .px(PAGE_INSET)
            .gap(GROUP_GAP)
            .child(
                h_flex()
                    .gap(GROUP_GAP)
                    .items_center()
                    .opacity(footer_opacity)
                    .children(leading),
            )
            .child(
                h_flex()
                    .gap(GROUP_GAP)
                    .items_center()
                    .opacity(footer_opacity)
                    .children(actions),
            )
    }

    fn cancel_button(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        Button::new("setup-cancel")
            .disabled(!self.motion.is_settled())
            .font_weight(FontWeight::MEDIUM)
            .ghost()
            .label(self.i18n.get("action_cancel"))
            .on_click(cx.listener(|this, _, window, cx| this.close_window(window, cx)))
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
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let DangerAction {
            id,
            label_key,
            focus,
            on_activate,
        } = action;
        let label = self.i18n.get(label_key);
        if !self.motion.is_settled() {
            return h_flex()
                .id(id)
                .h(px(28.0))
                .px(rems(0.625))
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .whitespace_nowrap()
                .rounded(RADIUS)
                .bg(tokens.danger_fill)
                .text_color(tokens.danger_label)
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .opacity(0.55)
                .child(label)
                .into_any_element();
        }
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
        cx: &mut Context<'_, Self>,
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

    /// The page title. It carries the heading role so a screen reader can name the page it is
    /// on, and its own id so the role reaches the accessibility tree.
    fn heading(&self, key: &'static str) -> Stateful<Div> {
        div()
            .id(key)
            .role(Role::Heading)
            .aria_level(1)
            .aria_label(self.i18n.get(key))
            .track_focus(&self.screen_focus)
            .text_size(rems(BRAND_TITLE_SIZE))
            .line_height(relative(SERIF_LINE_HEIGHT))
            .font(serif_font(FontWeight::SEMIBOLD))
            .child(heading_text(self.i18n.get(key), FontWeight::SEMIBOLD))
    }

    fn log_button(&self, cx: &mut Context<'_, Self>) -> AnyElement {
        Button::new("setup-log")
            .disabled(!self.motion.is_settled())
            .font_weight(FontWeight::MEDIUM)
            .ghost()
            .label(self.i18n.get("action_view_log"))
            .on_click(cx.listener(|this, _, window, cx| this.open_log(window, cx)))
            .into_any_element()
    }

    fn inline_log_detail(
        &self,
        key: &'static str,
        color: Hsla,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let label = self.i18n.get("action_view_log");
        let has_log = self.log.is_some();
        let text = self.i18n.format(
            key,
            &[
                ("has_log", if has_log { "true" } else { "false" }),
                ("logAction", LOG_ACTION_MARKER),
            ],
        );
        let plain = |text: String| {
            div()
                .text_sm()
                .line_height(relative(BODY_LINE_HEIGHT))
                .text_color(color)
                .child(text)
                .into_any_element()
        };
        if !has_log {
            return plain(text);
        }
        let Some((before, after)) = text.split_once(LOG_ACTION_MARKER) else {
            return plain(text);
        };
        let before = before.to_string();
        let after = after.to_string();
        h_flex()
            .w_full()
            .flex_wrap()
            .items_center()
            .child(
                div()
                    .min_w_0()
                    .text_sm()
                    .line_height(relative(BODY_LINE_HEIGHT))
                    .text_color(color)
                    .child(before),
            )
            .child(
                Button::new("setup-log-inline")
                    .disabled(!self.motion.is_settled())
                    .small()
                    .link()
                    .line_height(relative(BODY_LINE_HEIGHT))
                    .label(label)
                    .on_click(cx.listener(|this, _, window, cx| this.open_log(window, cx))),
            )
            .child(
                div()
                    .min_w_0()
                    .text_sm()
                    .line_height(relative(BODY_LINE_HEIGHT))
                    .text_color(color)
                    .child(after),
            )
            .into_any_element()
    }

    fn paragraph(&self, key: &str, tokens: &Tokens) -> Div {
        div()
            .text_sm()
            .line_height(relative(BODY_LINE_HEIGHT))
            .text_color(tokens.muted_foreground)
            .child(self.i18n.get(key))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PROGRESS_INDETERMINATE_WIDTH, failure_offers_retry, indeterminate_left,
        path_accessible_label, progress_fraction,
    };

    #[test]
    fn determinate_progress_clamps_to_the_track() {
        assert_eq!(progress_fraction(0.0), 0.0);
        assert!((progress_fraction(42.0) - 0.42).abs() < f32::EPSILON);
        assert_eq!(progress_fraction(100.0), 1.0);
        assert_eq!(progress_fraction(140.0), 1.0);
    }

    #[test]
    fn indeterminate_progress_stays_visible_and_opaque_under_reduced_motion() {
        let left = indeterminate_left(0.0, true);
        assert!(left >= 0.0, "segment must stay inside leading edge: {left}");
        assert!(
            left + PROGRESS_INDETERMINATE_WIDTH <= 1.0,
            "segment must stay inside trailing edge: {left}"
        );
        assert!(indeterminate_left(0.0, false) < 0.0);
    }

    #[test]
    fn the_path_label_carries_the_validation_message() {
        assert_eq!(
            path_accessible_label("Install location", None),
            "Install location"
        );
        assert_eq!(
            path_accessible_label("Install location", Some("Enter a full local path.")),
            "Install location: Enter a full local path."
        );
    }

    #[test]
    fn the_failure_page_only_retries_transient_failures() {
        for key in [
            "error_not_installed",
            "error_no_payload",
            "error_staging",
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
