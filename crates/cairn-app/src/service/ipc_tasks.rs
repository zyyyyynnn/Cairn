// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! IPC background task orchestration: connection monitoring, explicit service start, and scoped
//! configuration submission.

use crate::app::CairnApp;
use crate::service::config_submission::{
    self, IndexScopeChange, SubmissionOutcome, SubmissionState,
};
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
/// Connection attempts keep the status poll alive across an SCM auto-restart: the retry loop
/// sleeps 250 ms per attempt, so 40 attempts cover about ten seconds (the restart delay is 5 s).
const STATUS_MAX_CONNECT_FAILURES: u32 = 40;

/// Which wait a failure belongs to: reconnecting to a restarting service keeps the ten-second
/// window, while a service that answers but fails its request is terminal after three tries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusFailure {
    Connect,
    Request,
}

fn failure_limit(kind: StatusFailure) -> u32 {
    match kind {
        StatusFailure::Connect => STATUS_MAX_CONNECT_FAILURES,
        StatusFailure::Request => STATUS_MAX_CONSECUTIVE_FAILURES,
    }
}

fn failure_is_terminal(kind: StatusFailure, consecutive_failures: u32) -> bool {
    consecutive_failures >= failure_limit(kind)
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
        let runtime = get_tokio_runtime();
        let session = self.service.session.clone();
        self.service.is_probing_service = true;
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            crate::shell::perf::mark("probe start");
            runtime.spawn(async move {
                let started = std::time::Instant::now();
                let res = session.get_status().await;
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
                            this.apply_service_connected(envelope.volumes, cx);
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
    /// the capsule would freeze at "synchronizing".
    pub(crate) fn start_status_refresh(&mut self, cx: &mut Context<Self>) {
        if self.service.is_refreshing_status || !self.service.status.is_any_loading() {
            return;
        }
        self.service.is_refreshing_status = true;
        let runtime = get_tokio_runtime();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let status_task = runtime.spawn(async move {
            let mut client: Option<cairn_client::client::CairnClient> = None;
            let mut consecutive_failures = 0u32;
            loop {
                if client.is_none() {
                    match cairn_client::client::CairnClient::connect().await {
                        Ok(connected) => {
                            client = Some(connected);
                            consecutive_failures = 0;
                        }
                        Err(error) => {
                            consecutive_failures += 1;
                            if failure_is_terminal(StatusFailure::Connect, consecutive_failures) {
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
                        if failure_is_terminal(StatusFailure::Request, consecutive_failures) {
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

    fn apply_service_connected(
        &mut self,
        volumes: Vec<VolumeStatusSummary>,
        cx: &mut Context<Self>,
    ) {
        self.service.is_service_connected = true;
        self.service.service_issue = None;
        self.service.service_installed = true;
        self.service.has_attempted_silent_start = false;
        self.service.status.update_volumes(volumes);
        self.start_status_refresh(cx);
        // A connection probe must not replace the location the person is reading;
        // the bin does not need the index.
        if !self.recycle_bin.is_open() {
            self.trigger_search(cx);
        }
        self.refresh_content_status(cx);
    }

    /// Records a failed probe. Checks SCM registration off the main UI thread, and when the
    /// service is installed but stopped, attempts one silent SCM start before showing recovery UI.
    fn handle_probe_failure(&mut self, issue: ServiceIssue, cx: &mut Context<Self>) {
        if issue != ServiceIssue::NotRunning {
            self.service.service_issue = Some(issue);
            cx.notify();
            return;
        }
        let try_silent = !self.service.has_attempted_silent_start;
        self.service.has_attempted_silent_start = true;
        self.service.is_probing_service = true;
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            get_tokio_runtime().spawn_blocking(move || {
                let installed = crate::service::service_control::service_is_installed();
                let resolved_issue = if installed {
                    ServiceIssue::NotRunning
                } else {
                    ServiceIssue::NotInstalled
                };
                let silent_started = should_try_silent_start(resolved_issue, !try_silent)
                    && crate::service::service_control::try_start_installed_service().is_ok();
                let _ = tx.send((installed, silent_started));
            });
            let (installed, silent_started) = rx.await.unwrap_or((false, false));
            let _ = this.update(cx, |this: &mut CairnApp, cx| {
                this.service.is_probing_service = false;
                this.service.service_installed = installed;
                if silent_started {
                    this.poll_service_after_start(cx, false);
                    return;
                }
                this.service.service_issue = Some(if installed {
                    ServiceIssue::NotRunning
                } else {
                    ServiceIssue::NotInstalled
                });
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
        self.service.is_probing_service = true;
        cx.notify();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            get_tokio_runtime().spawn_blocking(move || {
                let _ = tx.send(crate::service::service_control::service_is_installed());
            });
            let installed = rx.await.unwrap_or(false);
            let _ = this.update(cx, |this: &mut CairnApp, cx| {
                this.service.is_probing_service = false;
                this.service.service_installed = installed;
                if !installed {
                    this.service.service_issue = Some(ServiceIssue::NotInstalled);
                    cx.notify();
                    return;
                }
                this.poll_service_after_start(cx, true);
            });
        })
        .detach();
    }

    /// Polls the pipe after an explicit elevation or a silent SCM start. `elevate` is the only
    /// difference: both paths end in the same "is the pipe answering?" loop, so a service that is
    /// still Starting is not reported as offline on the very next frame.
    fn poll_service_after_start(&mut self, cx: &mut Context<Self>, elevate: bool) {
        self.service.is_probing_service = true;
        cx.notify();

        let runtime = get_tokio_runtime();
        let session = self.service.session.clone();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            runtime.spawn(async move {
                if elevate {
                    // UAC and the elevated process are blocking; keep them off the IPC runtime's
                    // worker threads.
                    let launched = tokio::task::spawn_blocking(
                        crate::service::service_control::start_service_elevated,
                    )
                    .await;
                    if !matches!(launched, Ok(Ok(()))) {
                        let installed = tokio::task::spawn_blocking(
                            crate::service::service_control::service_is_installed,
                        )
                        .await
                        .unwrap_or(false);
                        let _ = tx.send((
                            Err(cairn_client::client::ClientError::ConnectFailed(
                                std::io::Error::new(
                                    std::io::ErrorKind::PermissionDenied,
                                    "Service start was cancelled or refused",
                                ),
                            )),
                            installed,
                        ));
                        return;
                    }
                }
                // The service reports Running only after its data layout is secured; the pipe
                // becomes available at the same point. Connection attempts fail fast now, so this
                // loop owns the whole cold-start budget instead of inheriting a per-attempt wait.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
                while std::time::Instant::now() < deadline {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    if let Ok(status) = session.get_status().await {
                        let _ = tx.send((Ok(status), true));
                        return;
                    }
                }
                let installed = tokio::task::spawn_blocking(
                    crate::service::service_control::service_is_installed,
                )
                .await
                .unwrap_or(false);
                let _ = tx.send((
                    Err(cairn_client::client::ClientError::ConnectFailed(
                        std::io::Error::new(
                            std::io::ErrorKind::NotConnected,
                            "Service did not answer after starting",
                        ),
                    )),
                    installed,
                ));
            });

            let outcome = rx.await;
            let _ = this.update(cx, |this: &mut CairnApp, cx| {
                this.service.is_probing_service = false;
                match outcome {
                    Ok((Ok(envelope), _)) => {
                        this.apply_service_connected(envelope.volumes, cx);
                    }
                    Ok((Err(_), installed)) => {
                        this.service.is_service_connected = false;
                        this.service.service_installed = installed;
                        this.service.service_issue = Some(if installed {
                            ServiceIssue::NotRunning
                        } else {
                            ServiceIssue::NotInstalled
                        });
                    }
                    Err(_) => {
                        this.service.is_service_connected = false;
                        this.service.service_issue = Some(ServiceIssue::NotRunning);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Reads the service's content-index lifecycle. While a backfill is running, one delayed
    /// follow-up keeps the progress fresh without a permanent polling loop.
    pub fn refresh_content_status(&mut self, cx: &mut Context<Self>) {
        if self.service.content_status_loading {
            return;
        }
        self.service.content_status_loading = true;
        let session = self.service.session.clone();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            get_tokio_runtime().spawn(async move {
                let _ = tx.send(session.get_content_index_status().await);
            });

            let result = rx.await;
            this.update(cx, |this: &mut CairnApp, cx| {
                this.service.content_status_loading = false;
                match result {
                    Ok(Ok(envelope)) => {
                        this.service.content_status_error = None;
                        this.service.content_action_error = None;
                        if let cairn_protocol::message::ResponsePayload::ContentIndexStatus {
                            status,
                        } = envelope.payload
                        {
                            this.service.content_status = status;
                        } else {
                            tracing::warn!(
                                "content index status: the service answered with a different payload"
                            );
                            this.service.content_status_error =
                                Some("the service answered with a different payload".to_string());
                        }
                    }
                    Ok(Err(err)) => {
                        tracing::warn!("content index status read failed: {err}");
                        this.service.content_status_error = Some(err.to_string());
                    }
                    Err(panic) => {
                        tracing::warn!("content index status task stopped: {panic}");
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
        if self.service.content_action_pending {
            return;
        }
        self.service.content_action_pending = true;
        self.service.content_action_error = None;
        self.repaint_shell_and_overlays(cx);

        let session = self.service.session.clone();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            get_tokio_runtime().spawn(async move {
                let outcome: Result<(), String> = async {
                    match action {
                        ContentAction::Set(enabled) => {
                            let state = config_submission::submit_index_scope(
                                &session,
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
                        ContentAction::Clear => match session.clear_content_index().await {
                            Ok(envelope) => match envelope.payload {
                                cairn_protocol::message::ResponsePayload::ClearContentIndexAck {
                                    success: true,
                                } => Ok(()),
                                _ => Err("the service did not accept the clear".to_string()),
                            },
                            Err(err) => Err(err.to_string()),
                        },
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
                    Ok(Err(err)) => {
                        tracing::warn!("content index action failed: {err}");
                        this.service.content_action_error = Some(err);
                    }
                    Err(panic) => {
                        tracing::warn!("content index action task stopped: {panic}");
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

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::{
        failure_is_terminal, status_refresh_decision, volume_status_visibility_changed,
        StatusFailure, StatusRefreshDecision,
    };
    use cairn_protocol::status::{VolumeState, VolumeStatusSummary};

    #[test]
    fn each_status_failure_kind_has_its_own_terminal_window() {
        assert!(!failure_is_terminal(StatusFailure::Request, 2));
        assert!(failure_is_terminal(StatusFailure::Request, 3));
        assert!(!failure_is_terminal(StatusFailure::Connect, 39));
        assert!(failure_is_terminal(StatusFailure::Connect, 40));
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
}
