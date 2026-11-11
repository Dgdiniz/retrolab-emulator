//! Integration tests for PSG in Genesis system
//!
//! Tests the PSG integration with the Genesis emulator,
//! including clocking, synchronization, and audio buffer management.
//!
//! Audio-related tests install a tiny stepping ROM (NOP + BRA-to-self)
//! so the 68K advances `master_cycles` every frame: the
//! `IntegerRateConverter` needs cycle deltas to emit any samples.
//! Tests that only inspect chip state (frame count, channel
//! registers) skip the stepping ROM since they don't depend on
//! sample generation.

use retro_core::audio::AudioDebugger;
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode};
use sg1_core::system::GenesisEmulator;

mod helpers;
use helpers::MiniCartridgeBuilder;

/// Build a GenesisEmulator with a tiny NOP-loop ROM and the debugger
/// set to Running mode, the minimum environment in which the audio
/// pipeline emits samples (the rate converter needs `master_cycles`
/// to advance every frame).
fn audio_emulator() -> GenesisEmulator {
    let mut emu = GenesisEmulator::new();
    let rom = MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("PSG_TEST")
        .with_instructions(0x400, &[0x4E71, 0x60FE])
        .build();
    emu.load_rom(&rom).expect("load mini rom");
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    emu
}

#[test]
fn test_psg_initialized() {
    let emu = GenesisEmulator::new();

    // All channels should be initialized (muted)
    let psg = emu.psg();
    for channel in 0..4 {
        let state = psg.get_channel_state(channel).unwrap();
        assert_eq!(
            state.attenuation, 15,
            "Channel {} should be muted on init",
            channel
        );
    }
}

#[test]
fn test_psg_reset() {
    let mut emu = GenesisEmulator::new();

    // Write some data straight to the PSG (not through the bus)
    emu.psg_mut().write(0x8E); // Tone 0 frequency low
    emu.psg_mut().write(0x0F); // Tone 0 frequency high
    emu.psg_mut().write(0x90); // Tone 0 volume max

    // Verify it was written
    let state = emu.psg().get_channel_state(0).unwrap();
    assert_eq!(state.attenuation, 0);

    // Reset system
    emu.reset();

    // PSG should be muted again
    let state = emu.psg().get_channel_state(0).unwrap();
    assert_eq!(state.attenuation, 15, "PSG should be muted after reset");
}

#[test]
fn test_psg_audio_buffer_generated() {
    let mut emu = audio_emulator();
    emu.psg_mut().write(0x8E);
    emu.psg_mut().write(0x0F);
    emu.psg_mut().write(0x90);

    assert_eq!(emu.audio_available(), 0, "ring should start empty");

    emu.run_frame();

    let buffer_len = emu.get_audio_buffer().len();
    assert!(
        buffer_len > 0,
        "Audio buffer should contain samples after frame"
    );
    assert!(
        (1370..=1570).contains(&buffer_len),
        "Expected ~1470 stereo-interleaved samples, got {}",
        buffer_len
    );
}

#[test]
fn test_psg_audio_buffer_accumulates() {
    let mut emu = audio_emulator();
    emu.psg_mut().write(0x8E);
    emu.psg_mut().write(0x0F);
    emu.psg_mut().write(0x90);

    // Peek the queue with `audio_available()` instead of
    // `get_audio_buffer()`, which would drain it between frames and
    // hide the accumulation we're trying to measure.
    emu.run_frame();
    let buffer_len_1 = emu.audio_available();

    emu.run_frame();
    let buffer_len_2 = emu.audio_available();

    assert!(
        buffer_len_2 > buffer_len_1,
        "Audio buffer should accumulate: {} vs {}",
        buffer_len_1,
        buffer_len_2
    );

    let expected_2x = buffer_len_1 * 2;
    let diff = (buffer_len_2 as i32 - expected_2x as i32).abs();
    assert!(
        diff < 50,
        "Buffer should be ~2x: expected {}, got {}",
        expected_2x,
        buffer_len_2
    );
}

#[test]
fn test_psg_audio_buffer_clears() {
    let mut emu = audio_emulator();
    emu.psg_mut().write(0x8E);
    emu.psg_mut().write(0x0F);
    emu.psg_mut().write(0x90);
    emu.run_frame();

    assert!(emu.audio_available() > 0);
    emu.clear_audio_buffer();
    assert_eq!(emu.audio_available(), 0, "Ring should be empty after clear");
}

#[test]
fn test_psg_audio_buffer_max_size() {
    let mut emu = audio_emulator();
    emu.psg_mut().write(0x8E);
    emu.psg_mut().write(0x0F);
    emu.psg_mut().write(0x90);

    // The ring is bounded at ~4 frames. 10 undrained frames trigger
    // overflow events, but the queued count must still respect the
    // hard cap.
    for _ in 0..10 {
        emu.run_frame();
    }

    let queued = emu.audio_available();
    assert!(
        queued <= 6400,
        "Ring should be hard-capped at ~4 frames, got {}",
        queued
    );
}

#[test]
fn test_psg_produces_audio_signal() {
    let mut emu = audio_emulator();
    emu.psg_mut().write(0x8E);
    emu.psg_mut().write(0x0F);
    emu.psg_mut().write(0x90);

    // Drain after every frame so the bounded ring (capacity ~4
    // frames) cannot drop early samples the test needs to inspect.
    let mut buffer: Vec<f32> = Vec::new();
    for _ in 0..3 {
        emu.run_frame();
        buffer.extend_from_slice(&emu.get_audio_buffer());
    }

    // Should have non-zero samples. The PSG averages all 4 channels
    // (sum / 4) and the system mixer scales PSG by 0.25, so a single
    // tone channel at maximum volume reaches ~ +/-0.0625 at the output.
    let max_amplitude = buffer.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    assert!(
        max_amplitude > 0.05,
        "PSG should produce audible signal (got {})",
        max_amplitude
    );

    // Should have both positive and negative samples (square wave)
    let has_positive = buffer.iter().any(|&s| s > 0.05);
    let has_negative = buffer.iter().any(|&s| s < -0.05);
    assert!(
        has_positive && has_negative,
        "Square wave should have both polarities (pos: {}, neg: {})",
        has_positive,
        has_negative
    );
}

#[test]
fn test_psg_muted_produces_silence() {
    let mut emu = GenesisEmulator::new();

    // Configure frequency but leave muted (attenuation = 15)
    emu.psg_mut().write(0x8E);
    emu.psg_mut().write(0x0F);
    emu.psg_mut().write(0x9F); // Ch0 volume min (15 = mute)

    // Run frame
    emu.run_frame();

    let buffer = emu.get_audio_buffer();

    // Should be silent
    let max_amplitude = buffer.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    assert!(
        max_amplitude < 0.01,
        "Muted PSG should be silent (got {})",
        max_amplitude
    );
}

#[test]
fn test_psg_multi_channel() {
    let mut emu = audio_emulator();
    emu.psg_mut().write(0x8E);
    emu.psg_mut().write(0x0F);
    emu.psg_mut().write(0x90);

    emu.psg_mut().write(0xA4);
    emu.psg_mut().write(0x0C);
    emu.psg_mut().write(0xB0);

    emu.psg_mut().write(0xCA);
    emu.psg_mut().write(0x0A);
    emu.psg_mut().write(0xD0);

    emu.run_frame();

    let buffer = emu.get_audio_buffer();

    // Should have strong signal (3 channels mixed)
    let max_amplitude = buffer.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    assert!(
        max_amplitude > 0.1,
        "Multi-channel should produce strong signal (got {})",
        max_amplitude
    );
}

#[test]
fn test_psg_noise_channel() {
    let mut emu = audio_emulator();

    emu.psg_mut().write(0xCE);
    emu.psg_mut().write(0x01);
    emu.psg_mut().write(0xE7);
    emu.psg_mut().write(0xF0);

    // Drain per-frame; see test_psg_produces_audio_signal for why
    // (bounded ring would otherwise drop early samples).
    let mut buffer: Vec<f32> = Vec::new();
    for _ in 0..3 {
        emu.run_frame();
        buffer.extend_from_slice(&emu.get_audio_buffer());
    }

    // Should produce noise. Same calibration as test_psg_produces_audio_signal:
    // a single PSG channel at max volume yields ~ +/-0.0625 after the
    // 4-channel average + system 0.25 PSG mix.
    let max_amplitude = buffer.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    assert!(
        max_amplitude > 0.05,
        "Noise should produce signal (got {})",
        max_amplitude
    );

    // Noise should vary (not constant)
    let first = buffer[0];
    let has_variation = buffer.iter().any(|&s| (s - first).abs() > 0.02);
    assert!(
        has_variation,
        "Noise should vary (first: {}, all same: {})",
        first,
        buffer.iter().all(|&s| (s - first).abs() < 0.01)
    );
}

#[test]
fn test_psg_channel_state_accessible() {
    let mut emu = GenesisEmulator::new();

    // Configure channel 1
    emu.psg_mut().write(0xA2); // Ch1 freq low
    emu.psg_mut().write(0x0C); // Ch1 freq high
    emu.psg_mut().write(0xB5); // Ch1 volume medium (5)

    // Access channel state
    let state = emu.psg().get_channel_state(1).unwrap();

    assert!(state.enabled);
    assert_eq!(state.attenuation, 5);
    assert_eq!(state.frequency_raw, 0x0C2);
}

#[test]
fn test_psg_frame_count_simple() {
    let mut emu = GenesisEmulator::new();
    emu.reset(); // Initialize

    // Set debugger to running mode
    emu.debugger().set_execution_mode(ExecutionMode::Running);

    assert_eq!(emu.frame_count(), 0, "Should start at 0 after reset");

    emu.run_frame();
    assert_eq!(emu.frame_count(), 1, "Should increment to 1");
}

#[test]
fn test_psg_frame_count_increments() {
    let mut emu = GenesisEmulator::new();
    emu.reset(); // Initialize

    assert_eq!(emu.frame_count(), 0);

    emu.run_frame();
    assert_eq!(emu.frame_count(), 1);

    emu.run_frame();
    assert_eq!(emu.frame_count(), 2);
}
