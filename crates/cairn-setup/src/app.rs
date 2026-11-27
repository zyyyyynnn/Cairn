// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The installer's state machine. It owns the visible flow; the MSI owns the transaction.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::Receiver;

use cairn_ui::brand::{BODY_LINE_HEIGHT, RADIUS, heading_text};
use cairn_ui::theme::{dialog_surface, overlay_scrollbar};
use cairn_ui::tooltip;
use gpui_kit::Focusable;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::gpui::{
    AppContext as _, Context, Entity, FocusHandle, FontWeight, InteractiveElement as _, KeyBinding,
    MouseButton, ParentElement as _, Pixels, Point, ScrollHandle, StatefulInteractiveElement as _,
    Styled as _, Subscription, WeakEntity, Window, div, px, relative,
};

use crate::i18n::I18n;
use crate::installed;
use crate::model::{
    InstalledProduct, Launch, PathProblem, Screen, Stage, default_install_dir, initial_screen,
    validate_install_path,
};
use crate::motion::{COMPLETION_FADE, COMPLETION_HOLD, PAGE_ENTER, PAGE_EXIT, SetupMotion};
use crate::msi::{self, MsiEvent};
use crate::payload::PayloadError;
use crate::platform;
use crate::preview::Preview;
use crate::quiet;

/// The key context carried by the installer's root. Bindings scoped to it answer wherever focus
/// happens to be, and a future overlay can override the same key without editing this table.
pub const WINDOW_CONTEXT: &str = "CairnSetup";

// The window-level Escape gesture. It is declared as an Action so the keymap can bind it and
// the window can answer it wherever focus happens to be.
gpui_kit::gpui::actions!(
    cairn_setup,
    [
        /// Leaves a page that offers a Cancel action.
        CancelSetup,
    ]
);

fn error_key_from_payload(error: &PayloadError) -> &'static str {
    match error {
        PayloadError::Missing
        | PayloadError::TooLarge
        | PayloadError::AlreadyEmbedded
        | PayloadError::EmptyInput => "error_no_payload",
        PayloadError::CorruptFooter | PayloadError::Checksum | PayloadError::SetupHash => {
            "error_msi_invalid"
        }
        PayloadError::Io(_) => "error_msi_fatal",
    }
}

/// MSI progress resets at action boundaries. The visible bar must not jump backwards, so the
/// displayed percentage is monotonic across one transaction.
fn progress_percent(previous: Option<f32>, current: u32, total: u32) -> Option<f32> {
    if total == 0 {
        return None;
    }
    let next = (current as f32 / total as f32 * 100.0).clamp(0.0, 100.0);
    Some(previous.map_or(next, |value| value.max(next)))
}

/// The display value while a transaction is still running. MSI progress totals can grow after a
/// tick reaches the current total; only `Done` means the work is finished, so the bar never claims
/// completion before that event.
pub(crate) fn visible_progress(progress: Option<f32>) -> f32 {
    progress.unwrap_or(0.0).min(99.0)
}

/// A product whose MSI record is missing can be removed by the cleanup pass. A damaged cached
/// package is different: it fails before Windows Installer can remove the product, so the page
/// offers the explicit file cleanup instead of a retry.
fn uninstall_can_fall_back_to_cleanup(kind: msi::FailureKind) -> bool {
    kind == msi::FailureKind::NotInstalled
}

/// The disclosure on the uninstall result page. After a forced cleanup, Windows Installer may still
/// hold the product record it could not remove; the page says so rather than claiming a clean
/// removal. A failed enumeration cannot confirm the record is gone, so it discloses as well.
fn record_remaining_after_cleanup(forced: bool, codes: Result<Vec<String>, u32>) -> bool {
    forced && codes.map_or(true, |codes| !codes.is_empty())
}

/// A forced cleanup starts from the progress page when Windows Installer reports the record gone,
/// or from the failure page when the person chooses it. Any other page has no cleanup to start.
fn force_cleanup_allowed(screen: Screen) -> bool {
    matches!(screen, Screen::Uninstalling | Screen::Failed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingCompletion {
    token: u64,
    next: Screen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScreenTransition {
    token: u64,
    target: Screen,
}

fn progress_caption_for(i18n: &I18n, screen: Screen, stage: Stage) -> String {
    if screen == Screen::Uninstalling {
        i18n.get("uninstall_progress_working")
    } else {
        i18n.get(stage.message_key())
    }
}

pub struct SetupApp {
    pub(crate) i18n: I18n,
    pub(crate) launch: Launch,
    pub(crate) screen: Screen,
    pub(crate) motion: SetupMotion,
    pub(crate) dark: bool,
    pub(crate) path_input: Option<Entity<InputState>>,
    pub(crate) path_error: Option<PathProblem>,
    pub(crate) installing: Rc<Cell<bool>>,
    pub(crate) progress: Option<f32>,
    pub(crate) stage: Stage,
    completion: Option<PendingCompletion>,
    completion_counter: u64,
    screen_transition: Option<ScreenTransition>,
    transition_counter: u64,
    pub(crate) log: Option<PathBuf>,
    pub(crate) error: Option<&'static str>,
    pub(crate) notice: Option<&'static str>,
    pub(crate) cleanup_incomplete: bool,
    pub(crate) restart_required: bool,
    /// The forced cleanup could not remove the hidden Windows Installer product record.
    pub(crate) record_remaining: bool,
    pub(crate) installed: Option<InstalledProduct>,
    pub(crate) install_dir: Option<PathBuf>,
    pub(crate) uninstall_attempted: bool,
    /// The products the running removal was started for. A forced cleanup re-checks this set
    /// under the Windows Installer lock, so a product registered under a new code is refused.
    active_product_codes: Vec<String>,
    /// The running transaction is a registration-only cleanup after Windows Installer could not
    /// remove the product itself. It stays set after a failed cleanup so the failure page can
    /// withhold Retry: a retry could target a newer install that appeared meanwhile.
    pub(crate) forced_cleanup: bool,
    /// The window's own focus handle. It is not a Tab stop, so it adds no stop to the Tab order;
    /// it gives the window a focus target so the window's key context stays on the dispatch path
    /// even when no control holds focus. Without it the keymap only sees the window chrome's own
    /// context, and the window-level Escape gesture never matches.
    pub(crate) root_focus: FocusHandle,
    /// The page heading receives focus when the screen changes, so a keyboard or screen-reader
    /// user lands on the new task instead of the disappearing control that opened it.
    pub(crate) screen_focus: FocusHandle,
    /// The destructive uninstall action owns its focus ring because the framework's filled-button
    /// focus line is drawn at reduced opacity inside the danger fill.
    pub(crate) uninstall_focus: FocusHandle,
    /// The failure page's explicit "remove remaining files" action.
    pub(crate) force_cleanup_focus: FocusHandle,
    pub(crate) focused_screen: Option<Screen>,
    payload_dir: Option<PathBuf>,
    _activation: Subscription,
    _focus_lost: Subscription,
    _path_change: Option<Subscription>,
    _keyboard_modality: Subscription,
}

impl SetupApp {
    pub(crate) fn new(
        window: &mut Window,
        cx: &mut Context<'_, Self>,
        launch: Launch,
        i18n: I18n,
        preview: Option<Preview>,
        dark: bool,
    ) -> Self {
        let live_installed = installed::installed_product();
        // A preview must render the scene it names, independent of what this machine happens to
        // have installed. Falling back to the live product made a clean-install scene show the
        // update action and turned the matrix into a property of the capture machine.
        let installed = match preview.as_ref() {
            Some(preview) => preview.installed_product(),
            None => live_installed,
        };
        let legacy_burn = preview.is_none()
            && launch == Launch::Setup
            && !installed::legacy_burn_uninstallers().is_empty();
        let screen = preview.as_ref().map_or_else(
            || initial_screen(launch, installed.as_ref(), legacy_burn),
            |preview| preview.screen,
        );
        let launch = preview.as_ref().map_or(launch, |preview| preview.launch);

        let (path_input, path_change) = if launch == Launch::Setup {
            let initial_dir = installed
                .as_ref()
                .map(|product| product.install_dir.clone())
                .filter(|path| validate_install_path(&path.to_string_lossy()).is_ok())
                .unwrap_or_else(default_install_dir);
            let input = cx.new(|cx| {
                InputState::new(window, cx).default_value(initial_dir.to_string_lossy().to_string())
            });
            let subscription = cx.subscribe_in(
                &input,
                window,
                |this, _input, event: &InputEvent, _window, cx| {
                    if matches!(event, InputEvent::Change)
                        && this.path_error.is_some()
                        && let Some(input) = &this.path_input
                    {
                        let raw = input.read(cx).value().to_string();
                        this.set_path_error(validate_install_path(&raw).err());
                        cx.notify();
                    }
                },
            );
            (Some(input), Some(subscription))
        } else {
            (None, None)
        };

        let preview_theme = preview.as_ref().and_then(|preview| preview.dark);
        let installing = Rc::new(Cell::new(preview.as_ref().is_some_and(Preview::is_busy)));
        let close_flag = installing.clone();
        window.on_window_should_close(cx, move |_, _| !close_flag.get());

        // Escape is the keyboard equivalent of the Cancel button. It is bound on the window's
        // key context so it answers wherever focus is, and it is a no-op on the pages that are
        // running the MSI: closing the window mid-transaction would leave the machine
        // half-installed.
        cx.bind_keys([KeyBinding::new("escape", CancelSetup, Some(WINDOW_CONTEXT))]);

        // The window's own focus handle. The keymap reads a binding's context from the dispatch
        // path of the focused node, and a window with nothing focused resolves to the window
        // chrome's own node, which does not carry this window's context. Holding the window's
        // focus keeps the context on the path, so the Escape gesture answers even when no control
        // holds focus.
        let root_focus = cx.focus_handle();
        let screen_focus = cx.focus_handle();
        let uninstall_focus = cx.focus_handle().tab_stop(true);
        let force_cleanup_focus = cx.focus_handle().tab_stop(true);

        // Keep the window key context reachable: if a rendered region disappears and focus is
        // dropped, return it to the window's own handle rather than leaving the commands
        // unreachable.
        let focus_lost = cx.on_focus_lost(window, |this, window, cx| {
            this.root_focus.focus(window, cx);
        });
        // Tab is consumed by the focus walk before an element listener runs, so the keyboard
        // modality is recorded at the app level.
        let keyboard_modality = cairn_ui::focus::observe_keyboard(cx);
        window.focus(&root_focus, cx);

        let activation = cx.observe_window_activation(window, move |this, _window, cx| {
            if preview_theme.is_some() {
                return;
            }
            let dark = cairn_ui::locale::system_prefers_dark();
            if dark != this.dark {
                this.dark = dark;
                cairn_ui::theme::set_dark_mode(dark, cx);
                cx.notify();
            }
        });

        let error = preview
            .as_ref()
            .and_then(|preview| preview.error)
            .or_else(|| {
                (launch == Launch::Uninstall && installed.is_none())
                    .then_some("error_not_installed")
            });
        let install_dir = installed
            .as_ref()
            .map(|product| product.install_dir.clone());
        let progress = preview.as_ref().and_then(|preview| preview.progress);
        let stage = preview
            .as_ref()
            .map_or(Stage::Preparing, |preview| preview.stage);
        let initial_caption = progress_caption_for(&i18n, screen, stage);
        let motion = SetupMotion::new(
            initial_caption,
            progress.map(|value| visible_progress(Some(value))),
        );
        Self {
            i18n,
            launch,
            screen,
            motion,
            dark,
            path_input,
            path_error: None,
            installing,
            progress,
            stage,
            completion: None,
            completion_counter: 0,
            screen_transition: None,
            transition_counter: 0,
            log: preview.as_ref().and_then(|preview| preview.log.clone()),
            error,
            notice: None,
            cleanup_incomplete: false,
            restart_required: false,
            record_remaining: preview
                .as_ref()
                .is_some_and(|preview| preview.record_remaining),
            installed,
            install_dir,
            uninstall_attempted: preview
                .as_ref()
                .is_some_and(|preview| preview.uninstall_attempted),
            active_product_codes: Vec::new(),
            forced_cleanup: false,
            root_focus,
            screen_focus,
            uninstall_focus,
            force_cleanup_focus,
            focused_screen: None,
            payload_dir: None,
            _activation: activation,
            _focus_lost: focus_lost,
            _path_change: path_change,
            _keyboard_modality: keyboard_modality,
        }
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.installing.get()
            || self.completion.is_some()
            || self.screen_transition.is_some()
            || matches!(self.screen, Screen::Installing | Screen::Uninstalling)
    }

    pub(crate) fn is_transitioning(&self) -> bool {
        self.motion.is_transitioning()
    }

    pub(crate) fn is_completing(&self) -> bool {
        self.completion.is_some()
    }

    pub(crate) fn is_uninstall(&self) -> bool {
        self.launch == Launch::Uninstall
    }

    /// The one way out of the window. The installer already runs elevated and nothing is written
    /// to the machine until the MSI transaction starts, so cancelling and closing are the same
    /// gesture. A page that is running the MSI has no way out: closing the window mid-transaction
    /// would leave the machine half-installed.
    pub(crate) fn close_window(&mut self, window: &mut Window, _cx: &mut Context<'_, Self>) {
        if !self.is_busy() {
            window.remove_window();
        }
    }

    /// The window-level Escape gesture. It is the keyboard way out of a page; the title-bar X is
    /// the pointer way out. A page that is running the MSI refuses both, because closing the
    /// window mid-transaction would leave the machine half-installed.
    pub(crate) fn on_escape(&mut self, window: &mut Window, _cx: &mut Context<'_, Self>) {
        if !self.is_busy() && self.screen.can_cancel() {
            window.remove_window();
        }
    }

    fn set_screen_now(&mut self, next: Screen) {
        self.screen = next;
        if !matches!(next, Screen::Installing | Screen::Uninstalling) {
            self.completion = None;
        }
    }

    fn request_screen(&mut self, next: Screen, cx: &mut Context<'_, Self>) {
        if next == self.screen && self.screen_transition.is_none() {
            return;
        }
        if cx.reduce_motion() {
            self.screen_transition = None;
            self.set_screen_now(next);
            self.installing
                .set(matches!(next, Screen::Installing | Screen::Uninstalling));
            cx.notify();
            return;
        }
        if let Some(mut transition) = self.screen_transition {
            if transition.target != next {
                transition.target = next;
                self.screen_transition = Some(transition);
            }
            return;
        }

        self.transition_counter += 1;
        let token = self.transition_counter;
        self.installing.set(true);
        self.screen_transition = Some(ScreenTransition {
            token,
            target: next,
        });
        self.motion.begin_exit();
        cx.notify();
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(PAGE_EXIT).await;
            let _ = weak.update(cx, |this, cx| this.enter_screen_transition(token, cx));
            cx.background_executor().timer(PAGE_ENTER).await;
            let _ = weak.update(cx, |this, cx| this.finish_screen_transition(token, cx));
        })
        .detach();
    }

    fn enter_screen_transition(&mut self, token: u64, cx: &mut Context<'_, Self>) {
        let Some(transition) = self.screen_transition else {
            return;
        };
        if transition.token != token {
            return;
        }
        self.set_screen_now(transition.target);
        self.motion.begin_enter();
        cx.notify();
    }

    fn finish_screen_transition(&mut self, token: u64, cx: &mut Context<'_, Self>) {
        let Some(transition) = self.screen_transition else {
            return;
        };
        if transition.token != token {
            return;
        }
        let target = transition.target;
        self.screen_transition = None;
        self.motion.settle();
        if !matches!(self.screen, Screen::Installing | Screen::Uninstalling) {
            self.installing.set(false);
        }
        cx.notify();
        if target != self.screen {
            self.request_screen(target, cx);
        }
    }

    fn set_path_error(&mut self, next: Option<PathProblem>) {
        self.path_error = next;
        self.motion.set_path_error(next);
    }

    fn begin_progress_screen(&mut self, screen: Screen, stage: Stage, cx: &mut Context<'_, Self>) {
        self.completion = None;
        self.progress = None;
        self.stage = stage;
        self.motion
            .reset_progress(progress_caption_for(&self.i18n, screen, stage));
        self.request_screen(screen, cx);
    }

    fn begin_completion(&mut self, next: Screen, cx: &mut Context<'_, Self>) {
        self.stage = Stage::Finishing;
        self.motion.set_caption(if self.is_uninstall() {
            self.i18n.get("uninstall_progress_working")
        } else {
            self.i18n.get(Stage::Finishing.message_key())
        });
        self.motion.set_progress(100.0);
        self.motion.begin_completion();
        self.completion_counter += 1;
        let token = self.completion_counter;
        self.completion = Some(PendingCompletion { token, next });
        if cx.reduce_motion() {
            self.finish_completion(token, cx);
            return;
        }
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(COMPLETION_FADE).await;
            let _ = weak.update(cx, |this, cx| this.hold_completion(token, cx));
        })
        .detach();
        cx.notify();
    }

    fn hold_completion(&mut self, token: u64, cx: &mut Context<'_, Self>) {
        if self.completion.is_none_or(|pending| pending.token != token) {
            return;
        }
        let weak = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            cx.background_executor().timer(COMPLETION_HOLD).await;
            let _ = weak.update(cx, |this, cx| this.finish_completion(token, cx));
        })
        .detach();
    }

    fn finish_completion(&mut self, token: u64, cx: &mut Context<'_, Self>) {
        let Some(pending) = self.completion else {
            return;
        };
        if pending.token != token {
            return;
        }
        self.request_screen(pending.next, cx);
    }

    pub(crate) fn browse(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.is_busy() {
            return;
        }
        let Some(input) = self.path_input.clone() else {
            return;
        };
        let title = self.i18n.get("location_title");
        let Some(path) = platform::browse_for_folder(&title) else {
            return;
        };
        self.set_path_error(None);
        input.update(cx, |state, cx| {
            state.set_value(path.to_string_lossy().to_string(), window, cx);
        });
        cx.notify();
    }

    fn fail_staging(&mut self, error: std::io::Error, cx: &mut Context<'_, Self>) {
        tracing::warn!("installer staging root is unavailable: {error}");
        self.log = None;
        self.error = Some("error_staging");
        self.request_screen(Screen::Failed, cx);
        cx.notify();
    }

    pub(crate) fn start_install(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.launch != Launch::Setup || self.is_busy() {
            return;
        }
        let Some(input) = self.path_input.clone() else {
            return;
        };
        let raw = input.read(cx).value().to_string();
        let install_dir = match validate_install_path(&raw) {
            Ok(path) => path,
            Err(problem) => {
                self.set_path_error(Some(problem));
                self.request_screen(Screen::Location, cx);
                self.focused_screen = Some(Screen::Location);
                input.read(cx).focus_handle(cx).focus(window, cx);
                cx.notify();
                return;
            }
        };
        let (msi, payload_dir) = match quiet::msi_for_this_build(None) {
            Ok(value) => value,
            Err(error) => {
                self.error = Some(error_key_from_payload(&error));
                self.request_screen(Screen::Failed, cx);
                cx.notify();
                return;
            }
        };

        let log = match platform::install_log_path() {
            Ok(log) => log,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&payload_dir);
                self.fail_staging(error, cx);
                return;
            }
        };
        // Re-scan at the moment of install: the window may have been open across an external
        // uninstall, and a stale boolean must not leave an old Burn entry behind.
        let legacy = installed::legacy_burn_uninstallers();
        let request = msi::InstallRequest {
            msi,
            install_dir: install_dir.clone(),
            log: log.clone(),
            legacy,
        };
        let session = msi::start_install(request);

        self.install_dir = Some(install_dir);
        self.payload_dir = Some(payload_dir);
        self.log = Some(log);
        self.error = None;
        self.notice = None;
        self.set_path_error(None);
        self.restart_required = false;
        self.cleanup_incomplete = false;
        self.record_remaining = false;
        self.active_product_codes.clear();
        self.forced_cleanup = false;
        self.uninstall_attempted = false;
        self.installing.set(true);
        self.begin_progress_screen(Screen::Installing, Stage::Preparing, cx);
        self.pump(session.events, cx);
        cx.notify();
    }

    pub(crate) fn start_uninstall(&mut self, cx: &mut Context<'_, Self>) {
        if self.is_busy() {
            return;
        }
        self.active_product_codes.clear();
        self.forced_cleanup = false;
        self.record_remaining = false;
        // The visible screen may have been opened before another process upgraded or removed
        // Cairn; the removal decision uses the current registration, not the cached banner state.
        let product = installed::installed_product();
        let Some(product) = product else {
            self.error = Some("error_not_installed");
            self.request_screen(Screen::Failed, cx);
            cx.notify();
            return;
        };
        let product_codes = match installed::find_product_codes_checked() {
            Ok(codes) => codes,
            Err(error) => {
                match platform::uninstall_log_path() {
                    Ok(log) => {
                        if let Err(write_err) = std::fs::write(
                            &log,
                            format!("MSI product enumeration failed: {error}\n"),
                        ) {
                            tracing::warn!(
                                "could not write uninstall log {}: {write_err}",
                                log.display()
                            );
                            self.log = None;
                        } else {
                            self.log = Some(log);
                        }
                    }
                    Err(log_error) => {
                        self.log = None;
                        tracing::warn!("uninstall log is unavailable: {log_error}");
                    }
                }
                self.error = Some("error_msi_unknown");
                self.request_screen(Screen::Failed, cx);
                cx.notify();
                return;
            }
        };
        self.active_product_codes = product_codes.clone();
        if product_codes.is_empty() {
            self.start_orphan_uninstall(product, cx);
            return;
        }

        let log = match platform::uninstall_log_path() {
            Ok(log) => log,
            Err(error) => {
                self.fail_staging(error, cx);
                return;
            }
        };
        let install_dir = product.install_dir.clone();
        let cleanup_dir = install_dir.clone();
        let session = msi::start_uninstall_with(product_codes, log.clone(), move || {
            let mut failures = platform::remove_product_data();
            for failure in &failures {
                tracing::warn!("uninstall cleanup: {failure}");
            }
            if let Err(error) = platform::spawn_install_dir_cleanup(&cleanup_dir) {
                failures.push(format!("install folder: {error}"));
            }
            failures
        });

        self.install_dir = Some(install_dir);
        self.log = Some(log);
        self.error = None;
        self.notice = None;
        self.cleanup_incomplete = false;
        self.uninstall_attempted = true;
        self.installing.set(true);
        self.begin_progress_screen(Screen::Uninstalling, Stage::Preparing, cx);
        self.pump(session.events, cx);
        cx.notify();
    }

    fn start_orphan_uninstall(&mut self, product: InstalledProduct, cx: &mut Context<'_, Self>) {
        let log = match platform::uninstall_log_path() {
            Ok(log) => log,
            Err(error) => {
                self.fail_staging(error, cx);
                return;
            }
        };
        let install_dir = product.install_dir;
        let cleanup_dir = install_dir.clone();
        let session = msi::start_cleanup(
            move || {
                let Some(_lock) = platform::acquire_msi_execution(30_000) else {
                    return Err("another Windows Installer operation is running".to_string());
                };
                match installed::find_product_codes_checked() {
                    Ok(codes) if codes.is_empty() => {
                        platform::remove_orphan_registration(&cleanup_dir)
                    }
                    Ok(_) => Err(
                        "Cairn was installed while the uninstaller was open; close this window and try again."
                            .to_string(),
                    ),
                    Err(error) => Err(format!("MSI product enumeration failed: {error}")),
                }
            },
            log.clone(),
        );

        self.install_dir = Some(install_dir);
        self.log = Some(log);
        self.error = None;
        self.notice = None;
        self.cleanup_incomplete = false;
        self.uninstall_attempted = true;
        self.installing.set(true);
        self.begin_progress_screen(Screen::Uninstalling, Stage::Finishing, cx);
        self.pump(session.events, cx);
        cx.notify();
    }

    /// Completes a removal Windows Installer could not finish. It runs after the product record is
    /// gone (the automatic fallback) or after the person explicitly chose the file cleanup on the
    /// failure page. Under the global installer lock it re-checks that every registered product is
    /// one this removal started with, so a product registered under a new code is refused.
    ///
    /// It must not reject a busy screen: the automatic fallback runs from the Done event while the
    /// progress page still owns the busy state, and refusing there would strand the window.
    pub(crate) fn start_force_cleanup(&mut self, cx: &mut Context<'_, Self>) {
        if !force_cleanup_allowed(self.screen) || self.is_transitioning() {
            return;
        }
        // The automatic fallback arrives with the previous session finished; a duplicate call
        // while this cleanup is already running must not start a second one.
        if self.forced_cleanup && self.is_busy() {
            return;
        }
        self.record_remaining = false;
        let Some(install_dir) = self.install_dir.clone() else {
            self.error = Some("error_msi_unknown");
            self.request_screen(Screen::Failed, cx);
            cx.notify();
            return;
        };
        let log = match self.log.clone() {
            Some(log) => log,
            None => match platform::uninstall_log_path() {
                Ok(log) => log,
                Err(error) => {
                    self.fail_staging(error, cx);
                    return;
                }
            },
        };
        let expected = self.active_product_codes.clone();
        let session = msi::start_cleanup(
            move || {
                let Some(_lock) = platform::acquire_msi_execution(30_000) else {
                    return Err("another Windows Installer operation is running".to_string());
                };
                let current = installed::find_product_codes_checked()
                    .map_err(|error| format!("MSI product enumeration failed: {error}"))?;
                if !installed::all_products_are_expected(&current, &expected) {
                    return Err(
                        "Cairn was installed while this window was open; close this window and start again."
                            .to_string(),
                    );
                }
                platform::remove_orphan_registration(&install_dir)
            },
            log.clone(),
        );

        self.log = Some(log);
        self.error = None;
        self.notice = None;
        self.cleanup_incomplete = false;
        self.uninstall_attempted = true;
        self.forced_cleanup = true;
        self.installing.set(true);
        self.begin_progress_screen(Screen::Uninstalling, Stage::Finishing, cx);
        self.pump(session.events, cx);
        cx.notify();
    }

    fn pump(&mut self, events: Receiver<MsiEvent>, cx: &mut Context<'_, Self>) {
        let weak: WeakEntity<Self> = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            let mut events = Some(events);
            let mut completed = false;
            loop {
                let rx = events.take().expect("msi event receiver");
                let (event, rx) = cx
                    .background_executor()
                    .spawn(async move {
                        let event = rx.recv().ok();
                        (event, rx)
                    })
                    .await;
                events = Some(rx);
                let Some(event) = event else {
                    break;
                };
                let done = matches!(&event, MsiEvent::Done(_));
                weak.update(cx, |this, cx| this.handle_msi_event(event, cx))
                    .ok();
                if done {
                    completed = true;
                    break;
                }
            }
            // The worker sends Done even on failure. A closed channel without it means the worker
            // ended abnormally. A Done handler may have started a second cleanup session; that
            // session owns the busy state, so the completed pump must not clear it.
            if !completed {
                weak.update(cx, SetupApp::handle_msi_channel_closed).ok();
            }
        })
        .detach();
    }

    fn handle_msi_channel_closed(&mut self, cx: &mut Context<'_, Self>) {
        if !self.is_busy() {
            return;
        }
        if let Some(dir) = self.payload_dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
        self.error = Some("error_msi_unknown");
        self.request_screen(Screen::Failed, cx);
        cx.notify();
    }

    fn handle_msi_event(&mut self, event: MsiEvent, cx: &mut Context<'_, Self>) {
        match event {
            MsiEvent::Action(action) => {
                if let Some(stage) = Stage::from_action(&action)
                    && stage != self.stage
                {
                    self.stage = stage;
                    if !self.is_uninstall() {
                        self.motion.set_caption(self.i18n.get(stage.message_key()));
                    }
                    cx.notify();
                }
            }
            MsiEvent::Progress { current, total } => {
                if let Some(next) = progress_percent(self.progress, current, total) {
                    self.progress = Some(next);
                    self.motion.set_progress(visible_progress(Some(next)));
                    cx.notify();
                }
            }
            MsiEvent::CleanupFailed => {
                self.cleanup_incomplete = true;
                cx.notify();
            }
            MsiEvent::Done(result) => {
                let was_uninstalling = self.is_uninstall();
                if let Some(dir) = self.payload_dir.take() {
                    let _ = std::fs::remove_dir_all(dir);
                }
                match result {
                    Ok(success) => {
                        self.restart_required = success.restart_required;
                        let next = if was_uninstalling {
                            self.record_remaining = record_remaining_after_cleanup(
                                self.forced_cleanup,
                                installed::find_product_codes_checked(),
                            );
                            Screen::Uninstalled
                        } else {
                            Screen::Finished
                        };
                        self.forced_cleanup = false;
                        self.notice = None;
                        self.begin_completion(next, cx);
                    }
                    Err(failure) => {
                        self.log = Some(failure.log.clone());
                        if was_uninstalling && uninstall_can_fall_back_to_cleanup(failure.kind) {
                            self.start_force_cleanup(cx);
                            return;
                        }
                        // A forced cleanup has its own failure wording: the page offers no retry,
                        // because a second attempt could target a newer install.
                        self.error = Some(if was_uninstalling && self.forced_cleanup {
                            "error_msi_cleanup_failed"
                        } else {
                            failure.kind.message_key()
                        });
                        self.request_screen(Screen::Failed, cx);
                    }
                }
                cx.notify();
            }
        }
    }

    pub(crate) fn open_installed(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(dir) = self.install_dir.clone() else {
            self.notice = Some("error_launch");
            cx.notify();
            return;
        };
        let exe = dir.join("Cairn.exe");
        if !exe.is_file() {
            self.notice = Some("error_launch");
            cx.notify();
            return;
        }
        match platform::launch_installed_app(&exe) {
            Ok(()) => window.remove_window(),
            Err(error) => {
                tracing::warn!("could not launch Cairn: {error}");
                self.notice = Some("error_launch");
                cx.notify();
            }
        }
    }

    pub(crate) fn open_log(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        let Some(log) = self.log.clone() else {
            return;
        };
        let text = match std::fs::read(&log) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(error) => {
                tracing::warn!("could not read the log: {error}");
                self.notice = Some("error_open_log");
                cx.notify();
                return;
            }
        };
        let title = self.i18n.get("log_viewer_title");
        let close_label = self.i18n.get("window_close");
        let _ = show_log_dialog(window, cx, title, text, close_label);
    }

    pub(crate) fn retry(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) {
        if self.is_uninstall() || self.uninstall_attempted {
            self.start_uninstall(cx);
        } else {
            self.start_install(window, cx);
        }
    }
}

fn log_scroll_offset(
    offset: Point<Pixels>,
    max_offset: Point<Pixels>,
    key: &str,
    line_step: Pixels,
    page_step: Pixels,
) -> Point<Pixels> {
    // GPUI scroll offsets run from 0 at the top down to -max_offset.y at the bottom.
    let min_y = -max_offset.y.max(px(0.0));
    let y = match key {
        "up" => (offset.y + line_step).clamp(min_y, px(0.0)),
        "down" => (offset.y - line_step).clamp(min_y, px(0.0)),
        "pageup" => (offset.y + page_step).clamp(min_y, px(0.0)),
        "pagedown" => (offset.y - page_step).clamp(min_y, px(0.0)),
        "home" => px(0.0),
        "end" => min_y,
        _ => offset.y,
    };
    Point::new(offset.x, y)
}

/// One scrollable read-only surface for the installer log. The text is already in memory, so
/// opening it never touches a user-readable file or the shell's viewer.
fn show_log_dialog(
    window: &mut Window,
    cx: &mut Context<'_, SetupApp>,
    title: String,
    text: String,
    close_label: String,
) -> ScrollHandle {
    let surface = dialog_surface(cx.theme());
    let scroll_handle = ScrollHandle::new();
    let returned_scroll = scroll_handle.clone();
    let focus_handle = cx.focus_handle().tab_stop(true);
    let focus_body = Rc::new(Cell::new(true));
    window.open_dialog(cx, move |dialog, window, cx| {
        if focus_body.replace(false) {
            focus_handle.focus(window, cx);
        }
        let rem = window.rem_size();
        let body_height = (window.viewport_size().height * 0.55).clamp(rem * 8.0, rem * 18.0);
        let body = v_flex()
            .w_full()
            .text_sm()
            .line_height(relative(BODY_LINE_HEIGHT))
            .font_family("Consolas");
        let body = text.lines().enumerate().fold(body, |body, (index, line)| {
            let display = if line.is_empty() {
                " ".to_string()
            } else {
                line.to_string()
            };
            let row = div()
                .id(("setup-log-line", index))
                .w_full()
                .min_w_0()
                .truncate();
            if line.is_empty() {
                return body.child(row.child(display));
            }
            let full_text = line.to_string();
            body.child(
                row.aria_label(full_text.clone())
                    .tooltip(tooltip::element(move |window, _| {
                        div()
                            .id("setup-log-tooltip-content")
                            .max_w(window.rem_size() * 28.0)
                            .whitespace_normal()
                            .line_height(relative(BODY_LINE_HEIGHT))
                            .child(full_text.clone())
                    }))
                    .child(display),
            )
        });
        let keyboard_scroll = scroll_handle.clone();
        let click_focus = focus_handle.clone();
        let scroll_body = div()
            .id("setup-log-body")
            .w_full()
            .h(body_height)
            .overflow_y_scroll()
            .track_scroll(&scroll_handle)
            .track_focus(&focus_handle)
            .aria_label(title.clone())
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cairn_ui::focus::set_keyboard_navigation(false, cx);
                click_focus.focus(window, cx);
            })
            .on_key_down(move |event, _, cx| {
                let key = event.keystroke.key.as_str();
                if !matches!(key, "up" | "down" | "pageup" | "pagedown" | "home" | "end") {
                    return;
                }
                let offset = keyboard_scroll.offset();
                let next = log_scroll_offset(
                    offset,
                    keyboard_scroll.max_offset(),
                    key,
                    rem * BODY_LINE_HEIGHT,
                    (body_height - rem).max(rem),
                );
                if next != offset {
                    keyboard_scroll.set_offset(next);
                    cx.refresh_windows();
                }
                cx.stop_propagation();
            })
            .child(body);
        let focus_edge = cairn_ui::focus::edge(
            &focus_handle,
            cx.theme().ring,
            RADIUS,
            cairn_ui::focus::keyboard_navigation(cx),
            window,
            cx,
        );
        let scroller = h_flex()
            .w_full()
            .items_stretch()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .child(scroll_body)
                    .children(focus_edge),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .w(Scrollbar::width())
                    .h(body_height)
                    .child(overlay_scrollbar(
                        Scrollbar::vertical(&scroll_handle)
                            .id("setup-log-scrollbar")
                            .viewport_from_layout(),
                    )),
            );
        let title_row = h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .child(heading_text(title.clone(), FontWeight::SEMIBOLD))
            .child(
                div().w(Scrollbar::width()).flex_shrink_0().child(
                    Button::new("setup-log-close")
                        .small()
                        .ghost()
                        .icon(IconName::Close)
                        .accessibility_label(close_label.clone())
                        .w_full()
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
            );
        dialog
            .width(window.rem_size() * 36.0)
            .bg(surface)
            .entrance(DialogEntrance::FadeSlide)
            .close_button(false)
            .title(title_row)
            .child(scroller)
    });
    returned_scroll
}

#[cfg(test)]
mod tests {
    use super::{
        SetupApp, force_cleanup_allowed, log_scroll_offset, progress_percent,
        record_remaining_after_cleanup, show_log_dialog, uninstall_can_fall_back_to_cleanup,
        visible_progress,
    };
    use crate::i18n::I18n;
    use crate::model::{Launch, Screen, Stage};
    use crate::msi::FailureKind;
    use crate::preview::Preview;

    #[test]
    fn a_forced_cleanup_can_start_from_the_progress_or_failure_page() {
        assert!(force_cleanup_allowed(Screen::Uninstalling));
        assert!(force_cleanup_allowed(Screen::Failed));
        for screen in [
            Screen::Location,
            Screen::Installing,
            Screen::Finished,
            Screen::Maintenance,
            Screen::Uninstall,
            Screen::Uninstalled,
        ] {
            assert!(!force_cleanup_allowed(screen), "{screen:?}");
        }
    }

    #[test]
    fn only_a_missing_msi_record_falls_back_to_cleanup_without_asking() {
        assert!(uninstall_can_fall_back_to_cleanup(
            FailureKind::NotInstalled
        ));
        assert!(!uninstall_can_fall_back_to_cleanup(
            FailureKind::InvalidPackage
        ));
        assert!(!uninstall_can_fall_back_to_cleanup(FailureKind::Fatal));
        assert!(!uninstall_can_fall_back_to_cleanup(FailureKind::Cancelled));
    }

    #[test]
    fn a_forced_cleanup_discloses_a_record_it_could_not_confirm_is_gone() {
        assert!(record_remaining_after_cleanup(
            true,
            Ok(vec!["{A}".to_string()])
        ));
        assert!(!record_remaining_after_cleanup(true, Ok(Vec::new())));
        assert!(
            record_remaining_after_cleanup(true, Err(5)),
            "an enumeration failure cannot confirm the record is gone"
        );
        assert!(
            !record_remaining_after_cleanup(false, Ok(vec!["{A}".to_string()])),
            "a normal uninstall reports its own state, not the forced cleanup's"
        );
    }
    #[test]
    fn log_scroll_keys_move_by_line_or_page_and_clamp_to_bounds() {
        let offset = gpui_kit::gpui::point(gpui_kit::gpui::px(0.0), gpui_kit::gpui::px(-40.0));
        let max = gpui_kit::gpui::point(gpui_kit::gpui::px(0.0), gpui_kit::gpui::px(100.0));
        let line = gpui_kit::gpui::px(20.0);
        let page = gpui_kit::gpui::px(30.0);
        assert_eq!(
            log_scroll_offset(offset, max, "up", line, page).y,
            gpui_kit::gpui::px(-20.0)
        );
        assert_eq!(
            log_scroll_offset(offset, max, "down", line, page).y,
            gpui_kit::gpui::px(-60.0)
        );
        assert_eq!(
            log_scroll_offset(offset, max, "pageup", line, page).y,
            gpui_kit::gpui::px(-10.0)
        );
        assert_eq!(
            log_scroll_offset(offset, max, "pagedown", line, page).y,
            gpui_kit::gpui::px(-70.0)
        );
        assert_eq!(
            log_scroll_offset(offset, max, "home", line, page).y,
            gpui_kit::gpui::px(0.0)
        );
        assert_eq!(
            log_scroll_offset(offset, max, "end", line, page).y,
            gpui_kit::gpui::px(-100.0)
        );
        assert_eq!(
            log_scroll_offset(
                gpui_kit::gpui::point(gpui_kit::gpui::px(0.0), gpui_kit::gpui::px(-10.0)),
                max,
                "up",
                line,
                page
            )
            .y,
            gpui_kit::gpui::px(0.0)
        );
        assert_eq!(
            log_scroll_offset(
                gpui_kit::gpui::point(gpui_kit::gpui::px(0.0), gpui_kit::gpui::px(-90.0)),
                max,
                "down",
                line,
                page
            )
            .y,
            gpui_kit::gpui::px(-100.0)
        );
        assert_eq!(
            log_scroll_offset(
                gpui_kit::gpui::point(gpui_kit::gpui::px(0.0), gpui_kit::gpui::px(0.0)),
                gpui_kit::gpui::point(gpui_kit::gpui::px(0.0), gpui_kit::gpui::px(0.0)),
                "down",
                line,
                page
            )
            .y,
            gpui_kit::gpui::px(0.0)
        );
        assert_eq!(log_scroll_offset(offset, max, "other", line, page), offset);
    }

    #[test]
    fn log_dialog_scrolls_by_keyboard_and_closes_on_escape() {
        use gpui_kit::component::WindowExt as _;
        use gpui_kit::gpui::{
            AppContext as _, Bounds, Keystroke, Size, WindowBounds, WindowOptions, px,
        };
        use std::io::Write as _;

        let mut file = tempfile::NamedTempFile::new().expect("temp log file");
        let lines: Vec<String> = (0..40)
            .map(|i| {
                if i % 2 == 0 {
                    format!("LINE_{i:02} MSI: Product Cairn step {i}")
                } else {
                    format!(
                        "LINE_{i:02} MSI: Executing CairnInstallGuard on C:\\Program Files\\Cairn\\CairnService.exe with verified staging DACL and rollback"
                    )
                }
            })
            .collect();
        let log_text = lines.join("\n");
        file.write_all(log_text.as_bytes()).expect("write temp log");
        let log_path = file.path().to_path_buf();

        gpui_kit::application().run(move |cx| {
            gpui_kit::init(cx);
            cairn_ui::locale::init_layout_direction(cx);
            cairn_ui::theme::apply_appearance(false, 1.0, cx);

            let (handle, app) = gpui_kit::open_window(
                WindowOptions {
                    show: false,
                    focus: false,
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        Size {
                            width: px(520.0),
                            height: px(240.0),
                        },
                        cx,
                    ))),
                    ..WindowOptions::default()
                },
                cx,
                |window, cx| {
                    let preview = Preview {
                        screen: Screen::Failed,
                        launch: Launch::Setup,
                        language: Some("zh-CN".to_string()),
                        dark: Some(false),
                        installed: false,
                        progress: None,
                        stage: Stage::Preparing,
                        log: Some(log_path.clone()),
                        error: Some("error_msi_unknown"),
                        uninstall_attempted: false,
                        record_remaining: false,
                    };
                    cx.new(|cx| {
                        SetupApp::new(
                            window,
                            cx,
                            Launch::Setup,
                            I18n::new("zh-CN"),
                            Some(preview),
                            false,
                        )
                    })
                },
            )
            .expect("setup test window opens");

            handle
                .update(cx, |_, window, cx| {
                    // Opening through the public SetupApp entry point reads the log and mounts a dialog.
                    app.update(cx, |this, cx| this.open_log(window, cx));
                    window.draw(cx).clear(cx);
                    assert!(
                        window.has_active_dialog(cx),
                        "open_log must open the log viewer dialog"
                    );
                    assert!(
                        window.dispatch_keystroke(Keystroke::parse("escape").expect("escape"), cx),
                        "Escape must be handled by the open dialog"
                    );
                    window.draw(cx).clear(cx);
                    assert!(
                        !window.has_active_dialog(cx),
                        "Escape must close the log dialog even while the log body holds focus"
                    );

                    // Mount directly to observe the real GPUI ScrollHandle through keyboard scroll events.
                    let scroll = app.update(cx, |_, cx| {
                        show_log_dialog(
                            window,
                            cx,
                            "Install log".to_string(),
                            log_text.clone(),
                            "Close".to_string(),
                        )
                    });
                    window.draw(cx).clear(cx);
                    window.draw(cx).clear(cx);

                    let max_y = scroll.max_offset().y;
                    assert!(
                        max_y > px(0.0),
                        "40-line log must overflow the dialog viewport, got max_y={max_y:?}"
                    );
                    assert_eq!(scroll.offset().y, px(0.0), "log starts at top");

                    window.dispatch_keystroke(Keystroke::parse("down").expect("down"), cx);
                    window.draw(cx).clear(cx);
                    let after_down = scroll.offset().y;
                    assert!(
                        after_down < px(0.0),
                        "Down key must move GPUI scroll offset negative, got {after_down:?}"
                    );

                    window.dispatch_keystroke(Keystroke::parse("pagedown").expect("pagedown"), cx);
                    window.draw(cx).clear(cx);
                    let after_pagedown = scroll.offset().y;
                    assert!(
                        after_pagedown < after_down,
                        "PageDown must scroll further down: {after_pagedown:?} vs {after_down:?}"
                    );

                    window.dispatch_keystroke(Keystroke::parse("end").expect("end"), cx);
                    window.draw(cx).clear(cx);
                    assert_eq!(
                        scroll.offset().y,
                        -max_y,
                        "End key must scroll to -max_offset.y"
                    );

                    window.dispatch_keystroke(Keystroke::parse("home").expect("home"), cx);
                    window.draw(cx).clear(cx);
                    assert_eq!(
                        scroll.offset().y,
                        px(0.0),
                        "Home key must return scroll offset to top"
                    );
                })
                .expect("window update succeeds");
            cx.quit();
        });
    }

    #[test]
    fn progress_never_moves_backwards_when_msi_resets_an_action() {
        assert_eq!(progress_percent(None, 0, 100), Some(0.0));
        assert_eq!(progress_percent(Some(42.0), 0, 100), Some(42.0));
        let forward = progress_percent(Some(42.0), 60, 100).expect("known total");
        assert!((forward - 60.0).abs() < 0.001, "{forward}");
        assert_eq!(progress_percent(Some(60.0), 10, 100), Some(60.0));
        assert_eq!(progress_percent(Some(60.0), 0, 0), None);
    }

    #[test]
    fn the_working_bar_never_claims_completion() {
        assert_eq!(visible_progress(None), 0.0);
        assert_eq!(visible_progress(Some(42.0)), 42.0);
        assert_eq!(visible_progress(Some(100.0)), 99.0);
    }

    #[test]
    fn motion_tracks_screen_stage_and_percent_transitions_and_honors_reduce_motion() {
        use crate::msi::{MsiEvent, MsiSuccess};
        use gpui_kit::gpui::{AppContext as _, Bounds, Size, WindowBounds, WindowOptions, px};

        gpui_kit::application().run(move |cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(false);
            cairn_ui::locale::init_layout_direction(cx);
            cairn_ui::theme::apply_appearance(false, 1.0, cx);

            let (handle, app) = gpui_kit::open_window(
                WindowOptions {
                    show: false,
                    focus: false,
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        Size {
                            width: px(520.0),
                            height: px(240.0),
                        },
                        cx,
                    ))),
                    ..WindowOptions::default()
                },
                cx,
                |window, cx| {
                    let preview = Preview {
                        screen: Screen::Location,
                        launch: Launch::Setup,
                        language: Some("en-US".to_string()),
                        dark: Some(false),
                        installed: false,
                        progress: None,
                        stage: Stage::Preparing,
                        log: None,
                        error: None,
                        uninstall_attempted: false,
                        record_remaining: false,
                    };
                    cx.new(|cx| {
                        SetupApp::new(
                            window,
                            cx,
                            Launch::Setup,
                            I18n::new("en-US"),
                            Some(preview),
                            false,
                        )
                    })
                },
            )
            .expect("setup motion test window opens");

            handle
                .update(cx, |_, window, cx| {
                    window.draw(cx).clear(cx);
                    app.update(cx, |this, cx| {
                        assert_eq!(this.motion.transition_epoch(), 0);
                        assert_eq!(this.motion.percent().value(), None);
                        this.begin_progress_screen(Screen::Installing, Stage::Preparing, cx);
                        assert_eq!(this.screen, Screen::Location);
                        let token = this.screen_transition.expect("exit pending").token;
                        this.enter_screen_transition(token, cx);
                        assert_eq!(this.screen, Screen::Installing);
                        this.finish_screen_transition(token, cx);
                    });
                    window.draw(cx).clear(cx);

                    app.update(cx, |this, cx| {
                        assert_eq!(this.motion.transition_epoch(), 1);
                        this.handle_msi_event(MsiEvent::Action(String::from("InstallFiles")), cx);
                        assert_eq!(this.motion.caption().current().as_ref(), "Copying files");

                        for current in [8, 9, 10, 14, 24] {
                            this.handle_msi_event(
                                MsiEvent::Progress {
                                    current,
                                    total: 100,
                                },
                                cx,
                            );
                        }
                        let (_, tens, ones) =
                            this.motion.percent().slots().expect("percent visible");
                        assert_eq!(tens.expect("tens slot").current, 2);
                        assert_eq!(ones.current, 4);
                        assert_eq!(ones.previous, Some(0));
                    });
                    window.draw(cx).clear(cx);

                    app.update(cx, |this, cx| {
                        this.installing.set(true);
                        this.handle_msi_event(MsiEvent::Action(String::from("StartServices")), cx);
                        this.handle_msi_event(
                            MsiEvent::Done(Ok(MsiSuccess {
                                restart_required: false,
                            })),
                            cx,
                        );
                        assert_eq!(this.screen, Screen::Installing);
                        assert_eq!(this.motion.percent().value(), Some(100));
                        assert_eq!(
                            this.motion.caption().current().as_ref(),
                            "Finishing the installation"
                        );
                        assert!(this.installing.get());
                        assert_eq!(this.motion.transition_epoch(), 1);
                        let stale_token = this.completion.expect("completion pending").token;
                        this.begin_progress_screen(Screen::Installing, Stage::Preparing, cx);
                        this.finish_completion(stale_token, cx);
                        assert_eq!(this.screen, Screen::Installing);
                        assert_eq!(this.progress, None);
                        assert!(this.completion.is_none());
                        assert!(this.installing.get());

                        this.begin_completion(Screen::Finished, cx);
                        let token = this.completion.expect("new completion pending").token;
                        this.finish_completion(token, cx);
                        assert_eq!(this.screen, Screen::Installing);
                        assert!(this.completion.is_some(), "100% stays stable through exit");
                        let transition_token = this.screen_transition.expect("result exit").token;
                        this.enter_screen_transition(transition_token, cx);
                        assert_eq!(this.screen, Screen::Finished);
                        assert!(this.completion.is_none());
                        this.finish_screen_transition(transition_token, cx);
                        assert_eq!(this.motion.transition_epoch(), 2);
                        assert!(!this.installing.get());

                        cx.set_reduce_motion(true);
                        this.request_screen(Screen::Maintenance, cx);
                        assert_eq!(this.screen, Screen::Maintenance);
                        assert!(this.motion.is_settled());

                        cx.set_reduce_motion(false);
                        this.request_screen(Screen::Uninstalled, cx);
                        this.request_screen(Screen::Finished, cx);
                        let token = this.screen_transition.expect("latest target pending").token;
                        assert_eq!(
                            this.screen_transition.expect("pending").target,
                            Screen::Finished
                        );
                        this.finish_screen_transition(token + 1, cx);
                        assert_eq!(this.screen, Screen::Maintenance);
                        this.enter_screen_transition(token, cx);
                        assert_eq!(this.screen, Screen::Finished);
                        this.finish_screen_transition(token, cx);
                        assert!(this.motion.is_settled());

                        this.launch = Launch::Uninstall;
                        this.begin_progress_screen(Screen::Uninstalling, Stage::Preparing, cx);
                        this.handle_msi_event(
                            MsiEvent::Done(Ok(MsiSuccess {
                                restart_required: false,
                            })),
                            cx,
                        );
                        assert_eq!(
                            this.completion.expect("uninstall completion").next,
                            Screen::Uninstalled
                        );
                    });
                    window.draw(cx).clear(cx);
                })
                .expect("window update succeeds");
            cx.quit();
        });
    }
}
