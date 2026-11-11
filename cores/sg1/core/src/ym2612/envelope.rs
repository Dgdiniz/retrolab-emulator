// YM2612 Envelope Generator - ADSR (Attack, Decay, Sustain, Release)
//
// Each operator has an envelope generator that controls its amplitude over time.
// The YM2612 uses a unique envelope system with:
// - 5-bit Attack Rate (AR): 0-31
// - 5-bit Decay Rate (D1R): 0-31
// - 4-bit Sustain Level (SL): 0-15
// - 5-bit Sustain Rate (D2R): 0-31 (secondary decay)
// - 4-bit Release Rate (RR): 0-15
//
// Envelope level: 0-1023 (0 = max volume, 1023 = silence)
// This is an ATTENUATION envelope, not amplitude!
//
// Hardware-accurate implementation using rate tables from YM2612 die analysis.

/// Envelope rate shift table
/// For each effective rate (0-63), determines how frequently the envelope updates.
/// The envelope updates when (counter >> shift) changes, i.e., every 2^shift clocks.
static EG_RATE_SHIFT: [u8; 64] = [
    12, 12, 12, 12, // 0-3: essentially no update
    11, 11, 11, 11, // 4-7
    10, 10, 10, 10, // 8-11
    9, 9, 9, 9, // 12-15
    8, 8, 8, 8, // 16-19
    7, 7, 7, 7, // 20-23
    6, 6, 6, 6, // 24-27
    5, 5, 5, 5, // 28-31
    4, 4, 4, 4, // 32-35
    3, 3, 3, 3, // 36-39
    2, 2, 2, 2, // 40-43
    1, 1, 1, 1, // 44-47
    0, 0, 0, 0, // 48-51
    0, 0, 0, 0, // 52-55
    0, 0, 0, 0, // 56-59
    0, 0, 0, 0, // 60-63
];

/// Envelope increment patterns, one hexadecimal digit per counter
/// position, position 0 in the lowest digit. A rate picks one of the four
/// patterns of its increment size: rates below 48 use the first group at
/// ever shorter intervals, rates 48-63 use the larger ones on every clock.
///
/// The patterns are those of ymfm's `s_increment_table` (`ymfm_fm.ipp`,
/// `attenuation_increment`), Copyright (c) 2021 Aaron Giles, BSD 3-Clause;
/// the notice is at the end of `LICENSE`.
const EG_INC_PATTERNS: [u32; 19] = [
    0x0000_0000, // row 0: rates 0-3 (no update)
    0x1010_1010, // row 1: x mod 4 == 0
    0x1011_1010, // row 2: x mod 4 == 1
    0x1110_1110, // row 3: x mod 4 == 2
    0x1111_1110, // row 4: x mod 4 == 3
    0x1111_1111, // row 5: rates 48+ base
    0x2111_2111, // row 6
    0x2121_2121, // row 7
    0x2221_2221, // row 8
    0x2222_2222, // row 9
    0x4222_4222, // row 10
    0x4242_4242, // row 11
    0x4442_4442, // row 12
    0x4444_4444, // row 13
    0x8444_8444, // row 14
    0x8484_8484, // row 15
    0x8884_8884, // row 16
    0x8888_8888, // row 17: maximum rate
    0x8888_8888, // row 18: overflow safety
];

/// Envelope increment table: each row has 8 entries, one per counter
/// position. [`EG_RATE_SELECT`] maps an effective rate to its row.
static EG_INC: [[u8; 8]; 19] = unpack_eg_inc();

const fn unpack_eg_inc() -> [[u8; 8]; 19] {
    let mut table = [[0u8; 8]; 19];
    let mut row = 0;
    while row < 19 {
        let mut position = 0;
        while position < 8 {
            table[row][position] = ((EG_INC_PATTERNS[row] >> (4 * position)) & 0xF) as u8;
            position += 1;
        }
        row += 1;
    }
    table
}

/// Maps effective rate to EG_INC row index
static EG_RATE_SELECT: [u8; 64] = [
    //  0   1   2   3
    0, 0, 0, 0, // rates 0-3:  no update
    //  4   5   6   7
    1, 2, 3, 4, // rates 4-7
    1, 2, 3, 4, // rates 8-11
    1, 2, 3, 4, // rates 12-15
    1, 2, 3, 4, // rates 16-19
    1, 2, 3, 4, // rates 20-23
    1, 2, 3, 4, // rates 24-27
    1, 2, 3, 4, // rates 28-31
    1, 2, 3, 4, // rates 32-35
    1, 2, 3, 4, // rates 36-39
    1, 2, 3, 4, // rates 40-43
    1, 2, 3, 4, // rates 44-47
    //  48  49  50  51
    5, 6, 7, 8, // rates 48-51: enhanced
    9, 10, 11, 12, // rates 52-55
    13, 14, 15, 16, // rates 56-59
    17, 17, 17, 17, // rates 60-63: maximum
];

/// Envelope state machine
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeState {
    /// Attack phase - envelope decreasing (volume increasing)
    Attack,
    /// Decay phase - envelope increasing to sustain level
    Decay,
    /// Sustain phase - envelope slowly increasing (secondary decay)
    Sustain,
    /// Release phase - envelope increasing to silence
    Release,
    /// Off - no sound, waiting for key on
    Off,
}

/// ADSR Envelope Generator
pub struct EnvelopeGenerator {
    // ===== Envelope Parameters =====
    /// Attack Rate (5-bit): 0-31
    pub attack_rate: u8,

    /// Decay Rate / D1R (5-bit): 0-31
    pub decay_rate: u8,

    /// Sustain Level (4-bit): 0-15
    /// Internal 10-bit: SL * 32 (SL=15 maps to 992)
    pub sustain_level: u8,

    /// Sustain Rate / D2R (5-bit): 0-31
    pub sustain_rate: u8,

    /// Release Rate (4-bit): 0-15
    pub release_rate: u8,

    // ===== State =====
    /// Current envelope state
    state: EnvelopeState,

    /// Current envelope level (10-bit: 0-1023)
    /// 0 = maximum volume (no attenuation)
    /// 1023 = silence (maximum attenuation)
    level: u16,

    /// Internal counter for rate timing
    counter: u32,

    /// Key on/off state
    key_on: bool,

    // ===== Key Scaling (set by Operator) =====
    /// Key code for rate scaling: block*2 + (fnum >> 10)
    key_code: u8,

    /// Rate scaling factor (0-3)
    rate_scaling: u8,

    // ===== SSG-EG Mode =====
    /// SSG-EG enable (bit 3 of register 0x90)
    ssg_eg_enable: bool,

    /// SSG-EG mode (4-bit value from register 0x90)
    ssg_eg_mode: u8,

    /// SSG-EG inversion flag: whether the current SSG-EG cycle runs upside
    /// down. Toggled by alternate mode on each SSG-EG cycle.
    /// Actual output inversion = ssg_eg_inverted XOR ssg_eg_attack().
    ssg_eg_inverted: bool,

    /// Flag set when SSG-EG cycle requests a phase generator reset.
    /// The operator should check this flag, reset phase if set, then clear it.
    pub ssg_phase_reset: bool,
}

impl EnvelopeGenerator {
    /// Create new envelope generator
    pub fn new() -> Self {
        Self {
            attack_rate: 31,  // Fast attack by default
            decay_rate: 0,    // No decay
            sustain_level: 0, // Max sustain level
            sustain_rate: 0,  // No secondary decay
            release_rate: 7,  // Medium release
            state: EnvelopeState::Off,
            level: 1023, // Start at silence
            counter: 0,
            key_on: false,
            key_code: 0,
            rate_scaling: 0,
            ssg_eg_enable: false,
            ssg_eg_mode: 0,
            ssg_eg_inverted: false,
            ssg_phase_reset: false,
        }
    }

    /// Set key code and rate scaling (called by Operator when frequency changes)
    pub fn set_key_scaling(&mut self, key_code: u8, rate_scaling: u8) {
        self.key_code = key_code;
        self.rate_scaling = rate_scaling;
    }

    /// Calculate effective rate from base rate + key scaling
    /// Formula: min(63, base_rate * 2 + (key_code >> (3 - rate_scaling)))
    fn effective_rate(&self, base_rate: u8) -> u8 {
        if base_rate == 0 {
            return 0;
        }

        let ks_shift = 3_u8.saturating_sub(self.rate_scaling);
        let ks_offset = self.key_code >> ks_shift;
        let rate = (base_rate as u16) * 2 + (ks_offset as u16);
        rate.min(63) as u8
    }

    /// Calculate effective rate for release phase
    /// Hardware formula: RR * 4 + 2 + key_scaling (direct, not through effective_rate)
    fn effective_rate_release(&self) -> u8 {
        let base = (self.release_rate as u16) * 4 + 2;
        let ks_shift = 3_u8.saturating_sub(self.rate_scaling);
        let ks_offset = self.key_code >> ks_shift;
        (base + ks_offset as u16).min(63) as u8
    }

    /// Get the envelope increment for the current counter position at given rate
    fn get_increment(&self, effective_rate: u8) -> u8 {
        let row = EG_RATE_SELECT[effective_rate as usize] as usize;
        let shift = EG_RATE_SHIFT[effective_rate as usize] as u32;
        let counter_pos = ((self.counter >> shift) & 7) as usize;
        EG_INC[row][counter_pos]
    }

    /// Check if envelope should update this clock cycle
    fn should_update(&self, effective_rate: u8) -> bool {
        if effective_rate < 2 {
            return false; // Rates 0-1 never update
        }
        let shift = EG_RATE_SHIFT[effective_rate as usize] as u32;
        if shift == 0 {
            return true; // Rate group updates every clock
        }
        // Update when the relevant counter bit toggles
        let mask = (1u32 << shift) - 1;
        (self.counter & mask) == 0
    }

    /// Trigger key on (start attack phase)
    pub fn key_on(&mut self) {
        self.key_on = true;
        self.state = EnvelopeState::Attack;
        self.counter = 0;
        // Note: Level is NOT reset on key on in YM2612
        // It continues from current level

        // SSG-EG: a key-on starts the first cycle the right way up.
        // Actual output inversion is determined by ssg_eg_inverted XOR attack bit.
        if self.ssg_eg_enable {
            self.ssg_eg_inverted = false;
        }
    }

    /// Trigger key off (start release phase)
    pub fn key_off(&mut self) {
        self.key_on = false;
        if self.state != EnvelopeState::Off {
            // With SSG-EG on, a key-off first turns an inverted level back
            // into a plain one, and a level in the upper half goes straight to
            // silence (the chip's behaviour; checked against Genesis Plus GX).
            if self.ssg_eg_enable {
                // Convert EG attenuation level if output was inverted
                if self.is_ssg_output_inverted() {
                    self.level = 512u16.wrapping_sub(self.level) & 0x3FF;
                }
                // Force max attenuation if level is in the SSG-EG upper range
                if self.level >= 512 {
                    self.level = 1023;
                    self.state = EnvelopeState::Off;
                    return;
                }
            }
            self.state = EnvelopeState::Release;
        }
    }

    /// Clock the envelope generator once
    pub fn clock(&mut self) {
        self.counter = self.counter.wrapping_add(1);

        match self.state {
            EnvelopeState::Attack => self.clock_attack(),
            EnvelopeState::Decay => self.clock_decay(),
            EnvelopeState::Sustain => self.clock_sustain(),
            EnvelopeState::Release => self.clock_release(),
            EnvelopeState::Off => {}
        }
    }

    /// Attack phase - level decreases (volume increases)
    /// Uses exponential curve: step proportional to current level
    /// This produces the characteristic fast-then-slow attack curve
    fn clock_attack(&mut self) {
        if self.attack_rate == 0 {
            return; // AR=0 means infinitely slow attack
        }

        let eff_rate = self.effective_rate(self.attack_rate);

        if !self.should_update(eff_rate) {
            return;
        }

        let inc = self.get_increment(eff_rate) as u32;
        if inc == 0 {
            return;
        }

        if eff_rate >= 62 {
            // Maximum rate: instant attack
            self.level = 0;
        } else if self.level > 0 {
            // Exponential attack: step is proportional to current level
            // Hardware formula approximation: level -= (level * inc) >> 4
            // This gives fast decay when far from 0, slowing as it approaches
            let step = ((self.level as u32) * inc) >> 4;
            self.level = self.level.saturating_sub(step.max(1) as u16);
        }

        if self.level == 0 {
            self.state = EnvelopeState::Decay;
        }
    }

    /// Decay phase - level increases to sustain level (linear in log domain)
    /// With SSG-EG on, the envelope moves four times as fast and only while
    /// the level is below 512.
    fn clock_decay(&mut self) {
        let sl_level = self.sustain_level_10bit();

        if self.level >= sl_level {
            self.state = EnvelopeState::Sustain;
            return;
        }

        if self.decay_rate == 0 {
            return; // D1R=0 means no decay
        }

        let eff_rate = self.effective_rate(self.decay_rate);

        if !self.should_update(eff_rate) {
            return;
        }

        let inc = self.get_increment(eff_rate) as u16;
        if inc == 0 {
            return;
        }

        if self.ssg_eg_enable {
            // SSG-EG: 4x increment, only when level < 512
            if self.level < 512 {
                self.level = (self.level + inc * 4).min(1023);
            }
        } else {
            self.level = (self.level + inc).min(1023);
        }

        if self.level >= sl_level {
            self.state = EnvelopeState::Sustain;
        }
    }

    /// Sustain phase - secondary decay D2R (linear in log domain)
    /// With SSG-EG on, the same four-times rate below 512 as the decay phase.
    /// The SSG-EG cycle detection is handled separately in `update_ssg_eg()`.
    fn clock_sustain(&mut self) {
        if self.sustain_rate == 0 {
            return; // D2R=0 means no secondary decay
        }

        let eff_rate = self.effective_rate(self.sustain_rate);

        if !self.should_update(eff_rate) {
            return;
        }

        let inc = self.get_increment(eff_rate) as u16;
        if inc == 0 {
            return;
        }

        if self.ssg_eg_enable {
            // SSG-EG: 4x increment, only when level < 512
            if self.level < 512 {
                self.level = (self.level + inc * 4).min(1023);
            }
        } else {
            self.level = (self.level + inc).min(1023);
            if self.level >= 1023 {
                self.state = EnvelopeState::Off;
            }
        }
    }

    /// Release phase - level increases to silence (linear in log domain)
    /// With SSG-EG on, the release moves four times as fast while the level
    /// is below 512.
    fn clock_release(&mut self) {
        // Hardware release rate formula: effective_rate = RR*4 + 2 + key_scaling
        // RR is 4-bit (0-15), so max base is 15*4+2 = 62
        let eff_rate = self.effective_rate_release();

        if !self.should_update(eff_rate) {
            return;
        }

        let inc = self.get_increment(eff_rate) as u16;
        if inc == 0 {
            return;
        }

        if self.ssg_eg_enable {
            // SSG-EG: 4x increment when level < 512
            if self.level < 512 {
                self.level = (self.level + inc * 4).min(1023);
            } else {
                self.level = (self.level + inc).min(1023);
            }
        } else {
            self.level = (self.level + inc).min(1023);
        }

        if self.level >= 1023 {
            self.state = EnvelopeState::Off;
        }
    }

    /// SSG-EG cycle detection and handling, called every sample, before the
    /// sample is produced (checked against Genesis Plus GX).
    ///
    /// SSG-EG mode bits (when enabled via bit 3):
    ///   E AtAlH  (bit3=Enable, bit2=Attack, bit1=Alternate, bit0=Hold)
    ///   1 0 0 0  \\\\
    ///   1 0 0 1  \___
    ///   1 0 1 0  \/\/
    ///   1 0 1 1  \---
    ///   1 1 0 0  ////
    ///   1 1 0 1  /---
    ///   1 1 1 0  /\/\
    ///   1 1 1 1  /___
    ///
    /// Sets `ssg_phase_reset` when the phase generator should be reset (the operator
    /// resets the phase and clears the flag).
    pub fn update_ssg_eg(&mut self) {
        if !self.ssg_eg_enable {
            return;
        }
        // Only process in non-release, non-off states when level reached SSG threshold
        if self.level < 512
            || self.state == EnvelopeState::Release
            || self.state == EnvelopeState::Off
        {
            return;
        }

        // The envelope reached the top of an SSG-EG cycle (level 512 or
        // more, before release).
        if self.ssg_eg_hold() {
            // Hold mode
            if self.ssg_eg_alternate() {
                // Hold after an alternate cycle: stay upside down.
                self.ssg_eg_inverted = true;
            }
            // Force max attenuation if not inverted output and not in attack
            if self.state != EnvelopeState::Attack && !self.is_ssg_output_inverted() {
                self.level = 1023;
            }
        } else {
            // Loop mode
            if self.ssg_eg_alternate() {
                // Alternate: each cycle runs the other way up.
                self.ssg_eg_inverted = !self.ssg_eg_inverted;
            } else {
                // Reset phase generator (non-alternate only)
                self.ssg_phase_reset = true;
            }

            // Restart envelope (same as key-on logic)
            if self.state != EnvelopeState::Attack {
                let ar_eff = self.effective_rate(self.attack_rate);
                if ar_eff >= 62 {
                    // Instant attack: set to min attenuation immediately
                    self.level = 0;
                    let sl = self.sustain_level_10bit();
                    self.state = if sl == 0 {
                        EnvelopeState::Sustain
                    } else {
                        EnvelopeState::Decay
                    };
                } else if self.level == 0 {
                    let sl = self.sustain_level_10bit();
                    self.state = if sl == 0 {
                        EnvelopeState::Sustain
                    } else {
                        EnvelopeState::Decay
                    };
                } else {
                    self.state = EnvelopeState::Attack;
                }
            }
        }
    }

    /// Convert 4-bit sustain level to 10-bit
    /// SL 0-14: SL * 32
    /// SL 15: 992 (near-max attenuation)
    fn sustain_level_10bit(&self) -> u16 {
        if self.sustain_level >= 15 {
            992 // 31 * 32
        } else {
            (self.sustain_level as u16) * 32
        }
    }

    /// Get current envelope level (0-1023)
    /// When SSG-EG output is inverted (and not in release/off), the level
    /// is mirrored around 512, within ten bits.
    pub fn get_level(&self) -> u16 {
        if self.ssg_eg_enable
            && self.is_ssg_output_inverted()
            && self.state != EnvelopeState::Release
            && self.state != EnvelopeState::Off
        {
            512u16.wrapping_sub(self.level) & 0x3FF
        } else {
            self.level
        }
    }

    /// Get current envelope state
    pub fn get_state(&self) -> EnvelopeState {
        self.state
    }

    pub fn is_key_on(&self) -> bool {
        self.key_on
    }

    /// Get attack rate
    pub fn get_attack_rate(&self) -> u8 {
        self.attack_rate
    }

    /// Get decay rate (D1R)
    pub fn get_decay_rate(&self) -> u8 {
        self.decay_rate
    }

    /// Get sustain rate (D2R)
    pub fn get_sustain_rate(&self) -> u8 {
        self.sustain_rate
    }

    /// Get release rate
    pub fn get_release_rate(&self) -> u8 {
        self.release_rate
    }

    /// Get sustain level
    pub fn get_sustain_level(&self) -> u8 {
        self.sustain_level
    }

    /// Reset envelope to initial state
    pub fn reset(&mut self) {
        self.state = EnvelopeState::Off;
        self.level = 1023;
        self.counter = 0;
        self.key_on = false;
        self.ssg_eg_enable = false;
        self.ssg_eg_mode = 0;
        self.ssg_eg_inverted = false;
        self.ssg_phase_reset = false;
    }

    /// Set SSG-EG mode (register 0x90-0x9F)
    pub fn set_ssg_eg(&mut self, value: u8) {
        self.ssg_eg_mode = value & 0x0F;
        self.ssg_eg_enable = (value & 0x08) != 0;
        if self.ssg_eg_enable {
            self.ssg_eg_inverted = false;
        }
    }

    /// Get SSG-EG mode
    pub fn get_ssg_eg(&self) -> u8 {
        self.ssg_eg_mode
    }

    /// Check if SSG-EG is enabled
    pub fn is_ssg_eg_enabled(&self) -> bool {
        self.ssg_eg_enable
    }

    /// Get SSG-EG bit flags
    /// Bit layout: E AtAlH (bit3=Enable, bit2=Attack, bit1=Alternate, bit0=Hold)
    fn ssg_eg_attack(&self) -> bool {
        (self.ssg_eg_mode & 0x04) != 0 // bit 2
    }

    fn ssg_eg_alternate(&self) -> bool {
        (self.ssg_eg_mode & 0x02) != 0 // bit 1
    }

    fn ssg_eg_hold(&self) -> bool {
        (self.ssg_eg_mode & 0x01) != 0 // bit 0
    }

    /// Check if SSG-EG output is currently inverted: the cycle's own flag,
    /// turned over once more when the mode's attack bit is set.
    fn is_ssg_output_inverted(&self) -> bool {
        self.ssg_eg_inverted ^ self.ssg_eg_attack()
    }
}

impl Default for EnvelopeGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvelopeGenerator {
    pub fn to_savestate(&self) -> crate::system::savestate::EnvelopeState {
        crate::system::savestate::EnvelopeState {
            attack_rate: self.attack_rate,
            decay_rate: self.decay_rate,
            sustain_level: self.sustain_level,
            sustain_rate: self.sustain_rate,
            release_rate: self.release_rate,
            state: self.state as u8,
            level: self.level,
            counter: self.counter,
            key_on: self.key_on,
            key_code: self.key_code,
            rate_scaling: self.rate_scaling,
            ssg_eg_enable: self.ssg_eg_enable,
            ssg_eg_mode: self.ssg_eg_mode,
            ssg_eg_inverted: self.ssg_eg_inverted,
            ssg_phase_reset: self.ssg_phase_reset,
        }
    }

    pub fn from_savestate(&mut self, s: &crate::system::savestate::EnvelopeState) {
        self.attack_rate = s.attack_rate;
        self.decay_rate = s.decay_rate;
        self.sustain_level = s.sustain_level;
        self.sustain_rate = s.sustain_rate;
        self.release_rate = s.release_rate;
        self.state = match s.state {
            0 => EnvelopeState::Attack,
            1 => EnvelopeState::Decay,
            2 => EnvelopeState::Sustain,
            3 => EnvelopeState::Release,
            _ => EnvelopeState::Off,
        };
        self.level = s.level;
        self.counter = s.counter;
        self.key_on = s.key_on;
        self.key_code = s.key_code;
        self.rate_scaling = s.rate_scaling;
        self.ssg_eg_enable = s.ssg_eg_enable;
        self.ssg_eg_mode = s.ssg_eg_mode;
        self.ssg_eg_inverted = s.ssg_eg_inverted;
        self.ssg_phase_reset = s.ssg_phase_reset;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ym2612::tables::table_checksum;

    #[test]
    fn eg_inc_is_unpacked_entry_for_entry() {
        assert_eq!(
            table_checksum(EG_INC.iter().flatten().copied()),
            0xb673_5ef3_98e4_22c5
        );
        assert_eq!(EG_INC[0], [0; 8]);
        assert_eq!(EG_INC[2], [0, 1, 0, 1, 1, 1, 0, 1]);
        assert_eq!(EG_INC[16], [4, 8, 8, 8, 4, 8, 8, 8]);
    }

    #[test]
    fn test_envelope_creation() {
        let eg = EnvelopeGenerator::new();

        assert_eq!(eg.state, EnvelopeState::Off);
        assert_eq!(eg.level, 1023); // Starts at silence
        assert!(!eg.key_on);
    }

    #[test]
    fn test_key_on_triggers_attack() {
        let mut eg = EnvelopeGenerator::new();
        eg.key_on();

        assert_eq!(eg.state, EnvelopeState::Attack);
        assert!(eg.is_key_on());
    }

    #[test]
    fn test_attack_phase() {
        let mut eg = EnvelopeGenerator::new();
        eg.attack_rate = 31; // Very fast attack
        eg.key_on();

        let initial_level = eg.level;

        // Clock until attack completes
        for _ in 0..100000 {
            eg.clock();
            if eg.state != EnvelopeState::Attack {
                break;
            }
        }

        // Level should have decreased (volume increased)
        assert!(
            eg.level < initial_level,
            "Level should decrease during attack"
        );

        // Should transition to decay
        assert_eq!(
            eg.state,
            EnvelopeState::Decay,
            "Should transition to decay after attack"
        );
        assert_eq!(eg.level, 0, "Should reach level 0");
    }

    #[test]
    fn test_attack_rate_affects_speed() {
        // Fast attack
        let mut eg_fast = EnvelopeGenerator::new();
        eg_fast.attack_rate = 31;
        eg_fast.key_on();

        let mut clocks_fast = 0;
        while eg_fast.state == EnvelopeState::Attack && clocks_fast < 100000 {
            eg_fast.clock();
            clocks_fast += 1;
        }

        // Slow attack
        let mut eg_slow = EnvelopeGenerator::new();
        eg_slow.attack_rate = 10;
        eg_slow.key_on();

        let mut clocks_slow = 0;
        while eg_slow.state == EnvelopeState::Attack && clocks_slow < 1000000 {
            eg_slow.clock();
            clocks_slow += 1;
        }

        println!(
            "Fast attack: {} clocks, Slow attack: {} clocks",
            clocks_fast, clocks_slow
        );

        // Fast should complete sooner
        assert!(
            clocks_fast < clocks_slow,
            "Fast attack ({}) should complete faster than slow ({})",
            clocks_fast,
            clocks_slow
        );
    }

    #[test]
    fn test_decay_phase() {
        let mut eg = EnvelopeGenerator::new();
        eg.attack_rate = 31;
        eg.decay_rate = 20;
        eg.sustain_level = 8; // SL=8 -> 8*32 = 256 in 10-bit
        eg.key_on();

        // Complete attack
        for _ in 0..100000 {
            eg.clock();
            if eg.state != EnvelopeState::Attack {
                break;
            }
        }

        assert_eq!(eg.state, EnvelopeState::Decay);

        // Clock through decay
        for _ in 0..500000 {
            eg.clock();
            if eg.state != EnvelopeState::Decay {
                break;
            }
        }

        // Should reach sustain
        assert_eq!(
            eg.state,
            EnvelopeState::Sustain,
            "Should reach sustain after decay"
        );

        // Level should be at or above sustain level (8 * 32 = 256)
        let expected_sl = 8_u16 * 32;
        assert!(
            eg.level >= expected_sl,
            "Level {} should be at sustain level {}",
            eg.level,
            expected_sl
        );
    }

    #[test]
    fn test_sustain_level_affects_decay_target() {
        let mut eg_low = EnvelopeGenerator::new();
        eg_low.attack_rate = 31;
        eg_low.decay_rate = 31;
        eg_low.sustain_level = 2; // Low sustain (loud)
        eg_low.key_on();

        for _ in 0..500000 {
            eg_low.clock();
            if eg_low.state == EnvelopeState::Sustain {
                break;
            }
        }

        let level_low = eg_low.level;

        let mut eg_high = EnvelopeGenerator::new();
        eg_high.attack_rate = 31;
        eg_high.decay_rate = 31;
        eg_high.sustain_level = 14; // High sustain (quiet)
        eg_high.key_on();

        for _ in 0..500000 {
            eg_high.clock();
            if eg_high.state == EnvelopeState::Sustain {
                break;
            }
        }

        let level_high = eg_high.level;

        println!("Low SL={}, High SL={}", level_low, level_high);

        assert!(
            level_high > level_low,
            "Higher sustain level = more attenuation"
        );
    }

    #[test]
    fn test_sustain_phase_with_d2r() {
        let mut eg = EnvelopeGenerator::new();
        eg.attack_rate = 31;
        eg.decay_rate = 31;
        eg.sustain_level = 4;
        eg.sustain_rate = 15;
        eg.key_on();

        for _ in 0..500000 {
            eg.clock();
            if eg.state == EnvelopeState::Sustain {
                break;
            }
        }

        let level_start = eg.level;

        for _ in 0..500000 {
            eg.clock();
        }

        assert!(
            eg.level > level_start,
            "Sustain with D2R should increase level"
        );
    }

    #[test]
    fn test_sustain_without_d2r() {
        let mut eg = EnvelopeGenerator::new();
        eg.attack_rate = 31;
        eg.decay_rate = 31;
        eg.sustain_level = 4;
        eg.sustain_rate = 0; // No secondary decay
        eg.key_on();

        for _ in 0..500000 {
            eg.clock();
            if eg.state == EnvelopeState::Sustain {
                break;
            }
        }

        let level_sustain = eg.level;

        for _ in 0..10000 {
            eg.clock();
        }

        assert_eq!(
            eg.level, level_sustain,
            "Sustain with D2R=0 should stay constant"
        );
    }

    #[test]
    fn test_release_phase() {
        let mut eg = EnvelopeGenerator::new();
        eg.attack_rate = 31;
        eg.release_rate = 15;
        eg.key_on();

        // Complete attack
        for _ in 0..100000 {
            eg.clock();
            if eg.state != EnvelopeState::Attack {
                break;
            }
        }

        eg.key_off();
        assert_eq!(eg.state, EnvelopeState::Release);
        assert!(!eg.is_key_on());

        let level_start = eg.level;

        for _ in 0..500000 {
            eg.clock();
            if eg.state == EnvelopeState::Off {
                break;
            }
        }

        assert!(eg.level > level_start, "Release should increase level");
        assert_eq!(eg.state, EnvelopeState::Off);
        assert_eq!(eg.level, 1023, "Should reach silence");
    }

    #[test]
    fn test_release_rate_affects_speed() {
        // Fast release
        let mut eg_fast = EnvelopeGenerator::new();
        eg_fast.attack_rate = 31;
        eg_fast.release_rate = 15;
        eg_fast.key_on();

        for _ in 0..10000 {
            eg_fast.clock();
        }
        eg_fast.key_off();

        let mut clocks_fast = 0;
        while eg_fast.state == EnvelopeState::Release && clocks_fast < 500000 {
            eg_fast.clock();
            clocks_fast += 1;
        }

        // Slow release
        let mut eg_slow = EnvelopeGenerator::new();
        eg_slow.attack_rate = 31;
        eg_slow.release_rate = 5;
        eg_slow.key_on();

        for _ in 0..10000 {
            eg_slow.clock();
        }
        eg_slow.key_off();

        let mut clocks_slow = 0;
        while eg_slow.state == EnvelopeState::Release && clocks_slow < 5000000 {
            eg_slow.clock();
            clocks_slow += 1;
        }

        println!(
            "Fast release: {} clocks, Slow release: {} clocks",
            clocks_fast, clocks_slow
        );

        assert!(
            clocks_fast < clocks_slow,
            "Fast release ({}) should complete faster than slow ({})",
            clocks_fast,
            clocks_slow
        );
    }

    #[test]
    fn test_full_envelope_cycle() {
        let mut eg = EnvelopeGenerator::new();
        eg.attack_rate = 31;
        eg.decay_rate = 25;
        eg.sustain_level = 8;
        eg.sustain_rate = 10;
        eg.release_rate = 12;

        assert_eq!(eg.state, EnvelopeState::Off);

        // Key on -> Attack
        eg.key_on();
        assert_eq!(eg.state, EnvelopeState::Attack);

        // Complete attack -> Decay
        for _ in 0..100000 {
            eg.clock();
            if eg.state != EnvelopeState::Attack {
                break;
            }
        }
        assert_eq!(eg.state, EnvelopeState::Decay);

        // Complete decay -> Sustain
        for _ in 0..500000 {
            eg.clock();
            if eg.state != EnvelopeState::Decay {
                break;
            }
        }
        assert_eq!(
            eg.state,
            EnvelopeState::Sustain,
            "Should reach sustain after decay"
        );

        // Stay in sustain for a while
        for _ in 0..10000 {
            eg.clock();
        }
        assert_eq!(eg.state, EnvelopeState::Sustain);

        // Key off -> Release
        eg.key_off();
        assert_eq!(eg.state, EnvelopeState::Release);

        // Complete release -> Off
        for _ in 0..500000 {
            eg.clock();
            if eg.state == EnvelopeState::Off {
                break;
            }
        }
        assert_eq!(eg.state, EnvelopeState::Off);
    }

    #[test]
    fn test_key_off_during_attack() {
        let mut eg = EnvelopeGenerator::new();
        eg.attack_rate = 20;
        eg.release_rate = 15;
        eg.key_on();

        for _ in 0..100 {
            eg.clock();
        }
        assert_eq!(eg.state, EnvelopeState::Attack);

        eg.key_off();
        assert_eq!(eg.state, EnvelopeState::Release);
    }

    #[test]
    fn test_envelope_reset() {
        let mut eg = EnvelopeGenerator::new();
        eg.attack_rate = 31;
        eg.key_on();

        for _ in 0..100 {
            eg.clock();
        }

        eg.reset();

        assert_eq!(eg.state, EnvelopeState::Off);
        assert_eq!(eg.level, 1023);
        assert!(!eg.is_key_on());
    }

    #[test]
    fn test_envelope_level_range() {
        let mut eg = EnvelopeGenerator::new();
        eg.attack_rate = 31;
        eg.decay_rate = 31;
        eg.sustain_level = 0;
        eg.sustain_rate = 20;
        eg.release_rate = 15;
        eg.key_on();

        for _ in 0..1000000 {
            eg.clock();
            assert!(eg.level <= 1023, "Level should not exceed 1023");
            if eg.state == EnvelopeState::Off {
                break;
            }
        }
    }

    #[test]
    fn test_effective_rate_calculation() {
        let mut eg = EnvelopeGenerator::new();

        // No key scaling
        eg.key_code = 0;
        eg.rate_scaling = 0;
        assert_eq!(eg.effective_rate(15), 30);

        // With key scaling
        eg.key_code = 14; // high note
        eg.rate_scaling = 3; // max scaling
        let rate = eg.effective_rate(15);
        assert!(rate > 30, "Key scaling should increase rate: got {}", rate);
        assert!(rate <= 63, "Rate should be clamped to 63");
    }

    #[test]
    fn test_sustain_level_conversion() {
        let mut eg = EnvelopeGenerator::new();

        eg.sustain_level = 0;
        assert_eq!(eg.sustain_level_10bit(), 0);

        eg.sustain_level = 8;
        assert_eq!(eg.sustain_level_10bit(), 256);

        eg.sustain_level = 15;
        assert_eq!(eg.sustain_level_10bit(), 992);
    }
}
