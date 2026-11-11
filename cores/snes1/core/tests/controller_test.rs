//! End-to-end standard-pad controller tests.
//!
//! Two access paths are exercised against the full [`SnesEmulator`]:
//!
//! - **Auto-joypad read**: with `NMITIMEN` bit 0 set, `run_frame`'s
//!   `VBlankStart` samples the pads and latches the 16-bit snapshot
//!   into JOY1/JOY2 (`$4218-$421B`), raising the HVBJOY busy flag
//!   (`$4212` bit 0) for the read window.
//! - **Manual serial read**: a `$4016` strobe latches each pad's shift
//!   register and successive `$4016`/`$4017` reads clock the buttons
//!   out MSB-first (B, Y, Select, Start, Up, Down, Left, Right, A, X,
//!   L, R), then read `1`.
//!
//! The pad shift/serial mechanics are unit-tested in `controller/mod.rs`
//! and the register plumbing in `bus.rs`; these cases pin the
//! integration through `set_input` -> input manager -> bus pads ->
//! scheduler that the unit layers can't reach.
//!
//! Reference: bsnes_v086 `controller/gamepad/gamepad.cpp` (serial) and
//! bsnes-plus `cpu/timing/joypad.cpp` (auto-read).

use retro_core::cpu::Addressable;
use retro_core::{EmulatorCore, InputState};
use snes1_core::controller::{BTN_A, BTN_B, BTN_START, BTN_UP, BTN_Y};
use snes1_core::{SnesEmulator, TimingProfile};

// -------------------------------------------------------------
// ROM synthesis (minimal idle LoROM, mirrors irq_test.rs)
// -------------------------------------------------------------

const RESET_BASE: usize = 0x0000;
const NMI_HANDLER_OFF: usize = 0x0040;
const NMI_HANDLER_ADDR: u16 = 0x8040;
const IRQ_HANDLER_ADDR: u16 = 0x8060;

fn stamp_header(chip: &mut [u8], header_offset: usize, reset_vector: u16) {
    let title = b"CONTROLLER TEST      ";
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

/// Build a 1 MiB LoROM with the given reset code and NMI handler, IRQ
/// vector pointing at a bare `RTI`.
fn build_rom(reset_code: &[u8], nmi_handler: &[u8]) -> Vec<u8> {
    const SIZE: usize = 0x10_0000;
    let mut chip = vec![0u8; SIZE];
    chip[RESET_BASE..RESET_BASE + reset_code.len()].copy_from_slice(reset_code);
    chip[NMI_HANDLER_OFF..NMI_HANDLER_OFF + nmi_handler.len()].copy_from_slice(nmi_handler);
    // IRQ handler is a lone RTI at $8060, never armed here.
    chip[0x0060] = 0x40;
    stamp_header(&mut chip, 0x7FB0, 0x8000);
    chip[0x7FFA..0x7FFC].copy_from_slice(&NMI_HANDLER_ADDR.to_le_bytes());
    chip[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    chip[0x7FFE..0x8000].copy_from_slice(&IRQ_HANDLER_ADDR.to_le_bytes());
    chip
}

// -------------------------------------------------------------
// Hand-assembled fragments (65C816, emulation mode / 8-bit)
// -------------------------------------------------------------

/// `LDA #$01 / STA $4200 / BRA -2`: enable auto-joypad read (NMITIMEN
/// bit 0), NMI disabled, then idle.
const RESET_ENABLE_AUTOREAD: [u8; 7] = [0xA9, 0x01, 0x8D, 0x00, 0x42, 0x80, 0xFE];

/// `LDA #$81 / STA $4200 / BRA -2`: enable NMI (bit 7) + auto-joypad
/// read (bit 0), then idle.
const RESET_ENABLE_NMI_AUTOREAD: [u8; 7] = [0xA9, 0x81, 0x8D, 0x00, 0x42, 0x80, 0xFE];

/// Bare `RTI` NMI handler for the auto-read-only cases (NMI never
/// enabled, so it is unreachable, but the vector must point somewhere).
const NMI_RTI: [u8; 1] = [0x40];

/// NMI handler that snapshots the joypad MMIO the moment the CPU enters
/// V-blank: exactly when a real game polls. Stores `$4212` -> DP `$10`,
/// `$4218` (JOY1L) -> DP `$11`, `$4219` (JOY1H) -> DP `$12`, then `RTI`.
///
/// `LDA $4212 / STA $10 / LDA $4218 / STA $11 / LDA $4219 / STA $12 / RTI`
const NMI_CAPTURE_JOY: [u8; 16] = [
    0xAD, 0x12, 0x42, // LDA $4212 (HVBJOY)
    0x85, 0x10, // STA $10
    0xAD, 0x18, 0x42, // LDA $4218 (JOY1L)
    0x85, 0x11, // STA $11
    0xAD, 0x19, 0x42, // LDA $4219 (JOY1H)
    0x85, 0x12, // STA $12
    0x40, // RTI
];

fn loaded(reset: &[u8], nmi: &[u8]) -> SnesEmulator {
    let rom = build_rom(reset, nmi);
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).expect("load_rom should succeed");
    assert_eq!(emu.timing(), TimingProfile::NTSC);
    emu
}

fn press(emu: &mut SnesEmulator, port: u8, buttons: u32) {
    emu.set_input(port, InputState { buttons });
}

/// Drive a `$4016` strobe pulse (high then low) to latch both pads,
/// then read `count` serial bits from `$4016` (port 0) bit 0.
fn manual_strobe_and_read(emu: &mut SnesEmulator, count: usize) -> Vec<u8> {
    emu.bus_mut().write(0x00_4016, 0x01);
    emu.bus_mut().write(0x00_4016, 0x00);
    (0..count)
        .map(|_| emu.bus_mut().read(0x00_4016) & 1)
        .collect()
}

// -------------------------------------------------------------
// Auto-joypad read
// -------------------------------------------------------------

#[test]
fn auto_read_after_vblank_latches_joy1() {
    let mut emu = loaded(&RESET_ENABLE_AUTOREAD, &NMI_RTI);
    press(&mut emu, 0, 1 << BTN_B);
    emu.run_frame();
    // JOY1 ($4218 low / $4219 high) holds B = bit 15 -> $4219 bit 7.
    assert_eq!(emu.bus_mut().read(0x00_4218), 0x00, "JOY1L");
    assert_eq!(emu.bus_mut().read(0x00_4219), 0x80, "JOY1H = B");
    // JOY2 / JOY3 / JOY4 stay clear (nothing on port 1, no multitap).
    for offset in 0x421Au32..=0x421F {
        assert_eq!(emu.bus_mut().read(offset), 0x00, "{offset:#06X}");
    }
}

#[test]
fn auto_read_does_not_run_when_disabled() {
    // Idle ROM never writes $4200 -> NMITIMEN bit 0 stays 0, so the
    // auto-read never fires and the JOY buffer remains zero even with a
    // button held.
    let mut emu = loaded(&[0x80, 0xFE], &NMI_RTI);
    press(&mut emu, 0, 1 << BTN_B);
    emu.run_frame();
    assert_eq!(emu.bus_mut().read(0x00_4219), 0x00);
}

#[test]
fn joy2_independent_of_joy1() {
    let mut emu = loaded(&RESET_ENABLE_AUTOREAD, &NMI_RTI);
    // Start on port 1 only (bit 12 of the SNES layout -> $421B bit 4).
    press(&mut emu, 1, 1 << BTN_START);
    emu.run_frame();
    // JOY1 untouched.
    assert_eq!(emu.bus_mut().read(0x00_4218), 0x00, "JOY1L");
    assert_eq!(emu.bus_mut().read(0x00_4219), 0x00, "JOY1H");
    // JOY2 carries Start (0x1000).
    assert_eq!(emu.bus_mut().read(0x00_421A), 0x00, "JOY2L");
    assert_eq!(emu.bus_mut().read(0x00_421B), 0x10, "JOY2H = Start");
}

#[test]
fn busy_flag_set_during_auto_read_then_cleared() {
    // NMI fires right after the auto-read latch, so the handler observes
    // the busy window directly; the scheduled AutoJoypadDone clears it
    // long before the frame ends.
    let mut emu = loaded(&RESET_ENABLE_NMI_AUTOREAD, &NMI_CAPTURE_JOY);
    press(&mut emu, 0, 1 << BTN_B);
    emu.run_frame();
    let captured_hvbjoy = emu.bus_mut().read(0x00_0010);
    // During the NMI: VBlank (bit 7) and joypad-busy (bit 0) both set.
    assert_eq!(captured_hvbjoy & 0x80, 0x80, "VBlank flag during NMI");
    assert_eq!(captured_hvbjoy & 0x01, 0x01, "joypad busy during NMI");
    // The handler also saw the freshly-latched JOY1 (B in $4219 bit 7).
    assert_eq!(emu.bus_mut().read(0x00_0012), 0x80, "JOY1H seen by NMI");
    // After the frame the busy flag has been lowered again.
    assert_eq!(emu.bus_mut().read(0x00_4212) & 0x01, 0x00, "busy cleared");
}

// -------------------------------------------------------------
// Auto-read <-> manual-read interaction (what Dark Law relies on)
// -------------------------------------------------------------

#[test]
fn auto_read_leaves_serial_exhausted_for_unstrobed_manual_read() {
    // Dark Law enables the hardware auto-read (NMITIMEN bit 0) and never
    // strobes $4016 itself: its per-frame NMI routine polls $4016 bit 0
    // directly, relying on the auto-read to have clocked the shift
    // register out to its exhausted (idle-high) state. On real hardware
    // that read returns 1. If the auto-read instead leaves the register
    // primed at bit 0, an *unpressed* B reads 0 and Dark Law zeroes its
    // input variables, d-pad dead, only B "works".
    let mut emu = loaded(&RESET_ENABLE_AUTOREAD, &NMI_RTI);
    // Hold Up but NOT B, so a register primed at bit 0 (B) would read 0,
    // the case that would expose the bug.
    press(&mut emu, 0, 1 << BTN_UP);
    emu.run_frame();
    // The auto-read still latches the live snapshot: Up is bit 11 of the
    // 16-bit word -> $4219 (JOY1H) bit 3.
    assert_eq!(
        emu.bus_mut().read(0x00_4219) & 0x08,
        0x08,
        "Up latched into JOY1H by the auto-read"
    );
    // A manual $4016/$4017 read WITHOUT a re-strobe returns the idle
    // data-line bit (1): the auto-read exhausted the shift register.
    assert_eq!(
        emu.bus_mut().read(0x00_4016) & 1,
        1,
        "$4016 idle (1) after auto-read, not stale button data"
    );
    assert_eq!(
        emu.bus_mut().read(0x00_4017) & 1,
        1,
        "$4017 idle (1) after auto-read"
    );
}

#[test]
fn auto_read_then_manual_strobe_still_reads_buttons() {
    // The exhaustion must not break the normal strobe path: a game that
    // DOES re-strobe after the auto-read reloads the shift register and
    // reads live buttons as usual.
    let mut emu = loaded(&RESET_ENABLE_AUTOREAD, &NMI_RTI);
    press(&mut emu, 0, 1 << BTN_B);
    emu.run_frame();
    // Re-strobe and clock: B is back as the first serial bit.
    let bits = manual_strobe_and_read(&mut emu, 2);
    assert_eq!(bits[0], 1, "B visible again after a manual re-strobe");
    assert_eq!(bits[1], 0, "Y unpressed");
}

// -------------------------------------------------------------
// Manual serial read ($4016/$4017)
// -------------------------------------------------------------

#[test]
fn manual_serial_read_b_button() {
    let mut emu = SnesEmulator::new();
    press(&mut emu, 0, 1 << BTN_B);
    let bits = manual_strobe_and_read(&mut emu, 18);
    // Bit 0 = B = 1; bits 1..=15 = 0; bits 16+ pull high.
    assert_eq!(bits[0], 1, "first serial bit is B");
    assert_eq!(&bits[1..16], &[0u8; 15], "remaining buttons unpressed");
    assert_eq!(&bits[16..18], &[1u8, 1u8], "past 16 bits reads 1");
}

#[test]
fn manual_serial_emits_buttons_in_hardware_order() {
    let mut emu = SnesEmulator::new();
    // B + Y + A held simultaneously.
    press(&mut emu, 0, (1 << BTN_B) | (1 << BTN_Y) | (1 << BTN_A));
    let bits = manual_strobe_and_read(&mut emu, 12);
    // Order: B Y Select Start Up Down Left Right A X L R.
    assert_eq!(bits, vec![1, 1, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0]);
}

#[test]
fn manual_serial_reads_one_without_strobe_until_latched() {
    // Fresh pad, no strobe yet: the shift register starts cleared, so
    // serial reads return 0 until the first 16 are exhausted, then 1.
    let mut emu = SnesEmulator::new();
    press(&mut emu, 0, 1 << BTN_B);
    // No strobe -> no latch -> register still all-zero; B is not visible.
    assert_eq!(emu.bus_mut().read(0x00_4016) & 1, 0);
}

#[test]
fn port1_serial_is_read_through_4017() {
    let mut emu = SnesEmulator::new();
    press(&mut emu, 1, 1 << BTN_B);
    emu.bus_mut().write(0x00_4016, 0x01);
    emu.bus_mut().write(0x00_4016, 0x00);
    // $4017 clocks port 1; first bit is B.
    assert_eq!(emu.bus_mut().read(0x00_4017) & 1, 1);
    assert_eq!(emu.bus_mut().read(0x00_4017) & 1, 0);
}
