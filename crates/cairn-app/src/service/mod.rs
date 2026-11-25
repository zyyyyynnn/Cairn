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
    pub generation: u64,
    pub in_flight_load: Option<u64>,
    pub superseded_refresh: bool,
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

    pub fn begin_load(&mut self) -> Option<u64> {
        if self.is_loading() || self.action_pending {
            return None;
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.load = LoadState::Loading;
        self.in_flight_load = Some(generation);
        Some(generation)
    }

    pub fn finish_load(&mut self, value: S) {
        self.generation = self.generation.wrapping_add(1);
        self.in_flight_load = None;
        self.value = value;
        self.load = LoadState::Idle;
        self.action_error = None;
    }

    pub fn finish_load_if_current(&mut self, generation: u64, value: S) -> bool {
        if self.in_flight_load == Some(generation) {
            self.in_flight_load = None;
        }
        if self.generation != generation {
            return false;
        }
        self.value = value;
        self.load = LoadState::Idle;
        self.action_error = None;
        true
    }

    pub fn fail_load_if_current(&mut self, generation: u64, error: impl Into<String>) -> bool {
        if self.in_flight_load == Some(generation) {
            self.in_flight_load = None;
        }
        if self.generation != generation {
            return false;
        }
        self.load = LoadState::Failed(error.into());
        true
    }

    pub fn supersede_load(&mut self) -> bool {
        self.generation = self.generation.wrapping_add(1);
        let superseded = self.in_flight_load.take().is_some();
        if superseded {
            self.load = LoadState::Idle;
            self.superseded_refresh = true;
        }
        superseded
    }

    pub fn begin_action(&mut self) -> Option<u64> {
        if self.action_pending {
            return None;
        }
        self.action_pending = true;
        self.action_error = None;
        self.generation = self.generation.wrapping_add(1);
        if self.in_flight_load.take().is_some() {
            self.load = LoadState::Idle;
            self.superseded_refresh = true;
        }
        Some(self.generation)
    }

    pub fn finish_action_ok(&mut self) -> bool {
        self.action_pending = false;
        self.action_error = None;
        std::mem::take(&mut self.superseded_refresh)
    }

    pub fn finish_action_err(&mut self, error: impl Into<String>) -> bool {
        self.action_pending = false;
        self.action_error = Some(error.into());
        std::mem::take(&mut self.superseded_refresh)
    }
}

#[cfg(test)]
mod tests {
    use super::SubsystemState;

    #[test]
    fn stale_load_completion_after_action_is_discarded() {
        let mut state = SubsystemState {
            value: "initial",
            ..Default::default()
        };
        let load_gen = state.begin_load().expect("first load starts");
        assert!(state.is_loading());
        assert!(state.begin_action().is_some());
        assert!(!state.is_loading());
        assert!(
            !state.finish_load_if_current(load_gen, "stale"),
            "superseded read must not overwrite newer action state"
        );
        assert_eq!(state.value, "initial");
        assert!(state.finish_action_ok());
    }

    #[test]
    fn direct_finish_load_supersedes_older_in_flight_read() {
        let mut state = SubsystemState::default();
        let load_gen = state.begin_load().expect("load starts");
        state.finish_load("cleared");
        assert!(!state.finish_load_if_current(load_gen, "resurrected"));
        assert_eq!(state.value, "cleared");
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
