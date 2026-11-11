//! Tests for audio traits and types.

use retro_core::audio::*;
use retro_core::cpu::Clockable;

// ===== DUMMY AUDIO CHIP FOR TESTING =====

struct DummyAudioChip {
    channels: u32,
    output_level: f32,
    registers: [u8; 256],
    sample_rate: u32,
}

impl DummyAudioChip {
    fn new(channels: u32, sample_rate: u32) -> Self {
        Self {
            channels,
            output_level: 0.0,
            registers: [0; 256],
            sample_rate,
        }
    }
}

impl Clockable for DummyAudioChip {
    fn clock(&mut self, _cycles: u32) -> u32 {
        // Dummy implementation
        0
    }
}

impl AudioChip for DummyAudioChip {
    fn chip_name(&self) -> &'static str {
        "DUMMY-01"
    }

    fn get_info(&self) -> AudioChipInfo {
        AudioChipInfo {
            name: "DUMMY-01",
            manufacturer: "Test Corp",
            channels: self.channels,
            sample_rate: self.sample_rate,
            stereo: false,
            channel_types: vec![ChannelType::Tone; self.channels as usize],
        }
    }

    fn write_register(&mut self, address: u8, data: u8) {
        self.registers[address as usize] = data;
    }

    fn read_register(&mut self, address: u8) -> u8 {
        self.registers[address as usize]
    }

    fn generate_samples(&mut self, output: &mut [f32], _sample_rate: u32) -> usize {
        // Generate simple sine wave for testing
        for (i, sample) in output.iter_mut().enumerate() {
            let t = i as f32 / 100.0;
            *sample = (t * std::f32::consts::PI * 2.0).sin() * 0.5;
        }
        output.len()
    }

    fn reset(&mut self) {
        self.registers.fill(0);
        self.output_level = 0.0;
    }

    fn get_output_level(&self) -> f32 {
        self.output_level
    }
}

impl AudioDebugger for DummyAudioChip {
    fn get_channel_count(&self) -> u32 {
        self.channels
    }

    fn get_channel_state(&self, channel: u32) -> Option<ChannelState> {
        if channel >= self.channels {
            return None;
        }

        Some(ChannelState {
            channel_id: channel,
            enabled: true,
            muted: false,
            frequency: 440.0,
            frequency_raw: 256,
            period: 1000,
            volume: 1.0,
            attenuation: 0,
            pan: 0.0,
            channel_type: ChannelType::Tone,
            envelope: None,
            current_output: 0.5,
            noise_params: None,
        })
    }

    fn mute_channel(&mut self, _channel: u32, _muted: bool) {
        // Dummy implementation
    }

    fn get_waveform(&self, _channel: u32, samples: u32) -> Vec<f32> {
        vec![0.0; samples as usize]
    }

    fn get_output_waveform(&self, samples: u32) -> Vec<f32> {
        vec![0.0; samples as usize]
    }
}

// ===== TESTS =====

#[cfg(test)]
mod tests {
    use super::*;

    // Test 1: AudioChip trait basic functionality
    #[test]
    fn test_audio_chip_creation() {
        let chip = DummyAudioChip::new(4, 44100);
        assert_eq!(chip.chip_name(), "DUMMY-01");
        assert_eq!(chip.get_output_level(), 0.0);
    }

    #[test]
    fn test_audio_chip_info() {
        let chip = DummyAudioChip::new(3, 48000);
        let info = chip.get_info();

        assert_eq!(info.name, "DUMMY-01");
        assert_eq!(info.manufacturer, "Test Corp");
        assert_eq!(info.channels, 3);
        assert_eq!(info.sample_rate, 48000);
        assert!(!info.stereo);
        assert_eq!(info.channel_types.len(), 3);
    }

    #[test]
    fn test_audio_chip_registers() {
        let mut chip = DummyAudioChip::new(4, 44100);

        // Write to register
        chip.write_register(0x10, 0xFF);
        assert_eq!(chip.read_register(0x10), 0xFF);

        // Write to another register
        chip.write_register(0x20, 0xAA);
        assert_eq!(chip.read_register(0x20), 0xAA);

        // Verify first register is still the same
        assert_eq!(chip.read_register(0x10), 0xFF);
    }

    #[test]
    fn test_audio_chip_reset() {
        let mut chip = DummyAudioChip::new(4, 44100);

        // Write some registers
        chip.write_register(0x10, 0xFF);
        chip.write_register(0x20, 0xAA);

        // Reset
        chip.reset();

        // Verify registers are cleared
        assert_eq!(chip.read_register(0x10), 0x00);
        assert_eq!(chip.read_register(0x20), 0x00);
    }

    #[test]
    fn test_audio_chip_sample_generation() {
        let mut chip = DummyAudioChip::new(4, 44100);
        let mut output = vec![0.0; 1024];

        let samples_generated = chip.generate_samples(&mut output, 44100);

        assert_eq!(samples_generated, 1024);

        // Verify samples are not all zero
        let non_zero = output.iter().any(|&s| s != 0.0);
        assert!(non_zero, "Generated samples should not all be zero");
    }

    // Test 2: AudioDebugger trait
    #[test]
    fn test_audio_debugger_channel_count() {
        let chip = DummyAudioChip::new(3, 44100);
        assert_eq!(chip.get_channel_count(), 3);
    }

    #[test]
    fn test_audio_debugger_channel_state() {
        let chip = DummyAudioChip::new(4, 44100);

        // Valid channel
        let state = chip.get_channel_state(0);
        assert!(state.is_some());

        let state = state.unwrap();
        assert_eq!(state.channel_id, 0);
        assert_eq!(state.channel_type, ChannelType::Tone);
        assert!(state.enabled);

        // Invalid channel
        let state = chip.get_channel_state(10);
        assert!(state.is_none());
    }

    #[test]
    fn test_audio_debugger_waveform() {
        let chip = DummyAudioChip::new(4, 44100);

        let waveform = chip.get_waveform(0, 256);
        assert_eq!(waveform.len(), 256);

        let mixed = chip.get_output_waveform(512);
        assert_eq!(mixed.len(), 512);
    }

    // Test 3: SimpleMixer
    #[test]
    fn test_simple_mixer_creation() {
        let mixer = SimpleMixer::new();
        assert_eq!(mixer.get_master_volume(), 1.0);
    }

    #[test]
    fn test_simple_mixer_master_volume() {
        let mut mixer = SimpleMixer::new();

        mixer.set_master_volume(0.5);
        assert_eq!(mixer.get_master_volume(), 0.5);

        // Test clamping
        mixer.set_master_volume(1.5);
        assert_eq!(mixer.get_master_volume(), 1.0);

        mixer.set_master_volume(-0.5);
        assert_eq!(mixer.get_master_volume(), 0.0);
    }

    #[test]
    fn test_simple_mixer_add_chip() {
        let mut mixer = SimpleMixer::new();

        let chip1 = Box::new(DummyAudioChip::new(3, 44100));
        let chip2 = Box::new(DummyAudioChip::new(4, 44100));

        mixer.add_chip(chip1);
        mixer.add_chip(chip2);

        // Mixer should now have 2 chips
        // (We can't easily test this directly without exposing internal state,
        // but we can test mixing behavior)
    }

    #[test]
    fn test_simple_mixer_empty() {
        let mut mixer = SimpleMixer::new();
        let mut output = vec![0.0; 1024];

        // Mix with no chips should produce silence
        let samples = mixer.mix_samples(&mut output, 44100);

        assert_eq!(samples, 1024);
        assert!(output.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn test_simple_mixer_single_chip() {
        let mut mixer = SimpleMixer::new();
        let chip = Box::new(DummyAudioChip::new(4, 44100));

        mixer.add_chip(chip);
        mixer.set_master_volume(1.0);

        let mut output = vec![0.0; 1024];
        mixer.mix_samples(&mut output, 44100);

        // Should have non-zero samples
        let non_zero = output.iter().any(|&s| s != 0.0);
        assert!(non_zero);
    }

    #[test]
    fn test_simple_mixer_volume_scaling() {
        let mut mixer = SimpleMixer::new();
        let chip = Box::new(DummyAudioChip::new(4, 44100));

        mixer.add_chip(chip);

        // Generate at full volume
        mixer.set_master_volume(1.0);
        let mut output_full = vec![0.0; 1024];
        mixer.mix_samples(&mut output_full, 44100);

        // Generate at half volume
        mixer.set_master_volume(0.5);
        let mut output_half = vec![0.0; 1024];
        mixer.mix_samples(&mut output_half, 44100);

        // Half volume samples should be roughly half of full volume
        // (allowing for some floating point error)
        for (full, half) in output_full.iter().zip(output_half.iter()) {
            if *full != 0.0 {
                let ratio = half / full;
                assert!(
                    (ratio - 0.5).abs() < 0.01,
                    "Volume scaling incorrect: {} vs {}",
                    full,
                    half
                );
            }
        }
    }

    #[test]
    fn test_simple_mixer_clipping() {
        let mut mixer = SimpleMixer::new();

        // Add two chips (their outputs will be added)
        let chip1 = Box::new(DummyAudioChip::new(4, 44100));
        let chip2 = Box::new(DummyAudioChip::new(4, 44100));

        mixer.add_chip(chip1);
        mixer.add_chip(chip2);
        mixer.set_master_volume(1.0);

        let mut output = vec![0.0; 1024];
        mixer.mix_samples(&mut output, 44100);

        // All samples should be clamped to [-1.0, 1.0]
        for &sample in &output {
            assert!(
                (-1.0..=1.0).contains(&sample),
                "Sample out of range: {}",
                sample
            );
        }
    }

    // Test 4: ChannelState serialization
    #[test]
    fn test_channel_state_serialization() {
        let state = ChannelState {
            channel_id: 0,
            enabled: true,
            muted: false,
            frequency: 440.0,
            frequency_raw: 253,
            period: 1000,
            volume: 0.8,
            attenuation: 2,
            pan: 0.0,
            channel_type: ChannelType::Tone,
            envelope: None,
            current_output: 0.5,
            noise_params: None,
        };

        // Serialize to JSON
        let json = serde_json::to_string(&state).unwrap();
        assert!(!json.is_empty());

        // Deserialize back
        let deserialized: ChannelState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.channel_id, state.channel_id);
        assert_eq!(deserialized.frequency, state.frequency);
        assert_eq!(deserialized.volume, state.volume);
    }

    // Test 5: NoiseParams
    #[test]
    fn test_noise_params() {
        let noise = NoiseParams {
            shift_rate: NoiseShiftRate::N1024,
            mode: NoiseMode::White,
            lfsr_state: 0x8000,
        };

        let state = ChannelState {
            channel_id: 3,
            enabled: true,
            muted: false,
            frequency: 0.0,
            frequency_raw: 0,
            period: 0,
            volume: 1.0,
            attenuation: 0,
            pan: 0.0,
            channel_type: ChannelType::Noise,
            envelope: None,
            current_output: 0.0,
            noise_params: Some(noise),
        };

        assert!(state.noise_params.is_some());
        let params = state.noise_params.unwrap();
        assert_eq!(params.mode, NoiseMode::White);
        assert_eq!(params.shift_rate, NoiseShiftRate::N1024);
    }

    // Test 6: AudioChipInfo creation and serialization
    #[test]
    fn test_audio_chip_info_serialization() {
        let info = AudioChipInfo {
            name: "TEST-CHIP",
            manufacturer: "Test Corp",
            channels: 4,
            sample_rate: 44100,
            stereo: true,
            channel_types: vec![
                ChannelType::Tone,
                ChannelType::Tone,
                ChannelType::Tone,
                ChannelType::Noise,
            ],
        };

        // Test serialization
        let json = serde_json::to_string(&info).unwrap();
        assert!(!json.is_empty());
        assert!(json.contains("TEST-CHIP"));
        assert!(json.contains("Test Corp"));

        // Verify fields
        assert_eq!(info.name, "TEST-CHIP");
        assert_eq!(info.manufacturer, "Test Corp");
        assert_eq!(info.channels, 4);
        assert_eq!(info.sample_rate, 44100);
        assert!(info.stereo);
        assert_eq!(info.channel_types.len(), 4);
    }
}
