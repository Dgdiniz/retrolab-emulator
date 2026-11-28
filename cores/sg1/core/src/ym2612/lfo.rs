//! YM2612 Low Frequency Oscillator (LFO)
//!
//! The LFO provides vibrato (PM - Pitch Modulation) and tremolo (AM - Amplitude Modulation).
//! It's a global effect that can be applied to all channels.
//!
//! Register 0x22:
//! - Bit 3: LFO Enable (0 = off, 1 = on)
//! - Bits 2-0: LFO Frequency (0-7)
//!
//! LFO Frequencies (Hz):
//! - 0: 3.98 Hz
//! - 1: 5.56 Hz
//! - 2: 6.02 Hz
//! - 3: 6.37 Hz
//! - 4: 6.88 Hz
//! - 5: 9.63 Hz
//! - 6: 48.1 Hz
//! - 7: 72.2 Hz

/// One of the chip's own samples, in the units of the LFO's counter: the
/// counter keeps fractions of a sample, 1/65536ths, the units a savestate
/// holds it in (see [`Lfo::clock`]).
pub const CHIP_SAMPLE: u32 = 1 << 16;

/// LFO (Low Frequency Oscillator)
///
/// Provides vibrato (PM) and tremolo (AM) modulation.
pub struct Lfo {
    /// LFO enabled flag
    enabled: bool,

    /// LFO frequency index (0-7)
    frequency: u8,

    /// LFO phase accumulator (0-127)
    phase: u8,

    /// The chip's samples since the phase last stepped, in [`CHIP_SAMPLE`]ths
    counter: u32,

    /// LFO frequency divider values: the chip's samples per phase step (see
    /// `new`)
    frequency_dividers: [u32; 8],
}

impl Lfo {
    /// Create a new LFO
    pub fn new() -> Self {
        Self {
            enabled: false,
            frequency: 0,
            phase: 0,
            counter: 0,
            // Samples each LFO step lasts, per rate setting. The chip's LFO is
            // clocked once per FM sample (one each 144 input clocks), not per
            // chip clock, and a cycle is 128 steps: on the Mega Drive's 53,267
            // samples a second (NTSC), rate 0's 108 a step make a cycle of 3.85 Hz.
            // The chip's own figures (ymfm lists them as counts of 109, 78, ...,
            // one more by its convention); checked against Genesis Plus GX.
            frequency_dividers: [
                108, // 0: 3.98 Hz
                77,  // 1: 5.56 Hz
                71,  // 2: 6.02 Hz
                67,  // 3: 6.37 Hz
                62,  // 4: 6.88 Hz
                44,  // 5: 9.63 Hz
                8,   // 6: 48.1 Hz
                5,   // 7: 72.2 Hz
            ],
        }
    }

    /// Set LFO enable and frequency (register 0x22)
    ///
    /// A write that turns the LFO on from off starts it from its first step.
    /// A write that leaves it on lets it go on from where it is, as Genesis
    /// Plus GX and ymfm run it: a new rate counts on from the samples
    /// counted so far, and one whose count they already reach steps at the
    /// chip's next sample and counts again from there.
    pub fn set_control(&mut self, value: u8) {
        let was_enabled = self.enabled;
        self.enabled = (value & 0x08) != 0;
        self.frequency = value & 0x07;

        if self.enabled && !was_enabled {
            self.phase = 0;
            self.counter = 0;
        } else if self.enabled {
            let divider = self.divider();
            if self.counter >= divider {
                // The count reaches the divider at the next of the chip's
                // samples: one step there.
                self.counter = divider - (CHIP_SAMPLE - self.counter % CHIP_SAMPLE);
            }
        }
    }

    /// The chip's samples a step lasts at the current rate, in
    /// [`CHIP_SAMPLE`]ths.
    fn divider(&self) -> u32 {
        self.frequency_dividers[self.frequency as usize] * CHIP_SAMPLE
    }

    /// Get LFO enabled status
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Get LFO frequency index
    pub fn get_frequency(&self) -> u8 {
        self.frequency
    }

    /// Run the LFO for `chip_samples` of the chip's own samples, in
    /// [`CHIP_SAMPLE`]ths.
    ///
    /// The dividers count the chip's samples (one each 144 input clocks):
    /// `Ym2612::generate_one_sample` calls this once a sample with one
    /// [`CHIP_SAMPLE`]. The phase steps each time the count reaches the
    /// rate's divider, and the count keeps what goes past it, so each step
    /// comes the divider's count of the chip's samples after the one
    /// before, as on the chip, whatever share of a sample each call
    /// brings.
    pub fn clock(&mut self, chip_samples: u32) {
        if !self.enabled {
            return;
        }

        self.counter = self.counter.saturating_add(chip_samples);
        let divider = self.divider();

        while self.counter >= divider {
            self.counter -= divider;
            self.phase = (self.phase + 1) & 0x7F; // 0-127
        }
    }

    /// Get current LFO PM step for hardware-accurate PM table lookup
    ///
    /// Returns a value from 0 to 31: pitch modulation moves once every four
    /// LFO steps. `compute_lfo_pm_offset` uses it as an index into
    /// `LFO_PM_OUTPUT` along with PMS and F-number.
    pub fn get_pm_step(&self) -> u8 {
        if !self.enabled {
            return 0;
        }

        self.phase >> 2 // 0-31
    }

    /// Get current LFO value for PM (Pitch Modulation); the FM path reads `get_pm_step`
    ///
    /// Returns a value from 0 to 63 (triangle wave)
    pub fn get_pm_value(&self) -> i16 {
        if !self.enabled {
            return 0;
        }

        // Triangle wave: up from 0 to 63, then back down to 0
        let phase = self.phase;
        if phase < 64 {
            phase as i16 // 0 to 63
        } else {
            (127 - phase) as i16 // 63 down to 0
        }
    }

    /// Get current LFO value for AM (Amplitude Modulation)
    ///
    /// Returns a value from 0 to 126 in steps of two: a triangle over the
    /// LFO's 128 steps that starts at its peak, comes down to 0 over the
    /// first half and climbs back over the second. (The chip's own shape, as
    /// ymfm describes it; checked against Genesis Plus GX.)
    pub fn get_am_value(&self) -> u8 {
        if !self.enabled {
            return 0;
        }

        let from_the_valley = if self.phase < 64 {
            63 - self.phase
        } else {
            self.phase - 64
        };
        from_the_valley << 1
    }

    /// Get current phase (for debugging)
    pub fn get_phase(&self) -> u8 {
        self.phase
    }

    /// Reset LFO
    pub fn reset(&mut self) {
        self.enabled = false;
        self.frequency = 0;
        self.phase = 0;
        self.counter = 0;
    }
}

impl Default for Lfo {
    fn default() -> Self {
        Self::new()
    }
}

impl Lfo {
    pub fn to_savestate(&self) -> (bool, u8, u8, u32) {
        (self.enabled, self.frequency, self.phase, self.counter)
    }

    pub fn from_savestate(&mut self, enabled: bool, frequency: u8, phase: u8, counter: u32) {
        self.enabled = enabled;
        self.frequency = frequency;
        self.phase = phase;
        self.counter = counter;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lfo_creation() {
        let lfo = Lfo::new();

        assert!(!lfo.is_enabled());
        assert_eq!(lfo.get_frequency(), 0);
        assert_eq!(lfo.get_phase(), 0);
    }

    #[test]
    fn test_lfo_enable() {
        let mut lfo = Lfo::new();

        // Disable LFO
        lfo.set_control(0x00);
        assert!(!lfo.is_enabled());

        // Enable LFO, frequency 3
        lfo.set_control(0x0B); // 0b00001011
        assert!(lfo.is_enabled());
        assert_eq!(lfo.get_frequency(), 3);
    }

    #[test]
    fn test_lfo_frequency_setting() {
        let mut lfo = Lfo::new();

        for freq in 0..8 {
            lfo.set_control(0x08 | freq);
            assert_eq!(lfo.get_frequency(), freq);
            assert!(lfo.is_enabled());
        }
    }

    #[test]
    fn test_lfo_clock_disabled() {
        let mut lfo = Lfo::new();

        // LFO disabled, phase should not advance
        for _ in 0..10000 {
            lfo.clock(CHIP_SAMPLE);
        }

        assert_eq!(lfo.get_phase(), 0);
    }

    #[test]
    fn test_lfo_clock_enabled() {
        let mut lfo = Lfo::new();
        lfo.set_control(0x0F); // Enable, max frequency

        // Clock many times
        for _ in 0..10000 {
            lfo.clock(CHIP_SAMPLE);
        }

        // Phase should have advanced
        assert!(lfo.get_phase() > 0, "Phase should advance when enabled");
    }

    #[test]
    fn test_lfo_phase_wraparound() {
        let mut lfo = Lfo::new();
        lfo.set_control(0x0F); // Enable, max frequency

        let mut prev_phase = 0;
        let mut wrapped = false;

        // Clock until phase wraps around
        for _ in 0..100000 {
            lfo.clock(CHIP_SAMPLE);
            let current_phase = lfo.get_phase();

            if current_phase < prev_phase {
                wrapped = true;
                break;
            }

            prev_phase = current_phase;
        }

        assert!(wrapped, "Phase should wrap around from 127 to 0");
    }

    #[test]
    fn test_lfo_pm_value() {
        let mut lfo = Lfo::new();
        lfo.set_control(0x08); // Enable, frequency 0

        // Disabled: should return 0
        lfo.enabled = false;
        assert_eq!(lfo.get_pm_value(), 0);

        // Enabled: check triangle wave
        lfo.enabled = true;

        lfo.phase = 0;
        assert_eq!(lfo.get_pm_value(), 0);

        lfo.phase = 32;
        assert_eq!(lfo.get_pm_value(), 32);

        lfo.phase = 63;
        assert_eq!(lfo.get_pm_value(), 63);

        lfo.phase = 64;
        assert_eq!(lfo.get_pm_value(), 63);

        lfo.phase = 96;
        assert_eq!(lfo.get_pm_value(), 31);

        lfo.phase = 127;
        assert_eq!(lfo.get_pm_value(), 0);
    }

    #[test]
    fn test_lfo_am_value() {
        let mut lfo = Lfo::new();
        lfo.set_control(0x08); // Enable

        // Disabled: should return 0
        lfo.enabled = false;
        assert_eq!(lfo.get_am_value(), 0);

        // Enabled: the triangle, peak first
        // Phase 0-63: (phase ^ 63) << 1 (126 down to 0)
        // Phase 64-127: (phase & 63) << 1 (0 up to 126)
        lfo.enabled = true;

        lfo.phase = 0;
        assert_eq!(lfo.get_am_value(), 126); // Start at max

        lfo.phase = 32;
        assert_eq!(lfo.get_am_value(), 62); // Midway descending

        lfo.phase = 63;
        assert_eq!(lfo.get_am_value(), 0); // Bottom

        lfo.phase = 64;
        assert_eq!(lfo.get_am_value(), 0); // Start ascending

        lfo.phase = 96;
        assert_eq!(lfo.get_am_value(), 64); // Midway ascending

        lfo.phase = 127;
        assert_eq!(lfo.get_am_value(), 126); // Back to max
    }

    #[test]
    fn test_lfo_reset() {
        let mut lfo = Lfo::new();
        lfo.set_control(0x0F);

        // Advance phase
        for _ in 0..10000 {
            lfo.clock(CHIP_SAMPLE);
        }

        assert!(lfo.get_phase() > 0);
        assert!(lfo.is_enabled());

        // Reset
        lfo.reset();

        assert!(!lfo.is_enabled());
        assert_eq!(lfo.get_frequency(), 0);
        assert_eq!(lfo.get_phase(), 0);
    }

    #[test]
    fn a_step_lasts_the_rate_s_count_of_the_chip_s_samples() {
        let mut lfo = Lfo::new();
        lfo.set_control(0x08); // Enable, rate 0: 108 samples a step

        for _ in 0..107 {
            lfo.clock(CHIP_SAMPLE);
        }
        assert_eq!(lfo.get_phase(), 0);
        lfo.clock(CHIP_SAMPLE);
        assert_eq!(lfo.get_phase(), 1);
    }

    #[test]
    fn the_fraction_of_a_sample_left_over_counts_towards_the_next_step() {
        let mut lfo = Lfo::new();
        lfo.set_control(0x0F); // Enable, rate 7: 5 samples a step

        // One and a half of the chip's samples a call: the steps come at
        // 5, 10 and 15 samples, after the 4th, 7th and 10th calls.
        let steps: Vec<u8> = (0..10)
            .map(|_| {
                lfo.clock(CHIP_SAMPLE * 3 / 2);
                lfo.get_phase()
            })
            .collect();
        assert_eq!(steps, [0, 0, 0, 1, 1, 1, 2, 2, 2, 3]);
    }

    #[test]
    fn a_write_that_leaves_the_lfo_on_does_not_restart_it() {
        let mut lfo = Lfo::new();
        lfo.set_control(0x08); // On, rate 0: 108 samples a step
        for _ in 0..(108 * 3 + 50) {
            lfo.clock(CHIP_SAMPLE);
        }
        assert_eq!(lfo.get_phase(), 3);

        // The same value again: the LFO goes on, and its count with it
        lfo.set_control(0x08);
        assert_eq!(lfo.get_phase(), 3);
        for _ in 0..57 {
            lfo.clock(CHIP_SAMPLE);
        }
        assert_eq!(lfo.get_phase(), 3);
        lfo.clock(CHIP_SAMPLE);
        assert_eq!(lfo.get_phase(), 4);
    }

    #[test]
    fn turning_the_lfo_on_from_off_starts_it_from_its_first_step() {
        let mut lfo = Lfo::new();
        lfo.set_control(0x08);
        for _ in 0..(108 * 3 + 50) {
            lfo.clock(CHIP_SAMPLE);
        }
        lfo.set_control(0x00); // Off
        assert_eq!(lfo.get_pm_step(), 0);
        lfo.set_control(0x08); // On again
        assert_eq!(lfo.get_phase(), 0);
        for _ in 0..107 {
            lfo.clock(CHIP_SAMPLE);
        }
        assert_eq!(lfo.get_phase(), 0);
        lfo.clock(CHIP_SAMPLE);
        assert_eq!(lfo.get_phase(), 1);
    }

    #[test]
    fn a_rate_whose_count_is_already_reached_steps_once_at_the_next_sample() {
        let mut lfo = Lfo::new();
        lfo.set_control(0x08); // On, rate 0: 108 samples a step
        for _ in 0..201 {
            lfo.clock(CHIP_SAMPLE / 2);
        }
        assert_eq!(lfo.get_phase(), 0); // 100.5 samples counted

        // Rate 7 steps every 5 samples: one step at the next sample, half a
        // sample on, then one every 5 from there.
        lfo.set_control(0x0F);
        assert_eq!(lfo.get_phase(), 0);
        lfo.clock(CHIP_SAMPLE / 2);
        assert_eq!(lfo.get_phase(), 1);
        for _ in 0..9 {
            lfo.clock(CHIP_SAMPLE / 2);
        }
        assert_eq!(lfo.get_phase(), 1);
        lfo.clock(CHIP_SAMPLE / 2);
        assert_eq!(lfo.get_phase(), 2);
    }

    #[test]
    fn test_lfo_frequency_dividers() {
        let lfo = Lfo::new();

        // All frequency dividers should be reasonable values
        for divider in &lfo.frequency_dividers {
            assert!(*divider > 0);
            assert!(*divider < 10000); // Sanity check
        }

        // Higher frequency index should have smaller divider
        assert!(lfo.frequency_dividers[7] < lfo.frequency_dividers[0]);
    }
}
