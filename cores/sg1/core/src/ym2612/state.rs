//! The YM2612 into and out of the savestate's layout, and its busy time
//! and the frame's samples into and out of the extension after it.

use super::Ym2612;
use crate::savestate::{Ym2612Extension, Ym2612State};

impl Ym2612 {
    pub fn to_savestate(&self) -> Ym2612State {
        let (lfo_enabled, lfo_freq, lfo_phase, lfo_counter) = self.lfo.to_savestate();
        Ym2612State {
            channels: [
                self.channels[0].to_savestate(),
                self.channels[1].to_savestate(),
                self.channels[2].to_savestate(),
                self.channels[3].to_savestate(),
                self.channels[4].to_savestate(),
                self.channels[5].to_savestate(),
            ],
            lfo_enabled,
            lfo_freq,
            lfo_phase,
            lfo_counter,
            dac_enabled: self.dac_enabled,
            dac_value: self.dac_value,
            dac_start_value: self.dac_start_value,
            clock_accumulator: self.clock_accumulator,
            timer_accumulator: self.timer_accumulator,
            ch3_mode: self.ch3_mode,
            ch3_fnum: self.ch3_fnum,
            ch3_block: self.ch3_block,
            csm_key_state: self.csm_key_state,
            address_latch_part0: self.address_latch_part0,
            address_latch_part1: self.address_latch_part1,
            eg_accumulator: self.eg_accumulator,
            eg_counter: self.eg_counter,
            busy_end_cycle: self.busy_end_cycle,
            current_cpu_cycle: self.current_cpu_cycle,
            timer: self.timers.to_savestate(),
        }
    }

    pub fn from_savestate(&mut self, s: &Ym2612State) {
        for i in 0..6 {
            self.channels[i].from_savestate(&s.channels[i]);
        }
        self.lfo
            .from_savestate(s.lfo_enabled, s.lfo_freq, s.lfo_phase, s.lfo_counter);
        self.dac_enabled = s.dac_enabled;
        self.dac_value = s.dac_value;
        self.dac_start_value = s.dac_start_value;
        self.clock_accumulator = s.clock_accumulator;
        self.timer_accumulator = s.timer_accumulator;
        self.ch3_mode = s.ch3_mode;
        self.ch3_fnum = s.ch3_fnum;
        self.ch3_block = s.ch3_block;
        self.csm_key_state = s.csm_key_state;
        self.address_latch_part0 = s.address_latch_part0;
        self.address_latch_part1 = s.address_latch_part1;
        self.eg_accumulator = s.eg_accumulator;
        self.eg_counter = s.eg_counter;
        self.busy_end_cycle = s.busy_end_cycle;
        self.current_cpu_cycle = s.current_cpu_cycle;
        self.timers.from_savestate(&s.timer);
        self.frame_samples.clear();
    }

    /// What the chip holds that the layout has no field for: the busy time
    /// left after a write (its status reads busy until it runs out), and
    /// the samples made so far in the frame (the frame's end takes them,
    /// and makes up a short count from the chip, which moves it).
    pub fn extension(&self) -> Ym2612Extension {
        Ym2612Extension {
            busy: self.busy_counter,
            samples: self.frame_samples.clone(),
        }
    }

    /// Make the busy time and the frame's samples `extension`'s, after
    /// [`Self::from_savestate`]. The samples are copied into the buffer the
    /// chip has, which keeps its capacity across a load.
    pub fn restore_extension(&mut self, extension: Ym2612Extension) {
        self.busy_counter = extension.busy;
        self.frame_samples.clear();
        self.frame_samples.extend_from_slice(&extension.samples);
    }
}
