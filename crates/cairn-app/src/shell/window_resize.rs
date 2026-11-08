use std::time::{Duration, Instant};

/// How long the window size must stop changing before the drag counts as settled.
const QUIET: Duration = Duration::from_millis(150);

/// Poll interval for the settle watcher.
pub(crate) const SETTLE_POLL: Duration = QUIET;

/// A settle watcher that never sees quiet input gives up after this many polls.
const MAX_POLLS: usize = 400;

/// A live window resize and the cover it owns.
///
/// The drag is judged by this window's own geometry: a size that stops changing
/// for [`QUIET`] has settled. Global pointer state cannot answer that — a left
/// button held in another program would keep this window's cover up while the
/// window itself sits still.
///
/// Every decision takes the clock as an argument, so the state machine is
/// exercised without waiting on a timer.
#[derive(Debug)]
pub(crate) struct WindowResizeTracker {
    /// The size the last frame reported. `(0, 0)` until the first frame adopts one.
    observed: (f32, f32),
    /// `Some` while the cover is up: the resize this tracker is waiting on.
    active: Option<Active>,
    /// Generation of the most recent resize, whether or not it is still active.
    epoch: u64,
    /// Generation whose dissolve is on screen; `0` means no dissolve.
    fading: u64,
}

#[derive(Debug)]
struct Active {
    epoch: u64,
    last_change: Instant,
}

/// What observing a new window size decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Observed {
    /// The window is the size it already was.
    Unchanged,
    /// A resize the cover was not yet hiding started; its watcher must be armed.
    Started(u64),
    /// The running resize changed size again, so its quiet window restarts.
    Continued,
}

impl Default for WindowResizeTracker {
    fn default() -> Self {
        Self {
            observed: (0.0, 0.0),
            active: None,
            epoch: 0,
            fading: 0,
        }
    }
}

impl WindowResizeTracker {
    /// Records the size this frame measured. A delta under half a pixel is the
    /// same size: the layout is expressed in logical pixels and rounding noise
    /// must not read as a drag that never stops.
    pub(crate) fn observe(&mut self, size: (f32, f32), now: Instant) -> Observed {
        let changed =
            (self.observed.0 - size.0).abs() > 0.5 || (self.observed.1 - size.1).abs() > 0.5;
        self.observed = size;
        if !changed {
            return Observed::Unchanged;
        }
        // The first size the window reports is its starting geometry, not a drag.
        if self.epoch == 0 && self.active.is_none() {
            self.epoch = 1;
            return Observed::Unchanged;
        }
        match &mut self.active {
            Some(active) => {
                active.last_change = now;
                Observed::Continued
            }
            None => {
                self.epoch += 1;
                self.active = Some(Active {
                    epoch: self.epoch,
                    last_change: now,
                });
                Observed::Started(self.epoch)
            }
        }
    }

    /// Whether the content is under the cover right now.
    pub(crate) fn is_covered(&self) -> bool {
        self.active.is_some()
    }

    /// Whether the running resize has stopped moving.
    pub(crate) fn is_quiet(&self, now: Instant) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| now.saturating_duration_since(active.last_change) >= QUIET)
    }

    /// The running resize's generation, which its watcher compares against so a
    /// superseded resize cannot settle the one now in progress.
    pub(crate) fn active_epoch(&self) -> Option<u64> {
        self.active.as_ref().map(|active| active.epoch)
    }

    /// Drops the cover for a settled resize and hands its generation to the dissolve.
    pub(crate) fn settle(&mut self, epoch: u64) -> bool {
        if self.active_epoch() != Some(epoch) {
            return false;
        }
        self.active = None;
        self.fading = epoch;
        true
    }

    /// The dissolve on screen, or `0` when there is none.
    pub(crate) fn fading_epoch(&self) -> u64 {
        self.fading
    }

    /// Ends a dissolve once its animation has run out. A newer resize keeps its own.
    pub(crate) fn clear_fade(&mut self, epoch: u64) -> bool {
        if self.fading == epoch && epoch != 0 {
            self.fading = 0;
            true
        } else {
            false
        }
    }

    /// The largest number of polls a single resize watcher takes before settling anyway.
    pub(crate) fn max_polls() -> usize {
        MAX_POLLS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_size_the_window_reports_is_its_starting_geometry() {
        let start = Instant::now();
        let mut tracker = WindowResizeTracker::default();
        assert_eq!(tracker.observe((1280.0, 800.0), start), Observed::Unchanged);
        assert!(!tracker.is_covered());
    }

    #[test]
    fn a_changing_size_covers_and_a_stopped_size_settles() {
        let start = Instant::now();
        let mut tracker = WindowResizeTracker::default();
        tracker.observe((1280.0, 800.0), start);

        assert_eq!(
            tracker.observe((1200.0, 800.0), start),
            Observed::Started(2)
        );
        assert!(tracker.is_covered());
        // The drag keeps moving, so nothing settles.
        assert_eq!(
            tracker.observe((1100.0, 800.0), start + Duration::from_millis(100)),
            Observed::Continued
        );
        assert!(!tracker.is_quiet(start + Duration::from_millis(200)));

        let epoch = tracker.active_epoch().expect("resize still running");
        assert!(!tracker.settle(epoch + 1));
        assert!(
            tracker.is_covered(),
            "a stale settle must not drop the cover"
        );
        assert!(tracker.settle(epoch));
        assert!(!tracker.is_covered());
        assert_eq!(tracker.fading_epoch(), epoch);
        assert!(tracker.clear_fade(epoch));
        assert_eq!(tracker.fading_epoch(), 0);
    }

    #[test]
    fn a_quiet_drag_settles_only_after_the_quiet_window_elapses() {
        let start = Instant::now();
        let mut tracker = WindowResizeTracker::default();
        tracker.observe((1280.0, 800.0), start);
        tracker.observe((1200.0, 800.0), start);
        assert!(tracker.is_covered());
        assert!(!tracker.is_quiet(start + Duration::from_millis(140)));
        assert!(tracker.is_quiet(start + QUIET));
    }

    #[test]
    fn rounding_noise_is_not_a_drag() {
        let start = Instant::now();
        let mut tracker = WindowResizeTracker::default();
        tracker.observe((1280.0, 800.0), start);
        assert_eq!(
            tracker.observe((1280.4, 800.0), start),
            Observed::Unchanged,
            "sub-pixel movement must not start a cover"
        );
    }

    #[test]
    fn a_second_drag_starts_a_fresh_generation() {
        let start = Instant::now();
        let mut tracker = WindowResizeTracker::default();
        tracker.observe((1280.0, 800.0), start);
        let first = tracker.observe((1200.0, 800.0), start);
        tracker.settle(match first {
            Observed::Started(epoch) => epoch,
            other => panic!("expected a started resize, got {other:?}"),
        });
        assert_eq!(
            tracker.observe((1100.0, 800.0), start + Duration::from_millis(400)),
            Observed::Started(3)
        );
        assert!(tracker.is_covered());
    }
}
