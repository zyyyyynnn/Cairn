// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Switching the window between the two search scopes.
//!
//! UI-10 gives the user one scope choice and one rule: a switch that is refused must not leave
//! the window half-changed. This module owns that rule. It decides what has to happen on the
//! machine, performs it through one elevation, checks that the machine really landed, and reports
//! one outcome the window can act on.
//!
//! The window's own state — which indexer it reads from, which error it shows — stays in the
//! window. This module returns what happened; it does not decide what the window looks like.

use crate::service::elevation::ElevationError;
use crate::service::search_scope::{scope_is_satisfied, switch_plan, ScopeSwitchPlan, SearchScope};
use crate::service::service_control::{self, ServiceChange};

/// Why a scope switch did not land.
///
/// The window turns this into a sentence in the user's language. The technical detail behind it
/// belongs in the log, never on the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeSwitchFailure {
    /// The service registration could not be read, so no safe plan could be made from it.
    RegistrationUnreadable,
    /// The elevated change did not succeed.
    ChangeFailed,
    /// The change reported success but the machine is not in the wanted state.
    NotInEffect,
    /// The administrator refused the change.
    Restricted,
}

/// The result of a scope switch, in the terms the window needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScopeSwitchResult {
    /// The new scope is in force. The window reads from the indexer that scope names.
    Applied {
        /// The scope the window is now in.
        scope: SearchScope,
    },
    /// The user dismissed the elevation prompt. The previous scope stays in force and nothing on
    /// the machine changed.
    Cancelled {
        /// The scope that is still in force.
        scope: SearchScope,
    },
    /// The operation failed. The previous scope stays in force; the reason names the cause.
    Failed {
        /// The scope that is still in force.
        scope: SearchScope,
        /// Which failure it was.
        reason: ScopeSwitchFailure,
        /// The technical detail, for the log. It is never shown to the user.
        detail: String,
    },
    /// The service this scope needs is not registered, so the scope cannot be reached.
    NotInstalled {
        /// The scope that is still in force.
        scope: SearchScope,
    },
}

impl ScopeSwitchResult {
    /// The scope the window is in after this result.
    ///
    /// A refused switch keeps the previous scope: the window does not claim a scope it did not
    /// reach, which is what makes "取消 UAC 不产生半切换" (UI-10) true.
    pub fn scope_in_force(&self) -> SearchScope {
        match self {
            ScopeSwitchResult::Applied { scope }
            | ScopeSwitchResult::Cancelled { scope }
            | ScopeSwitchResult::Failed { scope, .. }
            | ScopeSwitchResult::NotInstalled { scope } => *scope,
        }
    }
}

/// How long the machine is given to settle after a start or stop request.
///
/// `sc.exe` answers before the service has finished its transition, so the state is re-read for a
/// short while before a change is called a failure.
const SETTLE_ATTEMPTS: usize = 20;
const SETTLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// `ERROR_ACCESS_DENIED`: the machine refused the change for the administrator.
const ERROR_ACCESS_DENIED: u32 = 5;

/// Switches the window to `target`.
///
/// The plan is read first: a scope that needs no change to the machine is applied without ever
/// asking for elevation. Only a scope that changes the service's registration goes through the
/// elevated path, and a refused elevation leaves everything as it was.
pub fn switch_to(target: SearchScope, current: SearchScope) -> ScopeSwitchResult {
    let state = service_control::service_state();
    let plan = switch_plan(target, state);

    match plan {
        ScopeSwitchPlan::AlreadyInPlace => ScopeSwitchResult::Applied { scope: target },
        ScopeSwitchPlan::ServiceMissing => ScopeSwitchResult::NotInstalled { scope: current },
        ScopeSwitchPlan::Unreadable => ScopeSwitchResult::Failed {
            scope: current,
            reason: ScopeSwitchFailure::RegistrationUnreadable,
            detail: "the service registration could not be read".to_string(),
        },
        ScopeSwitchPlan::ChangeService {
            start_type,
            reconfigure,
            stop,
            start,
        } => {
            let change = ServiceChange {
                start_type,
                reconfigure,
                stop,
                start,
            };
            match service_control::apply_change(change) {
                Ok(()) => {
                    // The command reported success; the machine has the last word. A change that
                    // did not land is a failure, not a scope the window may claim.
                    let after = settle(target);
                    if scope_is_satisfied(target, after) {
                        ScopeSwitchResult::Applied { scope: target }
                    } else {
                        ScopeSwitchResult::Failed {
                            scope: current,
                            reason: ScopeSwitchFailure::NotInEffect,
                            detail: format!("the service reads as {after:?} after the change"),
                        }
                    }
                }
                Err(ElevationError::Cancelled) => ScopeSwitchResult::Cancelled { scope: current },
                Err(ElevationError::Failed { os_error, message }) => {
                    let reason = match os_error {
                        Some(ERROR_ACCESS_DENIED) => ScopeSwitchFailure::Restricted,
                        _ => ScopeSwitchFailure::ChangeFailed,
                    };
                    ScopeSwitchResult::Failed {
                        scope: current,
                        reason,
                        detail: message,
                    }
                }
            }
        }
    }
}

/// Reads the service state until it settles into the wanted shape, or the patience runs out.
///
/// A start or a stop is a request, not a fact: `sc.exe` has already returned while the service is
/// still transitioning. The window must not call that a failed switch.
fn settle(target: SearchScope) -> crate::service::search_scope::ServiceState {
    let mut state = service_control::service_state();
    for _ in 0..SETTLE_ATTEMPTS {
        if scope_is_satisfied(target, state) {
            break;
        }
        std::thread::sleep(SETTLE_INTERVAL);
        state = service_control::service_state();
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A refused switch is not a failure and not a half-change: the window keeps the scope it
    /// had. This is the rule UI-10 states as "取消 UAC 不产生半切换".
    #[test]
    fn a_refused_switch_keeps_the_previous_scope() {
        let previous = SearchScope::ReadableFiles;

        let cancelled = ScopeSwitchResult::Cancelled { scope: previous };
        assert_eq!(cancelled.scope_in_force(), previous);

        let failed = ScopeSwitchResult::Failed {
            scope: previous,
            reason: ScopeSwitchFailure::ChangeFailed,
            detail: "the elevation was refused".to_string(),
        };
        assert_eq!(failed.scope_in_force(), previous);

        let missing = ScopeSwitchResult::NotInstalled { scope: previous };
        assert_eq!(missing.scope_in_force(), previous);
    }

    /// An applied switch is the only outcome that moves the window to the requested scope.
    #[test]
    fn an_applied_switch_takes_the_requested_scope() {
        let applied = ScopeSwitchResult::Applied {
            scope: SearchScope::AllFiles,
        };
        assert_eq!(applied.scope_in_force(), SearchScope::AllFiles);
    }

    /// Switching to "readable files" only needs an elevation when the service is registered with
    /// a start type it must not keep. On a machine with no service there is nothing to change,
    /// so the switch applies without a prompt.
    #[test]
    fn switching_to_the_readable_scope_prompts_only_when_the_service_must_change() {
        use crate::service::search_scope::ServiceState;

        assert_eq!(
            switch_plan(SearchScope::ReadableFiles, ServiceState::NotInstalled),
            ScopeSwitchPlan::AlreadyInPlace
        );
        assert_eq!(
            switch_plan(
                SearchScope::ReadableFiles,
                ServiceState::Owned {
                    start_type: crate::service::search_scope::ServiceStartType::Manual,
                    running: false,
                }
            ),
            ScopeSwitchPlan::AlreadyInPlace
        );
        assert_eq!(
            switch_plan(
                SearchScope::ReadableFiles,
                ServiceState::Owned {
                    start_type: crate::service::search_scope::ServiceStartType::Auto,
                    running: true,
                }
            ),
            ScopeSwitchPlan::ChangeService {
                start_type: crate::service::search_scope::ServiceStartType::Manual,
                reconfigure: true,
                stop: true,
                start: false,
            }
        );
    }

    /// A scope that needs a service the machine does not have is reported as such instead of
    /// failing with a technical error.
    #[test]
    fn a_missing_service_is_reported_as_not_installed() {
        use crate::service::search_scope::ServiceState;

        assert_eq!(
            switch_plan(SearchScope::AllFiles, ServiceState::NotInstalled),
            ScopeSwitchPlan::ServiceMissing
        );
    }
}
