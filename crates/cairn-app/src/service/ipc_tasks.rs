// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! IPC background task orchestration: connection monitoring, explicit service start, and scoped
//! configuration submission.

use crate::app::CairnApp;
use crate::service::config_submission::{
    self, IndexScopeChange, SubmissionOutcome, SubmissionState,
};
use gpui_kit::gpui::{AsyncApp, Context};
use std::sync::OnceLock;

/// Why the window cannot show results, so the copy can name the condition and the recovery (SVC-03,
/// SVC-04). Derived from the client error type, never from a string match on a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceIssue {
    /// The service is installed but not running (or its pipe is not answering yet).
    NotRunning,
    /// The service is not registered; the user has to run the installer.
    NotInstalled,
    /// The service answered, but the protocol versions do not match.
    ProtocolIncompatible,
    /// The service refused this Windows account.
    Unauthorized,
}

#[derive(Clone, Copy)]
enum ContentAction {
    Set(bool),
    Cancel,
    Clear,
}

impl ServiceIssue {
    pub fn from_client_error(error: &cairn_client::client::ClientError) -> Self {
        if error.is_protocol_incompatible() {
            ServiceIssue::ProtocolIncompatible
        } else if error.is_unauthorized() {
            ServiceIssue::Unauthorized
        } else {
            ServiceIssue::NotRunning
        }
    }

    /// Registry entry naming the announcement this issue makes in the result region (NFR-11). Each
    /// issue gets its own entry because each names a different condition.
    pub const fn accessibility_key(self) -> &'static str {
        match self {
            ServiceIssue::NotRunning => "service-issue-not-running",
            ServiceIssue::NotInstalled => "service-issue-not-installed",
            ServiceIssue::ProtocolIncompatible => "service-issue-incompatible",
            ServiceIssue::Unauthorized => "service-issue-unauthorized",
        }
    }
}

/// One silent SCM start per connection-loss episode, and only when the service exists and is
/// merely stopped. A missing service must show install guidance, not request elevation.
fn should_try_silent_start(issue: ServiceIssue, already_attempted: bool) -> bool {
    issue == ServiceIssue::NotRunning && !already_attempted
}

/// What the status poll does after one answer, and whether it keeps polling.
///
/// The growing index and the final page are two different jobs. While volumes are still loading,
/// only a count that grew is worth another query, and a query already in flight is newer than the
/// one this tick would start. Once loading ends the table must be asked one last time even though
/// nothing will load afterwards, or the final batch stays missing; if an answer is still on its
/// way, wait for it instead of stopping with stale rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusFollowUp {
    Refresh,
    RefreshAndStop,
    Wait,
}

fn status_follow_up(now_loading: bool, searching: bool, grew: bool) -> StatusFollowUp {
    if searching {
        return StatusFollowUp::Wait;
    }
    if now_loading {
        if grew {
            StatusFollowUp::Refresh
        } else {
            StatusFollowUp::Wait
        }
    } else {
        StatusFollowUp::RefreshAndStop
    }
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
                            this.service.status.volumes = envelope.volumes;
                            this.start_status_refresh(cx);
                            // A connection probe must not replace the location the
                            // person is reading; the bin does not need the index.
                            if this.recycle_bin.is_none() {
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
        let runtime = get_tokio_runtime();
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            loop {
                // GPUI's executor owns this task; `tokio::time::sleep` here runs with no reactor.
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                let (tx, rx) = tokio::sync::oneshot::channel();
                runtime.spawn(async move {
                    let result = match crate::service::ipc_channel::connect().await {
                        Ok(mut client) => client.get_status().await,
                        Err(error) => Err(error),
                    };
                    let _ = tx.send(result);
                });

                let outcome = rx.await;
                let keep_going = this
                    .update(cx, |this: &mut CairnApp, cx| {
                        if !this.service.is_service_connected {
                            return false;
                        }
                        match outcome {
                            Ok(Ok(envelope)) => {
                                let previous: Vec<(char, u64)> = this
                                    .service
                                    .status
                                    .volumes
                                    .iter()
                                    .map(|volume| (volume.drive_letter, volume.indexed_count))
                                    .collect();
                                this.service.status.volumes = envelope.volumes;
                                let now_loading = this.service.status.is_any_loading();
                                let grew = this.service.status.volumes.iter().any(|volume| {
                                    previous.iter().any(|(drive, count)| {
                                        *drive == volume.drive_letter
                                            && volume.indexed_count > *count
                                    })
                                });
                                // A local scan appends rows while it walks. Refresh the table
                                // without clearing it when the count grows; once the walk ends,
                                // wait for any in-flight answer, then take one last fresh page so
                                // the final rows are included.
                                cx.notify();
                                match status_follow_up(now_loading, this.query.is_searching, grew) {
                                    StatusFollowUp::Refresh => {
                                        this.refresh_results(cx);
                                        true
                                    }
                                    StatusFollowUp::Wait => true,
                                    StatusFollowUp::RefreshAndStop => {
                                        this.refresh_results(cx);
                                        false
                                    }
                                }
                            }
                            _ => false,
                        }
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
            let _ = this.update(cx, |this: &mut CairnApp, _| {
                this.service.is_refreshing_status = false;
            });
        })
        .detach();
    }

    fn service_issue_after_probe(&mut self) -> ServiceIssue {
        self.service.service_installed = crate::shell::os::service_is_installed();
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
            if crate::shell::os::try_start_installed_service().is_ok() {
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
                        this.service.status.volumes = envelope.volumes;
                        this.start_status_refresh(cx);
                        if this.recycle_bin.is_none() {
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
        self.service.service_installed = crate::shell::os::service_is_installed();
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
                    let launched =
                        tokio::task::spawn_blocking(crate::shell::os::start_service_elevated).await;
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
                    this.service.status.volumes = envelope.volumes;
                    this.start_status_refresh(cx);
                    if this.recycle_bin.is_none() {
                        this.trigger_search(cx);
                    }
                    this.refresh_content_status(cx);
                } else if let Some(child) = this.service.user_indexer.as_mut() {
                    // The explicit start was cancelled or refused while a local index already
                    // serves the window: stay usable, and leave the service for the next launch.
                    let _ = child.try_wait();
                } else {
                    this.service.is_service_connected = false;
                    this.service.service_installed = crate::shell::os::service_is_installed();
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

    pub fn cancel_content_index(&mut self, cx: &mut Context<Self>) {
        self.run_content_action(ContentAction::Cancel, cx);
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
                                    .unwrap_or_else(|| "the service rejected the change".to_string())),
                            }
                        }
                        ContentAction::Cancel => {
                            let mut client = cairn_client::client::CairnClient::connect()
                                .await
                                .map_err(|err| err.to_string())?;
                            match client.cancel_content_index().await {
                                Ok(envelope) => match envelope.payload {
                                    cairn_protocol::message::ResponsePayload::CancelContentIndexAck {
                                        success: true,
                                    } => Ok(()),
                                    _ => Err("the service did not cancel the content index".to_string()),
                                },
                                Err(err) => Err(err.to_string()),
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

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::{should_try_silent_start, status_follow_up, ServiceIssue, StatusFollowUp};
    use crate::shell::i18n::I18nManager;

    #[test]
    fn a_stopped_installed_service_gets_one_silent_start_attempt() {
        assert!(should_try_silent_start(ServiceIssue::NotRunning, false));
        assert!(
            !should_try_silent_start(ServiceIssue::NotRunning, true),
            "a second silent attempt would loop without a user action"
        );
    }

    #[test]
    fn a_missing_or_incompatible_service_is_not_started_silently() {
        for issue in [
            ServiceIssue::NotInstalled,
            ServiceIssue::ProtocolIncompatible,
            ServiceIssue::Unauthorized,
        ] {
            assert!(!should_try_silent_start(issue, false), "{issue:?}");
        }
    }

    #[test]
    fn a_growing_index_refreshes_only_when_no_query_is_in_flight() {
        assert_eq!(
            status_follow_up(true, false, true),
            StatusFollowUp::Refresh,
            "rows appended during the scan must reach the table"
        );
        assert_eq!(
            status_follow_up(true, true, true),
            StatusFollowUp::Wait,
            "a second query would cancel the answer already coming"
        );
        assert_eq!(
            status_follow_up(true, false, false),
            StatusFollowUp::Wait,
            "no new rows means no reason to sort the whole index again"
        );
    }

    #[test]
    fn the_last_page_is_taken_after_the_scan_ends() {
        assert_eq!(
            status_follow_up(false, false, false),
            StatusFollowUp::RefreshAndStop,
            "the final batch is only in the index, not in the table"
        );
        assert_eq!(
            status_follow_up(false, true, false),
            StatusFollowUp::Wait,
            "stopping while a query is in flight would leave its answer unapplied"
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
            assert!(
                !i18n.get_text("menu_index_source_local").trim().is_empty(),
                "the local mode has no name in {locale}"
            );
        }
    }
}
