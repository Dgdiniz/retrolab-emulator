//! YM2612 Trait Integration Tests
//!
//! Tests that validate the YM2612 implements retro-core traits correctly
//! and works as an audio chip for debugger and emulation integration.

use retro_core::audio::{AudioChip, AudioChipInfo, AudioDebugger, ChannelType, EnvelopePhase};
use retro_core::cpu::Clockable;
use sg1_core::ym2612::Ym2612;

/// Helper: Clock YM2612 before generating samples
fn clock_and_generate(ym: &mut Ym2612, output: &mut [f32], sample_rate: u32) -> usize {
    let ym_clock = 7_670_454;
    let cycles_needed = ((ym_clock as f32 / sample_rate as f32) * output.len() as f32) as u32;
    ym.clock(cycles_needed + 1000);
    ym.generate_samples(output, sample_rate)
}

#[test]
fn test_audio_chip_metadata() {
    let ym = Ym2612::new(7_670_454, 44100);

    // Test chip_name
    assert_eq!(ym.chip_name(), "YM2612");

    // Test get_info
    let info: AudioChipInfo = ym.get_info();
    assert_eq!(info.name, "YM2612");
    assert_eq!(info.manufacturer, "Yamaha");
    assert_eq!(info.channels, 6);
    assert_eq!(info.sample_rate, 44100);
    assert!(info.stereo);
    assert_eq!(info.channel_types.len(), 6);

    // First 5 channels should be FM
    for i in 0..5 {
        assert_eq!(info.channel_types[i], ChannelType::FM);
    }

    // Channel 6 should be FM (DAC not enabled yet)
    assert_eq!(info.channel_types[5], ChannelType::FM);
}

#[test]
fn test_audio_chip_dac_mode() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Enable DAC
    ym.write_register(0, 0x2B, 0x80);

    let info = ym.get_info();

    // Channel 6 should now be PCM
    assert_eq!(info.channel_types[5], ChannelType::PCM);
}

#[test]
fn test_clockable_trait() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Configure and key on a simple sound
    ym.write_register(0, 0xB0, 0x3F); // FB=7, ALG=7 (all parallel)
    ym.write_register(0, 0x30, 0x01); // OP1: MUL=1
    ym.write_register(0, 0x40, 0x00); // OP1: TL=0 (max volume)
    ym.write_register(0, 0x50, 0x1F); // OP1: AR=31
    ym.write_register(0, 0xA0, 0xCE); // F-Num low
    ym.write_register(0, 0xA4, 0x22); // Block 4, F-Num high
    ym.write_register(0, 0x28, 0xF0); // Key on all operators, channel 0

    // Clock chip using Clockable trait
    let cycles = ym.clock(1);
    assert_eq!(cycles, 1);

    // `clock()` accumulates cycles and runs the timers but renders no audio;
    // the output level (`get_output_level()`) is refreshed by
    // `generate_one_sample`, which is invoked from `generate_samples`. So we
    // need to actually drain a few samples after clocking to see it update.
    for _ in 0..10000 {
        ym.clock(1);
    }
    let mut scratch = [0.0f32; 256];
    ym.generate_samples(&mut scratch, 44100);

    let level = ym.get_output_level();
    assert!(level > 0.0, "Output level should be non-zero after attack");
}

#[test]
fn test_generate_samples() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Configure channel 0 for sound
    ym.write_register(0, 0xB0, 0x07); // ALG=7
    ym.write_register(0, 0x30, 0x01); // OP1: MUL=1
    ym.write_register(0, 0x40, 0x00); // OP1: TL=0
    ym.write_register(0, 0x50, 0x1F); // OP1: AR=31
    ym.write_register(0, 0xA0, 0xCE); // F-Num low
    ym.write_register(0, 0xA4, 0x22); // Block 4
    ym.write_register(0, 0x28, 0xF0); // Key on

    // Generate samples
    let mut output = vec![0.0; 2048]; // 1024 stereo samples
    let count = clock_and_generate(&mut ym, &mut output, 44100);

    assert_eq!(count, 2048);

    // Verify samples are in valid range
    for sample in &output {
        assert!(sample.abs() <= 1.0, "Sample {} out of range", sample);
    }

    // After attack, some samples should be non-zero
    let non_zero = output.iter().filter(|&&s| s.abs() > 0.01).count();
    assert!(non_zero > 100, "Should have some non-zero samples");
}

#[test]
fn test_audio_debugger_channel_count() {
    let ym = Ym2612::new(7_670_454, 44100);

    assert_eq!(ym.get_channel_count(), 6);
}

#[test]
fn test_audio_debugger_channel_state() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Configure channel 0
    ym.write_register(0, 0xB0, 0x07); // ALG=7
    ym.write_register(0, 0xB4, 0xC0); // Both L+R
    ym.write_register(0, 0x30, 0x01); // OP1: MUL=1
    ym.write_register(0, 0x40, 0x20); // OP1: TL=32
    ym.write_register(0, 0x50, 0x1F); // OP1: AR=31
    ym.write_register(0, 0x60, 0x10); // OP1: D1R=16
    ym.write_register(0, 0x70, 0x08); // OP1: D2R=8
    ym.write_register(0, 0x80, 0x85); // OP1: SL=8, RR=5
    ym.write_register(0, 0xA0, 0xCE); // F-Num low = 0xCE
    ym.write_register(0, 0xA4, 0x22); // Block=4, F-Num high=2

    // Get channel state before key on
    let state = ym.get_channel_state(0).unwrap();

    assert_eq!(state.channel_id, 0);
    assert_eq!(state.channel_type, ChannelType::FM);
    assert!(!state.enabled); // No key on yet
    assert!(!state.muted);
    assert_eq!(state.frequency_raw, 0x2CE); // 718
                                            // Frequency calculation depends on operator settings, just check it's reasonable
    assert!(state.frequency >= 0.0 && state.frequency < 10000.0);
    assert_eq!(state.attenuation, 32);
    assert_eq!(state.pan, 0.0); // Center (both L+R)

    // Verify envelope state
    let envelope = state.envelope.unwrap();
    assert_eq!(envelope.attack, 31.0 / 31.0); // AR=31 normalized
    assert_eq!(envelope.decay, 16.0 / 31.0); // D1R=16 normalized
    assert_eq!(envelope.current_phase, EnvelopePhase::Off);

    // Key on
    ym.write_register(0, 0x28, 0xF0);

    // Get state after key on
    let state = ym.get_channel_state(0).unwrap();
    assert!(state.enabled);
}

#[test]
fn test_audio_debugger_stereo_panning() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Register 0xB4: bit 7 = L output enable, bit 6 = R output enable.
    // Test center (both L+R)
    ym.write_register(0, 0xB4, 0xC0); // 1100 0000 = L=1, R=1
    let state = ym.get_channel_state(0).unwrap();
    assert_eq!(state.pan, 0.0);

    // Test left only: L=1, R=0 -> bit 7 set, bit 6 clear -> 0x80
    ym.write_register(0, 0xB4, 0x80);
    let state = ym.get_channel_state(0).unwrap();
    assert_eq!(state.pan, -1.0);

    // Test right only: L=0, R=1 -> bit 7 clear, bit 6 set -> 0x40
    ym.write_register(0, 0xB4, 0x40);
    let state = ym.get_channel_state(0).unwrap();
    assert_eq!(state.pan, 1.0);
}

#[test]
fn test_audio_debugger_mute_channel() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Configure and key on channel 0
    ym.write_register(0, 0xB0, 0x07); // ALG=7
    ym.write_register(0, 0x30, 0x01); // OP1: MUL=1
    ym.write_register(0, 0x40, 0x00); // OP1: TL=0
    ym.write_register(0, 0x50, 0x1F); // OP1: AR=31
    ym.write_register(0, 0xA0, 0xCE); // F-Num
    ym.write_register(0, 0xA4, 0x22); // Block
    ym.write_register(0, 0x28, 0xF0); // Key on

    // Generate some samples before mute
    let mut output = vec![0.0; 2048];
    for _ in 0..5 {
        clock_and_generate(&mut ym, &mut output, 44100);
    }

    let max_before = output.iter().map(|&s| s.abs()).fold(0.0f32, f32::max);

    // Mute channel 0
    ym.mute_channel(0, true);

    // Verify muted in state
    let state = ym.get_channel_state(0).unwrap();
    assert!(state.muted);

    // Generate samples after mute
    output.fill(0.0);
    clock_and_generate(&mut ym, &mut output, 44100);

    let max_after = output.iter().map(|&s| s.abs()).fold(0.0f32, f32::max);

    // After muting, output should be much quieter (other channels might still produce sound)
    eprintln!("max_before: {}, max_after: {}", max_before, max_after);
    assert!(
        max_before > 0.01,
        "Should have output before mute (got {})",
        max_before
    );
    assert!(
        max_after < max_before / 10.0,
        "Should be much quieter after mute (before: {}, after: {})",
        max_before,
        max_after
    );
}

#[test]
fn test_audio_debugger_waveform() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Configure and key on
    ym.write_register(0, 0xB0, 0x07);
    ym.write_register(0, 0x30, 0x01);
    ym.write_register(0, 0x40, 0x00);
    ym.write_register(0, 0x50, 0x1F);
    ym.write_register(0, 0xA0, 0xCE);
    ym.write_register(0, 0xA4, 0x22);
    ym.write_register(0, 0x28, 0xF0);

    // Generate some samples
    let mut output = vec![0.0; 2048];
    clock_and_generate(&mut ym, &mut output, 44100);

    // Get output waveform
    let waveform = ym.get_output_waveform(512);

    assert_eq!(waveform.len(), 512);

    // Waveform should have some variation
    let min = waveform.iter().cloned().fold(f32::INFINITY, f32::min);
    let max = waveform.iter().cloned().fold(f32::NEG_INFINITY, f32::max);

    assert!(max - min > 0.01, "Waveform should have variation");
}

#[test]
fn test_audio_debugger_invalid_channel() {
    let ym = Ym2612::new(7_670_454, 44100);

    // Invalid channel indices
    assert!(ym.get_channel_state(6).is_none());
    assert!(ym.get_channel_state(100).is_none());
}

#[test]
fn test_reset_via_trait() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // Configure and key on
    ym.write_register(0, 0x22, 0x0F); // Enable LFO
    ym.write_register(0, 0x2B, 0x80); // Enable DAC
    ym.write_register(0, 0x28, 0xF0); // Key on

    // Mute channel
    ym.mute_channel(0, true);

    // Reset using trait method
    <Ym2612 as AudioChip>::reset(&mut ym);

    // Verify state is reset
    let info = ym.get_info();
    assert_eq!(info.channel_types[5], ChannelType::FM); // DAC disabled

    let state = ym.get_channel_state(0).unwrap();
    assert!(!state.enabled); // Key off
    assert!(!state.muted); // Unmuted

    let level = ym.get_output_level();
    assert_eq!(level, 0.0); // No output
}

#[test]
fn test_full_integration_flow() {
    let mut ym = Ym2612::new(7_670_454, 44100);

    // 1. Get chip info
    let info = ym.get_info();
    assert_eq!(info.name, "YM2612");
    assert_eq!(info.channels, 6);

    // 2. Configure channel 0 for piano-like sound
    // Algorithm 0 (series), moderate feedback
    ym.write_register(0, 0xB0, 0x18); // FB=3, ALG=0
    ym.write_register(0, 0xB4, 0xC0); // Both L+R

    // Configure all 4 operators
    for op in 0..4 {
        let op_offset = op * 4;
        ym.write_register(0, 0x30 + op_offset, 0x01); // MUL=1
        ym.write_register(0, 0x40 + op_offset, if op == 0 { 0x10 } else { 0x00 }); // TL
        ym.write_register(0, 0x50 + op_offset, 0x1F); // AR=31
        ym.write_register(0, 0x60 + op_offset, 0x10); // D1R=16
        ym.write_register(0, 0x70 + op_offset, 0x08); // D2R=8
        ym.write_register(0, 0x80 + op_offset, 0x85); // SL=8, RR=5
    }

    // Set frequency: F-number 0x2CE, block 4 (~292 Hz)
    ym.write_register(0, 0xA0, 0xCE);
    ym.write_register(0, 0xA4, 0x22);

    // 3. Check channel state before key on
    let state = ym.get_channel_state(0).unwrap();
    assert!(!state.enabled);
    assert_eq!(state.channel_type, ChannelType::FM);

    // 4. Key on
    ym.write_register(0, 0x28, 0xF0);

    // 5. Verify key on
    let state = ym.get_channel_state(0).unwrap();
    assert!(state.enabled);

    // 6. Generate audio samples
    let mut output = vec![0.0; 4096]; // 2048 stereo samples
    let count = clock_and_generate(&mut ym, &mut output, 44100);
    assert_eq!(count, 4096);

    // 7. Verify audio is being generated
    let rms = (output.iter().map(|&s| s * s).sum::<f32>() / output.len() as f32).sqrt();
    assert!(rms > 0.01, "Should have audible output");

    // 8. Get waveform for visualization
    let waveform = ym.get_output_waveform(256);
    assert_eq!(waveform.len(), 256);

    // 9. Mute channel
    ym.mute_channel(0, true);
    let state = ym.get_channel_state(0).unwrap();
    assert!(state.muted);

    // 10. Generate samples (should be silent)
    output.fill(0.0);
    clock_and_generate(&mut ym, &mut output, 44100);
    let rms_muted = (output.iter().map(|&s| s * s).sum::<f32>() / output.len() as f32).sqrt();
    assert!(rms_muted < rms / 10.0, "Muted should be much quieter");

    // 11. Unmute
    ym.mute_channel(0, false);

    // 12. Key off
    ym.write_register(0, 0x28, 0x00);

    // 13. After key off the channel may still be in its release phase:
    // nothing is checked here, the chip is only clocked on
    for _ in 0..1000 {
        ym.clock(1);
    }

    // 14. Reset chip
    <Ym2612 as AudioChip>::reset(&mut ym);

    // 15. Verify reset
    let state = ym.get_channel_state(0).unwrap();
    assert!(!state.enabled);
    assert!(!state.muted);

    let level = ym.get_output_level();
    assert_eq!(level, 0.0);
}
