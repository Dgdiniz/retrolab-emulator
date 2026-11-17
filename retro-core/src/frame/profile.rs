//! The profiler: the play tier with the time of each part of a frame call
//! measured, and the time of each part of the machine a core names
//! ([`StepHooks::chip_start`]). It needs a wall clock, so it exists only
//! off WebAssembly.

use std::time::{Duration, Instant};

use crate::debug::MAIN_CPU;

use super::adapter::{DebugMachine, FrameDebugger};
use super::hooks::{Flow, Part, StepHooks, Tier};

/// The most parts of the machine a profile keeps apart; spans of any other
/// part count nothing.
pub const MAX_PROFILED_CHIPS: usize = 12;

/// How many of the spans that run inside steps are timed: one in this many,
/// on average. A span inside a step is a few tens of nanoseconds at most,
/// about the cost of reading the clock twice, so timing each one would make
/// the frame measured mostly the timer's. The ones timed are scaled by it.
/// This is the smallest period that keeps a profiled frame of Sonic the
/// Hedgehog within 10 % of the time a played one takes.
pub const DEFAULT_SAMPLE_PERIOD: u32 = 128;

/// The time one part of the machine took in a frame, and the units of work
/// the core counted for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChipTime {
    /// The name the core gave the part.
    pub chip: &'static str,
    /// Its time in the frame, in nanoseconds.
    pub ns: u64,
    /// The units of work counted for it (instructions for a CPU).
    pub units: u64,
}

/// Where one frame's time went.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameProfile {
    /// The whole frame call, in nanoseconds.
    pub total_ns: u64,
    /// Handling what fell due.
    pub due_ns: u64,
    /// Running steps between deadlines.
    pub run_ns: u64,
    /// Closing the frame.
    pub close_ns: u64,
    /// The instructions the main CPU ran.
    pub instructions: u64,
    /// The time the frame took in the machine's own unit
    /// ([`super::FrameMachine::now`]).
    pub elapsed: u64,
    /// One in how many spans inside steps were timed
    /// ([`DEFAULT_SAMPLE_PERIOD`]); spans of what falls due and of the
    /// frame's end are all timed.
    pub sample_period: u32,
    chips: [ChipTime; MAX_PROFILED_CHIPS],
    chip_count: usize,
}

impl FrameProfile {
    /// The parts of the machine the core named, in the order each was first
    /// seen in the frame.
    pub fn chips(&self) -> &[ChipTime] {
        &self.chips[..self.chip_count]
    }

    /// The slot of `chip`, made if the frame has not seen it yet; `None`
    /// once [`MAX_PROFILED_CHIPS`] are kept.
    fn chip_slot(&mut self, chip: &'static str) -> Option<usize> {
        let seen = &self.chips[..self.chip_count];
        // The same literal is usually the same address: compare that first.
        if let Some(slot) = seen
            .iter()
            .position(|c| std::ptr::eq(c.chip, chip))
            .or_else(|| seen.iter().position(|c| c.chip == chip))
        {
            return Some(slot);
        }
        if self.chip_count == MAX_PROFILED_CHIPS {
            return None;
        }
        let slot = self.chip_count;
        self.chips[slot] = ChipTime {
            chip,
            ns: 0,
            units: 0,
        };
        self.chip_count += 1;
        Some(slot)
    }
}

/// A span begun by [`StepHooks::chip_start`].
#[derive(Debug)]
struct OpenSpan {
    /// The part of the machine whose work it is.
    chip: &'static str,
    /// When it began, and the scale its time is counted at, for a span that
    /// is timed.
    timed: Option<(Instant, u64)>,
}

/// The play tier with the parts of a frame call timed, its instructions
/// counted, and the parts of the machine the core names timed apart
/// ([`super::run_frame_profiled`]).
#[derive(Debug)]
pub struct Profiled {
    profile: FrameProfile,
    part: Option<(Part, Instant)>,
    in_run: bool,
    started: Instant,
    clock_at_start: u64,
    span: Option<OpenSpan>,
    sample_period: u32,
    countdown: u32,
    random: u32,
    /// What timing an empty span reads: taken off every span timed.
    timer_cost_ns: u64,
}

impl Profiled {
    /// A profiler with nothing measured yet, timing one in
    /// [`DEFAULT_SAMPLE_PERIOD`] spans inside steps.
    pub fn new() -> Self {
        Self::with_sample_period(DEFAULT_SAMPLE_PERIOD)
    }

    /// A profiler timing one in `period` spans inside steps (1 times every
    /// one). Which ones is chosen by a countdown a generator with a fixed
    /// seed sets again after each, so a profile repeats and no pattern in
    /// the order of the spans lines up with it.
    pub fn with_sample_period(period: u32) -> Self {
        let sample_period = period.max(1);
        let mut profiler = Self {
            profile: FrameProfile {
                sample_period,
                ..FrameProfile::default()
            },
            part: None,
            in_run: false,
            started: Instant::now(),
            clock_at_start: 0,
            span: None,
            sample_period,
            countdown: 0,
            random: 0x9E37_79B9,
            timer_cost_ns: timer_cost_ns(),
        };
        profiler.countdown = profiler.next_countdown();
        profiler
    }

    /// What it measured.
    pub fn into_profile(self) -> FrameProfile {
        self.profile
    }

    /// The spans until the next one timed: 1 to `2 * period - 1`, `period`
    /// on average.
    fn next_countdown(&mut self) -> u32 {
        // xorshift32
        let mut x = self.random;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.random = x;
        let span = 2 * self.sample_period - 1;
        1 + x % span
    }

    /// Whether the span beginning now is timed, and at what scale.
    #[inline]
    fn time_this_span(&mut self) -> Option<u64> {
        if !self.in_run {
            return Some(1);
        }
        self.countdown -= 1;
        if self.countdown > 0 {
            return None;
        }
        self.countdown = self.next_countdown();
        Some(u64::from(self.sample_period))
    }

    fn close_part(&mut self) {
        if let Some((part, since)) = self.part.take() {
            let ns = since.elapsed().as_nanos() as u64;
            match part {
                Part::Due => self.profile.due_ns += ns,
                Part::Run => self.profile.run_ns += ns,
                Part::Close => self.profile.close_ns += ns,
            }
        }
    }

    fn finish<M: DebugMachine>(&mut self, m: &M) {
        self.close_part();
        self.span = None;
        self.profile.total_ns = self.started.elapsed().as_nanos() as u64;
        self.profile.elapsed = m.now().saturating_sub(self.clock_at_start);
    }
}

impl Default for Profiled {
    fn default() -> Self {
        Self::new()
    }
}

/// What timing an empty span reads here: the median of 64.
fn timer_cost_ns() -> u64 {
    let mut reads: [u64; 64] = [0; 64];
    for read in &mut reads {
        let since = Instant::now();
        *read = since.elapsed().as_nanos() as u64;
    }
    reads.sort_unstable();
    reads[reads.len() / 2]
}

/// A timed span's time, the timer's own cost taken off.
fn span_ns(elapsed: Duration, timer_cost_ns: u64) -> u64 {
    (elapsed.as_nanos() as u64).saturating_sub(timer_cost_ns)
}

impl<M: DebugMachine> StepHooks<M> for Profiled {
    type Start = ();

    #[inline(always)]
    fn before_instruction(&mut self, m: &mut M, pc: u32) -> Flow<()> {
        m.debug_state_mut().record_coverage(MAIN_CPU, pc);
        self.profile.instructions += 1;
        Flow::Run(())
    }

    #[inline]
    fn chip_start(&mut self, chip: &'static str) {
        let timed = self.time_this_span().map(|scale| (Instant::now(), scale));
        self.span = Some(OpenSpan { chip, timed });
    }

    /// A span neither timed nor counting anything changes nothing, so its
    /// part is not looked up: most spans inside steps are such.
    #[inline]
    fn chip_finish(&mut self, units: u32) {
        let Some(span) = self.span.take() else {
            return;
        };
        if span.timed.is_none() && units == 0 {
            return;
        }
        let Some(slot) = self.profile.chip_slot(span.chip) else {
            return;
        };
        let ns = span.timed.map_or(0, |(since, scale)| {
            span_ns(since.elapsed(), self.timer_cost_ns) * scale
        });
        let chip = &mut self.profile.chips[slot];
        chip.ns += ns;
        chip.units += u64::from(units);
    }
}

impl<M: DebugMachine> Tier<M> for Profiled {
    fn enter(&mut self, m: &mut M) {
        self.started = Instant::now();
        self.clock_at_start = m.now();
    }

    fn part(&mut self, _m: &mut M, part: Part) {
        self.close_part();
        self.in_run = part == Part::Run;
        self.part = Some((part, Instant::now()));
    }

    fn held(&mut self, m: &mut M) {
        self.finish(m);
    }

    fn stopped(&mut self, m: &mut M) {
        self.finish(m);
    }

    fn leave(&mut self, m: &mut M) {
        self.finish(m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_part_is_kept_once_in_the_order_it_was_first_seen() {
        let mut profile = FrameProfile::default();
        let z80 = String::from("Z80");
        let z80: &'static str = Box::leak(z80.into_boxed_str());
        assert_eq!(profile.chip_slot("68000"), Some(0));
        assert_eq!(profile.chip_slot("PSG"), Some(1));
        assert_eq!(profile.chip_slot("68000"), Some(0));
        // The same name at another address is the same part.
        assert_eq!(profile.chip_slot("Z80"), Some(2));
        assert_eq!(profile.chip_slot(z80), Some(2));
        let names: Vec<&str> = profile.chips().iter().map(|c| c.chip).collect();
        assert_eq!(names, ["68000", "PSG", "Z80"]);
    }

    #[test]
    fn parts_past_the_most_kept_count_nothing() {
        const NAMES: [&str; MAX_PROFILED_CHIPS + 1] = [
            "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m",
        ];
        let mut profile = FrameProfile::default();
        for (i, name) in NAMES.iter().enumerate().take(MAX_PROFILED_CHIPS) {
            assert_eq!(profile.chip_slot(name), Some(i));
        }
        assert_eq!(profile.chip_slot(NAMES[MAX_PROFILED_CHIPS]), None);
        assert_eq!(profile.chips().len(), MAX_PROFILED_CHIPS);
    }

    #[test]
    fn spans_inside_steps_are_timed_one_in_the_period_on_average() {
        for period in [1, 4, 16, 64] {
            let mut profiler = Profiled::with_sample_period(period);
            profiler.in_run = true;
            let spans = 64_000;
            let mut timed = 0u64;
            for _ in 0..spans {
                if let Some(scale) = profiler.time_this_span() {
                    assert_eq!(scale, u64::from(period));
                    timed += 1;
                }
            }
            let expected = spans as f64 / f64::from(period);
            let ratio = timed as f64 / expected;
            assert!(
                (0.9..1.1).contains(&ratio),
                "period {period}: {timed} timed of {spans}"
            );
        }
    }

    #[test]
    fn spans_outside_steps_are_all_timed_at_their_own_time() {
        let mut profiler = Profiled::with_sample_period(64);
        for _ in 0..100 {
            assert_eq!(profiler.time_this_span(), Some(1));
        }
    }

    #[test]
    fn a_profile_repeats() {
        let pick = |p: &mut Profiled| -> Vec<bool> {
            p.in_run = true;
            (0..500).map(|_| p.time_this_span().is_some()).collect()
        };
        let first = pick(&mut Profiled::with_sample_period(16));
        let second = pick(&mut Profiled::with_sample_period(16));
        assert_eq!(first, second);
    }

    #[test]
    fn the_timers_cost_is_taken_off_and_never_below_zero() {
        assert_eq!(span_ns(Duration::from_nanos(100), 30), 70);
        assert_eq!(span_ns(Duration::from_nanos(10), 30), 0);
    }
}
