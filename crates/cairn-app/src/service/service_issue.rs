// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Why the window cannot show results, and what it may try about that.
//!
//! The issue is the window's vocabulary for a service that is unusable (SVC-03, SVC-04). It
//! carries the condition and the recovery the copy may name, so it is derived from the client
//! error type rather than from a string match on a message.
//!
//! It lives apart from the task orchestration on purpose: the accessibility table and the result
//! band both read it, and neither of them should have to reach through the window's state to ask
//! what went wrong.

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

impl ServiceIssue {
    /// Every issue the result band can report, for the coverage checks that walk the vocabulary.
    #[cfg(test)]
    pub const ALL: &[ServiceIssue] = &[
        ServiceIssue::NotRunning,
        ServiceIssue::NotInstalled,
        ServiceIssue::ProtocolIncompatible,
        ServiceIssue::Unauthorized,
    ];

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

    /// The message the result band states for this issue: the condition and the recovery the
    /// person can take outside the app.
    pub const fn message_key(self) -> &'static str {
        match self {
            ServiceIssue::NotRunning => "service_not_running_title",
            ServiceIssue::NotInstalled => "service_not_installed_title",
            ServiceIssue::ProtocolIncompatible => "service_incompatible_title",
            ServiceIssue::Unauthorized => "service_unauthorized_title",
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
        ServiceIssue::NotRunning => ServiceBandAction::Retry,
        ServiceIssue::NotInstalled
        | ServiceIssue::ProtocolIncompatible
        | ServiceIssue::Unauthorized => ServiceBandAction::None,
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::{band_action, should_try_silent_start, ServiceBandAction, ServiceIssue};

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

    /// An action the issue cannot clear is a dead control: the band offers none.
    #[test]
    fn only_a_recoverable_issue_offers_an_action() {
        assert_eq!(
            band_action(ServiceIssue::NotRunning, true),
            ServiceBandAction::Start
        );
        assert_eq!(
            band_action(ServiceIssue::NotRunning, false),
            ServiceBandAction::Retry
        );
        for issue in [
            ServiceIssue::NotInstalled,
            ServiceIssue::ProtocolIncompatible,
            ServiceIssue::Unauthorized,
        ] {
            for installed in [true, false] {
                assert_eq!(
                    band_action(issue, installed),
                    ServiceBandAction::None,
                    "{issue:?} with installed={installed}"
                );
            }
        }
    }

    #[test]
    fn each_issue_names_its_own_message() {
        let keys: Vec<&str> = ServiceIssue::ALL
            .iter()
            .map(|issue| issue.message_key())
            .collect();
        assert!(
            keys.iter().all(|key| !key.is_empty()),
            "an issue without a message key would render the raw id"
        );
        let mut unique = keys.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), keys.len(), "two issues share one message");
    }

    /// The result band reads its sentence from the registry entry the issue names, so the two
    /// mappings cannot drift apart.
    #[test]
    fn each_issue_speaks_the_message_its_registry_entry_registers() {
        for issue in ServiceIssue::ALL {
            let entry = crate::shell::accessibility::control(issue.accessibility_key());
            assert_eq!(
                entry.messages,
                [issue.message_key()].as_slice(),
                "{issue:?} does not speak its registered sentence"
            );
        }
    }

    #[test]
    fn each_issue_names_its_own_announcement() {
        let keys: Vec<&str> = ServiceIssue::ALL
            .iter()
            .map(|issue| issue.accessibility_key())
            .collect();
        let mut unique = keys.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            keys.len(),
            "two issues share one announcement"
        );
    }
}
