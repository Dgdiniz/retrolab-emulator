//! Debug test for full Genesis audio system.
//!
//! Requires a stepping ROM to drive `master_cycles` forward: the
//! audio sample count is strictly proportional to cycles executed,
//! so without a stepping CPU the rate converter emits nothing and
//! the test would be vacuously true. See `audio_debug_detailed.rs`
//! for the same setup applied to a more focused suite.

use retro_core::types::ExecutionMode;
use retro_core::{DebuggerCore, EmulatorCore};
use sg1_core::GenesisEmulator;

mod helpers;
use helpers::MiniCartridgeBuilder;

#[test]
fn test_genesis_audio_generation() {
    let mut genesis = GenesisEmulator::new();
    // NOP / BRA-to-self loop keeps the 68K stepping continuously.
    let rom = MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("AUDIO_GEN_TEST")
        .with_instructions(0x400, &[0x4E71, 0x60FE])
        .build();
    genesis.load_rom(&rom).expect("load mini rom");

    genesis
        .debugger()
        .set_execution_mode(ExecutionMode::Running);

    eprintln!("=== Genesis Audio Debug ===");
    eprintln!(
        "Initial audio buffer: {} samples",
        genesis.get_audio_buffer().len()
    );

    genesis.clear_audio_buffer();
    eprintln!("After clear: {} samples", genesis.get_audio_buffer().len());

    genesis.run_frame();

    let samples = genesis.get_audio_buffer();
    eprintln!(
        "After run_frame(): {} samples (stereo pairs: {})",
        samples.len(),
        samples.len() / 2
    );

    assert!(
        !samples.is_empty(),
        "Genesis must generate samples with a stepping ROM"
    );

    // Stereo-interleaved (L, R, L, R, ...) so ~735 pairs -> ~1470
    // floats per NTSC frame at 44.1 kHz.
    assert!(
        samples.len() >= 1370 && samples.len() <= 1570,
        "Expected ~1470 stereo-interleaved samples, got {}",
        samples.len()
    );
}
