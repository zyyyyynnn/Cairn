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
    pub(crate) restart_required: bool,
    pub(crate) installed: Option<InstalledProduct>,
    pub(crate) install_dir: Option<PathBuf>,
    pub(crate) uninstall_attempted: bool,
    /// The window's own focus handle. It is not a Tab stop, so it adds no stop to the Tab order;
    /// it gives the window a focus target so the window's key context stays on the dispatch path
    /// even when no control holds focus. Without it the keymap only sees the window chrome's own
    /// context, and the window-level Escape gesture never matches.
    pub(crate) root_focus: FocusHandle,
    /// The page heading receives focus when the screen changes, so a keyboard or screen-reader
    /// user lands on the new task instead of the disappearing control that opened it.
    pub(crate) screen_focus: FocusHandle,
    pub(crate) focused_screen: Option<Screen>,
    payload_dir: Option<PathBuf>,
    _activation: Subscription,
    _focus_lost: Subscription,
    _path_change: Subscription,
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

        // Keep the window key context reachable: if a rendered region disappears and focus is
        // dropped, return it to the window's own handle rather than leaving the commands
        // unreachable.
        let focus_lost = cx.on_focus_lost(window, |this, window, cx| {
            this.root_focus.focus(window, cx);
        });
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
            restart_required: false,
            installed,
            install_dir,
            uninstall_attempted: preview
                .as_ref()
                .is_some_and(|preview| preview.uninstall_attempted),
            root_focus,
            screen_focus,
            focused_screen: None,
            payload_dir: None,
            _activation: activation,
            _focus_lost: focus_lost,
            _path_change: path_change,
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

    pub(crate) fn start_install(&mut self, cx: &mut Context<Self>) {
        if self.launch != Launch::Setup || self.is_busy() {
            return;
        }
        let raw = self.path_input.read(cx).value().to_string();
        let install_dir = match validate_install_path(&raw) {
            Ok(path) => path,
            Err(problem) => {
                self.path_error = Some(problem);
                self.screen = Screen::Location;
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
        let product = self.installed.clone().or_else(installed::installed_product);
        let Some(product) = product else {
            self.error = Some(SetupError::simple("error_not_installed"));
            self.screen = Screen::Failed;
            cx.notify();
            return;
        };
        let Some(product_code) = product.product_code.clone() else {
            self.error = Some(SetupError::simple("error_not_installed"));
            self.screen = Screen::Failed;
            cx.notify();
            return;
        };

        let log = platform::uninstall_log_path();
        let helper = platform::prepare_install_dir_cleanup();
        let install_dir = product.install_dir.clone();
        let cleanup_dir = install_dir.clone();
        let session = msi::start_uninstall_with(product_code, log.clone(), move || {
            let failures = platform::remove_product_data();
            for failure in failures {
                tracing::warn!("uninstall cleanup: {failure}");
            }
            platform::spawn_install_dir_cleanup(&cleanup_dir, helper);
        });

        self.install_dir = Some(install_dir);
        self.log = Some(log);
        self.error = None;
        self.notice = None;
        self.progress = None;
        self.stage = Stage::Preparing;
        self.uninstall_attempted = true;
        self.screen = Screen::Uninstalling;
        self.installing.set(true);
        self.pump(session.events, cx);
        cx.notify();
    }

    fn pump(&mut self, mut events: UnboundedReceiver<MsiEvent>, cx: &mut Context<Self>) {
        let weak: WeakEntity<Self> = cx.entity().downgrade();
        cx.spawn(async move |_this, cx| {
            while let Some(event) = events.recv().await {
                let done = matches!(&event, MsiEvent::Done(_));
                weak.update(cx, |this, cx| this.handle_msi_event(event, cx))
                    .ok();
                if done {
                    break;
                }
            }
        })
        .detach();
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
                        self.screen = if was_uninstalling {
                            Screen::Uninstalled
                        } else {
                            Screen::Finished
                        };
                        self.notice = None;
                    }
                    Err(failure) => {
                        self.log = Some(failure.log.clone());
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
        match platform::launch_installed_app(&dir.join("Cairn.exe")) {
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
        if platform::open_path(&log).is_err() {
            self.notice = Some("error_open_log");
            cx.notify();
        }
    }

    pub(crate) fn view_license(&mut self, cx: &mut Context<Self>) {
        let path = std::env::temp_dir().join("Cairn-LICENSE.txt");
        let opened = std::fs::write(&path, crate::LICENSE_TEXT)
            .map_err(|error| error.to_string())
            .and_then(|()| platform::open_path(&path));
        if let Err(error) = opened {
            tracing::warn!("could not open the license: {error}");
            self.notice = Some("error_open_license");
            cx.notify();
        }
    }

    pub(crate) fn retry(&mut self, cx: &mut Context<Self>) {
        if self.is_uninstall() || self.uninstall_attempted {
            self.start_uninstall(cx);
        } else {
            self.start_install(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::progress_percent;

    #[test]
    fn progress_never_moves_backwards_when_msi_resets_an_action() {
        assert_eq!(progress_percent(None, 0, 100), Some(0.0));
        assert_eq!(progress_percent(Some(42.0), 0, 100), Some(42.0));
        let forward = progress_percent(Some(42.0), 60, 100).expect("known total");
        assert!((forward - 60.0).abs() < 0.001, "{forward}");
        assert_eq!(progress_percent(Some(60.0), 10, 100), Some(60.0));
        assert_eq!(progress_percent(Some(60.0), 0, 0), None);
    }
}
