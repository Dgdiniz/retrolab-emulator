//! Synthetic, CI-safe audio-drift regression test.
//!
//! Runs 60 simulated seconds of the audio path
//! (`IntegerRateConverter` + `AudioRingBuffer`) with a tiny NOP-loop
//! ROM and asserts that:
//!
//!   1. the total drained sample count matches the expected
//!      `44100 * 60` to within 0.1 % (drift bound);
//!   2. zero ring-buffer underruns occurred (the producer must stay
//!      ahead of the drain throughout the run).
//!
//! Uses a synthetic ROM rather than a real game so it can run on
//! every push without ROM dependencies or audio I/O.

use retro_core::types::ExecutionMode;
use retro_core::{DebuggerCore, EmulatorCore};
use sg1_core::GenesisEmulator;

mod helpers;
use helpers::MiniCartridgeBuilder;

const SAMPLE_RATE_HZ: u64 = 44_100;
const SECONDS: u64 = 60;
const EXPECTED_STEREO_PAIRS: u64 = SAMPLE_RATE_HZ * SECONDS;
const EXPECTED_FLOATS: u64 = EXPECTED_STEREO_PAIRS * 2;
const DRIFT_TOLERANCE: f64 = 0.001; // 0.1 %

fn stepping_emulator() -> GenesisEmulator {
    let mut emu = GenesisEmulator::new();
    let rom = MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("AUDIO_DRIFT_60S")
        .with_instructions(0x400, &[0x4E71, 0x60FE])
        .build();
    emu.load_rom(&rom).expect("load mini rom");
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    emu
}

#[test]
fn audio_no_drift_synthetic_60s() {
    let mut emu = stepping_emulator();

    let mut total_floats: u64 = 0;
    // Run exactly `SECONDS` of core time. The frame quantum is the
    // emulator's real refresh rate (59.92 fps NTSC), so 60 s is ~3595
    // frames, NOT 3600. Anchoring the count to `system_info()` keeps the
    // drained-sample expectation tied to a true 60.0 s of audio: a hardcoded
    // 3600 overshoots by ~0.08 s and manufactures ~0.13 % phantom drift that
    // has nothing to do with the rate converter it is meant to guard.
    //
    // `run_frame_fast()` skips debugger overhead but takes the same
    // audio path as `run_frame()`, so the drift assertion is still
    // meaningful while CI wall-clock stays under ~20 s.
    let refresh_rate_hz = f64::from(emu.system_info().display.refresh_rate);
    let frames = (refresh_rate_hz * SECONDS as f64).round() as u64;
    for _ in 0..frames {
        emu.run_frame_fast();
        // Drain on every frame to mirror what the host does in
        // production. This also keeps the bounded ring from
        // overflowing, which would otherwise count as silently
        // dropped samples and skew the drift measurement.
        let drained = emu.get_audio_buffer();
        total_floats += drained.len() as u64;
    }

    // The ring may hold a partial trailing frame; drain it.
    let final_drain = emu.get_audio_buffer();
    total_floats += final_drain.len() as u64;

    let drift_pct =
        (total_floats as f64 - EXPECTED_FLOATS as f64).abs() / (EXPECTED_FLOATS as f64) * 100.0;
    assert!(
        drift_pct < DRIFT_TOLERANCE * 100.0,
        "audio drift after 60s = {:.4}% (got {} floats, expected {} +/- {}%)",
        drift_pct,
        total_floats,
        EXPECTED_FLOATS,
        DRIFT_TOLERANCE * 100.0,
    );

    assert_eq!(
        emu.audio_ring_underruns(),
        0,
        "no underruns should have happened: the producer must always stay ahead of drains"
    );
}
