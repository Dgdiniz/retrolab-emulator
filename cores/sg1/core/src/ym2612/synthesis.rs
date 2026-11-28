//! The synthesis: the chip's own samples owed for the input clocks
//! received, one sample through the six channels, the timers, and the chip
//! read alone by a sample-and-hold; and the stems' tap on each sample's
//! channels.

use retro_core::audio::{AudioChip, AudioChipInfo, ChannelType, IntegerRateConverter, ScopeRing};
use retro_core::cpu::Clockable;

use super::{
    lfo, Ym2612, YmLevels, CLOCKS_PER_SAMPLE, MAX_OUTPUT_SAMPLES, MAX_TIMER_ACCUMULATOR,
    SAMPLES_PER_ENVELOPE_STEP,
};

/// The values a sample stages for the stems: FM channels 1 to 6, then the
/// DAC, each left and right.
pub const YM_STEM_VALUES: usize = 14;

/// The stems' tap: each sample's channel outputs as `channel_outputs` holds
/// them, muted or not, staged beside the record (one row a record entry,
/// taken and dropped with them) until the system's mix takes the frame;
/// and each channel's last samples for its waveform.
pub(super) struct YmStems {
    /// [`YM_STEM_VALUES`] a row, a row an entry of the record: the first the
    /// values the record started from, then each sample's. Channel 6 sits in
    /// its FM slot while it plays FM, in the DAC's while the DAC replaces
    /// it.
    staged: Vec<f32>,
    scopes: [ScopeRing; 6],
}

impl YmStems {
    fn new() -> Self {
        Self {
            staged: Vec::with_capacity(1024 * YM_STEM_VALUES),
            scopes: std::array::from_fn(|_| ScopeRing::new()),
        }
    }

    /// One sample's channels.
    #[inline]
    fn record(&mut self, outputs: &[(f32, f32); 6], dac: bool) {
        for (channel, &(left, right)) in outputs.iter().enumerate() {
            let (fm_left, fm_right) = if channel == 5 && dac {
                (0.0, 0.0)
            } else {
                (left, right)
            };
            self.staged.push(fm_left);
            self.staged.push(fm_right);
            self.scopes[channel].push((left + right) * 0.5);
        }
        let (dac_left, dac_right) = if dac { outputs[5] } else { (0.0, 0.0) };
        self.staged.push(dac_left);
        self.staged.push(dac_right);
    }

    /// Rows standing for `entries` entries of a record the chip made with
    /// no tap (a frame restored from a savestate, a tap just started):
    /// silence.
    pub(super) fn align(&mut self, entries: usize) {
        self.staged.clear();
        self.staged.resize(entries * YM_STEM_VALUES, 0.0);
    }

    /// The record starts again: its first row the last one staged.
    pub(super) fn restart(&mut self) {
        let rows = self.staged.len() / YM_STEM_VALUES;
        if rows == 0 {
            self.staged.resize(YM_STEM_VALUES, 0.0);
        } else {
            self.staged.drain(..(rows - 1) * YM_STEM_VALUES);
        }
    }

    /// The first `rows` rows into `into`; the record goes on from the last
    /// of them.
    pub(super) fn take(&mut self, rows: usize, into: &mut Vec<f32>) {
        let rows = rows.min(self.staged.len() / YM_STEM_VALUES).max(1);
        into.extend_from_slice(&self.staged[..rows * YM_STEM_VALUES]);
        self.staged.drain(..(rows - 1) * YM_STEM_VALUES);
    }

    /// Drop the oldest `rows` rows but the record's first, as the record
    /// drops its oldest samples: the first becomes the last row dropped.
    fn drop_oldest(&mut self, rows: usize) {
        let rows = rows.min((self.staged.len() / YM_STEM_VALUES).saturating_sub(1));
        self.staged.drain(..rows * YM_STEM_VALUES);
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
                stems.align(self.output.len());
                self.stems = Some(stems);
            }
            (false, true) => self.stems = None,
            _ => {}
        }
    }

    /// Whether the stems' tap is on.
    pub fn stem_tap(&self) -> bool {
        self.stems.is_some()
    }

    /// Stage, for the entries the record holds, rows that stand for them:
    /// after a load replaced it.
    pub(super) fn align_stems(&mut self) {
        let entries = self.output.len();
        if let Some(stems) = self.stems.as_deref_mut() {
            stems.align(entries);
        }
    }
}

impl Ym2612 {
    /// Make every sample the input clocks received since the last one come
    /// to, one each 144, with the chip's current register state, each kept
    /// in the record at the input clock it was made at. Called before each
    /// register write (so the write's effect starts at the right sample)
    /// and when the record is taken.
    pub(super) fn catch_up(&mut self) {
        if !self.rendering_enabled {
            // Audio off: no synthesis, only the place among a sample's clocks.
            self.unsampled %= CLOCKS_PER_SAMPLE;
            return;
        }

        while self.unsampled >= CLOCKS_PER_SAMPLE {
            self.unsampled -= CLOCKS_PER_SAMPLE;
            let (left, right) = self.generate_one_sample();
            let time = self.position.saturating_sub(u64::from(self.unsampled));
            self.output.push((time, [left, right]));
        }
        // Bound the record if nothing takes it: the oldest samples fold into
        // the level it starts from.
        if self.output.len() > MAX_OUTPUT_SAMPLES {
            let dropped = self.output.len() - MAX_OUTPUT_SAMPLES / 2;
            self.output.drain(..dropped);
            if let Some(stems) = self.stems.as_deref_mut() {
                stems.drop_oldest(dropped);
            }
        }
    }

    /// Make one of the chip's samples by running the full FM pipeline once.
    /// Returns (left, right) in [-1.0, 1.0].
    fn generate_one_sample(&mut self) -> (f32, f32) {
        self.lfo.clock(lfo::CHIP_SAMPLE);

        // The envelopes step every third sample (clock / 144 / 3 ~ 17.8 kHz).
        self.eg_samples += 1;
        let advance_eg = self.eg_samples == SAMPLES_PER_ENVELOPE_STEP;
        if advance_eg {
            self.eg_samples = 0;
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
            stems.record(&self.channel_outputs, self.dac_enabled);
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
        // Count the input clocks and run the timers. The samples are made by
        // `catch_up()` at each register write and when the record is taken,
        // so register changes land on their own sample.
        self.busy_counter = self.busy_counter.saturating_sub(cycles);
        self.input_clocks += u64::from(cycles);
        self.position += u64::from(cycles);
        self.clock_timers(cycles);
        self.unsampled += cycles;
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

    /// The chip's output read by a sample-and-hold at `sample_rate`: each
    /// output sample, interleaved left and right, the chip's sample held at
    /// its time, from the record, which this takes; as many as the input
    /// clocks received since the last call came to, the first
    /// `output.len() / 2` of them written. For a tool or a test reading the
    /// chip alone: the machine's sound goes through its band-limited host
    /// stage instead, which takes the record itself.
    fn generate_samples(&mut self, output: &mut [f32], sample_rate: u32) -> usize {
        if sample_rate != self.sample_rate {
            self.sample_rate = sample_rate;
            self.sampler = None;
        }
        let mut record = Vec::new();
        let mut stems = Vec::new();
        self.take_output(self.position, &mut record, &mut stems);
        if !self.rendering_enabled {
            self.sampler = None;
            return 0;
        }
        let start = record.first().map_or(self.position, |entry| entry.0);
        let sampler = self.sampler.get_or_insert_with(|| {
            let mut sampler =
                IntegerRateConverter::new(u64::from(sample_rate), u64::from(self.clock_freq));
            sampler.reset(start);
            sampler
        });
        let cumulative = sampler.cycles_to_samples(self.position);
        let samples = (cumulative - sampler.samples_at_reference()) as usize;
        let mut level: YmLevels = record.first().map_or([0.0; 2], |entry| entry.1);
        let mut next = 1;
        let written = samples.min(output.len() / 2);
        for (j, pair) in output[..2 * written]
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .enumerate()
        {
            // A sample made before this output sample's time is held for it.
            while let Some(&(time, made)) = record.get(next) {
                if sampler.position(time, 1).sample >= j as i64 {
                    break;
                }
                level = made;
                next += 1;
            }
            pair.copy_from_slice(&level);
        }
        sampler.advance(self.position, cumulative);
        2 * written
    }

    fn reset(&mut self) {
        Ym2612::reset(self);
    }

    fn get_output_level(&self) -> f32 {
        self.output_level
    }
}
