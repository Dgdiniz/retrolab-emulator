//! End-to-end Super FX (GSU) -> S-CPU IRQ handshake test.
//!
//! Synthesizes a minimal Super FX LoROM whose reset code starts the GSU
//! (`R15 = $0100` + GO, via `$301E`/`$301F`) and then idles in a `BRA -2`
//! loop. The GSU kernel loads `R0 = $1234` and executes `stop`, which, unless
//! masked by `CFGR.irq`, latches `SFR.irq` and asserts the S-CPU IRQ line.
//! The emulation-mode IRQ vector points at a handler that acknowledges the
//! latch (`LDA $3031`), bumps a 16-bit WRAM counter, and `RTI`s.
//!
//! Driving the whole [`SnesEmulator`] through `run_frame` exercises the entire
//! chain that the unit layers can't reach on their own:
//!
//! ```text
//! S-CPU CLI + STA $301E/$301F -> GSU GO -> per-instruction catch-up ->
//! GSU runs `iwt r0,#$1234 ; stop` -> SFR.irq -> cart_has_irq() ->
//! cpu.irq_pending (head of next instr) -> 65816 IRQ vector $FFFE ->
//! handler LDA $3031 (clears the latch) -> INC counter -> RTI
//! ```
//!
//! The GSU register/flag semantics are unit-tested in the `superfx-gsu` crate
//! (`stop_clears_go_and_raises_irq`, `stop_irq_masked_by_cfgr`); the chip-level
//! raise/clear in `cartridge::superfx` (`stop_raises_scpu_irq_and_read_3031_
//! clears_it`, `masked_stop_does_not_raise_irq`). The cases here pin the
//! integration through the real 65C816 those layers stop short of.
//!
//! Reference: bsnes-plus `snes/chip/superfx/*` + the S-CPU IRQ path in
//! `cpu/timing/irq.cpp`.
//!
//! Needs the `chip-superfx` feature (on by default): without the Super FX's
//! module this file compiles to nothing.

#![cfg(feature = "chip-superfx")]

use retro_core::cpu::Addressable;
use retro_core::EmulatorCore;
use snes1_core::{SnesEmulator, TimingProfile};

// -------------------------------------------------------------
// ROM synthesis
// -------------------------------------------------------------

/// S-CPU reset code base `$00:8000` (LoROM chip offset 0).
const RESET_BASE: usize = 0x0000;
/// IRQ handler base `$00:8060` (LoROM chip offset `$0060`).
const IRQ_HANDLER_OFF: usize = 0x0060;
/// GSU kernel base, chip offset `$0100`, reached by the GSU at `PBR = 0`,
/// `R15 = $0100`. Disjoint from the S-CPU reset code and IRQ handler.
const GSU_KERNEL_OFF: usize = 0x0100;
const IRQ_HANDLER_ADDR: u16 = 0x8060;

/// Stamp a Super FX LoROM header: LoROM map mode (`$20`) with a Super FX
/// `cartridge_type` (`$13`, Star Fox's 32 KiB volatile scratch RAM), so
/// [`snes1_core`] detection promotes the preset to Super FX and installs the
/// GSU. Otherwise identical to a stock LoROM header (title, ROM-size code,
/// USA/NTSC destination, consistent checksum, reset vector).
fn stamp_header(chip: &mut [u8], header_offset: usize, reset_vector: u16) {
    let title = b"SUPERFX IRQ TEST     ";
    chip[header_offset + 0x10..header_offset + 0x10 + 21].copy_from_slice(title);
    chip[header_offset + 0x25] = 0x20; // LoROM map mode (Super FX shares it)
    chip[header_offset + 0x26] = 0x13; // cartridge_type $13 -> Super FX (Star Fox)
    chip[header_offset + 0x27] = 0x0A; // 1 MiB
    chip[header_offset + 0x28] = 0x00; // no standard SRAM (GSU RAM is derived)
    chip[header_offset + 0x29] = 0x01; // USA -> NTSC
    chip[header_offset + 0x2A] = 0x01;
    chip[header_offset + 0x2B] = 0x00;
    let checksum: u16 = 0x1234;
    chip[header_offset + 0x2C..header_offset + 0x2E].copy_from_slice(&(!checksum).to_le_bytes());
    chip[header_offset + 0x2E..header_offset + 0x30].copy_from_slice(&checksum.to_le_bytes());
    chip[header_offset + 0x4C..header_offset + 0x4E].copy_from_slice(&reset_vector.to_le_bytes());
}

/// Build a 1 MiB Super FX LoROM: `reset_code` at chip `$0000`, `irq_handler`
/// at chip `$0060`, and `gsu_kernel` at chip `$0100` (the GSU program). The
/// emulation-mode reset vector (`$FFFC`) points at `$8000` and the IRQ vector
/// (`$FFFE`) at the handler; the NMI vector is left inert (NMI never enabled).
fn build_rom(reset_code: &[u8], irq_handler: &[u8], gsu_kernel: &[u8]) -> Vec<u8> {
    const SIZE: usize = 0x10_0000;
    let mut chip = vec![0u8; SIZE];
    chip[RESET_BASE..RESET_BASE + reset_code.len()].copy_from_slice(reset_code);
    chip[IRQ_HANDLER_OFF..IRQ_HANDLER_OFF + irq_handler.len()].copy_from_slice(irq_handler);
    chip[GSU_KERNEL_OFF..GSU_KERNEL_OFF + gsu_kernel.len()].copy_from_slice(gsu_kernel);
    stamp_header(&mut chip, 0x7FB0, 0x8000);
    // Emulation-mode vectors live at chip $7FFA (NMI) / $7FFC (reset) /
    // $7FFE (IRQ). Reset -> $8000, IRQ -> the handler.
    chip[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    chip[0x7FFE..0x8000].copy_from_slice(&IRQ_HANDLER_ADDR.to_le_bytes());
    chip
}

// -------------------------------------------------------------
// Hand-assembled fragments
// -------------------------------------------------------------

/// GSU kernel: `iwt r0,#$1234 ; stop`. A real compute op (so `R0` proves the
/// GSU executed) followed by the `stop` that raises the S-CPU IRQ.
const GSU_KERNEL: [u8; 4] = [0xF0, 0x34, 0x12, 0x00];

/// S-CPU reset code (65C816 emulation / 8-bit): unmask IRQs, start the GSU,
/// then idle. A coprocessor IRQ needs no `$4200` arming, only `CLI`.
///
/// `CLI` / `LDA #$00 : STA $301E` (R15 low) / `LDA #$01 : STA $301F`
/// (R15 high = $0100, sets GO) / `BRA -2`.
const RESET_ARM_GSU: [u8; 13] = [
    0x58, // CLI
    0xA9, 0x00, 0x8D, 0x1E, 0x30, // LDA #$00 : STA $301E  (R15 low)
    0xA9, 0x01, 0x8D, 0x1F, 0x30, // LDA #$01 : STA $301F  (R15 high -> GO)
    0x80, 0xFE, // BRA -2
];

/// Same as [`RESET_ARM_GSU`] but first sets `CFGR.irq` (`$3037` bit 7), so the
/// GSU `stop` is masked and raises no S-CPU IRQ.
///
/// `LDA #$80 : STA $3037` (CFGR.irq) / `CLI` / `LDA #$00 : STA $301E` /
/// `LDA #$01 : STA $301F` / `BRA -2`.
const RESET_ARM_GSU_MASKED: [u8; 18] = [
    0xA9, 0x80, 0x8D, 0x37, 0x30, // LDA #$80 : STA $3037  (CFGR.irq = 1)
    0x58, // CLI
    0xA9, 0x00, 0x8D, 0x1E, 0x30, // LDA #$00 : STA $301E  (R15 low)
    0xA9, 0x01, 0x8D, 0x1F, 0x30, // LDA #$01 : STA $301F  (R15 high -> GO)
    0x80, 0xFE, // BRA -2
];

/// IRQ handler: acknowledge the GSU STOP interrupt by reading `$3031` (which
/// deasserts the level line), bump a 16-bit counter at DP `$02/$03`, `RTI`.
///
/// `LDA $3031` / `INC $02` / `BNE +2` / `INC $03` / `RTI`.
const IRQ_HANDLER_ACK_3031: [u8; 10] = [0xAD, 0x31, 0x30, 0xE6, 0x02, 0xD0, 0x02, 0xE6, 0x03, 0x40];

/// Read the little-endian 16-bit counter at WRAM `addr`.
fn read_counter(emu: &mut SnesEmulator, addr: u32) -> u16 {
    let lo = u16::from(emu.bus_mut().read(addr));
    let hi = u16::from(emu.bus_mut().read(addr + 1));
    lo | (hi << 8)
}

/// Read the GSU's `R0` back through the `$3000/$3001` MMIO window (little-
/// endian). Side-effect-free for a stopped GSU: the catch-up is a no-op and
/// only `$3031` reads clear the IRQ latch.
fn read_gsu_r0(emu: &mut SnesEmulator) -> u16 {
    let bytes = emu.read_memory_debug(0x3000, 2);
    u16::from(bytes[0]) | (u16::from(bytes[1]) << 8)
}

fn loaded(reset: &[u8], irq: &[u8], kernel: &[u8]) -> SnesEmulator {
    let rom = build_rom(reset, irq, kernel);
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).expect("load_rom should succeed");
    assert_eq!(emu.timing(), TimingProfile::NTSC);
    emu
}

// -------------------------------------------------------------
// Tests
// -------------------------------------------------------------

#[test]
fn gsu_stop_raises_scpu_irq_and_handler_runs() {
    let mut emu = loaded(&RESET_ARM_GSU, &IRQ_HANDLER_ACK_3031, &GSU_KERNEL);
    for _ in 0..3 {
        emu.run_frame();
    }
    // Exactly one IRQ: firing at all proves the GSU `stop` raised the line and
    // it was delivered through the real 65C816; the count being *exactly* one
    // (not thousands) proves the handler's `$3031` read lowered the level line
    // so it never re-fired.
    assert_eq!(
        read_counter(&mut emu, 0x0002),
        1,
        "an unmasked GSU stop should deliver exactly one S-CPU IRQ"
    );
    // The GSU actually ran its kernel (R0 = $1234), so the IRQ is a real
    // handshake, not a spurious line.
    assert_eq!(
        read_gsu_r0(&mut emu),
        0x1234,
        "the GSU kernel ran (iwt r0,#$1234) before stopping"
    );
}

#[test]
fn gsu_stop_irq_masked_by_cfgr_never_fires() {
    let mut emu = loaded(&RESET_ARM_GSU_MASKED, &IRQ_HANDLER_ACK_3031, &GSU_KERNEL);
    for _ in 0..3 {
        emu.run_frame();
    }
    // CFGR.irq masks the STOP interrupt: the handler never runs.
    assert_eq!(
        read_counter(&mut emu, 0x0002),
        0,
        "a CFGR-masked GSU stop must raise no S-CPU IRQ"
    );
    // But the GSU still executed and stopped (R0 = $1234), proving the masked
    // case is a real run, not a vacuous pass where the GSU never started.
    assert_eq!(
        read_gsu_r0(&mut emu),
        0x1234,
        "the GSU kernel still ran + stopped under a masked IRQ"
    );
}
