//! Audio: the YM2612 and the PSG mixed at a frame's end into the host's
//! ring, the sound hooks a host calls (`SoundCore`), the stems the mix
//! takes, and the sound chips' debugger readers.

use retro_core::audio::{
    merge_by_time, SampleKind, SoundChipKind, SoundCore, SoundEvent, SoundLog, SoundLogContents,
    SoundLogEntry, SoundLogger, StemChannel, StemLayout, StemRecorder, StemSet, StemSum, StemTake,
};
use retro_core::frame::run_frame_on;
use retro_core::{EmulatorCore, ExecutionTier};

use super::GenesisEmulator;
use crate::psg::PSG_STEM_VALUES;
use crate::timing::GenesisTiming;
use crate::ym2612::{DacStatus, LfoStatus, TimerStatus, YM_STEM_VALUES};

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
/// YM2612 its channels' keys) on its own input clock; each frame's end
/// gathers them onto the master clock, the YM2612's before the PSG's at the
/// same time, then the frame's end, at the master clocks the frame ran. A
/// host setting, not saved.
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

/// A recorder of the stems at `rate`, holding at most two seconds.
pub(super) fn stem_recorder(rate: u32) -> StemRecorder<f32> {
    StemRecorder::new(&stem_layout(rate), rate * 2)
}

/// One side of a sample of the mix, before the limiter: the master the
/// stems sum to.
#[inline]
fn mix_sample(psg: f32, ym: f32) -> f32 {
    psg * PSG_SHARE + ym * FM_SHARE
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
    /// YM2612's alone.
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

    /// Each chip logs what `contents` asks of it: the PSG its writes.
    fn set_chip_logs(&mut self, contents: SoundLogContents) {
        self.bus.ym2612_mut().set_sound_log(contents);
        self.bus.psg_mut().set_sound_log(contents.writes);
    }

    /// What the chips logged since the log's mark, onto the log's clock,
    /// by time; where the log stands now. Each chip's input clocks are
    /// spread over the master clocks the machine ran since the mark: the
    /// chips are not always clocked in step with the master clock (the PSG
    /// gets more than its share of a line when the 68000 runs further past
    /// the line's end than past the one before, and inside a line the
    /// YM2612 runs ahead while the Z80 runs, then waits at the line's
    /// budget), so a chip's own count would carry its entries away from
    /// the other chip's and out of their frame.
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
        let merged = merge_by_time(&chips[0], &chips[1]);
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
    /// The first half (advancing `audio_rate_converter` and pulling each chip's
    /// samples, which drains its `clock_accumulator`) mutates *serialized
    /// machine state*, state the netplay desync hash (`state_hash`) covers, so
    /// it MUST run on every frame, audible or headless. If a headless
    /// rollback/catch-up frame skipped it, a re-simulated span would leave the
    /// converter + chip phase frozen where a live audible run advanced them, the
    /// two peers' hashes for that frame would differ, and the netplay desync
    /// check would fire a spurious full resync. The second half (the mix, the
    /// waveform-visualization ring, and the host audio ring) is pure
    /// presentation (none of it is serialized), so a headless frame returns
    /// before it: audio is generated but not rendered, exactly as rollback
    /// netcode requires. So a headless frame queues nothing, and after a
    /// rollback's frames there is nothing pending to discard (the trait's
    /// `discard_pending_audio` does nothing here): clearing the host ring
    /// instead would throw away sound presented and still on its way.
    pub(super) fn generate_and_mix_audio(&mut self, present: bool) {
        if !self.audio_enabled {
            return;
        }

        // Drift-free cycles->samples mapping. The converter is anchored
        // on `master_cycles`, so the delta between the previous
        // anchor's cumulative count and this frame's tells us exactly
        // how many stereo pairs to emit, bit-for-bit identical to a
        // single big-int compute done at the end of the session.
        let cumulative = self
            .audio_rate_converter
            .cycles_to_samples(self.master_cycles);
        let prev = self.audio_rate_converter.samples_at_reference();
        let samples_per_frame = cumulative.saturating_sub(prev) as usize;
        // Re-anchor immediately. `cumulative` already includes the
        // fractional remainder; carrying it forward inside `advance`
        // keeps the next frame's computation drift-free.
        self.audio_rate_converter
            .advance(self.master_cycles, cumulative);

        // Edge case: a paused / stepped emulator may report 0 delta
        // (no cycle advance since last call). Skip the heavy mix path
        // entirely: pushing an empty slice would be harmless but
        // wastes the chip generation work below.
        if samples_per_frame == 0 {
            return;
        }

        // Ensure pre-allocated buffers are large enough (resize only if needed)
        if self.psg_sample_buf.len() < samples_per_frame {
            self.psg_sample_buf.resize(samples_per_frame, 0.0);
        }
        if self.ym_sample_buf.len() < samples_per_frame * 2 {
            self.ym_sample_buf.resize(samples_per_frame * 2, 0.0);
        }
        if self.mix_sample_buf.len() < samples_per_frame * 2 {
            self.mix_sample_buf.resize(samples_per_frame * 2, 0.0);
        }

        // Zero the used portion of buffers
        for v in &mut self.psg_sample_buf[..samples_per_frame] {
            *v = 0.0;
        }
        for v in &mut self.ym_sample_buf[..samples_per_frame * 2] {
            *v = 0.0;
        }

        // Generate samples from PSG and YM2612.
        // Both buffers were pre-zeroed so any samples beyond what each chip
        // produces are already silence (0.0).
        use retro_core::audio::AudioChip;
        let _psg_count = self.bus.psg_mut().generate_samples(
            &mut self.psg_sample_buf[..samples_per_frame],
            self.audio_sample_rate,
        );

        let ym_count = self.bus.ym2612_mut().generate_samples(
            &mut self.ym_sample_buf[..samples_per_frame * 2],
            self.audio_sample_rate,
        );
        let _ym_stereo_count = ym_count / 2;

        // Everything above advanced serialized machine state (the converter and
        // each chip's `clock_accumulator`), so it ran on this frame regardless of
        // presentation. A headless frame stops here: the mix, the waveform ring,
        // and the host audio ring below are presentation-only and never enter the
        // netplay desync hash. See the fn doc for why this ordering is
        // load-bearing for rollback determinism. The stems follow the host:
        // what the chips staged for them goes with the frame.
        if !present {
            self.clear_staged_stems();
            return;
        }

        // Mix PSG + YM2612 (stereo interleaved: L, R, L, R, ...)
        // Always output exactly samples_per_frame to keep the audio buffer fill
        // rate matched to the video frame rate. Both sample buffers are pre-zeroed,
        // so if either chip produced slightly fewer samples, the tail is silence.
        let mix_count = samples_per_frame;

        for i in 0..mix_count {
            // PSG is mono - duplicate to L and R
            let psg = self.psg_sample_buf[i];

            // YM2612 is stereo interleaved
            let ym_left = self.ym_sample_buf[i * 2];
            let ym_right = self.ym_sample_buf[i * 2 + 1];

            // Mix: PSG (mono->both) + YM2612 (stereo), then the limiter.
            let mixed_left = soft_limit(mix_sample(psg, ym_left));
            let mixed_right = soft_limit(mix_sample(psg, ym_right));

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
        // reports.
        self.host_audio
            .push_slice(&self.mix_sample_buf[..mix_count * 2]);

        if self.stems.is_armed() {
            self.record_stems(mix_count);
        }
        self.clear_staged_stems();
    }

    /// The frame's stems from what the chips staged, one sample frame for
    /// each the mix made, as they enter the master; the master is the mix
    /// before its limiter. A sample frame departs where the YM2612's clamp,
    /// the limiter or a muted channel that sounds moved what the host hears
    /// away from the stems' sum, and where a chip handed the mix a sample
    /// it staged none for.
    fn record_stems(&mut self, frames: usize) {
        let (Some((ym_values, ym_departs)), Some((psg_values, psg_departs))) = (
            self.bus.ym2612().staged_stems(),
            self.bus.psg().staged_stems(),
        ) else {
            return;
        };
        let mut values = [0.0f32; YM_STEM_VALUES + PSG_STEM_VALUES];
        for i in 0..frames {
            let psg = self.psg_sample_buf[i];
            let (ym_left, ym_right) = (self.ym_sample_buf[i * 2], self.ym_sample_buf[i * 2 + 1]);
            let master = [mix_sample(psg, ym_left), mix_sample(psg, ym_right)];
            let mut departs = master[0] != self.mix_sample_buf[i * 2]
                || master[1] != self.mix_sample_buf[i * 2 + 1];
            let (fm, rest) = values.split_at_mut(YM_STEM_VALUES);
            match ym_values.get(i * YM_STEM_VALUES..(i + 1) * YM_STEM_VALUES) {
                Some(staged) => {
                    for (stem, &value) in fm.iter_mut().zip(staged) {
                        *stem = value * FM_STEM_SCALE;
                    }
                    departs |= ym_departs[i];
                }
                None => {
                    fm.fill(0.0);
                    departs |= ym_left != 0.0 || ym_right != 0.0;
                }
            }
            match psg_values.get(i * PSG_STEM_VALUES..(i + 1) * PSG_STEM_VALUES) {
                Some(staged) => {
                    for (stem, &value) in rest.iter_mut().zip(staged) {
                        *stem = value * PSG_STEM_SCALE;
                    }
                    departs |= psg_departs[i];
                }
                None => {
                    rest.fill(0.0);
                    departs |= psg != 0.0;
                }
            }
            self.stems.push(&values, master, departs);
        }
    }

    /// Drop what the chips staged for the stems: the frame's samples went.
    fn clear_staged_stems(&mut self) {
        self.bus.ym2612_mut().clear_staged_stems();
        self.bus.psg_mut().clear_staged_stems();
    }

    /// The chips stage each sample for the stems while they are armed.
    pub(super) fn set_stem_taps(&mut self, on: bool) {
        self.bus.ym2612_mut().set_stem_tap(on);
        self.bus.psg_mut().set_stem_tap(on);
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
