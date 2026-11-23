// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! What the result region shows: the precedence of rows, the in-flight cover, the empty band and
//! the offline band.

/// What the result region shows.
///
/// The loading cover answers a request that is actually in flight and nothing
/// else: a service problem or an empty answer is content with its own recovery
/// actions, and covering it would hide the only way out (SPEC SVC-04).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResultSurface {
    Rows,
    Loading,
    Empty,
    Offline,
}

/// Lifecycle stage of a shell-backed local listing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocalListingStage {
    Loading,
    Ready,
    Error,
}

/// Explicit surface owner: either an indexed search query or a shell-backed local listing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SurfaceLocation {
    #[default]
    Index,
    LocalListing(LocalListingStage),
}

/// The panel facts that decide what the result region shows.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ResultSurfaceInputs {
    pub(crate) total_rows: usize,
    pub(crate) location: SurfaceLocation,
    pub(crate) query_in_flight: bool,
    pub(crate) index_catching_up: bool,
    pub(crate) service_connected: bool,
    /// The last exchange failed with a service condition (a timeout or protocol
    /// refusal) even though the connection may still be alive.
    pub(crate) service_issue: bool,
    pub(crate) probing_service: bool,
    pub(crate) has_query: bool,
}

impl Default for ResultSurfaceInputs {
    fn default() -> Self {
        Self {
            total_rows: 0,
            location: SurfaceLocation::Index,
            query_in_flight: false,
            index_catching_up: false,
            service_connected: true,
            service_issue: false,
            probing_service: false,
            has_query: false,
        }
    }
}

/// Decides the result surface from the panel facts, in precedence order: rows in hand win over a
/// cover, a request actually in flight wins over a band, and only a location that can answer gets
/// the empty band instead of the offline one.
pub(crate) fn result_surface(inputs: ResultSurfaceInputs) -> ResultSurface {
    match inputs.location {
        SurfaceLocation::LocalListing(stage) => match stage {
            LocalListingStage::Error => ResultSurface::Empty,
            LocalListingStage::Loading => {
                if inputs.total_rows > 0 {
                    ResultSurface::Rows
                } else {
                    ResultSurface::Loading
                }
            }
            LocalListingStage::Ready => {
                if inputs.total_rows > 0 {
                    ResultSurface::Rows
                } else {
                    ResultSurface::Empty
                }
            }
        },
        SurfaceLocation::Index => {
            let first_page_loading = inputs.total_rows == 0
                && (inputs.query_in_flight || (inputs.probing_service && inputs.service_connected));
            let index_loading =
                !inputs.has_query && inputs.index_catching_up && inputs.total_rows == 0;
            let service_reachable =
                inputs.service_connected && (!inputs.service_issue || inputs.total_rows > 0);
            if service_reachable && inputs.total_rows > 0 {
                ResultSurface::Rows
            } else if first_page_loading || index_loading {
                ResultSurface::Loading
            } else if service_reachable {
                ResultSurface::Empty
            } else {
                ResultSurface::Offline
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LocalListingStage, ResultSurface, ResultSurfaceInputs, SurfaceLocation, result_surface,
    };

    #[test]
    fn the_cover_only_answers_a_request_that_is_actually_in_flight() {
        assert_eq!(
            result_surface(ResultSurfaceInputs::default()),
            ResultSurface::Empty
        );
        assert_eq!(
            result_surface(ResultSurfaceInputs {
                service_issue: true,
                ..Default::default()
            }),
            ResultSurface::Offline,
            "an empty table with a timed-out answer owns the service band"
        );
        assert_eq!(
            result_surface(ResultSurfaceInputs {
                service_issue: true,
                total_rows: 1,
                ..Default::default()
            }),
            ResultSurface::Rows,
            "rows already in hand survive a timed-out refresh"
        );
        assert_eq!(
            result_surface(ResultSurfaceInputs {
                query_in_flight: true,
                ..Default::default()
            }),
            ResultSurface::Loading
        );
        assert_eq!(
            result_surface(ResultSurfaceInputs {
                index_catching_up: true,
                ..Default::default()
            }),
            ResultSurface::Loading,
            "an index still being built is a request in flight"
        );
        assert_eq!(
            result_surface(ResultSurfaceInputs {
                probing_service: true,
                ..Default::default()
            }),
            ResultSurface::Loading
        );
        assert_eq!(
            result_surface(ResultSurfaceInputs {
                location: SurfaceLocation::LocalListing(LocalListingStage::Loading),
                ..Default::default()
            }),
            ResultSurface::Loading
        );
    }

    #[test]
    fn rows_outrank_every_cover() {
        assert_eq!(
            result_surface(ResultSurfaceInputs {
                total_rows: 3,
                query_in_flight: true,
                ..Default::default()
            }),
            ResultSurface::Rows
        );
    }

    #[test]
    fn a_stopped_service_keeps_the_offline_band() {
        assert_eq!(
            result_surface(ResultSurfaceInputs {
                service_connected: false,
                ..Default::default()
            }),
            ResultSurface::Offline
        );
        assert_eq!(
            result_surface(ResultSurfaceInputs {
                total_rows: 3,
                service_connected: false,
                ..Default::default()
            }),
            ResultSurface::Offline,
            "stale rows do not become actionable again without a service"
        );
    }

    #[test]
    fn an_unreadable_bin_hands_the_band_its_error_state() {
        assert_eq!(
            result_surface(ResultSurfaceInputs {
                total_rows: 5,
                location: SurfaceLocation::LocalListing(LocalListingStage::Error),
                ..Default::default()
            }),
            ResultSurface::Empty
        );
    }

    #[test]
    fn an_open_bin_keeps_the_empty_band_without_a_service() {
        assert_eq!(
            result_surface(ResultSurfaceInputs {
                location: SurfaceLocation::LocalListing(LocalListingStage::Ready),
                service_connected: false,
                ..Default::default()
            }),
            ResultSurface::Empty
        );
    }

    #[test]
    fn a_typed_query_does_not_borrow_the_index_build_cover() {
        assert_eq!(
            result_surface(ResultSurfaceInputs {
                has_query: true,
                index_catching_up: true,
                ..Default::default()
            }),
            ResultSurface::Empty
        );
    }
}
