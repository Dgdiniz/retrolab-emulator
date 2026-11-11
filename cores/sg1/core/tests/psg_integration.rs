//! Integration tests for PSG (SN76489)
//!
//! These tests validate the PSG from an external perspective,
//! testing complete functionality including:
//! - Melody playback
//! - Noise generation
//! - Multi-channel mixing
//! - Volume control
//! - Frequency sweeps

use retro_core::audio::{AudioChip, AudioDebugger};
use retro_core::cpu::Clockable;
use sg1_core::psg::Psg;

const TEST_CLOCK: u32 = 3_579_545;
const TEST_SAMPLE_RATE: u32 = 44100;

// Helper: Calculate PSG frequency value from Hz
fn hz_to_psg_freq(hz: f32, clock: u32) -> u16 {
    let freq = clock as f32 / (32.0 * hz);
    freq as u16
}

// Helper: Write frequency to channel
fn write_tone_frequency(psg: &mut Psg, channel: u8, freq_value: u16) {
    // Latch + channel + low 4 bits
    psg.write(0x80 | (channel << 5) | ((freq_value & 0x0F) as u8));
    // High 6 bits
    psg.write(((freq_value >> 4) & 0x3F) as u8);
}

// Helper: Write volume to channel
fn write_tone_volume(psg: &mut Psg, channel: u8, attenuation: u8) {
    // Latch + volume + channel + attenuation
    psg.write(0x90 | (channel << 5) | (attenuation & 0x0F));
}

// Helper: Write noise control
fn write_noise_control(psg: &mut Psg, control: u8) {
    // Latch + noise + control
    psg.write(0xE0 | (control & 0x07));
}

// Helper: Write noise volume
fn write_noise_volume(psg: &mut Psg, attenuation: u8) {
    // Latch + volume + noise + attenuation
    psg.write(0xF0 | (attenuation & 0x0F));
}

// CRITICAL HELPER: Clock PSG before generating samples
// generate_samples() only works if cycles have been accumulated via clock()
fn clock_and_generate(psg: &mut Psg, output: &mut [f32], sample_rate: u32) -> usize {
    // Calculate cycles needed for requested samples
    // cycles_per_sample = clock_freq / sample_rate
    // total_cycles = cycles_per_sample * sample_count
    let cycles_needed = ((TEST_CLOCK as f32 / sample_rate as f32) * output.len() as f32) as u32;

    // Add some buffer to ensure enough cycles
    psg.clock(cycles_needed + 1000);

    // Now generate samples
    psg.generate_samples(output, sample_rate)
}

#[test]
fn test_integration_melody_playback() {
    let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

    // Define a simple melody: C4, E4, G4, C5 (C major arpeggio)
    let notes = [
        (261.63, 0.25), // C4, 1/4 second
        (329.63, 0.25), // E4
        (392.00, 0.25), // G4
        (523.25, 0.25), // C5
    ];

    for (hz, duration) in notes {
        let freq_value = hz_to_psg_freq(hz, TEST_CLOCK);

        // Write frequency to channel 0
        write_tone_frequency(&mut psg, 0, freq_value);
        // Set volume to max
        write_tone_volume(&mut psg, 0, 0);

        // Generate samples for this note
        let sample_count = (TEST_SAMPLE_RATE as f32 * duration) as usize;
        let mut samples = vec![0.0; sample_count];
        clock_and_generate(&mut psg, &mut samples, TEST_SAMPLE_RATE);

        // Verify we got audio output
        // PSG divides by 4 channels, so max for 1 channel is ~0.25
        let max_amplitude = samples.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(
            max_amplitude > 0.15,
            "Note at {} Hz should produce significant output (got {})",
            hz,
            max_amplitude
        );

        // Verify channel state
        let state = psg.get_channel_state(0).unwrap();
        assert!(state.enabled);
        assert!(!state.muted);
        assert_eq!(state.attenuation, 0);
    }
}

#[test]
fn test_integration_chord_playback() {
    let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

    // Play C major chord: C (261.63 Hz), E (329.63 Hz), G (392.00 Hz)
    let chord_notes = [(261.63, 0), (329.63, 1), (392.00, 2)];

    for (hz, channel) in chord_notes {
        let freq_value = hz_to_psg_freq(hz, TEST_CLOCK);
        write_tone_frequency(&mut psg, channel, freq_value);
        write_tone_volume(&mut psg, channel, 0); // Max volume
    }

    // Generate samples (chord playing)
    let mut samples = vec![0.0; TEST_SAMPLE_RATE as usize / 2]; // 0.5 seconds
    clock_and_generate(&mut psg, &mut samples, TEST_SAMPLE_RATE);

    // Verify we got mixed output
    let max_amplitude = samples.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    assert!(
        max_amplitude > 0.15,
        "Chord should produce significant output"
    );

    // Verify all channels are active
    for channel in 0..3 {
        let state = psg.get_channel_state(channel).unwrap();
        assert!(state.enabled);
        assert_eq!(state.attenuation, 0);
    }
}

#[test]
fn test_integration_volume_fade() {
    let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

    // Set up a tone
    let freq_value = hz_to_psg_freq(440.0, TEST_CLOCK);
    write_tone_frequency(&mut psg, 0, freq_value);

    // Fade from max volume (0) to silence (15)
    for attenuation in 0..=15 {
        write_tone_volume(&mut psg, 0, attenuation);

        // Generate a small chunk of samples
        let mut samples = vec![0.0; 1024];
        clock_and_generate(&mut psg, &mut samples, TEST_SAMPLE_RATE);

        // Verify volume decreases
        let max_amplitude = samples.iter().fold(0.0f32, |a, &b| a.max(b.abs()));

        if attenuation == 0 {
            // Max volume: about 0.25, since the PSG divides by its 4 channels
            assert!(max_amplitude > 0.2, "Max volume should be high");
        } else if attenuation == 15 {
            // Silence should be 0.0
            assert!(max_amplitude < 0.01, "Silence should be near zero");
        }

        // Verify channel state
        let state = psg.get_channel_state(0).unwrap();
        assert_eq!(state.attenuation, attenuation);
    }
}

#[test]
fn test_integration_frequency_sweep() {
    let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

    // Sweep from 220 Hz to 880 Hz (2 octaves)
    let start_hz = 220.0;
    let end_hz = 880.0;
    let steps = 10;

    for step in 0..steps {
        let t = step as f32 / (steps - 1) as f32;
        let hz = start_hz + (end_hz - start_hz) * t;
        let freq_value = hz_to_psg_freq(hz, TEST_CLOCK);

        write_tone_frequency(&mut psg, 0, freq_value);
        write_tone_volume(&mut psg, 0, 0);

        // Generate samples
        let mut samples = vec![0.0; 2048];
        clock_and_generate(&mut psg, &mut samples, TEST_SAMPLE_RATE);

        // Verify output
        let max_amplitude = samples.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(
            max_amplitude > 0.15,
            "Sweep step {} should produce output",
            step
        );

        // Verify frequency is approximately correct
        let state = psg.get_channel_state(0).unwrap();
        let actual_hz = state.frequency;
        assert!(
            (actual_hz - hz).abs() < 5.0,
            "Frequency mismatch at step {}: expected {}, got {}",
            step,
            hz,
            actual_hz
        );
    }
}

#[test]
fn test_integration_white_noise() {
    let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

    // CRITICAL: When noise uses Tone3 rate (0x07), Tone2 must be configured first!
    // Configure Tone2 to provide clock for noise
    write_tone_frequency(&mut psg, 2, 30); // Fast frequency for noise

    // Configure white noise
    write_noise_control(&mut psg, 0x07); // Tone3 rate, White mode
    write_noise_volume(&mut psg, 0); // Max volume

    // Generate samples
    let mut samples = vec![0.0; TEST_SAMPLE_RATE as usize / 4]; // 0.25 seconds
    clock_and_generate(&mut psg, &mut samples, TEST_SAMPLE_RATE);

    // Verify we got noise output
    let max_amplitude = samples.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    assert!(max_amplitude > 0.15, "White noise should produce output");

    // Verify noise has variation (not constant)
    let first = samples[0];
    let has_variation = samples.iter().any(|&s| (s - first).abs() > 0.1);
    assert!(has_variation, "White noise should vary");

    // Verify channel state
    let state = psg.get_channel_state(3).unwrap();
    assert!(state.enabled);
    assert!(state.noise_params.is_some());
    let params = state.noise_params.unwrap();
    assert_eq!(params.mode, retro_core::audio::NoiseMode::White);
}

#[test]
fn test_integration_periodic_noise() {
    let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

    // Configure periodic noise
    write_noise_control(&mut psg, 0x02); // N2048, Periodic mode
    write_noise_volume(&mut psg, 0);

    // Generate samples
    let mut samples = vec![0.0; TEST_SAMPLE_RATE as usize / 4];
    clock_and_generate(&mut psg, &mut samples, TEST_SAMPLE_RATE);

    // Verify output
    let max_amplitude = samples.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    assert!(max_amplitude > 0.15, "Periodic noise should produce output");

    // Verify channel state
    let state = psg.get_channel_state(3).unwrap();
    let params = state.noise_params.unwrap();
    assert_eq!(params.mode, retro_core::audio::NoiseMode::Periodic);
}

#[test]
fn test_integration_all_channels_simultaneous() {
    let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

    // Set up all 3 tone channels with different frequencies
    let freqs = [440.0, 554.37, 659.25]; // A4, C#5, E5 (A major chord)
    for (channel, hz) in freqs.iter().enumerate() {
        let freq_value = hz_to_psg_freq(*hz, TEST_CLOCK);
        write_tone_frequency(&mut psg, channel as u8, freq_value);
        write_tone_volume(&mut psg, channel as u8, 0);
    }

    // Add noise
    write_noise_control(&mut psg, 0x04); // N512, White
    write_noise_volume(&mut psg, 0);

    // Generate samples
    let mut samples = vec![0.0; TEST_SAMPLE_RATE as usize / 2]; // 0.5 seconds
    clock_and_generate(&mut psg, &mut samples, TEST_SAMPLE_RATE);

    // Verify mixed output
    let max_amplitude = samples.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    assert!(
        max_amplitude > 0.15,
        "All channels should produce mixed output"
    );

    // Verify all channels are active
    for channel in 0..4 {
        let state = psg.get_channel_state(channel).unwrap();
        assert!(state.enabled);
        assert!(!state.muted);
        assert_eq!(state.attenuation, 0);
    }
}

#[test]
fn test_integration_channel_muting() {
    let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

    // Set up all channels
    for channel in 0..3 {
        let freq_value = hz_to_psg_freq(440.0 * (channel + 1) as f32, TEST_CLOCK);
        write_tone_frequency(&mut psg, channel, freq_value);
        write_tone_volume(&mut psg, channel, 0);
    }

    // Generate with all channels
    let mut samples_all = vec![0.0; 1024];
    clock_and_generate(&mut psg, &mut samples_all, TEST_SAMPLE_RATE);
    let amplitude_all = samples_all.iter().fold(0.0f32, |a, &b| a.max(b.abs()));

    // Mute channel 0
    psg.mute_channel(0, true);

    // Generate with one muted
    let mut samples_muted = vec![0.0; 1024];
    clock_and_generate(&mut psg, &mut samples_muted, TEST_SAMPLE_RATE);
    let amplitude_muted = samples_muted.iter().fold(0.0f32, |a, &b| a.max(b.abs()));

    // Amplitude should decrease when channel is muted
    assert!(
        amplitude_muted < amplitude_all,
        "Muting channel should reduce amplitude"
    );

    // Unmute
    psg.mute_channel(0, false);

    // Generate again
    let mut samples_unmuted = vec![0.0; 1024];
    clock_and_generate(&mut psg, &mut samples_unmuted, TEST_SAMPLE_RATE);
    let amplitude_unmuted = samples_unmuted.iter().fold(0.0f32, |a, &b| a.max(b.abs()));

    // Amplitude should increase again
    assert!(
        amplitude_unmuted > amplitude_muted,
        "Unmuting should restore amplitude"
    );
}

#[test]
fn test_integration_waveform_retrieval() {
    let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

    // Set up a tone
    let freq_value = hz_to_psg_freq(440.0, TEST_CLOCK);
    write_tone_frequency(&mut psg, 0, freq_value);
    write_tone_volume(&mut psg, 0, 0);

    // Generate some samples to populate waveform buffer
    let mut samples = vec![0.0; 4096];
    clock_and_generate(&mut psg, &mut samples, TEST_SAMPLE_RATE);

    // Retrieve waveform
    let waveform = psg.get_waveform(0, 256);
    assert!(!waveform.is_empty(), "Should have waveform data");

    // Verify waveform has content
    let has_positive = waveform.iter().any(|&x| x > 0.0);
    let has_negative = waveform.iter().any(|&x| x < 0.0);
    assert!(
        has_positive && has_negative,
        "Waveform should have square wave shape"
    );

    // Get mixed output waveform
    let mixed = psg.get_output_waveform(256);
    assert!(!mixed.is_empty(), "Should have mixed waveform data");
}

#[test]
fn test_integration_rapid_register_changes() {
    let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

    // Rapidly change frequency and volume
    for _ in 0..10 {
        let freq1 = hz_to_psg_freq(440.0, TEST_CLOCK);
        write_tone_frequency(&mut psg, 0, freq1);
        write_tone_volume(&mut psg, 0, 0);

        let freq2 = hz_to_psg_freq(880.0, TEST_CLOCK);
        write_tone_frequency(&mut psg, 0, freq2);
        write_tone_volume(&mut psg, 0, 7);

        // Generate a small chunk
        let mut samples = vec![0.0; 128];
        clock_and_generate(&mut psg, &mut samples, TEST_SAMPLE_RATE);
    }

    // PSG should still be stable
    let state = psg.get_channel_state(0).unwrap();
    assert_eq!(state.attenuation, 7);
}

#[test]
fn test_integration_silence_to_sound_transition() {
    let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

    // Start silent (attenuation = 15)
    let freq_value = hz_to_psg_freq(440.0, TEST_CLOCK);
    write_tone_frequency(&mut psg, 0, freq_value);
    write_tone_volume(&mut psg, 0, 15);

    // Generate silent samples
    let mut samples_silent = vec![0.0; 1024];
    clock_and_generate(&mut psg, &mut samples_silent, TEST_SAMPLE_RATE);
    let amplitude_silent = samples_silent.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    assert!(amplitude_silent < 0.01, "Should be silent");

    // Transition to max volume
    write_tone_volume(&mut psg, 0, 0);

    // Generate loud samples
    let mut samples_loud = vec![0.0; 1024];
    clock_and_generate(&mut psg, &mut samples_loud, TEST_SAMPLE_RATE);
    let amplitude_loud = samples_loud.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    // PSG divides by 4 channels, so max is ~0.25
    assert!(
        amplitude_loud > 0.2,
        "Should be loud (got {})",
        amplitude_loud
    );
}

#[test]
fn test_integration_noise_shift_rates() {
    let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

    // CRITICAL: Configure Tone2 for shift_rate 0x03 (Tone3) to avoid slow period
    write_tone_frequency(&mut psg, 2, 30); // Fast frequency for noise

    // Test each shift rate
    let shift_rates = [0x00, 0x01, 0x02, 0x03]; // N512, N1024, N2048, Tone3

    for &rate in &shift_rates {
        write_noise_control(&mut psg, rate | 0x04); // White mode
        write_noise_volume(&mut psg, 0);

        // Generate samples
        let mut samples = vec![0.0; 2048];
        clock_and_generate(&mut psg, &mut samples, TEST_SAMPLE_RATE);

        // Verify output
        let max_amplitude = samples.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(
            max_amplitude > 0.15,
            "Shift rate {:02X} should produce output",
            rate
        );
    }
}

#[test]
fn test_integration_reset() {
    let mut psg = Psg::new(TEST_CLOCK, TEST_SAMPLE_RATE);

    // Set up channels
    for channel in 0..3 {
        write_tone_frequency(&mut psg, channel, 100);
        write_tone_volume(&mut psg, channel, 0);
    }
    // CRITICAL: Configure Tone2 (channel 2) for noise that uses Tone3 rate
    write_tone_frequency(&mut psg, 2, 30);
    write_noise_control(&mut psg, 0x07);
    write_noise_volume(&mut psg, 0);

    // Reset
    psg.reset();

    // All channels should be muted after reset
    for channel in 0..4 {
        let state = psg.get_channel_state(channel).unwrap();
        assert_eq!(
            state.attenuation, 15,
            "Channel {} should be muted after reset",
            channel
        );
    }

    // Generate samples should be silent
    let mut samples = vec![0.0; 1024];
    clock_and_generate(&mut psg, &mut samples, TEST_SAMPLE_RATE);
    let max_amplitude = samples.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    assert!(max_amplitude < 0.01, "Should be silent after reset");
}

#[test]
fn test_integration_sample_rate_change() {
    let mut psg = Psg::new(TEST_CLOCK, 44100);

    // Set up a tone
    write_tone_frequency(&mut psg, 0, hz_to_psg_freq(440.0, TEST_CLOCK));
    write_tone_volume(&mut psg, 0, 0);

    // Generate at 44100 Hz
    let mut samples1 = vec![0.0; 1024];
    clock_and_generate(&mut psg, &mut samples1, 44100);

    // Generate at 48000 Hz
    let mut samples2 = vec![0.0; 1024];
    clock_and_generate(&mut psg, &mut samples2, 48000);

    // Both should produce output
    let amp1 = samples1.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    let amp2 = samples2.iter().fold(0.0f32, |a, &b| a.max(b.abs()));

    assert!(amp1 > 0.15, "44100 Hz should produce output (got {})", amp1);
    assert!(amp2 > 0.15, "48000 Hz should produce output (got {})", amp2);
}
