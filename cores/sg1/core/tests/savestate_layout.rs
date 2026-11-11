//! Every savestate layout this core has written keeps loading.
//!
//! A savestate is bincode inside zstd, and bincode reads every field of a
//! struct in order: a field added, removed or moved anywhere in the layout
//! makes every savestate written before unreadable, and `#[serde(default)]`
//! does not help. Players keep their savestates for months, so a change of
//! layout needs a reader for the old one before it ships.
//!
//! `tests/fixtures/savestate-layout/` holds one savestate per layout version,
//! each of the same program, built below and never changed: it sets two data
//! registers, a long word of work RAM, two VDP registers and the first colour
//! of CRAM, then loops on itself. This test loads every file there into an
//! emulator holding that program and checks those values, so a layout that
//! cannot read an older savestate fails here instead of on a player's
//! computer.
//!
//! A new layout takes a new version (`SAVE_STATE_VERSION`) and a reader for
//! the old one; then `SG1_SAVESTATE_LAYOUT_RECORD=1` writes the new version's
//! file. It never writes over a version that already has one.

use std::fs;
use std::path::PathBuf;

use retro_core::EmulatorCore;
use sg1_core::system::savestate::SAVE_STATE_VERSION;
use sg1_core::system::GenesisEmulator;

/// Where the program's loop is: the PC a savestate of it holds.
const LOOP_ADDRESS: u32 = 0x0000_0238;

/// The frames run before a savestate is written.
const FRAMES: u64 = 2;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/savestate-layout")
}

/// The program every fixture holds. Its bytes are the savestate's ROM hash,
/// so they must never change.
fn program_rom() -> Vec<u8> {
    let mut rom = vec![0xFF_u8; 0x400];
    rom[0x000..0x004].copy_from_slice(&0x00FF_FE00_u32.to_be_bytes()); // SSP, in work RAM
    rom[0x004..0x008].copy_from_slice(&0x0000_0200_u32.to_be_bytes()); // PC

    let header = &mut rom[0x100..0x200];
    header[0x00..0x10].copy_from_slice(b"SEGA MEGA DRIVE ");
    header[0x10..0x20].copy_from_slice(b"(C)TEST 2026.NOV");
    let mut title = [b' '; 48];
    title[..16].copy_from_slice(b"SAVESTATE LAYOUT");
    header[0x20..0x50].copy_from_slice(&title);
    header[0x50..0x80].copy_from_slice(&title);
    header[0x80..0x8E].copy_from_slice(b"GM 00000000-00");
    header[0x8E..0x90].copy_from_slice(&[0x00, 0x00]);
    header[0x90..0xA0].copy_from_slice(b"J               ");
    header[0xA0..0xA4].copy_from_slice(&0x0000_0000_u32.to_be_bytes()); // ROM start
    header[0xA4..0xA8].copy_from_slice(&0x0000_03FF_u32.to_be_bytes()); // ROM end
    header[0xA8..0xAC].copy_from_slice(&0x00FF_0000_u32.to_be_bytes()); // RAM start
    header[0xAC..0xB0].copy_from_slice(&0x00FF_FFFF_u32.to_be_bytes()); // RAM end
    header[0xB0..0xF0].fill(b' ');
    header[0xF0..0x100].copy_from_slice(b"JUE             ");

    #[rustfmt::skip]
    let code: [u8; 58] = [
        0x20, 0x3C, 0x11, 0x22, 0x33, 0x44,             // MOVE.L #$11223344,D0
        0x22, 0x3C, 0x55, 0x66, 0x77, 0x88,             // MOVE.L #$55667788,D1
        0x23, 0xFC, 0xCA, 0xFE, 0xBA, 0xBE,             // MOVE.L #$CAFEBABE,
        0x00, 0xFF, 0x00, 0x10,                         //        ($00FF0010).L
        0x33, 0xFC, 0x8F, 0x02, 0x00, 0xC0, 0x00, 0x04, // MOVE.W #$8F02,($C00004).L: auto-increment 2
        0x33, 0xFC, 0x81, 0x44, 0x00, 0xC0, 0x00, 0x04, // MOVE.W #$8144,($C00004).L: display on, no VINT
        0x23, 0xFC, 0xC0, 0x00, 0x00, 0x00,             // MOVE.L #$C0000000,
        0x00, 0xC0, 0x00, 0x04,                         //        ($C00004).L: CRAM write at 0
        0x33, 0xFC, 0x0E, 0xEE, 0x00, 0xC0, 0x00, 0x00, // MOVE.W #$0EEE,($C00000).L
        0x60, 0xFE,                                     // BRA *, at LOOP_ADDRESS
    ];
    rom[0x200..0x200 + code.len()].copy_from_slice(&code);
    rom
}

fn emulator_with_program() -> GenesisEmulator {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(&program_rom()).expect("the program loads");
    emu
}

/// The values the program leaves, checked on a machine restored from a
/// savestate of it.
fn assert_holds_the_program_state(emu: &GenesisEmulator, file: &str) {
    let cpu = emu.cpu();
    assert_eq!(cpu.pc, LOOP_ADDRESS, "{file}: PC");
    assert_eq!(cpu.d[0], 0x1122_3344, "{file}: D0");
    assert_eq!(cpu.d[1], 0x5566_7788, "{file}: D1");
    assert_eq!(cpu.a[7], 0x00FF_FE00, "{file}: A7");
    let ram = emu.bus().memory().work_ram.as_slice();
    assert_eq!(
        &ram[0x10..0x14],
        &[0xCA, 0xFE, 0xBA, 0xBE],
        "{file}: work RAM"
    );
    let registers = emu.vdp().registers();
    assert_eq!(registers[1], 0x44, "{file}: VDP register 1");
    assert_eq!(registers[15], 0x02, "{file}: VDP register 15");
    assert_eq!(emu.vdp().get_cram()[0], 0x0EEE, "{file}: CRAM 0");
    assert_eq!(emu.frame_count(), FRAMES, "{file}: frame count");
}

#[test]
fn the_program_reaches_its_loop_with_its_values() {
    let mut emu = emulator_with_program();
    for _ in 0..FRAMES {
        emu.run_frame_fast();
    }
    assert_holds_the_program_state(&emu, "a fresh run");
}

#[test]
fn every_savestate_layout_written_keeps_loading() {
    let dir = fixture_dir();
    if std::env::var_os("SG1_SAVESTATE_LAYOUT_RECORD").is_some() {
        let path = dir.join(format!("sg1-v{SAVE_STATE_VERSION}.state"));
        assert!(
            !path.exists(),
            "{} exists: a layout is written once, and a new layout takes a new version",
            path.display()
        );
        let mut emu = emulator_with_program();
        for _ in 0..FRAMES {
            emu.run_frame_fast();
        }
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
            .any(|path| path.ends_with(format!("sg1-v{SAVE_STATE_VERSION}.state"))),
        "no savestate of the current layout (version {SAVE_STATE_VERSION}) in {}: \
         write one with SG1_SAVESTATE_LAYOUT_RECORD=1",
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
