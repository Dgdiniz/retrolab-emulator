//! Unit tests of the bus: its address decode, its registers and ports, the
//! cartridge's pages and speeds, and the diagnostic traces.

use super::*;

#[inline]
fn make_addr(bank: u8, offset: u16) -> u32 {
    ((bank as u32) << 16) | (offset as u32)
}

#[test]
fn wram_mirror_low_bank_low_half() {
    let mut bus = SnesBus::new();
    bus.write(0x7E_0000, 0x42);
    assert_eq!(bus.read(0x00_0000), 0x42);
}

#[test]
fn io_trace_records_only_watched_accesses_with_pc() {
    let mut bus = SnesBus::new();
    bus.enable_io_trace(16);
    assert!(bus.diag_trace_enabled());
    bus.set_trace_pc(0x12_3456);

    // Watched: INIDISP write + RDNMI read (both MMIO-bank addresses).
    bus.write(0x00_2100, 0x0F);
    let _ = bus.read(0x00_4210);
    // Not watched: a WRAM byte and a VRAM-data port write.
    bus.write(0x7E_0000, 0xAB);
    bus.write(0x00_2118, 0xCD);
    // Same low offset but a non-MMIO bank ($40) must be ignored.
    bus.write(0x40_2100, 0x77);

    let trace = bus.take_io_trace();
    assert_eq!(trace.len(), 2, "only the two watched accesses logged");
    assert_eq!(trace[0].pc, 0x12_3456);
    assert!(trace[0].is_write);
    assert_eq!(trace[0].addr, 0x00_2100);
    assert_eq!(trace[0].value, 0x0F);
    assert!(!trace[1].is_write);
    assert_eq!(trace[1].addr, 0x00_4210);
    // Capture is one-shot: taking it disables further recording.
    assert!(!bus.diag_trace_enabled());
}

#[test]
fn io_trace_ring_buffer_keeps_only_the_tail() {
    let mut bus = SnesBus::new();
    bus.enable_io_trace(3);
    for i in 0..10u8 {
        bus.set_trace_pc(0xC0_0000 + i as u32);
        bus.write(0x00_2100, i);
    }
    let trace = bus.take_io_trace();
    assert_eq!(trace.len(), 3, "ring capped at cap");
    // Oldest-first: the last three writes (values 7,8,9) survive.
    assert_eq!(
        trace.iter().map(|a| a.value).collect::<Vec<_>>(),
        vec![7, 8, 9]
    );
}

#[test]
fn the_recorded_writes_and_accesses_stay_bounded_however_long_nothing_drains_them() {
    let mut bus = SnesBus::new();
    bus.set_debug_armed(true);
    for n in 0..10_000u32 {
        Addressable::write(&mut bus, 0x00_2100 + (n % 0x44), n as u8);
    }
    let raised = bus.raised_debug_events();
    assert_eq!(raised.len(), 0x40 + 4, "every register and port, once");
    assert_eq!(raised.dropped(), 0);
    let mut seen = Vec::new();
    raised.drain(|event| seen.push(event));
    assert_eq!(seen[0], BusDebugEvent::PpuRegWrite { reg: 0 });
    assert_eq!(seen[0x40], BusDebugEvent::ApuPortWrite { port: 0 });
    assert_eq!(
        bus.accesses().len(),
        AccessLog::CAPACITY,
        "the log stops at its capacity"
    );
    bus.clear_debug_events();
    assert!(bus.accesses().is_empty());
}

#[test]
fn wram_offset_resolves_direct_banks_and_low_mirror() {
    // Direct $7E/$7F banks.
    assert_eq!(wram_offset(0x7E_0158), Some(0x0158));
    assert_eq!(wram_offset(0x7F_0000), Some(0x1_0000));
    // Low-8 KiB mirror in system banks (both halves of the map).
    assert_eq!(wram_offset(0x00_0158), Some(0x0158));
    assert_eq!(wram_offset(0xB0_1FFF), Some(0x1FFF));
    // Above $1FFF in a system bank is cartridge/open-bus, not WRAM.
    assert_eq!(wram_offset(0x00_2000), None);
    assert_eq!(wram_offset(0x40_0158), None);
}

#[test]
fn addr_watch_catches_a_wram_cell_through_every_alias() {
    let mut bus = SnesBus::new();
    bus.enable_addr_watch(0x7E_0158, 16);
    assert!(bus.diag_trace_enabled());
    bus.set_trace_pc(0xC0_EC55);

    // Write via the direct bank, read back via the low mirror.
    bus.write(0x7E_0158, 0x80);
    let _ = bus.read(0x00_0158);
    // Unrelated WRAM cells must not be logged.
    bus.write(0x7E_0159, 0x11);
    bus.write(0x00_0000, 0x22);

    let trace = bus.take_addr_watch();
    assert_eq!(trace.len(), 2, "only the two accesses to $0158 logged");
    assert!(trace[0].is_write);
    assert_eq!(trace[0].value, 0x80);
    assert_eq!(trace[0].pc, 0xC0_EC55);
    assert!(!trace[1].is_write);
    assert!(!bus.diag_trace_enabled());
}

#[test]
fn pc_trace_captures_recent_program_counters() {
    let mut bus = SnesBus::new();
    bus.enable_pc_trace(4);
    assert!(bus.diag_trace_enabled());
    for pc in [0x10u32, 0x11, 0x12, 0x13, 0x14, 0x15] {
        bus.set_trace_pc(pc);
    }
    let pcs = bus.take_pc_trace();
    assert_eq!(pcs, vec![0x12, 0x13, 0x14, 0x15], "tail of the PC stream");
    assert!(!bus.diag_trace_enabled());
}

#[test]
fn wram_mirror_low_bank_high_half() {
    let mut bus = SnesBus::new();
    bus.write(0x7E_1FFF, 0x99);
    assert_eq!(bus.read(0x00_1FFF), 0x99);
}

#[test]
fn wram_mirror_high_bank() {
    let mut bus = SnesBus::new();
    bus.write(0x7E_0500, 0x37);
    assert_eq!(bus.read(0x80_0500), 0x37);
    assert_eq!(bus.read(0xB0_0500), 0x37);
}

#[test]
fn wram_no_mirror_beyond_2000() {
    let mut bus = SnesBus::new();
    bus.write(0x7E_5000, 0x77);
    // Direct read of physical WRAM sees the byte we wrote.
    assert_eq!(bus.read(0x7E_5000), 0x77);
    // After that read, MDR = 0x77. The system-area read at
    // $00:5000 lives in the OpenBus page, so it should return
    // the current MDR (0x77) rather than the WRAM byte.
    assert_eq!(bus.read(0x00_5000), 0x77);
    // To prove it is open-bus and not a mirror, flip MDR via a
    // write to a different open-bus address and read again.
    bus.write(0x00_6000, 0xAA);
    assert_eq!(bus.read(0x00_5000), 0xAA);
}

#[test]
fn wram_direct_full_range_endpoints() {
    let mut bus = SnesBus::new();
    bus.write(0x7E_0000, 0x11);
    bus.write(0x7F_FFFF, 0x22);
    assert_eq!(bus.read(0x7E_0000), 0x11);
    assert_eq!(bus.read(0x7F_FFFF), 0x22);
    // Middle of the WRAM image.
    bus.write(0x7E_FFFF, 0x33);
    bus.write(0x7F_0000, 0x44);
    assert_eq!(bus.read(0x7E_FFFF), 0x33);
    assert_eq!(bus.read(0x7F_0000), 0x44);
}

#[test]
fn mdr_starts_at_zero() {
    let mut bus = SnesBus::new();
    // $00:FF00 is in the cartridge area, which defaults to
    // OpenBus before any cartridge install, so the result is
    // whatever MDR currently holds, 0 on a fresh bus.
    assert_eq!(bus.read(0x00_FF00), 0x00);
    assert_eq!(bus.mdr(), 0x00);
}

#[test]
fn mdr_updates_on_read() {
    let mut bus = SnesBus::new();
    bus.write(0x7E_0000, 0x55);
    // Reading WRAM sets MDR to the WRAM byte.
    assert_eq!(bus.read(0x7E_0000), 0x55);
    // Now reading from an open-bus address surfaces the latched
    // MDR (no other side effects).
    assert_eq!(bus.read(0x00_FF00), 0x55);
    assert_eq!(bus.mdr(), 0x55);
}

#[test]
fn mdr_updates_on_write() {
    let mut bus = SnesBus::new();
    // $00:FF00 is OpenBus; the write only latches MDR.
    bus.write(0x00_FF00, 0xAA);
    assert_eq!(bus.mdr(), 0xAA);
    // Reading another OpenBus address surfaces the latched MDR.
    assert_eq!(bus.read(0x00_FE00), 0xAA);
}

#[test]
fn b_bus_window_low_write_only_returns_mdr() {
    let mut bus = SnesBus::new();
    bus.write(0x7E_0000, 0xC3);
    let _ = bus.read(0x7E_0000); // latch MDR to 0xC3

    // Write-only PPU registers that fall through to the
    // catch-all "return A-bus MDR" branch. The PPU1 MDR-only
    // subset and the
    // read-only registers ($2134-$2136 MPY, $2138-$213B OAM/
    // VRAM/CGRAM reads, $213E STAT77) exercise different paths
    // and live in their own tests.
    for offset in [
        0x2100u16, 0x2101, 0x2102, 0x2103, 0x2107, 0x210B, 0x210C, 0x210D, 0x210E, 0x210F, 0x2110,
        0x2111, 0x2112, 0x2113, 0x2117, 0x211B, 0x211C, 0x211D, 0x211E, 0x211F, 0x2120, 0x2121,
        0x2122, 0x2123, 0x2127, 0x212B, 0x212C, 0x212D, 0x212E, 0x212F, 0x2130, 0x2131, 0x2132,
        0x2133,
    ] {
        assert_eq!(
            bus.read(make_addr(0x00, offset)),
            0xC3,
            "write-only ${offset:04X} should return A-bus MDR"
        );
    }
    // Expansion / reserved range ($2184-$21FF): open bus = MDR.
    for offset in 0x2184u16..=0x21FF {
        assert_eq!(bus.read(make_addr(0x00, offset)), 0xC3);
    }
}

#[test]
fn b_bus_window_high_write_only_returns_mdr() {
    let mut bus = SnesBus::new();
    bus.write(0x7E_0000, 0x5A);
    let _ = bus.read(0x7E_0000);
    // Mirror in the $80-$BF half. Same surface.
    for offset in [0x2100u16, 0x2101, 0x2107, 0x2117, 0x2123, 0x2130, 0x2133] {
        assert_eq!(bus.read(make_addr(0x80, offset)), 0x5A);
    }
    for offset in 0x2184u16..=0x21FF {
        assert_eq!(bus.read(make_addr(0x80, offset)), 0x5A);
    }
}

#[test]
fn b_bus_window_ppu1_mdr_only_registers_return_ppu1_mdr() {
    let mut bus = SnesBus::new();
    // Latch A-bus MDR to a recognisable value.
    bus.write(0x7E_0000, 0xC3);
    let _ = bus.read(0x7E_0000);
    // The 18 write-only addresses whose read returns the PPU1
    // latch (checked against bsnes-plus). PPU1 MDR starts at 0 so
    // the reads should surface 0, NOT the A-bus MDR.
    for offset in [
        0x2104u16, 0x2105, 0x2106, 0x2108, 0x2109, 0x210A, 0x2114, 0x2115, 0x2116, 0x2118, 0x2119,
        0x211A, 0x2124, 0x2125, 0x2126, 0x2128, 0x2129, 0x212A,
    ] {
        assert_eq!(
            bus.read(make_addr(0x00, offset)),
            0,
            "PPU1 MDR-only ${offset:04X} should return PPU1 MDR (0 at boot), not A-bus MDR"
        );
    }
}

#[test]
fn cpu_mmio_write_only_system_regs_return_mdr() {
    let mut bus = SnesBus::new();
    bus.write(0x7E_0000, 0x99);
    let _ = bus.read(0x7E_0000);
    // $4200-$420F are write-only system registers -> reads surface
    // the MDR latch.
    assert_eq!(bus.read(0x00_4200), 0x99);
    // A write to CPU MMIO should latch MDR without panicking.
    bus.write(0x00_4200, 0x66);
    assert_eq!(bus.mdr(), 0x66);
}

#[test]
fn cartridge_slot_returns_mdr_when_no_cart() {
    let mut bus = SnesBus::new();
    bus.write(0x7E_0000, 0x77);
    let _ = bus.read(0x7E_0000);
    // Without a cartridge installed every page outside the
    // system area is OpenBus, so $80:8000 surfaces MDR.
    assert_eq!(bus.read(0x80_8000), 0x77);
}

#[test]
fn page_handler_inspection_matches_default_table() {
    let bus = SnesBus::new();
    assert_eq!(bus.page_handler(0x7E_0000), PageHandler::Wram);
    assert_eq!(bus.page_handler(0x7F_FFFF), PageHandler::Wram);
    assert_eq!(bus.page_handler(0x00_0000), PageHandler::WramMirror);
    assert_eq!(bus.page_handler(0x00_1FFF), PageHandler::WramMirror);
    assert_eq!(bus.page_handler(0xBF_0000), PageHandler::WramMirror);
    assert_eq!(bus.page_handler(0x00_2100), PageHandler::BBus);
    assert_eq!(bus.page_handler(0x80_2100), PageHandler::BBus);
    assert_eq!(bus.page_handler(0x00_4200), PageHandler::CpuMmio);
    assert_eq!(bus.page_handler(0x80_4200), PageHandler::CpuMmio);
    assert_eq!(bus.page_handler(0x80_8000), PageHandler::OpenBus);
    assert_eq!(bus.page_handler(0xC0_0000), PageHandler::OpenBus);
}

// -- Walker / cartridge / speed-table tests -------------

use crate::cartridge::{test_cartridge, BusSpeed, MapPlan, MapSlot, MapTarget};

fn lorom_like_plan(speed: BusSpeed) -> MapPlan {
    let mut plan = MapPlan::new("lorom-like");
    plan.push(MapSlot::new(
        0x80..=0xFF,
        0x8000..=0xFFFF,
        MapTarget::Rom { base_offset: 0 },
        speed,
    ));
    plan
}

fn ramp_rom(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i & 0xFF) as u8).collect()
}

#[test]
fn walker_promotes_open_bus_to_cartridge() {
    let plan = lorom_like_plan(BusSpeed::Fast);
    let cart = test_cartridge(ramp_rom(0x8000), 0, plan);
    let mut bus = SnesBus::new();
    bus.install_cartridge(Box::new(cart));

    assert_eq!(
        bus.page_handler(0x80_8000),
        PageHandler::Cartridge { slot_index: 0 }
    );
    assert_eq!(
        bus.page_handler(0xFF_F000),
        PageHandler::Cartridge { slot_index: 0 }
    );
    // Cart was promoted only inside $8000-$FFFF: $80:7FFF is in
    // bank $80 page $7, which is OpenBus in the default table
    // and is NOT covered by this slot.
    assert_eq!(bus.page_handler(0x80_7000), PageHandler::OpenBus);
}

#[test]
fn walker_preserves_system_pages() {
    // A slot covering the *entire* system area must not stomp
    // on Wram / WramMirror / BBus / CpuMmio. The walker's
    // first-wins-on-OpenBus rule is what guarantees this.
    let mut plan = MapPlan::new("aggressive");
    plan.push(MapSlot::new(
        0x00..=0xFF,
        0x0000..=0xFFFF,
        MapTarget::Rom { base_offset: 0 },
        BusSpeed::Fast,
    ));
    let cart = test_cartridge(ramp_rom(0x100), 0, plan);
    let mut bus = SnesBus::new();
    bus.install_cartridge(Box::new(cart));

    // Wram banks $7E-$7F untouched.
    assert_eq!(bus.page_handler(0x7E_0000), PageHandler::Wram);
    assert_eq!(bus.page_handler(0x7F_FFFF), PageHandler::Wram);
    // WramMirror in low system-area banks untouched.
    assert_eq!(bus.page_handler(0x00_0000), PageHandler::WramMirror);
    assert_eq!(bus.page_handler(0xBF_1FFF), PageHandler::WramMirror);
    // B-bus window page untouched.
    assert_eq!(bus.page_handler(0x00_2100), PageHandler::BBus);
    assert_eq!(bus.page_handler(0x80_2100), PageHandler::BBus);
    // CPU MMIO page untouched.
    assert_eq!(bus.page_handler(0x00_4200), PageHandler::CpuMmio);
    // But pages that *were* OpenBus inside the system area do
    // get promoted (e.g. $3000-$3FFF, $5000-$7FFF, $8000-$FFFF).
    assert_eq!(
        bus.page_handler(0x00_8000),
        PageHandler::Cartridge { slot_index: 0 }
    );
    assert_eq!(
        bus.page_handler(0x00_3000),
        PageHandler::Cartridge { slot_index: 0 }
    );
}

#[test]
fn walker_first_wins_between_slots() {
    let mut plan = MapPlan::new("overlap");
    // First slot wins.
    plan.push(MapSlot::new(
        0x80..=0x80,
        0x8000..=0xFFFF,
        MapTarget::Rom { base_offset: 0 },
        BusSpeed::Fast,
    ));
    // Second slot overlaps but should be silently ignored on
    // the pages already covered by slot 0.
    plan.push(MapSlot::new(
        0x80..=0x80,
        0x8000..=0xFFFF,
        MapTarget::Rom {
            base_offset: 0x8000,
        },
        BusSpeed::Slow,
    ));
    let cart = test_cartridge(ramp_rom(0x10000), 0, plan);
    let mut bus = SnesBus::new();
    bus.install_cartridge(Box::new(cart));

    assert_eq!(
        bus.page_handler(0x80_8000),
        PageHandler::Cartridge { slot_index: 0 }
    );
    // At install time the bus pins FastROM-eligible pages to
    // Slow (8), MEMSEL boots disabled. Flipping MEMSEL on
    // demonstrates that the FastROM-eligible cache picked up
    // slot 0 (Fast -> 6), not slot 1 (Slow -> would never go to
    // 6 because Slow slots are excluded from the cache).
    assert_eq!(bus.cycles_for(0x80_8000), 8);
    bus.apply_memsel(true);
    assert_eq!(bus.cycles_for(0x80_8000), 6);
}

#[test]
fn speed_table_default() {
    let bus = SnesBus::new();
    // Low RAM / WRAM mirror in the I/O banks -> Slow (8).
    assert_eq!(bus.cycles_for(0x00_0000), 8);
    assert_eq!(bus.cycles_for(0xBF_1FFF), 8);
    // B-bus window ($2000-$3FFF: PPU/APU/WMDATA) -> Fast (6).
    assert_eq!(bus.cycles_for(0x00_2100), 6);
    assert_eq!(bus.cycles_for(0x00_2140), 6);
    // Manual joypad window ($4000-$41FF) -> XSlow (12).
    assert_eq!(bus.cycles_for(0x00_4000), 12);
    assert_eq!(bus.cycles_for(0x80_4016), 12);
    // CPU MMIO / DMA register file ($4200-$5FFF) -> Fast (6),
    // including $4212 HVBJOY and the $4218-$421F auto-joypad
    // results that boot loops poll heavily.
    assert_eq!(bus.cycles_for(0x80_4200), 6);
    assert_eq!(bus.cycles_for(0x00_4212), 6);
    assert_eq!(bus.cycles_for(0x00_4300), 6);
    // $6000-$7FFF expansion -> Slow (8).
    assert_eq!(bus.cycles_for(0x00_6000), 8);
    // ROM region defaults to Slow (8) until a cart/MEMSEL retunes it.
    assert_eq!(bus.cycles_for(0x80_8000), 8);
    // Direct WRAM banks $7E-$7F are Slow as well.
    assert_eq!(bus.cycles_for(0x7E_0000), 8);
}

#[test]
fn speed_table_after_install_uses_slot_speed() {
    let cart = test_cartridge(ramp_rom(0x8000), 0, lorom_like_plan(BusSpeed::XSlow));
    let mut bus = SnesBus::new();
    bus.install_cartridge(Box::new(cart));
    // Every promoted page takes the slot's declared speed, XSlow
    // (12) here; MEMSEL only rewrites pages of `Fast` slots.
    assert_eq!(bus.cycles_for(0x80_8000), 12);
    assert_eq!(bus.cycles_for(0xFF_F000), 12);
    // Pages still OpenBus (e.g. $80:7000) keep the default Slow
    // because the walker only overwrote promoted pages.
    assert_eq!(bus.cycles_for(0x80_7000), 8);
}

#[test]
fn cartridge_rom_round_trip() {
    // 32 KiB ROM with a ramp pattern, slot $80:$8000-$FFFF.
    // The slot's `compute_target_offset` returns
    // (addr - $8000) & 0xFFFF, which is what we want when the
    // ROM is exactly 32 KiB and the slot is a single bank.
    let cart = test_cartridge(ramp_rom(0x8000), 0, lorom_like_plan(BusSpeed::Fast));
    let mut bus = SnesBus::new();
    bus.install_cartridge(Box::new(cart));
    // ROM[0] = 0x00 at $80:8000.
    assert_eq!(bus.read(0x80_8000), 0x00);
    // ROM[$7FFF] = 0xFF at $80:FFFF.
    assert_eq!(bus.read(0x80_FFFF), 0xFF);
    // Read updates MDR.
    assert_eq!(bus.mdr(), 0xFF);
}

#[test]
fn cartridge_sram_round_trip() {
    // SRAM at LoROM-style $70:$0000-$7FFF (32 KiB).
    let mut plan = MapPlan::new("sram-lorom");
    plan.push(MapSlot::new(
        0x70..=0x70,
        0x0000..=0x7FFF,
        MapTarget::Sram { base_offset: 0 },
        BusSpeed::Slow,
    ));
    let cart = test_cartridge(ramp_rom(0), 0x8000, plan);
    let mut bus = SnesBus::new();
    bus.install_cartridge(Box::new(cart));
    bus.write(0x70_0000, 0xAB);
    bus.write(0x70_7FFF, 0xCD);
    assert_eq!(bus.read(0x70_0000), 0xAB);
    assert_eq!(bus.read(0x70_7FFF), 0xCD);
}

#[test]
fn cartridge_open_bus_slot_returns_mdr() {
    let mut plan = MapPlan::new("open-bus-slot");
    plan.push(MapSlot::new(
        0x80..=0x80,
        0x8000..=0xFFFF,
        MapTarget::OpenBus,
        BusSpeed::Fast,
    ));
    let cart = test_cartridge(ramp_rom(0x8000), 0, plan);
    let mut bus = SnesBus::new();
    bus.install_cartridge(Box::new(cart));
    // The page IS promoted (handler became Cartridge), but the
    // slot's target is OpenBus, so reads return MDR.
    assert_eq!(
        bus.page_handler(0x80_8000),
        PageHandler::Cartridge { slot_index: 0 }
    );
    bus.write(0x7E_0000, 0x5A);
    let _ = bus.read(0x7E_0000); // MDR = 0x5A
    assert_eq!(bus.read(0x80_8000), 0x5A);
}

#[test]
fn wmdata_round_trip_through_bus() {
    let mut bus = SnesBus::new();
    // WMADD = $00:0000 (i.e. $7E:0000).
    bus.write(0x00_2181, 0x00);
    bus.write(0x00_2182, 0x00);
    bus.write(0x00_2183, 0x00);
    // Write through WMDATA.
    bus.write(0x00_2180, 0x42);
    // Underlying WRAM byte at $7E:0000 was updated.
    assert_eq!(bus.read(0x7E_0000), 0x42);
}

#[test]
fn wmdata_auto_increment_through_bus() {
    let mut bus = SnesBus::new();
    // Reset WMADD to 0.
    bus.write(0x00_2181, 0x00);
    bus.write(0x00_2182, 0x00);
    bus.write(0x00_2183, 0x00);
    bus.write(0x00_2180, 0x11); // -> $7E:0000 = 0x11, WMADD = 1
    bus.write(0x00_2180, 0x22); // -> $7E:0001 = 0x22, WMADD = 2
    assert_eq!(bus.read(0x7E_0000), 0x11);
    assert_eq!(bus.read(0x7E_0001), 0x22);

    // Reads also auto-increment. Reset WMADD to 0, then read
    // twice through WMDATA and confirm we got both bytes.
    bus.write(0x00_2181, 0x00);
    bus.write(0x00_2182, 0x00);
    bus.write(0x00_2183, 0x00);
    assert_eq!(bus.read(0x00_2180), 0x11);
    assert_eq!(bus.read(0x00_2180), 0x22);
}

// -- System registers + MEMSEL + HV + tier dispatch ---------

fn fastrom_lorom_like_plan() -> MapPlan {
    let mut plan = MapPlan::new("fastrom-lorom");
    plan.push(MapSlot::new(
        0x80..=0xBF,
        0x8000..=0xFFFF,
        MapTarget::Rom { base_offset: 0 },
        BusSpeed::Fast,
    ));
    plan
}

fn install_fastrom_lorom(bus: &mut SnesBus) {
    let cart = test_cartridge(ramp_rom(0x40 * 0x8000), 0, fastrom_lorom_like_plan());
    bus.install_cartridge(Box::new(cart));
}

#[test]
fn system_regs_dispatch_through_bus() {
    let mut bus = SnesBus::new();
    // Write WRMPYA + WRMPYB through the bus to trigger an
    // unsigned 8x8 multiply, then read RDMPY through the bus.
    bus.write(0x00_4202, 12);
    bus.write(0x00_4203, 11); // 12 * 11 = 132 = 0x0084
    assert_eq!(bus.read(0x00_4216), 0x84);
    assert_eq!(bus.read(0x00_4217), 0x00);
    // The accessor agrees with the bus dispatch.
    assert_eq!(bus.system_regs().math_cycles_remaining(), 8);
}

#[test]
fn memsel_rebuilds_fastrom_speed_on_toggle() {
    let mut bus = SnesBus::new();
    install_fastrom_lorom(&mut bus);
    // Boot: MEMSEL=0 -> FastROM region pinned to Slow (8).
    assert_eq!(bus.cycles_for(0x80_8000), 8);
    assert_eq!(bus.cycles_for(0xBF_FFFF), 8);
    // Enable FastROM via $420D bit 0 through the bus.
    bus.write(0x00_420D, 0x01);
    assert!(bus.memsel_fast());
    assert_eq!(bus.cycles_for(0x80_8000), 6);
    assert_eq!(bus.cycles_for(0xBF_FFFF), 6);
    // Toggle back to Slow.
    bus.write(0x00_420D, 0x00);
    assert!(!bus.memsel_fast());
    assert_eq!(bus.cycles_for(0x80_8000), 8);
}

#[test]
fn memsel_switches_banks_c0_to_ff_too() {
    // Banks $C0-$FF belong to the FastROM region whole, both
    // halves of each bank: HiROM code running there is as fast as
    // code running from $80-$BF.
    let mut plan = MapPlan::new("fastrom-hirom");
    plan.push(MapSlot::new(
        0xC0..=0xFF,
        0x0000..=0xFFFF,
        MapTarget::Rom { base_offset: 0 },
        BusSpeed::Fast,
    ));
    let cart = test_cartridge(ramp_rom(0x40_0000), 0, plan);
    let mut bus = SnesBus::new();
    bus.install_cartridge(Box::new(cart));
    // Boot: MEMSEL=0 -> pinned to Slow (8).
    assert_eq!(bus.cycles_for(0xC0_0000), 8);
    assert_eq!(bus.cycles_for(0xFF_FFFF), 8);
    bus.write(0x00_420D, 0x01);
    assert_eq!(bus.cycles_for(0xC0_0000), 6);
    assert_eq!(bus.cycles_for(0xC0_7FFF), 6);
    assert_eq!(bus.cycles_for(0xFF_FFFF), 6);
    bus.write(0x00_420D, 0x00);
    assert_eq!(bus.cycles_for(0xC0_0000), 8);
    assert_eq!(bus.cycles_for(0xFF_FFFF), 8);
}

#[test]
fn memsel_no_effect_on_non_fastrom_slot() {
    let mut plan = MapPlan::new("slow-fastrom-region");
    plan.push(MapSlot::new(
        0x80..=0xBF,
        0x8000..=0xFFFF,
        MapTarget::Rom { base_offset: 0 },
        // Cartridge declares Slow -> not FastROM-capable
        // hardware, so MEMSEL must be a no-op here.
        BusSpeed::Slow,
    ));
    let cart = test_cartridge(ramp_rom(0x40 * 0x8000), 0, plan);
    let mut bus = SnesBus::new();
    bus.install_cartridge(Box::new(cart));
    assert_eq!(bus.cycles_for(0x80_8000), 8);
    bus.write(0x00_420D, 0x01);
    assert_eq!(bus.cycles_for(0x80_8000), 8);
}

#[test]
fn memsel_no_effect_on_non_fastrom_region() {
    // Slot is declared Fast but mapped into banks $40-$5F,
    // which sit outside the FastROM region ($80-$BF:$8000-$FFFF
    // and banks $C0-$FF).
    let mut plan = MapPlan::new("fast-non-fastrom-region");
    plan.push(MapSlot::new(
        0x40..=0x5F,
        0x8000..=0xFFFF,
        MapTarget::Rom { base_offset: 0 },
        BusSpeed::Fast,
    ));
    let cart = test_cartridge(ramp_rom(0x20 * 0x8000), 0, plan);
    let mut bus = SnesBus::new();
    bus.install_cartridge(Box::new(cart));
    // The slot's declared Fast speed is honoured at install
    // because MEMSEL never touched this region.
    assert_eq!(bus.cycles_for(0x40_8000), 6);
    bus.write(0x00_420D, 0x01);
    assert_eq!(bus.cycles_for(0x40_8000), 6);
    bus.write(0x00_420D, 0x00);
    assert_eq!(bus.cycles_for(0x40_8000), 6);
}

#[test]
fn hv_latched_via_4201_falling_edge() {
    let mut bus = SnesBus::new();
    // WRIO bit 7 high -> low triggers an HV latch. The bus has
    // no scheduler wired, so the latched value is (0, 0).
    bus.write(0x00_4201, 0x80);
    bus.write(0x00_4201, 0x00);
    // OPHCT / OPVCT now read 0 for both bytes.
    assert_eq!(bus.read(0x00_213C), 0);
    assert_eq!(bus.read(0x00_213C), 0);
    assert_eq!(bus.read(0x00_213D), 0);
    assert_eq!(bus.read(0x00_213D), 0);
}

#[test]
fn hv_latched_via_213f_read() {
    let mut bus = SnesBus::new();
    // Reading STAT78 latches HV as a side-effect on real
    // hardware. Without a scheduler we latch (0, 0).
    let _ = bus.read(0x00_213F);
    assert_eq!(bus.read(0x00_213C), 0);
    assert_eq!(bus.read(0x00_213C), 0);
}

#[test]
fn joy_data_through_bus_defaults_zero() {
    let mut bus = SnesBus::new();
    // The auto-joypad buffer ($4218-$421F) reads 0 before an
    // auto-read populates it, not MDR open-bus, because the
    // SystemRegs dispatcher returns the buffer byte.
    bus.write(0x7E_0000, 0xFF);
    let _ = bus.read(0x7E_0000); // MDR = 0xFF
    for offset in 0x4218u16..=0x421F {
        assert_eq!(bus.read(make_addr(0x00, offset)), 0);
    }
}

#[test]
fn controller_manual_serial_reads_bit0_through_bus() {
    let mut bus = SnesBus::new();
    // Hold B (bit 15) on port 0.
    bus.set_controller_buttons(0, 0x8000);
    // Strobe high then low via $4016 bit 0 to latch.
    bus.write(make_addr(0x00, 0x4016), 0x01);
    bus.write(make_addr(0x00, 0x4016), 0x00);
    // First serial read returns B in bit 0; upper bits are open bus.
    assert_eq!(bus.read(make_addr(0x00, 0x4016)) & 1, 1);
    // The next 15 bits are 0 (no other button held).
    for _ in 0..15 {
        assert_eq!(bus.read(make_addr(0x00, 0x4016)) & 1, 0);
    }
    // Past the 16 button bits the data line pulls high.
    assert_eq!(bus.read(make_addr(0x00, 0x4016)) & 1, 1);
}

#[test]
fn controller_serial_upper_bits_are_open_bus() {
    let mut bus = SnesBus::new();
    bus.set_controller_buttons(0, 0x8000);
    bus.write(make_addr(0x00, 0x4016), 0x01);
    bus.write(make_addr(0x00, 0x4016), 0x00);
    // Latch a known MDR (high bits) via WRAM.
    bus.write(0x7E_0000, 0xFE);
    let _ = bus.read(0x7E_0000); // MDR = 0xFE
                                 // $4016 read: bit 0 = B (1), bits 1-7 from MDR (0xFE & 0xFE).
    assert_eq!(bus.read(make_addr(0x00, 0x4016)), 0xFF);
}

#[test]
fn auto_joypad_read_latches_both_ports_and_sets_busy() {
    let mut bus = SnesBus::new();
    // Port 0 = B (0x8000), port 1 = Start (0x1000).
    bus.set_controller_buttons(0, 0x8000);
    bus.set_controller_buttons(1, 0x1000);
    bus.auto_joypad_read();
    // JOY1 ($4218 low / $4219 high) = 0x8000.
    assert_eq!(bus.read(make_addr(0x00, 0x4218)), 0x00);
    assert_eq!(bus.read(make_addr(0x00, 0x4219)), 0x80);
    // JOY2 ($421A low / $421B high) = 0x1000.
    assert_eq!(bus.read(make_addr(0x00, 0x421A)), 0x00);
    assert_eq!(bus.read(make_addr(0x00, 0x421B)), 0x10);
    // JOY3/JOY4 stay 0 (no multitap).
    for offset in 0x421Cu16..=0x421F {
        assert_eq!(bus.read(make_addr(0x00, offset)), 0);
    }
    // HVBJOY ($4212) bit 0 (busy) is set; clearing it leaves the
    // other bits untouched.
    assert_eq!(bus.read(make_addr(0x00, 0x4212)) & 0x01, 0x01);
    bus.auto_joypad_finish();
    assert_eq!(bus.read(make_addr(0x00, 0x4212)) & 0x01, 0x00);
}

#[test]
fn reset_controllers_clears_held_state() {
    let mut bus = SnesBus::new();
    bus.set_controller_buttons(0, 0xFFFF);
    bus.reset_controllers();
    bus.auto_joypad_read();
    assert_eq!(bus.read(make_addr(0x00, 0x4218)), 0x00);
    assert_eq!(bus.read(make_addr(0x00, 0x4219)), 0x00);
}

#[test]
fn tier_dispatch_returns_same_value() {
    let mut bus = SnesBus::new();
    install_fastrom_lorom(&mut bus);
    // ROM byte at $80:8000, covers Cartridge path.
    for addr in [
        0x7E_0000u32, // Wram
        0x00_0000,    // WramMirror
        0x00_2180,    // BBus (WMDATA path)
        0x00_4216,    // CpuMmio (SystemRegs read)
        0x80_8000,    // Cartridge
        0x00_5000,    // OpenBus
    ] {
        let baseline = bus.read(addr);
        assert_eq!(bus.read_full(addr), baseline);
        assert_eq!(bus.read_light(addr), baseline);
        assert_eq!(bus.read_fast(addr), baseline);
    }

    // Mirror for writes: every tier must touch the same backing
    // store. Pick WRAM so the side-effect is observable.
    bus.write_fast(0x7E_0010, 0xAA);
    assert_eq!(bus.read(0x7E_0010), 0xAA);
    bus.write_light(0x7E_0011, 0xBB);
    assert_eq!(bus.read(0x7E_0011), 0xBB);
    bus.write_full(0x7E_0012, 0xCC);
    assert_eq!(bus.read(0x7E_0012), 0xCC);
}

#[test]
fn cpu_mmio_unmapped_returns_mdr() {
    let mut bus = SnesBus::new();
    // Latch a known MDR.
    bus.write(0x7E_0000, 0xC7);
    let _ = bus.read(0x7E_0000);
    // $430C-$430E are unmapped DMA slots: writes drop, reads
    // surface MDR. Writing still latches MDR like any bus write.
    bus.write(0x00_430C, 0x42);
    assert_eq!(bus.mdr(), 0x42);
    // Refresh MDR via WRAM, then confirm the unmapped slot reads
    // the open-bus value rather than any stored byte.
    bus.write(0x7E_0001, 0x5A);
    let _ = bus.read(0x7E_0001);
    assert_eq!(bus.read(0x00_430C), 0x5A);
    // ($4016/$4017 are not open bus: the controller ports drive
    // bit 0; see the controller_* tests.)
}

#[test]
fn writing_mdmaen_flags_pending_general_dma() {
    let mut bus = SnesBus::new();
    // No general DMA pending at boot.
    assert_eq!(bus.take_pending_general_dma(), 0);
    // A non-zero $420B write flags the pending mask but does NOT
    // run the transfer (that's the instruction-boundary job).
    bus.write(0x00_420B, 0x03);
    assert_eq!(bus.take_pending_general_dma(), 0x03);
    // take_* clears it.
    assert_eq!(bus.take_pending_general_dma(), 0);
    // A zero $420B write flags nothing.
    bus.write(0x00_420B, 0x00);
    assert_eq!(bus.take_pending_general_dma(), 0);
}

#[test]
fn dma_channel_registers_round_trip_through_bus() {
    let mut bus = SnesBus::new();
    // Program channel 0 DMAPx + BBADx + A1Tx through the bus and
    // read them back; channel = bits 4-6 of the low byte.
    bus.write(0x00_4300, 0x01); // DMAP0: mode 1, A->B
    bus.write(0x00_4301, 0x18); // BBAD0 -> $2118 (VMDATA)
    bus.write(0x00_4302, 0x34);
    bus.write(0x00_4303, 0x12); // A1T0 = $1234
    bus.write(0x00_4304, 0x7E); // A1B0 = $7E
    assert_eq!(bus.read(0x00_4300), 0x01);
    assert_eq!(bus.read(0x00_4301), 0x18);
    assert_eq!(bus.read(0x00_4302), 0x34);
    assert_eq!(bus.read(0x00_4303), 0x12);
    assert_eq!(bus.read(0x00_4304), 0x7E);
    // Channel 7 lives at $4370-$437F.
    bus.write(0x00_437A, 0x09); // NTRL7 line counter
    assert_eq!(bus.read(0x00_437A), 0x09);
    // The controller accessor agrees with the bus dispatch.
    assert_eq!(bus.dma().channels[0].b_bus_port, 0x18);
    assert_eq!(bus.dma().channels[0].a_bus_address, 0x1234);
    assert_eq!(bus.dma().channels[7].line_counter, 0x09);
}

#[test]
fn slhv_2137_latches_cached_raster() {
    let mut bus = SnesBus::new();
    bus.set_raster_snapshot(123, 45);
    // SLHV read returns A-bus MDR; we just need to make sure
    // it doesn't crash.
    bus.write(0x7E_0000, 0x99);
    let _ = bus.read(0x7E_0000);
    let r = bus.read(0x00_2137);
    assert_eq!(r, 0x99); // MDR
                         // OPHCT / OPVCT now expose (123, 45).
    assert_eq!(bus.read(0x00_213C), 123u8);
    assert_eq!(bus.read(0x00_213C), (123u16 >> 8) as u8);
    assert_eq!(bus.read(0x00_213D), 45u8);
    assert_eq!(bus.read(0x00_213D), (45u16 >> 8) as u8);
}

#[test]
fn stat78_213f_consumes_counters_latched_flag() {
    let mut bus = SnesBus::new();
    // Initial read: no recent latch -> bit 6 = 1.
    let r0 = bus.read(0x00_213F);
    assert!(r0 & 0x40 != 0, "bit 6 should be 1 before any latch");
    // Strobe SLHV to set counters_latched.
    bus.set_raster_snapshot(10, 20);
    let _ = bus.read(0x00_2137);
    // First STAT78 read consumes the flag -> bit 6 = 0.
    let r1 = bus.read(0x00_213F);
    assert!(r1 & 0x40 == 0, "bit 6 should be 0 right after a latch");
    // Second read: flag was cleared on the previous read, so
    // we're back to "idle" -> bit 6 = 1.
    let r2 = bus.read(0x00_213F);
    assert!(r2 & 0x40 != 0, "bit 6 should be 1 again after consume");
}

#[test]
fn stat78_213f_carries_ppu2_version_and_pal_bit() {
    let mut bus = SnesBus::new();
    // NTSC default.
    assert!(bus.read(0x00_213F) & 0x10 == 0);
    // Bits 0-3 hold PPU2 version = 1.
    let r_ntsc = bus.read(0x00_213F);
    assert_eq!(r_ntsc & 0x0F, 1);
    // Flip to PAL.
    bus.system_regs_mut().set_timing_is_pal(true);
    let r_pal = bus.read(0x00_213F);
    assert!(r_pal & 0x10 != 0, "PAL bit must be set when timing is PAL");
    assert_eq!(r_pal & 0x0F, 1);
}

#[test]
fn stat78_213f_bit7_mirrors_ppu_field() {
    let mut bus = SnesBus::new();
    // Fresh PPU -> field = false -> bit 7 = 0.
    let r0 = bus.read(0x00_213F);
    assert_eq!(r0 & 0x80, 0, "field=false -> STAT78 bit 7 clear");
    // Flip the PPU field directly (the system layer drives
    // this from VBlankStart, but the bus path is what we're
    // testing here).
    bus.ppu_mut().toggle_field();
    let r1 = bus.read(0x00_213F);
    assert!(r1 & 0x80 != 0, "field=true -> STAT78 bit 7 set");
    bus.ppu_mut().toggle_field();
    let r2 = bus.read(0x00_213F);
    assert_eq!(r2 & 0x80, 0, "second toggle returns bit 7 to clear");
}

#[test]
fn stat78_213f_resets_hv_toggles() {
    let mut bus = SnesBus::new();
    bus.set_raster_snapshot(0x1234, 0x0123);
    // SLHV latches.
    let _ = bus.read(0x00_2137);
    // Half-read OPHCT so the toggle moves to "high byte next".
    let lo = bus.read(0x00_213C);
    assert_eq!(lo, 0x34);
    // Read STAT78, this should reset the H/V toggles.
    let _ = bus.read(0x00_213F);
    // Next OPHCT read goes back to LOW byte instead of high.
    assert_eq!(bus.read(0x00_213C), 0x34);
    assert_eq!(bus.read(0x00_213C), 0x12);
}

#[test]
fn wrio_falling_edge_uses_cached_raster() {
    let mut bus = SnesBus::new();
    bus.set_raster_snapshot(50, 100);
    // Prime $4201 high so the next write produces a falling
    // edge on bit 7.
    bus.write(0x00_4201, 0x80);
    // Now write 0 -> falling edge -> LatchHv side-effect.
    bus.write(0x00_4201, 0x00);
    // OPHCT / OPVCT now reflect the cached snapshot, not (0, 0).
    assert_eq!(bus.read(0x00_213C), 50);
    let _ = bus.read(0x00_213C); // discard high byte
    assert_eq!(bus.read(0x00_213D), 100);
}

#[test]
fn ppu_accessor_round_trips() {
    let mut bus = SnesBus::new();
    // Drive INIDISP brightness via the bus dispatch.
    bus.write(0x00_2100, 0x0F);
    assert_eq!(bus.ppu().display_brightness(), 0x0F);
    assert!(!bus.ppu().display_disable());
    // Re-blank via the mutator.
    bus.ppu_mut().reset();
    assert!(bus.ppu().display_disable());
}

#[test]
fn raster_snapshot_propagation() {
    let mut bus = SnesBus::new();
    assert_eq!(bus.cached_h_dot(), 0);
    assert_eq!(bus.cached_v_scanline(), 0);
    bus.set_raster_snapshot(200, 150);
    assert_eq!(bus.cached_h_dot(), 200);
    assert_eq!(bus.cached_v_scanline(), 150);
}

// -- Expansion-chip MMIO carve-out + $43xx observation ----------

mod expansion {
    use crate::bus::SnesBus;
    use crate::cartridge::{
        test_cartridge, test_cartridge_with_chips, BusSpeed, MapPlan, MapSlot, MapTarget,
        SnesCartridge,
    };
    use retro_core::cartridge::{
        ChipSet, Coprocessor, ExpansionChipInfo, ExpansionChipState, ExpansionChipType,
    };
    use std::sync::{Arc, Mutex};

    /// Shared, inspectable record of everything the console pushed at the
    /// chip. `Coprocessor` is `Send`, so the handle is `Arc<Mutex<_>>`,
    /// never `Rc`.
    #[derive(Default)]
    struct Log {
        reads: u64,
        writes: Vec<(u32, u8)>,
        observed: Vec<(u32, u8)>,
    }

    /// A minimal coprocessor: reads return a sentinel encoding the low
    /// offset nibble; writes and observed bus writes are logged so the
    /// test can prove the bus reached the chip through each path.
    struct SpyChip {
        info: ExpansionChipInfo,
        log: Arc<Mutex<Log>>,
    }

    impl Coprocessor for SpyChip {
        fn info(&self) -> &ExpansionChipInfo {
            &self.info
        }
        fn read(&mut self, offset: u32, _master_cycle: u64) -> u8 {
            self.log.lock().unwrap().reads += 1;
            0xA0 | (offset as u8 & 0x0F)
        }
        fn write(&mut self, offset: u32, value: u8, _master_cycle: u64) {
            self.log.lock().unwrap().writes.push((offset, value));
        }
        fn observe_write(&mut self, addr: u32, value: u8) {
            self.log.lock().unwrap().observed.push((addr, value));
        }
        fn reset(&mut self, _master_clock_hz: u64) {}
        fn serialize(&self) -> Vec<u8> {
            Vec::new()
        }
        fn restore(&mut self, _data: &[u8], _master_clock_hz: u64) {}
        fn debug_state(&self) -> ExpansionChipState {
            ExpansionChipState {
                chip_type: self.info.chip_type,
                registers: Vec::new(),
                internal_ram: None,
                program_counter: None,
                cycle_count: 0,
            }
        }
        fn label(&self) -> &str {
            "SPY"
        }
    }

    fn spy_chip(log: Arc<Mutex<Log>>) -> Box<SpyChip> {
        Box::new(SpyChip {
            info: ExpansionChipInfo {
                name: "SPY".to_string(),
                chip_type: ExpansionChipType::DSP1,
                description: String::new(),
                features: Vec::new(),
            },
            log,
        })
    }

    /// A composable cartridge whose one chip (id 1) both *owns* the
    /// expansion-MMIO window `$00-$3F`/`$80-$BF : $4800-$480F` (a chip
    /// window inside the CpuMmio page the walker can't promote) and
    /// *observes* the DMA registers `$4300-$437F`.
    fn cart_with_expansion_chip(log: Arc<Mutex<Log>>) -> Box<SnesCartridge> {
        let mut plan = MapPlan::new("expansion-test");
        // Stock ROM window so the cartridge dispatches normally elsewhere.
        plan.push(MapSlot::new(
            0x80..=0xFF,
            0x8000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            BusSpeed::Fast,
        ));
        // Chip MMIO carved out of the CpuMmio page. The 16-byte window
        // with base 0 makes the slot-relative offset ($0-$F) reach the
        // chip register directly. Two slots cover the two CpuMmio bank
        // halves ($00-$3F and $80-$BF).
        plan.push(MapSlot::new(
            0x00..=0x3F,
            0x4800..=0x480F,
            MapTarget::ChipMmio {
                chip_id: 1,
                base: 0,
            },
            BusSpeed::Fast,
        ));
        plan.push(MapSlot::new(
            0x80..=0xBF,
            0x4800..=0x480F,
            MapTarget::ChipMmio {
                chip_id: 1,
                base: 0,
            },
            BusSpeed::Fast,
        ));
        let mut chips = ChipSet::default();
        chips.insert(1, spy_chip(log));
        chips.add_observer(0x4300..=0x437F, 1);
        Box::new(test_cartridge_with_chips(vec![0u8; 0x8000], plan, chips))
    }

    #[test]
    fn expansion_mmio_read_reaches_chip() {
        let log = Arc::new(Mutex::new(Log::default()));
        let mut bus = SnesBus::new();
        bus.install_cartridge(cart_with_expansion_chip(log.clone()));

        // $4800 lives on a CpuMmio page the walker left unpromoted; the
        // carve-out routes it to the chip. Sentinel = 0xA0 | (offset & F).
        assert_eq!(bus.read_full(0x00_4800), 0xA0);
        assert_eq!(bus.read_full(0x00_4805), 0xA5);
        // The $80-$BF mirror of the CpuMmio page reaches the same chip,
        // proof the carve-out follows the map plan for both bank halves.
        assert_eq!(bus.read_full(0x80_4800), 0xA0);
        assert_eq!(log.lock().unwrap().reads, 3);
    }

    #[test]
    fn expansion_mmio_write_reaches_chip() {
        let log = Arc::new(Mutex::new(Log::default()));
        let mut bus = SnesBus::new();
        bus.install_cartridge(cart_with_expansion_chip(log.clone()));

        bus.write_full(0x00_4800, 0x5A);
        bus.write_full(0x00_4803, 0x77);
        assert_eq!(log.lock().unwrap().writes, vec![(0, 0x5A), (3, 0x77)]);
    }

    #[test]
    fn dma_reg_write_is_observed_by_chip() {
        let log = Arc::new(Mutex::new(Log::default()));
        let mut bus = SnesBus::new();
        bus.install_cartridge(cart_with_expansion_chip(log.clone()));

        // $4302 = channel 0 A-bus source-address low byte, a plain latch;
        // the round-trip proves the DMA controller still owns the reg.
        bus.write_full(0x00_4302, 0x12);
        assert_eq!(bus.read_full(0x00_4302), 0x12, "DMA register still works");
        // The $80-$BF mirror hits the same 16-bit register; the observer's
        // single $4300-$437F range must still match (bank-normalized key).
        bus.write_full(0x80_4302, 0x34);

        assert_eq!(
            log.lock().unwrap().observed,
            vec![(0x4302, 0x12), (0x4302, 0x34)],
            "both bank mirrors forward the bank-agnostic 16-bit offset"
        );
    }

    #[test]
    fn plain_cartridge_does_not_regress() {
        let mut plan = MapPlan::new("plain");
        plan.push(MapSlot::new(
            0x80..=0xFF,
            0x8000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            BusSpeed::Fast,
        ));
        let cart = test_cartridge(vec![0x11u8; 0x8000], 0, plan);
        let mut bus = SnesBus::new();
        bus.install_cartridge(Box::new(cart));

        // Carve-out disarmed -> $4800 is open bus (returns MDR = the last
        // value on the bus, here the ROM byte just read).
        let rom_byte = bus.read_full(0x80_8000);
        assert_eq!(rom_byte, 0x11);
        assert_eq!(
            bus.read_full(0x00_4800),
            rom_byte,
            "no chip claims $4800 -> open bus, not a carve-out"
        );
        // The B-bus carve-out stays disarmed too: $2200 is open bus (MDR is
        // still the ROM byte from the read just above).
        assert_eq!(
            bus.read_full(0x00_2200),
            rom_byte,
            "no chip claims $2200 -> open bus, not a carve-out"
        );
        // DMA registers untouched by the (disarmed) observe hook.
        bus.write_full(0x00_4302, 0x55);
        assert_eq!(bus.read_full(0x00_4302), 0x55);
    }

    /// A composable cartridge whose one chip (id 1) owns the B-bus MMIO
    /// window `$00-$3F`/`$80-$BF : $2200-$220F`, a chip window inside the
    /// B-bus page ($2000-$2FFF) the page-table walker can't promote. This
    /// is the SA-1 register-file seam.
    fn cart_with_bbus_chip(log: Arc<Mutex<Log>>) -> Box<SnesCartridge> {
        let mut plan = MapPlan::new("bbus-test");
        plan.push(MapSlot::new(
            0x80..=0xFF,
            0x8000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            BusSpeed::Fast,
        ));
        // Chip MMIO carved out of the B-bus page, both bank halves. Base 0 +
        // 16-byte window makes the slot-relative offset ($0-$F) reach the
        // chip register directly.
        for banks in [0x00..=0x3F_u8, 0x80..=0xBF_u8] {
            plan.push(MapSlot::new(
                banks,
                0x2200..=0x220F,
                MapTarget::ChipMmio {
                    chip_id: 1,
                    base: 0,
                },
                BusSpeed::Slow,
            ));
        }
        let mut chips = ChipSet::default();
        chips.insert(1, spy_chip(log));
        Box::new(test_cartridge_with_chips(vec![0u8; 0x8000], plan, chips))
    }

    #[test]
    fn bbus_mmio_read_reaches_chip() {
        let log = Arc::new(Mutex::new(Log::default()));
        let mut bus = SnesBus::new();
        bus.install_cartridge(cart_with_bbus_chip(log.clone()));

        // $2200 lives on a BBus page the walker left unpromoted; the
        // carve-out routes it to the chip. Sentinel = 0xA0 | (offset & F).
        assert_eq!(bus.read_full(0x00_2200), 0xA0);
        assert_eq!(bus.read_full(0x00_2205), 0xA5);
        // The $80-$BF mirror of the BBus page reaches the same chip.
        assert_eq!(bus.read_full(0x80_2200), 0xA0);
        assert_eq!(log.lock().unwrap().reads, 3);
    }

    #[test]
    fn bbus_mmio_write_reaches_chip() {
        let log = Arc::new(Mutex::new(Log::default()));
        let mut bus = SnesBus::new();
        bus.install_cartridge(cart_with_bbus_chip(log.clone()));

        bus.write_full(0x00_2203, 0x5A);
        bus.write_full(0x80_2207, 0x77);
        assert_eq!(log.lock().unwrap().writes, vec![(3, 0x5A), (7, 0x77)]);
    }

    #[test]
    fn bbus_ppu_and_apu_untouched_by_carve_out() {
        // The carve-out arm sits below the $2100-$21FF (PPU) and
        // $2140-$217F (APU) arms, so a chip claiming $2200-$23FF must not
        // shadow the PPU/APU windows in the same page.
        let log = Arc::new(Mutex::new(Log::default()));
        let mut bus = SnesBus::new();
        bus.install_cartridge(cart_with_bbus_chip(log.clone()));
        // A PPU register write ($2100 INIDISP) + an APU port round-trip go
        // to their real handlers, never the spy chip.
        bus.write_full(0x00_2100, 0x0F);
        let _ = bus.read_full(0x00_2140);
        assert!(
            log.lock().unwrap().writes.is_empty(),
            "PPU/APU traffic must not reach the SA-1 register carve-out"
        );
    }

    #[test]
    fn cart_runs_burst_cpu_gate_tracks_bbus_claim() {
        // The SA-1 seam (a chip claiming $2200-$23FF) arms the
        // per-instruction burst-CPU catch-up in system.rs.
        let log = Arc::new(Mutex::new(Log::default()));
        let mut bus = SnesBus::new();
        bus.install_cartridge(cart_with_bbus_chip(log));
        assert!(
            bus.cart_runs_burst_cpu(),
            "a B-bus MMIO claimer (SA-1) runs a burst CPU"
        );

        // A plain ROM cartridge leaves the gate disarmed.
        let mut plan = MapPlan::new("plain-burst");
        plan.push(MapSlot::new(
            0x80..=0xFF,
            0x8000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            BusSpeed::Fast,
        ));
        let mut plain_bus = SnesBus::new();
        plain_bus.install_cartridge(Box::new(test_cartridge(vec![0u8; 0x8000], 0, plan)));
        assert!(
            !plain_bus.cart_runs_burst_cpu(),
            "a plain cart keeps its current cadence"
        );

        // The Super FX opts in independently: a ChipMmio slot covering
        // $3000-$34FF on a system-area bank arms the same burst gate with no
        // B-bus carve-out (the $3xxx page is promoted, not carved).
        let mut sfx_plan = MapPlan::new("superfx-burst");
        sfx_plan.push(MapSlot::new(
            0x00..=0x3F,
            0x3000..=0x34FF,
            MapTarget::ChipMmio {
                chip_id: 8,
                base: 0,
            },
            BusSpeed::Slow,
        ));
        let mut sfx_bus = SnesBus::new();
        sfx_bus.install_cartridge(Box::new(test_cartridge(vec![0u8; 0x8000], 0, sfx_plan)));
        assert!(
            sfx_bus.cart_runs_burst_cpu(),
            "a $3000-$34FF MMIO claimer (Super FX) runs a burst CPU"
        );

        // The Cx4 opts in too: a ChipMmio slot with the Cx4 chip_id (9)
        // covering $6000-$7FFF on a system-area bank arms the burst gate.
        let mut cx4_plan = MapPlan::new("cx4-burst");
        cx4_plan.push(MapSlot::new(
            0x00..=0x3F,
            0x6000..=0x7FFF,
            MapTarget::ChipMmio {
                chip_id: 9,
                base: 0,
            },
            BusSpeed::Slow,
        ));
        let mut cx4_bus = SnesBus::new();
        cx4_bus.install_cartridge(Box::new(test_cartridge(vec![0u8; 0x8000], 0, cx4_plan)));
        assert!(
            cx4_bus.cart_runs_burst_cpu(),
            "a $6000-$7FFF MMIO claimer with the Cx4 chip_id runs a burst CPU"
        );

        // The Cx4 detector is chip_id-specific: an SPC7110-style SRAM slot
        // (a *different* chip_id) over the same $6000-$7FFF window must NOT
        // arm the burst gate, it is not a burst CPU.
        let mut spc_plan = MapPlan::new("spc7110-sram");
        spc_plan.push(MapSlot::new(
            0x00..=0x3F,
            0x6000..=0x7FFF,
            MapTarget::ChipMmio {
                chip_id: 6,
                base: 0,
            },
            BusSpeed::Slow,
        ));
        let mut spc_bus = SnesBus::new();
        spc_bus.install_cartridge(Box::new(test_cartridge(vec![0u8; 0x8000], 0, spc_plan)));
        assert!(
            !spc_bus.cart_runs_burst_cpu(),
            "an SPC7110 SRAM window ($6000-$7FFF, other chip_id) is not a burst CPU"
        );
    }

    /// A coprocessor whose S-CPU IRQ line is always asserted, to prove the
    /// `has_irq()` chain (`Coprocessor` -> `ChipSet` -> `SnesCartridge`
    /// -> `SnesBus::cart_has_irq`) propagates end to end.
    struct AlwaysIrqChip {
        info: ExpansionChipInfo,
    }

    impl Coprocessor for AlwaysIrqChip {
        fn info(&self) -> &ExpansionChipInfo {
            &self.info
        }
        fn read(&mut self, _offset: u32, _master_cycle: u64) -> u8 {
            0
        }
        fn write(&mut self, _offset: u32, _value: u8, _master_cycle: u64) {}
        fn reset(&mut self, _master_clock_hz: u64) {}
        fn serialize(&self) -> Vec<u8> {
            Vec::new()
        }
        fn restore(&mut self, _data: &[u8], _master_clock_hz: u64) {}
        fn debug_state(&self) -> ExpansionChipState {
            ExpansionChipState {
                chip_type: self.info.chip_type,
                registers: Vec::new(),
                internal_ram: None,
                program_counter: None,
                cycle_count: 0,
            }
        }
        fn label(&self) -> &str {
            "IRQ"
        }
        fn has_irq(&self) -> bool {
            true
        }
    }

    #[test]
    fn cart_has_irq_propagates_from_chip_to_bus() {
        // An SA-1-style cart with an IRQ-asserting chip reports the line.
        let mut plan = MapPlan::new("irq-cart");
        plan.push(MapSlot::new(
            0x80..=0xFF,
            0x8000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            BusSpeed::Fast,
        ));
        let mut chips = ChipSet::default();
        chips.insert(
            7,
            Box::new(AlwaysIrqChip {
                info: ExpansionChipInfo {
                    name: "IRQ".to_string(),
                    chip_type: ExpansionChipType::SA1,
                    description: String::new(),
                    features: Vec::new(),
                },
            }),
        );
        let mut bus = SnesBus::new();
        bus.install_cartridge(Box::new(test_cartridge_with_chips(
            vec![0u8; 0x8000],
            plan,
            chips,
        )));
        assert!(bus.cart_has_irq(), "an IRQ-asserting chip raises the line");

        // A plain ROM cart never asserts the S-CPU IRQ line.
        let mut plain_plan = MapPlan::new("plain-irq");
        plain_plan.push(MapSlot::new(
            0x80..=0xFF,
            0x8000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            BusSpeed::Fast,
        ));
        let mut plain_bus = SnesBus::new();
        plain_bus.install_cartridge(Box::new(test_cartridge(vec![0u8; 0x8000], 0, plain_plan)));
        assert!(!plain_bus.cart_has_irq(), "a plain cart never asserts IRQ");
    }
}
