// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! IPC background task orchestration: connection monitoring, explicit service start, and scoped
//! configuration submission.

use crate::app::CairnApp;
use crate::search::ResultSetChange;
use crate::service::config_submission::{
    self, IndexScopeChange, SubmissionOutcome, SubmissionState,
};
use crate::service::service_issue::{should_try_silent_start, ServiceIssue};
use cairn_client::client::ClientError;
use cairn_protocol::message::{Envelope, ResponsePayload, SourceId};
use cairn_protocol::status::{
    ContentIndexState, SourceKind, SourceStatusSummary, VolumeStatusSummary,
};
use gpui_kit::gpui::{AsyncApp, Context};
use std::sync::OnceLock;
use std::time::Duration;

pub(crate) fn source_id_for_drive(
    sources: &[SourceStatusSummary],
    drive: char,
) -> Option<SourceId> {
    let drive_upper = drive.to_ascii_uppercase();
    sources
        .iter()
        .find(|source| {
            source.kind == SourceKind::NtfsVolume
                && source
                    .root_path
                    .chars()
                    .next()
                    .is_some_and(|ch| ch.to_ascii_uppercase() == drive_upper)
        })
        .map(|source| source.source_id)
        .or_else(|| SourceId::for_drive(drive_upper))
}

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
const SUBSYSTEM_BACKFILL_POLL_INTERVAL: Duration = Duration::from_millis(700);
const STATUS_MAX_CONSECUTIVE_FAILURES: u32 = 3;
/// 40 x 250 ms covers ~10 s across SCM's 5 s service restart delay.
const STATUS_MAX_CONNECT_FAILURES: u32 = 40;

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

/// Counts alone are not rendered while volumes load; only state/identity/presence transitions need a repaint.
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

async fn wait_for_service_pipe(
    session: &cairn_client::ServiceSession,
) -> (Result<Vec<VolumeStatusSummary>, ClientError>, bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Ok(envelope) = session.get_status().await {
            return (Ok(envelope.volumes), true);
        }
    }
    let installed =
        tokio::task::spawn_blocking(crate::service::service_control::service_is_installed)
            .await
            .unwrap_or(false);
    (
        Err(ClientError::ConnectFailed(std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            "Service did not answer after starting",
        ))),
        installed,
    )
}

fn finish_subsystem_load<S>(
    state: &mut crate::service::SubsystemState<S>,
    result: Option<Result<Envelope<ResponsePayload>, ClientError>>,
    task_label: &str,
    extract: impl FnOnce(ResponsePayload) -> Result<S, String>,
) -> bool {
    match result {
        Some(Ok(envelope)) => match envelope.payload {
            ResponsePayload::Error { code, message } => {
                tracing::warn!(
                    task = task_label,
                    code,
                    error = %message,
                    "service request failed"
                );
                state.fail_load(message);
                false
            }
            payload => match extract(payload) {
                Ok(value) => {
                    state.finish_load(value);
                    true
                }
                Err(msg) => {
                    tracing::warn!(task = task_label, error = %msg, "service request failed");
                    state.fail_load(msg);
                    false
                }
            },
        },
        Some(Err(err)) => {
            tracing::warn!(task = task_label, error = %err, "service request failed");
            state.fail_load(err.to_string());
            false
        }
        None => {
            tracing::warn!(task = task_label, "service request stopped");
            state.fail_load(format!("{task_label} task stopped"));
            false
        }
    }
}

impl CairnApp {
    /// Probes service reachability without requesting UAC elevation.
    pub fn check_service_connection(&mut self, cx: &mut Context<Self>) {
        let runtime = get_tokio_runtime();
        let session = self.service.session.clone();
        self.service.connection.set_probing(true);
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
                this.service.connection.set_probing(false);
                match outcome {
                    Ok(Ok(envelope)) => {
                        this.apply_service_connected(envelope.volumes, cx);
                    }
                    Ok(Err(e)) => {
                        let issue = ServiceIssue::from_client_error(&e);
                        let installed = this.service.connection.is_installed();
                        let attempted = this.service.connection.silent_start_attempted();
                        this.service
                            .connection
                            .mark_disconnected(installed, issue, false, attempted);
                        this.handle_probe_failure(issue, cx);
                    }
                    Err(e) => {
                        tracing::warn!("Cairn connection probe task lost: {e}");
                        this.service.connection.mark_lost_running();
                        this.handle_probe_failure(ServiceIssue::NotRunning, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Polls the service while any volume is building or catching up.
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
                        if !this.service.connection.is_connected() {
                            return false;
                        }
                        match outcome {
                            Ok(envelope) => {
                                let visible_change = volume_status_visibility_changed(
                                    &this.service.status.volumes,
                                    &envelope.volumes,
                                );
                                this.service.status.update_volumes(envelope.volumes);
                                let now_loading = this.service.status.is_any_loading();
                                let table_visible =
                                    !this.recycle_bin.is_open() && this.drive_view.is_none();
                                if visible_change {
                                    cx.notify();
                                }
                                match status_refresh_decision(
                                    now_loading,
                                    this.query.is_searching(),
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
                                this.service.connection.note_client_error(&error);
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
        self.service.connection.mark_connected();
        self.service.status.update_volumes(volumes);
        self.start_status_refresh(cx);
        if !self.recycle_bin.is_open() {
            self.request_result_set(ResultSetChange::Probe, cx);
        }
        self.refresh_content_status(cx);
        self.refresh_property_index_status(cx);
        self.refresh_index_journal_status(cx);
        self.refresh_sources(cx);
    }

    /// Checks SCM registration off the main UI thread and attempts one silent start when stopped.
    fn handle_probe_failure(&mut self, issue: ServiceIssue, cx: &mut Context<Self>) {
        if issue != ServiceIssue::NotRunning {
            let installed = self.service.connection.is_installed();
            let probing = self.service.connection.is_probing();
            let attempted = self.service.connection.silent_start_attempted();
            self.service
                .connection
                .mark_disconnected(installed, issue, probing, attempted);
            cx.notify();
            return;
        }
        let try_silent = !self.service.connection.silent_start_attempted();
        let installed = self.service.connection.is_installed();
        self.service
            .connection
            .mark_disconnected(installed, ServiceIssue::NotRunning, true, true);
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
                this.service.connection.set_probing(false);
                if silent_started {
                    this.poll_service_until_ready(cx);
                    return;
                }
                let issue = if installed {
                    ServiceIssue::NotRunning
                } else {
                    ServiceIssue::NotInstalled
                };
                this.service
                    .connection
                    .mark_disconnected(installed, issue, false, true);
                cx.notify();
            });
        })
        .detach();
    }

    pub fn retry_service_connection(&mut self, cx: &mut Context<Self>) {
        if self.service.connection.is_probing() {
            return;
        }
        self.check_service_connection(cx);
        cx.notify();
    }

    /// Asks SCM to start the installed service through one UAC elevation, then polls for its pipe.
    pub fn start_service(&mut self, cx: &mut Context<Self>) {
        if self.service.connection.is_probing() {
            return;
        }
        self.service.connection.set_probing(true);
        cx.notify();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            get_tokio_runtime().spawn_blocking(move || {
                let _ = tx.send(crate::service::service_control::service_is_installed());
            });
            let installed = rx.await.unwrap_or(false);
            let _ = this.update(cx, |this: &mut CairnApp, cx| {
                if !installed {
                    let attempted = this.service.connection.silent_start_attempted();
                    this.service.connection.mark_disconnected(
                        false,
                        ServiceIssue::NotInstalled,
                        false,
                        attempted,
                    );
                    cx.notify();
                    return;
                }
                this.launch_elevated_service_then_poll(cx);
            });
        })
        .detach();
    }

    fn launch_elevated_service_then_poll(&mut self, cx: &mut Context<Self>) {
        self.spawn_service_connection_task(cx, move |session| async move {
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
                return (
                    Err(ClientError::ConnectFailed(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "Service start was cancelled or refused",
                    ))),
                    installed,
                );
            }
            wait_for_service_pipe(&session).await
        });
    }

    fn poll_service_until_ready(&mut self, cx: &mut Context<Self>) {
        self.spawn_service_connection_task(cx, |session| async move {
            wait_for_service_pipe(&session).await
        });
    }

    fn spawn_service_connection_task<F, Fut>(&mut self, cx: &mut Context<Self>, work: F)
    where
        F: FnOnce(cairn_client::ServiceSession) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = (Result<Vec<VolumeStatusSummary>, ClientError>, bool)>
            + Send
            + 'static,
    {
        self.service.connection.set_probing(true);
        cx.notify();

        let runtime = get_tokio_runtime();
        let session = self.service.session.clone();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            runtime.spawn(async move {
                let _ = tx.send(work(session).await);
            });

            let outcome = rx.await;
            let _ = this.update(cx, |this: &mut CairnApp, cx| {
                this.service.connection.set_probing(false);
                match outcome {
                    Ok((Ok(volumes), _)) => {
                        this.apply_service_connected(volumes, cx);
                    }
                    Ok((Err(_), installed)) => {
                        let issue = if installed {
                            ServiceIssue::NotRunning
                        } else {
                            ServiceIssue::NotInstalled
                        };
                        let attempted = this.service.connection.silent_start_attempted();
                        this.service
                            .connection
                            .mark_disconnected(installed, issue, false, attempted);
                    }
                    Err(_) => {
                        this.service.connection.mark_lost_running();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn spawn_service_task<T, Fut, OnDone>(
        &mut self,
        cx: &mut Context<Self>,
        work: impl FnOnce(cairn_client::ServiceSession) -> Fut + Send + 'static,
        on_done: OnDone,
    ) where
        T: Send + 'static,
        Fut: std::future::Future<Output = T> + Send + 'static,
        OnDone: FnOnce(&mut CairnApp, Option<T>, &mut Context<Self>) + 'static,
    {
        let session = self.service.session.clone();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let (tx, rx) = tokio::sync::oneshot::channel();
            get_tokio_runtime().spawn(async move {
                let _ = tx.send(work(session).await);
            });
            let result = rx.await.ok();
            let _ = this.update(cx, |this: &mut CairnApp, cx| {
                on_done(this, result, cx);
            });
        })
        .detach();
    }

    fn schedule_delayed_refresh(
        &mut self,
        cx: &mut Context<Self>,
        refresh: fn(&mut CairnApp, &mut Context<Self>),
    ) {
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            cx.background_executor()
                .timer(SUBSYSTEM_BACKFILL_POLL_INTERVAL)
                .await;
            let _ = this.update(cx, |this: &mut CairnApp, cx| refresh(this, cx));
        })
        .detach();
    }

    pub fn refresh_content_status(&mut self, cx: &mut Context<Self>) {
        if !self.service.content.begin_load() {
            return;
        }
        self.spawn_service_task(
            cx,
            |session| async move { session.get_content_index_status().await },
            |this, result, cx| {
                let ok = finish_subsystem_load(
                    &mut this.service.content,
                    result,
                    "content-index status",
                    |payload| match payload {
                        ResponsePayload::ContentIndexStatus { status } => Ok(status),
                        _ => Err("the service answered with a different payload".to_string()),
                    },
                );
                let follow_up = ok
                    && matches!(
                        this.service.content.value.state,
                        ContentIndexState::Backfilling { .. }
                    );
                this.repaint_shell_and_overlays(cx);
                if follow_up {
                    this.schedule_delayed_refresh(cx, Self::refresh_content_status);
                }
            },
        );
    }

    pub fn set_content_index_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.run_content_action(ContentAction::Set(enabled), cx);
    }

    pub fn clear_content_index(&mut self, cx: &mut Context<Self>) {
        self.run_content_action(ContentAction::Clear, cx);
    }

    fn run_content_action(&mut self, action: ContentAction, cx: &mut Context<Self>) {
        if !self.service.content.begin_action() {
            return;
        }
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
                                ResponsePayload::ClearContentIndexAck { success: true } => Ok(()),
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
                match outcome {
                    Ok(Ok(())) => {
                        this.service.content.finish_action_ok();
                        this.refresh_content_status(cx);
                    }
                    Ok(Err(err)) => {
                        tracing::warn!("content index action failed: {err}");
                        this.service.content.finish_action_err(err);
                    }
                    Err(panic) => {
                        tracing::warn!("content index action task stopped: {panic}");
                        this.service.content.finish_action_err(format!(
                            "the content-index action task stopped: {panic}"
                        ));
                    }
                }
                this.repaint_shell_and_overlays(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Logs the technical failure and reports it through one localized title/body pair. Service
    /// messages are English protocol text, so they stay in the log instead of the shell.
    fn report_request_failure(
        &mut self,
        title_key: &str,
        body_key: &str,
        detail: impl std::fmt::Display,
        cx: &mut Context<Self>,
    ) {
        tracing::warn!(request = title_key, error = %detail, "service request failed");
        let title = self.i18n.get_text(title_key);
        let body = self.i18n.get_text(body_key);
        self.show_error_toast(title, body, cx);
    }

    pub fn refresh_property_index_status(&mut self, cx: &mut Context<Self>) {
        if !self.service.property.begin_load() {
            return;
        }
        self.spawn_service_task(
            cx,
            |session| async move { session.get_property_index_status().await },
            |this, result, cx| {
                let ok = finish_subsystem_load(
                    &mut this.service.property,
                    result,
                    "property index status",
                    |payload| match payload {
                        ResponsePayload::PropertyIndexStatus { status } => Ok(status),
                        _ => Err("unexpected property index status payload".to_string()),
                    },
                );
                let follow_up = ok
                    && matches!(
                        this.service.property.value.state,
                        ContentIndexState::Backfilling { .. }
                    );
                this.repaint_shell_and_overlays(cx);
                if follow_up {
                    this.schedule_delayed_refresh(cx, Self::refresh_property_index_status);
                }
            },
        );
    }

    pub fn set_property_index_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if !self.service.property.begin_action() {
            return;
        }
        self.repaint_shell_and_overlays(cx);
        self.spawn_service_task(
            cx,
            move |session| async move { session.set_property_index_enabled(enabled).await },
            |this, result, cx| {
                this.service.property.action_pending = false;
                let res = match result {
                    Some(Ok(envelope)) => match envelope.payload {
                        ResponsePayload::PropertyIndexStatus { status } => {
                            let backfill =
                                matches!(status.state, ContentIndexState::Backfilling { .. });
                            this.service.property.finish_load(status);
                            if backfill {
                                this.schedule_delayed_refresh(
                                    cx,
                                    Self::refresh_property_index_status,
                                );
                            }
                            Ok(())
                        }
                        ResponsePayload::Error { message, .. } => Err(message),
                        _ => Err("unexpected property index update response".to_string()),
                    },
                    Some(Err(err)) => Err(err.to_string()),
                    None => Err("property index task stopped".to_string()),
                };
                if let Err(err) = res {
                    this.report_request_failure(
                        "toast_property_index_failed_title",
                        "toast_property_index_failed_body",
                        err,
                        cx,
                    );
                }
                this.repaint_shell_and_overlays(cx);
            },
        );
    }

    pub fn refresh_index_journal_status(&mut self, cx: &mut Context<Self>) {
        if !self.service.journal.begin_load() {
            return;
        }
        self.spawn_service_task(
            cx,
            |session| async move { session.get_index_journal(0, 0).await },
            |this, result, cx| {
                finish_subsystem_load(
                    &mut this.service.journal,
                    result,
                    "index journal status",
                    |payload| match payload {
                        ResponsePayload::IndexJournalResult { status, .. }
                        | ResponsePayload::IndexJournalStatus { status } => Ok(status),
                        _ => Err("unexpected index journal payload".to_string()),
                    },
                );
                this.repaint_shell_and_overlays(cx);
            },
        );
    }

    pub fn refresh_index_journal_dialog(&mut self, cx: &mut Context<Self>) {
        const JOURNAL_DIALOG_LIMIT: usize = 200;

        if !self.service.journal_entries.begin_load() {
            return;
        }
        let initial_after = self
            .service
            .journal
            .value
            .newest_seq
            .saturating_sub(JOURNAL_DIALOG_LIMIT as u64);
        self.repaint_shell_and_overlays(cx);
        self.spawn_service_task(
            cx,
            move |session| async move {
                let first = session
                    .get_index_journal(initial_after, JOURNAL_DIALOG_LIMIT)
                    .await;
                let Ok(envelope) = first else {
                    return first;
                };
                if let ResponsePayload::IndexJournalResult { ref status, .. } = envelope.payload {
                    let latest_after = status
                        .newest_seq
                        .saturating_sub(JOURNAL_DIALOG_LIMIT as u64);
                    if latest_after > initial_after {
                        return session
                            .get_index_journal(latest_after, JOURNAL_DIALOG_LIMIT)
                            .await;
                    }
                }
                Ok(envelope)
            },
            |this, result, cx| {
                match result {
                    Some(Ok(envelope)) => match envelope.payload {
                        ResponsePayload::IndexJournalResult {
                            status,
                            mut entries,
                        } => {
                            entries.reverse();
                            this.service.journal.finish_load(status);
                            this.service.journal_entries.finish_load(entries);
                        }
                        ResponsePayload::Error { message, .. } => {
                            this.service.journal_entries.fail_load(message);
                        }
                        _ => {
                            this.service
                                .journal_entries
                                .fail_load("unexpected index journal payload");
                        }
                    },
                    Some(Err(err)) => {
                        this.service.journal_entries.fail_load(err.to_string());
                    }
                    None => {
                        this.service
                            .journal_entries
                            .fail_load("index journal task stopped");
                    }
                }
                this.repaint_shell_and_overlays(cx);
            },
        );
    }

    pub fn set_index_journal_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if !self.service.journal.begin_action() {
            return;
        }
        self.repaint_shell_and_overlays(cx);
        self.spawn_service_task(
            cx,
            move |session| async move { session.set_index_journal_enabled(enabled).await },
            |this, result, cx| {
                this.service.journal.action_pending = false;
                let res = match result {
                    Some(Ok(envelope)) => match envelope.payload {
                        ResponsePayload::IndexJournalStatus { status }
                        | ResponsePayload::IndexJournalResult { status, .. } => {
                            let is_enabled = status.enabled;
                            this.service.journal.finish_load(status);
                            if is_enabled {
                                this.refresh_index_journal_dialog(cx);
                            } else {
                                this.service.journal_entries.finish_load(Vec::new());
                            }
                            Ok(())
                        }
                        ResponsePayload::Error { message, .. } => Err(message),
                        _ => Err("unexpected index journal update response".to_string()),
                    },
                    Some(Err(err)) => Err(err.to_string()),
                    None => Err("index journal task stopped".to_string()),
                };
                if let Err(err) = res {
                    this.report_request_failure(
                        "toast_index_journal_failed_title",
                        "toast_index_journal_failed_body",
                        err,
                        cx,
                    );
                }
                this.repaint_shell_and_overlays(cx);
            },
        );
    }

    pub fn refresh_sources(&mut self, cx: &mut Context<Self>) {
        self.spawn_service_task(
            cx,
            |session| async move { session.get_sources().await },
            |this, result, cx| {
                if let Some(Ok(envelope)) = result {
                    if let ResponsePayload::SourcesResult { sources } = envelope.payload {
                        this.service.sources = sources;
                        cx.notify();
                    }
                }
            },
        );
    }

    pub fn rescan_all(&mut self, cx: &mut Context<Self>) {
        if self.service.connection.is_control_pending() {
            return;
        }
        self.service.connection.set_control_pending(true);
        self.repaint_shell_and_overlays(cx);
        self.spawn_service_task(
            cx,
            |session| async move { session.rescan(None).await },
            |this, result, cx| {
                this.service.connection.set_control_pending(false);
                let detail = match result {
                    Some(Ok(envelope)) => match envelope.payload {
                        ResponsePayload::RescanAck { success: true, .. } => {
                            let title = this.i18n.get_text("toast_rescan_started_title");
                            let body = this.i18n.get_text("toast_rescan_started_body");
                            this.show_info_toast(title, body, cx);
                            this.start_status_refresh(cx);
                            None
                        }
                        ResponsePayload::Error { code, message } => {
                            Some(format!("{code}: {message}"))
                        }
                        ResponsePayload::RescanAck { success: false, .. } => {
                            Some("the service refused the rescan request".to_string())
                        }
                        _ => Some("unexpected rescan response".to_string()),
                    },
                    Some(Err(err)) => Some(err.to_string()),
                    None => Some("rescan task stopped".to_string()),
                };
                if let Some(detail) = detail {
                    this.report_request_failure(
                        "toast_rescan_failed_title",
                        "toast_rescan_failed_body",
                        detail,
                        cx,
                    );
                }
                this.repaint_shell_and_overlays(cx);
            },
        );
    }

    pub fn rescan_drive(&mut self, drive: char, cx: &mut Context<Self>) {
        let drive_upper = drive.to_ascii_uppercase();
        let Some(source_id) = source_id_for_drive(&self.service.sources, drive_upper) else {
            self.report_request_failure(
                "toast_rescan_failed_title",
                "toast_rescan_source_missing_body",
                format!("no source registered for {drive_upper}:\\"),
                cx,
            );
            return;
        };
        let mut args = fluent_bundle::FluentArgs::new();
        args.set("drive", drive_upper.to_string());
        let label = self.i18n.get_text_with_args("drive_label", &args);
        self.rescan_source(source_id, label, cx);
    }

    pub fn rescan_source(&mut self, source_id: SourceId, label: String, cx: &mut Context<Self>) {
        if self.service.connection.is_control_pending() {
            return;
        }
        self.service.connection.set_control_pending(true);
        self.repaint_shell_and_overlays(cx);
        self.spawn_service_task(
            cx,
            move |session| async move { session.rescan_source(source_id).await },
            move |this, result, cx| {
                this.service.connection.set_control_pending(false);
                let detail = match result {
                    Some(Ok(envelope)) => match envelope.payload {
                        ResponsePayload::RescanSourceAck { success: true, .. } => {
                            let title = this.i18n.get_text("toast_rescan_started_title");
                            let mut args = fluent_bundle::FluentArgs::new();
                            args.set("source", label.as_str());
                            let body = this
                                .i18n
                                .get_text_with_args("toast_rescan_source_started_body", &args);
                            this.show_info_toast(title, body, cx);
                            this.start_status_refresh(cx);
                            this.refresh_sources(cx);
                            None
                        }
                        ResponsePayload::Error { code, message } => {
                            Some(format!("{code}: {message}"))
                        }
                        ResponsePayload::RescanSourceAck { success: false, .. } => {
                            Some("the service refused the source rescan".to_string())
                        }
                        _ => Some("unexpected source rescan response".to_string()),
                    },
                    Some(Err(err)) => Some(err.to_string()),
                    None => Some("source rescan task stopped".to_string()),
                };
                if let Some(detail) = detail {
                    this.report_request_failure(
                        "toast_rescan_failed_title",
                        "toast_rescan_failed_body",
                        detail,
                        cx,
                    );
                }
                this.repaint_shell_and_overlays(cx);
            },
        );
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

    #[test]
    fn source_id_resolves_drive_from_sources_or_drive_fallback() {
        use cairn_protocol::message::SourceId;
        use cairn_protocol::status::{SourceKind, SourceStatusSummary, VolumeState};

        let custom_c = SourceId(99);
        let sources = vec![
            SourceStatusSummary {
                source_id: SourceId(200),
                kind: SourceKind::Folder,
                root_path: r"C:\Projects".to_string(),
                volume_guid: String::new(),
                status: VolumeState::Synced,
                indexed_count: 10,
                live_watch_enabled: true,
            },
            SourceStatusSummary {
                source_id: custom_c,
                kind: SourceKind::NtfsVolume,
                root_path: r"C:\".to_string(),
                volume_guid: "guid-c".to_string(),
                status: VolumeState::Synced,
                indexed_count: 100,
                live_watch_enabled: true,
            },
        ];
        assert_eq!(super::source_id_for_drive(&sources, 'c'), Some(custom_c));
        assert_eq!(
            super::source_id_for_drive(&sources, 'd'),
            SourceId::for_drive('D')
        );
    }
}
