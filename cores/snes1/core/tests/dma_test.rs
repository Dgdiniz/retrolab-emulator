//! End-to-end DMA / HDMA tests.
//!
//! Every scenario programs the `$43xx` channel registers through the
//! full A-bus dispatch (`SnesBus::write`), seeds source data in WRAM,
//! and then drives a transfer either through the deferred general-DMA
//! trigger (`$420B` -> `take_pending_general_dma` -> `run_general_dma`)
//! or by calling the scheduler-side HDMA entry points
//! (`hdma_init_frame` / `hdma_run_line`) directly, mirroring how the
//! PPU suites call `render_scanline` instead of spinning a full CPU
//! frame. One smoke test exercises the whole `run_frame` scheduler
//! path on a real [`SnesEmulator`].
//!
//! Unit-level coverage of the pure helpers (B-bus offset patterns,
//! transfer lengths, address stepping, validity guards, register
//! decode) lives in `dma/mod.rs`; the cases here pin the execution the
//! pure layer can't reach, the A<->B copy, mode B-bus addressing into
//! real PPU/APU ports, the HDMA table walk (direct + indirect), the
//! continuous flag, table termination, and the cycle accounting.
//!
//! The expected values were checked against bsnes-plus.

use retro_core::cpu::Addressable;
use retro_core::EmulatorCore;
use snes1_core::{SnesBus, SnesEmulator};

// -------------------------------------------------------------
// Helpers
// -------------------------------------------------------------

fn addr(lo: u16) -> u32 {
    u32::from(lo) & 0x00FF_FFFF
}

/// Write a `$43xN` channel register through the bus.
fn set_ch(bus: &mut SnesBus, ch: u8, offset: u8, value: u8) {
    bus.write(0x4300 | (u32::from(ch) << 4) | u32::from(offset), value);
}

/// Program the common channel fields (DMAPx, BBADx, A1Tx, A1Bx, DASx).
fn program_channel(
    bus: &mut SnesBus,
    ch: u8,
    params: u8,
    dest: u8,
    src_addr: u16,
    src_bank: u8,
    byte_count: u16,
) {
    set_ch(bus, ch, 0x0, params);
    set_ch(bus, ch, 0x1, dest);
    set_ch(bus, ch, 0x2, (src_addr & 0xFF) as u8);
    set_ch(bus, ch, 0x3, (src_addr >> 8) as u8);
    set_ch(bus, ch, 0x4, src_bank);
    set_ch(bus, ch, 0x5, (byte_count & 0xFF) as u8);
    set_ch(bus, ch, 0x6, (byte_count >> 8) as u8);
}

/// Seed WRAM bank `$7E` at `offset` with `bytes` (direct bus writes).
fn poke_wram(bus: &mut SnesBus, offset: u16, bytes: &[u8]) {
    for (i, &b) in bytes.iter().enumerate() {
        bus.write(0x7E_0000 + offset as u32 + i as u32, b);
    }
}

/// Fire general DMA for `mask` through the deferred trigger path, the
/// same way `system.rs` does it at the instruction boundary. Returns
/// the stolen master cycles.
fn run_general_dma(bus: &mut SnesBus, mask: u8) -> u64 {
    bus.write(addr(0x420B), mask);
    let pending = bus.take_pending_general_dma();
    assert_eq!(pending, mask, "$420B write must flag the pending mask");
    bus.run_general_dma(pending)
}

/// Enable the given HDMA channels via `$420C`.
fn set_hdmaen(bus: &mut SnesBus, mask: u8) {
    bus.write(addr(0x420C), mask);
}

// DMAPx bit fields.
const DIR_B_TO_A: u8 = 0x80;
const INDIRECT: u8 = 0x40;
const A_DECREMENT: u8 = 0x10;
const A_FIXED: u8 = 0x08;

/// Read back the CPU->APU mailbox **input** latch for `port` (0-3).
///
/// A CPU or DMA write to `$2140-$2143` lands in this latch, it is what
/// the SPC700 sees at `$F4-$F7`. A CPU *read* of `$2140-$2143`
/// (`bus.read`) returns the *separate* APU->CPU output latch the SPC700
/// drives, which stays 0 (or holds the IPL's `$AA/$BB` handshake once the
/// SPC has run) until the SPC writes it. So a DMA-destination assertion
/// must observe the input latch, not the readable port.
fn apu_dest_latch(bus: &mut SnesBus, port: u8) -> u8 {
    bus.apu_mut().spc_read(0x00F4 + u16::from(port))
}

/// Seed the APU->CPU mailbox **output** latch for `port` (0-3) so a B->A
/// DMA read of `$2140-$2143` streams a known value out. On hardware the
/// SPC700 drives this latch via its `$F4-$F7` writes; here we set it
/// directly to give the B->A transfer a stable source.
fn seed_apu_source_latch(bus: &mut SnesBus, port: u8, value: u8) {
    bus.apu_mut().spc_write(0x00F4 + u16::from(port), value);
}

// -------------------------------------------------------------
// General DMA
// -------------------------------------------------------------

#[test]
fn general_dma_mode_0_wram_to_vram() {
    let mut bus = SnesBus::new();
    // VMAIN: low-side increment (write to $2118 advances), step 1.
    bus.write(addr(0x2115), 0x00);
    bus.write(addr(0x2116), 0x00);
    bus.write(addr(0x2117), 0x00);

    let src: Vec<u8> = (0..256).map(|i| (i & 0xFF) as u8).collect();
    poke_wram(&mut bus, 0x0000, &src);

    // Channel 0: A->B, mode 0 (single port), source $7E:0000, 256 bytes,
    // dest $2118 (VMDATAL).
    program_channel(&mut bus, 0, 0x00, 0x18, 0x0000, 0x7E, 256);
    run_general_dma(&mut bus, 0x01);

    // Each byte landed in the low half of a successive VRAM word.
    for (i, &byte) in src.iter().enumerate() {
        assert_eq!(
            bus.ppu().vram()[i] & 0xFF,
            u16::from(byte),
            "VRAM word {i} low byte"
        );
        assert_eq!(
            bus.ppu().vram()[i] >> 8,
            0,
            "VRAM word {i} high byte stays 0"
        );
    }
    // The channel's byte count drained to zero.
    assert_eq!(bus.dma().channels[0].byte_count, 0);
    // Source pointer advanced by the byte count.
    assert_eq!(bus.dma().channels[0].a_bus_address, 256);
}

#[test]
fn general_dma_vram_upload_mode_1() {
    let mut bus = SnesBus::new();
    // VMAIN: high-side increment (write to $2119 advances), step 1.
    bus.write(addr(0x2115), 0x80);
    bus.write(addr(0x2116), 0x00);
    bus.write(addr(0x2117), 0x10); // VRAM word $1000

    // Source = little-endian word pairs.
    let words: [u16; 4] = [0x1234, 0xABCD, 0x0F0F, 0x8001];
    let mut src = Vec::new();
    for w in words {
        src.push((w & 0xFF) as u8);
        src.push((w >> 8) as u8);
    }
    poke_wram(&mut bus, 0x0000, &src);

    // Channel 0: A->B, mode 1 ($2118/$2119 alternating), 8 bytes.
    program_channel(&mut bus, 0, 0x01, 0x18, 0x0000, 0x7E, src.len() as u16);
    run_general_dma(&mut bus, 0x01);

    for (i, w) in words.iter().enumerate() {
        assert_eq!(bus.ppu().vram()[0x1000 + i], *w, "VRAM word {i}");
    }
}

#[test]
fn general_dma_palette_to_cgram() {
    let mut bus = SnesBus::new();
    // CGRAM uses a single byte port ($2122) that pairs low+high writes
    // into one 15-bit colour, so the canonical palette DMA is mode 0.
    bus.write(addr(0x2121), 0x00); // CGADD = colour 0

    // Four colours as low/high byte pairs (high bytes masked to 0x7F).
    let colours: [u16; 4] = [0x7FFF, 0x001F, 0x03E0, 0x7C00];
    let mut src = Vec::new();
    for c in colours {
        src.push((c & 0xFF) as u8);
        src.push((c >> 8) as u8);
    }
    poke_wram(&mut bus, 0x0100, &src);

    program_channel(&mut bus, 0, 0x00, 0x22, 0x0100, 0x7E, src.len() as u16);
    run_general_dma(&mut bus, 0x01);

    for (i, c) in colours.iter().enumerate() {
        assert_eq!(bus.ppu().cgram()[i], c & 0x7FFF, "CGRAM colour {i}");
    }
}

#[test]
fn general_dma_oam_upload() {
    let mut bus = SnesBus::new();
    bus.write(addr(0x2102), 0x00); // OAMADD = 0
    bus.write(addr(0x2103), 0x00);

    // Full 544-byte OAM image with a recognisable ramp.
    let src: Vec<u8> = (0..0x220u32)
        .map(|i| (i.wrapping_mul(7) & 0xFF) as u8)
        .collect();
    poke_wram(&mut bus, 0x0000, &src);

    // Mode 0 -> $2104 (OAMDATA), 544 bytes.
    program_channel(&mut bus, 0, 0x00, 0x04, 0x0000, 0x7E, 0x220);
    run_general_dma(&mut bus, 0x01);

    assert_eq!(&bus.ppu().oam()[..], &src[..], "OAM image mismatch");
}

#[test]
fn general_dma_fixed_source() {
    let mut bus = SnesBus::new();
    bus.write(addr(0x2115), 0x00); // VMAIN low-side inc
    bus.write(addr(0x2116), 0x00);
    bus.write(addr(0x2117), 0x00);

    // Single source byte; the A-fixed flag means it is re-read every
    // transfer.
    poke_wram(&mut bus, 0x0040, &[0x5A]);

    program_channel(&mut bus, 0, A_FIXED, 0x18, 0x0040, 0x7E, 16);
    run_general_dma(&mut bus, 0x01);

    for i in 0..16 {
        assert_eq!(bus.ppu().vram()[i] & 0xFF, 0x5A, "fixed source byte at {i}");
    }
    // A-fixed -> source pointer never moved.
    assert_eq!(bus.dma().channels[0].a_bus_address, 0x0040);
}

#[test]
fn general_dma_a_decrement_source() {
    let mut bus = SnesBus::new();
    bus.write(addr(0x2115), 0x00);
    bus.write(addr(0x2116), 0x00);
    bus.write(addr(0x2117), 0x00);

    // Source bytes 0..4 at $7E:0010..; decrement means the transfer
    // walks them backwards starting at $0013.
    poke_wram(&mut bus, 0x0010, &[0x10, 0x11, 0x12, 0x13]);

    program_channel(&mut bus, 0, A_DECREMENT, 0x18, 0x0013, 0x7E, 4);
    run_general_dma(&mut bus, 0x01);

    // Read order: $13, $12, $11, $10 -> 0x13, 0x12, 0x11, 0x10.
    let got: Vec<u16> = (0..4).map(|i| bus.ppu().vram()[i] & 0xFF).collect();
    assert_eq!(got, vec![0x13, 0x12, 0x11, 0x10]);
    // Source decremented past the start (0x0013 - 4 = 0x000F).
    assert_eq!(bus.dma().channels[0].a_bus_address, 0x000F);
}

#[test]
fn general_dma_direction_b_to_a() {
    let mut bus = SnesBus::new();
    // Seed APU->CPU output port 0 ($2140 on the read side); a B->A read of
    // $2140 streams that latched value back out into WRAM.
    seed_apu_source_latch(&mut bus, 0, 0x99);

    program_channel(&mut bus, 0, DIR_B_TO_A, 0x40, 0x0200, 0x7E, 8);
    run_general_dma(&mut bus, 0x01);

    // The 8 destination bytes in WRAM all read 0x99 from $2140.
    for i in 0..8 {
        assert_eq!(bus.read(0x7E_0200 + i), 0x99, "WRAM dest byte {i}");
    }
}

#[test]
fn general_dma_zero_count_is_64kib() {
    let mut bus = SnesBus::new();
    // A harmless fixed B target ($2100 INIDISP) so the 65536-byte run
    // is cheap; the point is the count semantics, not the sink.
    program_channel(&mut bus, 0, 0x00, 0x00, 0x0000, 0x7E, 0x0000);
    let cycles = run_general_dma(&mut bus, 0x01);

    // 8 setup + 8 channel + 65536 bytes x 8.
    assert_eq!(cycles, 8 + 8 + 65536 * 8);
    // Count register drained to zero; source wrapped a full 16 bits.
    assert_eq!(bus.dma().channels[0].byte_count, 0);
    assert_eq!(bus.dma().channels[0].a_bus_address, 0x0000);
}

#[test]
fn general_dma_transfer_mode_offsets() {
    // The APU mailbox exposes four independent CPU->APU input latches
    // ($2140-$2143), perfect for proving each transfer mode's B-bus
    // offset pattern end-to-end. We observe the input latches (where the
    // DMA writes land) rather than the readable output ports.
    let probe = |params: u8, src: &[u8]| -> [u8; 4] {
        let mut bus = SnesBus::new();
        poke_wram(&mut bus, 0x0000, src);
        program_channel(&mut bus, 0, params, 0x40, 0x0000, 0x7E, src.len() as u16);
        run_general_dma(&mut bus, 0x01);
        [
            apu_dest_latch(&mut bus, 0),
            apu_dest_latch(&mut bus, 1),
            apu_dest_latch(&mut bus, 2),
            apu_dest_latch(&mut bus, 3),
        ]
    };

    // Mode 1 (0,1): A->$2140, B->$2141.
    assert_eq!(probe(0x01, &[0xA0, 0xA1]), [0xA0, 0xA1, 0x00, 0x00]);
    // Mode 2 (0,0): both writes hit $2140 (last wins).
    assert_eq!(probe(0x02, &[0xB0, 0xB1]), [0xB1, 0x00, 0x00, 0x00]);
    // Mode 3 (0,0,1,1): $2140 = 2nd, $2141 = 4th.
    assert_eq!(
        probe(0x03, &[0xC0, 0xC1, 0xC2, 0xC3]),
        [0xC1, 0xC3, 0x00, 0x00]
    );
    // Mode 4 (0,1,2,3): one byte per port.
    assert_eq!(
        probe(0x04, &[0xD0, 0xD1, 0xD2, 0xD3]),
        [0xD0, 0xD1, 0xD2, 0xD3]
    );
    // Mode 5 (0,1,0,1): $2140 = 3rd, $2141 = 4th.
    assert_eq!(
        probe(0x05, &[0xE0, 0xE1, 0xE2, 0xE3]),
        [0xE2, 0xE3, 0x00, 0x00]
    );
}

#[test]
fn general_dma_clears_enable_after_run() {
    let mut bus = SnesBus::new();
    poke_wram(&mut bus, 0x0000, &[0x01, 0x02]);
    program_channel(&mut bus, 0, 0x00, 0x40, 0x0000, 0x7E, 2);
    // Drive through the full trigger path; MDMAEN clears once done.
    bus.write(addr(0x420B), 0x01);
    assert_eq!(bus.system_regs().mdmaen(), 0x01);
    let mask = bus.take_pending_general_dma();
    bus.run_general_dma(mask);
    assert_eq!(
        bus.system_regs().mdmaen(),
        0x00,
        "MDMAEN clears after the block completes"
    );
}

// -------------------------------------------------------------
// HDMA
// -------------------------------------------------------------

#[test]
fn hdma_init_loads_table_pointer_and_counter() {
    let mut bus = SnesBus::new();
    // Direct table at $7E:8000: line-count 5, then a data byte.
    poke_wram(&mut bus, 0x8000, &[0x05, 0xAB, 0x00]);
    // Channel 0: mode 0, dest $2140, table at $7E:8000.
    program_channel(&mut bus, 0, 0x00, 0x40, 0x8000, 0x7E, 0);
    set_hdmaen(&mut bus, 0x01);

    let cycles = bus.hdma_init_frame();
    // init: 8 setup + 8 table read for the one enabled channel.
    assert_eq!(cycles, 8 + 8);
    // The working pointer started at a_bus_address and advanced past the
    // count byte; the line counter loaded the count.
    assert_eq!(bus.dma().channels[0].table_address, 0x8001);
    assert_eq!(bus.dma().channels[0].line_counter, 0x05);
    assert!(bus.dma().channels[0].transfer_this_line);
    assert!(!bus.dma().channels[0].table_done);
}

#[test]
fn hdma_nonrepeat_holds_value_for_count_lines() {
    let mut bus = SnesBus::new();
    // Non-repeat count 5, one data byte 0xAB, then terminator.
    poke_wram(&mut bus, 0x8000, &[0x05, 0xAB, 0x00]);
    program_channel(&mut bus, 0, 0x00, 0x40, 0x8000, 0x7E, 0);
    set_hdmaen(&mut bus, 0x01);
    bus.hdma_init_frame();

    // Line 1: the single transfer fires.
    bus.hdma_run_line();
    assert_eq!(apu_dest_latch(&mut bus, 0), 0xAB);

    // Overwrite the target latch, then confirm lines 2-5 do NOT
    // re-transfer (non-repeat holds the same value without touching the
    // port).
    bus.write(addr(0x2140), 0x00);
    for _ in 0..4 {
        bus.hdma_run_line();
        assert_eq!(
            apu_dest_latch(&mut bus, 0),
            0x00,
            "non-repeat must not re-transfer"
        );
    }
    // After the 5-line run the terminator (0x00) reloaded -> completed.
    assert!(bus.dma().channels[0].table_done);
}

#[test]
fn hdma_repeat_flag_transfers_every_line() {
    let mut bus = SnesBus::new();
    // Repeat (bit 7) count 3: a fresh data byte each of the 3 lines.
    poke_wram(&mut bus, 0x8000, &[0x83, 0x11, 0x22, 0x33, 0x00]);
    program_channel(&mut bus, 0, 0x00, 0x40, 0x8000, 0x7E, 0);
    set_hdmaen(&mut bus, 0x01);
    bus.hdma_init_frame();

    for expected in [0x11u8, 0x22, 0x33] {
        bus.hdma_run_line();
        assert_eq!(apu_dest_latch(&mut bus, 0), expected, "repeat line value");
    }
    // The terminator reloaded on the third line's update -> completed.
    assert!(bus.dma().channels[0].table_done);
    // A completed channel is inert: further lines steal no cycles.
    assert_eq!(bus.hdma_run_line(), 0);
}

#[test]
fn hdma_terminate_on_zero() {
    let mut bus = SnesBus::new();
    // An immediate 0x00 line-count terminates the channel at init.
    poke_wram(&mut bus, 0x8000, &[0x00]);
    program_channel(&mut bus, 0, 0x00, 0x40, 0x8000, 0x7E, 0);
    set_hdmaen(&mut bus, 0x01);
    bus.hdma_init_frame();

    assert!(bus.dma().channels[0].table_done);
    assert!(!bus.dma().channels[0].transfer_this_line);
    // No active channel -> a line steals nothing.
    assert_eq!(bus.hdma_run_line(), 0);
}

#[test]
fn hdma_indirect_mode() {
    let mut bus = SnesBus::new();
    // Indirect table at $7E:8000: repeat count 2 then a 2-byte pointer
    // ($9000) into the data block. Data block at $7E:9000.
    poke_wram(&mut bus, 0x8000, &[0x82, 0x00, 0x90, 0x00]);
    poke_wram(&mut bus, 0x9000, &[0xDA, 0xDB]);

    // Mode 0 + indirect; A1T = table $8000 bank $7E; DASB (indirect
    // bank, $43x7) = $7E.
    program_channel(&mut bus, 0, INDIRECT, 0x40, 0x8000, 0x7E, 0);
    set_ch(&mut bus, 0, 0x7, 0x7E); // indirect data bank
    set_hdmaen(&mut bus, 0x01);
    bus.hdma_init_frame();

    // After init the indirect pointer points at the data block.
    assert_eq!(bus.dma().channels[0].byte_count, 0x9000);

    bus.hdma_run_line();
    assert_eq!(apu_dest_latch(&mut bus, 0), 0xDA, "indirect line 1");
    bus.hdma_run_line();
    assert_eq!(apu_dest_latch(&mut bus, 0), 0xDB, "indirect line 2");
    assert!(bus.dma().channels[0].table_done);
}

#[test]
fn hdma_inactive_when_disabled() {
    let mut bus = SnesBus::new();
    poke_wram(&mut bus, 0x8000, &[0x05, 0xAB, 0x00]);
    program_channel(&mut bus, 0, 0x00, 0x40, 0x8000, 0x7E, 0);
    // HDMAEN left at 0 -> no channel is enabled.
    assert_eq!(
        bus.hdma_init_frame(),
        0,
        "init steals nothing when disabled"
    );
    assert_eq!(bus.hdma_run_line(), 0, "run steals nothing when disabled");
    // The destination latch was never written.
    assert_eq!(apu_dest_latch(&mut bus, 0), 0x00);
}

#[test]
fn mode7_matrix_hdma_over_lines() {
    let mut bus = SnesBus::new();
    // Mode 2 (0,0 -> two bytes per line, both to $211B M7A) with a
    // repeat count of 2: a per-line 16-bit matrix value, the classic
    // Mode 7 roto-zoom pattern. Table provides lo,hi pairs.
    // m7a = (hi << 8) | lo, so [0x00,0x01] -> 0x0100, [0x00,0x02] -> 0x0200.
    poke_wram(&mut bus, 0x8000, &[0x82, 0x00, 0x01, 0x00, 0x02, 0x00]);
    program_channel(&mut bus, 0, 0x02, 0x1B, 0x8000, 0x7E, 0);
    set_hdmaen(&mut bus, 0x01);
    bus.hdma_init_frame();

    bus.hdma_run_line();
    assert_eq!(bus.ppu().m7a(), 0x0100, "Mode 7 matrix line 1");
    bus.hdma_run_line();
    assert_eq!(bus.ppu().m7a(), 0x0200, "Mode 7 matrix line 2");
}

#[test]
fn hdma_two_channels_independent() {
    let mut bus = SnesBus::new();
    // Channel 0 -> APU port 0; channel 3 -> APU port 1, each with its
    // own repeat table, running in the same line.
    poke_wram(&mut bus, 0x8000, &[0x82, 0x11, 0x22, 0x00]);
    poke_wram(&mut bus, 0x8100, &[0x82, 0xAA, 0xBB, 0x00]);
    program_channel(&mut bus, 0, 0x00, 0x40, 0x8000, 0x7E, 0);
    program_channel(&mut bus, 3, 0x00, 0x41, 0x8100, 0x7E, 0);
    set_hdmaen(&mut bus, 0x09); // channels 0 and 3
    bus.hdma_init_frame();

    bus.hdma_run_line();
    assert_eq!(apu_dest_latch(&mut bus, 0), 0x11, "ch0 line 1");
    assert_eq!(apu_dest_latch(&mut bus, 1), 0xAA, "ch3 line 1");
    bus.hdma_run_line();
    assert_eq!(apu_dest_latch(&mut bus, 0), 0x22, "ch0 line 2");
    assert_eq!(apu_dest_latch(&mut bus, 1), 0xBB, "ch3 line 2");
}

// -------------------------------------------------------------
// Full scheduler path
// -------------------------------------------------------------

#[test]
fn run_frame_drives_hdma_through_scheduler() {
    // No ROM: the CPU spins on open-bus instructions, but the
    // scheduler still fires HdmaInit + per-line HdmaTrigger events,
    // so an HDMA channel set up before the frame executes end to end.
    let mut emu = SnesEmulator::new();
    emu.reset();

    // Table in high WRAM ($7E:8000) so the CPU's open-bus BRK stack
    // churn (bank 0 / WRAM mirror) can't clobber it: repeat count 1,
    // one data byte 0xAB, terminator.
    let bus = emu.bus_mut();
    poke_wram(bus, 0x8000, &[0x81, 0xAB, 0x00]);
    program_channel(bus, 0, 0x00, 0x40, 0x8000, 0x7E, 0);
    set_hdmaen(bus, 0x01);

    emu.run_frame();

    // The scheduler's HdmaInit + HdmaTrigger drove the single transfer
    // into APU input port 0. We observe the CPU->APU input latch
    // (`$F4`): reading $2140 would return the APU->CPU output latch, which
    // by now holds the SPC IPL's $AA handshake byte, not the HDMA value.
    assert_eq!(emu.bus_mut().apu_mut().spc_read(0x00F4), 0xAB);
    assert!(emu.bus().dma().channels[0].table_done);
}
