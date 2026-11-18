//! The machine conformance suite (`retro_core::emulator::conformance`) on
//! the SNES: a LoROM whose reset routine turns the NMI and the automatic
//! pad read on, then adds pad 1's bytes into work RAM forever, its
//! cartridge with battery RAM.

use retro_core::emulator::conformance::MachineUnderTest;
use retro_core::{Breakpoint, BreakpointType, EmulatorCore, ExecutionMode};
use snes1_core::{snes_events, SnesEmulator};

/// Pad 1's buttons in `InputState::buttons`: A spans JOY1's low byte and
/// Start its high byte, so pressing both moves both sums.
const BTN_A: u32 = 1 << 4;
const BTN_B: u32 = 1 << 5;
const BTN_START: u32 = 1 << 10;

/// ```text
/// LDA #$80 / STA $2100   ; forced blank
/// LDA #$81 / STA $4200   ; NMI (bit 7) and the automatic pad read (bit 0)
/// LDA #$0F / STA $2100   ; display on, full brightness
/// loop:
///   LDA $4218 / CLC / ADC $00 / STA $00   ; RAM[$00] += JOY1L
///   LDA $4219 / CLC / ADC $01 / STA $01   ; RAM[$01] += JOY1H
///   BRA loop
/// ```
#[rustfmt::skip]
const RESET: [u8; 33] = [
    0xA9, 0x80, 0x8D, 0x00, 0x21,
    0xA9, 0x81, 0x8D, 0x00, 0x42,
    0xA9, 0x0F, 0x8D, 0x00, 0x21,
    0xAD, 0x18, 0x42,
    0x18, 0x65, 0x00, 0x85, 0x00,
    0xAD, 0x19, 0x42,
    0x18, 0x65, 0x01, 0x85, 0x01,
    0x80, 0xEE,
];

fn program(title: &[u8; 21]) -> Vec<u8> {
    let mut chip = vec![0u8; 0x10_0000];
    chip[..RESET.len()].copy_from_slice(&RESET);
    chip[0x40] = 0x40; // NMI: RTI
    chip[0x60] = 0x40; // IRQ: RTI
    let h = 0x7FB0;
    chip[h + 0x10..h + 0x10 + 21].copy_from_slice(title);
    chip[h + 0x25] = 0x20; // LoROM
    chip[h + 0x26] = 0x02; // ROM, RAM and battery
    chip[h + 0x27] = 0x0A; // 1 MiB
    chip[h + 0x28] = 0x03; // 8 KiB of RAM
    chip[h + 0x29] = 0x01; // North America: NTSC
    chip[h + 0x2A] = 0x01;
    let checksum: u16 = 0x1234;
    chip[h + 0x2C..h + 0x2E].copy_from_slice(&(!checksum).to_le_bytes());
    chip[h + 0x2E..h + 0x30].copy_from_slice(&checksum.to_le_bytes());
    chip[h + 0x4C..h + 0x4E].copy_from_slice(&0x8000u16.to_le_bytes());
    chip[0x7FFA..0x7FFC].copy_from_slice(&0x8040u16.to_le_bytes());
    chip[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    chip[0x7FFE..0x8000].copy_from_slice(&0x8060u16.to_le_bytes());
    chip
}

fn rom() -> Vec<u8> {
    program(b"MACHINE CONFORMANCE  ")
}

fn other_rom() -> Vec<u8> {
    program(b"MACHINE CONFORMANCE B")
}

fn build() -> SnesEmulator {
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom()).expect("the program loads");
    emu.set_audio_enabled(true);
    emu.set_execution_mode(ExecutionMode::Running);
    emu
}

fn input(frame: u64) -> u32 {
    match frame {
        120..=239 => BTN_A | BTN_B,
        240..=359 => BTN_START,
        _ => 0,
    }
}

/// Stopped on the NMI event: the program never reads `$4210`, so its NMI
/// is on its way, delivered three instructions on.
fn an_nmi_on_its_way(emu: &mut SnesEmulator) {
    let id = emu.debugger().add_breakpoint(Breakpoint {
        address: 0,
        bp_type: BreakpointType::Event {
            code: snes_events::NMI,
            param: 0,
        },
        cpu_id: None,
        condition: None,
    });
    emu.run_frame();
    assert_eq!(
        emu.execution_mode(),
        ExecutionMode::Paused,
        "stopped on the NMI"
    );
    emu.debugger().remove_breakpoint(id);
}

fn machine() -> MachineUnderTest<SnesEmulator> {
    MachineUnderTest {
        build,
        rom,
        other_rom,
        input,
        work_ram: (0x7E_0000, 16),
        drift_seconds: 30,
        extension_moment: Some(an_nmi_on_its_way),
        // The pad read at the top of the program's loop.
        mid_frame_stop: Breakpoint {
            address: 0x00_800F,
            bp_type: BreakpointType::execute(),
            cpu_id: Some(0),
            condition: None,
        },
    }
}

retro_core::emulator_conformance_tests!(machine());
