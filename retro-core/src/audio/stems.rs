//! Stems: each sound channel's samples as the channel enters the
//! console's mix, beside the mix itself, the master.
//!
//! A core describes its stems in a [`StemLayout`]; a host arms the ones it
//! wants with a [`StemSet`] and takes what was recorded as a [`StemTake`]
//! ([`crate::audio::SoundCore`]). The core fills a [`StemRecorder`] where
//! its mix has each channel's samples, and keeps a [`ScopeRing`] per
//! channel for the waveform its debugger shows.
//!
//! The stems follow the host's stream: a frame the host is not shown (a
//! headless frame) is left out, and a take holds whole frames only. On a
//! sample nothing moved, the stems that are the master's parts
//! ([`StemChannel::in_master`]) add up to it by the layout's [`StemSum`];
//! the samples where a clamp, a limiter or a mute moved the master away
//! from that sum are the take's departures.

use serde::Serialize;

/// One stem: a channel of a sound chip, or a part of the mix that is no
/// channel's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StemChannel {
    /// Its name in files and on a command line: `fm1`, `voice0-dry`,
    /// `echo`, `master`.
    pub name: &'static str,
    /// The sound chip it is taken from, the index
    /// [`crate::EmulatorCore::audio_debugger`] takes; `None` for the master.
    pub chip: Option<u8>,
    /// The chip's channel it is, the index
    /// [`crate::audio::AudioDebugger::get_channel_state`] takes; `None` for
    /// a part of the mix no channel makes (an echo, the master).
    pub channel: Option<u8>,
    /// Two samples a sample frame, left then right; one when it is mono.
    pub stereo: bool,
    /// Whether it is one of the parts the master is the sum of; a stem
    /// that is not shows sound another stem holds too (a voice before its
    /// volumes, where another stem is the voice through them).
    pub in_master: bool,
}

impl StemChannel {
    /// The samples a sample frame of it holds: 2 when it is stereo, else 1.
    pub fn width(&self) -> usize {
        if self.stereo {
            2
        } else {
            1
        }
    }
}

/// The kind of a console's samples, as its mix makes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SampleKind {
    /// 16-bit integers, full scale -32768 to 32767.
    I16,
    /// 32-bit floats, full scale -1 to 1.
    F32,
}

/// The largest difference [`StemSum::Exact`] allows between float samples:
/// the stems' sum and the mix round their additions in different orders.
pub const FLOAT_SUM_ROUNDING: f64 = 1.0e-6;

/// How the stems that are the master's parts add up to it on a sample no
/// departure moved, a mono stem added to both sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum StemSum {
    /// Exactly: integer samples are equal, float samples equal but for the
    /// rounding of the two sums ([`FLOAT_SUM_ROUNDING`]).
    Exact,
    /// Within this many steps of a 16-bit sample: each stem is rounded on
    /// its own where the mix rounds the stems' sum once.
    WithinLsb(u16),
}

impl StemSum {
    /// The largest difference the rule allows between the master and the
    /// stems' sum, in the unit of `kind`'s samples.
    pub fn tolerance(self, kind: SampleKind) -> f64 {
        match (self, kind) {
            (StemSum::Exact, SampleKind::I16) => 0.0,
            (StemSum::Exact, SampleKind::F32) => FLOAT_SUM_ROUNDING,
            (StemSum::WithinLsb(steps), SampleKind::I16) => f64::from(steps),
            (StemSum::WithinLsb(steps), SampleKind::F32) => f64::from(steps) / 32768.0,
        }
    }
}

/// The stems a console takes, and how they relate to its mix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StemLayout {
    /// The sample rate of the stems and the master, in Hz: the console's
    /// own, before any resampling for the host.
    pub rate: u32,
    /// The kind of their samples.
    pub kind: SampleKind,
    /// The stems in index order: stem `n` is bit `n` of a [`StemSet`].
    pub channels: Vec<StemChannel>,
    /// The master: the console's mix as its chips make it, what the
    /// speakers would get before the host's own processing.
    pub master: StemChannel,
    /// How the stems add up to the master.
    pub sum: StemSum,
}

impl StemLayout {
    /// The index of the stem named `name`.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.channels.iter().position(|stem| stem.name == name)
    }

    /// Every stem of the layout.
    pub fn all(&self) -> StemSet {
        StemSet::all(self.channels.len())
    }
}

/// A set of stems, by their index in the [`StemLayout`]: stem `n` is bit
/// `n`. The empty set is no stem.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct StemSet(u64);

impl StemSet {
    /// No stem.
    pub const EMPTY: StemSet = StemSet(0);

    /// The set whose bits are `bits`.
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// The set's bits.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// The first `count` stems.
    pub fn all(count: usize) -> Self {
        match count {
            0 => Self::EMPTY,
            64.. => Self(u64::MAX),
            n => Self((1u64 << n) - 1),
        }
    }

    /// The set with stem `index` added.
    pub fn with(self, index: usize) -> Self {
        Self(self.0 | 1u64.checked_shl(index as u32).unwrap_or(0))
    }

    /// Whether stem `index` is in the set.
    pub fn contains(self, index: usize) -> bool {
        index < 64 && self.0 & (1u64 << index) != 0
    }

    /// Whether the set holds no stem.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The stems in the set.
    pub fn len(self) -> usize {
        self.0.count_ones() as usize
    }

    /// The set's stem indices, ascending.
    pub fn iter(self) -> impl Iterator<Item = usize> {
        (0..64).filter(move |&index| self.contains(index))
    }

    /// The set less the stems `count` and above, which a layout of
    /// `count` stems does not have.
    pub fn within(self, count: usize) -> Self {
        Self(self.0 & Self::all(count).0)
    }
}

/// A buffer of samples of a console's own kind, neither converted.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum SoundSamples {
    I16(Vec<i16>),
    F32(Vec<f32>),
}

impl SoundSamples {
    /// The samples held.
    pub fn len(&self) -> usize {
        match self {
            SoundSamples::I16(samples) => samples.len(),
            SoundSamples::F32(samples) => samples.len(),
        }
    }

    /// Whether no sample is held.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Sample `index` in its own unit: a 16-bit step, or the float.
    pub fn value(&self, index: usize) -> f64 {
        match self {
            SoundSamples::I16(samples) => f64::from(samples[index]),
            SoundSamples::F32(samples) => f64::from(samples[index]),
        }
    }

    /// The samples as floats at full scale -1 to 1.
    pub fn to_f32(&self) -> Vec<f32> {
        match self {
            SoundSamples::I16(samples) => samples.iter().map(|&s| f32::from(s) / 32768.0).collect(),
            SoundSamples::F32(samples) => samples.clone(),
        }
    }
}

/// A run of sample frames: the first and how many.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SampleSpan {
    pub start: u32,
    pub frames: u32,
}

impl SampleSpan {
    /// The frame after the run.
    pub fn end(&self) -> u32 {
        self.start + self.frames
    }

    /// Whether sample frame `frame` is in the run.
    pub fn contains(&self, frame: u32) -> bool {
        frame >= self.start && frame < self.end()
    }
}

/// What a take holds: the armed stems and the master over the same whole
/// frames.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StemTake {
    /// The sample frames taken, which every stem and the master hold (a
    /// stereo one twice as many samples, left and right interleaved).
    pub frames: u32,
    /// The stems taken.
    pub armed: StemSet,
    /// One buffer per stem of `armed`, in index order.
    pub stems: Vec<SoundSamples>,
    /// The master, stereo.
    pub master: SoundSamples,
    /// The sample frames where a clamp, a limiter or a mute moved what the
    /// speakers get away from the stems' sum, in order.
    pub departures: Vec<SampleSpan>,
    /// The sample frames dropped since the last take, oldest first, because
    /// the recorder's bound was reached before the host took them.
    pub dropped: u64,
}

impl StemTake {
    /// Whether sample frame `frame` is one of the departures.
    pub fn departs(&self, frame: u32) -> bool {
        self.departures.iter().any(|span| span.contains(frame))
    }
}

/// A sample kind a [`StemRecorder`] holds.
pub trait StemSample: Copy + Default + PartialEq + 'static {
    /// The samples as a [`SoundSamples`].
    fn samples(values: Vec<Self>) -> SoundSamples;
    /// The sample as a float at full scale -1 to 1.
    fn as_f32(self) -> f32;
}

impl StemSample for i16 {
    fn samples(values: Vec<Self>) -> SoundSamples {
        SoundSamples::I16(values)
    }
    fn as_f32(self) -> f32 {
        f32::from(self) / 32768.0
    }
}

impl StemSample for f32 {
    fn samples(values: Vec<Self>) -> SoundSamples {
        SoundSamples::F32(values)
    }
    fn as_f32(self) -> f32 {
        self
    }
}

/// Where an armed stem's samples are in a sample frame of every stem.
#[derive(Debug, Clone, Copy)]
struct Armed {
    /// The stem's first value in the frame.
    offset: usize,
    /// Its values in the frame: 1 or 2.
    width: usize,
}

/// The stems a core records and a host takes: the armed stems' samples and
/// the master's, by sample frame, with the frames that depart from the
/// stems' sum.
///
/// The core pushes every sample frame of a frame the host is shown, and
/// ends each frame with [`Self::frame_end`], which takes a frame the host
/// was not shown back out and arms what the host asked for since. A take
/// holds the whole frames recorded; the samples of a frame still running
/// wait for its end. Past the bound, the oldest frames are dropped and
/// counted.
#[derive(Debug, Clone)]
pub struct StemRecorder<T> {
    /// Each stem's values in a sample frame, layout order.
    widths: Vec<usize>,
    /// Where each stem's values start in a sample frame.
    offsets: Vec<usize>,
    armed: StemSet,
    /// The stems of `armed`, in index order.
    slots: Vec<Armed>,
    /// What the next frame's start arms.
    pending: Option<StemSet>,
    stems: Vec<Vec<T>>,
    master: Vec<T>,
    departures: Vec<SampleSpan>,
    /// Sample frames held.
    frames: u32,
    /// Sample frames held when the frame running began.
    frame_start: u32,
    /// The most sample frames held.
    bound: u32,
    dropped: u64,
}

impl<T: StemSample> StemRecorder<T> {
    /// A recorder of `layout`'s stems holding at most `bound` sample frames,
    /// nothing armed.
    pub fn new(layout: &StemLayout, bound: u32) -> Self {
        let widths: Vec<usize> = layout.channels.iter().map(StemChannel::width).collect();
        let offsets = widths
            .iter()
            .scan(0, |next, &width| {
                let offset = *next;
                *next += width;
                Some(offset)
            })
            .collect();
        Self {
            widths,
            offsets,
            armed: StemSet::EMPTY,
            slots: Vec::new(),
            pending: None,
            stems: Vec::new(),
            master: Vec::new(),
            departures: Vec::new(),
            frames: 0,
            frame_start: 0,
            bound: bound.max(1),
            dropped: 0,
        }
    }

    /// The values a sample frame of every stem holds: what
    /// [`Self::push`] takes.
    pub fn frame_width(&self) -> usize {
        self.widths.iter().sum()
    }

    /// The stems recorded now.
    pub fn armed(&self) -> StemSet {
        self.armed
    }

    /// Whether anything is recorded now.
    pub fn is_armed(&self) -> bool {
        !self.armed.is_empty()
    }

    /// Whether a set waits for the next frame's start.
    pub fn arming(&self) -> bool {
        self.pending.is_some()
    }

    /// Record `stems` from the next frame's start, or from now when
    /// `between_frames` (the machine stands between two frames). An empty
    /// set disarms. Another set than the one armed starts over: what was
    /// held and not taken is dropped.
    pub fn arm(&mut self, stems: StemSet, between_frames: bool) {
        let stems = stems.within(self.widths.len());
        if between_frames {
            self.pending = None;
            self.apply(stems);
        } else {
            self.pending = Some(stems);
        }
    }

    fn apply(&mut self, stems: StemSet) {
        if stems == self.armed {
            return;
        }
        self.armed = stems;
        self.slots = stems
            .iter()
            .map(|index| Armed {
                offset: self.offsets[index],
                width: self.widths[index],
            })
            .collect();
        self.stems = vec![Vec::new(); self.slots.len()];
        self.master.clear();
        self.departures.clear();
        self.frames = 0;
        self.frame_start = 0;
    }

    /// One sample frame: `values` holds every stem's values in layout order
    /// (a stereo stem's left then right), armed or not; `master` the
    /// master's left and right; `departs` whether a clamp, a limiter or a
    /// mute moved what the speakers get away from the stems' sum. Nothing
    /// while nothing is armed.
    #[inline]
    pub fn push(&mut self, values: &[T], master: [T; 2], departs: bool) {
        if self.armed.is_empty() {
            return;
        }
        if self.frames >= self.bound {
            self.drop_oldest((self.bound / 4).max(1));
        }
        for (slot, stem) in self.slots.iter().zip(self.stems.iter_mut()) {
            stem.extend_from_slice(&values[slot.offset..slot.offset + slot.width]);
        }
        self.master.extend_from_slice(&master);
        if departs {
            match self.departures.last_mut() {
                Some(span) if span.end() == self.frames => span.frames += 1,
                _ => self.departures.push(SampleSpan {
                    start: self.frames,
                    frames: 1,
                }),
            }
        }
        self.frames += 1;
    }

    /// The end of a frame: when the host was not shown it (`presented`
    /// false), its sample frames are taken back out; then the set the host
    /// asked for since is armed.
    pub fn frame_end(&mut self, presented: bool) {
        if presented {
            self.frame_start = self.frames;
        } else {
            self.truncate(self.frame_start);
        }
        if let Some(stems) = self.pending.take() {
            self.apply(stems);
        }
    }

    /// Take what was held since the last take, by whole frames: `None`
    /// while nothing is armed, waits to be armed or is held.
    pub fn take(&mut self) -> Option<StemTake> {
        let arming = self.pending.is_some_and(|stems| !stems.is_empty());
        if self.armed.is_empty() && !arming && self.frames == 0 && self.dropped == 0 {
            return None;
        }
        let taken = self.frame_start;
        let stems = self
            .slots
            .iter()
            .zip(self.stems.iter_mut())
            .map(|(slot, stem)| {
                let rest = stem.split_off(taken as usize * slot.width);
                T::samples(std::mem::replace(stem, rest))
            })
            .collect();
        let rest = self.master.split_off(taken as usize * 2);
        let master = T::samples(std::mem::replace(&mut self.master, rest));
        let departures = self.split_departures(taken);
        self.frames -= taken;
        self.frame_start = 0;
        Some(StemTake {
            frames: taken,
            armed: self.armed,
            stems,
            master,
            departures,
            dropped: std::mem::take(&mut self.dropped),
        })
    }

    /// The departures before frame `at`, moved out; the rest counted from
    /// `at`.
    fn split_departures(&mut self, at: u32) -> Vec<SampleSpan> {
        let mut before = Vec::new();
        let mut after = Vec::new();
        for span in self.departures.drain(..) {
            if span.end() <= at {
                before.push(span);
            } else if span.start >= at {
                after.push(SampleSpan {
                    start: span.start - at,
                    frames: span.frames,
                });
            } else {
                before.push(SampleSpan {
                    start: span.start,
                    frames: at - span.start,
                });
                after.push(SampleSpan {
                    start: 0,
                    frames: span.end() - at,
                });
            }
        }
        self.departures = after;
        before
    }

    /// Drop the oldest `frames` sample frames, counting them.
    fn drop_oldest(&mut self, frames: u32) {
        let frames = frames.min(self.frames);
        for (slot, stem) in self.slots.iter().zip(self.stems.iter_mut()) {
            stem.drain(..frames as usize * slot.width);
        }
        self.master.drain(..frames as usize * 2);
        let _ = self.split_departures(frames);
        self.frames -= frames;
        self.frame_start = self.frame_start.saturating_sub(frames);
        self.dropped += u64::from(frames);
    }

    /// Keep only the first `frames` sample frames.
    fn truncate(&mut self, frames: u32) {
        for (slot, stem) in self.slots.iter().zip(self.stems.iter_mut()) {
            stem.truncate(frames as usize * slot.width);
        }
        self.master.truncate(frames as usize * 2);
        let kept = self.split_departures(frames);
        self.departures = kept;
        self.frames = frames;
    }
}

/// The last [`ScopeRing::LEN`] samples of a channel, mono, for the
/// waveform a debugger shows.
#[derive(Debug, Clone)]
pub struct ScopeRing {
    samples: Box<[f32; ScopeRing::LEN]>,
    /// Where the next sample goes.
    head: usize,
    /// Samples held, up to `LEN`.
    held: usize,
}

impl Default for ScopeRing {
    fn default() -> Self {
        Self::new()
    }
}

impl ScopeRing {
    /// The samples a ring keeps.
    pub const LEN: usize = 1024;

    /// An empty ring.
    pub fn new() -> Self {
        Self {
            samples: Box::new([0.0; Self::LEN]),
            head: 0,
            held: 0,
        }
    }

    /// Keep `sample`, dropping the oldest once full.
    #[inline]
    pub fn push(&mut self, sample: f32) {
        self.samples[self.head] = sample;
        self.head = (self.head + 1) % Self::LEN;
        self.held = (self.held + 1).min(Self::LEN);
    }

    /// The last `samples` (at most [`Self::LEN`]), oldest first, zeros in
    /// front while fewer were kept.
    pub fn read(&self, samples: u32) -> Vec<f32> {
        let wanted = (samples as usize).min(Self::LEN);
        let have = wanted.min(self.held);
        let mut out = vec![0.0; wanted - have];
        let start = (self.head + Self::LEN - have) % Self::LEN;
        out.extend((0..have).map(|i| self.samples[(start + i) % Self::LEN]));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> StemLayout {
        StemLayout {
            rate: 1000,
            kind: SampleKind::I16,
            channels: vec![
                StemChannel {
                    name: "a",
                    chip: Some(0),
                    channel: Some(0),
                    stereo: true,
                    in_master: true,
                },
                StemChannel {
                    name: "b",
                    chip: Some(0),
                    channel: Some(1),
                    stereo: false,
                    in_master: true,
                },
            ],
            master: StemChannel {
                name: "master",
                chip: None,
                channel: None,
                stereo: true,
                in_master: false,
            },
            sum: StemSum::Exact,
        }
    }

    /// A sample frame numbered `n`: stem a (n, -n), stem b (n * 10).
    fn push(recorder: &mut StemRecorder<i16>, n: i16, departs: bool) {
        recorder.push(&[n, -n, n * 10], [n, -n], departs);
    }

    #[test]
    fn a_set_names_its_stems_by_index() {
        let set = StemSet::EMPTY.with(0).with(3);
        assert!(set.contains(0) && set.contains(3) && !set.contains(1));
        assert_eq!(set.len(), 2);
        assert_eq!(set.iter().collect::<Vec<_>>(), [0, 3]);
        assert_eq!(StemSet::all(3).bits(), 0b111);
        assert_eq!(StemSet::all(64).bits(), u64::MAX);
        assert_eq!(set.within(2).bits(), 1);
        assert!(StemSet::EMPTY.is_empty());
        assert_eq!(layout().index_of("b"), Some(1));
        assert_eq!(layout().all().len(), 2);
    }

    #[test]
    fn the_sum_rule_gives_its_tolerance() {
        assert_eq!(StemSum::Exact.tolerance(SampleKind::I16), 0.0);
        assert_eq!(
            StemSum::Exact.tolerance(SampleKind::F32),
            FLOAT_SUM_ROUNDING
        );
        assert_eq!(StemSum::WithinLsb(8).tolerance(SampleKind::I16), 8.0);
    }

    #[test]
    fn nothing_is_recorded_until_armed_and_a_take_holds_whole_frames() {
        let mut recorder = StemRecorder::<i16>::new(&layout(), 100);
        push(&mut recorder, 1, false);
        assert_eq!(recorder.take(), None, "nothing armed, nothing held");
        recorder.arm(StemSet::all(2), false);
        push(&mut recorder, 2, false);
        assert!(!recorder.is_armed(), "armed at the next frame's start");
        recorder.frame_end(true);
        assert!(recorder.is_armed());
        push(&mut recorder, 3, false);
        push(&mut recorder, 4, true);
        assert_eq!(recorder.take().unwrap().frames, 0, "the frame still runs");
        recorder.frame_end(true);
        push(&mut recorder, 5, true);
        let take = recorder.take().unwrap();
        assert_eq!(take.frames, 2);
        assert_eq!(take.stems[0], SoundSamples::I16(vec![3, -3, 4, -4]));
        assert_eq!(take.stems[1], SoundSamples::I16(vec![30, 40]));
        assert_eq!(take.master, SoundSamples::I16(vec![3, -3, 4, -4]));
        assert_eq!(
            take.departures,
            [SampleSpan {
                start: 1,
                frames: 1
            }]
        );
        recorder.frame_end(true);
        let take = recorder.take().unwrap();
        assert_eq!(take.frames, 1, "the frame still running when taken");
        assert_eq!(
            take.departures,
            [SampleSpan {
                start: 0,
                frames: 1
            }]
        );
    }

    #[test]
    fn a_frame_the_host_was_not_shown_is_taken_back() {
        let mut recorder = StemRecorder::<i16>::new(&layout(), 100);
        recorder.arm(StemSet::EMPTY.with(1), true);
        push(&mut recorder, 1, false);
        recorder.frame_end(true);
        push(&mut recorder, 2, true);
        push(&mut recorder, 3, true);
        recorder.frame_end(false);
        push(&mut recorder, 4, false);
        recorder.frame_end(true);
        let take = recorder.take().unwrap();
        assert_eq!(take.stems, [SoundSamples::I16(vec![10, 40])]);
        assert!(take.departures.is_empty());
    }

    #[test]
    fn past_the_bound_the_oldest_frames_are_dropped_and_counted() {
        let mut recorder = StemRecorder::<i16>::new(&layout(), 8);
        recorder.arm(StemSet::EMPTY.with(1), true);
        for n in 0..10 {
            push(&mut recorder, n, n == 1);
        }
        recorder.frame_end(true);
        let take = recorder.take().unwrap();
        assert_eq!(take.dropped, 2);
        assert_eq!(take.frames, 8);
        assert_eq!(
            take.stems,
            [SoundSamples::I16((2..10).map(|n| n * 10).collect())]
        );
        assert!(
            take.departures.is_empty(),
            "the departing frame was dropped"
        );
    }

    #[test]
    fn another_set_starts_over_and_an_empty_one_disarms() {
        let mut recorder = StemRecorder::<i16>::new(&layout(), 100);
        recorder.arm(StemSet::all(2), true);
        push(&mut recorder, 1, false);
        recorder.arm(StemSet::EMPTY.with(0), true);
        push(&mut recorder, 2, false);
        recorder.frame_end(true);
        let take = recorder.take().unwrap();
        assert_eq!(take.stems, [SoundSamples::I16(vec![2, -2])]);
        recorder.arm(StemSet::EMPTY, false);
        recorder.frame_end(true);
        assert!(!recorder.is_armed());
        assert_eq!(recorder.take(), None);
    }

    #[test]
    fn a_scope_keeps_the_last_samples_oldest_first() {
        let mut scope = ScopeRing::new();
        assert_eq!(scope.read(3), [0.0; 3]);
        scope.push(1.0);
        scope.push(2.0);
        assert_eq!(scope.read(3), [0.0, 1.0, 2.0]);
        for n in 0..ScopeRing::LEN + 5 {
            scope.push(n as f32);
        }
        let last = scope.read(2);
        assert_eq!(
            last,
            [(ScopeRing::LEN + 3) as f32, (ScopeRing::LEN + 4) as f32]
        );
        assert_eq!(scope.read(5000).len(), ScopeRing::LEN);
    }
}
