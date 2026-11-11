//! Cx4 (Hitachi HG51B169) boot tests on a synthetic cartridge: a Cx4-shaped
//! LoROM with a `BRA -2` reset loop installs the `Cx4Chip`, boots, runs
//! frames without panic, and the S-CPU physically reaches the `$6000-7FFF`
//! window (data RAM round-trip + an idle status read that never hangs).
//!
//! Needs the `chip-cx4` feature (on by default): without the Cx4's
//! module this file compiles to nothing.

#![cfg(feature = "chip-cx4")]

use retro_core::cartridge::ExpansionChipType;
use retro_core::EmulatorCore;
use snes1_core::{detect_preset, PresetKind, SnesEmulator};

// -----------------------------------------------------------------
// Synthetic Cx4 ROM synthesis
// -----------------------------------------------------------------

/// `BRA -2`: branch always to itself, the standard "halt at the boot vector"
/// idiom (`80 FE`).
const RESET_LOOP_BYTES: [u8; 2] = [0x80, 0xFE];

/// A 2 MiB Cx4-shaped LoROM: header at `$7FB0` with `map_mode $20` (LoROM) and
/// `cartridge_type $F3` (the Cx4 signature), a `BRA -2` reset loop at
/// `$00:8000`, and the emulation-mode reset vector pointing there. 2 MiB is the
/// real Mega Man X3 size, so the preset carries no fold mask.
fn synth_cx4_rom() -> Vec<u8> {
    const SIZE: usize = 0x20_0000; // 2 MiB
    let mut chip = vec![0u8; SIZE];

    // Reset loop at chip[0..2], visible as `$00:8000` through the LoROM window.
    chip[0..2].copy_from_slice(&RESET_LOOP_BYTES);

    // Reset vector at chip[$7FFC..$7FFD] = `00 80` (LE) -> `$8000`.
    chip[0x7FFC] = 0x00;
    chip[0x7FFD] = 0x80;

    let header_offset = 0x7FB0;
    let title = b"CX4 BOOT TEST        ";
    chip[header_offset + 0x10..header_offset + 0x10 + 21].copy_from_slice(title);
    chip[header_offset + 0x25] = 0x20; // map_mode = LoROM
    chip[header_offset + 0x26] = 0xF3; // cartridge_type = Cx4
    chip[header_offset + 0x27] = 0x0B; // rom_size_code = 2 MiB
    chip[header_offset + 0x28] = 0x00; // sram_size_code = none (password saves)
    chip[header_offset + 0x29] = 0x01; // destination = USA -> NTSC
    chip[header_offset + 0x2A] = 0x01; // developer_id (non-extended)
    chip[header_offset + 0x2B] = 0x00; // rom_version

    // Consistent checksum pair (complement + checksum sum to $FFFF) -> +6 score.
    let checksum: u16 = 0x1234;
    let complement: u16 = !checksum;
    chip[header_offset + 0x2C..header_offset + 0x2E].copy_from_slice(&complement.to_le_bytes());
    chip[header_offset + 0x2E..header_offset + 0x30].copy_from_slice(&checksum.to_le_bytes());

    chip
}

// -----------------------------------------------------------------
// Synthetic boot smoke (always runs)
// -----------------------------------------------------------------

#[test]
fn synthetic_cx4_detects_installs_and_boots() {
    let rom = synth_cx4_rom();

    // Detection promotes the LoROM-located header to a Cx4 cartridge.
    let detected = detect_preset(&rom).expect("synthetic Cx4 detects a preset");
    assert_eq!(
        detected.kind,
        PresetKind::Cx4,
        "map_mode $20 + cartridge_type $F3 must detect as Cx4"
    );
    assert!(detected.header.is_cx4(), "header.is_cx4()");

    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).expect("synthetic Cx4 loads");
    assert_eq!(emu.preset_kind(), Some(PresetKind::Cx4), "installs as Cx4");
    assert_eq!(
        emu.coprocessor_label(),
        Some("Cx4"),
        "the Cx4 chip must be installed"
    );

    // The reset vector took the CPU to `$00:8000`, through the chip-routed ROM
    // window (a broken ROM slot or a missing chip would fault the fetch).
    assert_eq!(emu.cpu().pbr, 0x00, "PBR after reset");
    assert_eq!(
        emu.cpu().pc,
        0x8000,
        "PC after reset (chip-routed ROM fetch)"
    );
}

#[test]
fn synthetic_cx4_scpu_reaches_the_6000_window() {
    let rom = synth_cx4_rom();
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).expect("synthetic Cx4 loads");

    // The `$6000-7FFF` page defaults to open bus and is promoted to the
    // cartridge only because the Cx4 MMIO slot covers it. A byte written to the
    // internal data RAM ($6000) through the full S-CPU bus must read back, a
    // mismatch means the map slot or the page promotion is broken.
    emu.write_memory_debug(0x00_6000, &[0x5A]);
    assert_eq!(
        emu.read_memory_debug(0x00_6000, 1),
        vec![0x5A],
        "S-CPU must reach the Cx4 data RAM via the promoted $6000 page"
    );

    // A status-register read ($7F53) returns 0 (idle): the `BRA -2` ROM never
    // arms the HG51B169, so it stays halted and a booting game's busy-poll
    // exits immediately instead of hanging.
    assert_eq!(
        emu.read_memory_debug(0x00_7F53, 1),
        vec![0x00],
        "an unarmed Cx4 status must read idle so a busy-poll never hangs"
    );

    // The generic coprocessor debug state surfaces the chip as a Cx4 exposing
    // its 3 KiB internal data RAM. The chip is live (lazy catch-up runs on the
    // S-CPU clock), but this ROM never starts a program, so the byte the S-CPU
    // wrote to data RAM is untouched by the DSP.
    let state = emu
        .coprocessor_debug_state()
        .expect("Cx4 cartridge exposes chip state");
    assert_eq!(state.chip_type, ExpansionChipType::CX4);
    assert_eq!(
        state.internal_ram.as_ref().map(Vec::len),
        Some(3072),
        "Cx4 exposes its 3 KiB internal data RAM"
    );
    assert_eq!(
        state.internal_ram.as_ref().map(|r| r[0]),
        Some(0x5A),
        "an unarmed DSP leaves the S-CPU's data-RAM byte intact"
    );
}

#[test]
fn synthetic_cx4_runs_frames_without_panic() {
    let rom = synth_cx4_rom();
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).expect("synthetic Cx4 loads");

    // The `BRA -2` loop never arms the chip, but the whole cartridge -> bus ->
    // CPU reset -> scheduler -> run-frame chain (with the burst-CPU gate now on)
    // must survive many frames without panicking.
    for _ in 0..60 {
        emu.run_frame();
    }
    assert_eq!(emu.frame_count(), 60);
}
