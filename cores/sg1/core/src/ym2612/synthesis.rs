//! The synthesis: the output samples owed for the input clocks received,
//! one sample through the six channels, the timers, and the chip as the
//! system's mix pulls it; and the stems' tap on each sample's channels.

use retro_core::audio::{AudioChip, AudioChipInfo, ChannelType, ScopeRing};
use retro_core::cpu::Clockable;

use super::{Ym2612, MAX_BORROWED_PAIRS, MAX_FRAME_SAMPLES, MAX_TIMER_ACCUMULATOR};

/// The values a sample stages for the stems: FM channels 1 to 6, then the
/// DAC, each left and right.
pub const YM_STEM_VALUES: usize = 14;

/// The stems' tap: each sample's channel outputs as `channel_outputs` holds
/// them, muted or not, staged beside `frame_samples` (one entry a sample,
/// trimmed and dropped with them) until the system's mix takes the frame;
/// and each channel's last samples for its waveform.
pub(super) struct YmStems {
    /// [`YM_STEM_VALUES`] a sample: channel 6 in its FM slot while it plays
    /// FM, in the DAC's while the DAC replaces it.
    staged: Vec<f32>,
    /// A sample: whether the chip's clamp or a muted channel that sounds
    /// moved its sum away from the channels'.
    departs: Vec<bool>,
    scopes: [ScopeRing; 6],
}

impl YmStems {
    fn new() -> Self {
        Self {
            staged: Vec::with_capacity(2048 * YM_STEM_VALUES / 2),
            departs: Vec::with_capacity(2048),
            scopes: std::array::from_fn(|_| ScopeRing::new()),
        }
    }

    /// One sample's channels.
    #[inline]
    fn record(&mut self, outputs: &[(f32, f32); 6], dac: bool, muted: &[bool; 6], clamped: bool) {
        let mut departs = clamped;
        for (channel, &(left, right)) in outputs.iter().enumerate() {
            let (fm_left, fm_right) = if channel == 5 && dac {
                (0.0, 0.0)
            } else {
                (left, right)
            };
            self.staged.push(fm_left);
            self.staged.push(fm_right);
            departs |= muted[channel] && (left != 0.0 || right != 0.0);
            self.scopes[channel].push((left + right) * 0.5);
        }
        let (dac_left, dac_right) = if dac { outputs[5] } else { (0.0, 0.0) };
        self.staged.push(dac_left);
        self.staged.push(dac_right);
        self.departs.push(departs);
    }

    /// Staged samples standing for `samples` samples the chip made with no
    /// tap (a frame restored from a savestate): silence, each a departure.
    pub(super) fn align(&mut self, samples: usize) {
        self.staged.clear();
        self.staged.resize(samples * YM_STEM_VALUES, 0.0);
        self.departs.clear();
        self.departs.resize(samples, true);
    }

    /// Drop the oldest `samples` staged samples, as `frame_samples` drops
    /// them.
    fn drop_oldest(&mut self, samples: usize) {
        let samples = samples.min(self.departs.len());
        self.staged.drain(..samples * YM_STEM_VALUES);
        self.departs.drain(..samples);
    }

    /// Channel `channel`'s last `samples`, oldest first.
    pub(super) fn scope(&self, channel: usize, samples: u32) -> Vec<f32> {
        match self.scopes.get(channel) {
            Some(scope) => scope.read(samples),
            None => vec![0.0; samples as usize],
        }
    }
}

impl Ym2612 {
    /// Stage each sample's channels for the stems from now on (`true`), or
    /// stop. A host setting, not saved.
    pub fn set_stem_tap(&mut self, on: bool) {
        match (on, self.stems.is_some()) {
            (true, false) => {
                let mut stems = Box::new(YmStems::new());
                stems.align(self.frame_samples.len() / 2);
                self.stems = Some(stems);
            }
            (false, true) => self.stems = None,
            _ => {}
        }
    }

    /// The samples staged for the stems, one per sample `frame_samples`
    /// holds: [`YM_STEM_VALUES`] values each, and whether each departs;
    /// `None` while the tap is off.
    pub fn staged_stems(&self) -> Option<(&[f32], &[bool])> {
        self.stems
            .as_deref()
            .map(|stems| (&stems.staged[..], &stems.departs[..]))
    }

    /// Drop the samples staged for the stems, as the mix drops the frame's
    /// samples once it has taken them.
    pub fn clear_staged_stems(&mut self) {
        if let Some(stems) = self.stems.as_deref_mut() {
            stems.align(0);
        }
    }

    /// Stage, for the samples `frame_samples` holds, samples that stand for
    /// them: after a load replaced them.
    pub(super) fn align_stems(&mut self) {
        let samples = self.frame_samples.len() / 2;
        if let Some(stems) = self.stems.as_deref_mut() {
            stems.align(samples);
        }
    }
}

impl Ym2612 {
    /// Render every whole output sample owed for the clocks accumulated since the
    /// last render point, appending them to `frame_samples` using the chip's
    /// current register state. Called before each register write (so the write's
    /// effect starts at the right sample) and once more at frame end by
    /// `generate_samples()`.
    pub(super) fn catch_up(&mut self) {
        if !self.rendering_enabled {
            // Audio off: don't synthesize, just keep the accumulator bounded.
            if self.clock_accumulator >= self.clocks_per_sample {
                self.clock_accumulator %= self.clocks_per_sample;
            }
            return;
        }

        while self.clock_accumulator >= self.clocks_per_sample {
            self.clock_accumulator -= self.clocks_per_sample;
            let (left, right) = self.generate_one_sample();
            self.frame_samples.push(left);
            self.frame_samples.push(right);
        }
        // Bound the buffer if the host stops draining it, so it can't grow without limit.
        if self.frame_samples.len() > MAX_FRAME_SAMPLES {
            let excess = self.frame_samples.len() - MAX_FRAME_SAMPLES;
            self.frame_samples.drain(..excess);
            if let Some(stems) = self.stems.as_deref_mut() {
                stems.drop_oldest(excess / 2);
            }
        }
    }

    /// Generate one output sample by running the full FM pipeline once.
    /// Returns (left, right) in [-1.0, 1.0].
    /// Called ~735 times per frame (at 44.1 kHz / 60 fps) instead of ~128K times.
    fn generate_one_sample(&mut self) -> (f32, f32) {
        self.lfo.clock(self.chip_samples_per_sample);

        // EG advance: hardware rate = clock_freq / 432 ~ 17.8 kHz.
        // eg_samples_per_tick = sample_rate / (clock_freq / 432) ~ 2.48
        self.eg_accumulator += 1.0;
        let advance_eg = self.eg_accumulator >= self.eg_samples_per_tick;
        if advance_eg {
            self.eg_accumulator -= self.eg_samples_per_tick;
            self.eg_counter += 1;
            if self.eg_counter >= 4096 {
                self.eg_counter = 1;
            }
        }

        let pm_step = self.lfo.get_pm_step();
        let am_value = self.lfo.get_am_value();

        let mut left_sum = 0.0f32;
        let mut right_sum = 0.0f32;

        // Every channel runs whatever its mute: a mute is the mix's, which
        // leaves the channel's output out of the sum.
        for i in 0..6 {
            let output = if i == 5 && self.dac_enabled {
                let dac_scaled = (self.dac_value as i32) << 6;
                let dac_f32 = dac_scaled as f32 / 8192.0;
                let stereo = self.channels[5].get_stereo();
                let left_dac = if stereo.left { dac_f32 } else { 0.0 };
                let right_dac = if stereo.right { dac_f32 } else { 0.0 };
                (left_dac, right_dac)
            } else {
                let (left, right) =
                    self.channels[i].clock_with_lfo_eg(pm_step, am_value, advance_eg);
                (left as f32 / 8192.0, right as f32 / 8192.0)
            };
            self.channel_outputs[i] = output;
            if !self.mute_flags[i] {
                left_sum += output.0;
                right_sum += output.1;
            }
        }

        left_sum /= 6.0;
        right_sum /= 6.0;

        let left_sample = left_sum.clamp(-1.0, 1.0);
        let right_sample = right_sum.clamp(-1.0, 1.0);

        if let Some(stems) = self.stems.as_deref_mut() {
            let clamped = left_sample != left_sum || right_sample != right_sum;
            stems.record(
                &self.channel_outputs,
                self.dac_enabled,
                &self.mute_flags,
                clamped,
            );
        }

        let abs_l = left_sample.abs();
        let abs_r = right_sample.abs();
        self.output_level = if abs_l > abs_r { abs_l } else { abs_r };

        // Waveform visualization
        let mono = (left_sample + right_sample) * 0.5;
        if self.waveform_buffer.len() >= self.max_waveform_samples {
            self.waveform_buffer.pop_front();
        }
        self.waveform_buffer.push_back(mono);

        (left_sample, right_sample)
    }

    /// Run timers for the given number of input clock cycles.
    /// Timers tick at the internal FM rate (every 144 input clocks).
    fn clock_timers(&mut self, cycles: u32) {
        let start = self.timer_accumulator;
        self.timer_accumulator += cycles as f32;
        if self.timer_accumulator > MAX_TIMER_ACCUMULATOR {
            self.timer_accumulator = MAX_TIMER_ACCUMULATOR;
        }
        let mut timer_ticks: i32 = 0;
        while self.timer_accumulator >= 144.0 {
            self.timer_accumulator -= 144.0;
            timer_ticks += 1;

            self.csm_key_state <<= 1;
            if self.csm_key_state & 0x02 != 0 {
                let keyed_before = self.logs_keys().then(|| self.keyed_operators(2));
                self.channels[2].key_off_all();
                self.csm_key_state &= !0x02;
                if let Some(before) = keyed_before {
                    let clock = self.tick_clock(cycles, start, timer_ticks);
                    self.log_key_change(clock, 2, before);
                }
            }

            let timer_a_overflow = self.timers.clock_timer_a();
            if timer_a_overflow && self.timers.is_csm_mode() {
                let keyed_before = self.logs_keys().then(|| self.keyed_operators(2));
                self.channels[2].key_on_all();
                self.csm_key_state |= 0x01;
                if let Some(before) = keyed_before {
                    let clock = self.tick_clock(cycles, start, timer_ticks);
                    self.log_key_change(clock, 2, before);
                }
            }
        }
        if timer_ticks > 0 {
            self.timers.clock_timer_b(timer_ticks);
        }
    }

    /// The input clock of tick `tick` (from 1) of a `clock(cycles)` call
    /// that found `start` clocks in the timers' accumulator: each tick comes
    /// 144 clocks after the one before, the first once the accumulator
    /// reaches 144.
    fn tick_clock(&self, cycles: u32, start: f32, tick: i32) -> u64 {
        let into_call = (144.0 * tick as f32 - start) as u64;
        self.input_clocks - u64::from(cycles) + into_call
    }
}

impl Clockable for Ym2612 {
    fn clock(&mut self, cycles: u32) -> u32 {
        // Accumulate input clocks and run timers. Audio is rendered incrementally
        // by `catch_up()` at each register write and flushed by `generate_samples()`,
        // so register changes land at their true sub-frame sample position.
        self.busy_counter = self.busy_counter.saturating_sub(cycles);
        self.input_clocks += u64::from(cycles);
        self.clock_timers(cycles);
        self.clock_accumulator += cycles as f32;
        cycles
    }
}

impl AudioChip for Ym2612 {
    fn chip_name(&self) -> &'static str {
        "YM2612"
    }

    fn get_info(&self) -> AudioChipInfo {
        AudioChipInfo {
            name: "YM2612",
            manufacturer: "Yamaha",
            channels: 6,
            sample_rate: self.sample_rate,
            stereo: true,
            channel_types: vec![
                ChannelType::FM,
                ChannelType::FM,
                ChannelType::FM,
                ChannelType::FM,
                ChannelType::FM,
                if self.dac_enabled {
                    ChannelType::PCM
                } else {
                    ChannelType::FM
                }, // Channel 6: FM or DAC
            ],
        }
    }

    /// `address` is the port, 0-3, as at $4000-$4003: port 0 (or 2) latches
    /// a register number of part 0 (or 1), port 1 (or 3) writes its data.
    /// A single byte cannot hold both a part and a register number, so the
    /// chip is driven here as a program drives it.
    fn write_register(&mut self, address: u8, data: u8) {
        self.write_port(address & 0x03, data);
    }

    /// `address` is the port, 0-3: every port reads the status register.
    fn read_register(&mut self, address: u8) -> u8 {
        self.read_port(address & 0x03)
    }

    fn generate_samples(&mut self, output: &mut [f32], sample_rate: u32) -> usize {
        if sample_rate != self.sample_rate {
            self.sample_rate = sample_rate;
            self.retime();
            for ch in &mut self.channels {
                ch.set_sample_rate(sample_rate);
            }
        }

        // Flush the samples owed for the clocks accumulated since the last write
        // (rendered at the current register state), then hand the caller what it
        // asked for. The drift-free sample *count* over time is owned by the
        // system's cycles->samples converter (see `system/audio.rs`), not by this buffer,
        // so any whole samples beyond what the caller drains are a sub-sample
        // rounding tail that must NOT persist: carrying them would replay stale
        // audio on the next call after a register write has since changed the chip
        // state. Drop the remainder so every `generate_samples` reflects state up
        // to *now*. Sub-frame accuracy is provided by `catch_up()` running at each
        // `write_port`, not by cross-call buffering.
        self.catch_up();

        let want = output.len() & !1; // copy whole stereo pairs only

        // Fill a one-pair shortfall rather than letting it reach the mixer as
        // silence. The system's cycles->samples converter owns the frame's
        // sample count, and this chip's `clock_accumulator` is a second,
        // floating-point quantisation of the same elapsed time -- so on a few
        // frames in a hundred it lands exactly one stereo pair short of what
        // the caller asked for. The caller's buffer is pre-zeroed, so an unfilled
        // pair would reach the mixer as a sample of silence in the middle of a
        // sustained note: an audible tick a few times a second, loudest where
        // the waveform sits furthest from zero.
        //
        // Each borrowed pair is charged to the accumulator, so the chip gives
        // the time back on a later frame and its long-run rate is unchanged.
        // The borrow is capped because two quantisations of one elapsed time
        // can only disagree by about a pair: a caller handing over an oversized
        // scratch buffer is asking for audio the machine has not lived through
        // yet, and that must not drag the chip ahead of it.
        if self.rendering_enabled {
            let mut borrowed = 0;
            while self.frame_samples.len() < want && borrowed < MAX_BORROWED_PAIRS {
                self.clock_accumulator -= self.clocks_per_sample;
                let (left, right) = self.generate_one_sample();
                self.frame_samples.push(left);
                self.frame_samples.push(right);
                borrowed += 1;
            }
        }

        let n = want.min(self.frame_samples.len() & !1);
        output[..n].copy_from_slice(&self.frame_samples[..n]);
        self.frame_samples.clear();

        n
    }

    fn reset(&mut self) {
        Ym2612::reset(self);
    }

    fn get_output_level(&self) -> f32 {
        self.output_level
    }
}
