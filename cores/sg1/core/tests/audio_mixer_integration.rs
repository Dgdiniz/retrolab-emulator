//! Integration tests for PSG + YM2612 audio mixing.
//!
//! Validates that both chips are clocked and mixed correctly in the
//! Genesis system loop, and that the audio pipeline
//! (`IntegerRateConverter` + `AudioRingBuffer`) behaves as designed.
//!
//! Each test loads a tiny synthetic ROM (NOP / BRA-to-self) so the
//! 68K steps continuously and `master_cycles` advances. Without that
//! the rate converter would correctly emit zero samples and the
//! mixer tests would be vacuously true.

use retro_core::types::ExecutionMode;
use retro_core::{DebuggerCore, EmulatorCore};
use sg1_core::GenesisEmulator;

mod helpers;
use helpers::MiniCartridgeBuilder;

fn stepping_emulator() -> GenesisEmulator {
    let mut genesis = GenesisEmulator::new();
    let rom = MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("AUDIO_MIX_TEST")
        .with_instructions(0x400, &[0x4E71, 0x60FE]) // NOP ; BRA.S *
        .build();
    genesis.load_rom(&rom).expect("load mini rom");
    // run_frame() inspects the debugger's execution mode at every
    // 68K step: Paused would short-circuit and emit no samples.
    genesis
        .debugger()
        .set_execution_mode(ExecutionMode::Running);
    genesis
}

#[test]
fn test_audio_samples_generated_per_frame() {
    let mut genesis = stepping_emulator();
    genesis.clear_audio_buffer();
    genesis.run_frame();

    let samples = genesis.get_audio_buffer();
    let expected_samples = 1470; // 735 stereo pairs * 2 channels
    let tolerance = 100;

    assert!(
        samples.len() >= expected_samples - tolerance
            && samples.len() <= expected_samples + tolerance,
        "Expected ~{} stereo-interleaved samples per frame (+/-{}), got {}",
        expected_samples,
        tolerance,
        samples.len()
    );
}

#[test]
fn test_psg_ym2612_both_contribute_to_buffer() {
    let mut genesis = stepping_emulator();
    genesis.clear_audio_buffer();
    genesis.run_frame();

    let samples = genesis.get_audio_buffer();
    assert!(
        !samples.is_empty(),
        "Audio mixer must generate samples even when chips are silent"
    );

    let avg_amplitude: f32 = samples.iter().map(|s| s.abs()).sum::<f32>() / samples.len() as f32;
    assert!(
        avg_amplitude < 0.1,
        "Silent chips should produce low amplitude samples, got avg {}",
        avg_amplitude
    );
}

#[test]
fn test_audio_clocking_synchronization() {
    let mut genesis = stepping_emulator();

    for frame in 0..10 {
        genesis.clear_audio_buffer();
        genesis.run_frame();

        let samples = genesis.get_audio_buffer();
        assert!(!samples.is_empty(), "Frame {} produced no samples!", frame);
        assert!(
            samples.len() >= 1370 && samples.len() <= 1570,
            "Frame {} sample count {} is out of expected range [1370-1570]",
            frame,
            samples.len()
        );
    }
}

#[test]
fn test_audio_mixer_no_clipping() {
    let mut genesis = stepping_emulator();
    genesis.clear_audio_buffer();

    // Drain after every frame: the ring caps at ~4 frames, so 30
    // un-drained frames would otherwise drop the early samples we'd
    // want to validate.
    let mut all_samples: Vec<f32> = Vec::with_capacity(30 * 1470);
    for _ in 0..30 {
        genesis.run_frame();
        all_samples.extend_from_slice(&genesis.get_audio_buffer());
    }

    assert!(
        !all_samples.is_empty(),
        "No samples generated after 30 frames!"
    );

    for (i, &sample) in all_samples.iter().enumerate() {
        assert!(
            (-1.0..=1.0).contains(&sample),
            "Sample {} out of range: {}",
            i,
            sample
        );
        assert!(!sample.is_nan(), "Sample {} is NaN!", i);
    }
}

#[test]
fn test_multiple_frames_consistent_output() {
    let mut genesis = stepping_emulator();
    let mut sample_counts = Vec::new();

    for _ in 0..10 {
        genesis.clear_audio_buffer();
        genesis.run_frame();
        sample_counts.push(genesis.get_audio_buffer().len());
    }

    let first_count = sample_counts[0];
    for (i, &count) in sample_counts.iter().enumerate() {
        assert!(
            (first_count as i32 - count as i32).abs() <= 2,
            "Frame {} produced {} samples, but frame 0 produced {}",
            i,
            count,
            first_count
        );
    }
}

#[test]
fn test_reset_clears_audio_state() {
    let mut genesis = stepping_emulator();

    for _ in 0..10 {
        genesis.run_frame();
    }
    // Drain *available* count instead of `get_audio_buffer().len()`
    // because the ring caps at ~4 frames: 10 frames un-drained will
    // have overflowed and the consumer count won't be `10 *
    // samples_per_frame`. The point of this test is "reset doesn't
    // break later audio", not "buffer is unbounded".
    let queued_before_reset = genesis.audio_available();
    assert!(queued_before_reset > 0, "Should have samples before reset");

    genesis.reset();
    // reset() leaves the debugger in Paused; re-enable Running so
    // run_frame() actually steps the CPU. The ROM stays loaded:
    // reset does not unload it.
    genesis
        .debugger()
        .set_execution_mode(ExecutionMode::Running);

    genesis.clear_audio_buffer();
    genesis.run_frame();

    let samples_after_reset = genesis.get_audio_buffer().len();
    assert!(samples_after_reset > 0, "Must generate samples after reset");
}

#[test]
fn test_audio_buffer_accumulation() {
    let mut genesis = stepping_emulator();
    genesis.clear_audio_buffer();

    // Use `audio_available()` to peek instead of `get_audio_buffer()`,
    // which drains the ring. We want to verify that running two
    // frames back-to-back without draining produces ~2x the queue.
    genesis.run_frame();
    let after_frame1 = genesis.audio_available();
    genesis.run_frame();
    let after_frame2 = genesis.audio_available();

    assert!(
        after_frame2 > after_frame1,
        "Audio buffer must accumulate samples across frames! \
         After frame 1: {}, after frame 2: {}",
        after_frame1,
        after_frame2
    );
}

// The two tests below assert the bounded `AudioRingBuffer`'s hard
// cap: the queued count must never exceed ~4 frames' worth of
// stereo samples no matter how many frames the host fails to
// drain. Overflow drops the oldest samples and bumps
// `overflow_count`.

#[test]
fn test_audio_buffer_size_limited() {
    let mut genesis = stepping_emulator();

    for _ in 0..100 {
        genesis.run_frame();
    }

    let queued = genesis.audio_available();
    assert!(
        queued <= 6000,
        "Ring buffer should hard-cap at ~4 frames (<=6000 stereo samples), got {}",
        queued
    );
    // After 100 undrained frames with a ~5880-sample ring, every
    // overflow event drops samples, so `overflow_count()` must be
    // strictly positive.
    assert!(
        genesis.audio_ring_overflows() > 0,
        "Expected overflow events after 100 undrained frames"
    );
}

#[test]
fn test_long_running_stability() {
    let mut genesis = stepping_emulator();

    for frame in 0..300 {
        genesis.run_frame();

        if frame % 60 == 0 {
            let queued = genesis.audio_available();
            assert!(
                queued <= 6000,
                "Frame {}: ring queue size {} exceeds 6000-cap",
                frame,
                queued
            );
            // Continuously drain so the ring doesn't permanently
            // overflow: this mirrors what the real host does each frame.
            let _ = genesis.get_audio_buffer();
        }
    }
}
