//! End-to-end boot tests for the SNES system skeleton.
//!
//! Each test synthesizes a minimal LoROM / HiROM / ExHiROM image
//! whose only code is a 2-byte `BRA -2` infinite loop at `$00:8000`,
//! then drives the [`SnesEmulator`] end-to-end through `load_rom`,
//! `reset`, and many `run_frame` invocations. The intent is to
//! exercise the cartridge builder -> bus install -> CPU reset ->
//! scheduler -> run-frame chain without depending on commercial
//! ROMs.
//!
//! Commercial games (Super Mario World, Donkey Kong Country, Chrono
//! Trigger, Super Mario RPG, Tales of Phantasia) are checked outside
//! this suite: no copyrighted dumps ship in the repository.

use retro_core::EmulatorCore;
use snes1_core::{PresetKind, SnesEmulator, TimingProfile};

// -----------------------------------------------------------------
// ROM synthesis helpers
// -----------------------------------------------------------------

/// `BRA -2`: branch always to the instruction at PC itself. Two
/// bytes (`80 FE`) encoding "branch always with signed offset -2",
/// which jumps back to the BRA opcode. Standard "halt at the boot
/// vector" idiom in tests.
const RESET_LOOP_BYTES: [u8; 2] = [0x80, 0xFE];

/// Stamp a well-formed SNES header into `chip` at `header_offset`.
/// Sets the title, map mode, ROM size code, destination byte,
/// checksum pair (consistent so it scores `+6`), and emulation-mode
/// reset vector. Other vectors stay zero, the boot tests never
/// take an NMI/IRQ in their tiny BRA loop.
fn stamp_header(
    chip: &mut [u8],
    header_offset: usize,
    map_mode: u8,
    rom_size_code: u8,
    destination: u8,
    reset_vector: u16,
) {
    // +0x10..+0x25: 21-byte ASCII title, space-padded.
    let title = b"BOOT TEST            ";
    chip[header_offset + 0x10..header_offset + 0x10 + 21].copy_from_slice(title);
    chip[header_offset + 0x25] = map_mode;
    chip[header_offset + 0x26] = 0x00; // cartridge_type = ROM only
    chip[header_offset + 0x27] = rom_size_code;
    chip[header_offset + 0x28] = 0x00; // sram_size_code = none
    chip[header_offset + 0x29] = destination;
    chip[header_offset + 0x2A] = 0x01; // developer_id (non-extended)
    chip[header_offset + 0x2B] = 0x00; // rom_version

    // Consistent checksum pair: complement + checksum sum to $FFFF.
    let checksum: u16 = 0x1234;
    let complement: u16 = !checksum;
    chip[header_offset + 0x2C..header_offset + 0x2E].copy_from_slice(&complement.to_le_bytes());
    chip[header_offset + 0x2E..header_offset + 0x30].copy_from_slice(&checksum.to_le_bytes());

    // Emulation-mode reset vector at +0x4C (= header_offset + 0x4C =
    // chip offset $7FFC for LoROM, $FFFC for HiROM, $40FFFC for
    // ExHiROM).
    chip[header_offset + 0x4C..header_offset + 0x4E].copy_from_slice(&reset_vector.to_le_bytes());
}

/// `log2(size / 1024)` for power-of-2 sizes >= 1 KiB. Matches the
/// SNES header's `rom_size_code` semantics
/// (`rom_size_bytes = 1024 << rom_size_code`).
fn rom_size_code_for(size: usize) -> u8 {
    assert!(size >= 1024 && size.is_power_of_two());
    (size.trailing_zeros() - 10) as u8
}

/// 1 MiB LoROM with a reset loop at `$00:8000`. Destination byte
/// controls the timing profile: `0x00` Japan / `0x01` USA -> NTSC,
/// `0x02` Europe -> PAL.
fn synth_minimal_lorom(destination: u8) -> Vec<u8> {
    const SIZE: usize = 0x10_0000; // 1 MiB, power of two
    let mut chip = vec![0u8; SIZE];

    // Reset loop at chip[0..2], visible as `$00:8000` through the
    // LoROM `$00-$7D:8000-FFFF` slot.
    chip[0..2].copy_from_slice(&RESET_LOOP_BYTES);

    // Reset vector at chip[$7FFC..$7FFD] = `00 80` (LE) -> `$8000`.
    chip[0x7FFC] = 0x00;
    chip[0x7FFD] = 0x80;

    // Header at chip[$7FB0..$7FFF]; map_mode `$20` = LoROM,
    // rom_size_code 0x0A (= 1 MiB), reset vector `$8000`.
    stamp_header(
        &mut chip,
        0x7FB0,
        0x20,
        rom_size_code_for(SIZE),
        destination,
        0x8000,
    );
    chip
}

/// 1 MiB HiROM with a reset loop at `$00:8000`.
fn synth_minimal_hirom(destination: u8) -> Vec<u8> {
    const SIZE: usize = 0x10_0000; // 1 MiB
    let mut chip = vec![0u8; SIZE];

    // HiROM mirror low (`$00-$3F:8000-FFFF`) reads from chip
    // `[0x8000..0xFFFF]` for bank 0. Reset loop goes at chip[0x8000].
    chip[0x8000..0x8002].copy_from_slice(&RESET_LOOP_BYTES);

    // Reset vector at chip[$FFFC..$FFFD] = `00 80` -> `$8000`.
    chip[0xFFFC] = 0x00;
    chip[0xFFFD] = 0x80;

    // Header at chip[$FFB0..$FFFF]; map_mode `$21` = HiROM.
    stamp_header(
        &mut chip,
        0xFFB0,
        0x21,
        rom_size_code_for(SIZE),
        destination,
        0x8000,
    );
    chip
}

/// 8 MiB ExHiROM with a reset loop at `$00:8000`. ExHiROM's
/// boot-critical mirror is `$00-$3F:8000-FFFF` -> chip
/// `[0x40_8000..0x40_FFFF]` for bank 0 (because the mirror has
/// `base_offset = 0x40_8000` and full bank stride). Reset loop at
/// chip[0x40_8000].
fn synth_minimal_exhirom(destination: u8) -> Vec<u8> {
    const SIZE: usize = 0x80_0000; // 8 MiB (next power of two over 6 MiB)
    let mut chip = vec![0u8; SIZE];

    chip[0x40_8000..0x40_8002].copy_from_slice(&RESET_LOOP_BYTES);

    // Reset vector at chip[$40_FFFC..$40_FFFD] = `00 80` -> `$8000`.
    chip[0x40_FFFC] = 0x00;
    chip[0x40_FFFD] = 0x80;

    // Header at chip[$40_FFB0..$40_FFFF]; map_mode `$25` = ExHiROM.
    stamp_header(
        &mut chip,
        0x40_FFB0,
        0x25,
        rom_size_code_for(SIZE),
        destination,
        0x8000,
    );
    chip
}

// -----------------------------------------------------------------
// Boot tests, PC after reset
// -----------------------------------------------------------------

#[test]
fn lorom_boots_pc_at_8000() {
    let rom = synth_minimal_lorom(0x01); // USA
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).expect("load_rom should succeed");

    assert_eq!(emu.cpu().pbr, 0x00, "PBR after reset");
    assert_eq!(emu.cpu().pc, 0x8000, "PC after reset");
    assert_eq!(emu.preset_kind(), Some(PresetKind::LoRom));
    assert_eq!(emu.timing(), TimingProfile::NTSC);
}

#[test]
fn hirom_boots_pc_at_8000() {
    let rom = synth_minimal_hirom(0x01);
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).expect("load_rom should succeed");

    assert_eq!(emu.cpu().pbr, 0x00);
    assert_eq!(emu.cpu().pc, 0x8000);
    assert_eq!(emu.preset_kind(), Some(PresetKind::HiRom));
}

#[test]
fn exhirom_boots_pc_at_8000() {
    let rom = synth_minimal_exhirom(0x01);
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).expect("load_rom should succeed");

    assert_eq!(emu.cpu().pbr, 0x00);
    assert_eq!(emu.cpu().pc, 0x8000);
    assert_eq!(emu.preset_kind(), Some(PresetKind::ExHiRom));
}

// -----------------------------------------------------------------
// Boot tests, running frames without panic
// -----------------------------------------------------------------

#[test]
fn lorom_runs_60_frames_without_panic() {
    let rom = synth_minimal_lorom(0x01);
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).unwrap();

    for _ in 0..60 {
        emu.run_frame();
    }
    assert_eq!(emu.frame_count(), 60);
    // 60 frames at NTSC ~ 60 * 357368 = 21_442_080 master clocks.
    // A frame ends on an instruction boundary, so it overshoots by
    // at most one instruction's worth of master clocks, and
    // master_cycles >= the floor.
    let floor = 60 * TimingProfile::NTSC.cycles_per_frame();
    assert!(
        emu.master_cycles() >= floor,
        "master_cycles ({}) must be >= {floor}",
        emu.master_cycles()
    );
    // Hard upper bound: floor + (60 * max-instruction-overshoot),
    // allowing a generous 200 master clocks a frame = floor + 12000.
    // Real BRA-only ROM is much tighter than that (a BRA from slow
    // ROM is 22 master clocks: two 8-clock fetches and one 6-clock
    // internal cycle).
    assert!(
        emu.master_cycles() < floor + 60 * 200,
        "master_cycles ({}) overshot the budget unreasonably",
        emu.master_cycles()
    );
}

#[test]
fn hirom_runs_60_frames_without_panic() {
    let rom = synth_minimal_hirom(0x01);
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).unwrap();
    for _ in 0..60 {
        emu.run_frame();
    }
    assert_eq!(emu.frame_count(), 60);
}

#[test]
fn exhirom_runs_60_frames_without_panic() {
    let rom = synth_minimal_exhirom(0x01);
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).unwrap();
    for _ in 0..60 {
        emu.run_frame();
    }
    assert_eq!(emu.frame_count(), 60);
}

// -----------------------------------------------------------------
// Frame budget and timing-profile selection
// -----------------------------------------------------------------

#[test]
fn master_cycles_match_ntsc_frame_budget() {
    let rom = synth_minimal_lorom(0x01);
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).unwrap();
    emu.run_frame();
    let budget = TimingProfile::NTSC.cycles_per_frame();
    let cycles = emu.master_cycles();
    assert!(
        cycles >= budget && cycles < budget + 200,
        "1 NTSC frame should produce ~{budget} master clocks, got {cycles}"
    );
}

#[test]
fn pal_rom_uses_312_scanlines() {
    let rom = synth_minimal_lorom(0x02); // Europe
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).unwrap();
    assert_eq!(emu.timing(), TimingProfile::PAL);

    emu.run_frame();
    let budget = TimingProfile::PAL.cycles_per_frame();
    let cycles = emu.master_cycles();
    assert!(
        cycles >= budget && cycles < budget + 200,
        "1 PAL frame should produce ~{budget} master clocks, got {cycles}"
    );
    // Sanity: PAL frame is meaningfully longer than NTSC.
    assert!(
        budget > TimingProfile::NTSC.cycles_per_frame(),
        "PAL frame ({budget}) should be longer than NTSC ({})",
        TimingProfile::NTSC.cycles_per_frame()
    );
}

#[test]
fn frame_count_increments_per_run_frame() {
    let rom = synth_minimal_lorom(0x01);
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).unwrap();
    assert_eq!(emu.frame_count(), 0);
    emu.run_frame();
    assert_eq!(emu.frame_count(), 1);
    emu.run_frame();
    emu.run_frame();
    assert_eq!(emu.frame_count(), 3);
}

#[test]
fn instruction_stepping_advances_frames_like_run_frame() {
    // `step_cpu(0)` (the path behind the debugger's single-step and the
    // CLI's instruction-bounded trace/compare) drains the scheduler events
    // that come due and runs the end-of-frame tail. Without that, the
    // frame counter would stay at 0 and VBlank / NMI / HDMA would stop
    // firing after frame 0, so an instruction-stepped timeline would
    // drift from a continuously-running console.
    //
    // Stepping a bare `BRA -2` loop must cross frame boundaries
    // exactly like `run_frame`: same frame count, same master cycle at
    // the boundary (the loop enables no NMI/HDMA, so fired events add no
    // cycles and the two paths must land on the identical instruction).
    let rom = synth_minimal_lorom(0x01); // USA -> NTSC

    let mut framed = SnesEmulator::new();
    framed.load_rom(&rom).unwrap();
    for _ in 0..3 {
        framed.run_frame();
    }
    assert_eq!(framed.frame_count(), 3);

    let mut stepped = SnesEmulator::new();
    stepped.load_rom(&rom).unwrap();
    assert_eq!(stepped.frame_count(), 0);
    let mut steps = 0u64;
    // Generous cap: a 3-cycle BRA from slow ROM is 22 master clocks, three
    // NTSC frames are ~1.07M clocks -> ~49k steps. 500k can't be reached
    // unless the boundary close is broken.
    while stepped.frame_count() < 3 && steps < 500_000 {
        stepped.step_cpu(0);
        steps += 1;
    }
    assert_eq!(
        stepped.frame_count(),
        3,
        "instruction stepping must advance frames via the end-of-frame tail"
    );
    assert_eq!(
        stepped.master_cycles(),
        framed.master_cycles(),
        "stepped and frame-loop runs must close frame 3 at the identical master cycle"
    );
}

#[test]
fn step_returns_master_cycles_after_load() {
    let rom = synth_minimal_lorom(0x01);
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).unwrap();
    let before = emu.master_cycles();
    let result = emu.step();
    assert!(result.instruction_executed);
    assert!(result.cycles > 0, "BRA must consume > 0 master clocks");
    assert_eq!(emu.master_cycles() - before, result.cycles as u64);
}

// -----------------------------------------------------------------
// RomInfo / metadata
// -----------------------------------------------------------------

#[test]
fn rom_info_populated_after_load_lorom() {
    let rom = synth_minimal_lorom(0x01);
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).unwrap();
    let info = emu.get_rom_info().expect("rom_info after load");
    assert_eq!(info.title.trim_end(), "BOOT TEST");
    assert_eq!(info.region, "USA");
    assert_eq!(info.size, 0x10_0000);
    assert_eq!(info.mapper.as_deref(), Some("LoROM"));
    assert_eq!(info.title_confidence, "high");
}

#[test]
fn rom_info_populated_after_load_hirom_europe() {
    let rom = synth_minimal_hirom(0x02);
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).unwrap();
    let info = emu.get_rom_info().expect("rom_info after load");
    assert_eq!(info.region, "Europe");
    assert_eq!(info.mapper.as_deref(), Some("HiROM"));
}

#[test]
fn rom_info_populated_after_load_exhirom() {
    let rom = synth_minimal_exhirom(0x01);
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).unwrap();
    let info = emu.get_rom_info().expect("rom_info after load");
    assert_eq!(info.size, 0x80_0000);
    assert_eq!(info.mapper.as_deref(), Some("ExHiROM"));
    assert_eq!(info.region, "USA");
}

// -----------------------------------------------------------------
// Re-load resets clean
// -----------------------------------------------------------------

#[test]
fn re_loading_a_different_rom_resets_counters() {
    let lorom = synth_minimal_lorom(0x01);
    let mut emu = SnesEmulator::new();
    emu.load_rom(&lorom).unwrap();
    for _ in 0..10 {
        emu.run_frame();
    }
    assert_eq!(emu.frame_count(), 10);

    // Switching cartridges installs the new map plan over a fresh
    // bus state and re-anchors the scheduler / cycle counters.
    let hirom = synth_minimal_hirom(0x02);
    emu.load_rom(&hirom).unwrap();
    assert_eq!(emu.frame_count(), 0);
    assert_eq!(emu.master_cycles(), 0);
    assert_eq!(emu.timing(), TimingProfile::PAL);
    assert_eq!(emu.preset_kind(), Some(PresetKind::HiRom));
    assert_eq!(emu.cpu().pc, 0x8000);
}
