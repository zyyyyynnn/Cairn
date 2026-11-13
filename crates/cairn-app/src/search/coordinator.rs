// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Search query coordinator: adaptive short-window debounce, stale-timer token checks, explicit
//! immediate dispatch, and prior in-flight `Cancel` issuance before dispatching a newer query.

use std::time::{Duration, Instant};

use cairn_protocol::message::{QueryDefaults, SortColumn};

/// Default cooldown / coalesce window for rapid typing bursts.
pub(crate) const SEARCH_COALESCE_WINDOW: Duration = Duration::from_millis(50);

/// Monotonic time source seam for search debouncing.
pub(crate) trait Clock {
    fn now(&self) -> Instant;
}

/// Production monotonic clock.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MonotonicClock;

impl Clock for MonotonicClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Complete specification of a single query RPC issued to the service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SearchCommand {
    pub query: String,
    pub defaults: QueryDefaults,
    pub sort_column: SortColumn,
    pub sort_asc: bool,
    pub page_size: usize,
    pub reset_scroll_on_apply: bool,
}

/// Output transport seam used by [`SearchCoordinator`] to send `Cancel` and `Query` requests.
pub(crate) trait ServiceTransport {
    fn send_cancel(&mut self, query_id: u64);
    fn send_query(&mut self, query_id: u64, command: SearchCommand);
}

/// Outcome returned when submitting a query edit to [`SearchCoordinator`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SearchPlan {
    /// Query `query_id` was dispatched right away.
    Dispatched { query_id: u64 },
    /// Rapid edit was queued behind `delay`; caller should flush with `token` when the timer fires.
    Debounced { token: u64, delay: Duration },
}

#[derive(Clone, Debug)]
struct PendingSearch {
    token: u64,
    command: SearchCommand,
}

/// Coordinates query lifecycle across debounced keystrokes, explicit triggers, cancellations, and
/// out-of-order responses.
#[derive(Debug)]
pub(crate) struct SearchCoordinator {
    debounce_window: Duration,
    current_query_id: u64,
    in_flight_query_id: Option<u64>,
    last_dispatch_at: Option<Instant>,
    debounce_token: u64,
    pending: Option<PendingSearch>,
    cancelled: bool,
    reset_scroll_pending: bool,
}

impl Default for SearchCoordinator {
    fn default() -> Self {
        Self::new(SEARCH_COALESCE_WINDOW)
    }
}

impl SearchCoordinator {
    pub fn new(debounce_window: Duration) -> Self {
        Self {
            debounce_window,
            current_query_id: 0,
            in_flight_query_id: None,
            last_dispatch_at: None,
            debounce_token: 0,
            pending: None,
            cancelled: false,
            reset_scroll_pending: false,
        }
    }

    pub fn current_query_id(&self) -> u64 {
        self.current_query_id
    }

    pub fn is_searching(&self) -> bool {
        self.in_flight_query_id.is_some() || self.pending.is_some()
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    pub fn set_cancelled(&mut self, cancelled: bool) {
        self.cancelled = cancelled;
    }

    pub fn can_background_refresh(&self) -> bool {
        !self.cancelled && !self.is_searching()
    }

    pub fn schedule_text_edit(
        &mut self,
        command: SearchCommand,
        clock: &dyn Clock,
        transport: &mut dyn ServiceTransport,
    ) -> SearchPlan {
        self.cancelled = false;
        let now = clock.now();
        let within_cooldown = self
            .last_dispatch_at
            .is_some_and(|prev| now.saturating_duration_since(prev) < self.debounce_window);

        if !within_cooldown && self.pending.is_none() && self.in_flight_query_id.is_none() {
            let query_id = self.dispatch_now(command, now, transport);
            return SearchPlan::Dispatched { query_id };
        }

        self.debounce_token = self.debounce_token.wrapping_add(1);
        let token = self.debounce_token;
        self.pending = Some(PendingSearch { token, command });
        SearchPlan::Debounced {
            token,
            delay: self.debounce_window,
        }
    }

    pub fn dispatch_immediate(
        &mut self,
        command: SearchCommand,
        clock: &dyn Clock,
        transport: &mut dyn ServiceTransport,
    ) -> u64 {
        self.cancelled = false;
        self.debounce_token = self.debounce_token.wrapping_add(1);
        self.pending = None;
        self.dispatch_now(command, clock.now(), transport)
    }

    pub fn flush_debounced(
        &mut self,
        token: u64,
        clock: &dyn Clock,
        transport: &mut dyn ServiceTransport,
    ) -> Option<u64> {
        let pending = self.pending.as_ref()?;
        if pending.token != token {
            return None;
        }
        let pending = self.pending.take().expect("checked pending");
        Some(self.dispatch_now(pending.command, clock.now(), transport))
    }

    pub fn cancel_in_flight(&mut self, transport: &mut dyn ServiceTransport) -> Option<u64> {
        self.debounce_token = self.debounce_token.wrapping_add(1);
        self.pending = None;
        self.cancelled = true;
        self.reset_scroll_pending = false;
        self.current_query_id += 1;
        let previous = self.in_flight_query_id.take();
        if let Some(old_qid) = previous {
            transport.send_cancel(old_qid);
        }
        previous
    }

    /// Validates an incoming query response, returning `Some(reset_scroll)` if still current.
    pub fn accept_response(&mut self, query_id: u64) -> Option<bool> {
        if query_id != self.current_query_id || self.cancelled {
            return None;
        }
        self.in_flight_query_id = None;
        Some(std::mem::take(&mut self.reset_scroll_pending))
    }

    fn dispatch_now(
        &mut self,
        command: SearchCommand,
        now: Instant,
        transport: &mut dyn ServiceTransport,
    ) -> u64 {
        if let Some(old_qid) = self.in_flight_query_id.take() {
            transport.send_cancel(old_qid);
        }
        self.current_query_id += 1;
        let query_id = self.current_query_id;
        self.in_flight_query_id = Some(query_id);
        self.last_dispatch_at = Some(now);
        self.reset_scroll_pending = command.reset_scroll_on_apply;
        transport.send_query(query_id, command);
        query_id
    }

    pub fn abort_waiting_on_error(&mut self, query_id: u64) -> bool {
        if query_id != self.current_query_id {
            return false;
        }
        self.in_flight_query_id = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    struct ManualClock {
        origin: Instant,
        elapsed: Cell<Duration>,
    }

    impl ManualClock {
        fn new() -> Self {
            Self {
                origin: Instant::now(),
                elapsed: Cell::new(Duration::ZERO),
            }
        }

        fn advance(&self, delta: Duration) {
            self.elapsed.set(self.elapsed.get() + delta);
        }
    }

    impl Clock for ManualClock {
        fn now(&self) -> Instant {
            self.origin + self.elapsed.get()
        }
    }

    #[derive(Default)]
    struct RecordingTransport {
        events: Vec<TransportEvent>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum TransportEvent {
        Cancel(u64),
        Query { query_id: u64, text: String },
    }

    impl ServiceTransport for RecordingTransport {
        fn send_cancel(&mut self, query_id: u64) {
            self.events.push(TransportEvent::Cancel(query_id));
        }

        fn send_query(&mut self, query_id: u64, command: SearchCommand) {
            self.events.push(TransportEvent::Query {
                query_id,
                text: command.query,
            });
        }
    }

    fn cmd(text: &str, reset_scroll: bool) -> SearchCommand {
        SearchCommand {
            query: text.to_string(),
            defaults: QueryDefaults {
                case_sensitive: false,
                whole_words: false,
                match_path: false,
                is_regex: false,
                content: false,
                exclude_hidden: false,
                exclude_system: false,
            },
            sort_column: SortColumn::Name,
            sort_asc: true,
            page_size: 100,
            reset_scroll_on_apply: reset_scroll,
        }
    }

    #[test]
    fn leading_edit_dispatches_immediately_and_burst_edits_coalesce_with_cancel() {
        let clock = ManualClock::new();
        let mut transport = RecordingTransport::default();
        let mut coord = SearchCoordinator::new(Duration::from_millis(50));

        let first = coord.schedule_text_edit(cmd("c", true), &clock, &mut transport);
        assert_eq!(first, SearchPlan::Dispatched { query_id: 1 });
        assert!(coord.is_searching());

        clock.advance(Duration::from_millis(15));
        let second = coord.schedule_text_edit(cmd("ca", true), &clock, &mut transport);
        let token1 = match second {
            SearchPlan::Debounced { token, delay } => {
                assert_eq!(delay, Duration::from_millis(50));
                token
            }
            other => panic!("expected debounce on rapid keystroke, got {other:?}"),
        };

        clock.advance(Duration::from_millis(15));
        let third = coord.schedule_text_edit(cmd("car", true), &clock, &mut transport);
        let token2 = match third {
            SearchPlan::Debounced { token, .. } => token,
            other => panic!("expected debounce on rapid keystroke, got {other:?}"),
        };
        assert_ne!(token1, token2);

        // Stale timer token must do nothing.
        clock.advance(Duration::from_millis(50));
        assert_eq!(coord.flush_debounced(token1, &clock, &mut transport), None);

        // Current timer fires: cancels query_id 1 before sending query_id 2 ("car").
        assert_eq!(
            coord.flush_debounced(token2, &clock, &mut transport),
            Some(2)
        );
        assert_eq!(
            transport.events,
            vec![
                TransportEvent::Query {
                    query_id: 1,
                    text: "c".to_string()
                },
                TransportEvent::Cancel(1),
                TransportEvent::Query {
                    query_id: 2,
                    text: "car".to_string()
                },
            ]
        );
        assert_eq!(coord.accept_response(1), None);
        assert_eq!(coord.accept_response(2), Some(true));
        assert!(!coord.is_searching());
    }

    #[test]
    fn immediate_trigger_supersedes_pending_debounce_and_cancels_in_flight() {
        let clock = ManualClock::new();
        let mut transport = RecordingTransport::default();
        let mut coord = SearchCoordinator::new(Duration::from_millis(50));

        assert_eq!(
            coord.dispatch_immediate(cmd("alpha", true), &clock, &mut transport),
            1
        );
        clock.advance(Duration::from_millis(10));
        let debounced = coord.schedule_text_edit(cmd("alpha b", true), &clock, &mut transport);
        let stale_token = match debounced {
            SearchPlan::Debounced { token, .. } => token,
            other => panic!("expected debounce while in flight, got {other:?}"),
        };

        // Explicit trigger (Enter / filter change / sort) runs immediately and kills stale token.
        let qid = coord.dispatch_immediate(cmd("beta", true), &clock, &mut transport);
        assert_eq!(qid, 2);
        assert_eq!(
            coord.flush_debounced(stale_token, &clock, &mut transport),
            None
        );
        assert_eq!(
            transport.events,
            vec![
                TransportEvent::Query {
                    query_id: 1,
                    text: "alpha".to_string()
                },
                TransportEvent::Cancel(1),
                TransportEvent::Query {
                    query_id: 2,
                    text: "beta".to_string()
                },
            ]
        );
    }

    #[test]
    fn cancelling_query_advances_id_and_blocks_background_refresh() {
        let clock = ManualClock::new();
        let mut transport = RecordingTransport::default();
        let mut coord = SearchCoordinator::new(Duration::from_millis(50));

        assert_eq!(
            coord.dispatch_immediate(cmd("report", true), &clock, &mut transport),
            1
        );
        assert_eq!(coord.cancel_in_flight(&mut transport), Some(1));
        assert!(coord.is_cancelled());
        assert!(!coord.is_searching());
        assert!(!coord.can_background_refresh());
        assert_eq!(coord.current_query_id(), 2);
        assert_eq!(coord.accept_response(1), None);

        coord.set_cancelled(false);
        assert!(coord.can_background_refresh());
        assert_eq!(
            coord.dispatch_immediate(cmd("report", false), &clock, &mut transport),
            3
        );
        assert_eq!(coord.accept_response(3), Some(false));
    }
}
