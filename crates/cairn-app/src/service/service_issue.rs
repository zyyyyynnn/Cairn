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
pub fn should_try_silent_start(issue: ServiceIssue, already_attempted: bool) -> bool {
    issue == ServiceIssue::NotRunning && !already_attempted
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::{should_try_silent_start, ServiceIssue};

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
    fn each_issue_names_its_own_announcement() {
        let keys: Vec<&str> = [
            ServiceIssue::NotRunning,
            ServiceIssue::NotInstalled,
            ServiceIssue::ProtocolIncompatible,
            ServiceIssue::Unauthorized,
        ]
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
