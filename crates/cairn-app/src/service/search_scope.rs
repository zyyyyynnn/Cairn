// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! The user's search scope: which indexer the window is allowed to read from.
//!
//! UI-10 names exactly two scopes. "All files" is the installed LocalSystem service, which reads
//! the raw volume and therefore sees every file on the machine. "Readable files" is a
//! user-privilege indexer that only sees what this account can open, so it needs no elevation and
//! works where an administrator policy forbids the service.
//!
//! This module owns the *meaning* of that choice: which scope a fresh window starts in, what a
//! switch has to do to the machine before it can take effect, and what the window may conclude
//! from the outcome. It deliberately owns no Win32 calls and no UI, so the whole policy is
//! verifiable without a service, a pipe or a window.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The two search scopes the "搜索范围" submenu offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchScope {
    /// The installed LocalSystem service. Every file on the machine, at the cost of elevation.
    AllFiles,
    /// A user-privilege indexer. Only the files this account can read, at no elevation cost.
    ReadableFiles,
}

/// The stored spelling of the "all files" scope. The menu names a scope by this string so the
/// menu's vocabulary and the persisted vocabulary cannot drift apart.
pub const SEARCH_SCOPE_ALL_FILES: &str = "all-files";

/// The stored spelling of the "readable files" scope.
pub const SEARCH_SCOPE_READABLE_FILES: &str = "readable-files";

/// The scope is persisted by its own spelling, not by the variant name, so a rename in the code
/// cannot invalidate a `settings.toml` that is already on disk.
impl Serialize for SearchScope {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SearchScope {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let stored = String::deserialize(deserializer)?;
        Ok(SearchScope::from_stored(&stored))
    }
}

impl SearchScope {
    /// The persisted spelling of this scope. The value is written to `settings.toml`, so it is
    /// part of the on-disk contract and must not drift with a rename.
    pub const fn as_str(self) -> &'static str {
        match self {
            SearchScope::AllFiles => SEARCH_SCOPE_ALL_FILES,
            SearchScope::ReadableFiles => SEARCH_SCOPE_READABLE_FILES,
        }
    }

    /// Reads a persisted scope, or `None` when the value is not one of the two documented
    /// spellings. A value this build cannot read is no preference at all, so the machine decides.
    pub fn parse_stored(value: &str) -> Option<Self> {
        match value {
            SEARCH_SCOPE_ALL_FILES => Some(SearchScope::AllFiles),
            SEARCH_SCOPE_READABLE_FILES => Some(SearchScope::ReadableFiles),
            _ => None,
        }
    }

    /// Reads a persisted scope. An unknown value falls back to the scope that needs no elevation
    /// rather than failing: a settings file written by a newer build must still open, and a value
    /// this build cannot read must not cost the user the whole preference file.
    pub fn from_stored(value: &str) -> Self {
        Self::parse_stored(value).unwrap_or(SearchScope::ReadableFiles)
    }

    /// `true` when reaching this scope needs the service, and therefore an administrator.
    pub const fn needs_service(self) -> bool {
        matches!(self, SearchScope::AllFiles)
    }

    /// `true` when this scope runs on the user-privilege indexer, which has no content index and
    /// therefore no content-search switch to offer.
    pub const fn needs_local_only(self) -> bool {
        matches!(self, SearchScope::ReadableFiles)
    }

    /// The service start type this scope implies.
    ///
    /// "All files" needs the service running on every boot, so it is `Auto`. "Readable files"
    /// runs on the service account's own terms, so it is `Manual` and starts on demand.
    pub const fn service_start_type(self) -> ServiceStartType {
        match self {
            SearchScope::AllFiles => ServiceStartType::Auto,
            SearchScope::ReadableFiles => ServiceStartType::Manual,
        }
    }
}

/// The start type the Windows service is registered with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceStartType {
    /// Starts when the machine boots.
    Auto,
    /// Starts only when something asks for it.
    Manual,
}

impl ServiceStartType {
    /// The `sc.exe` argument that sets this start type.
    pub const fn as_sc_argument(self) -> &'static str {
        match self {
            ServiceStartType::Auto => "auto",
            ServiceStartType::Manual => "demand",
        }
    }
}

/// What the machine knows about the service registration right now.
///
/// The scope owns two pieces of machine state: the start type the service is registered with, and
/// whether it is running. Both decide whether the scope can be reached, so both are read together
/// and never guessed at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceState {
    /// The service is not registered on this machine.
    NotInstalled,
    /// The service is registered with a start type this product owns.
    Owned {
        /// The start type the service is registered with.
        start_type: ServiceStartType,
        /// Whether the service is running right now.
        running: bool,
    },
    /// The service is registered with a start type this product never sets (disabled, boot,
    /// system). The switch can still rewrite it, so this is not a dead end.
    Foreign {
        /// Whether the service is running right now.
        running: bool,
    },
    /// The registration could not be read, so no safe plan can be made from it.
    Unreadable,
}

/// What a scope switch has to do to the machine before the new scope can answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeSwitchPlan {
    /// Nothing to do: the machine already matches the scope, and no elevation is needed.
    AlreadyInPlace,
    /// The service has to change. Every step needs an administrator, so the whole change goes
    /// through one elevation.
    ChangeService {
        /// The start type the scope needs.
        start_type: ServiceStartType,
        /// Whether the registration's start type has to be rewritten.
        reconfigure: bool,
        /// Whether the service has to be stopped as part of the change.
        stop: bool,
        /// Whether the service has to be started as part of the change.
        start: bool,
    },
    /// The service is not registered, so the scope that needs it cannot be reached.
    ServiceMissing,
    /// The registration could not be read, so the window cannot safely claim any scope.
    Unreadable,
}

/// The plan for reaching `target` from the machine state `state` describes.
///
/// The scope owns the service's start type in both directions *and* whether the service runs.
/// "All files" is answered by the service, so the service has to be up; "readable files" is
/// answered by the user indexer, so the service has to be down, otherwise two indexers walk the
/// same volumes at the same time.
pub fn switch_plan(target: SearchScope, state: ServiceState) -> ScopeSwitchPlan {
    let want_running = target.needs_service();

    let (reconfigure, running) = match state {
        ServiceState::Unreadable => return ScopeSwitchPlan::Unreadable,
        ServiceState::NotInstalled => {
            // Without a registration there is nothing to reconfigure. A scope that needs the
            // service cannot be reached until it is installed; the other scope is already in
            // force, because a service that is not there cannot be running.
            return if want_running {
                ScopeSwitchPlan::ServiceMissing
            } else {
                ScopeSwitchPlan::AlreadyInPlace
            };
        }
        ServiceState::Owned {
            start_type,
            running,
        } => (start_type != target.service_start_type(), running),
        // A foreign start type never matches, so the registration has to be rewritten.
        ServiceState::Foreign { running } => (true, running),
    };

    let stop = running && !want_running;
    let start = !running && want_running;
    if !reconfigure && !stop && !start {
        return ScopeSwitchPlan::AlreadyInPlace;
    }
    ScopeSwitchPlan::ChangeService {
        start_type: target.service_start_type(),
        reconfigure,
        stop,
        start,
    }
}

/// Whether the machine state `state` already satisfies `scope`.
///
/// The window checks this after the change, so a switch that did not land is reported as a failure
/// instead of a success the user can see through.
pub fn scope_is_satisfied(scope: SearchScope, state: ServiceState) -> bool {
    match state {
        ServiceState::Unreadable => false,
        ServiceState::NotInstalled => !scope.needs_service(),
        ServiceState::Owned {
            start_type,
            running,
        } => start_type == scope.service_start_type() && running == scope.needs_service(),
        ServiceState::Foreign { .. } => false,
    }
}

/// The scope a window opens in, given the saved preference and whether the service is running.
///
/// The saved preference wins. Without one, the window follows the machine: a service that is
/// already running is the answer the window would otherwise have to ask for, and taking it does
/// not prompt for administrator rights (SPEC SVC-04).
pub fn initial_scope(saved: Option<SearchScope>, service_running: bool) -> SearchScope {
    match saved {
        Some(scope) => scope,
        None if service_running => SearchScope::AllFiles,
        None => SearchScope::ReadableFiles,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_scopes_are_the_only_ones_the_menu_offers() {
        assert_eq!(SearchScope::AllFiles.as_str(), "all-files");
        assert_eq!(SearchScope::ReadableFiles.as_str(), "readable-files");
        // An unknown stored value resolves to the scope that needs no elevation instead of
        // failing the settings load.
        assert_eq!(
            SearchScope::from_stored("something-else"),
            SearchScope::ReadableFiles
        );
        assert_eq!(SearchScope::from_stored("all-files"), SearchScope::AllFiles);
    }

    #[test]
    fn only_the_all_files_scope_needs_the_service() {
        assert!(SearchScope::AllFiles.needs_service());
        assert!(!SearchScope::ReadableFiles.needs_service());
        assert_eq!(
            SearchScope::AllFiles.service_start_type(),
            ServiceStartType::Auto
        );
        assert_eq!(
            SearchScope::ReadableFiles.service_start_type(),
            ServiceStartType::Manual
        );
    }

    #[test]
    fn switching_to_the_service_scope_changes_its_start_type_and_restarts_it() {
        let plan = switch_plan(
            SearchScope::AllFiles,
            ServiceState::Owned {
                start_type: ServiceStartType::Manual,
                running: true,
            },
        );
        assert_eq!(
            plan,
            ScopeSwitchPlan::ChangeService {
                start_type: ServiceStartType::Auto,
                reconfigure: true,
                stop: false,
                start: false,
            }
        );
    }

    #[test]
    fn the_readable_scope_also_owns_the_service_start_type() {
        // Switching to "readable files" means the service must stop coming back on every boot and
        // must not keep running, so it changes the registration just as the other direction does.
        let plan = switch_plan(
            SearchScope::ReadableFiles,
            ServiceState::Owned {
                start_type: ServiceStartType::Auto,
                running: true,
            },
        );
        assert_eq!(
            plan,
            ScopeSwitchPlan::ChangeService {
                start_type: ServiceStartType::Manual,
                reconfigure: true,
                stop: true,
                start: false,
            }
        );
    }

    #[test]
    fn a_scope_that_needs_no_service_change_never_asks_for_elevation() {
        let plan = switch_plan(
            SearchScope::ReadableFiles,
            ServiceState::Owned {
                start_type: ServiceStartType::Manual,
                running: false,
            },
        );
        assert_eq!(plan, ScopeSwitchPlan::AlreadyInPlace);
    }

    #[test]
    fn a_service_in_the_wanted_start_type_needs_no_change() {
        let plan = switch_plan(
            SearchScope::AllFiles,
            ServiceState::Owned {
                start_type: ServiceStartType::Auto,
                running: true,
            },
        );
        assert_eq!(plan, ScopeSwitchPlan::AlreadyInPlace);
    }

    #[test]
    fn a_service_in_the_wanted_start_type_but_stopped_is_started() {
        // The start type is right but the service is down, so the scope that reads from it is not
        // actually in force yet.
        let plan = switch_plan(
            SearchScope::AllFiles,
            ServiceState::Owned {
                start_type: ServiceStartType::Auto,
                running: false,
            },
        );
        assert_eq!(
            plan,
            ScopeSwitchPlan::ChangeService {
                start_type: ServiceStartType::Auto,
                reconfigure: false,
                stop: false,
                start: true,
            }
        );
    }

    #[test]
    fn a_service_that_is_not_registered_cannot_be_switched_to() {
        let plan = switch_plan(SearchScope::AllFiles, ServiceState::NotInstalled);
        assert_eq!(plan, ScopeSwitchPlan::ServiceMissing);
    }

    #[test]
    fn a_scope_that_does_not_need_the_service_needs_nothing_to_reach_it() {
        let plan = switch_plan(SearchScope::ReadableFiles, ServiceState::NotInstalled);
        assert_eq!(plan, ScopeSwitchPlan::AlreadyInPlace);
    }

    #[test]
    fn an_unreadable_registration_yields_no_plan() {
        let plan = switch_plan(SearchScope::AllFiles, ServiceState::Unreadable);
        assert_eq!(plan, ScopeSwitchPlan::Unreadable);
    }

    #[test]
    fn a_first_run_follows_the_machine() {
        // SVC-04: no saved preference means the service decides, and neither answer prompts for
        // administrator rights.
        assert_eq!(initial_scope(None, true), SearchScope::AllFiles);
        assert_eq!(initial_scope(None, false), SearchScope::ReadableFiles);
    }

    #[test]
    fn a_saved_scope_survives_a_restart() {
        assert_eq!(
            initial_scope(Some(SearchScope::AllFiles), false),
            SearchScope::AllFiles
        );
        assert_eq!(
            initial_scope(Some(SearchScope::ReadableFiles), true),
            SearchScope::ReadableFiles
        );
    }

    #[test]
    fn only_the_state_that_matches_the_scope_satisfies_it() {
        assert!(scope_is_satisfied(
            SearchScope::AllFiles,
            ServiceState::Owned {
                start_type: ServiceStartType::Auto,
                running: true,
            }
        ));
        assert!(!scope_is_satisfied(
            SearchScope::AllFiles,
            ServiceState::Owned {
                start_type: ServiceStartType::Auto,
                running: false,
            }
        ));
        assert!(scope_is_satisfied(
            SearchScope::ReadableFiles,
            ServiceState::Owned {
                start_type: ServiceStartType::Manual,
                running: false,
            }
        ));
        assert!(!scope_is_satisfied(
            SearchScope::ReadableFiles,
            ServiceState::Owned {
                start_type: ServiceStartType::Manual,
                running: true,
            }
        ));
    }
}
