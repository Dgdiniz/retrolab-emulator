//! The YM2612 into and out of the savestate's layout, and its busy time
//! into and out of the extension after it.
//!
//! The layout's fields keep the meaning older builds give them, which made
//! the chip's samples at the host's rate: each operator's phase increment
//! is written scaled for 44.1 kHz on the chip's clock, as they hold it, and
//! read back by the scale's exact inverse on the clock of the state; the
//! input clocks not yet made into samples are written where their count
//! was, and the samples since the envelopes last stepped where theirs was.

use super::{Ym2612, SAMPLES_PER_ENVELOPE_STEP};
use crate::savestate::{Ym2612Extension, Ym2612State};

/// The host's rate older builds made the chip's samples at, the rate the
/// increments a savestate holds are scaled for.
const SAVED_INCREMENT_RATE: u64 = 44_100;

/// The scale an older build applies to the chip's own increment on a clock
/// of `clock_freq`, in 16.16 fixed point: the chip's samples in one of the
/// host's, `(clock << 16) / (144 x 44,100)`, about 1.2 on a console's
/// clock.
pub(super) fn increment_scale(clock_freq: u32) -> u64 {
    (u64::from(clock_freq) << 16) / (144 * SAVED_INCREMENT_RATE)
}

/// The increment a savestate holds for the chip's own `increment`.
pub(super) fn saved_increment(increment: u32, scale: u64) -> u32 {
    ((u64::from(increment) * scale) >> 16) as u32
}

/// The chip's own increment from one a savestate holds. A scale above 1
/// gives each own increment a saved one of its own, so the smallest own
/// increment whose saved one reaches `saved` is the one it was made from.
pub(super) fn own_increment(saved: u32, scale: u64) -> u32 {
    (u64::from(saved) << 16)
        .div_ceil(scale.max(1))
        .min(u64::from(u32::MAX)) as u32
}

impl Ym2612 {
    pub fn to_savestate(&self) -> Ym2612State {
        let (lfo_enabled, lfo_freq, lfo_phase, lfo_counter) = self.lfo.to_savestate();
        let scale = increment_scale(self.clock_freq);
        let mut channels = [
            self.channels[0].to_savestate(),
            self.channels[1].to_savestate(),
            self.channels[2].to_savestate(),
            self.channels[3].to_savestate(),
            self.channels[4].to_savestate(),
            self.channels[5].to_savestate(),
        ];
        for operator in channels.iter_mut().flat_map(|ch| ch.operators.iter_mut()) {
            operator.phase_increment = saved_increment(operator.phase_increment, scale);
        }
        Ym2612State {
            channels,
            lfo_enabled,
            lfo_freq,
            lfo_phase,
            lfo_counter,
            dac_enabled: self.dac_enabled,
            dac_value: self.dac_value,
            dac_start_value: self.dac_start_value,
            clock_accumulator: self.unsampled as f32,
            timer_accumulator: self.timer_accumulator,
            ch3_mode: self.ch3_mode,
            ch3_fnum: self.ch3_fnum,
            ch3_block: self.ch3_block,
            csm_key_state: self.csm_key_state,
            address_latch_part0: self.address_latch_part0,
            address_latch_part1: self.address_latch_part1,
            eg_accumulator: self.eg_samples as f32,
            eg_counter: self.eg_counter,
            busy_end_cycle: self.busy_end_cycle,
            current_cpu_cycle: self.current_cpu_cycle,
            timer: self.timers.to_savestate(),
        }
    }

    /// Make the chip the state's, the state made on a chip clocked at
    /// `state_clock` (the YM2612's clock of the state's region), which its
    /// increments are scaled on. A count an older build kept below 0 after
    /// it made a sample ahead reads as 0. The record starts afresh from the
    /// level the chip held ([`Self::restore_frame_output`] then gives it
    /// the state's).
    pub fn from_savestate(&mut self, s: &Ym2612State, state_clock: u32) {
        let scale = increment_scale(state_clock);
        for (channel, saved) in self.channels.iter_mut().zip(&s.channels) {
            channel.from_savestate(saved);
            for (index, operator) in saved.operators.iter().enumerate() {
                channel
                    .operator_mut(index)
                    .set_phase_increment(own_increment(operator.phase_increment, scale));
            }
        }
        self.lfo
            .from_savestate(s.lfo_enabled, s.lfo_freq, s.lfo_phase, s.lfo_counter);
        self.dac_enabled = s.dac_enabled;
        self.dac_value = s.dac_value;
        self.dac_start_value = s.dac_start_value;
        self.unsampled = s.clock_accumulator.max(0.0) as u32;
        self.timer_accumulator = s.timer_accumulator;
        self.ch3_mode = s.ch3_mode;
        self.ch3_fnum = s.ch3_fnum;
        self.ch3_block = s.ch3_block;
        self.csm_key_state = s.csm_key_state;
        self.address_latch_part0 = s.address_latch_part0;
        self.address_latch_part1 = s.address_latch_part1;
        self.eg_samples = (s.eg_accumulator.max(0.0) as u32).min(SAMPLES_PER_ENVELOPE_STEP - 1);
        self.eg_counter = s.eg_counter;
        self.busy_end_cycle = s.busy_end_cycle;
        self.current_cpu_cycle = s.current_cpu_cycle;
        self.timers.from_savestate(&s.timer);
        self.restart_output();
        self.align_stems();
    }

    /// What the chip holds that the layout has no field for: the busy time
    /// left after a write (its status reads busy until it runs out). The
    /// samples an older build made at the host's rate are written empty:
    /// the chip's record of a frame in progress is the extension's own
    /// field ([`Self::frame_output`]).
    pub fn extension(&self) -> Ym2612Extension {
        Ym2612Extension {
            busy: self.busy_counter,
            samples: Vec::new(),
        }
    }

    /// Make the busy time `extension`'s, after [`Self::from_savestate`].
    /// Samples an older build made at the host's rate are not read.
    pub fn restore_extension(&mut self, extension: Ym2612Extension) {
        self.busy_counter = extension.busy;
    }
}
