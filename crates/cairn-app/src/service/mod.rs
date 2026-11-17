// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Service connection: the client pipe channel, background IPC tasks, and
//! configuration submission to the indexing service.

pub(crate) mod config_submission;
pub(crate) mod elevation;
pub(crate) mod ipc_tasks;
pub(crate) mod service_control;
pub(crate) mod service_issue;

use cairn_client::client::ClientError;
use service_issue::ServiceIssue;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LoadState {
    #[default]
    Idle,
    Loading,
    Failed(String),
}

impl LoadState {
    pub fn is_loading(&self) -> bool {
        matches!(self, Self::Loading)
    }

    pub fn is_failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }

    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Failed(err) => Some(err.as_str()),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SubsystemState<S> {
    pub value: S,
    pub load: LoadState,
    pub action_pending: bool,
    pub action_error: Option<String>,
}

impl<S> SubsystemState<S> {
    pub fn is_loading(&self) -> bool {
        self.load.is_loading()
    }

    pub fn load_error(&self) -> Option<&str> {
        self.load.error()
    }

    pub fn has_failure(&self) -> bool {
        self.load.is_failed() || self.action_error.is_some()
    }

    pub fn begin_load(&mut self) -> bool {
        if self.is_loading() {
            return false;
        }
        self.load = LoadState::Loading;
        true
    }

    pub fn finish_load(&mut self, value: S) {
        self.value = value;
        self.load = LoadState::Idle;
        self.action_error = None;
    }

    pub fn fail_load(&mut self, error: impl Into<String>) {
        self.load = LoadState::Failed(error.into());
    }

    pub fn begin_action(&mut self) -> bool {
        if self.action_pending {
            return false;
        }
        self.action_pending = true;
        self.action_error = None;
        true
    }

    pub fn finish_action_ok(&mut self) {
        self.action_pending = false;
        self.action_error = None;
    }

    pub fn finish_action_err(&mut self, error: impl Into<String>) {
        self.action_pending = false;
        self.action_error = Some(error.into());
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ServiceConnection {
    Connected {
        installed: bool,
        probing: bool,
        issue: Option<ServiceIssue>,
        control_pending: bool,
    },
    Disconnected {
        installed: bool,
        probing: bool,
        silent_start_attempted: bool,
        issue: ServiceIssue,
        control_pending: bool,
    },
}

impl ServiceConnection {
    pub fn initial() -> Self {
        Self::Connected {
            installed: false,
            probing: true,
            issue: None,
            control_pending: false,
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }

    pub fn is_probing(&self) -> bool {
        match self {
            Self::Connected { probing, .. } | Self::Disconnected { probing, .. } => *probing,
        }
    }

    pub fn is_installed(&self) -> bool {
        match self {
            Self::Connected { installed, .. } | Self::Disconnected { installed, .. } => *installed,
        }
    }

    pub fn is_control_pending(&self) -> bool {
        match self {
            Self::Connected {
                control_pending, ..
            }
            | Self::Disconnected {
                control_pending, ..
            } => *control_pending,
        }
    }

    pub fn silent_start_attempted(&self) -> bool {
        match self {
            Self::Connected { .. } => false,
            Self::Disconnected {
                silent_start_attempted,
                ..
            } => *silent_start_attempted,
        }
    }

    pub fn issue(&self) -> Option<ServiceIssue> {
        match self {
            Self::Connected { issue, .. } => *issue,
            Self::Disconnected { issue, .. } => Some(*issue),
        }
    }

    pub fn set_probing(&mut self, next: bool) {
        match self {
            Self::Connected { probing, .. } | Self::Disconnected { probing, .. } => {
                *probing = next;
            }
        }
    }

    pub fn set_control_pending(&mut self, next: bool) {
        match self {
            Self::Connected {
                control_pending, ..
            }
            | Self::Disconnected {
                control_pending, ..
            } => {
                *control_pending = next;
            }
        }
    }

    pub fn mark_connected(&mut self) {
        *self = Self::Connected {
            installed: true,
            probing: false,
            issue: None,
            control_pending: self.is_control_pending(),
        };
    }

    pub fn mark_disconnected(
        &mut self,
        installed: bool,
        issue: ServiceIssue,
        probing: bool,
        silent_start_attempted: bool,
    ) {
        *self = Self::Disconnected {
            installed,
            probing,
            silent_start_attempted,
            issue,
            control_pending: self.is_control_pending(),
        };
    }

    pub fn mark_lost_running(&mut self) {
        self.mark_disconnected(
            self.is_installed(),
            ServiceIssue::NotRunning,
            false,
            self.silent_start_attempted(),
        );
    }

    pub fn note_client_error(&mut self, error: &ClientError) {
        let issue = ServiceIssue::from_client_error(error);
        if matches!(error, ClientError::Timeout) {
            *self = Self::Connected {
                installed: self.is_installed(),
                probing: self.is_probing(),
                issue: Some(issue),
                control_pending: self.is_control_pending(),
            };
        } else {
            self.mark_disconnected(
                self.is_installed(),
                issue,
                false,
                self.silent_start_attempted(),
            );
        }
    }
}
