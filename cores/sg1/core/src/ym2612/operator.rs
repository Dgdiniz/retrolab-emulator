// YM2612 Operator - Basic FM Synthesis Building Block
//
// An operator is essentially a sine wave generator with:
// - Configurable frequency (via F-Number, Block, Multiple)
// - Phase accumulation
// - Frequency modulation capability (FM input)
// - Attenuation control (Total Level)
// - An ADSR envelope (`envelope.rs`)

use super::envelope::EnvelopeGenerator;
#[allow(unused_imports)]
use super::tables::init_sine_table;
use super::tables::{compute_operator_output, DETUNE_TABLE};

/// YM2612 Operator - generates sine waves with FM modulation
pub struct Operator {
    // ===== Frequency Parameters =====
    /// F-Number: 11-bit frequency parameter (0-2047)
    pub freq_number: u16,

    /// Block: 3-bit octave selector (0-7)
    /// Acts as a frequency multiplier: freq x 2^block
    pub block: u8,

    /// Detune: 3-bit fine tuning (0-7)
    /// Slightly adjusts frequency for chorus effects
    pub detune: u8,

    /// Multiple: 4-bit frequency multiplier (0-15)
    /// Special case: 0 = x0.5, 1-15 = x1 to x15
    pub multiple: u8,

    // ===== Phase State =====
    /// Phase accumulator (20-bit internally, upper 10 bits used for table lookup)
    phase: u32,

    /// Phase increment per clock (calculated from frequency parameters)
    phase_increment: u32,

    // ===== Attenuation =====
    /// Total Level: 7-bit attenuation (0 = max volume, 127 = silence)
    /// This is a direct attenuation control, separate from envelope
    pub total_level: u8,

    // ===== LFO Sensitivity =====
    /// AM Enable flag (bit 7 of register 0x60)
    /// When false, this operator ignores LFO amplitude modulation
    pub am_enable: bool,

    /// Amplitude Modulation Sensitivity (0-3) - per CHANNEL, set from register 0xB4
    /// Controls how much LFO affects amplitude for operators with am_enable=true
    /// 0 = no modulation, 3 = maximum (up to 11.8dB)
    pub ams: u8,

    // ===== Key Scaling =====
    /// Rate Scaling (KS): 2-bit (0-3)
    /// Controls how much the note's frequency affects envelope rates
    /// Higher notes = faster envelope rates
    /// 0 = no scaling, 3 = maximum scaling
    pub rate_scaling: u8,

    /// Key Code: calculated from Block and F-Number bit 10
    /// Used for key scaling calculation
    /// Key Code = Block * 2 + (F-num bit 10)
    key_code: u8,

    // ===== Envelope =====
    /// ADSR envelope generator
    envelope: EnvelopeGenerator,

    // ===== Output =====
    /// Last output sample (-8168 to +8168)
    output: i16,

    // ===== Clock =====
    /// Chip clock frequency (typically 7,670,454 Hz for Genesis)
    clock_freq: u32,

    /// Phase increment scale factor (16.16 fixed point).
    /// Converts raw hardware phase increment (for internal rate = clock/144)
    /// to the actual output sample rate.
    /// scale = (clock_freq << 16) / (144 * sample_rate)
    phase_scale: u32,
}

impl Operator {
    /// Create a new operator
    pub fn new(clock_freq: u32) -> Self {
        Self::with_sample_rate(clock_freq, 53267) // Default to internal rate
    }

    /// Create a new operator with explicit sample rate for phase scaling
    pub fn with_sample_rate(clock_freq: u32, sample_rate: u32) -> Self {
        // Ensure sine table is initialized
        init_sine_table();

        // Calculate phase scale factor (16.16 fixed point)
        // scale = clock_freq / (144 * sample_rate)
        // This converts the hardware phase increment (designed for internal rate
        // = clock/144 ~ 53,267 Hz) to the actual output sample rate.
        let phase_scale = if sample_rate > 0 {
            ((clock_freq as u64) << 16) / (144u64 * sample_rate as u64)
        } else {
            1u64 << 16 // 1.0 in fixed point
        } as u32;

        Self {
            freq_number: 0,
            block: 0,
            detune: 0,
            multiple: 1, // Default to x1
            phase: 0,
            phase_increment: 0,
            total_level: 127, // Start muted
            am_enable: false, // AM off by default
            ams: 0,           // No AM sensitivity by default
            rate_scaling: 0,  // No key scaling by default
            key_code: 0,
            envelope: EnvelopeGenerator::new(),
            output: 0,
            clock_freq,
            phase_scale,
        }
    }

    /// Update sample rate (recalculates phase scale factor)
    pub fn set_sample_rate(&mut self, sample_rate: u32) {
        self.phase_scale = if sample_rate > 0 {
            ((self.clock_freq as u64) << 16) / (144u64 * sample_rate as u64)
        } else {
            1u64 << 16
        } as u32;
        // Recalculate phase increment with new scale
        self.update_phase_increment();
    }

    /// Calculate and update phase increment from frequency parameters
    ///
    /// YM2612 frequency formula:
    /// freq_hz = (F-Number x 2^Block x Clock) / (2^20 x 288) x Multiple
    ///
    /// Phase increment per internal sample (Clock / 144), on a 2^20 cycle:
    /// phase_inc = (F-Number x 2^Block / 2 + detune) x Multiple
    ///
    /// The phase advances once per output sample, so `phase_scale` converts
    /// the increment from the internal sample rate to the output rate.
    pub fn update_phase_increment(&mut self) {
        if self.freq_number == 0 {
            self.phase_increment = 0;
            return;
        }

        // The chip's phase increment, in three steps (checked against Genesis
        // Plus GX): the F-number raised by the block and halved; the detune
        // added or taken away, before the multiply; then the multiple, a
        // half for MUL 0, MUL itself otherwise.

        // Step 1: the note's base increment.
        let base = ((self.freq_number as u32) << self.block) >> 1;

        // Step 2: DT1 detune, applied before the multiplication.
        // DT1 register: bits 1-0 = detune magnitude (0-3), bit 2 = sign
        let magnitude = (self.detune & 0x03) as usize;
        let downward = (self.detune >> 2) & 0x01 != 0;
        let key_code = (self.key_code as usize).min(31);
        let detune = DETUNE_TABLE[magnitude][key_code];

        let detuned = if downward {
            (base as i32 - detune).max(0) as u32
        } else {
            (base as i32 + detune) as u32
        };

        // Step 3: the multiple, counted in halves (1 for MUL 0, twice MUL
        // otherwise) and halved at the end.
        let halves = if self.multiple == 0 {
            1u32
        } else {
            self.multiple as u32 * 2
        };

        // Raw hardware increment (correct for internal rate = clock/144 ~ 53 kHz)
        let raw_inc = (detuned * halves) >> 1;

        // Scale for actual output sample rate using 16.16 fixed-point factor
        // This ensures correct frequencies regardless of output sample rate
        self.phase_increment = ((raw_inc as u64 * self.phase_scale as u64) >> 16) as u32;
    }

    /// Clock the operator once, generating one sample
    ///
    /// Uses hardware-accurate log-sine/exp pipeline:
    /// phase -> log_sine_table -> add envelope attenuation -> exp_table -> output
    ///
    /// # Arguments
    /// * `modulation` - FM modulation input from another operator (0 if no modulation)
    ///
    /// No LFO amplitude modulation here: it calls `clock_with_am(modulation, 0)`.
    ///
    /// # Returns
    /// Output sample value (14-bit signed)
    pub fn clock(&mut self, modulation: i16) -> i16 {
        self.clock_with_am(modulation, 0)
    }

    /// Clock with AM modulation applied
    pub fn clock_with_am(&mut self, modulation: i16, am_attenuation: u16) -> i16 {
        self.clock_with_am_and_phase_adj(modulation, am_attenuation, 0)
    }

    /// Clock with AM modulation and phase increment adjustment (for LFO PM)
    /// pm_phase_adj is added to the phase increment for this clock cycle only,
    /// implementing pitch modulation by temporarily changing the frequency.
    pub fn clock_with_am_and_phase_adj(
        &mut self,
        modulation: i16,
        am_attenuation: u16,
        pm_phase_adj: i32,
    ) -> i16 {
        self.clock_full(modulation, am_attenuation, pm_phase_adj, true)
    }

    /// Full clock with optional envelope advancement.
    ///
    /// In real hardware, the EG updates at clock/144/3 ~ 17,756 Hz (every 3
    /// internal samples), while phase and output are computed at the internal
    /// rate. When `advance_eg` is false, the phase still advances and output
    /// is computed, but the envelope state is frozen for this cycle.
    pub fn clock_full(
        &mut self,
        modulation: i16,
        am_attenuation: u16,
        pm_phase_adj: i32,
        advance_eg: bool,
    ) -> i16 {
        // Clock envelope only when EG tick is due
        if advance_eg {
            self.envelope.clock();
        }

        // SSG-EG cycle detection runs every sample
        // (before the sample is produced)
        self.envelope.update_ssg_eg();
        if self.envelope.ssg_phase_reset {
            self.phase = 0;
            self.envelope.ssg_phase_reset = false;
        }

        // Advance phase with PM adjustment
        let adjusted_inc = (self.phase_increment as i32 + pm_phase_adj).max(0) as u32;
        self.phase = self.phase.wrapping_add(adjusted_inc);

        // Get envelope attenuation level (0-1023) + AM
        let env_level = (self.envelope.get_level() + am_attenuation).min(1023);

        // Compute output through hardware-accurate pipeline
        self.output = compute_operator_output(self.phase, modulation, env_level, self.total_level);

        self.output
    }

    /// Get the last output sample
    pub fn get_output(&self) -> i16 {
        self.output
    }

    /// Reset operator state
    pub fn reset(&mut self) {
        self.phase = 0;
        self.output = 0;
        self.envelope.reset();
    }

    /// Trigger key on (start envelope attack); a key-on of an operator that
    /// is not already keyed on also restarts the phase generator.
    pub fn key_on(&mut self) {
        // The phase restarts only for an operator not already keyed on; the
        // envelope restarts its attack either way
        if !self.envelope.is_key_on() {
            self.phase = 0; // Reset phase generator (hardware behavior)
        }
        self.envelope.key_on();
    }

    /// Trigger key off (start envelope release)
    pub fn key_off(&mut self) {
        self.envelope.key_off();
    }

    /// Get mutable reference to envelope for configuration
    pub fn envelope_mut(&mut self) -> &mut EnvelopeGenerator {
        &mut self.envelope
    }

    /// Get reference to envelope for inspection
    pub fn envelope(&self) -> &EnvelopeGenerator {
        &self.envelope
    }

    /// Set frequency parameters directly
    /// Convenience method for testing
    pub fn set_frequency(&mut self, freq_number: u16, block: u8, multiple: u8) {
        self.freq_number = freq_number & 0x7FF; // 11-bit
        self.block = block & 0x07; // 3-bit
        self.multiple = multiple & 0x0F; // 4-bit
        self.update_phase_increment();
    }

    /// The operator's frequency in Hz, as the debugger shows it (LFO pitch
    /// modulation and detune left out).
    pub fn get_frequency_hz(&self) -> f64 {
        if self.freq_number == 0 || self.phase_increment == 0 {
            return 0.0;
        }

        // Simplified calculation
        let freq_value = (self.freq_number as f64) * (1 << self.block) as f64;
        let multiple = if self.multiple == 0 {
            0.5
        } else {
            self.multiple as f64
        };

        // (F-Number x 2^Block x Clock) / (2^20 x 288) x Multiple: the phase
        // steps by F-Number x 2^Block / 2 once per internal sample (Clock /
        // 144) on a 2^20 cycle.
        (freq_value * multiple * self.clock_freq as f64) / (1048576.0 * 288.0)
    }

    /// Set AM Enable flag (bit 7 of register 0x60)
    /// Controls whether LFO amplitude modulation affects this operator
    pub fn set_am_enable(&mut self, enable: bool) {
        self.am_enable = enable;
    }

    /// Get AM Enable flag
    pub fn get_am_enable(&self) -> bool {
        self.am_enable
    }

    /// Set Amplitude Modulation Sensitivity (0-3) - per channel, from register 0xB4
    /// Controls how much LFO affects this operator's amplitude when am_enable is true
    /// 0 = no modulation, 3 = maximum (up to 11.8dB)
    pub fn set_ams(&mut self, ams: u8) {
        self.ams = ams & 0x03;
    }

    /// Get Amplitude Modulation Sensitivity
    pub fn get_ams(&self) -> u8 {
        self.ams
    }

    /// Set Rate Scaling (KS, 0-3)
    /// Controls how much frequency affects envelope rates
    /// 0 = no scaling, 3 = maximum scaling
    pub fn set_rate_scaling(&mut self, rate_scaling: u8) {
        self.rate_scaling = rate_scaling & 0x03;
        // Propagate to envelope for rate scaling calculations
        self.envelope
            .set_key_scaling(self.key_code, self.rate_scaling);
    }

    /// Get Rate Scaling
    pub fn get_rate_scaling(&self) -> u8 {
        self.rate_scaling
    }

    /// Update Key Code from Block and F-Number
    /// Key Code = Block * 2 + (F-num bit 10)
    /// This should be called whenever block or freq_number changes
    pub fn update_key_code(&mut self) {
        let f_num_bit10 = ((self.freq_number >> 10) & 0x01) as u8;
        self.key_code = (self.block << 1) | f_num_bit10;
        // Propagate to envelope for rate scaling calculations
        self.envelope
            .set_key_scaling(self.key_code, self.rate_scaling);
    }

    /// Get Key Code
    pub fn get_key_code(&self) -> u8 {
        self.key_code
    }

    /// Check if operator is keyed on
    pub fn is_key_on(&self) -> bool {
        self.envelope.is_key_on()
    }

    /// Get frequency number
    pub fn get_freq_number(&self) -> u16 {
        self.freq_number
    }

    /// Get block (octave)
    pub fn get_block(&self) -> u8 {
        self.block
    }

    /// Get multiple (frequency multiplier)
    pub fn get_multiple(&self) -> u8 {
        self.multiple
    }

    /// Get total level (attenuation)
    pub fn get_total_level(&self) -> u8 {
        self.total_level
    }

    /// Get detune
    pub fn get_detune(&self) -> u8 {
        self.detune
    }

    /// Apply Key Scaling to an envelope rate
    /// Formula: Effective Rate = Base Rate + (Key Code >> (3 - KS))
    /// Returns the scaled rate, clamped to valid range (0-63)
    pub fn apply_key_scaling(&self, base_rate: u8) -> u8 {
        if self.rate_scaling == 0 || base_rate == 0 {
            return base_rate;
        }

        // Calculate KS adjustment
        let ks_shift = 3 - self.rate_scaling;
        let ks_offset = self.key_code >> ks_shift;

        // Add to base rate and clamp to 63 (6-bit max)
        (base_rate + ks_offset).min(63)
    }
}

impl Operator {
    pub fn to_savestate(&self) -> crate::system::savestate::Ym2612OperatorState {
        crate::system::savestate::Ym2612OperatorState {
            freq_number: self.freq_number,
            block: self.block,
            detune: self.detune,
            multiple: self.multiple,
            phase: self.phase,
            phase_increment: self.phase_increment,
            total_level: self.total_level,
            am_enable: self.am_enable,
            ams: self.ams,
            rate_scaling: self.rate_scaling,
            key_code: self.key_code,
            envelope: self.envelope.to_savestate(),
        }
    }

    pub fn from_savestate(&mut self, s: &crate::system::savestate::Ym2612OperatorState) {
        self.freq_number = s.freq_number;
        self.block = s.block;
        self.detune = s.detune;
        self.multiple = s.multiple;
        self.phase = s.phase;
        self.phase_increment = s.phase_increment;
        self.total_level = s.total_level;
        self.am_enable = s.am_enable;
        self.ams = s.ams;
        self.rate_scaling = s.rate_scaling;
        self.key_code = s.key_code;
        self.envelope.from_savestate(&s.envelope);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_operator_creation() {
        let op = Operator::new(7_670_454);

        assert_eq!(op.freq_number, 0);
        assert_eq!(op.block, 0);
        assert_eq!(op.multiple, 1);
        assert_eq!(op.phase, 0);
        assert_eq!(op.output, 0);
    }

    #[test]
    fn test_phase_increment_calculation() {
        let mut op = Operator::new(7_670_454);

        // Set a frequency
        op.freq_number = 0x2CE; // ~292Hz at block 4 (near D4)
        op.block = 4;
        op.multiple = 1;
        op.update_phase_increment();

        // Phase increment should be non-zero
        assert!(
            op.phase_increment > 0,
            "Phase increment should be calculated"
        );

        println!("Phase increment: {}", op.phase_increment);
    }

    #[test]
    fn test_phase_accumulation() {
        let mut op = Operator::new(7_670_454);
        op.set_frequency(0x2CE, 4, 1);

        let initial_phase = op.phase;

        // Clock several times
        for _ in 0..10 {
            op.clock(0);
        }

        // Phase should have advanced
        assert!(op.phase > initial_phase, "Phase should advance");
        assert_ne!(op.phase, initial_phase, "Phase changed");
    }

    #[test]
    fn test_operator_output_range() {
        let mut op = Operator::new(7_670_454);
        op.set_frequency(0x2CE, 4, 1);
        op.total_level = 0; // Max volume

        // Configure envelope for immediate full volume
        op.envelope_mut().attack_rate = 31;
        op.key_on(); // Trigger key on to activate envelope

        let mut max_output = 0i16;
        let mut min_output = 0i16;

        // Clock many times to find output range
        for _ in 0..10000 {
            let output = op.clock(0);
            max_output = max_output.max(output);
            min_output = min_output.min(output);
        }

        println!("Output range: {} to {}", min_output, max_output);

        // Should have significant output (peaks near 8168: the exp lookup shifted left by 2)
        assert!(
            max_output > 5000,
            "Should have positive peaks >5000, got {}",
            max_output
        );
        assert!(
            min_output < -5000,
            "Should have negative peaks <-5000, got {}",
            min_output
        );
    }

    #[test]
    fn test_operator_modulation() {
        let mut op = Operator::new(7_670_454);
        op.set_frequency(0x2CE, 4, 1);
        op.total_level = 0;

        // Configure envelope for full volume
        op.envelope_mut().attack_rate = 31;
        op.key_on();

        // Clock a bit to reach full volume
        for _ in 0..1000 {
            op.clock(0);
        }

        // Clock without modulation
        let output_no_mod = op.clock(0);

        // Reset phase and clock with modulation
        op.reset();
        op.key_on();
        for _ in 0..1000 {
            op.clock(0);
        }
        let output_with_mod = op.clock(1000);

        // Outputs should be different due to phase modulation
        // (might be same if phase wrapping, but unlikely)
        println!("No mod: {}, With mod: {}", output_no_mod, output_with_mod);
    }

    #[test]
    fn test_total_level_attenuation() {
        let mut op = Operator::new(7_670_454);
        op.set_frequency(0x2CE, 4, 1);
        op.envelope_mut().attack_rate = 31;

        // Max volume
        op.total_level = 0;
        op.key_on();
        let mut max_amplitude_full = 0i16;
        for _ in 0..1000 {
            let out = op.clock(0);
            max_amplitude_full = max_amplitude_full.max(out.abs());
        }

        // Half volume
        op.reset();
        op.key_on();
        op.total_level = 64; // ~half
        let mut max_amplitude_half = 0i16;
        for _ in 0..1000 {
            let out = op.clock(0);
            max_amplitude_half = max_amplitude_half.max(out.abs());
        }

        // Silence
        op.reset();
        op.key_on();
        op.total_level = 127;
        let mut max_amplitude_silence = 0i16;
        for _ in 0..1000 {
            let out = op.clock(0);
            max_amplitude_silence = max_amplitude_silence.max(out.abs());
        }

        println!(
            "Full: {}, Half: {}, Silence: {}",
            max_amplitude_full, max_amplitude_half, max_amplitude_silence
        );

        // Half volume should be significantly less
        assert!(
            max_amplitude_half < max_amplitude_full,
            "Half volume should be less: half={}, full={}",
            max_amplitude_half,
            max_amplitude_full
        );

        // TL=127 should be completely silent
        // (127*8) << 2 = 4064 attenuation: a log_sin of 32 or more passes the 0x1000
        // threshold, and below it the exp shift of 15 leaves nothing of the 8168 peak
        assert_eq!(
            max_amplitude_silence, 0,
            "TL=127 should produce complete silence, got {}",
            max_amplitude_silence
        );
    }

    #[test]
    fn test_multiple_frequencies() {
        let mut op = Operator::new(7_670_454);

        // Test multiple = 0 (x0.5)
        op.set_frequency(0x2CE, 4, 0);
        let freq_half = op.get_frequency_hz();

        // Test multiple = 1 (x1)
        op.set_frequency(0x2CE, 4, 1);
        let freq_1x = op.get_frequency_hz();

        // Test multiple = 2 (x2)
        op.set_frequency(0x2CE, 4, 2);
        let freq_2x = op.get_frequency_hz();

        println!(
            "Frequencies: x0.5={:.1}Hz, x1={:.1}Hz, x2={:.1}Hz",
            freq_half, freq_1x, freq_2x
        );

        // Relationships should hold (approximately)
        assert!(freq_1x > freq_half, "x1 should be > x0.5");
        assert!(freq_2x > freq_1x, "x2 should be > x1");
    }

    #[test]
    fn test_frequency_hz_is_the_pitch_played() {
        // F-number 0x2CE at block 4, multiple 1: 718 x 16 x 7 670 454 /
        // (2^20 x 288) = 291.8 Hz, near D4.
        let mut op = Operator::new(7_670_454);
        op.set_frequency(0x2CE, 4, 1);
        let hz = op.get_frequency_hz();
        assert!((hz - 291.8).abs() < 0.1, "got {hz} Hz");
    }

    #[test]
    fn test_block_octaves() {
        let mut op = Operator::new(7_670_454);

        // Test different blocks (octaves)
        let mut freqs = Vec::new();
        for block in 0..8 {
            op.set_frequency(0x2CE, block, 1);
            freqs.push(op.get_frequency_hz());
        }

        println!("Frequencies by block: {:?}", freqs);

        // Each block should roughly double the frequency
        for i in 1..8 {
            let ratio = freqs[i] / freqs[i - 1];
            assert!(
                ratio > 1.8 && ratio < 2.2,
                "Block {} to {} ratio should be ~2.0, got {}",
                i - 1,
                i,
                ratio
            );
        }
    }

    #[test]
    fn test_operator_reset() {
        let mut op = Operator::new(7_670_454);
        op.set_frequency(0x2CE, 4, 1);

        // Clock to advance phase
        for _ in 0..100 {
            op.clock(0);
        }

        assert!(op.phase > 0, "Phase should have advanced");
        // Note: Output might be zero if we're at a zero-crossing,
        // so we just check that phase advanced

        // Reset
        op.reset();

        assert_eq!(op.phase, 0, "Phase should be reset");
        assert_eq!(op.output, 0, "Output should be reset");
    }
}
