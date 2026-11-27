// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Presentation-only motion state and element builders for the installer and uninstaller.

use std::time::Duration;

use cairn_ui::brand::tabular_figures;
use gpui_kit::base::motion::{Presence, Transition};
use gpui_kit::component::h_flex;
use gpui_kit::gpui::{
    AnyElement, App, ElementId, Hsla, InteractiveElement as _, IntoElement as _,
    ParentElement as _, Pixels, Role, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, px,
};

use crate::model::PathProblem;

/// Marker passed to Fluent's `progress_percent` template to split prefix and suffix affixes
/// around the rolling digit slots without hardcoding locale ordering.
pub(crate) const PERCENT_DIGITS_MARKER: &str = "\u{E001}";

pub(crate) const PAGE_EXIT: Duration = Duration::from_millis(120);
pub(crate) const PAGE_ENTER: Duration = Duration::from_millis(220);
pub(crate) const FOOTER_EXIT: Duration = Duration::from_millis(90);
pub(crate) const FOOTER_ENTER: Duration = Duration::from_millis(160);
pub(crate) const FOOTER_ENTER_DELAY: Duration = Duration::from_millis(20);
pub(crate) const NOTICE_ENTER: Duration = Duration::from_millis(120);
pub(crate) const STAGE_EXIT: Duration = Duration::from_millis(90);
pub(crate) const STAGE_ENTER: Duration = Duration::from_millis(140);
pub(crate) const STAGE_ENTER_DELAY: Duration = Duration::from_millis(20);
pub(crate) const CAPTION_SLIDE: Pixels = px(4.0);
pub(crate) const DIGIT_ROLL: Duration = Duration::from_millis(140);
pub(crate) const DIGIT_TRAVEL: f32 = 0.65;
pub(crate) const INLINE_APPEAR: Duration = Duration::from_millis(120);
pub(crate) const COMPLETION_FADE: Duration = Duration::from_millis(120);
pub(crate) const COMPLETION_HOLD: Duration = Duration::from_millis(240);
pub(crate) const PROGRESS_SWEEP: Duration = Duration::from_millis(1800);

pub(crate) fn fade_enter(duration: Duration) -> Transition {
    cairn_ui::motion::flow_enter(duration)
}

pub(crate) fn fade_exit(duration: Duration) -> Transition {
    cairn_ui::motion::flow_exit(duration)
}

/// Tracks settled caption and incoming replacement across stage updates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CaptionSwap {
    current: SharedString,
    previous: Option<SharedString>,
    epoch: u64,
    counter: u64,
}

impl CaptionSwap {
    fn new(initial: impl Into<SharedString>) -> Self {
        Self {
            current: initial.into(),
            previous: None,
            epoch: 0,
            counter: 0,
        }
    }

    pub(crate) fn current(&self) -> &SharedString {
        &self.current
    }

    fn reset(&mut self, initial: impl Into<SharedString>) {
        self.current = initial.into();
        self.previous = None;
        self.epoch = 0;
    }

    fn advance(&mut self, next: impl Into<SharedString>) {
        let next = next.into();
        if next == self.current {
            return;
        }
        self.counter += 1;
        self.previous = Some(self.current.clone());
        self.current = next;
        self.epoch = self.counter;
    }
}

/// One digit column inside [`PercentReel`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DigitSlot {
    pub(crate) previous: Option<u8>,
    pub(crate) current: u8,
    pub(crate) epoch: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReelState {
    Hidden,
    Visible {
        appear_epoch: u64,
        value: u8,
        hundreds: Option<DigitSlot>,
        tens: Option<DigitSlot>,
        ones: DigitSlot,
    },
}

/// Tracks percentage readout visibility and independent per-digit vertical transitions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PercentReel {
    appear_counter: u64,
    digit_counter: u64,
    state: ReelState,
}

impl PercentReel {
    fn new(initial: Option<f32>) -> Self {
        let state = match initial {
            Some(value) => {
                let value = quantize_percent(value);
                ReelState::Visible {
                    appear_epoch: 0,
                    value,
                    hundreds: (value >= 100).then_some(DigitSlot {
                        previous: None,
                        current: value / 100,
                        epoch: 0,
                    }),
                    tens: (value >= 10).then_some(DigitSlot {
                        previous: None,
                        current: value / 10 % 10,
                        epoch: 0,
                    }),
                    ones: DigitSlot {
                        previous: None,
                        current: value % 10,
                        epoch: 0,
                    },
                }
            }
            None => ReelState::Hidden,
        };
        Self {
            appear_counter: 0,
            digit_counter: 0,
            state,
        }
    }

    fn reset(&mut self) {
        self.state = ReelState::Hidden;
    }

    fn advance_slot(counter: &mut u64, slot: &mut Option<DigitSlot>, next: Option<u8>) {
        match (slot.take(), next) {
            (_, None) => {}
            (None, Some(digit)) => {
                *counter += 1;
                *slot = Some(DigitSlot {
                    previous: None,
                    current: digit,
                    epoch: *counter,
                });
            }
            (Some(current), Some(digit)) if current.current == digit => *slot = Some(current),
            (Some(current), Some(digit)) => {
                *counter += 1;
                *slot = Some(DigitSlot {
                    previous: Some(current.current),
                    current: digit,
                    epoch: *counter,
                });
            }
        }
    }

    fn update(&mut self, raw: f32) {
        let next = quantize_percent(raw);
        let next_hundreds = (next >= 100).then_some(next / 100);
        let next_tens = (next >= 10).then_some(next / 10 % 10);
        let next_ones = next % 10;

        match &mut self.state {
            ReelState::Hidden => {
                self.appear_counter += 1;
                self.state = ReelState::Visible {
                    appear_epoch: self.appear_counter,
                    value: next,
                    hundreds: next_hundreds.map(|digit| DigitSlot {
                        previous: None,
                        current: digit,
                        epoch: 0,
                    }),
                    tens: next_tens.map(|digit| DigitSlot {
                        previous: None,
                        current: digit,
                        epoch: 0,
                    }),
                    ones: DigitSlot {
                        previous: None,
                        current: next_ones,
                        epoch: 0,
                    },
                };
            }
            ReelState::Visible {
                value,
                hundreds,
                tens,
                ones,
                ..
            } => {
                if *value == next {
                    return;
                }
                Self::advance_slot(&mut self.digit_counter, hundreds, next_hundreds);
                Self::advance_slot(&mut self.digit_counter, tens, next_tens);
                if ones.current != next_ones {
                    self.digit_counter += 1;
                    *ones = DigitSlot {
                        previous: Some(ones.current),
                        current: next_ones,
                        epoch: self.digit_counter,
                    };
                }
                *value = next;
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn value(&self) -> Option<u8> {
        match self.state {
            ReelState::Hidden => None,
            ReelState::Visible { value, .. } => Some(value),
        }
    }

    #[cfg(test)]
    pub(crate) fn slots(&self) -> Option<(Option<DigitSlot>, Option<DigitSlot>, DigitSlot)> {
        match self.state {
            ReelState::Hidden => None,
            ReelState::Visible {
                hundreds,
                tens,
                ones,
                ..
            } => Some((hundreds, tens, ones)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PagePhase {
    Settled,
    Exiting,
    Entering,
}

/// All animation epochs and reel state owned by [`crate::app::SetupApp`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SetupMotion {
    transition_epoch: u64,
    page_phase: PagePhase,
    completion_epoch: u64,
    path_error: Option<PathProblem>,
    path_notice_epoch: u64,
    caption: CaptionSwap,
    percent: PercentReel,
}

impl SetupMotion {
    pub(crate) fn new(
        initial_caption: impl Into<SharedString>,
        initial_progress: Option<f32>,
    ) -> Self {
        Self {
            transition_epoch: 0,
            page_phase: PagePhase::Settled,
            completion_epoch: 0,
            path_error: None,
            path_notice_epoch: 0,
            caption: CaptionSwap::new(initial_caption),
            percent: PercentReel::new(initial_progress),
        }
    }

    pub(crate) fn begin_exit(&mut self) {
        self.transition_epoch += 1;
        self.page_phase = PagePhase::Exiting;
    }

    pub(crate) fn begin_enter(&mut self) {
        self.page_phase = PagePhase::Entering;
    }

    pub(crate) fn settle(&mut self) {
        self.page_phase = PagePhase::Settled;
    }

    pub(crate) fn set_path_error(&mut self, next: Option<PathProblem>) {
        if next != self.path_error {
            self.path_error = next;
            self.path_notice_epoch += 1;
        }
    }

    pub(crate) fn reset_progress(&mut self, initial_caption: impl Into<SharedString>) {
        self.caption.reset(initial_caption);
        self.percent.reset();
    }

    pub(crate) fn set_caption(&mut self, next: impl Into<SharedString>) {
        self.caption.advance(next);
    }

    pub(crate) fn set_progress(&mut self, value: f32) {
        self.percent.update(value);
    }

    pub(crate) fn transition_epoch(&self) -> u64 {
        self.transition_epoch
    }

    pub(crate) fn page_phase(&self) -> PagePhase {
        self.page_phase
    }

    pub(crate) fn is_settled(&self) -> bool {
        self.page_phase == PagePhase::Settled
    }

    pub(crate) fn is_transitioning(&self) -> bool {
        self.page_phase != PagePhase::Settled
    }

    pub(crate) fn begin_completion(&mut self) {
        self.completion_epoch += 1;
    }

    pub(crate) fn completion_epoch(&self) -> u64 {
        self.completion_epoch
    }

    pub(crate) fn path_notice_epoch(&self) -> u64 {
        self.path_notice_epoch
    }

    pub(crate) fn caption(&self) -> &CaptionSwap {
        &self.caption
    }

    pub(crate) fn percent(&self) -> &PercentReel {
        &self.percent
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DigitRollFrame {
    pub(crate) incoming_y: Pixels,
    pub(crate) outgoing_y: Pixels,
    pub(crate) incoming_opacity: f32,
    pub(crate) outgoing_opacity: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CaptionFrames {
    pub(crate) incoming_y: Pixels,
    pub(crate) outgoing_y: Pixels,
    pub(crate) incoming_opacity: f32,
    pub(crate) outgoing_opacity: f32,
}

pub(crate) fn quantize_percent(raw: f32) -> u8 {
    raw.round().clamp(0.0, 100.0) as u8
}

pub(crate) fn split_percent_pattern(formatted: &str) -> (&str, &str) {
    match formatted.split_once(PERCENT_DIGITS_MARKER) {
        Some((prefix, suffix)) => (
            prefix.trim_end_matches(['\u{2068}', '\u{2069}']),
            suffix.trim_start_matches(['\u{2068}', '\u{2069}']),
        ),
        None => ("", formatted.trim_matches(['\u{2068}', '\u{2069}'])),
    }
}

pub(crate) fn digit_roll_frame(progress: f32, slot_height: Pixels) -> DigitRollFrame {
    let p = progress.clamp(0.0, 1.0);
    let travel = slot_height * DIGIT_TRAVEL;
    DigitRollFrame {
        incoming_y: travel * (1.0 - p),
        outgoing_y: -travel * p,
        incoming_opacity: ((p - 0.1) / 0.9).clamp(0.0, 1.0),
        outgoing_opacity: (1.0 - p * 1.25).clamp(0.0, 1.0),
    }
}

pub(crate) fn caption_swap_frames(exit_progress: f32, enter_progress: f32) -> CaptionFrames {
    let exit_p = exit_progress.clamp(0.0, 1.0);
    let enter_p = enter_progress.clamp(0.0, 1.0);
    CaptionFrames {
        incoming_y: CAPTION_SLIDE * (1.0 - enter_p),
        outgoing_y: -CAPTION_SLIDE * exit_p,
        incoming_opacity: enter_p,
        outgoing_opacity: 1.0 - exit_p,
    }
}

/// Samples a `0.0 → 1.0` transition keyed by `(prefix, epoch)`. Epoch `0` settles immediately so
/// the window's initial open frame never flashes from transparent.
pub(crate) fn sample_progress(
    prefix: &'static str,
    epoch: u64,
    transition: Transition,
    window: &mut Window,
    cx: &mut App,
) -> f32 {
    if epoch == 0 {
        return 1.0;
    }
    Presence::new(ElementId::NamedInteger(prefix.into(), epoch), true)
        .transition(transition)
        .sample(window, cx)
        .progress
        .clamp(0.0, 1.0)
}

pub(crate) fn sample_fade(
    prefix: &'static str,
    epoch: u64,
    duration: Duration,
    window: &mut Window,
    cx: &mut App,
) -> f32 {
    sample_progress(prefix, epoch, fade_enter(duration), window, cx)
}

pub(crate) fn render_stage_caption(
    swap: &CaptionSwap,
    text_color: Hsla,
    line_height: Pixels,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let container = div()
        .id("setup-stage-caption")
        .role(Role::Status)
        .relative()
        .flex_1()
        .min_w_0()
        .h(line_height)
        .overflow_hidden();

    if swap.epoch == 0 || cx.reduce_motion() {
        return container
            .child(
                div()
                    .text_sm()
                    .line_height(line_height)
                    .text_color(text_color)
                    .child(swap.current.clone()),
            )
            .into_any_element();
    }

    let exit_p = sample_progress(
        "setup-stage-out",
        swap.epoch,
        fade_exit(STAGE_EXIT),
        window,
        cx,
    );

    let enter_p = sample_progress(
        "setup-stage-in",
        swap.epoch,
        fade_enter(STAGE_ENTER).delay(STAGE_ENTER_DELAY),
        window,
        cx,
    );

    let frames = caption_swap_frames(exit_p, enter_p);
    let mut layer = container;
    if exit_p < 1.0
        && let Some(prev) = &swap.previous
    {
        layer = layer.child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .h(line_height)
                .top(frames.outgoing_y)
                .opacity(frames.outgoing_opacity)
                .text_sm()
                .line_height(line_height)
                .text_color(text_color)
                .child(prev.clone()),
        );
    }

    layer
        .child(
            div()
                .relative()
                .h(line_height)
                .top(frames.incoming_y)
                .opacity(frames.incoming_opacity)
                .text_sm()
                .line_height(line_height)
                .text_color(text_color)
                .child(swap.current.clone()),
        )
        .into_any_element()
}

pub(crate) fn render_percent_reel(
    reel: &PercentReel,
    pattern: &str,
    text_color: Hsla,
    line_height: Pixels,
    window: &mut Window,
    cx: &mut App,
) -> Option<AnyElement> {
    let ReelState::Visible {
        appear_epoch,
        hundreds,
        tens,
        ones,
        ..
    } = reel.state
    else {
        return None;
    };

    let appear_alpha = sample_fade(
        "setup-percent-appear",
        appear_epoch,
        INLINE_APPEAR,
        window,
        cx,
    );
    let (prefix, suffix) = split_percent_pattern(pattern);

    let mut row = h_flex()
        .items_center()
        .flex_shrink_0()
        .h(line_height)
        .text_sm()
        .line_height(line_height)
        .text_color(text_color)
        .font_features(tabular_figures())
        .opacity(appear_alpha);

    if !prefix.is_empty() {
        row = row.child(prefix.to_string());
    }
    if let Some(hundreds) = hundreds {
        row = row.child(render_digit_slot(
            "setup-digit-hundreds",
            hundreds,
            line_height,
            window,
            cx,
        ));
    }
    if let Some(tens) = tens {
        row = row.child(render_digit_slot(
            "setup-digit-tens",
            tens,
            line_height,
            window,
            cx,
        ));
    }
    row = row.child(render_digit_slot(
        "setup-digit-ones",
        ones,
        line_height,
        window,
        cx,
    ));
    if !suffix.is_empty() {
        row = row.child(suffix.to_string());
    }

    Some(row.into_any_element())
}

fn render_digit_slot(
    prefix: &'static str,
    slot: DigitSlot,
    slot_height: Pixels,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let wrapper = div()
        .relative()
        .h(slot_height)
        .line_height(slot_height)
        .overflow_hidden();

    if slot.epoch == 0 || cx.reduce_motion() {
        return wrapper.child(digit_str(slot.current)).into_any_element();
    }

    let progress = sample_fade(prefix, slot.epoch, DIGIT_ROLL, window, cx);
    let frame = digit_roll_frame(progress, slot_height);

    let mut el = wrapper;
    if progress < 1.0
        && let Some(prev) = slot.previous
    {
        el = el.child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .h(slot_height)
                .line_height(slot_height)
                .top(frame.outgoing_y)
                .opacity(frame.outgoing_opacity)
                .child(digit_str(prev)),
        );
    }

    el.child(
        div()
            .relative()
            .h(slot_height)
            .line_height(slot_height)
            .top(frame.incoming_y)
            .opacity(frame.incoming_opacity)
            .child(digit_str(slot.current)),
    )
    .into_any_element()
}

fn digit_str(digit: u8) -> &'static str {
    match digit {
        0 => "0",
        1 => "1",
        2 => "2",
        3 => "3",
        4 => "4",
        5 => "5",
        6 => "6",
        7 => "7",
        8 => "8",
        9 => "9",
        _ => "0",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PERCENT_DIGITS_MARKER, PercentReel, SetupMotion, caption_swap_frames, digit_roll_frame,
        split_percent_pattern,
    };
    use gpui_kit::gpui::px;

    #[test]
    fn screen_transitions_are_explicit_and_settle() {
        let mut motion = SetupMotion::new("Preparing", None);
        assert!(motion.is_settled());

        motion.begin_exit();
        assert!(motion.is_transitioning());
        motion.begin_enter();
        assert!(motion.is_transitioning());
        motion.settle();
        assert!(motion.is_settled());
        motion.begin_exit();
        assert!(motion.is_transitioning());
    }

    #[test]
    fn caption_swap_deduplicates_and_preserves_monotonic_epochs_across_reset() {
        let mut motion = SetupMotion::new("Preparing", None);
        let initial_epoch = motion.caption().epoch;
        assert_eq!(motion.caption().previous, None);

        motion.set_caption("Preparing");
        assert_eq!(motion.caption().epoch, initial_epoch);

        motion.set_caption("Copying files");
        assert_ne!(motion.caption().epoch, initial_epoch);
        assert_eq!(motion.caption().previous.as_deref(), Some("Preparing"));
        assert_eq!(motion.caption().current().as_ref(), "Copying files");
        let changed_epoch = motion.caption().epoch;

        motion.reset_progress("Preparing");
        assert_eq!(motion.caption().previous, None);

        motion.set_caption("Finishing");
        assert_ne!(motion.caption().epoch, changed_epoch);
        assert_eq!(motion.caption().previous.as_deref(), Some("Preparing"));
        assert_eq!(motion.caption().current().as_ref(), "Finishing");
    }

    #[test]
    fn percent_reel_rolls_only_changed_digits_and_keeps_epochs_monotonic() {
        let mut reel = PercentReel::new(None);
        assert_eq!(reel.value(), None);

        // Initial appearance fades in without rolling from a fake prior digit.
        reel.update(8.0);
        let (_, tens, ones) = reel.slots().expect("visible at 8%");
        assert!(tens.is_none());
        assert_eq!(ones.current, 8);
        assert_eq!(ones.previous, None);
        let ones_epoch = ones.epoch;

        // 8 -> 9 rolls ones only.
        reel.update(9.0);
        let (_, tens, ones) = reel.slots().expect("visible at 9%");
        assert!(tens.is_none());
        assert_eq!(ones.previous, Some(8));
        assert_eq!(ones.current, 9);
        assert!(ones.epoch > ones_epoch);
        let ones_9_epoch = ones.epoch;

        // 9 -> 10 introduces tens from below and rolls ones 9 -> 0.
        reel.update(10.0);
        let (_, tens, ones) = reel.slots().expect("visible at 10%");
        let tens = tens.expect("tens slot");
        assert_eq!(tens.previous, None);
        assert_eq!(tens.current, 1);
        let tens_epoch = tens.epoch;
        assert!(tens_epoch > ones_9_epoch);
        assert_eq!(ones.previous, Some(9));
        assert_eq!(ones.current, 0);
        assert!(ones.epoch > tens_epoch);

        // 10 -> 14 keeps tens stationary and rolls ones 0 -> 4.
        reel.update(14.0);
        let (_, tens_14, ones_14) = reel.slots().expect("visible at 14%");
        let tens_14 = tens_14.expect("tens slot");
        assert_eq!(tens_14.epoch, tens_epoch);
        assert_eq!(ones_14.previous, Some(0));
        assert_eq!(ones_14.current, 4);
        assert!(ones_14.epoch > ones.epoch);

        // Fractional delta within the same rounded integer is ignored.
        reel.update(14.4);
        assert_eq!(reel.slots(), Some((None, Some(tens_14), ones_14)));

        // 14 -> 24 rolls tens 1 -> 2 and keeps ones stationary.
        reel.update(24.0);
        let (_, tens_24, ones_24) = reel.slots().expect("visible at 24%");
        let tens_24 = tens_24.expect("tens slot");
        assert_eq!(tens_24.previous, Some(1));
        assert_eq!(tens_24.current, 2);
        assert!(tens_24.epoch > ones_14.epoch);
        assert_eq!(ones_24, ones_14);

        // 24 -> 68 rolls both columns once.
        reel.update(68.0);
        let (_, tens_68, ones_68) = reel.slots().expect("visible at 68%");
        let tens_68 = tens_68.expect("tens slot");
        assert_eq!(tens_68.previous, Some(2));
        assert_eq!(tens_68.current, 6);
        assert_eq!(ones_68.previous, Some(4));
        assert_eq!(ones_68.current, 8);
        assert!(ones_68.epoch > tens_68.epoch);

        // Resetting for a retry hides the reel while keeping epochs strictly newer next run.
        let prev_max_epoch = ones_68.epoch;
        reel.reset();
        assert_eq!(reel.value(), None);
        reel.update(12.0);
        reel.update(13.0);
        let (_, _, ones_retry) = reel.slots().expect("visible on retry");
        assert!(ones_retry.epoch > prev_max_epoch);

        // Initialized from a static value starts without a roll.
        let preset = PercentReel::new(Some(42.0));
        let (_, preset_tens, preset_ones) = preset.slots().expect("preset visible");
        assert_eq!(preset_tens.expect("tens").current, 4);
        assert_eq!(preset_ones.current, 2);
        assert_eq!(preset_ones.previous, None);
    }

    #[test]
    fn percent_reel_adds_hundreds_only_at_completion() {
        let mut reel = PercentReel::new(None);
        reel.update(99.0);
        let (hundreds, tens, ones) = reel.slots().expect("visible at 99%");
        assert!(hundreds.is_none());
        assert_eq!(tens.expect("tens slot").current, 9);
        assert_eq!(ones.current, 9);

        reel.update(100.0);
        let (hundreds, tens, ones) = reel.slots().expect("visible at 100%");
        assert_eq!(hundreds.expect("hundreds slot").current, 1);
        assert_eq!(tens.expect("tens slot").previous, Some(9));
        assert_eq!(tens.expect("tens slot").current, 0);
        assert_eq!(ones.previous, Some(9));
        assert_eq!(ones.current, 0);
    }

    #[test]
    fn percent_pattern_splitting_strips_fluent_isolate_markers() {
        let isolated = format!("\u{2068}{PERCENT_DIGITS_MARKER}\u{2069}%");
        assert_eq!(split_percent_pattern(&isolated), ("", "%"));
        let prefixed = format!("% \u{2068}{PERCENT_DIGITS_MARKER}\u{2069}");
        assert_eq!(split_percent_pattern(&prefixed), ("% ", ""));
    }

    #[test]
    fn optical_roll_and_caption_frames_hit_exact_endpoints() {
        let height = px(20.0);
        let start = digit_roll_frame(0.0, height);
        assert_eq!(start.outgoing_y, px(0.0));
        assert_eq!(start.outgoing_opacity, 1.0);
        assert!(start.incoming_y > px(0.0));
        assert_eq!(start.incoming_opacity, 0.0);

        let end = digit_roll_frame(1.0, height);
        assert_eq!(end.incoming_y, px(0.0));
        assert_eq!(end.incoming_opacity, 1.0);
        assert_eq!(end.outgoing_opacity, 0.0);

        let cap_start = caption_swap_frames(0.0, 0.0);
        assert_eq!(cap_start.outgoing_y, px(0.0));
        assert_eq!(cap_start.outgoing_opacity, 1.0);
        assert_eq!(cap_start.incoming_opacity, 0.0);

        let cap_end = caption_swap_frames(1.0, 1.0);
        assert_eq!(cap_end.incoming_y, px(0.0));
        assert_eq!(cap_end.incoming_opacity, 1.0);
        assert_eq!(cap_end.outgoing_opacity, 0.0);
    }
}
