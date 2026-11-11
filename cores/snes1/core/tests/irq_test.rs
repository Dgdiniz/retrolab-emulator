//! End-to-end NMI / IRQ tests.
//!
//! Each scenario synthesizes a minimal LoROM whose reset code arms
//! the relevant interrupt source through the real `$4200`/`$4207-A`
//! MMIO path and then idles in a `BRA -2` loop, with NMI / IRQ
//! vectors pointing at tiny handlers that bump a 16-bit counter in
//! WRAM (and, for IRQ, acknowledge `$4211`). Driving the full
//! [`SnesEmulator`] through `run_frame` exercises the entire chain:
//! scheduler event -> `SystemRegs` flag -> CPU `nmi_pending` /
//! `irq_pending` -> 65C816 interrupt service -> handler.
//!
//! The pure scheduling math (V/H/HV placement, 9-bit targets,
//! `min_cycle` filtering) is unit-tested in `irq.rs`; the flag /
//! re-trigger / line-clear semantics in `system_regs.rs`. The cases
//! here pin the integration those unit layers can't reach.
//!
//! Reference: bsnes-plus `cpu/timing/irq.cpp` + `cpu/mmio/mmio.cpp`.

use retro_core::cpu::Addressable;
use retro_core::EmulatorCore;
use snes1_core::{SnesEmulator, TimingProfile};

// -------------------------------------------------------------
// ROM synthesis
// -------------------------------------------------------------

/// Reset code base `$00:8000` (LoROM chip offset 0).
const RESET_BASE: usize = 0x0000;
/// NMI handler base `$00:8040`.
const NMI_HANDLER_OFF: usize = 0x0040;
/// IRQ handler base `$00:8060`.
const IRQ_HANDLER_OFF: usize = 0x0060;
const NMI_HANDLER_ADDR: u16 = 0x8040;
const IRQ_HANDLER_ADDR: u16 = 0x8060;

/// Stamp the SNES header (title, LoROM map mode, ROM-size code,
/// USA/NTSC destination, consistent checksum, reset vector).
fn stamp_header(chip: &mut [u8], header_offset: usize, reset_vector: u16) {
    let title = b"IRQ TEST             ";
    chip[header_offset + 0x10..header_offset + 0x10 + 21].copy_from_slice(title);
    chip[header_offset + 0x25] = 0x20; // LoROM
    chip[header_offset + 0x26] = 0x00; // ROM only
    chip[header_offset + 0x27] = 0x0A; // 1 MiB
    chip[header_offset + 0x28] = 0x00; // no SRAM
    chip[header_offset + 0x29] = 0x01; // USA -> NTSC
    chip[header_offset + 0x2A] = 0x01;
    chip[header_offset + 0x2B] = 0x00;
    let checksum: u16 = 0x1234;
    chip[header_offset + 0x2C..header_offset + 0x2E].copy_from_slice(&(!checksum).to_le_bytes());
    chip[header_offset + 0x2E..header_offset + 0x30].copy_from_slice(&checksum.to_le_bytes());
    chip[header_offset + 0x4C..header_offset + 0x4E].copy_from_slice(&reset_vector.to_le_bytes());
}

/// Build a 1 MiB LoROM with the given reset code and NMI / IRQ
/// handlers, wiring the emulation-mode vectors at `$FFFA` (NMI),
/// `$FFFC` (reset -> `$8000`), and `$FFFE` (IRQ).
fn build_rom(reset_code: &[u8], nmi_handler: &[u8], irq_handler: &[u8]) -> Vec<u8> {
    const SIZE: usize = 0x10_0000;
    let mut chip = vec![0u8; SIZE];
    chip[RESET_BASE..RESET_BASE + reset_code.len()].copy_from_slice(reset_code);
    chip[NMI_HANDLER_OFF..NMI_HANDLER_OFF + nmi_handler.len()].copy_from_slice(nmi_handler);
    chip[IRQ_HANDLER_OFF..IRQ_HANDLER_OFF + irq_handler.len()].copy_from_slice(irq_handler);
    stamp_header(&mut chip, 0x7FB0, 0x8000);
    // Emulation-mode vectors live at chip $7FFA / $7FFC / $7FFE.
    chip[0x7FFA..0x7FFC].copy_from_slice(&NMI_HANDLER_ADDR.to_le_bytes());
    chip[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    chip[0x7FFE..0x8000].copy_from_slice(&IRQ_HANDLER_ADDR.to_le_bytes());
    chip
}

// -------------------------------------------------------------
// Hand-assembled code fragments (65C816, emulation mode / 8-bit)
// -------------------------------------------------------------

/// `BRA -2`: idle forever at the current PC.
const IDLE: [u8; 2] = [0x80, 0xFE];

/// NMI handler: 16-bit counter at DP `$00/$01`, then `RTI`. No
/// `$4210` read, the VBlank-flag clear path lives in the scheduler.
///
/// `$8040 INC $00` / `$8042 BNE +2` / `$8044 INC $01` / `$8046 RTI`.
const NMI_HANDLER: [u8; 7] = [0xE6, 0x00, 0xD0, 0x02, 0xE6, 0x01, 0x40];

/// IRQ handler that acknowledges: read `$4211` (clears the level
/// line), bump a 16-bit counter at DP `$02/$03`, then `RTI`.
///
/// `$8060 LDA $4211` / `$8063 INC $02` / `$8065 BNE +2` /
/// `$8067 INC $03` / `$8069 RTI`.
const IRQ_HANDLER_ACK: [u8; 10] = [0xAD, 0x11, 0x42, 0xE6, 0x02, 0xD0, 0x02, 0xE6, 0x03, 0x40];

/// IRQ handler that does NOT acknowledge, leaves the level line
/// raised so the next instruction re-enters the IRQ. Bumps the
/// 16-bit counter at DP `$02/$03`.
const IRQ_HANDLER_NOACK: [u8; 7] = [0xE6, 0x02, 0xD0, 0x02, 0xE6, 0x03, 0x40];

/// Read the little-endian 16-bit counter at WRAM `addr`.
fn read_counter(emu: &mut SnesEmulator, addr: u32) -> u16 {
    let lo = u16::from(emu.bus_mut().read(addr));
    let hi = u16::from(emu.bus_mut().read(addr + 1));
    lo | (hi << 8)
}

fn loaded(reset: &[u8], nmi: &[u8], irq: &[u8]) -> SnesEmulator {
    let rom = build_rom(reset, nmi, irq);
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).expect("load_rom should succeed");
    assert_eq!(emu.timing(), TimingProfile::NTSC);
    emu
}

// -------------------------------------------------------------
// NMI
// -------------------------------------------------------------

/// `LDA #$80 / STA $4200 / BRA -2`: enable NMI, then idle.
const RESET_ENABLE_NMI: [u8; 7] = [0xA9, 0x80, 0x8D, 0x00, 0x42, 0x80, 0xFE];

#[test]
fn nmi_fires_once_per_frame_when_enabled() {
    let mut emu = loaded(&RESET_ENABLE_NMI, &NMI_HANDLER, &IRQ_HANDLER_ACK);
    for _ in 0..5 {
        emu.run_frame();
    }
    // Exactly one NMI per VBlank, no double-fire, no miss.
    assert_eq!(read_counter(&mut emu, 0x0000), 5);
}

#[test]
fn nmi_does_not_fire_when_disabled() {
    // Idle loop never writes $4200 -> NMITIMEN stays 0. The $4210
    // flag still latches at VBlank, but the CPU NMI is gated off.
    let mut emu = loaded(&IDLE, &NMI_HANDLER, &IRQ_HANDLER_ACK);
    for _ in 0..5 {
        emu.run_frame();
    }
    assert_eq!(read_counter(&mut emu, 0x0000), 0);
}

#[test]
fn nmi_retrigger_when_enabled_during_latched_vblank() {
    // Start with NMI disabled (idle ROM), then mimic the quirk: the
    // VBlank handler latched $4210 and a $4200 write now enables NMI
    // (0->1) while that flag is still set -> the CPU takes a fresh NMI.
    let mut emu = loaded(&IDLE, &NMI_HANDLER, &IRQ_HANDLER_ACK);
    emu.bus_mut().system_regs_mut().set_nmi_flag(true);
    emu.bus_mut().write(0x4200, 0x80);
    // The bus flagged a pending NMI; step through the boundary drain,
    // the service, and the handler body.
    for _ in 0..10 {
        emu.step();
    }
    assert_eq!(read_counter(&mut emu, 0x0000), 1);
}

// -------------------------------------------------------------
// IRQ, V / H / HV
// -------------------------------------------------------------

/// `CLI / LDA #$20 / STA $4200 / LDA #$50 / STA $4209 / LDA #$00 /
/// STA $420A / BRA -2`, V-IRQ on scanline `$050` (80).
const RESET_ENABLE_VIRQ: [u8; 18] = [
    0x58, // CLI
    0xA9, 0x20, 0x8D, 0x00, 0x42, // STA $4200 = V-IRQ enable
    0xA9, 0x50, 0x8D, 0x09, 0x42, // STA $4209 = VTIMEL (80)
    0xA9, 0x00, 0x8D, 0x0A, 0x42, // STA $420A = VTIMEH (0)
    0x80, 0xFE, // BRA -2
];

#[test]
fn irq_v_fires_once_per_frame_and_clears_on_ack() {
    let mut emu = loaded(&RESET_ENABLE_VIRQ, &NMI_HANDLER, &IRQ_HANDLER_ACK);
    for _ in 0..4 {
        emu.run_frame();
    }
    // Exactly one V-IRQ per frame: firing proves the schedule + line
    // raise; the count being *exactly* 4 (not thousands) proves the
    // $4211 ack lowered the level line each time.
    assert_eq!(read_counter(&mut emu, 0x0002), 4);
    // NMI handler must not have run (NMI never enabled).
    assert_eq!(read_counter(&mut emu, 0x0000), 0);
}

/// `CLI / LDA #$10 / STA $4200 / LDA #$80 / STA $4207 / LDA #$00 /
/// STA $4208 / BRA -2`, H-IRQ at dot `$080` (128) on every line.
const RESET_ENABLE_HIRQ: [u8; 18] = [
    0x58, // CLI
    0xA9, 0x10, 0x8D, 0x00, 0x42, // STA $4200 = H-IRQ enable
    0xA9, 0x80, 0x8D, 0x07, 0x42, // STA $4207 = HTIMEL (128)
    0xA9, 0x00, 0x8D, 0x08, 0x42, // STA $4208 = HTIMEH (0)
    0x80, 0xFE, // BRA -2
];

#[test]
fn irq_h_fires_every_scanline() {
    let mut emu = loaded(&RESET_ENABLE_HIRQ, &NMI_HANDLER, &IRQ_HANDLER_ACK);
    emu.run_frame();
    // One H-IRQ per scanline (262 NTSC). Allow a small slack for the
    // first line or two being filtered by `min_cycle` while the reset
    // code is still arming the timer, the meaningful signal is
    // "hundreds per frame", clearly distinct from V-IRQ's one.
    let count = read_counter(&mut emu, 0x0002);
    assert!(
        (258..=262).contains(&count),
        "expected ~262 H-IRQs in one NTSC frame, got {count}"
    );
}

/// `CLI / LDA #$30 / STA $4200 / LDA #$80 / STA $4207 / LDA #$00 /
/// STA $4208 / LDA #$50 / STA $4209 / LDA #$00 / STA $420A / BRA -2`,
/// HV-IRQ at scanline 80, dot 128.
const RESET_ENABLE_HVIRQ: [u8; 28] = [
    0x58, // CLI
    0xA9, 0x30, 0x8D, 0x00, 0x42, // STA $4200 = H+V IRQ enable
    0xA9, 0x80, 0x8D, 0x07, 0x42, // STA $4207 = HTIMEL (128)
    0xA9, 0x00, 0x8D, 0x08, 0x42, // STA $4208 = HTIMEH (0)
    0xA9, 0x50, 0x8D, 0x09, 0x42, // STA $4209 = VTIMEL (80)
    0xA9, 0x00, 0x8D, 0x0A, 0x42, // STA $420A = VTIMEH (0)
    0x80, 0xFE, // BRA -2
];

#[test]
fn irq_hv_fires_once_per_frame() {
    let mut emu = loaded(&RESET_ENABLE_HVIRQ, &NMI_HANDLER, &IRQ_HANDLER_ACK);
    for _ in 0..3 {
        emu.run_frame();
    }
    assert_eq!(read_counter(&mut emu, 0x0002), 3);
}

// -------------------------------------------------------------
// IRQ, masking, level persistence, reschedule
// -------------------------------------------------------------

/// Same V-IRQ setup as `RESET_ENABLE_VIRQ` but WITHOUT the leading
/// `CLI`, so the I flag stays set and the IRQ is never serviced.
const RESET_VIRQ_NO_CLI: [u8; 17] = [
    0xA9, 0x20, 0x8D, 0x00, 0x42, // STA $4200 = V-IRQ enable
    0xA9, 0x50, 0x8D, 0x09, 0x42, // STA $4209 = VTIMEL (80)
    0xA9, 0x00, 0x8D, 0x0A, 0x42, // STA $420A = VTIMEH (0)
    0x80, 0xFE, // BRA -2
];

#[test]
fn irq_masked_when_i_flag_set() {
    let mut emu = loaded(&RESET_VIRQ_NO_CLI, &NMI_HANDLER, &IRQ_HANDLER_ACK);
    for _ in 0..3 {
        emu.run_frame();
    }
    // Line is raised every frame but I=1 blocks the service.
    assert_eq!(read_counter(&mut emu, 0x0002), 0);
}

/// `SEI / arm V-IRQ line 80 / loop: WAI ; LDA $4211 (ack) ; INC counter ;
/// BRA loop`, the Super FX raster-sync idiom (Star Fox / Doom 3D frame
/// loops). Interrupts are *disabled* (`I=1`), so the V-IRQ is never vectored;
/// the code instead uses `WAI` to sleep until the raster line and drives its
/// own frame counter inline. The CPU (`wdc65816`, `lib.rs`) leaves WAI on the
/// asserted IRQ line even with `I` set; a wake gated on `!I` would deadlock
/// here forever (Doom's 3D view would stay black once it arms this loop).
/// Reading `$4211` each pass lowers the level line so the next `WAI` blocks
/// until the following frame, giving exactly one wake per frame.
const RESET_WAI_VIRQ_ACK: [u8; 28] = [
    0x78, // SEI (I stays set, no CLI)
    0xA9, 0x20, 0x8D, 0x00, 0x42, // LDA #$20 : STA $4200  (V-IRQ enable, NMI off)
    0xA9, 0x50, 0x8D, 0x09, 0x42, // LDA #$50 : STA $4209  (VTIMEL = 80)
    0xA9, 0x00, 0x8D, 0x0A, 0x42, // LDA #$00 : STA $420A  (VTIMEH = 0)
    0xCB, // $8010 WAI            (sleep until the V-IRQ raster line)
    0xAD, 0x11, 0x42, // LDA $4211            (ack: lower the level line)
    0xE6, 0x02, 0xD0, 0x02, 0xE6, 0x03, // INC $02 : BNE +2 : INC $03  (16-bit counter)
    0x80, 0xF4, // BRA $8010
];

#[test]
fn wai_wakes_on_raster_irq_with_i_set() {
    let mut emu = loaded(&RESET_WAI_VIRQ_ACK, &NMI_HANDLER, &IRQ_HANDLER_ACK);
    for _ in 0..5 {
        emu.run_frame();
    }
    // Exactly one wake per frame: a non-zero count proves WAI woke despite
    // I=1 (no deadlock); exactly 5 proves the `$4211` ack re-armed
    // the wait each frame instead of spinning. The IRQ handler must NOT have
    // run (I=1 blocks vectoring): it bumps the same counter, so a run would
    // push the count past 5.
    assert_eq!(
        read_counter(&mut emu, 0x0002),
        5,
        "WAI must wake once per frame on the raster IRQ line even with I set"
    );
}

#[test]
fn irq_level_refires_until_acknowledged() {
    // The no-ack handler leaves $4211 bit 7 set, so the level line
    // stays high and the CPU re-enters the IRQ on every following
    // instruction until the frame ends, far more than once.
    let mut emu = loaded(&RESET_ENABLE_VIRQ, &NMI_HANDLER, &IRQ_HANDLER_NOACK);
    emu.run_frame();
    let count = read_counter(&mut emu, 0x0002);
    assert!(
        count > 100,
        "unacknowledged level IRQ should re-fire many times, got {count}"
    );
}

#[test]
fn irq_disabled_clears_the_line() {
    // Raise the line, confirm it is asserted, then disable both IRQ
    // sources via $4200, the line must drop immediately.
    let mut emu = loaded(&IDLE, &NMI_HANDLER, &IRQ_HANDLER_ACK);
    emu.bus_mut().system_regs_mut().set_irq_flag(true);
    assert!(emu.bus_mut().system_regs().irq_line());
    emu.bus_mut().write(0x4200, 0x00);
    assert!(!emu.bus_mut().system_regs().irq_line());
}

#[test]
fn irq_reschedule_cancels_stale_event() {
    // Arm V-IRQ at line 100, then move it to line 150. The stale
    // event must be cancelled, not stacked, so the pending count is
    // unchanged across the second arming (no frame events drain in
    // the two short steps, so any growth would be the leaked IrqV).
    let mut emu = loaded(&IDLE, &NMI_HANDLER, &IRQ_HANDLER_ACK);
    emu.bus_mut().write(0x4200, 0x20); // V-IRQ enable
    emu.bus_mut().write(0x4209, 0x64); // VTIMEL = 100
    emu.bus_mut().write(0x420A, 0x00);
    emu.step(); // drain reschedule -> one IrqV armed
    let before = emu.scheduler().pending_count();

    emu.bus_mut().write(0x4209, 0x96); // VTIMEL = 150
    emu.step(); // reschedule: cancel old IrqV, arm new one
    let after = emu.scheduler().pending_count();

    assert_eq!(
        after, before,
        "rescheduling must cancel the stale IrqV instead of accumulating events"
    );
}

// -------------------------------------------------------------
// TIMEUP polled while the IRQ is live
// -------------------------------------------------------------

/// Native-mode IRQ vector `$FFEE` (LoROM chip `$7FEE`).
const NATIVE_IRQ_VECTOR_OFF: usize = 0x7FEE;

/// Like [`loaded`], with the native-mode IRQ vector also pointing at the
/// IRQ handler, for reset code that switches to native mode.
fn loaded_native(reset: &[u8], irq: &[u8]) -> SnesEmulator {
    let mut rom = build_rom(reset, &NMI_HANDLER, irq);
    rom[NATIVE_IRQ_VECTOR_OFF..NATIVE_IRQ_VECTOR_OFF + 2]
        .copy_from_slice(&IRQ_HANDLER_ADDR.to_le_bytes());
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).expect("load_rom should succeed");
    emu
}

/// Native mode with a 16-bit accumulator and a V-IRQ on line 80.
///
/// `CLC` / `XCE` / `REP #$30` / `LDA #$0050` / `STA $4209` / `SEP #$20` /
/// `LDA #$20` / `STA $4200` / `REP #$20`.
const NATIVE_VIRQ_SETUP: [u8; 19] = [
    0x18, 0xFB, // CLC / XCE: native mode
    0xC2, 0x30, // REP #$30: 16-bit A, X, Y
    0xA9, 0x50, 0x00, 0x8D, 0x09, 0x42, // STA $4209 = VTIME (80)
    0xE2, 0x20, // SEP #$20
    0xA9, 0x20, 0x8D, 0x00, 0x42, // STA $4200 = V-IRQ enable
    0xC2, 0x20, // REP #$20
];

/// A wait loop whose `LDA $4210` reads `$4210` and `$4211` together and
/// leaves when TIMEUP's bit 7 (bit 15 of A) is set, counting each exit in
/// WRAM `$0004`.
///
/// `LDA $4210` / `BPL -5` / `INC $0004` / `BRA -10`.
const POLL_TIMEUP_LOOP: [u8; 10] = [
    0xAD, 0x10, 0x42, // LDA $4210
    0x10, 0xFB, // BPL back to the LDA
    0xEE, 0x04, 0x00, // INC $0004
    0x80, 0xF6, // BRA back to the LDA
];

/// Reset code for the TIMEUP polls: the setup, `nops` NOPs (which move
/// the point of the loop where the IRQ rises), a `CLI` when `cli` is set,
/// then the wait loop. Without the `CLI` the I flag stays set from reset,
/// so the IRQ is never taken and the loop is the only reader of `$4211`.
fn poll_timeup_code(cli: bool, nops: usize) -> Vec<u8> {
    let mut code = NATIVE_VIRQ_SETUP.to_vec();
    code.extend(std::iter::repeat_n(0xEA, nops));
    if cli {
        code.push(0x58);
    }
    code.extend_from_slice(&POLL_TIMEUP_LOOP);
    code
}

/// Native-mode IRQ handler with a 16-bit accumulator: acknowledge by
/// reading `$4211`, count the run in WRAM `$0002`, return.
///
/// `$8060 LDA $4211` / `$8063 INC $0002` / `$8066 RTI`.
const IRQ_HANDLER_ACK_NATIVE: [u8; 7] = [0xAD, 0x11, 0x42, 0xEE, 0x02, 0x00, 0x40];

#[test]
fn timeup_poll_sees_an_irq_rising_inside_the_read() {
    // The handler acknowledges every IRQ, so the wait loop can only leave
    // when its own read sees the flag first. On hardware the flag rises at
    // the IRQ's raster point, inside the `LDA` making the read, and the
    // CPU takes the interrupt only after that instruction; a loop polling
    // this way (Traverse: Starlight & Prairie waits on its line-2 IRQ like
    // this) leaves on the frames where the IRQ rises during the read.
    // Where the IRQ rises in the loop depends on the code that ran before
    // it, and once the handler runs that point repeats every frame, so try
    // several.
    let mut exits = 0;
    for nops in 0..8 {
        let mut emu = loaded_native(&poll_timeup_code(true, nops), &IRQ_HANDLER_ACK_NATIVE);
        for _ in 0..4 {
            emu.run_frame();
        }
        // The CPU still takes every IRQ: one handler run per frame.
        assert_eq!(read_counter(&mut emu, 0x0002), 4, "{nops} NOPs");
        exits += read_counter(&mut emu, 0x0004);
    }
    assert!(exits > 0, "the wait loop never saw TIMEUP's bit 7");
}

#[test]
fn timeup_poll_with_i_set_sees_each_irq_once() {
    // With I set the IRQ is never taken. A read that sees the IRQ inside
    // the instruction during which it rises acknowledges it, so it cannot
    // be seen a second time once its event fires: one exit per frame.
    let mut emu = loaded_native(&poll_timeup_code(false, 0), &IRQ_HANDLER_ACK_NATIVE);
    for _ in 0..30 {
        emu.run_frame();
    }
    assert_eq!(read_counter(&mut emu, 0x0004), 30);
    assert_eq!(read_counter(&mut emu, 0x0002), 0, "the handler never runs");
}

// -------------------------------------------------------------
// Combined smoke
// -------------------------------------------------------------

/// `CLI / LDA #$A0 / STA $4200 / LDA #$50 / STA $4209 / LDA #$00 /
/// STA $420A / BRA -2`, NMI (bit7) + V-IRQ (bit5) on line 80.
const RESET_ENABLE_NMI_AND_VIRQ: [u8; 18] = [
    0x58, // CLI
    0xA9, 0xA0, 0x8D, 0x00, 0x42, // STA $4200 = NMI + V-IRQ
    0xA9, 0x50, 0x8D, 0x09, 0x42, // STA $4209 = VTIMEL (80)
    0xA9, 0x00, 0x8D, 0x0A, 0x42, // STA $420A = VTIMEH (0)
    0x80, 0xFE, // BRA -2
];

#[test]
fn nmi_and_irq_coexist_over_frames() {
    let mut emu = loaded(&RESET_ENABLE_NMI_AND_VIRQ, &NMI_HANDLER, &IRQ_HANDLER_ACK);
    for _ in 0..6 {
        emu.run_frame();
    }
    assert_eq!(read_counter(&mut emu, 0x0000), 6, "one NMI per frame");
    assert_eq!(read_counter(&mut emu, 0x0002), 6, "one V-IRQ per frame");
    assert_eq!(emu.frame_count(), 6);
}
