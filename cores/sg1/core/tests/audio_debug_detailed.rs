//! Detailed audio-pipeline integration tests.
//!
//! The audio contract is "cycles -> samples derived from
//! `IntegerRateConverter::cycles_to_samples(master_cycles)`": without
//! `master_cycles` advancing, no audio is emitted. These tests load
//! a ROM built by `MiniCartridgeBuilder` with a NOP-loop so the 68K keeps
//! stepping every frame, which makes the converter emit the
//! expected ~735 stereo pairs / frame at 44.1 kHz NTSC.

use retro_core::types::ExecutionMode;
use retro_core::{DebuggerCore, EmulatorCore};
use sg1_core::system::GenesisEmulator;

mod helpers;
use helpers::MiniCartridgeBuilder;

/// Tiny NOP-loop ROM used to keep the 68K stepping (master_cycles
/// advancing) without needing a real game image. Reset PC is 0x400
/// (set by `MiniCartridgeBuilder::build_genesis_header`).
fn install_nop_loop(genesis: &mut GenesisEmulator) {
    let rom = MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("AUDIO_TEST")
        // 0x4E71 = NOP, 0x60FE = BRA.S * (branch to self). Together
        // they keep the 68K stepping at a steady rate, advancing
        // master_cycles every frame without IRQ noise.
        .with_instructions(0x400, &[0x4E71, 0x60FE])
        .build();
    genesis.load_rom(&rom).expect("load mini cart");
}

#[test]
fn test_genesis_audio_step_by_step() {
    let mut genesis = GenesisEmulator::new();
    install_nop_loop(&mut genesis);

    genesis
        .debugger()
        .set_execution_mode(ExecutionMode::Running);

    eprintln!("\n=== DETAILED AUDIO DEBUG ===\n");
    eprintln!(
        "Step 1: Initial buffer = {} samples",
        genesis.get_audio_buffer().len()
    );

    genesis.clear_audio_buffer();
    eprintln!("Step 2: After clear = {}", genesis.get_audio_buffer().len());

    eprintln!("Step 3: ROM loaded = {}", genesis.get_rom_info().is_some());

    eprintln!("Step 4: Running one frame...");
    genesis.run_frame();

    let samples = genesis.get_audio_buffer();
    eprintln!(
        "Step 5: After run_frame() = {} samples ({} stereo pairs)",
        samples.len(),
        samples.len() / 2
    );

    assert!(
        !samples.is_empty(),
        "Expected audio after one frame with a stepping ROM"
    );
    // Audio is stereo-interleaved (L, R, L, R, ...) so ~735 pairs
    // -> ~1470 floats per NTSC frame at 44.1 kHz.
    assert!(
        samples.len() >= 1370 && samples.len() <= 1570,
        "Expected ~1470 stereo-interleaved samples per frame, got {}",
        samples.len()
    );
}

#[test]
fn test_genesis_multiple_frames() {
    let mut genesis = GenesisEmulator::new();
    install_nop_loop(&mut genesis);
    genesis
        .debugger()
        .set_execution_mode(ExecutionMode::Running);

    eprintln!("\n=== MULTIPLE FRAMES TEST ===\n");

    for frame in 0..5 {
        genesis.clear_audio_buffer();
        genesis.run_frame();

        let samples = genesis.get_audio_buffer().len();
        eprintln!("Frame {}: {} samples", frame, samples);
        assert!(samples > 0, "Frame {} produced no samples!", frame);
    }
}

/// An emulator with no ROM produces no audio: with the CPU sitting
/// idle, `master_cycles` stays at the reset baseline and the rate
/// converter's `cycles_to_samples(master_cycles)` delta is zero,
/// nothing to push into the ring.
#[test]
fn test_no_rom_produces_no_audio() {
    let mut genesis = GenesisEmulator::new();
    genesis
        .debugger()
        .set_execution_mode(ExecutionMode::Running);

    genesis.run_frame();

    assert!(
        genesis.get_audio_buffer().is_empty(),
        "Without a ROM, no master_cycles advance and the rate converter must emit 0 samples"
    );
}
