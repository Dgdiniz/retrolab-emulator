//! Debug test for PSG integration with Genesis.
//!
//! Audio samples require an actively-stepping 68K (the rate
//! converter only emits when `master_cycles` advances), so each
//! test installs a tiny NOP-loop ROM and forces the debugger into
//! Running mode.

use retro_core::audio::AudioDebugger;
use retro_core::types::ExecutionMode;
use retro_core::{DebuggerCore, EmulatorCore};
use sg1_core::GenesisEmulator;

mod helpers;
use helpers::MiniCartridgeBuilder;

fn debug_emulator() -> GenesisEmulator {
    let mut emu = GenesisEmulator::new();
    let rom = MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("PSG_DEBUG")
        .with_instructions(0x400, &[0x4E71, 0x60FE])
        .build();
    emu.load_rom(&rom).expect("load mini rom");
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    emu
}

#[test]
fn debug_psg_tone_in_genesis() {
    let mut emu = debug_emulator();

    eprintln!("\n=== PSG TONE DEBUG IN GENESIS ===");

    // Configure 440Hz tone at max volume
    eprintln!("\nConfiguring PSG channel 0...");
    emu.psg_mut().write(0x8E); // Ch0 freq low
    emu.psg_mut().write(0x0F); // Ch0 freq high -> 0x0FE
    emu.psg_mut().write(0x90); // Ch0 volume max

    // Check PSG state before run_frame
    let psg_before = emu.psg();
    eprintln!("PSG before run_frame:");
    eprintln!("  Channel 0 state: {:?}", psg_before.get_channel_state(0));

    // Drain each frame to keep all 5 frames' samples (ring caps at
    // ~4 frames at 44.1 kHz/60 fps).
    eprintln!("\nRunning 5 frames to allow wave to oscillate...");
    let mut buffer: Vec<f32> = Vec::new();
    for i in 0..5 {
        emu.run_frame();
        let drained = emu.get_audio_buffer();
        eprintln!("  Frame {}: {} samples", i + 1, drained.len());
        buffer.extend_from_slice(&drained);
    }

    eprintln!("\nAudio buffer after 5 frames:");
    eprintln!("  Total samples: {}", buffer.len());
    eprintln!(
        "  Last 40 samples: {:?}",
        &buffer[buffer.len().saturating_sub(40)..]
    );

    // Stats
    let max_amp = buffer.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    let has_pos = buffer.iter().any(|&s| s > 0.01);
    let has_neg = buffer.iter().any(|&s| s < -0.01);
    let all_zero = buffer.iter().all(|&s| s == 0.0);
    let unique_values: std::collections::HashSet<_> =
        buffer.iter().map(|&s| (s * 1000.0) as i32).collect();

    eprintln!("\nStats:");
    eprintln!("  Max amplitude: {}", max_amp);
    eprintln!("  Has positive: {}", has_pos);
    eprintln!("  Has negative: {}", has_neg);
    eprintln!("  All zero: {}", all_zero);
    eprintln!(
        "  Unique values: {} (should be 2 for square wave)",
        unique_values.len()
    );
}

#[test]
fn debug_psg_noise_in_genesis() {
    let mut emu = debug_emulator();

    eprintln!("\n=== PSG NOISE DEBUG IN GENESIS ===");

    emu.psg_mut().write(0xE7);
    emu.psg_mut().write(0xF0);

    let psg_before = emu.psg();
    eprintln!("PSG before run_frame:");
    eprintln!(
        "  Noise channel state: {:?}",
        psg_before.get_channel_state(3)
    );

    eprintln!("\nRunning 5 frames...");
    let mut buffer: Vec<f32> = Vec::new();
    for i in 0..5 {
        emu.run_frame();
        let drained = emu.get_audio_buffer();
        eprintln!("  Frame {}: {} samples", i + 1, drained.len());
        buffer.extend_from_slice(&drained);
    }

    eprintln!("\nAudio buffer after 5 frames:");
    eprintln!("  Total samples: {}", buffer.len());
    eprintln!("  First 50 samples: {:?}", &buffer[..50.min(buffer.len())]);
    eprintln!(
        "  Last 50 samples: {:?}",
        &buffer[buffer.len().saturating_sub(50)..]
    );

    // Stats
    let unique_values: std::collections::HashSet<_> =
        buffer.iter().map(|&s| (s * 1000.0) as i32).collect();
    let first = buffer[0];
    let all_same = buffer.iter().all(|&s| (s - first).abs() < 0.001);

    eprintln!("\nStats:");
    eprintln!(
        "  Unique values: {} (should be >2 for noise)",
        unique_values.len()
    );
    eprintln!("  First value: {}", first);
    eprintln!("  All same: {}", all_same);

    if all_same {
        eprintln!("\nWARNING: Noise LFSR not advancing!");
        eprintln!("  Possible causes:");
        eprintln!("  1. Noise channel not being clocked");
        eprintln!("  2. LFSR stuck at initial state");
        eprintln!("  3. Noise frequency register incorrect");
    }
}
