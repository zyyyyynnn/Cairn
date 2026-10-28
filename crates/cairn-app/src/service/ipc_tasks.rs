// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! IPC background task orchestration: connection monitoring, explicit service start, and scoped
//! configuration submission.

use crate::app::CairnApp;
use crate::service::config_submission::{
    self, IndexScopeChange, SubmissionOutcome, SubmissionState,
};
use crate::service::scope_switch::{ScopeSwitchFailure, ScopeSwitchResult};
use crate::service::search_scope::SearchScope;
use crate::service::service_issue::{should_try_silent_start, ServiceIssue};
use cairn_protocol::status::VolumeStatusSummary;
use gpui_kit::gpui::{AsyncApp, Context};
use std::sync::OnceLock;
use std::time::Duration;

#[derive(Clone, Copy)]
enum ContentAction {
    Set(bool),
    Clear,
}

/// Volume status is sampled every 100ms so the sidebar and loading state stay fresh. Result rows
/// are not: a query is a full index scan, so a growing index is queried once when it settles, not
/// on every batch.
const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(100);
const STATUS_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const STATUS_MAX_CONSECUTIVE_FAILURES: u32 = 3;

fn status_failure_is_terminal(consecutive_failures: u32) -> bool {
    consecutive_failures >= STATUS_MAX_CONSECUTIVE_FAILURES
}

/// What the status poll does after one answer, and whether it keeps polling.
///
/// A query over a growing index is a full scan of a partial index: its total is not final, and
/// repeating it for every batch spends the indexer's CPU to publish a number that will change
/// again. The poll therefore only waits while volumes load, then asks a visible table once for
/// the result set of the completed index. If an answer is still on its way, wait for it. A hidden
/// table (Treemap/Recycle Bin) never receives a background query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusRefreshDecision {
    RefreshAndStop,
    Wait,
    Stop,
}

fn status_refresh_decision(
    now_loading: bool,
    searching: bool,
    table_visible: bool,
) -> StatusRefreshDecision {
    if searching || now_loading {
        StatusRefreshDecision::Wait
    } else if table_visible {
        StatusRefreshDecision::RefreshAndStop
    } else {
        StatusRefreshDecision::Stop
    }
}

/// Whether two status samples differ in anything the window actually shows.
///
/// The indexed count drives no visible label: the band publishes only a completed query's total,
/// and the sidebar shows the volume's state. A growing count therefore must not repaint the
/// window, or a long build repaints at the poll rate for a number nobody can see. The state,
/// identity and presence of a volume are visible and do repaint.
fn volume_status_visibility_changed(
    previous: &[VolumeStatusSummary],
    next: &[VolumeStatusSummary],
) -> bool {
    previous.len() != next.len()
        || previous.iter().zip(next).any(|(old, new)| {
            old.drive_letter != new.drive_letter
                || old.volume_guid != new.volume_guid
                || old.status != new.status
                || old.usn_enabled != new.usn_enabled
        })
}

/// Bounded multi-threaded Tokio runtime for desktop IPC operations.
/// Explicitly capped to 2 worker threads and 4 blocking threads to minimize idle heap/stack footprint.
pub fn get_tokio_runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(4)
            .enable_all()
            .build()
            .expect("Failed to initialize Tokio runtime")
    })
}

impl CairnApp {
    /// Asks the service whether it is there, and records which kind of answer came back (SVC-03,
    /// SVC-04). One probe, one owner method: the window's first frame and the retry control in the
    /// service-issue band both go through here, so a retry cannot speak a different protocol from the
    /// startup check.
    pub fn check_service_connection(&mut self, cx: &mut Context<Self>) {
        // The scope the user chose decides which indexer answers, not only which registration the
        // service carries: a window that remembers "readable files" must read from the user
        // indexer even when the service is running (UI-10).
        if self.service.search_scope.needs_local_only() {
            self.start_local_indexer(cx);
            return;
        }
        let runtime = get_tokio_runtime();
        self.service.is_probing_service = true;
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::shell::perf::mark("probe start");
            runtime.spawn(async move {
                let started = std::time::Instant::now();
                let res = match cairn_client::client::CairnClient::connect().await {
                    Ok(mut client) => client.get_status().await,
                    Err(e) => Err(e),
                };
                let outcome = match &res {
                    Ok(_) => "ok",
                    Err(_) => "failed",
                };
                crate::shell::perf::mark(&format!(
                    "probe {outcome} {:.3}ms",
                    started.elapsed().as_secs_f64() * 1000.0
                ));
                let _ = tx.send(res);
            });

            let outcome = rx.await;
            let _ = this.update(cx, |this: &mut CairnApp, cx| {
                this.service.is_probing_service = false;
                match outcome {
                    Ok(res) => match res {
                        Ok(envelope) => {
                            this.service.is_service_connected = true;
                            this.service.service_issue = None;
                            this.service.has_attempted_silent_start = false;
                            this.service.status.update_volumes(envelope.volumes);
                            this.start_status_refresh(cx);
                            // A connection probe must not replace the location the
                            // person is reading; the bin does not need the index.
                            if !this.recycle_bin.is_open() {
                                this.trigger_search(cx);
                            }
                            this.refresh_content_status(cx);
                        }
                        Err(e) => {
                            this.service.is_service_connected = false;
                            let issue = ServiceIssue::from_client_error(&e);
                            this.handle_probe_failure(issue, cx);
                        }
                    },
                    Err(e) => {
                        tracing::warn!("Cairn connection probe task lost: {e}");
                        this.service.is_service_connected = false;
                        this.handle_probe_failure(ServiceIssue::NotRunning, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Distinguishes "installed but not answering" from "not installed" without ever elevating.
    /// Opening the window only asks this question; starting the service is an explicit action.
    /// Polls the service while any volume is building or catching up. The probe's first answer can
    /// be `CatchingUp(0)`, and without this loop the GUI would keep that stale snapshot forever:
    /// the skeleton would never clear and the capsule would freeze at "synchronizing".
    pub(crate) fn start_status_refresh(&mut self, cx: &mut Context<Self>) {
        if self.service.is_refreshing_status || !self.service.status.is_any_loading() {
            return;
        }
        self.service.is_refreshing_status = true;
        // The poll belongs to the indexer that is answering now. When the scope changes, the
        // generation changes with it and this poll ends rather than reporting the old scope's
        // state into the new one.
        let generation = self.service.status_generation;
        let runtime = get_tokio_runtime();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let status_task = runtime.spawn(async move {
            let mut client: Option<cairn_client::client::CairnClient> = None;
            let mut consecutive_failures = 0u32;
            loop {
                if client.is_none() {
                    match crate::service::ipc_channel::connect().await {
                        Ok(connected) => {
                            client = Some(connected);
                            consecutive_failures = 0;
                        }
                        Err(error) => {
                            consecutive_failures += 1;
                            if status_failure_is_terminal(consecutive_failures) {
                                let _ = tx.send(Err(error));
                                break;
                            }
                            tokio::time::sleep(STATUS_RETRY_INTERVAL).await;
                            continue;
                        }
                    }
                }

                let outcome = client
                    .as_mut()
                    .expect("client was just connected")
                    .get_status()
                    .await;
                match outcome {
                    Ok(envelope) => {
                        consecutive_failures = 0;
                        if tx.send(Ok(envelope)).is_err() {
                            break;
                        }
                        tokio::time::sleep(STATUS_POLL_INTERVAL).await;
                    }
                    Err(error) => {
                        client = None;
                        consecutive_failures += 1;
                        if status_failure_is_terminal(consecutive_failures) {
                            let _ = tx.send(Err(error));
                            break;
                        }
                        tokio::time::sleep(STATUS_RETRY_INTERVAL).await;
                    }
                }
            }
        });

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            while let Some(outcome) = rx.recv().await {
                let keep_going = this
                    .update(cx, |this: &mut CairnApp, cx| {
                        if !this.service.is_service_connected {
                            return false;
                        }
                        if this.service.status_generation != generation {
                            return false;
                        }
                        match outcome {
                            Ok(envelope) => {
                                // Only a sample that changes what the window shows is worth a
                                // frame. A growing indexed count is not visible anywhere in the
                                // loading state, so repainting for it would cost a full frame ten
                                // times a second while the index builds.
                                let visible_change = volume_status_visibility_changed(
                                    &this.service.status.volumes,
                                    &envelope.volumes,
                                );
                                this.service.status.update_volumes(envelope.volumes);
                                let now_loading = this.service.status.is_any_loading();
                                let table_visible =
                                    !this.recycle_bin.is_open() && this.drive_view.is_none();
                                // The count is owned by the query result set, not by the raw
                                // volume total. Status polling only refreshes rows and state.
                                if visible_change {
                                    cx.notify();
                                }
                                match status_refresh_decision(
                                    now_loading,
                                    this.query.is_searching,
                                    table_visible,
                                ) {
                                    StatusRefreshDecision::RefreshAndStop => {
                                        this.refresh_results(cx);
                                        false
                                    }
                                    StatusRefreshDecision::Wait => true,
                                    StatusRefreshDecision::Stop => false,
                                }
                            }
                            Err(error) => {
                                this.service.is_service_connected = false;
                                this.service.service_issue =
                                    Some(ServiceIssue::from_client_error(&error));
                                cx.notify();
                                false
                            }
                        }
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
            status_task.abort();
            let _ = this.update(cx, |this: &mut CairnApp, _| {
                this.service.is_refreshing_status = false;
            });
        })
        .detach();
    }

    fn service_issue_after_probe(&mut self) -> ServiceIssue {
        self.service.service_installed = crate::service::service_control::service_is_installed();
        if self.service.service_installed {
            ServiceIssue::NotRunning
        } else {
            ServiceIssue::NotInstalled
        }
    }

    /// Records a failed probe. When the service is installed but stopped, one silent SCM start is
    /// attempted first so a normal non-administrator launch recovers without a UAC prompt. When
    /// the pipe still does not answer, the window starts its own user-privilege indexer: the app
    /// must be usable without elevation, so "the service did not start" is a smaller index, not a
    /// dead window.
    fn handle_probe_failure(&mut self, issue: ServiceIssue, cx: &mut Context<Self>) {
        let issue = if issue == ServiceIssue::NotRunning {
            self.service_issue_after_probe()
        } else {
            issue
        };
        if should_try_silent_start(issue, self.service.has_attempted_silent_start) {
            self.service.has_attempted_silent_start = true;
            if crate::service::service_control::try_start_installed_service().is_ok() {
                self.poll_service_after_start(cx, false);
                return;
            }
        }
        // A service that answered and refused this account is a policy decision. Falling back to a
        // local index would quietly bypass it, so that issue stays visible instead.
        if issue == ServiceIssue::Unauthorized {
            self.service.service_issue = Some(issue);
            return;
        }
        self.start_local_indexer(cx);
    }

    /// Starts the bundled user-privilege indexer and connects to its pipe. This is the normal
    /// non-administrator path; it never requests elevation.
    fn start_local_indexer(&mut self, cx: &mut Context<Self>) {
        if let Some(child) = self.service.user_indexer.as_mut() {
            // A previous local indexer is still serving (the service was just probed again):
            // reuse it instead of starting a second scan of the same tree.
            if matches!(child.try_wait(), Ok(None)) {
                self.poll_user_pipe(cx);
                return;
            }
            self.service.user_indexer = None;
        }

        match crate::shell::os::spawn_user_indexer(std::process::id()) {
            Ok(child) => {
                self.service.user_indexer = Some(child);
                self.poll_user_pipe(cx);
            }
            Err(error) => {
                tracing::warn!("the local indexer could not start: {error}");
                self.service.is_service_connected = false;
                self.service.service_issue = Some(
                    self.service
                        .service_issue
                        .unwrap_or(ServiceIssue::NotRunning),
                );
                cx.notify();
            }
        }
    }

    /// Polls the user pipe while the freshly started indexer binds it and opens its first volumes.
    fn poll_user_pipe(&mut self, cx: &mut Context<Self>) {
        self.service.is_probing_service = true;
        cx.notify();

        let runtime = get_tokio_runtime();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            runtime.spawn(async move {
                let mut last_error = None;
                for attempt in 0..3 {
                    if attempt > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    }
                    match crate::service::ipc_channel::connect_user().await {
                        Ok(mut client) => {
                            let _ = tx.send(client.get_status().await);
                            return;
                        }
                        Err(error) => last_error = Some(error),
                    }
                }
                let _ = tx.send(Err(last_error.unwrap_or(
                    cairn_client::client::ClientError::ConnectFailed(std::io::Error::new(
                        std::io::ErrorKind::NotConnected,
                        "the user-mode indexer did not answer",
                    )),
                )));
            });

            let outcome = rx.await;
            let _ = this.update(cx, |this: &mut CairnApp, cx| {
                this.service.is_probing_service = false;
                match outcome {
                    Ok(Ok(envelope)) => {
                        crate::service::ipc_channel::use_local_indexer();
                        this.service.local_mode = true;
                        this.service.is_service_connected = true;
                        this.service.service_issue = None;
                        this.service.status.update_volumes(envelope.volumes);
                        this.start_status_refresh(cx);
                        if !this.recycle_bin.is_open() {
                            this.trigger_search(cx);
                        }
                    }
                    _ => {
                        crate::service::ipc_channel::use_service();
                        this.service.local_mode = false;
                        this.service.is_service_connected = false;
                        this.service.service_issue = Some(ServiceIssue::NotRunning);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The retry path the service-issue band offers (SVC-03). While a probe is already in flight the
    /// request is dropped: the band's own control is the guard, and re-entering here would otherwise
    /// run two probes that answer in whichever order they finish.
    pub fn retry_service_connection(&mut self, cx: &mut Context<Self>) {
        if self.service.is_probing_service {
            return;
        }
        self.check_service_connection(cx);
        cx.notify();
    }

    /// The explicit action behind the offline band's button: ask SCM to start the installed service
    /// through one UAC elevation, then poll for its pipe. A service that is not installed is reported
    /// as such and never triggers a UAC prompt.
    pub fn start_service(&mut self, cx: &mut Context<Self>) {
        if self.service.is_probing_service {
            return;
        }
        self.service.service_installed = crate::service::service_control::service_is_installed();
        if !self.service.service_installed {
            self.service.service_issue = Some(ServiceIssue::NotInstalled);
            cx.notify();
            return;
        }
        self.poll_service_after_start(cx, true);
    }

    /// Polls the pipe after an explicit elevation or a silent SCM start. `elevate` is the only
    /// difference: both paths end in the same "is the pipe answering?" loop, so a service that is
    /// still Starting is not reported as offline on the very next frame.
    fn poll_service_after_start(&mut self, cx: &mut Context<Self>, elevate: bool) {
        self.service.is_probing_service = true;
        cx.notify();

        let runtime = get_tokio_runtime();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            runtime.spawn(async move {
                if elevate {
                    // UAC and sc.exe are blocking; keep them off the IPC runtime's worker threads.
                    let launched = tokio::task::spawn_blocking(
                        crate::service::service_control::start_service_elevated,
                    )
                    .await;
                    if !matches!(launched, Ok(Ok(()))) {
                        let _ = tx.send(Err(cairn_client::client::ClientError::ConnectFailed(
                            std::io::Error::new(
                                std::io::ErrorKind::PermissionDenied,
                                "Service start was cancelled or refused",
                            ),
                        )));
                        return;
                    }
                }
                // The service reports Running only after its data layout is secured; the pipe
                // becomes available at the same point. Poll long enough for a cold cache load.
                for _ in 0..30 {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    if let Ok(mut client) = cairn_client::client::CairnClient::connect().await {
                        let status = client.get_status().await;
                        let _ = tx.send(status);
                        return;
                    }
                }
                let _ = tx.send(Err(cairn_client::client::ClientError::ConnectFailed(
                    std::io::Error::new(
                        std::io::ErrorKind::NotConnected,
                        "Service did not answer after starting",
                    ),
                )));
            });

            let outcome = rx.await;
            let _ = this.update(cx, |this: &mut CairnApp, cx| {
                this.service.is_probing_service = false;
                if let Ok(Ok(envelope)) = outcome {
                    // The service is authoritative; stop the local scan so two indexers do not
                    // walk the same volumes and the window cannot mix their results.
                    if let Some(mut child) = this.service.user_indexer.take() {
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    crate::service::ipc_channel::use_service();
                    this.service.local_mode = false;
                    this.service.is_service_connected = true;
                    this.service.service_issue = None;
                    this.service.service_installed = true;
                    this.service.has_attempted_silent_start = false;
                    this.service.status.update_volumes(envelope.volumes);
                    this.start_status_refresh(cx);
                    if !this.recycle_bin.is_open() {
                        this.trigger_search(cx);
                    }
                    this.refresh_content_status(cx);
                } else if let Some(child) = this.service.user_indexer.as_mut() {
                    // The explicit start was cancelled or refused while a local index already
                    // serves the window: stay usable, and leave the service for the next launch.
                    let _ = child.try_wait();
                } else {
                    this.service.is_service_connected = false;
                    this.service.service_installed =
                        crate::service::service_control::service_is_installed();
                    let issue = if this.service.service_installed {
                        ServiceIssue::NotRunning
                    } else {
                        ServiceIssue::NotInstalled
                    };
                    this.service.service_issue = Some(issue);
                    this.start_local_indexer(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Switches the window between the two search scopes (UI-10).
    ///
    /// The scope that needs the service may have to change the service's start type, which needs
    /// an administrator. The elevation runs on a worker thread because UAC and `sc.exe` block.
    /// A refused elevation keeps the previous scope: the window does not claim the new scope
    /// before it is actually in force, and it does not half-apply one.
    pub fn switch_search_scope(&mut self, target: SearchScope, cx: &mut Context<Self>) {
        if self.service.is_switching_scope || target == self.service.search_scope {
            return;
        }
        let previous = self.service.search_scope;
        self.service.is_switching_scope = true;
        cx.notify();

        let runtime = get_tokio_runtime();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            runtime.spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    crate::service::scope_switch::switch_to(target, previous)
                })
                .await;
                let _ = tx.send(result);
            });

            let outcome = rx.await;
            let _ = this.update(cx, |this: &mut CairnApp, cx| {
                this.service.is_switching_scope = false;
                match outcome {
                    Ok(Ok(result)) => {
                        // The result carries the scope that is in force. Only an applied switch
                        // moves the window; every other outcome leaves it where it was, which is
                        // what keeps a refused switch from half-applying.
                        this.service.search_scope = result.scope_in_force();
                        match result {
                            ScopeSwitchResult::Applied { scope } => {
                                this.apply_search_scope(scope, cx);
                            }
                            ScopeSwitchResult::Cancelled { .. } => {
                                // The user dismissed the prompt. The window stays usable and
                                // nothing on the machine changed, so this is a note rather than an
                                // error.
                                let message = this.i18n.get_text("menu_search_scope_cancelled");
                                this.show_info_toast(
                                    this.i18n.get_text("menu_search_scope"),
                                    message,
                                    cx,
                                );
                            }
                            ScopeSwitchResult::Failed { reason, detail, .. } => {
                                // The technical detail belongs in the log. The page gets one
                                // sentence in the user's language.
                                tracing::warn!("the scope switch failed: {reason:?} ({detail})");
                                let title = this.i18n.get_text("menu_search_scope");
                                let message = this.i18n.get_text(scope_switch_failure_key(reason));
                                this.show_error_toast(title, message, cx);
                            }
                            ScopeSwitchResult::NotInstalled { .. } => {
                                let message = this.i18n.get_text("menu_search_scope_not_installed");
                                this.show_error_toast(
                                    this.i18n.get_text("menu_search_scope"),
                                    message,
                                    cx,
                                );
                            }
                        }
                    }
                    // The worker thread panicked or the channel closed: the switch did not happen,
                    // so the previous scope stays in force.
                    Ok(Err(join_error)) => {
                        tracing::warn!("the scope switch task stopped: {join_error}");
                        let title = this.i18n.get_text("menu_search_scope");
                        let message = this.i18n.get_text("menu_search_scope_change_failed");
                        this.show_error_toast(title, message, cx);
                    }
                    Err(_) => {
                        let title = this.i18n.get_text("menu_search_scope");
                        let message = this.i18n.get_text("menu_search_scope_cancelled");
                        this.show_info_toast(title, message, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Puts the window on `scope`: it routes every later IPC call to the matching indexer and
    /// re-reads what that indexer knows.
    fn apply_search_scope(&mut self, scope: SearchScope, cx: &mut Context<Self>) {
        self.service.search_scope = scope;
        self.settings.search_scope = Some(scope);
        let _ = self.settings.save();
        // A different indexer answers from now on, so the status poll of the previous one ends.
        self.service.status_generation = self.service.status_generation.wrapping_add(1);
        self.service.is_refreshing_status = false;

        match scope {
            SearchScope::AllFiles => {
                // The service is the source of truth for the whole machine; the local indexer is
                // ended so two indexers never walk the same volumes.
                if let Some(mut child) = self.service.user_indexer.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                crate::service::ipc_channel::use_service();
                self.service.local_mode = false;
                self.check_service_connection(cx);
            }
            SearchScope::ReadableFiles => {
                crate::service::ipc_channel::use_local_indexer();
                self.start_local_indexer(cx);
            }
        }
    }

    /// Reads the service's content-index lifecycle. While a backfill is running, one delayed
    /// follow-up keeps the progress fresh without a permanent polling loop.
    pub fn refresh_content_status(&mut self, cx: &mut Context<Self>) {
        if self.service.local_mode || self.service.content_status_loading {
            return;
        }
        self.service.content_status_loading = true;
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            get_tokio_runtime().spawn(async move {
                let result = match cairn_client::client::CairnClient::connect().await {
                    Ok(mut client) => client.get_content_index_status().await,
                    Err(err) => Err(err),
                };
                let _ = tx.send(result);
            });

            let result = rx.await;
            this.update(cx, |this: &mut CairnApp, cx| {
                this.service.content_status_loading = false;
                match result {
                    Ok(Ok(envelope)) => {
                        this.service.content_status_error = None;
                        if let cairn_protocol::message::ResponsePayload::ContentIndexStatus {
                            status,
                        } = envelope.payload
                        {
                            this.service.content_status = status;
                        } else {
                            this.service.content_status_error =
                                Some("the service answered with a different payload".to_string());
                        }
                    }
                    Ok(Err(err)) => this.service.content_status_error = Some(err.to_string()),
                    Err(panic) => {
                        this.service.content_status_error =
                            Some(format!("the content-index status task stopped: {panic}"));
                    }
                }
                let follow_up = matches!(
                    this.service.content_status.state,
                    cairn_protocol::status::ContentIndexState::Backfilling { .. }
                ) && this.service.content_status_error.is_none();
                this.repaint_shell_and_overlays(cx);
                if follow_up {
                    this.schedule_content_status_refresh(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn schedule_content_status_refresh(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            // GPUI's executor owns this task; Tokio's timer would panic with no reactor.
            cx.background_executor()
                .timer(std::time::Duration::from_millis(700))
                .await;
            this.update(cx, |this: &mut CairnApp, cx| {
                this.refresh_content_status(cx)
            })
            .ok();
        })
        .detach();
    }

    pub fn set_content_index_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.run_content_action(ContentAction::Set(enabled), cx);
    }

    pub fn clear_content_index(&mut self, cx: &mut Context<Self>) {
        self.run_content_action(ContentAction::Clear, cx);
    }

    fn run_content_action(&mut self, action: ContentAction, cx: &mut Context<Self>) {
        if self.service.local_mode || self.service.content_action_pending {
            return;
        }
        self.service.content_action_pending = true;
        self.service.content_action_error = None;
        self.repaint_shell_and_overlays(cx);

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            get_tokio_runtime().spawn(async move {
                let outcome: Result<(), String> = async {
                    match action {
                        ContentAction::Set(enabled) => {
                            let state = config_submission::submit_index_scope(
                                IndexScopeChange::SetContentIndex { enabled },
                            )
                            .await;
                            match state {
                                SubmissionState::Finished {
                                    outcome: SubmissionOutcome::Applied,
                                    ..
                                } => Ok(()),
                                SubmissionState::Finished { detail, .. } => Err(detail
                                    .unwrap_or_else(|| {
                                        "the service rejected the change".to_string()
                                    })),
                            }
                        }
                        ContentAction::Clear => {
                            let mut client = cairn_client::client::CairnClient::connect()
                                .await
                                .map_err(|err| err.to_string())?;
                            match client.clear_content_index().await {
                                Ok(envelope) => match envelope.payload {
                                    cairn_protocol::message::ResponsePayload::ClearContentIndexAck {
                                        success: true,
                                    } => Ok(()),
                                    _ => Err("the service did not accept the clear".to_string()),
                                },
                                Err(err) => Err(err.to_string()),
                            }
                        }
                    }
                }
                .await;
                let _ = tx.send(outcome);
            });

            let outcome = rx.await;
            this.update(cx, |this: &mut CairnApp, cx| {
                this.service.content_action_pending = false;
                match outcome {
                    Ok(Ok(())) => {
                        this.service.content_action_error = None;
                        this.refresh_content_status(cx);
                    }
                    Ok(Err(err)) => this.service.content_action_error = Some(err),
                    Err(panic) => {
                        this.service.content_action_error =
                            Some(format!("the content-index action task stopped: {panic}"));
                    }
                }
                this.repaint_shell_and_overlays(cx);
            })
            .ok();
        })
        .detach();
    }
}

/// The i18n key that explains a scope switch failure in the user's language.
///
/// The switch reports *what* went wrong; the window decides how to say it. The technical detail
/// behind the failure belongs in the log, not on the page.
fn scope_switch_failure_key(reason: ScopeSwitchFailure) -> &'static str {
    match reason {
        ScopeSwitchFailure::RegistrationUnreadable => "menu_search_scope_unreadable",
        ScopeSwitchFailure::ChangeFailed => "menu_search_scope_change_failed",
        ScopeSwitchFailure::NotInEffect => "menu_search_scope_not_in_effect",
        ScopeSwitchFailure::Restricted => "menu_search_scope_restricted",
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::{
        status_failure_is_terminal, status_refresh_decision, volume_status_visibility_changed,
        StatusRefreshDecision,
    };
    use crate::shell::i18n::I18nManager;
    use cairn_protocol::status::{VolumeState, VolumeStatusSummary};

    #[test]
    fn a_status_poll_reports_a_failure_only_after_consecutive_failures() {
        assert!(!status_failure_is_terminal(1));
        assert!(!status_failure_is_terminal(2));
        assert!(status_failure_is_terminal(3));
        assert!(status_failure_is_terminal(4));
    }

    fn volume(status: VolumeState, indexed_count: u64) -> VolumeStatusSummary {
        VolumeStatusSummary {
            drive_letter: 'C',
            volume_guid: "guid-c".to_string(),
            status,
            indexed_count,
            usn_enabled: true,
        }
    }

    #[test]
    fn a_growing_count_alone_does_not_repaint_the_window() {
        let previous = volume(VolumeState::CatchingUp(0), 8_192);
        let next = volume(VolumeState::CatchingUp(0), 16_384);
        assert!(
            !volume_status_visibility_changed(
                std::slice::from_ref(&previous),
                std::slice::from_ref(&next)
            ),
            "an indexed count that nothing renders must not schedule a frame"
        );

        let synced = volume(VolumeState::Synced, 16_384);
        assert!(
            volume_status_visibility_changed(
                std::slice::from_ref(&previous),
                std::slice::from_ref(&synced)
            ),
            "the visible volume state changing must repaint"
        );
        assert!(
            volume_status_visibility_changed(&[], std::slice::from_ref(&synced)),
            "a volume appearing must repaint"
        );
    }

    #[test]
    fn a_growing_index_is_never_queried_for_a_partial_result_set() {
        assert_eq!(
            status_refresh_decision(true, false, true),
            StatusRefreshDecision::Wait,
            "a growing index has no final total; querying it only burns the indexer's CPU"
        );
        assert_eq!(
            status_refresh_decision(true, false, false),
            StatusRefreshDecision::Wait,
            "a hidden table waits without querying, exactly like a visible one"
        );
        assert_eq!(
            status_refresh_decision(true, true, true),
            StatusRefreshDecision::Wait,
            "a second query would cancel the answer already coming"
        );
    }

    #[test]
    fn the_result_set_is_taken_once_after_the_index_settles() {
        assert_eq!(
            status_refresh_decision(false, false, true),
            StatusRefreshDecision::RefreshAndStop,
            "the final batch is only in the index, not in the table"
        );
        assert_eq!(
            status_refresh_decision(false, true, true),
            StatusRefreshDecision::Wait,
            "stopping while a query is in flight would leave its answer unapplied"
        );
        assert_eq!(
            status_refresh_decision(false, false, false),
            StatusRefreshDecision::Stop,
            "a hidden table has nothing to refresh when loading ends"
        );
    }

    /// The copy that called a working local index a dead background service, and sent the user to
    /// the installer for a missing service, was the visible half of the bug. It cannot come back.
    #[test]
    fn unavailable_copy_names_recovery_without_a_blocking_service_card() {
        for locale in ["zh-CN", "en-US"] {
            let i18n = I18nManager::new(locale);
            for key in [
                "service_not_running_title",
                "service_not_running_desc",
                "service_not_installed_title",
                "service_not_installed_desc",
            ] {
                let text = i18n.get_text(key);
                assert!(!text.trim().is_empty() && text != key, "{key} in {locale}");
                for forbidden in ["后台服务未运行", "isn’t running", "isn't running"] {
                    assert!(
                        !text.contains(forbidden),
                        "{key} ({locale}) still reports a dead background service: {text}"
                    );
                }
            }
        }
    }
}
