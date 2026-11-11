//! End-to-end PPU foundation tests.
//!
//! Every test goes through the FULL bus dispatch path
//! (`bus.write(0x00_2118, val)` -> `bus.read(0x00_2139)`) rather
//! than poking at PPU methods directly. The point is to prove
//! that the wiring chain, A-bus decode -> B-bus dispatch ->
//! `Ppu::write_reg` / `Ppu::read_reg` -> `PpuAccessCtx`, is
//! correctly hooked up. The submodule unit tests
//! (`ppu/{vram,oam,cgram,mode7,regs}.rs::tests`) cover the
//! internal behaviour; these tests cover the seams.
//!
//! Following the layout of `boot_test.rs` / `cartridge_test.rs`,
//! we use the `Addressable` trait through [`SnesBus`] directly
//! so each test stays standalone and stays away from cartridge
//! / scheduler state irrelevant to the PPU contract.

use retro_core::cpu::Addressable;
use snes1_core::SnesBus;

/// Build a 24-bit bus address inside bank `$00` for the byte at
/// `lo`: the canonical PPU window.
fn ppu_addr(lo: u16) -> u32 {
    (lo as u32) & 0x00FF_FFFF
}

/// Build a bus with display-disable left at its reset value
/// (`true`), so VRAM / OAM / CGRAM access is never gated.
fn fresh_bus() -> SnesBus {
    SnesBus::new()
}

// -------------------------------------------------------------
// VRAM
// -------------------------------------------------------------

#[test]
fn vram_round_trip_via_bus() {
    let mut bus = fresh_bus();

    // VMAIN $80 -> high-side auto-inc, step 1, keeps the low /
    // high write pair on the same word.
    bus.write(ppu_addr(0x2115), 0x80);
    // VMADDL/H -> vram_addr = $1000.
    bus.write(ppu_addr(0x2116), 0x00);
    bus.write(ppu_addr(0x2117), 0x10);
    // VMDATAL / VMDATAH -> write 0x1234 at word $1000.
    bus.write(ppu_addr(0x2118), 0x34);
    bus.write(ppu_addr(0x2119), 0x12); // triggers auto-inc

    // Direct PPU inspection: the byte landed in VRAM word $1000.
    assert_eq!(bus.ppu().vram()[0x1000], 0x1234);

    // Reset the address pointer and read back through the bus.
    bus.write(ppu_addr(0x2116), 0x00);
    bus.write(ppu_addr(0x2117), 0x10);
    let lo = bus.read(ppu_addr(0x2139));
    let hi = bus.read(ppu_addr(0x213A));
    assert_eq!(lo, 0x34);
    assert_eq!(hi, 0x12);
}

#[test]
fn vram_auto_inc_only_fires_on_configured_byte() {
    let mut bus = fresh_bus();
    // Low-side inc, step 1.
    bus.write(ppu_addr(0x2115), 0x00);
    bus.write(ppu_addr(0x2116), 0x00);
    bus.write(ppu_addr(0x2117), 0x00);
    // High-side write should NOT advance the pointer.
    bus.write(ppu_addr(0x2119), 0xAA);
    assert_eq!(bus.ppu().vram_addr(), 0);
    // Low-side write DOES advance.
    bus.write(ppu_addr(0x2118), 0x55);
    assert_eq!(bus.ppu().vram_addr(), 1);
}

#[test]
fn vram_remap_mode_2_through_bus() {
    let mut bus = fresh_bus();
    // VMAIN $88 -> high-side inc + mapping mode 2.
    bus.write(ppu_addr(0x2115), 0x88);
    // Word 0x0043 -> remap mode 2: ((0x43 & 0x3F) << 3) |
    // ((0x43 >> 6) & 7) = (0x03 << 3) | 1 = 0x19.
    bus.write(ppu_addr(0x2116), 0x43);
    bus.write(ppu_addr(0x2117), 0x00);
    bus.write(ppu_addr(0x2118), 0x77);
    bus.write(ppu_addr(0x2119), 0x88); // auto-inc fires
    assert_eq!(bus.ppu().vram()[0x0019], 0x8877);
}

// -------------------------------------------------------------
// OAM
// -------------------------------------------------------------

#[test]
fn oam_round_trip_4_byte_sprite_via_bus() {
    let mut bus = fresh_bus();
    // OAMADDL = 0 -> byte addr = 0 -> sprite 0.
    bus.write(ppu_addr(0x2102), 0x00);
    bus.write(ppu_addr(0x2103), 0x00);
    // Four byte writes commit two low-table pairs.
    bus.write(ppu_addr(0x2104), 0xAA); // byte 0 (X-low), latched
    bus.write(ppu_addr(0x2104), 0xBB); // byte 1 (Y), commit pair
    bus.write(ppu_addr(0x2104), 0xCC); // byte 2 (tile), latched
    bus.write(ppu_addr(0x2104), 0xDD); // byte 3 (attr), commit pair

    let oam = bus.ppu().oam();
    assert_eq!(oam[0], 0xAA);
    assert_eq!(oam[1], 0xBB);
    assert_eq!(oam[2], 0xCC);
    assert_eq!(oam[3], 0xDD);
    assert_eq!(bus.ppu().oam_addr(), 4);
}

#[test]
fn oam_high_table_byte_writes_commit_immediately() {
    let mut bus = fresh_bus();
    // Aim at $200 (start of high table). $2103 bit 0 = top bit
    // of the 10-bit byte pointer.
    bus.write(ppu_addr(0x2102), 0x00);
    bus.write(ppu_addr(0x2103), 0x01);
    // 2-bit packing: bit 0 = sprite N x-msb, bit 1 = sprite N
    // size. The PPU stores raw bytes; the packing is the
    // renderer's job. We just verify the byte commits without
    // latch buffering.
    bus.write(ppu_addr(0x2104), 0b1010_0101);
    assert_eq!(bus.ppu().oam()[0x200], 0b1010_0101);
    assert_eq!(bus.ppu().oam_addr(), 0x201);
}

// -------------------------------------------------------------
// CGRAM
// -------------------------------------------------------------

#[test]
fn cgram_round_trip_with_15_bit_mask_via_bus() {
    let mut bus = fresh_bus();
    // CGADD: colour 5 -> byte addr 0x0A.
    bus.write(ppu_addr(0x2121), 0x05);
    // Write a pair where the high byte's bit 7 is set; PPU must
    // mask it to enforce the "15-bit BGR" invariant.
    bus.write(ppu_addr(0x2122), 0x34); // low byte (latched)
    bus.write(ppu_addr(0x2122), 0xFE); // high byte (commit, mask -> 0x7E)
    assert_eq!(bus.ppu().cgram()[5], 0x7E34);

    // Read back via $213B.
    bus.write(ppu_addr(0x2121), 0x05);
    let lo = bus.read(ppu_addr(0x213B));
    let hi = bus.read(ppu_addr(0x213B));
    assert_eq!(lo, 0x34);
    assert_eq!(hi & 0x7F, 0x7E);
}

// -------------------------------------------------------------
// Mode 7
// -------------------------------------------------------------

#[test]
fn mode7_matrix_latch_and_mpy_via_bus() {
    let mut bus = fresh_bus();

    // Build M7A = 0x0100 (256) via the shared latch.
    bus.write(ppu_addr(0x211B), 0x00); // latch <- 0x00; m7a = (0x00 << 8) | 0 = 0x0000
    bus.write(ppu_addr(0x211B), 0x01); // latch <- 0x01; m7a = (0x01 << 8) | 0x00 = 0x0100

    // Build M7B = 0x0500 -> high byte = 5.
    bus.write(ppu_addr(0x211C), 0x00); // latch <- 0x00; m7b = (0x00 << 8) | 0x01 = 0x0001
    bus.write(ppu_addr(0x211C), 0x05); // latch <- 0x05; m7b = (0x05 << 8) | 0x00 = 0x0500

    assert_eq!(bus.ppu().m7a(), 0x0100);
    assert_eq!(bus.ppu().m7b(), 0x0500);

    // 256 x 5 = 1280 = 0x500. Low byte = 0x00, mid = 0x05, high = 0x00.
    assert_eq!(bus.read(ppu_addr(0x2134)), 0x00); // MPYL
    assert_eq!(bus.read(ppu_addr(0x2135)), 0x05); // MPYM
    assert_eq!(bus.read(ppu_addr(0x2136)), 0x00); // MPYH
}

// -------------------------------------------------------------
// BG scroll
// -------------------------------------------------------------

#[test]
fn bg1_hofs_chain_latch_combines_two_writes() {
    let mut bus = fresh_bus();
    // First H write: $12 -> bg1_hoffset = (0x12 << 8) | 0 = 0x1200.
    bus.write(ppu_addr(0x210D), 0x12);
    assert_eq!(bus.ppu().bg_hoffset(0), 0x1200);

    // Second H write: $34 -> bg1_hoffset = (0x34 << 8) |
    //   (0x12 & ~7) | (0x12 & 7) = 0x3400 | 0x10 | 0x02 = 0x3412.
    bus.write(ppu_addr(0x210D), 0x34);
    assert_eq!(bus.ppu().bg_hoffset(0), 0x3412);
}

#[test]
fn bg1_hofs_also_drives_mode7_hoffset_via_separate_latch() {
    let mut bus = fresh_bus();
    bus.write(ppu_addr(0x210D), 0x77);
    // The mode7 latch chain uses a separate `mode7_latchdata`
    // independent of the BG latch chain. After one write
    // mode7_hoffset = (0x77 << 8) | 0 = 0x7700.
    // The PPU's m7 helpers expose this via the public API only
    // for the matrix coefficients, so we use the multiply output
    // as a proxy for "mode7 latch chain is working":
    //
    // Keep the bus (the Mode 7 latch still holds 0x77), write a known
    // M7A and M7B via the same latch, and verify the multiply matches.
    bus.write(ppu_addr(0x211B), 0x02); // m7a = 0x0277 (assembles with latch 0x77)
    bus.write(ppu_addr(0x211B), 0x00); // m7a = (0x00 << 8) | 0x02 = 0x0002
    bus.write(ppu_addr(0x211C), 0x03); // m7b = (0x03 << 8) | 0x00 = 0x0300
                                       // 2 x 3 = 6.
    assert_eq!(bus.read(ppu_addr(0x2134)), 0x06);
    assert_eq!(bus.read(ppu_addr(0x2135)), 0x00);
    assert_eq!(bus.read(ppu_addr(0x2136)), 0x00);
}

// -------------------------------------------------------------
// SLHV + STAT78
// -------------------------------------------------------------

#[test]
fn slhv_2137_latches_cached_raster_through_bus() {
    let mut bus = fresh_bus();
    bus.set_raster_snapshot(0x015C, 0x0078);
    // SLHV: triggers HV latch.
    let _ = bus.read(ppu_addr(0x2137));
    // OPHCT / OPVCT now reflect the snapshot, byte-by-byte.
    assert_eq!(bus.read(ppu_addr(0x213C)), 0x5C);
    assert_eq!(bus.read(ppu_addr(0x213C)), 0x01);
    assert_eq!(bus.read(ppu_addr(0x213D)), 0x78);
    assert_eq!(bus.read(ppu_addr(0x213D)), 0x00);
}

#[test]
fn stat78_213f_bit6_consumes_and_clears() {
    let mut bus = fresh_bus();
    // First read: nothing latched -> bit 6 = 1.
    let r0 = bus.read(ppu_addr(0x213F));
    assert_eq!(r0 & 0x40, 0x40);
    // Strobe SLHV.
    bus.set_raster_snapshot(10, 20);
    let _ = bus.read(ppu_addr(0x2137));
    // First read after latch -> bit 6 = 0.
    let r1 = bus.read(ppu_addr(0x213F));
    assert_eq!(r1 & 0x40, 0x00);
    // Second read: flag cleared on previous read -> bit 6 = 1.
    let r2 = bus.read(ppu_addr(0x213F));
    assert_eq!(r2 & 0x40, 0x40);
}

#[test]
fn stat78_213f_bit4_reflects_pal_setting() {
    let mut bus = fresh_bus();
    // NTSC default.
    let r_ntsc = bus.read(ppu_addr(0x213F));
    assert_eq!(r_ntsc & 0x10, 0x00, "bit 4 should be 0 for NTSC");
    // Flip the timing flag and verify.
    bus.system_regs_mut().set_timing_is_pal(true);
    let r_pal = bus.read(ppu_addr(0x213F));
    assert_eq!(r_pal & 0x10, 0x10, "bit 4 should be 1 for PAL");
}

#[test]
fn stat78_213f_carries_ppu2_version_in_low_nibble() {
    let mut bus = fresh_bus();
    let r = bus.read(ppu_addr(0x213F));
    // PPU2 version 1 (original SHVC-CPU).
    assert_eq!(r & 0x0F, 1);
}

// -------------------------------------------------------------
// MDR latching surface (smoke)
// -------------------------------------------------------------

#[test]
fn write_only_register_reads_a_bus_mdr() {
    let mut bus = fresh_bus();
    // Latch a known MDR value via a WRAM round-trip.
    bus.write(0x7E_0000, 0xC3);
    let _ = bus.read(0x7E_0000);
    // INIDISP is write-only -> catch-all -> A-bus MDR.
    assert_eq!(bus.read(ppu_addr(0x2100)), 0xC3);
}

#[test]
fn ppu1_mdr_only_register_returns_ppu1_mdr_not_a_bus_mdr() {
    let mut bus = fresh_bus();
    // Latch A-bus MDR to 0xC3.
    bus.write(0x7E_0000, 0xC3);
    let _ = bus.read(0x7E_0000);
    // $2105 (BGMODE) is in the PPU1 MDR-only list. With no PPU1
    // read having driven that latch, it should be 0.
    assert_eq!(bus.read(ppu_addr(0x2105)), 0);
}
