use crate::motion::{RESIZE_MAX_WAIT, RESIZE_POLL, RESIZE_QUIET};
use std::time::{Duration, Instant};

/// Poll interval for the settle watcher.
pub(crate) const SETTLE_POLL: Duration = RESIZE_POLL;

/// A settle watcher that never sees quiet input gives up after this many polls.
const MAX_POLLS: usize = (RESIZE_MAX_WAIT.as_millis() / RESIZE_POLL.as_millis()) as usize;

/// A live window resize and the cover it owns.
///
/// The drag is judged by this window's own geometry: a size that stops changing
/// for [`RESIZE_QUIET`] has settled. Global pointer state cannot answer that — a left
/// button held in another program would keep this window's cover up while the
/// window itself sits still.
///
/// Every decision takes the clock as an argument, so the state machine is
/// exercised without waiting on a timer.
#[derive(Debug)]
pub(crate) struct WindowResizeTracker {
    /// The size the last frame reported, or `None` before the first frame.
    observed: Option<(f32, f32)>,
    phase: WindowResizePhase,
    /// Generation of the most recent resize, whether or not it is still active.
    epoch: u64,
}

#[derive(Debug)]
enum WindowResizePhase {
    Uncovered,
    Covered(Active),
    Dissolving(u64),
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
            observed: None,
            phase: WindowResizePhase::Uncovered,
            epoch: 0,
        }
    }
}

impl WindowResizeTracker {
    /// Records the size this frame measured. A delta under half a pixel is the
    /// same size: the layout is expressed in logical pixels and rounding noise
    /// must not read as a drag that never stops.
    pub(crate) fn observe(&mut self, size: (f32, f32), now: Instant) -> Observed {
        let changed = self.observed.is_some_and(|previous| {
            (previous.0 - size.0).abs() > 0.5 || (previous.1 - size.1).abs() > 0.5
        });
        self.observed = Some(size);
        // The first size the window reports is its starting geometry, not a drag.
        if self.epoch == 0 {
            self.epoch = 1;
            return Observed::Unchanged;
        }
        if !changed {
            return Observed::Unchanged;
        }
        match &mut self.phase {
            WindowResizePhase::Covered(active) => {
                active.last_change = now;
                Observed::Continued
            }
            WindowResizePhase::Uncovered | WindowResizePhase::Dissolving(_) => {
                self.epoch += 1;
                self.phase = WindowResizePhase::Covered(Active {
                    epoch: self.epoch,
                    last_change: now,
                });
                Observed::Started(self.epoch)
            }
        }
    }

    /// Whether the content is under the solid cover right now.
    pub(crate) fn is_covered(&self) -> bool {
        matches!(self.phase, WindowResizePhase::Covered(_))
    }

    /// Whether the running resize has stopped moving.
    pub(crate) fn is_quiet(&self, now: Instant) -> bool {
        match &self.phase {
            WindowResizePhase::Covered(active) => {
                now.saturating_duration_since(active.last_change) >= RESIZE_QUIET
            }
            WindowResizePhase::Uncovered | WindowResizePhase::Dissolving(_) => false,
        }
    }

    /// The running resize's generation, which its watcher compares against so a
    /// superseded resize cannot settle the one now in progress.
    pub(crate) fn active_epoch(&self) -> Option<u64> {
        match &self.phase {
            WindowResizePhase::Covered(active) => Some(active.epoch),
            WindowResizePhase::Uncovered | WindowResizePhase::Dissolving(_) => None,
        }
    }

    /// Drops the solid cover for a settled resize and hands its generation to
    /// the dissolve. Reduced motion removes the cover immediately.
    pub(crate) fn settle(&mut self, epoch: u64, reduce_motion: bool) -> bool {
        if self.active_epoch() != Some(epoch) {
            return false;
        }
        self.phase = if reduce_motion {
            WindowResizePhase::Uncovered
        } else {
            WindowResizePhase::Dissolving(epoch)
        };
        true
    }

    /// The dissolve on screen, or `None` when there is none.
    pub(crate) fn fading_epoch(&self) -> Option<u64> {
        match &self.phase {
            WindowResizePhase::Dissolving(epoch) => Some(*epoch),
            WindowResizePhase::Uncovered | WindowResizePhase::Covered(_) => None,
        }
    }

    /// Ends a dissolve once its animation has run out. A newer resize keeps its own.
    pub(crate) fn clear_fade(&mut self, epoch: u64) -> bool {
        if self.fading_epoch() == Some(epoch) {
            self.phase = WindowResizePhase::Uncovered;
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
        assert!(!tracker.settle(epoch + 1, false));
        assert!(
            tracker.is_covered(),
            "a stale settle must not drop the cover"
        );
        assert!(tracker.settle(epoch, false));
        assert!(!tracker.is_covered());
        assert_eq!(tracker.fading_epoch(), Some(epoch));
        assert!(tracker.clear_fade(epoch));
        assert_eq!(tracker.fading_epoch(), None);
    }

    #[test]
    fn a_quiet_drag_settles_only_after_the_quiet_window_elapses() {
        let start = Instant::now();
        let mut tracker = WindowResizeTracker::default();
        tracker.observe((1280.0, 800.0), start);
        tracker.observe((1200.0, 800.0), start);
        assert!(tracker.is_covered());
        assert!(!tracker.is_quiet(start + Duration::from_millis(140)));
        assert!(tracker.is_quiet(start + RESIZE_QUIET));
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
        let first_epoch = match first {
            Observed::Started(epoch) => epoch,
            other => panic!("expected a started resize, got {other:?}"),
        };
        assert!(tracker.settle(first_epoch, false));
        assert_eq!(
            tracker.observe((1100.0, 800.0), start + Duration::from_millis(400)),
            Observed::Started(3)
        );
        assert!(tracker.is_covered());
    }

    #[test]
    fn an_old_dissolve_cannot_clear_a_new_cover() {
        let start = Instant::now();
        let mut tracker = WindowResizeTracker::default();
        tracker.observe((1280.0, 800.0), start);
        let first = tracker.observe((1200.0, 800.0), start);
        let first_epoch = match first {
            Observed::Started(epoch) => epoch,
            other => panic!("expected a started resize, got {other:?}"),
        };
        assert!(tracker.settle(first_epoch, false));
        assert_eq!(tracker.fading_epoch(), Some(first_epoch));

        assert_eq!(
            tracker.observe((1100.0, 800.0), start + Duration::from_millis(200)),
            Observed::Started(3)
        );
        assert!(!tracker.clear_fade(first_epoch));
        assert!(tracker.is_covered());
        assert_eq!(tracker.active_epoch(), Some(3));
    }

    #[test]
    fn reduced_motion_removes_a_settled_cover_without_a_dissolve() {
        let start = Instant::now();
        let mut tracker = WindowResizeTracker::default();
        tracker.observe((1280.0, 800.0), start);
        let epoch = match tracker.observe((1200.0, 800.0), start) {
            Observed::Started(epoch) => epoch,
            other => panic!("expected a started resize, got {other:?}"),
        };

        assert!(tracker.settle(epoch, true));
        assert!(!tracker.is_covered());
        assert_eq!(tracker.fading_epoch(), None);
    }
}
