// YM2612 FM Channel - Contains 4 Operators + Algorithm
//
// Each channel has:
// - 4 operators (configurable via registers)
// - 1 algorithm selection (0-7)
// - Feedback for operator 1 (0-7)
// - Stereo output (L/R enable flags)
// - Per-operator frequency and envelope settings

use super::algorithms::Algorithm;
use super::operator::Operator;
use super::tables::compute_lfo_pm_offset;

/// Stereo output control
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StereoFlags {
    pub left: bool,
    pub right: bool,
}

impl StereoFlags {
    pub fn new() -> Self {
        // Default to both channels enabled for compatibility.
        // While real hardware initializes registers to 0 (L=0, R=0),
        // many games (e.g. Sonic 1's SE-GA jingle) play DAC audio before
        // writing to $B4, expecting sound to be audible from the start.
        Self {
            left: true,
            right: true,
        }
    }

    pub fn both() -> Self {
        Self {
            left: true,
            right: true,
        }
    }

    pub fn left_only() -> Self {
        Self {
            left: true,
            right: false,
        }
    }

    pub fn right_only() -> Self {
        Self {
            left: false,
            right: true,
        }
    }

    pub fn muted() -> Self {
        Self {
            left: false,
            right: false,
        }
    }

    /// Parse from 2-bit register value (bit 7=L, bit 6=R)
    pub fn from_register(value: u8) -> Self {
        Self {
            left: (value & 0x80) != 0,
            right: (value & 0x40) != 0,
        }
    }

    /// Convert to register value (bit 7=L, bit 6=R)
    pub fn to_register(self) -> u8 {
        let mut value = 0;
        if self.left {
            value |= 0x80;
        }
        if self.right {
            value |= 0x40;
        }
        value
    }
}

impl Default for StereoFlags {
    fn default() -> Self {
        Self::new()
    }
}

/// YM2612 FM Channel
pub struct Channel {
    /// 4 operators, in register order: OP1, OP3, OP2, OP4 (see `clock_with_lfo_eg`)
    operators: [Operator; 4],

    /// Algorithm (0-7)
    algorithm: Algorithm,

    /// Feedback for OP1 (0-7)
    /// 0 = no feedback, 7 = maximum feedback
    feedback: u8,

    /// Last two OP1 outputs for feedback calculation
    /// Real YM2612 uses average of last two outputs: (out[-1] + out[-2]) / 2
    op1_prev: [i16; 2],

    /// Stereo output flags (L/R)
    stereo: StereoFlags,

    /// Pitch Modulation Sensitivity (0-7) - How much LFO affects pitch
    /// 0 = no modulation, 7 = maximum (+/-80 cents)
    pms: u8,

    /// Channel base F-number (11-bit) - used for LFO PM calculation
    /// On real hardware, PM modifies the F-number which changes phase increment
    fnum: u16,

    /// Channel base block/octave (3-bit) - used for LFO PM calculation
    block: u8,

    /// CH3 special mode flag. When true, each operator uses its own
    /// fnum/block for LFO PM instead of the channel-wide values.
    pub ch3_special: bool,

    /// Per-operator F-number for CH3 special mode LFO PM.
    /// Only used when `ch3_special` is true.
    pub op_fnum: [u16; 4],

    /// Per-operator block for CH3 special mode LFO PM.
    pub op_block: [u8; 4],

    /// Chip clock frequency
    _clock_freq: u32,
}

impl Channel {
    /// Create a new channel
    pub fn new(clock_freq: u32) -> Self {
        Self::with_sample_rate(clock_freq, 53267) // Default to internal rate
    }

    /// Create a new channel with explicit sample rate for phase scaling
    pub fn with_sample_rate(clock_freq: u32, sample_rate: u32) -> Self {
        Self {
            operators: [
                Operator::with_sample_rate(clock_freq, sample_rate),
                Operator::with_sample_rate(clock_freq, sample_rate),
                Operator::with_sample_rate(clock_freq, sample_rate),
                Operator::with_sample_rate(clock_freq, sample_rate),
            ],
            algorithm: Algorithm::Algorithm0,
            feedback: 0,
            op1_prev: [0; 2],
            stereo: StereoFlags::both(),
            pms: 0,
            fnum: 0,
            block: 0,
            ch3_special: false,
            op_fnum: [0; 4],
            op_block: [0; 4],
            _clock_freq: clock_freq,
        }
    }

    /// Update sample rate for all operators
    pub fn set_sample_rate(&mut self, sample_rate: u32) {
        for op in &mut self.operators {
            op.set_sample_rate(sample_rate);
        }
    }

    /// Set channel base frequency (used for LFO PM calculation)
    /// Should be called whenever frequency registers are written
    pub fn set_base_frequency(&mut self, fnum: u16, block: u8) {
        self.fnum = fnum;
        self.block = block;
    }

    /// Get channel base F-number
    pub fn get_fnum(&self) -> u16 {
        self.fnum
    }

    /// Get channel base block
    pub fn get_block(&self) -> u8 {
        self.block
    }

    /// Get mutable reference to an operator (0-3)
    pub fn operator_mut(&mut self, index: usize) -> &mut Operator {
        &mut self.operators[index]
    }

    /// Get reference to an operator (0-3)
    pub fn operator(&self, index: usize) -> &Operator {
        &self.operators[index]
    }

    /// Set algorithm (0-7)
    pub fn set_algorithm(&mut self, algorithm: u8) {
        self.algorithm = Algorithm::from_u8(algorithm);
    }

    /// Get current algorithm
    pub fn get_algorithm(&self) -> Algorithm {
        self.algorithm
    }

    /// Set feedback (0-7)
    pub fn set_feedback(&mut self, feedback: u8) {
        self.feedback = feedback & 0x07;
    }

    /// Get current feedback
    pub fn get_feedback(&self) -> u8 {
        self.feedback
    }

    /// Set stereo flags
    pub fn set_stereo(&mut self, stereo: StereoFlags) {
        self.stereo = stereo;
    }

    /// Get stereo flags
    pub fn get_stereo(&self) -> StereoFlags {
        self.stereo
    }

    /// Check if left channel is enabled
    pub fn is_left_enabled(&self) -> bool {
        self.stereo.left
    }

    /// Check if right channel is enabled
    pub fn is_right_enabled(&self) -> bool {
        self.stereo.right
    }

    /// Set Pitch Modulation Sensitivity (0-7)
    /// Controls how much LFO affects the pitch of this channel
    /// 0 = no modulation, 7 = maximum (+/-80 cents)
    pub fn set_pms(&mut self, pms: u8) {
        self.pms = pms & 0x07;
    }

    /// Get Pitch Modulation Sensitivity
    pub fn get_pms(&self) -> u8 {
        self.pms
    }

    /// Clock the channel once with no LFO: `clock_with_lfo(0, 0)`.
    pub fn clock(&mut self) -> (i16, i16) {
        self.clock_with_lfo(0, 0)
    }

    /// Clock the channel once, generating one sample with LFO modulation
    pub fn clock_with_lfo(&mut self, pm_step: u8, am_value: u8) -> (i16, i16) {
        self.clock_with_lfo_eg(pm_step, am_value, true)
    }

    /// Clock the channel once, generating one sample with LFO modulation
    /// and optional envelope generator advancement.
    ///
    /// In real YM2612 hardware, the EG runs at clock/144/3 ~ 17,756 Hz
    /// (every 3 internal samples), while phase/output run at the full
    /// internal rate. When `advance_eg` is false, the phase still advances
    /// and output is computed, but the envelope level is frozen.
    ///
    /// # Arguments
    /// * `pm_step` - LFO PM step (0-31: the LFO's step divided by four)
    /// * `am_value` - LFO amplitude modulation value (0-126, triangle wave)
    /// * `advance_eg` - Whether to advance the envelope generator this cycle
    ///
    /// # Returns
    /// Tuple of (left_sample, right_sample)
    pub fn clock_with_lfo_eg(&mut self, pm_step: u8, am_value: u8, advance_eg: bool) -> (i16, i16) {
        // Calculate feedback modulation for OP1
        // Real YM2612 uses average of last two OP1 outputs
        let fb_mod = if self.feedback > 0 {
            let avg = ((self.op1_prev[0] as i32) + (self.op1_prev[1] as i32)) / 2;
            (avg >> (9 - self.feedback as i32)) as i16
        } else {
            0
        };

        // Calculate PM (pitch modulation) as a phase increment adjustment
        // Hardware-accurate: uses LFO_PM_OUTPUT indexed by F-number bits, PMS, and LFO step
        // The table gives an F-number offset, which we convert to a phase increment delta
        //
        // In CH3 special mode, each operator uses its own fnum/block for PM calculation.
        let pm_phase_adj: [i32; 4] = if self.pms > 0 {
            if self.ch3_special {
                // CH3 special: per-operator PM using each operator's own frequency
                [
                    Self::calc_pm(self.op_fnum[0], self.op_block[0], self.pms, pm_step),
                    Self::calc_pm(self.op_fnum[1], self.op_block[1], self.pms, pm_step),
                    Self::calc_pm(self.op_fnum[2], self.op_block[2], self.pms, pm_step),
                    Self::calc_pm(self.op_fnum[3], self.op_block[3], self.pms, pm_step),
                ]
            } else {
                // Normal mode: all operators share channel-wide fnum/block
                let adj = Self::calc_pm(self.fnum, self.block, self.pms, pm_step);
                [adj, adj, adj, adj]
            }
        } else {
            [0, 0, 0, 0]
        };

        // Calculate per-operator AM attenuation
        // AM is only applied to operators with am_enable flag set (register 0x60 bit 7)
        // AMS sensitivity (register 0xB4 bits 5-4) controls depth per channel:
        // AMS 0: 0 (off)
        // AMS 1: max ~15 (~1.4 dB)
        // AMS 2: max ~63 (~5.9 dB)
        // AMS 3: max ~126 (~11.8 dB)
        // An operator's attenuation is its envelope output plus the LFO's
        // tremolo, and the tremolo only counts on an operator with AM on.
        let am_attenuation: [u16; 4] = [
            if self.operators[0].am_enable {
                Self::calc_am_attenuation(am_value, self.operators[0].ams)
            } else {
                0
            },
            if self.operators[1].am_enable {
                Self::calc_am_attenuation(am_value, self.operators[1].ams)
            } else {
                0
            },
            if self.operators[2].am_enable {
                Self::calc_am_attenuation(am_value, self.operators[2].ams)
            } else {
                0
            },
            if self.operators[3].am_enable {
                Self::calc_am_attenuation(am_value, self.operators[3].ams)
            } else {
                0
            },
        ];

        // Feedback modulation for OP1 only (PM is applied via the phase increment)
        let fb_with_pm = fb_mod;

        // The chip's four slots and where each sits in `operators`:
        //   slot 1 = first modulator  (M1) = operators[0]
        //   slot 2 = first carrier    (C1) = operators[2]
        //   slot 3 = second modulator (M2) = operators[1]
        //   slot 4 = second carrier   (C2) = operators[3]
        //
        // Register offset 0->[0]=M1, offset 4->[1]=M2, offset 8->[2]=C1, offset 12->[3]=C2
        // Key-on: bit4->[0]=M1, bit5->[2]=C1, bit6->[1]=M2, bit7->[3]=C2
        //
        // Algorithm 0 flow: M1(0)->C1(2)->MEM->M2(1)->C2(3)->out
        const M1: usize = 0;
        const C1: usize = 2;
        const M2: usize = 1;
        const C2: usize = 3;

        // Clock operators per algorithm with correct routing
        // Outside CH3 special mode all operators receive the same pm_phase_adj
        // (LFO PM modifies all equally); in it each operator has its own.
        //
        // IMPORTANT: the two kinds of operator take their modulation differently:
        // - the feedback operator uses its modulation as it is;
        // - every other operator uses half of the modulation it is given.
        //
        // We apply >> 1 to all inter-operator modulation (non-feedback).
        // Feedback (fb_with_pm) is already correctly scaled and used directly by M1.
        // When 0 modulation is passed, >> 1 has no effect.
        //
        // The eight algorithms, as the chip's manual draws them and ymfm
        // lists them (M1=OP1, C1=OP2, M2=OP3, C2=OP4); checked against
        // Genesis Plus GX:
        // Per-operator PM for CH3 special mode support.
        let eg = advance_eg;
        let output = match self.algorithm {
            // Alg 0: M1 -> C1 -> M2 -> C2 -> out
            Algorithm::Algorithm0 => {
                let m1 = self.operators[M1].clock_full(
                    fb_with_pm,
                    am_attenuation[M1],
                    pm_phase_adj[M1],
                    eg,
                );
                let c1 = self.operators[C1].clock_full(
                    m1 >> 1,
                    am_attenuation[C1],
                    pm_phase_adj[C1],
                    eg,
                );
                let m2 = self.operators[M2].clock_full(
                    c1 >> 1,
                    am_attenuation[M2],
                    pm_phase_adj[M2],
                    eg,
                );
                let c2 = self.operators[C2].clock_full(
                    m2 >> 1,
                    am_attenuation[C2],
                    pm_phase_adj[C2],
                    eg,
                );
                self.op1_prev = [m1, self.op1_prev[0]];
                c2
            }
            // Alg 1: (M1 + C1) -> M2 -> C2 -> out
            Algorithm::Algorithm1 => {
                let m1 = self.operators[M1].clock_full(
                    fb_with_pm,
                    am_attenuation[M1],
                    pm_phase_adj[M1],
                    eg,
                );
                let c1 = self.operators[C1].clock_full(0, am_attenuation[C1], pm_phase_adj[C1], eg);
                let sum = (m1 as i32 + c1 as i32).clamp(-32768, 32767) as i16;
                let m2 = self.operators[M2].clock_full(
                    sum >> 1,
                    am_attenuation[M2],
                    pm_phase_adj[M2],
                    eg,
                );
                let c2 = self.operators[C2].clock_full(
                    m2 >> 1,
                    am_attenuation[C2],
                    pm_phase_adj[C2],
                    eg,
                );
                self.op1_prev = [m1, self.op1_prev[0]];
                c2
            }
            // Alg 2: M1 + (C1 -> M2) -> C2 -> out
            Algorithm::Algorithm2 => {
                let m1 = self.operators[M1].clock_full(
                    fb_with_pm,
                    am_attenuation[M1],
                    pm_phase_adj[M1],
                    eg,
                );
                let c1 = self.operators[C1].clock_full(0, am_attenuation[C1], pm_phase_adj[C1], eg);
                let m2 = self.operators[M2].clock_full(
                    c1 >> 1,
                    am_attenuation[M2],
                    pm_phase_adj[M2],
                    eg,
                );
                let sum = (m1 as i32 + m2 as i32).clamp(-32768, 32767) as i16;
                let c2 = self.operators[C2].clock_full(
                    sum >> 1,
                    am_attenuation[C2],
                    pm_phase_adj[C2],
                    eg,
                );
                self.op1_prev = [m1, self.op1_prev[0]];
                c2
            }
            // Alg 3: (M1 -> C1) + M2 -> C2 -> out
            Algorithm::Algorithm3 => {
                let m1 = self.operators[M1].clock_full(
                    fb_with_pm,
                    am_attenuation[M1],
                    pm_phase_adj[M1],
                    eg,
                );
                let c1 = self.operators[C1].clock_full(
                    m1 >> 1,
                    am_attenuation[C1],
                    pm_phase_adj[C1],
                    eg,
                );
                let m2 = self.operators[M2].clock_full(0, am_attenuation[M2], pm_phase_adj[M2], eg);
                let sum = (c1 as i32 + m2 as i32).clamp(-32768, 32767) as i16;
                let c2 = self.operators[C2].clock_full(
                    sum >> 1,
                    am_attenuation[C2],
                    pm_phase_adj[C2],
                    eg,
                );
                self.op1_prev = [m1, self.op1_prev[0]];
                c2
            }
            // Alg 4: (M1 -> C1) + (M2 -> C2) -> out
            Algorithm::Algorithm4 => {
                let m1 = self.operators[M1].clock_full(
                    fb_with_pm,
                    am_attenuation[M1],
                    pm_phase_adj[M1],
                    eg,
                );
                let c1 = self.operators[C1].clock_full(
                    m1 >> 1,
                    am_attenuation[C1],
                    pm_phase_adj[C1],
                    eg,
                );
                let m2 = self.operators[M2].clock_full(0, am_attenuation[M2], pm_phase_adj[M2], eg);
                let c2 = self.operators[C2].clock_full(
                    m2 >> 1,
                    am_attenuation[C2],
                    pm_phase_adj[C2],
                    eg,
                );
                self.op1_prev = [m1, self.op1_prev[0]];
                (c1 as i32 + c2 as i32).clamp(-32768, 32767) as i16
            }
            // Alg 5: M1 -> (C1 + M2 + C2) -> out
            Algorithm::Algorithm5 => {
                let m1 = self.operators[M1].clock_full(
                    fb_with_pm,
                    am_attenuation[M1],
                    pm_phase_adj[M1],
                    eg,
                );
                let m1_mod = m1 >> 1; // halved for non-feedback modulation
                let c1 =
                    self.operators[C1].clock_full(m1_mod, am_attenuation[C1], pm_phase_adj[C1], eg);
                let m2 =
                    self.operators[M2].clock_full(m1_mod, am_attenuation[M2], pm_phase_adj[M2], eg);
                let c2 =
                    self.operators[C2].clock_full(m1_mod, am_attenuation[C2], pm_phase_adj[C2], eg);
                self.op1_prev = [m1, self.op1_prev[0]];
                (c1 as i32 + m2 as i32 + c2 as i32).clamp(-32768, 32767) as i16
            }
            // Alg 6: (M1 -> C1) + M2 + C2 -> out
            Algorithm::Algorithm6 => {
                let m1 = self.operators[M1].clock_full(
                    fb_with_pm,
                    am_attenuation[M1],
                    pm_phase_adj[M1],
                    eg,
                );
                let c1 = self.operators[C1].clock_full(
                    m1 >> 1,
                    am_attenuation[C1],
                    pm_phase_adj[C1],
                    eg,
                );
                let m2 = self.operators[M2].clock_full(0, am_attenuation[M2], pm_phase_adj[M2], eg);
                let c2 = self.operators[C2].clock_full(0, am_attenuation[C2], pm_phase_adj[C2], eg);
                self.op1_prev = [m1, self.op1_prev[0]];
                (c1 as i32 + m2 as i32 + c2 as i32).clamp(-32768, 32767) as i16
            }
            // Alg 7: M1 + C1 + M2 + C2 -> out (all carriers, no modulation)
            Algorithm::Algorithm7 => {
                let m1 = self.operators[M1].clock_full(
                    fb_with_pm,
                    am_attenuation[M1],
                    pm_phase_adj[M1],
                    eg,
                );
                let c1 = self.operators[C1].clock_full(0, am_attenuation[C1], pm_phase_adj[C1], eg);
                let m2 = self.operators[M2].clock_full(0, am_attenuation[M2], pm_phase_adj[M2], eg);
                let c2 = self.operators[C2].clock_full(0, am_attenuation[C2], pm_phase_adj[C2], eg);
                self.op1_prev = [m1, self.op1_prev[0]];
                (m1 as i32 + c1 as i32 + m2 as i32 + c2 as i32).clamp(-32768, 32767) as i16
            }
        };

        // Per-channel 14-bit clamping (the chip's behaviour; checked against
        // Genesis Plus GX).
        // On real YM2612, each channel's DAC output is 14-bit, so it cannot
        // exceed -8192..8191 regardless of algorithm. Multi-carrier algorithms
        // (5, 6, 7) can sum 3-4 operators exceeding this range without clamping.
        let output = output.clamp(-8192, 8191);

        // Apply stereo panning
        let left = if self.stereo.left { output } else { 0 };
        let right = if self.stereo.right { output } else { 0 };

        (left, right)
    }

    /// Calculate PM phase adjustment for a given fnum/block/pms/lfo_step.
    fn calc_pm(fnum: u16, block: u8, pms: u8, pm_step: u8) -> i32 {
        let fn_offset = compute_lfo_pm_offset(fnum, pms, pm_step);
        if fn_offset != 0 {
            fn_offset << (block as i32)
        } else {
            0
        }
    }

    /// Calculate AM attenuation for an operator based on LFO AM value and operator AMS
    /// Calculate AM attenuation using hardware-accurate depth shifts
    /// The LFO's tremolo value shifted right by 8, 3, 1 or 0 for AMS 0-3.
    /// LFO_AM range is 0-126 (triangle wave)
    fn calc_am_attenuation(am_value: u8, ams: u8) -> u16 {
        // The chip's AMS depths, as shifts (checked against Genesis Plus GX)
        // AMS 0: shift 8 -> max 0 (effectively off)
        // AMS 1: shift 3 -> max 15 (~1.4 dB)
        // AMS 2: shift 1 -> max 63 (~5.9 dB)
        // AMS 3: shift 0 -> max 126 (~11.8 dB)
        const AMS_DEPTH_SHIFT: [u8; 4] = [8, 3, 1, 0];
        let shift = AMS_DEPTH_SHIFT[ams as usize & 3];
        (am_value as u16) >> shift
    }

    /// Trigger key on for all operators
    pub fn key_on_all(&mut self) {
        for op in &mut self.operators {
            op.key_on();
        }
    }

    /// Trigger key off for all operators
    pub fn key_off_all(&mut self) {
        for op in &mut self.operators {
            op.key_off();
        }
    }

    /// Trigger key on for specific operators (bitmask: bit i for `operators[i]`)
    pub fn key_on(&mut self, operators: u8) {
        for i in 0..4 {
            if (operators & (1 << i)) != 0 {
                self.operators[i].key_on();
            }
        }
    }

    /// Trigger key off for specific operators (bitmask: bit i for `operators[i]`)
    pub fn key_off(&mut self, operators: u8) {
        for i in 0..4 {
            if (operators & (1 << i)) != 0 {
                self.operators[i].key_off();
            }
        }
    }

    /// Reset channel state
    pub fn reset(&mut self) {
        for op in &mut self.operators {
            op.reset();
        }
        self.op1_prev = [0; 2];
        // Keep stereo flags as-is on reset for compatibility.
        // Games may not re-write $B4 after reset and expect sound to continue.
        self.ch3_special = false;
        self.op_fnum = [0; 4];
        self.op_block = [0; 4];
    }
}

impl Channel {
    pub fn to_savestate(&self) -> crate::system::savestate::Ym2612ChannelState {
        crate::system::savestate::Ym2612ChannelState {
            operators: [
                self.operators[0].to_savestate(),
                self.operators[1].to_savestate(),
                self.operators[2].to_savestate(),
                self.operators[3].to_savestate(),
            ],
            algorithm: self.algorithm as u8,
            feedback: self.feedback,
            op1_prev: self.op1_prev,
            stereo: self.stereo.to_register(),
            pms: self.pms,
            fnum: self.fnum,
            block: self.block,
            ch3_special: self.ch3_special,
            op_fnum: self.op_fnum,
            op_block: self.op_block,
        }
    }

    pub fn from_savestate(&mut self, s: &crate::system::savestate::Ym2612ChannelState) {
        for i in 0..4 {
            self.operators[i].from_savestate(&s.operators[i]);
        }
        self.algorithm = Algorithm::from_u8(s.algorithm);
        self.feedback = s.feedback;
        self.op1_prev = s.op1_prev;
        self.stereo = StereoFlags::from_register(s.stereo);
        self.pms = s.pms;
        self.fnum = s.fnum;
        self.block = s.block;
        self.ch3_special = s.ch3_special;
        self.op_fnum = s.op_fnum;
        self.op_block = s.op_block;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_creation() {
        let channel = Channel::new(7_670_454);

        assert_eq!(channel.algorithm, Algorithm::Algorithm0);
        assert_eq!(channel.feedback, 0);
        assert!(channel.stereo.left);
        assert!(channel.stereo.right);
    }

    #[test]
    fn test_operator_access() {
        let mut channel = Channel::new(7_670_454);

        // Set frequency on OP1
        channel.operator_mut(0).set_frequency(0x2CE, 4, 1);

        // Verify
        assert_eq!(channel.operator(0).freq_number, 0x2CE);
    }

    #[test]
    fn test_algorithm_setting() {
        let mut channel = Channel::new(7_670_454);

        channel.set_algorithm(7);
        assert_eq!(channel.get_algorithm(), Algorithm::Algorithm7);

        channel.set_algorithm(0);
        assert_eq!(channel.get_algorithm(), Algorithm::Algorithm0);
    }

    #[test]
    fn test_feedback_setting() {
        let mut channel = Channel::new(7_670_454);

        channel.set_feedback(7);
        assert_eq!(channel.get_feedback(), 7);

        channel.set_feedback(3);
        assert_eq!(channel.get_feedback(), 3);

        // Should wrap to 3 bits
        channel.set_feedback(15);
        assert_eq!(channel.get_feedback(), 7);
    }

    #[test]
    fn test_stereo_flags() {
        let both = StereoFlags::both();
        assert!(both.left && both.right);

        let left = StereoFlags::left_only();
        assert!(left.left && !left.right);

        let right = StereoFlags::right_only();
        assert!(!right.left && right.right);

        let muted = StereoFlags::muted();
        assert!(!muted.left && !muted.right);
    }

    #[test]
    fn test_stereo_register_conversion() {
        // Both enabled: 0xC0 (bit7=L, bit6=R)
        let both = StereoFlags::from_register(0xC0);
        assert!(both.left && both.right);
        assert_eq!(both.to_register(), 0xC0);

        // Left only: 0x80 (bit7=L set, bit6=R clear)
        let left = StereoFlags::from_register(0x80);
        assert!(left.left && !left.right);
        assert_eq!(left.to_register(), 0x80);

        // Right only: 0x40 (bit7=L clear, bit6=R set)
        let right = StereoFlags::from_register(0x40);
        assert!(!right.left && right.right);
        assert_eq!(right.to_register(), 0x40);

        // Muted: 0x00
        let muted = StereoFlags::from_register(0x00);
        assert!(!muted.left && !muted.right);
        assert_eq!(muted.to_register(), 0x00);
    }

    #[test]
    fn test_channel_stereo_panning() {
        let mut channel = Channel::new(7_670_454);

        // Setup operators for sound
        for i in 0..4 {
            channel.operator_mut(i).set_frequency(0x2CE, 4, 1);
            channel.operator_mut(i).total_level = 0;
            channel.operator_mut(i).envelope_mut().attack_rate = 31;
            channel.operator_mut(i).key_on();
        }

        // Clock to build up volume
        for _ in 0..1000 {
            channel.clock();
        }

        // Both channels enabled
        channel.set_stereo(StereoFlags::both());
        let (left, right) = channel.clock();
        println!("Both: L={}, R={}", left, right);
        assert_ne!(left, 0);
        assert_ne!(right, 0);
        assert_eq!(left, right); // Should be equal

        // Left only
        channel.set_stereo(StereoFlags::left_only());
        let (left, right) = channel.clock();
        println!("Left: L={}, R={}", left, right);
        assert_ne!(left, 0);
        assert_eq!(right, 0);

        // Right only
        channel.set_stereo(StereoFlags::right_only());
        let (left, right) = channel.clock();
        println!("Right: L={}, R={}", left, right);
        assert_eq!(left, 0);
        assert_ne!(right, 0);

        // Muted
        channel.set_stereo(StereoFlags::muted());
        let (left, right) = channel.clock();
        println!("Muted: L={}, R={}", left, right);
        assert_eq!(left, 0);
        assert_eq!(right, 0);
    }

    #[test]
    fn test_key_on_off() {
        let mut channel = Channel::new(7_670_454);

        // Setup operators
        for i in 0..4 {
            channel.operator_mut(i).set_frequency(0x2CE, 4, 1);
            channel.operator_mut(i).total_level = 0;
            channel.operator_mut(i).envelope_mut().attack_rate = 31;
        }

        // Key on all
        channel.key_on_all();
        for i in 0..4 {
            assert!(channel.operator(i).envelope().is_key_on());
        }

        // Key off all
        channel.key_off_all();
        for i in 0..4 {
            assert!(!channel.operator(i).envelope().is_key_on());
        }
    }

    #[test]
    fn test_selective_key_on_off() {
        let mut channel = Channel::new(7_670_454);

        // Setup operators
        for i in 0..4 {
            channel.operator_mut(i).set_frequency(0x2CE, 4, 1);
            channel.operator_mut(i).total_level = 0;
            channel.operator_mut(i).envelope_mut().attack_rate = 31;
        }

        // Key on operators 0 and 2 (bitmask: 0b0101 = 0x05)
        channel.key_on(0x05);
        assert!(channel.operator(0).envelope().is_key_on());
        assert!(!channel.operator(1).envelope().is_key_on());
        assert!(channel.operator(2).envelope().is_key_on());
        assert!(!channel.operator(3).envelope().is_key_on());

        // Key off OP1 (bitmask: 0x01)
        channel.key_off(0x01);
        assert!(!channel.operator(0).envelope().is_key_on());
        assert!(channel.operator(2).envelope().is_key_on());
    }

    #[test]
    fn test_algorithm7_additive() {
        let mut channel = Channel::new(7_670_454);
        channel.set_algorithm(7); // All parallel

        // Setup all operators with different frequencies
        for i in 0..4 {
            channel
                .operator_mut(i)
                .set_frequency(0x100 * (i as u16 + 1), 4, 1);
            channel.operator_mut(i).total_level = 0;
            channel.operator_mut(i).envelope_mut().attack_rate = 31;
            channel.operator_mut(i).envelope_mut().decay_rate = 0;
            channel.operator_mut(i).envelope_mut().sustain_level = 0;
        }

        // Key on all
        channel.key_on_all();

        // Clock to build up volume
        for _ in 0..5000 {
            channel.clock();
        }

        // Should have output (additive synthesis)
        let (left, right) = channel.clock();
        println!("Algorithm 7 output: L={}, R={}", left, right);

        // With 4 operators, should have significant output
        assert!(
            left.abs() > 100,
            "Should have output from additive synthesis"
        );
    }

    #[test]
    fn test_channel_reset() {
        let mut channel = Channel::new(7_670_454);

        // Setup and key on
        channel.operator_mut(0).set_frequency(0x2CE, 4, 1);
        channel.operator_mut(0).key_on();

        // Clock
        for _ in 0..100 {
            channel.clock();
        }

        // Reset
        channel.reset();

        // Operators should be reset
        assert_eq!(channel.operator(0).get_output(), 0);
        assert_eq!(channel.op1_prev, [0, 0]);
    }
}
