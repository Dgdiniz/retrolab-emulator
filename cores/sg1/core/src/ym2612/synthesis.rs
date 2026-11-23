//! The synthesis: the output samples owed for the input clocks received,
//! one sample through the six channels, the timers, and the chip as the
//! system's mix pulls it.

use retro_core::audio::{AudioChip, AudioChipInfo, ChannelType};
use retro_core::cpu::Clockable;

use super::{Ym2612, MAX_BORROWED_PAIRS, MAX_FRAME_SAMPLES, MAX_TIMER_ACCUMULATOR};

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
        }
    }

    /// Generate one output sample by running the full FM pipeline once.
    /// Returns (left, right) in [-1.0, 1.0].
    /// Called ~735 times per frame (at 44.1 kHz / 60 fps) instead of ~128K times.
    fn generate_one_sample(&mut self) -> (f32, f32) {
        self.lfo.clock();

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
                self.channels[2].key_off_all();
                self.csm_key_state &= !0x02;
            }

            let timer_a_overflow = self.timers.clock_timer_a();
            if timer_a_overflow && self.timers.is_csm_mode() {
                self.channels[2].key_on_all();
                self.csm_key_state |= 0x01;
            }
        }
        if timer_ticks > 0 {
            self.timers.clock_timer_b(timer_ticks);
        }
    }
}

impl Clockable for Ym2612 {
    fn clock(&mut self, cycles: u32) -> u32 {
        // Accumulate input clocks and run timers. Audio is rendered incrementally
        // by `catch_up()` at each register write and flushed by `generate_samples()`,
        // so register changes land at their true sub-frame sample position.
        self.busy_counter = self.busy_counter.saturating_sub(cycles);
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
