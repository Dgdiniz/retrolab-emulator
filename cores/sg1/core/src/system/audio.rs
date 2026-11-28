//! Audio: the YM2612's and the PSG's records of a frame mixed at its end
//! through the host's band-limited stage into the host's ring, the sound
//! hooks a host calls (`SoundCore`), the stems the mix takes, and the
//! sound chips' debugger readers.

use std::iter::Peekable;
use std::sync::Arc;

use retro_core::audio::{
    merge_by_time, BandLimitedSteps, IntegerRateConverter, SampleKind, SampleTime, SincKernel,
    SoundChipKind, SoundCore, SoundEvent, SoundLog, SoundLogContents, SoundLogEntry, SoundLogger,
    StemChannel, StemLayout, StemRecorder, StemSet, StemSum, StemTake,
};
use retro_core::frame::run_frame_on;
use retro_core::{EmulatorCore, ExecutionTier};

use super::GenesisEmulator;
use crate::psg::{PsgLevels, PSG_STEM_VALUES};
use crate::timing::GenesisTiming;
use crate::ym2612::{DacStatus, FmDacModel, LfoStatus, TimerStatus, YmLevels, YM_STEM_VALUES};

/// The PSG's share of the mix, and the YM2612's: the PSG at full scale is
/// about a third of the FM at full scale (0.34, the ratio Genesis Plus
/// GX's default levels give); 0.25 and 0.75 give 0.333, with the most the
/// mix can reach 1.0.
const PSG_SHARE: f32 = 0.25;
const FM_SHARE: f32 = 0.75;

/// An FM channel's stem as it enters the master: its share of the
/// YM2612's sum, which divides by 6, and of the mix; a PSG channel's, of
/// the PSG's sum (divided by 4) and of the mix. Both are powers of two, so
/// the scaling is exact.
const FM_STEM_SCALE: f32 = FM_SHARE / 6.0;
const PSG_STEM_SCALE: f32 = PSG_SHARE / 4.0;

/// The master clocks in one of the PSG's input clocks: it runs on the Z80's
/// clock, a fifteenth of the master clock.
const PSG_MASTER_CLOCKS: u64 = 15;

/// The YM2612's stems: its six channels and its DAC, each stereo.
const FM_STEMS: usize = YM_STEM_VALUES / 2;

/// How far past the samples rendered so far a change may lie before the
/// samples up to it are rendered first, so the stage's buffers never reach
/// further ahead than this.
const STAGE_RENDER_AHEAD: usize = 4096;

/// The kernel of the host's band-limited stage: a Kaiser-windowed sinc
/// spanning 32 of the host's samples each side, its cutoff at 0.92 of the
/// host's Nyquist frequency (20.3 kHz at 44.1 kHz) and its stopband 80 dB
/// down from the Nyquist frequency on, so what lies past half the host's
/// rate is gone before it can fold back into what is heard.
pub(super) fn host_kernel() -> Arc<SincKernel> {
    Arc::new(SincKernel::new(32, 0.92, 80.0, 128))
}

/// The stems' own stages while the host takes stems: each of the
/// YM2612's channels and its DAC, and each of the PSG's channels,
/// band-limited by a stage of its own, as the master is, so the stems add
/// up to the master but for the rounding of the two sums. Made from
/// silence when the stems are armed (their first span departs from the
/// master, whose stage was running), dropped when they are not.
pub(super) struct StemStages {
    fm: [BandLimitedSteps<2>; FM_STEMS],
    psg: [BandLimitedSteps<1>; PSG_STEM_VALUES],
    /// Each FM stem's samples of the frame, stereo interleaved.
    fm_samples: [Vec<f32>; FM_STEMS],
    /// Each PSG stem's samples of the frame.
    psg_samples: [Vec<f32>; PSG_STEM_VALUES],
}

impl StemStages {
    fn new(kernel: &Arc<SincKernel>) -> Self {
        Self {
            fm: std::array::from_fn(|_| BandLimitedSteps::new(kernel.clone())),
            psg: std::array::from_fn(|_| BandLimitedSteps::new(kernel.clone())),
            fm_samples: std::array::from_fn(|_| Vec::new()),
            psg_samples: std::array::from_fn(|_| Vec::new()),
        }
    }
}

/// The PSG's part of the master from its four outputs: their sum, the
/// muted left out, at the PSG's share of the mix and over its 4 channels
/// (exact in f64: four values of an f32's precision and a power of two).
fn psg_master_level(levels: &PsgLevels, mutes: [bool; PSG_STEM_VALUES]) -> f64 {
    let sum: f64 = levels
        .iter()
        .zip(mutes)
        .map(|(&level, muted)| if muted { 0.0 } else { f64::from(level) })
        .sum();
    sum * f64::from(PSG_STEM_SCALE)
}

/// The YM2612's part of the master from its sample: at the YM2612's share
/// of the mix on each side (exact in f64).
fn fm_master_level(levels: &YmLevels) -> [f64; 2] {
    levels.map(|level| f64::from(level) * f64::from(FM_SHARE))
}

/// The master's levels at times, in order, from the PSG's part and the
/// YM2612's, each at times in order: their sum, at each change of either.
/// Each part starts from the first level its record gives, the level the
/// chip held as its record started.
struct MasterLevels<P: Iterator, Y: Iterator> {
    psg: Peekable<P>,
    ym: Peekable<Y>,
    psg_level: f64,
    ym_level: [f64; 2],
}

impl<P, Y> MasterLevels<P, Y>
where
    P: Iterator<Item = (SampleTime, f64)>,
    Y: Iterator<Item = (SampleTime, [f64; 2])>,
{
    fn new(psg: P, ym: Y) -> Self {
        let mut psg = psg.peekable();
        let mut ym = ym.peekable();
        let psg_level = psg.peek().map_or(0.0, |entry| entry.1);
        let ym_level = ym.peek().map_or([0.0; 2], |entry| entry.1);
        Self {
            psg,
            ym,
            psg_level,
            ym_level,
        }
    }
}

impl<P, Y> Iterator for MasterLevels<P, Y>
where
    P: Iterator<Item = (SampleTime, f64)>,
    Y: Iterator<Item = (SampleTime, [f64; 2])>,
{
    type Item = (SampleTime, [f64; 2]);

    fn next(&mut self) -> Option<Self::Item> {
        let psg_first = match (self.psg.peek(), self.ym.peek()) {
            (Some(psg), Some(ym)) => psg.0 <= ym.0,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => return None,
        };
        let at = if psg_first {
            let (at, level) = self.psg.next()?;
            self.psg_level = level;
            at
        } else {
            let (at, level) = self.ym.next()?;
            self.ym_level = level;
            at
        };
        Some((at, self.ym_level.map(|ym| self.psg_level + ym)))
    }
}

/// Levels at times, in order, set on `steps` and the frame's `frames`
/// samples rendered into `out` (`N` interleaved values each). A change far
/// past the samples rendered so far has the samples before it rendered
/// first, so the stage's buffers stay short in a frame of any length.
fn stage_frame<const N: usize>(
    steps: &mut BandLimitedSteps<N>,
    levels: impl Iterator<Item = (SampleTime, [f64; N])>,
    frames: usize,
    out: &mut [f32],
) {
    let mut rendered = 0usize;
    for (at, level) in levels {
        let ready = ((at.sample + 1).max(0) as usize).min(frames);
        if ready > rendered + STAGE_RENDER_AHEAD {
            steps.render(ready - rendered, &mut out[N * rendered..N * ready]);
            rendered = ready;
        }
        let at = SampleTime {
            sample: at.sample - rendered as i64,
            fraction: at.fraction,
        };
        steps.set_level(at, level);
    }
    steps.render(frames - rendered, &mut out[N * rendered..N * frames]);
}

/// Where the PSG's input clock `time` lands on the frame's samples, as
/// `converter` counts them from the frame's start: 15 master clocks a
/// clock, 7 (`cpu_divider`) to the converter's 68000 cycle.
fn psg_place(converter: &IntegerRateConverter, time: u64, cpu_divider: u64) -> SampleTime {
    converter.position(PSG_MASTER_CLOCKS * time, cpu_divider)
}

/// Where the YM2612's input clock `time` lands on the frame's samples: it
/// runs on the 68000's clock, the converter's own cycle, placed in master
/// clocks as the PSG's is.
fn ym_place(converter: &IntegerRateConverter, time: u64, cpu_divider: u64) -> SampleTime {
    converter.position(cpu_divider * time, cpu_divider)
}

/// The stems, in index order: the YM2612's six channels and its DAC, which
/// plays in place of channel 6 (whose FM stem is then silent), the PSG's
/// three tone channels and its noise.
const STEM_CHANNELS: [StemChannel; 11] = [
    fm("fm1", 0),
    fm("fm2", 1),
    fm("fm3", 2),
    fm("fm4", 3),
    fm("fm5", 4),
    fm("fm6", 5),
    fm("dac", 5),
    psg("psg1", 0),
    psg("psg2", 1),
    psg("psg3", 2),
    psg("noise", 3),
];

const fn fm(name: &'static str, channel: u8) -> StemChannel {
    StemChannel {
        name,
        chip: Some(0),
        channel: Some(channel),
        stereo: true,
        in_master: true,
    }
}

const fn psg(name: &'static str, channel: u8) -> StemChannel {
    StemChannel {
        name,
        chip: Some(1),
        channel: Some(channel),
        stereo: false,
        in_master: true,
    }
}

/// The Mega Drive's stems at the mix's `rate`: the chips' channels as they
/// enter the master, floats, and the master the mix before its limiter,
/// which they sum to exactly.
fn stem_layout(rate: u32) -> StemLayout {
    StemLayout {
        rate,
        kind: SampleKind::F32,
        channels: STEM_CHANNELS.to_vec(),
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

/// The sound log while a host logs. Each chip logs its writes (and the
/// YM2612 its channels' keys) on its own input clock, and the bus the
/// 68000's writes into the Z80's RAM, the mailbox, on the 68000's; each
/// frame's end gathers them onto the master clock, the YM2612's before the
/// PSG's and the PSG's before the mailbox's at the same time, then the
/// frame's end, at the master clocks the frame ran. A host setting, not
/// saved.
#[derive(Debug, Default)]
pub(super) struct SoundLogState {
    logger: SoundLogger,
    /// Where the log stood at its start, at the last frame's end, or when
    /// it went on after a pause: what it gathers next lies after it.
    mark: LogMark,
    /// A reset, a load, a power cycle or a savestate's load is under way:
    /// the log goes on once it ends.
    paused: bool,
}

/// A moment of the sound log: its time, in master clocks; the machine's
/// master clocks then; and each chip's input clocks (the YM2612's, the
/// PSG's).
#[derive(Debug, Default, Clone, Copy)]
struct LogMark {
    time: u64,
    master: u64,
    chips: [u64; 2],
}

/// The log's time of input clock `clock` of a chip that counted from
/// `counted.0` to `counted.1` while the log's time ran from `span.0` to
/// `span.1`: the chip's clocks spread evenly over that time, so its entries
/// keep their order and lie inside it.
fn spread_over(clock: u64, counted: (u64, u64), span: (u64, u64)) -> u64 {
    let (from, to) = (counted.0, counted.1.max(counted.0));
    if to == from {
        return span.0;
    }
    let into = clock.clamp(from, to) - from;
    let length = span.1.saturating_sub(span.0);
    span.0 + (u128::from(into) * u128::from(length) / u128::from(to - from)) as u64
}

/// The log's time of a write the bus stamped at master clock `master`, the
/// log's time having run from `span.0` to `span.1` while the machine ran
/// from master clock `from`: the master clocks since, kept inside that
/// time (a write the host made between two frames bears the last
/// instruction's stamp, from before the frame's end).
fn at_master_clock(master: u64, from: u64, span: (u64, u64)) -> u64 {
    (span.0 + master.saturating_sub(from)).min(span.1)
}

/// A recorder of the stems at `rate`, holding at most two seconds.
pub(super) fn stem_recorder(rate: u32) -> StemRecorder<f32> {
    StemRecorder::new(&stem_layout(rate), rate * 2)
}

/// The mix's limiter: below 0.8 the signal passes through linearly, above
/// it a smooth compression takes the place of a hard clip. Always in
/// [-1, 1].
#[inline]
fn soft_limit(x: f32) -> f32 {
    const THRESHOLD: f32 = 0.8;
    if x.abs() <= THRESHOLD {
        x
    } else {
        let sign = x.signum();
        let excess = x.abs() - THRESHOLD;
        let compressed = THRESHOLD + (1.0 - THRESHOLD) * (1.0 - (-excess * 3.0).exp());
        sign * compressed.min(1.0)
    }
}

impl SoundCore for GenesisEmulator {
    fn render_frame(&mut self) {
        run_frame_on(self, ExecutionTier::Play);
    }

    fn stem_layout(&self) -> Option<StemLayout> {
        Some(stem_layout(self.audio_sample_rate))
    }

    fn arm_stems(&mut self, stems: StemSet) {
        let was = self.stems.is_armed();
        self.stems.arm(stems, !self.frame_open);
        if self.stems.is_armed() != was {
            self.set_stem_taps(self.stems.is_armed());
        }
    }

    fn take_stems(&mut self) -> Option<StemTake> {
        self.stems.take()
    }

    fn sound_log_chips(&self) -> Vec<SoundChipKind> {
        vec![
            SoundChipKind::Ym2612 {
                clock_hz: self.bus.ym2612().clock_freq(),
            },
            SoundChipKind::Sn76489 {
                clock_hz: self.bus.psg().clock_freq(),
            },
        ]
    }

    /// The log's clock is the master clock. The YM2612 keys its channels;
    /// the PSG's tones have no keys, so a log of the keys names the
    /// YM2612's alone. The mailbox is the Z80's RAM, each write at its
    /// address there.
    fn start_sound_log(&mut self, contents: SoundLogContents) {
        if !contents.any() {
            self.stop_sound_log();
            return;
        }
        self.set_chip_logs(contents);
        self.sound_log.mark = self.log_mark(0);
        self.sound_log.paused = false;
        let image = self.sound_log_image(0, contents);
        self.sound_log.logger.start(contents, image);
    }

    fn stop_sound_log(&mut self) {
        self.gather_sound_log();
        self.set_chip_logs(SoundLogContents::default());
        self.sound_log.logger.stop();
    }

    fn take_sound_log(&mut self) -> Option<SoundLog> {
        self.sound_log.logger.take(self.region.master_clock())
    }
}

impl GenesisEmulator {
    /// Each chip's input clocks received: the YM2612's, the PSG's.
    fn chip_clocks(&self) -> [u64; 2] {
        [
            self.bus.ym2612().input_clocks(),
            self.bus.psg().input_clocks(),
        ]
    }

    /// A mark of the log at `time`, with the machine's master clocks and
    /// the chips' input clocks as they stand now.
    fn log_mark(&self, time: u64) -> LogMark {
        LogMark {
            time,
            master: EmulatorCore::master_cycles(self),
            chips: self.chip_clocks(),
        }
    }

    /// Where the log stands now: its mark's time and the master clocks the
    /// machine ran since.
    fn log_now(&self) -> LogMark {
        let mark = self.sound_log.mark;
        let ran = EmulatorCore::master_cycles(self).saturating_sub(mark.master);
        self.log_mark(mark.time + ran)
    }

    /// The chips as entries at `time`, what `contents` holds of them: with
    /// the writes, the YM2612's registers, then the PSG's; with the keys, a
    /// key-on for each YM2612 channel keyed.
    fn sound_log_image(&self, time: u64, contents: SoundLogContents) -> Vec<SoundLogEntry> {
        let entry = |chip, event| SoundLogEntry { time, chip, event };
        let write = |chip, register, value| entry(chip, SoundEvent::Write { register, value });
        let mut image = Vec::new();
        if contents.writes {
            let ym = self.bus.ym2612().register_image();
            let psg = self.bus.psg().register_image();
            image.extend(
                ym.into_iter()
                    .map(|(register, value)| write(0, register, value))
                    .chain(psg.into_iter().map(|value| write(1, 0, value))),
            );
        }
        if contents.keys {
            let keys = self.bus.ym2612().key_image();
            image.extend(keys.into_iter().map(|event| entry(0, event)));
        }
        image
    }

    /// Each chip logs what `contents` asks of it, the PSG its writes; and
    /// the bus the 68000's writes into the Z80's RAM, with the mailbox.
    fn set_chip_logs(&mut self, contents: SoundLogContents) {
        self.bus.ym2612_mut().set_sound_log(SoundLogContents {
            mailbox: false,
            ..contents
        });
        self.bus.psg_mut().set_sound_log(contents.writes);
        self.bus.set_mailbox_log(contents.mailbox);
    }

    /// What the chips and the mailbox logged since the log's mark, onto
    /// the log's clock, by time; where the log stands now. Each chip's
    /// input clocks are spread over the master clocks the machine ran since
    /// the mark: the chips are not always clocked in step with the master
    /// clock (the PSG gets more than its share of a line when the 68000
    /// runs further past the line's end than past the one before, and
    /// inside a line the YM2612 runs ahead while the Z80 runs, then waits
    /// at the line's budget), so a chip's own count would carry its entries
    /// away from the other chip's and out of their frame. The mailbox's
    /// writes bear the 68000's cycle, which counts the master clock over
    /// the 68000's divider: they go onto the log's clock as they are.
    fn gather_sound_log(&mut self) -> LogMark {
        let (from, now) = (self.sound_log.mark, self.log_now());
        if !self.sound_log.logger.is_on() {
            return now;
        }
        let mut chips: [Vec<SoundLogEntry>; 2] = [Vec::new(), Vec::new()];
        let logged = [
            self.bus.ym2612_mut().take_sound_log(),
            self.bus.psg_mut().take_sound_log(),
        ];
        for (chip, (entries, logged)) in chips.iter_mut().zip(logged).enumerate() {
            let counted = (from.chips[chip], now.chips[chip]);
            entries.extend(logged.into_iter().map(|(clock, event)| SoundLogEntry {
                time: spread_over(clock, counted, (from.time, now.time)),
                chip: chip as u8,
                event,
            }));
        }
        let mut merged = merge_by_time(&chips[0], &chips[1]);
        let mailbox = self.bus.take_mailbox_log();
        if !mailbox.is_empty() {
            let divider = u64::from(self.region.cpu_divider());
            let mailbox: Vec<SoundLogEntry> = mailbox
                .into_iter()
                .map(|(cycle, event)| SoundLogEntry {
                    time: at_master_clock(cycle * divider, from.master, (from.time, now.time)),
                    chip: 0,
                    event,
                })
                .collect();
            merged = merge_by_time(&merged, &mailbox);
        }
        self.sound_log.logger.extend(merged);
        now
    }

    /// The end of a frame for the log: what the chips logged, then the
    /// frame's end, as many master clocks after the log's mark as the
    /// machine ran since; the mark moves there.
    pub(super) fn end_sound_log_frame(&mut self) {
        if !self.sound_log.logger.is_on() {
            return;
        }
        let now = self.gather_sound_log();
        self.sound_log.logger.extend([SoundLogEntry {
            time: now.time,
            chip: 0,
            event: SoundEvent::FrameEnd,
        }]);
        self.sound_log.mark = now;
    }

    /// A reset, a load, a power cycle or a savestate's load begins: what
    /// the chips logged goes onto the log before their state goes.
    pub(super) fn pause_sound_log(&mut self) {
        if self.sound_log.logger.is_on() && !self.sound_log.paused {
            self.gather_sound_log();
            self.sound_log.paused = true;
        }
    }

    /// It ended: the log goes on from the latest time it logged, the next
    /// take marked interrupted, with the chips' registers as they stand
    /// now (chips built again log too).
    pub(super) fn resume_sound_log(&mut self) {
        if !self.sound_log.paused {
            return;
        }
        self.sound_log.paused = false;
        let contents = self.sound_log.logger.contents();
        self.set_chip_logs(contents);
        let time = self.sound_log.logger.latest();
        self.sound_log.mark = self.log_mark(time);
        self.sound_log.logger.interrupt();
        let image = self.sound_log_image(time, contents);
        self.sound_log.logger.extend(image);
    }
}

impl GenesisEmulator {
    /// Get YM2612 channel state for debugging.
    pub fn get_ym2612_channel_state(
        &self,
        channel: u32,
    ) -> Option<retro_core::audio::ChannelState> {
        use retro_core::audio::AudioDebugger;
        self.ym2612().get_channel_state(channel)
    }

    /// Mute/unmute YM2612 channel.
    pub fn mute_ym2612_channel(&mut self, channel: u32, muted: bool) {
        use retro_core::audio::AudioDebugger;
        self.ym2612_mut().mute_channel(channel, muted);
    }

    /// Make the YM2612's channels reach the output through the DAC of
    /// `model`'s chip: the discrete YM2612 of the first consoles, the
    /// default, or the YM3438 the later ones integrate. A host setting, as
    /// a channel's mute is: no state the machine holds changes, the
    /// savestate holds nothing of it and the state hash reads nothing of
    /// it, and a ROM's load, the resets and a savestate's load keep it.
    pub fn set_fm_dac_model(&mut self, model: FmDacModel) {
        self.bus.ym2612_mut().set_dac_model(model);
    }

    /// The DAC the YM2612's channels reach the output through.
    pub fn fm_dac_model(&self) -> FmDacModel {
        self.bus.ym2612().dac_model()
    }

    /// The YM2612's LFO: `{ enabled, frequency }`.
    pub fn get_ym2612_lfo_state(&self) -> LfoStatus {
        self.ym2612().lfo_status()
    }

    /// The YM2612's DAC: `{ enabled, value }`.
    pub fn get_ym2612_dac_state(&self) -> DacStatus {
        self.ym2612().dac_status()
    }

    /// The YM2612's timer A: `{ running, period, overflowed }`.
    pub fn get_ym2612_timer_a_state(&self) -> TimerStatus {
        self.ym2612().timer_a_status()
    }

    /// The YM2612's timer B: `{ running, period, overflowed }`.
    pub fn get_ym2612_timer_b_state(&self) -> TimerStatus {
        self.ym2612().timer_b_status()
    }

    /// The last `samples` of the mixed PSG and YM2612 output, mono, oldest
    /// first, zeros in front while fewer were made
    /// ([`EmulatorCore::output_waveform`](retro_core::EmulatorCore::output_waveform)).
    pub(super) fn mixed_waveform(&self, samples: u32) -> Vec<f32> {
        let count = (samples as usize).min(self.mixed_waveform_buffer.len());
        if count == 0 {
            return vec![0.0; samples as usize];
        }
        let skip = self.mixed_waveform_buffer.len() - count;
        let mut result: Vec<f32> = self
            .mixed_waveform_buffer
            .iter()
            .skip(skip)
            .copied()
            .collect();
        // Pad front with zeros if not enough samples yet
        if result.len() < samples as usize {
            let mut padded = vec![0.0; samples as usize - result.len()];
            padded.append(&mut result);
            padded
        } else {
            result
        }
    }
}

impl GenesisEmulator {
    /// Advance the audio subsystem one frame, and, when `present`, mix and
    /// push the samples to the output ring. Uses pre-allocated buffers to avoid
    /// per-frame heap allocations.
    ///
    /// The first half (advancing `audio_rate_converter` and taking the
    /// chips' records of the frame) mutates *serialized machine state*,
    /// state the netplay desync hash (`state_hash`) covers, so it MUST run
    /// on every frame, audible or headless, the sound on or off. If a
    /// headless rollback/catch-up frame skipped it, a re-simulated span
    /// would leave the converter and the chips' records where a live
    /// audible run moved them on, the two peers' hashes for that frame
    /// would differ, and the netplay desync check would fire a spurious
    /// full resync. The second half (the records through the host's stage,
    /// the limiter, the waveform-visualization ring, the stems and the host
    /// audio ring) is pure presentation (none of it is serialized), so a
    /// headless frame skips it: audio is generated but not rendered,
    /// exactly as rollback netcode requires. So a headless frame queues
    /// nothing, and after a rollback's frames there is nothing pending to
    /// discard (the trait's `discard_pending_audio` does nothing here):
    /// clearing the host ring instead would throw away sound presented and
    /// still on its way. The stage steps to the chips' levels at the next
    /// frame presented.
    ///
    /// The sound switch keeps the frame out of the host's ring and nothing
    /// else: a frame shown with the sound off and stems armed goes through
    /// the stage and makes its stems as with the sound on; one with nothing
    /// armed leaves the stage where it stands, as a headless frame does, so
    /// the frame that brings the sound back queues that frame's sound alone.
    pub(super) fn generate_and_mix_audio(&mut self, present: bool) {
        // Drift-free cycles->samples mapping. The converter is anchored
        // on `master_cycles`, so the delta between the previous
        // anchor's cumulative count and this frame's tells us exactly
        // how many stereo pairs to emit, bit-for-bit identical to a
        // single big-int compute done at the end of the session. Its
        // anchor is the frame's start, which the chips' times are placed
        // from before it moves on.
        let cumulative = self
            .audio_rate_converter
            .cycles_to_samples(self.master_cycles);
        let prev = self.audio_rate_converter.samples_at_reference();
        let samples_per_frame = cumulative.saturating_sub(prev) as usize;

        // The chips' records of the frame, taken whether it is heard or
        // not: the YM2612's up to the frame's end (a sample it made past
        // it, following the Z80, is the next frame's).
        let mut psg_record = std::mem::take(&mut self.psg_record);
        self.bus.psg_mut().take_output(&mut psg_record);
        let mut ym_record = std::mem::take(&mut self.ym_record);
        let mut ym_stem_rows = std::mem::take(&mut self.ym_stem_rows);
        self.bus
            .ym2612_mut()
            .take_output(self.master_cycles, &mut ym_record, &mut ym_stem_rows);

        // A paused / stepped emulator may report 0 delta (no cycle advance
        // since last call): no samples to render. With the sound off and
        // nothing armed, nobody takes them.
        let staged =
            present && samples_per_frame > 0 && (self.audio_enabled || self.stems.is_armed());
        if staged {
            self.stage_chips(&psg_record, &ym_record, &ym_stem_rows, samples_per_frame);
        }

        // Re-anchor: the next frame's count and times start here.
        // `cumulative` already includes the fractional remainder; carrying
        // it forward inside `advance` keeps the next frame's computation
        // drift-free.
        self.audio_rate_converter
            .advance(self.master_cycles, cumulative);
        self.psg_record = psg_record;
        self.ym_record = ym_record;
        self.ym_stem_rows = ym_stem_rows;

        // Everything above advanced serialized machine state (the converter
        // and the chips' records), so it ran on this frame regardless of
        // presentation. A headless frame stops here: the limiter, the
        // waveform ring, the stems and the host audio ring below are
        // presentation-only and never enter the netplay desync hash. See
        // the fn doc for why this ordering is load-bearing for rollback
        // determinism.
        if !staged {
            return;
        }

        // The limiter on the master (stereo interleaved: L, R, L, R, ...).
        // Always output exactly samples_per_frame to keep the audio buffer
        // fill rate matched to the video frame rate.
        let mix_count = samples_per_frame;
        if self.mix_sample_buf.len() < mix_count * 2 {
            self.mix_sample_buf.resize(mix_count * 2, 0.0);
        }
        for i in 0..mix_count {
            let mixed_left = soft_limit(self.master_sample_buf[i * 2]);
            let mixed_right = soft_limit(self.master_sample_buf[i * 2 + 1]);

            // The stereo-interleaved mixed output, staged for the ring.
            self.mix_sample_buf[i * 2] = mixed_left;
            self.mix_sample_buf[i * 2 + 1] = mixed_right;

            // Store mono mix for waveform visualization
            let mono = (mixed_left + mixed_right) * 0.5;
            if self.mixed_waveform_buffer.len() >= 2048 {
                self.mixed_waveform_buffer.pop_front();
            }
            self.mixed_waveform_buffer.push_back(mono);
        }

        // The ring is bounded; if the host is way behind (debugger
        // paused, tab backgrounded) `push_slice` quietly drops the
        // oldest samples and counts them, which `host_audio().overflows()`
        // reports. The sound switch keeps the frame out of it.
        if self.audio_enabled {
            self.host_audio
                .push_slice(&self.mix_sample_buf[..mix_count * 2]);
        }

        if self.stems.is_armed() {
            self.record_stems(mix_count);
        }
    }

    /// The chips' records of the frame through the host's stage onto its
    /// `frames` samples, into `master_sample_buf`: the master (the PSG's
    /// channels the debugger did not mute, at the PSG's share on both
    /// sides, and the YM2612's samples, which leave out its muted channels,
    /// at its share) and, while the stems are armed, each stem, from the
    /// stems the YM2612 staged beside its record (`ym_stem_rows`, a row an
    /// entry) and from the PSG's record. The converter still stands at the
    /// frame's start.
    fn stage_chips(
        &mut self,
        psg: &[(u64, PsgLevels)],
        ym: &[(u64, YmLevels)],
        ym_stem_rows: &[f32],
        frames: usize,
    ) {
        if self.master_sample_buf.len() < frames * 2 {
            self.master_sample_buf.resize(frames * 2, 0.0);
        }
        let mutes = self.bus.psg().channel_mutes();
        let divider = u64::from(self.region.cpu_divider());
        let converter = &self.audio_rate_converter;
        let psg_levels = psg.iter().map(|(time, levels)| {
            let at = psg_place(converter, *time, divider);
            (at, psg_master_level(levels, mutes))
        });
        let ym_levels = ym.iter().map(|(time, levels)| {
            let at = ym_place(converter, *time, divider);
            (at, fm_master_level(levels))
        });
        stage_frame(
            &mut self.master_stage,
            MasterLevels::new(psg_levels, ym_levels),
            frames,
            &mut self.master_sample_buf[..frames * 2],
        );
        let Some(stems) = self.stem_stages.as_deref_mut() else {
            return;
        };
        for (stem, (steps, samples)) in stems.fm.iter_mut().zip(&mut stems.fm_samples).enumerate() {
            samples.resize((frames * 2).max(samples.len()), 0.0);
            let levels = ym.iter().enumerate().map(|(row, (time, _))| {
                let at = ym_place(converter, *time, divider);
                let first = row * YM_STEM_VALUES + 2 * stem;
                let level = ym_stem_rows.get(first..first + 2).map_or([0.0; 2], |pair| {
                    [pair[0], pair[1]].map(|value| f64::from(value) * f64::from(FM_STEM_SCALE))
                });
                (at, level)
            });
            stage_frame(steps, levels, frames, &mut samples[..frames * 2]);
        }
        for (channel, (steps, samples)) in
            stems.psg.iter_mut().zip(&mut stems.psg_samples).enumerate()
        {
            samples.resize(frames.max(samples.len()), 0.0);
            let levels = psg.iter().map(|(time, levels)| {
                let at = psg_place(converter, *time, divider);
                (at, [f64::from(levels[channel]) * f64::from(PSG_STEM_SCALE)])
            });
            stage_frame(steps, levels, frames, &mut samples[..frames]);
        }
    }

    /// The frame's stems, one sample frame for each the mix made, as they
    /// enter the master; the master is the mix before its limiter. Each
    /// stem is its channel through its own stage. A sample frame departs
    /// where the limiter moved what the host hears, and where the stems'
    /// sum parts from the master by more than the layout's sum rule allows:
    /// the YM2612's clamp or a muted channel that sounds (a mute is the
    /// mix's, which leaves the channel out), and the span a stem's stage
    /// takes to reach its channel after the stems are armed.
    fn record_stems(&mut self, frames: usize) {
        let Some(stages) = self.stem_stages.as_deref() else {
            return;
        };
        let tolerance = StemSum::Exact.tolerance(SampleKind::F32);
        let mut values = [0.0f32; YM_STEM_VALUES + PSG_STEM_VALUES];
        for i in 0..frames {
            let master = [
                self.master_sample_buf[i * 2],
                self.master_sample_buf[i * 2 + 1],
            ];
            let mut departs = master[0] != self.mix_sample_buf[i * 2]
                || master[1] != self.mix_sample_buf[i * 2 + 1];
            let (fm, rest) = values.split_at_mut(YM_STEM_VALUES);
            for (pair, samples) in fm.as_chunks_mut::<2>().0.iter_mut().zip(&stages.fm_samples) {
                pair.copy_from_slice(&samples[i * 2..i * 2 + 2]);
            }
            for (stem, samples) in rest.iter_mut().zip(&stages.psg_samples) {
                *stem = samples[i];
            }
            let psg_sum: f64 = rest.iter().map(|&v| f64::from(v)).sum();
            for (side, &master) in master.iter().enumerate() {
                let fm_sum: f64 = fm.iter().skip(side).step_by(2).map(|&v| f64::from(v)).sum();
                departs |= (fm_sum + psg_sum - f64::from(master)).abs() > tolerance;
            }
            self.stems.push(&values, master, departs);
        }
    }

    /// The YM2612 stages each sample's channels for the stems while they
    /// are armed, and the stems have stages of their own, made from
    /// silence.
    pub(super) fn set_stem_taps(&mut self, on: bool) {
        self.bus.ym2612_mut().set_stem_tap(on);
        match (on, self.stem_stages.is_some()) {
            (true, false) => {
                self.stem_stages = Some(Box::new(StemStages::new(&self.stage_kernel)));
            }
            (false, true) => self.stem_stages = None,
            _ => {}
        }
    }

    /// The host's stages start again from silence: a reset.
    pub(super) fn clear_stages(&mut self) {
        self.master_stage.clear();
        if let Some(stems) = self.stem_stages.as_deref_mut() {
            stems.fm.iter_mut().for_each(BandLimitedSteps::clear);
            stems.psg.iter_mut().for_each(BandLimitedSteps::clear);
        }
    }

    /// The end of a frame for the stems: a frame the host was not shown is
    /// left out, and what the host armed since is armed now.
    pub(super) fn end_stem_frame(&mut self) {
        let was = self.stems.is_armed();
        self.stems.frame_end(self.present);
        if self.stems.is_armed() != was {
            self.set_stem_taps(self.stems.is_armed());
        }
    }
}
