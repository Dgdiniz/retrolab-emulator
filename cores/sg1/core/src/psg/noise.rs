//! PSG Noise Channel Implementation
//!
//! Implements the noise channel for the SN76489 PSG.
//! The noise channel uses a 16-bit Linear Feedback Shift Register (LFSR)
//! to generate pseudo-random noise (white or periodic).

use retro_core::audio::{ChannelState, ChannelType, NoiseMode, NoiseParams, NoiseShiftRate};

/// PSG Noise Channel
///
/// Generates noise using a 16-bit LFSR (Genesis SN76496 integrated variant).
/// The shift rate and feedback tap determine the character of the noise
/// (white vs periodic). The LFSR only shifts on positive clock edges
/// (every other counter period), matching real hardware behavior.
pub struct NoiseChannel {
    /// Shift rate select (2 bits)
    shift_rate: NoiseShiftRate,

    /// Mode (1 bit): Periodic or White noise
    mode: NoiseMode,

    /// 16-bit Linear Feedback Shift Register (Genesis variant)
    /// Initialized to 0x8000 on reset
    lfsr: u16,

    /// Attenuation (4-bit value, 0-15)
    attenuation: u8,

    /// Counter for shift rate
    counter: u16,

    /// Noise clock polarity - LFSR only shifts on positive edge (false->true)
    polarity: bool,

    /// Reference frequency from tone channel 3 (for Tone3 mode)
    tone3_frequency: u16,

    /// Channel muted by debugger
    pub muted: bool,

    /// Clock frequency (for calculating Hz)
    _clock_freq: u32,
}

impl NoiseChannel {
    /// Create a new noise channel with the given clock frequency
    pub fn new(clock_freq: u32) -> Self {
        Self {
            shift_rate: NoiseShiftRate::N512,
            mode: NoiseMode::Periodic,
            lfsr: 0x8000,    // Initial LFSR state
            attenuation: 15, // Start muted
            counter: 0,
            polarity: false,
            tone3_frequency: 0,
            muted: false,
            _clock_freq: clock_freq,
        }
    }

    /// Set control register (3 bits: SR1 SR0 M)
    ///
    /// Bits [1:0] = Shift rate select
    /// Bit [2] = Mode (0 = periodic, 1 = white)
    ///
    /// # Arguments
    /// * `control` - 3-bit control value (masked to 0x07)
    pub fn set_control(&mut self, control: u8) {
        self.shift_rate = match control & 0x03 {
            0 => NoiseShiftRate::N512,
            1 => NoiseShiftRate::N1024,
            2 => NoiseShiftRate::N2048,
            3 => NoiseShiftRate::Tone3,
            _ => unreachable!(),
        };

        self.mode = if (control & 0x04) != 0 {
            NoiseMode::White
        } else {
            NoiseMode::Periodic
        };

        // Writing to the noise register resets the shift register
        // (one bit set at the top of the shift register's width)
        self.lfsr = 0x8000;
        self.polarity = false;
    }

    /// Get control register value
    pub fn get_control(&self) -> u8 {
        let sr = match self.shift_rate {
            NoiseShiftRate::N512 => 0,
            NoiseShiftRate::N1024 => 1,
            NoiseShiftRate::N2048 => 2,
            NoiseShiftRate::Tone3 => 3,
        };

        let mode_bit = match self.mode {
            NoiseMode::Periodic => 0,
            NoiseMode::White => 0x04,
        };

        sr | mode_bit
    }

    /// Set tone channel 3 frequency (for Tone3 shift rate mode)
    pub fn set_tone3_frequency(&mut self, freq: u16) {
        self.tone3_frequency = freq & 0x3FF;
    }

    /// Set attenuation (4-bit value, 0-15)
    ///
    /// # Arguments
    /// * `atten` - 4-bit attenuation value (masked to 0xF)
    pub fn set_attenuation(&mut self, atten: u8) {
        self.attenuation = atten & 0x0F;
    }

    /// Get attenuation value
    pub fn get_attenuation(&self) -> u8 {
        self.attenuation
    }

    /// Get normalized volume (0.0 to 1.0)
    pub fn get_volume(&self) -> f32 {
        if self.attenuation == 15 {
            0.0
        } else {
            0.794328_f32.powi(self.attenuation as i32)
        }
    }

    /// Get the period for the current shift rate
    fn get_period(&self) -> u16 {
        match self.shift_rate {
            NoiseShiftRate::N512 => 0x10,
            NoiseShiftRate::N1024 => 0x20,
            NoiseShiftRate::N2048 => 0x40,
            NoiseShiftRate::Tone3 => {
                if self.tone3_frequency == 0 {
                    1024
                } else {
                    self.tone3_frequency
                }
            }
        }
    }

    /// Clock the noise channel one cycle, returns output sample
    ///
    /// On real hardware, the LFSR only shifts on the positive edge of the
    /// noise clock (every other counter period). The polarity toggles each
    /// time the counter expires, and the LFSR shifts only when polarity
    /// transitions to true (positive edge).
    ///
    /// # Returns
    /// Sample value in range [-1.0, 1.0], or 0.0 if muted
    pub fn clock(&mut self) -> f32 {
        // Decrement counter based on shift rate
        if self.counter == 0 {
            self.counter = self.get_period();
            // Toggle polarity on each counter expiration
            self.polarity = !self.polarity;
            // LFSR only shifts on positive edge (when polarity becomes true)
            if self.polarity {
                self.shift_lfsr();
            }
        } else {
            self.counter -= 1;
        }

        // Return output sample
        self.get_output()
    }

    /// Get current output without clocking
    ///
    /// # Returns
    /// Sample value in range [-1.0, 1.0], or 0.0 if muted
    pub fn get_output(&self) -> f32 {
        if self.muted {
            return 0.0;
        }

        // Get volume
        let volume = self.get_volume();

        // Get output from LFSR bit 0
        let bit = (self.lfsr & 1) != 0;

        if bit {
            volume
        } else {
            -volume
        }
    }

    /// Shift the LFSR and generate feedback
    ///
    /// White noise: XOR bits 0 and 3 (taps at positions 0 and 3)
    /// Periodic: Just use bit 0
    ///
    /// The feedback bit is inserted at position 15 (after right shift).
    fn shift_lfsr(&mut self) {
        let feedback = match self.mode {
            // White noise: XOR bits 0 and 3
            NoiseMode::White => (self.lfsr & 1) ^ ((self.lfsr >> 3) & 1),
            // Periodic: just use bit 0
            NoiseMode::Periodic => self.lfsr & 1,
        };

        // Shift right and insert feedback at bit 15 (Genesis uses 16-bit LFSR)
        self.lfsr = (self.lfsr >> 1) | (feedback << 15);
    }

    /// Reset the channel to initial state
    pub fn reset(&mut self) {
        self.shift_rate = NoiseShiftRate::N512;
        self.mode = NoiseMode::Periodic;
        self.lfsr = 0x8000;
        self.attenuation = 15; // Muted
        self.counter = 0;
        self.polarity = false;
        self.tone3_frequency = 0;
    }

    /// Get current state for debugging
    pub fn get_state(&self, channel_id: u32) -> ChannelState {
        ChannelState {
            channel_id,
            enabled: self.attenuation < 15,
            muted: self.muted,
            frequency: 0.0, // Noise doesn't have a specific frequency
            frequency_raw: 0,
            period: self.get_period() as u32 * 32, // In clock cycles
            volume: self.get_volume(),
            attenuation: self.attenuation,
            pan: 0.0, // PSG is mono
            channel_type: ChannelType::Noise,
            envelope: None,
            current_output: self.get_output(),
            noise_params: Some(NoiseParams {
                shift_rate: self.shift_rate,
                mode: self.mode,
                lfsr_state: self.lfsr,
            }),
        }
    }
}

impl NoiseChannel {
    pub fn shift_rate_raw(&self) -> u8 {
        match self.shift_rate {
            NoiseShiftRate::N512 => 0,
            NoiseShiftRate::N1024 => 1,
            NoiseShiftRate::N2048 => 2,
            NoiseShiftRate::Tone3 => 3,
        }
    }
    pub fn mode_raw(&self) -> u8 {
        match self.mode {
            NoiseMode::Periodic => 0,
            NoiseMode::White => 1,
        }
    }
    pub fn lfsr(&self) -> u16 {
        self.lfsr
    }
    pub fn attenuation(&self) -> u8 {
        self.attenuation
    }
    pub fn counter(&self) -> u16 {
        self.counter
    }
    pub fn polarity(&self) -> bool {
        self.polarity
    }
    pub fn tone3_frequency(&self) -> u16 {
        self.tone3_frequency
    }

    #[allow(clippy::too_many_arguments)]
    pub fn restore_state(
        &mut self,
        shift_rate: u8,
        mode: u8,
        lfsr: u16,
        attenuation: u8,
        counter: u16,
        polarity: bool,
        tone3_freq: u16,
    ) {
        self.shift_rate = match shift_rate {
            0 => NoiseShiftRate::N512,
            1 => NoiseShiftRate::N1024,
            2 => NoiseShiftRate::N2048,
            _ => NoiseShiftRate::Tone3,
        };
        self.mode = if mode == 0 {
            NoiseMode::Periodic
        } else {
            NoiseMode::White
        };
        self.lfsr = lfsr;
        self.attenuation = attenuation;
        self.counter = counter;
        self.polarity = polarity;
        self.tone3_frequency = tone3_freq;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CLOCK: u32 = 3_579_545;

    #[test]
    fn test_noise_channel_creation() {
        let channel = NoiseChannel::new(TEST_CLOCK);
        assert_eq!(channel.get_attenuation(), 15);
        assert_eq!(channel.lfsr, 0x8000);
        assert!(!channel.muted);
    }

    #[test]
    fn test_set_control() {
        let mut channel = NoiseChannel::new(TEST_CLOCK);

        // N512, Periodic
        channel.set_control(0x00);
        assert!(matches!(channel.shift_rate, NoiseShiftRate::N512));
        assert!(matches!(channel.mode, NoiseMode::Periodic));

        // N1024, White
        channel.set_control(0x05); // 0b101
        assert!(matches!(channel.shift_rate, NoiseShiftRate::N1024));
        assert!(matches!(channel.mode, NoiseMode::White));

        // N2048, Periodic
        channel.set_control(0x02);
        assert!(matches!(channel.shift_rate, NoiseShiftRate::N2048));
        assert!(matches!(channel.mode, NoiseMode::Periodic));

        // Tone3, White
        channel.set_control(0x07);
        assert!(matches!(channel.shift_rate, NoiseShiftRate::Tone3));
        assert!(matches!(channel.mode, NoiseMode::White));
    }

    #[test]
    fn test_get_control() {
        let mut channel = NoiseChannel::new(TEST_CLOCK);

        channel.set_control(0x07);
        assert_eq!(channel.get_control(), 0x07);

        channel.set_control(0x02);
        assert_eq!(channel.get_control(), 0x02);
    }

    #[test]
    fn test_set_attenuation() {
        let mut channel = NoiseChannel::new(TEST_CLOCK);

        channel.set_attenuation(0);
        assert_eq!(channel.get_attenuation(), 0);
        assert_eq!(channel.get_volume(), 1.0);

        channel.set_attenuation(15);
        assert_eq!(channel.get_attenuation(), 15);
        assert_eq!(channel.get_volume(), 0.0);
    }

    #[test]
    fn test_lfsr_shift_white_noise() {
        let mut channel = NoiseChannel::new(TEST_CLOCK);
        channel.set_control(0x04); // White noise
        channel.lfsr = 0x8000;

        let initial = channel.lfsr;

        // Shift LFSR multiple times
        for _ in 0..100 {
            channel.shift_lfsr();
        }

        // LFSR should have changed
        assert_ne!(channel.lfsr, initial);

        // LFSR should not be zero (for white noise)
        assert_ne!(channel.lfsr, 0);
    }

    #[test]
    fn test_lfsr_shift_periodic() {
        let mut channel = NoiseChannel::new(TEST_CLOCK);
        channel.set_control(0x00); // Periodic
        channel.lfsr = 0x8000;

        // Periodic noise should eventually repeat
        let mut states = Vec::new();
        for _ in 0..50 {
            states.push(channel.lfsr);
            channel.shift_lfsr();
        }

        // Should have some variation
        let unique_count = states
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len();
        assert!(unique_count > 1);
    }

    #[test]
    fn test_clock_generates_output() {
        let mut channel = NoiseChannel::new(TEST_CLOCK);
        channel.set_control(0x04); // White noise
        channel.set_attenuation(0); // Max volume

        // Start with a different LFSR state that has bit 0 set
        channel.lfsr = 0x4001; // This has bit 0 = 1

        let mut outputs = Vec::new();
        // Clock enough times to see LFSR variations
        let period = channel.get_period();
        for _ in 0..(period as usize * 50) {
            outputs.push(channel.clock());
        }

        // Should have both positive and negative values
        let has_positive = outputs.iter().any(|&x| x > 0.0);
        let has_negative = outputs.iter().any(|&x| x < 0.0);

        // Also verify output is not always the same
        let first = outputs[0];
        let has_variation = outputs.iter().any(|&x| (x - first).abs() > 0.001);

        assert!(
            has_positive,
            "No positive values found in {} samples",
            outputs.len()
        );
        assert!(
            has_negative,
            "No negative values found in {} samples",
            outputs.len()
        );
        assert!(has_variation, "Output has no variation");
    }

    #[test]
    fn test_mute() {
        let mut channel = NoiseChannel::new(TEST_CLOCK);
        channel.set_control(0x04);
        channel.set_attenuation(0);

        // Not muted
        channel.clock();
        let output = channel.get_output();
        assert!(output.abs() > 0.0);

        // Muted
        channel.muted = true;
        assert_eq!(channel.get_output(), 0.0);
        assert_eq!(channel.clock(), 0.0);
    }

    #[test]
    fn test_reset() {
        let mut channel = NoiseChannel::new(TEST_CLOCK);

        channel.set_control(0x07);
        channel.set_attenuation(5);
        channel.lfsr = 0x1234;

        channel.reset();

        assert_eq!(channel.get_attenuation(), 15);
        assert_eq!(channel.lfsr, 0x8000);
        assert!(matches!(channel.shift_rate, NoiseShiftRate::N512));
        assert!(matches!(channel.mode, NoiseMode::Periodic));
    }

    #[test]
    fn test_tone3_mode() {
        let mut channel = NoiseChannel::new(TEST_CLOCK);

        // Set to Tone3 mode
        channel.set_control(0x03);
        assert!(matches!(channel.shift_rate, NoiseShiftRate::Tone3));

        // Set tone3 frequency
        channel.set_tone3_frequency(100);
        assert_eq!(channel.get_period(), 100);
    }

    #[test]
    fn test_get_state() {
        let mut channel = NoiseChannel::new(TEST_CLOCK);
        channel.set_control(0x05); // N1024, White
        channel.set_attenuation(3);

        let state = channel.get_state(3);

        assert_eq!(state.channel_id, 3);
        assert!(state.enabled);
        assert!(!state.muted);
        assert_eq!(state.attenuation, 3);
        assert_eq!(state.channel_type, ChannelType::Noise);

        let params = state.noise_params.unwrap();
        assert!(matches!(params.shift_rate, NoiseShiftRate::N1024));
        assert!(matches!(params.mode, NoiseMode::White));
    }

    #[test]
    fn test_get_period() {
        let mut channel = NoiseChannel::new(TEST_CLOCK);

        channel.set_control(0x00); // N512
        assert_eq!(channel.get_period(), 0x10);

        channel.set_control(0x01); // N1024
        assert_eq!(channel.get_period(), 0x20);

        channel.set_control(0x02); // N2048
        assert_eq!(channel.get_period(), 0x40);

        // Tone3 mode
        channel.set_control(0x03);
        channel.set_tone3_frequency(123);
        assert_eq!(channel.get_period(), 123);
    }
}
