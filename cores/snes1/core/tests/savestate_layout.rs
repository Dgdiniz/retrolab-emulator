//! Every savestate layout this core has written keeps loading.
//!
//! A savestate is bincode inside zstd, and bincode reads every field of a
//! struct in order: a field added, removed or moved anywhere in the layout
//! makes every savestate written before unreadable, and `#[serde(default)]`
//! does not help. Players keep their savestates for months, so a change of
//! layout needs a reader for the old one before it ships.
//!
//! `tests/fixtures/savestate-layout/` holds one savestate per layout version,
//! each of the same program, built below and never changed: it writes a VRAM
//! word, the backdrop colour and a byte of WRAM, loads A, X and Y, and loops
//! on itself. This test loads every file there into an
//! emulator holding that program and checks those values, so a layout that
//! cannot read an older savestate fails here instead of on a player's
//! computer.
//!
//! A new layout takes a new version (`SNES_SAVESTATE_VERSION`) and a reader
//! for the old one; then `SNES1_SAVESTATE_LAYOUT_RECORD=1` writes the new
//! version's file. It never writes over a version that already has one.

use std::fs;
use std::path::PathBuf;

use retro_core::EmulatorCore;
use snes1_core::{SnesEmulator, SNES_SAVESTATE_VERSION};

/// Where the program's loop is (bank 0): the PC a savestate of it holds.
const LOOP_ADDRESS: u16 = 0x8034;

/// The frames run before a savestate is written.
const FRAMES: u64 = 2;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/savestate-layout")
}

/// The program every fixture holds: a 128 KiB LoROM. Its bytes are the
/// savestate's ROM hash, so they must never change.
fn program_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x2_0000];

    // The code runs in emulation mode, 8-bit, from $00:8000.
    #[rustfmt::skip]
    let code: [u8; 54] = [
        0xA9, 0x80, 0x8D, 0x00, 0x21,       // LDA #$80 / STA $2100: forced blank
        0x8D, 0x15, 0x21,                   // STA $2115: the address steps after the high byte
        0xA9, 0x00, 0x8D, 0x16, 0x21,       // LDA #$00 / STA $2116
        0x8D, 0x17, 0x21,                   // STA $2117: VRAM address 0
        0xA9, 0xAB, 0x8D, 0x18, 0x21,       // LDA #$AB / STA $2118
        0xA9, 0xCD, 0x8D, 0x19, 0x21,       // LDA #$CD / STA $2119: VRAM word $CDAB
        0xA9, 0x00, 0x8D, 0x21, 0x21,       // LDA #$00 / STA $2121: CGRAM address 0
        0xA9, 0x1F, 0x8D, 0x22, 0x21,       // LDA #$1F / STA $2122
        0xA9, 0x7C, 0x8D, 0x22, 0x21,       // LDA #$7C / STA $2122: backdrop $7C1F
        0xA9, 0x5A, 0x8D, 0x10, 0x00,       // LDA #$5A / STA $0010: WRAM $10
        0xA2, 0x42,                         // LDX #$42
        0xA0, 0x24,                         // LDY #$24
        0xA9, 0x99,                         // LDA #$99
        0x80, 0xFE,                         // BRA *, at LOOP_ADDRESS
    ];
    rom[..code.len()].copy_from_slice(&code);

    // The header, at the LoROM place ($7FB0 in the file).
    let header = 0x7FB0;
    rom[header + 0x10..header + 0x25].copy_from_slice(b"SAVESTATE LAYOUT     ");
    rom[header + 0x25] = 0x20; // LoROM
    rom[header + 0x26] = 0x00; // ROM only
    rom[header + 0x27] = 0x07; // 128 KiB
    rom[header + 0x28] = 0x00; // no SRAM
    rom[header + 0x29] = 0x01; // USA: NTSC
    rom[header + 0x2A] = 0x01;
    rom[header + 0x2B] = 0x00;
    let checksum: u16 = 0x1234;
    rom[header + 0x2C..header + 0x2E].copy_from_slice(&(!checksum).to_le_bytes());
    rom[header + 0x2E..header + 0x30].copy_from_slice(&checksum.to_le_bytes());
    // The emulation-mode vectors: NMI, reset and IRQ. The first and the last
    // point at an RTI; neither is enabled.
    rom[0x0040] = 0x40; // RTI at $8040
    rom[0x7FFA..0x7FFC].copy_from_slice(&0x8040_u16.to_le_bytes());
    rom[0x7FFC..0x7FFE].copy_from_slice(&0x8000_u16.to_le_bytes());
    rom[0x7FFE..0x8000].copy_from_slice(&0x8040_u16.to_le_bytes());
    rom
}

fn emulator_with_program() -> SnesEmulator {
    let mut emu = SnesEmulator::new();
    emu.load_rom(&program_rom()).expect("the program loads");
    emu
}

/// The values the program leaves, checked on a machine restored from a
/// savestate of it.
fn assert_holds_the_program_state(emu: &SnesEmulator, file: &str) {
    let cpu = emu.cpu();
    assert_eq!((cpu.pbr, cpu.pc), (0x00, LOOP_ADDRESS), "{file}: PC");
    assert_eq!(cpu.c & 0x00FF, 0x0099, "{file}: A");
    assert_eq!(cpu.x & 0x00FF, 0x0042, "{file}: X");
    assert_eq!(cpu.y & 0x00FF, 0x0024, "{file}: Y");
    assert_eq!(emu.bus().wram()[0x10], 0x5A, "{file}: WRAM");
    assert_eq!(emu.ppu().vram()[0], 0xCDAB, "{file}: VRAM");
    assert_eq!(emu.ppu().cgram()[0], 0x7C1F, "{file}: CGRAM");
    assert_eq!(emu.frame_count(), FRAMES, "{file}: frame count");
}

fn run_program(emu: &mut SnesEmulator) {
    for _ in 0..FRAMES {
        emu.run_frame();
    }
}

#[test]
fn the_program_reaches_its_loop_with_its_values() {
    let mut emu = emulator_with_program();
    run_program(&mut emu);
    assert_holds_the_program_state(&emu, "a fresh run");
}

#[test]
fn every_savestate_layout_written_keeps_loading() {
    let dir = fixture_dir();
    if std::env::var_os("SNES1_SAVESTATE_LAYOUT_RECORD").is_some() {
        let path = dir.join(format!("snes1-v{SNES_SAVESTATE_VERSION}.state"));
        assert!(
            !path.exists(),
            "{} exists: a layout is written once, and a new layout takes a new version",
            path.display()
        );
        let mut emu = emulator_with_program();
        run_program(&mut emu);
        fs::create_dir_all(&dir).expect("the fixture folder");
        fs::write(&path, emu.save_state()).expect("the fixture is written");
        return;
    }

    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.expect("a fixture entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "state"))
        .collect();
    files.sort();
    assert!(
        files
            .iter()
            .any(|path| path.ends_with(format!("snes1-v{SNES_SAVESTATE_VERSION}.state"))),
        "no savestate of the current layout (version {SNES_SAVESTATE_VERSION}) in {}: \
         write one with SNES1_SAVESTATE_LAYOUT_RECORD=1",
        dir.display()
    );

    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = fs::read(&path).expect("the fixture reads");
        let mut emu = emulator_with_program();
        emu.load_state(&bytes)
            .unwrap_or_else(|e| panic!("{name} no longer loads: {e}"));
        assert_holds_the_program_state(&emu, &name);
    }
}
