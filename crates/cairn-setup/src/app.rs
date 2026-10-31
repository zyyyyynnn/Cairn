// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The installer's state machine. It owns the visible flow; the MSI owns the transaction.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::gpui::{
    AppContext as _, Context, Entity, FocusHandle, KeyBinding, Subscription, WeakEntity, Window,
};
use gpui_kit::Focusable;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::i18n::I18n;
use crate::installed;
use crate::model::{
    default_install_dir, initial_screen, validate_install_path, InstalledProduct, Launch,
    PathProblem, Screen, Stage,
};
use crate::msi::{self, MsiEvent, MsiFailure};
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

/// The page-level outcome of a failed transaction. It names the sentence to show. The raw
/// installer code is not part of it: that code belongs to the MSI log, which the failure page
/// links to, and nowhere on the page itself.
#[derive(Clone, Debug)]
pub struct SetupError {
    pub key: &'static str,
}

impl SetupError {
    pub fn simple(key: &'static str) -> Self {
        Self { key }
    }

    pub fn from_failure(failure: &MsiFailure) -> Self {
        Self::simple(failure.kind.message_key())
    }

    pub fn from_payload(error: &PayloadError) -> Self {
        let key = match error {
            PayloadError::Missing | PayloadError::TooLarge | PayloadError::AlreadyEmbedded => {
                "error_no_payload"
            }
            PayloadError::CorruptFooter | PayloadError::Checksum => "error_msi_invalid",
            PayloadError::Io(_) => "error_msi_fatal",
        };
        Self::simple(key)
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

pub struct SetupApp {
    pub(crate) i18n: I18n,
    pub(crate) launch: Launch,
    pub(crate) screen: Screen,
    pub(crate) dark: bool,
    pub(crate) accepted: bool,
    pub(crate) path_input: Entity<InputState>,
    pub(crate) path_error: Option<PathProblem>,
    pub(crate) installing: Rc<Cell<bool>>,
    pub(crate) progress: Option<f32>,
    pub(crate) stage: Stage,
    pub(crate) log: Option<PathBuf>,
    pub(crate) error: Option<SetupError>,
    pub(crate) notice: Option<&'static str>,
    pub(crate) cleanup_incomplete: bool,
    pub(crate) restart_required: bool,
    /// The forced cleanup could not remove the hidden Windows Installer product record.
    pub(crate) record_remaining: bool,
    pub(crate) installed: Option<InstalledProduct>,
    pub(crate) install_dir: Option<PathBuf>,
    pub(crate) uninstall_attempted: bool,
    /// The products the running removal was started for. A forced cleanup re-checks this set
    /// under the Windows Installer lock so it never removes a newer install.
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
    /// The license agreement is the field that a missing acceptance refers to, so an error can
    /// return the person to it instead of leaving focus on the Continue button.
    pub(crate) license_focus: FocusHandle,
    /// The destructive uninstall action owns its focus ring because the framework's filled-button
    /// focus line is drawn at reduced opacity inside the danger fill.
    pub(crate) uninstall_focus: FocusHandle,
    /// The failure page's explicit "remove remaining files" action.
    pub(crate) force_cleanup_focus: FocusHandle,
    pub(crate) focused_screen: Option<Screen>,
    payload_dir: Option<PathBuf>,
    _activation: Subscription,
    _focus_lost: Subscription,
    _path_change: Subscription,
    _keyboard_modality: Subscription,
}

impl SetupApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, launch: Launch) -> Self {
        let preview = Preview::from_env();
        let language = preview
            .as_ref()
            .and_then(|preview| preview.language.clone())
            .unwrap_or_else(|| cairn_ui::locale::system_language().to_string());
        let i18n = I18n::new(&language);
        let dark = preview
            .as_ref()
            .and_then(|preview| preview.dark)
            .unwrap_or_else(cairn_ui::locale::system_prefers_dark);
        cairn_ui::theme::apply_appearance(dark, 1.0, cx);

        let live_installed = installed::installed_product();
        let installed = preview
            .as_ref()
            .and_then(Preview::installed_product)
            .or(live_installed);
        let legacy_burn = preview.is_none()
            && launch == Launch::Setup
            && !installed::legacy_burn_uninstallers().is_empty();
        let screen = preview
            .as_ref()
            .map(|preview| preview.screen)
            .unwrap_or_else(|| initial_screen(launch, installed.as_ref(), legacy_burn));
        let launch = preview
            .as_ref()
            .map(|preview| preview.launch)
            .unwrap_or(launch);

        let initial_dir = installed
            .as_ref()
            .map(|product| product.install_dir.clone())
            .filter(|path| validate_install_path(&path.to_string_lossy()).is_ok())
            .unwrap_or_else(default_install_dir);
        let path_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(initial_dir.to_string_lossy().to_string())
        });

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
        let license_focus = cx.focus_handle().tab_stop(true);
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
        let path_change = cx.subscribe_in(
            &path_input,
            window,
            |this, _input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) && this.path_error.is_some() {
                    let raw = this.path_input.read(cx).value().to_string();
                    this.path_error = validate_install_path(&raw).err();
                    cx.notify();
                }
            },
        );
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
            .and_then(|preview| preview.error.clone())
            .or_else(|| {
                (launch == Launch::Uninstall && installed.is_none())
                    .then_some(SetupError::simple("error_not_installed"))
            });
        let install_dir = installed
            .as_ref()
            .map(|product| product.install_dir.clone());

        Self {
            i18n,
            launch,
            screen,
            dark,
            accepted: preview.as_ref().is_some_and(|preview| preview.accepted),
            path_input,
            path_error: None,
            installing,
            progress: preview.as_ref().and_then(|preview| preview.progress),
            stage: preview
                .as_ref()
                .map(|preview| preview.stage)
                .unwrap_or(Stage::Preparing),
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
            license_focus,
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
        matches!(self.screen, Screen::Installing | Screen::Uninstalling)
    }

    pub(crate) fn is_uninstall(&self) -> bool {
        self.launch == Launch::Uninstall
    }

    /// The one way out of the window. The installer already runs elevated and nothing is written
    /// to the machine until the MSI transaction starts, so cancelling and closing are the same
    /// gesture. A page that is running the MSI has no way out: closing the window mid-transaction
    /// would leave the machine half-installed.
    pub(crate) fn close_window(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        if !self.is_busy() {
            window.remove_window();
        }
    }

    /// The window-level Escape gesture. It is the keyboard equivalent of the Cancel button, so a
    /// person who never uses the pointer can still leave every page that has a Cancel action.
    /// A page that is running the MSI has no Cancel action and therefore no Escape either.
    pub(crate) fn on_escape(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        if !self.is_busy() && self.screen.can_cancel() {
            window.remove_window();
        }
    }

    pub(crate) fn continue_from_license(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.accepted {
            self.notice = Some("error_license_required");
            self.license_focus.focus(window, cx);
            cx.notify();
            return;
        }
        self.screen = Screen::Location;
        self.focused_screen = Some(Screen::Location);
        self.notice = None;
        self.path_input.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    pub(crate) fn back(&mut self, cx: &mut Context<Self>) {
        if self.screen == Screen::Location {
            self.screen = Screen::License;
            self.path_error = None;
            cx.notify();
        }
    }

    pub(crate) fn set_accepted(&mut self, accepted: bool, cx: &mut Context<Self>) {
        self.accepted = accepted;
        if accepted {
            self.notice = None;
        }
        cx.notify();
    }

    pub(crate) fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_busy() {
            return;
        }
        let title = self.i18n.get("location_title");
        let Some(path) = platform::browse_for_folder(&title) else {
            return;
        };
        self.path_error = None;
        self.path_input.update(cx, |state, cx| {
            state.set_value(path.to_string_lossy().to_string(), window, cx);
        });
        cx.notify();
    }

    pub(crate) fn start_install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.launch != Launch::Setup || self.is_busy() {
            return;
        }
        let raw = self.path_input.read(cx).value().to_string();
        let install_dir = match validate_install_path(&raw) {
            Ok(path) => path,
            Err(problem) => {
                self.path_error = Some(problem);
                self.screen = Screen::Location;
                self.focused_screen = Some(Screen::Location);
                self.path_input.read(cx).focus_handle(cx).focus(window, cx);
                cx.notify();
                return;
            }
        };

        let (msi, payload_dir) = match quiet::msi_for_this_build() {
            Ok(value) => value,
            Err(error) => {
                self.error = Some(SetupError::from_payload(&error));
                self.screen = Screen::Failed;
                cx.notify();
                return;
            }
        };

        let log = platform::install_log_path();
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
        self.path_error = None;
        self.progress = None;
        self.stage = Stage::Preparing;
        self.restart_required = false;
        self.cleanup_incomplete = false;
        self.record_remaining = false;
        self.active_product_codes.clear();
        self.forced_cleanup = false;
        self.uninstall_attempted = false;
        self.screen = Screen::Installing;
        self.installing.set(true);
        self.pump(session.events, cx);
        cx.notify();
    }

    pub(crate) fn start_uninstall(&mut self, cx: &mut Context<Self>) {
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
            self.error = Some(SetupError::simple("error_not_installed"));
            self.screen = Screen::Failed;
            cx.notify();
            return;
        };
        let product_codes = match installed::find_product_codes_checked() {
            Ok(codes) => codes,
            Err(error) => {
                let log = platform::uninstall_log_path();
                let _ = std::fs::write(&log, format!("MSI product enumeration failed: {error}\n"));
                self.log = Some(log);
                self.error = Some(SetupError::simple("error_msi_unknown"));
                self.screen = Screen::Failed;
                cx.notify();
                return;
            }
        };
        self.active_product_codes = product_codes.clone();
        if product_codes.is_empty() {
            self.start_orphan_uninstall(product, cx);
            return;
        }

        let log = platform::uninstall_log_path();
        let helper = platform::prepare_install_dir_cleanup();
        let install_dir = product.install_dir.clone();
        let cleanup_dir = install_dir.clone();
        let session = msi::start_uninstall_with(product_codes, log.clone(), move || {
            let mut failures = platform::remove_product_data();
            for failure in &failures {
                tracing::warn!("uninstall cleanup: {failure}");
            }
            if let Err(error) = platform::spawn_install_dir_cleanup(&cleanup_dir, helper) {
                failures.push(format!("install folder: {error}"));
            }
            failures
        });

        self.install_dir = Some(install_dir);
        self.log = Some(log);
        self.error = None;
        self.notice = None;
        self.cleanup_incomplete = false;
        self.progress = None;
        self.stage = Stage::Preparing;
        self.uninstall_attempted = true;
        self.screen = Screen::Uninstalling;
        self.installing.set(true);
        self.pump(session.events, cx);
        cx.notify();
    }

    fn start_orphan_uninstall(&mut self, product: InstalledProduct, cx: &mut Context<Self>) {
        let log = platform::uninstall_log_path();
        let helper = platform::prepare_install_dir_cleanup();
        let install_dir = product.install_dir;
        let cleanup_dir = install_dir.clone();
        let session = msi::start_cleanup(
            move || {
                let Some(_lock) = platform::acquire_msi_execution(30_000) else {
                    return Err("another Windows Installer operation is running".to_string());
                };
                match installed::find_product_codes_checked() {
                    Ok(codes) if codes.is_empty() => {
                        platform::remove_orphan_registration(&cleanup_dir, helper)
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
        self.progress = None;
        self.stage = Stage::Finishing;
        self.uninstall_attempted = true;
        self.screen = Screen::Uninstalling;
        self.installing.set(true);
        self.pump(session.events, cx);
        cx.notify();
    }

    /// Completes a removal Windows Installer could not finish. It runs after the product record is
    /// gone (the automatic fallback) or after the person explicitly chose the file cleanup on the
    /// failure page. Under the global installer lock it re-checks that every registered product is
    /// one this removal started with, so a newer install is never deleted.
    ///
    /// It must not reject a busy screen: the automatic fallback runs from the Done event while the
    /// progress page still owns the busy state, and refusing there would strand the window.
    pub(crate) fn start_force_cleanup(&mut self, cx: &mut Context<Self>) {
        if !force_cleanup_allowed(self.screen) {
            return;
        }
        self.record_remaining = false;
        let Some(install_dir) = self.install_dir.clone() else {
            self.error = Some(SetupError::simple("error_msi_unknown"));
            self.screen = Screen::Failed;
            cx.notify();
            return;
        };
        let log = self
            .log
            .clone()
            .unwrap_or_else(platform::uninstall_log_path);
        let helper = platform::prepare_install_dir_cleanup();
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
                platform::remove_orphan_registration(&install_dir, helper)
            },
            log.clone(),
        );

        self.log = Some(log);
        self.error = None;
        self.notice = None;
        self.cleanup_incomplete = false;
        self.progress = None;
        self.stage = Stage::Finishing;
        self.uninstall_attempted = true;
        self.forced_cleanup = true;
        self.screen = Screen::Uninstalling;
        self.installing.set(true);
        self.pump(session.events, cx);
        cx.notify();
    }

    fn pump(&mut self, mut events: UnboundedReceiver<MsiEvent>, cx: &mut Context<Self>) {
        let weak: WeakEntity<Self> = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            let mut completed = false;
            while let Some(event) = events.recv().await {
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
                weak.update(cx, |this, cx| this.handle_msi_channel_closed(cx))
                    .ok();
            }
        })
        .detach();
    }

    fn handle_msi_channel_closed(&mut self, cx: &mut Context<Self>) {
        if !self.is_busy() {
            return;
        }
        self.installing.set(false);
        if let Some(dir) = self.payload_dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
        self.error = Some(SetupError::simple("error_msi_unknown"));
        self.screen = Screen::Failed;
        cx.notify();
    }

    fn handle_msi_event(&mut self, event: MsiEvent, cx: &mut Context<Self>) {
        match event {
            MsiEvent::Action(action) => {
                if let Some(stage) = Stage::from_action(&action) {
                    if stage != self.stage {
                        self.stage = stage;
                        cx.notify();
                    }
                }
            }
            MsiEvent::Progress { current, total } => {
                if let Some(next) = progress_percent(self.progress, current, total) {
                    self.progress = Some(next);
                    cx.notify();
                }
            }
            MsiEvent::CleanupFailed => {
                self.cleanup_incomplete = true;
                cx.notify();
            }
            MsiEvent::Done(result) => {
                let was_uninstalling = self.screen == Screen::Uninstalling;
                self.installing.set(false);
                if let Some(dir) = self.payload_dir.take() {
                    let _ = std::fs::remove_dir_all(dir);
                }
                match result {
                    Ok(success) => {
                        self.progress = Some(100.0);
                        self.stage = Stage::Finishing;
                        self.restart_required = success.restart_required;
                        if was_uninstalling {
                            self.record_remaining = record_remaining_after_cleanup(
                                self.forced_cleanup,
                                installed::find_product_codes_checked(),
                            );
                            self.screen = Screen::Uninstalled;
                        } else {
                            self.screen = Screen::Finished;
                        }
                        self.forced_cleanup = false;
                        self.notice = None;
                    }
                    Err(failure) => {
                        self.log = Some(failure.log.clone());
                        if was_uninstalling && uninstall_can_fall_back_to_cleanup(failure.kind) {
                            self.start_force_cleanup(cx);
                            return;
                        }
                        self.error = Some(SetupError::from_failure(&failure));
                        self.screen = Screen::Failed;
                    }
                }
                cx.notify();
            }
        }
    }

    pub(crate) fn open_installed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(crate) fn open_log(&mut self, cx: &mut Context<Self>) {
        let Some(log) = self.log.clone() else {
            return;
        };
        if !log.is_file() || platform::open_path(&log).is_err() {
            self.notice = Some("error_open_log");
            cx.notify();
        }
    }

    pub(crate) fn view_license(&mut self, cx: &mut Context<Self>) {
        let path = platform::license_path();
        let opened = std::fs::write(&path, crate::LICENSE_TEXT)
            .map_err(|error| error.to_string())
            .and_then(|()| platform::open_path(&path));
        if let Err(error) = opened {
            tracing::warn!("could not open the license: {error}");
            self.notice = Some("error_open_license");
            cx.notify();
        }
    }

    pub(crate) fn retry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_uninstall() || self.uninstall_attempted {
            self.start_uninstall(cx);
        } else {
            self.start_install(window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        force_cleanup_allowed, progress_percent, record_remaining_after_cleanup,
        uninstall_can_fall_back_to_cleanup, visible_progress,
    };
    use crate::model::Screen;
    use crate::msi::FailureKind;

    #[test]
    fn a_forced_cleanup_can_start_from_the_progress_or_failure_page() {
        assert!(force_cleanup_allowed(Screen::Uninstalling));
        assert!(force_cleanup_allowed(Screen::Failed));
        for screen in [
            Screen::License,
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
}
