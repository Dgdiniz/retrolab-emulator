//! PSG Tone Channel Implementation
//!
//! Implements a single tone channel for the SN76489 PSG.
//! Each tone channel generates a square wave at a configurable frequency
//! with programmable attenuation (volume).

use retro_core::audio::{ChannelState, ChannelType};

/// PSG Tone Channel
///
/// Generates square waves using a counter that counts down from the
/// frequency register value. When the counter reaches zero, it reloads
/// and the output polarity flips.
pub struct ToneChannel {
    /// Tone frequency (10-bit value, 0-1023)
    /// Actual frequency = Clock / (32 * frequency_value)
    /// frequency_value = 0 is a constant DC output in `clock()`; only the
    /// debugger's Hz and period read it as 1024
    frequency: u16,

    /// Attenuation (4-bit value, 0-15)
    /// 0 = max volume, 15 = silence
    /// Each step = approximately -2dB (factor of ~0.794)
    attenuation: u8,

    /// Counter for waveform generation
    /// Counts down from frequency value, flips polarity on zero
    counter: u16,

    /// Output polarity (square wave)
    /// true = positive half, false = negative half
    polarity: bool,

    /// Channel muted by debugger
    pub muted: bool,

    /// Clock frequency (for calculating Hz)
    clock_freq: u32,
}

impl ToneChannel {
    /// Create a new tone channel with the given clock frequency
    pub fn new(clock_freq: u32) -> Self {
        Self {
            frequency: 0,
            attenuation: 15, // Start muted
            counter: 0,
            polarity: false,
            muted: false,
            clock_freq,
        }
    }

    /// Set frequency (10-bit value, 0-1023)
    ///
    /// # Arguments
    /// * `freq` - 10-bit frequency value (masked to 0x3FF)
    pub fn set_frequency(&mut self, freq: u16) {
        self.frequency = freq & 0x3FF;
        // If counter is 0, reload immediately (freq 0 = DC, no reload needed)
        if self.counter == 0 && self.frequency != 0 {
            self.counter = self.frequency;
        }
    }

    /// Get frequency register value
    pub fn get_frequency(&self) -> u16 {
        self.frequency
    }

    /// Get frequency in Hz
    ///
    /// Formula: Clock / (32 * frequency_value)
    pub fn get_frequency_hz(&self) -> f32 {
        let freq_val = if self.frequency == 0 {
            1024
        } else {
            self.frequency
        };
        self.clock_freq as f32 / (32.0 * freq_val as f32)
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
    ///
    /// Converts attenuation to linear volume.
    /// Each attenuation step is approximately -2dB (factor of ~0.794).
    pub fn get_volume(&self) -> f32 {
        // Pre-computed attenuation lookup table: 0.794328^n for n=0..15
        // Attenuation 15 = mute (0.0), each step is -2dB
        static VOLUME_TABLE: [f32; 16] = [
            1.0,      // 0
            0.794328, // 1
            0.630957, // 2
            0.501187, // 3
            0.398107, // 4
            0.316228, // 5
            0.251189, // 6
            0.199526, // 7
            0.158489, // 8
            0.125893, // 9
            0.1,      // 10
            0.079433, // 11
            0.063096, // 12
            0.050119, // 13
            0.039811, // 14
            0.0,      // 15 (mute)
        ];
        VOLUME_TABLE[self.attenuation as usize & 0xF]
    }

    /// Clock the channel one cycle, returns output sample
    ///
    /// The counter counts down from the frequency value. When it reaches
    /// zero, it reloads and the polarity flips, creating a square wave.
    ///
    /// # Returns
    /// Sample value in range [-1.0, 1.0], or 0.0 if muted
    pub fn clock(&mut self) -> f32 {
        // On the Genesis (SN76496 integrated PSG), frequency register 0
        // produces a constant DC output (high). The counter doesn't toggle.
        if self.frequency == 0 {
            self.polarity = true;
            return self.get_output();
        }

        // Counter counts down
        if self.counter == 0 {
            // Reload counter
            self.counter = self.frequency;
            // Flip polarity
            self.polarity = !self.polarity;
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

        // Output square wave
        if self.polarity {
            volume
        } else {
            -volume
        }
    }

    /// Reset the channel to initial state
    pub fn reset(&mut self) {
        self.frequency = 0;
        self.attenuation = 15; // Muted
        self.counter = 0;
        self.polarity = false;
    }

    /// Get current state for debugging
    pub fn get_state(&self, channel_id: u32) -> ChannelState {
        let freq_val = if self.frequency == 0 {
            1024
        } else {
            self.frequency
        };

        ChannelState {
            channel_id,
            enabled: self.attenuation < 15,
            muted: self.muted,
            frequency: self.get_frequency_hz(),
            frequency_raw: self.frequency,
            period: freq_val as u32 * 32, // In clock cycles
            volume: self.get_volume(),
            attenuation: self.attenuation,
            pan: 0.0, // PSG is mono
            channel_type: ChannelType::Tone,
            envelope: None,
            current_output: self.get_output(),
            noise_params: None,
        }
    }
}

impl ToneChannel {
    pub fn frequency(&self) -> u16 {
        self.frequency
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

    pub fn restore_state(&mut self, frequency: u16, attenuation: u8, counter: u16, polarity: bool) {
        self.frequency = frequency;
        self.attenuation = attenuation;
        self.counter = counter;
        self.polarity = polarity;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CLOCK: u32 = 3_579_545; // Genesis master clock / 15

    #[test]
    fn test_tone_channel_creation() {
        let channel = ToneChannel::new(TEST_CLOCK);
        assert_eq!(channel.get_frequency(), 0);
        assert_eq!(channel.get_attenuation(), 15); // Start muted
        assert!(!channel.muted);
    }

    #[test]
    fn test_set_frequency() {
        let mut channel = ToneChannel::new(TEST_CLOCK);

        channel.set_frequency(253);
        assert_eq!(channel.get_frequency(), 253);

        // Test masking to 10 bits
        channel.set_frequency(0xFFFF);
        assert_eq!(channel.get_frequency(), 0x3FF);
    }

    #[test]
    fn test_frequency_hz_calculation() {
        let mut channel = ToneChannel::new(TEST_CLOCK);

        // Test A440 (approximately)
        // PSG freq = Clock / (32 * Hz)
        // 253 = 3579545 / (32 * 440) ~ 441.9 Hz
        channel.set_frequency(253);
        let freq_hz = channel.get_frequency_hz();
        assert!((freq_hz - 441.9).abs() < 1.0);
    }

    #[test]
    fn test_frequency_zero_treated_as_1024() {
        let mut channel = ToneChannel::new(TEST_CLOCK);

        channel.set_frequency(0);
        let expected_hz = TEST_CLOCK as f32 / (32.0 * 1024.0);
        assert!((channel.get_frequency_hz() - expected_hz).abs() < 0.1);
    }

    #[test]
    fn test_set_attenuation() {
        let mut channel = ToneChannel::new(TEST_CLOCK);

        channel.set_attenuation(0);
        assert_eq!(channel.get_attenuation(), 0);
        assert_eq!(channel.get_volume(), 1.0);

        channel.set_attenuation(15);
        assert_eq!(channel.get_attenuation(), 15);
        assert_eq!(channel.get_volume(), 0.0);

        // Test masking to 4 bits
        channel.set_attenuation(0xFF);
        assert_eq!(channel.get_attenuation(), 0x0F);
    }

    #[test]
    fn test_volume_calculation() {
        let mut channel = ToneChannel::new(TEST_CLOCK);

        // Attenuation 0 = max volume
        channel.set_attenuation(0);
        assert_eq!(channel.get_volume(), 1.0);

        // Attenuation 1 ~ 0.794
        channel.set_attenuation(1);
        let vol = channel.get_volume();
        assert!((vol - 0.794328).abs() < 0.001);

        // Attenuation 15 = silence
        channel.set_attenuation(15);
        assert_eq!(channel.get_volume(), 0.0);
    }

    #[test]
    fn test_clock_generates_square_wave() {
        let mut channel = ToneChannel::new(TEST_CLOCK);

        // Set frequency to 4 (the counter is loaded with 4 and counts down)
        channel.set_frequency(4);
        channel.set_attenuation(0); // Max volume

        // Clock 4 times: the counter runs down to 0, the polarity holds
        let out1 = channel.clock();
        let out2 = channel.clock();
        let out3 = channel.clock();
        let out4 = channel.clock();

        // All should be same polarity
        assert_eq!(out1, out2);
        assert_eq!(out2, out3);
        assert_eq!(out3, out4);

        // The fifth clock finds the counter at 0: it reloads and flips
        let out5 = channel.clock();
        assert_ne!(out4, out5);
        assert_eq!(out5, -out4);
    }

    #[test]
    fn test_mute() {
        let mut channel = ToneChannel::new(TEST_CLOCK);
        channel.set_frequency(100);
        channel.set_attenuation(0);

        // Not muted
        channel.clock();
        let output = channel.get_output();
        assert_ne!(output, 0.0);

        // Muted
        channel.muted = true;
        assert_eq!(channel.get_output(), 0.0);
        assert_eq!(channel.clock(), 0.0);
    }

    #[test]
    fn test_reset() {
        let mut channel = ToneChannel::new(TEST_CLOCK);

        channel.set_frequency(440);
        channel.set_attenuation(5);

        channel.reset();

        assert_eq!(channel.get_frequency(), 0);
        assert_eq!(channel.get_attenuation(), 15);
        assert_eq!(channel.counter, 0);
        assert!(!channel.polarity);
    }

    #[test]
    fn test_get_state() {
        let mut channel = ToneChannel::new(TEST_CLOCK);
        channel.set_frequency(253);
        channel.set_attenuation(3);

        let state = channel.get_state(0);

        assert_eq!(state.channel_id, 0);
        assert!(state.enabled); // Attenuation < 15
        assert!(!state.muted);
        assert_eq!(state.frequency_raw, 253);
        assert_eq!(state.attenuation, 3);
        assert_eq!(state.channel_type, ChannelType::Tone);
        assert!(state.noise_params.is_none());
    }
}
