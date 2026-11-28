// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Why the window cannot show results, and what it may try about that.
//!
//! The issue is the window's vocabulary for a service that is unusable (SVC-03, SVC-04). It
//! carries the condition and the recovery the copy may name, so it is derived from the client
//! error type rather than from a string match on a message.

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
    /// The service is reachable but did not answer within the client budget.
    Timeout,
}

impl ServiceIssue {
    #[cfg(test)]
    pub const ALL: &[ServiceIssue] = &[
        ServiceIssue::NotRunning,
        ServiceIssue::NotInstalled,
        ServiceIssue::ProtocolIncompatible,
        ServiceIssue::Unauthorized,
        ServiceIssue::Timeout,
    ];

    pub fn from_client_error(error: &cairn_client::client::ClientError) -> Self {
        if error.is_protocol_incompatible() {
            ServiceIssue::ProtocolIncompatible
        } else if error.is_unauthorized() {
            ServiceIssue::Unauthorized
        } else if matches!(error, cairn_client::client::ClientError::Timeout) {
            ServiceIssue::Timeout
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
            ServiceIssue::Timeout => "service-issue-timeout",
        }
    }

    /// The message the result band states for this issue: the condition and the recovery the
    /// person can take outside the app.
    pub const fn message_key(self) -> &'static str {
        match self {
            ServiceIssue::NotRunning => "service_not_running_title",
            ServiceIssue::NotInstalled => "service_not_installed_title",
            ServiceIssue::ProtocolIncompatible => "service_incompatible_title",
            ServiceIssue::Unauthorized => "service_unauthorized_title",
            ServiceIssue::Timeout => "service_timeout_title",
        }
    }
}

/// One silent SCM start per connection-loss episode, and only when the service exists and is
/// merely stopped. A missing service must show install guidance, not request elevation.
pub fn should_try_silent_start(issue: ServiceIssue, already_attempted: bool) -> bool {
    issue == ServiceIssue::NotRunning && !already_attempted
}

/// What the result band may offer for an issue. An action has to be able to change the outcome:
/// re-probing only helps while the service might answer next time, and starting it only helps when
/// the service exists but is stopped. The remaining issues are cleared outside the app, so the band
/// names that recovery in its sentence and offers no control (SVC-04).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceBandAction {
    Start,
    Retry,
    None,
}

pub const fn band_action(issue: ServiceIssue, service_installed: bool) -> ServiceBandAction {
    match issue {
        ServiceIssue::NotRunning if service_installed => ServiceBandAction::Start,
        ServiceIssue::NotRunning | ServiceIssue::Timeout => ServiceBandAction::Retry,
        ServiceIssue::NotInstalled
        | ServiceIssue::ProtocolIncompatible
        | ServiceIssue::Unauthorized => ServiceBandAction::None,
    }
}

#[cfg(test)]
mod tests {
    use super::{ServiceIssue, should_try_silent_start};

    #[test]
    fn a_stopped_installed_service_gets_one_silent_start_attempt() {
        assert!(should_try_silent_start(ServiceIssue::NotRunning, false));
        assert!(
            !should_try_silent_start(ServiceIssue::NotRunning, true),
            "a second silent attempt would loop without a user action"
        );
    }

    #[test]
    fn a_timeout_is_its_own_retryable_condition() {
        assert_eq!(
            ServiceIssue::from_client_error(&cairn_client::client::ClientError::Timeout),
            ServiceIssue::Timeout
        );
        assert_eq!(
            ServiceIssue::from_client_error(&cairn_client::client::ClientError::ConnectionClosed),
            ServiceIssue::NotRunning
        );
        let entry = crate::shell::accessibility::control(ServiceIssue::Timeout.accessibility_key());
        assert_eq!(entry.messages, ["service_timeout_title"].as_slice());
    }

    #[test]
    fn each_issue_has_unique_message_and_registered_announcement() {
        let mut messages = std::collections::HashSet::new();
        let mut announcements = std::collections::HashSet::new();
        for issue in ServiceIssue::ALL {
            let msg = issue.message_key();
            assert!(!msg.is_empty(), "{issue:?} must carry a message key");
            assert!(messages.insert(msg), "two issues share message key `{msg}`");
            let a11y_key = issue.accessibility_key();
            assert!(
                announcements.insert(a11y_key),
                "two issues share announcement key `{a11y_key}`"
            );
            let entry = crate::shell::accessibility::control(a11y_key);
            assert_eq!(
                entry.messages,
                [msg].as_slice(),
                "{issue:?} does not speak its registered sentence"
            );
        }
    }
}
