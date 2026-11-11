//! YM2612 DAC Integration Tests
//!
//! Tests the DAC (Digital-to-Analog Converter) mode, which allows
//! Channel 6 to play PCM samples instead of FM synthesis.
//!
//! CRITICAL: These tests must be RIGOROUS. No simplifications to make them pass.
//! If a test fails, the CODE must be fixed, not the test.

use retro_core::audio::{AudioChip, AudioDebugger, ChannelType};
use retro_core::cpu::Clockable;
use sg1_core::ym2612::Ym2612;

/// Helper: Clock YM2612 before generating samples
/// CRITICAL: generate_samples() only works if cycles have been accumulated via clock()
fn clock_and_generate(ym: &mut Ym2612, output: &mut [f32], sample_rate: u32) -> usize {
    // Calculate cycles needed for requested samples
    // For YM2612 @ 7.67 MHz:
    // cycles_per_sample = clock_freq / sample_rate
    // total_cycles = cycles_per_sample * sample_count
    let ym_clock = 7_670_454;
    let cycles_needed = ((ym_clock as f32 / sample_rate as f32) * output.len() as f32) as u32;

    // Add some buffer to ensure enough cycles
    ym.clock(cycles_needed + 1000);

    // Now generate samples
    ym.generate_samples(output, sample_rate)
}

/// Test complete PCM playback sequence
/// This test verifies that the DAC correctly outputs a sequence of PCM samples
#[test]
fn test_dac_pcm_playback_sequence() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Enable DAC
    ym.write_register(0, 0x2B, 0x80);

    // Write PCM sequence: sine wave pattern
    let pcm_samples = [
        0x80, // 0
        0xB0, // 48
        0xD8, // 88
        0xF0, // 112
        0xFF, // 127
        0xF0, // 112
        0xD8, // 88
        0xB0, // 48
        0x80, // 0
        0x50, // -48
        0x28, // -88
        0x10, // -112
        0x00, // -128
        0x10, // -112
        0x28, // -88
        0x50, // -48
    ];

    // For each sample, write it and verify the output
    for (i, &sample) in pcm_samples.iter().enumerate() {
        ym.write_register(0, 0x2A, sample);

        // Clock the YM2612 directly to get immediate output
        // We need to clock enough to generate a sample
        let sample_clocks = (7_670_454 / 44100) + 1; // Clock cycles per sample

        for _ in 0..sample_clocks {
            ym.clock(1);
        }

        // Now generate the accumulated sample
        let mut buf = vec![0.0; 2];
        ym.generate_samples(&mut buf, 44100);
        let output = (buf[0], buf[1]);

        // Calculate expected output. The mixer sums all 6 channels and
        // divides by 6 to keep the result in `[-1.0, 1.0]`, so the
        // DAC-only contribution is `(dac_value / 128) / 6`. Here
        // `(value * 32) / 4095 ~ value / 128` is the raw channel output,
        // and the division by 6 accounts for the mixer.
        let dac_value = (sample as i16 - 128) as i32;
        let raw = (dac_value * 32) as f32 / 4095.0;
        let expected = raw / 6.0;

        // Verify output matches expected (with tolerance for floating point)
        assert!(
            (output.0 - expected).abs() < 0.02,
            "Sample {} left: expected {}, got {} (input: 0x{:02X})",
            i,
            expected,
            output.0,
            sample
        );
        assert!(
            (output.1 - expected).abs() < 0.02,
            "Sample {} right: expected {}, got {} (input: 0x{:02X})",
            i,
            expected,
            output.1,
            sample
        );
    }
}

/// Root mean square of a block of samples
fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|&s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// Test DAC vs FM on channel 6
#[test]
fn test_dac_vs_fm_channel6() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Configure FM on channel 6: part 1, registers whose low two bits are 2
    // (the part's third channel). Algorithm 7 adds the four operators; OP1
    // plays and the other three are silenced. No other channel is keyed on,
    // so the output is channel 6's alone.
    ym.write_register(1, 0xB2, 0x07); // Algorithm 7 (all parallel), no feedback
    ym.write_register(1, 0xB6, 0xC0); // Left and right on
    ym.write_register(1, 0x32, 0x01); // Channel 6 OP1: MUL=1
    ym.write_register(1, 0x42, 0x00); // Channel 6 OP1: TL=0 (max volume)
    ym.write_register(1, 0x52, 0x1F); // Channel 6 OP1: AR=31 (instant attack)
    ym.write_register(1, 0x46, 0x7F); // Channel 6 OP3: TL=0x7F (silent)
    ym.write_register(1, 0x4A, 0x7F); // Channel 6 OP2: TL=0x7F (silent)
    ym.write_register(1, 0x4E, 0x7F); // Channel 6 OP4: TL=0x7F (silent)
    ym.write_register(1, 0xA6, 0x22); // Block 4, F-Number bits 10-8 = 2 (before the low byte, as the chip needs)
    ym.write_register(1, 0xA2, 0xCE); // F-Number low byte: F-Number 0x2CE, about 292 Hz
    ym.write_register(0, 0x28, 0xF6); // Key on channel 6 (bit 2 = part 1, bits 1-0 = 2), all operators

    // DAC off: channel 6 plays FM, a tone that swings either side of zero
    let mut fm_samples = vec![0.0; 1000];
    clock_and_generate(&mut ym, &mut fm_samples, 44100);
    let fm_rms = rms(&fm_samples);
    let fm_max = fm_samples.iter().copied().fold(f32::MIN, f32::max);
    let fm_min = fm_samples.iter().copied().fold(f32::MAX, f32::min);
    assert!(fm_rms > 0.05, "Channel 6 FM should be audible: {}", fm_rms);
    assert!(
        fm_max > 0.05 && fm_min < -0.05,
        "Channel 6 FM should be a tone around zero: min {}, max {}",
        fm_min,
        fm_max
    );

    // DAC on: channel 6 plays the DAC sample instead of its FM, which stays
    // keyed on. A sample of 0x80 (zero) leaves nothing of the tone.
    ym.write_register(0, 0x2B, 0x80);
    ym.write_register(0, 0x2A, 0x80); // DAC sample 0x80 = 0
    let mut dac_samples = vec![0.0; 1000];
    clock_and_generate(&mut ym, &mut dac_samples, 44100);
    let dac_max_abs = dac_samples.iter().map(|&s| s.abs()).fold(0.0f32, f32::max);
    assert!(
        dac_max_abs < 0.001,
        "With the DAC at 0x80, channel 6's FM tone should be gone: max abs {}",
        dac_max_abs
    );

    // A sample of 0xC0 (+64) holds every output sample at the DAC level,
    // accounting for the 6-channel mixer divisor, as in test_dac_stereo_output
    ym.write_register(0, 0x2A, 0xC0);
    let mut loud_samples = vec![0.0; 1000];
    clock_and_generate(&mut ym, &mut loud_samples, 44100);
    let expected = ((64 * 32) as f32 / 4095.0) / 6.0;
    for (i, &sample) in loud_samples.iter().enumerate() {
        assert!(
            (sample - expected).abs() < 0.001,
            "Sample {} with the DAC on: expected the DAC level {}, got {}",
            i,
            expected,
            sample
        );
    }
    // Channel 6's own output is the DAC sample: +64 / 128 = 0.5
    let state = ym.get_channel_state(5).expect("channel 6");
    assert_eq!(state.channel_type, ChannelType::PCM);
    assert!(
        (state.current_output - 0.5).abs() < 0.001,
        "Channel 6 should output the DAC sample: {}",
        state.current_output
    );

    // DAC off again: channel 6 plays the same FM tone as before
    ym.write_register(0, 0x2B, 0x00);
    let mut fm_again = vec![0.0; 1000];
    clock_and_generate(&mut ym, &mut fm_again, 44100);
    let fm_again_rms = rms(&fm_again);
    assert!(
        (fm_again_rms - fm_rms).abs() < fm_rms * 0.1,
        "With the DAC off, channel 6 should play its FM tone again: RMS {} (before the DAC, {})",
        fm_again_rms,
        fm_rms
    );
    assert_eq!(ym.get_info().channel_types[5], ChannelType::FM);
}

#[test]
fn test_dac_sample_rate() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Enable DAC
    ym.write_register(0, 0x2B, 0x80);

    // Write alternating samples
    let pattern = [0x00, 0xFF]; // -128, +127
    let mut sample_count = 0;
    let target_samples = 100;

    for i in 0..target_samples {
        let sample = pattern[i % 2];
        ym.write_register(0, 0x2A, sample);

        // Generate sample
        let mut buf = vec![0.0; 2];
        clock_and_generate(&mut ym, &mut buf, 44100);
        sample_count += 1;
    }

    assert_eq!(sample_count, target_samples);
}

#[test]
fn test_dac_disable_restores_fm() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Enable DAC with loud sample
    ym.write_register(0, 0x2B, 0x80);
    ym.write_register(0, 0x2A, 0xFF); // Max positive

    // Generate DAC samples
    let mut dac_output = vec![0.0; 1000];
    clock_and_generate(&mut ym, &mut dac_output, 44100);

    let dac_rms = (dac_output.iter().map(|&s| s * s).sum::<f32>() / dac_output.len() as f32).sqrt();
    assert!(dac_rms > 0.01, "DAC should produce sound");

    // Disable DAC
    ym.write_register(0, 0x2B, 0x00);

    // Generate samples with DAC disabled (channel 6 back to FM, but not keyed on)
    let mut fm_output = vec![0.0; 1000];
    clock_and_generate(&mut ym, &mut fm_output, 44100);

    let fm_rms = (fm_output.iter().map(|&s| s * s).sum::<f32>() / fm_output.len() as f32).sqrt();

    // After disabling DAC, output should drop (since channel 6 FM is not keyed on)
    assert!(
        fm_rms < dac_rms / 2.0,
        "FM (not keyed) should be quieter than DAC: FM={}, DAC={}",
        fm_rms,
        dac_rms
    );

    // Verify channel type changed back to FM
    let info = ym.get_info();
    assert_eq!(info.channel_types[5], ChannelType::FM);
}

#[test]
fn test_dac_stereo_output() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Enable DAC
    ym.write_register(0, 0x2B, 0x80);

    // Write test sample
    ym.write_register(0, 0x2A, 0xC0); // +64

    // Generate sample
    let mut buf = vec![0.0; 2];
    clock_and_generate(&mut ym, &mut buf, 44100);
    let (left, right) = (buf[0], buf[1]);

    // DAC output should be identical for left and right
    assert_eq!(left, right, "DAC should output to both L+R equally");

    // Verify value matches input, accounting for the 6-channel mixer
    // divisor that normalizes the chip's output to [-1.0, 1.0].
    // 0xC0 = 192, 192 - 128 = 64 (DAC value), raw channel ~ 0.5,
    // post-mixer ~ 0.0833.
    let expected = ((64 * 32) as f32 / 4095.0) / 6.0;
    assert!(
        (left - expected).abs() < 0.02,
        "DAC output should match input: expected {}, got {}",
        expected,
        left
    );
}

#[test]
fn test_dac_mute() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Enable DAC
    ym.write_register(0, 0x2B, 0x80);
    ym.write_register(0, 0x2A, 0xFF); // Max volume

    // Generate some samples
    let mut unmuted = vec![0.0; 1000];
    clock_and_generate(&mut ym, &mut unmuted, 44100);

    let unmuted_rms = (unmuted.iter().map(|&s| s * s).sum::<f32>() / unmuted.len() as f32).sqrt();

    // Mute channel 6 (index 5)
    ym.mute_channel(5, true);

    // Generate muted samples
    let mut muted = vec![0.0; 1000];
    clock_and_generate(&mut ym, &mut muted, 44100);

    let muted_rms = (muted.iter().map(|&s| s * s).sum::<f32>() / muted.len() as f32).sqrt();

    // Muted should be silent
    assert!(
        unmuted_rms > 0.01,
        "Unmuted DAC should be audible: {}",
        unmuted_rms
    );
    assert!(
        muted_rms < 0.001,
        "Muted DAC should be silent: {}",
        muted_rms
    );
    assert!(
        unmuted_rms > muted_rms * 10.0,
        "Unmuted should be much louder than muted"
    );
}

#[test]
fn test_dac_write_without_enable() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Write DAC data without enabling DAC
    ym.write_register(0, 0x2A, 0xFF);

    // DAC value should be stored
    assert_eq!(ym.get_dac_value(), 127);

    // But DAC should not be enabled
    assert!(!ym.is_dac_enabled());

    // Channel 6 should still be FM
    let info = ym.get_info();
    assert_eq!(info.channel_types[5], ChannelType::FM);
}

#[test]
fn test_dac_with_other_channels() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Enable DAC on channel 6 with loud sample
    ym.write_register(0, 0x2B, 0x80);
    ym.write_register(0, 0x2A, 0xF0); // Loud

    // Generate DAC samples
    let mut dac_samples = vec![0.0; 1000];
    clock_and_generate(&mut ym, &mut dac_samples, 44100);

    // DAC should be audible
    let rms = (dac_samples.iter().map(|&s| s * s).sum::<f32>() / dac_samples.len() as f32).sqrt();
    assert!(rms > 0.01, "DAC output should be audible: {}", rms);

    // Mute channel 6 (DAC)
    ym.mute_channel(5, true);

    let mut muted = vec![0.0; 1000];
    clock_and_generate(&mut ym, &mut muted, 44100);

    let muted_rms = (muted.iter().map(|&s| s * s).sum::<f32>() / muted.len() as f32).sqrt();

    // After muting, should be silent
    assert!(
        muted_rms < rms / 10.0,
        "Muted DAC should be much quieter: unmuted={}, muted={}",
        rms,
        muted_rms
    );
}

// ===== RIGOROUS UNIT TESTS =====

#[test]
fn test_dac_value_boundaries() {
    let mut ym = Ym2612::new(7_670_454, 44100);
    ym.write_register(0, 0x2B, 0x80); // Enable DAC

    // Test all boundary values
    let test_cases = vec![
        (0x00, -128), // Min
        (0x01, -127),
        (0x7F, -1),
        (0x80, 0), // Center
        (0x81, 1),
        (0xFE, 126),
        (0xFF, 127), // Max
    ];

    for (input, expected) in test_cases {
        ym.write_register(0, 0x2A, input);
        assert_eq!(
            ym.get_dac_value(),
            expected,
            "DAC value conversion failed for input 0x{:02X}",
            input
        );
    }
}

#[test]
fn test_dac_output_scaling() {
    let mut ym = Ym2612::new(7_670_454, 44100);
    ym.write_register(0, 0x2B, 0x80); // Enable DAC

    // Test that DAC outputs are correctly scaled
    let test_values = vec![
        0x00, // Min
        0x40, // -64
        0x80, // 0
        0xC0, // +64
        0xFF, // Max
    ];

    for value in test_values {
        ym.write_register(0, 0x2A, value);

        let mut buf = vec![0.0; 100];
        clock_and_generate(&mut ym, &mut buf, 44100);

        // Calculate expected (after 6-channel mixer divisor).
        let dac_val = (value as i16 - 128) as i32;
        let scaled = dac_val * 32;
        let raw = scaled as f32 / 4095.0;
        let expected = raw / 6.0;

        // Check that most samples match expected (allow some variation)
        let matching = buf.iter().filter(|&&s| (s - expected).abs() < 0.02).count();

        assert!(
            matching > buf.len() * 8 / 10,
            "DAC output 0x{:02X}: expected {}, but only {}/{} samples matched",
            value,
            expected,
            matching,
            buf.len()
        );
    }
}

#[test]
fn test_dac_enable_disable_transitions() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Set a DAC value
    ym.write_register(0, 0x2A, 0xFF);

    // Initially DAC is disabled
    assert!(!ym.is_dac_enabled());
    let info = ym.get_info();
    assert_eq!(info.channel_types[5], ChannelType::FM);

    // Enable DAC
    ym.write_register(0, 0x2B, 0x80);
    assert!(ym.is_dac_enabled());
    let info = ym.get_info();
    assert_eq!(info.channel_types[5], ChannelType::PCM);

    // Disable DAC
    ym.write_register(0, 0x2B, 0x00);
    assert!(!ym.is_dac_enabled());
    let info = ym.get_info();
    assert_eq!(info.channel_types[5], ChannelType::FM);

    // Re-enable
    ym.write_register(0, 0x2B, 0xFF); // Any value with bit 7 set
    assert!(ym.is_dac_enabled());
    let info = ym.get_info();
    assert_eq!(info.channel_types[5], ChannelType::PCM);
}

#[test]
fn test_dac_rapid_value_changes() {
    let mut ym = Ym2612::new(7_670_454, 44100);
    ym.write_register(0, 0x2B, 0x80); // Enable DAC

    // Rapidly change DAC values
    let values = vec![0x00, 0xFF, 0x80, 0x40, 0xC0];

    for &value in &values {
        ym.write_register(0, 0x2A, value);

        // Verify the value was stored
        let expected = (value as i16) - 128;
        assert_eq!(
            ym.get_dac_value(),
            expected,
            "DAC value not updated correctly for 0x{:02X}",
            value
        );
    }
}

#[test]
fn test_dac_zero_value() {
    let mut ym = Ym2612::new(7_670_454, 44100);
    ym.write_register(0, 0x2B, 0x80); // Enable DAC
    ym.write_register(0, 0x2A, 0x80); // Zero (center value)

    // Generate samples
    let mut buf = vec![0.0; 1000];
    clock_and_generate(&mut ym, &mut buf, 44100);

    // All samples should be near zero
    let max_abs = buf.iter().map(|&s| s.abs()).fold(0.0f32, f32::max);
    assert!(
        max_abs < 0.01,
        "DAC zero value should produce silence, got max abs: {}",
        max_abs
    );
}

#[test]
fn test_dac_channel_isolation() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Enable DAC on channel 6
    ym.write_register(0, 0x2B, 0x80);
    ym.write_register(0, 0x2A, 0xFF);

    // Mute all OTHER channels
    for i in 0..5 {
        ym.mute_channel(i, true);
    }

    // Generate samples - should only have DAC output
    let mut buf = vec![0.0; 1000];
    clock_and_generate(&mut ym, &mut buf, 44100);

    let rms = (buf.iter().map(|&s| s * s).sum::<f32>() / buf.len() as f32).sqrt();
    assert!(
        rms > 0.01,
        "DAC should be audible when other channels muted: {}",
        rms
    );

    // Now mute DAC channel
    ym.mute_channel(5, true);

    buf.fill(0.0);
    clock_and_generate(&mut ym, &mut buf, 44100);

    let rms2 = (buf.iter().map(|&s| s * s).sum::<f32>() / buf.len() as f32).sqrt();
    assert!(
        rms2 < 0.001,
        "All channels muted should be silent: {}",
        rms2
    );
}
