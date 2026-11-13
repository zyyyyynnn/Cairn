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

/// The panel facts that decide what the result region shows.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ResultSurfaceInputs {
    pub(crate) total_rows: usize,
    pub(crate) recycle_bin_open: bool,
    pub(crate) recycle_bin_loading: bool,
    pub(crate) recycle_bin_error: bool,
    pub(crate) block_listing_open: bool,
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
            recycle_bin_open: false,
            recycle_bin_loading: false,
            recycle_bin_error: false,
            block_listing_open: false,
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
    let bin_loading = inputs.recycle_bin_open && inputs.recycle_bin_loading;
    // A block listing holds in-memory members too: no index request belongs to
    // it, so a query in flight or an index catch-up must not cover its rows.
    let local_listing = inputs.recycle_bin_open || inputs.block_listing_open;
    let first_page_loading = (!local_listing
        && inputs.total_rows == 0
        && (inputs.query_in_flight || (inputs.probing_service && inputs.service_connected)))
        || (bin_loading && inputs.total_rows == 0);
    let index_loading =
        !local_listing && !inputs.has_query && inputs.index_catching_up && inputs.total_rows == 0;
    // The bin and a block listing are shell/tree data, not index data: a stopped service
    // must not replace them with the offline band, and an empty location is its own state,
    // not an offline one. A timeout leaves the connection alive and any rows already in
    // hand valid; with no rows it still needs the service band rather than an empty result.
    let local_location = local_listing
        || (inputs.service_connected && (!inputs.service_issue || inputs.total_rows > 0));
    // An unreadable bin is authoritative over any rows still in the table. The state band owns the
    // retry path; stale rows must never become actionable again.
    let has_content = !inputs.recycle_bin_error && local_location && inputs.total_rows > 0;
    if has_content {
        ResultSurface::Rows
    } else if first_page_loading || index_loading {
        ResultSurface::Loading
    } else if local_location {
        ResultSurface::Empty
    } else {
        ResultSurface::Offline
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::rust_2021::test;

    use super::{result_surface, ResultSurface, ResultSurfaceInputs};

    #[test]
    fn the_cover_only_answers_a_request_that_is_actually_in_flight() {
        // The cover answers an in-flight request only. With nothing in flight and no rows the band
        // owns the region, because its recovery actions are the only way out of a stopped service
        // (SPEC SVC-04).
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
                recycle_bin_open: true,
                recycle_bin_loading: true,
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
    fn a_block_listing_is_a_local_location() {
        assert_eq!(
            result_surface(ResultSurfaceInputs {
                block_listing_open: true,
                ..Default::default()
            }),
            ResultSurface::Empty
        );
        assert_eq!(
            result_surface(ResultSurfaceInputs {
                block_listing_open: true,
                service_connected: false,
                ..Default::default()
            }),
            ResultSurface::Empty,
            "in-memory rows do not need the index service"
        );
        assert_eq!(
            result_surface(ResultSurfaceInputs {
                block_listing_open: true,
                total_rows: 2,
                service_connected: false,
                probing_service: true,
                ..Default::default()
            }),
            ResultSurface::Rows
        );
        assert_eq!(
            result_surface(ResultSurfaceInputs {
                block_listing_open: true,
                index_catching_up: true,
                ..Default::default()
            }),
            ResultSurface::Empty,
            "an index catch-up is not this location's request"
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
                recycle_bin_open: true,
                recycle_bin_error: true,
                ..Default::default()
            }),
            ResultSurface::Empty
        );
    }

    /// The bin is a location the app can answer without the service, so it keeps the empty band
    /// even when the service is stopped.
    #[test]
    fn an_open_bin_keeps_the_empty_band_without_a_service() {
        assert_eq!(
            result_surface(ResultSurfaceInputs {
                recycle_bin_open: true,
                service_connected: false,
                ..Default::default()
            }),
            ResultSurface::Empty
        );
    }

    /// A typed query is the request in flight; index progress only covers the all-files view.
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
