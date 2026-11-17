use super::*;

const NTSC_HZ: u64 = 21_477_270;

/// A ROM whose every byte encodes its own offset (low byte), so a read-back
/// uniquely identifies which ROM offset the Super MMC resolved.
fn ramp_rom(len: usize) -> Arc<[u8]> {
    let bytes: Vec<u8> = (0..len).map(|i| (i & 0xFF) as u8).collect();
    Arc::from(bytes.into_boxed_slice())
}

/// Read a ROM byte through the public [`Coprocessor::read`] path, as the
/// bus would after applying the ROM slot's `SA1_ROM_BASE + addr24` base.
fn read_rom_addr(chip: &mut Sa1Chip, addr24: u32) -> u8 {
    chip.read(SA1_ROM_BASE + addr24, 0)
}

fn write_mmio(chip: &mut Sa1Chip, reg: u16, value: u8) {
    chip.write(SA1_MMIO_BASE + u32::from(reg - 0x2200), value, 0);
}

fn read_mmio(chip: &mut Sa1Chip, reg: u16) -> u8 {
    chip.read(SA1_MMIO_BASE + u32::from(reg - 0x2200), 0)
}

#[test]
fn both_bwram_views_are_the_cartridges_ram_and_the_rest_the_chips() {
    let chip = Sa1Chip::new(ramp_rom(0x10_0000), 0x8000, true, NTSC_HZ);
    for offset in [SA1_BWRAM_BASE + 0x10, SA1_BWRAM_LINEAR_BASE + 0x1_2345] {
        assert_eq!(
            chip.source_place(offset),
            SourcePlace::CartridgeRam,
            "{offset:#x}"
        );
    }
    for offset in [
        SA1_MMIO_BASE + 0x09,
        SA1_IRAM_BASE + 0x10,
        SA1_ROM_BASE + 0x00_8000,
    ] {
        assert_eq!(
            chip.source_place(offset),
            SourcePlace::Coprocessor,
            "{offset:#x}"
        );
    }
}

#[test]
fn label_type_and_reset_defaults() {
    let chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    assert_eq!(chip.label(), "SA-1");
    assert_eq!(chip.info().chip_type, ExpansionChipType::SA1);
    // Reset default banking is 0/1/2/3, LoROM-fixed projection.
    assert_eq!(chip.rom_block, [0, 1, 2, 3]);
    assert_eq!(chip.rom_block_mode, [false; 4]);
}

#[test]
fn default_mmc_maps_first_four_mib_linearly() {
    let rom = ramp_rom(0x40_0000); // 4 MiB
    let mut chip = Sa1Chip::new(rom.clone(), 0, false, NTSC_HZ);
    // $00:8000 -> block C LoROM-fixed base 0 -> ROM offset 0.
    assert_eq!(read_rom_addr(&mut chip, 0x00_8000), rom[0]);
    // $01:8000 -> 1 * 0x8000 (32 KiB/bank).
    assert_eq!(read_rom_addr(&mut chip, 0x01_8000), rom[0x8000]);
    // $20:8000 -> block D fixed base 0x100000.
    assert_eq!(read_rom_addr(&mut chip, 0x20_8000), rom[0x10_0000]);
    // $80:8000 -> block E fixed base 0x200000.
    assert_eq!(read_rom_addr(&mut chip, 0x80_8000), rom[0x20_0000]);
    // $A0:8000 -> block F fixed base 0x300000.
    assert_eq!(read_rom_addr(&mut chip, 0xA0_8000), rom[0x30_0000]);
}

#[test]
fn cx_fx_window_is_linear_64k_per_bank() {
    let rom = ramp_rom(0x40_0000);
    let mut chip = Sa1Chip::new(rom.clone(), 0, false, NTSC_HZ);
    // $C0:0000 -> block C selected (bank 0) -> ROM offset 0.
    assert_eq!(read_rom_addr(&mut chip, 0xC0_0000), rom[0]);
    // $C1:0000 -> +0x10000 (64 KiB/bank linear).
    assert_eq!(read_rom_addr(&mut chip, 0xC1_0000), rom[0x1_0000]);
    // $D0:0000 -> block D selected (bank 1) -> 0x100000.
    assert_eq!(read_rom_addr(&mut chip, 0xD0_0000), rom[0x10_0000]);
    // $F0:0000 -> block F selected (bank 3) -> 0x300000.
    assert_eq!(read_rom_addr(&mut chip, 0xF0_0000), rom[0x30_0000]);
}

#[test]
fn mmc_bmode_zero_pins_8000_window_to_fixed_projection() {
    let rom = ramp_rom(0x80_0000); // 8 MiB, blocks 0..7
    let mut chip = Sa1Chip::new(rom.clone(), 0, false, NTSC_HZ);
    // Select block 5 for C but keep bmode 0: the $8000 window stays on the
    // fixed C projection (base 0), while the $Cx window follows the select.
    write_mmio(&mut chip, 0x2220, 5); // cb = 5, cbmode = 0
    assert_eq!(
        read_rom_addr(&mut chip, 0x00_8000),
        rom[0],
        "bmode0 -> fixed"
    );
    assert_eq!(
        read_rom_addr(&mut chip, 0xC0_0000),
        rom[5 << 20],
        "$Cx always follows the select"
    );
}

#[test]
fn mmc_bmode_one_puts_selected_block_in_8000_window() {
    let rom = ramp_rom(0x80_0000);
    let mut chip = Sa1Chip::new(rom.clone(), 0, false, NTSC_HZ);
    // cb = 6, cbmode = 1 (bit7): the $8000 window now projects block 6.
    write_mmio(&mut chip, 0x2220, 0x80 | 6);
    assert_eq!(read_rom_addr(&mut chip, 0x00_8000), rom[6 << 20]);
    assert_eq!(read_rom_addr(&mut chip, 0x01_8000), rom[(6 << 20) + 0x8000]);
}

#[test]
fn iram_is_scpu_visible_only_at_3000_37ff() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    // Write + read back through the I-RAM window (the bus hands the chip
    // SA1_IRAM_BASE + (addr - 0x3000)).
    chip.write(SA1_IRAM_BASE + 0x10, 0xAB, 0);
    assert_eq!(chip.read(SA1_IRAM_BASE + 0x10, 0), 0xAB);
    // The 2 KiB wraps: offset 0x800 aliases offset 0.
    chip.write(SA1_IRAM_BASE, 0xCD, 0);
    assert_eq!(chip.read(SA1_IRAM_BASE + 0x800, 0), 0xCD);
}

#[test]
fn bwram_window_follows_the_2224_block_select() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ); // 32 KiB
                                                                             // Default sbm=0: window byte 0 is BW-RAM byte 0.
    chip.write(SA1_BWRAM_BASE, 0x11, 0);
    assert_eq!(chip.read(SA1_BWRAM_BASE, 0), 0x11);
    // sbm=1: the 8 KiB window now points at BW-RAM offset 0x2000.
    write_mmio(&mut chip, 0x2224, 1);
    chip.write(SA1_BWRAM_BASE, 0x22, 0);
    assert_eq!(chip.read(SA1_BWRAM_BASE, 0), 0x22);
    // The linear view confirms the two writes landed at 0 and 0x2000.
    assert_eq!(chip.read(SA1_BWRAM_LINEAR_BASE, 0), 0x11);
    assert_eq!(chip.read(SA1_BWRAM_LINEAR_BASE + 0x2000, 0), 0x22);
}

#[test]
fn bmap_stores_cbm_and_sw46_bit() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
    // $2225 splits into cbm (bits 0-6) and the sw46 bitmap-mode select
    // (bit 7). A value with bit 7 clear stores cbm only.
    write_mmio(&mut chip, 0x2225, 0x2A);
    assert_eq!(chip.sa1_bwram_block, 0x2A);
    assert!(!chip.bwram_bitmap, "bit 7 clear -> normal window mode");
    // Bit 7 set flips the bitmap-mode select and keeps only bits 0-6 in cbm.
    write_mmio(&mut chip, 0x2225, 0x80 | 0x35);
    assert_eq!(chip.sa1_bwram_block, 0x35, "cbm masked to 7 bits");
    assert!(chip.bwram_bitmap, "bit 7 set -> sw46 (bitmap mode)");
    // The debug row folds sw46 back into bit 7 of the BMAP value.
    let bmap = chip
        .debug_state()
        .registers
        .into_iter()
        .find(|r| r.name == "BMAP")
        .unwrap();
    assert_eq!(bmap.value, 0x35 | (1 << 7));
}

#[test]
fn bwram_linear_view_spans_the_whole_buffer() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x1_0000, true, NTSC_HZ); // 64 KiB
    chip.write(SA1_BWRAM_LINEAR_BASE + 0xABCD, 0x5A, 0);
    assert_eq!(chip.read(SA1_BWRAM_LINEAR_BASE + 0xABCD, 0), 0x5A);
}

#[test]
fn no_bwram_reads_zero_and_drops_writes() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    chip.write(SA1_BWRAM_BASE, 0xFF, 0);
    chip.write(SA1_BWRAM_LINEAR_BASE, 0xFF, 0);
    assert_eq!(chip.read(SA1_BWRAM_BASE, 0), 0);
    assert_eq!(chip.read(SA1_BWRAM_LINEAR_BASE, 0), 0);
}

#[test]
fn mmio_reads_are_stubbed_zero_and_out_of_range_writes_ignored() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    // SFR ($2300) reads back 0 at reset: no flag is set.
    assert_eq!(chip.read(SA1_MMIO_BASE + (0x2300 - 0x2200), 0), 0);
    // A write into the $2300-$23FF read range is a no-op (no panic, no
    // state change): the MMC still reads the default mapping afterwards.
    chip.write(SA1_MMIO_BASE + (0x2301 - 0x2200), 0xFF, 0);
    let rom = ramp_rom(0x40_0000);
    assert_eq!(read_rom_addr(&mut chip, 0x00_8000), rom[0]);
}

#[test]
fn arithmetic_unit_runs_through_the_mmio_path() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    // Multiply (MCNT=0): 100 x 3 = 300 = 0x0000_012C. The $2254 high byte
    // triggers; the 40-bit result reads little-endian at $2306+.
    write_mmio(&mut chip, 0x2250, 0x00);
    write_mmio(&mut chip, 0x2251, 100);
    write_mmio(&mut chip, 0x2252, 0);
    write_mmio(&mut chip, 0x2253, 3);
    write_mmio(&mut chip, 0x2254, 0);
    assert_eq!(read_mmio(&mut chip, 0x2306), 0x2C);
    assert_eq!(read_mmio(&mut chip, 0x2307), 0x01);
    assert_eq!(read_mmio(&mut chip, 0x2308), 0x00);

    // Divide (MCNT=1): 100 / 7 = 14 rem 2 -> quotient at $2306, remainder at
    // the high word ($2308).
    write_mmio(&mut chip, 0x2250, 0x01);
    write_mmio(&mut chip, 0x2251, 100);
    write_mmio(&mut chip, 0x2252, 0);
    write_mmio(&mut chip, 0x2253, 7);
    write_mmio(&mut chip, 0x2254, 0);
    assert_eq!(read_mmio(&mut chip, 0x2306), 14, "quotient");
    assert_eq!(read_mmio(&mut chip, 0x2308), 2, "remainder (bits 16-23)");
}

#[test]
fn savestate_round_trips_the_math_unit() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    // MAC 1 x -1 into a fresh accumulator -> mr = 2^40 - 1, overflow set.
    write_mmio(&mut chip, 0x2250, 0x02); // acm=1 (resets mr)
    write_mmio(&mut chip, 0x2251, 1);
    write_mmio(&mut chip, 0x2252, 0);
    write_mmio(&mut chip, 0x2253, 0xFF);
    write_mmio(&mut chip, 0x2254, 0xFF); // trigger
    let saved = chip.serialize();

    let mut restored = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    restored.restore(&saved, NTSC_HZ);
    assert_eq!(read_mmio(&mut restored, 0x2306), 0xFF);
    assert_eq!(
        read_mmio(&mut restored, 0x230A),
        0xFF,
        "top byte of the 40-bit mr"
    );
    assert_eq!(
        read_mmio(&mut restored, 0x230B),
        0x80,
        "MAC overflow survives"
    );
    assert_eq!(restored.serialize(), saved, "resave is byte-identical");
}

#[test]
fn reset_clears_the_math_unit() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    write_mmio(&mut chip, 0x2250, 0x02);
    write_mmio(&mut chip, 0x2251, 1);
    write_mmio(&mut chip, 0x2253, 0xFF);
    write_mmio(&mut chip, 0x2254, 0xFF); // MAC -> mr + overflow set
    assert_ne!(read_mmio(&mut chip, 0x2306), 0);
    chip.reset(NTSC_HZ);
    assert_eq!(read_mmio(&mut chip, 0x2306), 0, "reset clears MR");
    assert_eq!(
        read_mmio(&mut chip, 0x230B),
        0,
        "reset clears the overflow flag"
    );
}

#[test]
fn battery_gated_on_header_flag() {
    // $35 cart: BW-RAM persists.
    let with = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
    assert_eq!(with.battery().map(<[u8]>::len), Some(0x8000));
    // RAM without a battery: BW-RAM works but does not persist.
    let without = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, false, NTSC_HZ);
    assert!(without.battery().is_none());
    // No RAM at all: no battery.
    let none = Sa1Chip::new(ramp_rom(0x40_0000), 0, true, NTSC_HZ);
    assert!(none.battery().is_none());
}

#[test]
fn battery_round_trips_and_tracks_dirty() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
    assert!(!chip.battery_dirty());
    chip.write(SA1_BWRAM_LINEAR_BASE + 0x100, 0x99, 0);
    assert!(
        chip.battery_dirty(),
        "a real BW-RAM write dirties the battery"
    );
    let mut save = vec![0u8; 0x8000];
    save[0x100] = 0x99;
    save[0x7FFF] = 0x42;
    chip.restore_battery(&save);
    assert!(!chip.battery_dirty(), "restore clears the dirty flag");
    assert_eq!(chip.read(SA1_BWRAM_LINEAR_BASE + 0x7FFF, 0), 0x42);
}

#[test]
fn reset_restores_default_banking_but_keeps_bwram() {
    let mut chip = Sa1Chip::new(ramp_rom(0x80_0000), 0x8000, true, NTSC_HZ);
    write_mmio(&mut chip, 0x2220, 0x80 | 7); // remap block C
    write_mmio(&mut chip, 0x2224, 3); // move the BW-RAM window
    write_mmio(&mut chip, 0x2225, 0x80 | 5); // set cbm + sw46
    chip.write(SA1_BWRAM_LINEAR_BASE + 0x10, 0x7E, 0);
    chip.reset(NTSC_HZ);
    assert_eq!(
        chip.rom_block,
        [0, 1, 2, 3],
        "reset restores default banking"
    );
    assert_eq!(chip.rom_block_mode, [false; 4]);
    assert_eq!(chip.scpu_bwram_block, 0);
    assert_eq!(chip.sa1_bwram_block, 0);
    assert!(!chip.bwram_bitmap, "reset clears the sw46 bitmap select");
    // BW-RAM survives the reset (battery).
    assert_eq!(chip.read(SA1_BWRAM_LINEAR_BASE + 0x10, 0), 0x7E);
}

#[test]
fn accesses_count_all_windows_and_reset_clears() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
    assert_eq!(chip.chip_accesses(), 0);
    read_rom_addr(&mut chip, 0x00_8000); // ROM read
    chip.read(SA1_IRAM_BASE, 0); // I-RAM read
    chip.read(SA1_BWRAM_BASE, 0); // BW-RAM read
    write_mmio(&mut chip, 0x2220, 1); // MMIO write
    assert_eq!(chip.chip_accesses(), 4);
    chip.reset(NTSC_HZ);
    assert_eq!(chip.chip_accesses(), 0);
}

#[test]
fn savestate_round_trips_mapping_and_iram() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
    // Drive every persisted field away from its reset default.
    write_mmio(&mut chip, 0x2220, 0x80 | 4);
    write_mmio(&mut chip, 0x2221, 5);
    write_mmio(&mut chip, 0x2224, 2); // sbm
    write_mmio(&mut chip, 0x2225, 0x80 | 3); // cbm=3 + sw46 (bitmap mode)
    write_mmio(&mut chip, 0x2229, 0x0F); // a protection latch
    chip.write(SA1_IRAM_BASE + 0x40, 0xC3, 0);

    let saved = chip.serialize();
    assert!(!saved.is_empty(), "SA-1 always persists its mapping state");

    let mut restored = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
    restored.restore(&saved, NTSC_HZ);
    assert_eq!(restored.rom_block, chip.rom_block);
    assert_eq!(restored.rom_block_mode, chip.rom_block_mode);
    assert_eq!(restored.scpu_bwram_block, 2);
    assert_eq!(restored.sa1_bwram_block, 3);
    assert!(
        restored.bwram_bitmap,
        "sw46 round-trips through the savestate"
    );
    assert_eq!(restored.write_protection[3], 0x0F); // $2229 -> index 3
    assert_eq!(restored.read(SA1_IRAM_BASE + 0x40, 0), 0xC3);
    // Re-serializing after the restore reproduces the same bytes.
    assert_eq!(restored.serialize(), saved, "resave must be byte-identical");
}

#[test]
fn restore_ignores_garbage_and_foreign_variants() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    write_mmio(&mut chip, 0x2220, 0x80 | 5); // a non-default mapping
    let before = chip.serialize();
    chip.restore(&[1, 2, 3], NTSC_HZ); // garbage
    let foreign = bincode::serialize(&CoprocessorState::Sdd1(
        crate::savestate::Sdd1State::default(),
    ))
    .unwrap();
    chip.restore(&foreign, NTSC_HZ); // foreign variant
    assert_eq!(
        chip.serialize(),
        before,
        "bad blobs leave the chip untouched"
    );
}

#[test]
fn debug_state_surfaces_mmc_and_iram() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
    write_mmio(&mut chip, 0x2221, 0x80 | 2); // db = 2, dbmode = 1
    let st = chip.debug_state();
    assert_eq!(st.chip_type, ExpansionChipType::SA1);
    assert_eq!(st.internal_ram.as_ref().map(Vec::len), Some(IRAM_LEN));
    let names: Vec<&str> = st.registers.iter().map(|r| r.name.as_str()).collect();
    for want in ["CXB", "DXB", "BMAPS", "BWRAM", "BATTERY", "ACCESSES"] {
        assert!(names.contains(&want), "missing debug row {want}");
    }
    let dxb = st.registers.iter().find(|r| r.name == "DXB").unwrap();
    assert_eq!(dxb.value, 2 | (1 << 8), "db=2 with projection mode set");
}

#[test]
fn rom_mirrors_when_offset_exceeds_rom() {
    // A 2 MiB ROM: block E's fixed base (0x200000) wraps modulo the len.
    let rom = ramp_rom(0x20_0000);
    let mut chip = Sa1Chip::new(rom.clone(), 0, false, NTSC_HZ);
    let got = read_rom_addr(&mut chip, 0x80_8000); // fixed base 0x200000
    assert_eq!(got, rom[0x20_0000 % rom.len()]);
}

// -- Second 65816 CPU: wake, catch-up/burst, savestate ----------------

/// Load an SA-1 program into I-RAM starting at `offset`, via the S-CPU I-RAM
/// window. `master_cycle = 0` keeps the SA-1 asleep during setup (a
/// `catch_up(0)` while the anchor is 0 is a monotonic no-op).
fn load_iram(chip: &mut Sa1Chip, offset: u32, program: &[u8]) {
    for (i, &byte) in program.iter().enumerate() {
        chip.write(SA1_IRAM_BASE + offset + i as u32, byte, 0);
    }
}

/// Read an I-RAM byte back through the S-CPU window without advancing the
/// SA-1 (the `catch_up(0)` is a no-op once the anchor is past 0).
fn peek_iram(chip: &mut Sa1Chip, offset: u32) -> u8 {
    chip.read(SA1_IRAM_BASE + offset, 0)
}

/// Point CRV at `vector` and release reset, CCNT with resb (bit 5), rdyb
/// (bit 6) and the IRQ message (bit 7) all clear, which arms the deferred
/// wake (`pending_reset = Some(crv)`).
fn wake_at(chip: &mut Sa1Chip, vector: u16) {
    write_mmio(chip, 0x2203, (vector & 0xFF) as u8);
    write_mmio(chip, 0x2204, (vector >> 8) as u8);
    write_mmio(chip, 0x2200, 0x00);
}

#[test]
fn sa1_wakes_and_executes() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    // LDA #$42 ; STA $10 ; STP  (emulation mode, 8-bit A, D=0).
    load_iram(&mut chip, 0, &[0xA9, 0x42, 0x85, 0x10, 0xDB]);
    wake_at(&mut chip, 0x0000);
    chip.catch_up(1000);
    assert_eq!(peek_iram(&mut chip, 0x10), 0x42, "SA-1 ran its program");
    assert!(chip.cpu.cycles_executed > 0, "the CPU actually stepped");
}

#[test]
fn wake_loads_pc_from_crv() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    // A decoy at offset 0 would write 0xBB; the real program at offset 8
    // writes 0xAA. Executing from CRV=8 proves PC was loaded from CRV.
    load_iram(&mut chip, 0, &[0xA9, 0xBB, 0x85, 0x20, 0xDB]);
    load_iram(&mut chip, 8, &[0xA9, 0xAA, 0x85, 0x20, 0xDB]);
    wake_at(&mut chip, 0x0008);
    chip.catch_up(1000);
    assert_eq!(
        peek_iram(&mut chip, 0x20),
        0xAA,
        "started at CRV, not $0000"
    );
}

#[test]
fn catch_up_is_monotonic() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    load_iram(&mut chip, 0, &[0xA9, 0x42, 0x85, 0x10, 0xDB]);
    wake_at(&mut chip, 0x0000);
    chip.catch_up(1000);
    let anchor = chip.sa1_master;
    let cycles = chip.cpu.cycles_executed;
    assert_eq!(anchor, 1000, "asleep-or-STP burst lands exactly on target");
    // A target at or behind the anchor does nothing.
    chip.catch_up(500);
    chip.catch_up(1000);
    assert_eq!(chip.sa1_master, anchor, "backward/equal targets are no-ops");
    assert_eq!(
        chip.cpu.cycles_executed, cycles,
        "no extra instructions run"
    );
}

#[test]
fn sleeping_consumes_time_without_executing() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    // A program that WOULD write a sentinel, but the SA-1 is never woken.
    load_iram(&mut chip, 0, &[0xA9, 0x99, 0x85, 0x30, 0xDB]);
    chip.catch_up(5000);
    assert_eq!(chip.sa1_master, 5000, "an asleep SA-1 still consumes time");
    assert_eq!(
        chip.cpu.cycles_executed, 0,
        "but never executes an instruction"
    );
    assert_eq!(
        peek_iram(&mut chip, 0x30),
        0,
        "the sentinel was never written"
    );
}

#[test]
fn handshake_via_iram() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    // LDA $10 ; INC A ; STA $11 ; BRA * (spin). Reads a job, writes job+1.
    load_iram(&mut chip, 0, &[0xA5, 0x10, 0x1A, 0x85, 0x11, 0x80, 0xFE]);
    // S-CPU posts the job (SA-1 still asleep, mc=0), then wakes it.
    chip.write(SA1_IRAM_BASE + 0x10, 0x41, 0);
    wake_at(&mut chip, 0x0000);
    // The S-CPU reads the result: the per-access catch-up runs the SA-1
    // forward to this instant, so the response is already there.
    let result = chip.read(SA1_IRAM_BASE + 0x11, 100_000);
    assert_eq!(result, 0x42, "SA-1 answered job+1 via I-RAM");
}

#[test]
fn bwram_access_costs_more_ticks() {
    // Same instruction (STA absolute-long), different target: the BW-RAM
    // store costs the extra wait-state tick that the I-RAM store does not.
    let mut iram_chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
    load_iram(&mut iram_chip, 0, &[0x8F, 0x10, 0x00, 0x00]); // STA $000010 (I-RAM)
    wake_at(&mut iram_chip, 0x0000);
    let cost_iram = iram_chip.test_single_step_cost();

    let mut bwram_chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
    load_iram(&mut bwram_chip, 0, &[0x8F, 0x00, 0x00, 0x40]); // STA $400000 (BW-RAM)
    wake_at(&mut bwram_chip, 0x0000);
    let cost_bwram = bwram_chip.test_single_step_cost();

    assert!(
        cost_bwram > cost_iram,
        "BW-RAM store ({cost_bwram}) must outcost the I-RAM store ({cost_iram})"
    );
}

#[test]
fn savestate_round_trips_cpu_and_anchor() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
    load_iram(
        &mut chip,
        0,
        &[0xA9, 0x33, 0x85, 0x12, 0xA9, 0x44, 0x85, 0x13, 0xDB],
    );
    wake_at(&mut chip, 0x0000);
    chip.catch_up(1000);
    let saved = chip.serialize();

    let mut restored = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
    restored.restore(&saved, NTSC_HZ);
    assert_eq!(restored.sa1_master, chip.sa1_master, "anchor round-trips");
    assert_eq!(restored.reset_vector, chip.reset_vector);
    assert_eq!(restored.held_in_reset, chip.held_in_reset);
    assert_eq!(restored.paused, chip.paused);
    assert_eq!(restored.pending_reset, chip.pending_reset);
    assert_eq!(restored.cpu.pc, chip.cpu.pc, "CPU PC round-trips");
    assert_eq!(restored.cpu.c, chip.cpu.c);
    assert_eq!(restored.cpu.sp, chip.cpu.sp);
    assert_eq!(restored.cpu.cycles_executed, chip.cpu.cycles_executed);
    // The written sentinels survived in I-RAM.
    assert_eq!(peek_iram(&mut restored, 0x12), 0x33);
    assert_eq!(peek_iram(&mut restored, 0x13), 0x44);
    assert_eq!(restored.serialize(), saved, "resave is byte-identical");
}

#[test]
fn debug_state_surfaces_the_second_cpu() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    load_iram(&mut chip, 0, &[0xA9, 0x42, 0x85, 0x10, 0xDB]);
    wake_at(&mut chip, 0x0000);
    chip.catch_up(1000);
    let st = chip.debug_state();
    let names: Vec<&str> = st.registers.iter().map(|r| r.name.as_str()).collect();
    for want in ["SA1_PC", "SA1_A", "SA1_S", "SA1_P", "SA1_E", "SLEEP", "CRV"] {
        assert!(names.contains(&want), "missing SA-1 CPU debug row {want}");
    }
    assert!(st.program_counter.is_some(), "PC surfaces for the CLI dump");
    assert!(st.cycle_count > 0, "cycle_count reflects the SA-1 CPU");
    // Awake: SLEEP row reads 0 (resb+rdyb both clear).
    let sleep = st.registers.iter().find(|r| r.name == "SLEEP").unwrap();
    assert_eq!(sleep.value, 0, "SLEEP clear once the SA-1 is running");
}

// -- Debugger single-step / register snapshot / disassembly -----------

#[test]
fn step_secondary_cpu_advances_one_instruction_at_a_time() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    // LDA #$42 ; STA $10 ; STP  (emulation mode, 8-bit A, D=0).
    load_iram(&mut chip, 0, &[0xA9, 0x42, 0x85, 0x10, 0xDB]);
    wake_at(&mut chip, 0x0000);

    // First step applies the deferred CRV wake and runs LDA #$42.
    let r0 = chip
        .step_secondary_cpu()
        .expect("the SA-1 hosts a steppable CPU");
    assert!(r0.instruction_executed, "one instruction ran");
    assert!(r0.cycles > 0, "the step charged master clocks");
    assert_eq!(chip.cpu.c & 0xFF, 0x42, "A holds the loaded immediate");
    assert_eq!(peek_iram(&mut chip, 0x10), 0, "STA has not run yet");

    // Second step runs STA $10, the store lands only now.
    let r1 = chip.step_secondary_cpu().expect("still hosts a CPU");
    assert!(r1.instruction_executed);
    assert_eq!(
        peek_iram(&mut chip, 0x10),
        0x42,
        "STA ran on the second single-step"
    );
}

#[test]
fn step_secondary_cpu_is_a_noop_while_held_in_reset() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    // A program that WOULD write a sentinel, but the SA-1 is never woken,
    // so resb is held and a single-step must do nothing.
    load_iram(&mut chip, 0, &[0xA9, 0x99, 0x85, 0x30, 0xDB]);
    let r = chip
        .step_secondary_cpu()
        .expect("hosts a CPU even while asleep");
    assert!(!r.instruction_executed, "held in reset: nothing steps");
    assert_eq!(r.cycles, 0, "no clocks charged while parked");
    assert_eq!(
        peek_iram(&mut chip, 0x30),
        0,
        "the sentinel was never written"
    );
}

#[test]
fn secondary_cpu_state_tracks_the_sa1_registers() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    load_iram(&mut chip, 0, &[0xA9, 0x42, 0x85, 0x10, 0xDB]);
    wake_at(&mut chip, 0x0000);

    // The chip renders its CPU through the shared 65816 helper, which
    // stamps cpu_id 0; the console rewrites it to the real slot.
    let before = chip.secondary_cpu_state().expect("hosts a CPU");
    assert_eq!(before.cpu_id, 0, "chip reports the raw 65816 snapshot");

    chip.step_secondary_cpu(); // LDA #$42
    let after = chip.secondary_cpu_state().unwrap();
    let a = after.registers.iter().find(|r| r.name == "A").unwrap();
    assert_eq!(a.value & 0xFF, 0x42, "A surfaces in the register snapshot");
}

#[test]
fn disassemble_secondary_decodes_the_sa1_program() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    // LDA #$42 ; STA $10 at I-RAM $0000 (the SA-1 sees I-RAM low at bank 0).
    load_iram(&mut chip, 0, &[0xA9, 0x42, 0x85, 0x10]);

    // Fresh CPU is in emulation mode -> 8-bit immediate, so LDA #$42 is two
    // bytes and STA $10 follows at $0002.
    let rows = chip.disassemble_secondary(0x0000, 2);
    assert_eq!(rows.len(), 2, "decoded two instructions");
    assert_eq!(rows[0].address, 0x0000);
    assert!(
        rows[0].mnemonic.to_uppercase().contains("LDA"),
        "first row is LDA, got {:?}",
        rows[0].mnemonic
    );
    assert_eq!(rows[1].address, 0x0002, "advanced past the 2-byte LDA #imm");
    assert!(
        rows[1].mnemonic.to_uppercase().contains("STA"),
        "second row is STA, got {:?}",
        rows[1].mnemonic
    );
}

#[test]
fn disassemble_secondary_reads_rom_without_side_effects() {
    // A ROM whose bank-0 $8000 window holds RTS ($60). Decoding it must not
    // latch MMIO or advance any cursor, the peek bus reads ROM inertly.
    let mut rom = vec![0u8; 0x40_0000];
    rom[0] = 0x60; // block C fixed base 0 -> SA-1 $00:8000
    let chip = Sa1Chip::new(Arc::from(rom.into_boxed_slice()), 0, false, NTSC_HZ);
    let rows = chip.disassemble_secondary(0x00_8000, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].address, 0x00_8000);
    assert!(
        rows[0].mnemonic.to_uppercase().contains("RTS"),
        "decoded ROM opcode, got {:?}",
        rows[0].mnemonic
    );
}

// -- Interrupts, H/V timer, DMA ---------------------------------------

/// The SA-1 takes a timer IRQ and vectors through CIV (`$2207/08`), not the
/// `$FFEx` bus vector. The clear latch is disarmed first (CIC) so *only* a
/// timer compare can raise the line, proving the timer->IRQ path.
#[test]
fn sa1_takes_timer_irq_via_civ() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    // ISR at $40 writes a sentinel then halts; main clears I and spins.
    load_iram(&mut chip, 0x40, &[0xA9, 0x5A, 0x85, 0x30, 0xDB]);
    load_iram(&mut chip, 0, &[0x58, 0x80, 0xFE]); // CLI ; BRA *
    write_mmio(&mut chip, 0x2207, 0x40); // CIV -> $0040
    write_mmio(&mut chip, 0x2208, 0x00);
    write_mmio(&mut chip, 0x2212, 0x01); // HCNT = 1 dot (4 clocks)
    write_mmio(&mut chip, 0x2210, 0x01); // TMC: hen
    write_mmio(&mut chip, 0x220B, 0x40); // CIC: disarm timer_irqcl
    write_mmio(&mut chip, 0x220A, 0x40); // CIE: enable timer IRQ
    wake_at(&mut chip, 0x0000);
    chip.catch_up(5000);
    assert_eq!(
        peek_iram(&mut chip, 0x30),
        0x5A,
        "timer IRQ vectored through CIV"
    );
}

/// The same timer IRQ is masked while the SA-1's `I` flag is set: the flag
/// still latches (CFR bit 6) but the ISR never runs.
#[test]
fn timer_irq_is_i_gated() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    load_iram(&mut chip, 0x40, &[0xA9, 0x5A, 0x85, 0x30, 0xDB]);
    load_iram(&mut chip, 0, &[0x80, 0xFE]); // BRA *, never clears I (reset P.I = 1)
    write_mmio(&mut chip, 0x2207, 0x40);
    write_mmio(&mut chip, 0x2208, 0x00);
    write_mmio(&mut chip, 0x2212, 0x01);
    write_mmio(&mut chip, 0x2210, 0x01);
    write_mmio(&mut chip, 0x220B, 0x40);
    write_mmio(&mut chip, 0x220A, 0x40);
    wake_at(&mut chip, 0x0000);
    chip.catch_up(5000);
    assert_ne!(
        read_mmio(&mut chip, 0x2301) & 0x40,
        0,
        "the timer fired (CFR timer flag latched)"
    );
    assert_eq!(
        peek_iram(&mut chip, 0x30),
        0,
        "but the I flag masked the IRQ"
    );
}

/// An S-CPU NMI message (CCNT `$2200` bit 4) is edge-triggered and vectors
/// through CNV (`$2205/06`), ungated by `I`.
#[test]
fn sa1_takes_nmi_via_cnv() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    load_iram(&mut chip, 0x40, &[0xA9, 0xA5, 0x85, 0x31, 0xDB]);
    load_iram(&mut chip, 0, &[0x80, 0xFE]); // spin
    write_mmio(&mut chip, 0x2205, 0x40); // CNV -> $0040
    write_mmio(&mut chip, 0x2206, 0x00);
    wake_at(&mut chip, 0x0000);
    chip.catch_up(500);
    assert_eq!(peek_iram(&mut chip, 0x31), 0, "no NMI before the message");
    // S-CPU posts the NMI while the SA-1 is awake (resb stays 0).
    write_mmio(&mut chip, 0x2200, 0x10);
    chip.catch_up(2000);
    assert_eq!(peek_iram(&mut chip, 0x31), 0xA5, "NMI vectored through CNV");
}

/// The SA-1->S-CPU IRQ line ([`Coprocessor::has_irq`]) is raised by a SCNT
/// (`$2209`) message once SIE (`$2201`) enables it, and lowered by SIC
/// (`$2202`).
#[test]
fn scpu_irq_line_via_scnt_and_sic() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    assert!(!chip.has_irq(), "no IRQ at reset");
    write_mmio(&mut chip, 0x2201, 0x80); // SIE: cpu_irqen (flag still clear -> no line)
    assert!(!chip.has_irq());
    write_mmio(&mut chip, 0x2209, 0x80); // SCNT: cpu_irq message -> line asserts
    assert!(chip.has_irq(), "SCNT bit 7 raises the S-CPU IRQ line");
    write_mmio(&mut chip, 0x2202, 0x80); // SIC: cpu_irqcl -> line drops
    assert!(!chip.has_irq(), "SIC lowers the S-CPU IRQ line");
}

/// VSPROM: the S-CPU's own native NMI/IRQ vectors read SNV/SIV when SCNT
/// (`$2209`) selects the override (bits 4/6), and ROM otherwise. Only the
/// native pair (`$FFEA/EB`, `$FFEE/EF`) is overridden, the emulation pair
/// (`$FFFx`) always reads ROM.
#[test]
fn vsprom_overrides_native_scpu_vectors() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    let rom_nmi = read_rom_addr(&mut chip, 0x00_FFEA);
    let rom_irq = read_rom_addr(&mut chip, 0x00_FFEE);
    let rom_emu_nmi = read_rom_addr(&mut chip, 0x00_FFFA);
    write_mmio(&mut chip, 0x220C, 0xBC); // SNV = 0x9ABC
    write_mmio(&mut chip, 0x220D, 0x9A);
    write_mmio(&mut chip, 0x220E, 0xF0); // SIV = 0xDEF0
    write_mmio(&mut chip, 0x220F, 0xDE);
    // Overrides disabled -> ROM.
    assert_eq!(read_rom_addr(&mut chip, 0x00_FFEA), rom_nmi);
    assert_eq!(read_rom_addr(&mut chip, 0x00_FFEE), rom_irq);
    // Enable nvsw (bit 4) + ivsw (bit 6), no message (bit 7 clear).
    write_mmio(&mut chip, 0x2209, 0x50);
    assert_eq!(read_rom_addr(&mut chip, 0x00_FFEA), 0xBC, "NMI lo -> SNV");
    assert_eq!(read_rom_addr(&mut chip, 0x00_FFEB), 0x9A, "NMI hi -> SNV");
    assert_eq!(read_rom_addr(&mut chip, 0x00_FFEE), 0xF0, "IRQ lo -> SIV");
    assert_eq!(read_rom_addr(&mut chip, 0x00_FFEF), 0xDE, "IRQ hi -> SIV");
    assert_eq!(
        read_rom_addr(&mut chip, 0x00_FFFA),
        rom_emu_nmi,
        "emulation NMI vector is not VSPROM-overridden"
    );
}

/// A normal DMA copies ROM -> I-RAM instantaneously on the `$2236` (I-RAM
/// destination) trigger and latches the DMA-end flag (CFR bit 5).
#[test]
fn dma_normal_rom_to_iram() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    write_mmio(&mut chip, 0x2230, 0x80); // DCNT: dmaen, cden=0, dd=IRAM, sd=ROM
    write_mmio(&mut chip, 0x2232, 0x00); // SDA = $008000 (ROM window -> ramp 0,1,2,3)
    write_mmio(&mut chip, 0x2233, 0x80);
    write_mmio(&mut chip, 0x2234, 0x00);
    write_mmio(&mut chip, 0x2238, 0x04); // DTC = 4
    write_mmio(&mut chip, 0x2239, 0x00);
    write_mmio(&mut chip, 0x2235, 0x10); // DDA lo = I-RAM offset $10
    write_mmio(&mut chip, 0x2236, 0x00); // DDA mid -> fires the transfer
    for i in 0..4u32 {
        assert_eq!(peek_iram(&mut chip, 0x10 + i), i as u8, "copied byte {i}");
    }
    assert_ne!(read_mmio(&mut chip, 0x2301) & 0x20, 0, "dma_irqfl latched");
}

/// A copy within one memory (I-RAM -> I-RAM) moves nothing, yet still
/// raises the DMA-end flag.
#[test]
fn dma_normal_same_device_is_skipped() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    load_iram(&mut chip, 0x00, &[0x11]); // source sentinel
    load_iram(&mut chip, 0x40, &[0x22]); // destination sentinel
    write_mmio(&mut chip, 0x2230, 0x82); // DCNT: dmaen, dd=IRAM(0), sd=IRAM(2)
    write_mmio(&mut chip, 0x2232, 0x00);
    write_mmio(&mut chip, 0x2233, 0x00);
    write_mmio(&mut chip, 0x2234, 0x00); // SDA = $000000
    write_mmio(&mut chip, 0x2238, 0x04);
    write_mmio(&mut chip, 0x2239, 0x00); // DTC = 4
    write_mmio(&mut chip, 0x2235, 0x40); // DDA lo = $40
    write_mmio(&mut chip, 0x2236, 0x00); // trigger
    assert_eq!(
        peek_iram(&mut chip, 0x40),
        0x22,
        "I-RAM -> I-RAM transfer skipped"
    );
    assert_ne!(
        read_mmio(&mut chip, 0x2301) & 0x20,
        0,
        "dma_irqfl still set"
    );
}

/// The full timer + interrupt-controller + DMA-register-file state
/// round-trips through the savestate and re-serializes byte-identically,
/// including mid-execution timer counters and a live S-CPU IRQ line.
#[test]
fn savestate_round_trips_timer_irq_dma() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
    write_mmio(&mut chip, 0x2210, 0x01); // TMC: hen
    write_mmio(&mut chip, 0x2212, 0x20); // HCNT
    write_mmio(&mut chip, 0x2214, 0x05); // VCNT
    write_mmio(&mut chip, 0x2205, 0x34); // CNV = 0x1234
    write_mmio(&mut chip, 0x2206, 0x12);
    write_mmio(&mut chip, 0x2207, 0x78); // CIV = 0x5678
    write_mmio(&mut chip, 0x2208, 0x56);
    write_mmio(&mut chip, 0x220C, 0xBC); // SNV = 0x9ABC
    write_mmio(&mut chip, 0x220D, 0x9A);
    write_mmio(&mut chip, 0x220E, 0xF0); // SIV = 0xDEF0
    write_mmio(&mut chip, 0x220F, 0xDE);
    write_mmio(&mut chip, 0x2201, 0x80); // SIE: cpu_irqen
    write_mmio(&mut chip, 0x2209, 0x80); // SCNT message -> scpu_irq
    write_mmio(&mut chip, 0x220A, 0x40); // CIE: timer_irqen
    write_mmio(&mut chip, 0x2230, 0x00); // DCNT (dmaen=0: store, no trigger)
    write_mmio(&mut chip, 0x2232, 0x56); // SDA = 0x123456
    write_mmio(&mut chip, 0x2233, 0x34);
    write_mmio(&mut chip, 0x2234, 0x12);
    write_mmio(&mut chip, 0x2238, 0x42); // DTC lo
    for i in 0..16u16 {
        write_mmio(&mut chip, 0x2240 + i, (0xF0 | i) as u8); // BRF
    }
    chip.catch_up(3000); // asleep, but the H/V timer advances

    let saved = chip.serialize();
    let mut restored = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
    restored.restore(&saved, NTSC_HZ);
    assert_eq!(restored.serialize(), saved, "resave is byte-identical");
    assert!(
        restored.has_irq(),
        "the S-CPU IRQ line survives the savestate"
    );
    let rows = restored.debug_state().registers;
    let field = |name: &str| rows.iter().find(|r| r.name == name).unwrap().value;
    assert_eq!(field("CIV"), 0x5678);
    assert_eq!(field("CNV"), 0x1234);
    assert_eq!(field("SIV"), 0xDEF0);
    assert_eq!(field("SDA"), 0x12_3456);
}

/// The H/V timer keeps counting while the SA-1 CPU is asleep, so a target
/// reached during sleep is waiting when it wakes.
#[test]
fn timer_advances_while_asleep() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    write_mmio(&mut chip, 0x2210, 0x01); // hen
    write_mmio(&mut chip, 0x2212, 0x10); // HCNT = 16 dots (64 clocks)
    write_mmio(&mut chip, 0x220B, 0x40); // disarm
    write_mmio(&mut chip, 0x220A, 0x40); // enable timer IRQ
                                         // Never woken: the asleep branch still advances the timer.
    chip.catch_up(5000);
    assert_ne!(
        read_mmio(&mut chip, 0x2301) & 0x40,
        0,
        "timer flag latched while asleep"
    );
}

/// Arming online CC-DMA type 1 (writing `$2236` with `cden && cdsel`) raises
/// the CHDMA IRQ to the S-CPU, and the S-CPU's subsequent BW-RAM read
/// converts the bitmap character into I-RAM. 2bpp: a single low bit in the
/// source lands in plane 0, column 0 (output bit 7).
#[test]
fn cc1_arm_raises_irq_and_converts_on_read() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
    // Seed BW-RAM byte 0 (the char source) with pixel-0 plane-0 set.
    chip.write(SA1_BWRAM_LINEAR_BASE, 0x01, 0);
    write_mmio(&mut chip, 0x2201, 0x20); // SIE: chdma_irqen
    assert!(!chip.has_irq(), "no IRQ before the arm");
    write_mmio(&mut chip, 0x2230, 0xB0); // DCNT: dmaen + cden + cdsel
    write_mmio(&mut chip, 0x2231, 0x02); // CDMA: dmacb = 2 (2bpp)
    write_mmio(&mut chip, 0x2232, 0x00); // dsa = 0
    write_mmio(&mut chip, 0x2233, 0x00);
    write_mmio(&mut chip, 0x2234, 0x00);
    write_mmio(&mut chip, 0x2235, 0x00); // dda lo = 0
    write_mmio(&mut chip, 0x2236, 0x00); // dda mid -> arms CC1 + raises CHDMA IRQ
    assert!(chip.dma.conversion_armed(), "the $2236 write armed CC1");
    assert!(chip.has_irq(), "CHDMA IRQ asserts the S-CPU IRQ line");

    let byte = chip.read(SA1_BWRAM_LINEAR_BASE, 0);
    assert_eq!(
        byte, 0x80,
        "boundary read returns the buffered plane-0 byte"
    );
    assert_eq!(peek_iram(&mut chip, 0), 0x80, "I-RAM plane 0, column 0");
    assert_eq!(peek_iram(&mut chip, 1), 0x00, "I-RAM plane 1 clear");
}

/// CC-DMA type 1 also converts through the S-CPU's 8 KiB BW-RAM window
/// (`$6000-7FFF`), folding the window into BW-RAM via `sbm`. 4bpp: a full low
/// nibble sets column 0 across all four planes (planar indices `{0,1,16,17}`).
#[test]
fn cc1_converts_through_windowed_view_4bpp() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
    write_mmio(&mut chip, 0x2224, 0x01); // sbm = 1 -> window maps to BW-RAM $2000
    chip.write(SA1_BWRAM_LINEAR_BASE + 0x2000, 0x0F, 0); // colour 15 at the source
    write_mmio(&mut chip, 0x2230, 0xB0); // dmaen + cden + cdsel
    write_mmio(&mut chip, 0x2231, 0x01); // dmacb = 1 (4bpp)
    write_mmio(&mut chip, 0x2232, 0x00); // dsa = 0x2000
    write_mmio(&mut chip, 0x2233, 0x20);
    write_mmio(&mut chip, 0x2234, 0x00);
    write_mmio(&mut chip, 0x2235, 0x00); // dda = 0
    write_mmio(&mut chip, 0x2236, 0x00); // arm

    // Read the window at $6000 (sbm=1 -> cartram offset 0x2000 = dsa, a
    // char boundary), which buffers the converted tile into I-RAM.
    let byte = chip.read(SA1_BWRAM_BASE, 0);
    assert_eq!(byte, 0x80);
    for &p in &[0u32, 1, 16, 17] {
        assert_eq!(peek_iram(&mut chip, p), 0x80, "plane at planar index {p}");
    }
    assert_eq!(peek_iram(&mut chip, 32), 0x00, "8bpp-only plane untouched");
}

/// A `chdend` write (`$2231` bit 7) ends the online run: the armed flag
/// drops and the S-CPU's BW-RAM reads return the raw byte again.
#[test]
fn cc1_chdend_disarms_and_reads_go_raw() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
    chip.write(SA1_BWRAM_LINEAR_BASE, 0x01, 0);
    write_mmio(&mut chip, 0x2230, 0xB0);
    write_mmio(&mut chip, 0x2231, 0x02); // 2bpp
    write_mmio(&mut chip, 0x2236, 0x00); // arm
    assert_eq!(
        chip.read(SA1_BWRAM_LINEAR_BASE, 0),
        0x80,
        "armed -> converted"
    );

    write_mmio(&mut chip, 0x2231, 0x80); // chdend -> disarm
    assert!(!chip.dma.conversion_armed());
    assert_eq!(
        chip.read(SA1_BWRAM_LINEAR_BASE, 0),
        0x01,
        "disarmed -> raw BW-RAM byte"
    );
}

/// The CC1 armed flag, the BW-RAM bitmap format (`bbf`), and the SA-1 bitmap
/// window select (`sw46`) all round-trip through the savestate.
#[test]
fn savestate_round_trips_cc1_and_bitmap() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
    write_mmio(&mut chip, 0x223F, 0x80); // bbf = 2bpp
    write_mmio(&mut chip, 0x2225, 0x80 | 0x05); // sw46 + cbm = 5
    write_mmio(&mut chip, 0x2230, 0xB0); // dmaen + cden + cdsel
    write_mmio(&mut chip, 0x2231, 0x02); // dmacb = 2
    write_mmio(&mut chip, 0x2235, 0x00);
    write_mmio(&mut chip, 0x2236, 0x00); // arm CC1
    assert!(chip.dma.conversion_armed());

    let saved = chip.serialize();
    let mut restored = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
    restored.restore(&saved, NTSC_HZ);
    assert_eq!(restored.serialize(), saved, "resave is byte-identical");
    assert!(restored.dma.conversion_armed(), "cc1_armed restored");
    assert!(restored.dma.bitmap_2bpp(), "bbf restored");
    assert!(restored.bwram_bitmap, "sw46 restored");
}

/// Reset disarms a type-1 conversion that was armed.
#[test]
fn reset_clears_cc1_armed() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
    write_mmio(&mut chip, 0x2230, 0xB0);
    write_mmio(&mut chip, 0x2231, 0x02);
    write_mmio(&mut chip, 0x2236, 0x00); // arm
    assert!(chip.dma.conversion_armed());
    chip.reset(NTSC_HZ);
    assert!(!chip.dma.conversion_armed(), "reset disarms CC1");
}

/// Point the reader at a ROM address (`va`) and set a 24-bit stream cursor.
fn set_vda(chip: &mut Sa1Chip, va: u32) {
    write_mmio(chip, 0x2259, (va & 0xFF) as u8);
    write_mmio(chip, 0x225A, ((va >> 8) & 0xFF) as u8);
    write_mmio(chip, 0x225B, ((va >> 16) & 0xFF) as u8); // also zeroes vbit
}

/// The variable-length reader pulls its bytes through the Super MMC from ROM
/// (`$C0-FF` linear), and VDPH in auto-increment mode steps the cursor to the
/// next field. The ramp ROM yields byte == offset.
#[test]
fn varbit_extracts_rom_field_and_auto_increments() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    // $C0:0000 -> block C bank 0 -> ROM offset 0, so va+n reads ROM[n].
    set_vda(&mut chip, 0xC0_0000);
    write_mmio(&mut chip, 0x2258, 0x88); // hl = 1 (auto), vb = 8
                                         // vbit = 0: VDPL = ROM[0], VDPH high byte = ROM[1]. VDPL never advances.
    assert_eq!(read_mmio(&mut chip, 0x230C), 0x00, "VDPL = ROM[0]");
    assert_eq!(
        read_mmio(&mut chip, 0x230C),
        0x00,
        "VDPL is side-effect-free"
    );
    // Reading VDPH advances the cursor by the width (8 bits -> one byte).
    assert_eq!(
        read_mmio(&mut chip, 0x230D),
        0x01,
        "VDPH high byte = ROM[1]"
    );
    assert_eq!(
        read_mmio(&mut chip, 0x230C),
        0x01,
        "cursor advanced to ROM[1]"
    );
    assert_eq!(read_mmio(&mut chip, 0x230D), 0x02, "and again to ROM[2]");
}

/// The bit reader's view (`Sa1Chip::vbr_read`) maps I-RAM (`$0000-07FF`)
/// and the *linear* BW-RAM views (`$40-4F` and the tiled
/// `$00-3F:6000-7FFF` window). A 4-bit fixed-mode shift extracts a field
/// straddling two I-RAM bytes.
#[test]
fn varbit_reads_iram_and_bwram_via_vbrbus() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0x8000, true, NTSC_HZ);
    // I-RAM stream 0x21 0x43 0x65 -> little-endian window 0x654321.
    load_iram(&mut chip, 0, &[0x21, 0x43, 0x65]);
    set_vda(&mut chip, 0x00_0000); // $00:0000 -> I-RAM low mirror, vbit = 0
    write_mmio(&mut chip, 0x2258, 0x04); // fixed, vb = 4 -> advance parks vbit at 4
                                         // window >> 4 = 0x065432 -> VDPL = 0x32, VDPH = 0x54 (fixed VDPH is inert).
    assert_eq!(read_mmio(&mut chip, 0x230C), 0x32, "I-RAM cross-byte low");
    assert_eq!(read_mmio(&mut chip, 0x230D), 0x54, "I-RAM cross-byte high");

    // Linear BW-RAM at $40:0100.
    chip.write(SA1_BWRAM_LINEAR_BASE + 0x100, 0xBE, 0);
    chip.write(SA1_BWRAM_LINEAR_BASE + 0x101, 0xEF, 0);
    set_vda(&mut chip, 0x40_0100);
    write_mmio(&mut chip, 0x2258, 0x88); // hl = 1, vb = 8
    assert_eq!(read_mmio(&mut chip, 0x230C), 0xBE, "linear BW-RAM $40:0100");
    assert_eq!(read_mmio(&mut chip, 0x230D), 0xEF, "linear BW-RAM $40:0101");

    // The tiled `$00:6000` window aliases BW-RAM offset 0 (8 KiB/bank).
    chip.write(SA1_BWRAM_LINEAR_BASE, 0x77, 0);
    set_vda(&mut chip, 0x00_6000);
    write_mmio(&mut chip, 0x2258, 0x88);
    assert_eq!(read_mmio(&mut chip, 0x230C), 0x77, "$00:6000 -> BW-RAM[0]");
}

/// The bit reader's view (`Sa1Chip::vbr_read`) deliberately does NOT map
/// MMIO: a stream cursor pointed at a register reads open bus (the CPU
/// MDR, `0` on a fresh chip), not the register: the view never reaches
/// `$2200-23FF`.
#[test]
fn varbit_vbrbus_excludes_mmio() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    // VC ($230E) reads 0x01 through the normal MMIO path.
    assert_eq!(read_mmio(&mut chip, 0x230E), 0x01);
    // Point the reader at $00:230E; its view returns open bus, not 0x01.
    set_vda(&mut chip, 0x00_230E);
    write_mmio(&mut chip, 0x2258, 0x88); // hl = 1, vb = 8
    assert_eq!(
        read_mmio(&mut chip, 0x230C),
        0x00,
        "VBRBus reads open bus at an MMIO address, not the register"
    );
}

/// Fixed mode advances the cursor on the VBD write, so writing a fresh width
/// before each read walks a byte-packed ROM stream.
#[test]
fn varbit_fixed_mode_advances_on_vbd_write() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    set_vda(&mut chip, 0xC0_0000); // va = ROM offset 0
    write_mmio(&mut chip, 0x2258, 0x08); // fixed, vb = 8 -> va steps to ROM[1]
    assert_eq!(read_mmio(&mut chip, 0x230C), 0x01, "after write -> ROM[1]");
    write_mmio(&mut chip, 0x2258, 0x08); // -> ROM[2]
    assert_eq!(read_mmio(&mut chip, 0x230C), 0x02, "next write -> ROM[2]");
}

/// The reader cursor / width / mode survive a savestate round-trip: a
/// restored chip resumes the stream at the same ROM byte.
#[test]
fn varbit_savestate_round_trips_through_mmio() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    set_vda(&mut chip, 0xC0_0005); // va -> ROM[5]
    write_mmio(&mut chip, 0x2258, 0x88); // hl = 1, vb = 8
    let saved = chip.serialize();

    let mut restored = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    restored.restore(&saved, NTSC_HZ);
    assert_eq!(read_mmio(&mut restored, 0x230C), 0x05, "resumes at ROM[5]");
    assert!(restored.varbit.advances_on_read(), "advance mode restored");
    assert_eq!(restored.varbit.width(), 8, "field width restored");
    assert_eq!(restored.serialize(), saved, "resave is byte-identical");
}

/// Reset returns the reader to power-on: the cursor, width, and mode clear.
#[test]
fn varbit_reset_clears_reader() {
    let mut chip = Sa1Chip::new(ramp_rom(0x40_0000), 0, false, NTSC_HZ);
    set_vda(&mut chip, 0xC0_1234);
    write_mmio(&mut chip, 0x2258, 0x8C); // hl = 1, vb = 12
    assert_ne!(chip.varbit.address(), 0);
    chip.reset(NTSC_HZ);
    assert_eq!(chip.varbit.address(), 0, "cursor cleared");
    assert_eq!(chip.varbit.width(), 0, "width cleared");
    assert!(!chip.varbit.advances_on_read(), "advance mode cleared");
}
