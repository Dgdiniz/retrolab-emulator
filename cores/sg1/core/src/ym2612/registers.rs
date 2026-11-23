//! The register file: what a write to each register does, the channel
//! and the operator a register names, and the registers as the chip's
//! state gives them back.

use super::{StereoFlags, Ym2612};

/// The operators whose key-on bits 4 to 7 of register 0x28 are, in this
/// core's order: slots 1, 2, 3 and 4 are operators 0, 2, 1 and 3.
const KEY_BIT_OPERATORS: [usize; 4] = [0, 2, 1, 3];

/// Channel 3's own frequency registers (0xA8 to 0xAA, and 0xAC to 0xAE for
/// the block and high bits) and the operator each sets, in this core's
/// order: slots 3, 1 and 2.
const CH3_FREQUENCY_OPERATORS: [usize; 3] = [2, 0, 1];

impl Ym2612 {
    /// Write to a YM2612 register
    ///
    /// YM2612 has two register banks (part 0 and part 1), selected via address 0/2.
    /// Data is written via address 1/3.
    ///
    /// # Arguments
    /// * `part` - Register bank (0 or 1)
    /// * `register` - Register address (0x00-0xFF)
    /// * `value` - Value to write (8-bit)
    pub fn write_register(&mut self, part: u8, register: u8, value: u8) {
        match register {
            // Global registers (part 0 only)
            0x22 => {
                // LFO control
                self.lfo.set_control(value);
            }

            0x24 => {
                // Timer A period, upper eight of its ten bits
                self.timers.set_timer_a_high(value);
            }

            0x25 => {
                // Timer A period, lower two bits
                self.timers.set_timer_a_low(value);
            }

            0x26 => {
                // Timer B: TBL = (256 - value) << 4
                self.timers.set_timer_b_period(value);
            }

            0x27 => {
                // CH3 mode and timer control
                // Bits 7-6: CH3 mode (0x00=normal, 0x40=CH3 special, 0x80=CSM, 0xC0=CSM+CH3)
                // Bits 5-0: Timer control
                self.ch3_mode = value & 0xC0; // Extract bits 7-6
                                              // Update channel 2's special mode flag for per-operator LFO PM
                self.channels[2].ch3_special = (self.ch3_mode & 0x40) != 0;
                self.timers.set_control(value);
            }

            0x28 => {
                // Key On/Off (channel decoding checked against Genesis Plus GX)
                // Channel encoding: bits 0-1 = channel within bank (0-2, value 3 is INVALID)
                //                   bit 2 = bank select (0 = ch0-2, 1 = ch3-5)
                // Operator bits: bit 4=SLOT1(op0), bit 5=SLOT2(op2), bit 6=SLOT3(op1),
                // bit 7=SLOT4(op3)
                // Note: bits 5/6 are swapped due to YM2612's non-standard slot ordering
                let ch_low = (value & 0x03) as usize;
                if ch_low == 3 {
                    // Channel value 3 names no channel: the write is ignored
                    // Do nothing
                } else {
                    let channel_idx = if (value & 0x04) != 0 {
                        ch_low + 3
                    } else {
                        ch_low
                    };
                    if channel_idx < 6 {
                        // Key-on bits 4-7 name the chip's slots 1-4, and
                        // slots 2 and 3 are operators 3 and 2 in our order.
                        let op_bits = (value >> 4) & 0x0F;
                        for (i, &op_idx) in KEY_BIT_OPERATORS.iter().enumerate() {
                            if (op_bits & (1 << i)) != 0 {
                                self.channels[channel_idx].operator_mut(op_idx).key_on();
                            } else {
                                self.channels[channel_idx].operator_mut(op_idx).key_off();
                            }
                        }
                    }
                }
            }

            0x2A => {
                // DAC data. `write_port` already rendered the audio up to this
                // write via `catch_up()`, so updating the DAC value live is
                // sample-accurate for the remainder of the frame.
                self.dac_value = (value as i16) - 128;
            }

            0x2B => {
                // DAC enable
                self.dac_enabled = (value & 0x80) != 0;
            }

            // Per-channel/operator registers
            0x30..=0xB6 => {
                let channel_idx = self.get_channel_index(part, register);

                if channel_idx < 6 {
                    self.write_channel_register(channel_idx, register, value);
                }
            }

            _ => {
                // Unimplemented or invalid register
            }
        }
    }

    /// The registers as the chip's state gives them back, each a register
    /// (part 1's in the high byte) and a value, in an order a chip written
    /// with them in turn takes back the same state: the LFO, the timers'
    /// periods, channel 3's mode with the timers' run and flag bits, the
    /// DAC's switch and value, each channel's operators in register-slot
    /// order (detune and multiple, total level, rate scaling and attack, AM
    /// and decay, sustain rate, sustain level and release, SSG-EG), the
    /// frequencies (block and high bits before the low byte), channel 3's
    /// own frequencies, feedback and algorithm, stereo and sensitivities,
    /// and each channel's keyed operators last. Registers cannot hold an
    /// envelope's level: a note keyed on starts again from its attack.
    pub fn register_image(&self) -> Vec<(u16, u8)> {
        let mut image = Vec::with_capacity(256);
        let mut put = |part: u16, register: u8, value: u8| {
            image.push((part << 8 | u16::from(register), value));
        };
        let timer_a = self.timers.get_timer_a_period();
        put(
            0,
            0x22,
            u8::from(self.lfo.is_enabled()) << 3 | self.lfo.get_frequency(),
        );
        put(0, 0x24, (timer_a >> 2) as u8);
        put(0, 0x25, (timer_a & 0x03) as u8);
        put(0, 0x26, self.timers.get_timer_b_period());
        put(0, 0x27, self.ch3_mode | self.timers.control() & 0x0F);
        put(0, 0x2B, u8::from(self.dac_enabled) << 7);
        put(0, 0x2A, (self.dac_value + 128) as u8);
        let places = |channel: usize| ((channel / 3) as u16, (channel % 3) as u8);
        for (channel, state) in self.channels.iter().enumerate() {
            let (part, low) = places(channel);
            for slot in 0..4u8 {
                let operator = state.operator(usize::from(slot));
                let envelope = operator.envelope();
                let register = low + slot * 4;
                put(
                    part,
                    0x30 + register,
                    operator.detune << 4 | operator.multiple,
                );
                put(part, 0x40 + register, operator.total_level);
                put(
                    part,
                    0x50 + register,
                    operator.rate_scaling << 6 | envelope.attack_rate,
                );
                put(
                    part,
                    0x60 + register,
                    u8::from(operator.am_enable) << 7 | envelope.decay_rate,
                );
                put(part, 0x70 + register, envelope.sustain_rate);
                put(
                    part,
                    0x80 + register,
                    envelope.sustain_level << 4 | envelope.release_rate,
                );
                put(part, 0x90 + register, envelope.get_ssg_eg());
            }
        }
        for (channel, state) in self.channels.iter().enumerate() {
            let (part, low) = places(channel);
            let (number, block) = if channel == 2 && self.ch3_mode & 0x40 != 0 {
                (self.ch3_fnum[3], self.ch3_block[3])
            } else {
                (state.operator(0).freq_number, state.operator(0).block)
            };
            put(part, 0xA4 + low, block << 3 | (number >> 8) as u8 & 0x07);
            put(part, 0xA0 + low, number as u8);
        }
        for (low, &operator) in CH3_FREQUENCY_OPERATORS.iter().enumerate() {
            let (number, block) = (self.ch3_fnum[operator], self.ch3_block[operator]);
            put(0, 0xAC + low as u8, block << 3 | (number >> 8) as u8 & 0x07);
            put(0, 0xA8 + low as u8, number as u8);
        }
        for (channel, state) in self.channels.iter().enumerate() {
            let (part, low) = places(channel);
            put(
                part,
                0xB0 + low,
                state.get_feedback() << 3 | state.get_algorithm().as_u8(),
            );
            let sensitivities = state.operator(0).get_ams() << 4 | state.get_pms();
            put(
                part,
                0xB4 + low,
                state.get_stereo().to_register() | sensitivities,
            );
        }
        for (channel, state) in self.channels.iter().enumerate() {
            let keyed = KEY_BIT_OPERATORS
                .iter()
                .enumerate()
                .filter(|(_, &operator)| state.operator(operator).is_key_on())
                .fold(0u8, |bits, (bit, _)| bits | 0x10 << bit);
            let (part, low) = places(channel);
            put(0, 0x28, keyed | (part as u8) << 2 | low);
        }
        image
    }

    /// Get channel index from register address and part
    fn get_channel_index(&self, part: u8, register: u8) -> usize {
        let channel_bits = register & 0x03;
        let base_channel = channel_bits as usize;

        // Part 1 addresses channels 3-5
        if part == 1 {
            base_channel + 3
        } else {
            base_channel
        }
    }

    /// Get operator index from register address
    fn get_operator_index(&self, register: u8) -> usize {
        // Operators are arranged in a specific pattern in registers
        // 0x30-0x3F: OP1, 0x40-0x4F: OP2, 0x50-0x5F: OP3, 0x60-0x6F: OP4 (for DT/MUL)
        // But not all register groups follow this pattern
        match register & 0xF0 {
            0x30 => ((register >> 2) & 0x03) as usize, // Detune/Multiple
            0x40 => ((register >> 2) & 0x03) as usize, // Total Level
            0x50 => ((register >> 2) & 0x03) as usize, // Rate Scaling/Attack Rate
            0x60 => ((register >> 2) & 0x03) as usize, // Amplitude Mod/Decay Rate
            0x70 => ((register >> 2) & 0x03) as usize, // Sustain Rate
            0x80 => ((register >> 2) & 0x03) as usize, // Sustain Level/Release Rate
            0x90 => ((register >> 2) & 0x03) as usize, // SSG-EG
            _ => 0,
        }
    }

    /// Write to a per-channel/operator register
    fn write_channel_register(&mut self, channel_idx: usize, register: u8, value: u8) {
        // Match against full register for frequency/algorithm registers,
        // and against register & 0xF0 for per-operator registers
        if matches!(register, 0xA0..=0xA2 | 0xA4..=0xA6 | 0xB0..=0xB2 | 0xB4..=0xB6) {
            // Per-channel registers (frequency, algorithm, stereo)
            self.write_perchannel_register(channel_idx, register, value);
            return;
        }

        // Get operator index for per-operator registers
        let operator_idx = self.get_operator_index(register);

        match register & 0xF0 {
            0x30 => {
                // Detune (DT) and Multiple (MUL) - per operator
                let detune = (value >> 4) & 0x07;
                let multiple = value & 0x0F;

                self.channels[channel_idx].operator_mut(operator_idx).detune = detune;
                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .multiple = multiple;
                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .update_phase_increment();
            }

            0x40 => {
                // Total Level (TL) - per operator
                let total_level = value & 0x7F;
                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .total_level = total_level;
            }

            0x50 => {
                // Rate Scaling (KS) and Attack Rate (AR) - per operator
                // Bits 7-6: Rate Scaling (KS, 0-3)
                // Bits 4-0: Attack Rate (AR, 0-31)
                let rate_scaling = (value >> 6) & 0x03;
                let attack_rate = value & 0x1F;

                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .set_rate_scaling(rate_scaling);
                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .envelope_mut()
                    .attack_rate = attack_rate;
            }

            0x60 => {
                // AM Enable and First Decay Rate (D1R) - per operator
                // Bit 7: AM Enable (1 = this operator responds to LFO AM)
                // Bits 6-5: Unused
                // Bits 4-0: D1R (First Decay Rate)
                let am_enable = (value & 0x80) != 0;
                let decay_rate = value & 0x1F;

                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .set_am_enable(am_enable);
                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .envelope_mut()
                    .decay_rate = decay_rate;
            }

            0x70 => {
                // Secondary Decay Rate (D2R / SR) - per operator
                let sustain_rate = value & 0x1F;
                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .envelope_mut()
                    .sustain_rate = sustain_rate;
            }

            0x80 => {
                // Sustain Level (SL) and Release Rate (RR) - per operator
                let sustain_level = (value >> 4) & 0x0F;
                let release_rate = value & 0x0F;

                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .envelope_mut()
                    .sustain_level = sustain_level;
                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .envelope_mut()
                    .release_rate = release_rate;
            }

            0x90 => {
                // SSG-EG (the envelope modes of the SSG, the OPN's Software-controlled
                // Sound Generator) - per operator
                // Bit 3: Enable SSG-EG
                // Bit 2: Attack
                // Bit 1: Alternate
                // Bit 0: Hold
                self.channels[channel_idx]
                    .operator_mut(operator_idx)
                    .envelope_mut()
                    .set_ssg_eg(value);
            }

            _ => {}
        }
    }

    /// Write to per-channel register (frequency, algorithm, stereo)
    fn write_perchannel_register(&mut self, channel_idx: usize, register: u8, value: u8) {
        match register {
            // CH3 Special Mode registers (part 0 only)
            // The slots are Plutiedev's and Nuked OPN2's; the indices are what this
            // code writes. In this core's order slot 3 is index 1 and slot 2 is
            // index 2, so $A8/$AC and $AA/$AE drive each other's slot:
            //   $A8/$AC -> Slot 3 (S3) -> operator index 2 (C1)
            //   $A9/$AD -> Slot 1 (S1) -> operator index 0 (M1)
            //   $AA/$AE -> Slot 2 (S2) -> operator index 1 (M2)
            //   $A2/$A6 -> Slot 4 (S4) -> operator index 3 (C2) [normal channel reg]
            0xA8 => {
                // CH3 S3 (OP2) F-Number (low 8 bits)
                let f_num_low = value as u16;
                self.ch3_fnum[2] = (self.ch3_fnum[2] & 0x700) | f_num_low;
            }
            0xA9 => {
                // CH3 S1 (OP0) F-Number (low 8 bits)
                let f_num_low = value as u16;
                self.ch3_fnum[0] = (self.ch3_fnum[0] & 0x700) | f_num_low;
            }
            0xAA => {
                // CH3 S2 (OP1) F-Number (low 8 bits)
                let f_num_low = value as u16;
                self.ch3_fnum[1] = (self.ch3_fnum[1] & 0x700) | f_num_low;
            }
            0xAC => {
                // CH3 S3 (OP2) F-Number (high 3 bits) and Block
                let block = (value >> 3) & 0x07;
                let f_num_high = (value & 0x07) as u16;
                self.ch3_fnum[2] = (self.ch3_fnum[2] & 0x0FF) | (f_num_high << 8);
                self.ch3_block[2] = block;
                // Apply to CH3 OP2 if special mode is active
                if (self.ch3_mode & 0x40) != 0 {
                    self.channels[2].operator_mut(2).freq_number = self.ch3_fnum[2];
                    self.channels[2].operator_mut(2).block = self.ch3_block[2];
                    self.channels[2].operator_mut(2).update_phase_increment();
                    self.channels[2].operator_mut(2).update_key_code();
                    // Sync per-operator freq for LFO PM
                    self.channels[2].op_fnum[2] = self.ch3_fnum[2];
                    self.channels[2].op_block[2] = self.ch3_block[2];
                }
            }
            0xAD => {
                // CH3 S1 (OP0) F-Number (high 3 bits) and Block
                let block = (value >> 3) & 0x07;
                let f_num_high = (value & 0x07) as u16;
                self.ch3_fnum[0] = (self.ch3_fnum[0] & 0x0FF) | (f_num_high << 8);
                self.ch3_block[0] = block;
                // Apply to CH3 OP0 if special mode is active
                if (self.ch3_mode & 0x40) != 0 {
                    self.channels[2].operator_mut(0).freq_number = self.ch3_fnum[0];
                    self.channels[2].operator_mut(0).block = self.ch3_block[0];
                    self.channels[2].operator_mut(0).update_phase_increment();
                    self.channels[2].operator_mut(0).update_key_code();
                    // Sync per-operator freq for LFO PM
                    self.channels[2].op_fnum[0] = self.ch3_fnum[0];
                    self.channels[2].op_block[0] = self.ch3_block[0];
                }
            }
            0xAE => {
                // CH3 S2 (OP1) F-Number (high 3 bits) and Block
                let block = (value >> 3) & 0x07;
                let f_num_high = (value & 0x07) as u16;
                self.ch3_fnum[1] = (self.ch3_fnum[1] & 0x0FF) | (f_num_high << 8);
                self.ch3_block[1] = block;
                // Apply to CH3 OP1 if special mode is active
                if (self.ch3_mode & 0x40) != 0 {
                    self.channels[2].operator_mut(1).freq_number = self.ch3_fnum[1];
                    self.channels[2].operator_mut(1).block = self.ch3_block[1];
                    self.channels[2].operator_mut(1).update_phase_increment();
                    self.channels[2].operator_mut(1).update_key_code();
                    // Sync per-operator freq for LFO PM
                    self.channels[2].op_fnum[1] = self.ch3_fnum[1];
                    self.channels[2].op_block[1] = self.ch3_block[1];
                }
            }

            0xA0..=0xA2 => {
                // F-Number (low 8 bits) - per channel
                let f_num_low = value as u16;

                // Update all operators in the channel (or CH3 OP3 if CH3 special mode)
                if channel_idx == 2 && (self.ch3_mode & 0x40) != 0 {
                    // CH3 special mode: only update OP3 (OP0-2 use independent frequencies)
                    let current_f_num = self.ch3_fnum[3];
                    let new_f_num = (current_f_num & 0x700) | f_num_low;
                    self.ch3_fnum[3] = new_f_num;
                    // Also update the channel's operator
                    self.channels[channel_idx].operator_mut(3).freq_number = new_f_num;
                    self.channels[channel_idx]
                        .operator_mut(3)
                        .update_phase_increment();
                    self.channels[channel_idx].operator_mut(3).update_key_code();
                    // Sync per-operator freq for LFO PM
                    self.channels[2].op_fnum[3] = new_f_num;
                } else {
                    // Normal mode: update all operators
                    let current_f_num = self.channels[channel_idx].operator(0).freq_number;
                    let new_f_num = (current_f_num & 0x700) | f_num_low;
                    // Update channel base frequency for LFO PM calculation
                    let block = self.channels[channel_idx].operator(0).block;
                    self.channels[channel_idx].set_base_frequency(new_f_num, block);
                    for i in 0..4 {
                        self.channels[channel_idx].operator_mut(i).freq_number = new_f_num;
                        self.channels[channel_idx]
                            .operator_mut(i)
                            .update_phase_increment();
                        self.channels[channel_idx].operator_mut(i).update_key_code();
                    }
                }
            }

            0xA4..=0xA6 => {
                // F-Number (high 3 bits) and Block - per channel
                let block = (value >> 3) & 0x07;
                let f_num_high = (value & 0x07) as u16;

                // Update all operators in the channel (or CH3 OP3 if CH3 special mode)
                if channel_idx == 2 && (self.ch3_mode & 0x40) != 0 {
                    // CH3 special mode: only update OP3 (OP0-2 use independent frequencies)
                    let current_f_num = self.ch3_fnum[3];
                    let new_f_num = (current_f_num & 0xFF) | (f_num_high << 8);
                    self.ch3_fnum[3] = new_f_num;
                    self.ch3_block[3] = block;
                    // Also update the channel's operator
                    self.channels[channel_idx].operator_mut(3).freq_number = new_f_num;
                    self.channels[channel_idx].operator_mut(3).block = block;
                    self.channels[channel_idx]
                        .operator_mut(3)
                        .update_phase_increment();
                    self.channels[channel_idx].operator_mut(3).update_key_code();
                    // Sync per-operator freq for LFO PM
                    self.channels[2].op_fnum[3] = new_f_num;
                    self.channels[2].op_block[3] = block;
                } else {
                    // Normal mode: update all operators
                    let current_f_num = self.channels[channel_idx].operator(0).freq_number;
                    let new_f_num = (current_f_num & 0xFF) | (f_num_high << 8);
                    // Update channel base frequency for LFO PM calculation
                    self.channels[channel_idx].set_base_frequency(new_f_num, block);
                    for i in 0..4 {
                        self.channels[channel_idx].operator_mut(i).freq_number = new_f_num;
                        self.channels[channel_idx].operator_mut(i).block = block;
                        self.channels[channel_idx]
                            .operator_mut(i)
                            .update_phase_increment();
                        self.channels[channel_idx].operator_mut(i).update_key_code();
                    }
                }
            }

            0xB0..=0xB2 => {
                // Feedback (FB) and Algorithm (ALG)
                let feedback = (value >> 3) & 0x07;
                let algorithm = value & 0x07;

                self.channels[channel_idx].set_feedback(feedback);
                self.channels[channel_idx].set_algorithm(algorithm);
            }

            0xB4..=0xB6 => {
                // Stereo output (L/R), AMS and PMS - per channel
                // Bit 7: L (Left) output enable
                // Bit 6: R (Right) output enable
                // Bits 5-4: AMS (Amplitude Modulation Sensitivity, 0-3) - PER CHANNEL
                // Bits 2-0: PMS (Pitch Modulation Sensitivity, 0-7)
                let stereo = StereoFlags::from_register(value);
                let ams = (value >> 4) & 0x03; // Bits 5-4: AMS per channel
                let pms = value & 0x07; // Bits 2-0: PMS per channel

                self.channels[channel_idx].set_stereo(stereo);
                self.channels[channel_idx].set_pms(pms);

                // AMS is per-channel: set on all 4 operators
                // (AM enable is per-operator, set via register 0x60 bit 7)
                for i in 0..4 {
                    self.channels[channel_idx].operator_mut(i).set_ams(ams);
                }
            }

            _ => {}
        }
    }
}
