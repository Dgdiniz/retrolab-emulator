//! The machine conformance suite (`retro_core::emulator::conformance`) on
//! the Mega Drive: a program that keys a PSG tone, then reads pad 1 forever
//! and adds what it reads into work RAM, its cartridge with battery RAM.

use retro_core::emulator::conformance::MachineUnderTest;
use retro_core::{EmulatorCore, ExecutionMode};
use sg1_core::GenesisEmulator;

mod helpers;
use helpers::MiniCartridgeBuilder;

/// Pad 1's buttons in `InputState::buttons` the program can see: the port
/// stays in its TH=1 phase, where B, C and the directions are.
const BTN_B: u32 = 1 << 4;
const BTN_C: u32 = 1 << 5;

/// ```text
/// 0x400: MOVE.B #$8F, $00C00011   ; tone 0, low four bits
///        MOVE.B #$03, $00C00011   ; tone 0, high bits
///        MOVE.B #$90, $00C00011   ; tone 0 at full volume
/// 0x418: MOVE.B $00A10003, D1     ; pad 1's data port
///        ADD.B  D1, $00FF0000     ; RAM[$FF0000] += D1
///        BRA.w  0x418
/// ```
fn program(title: &str) -> Vec<u8> {
    MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title(title)
        .with_sram(0x2000)
        .with_instructions(
            0x400,
            &[
                0x13FC, 0x008F, 0x00C0, 0x0011, // MOVE.B #$8F, $C00011
                0x13FC, 0x0003, 0x00C0, 0x0011, // MOVE.B #$03, $C00011
                0x13FC, 0x0090, 0x00C0, 0x0011, // MOVE.B #$90, $C00011
                0x1239, 0x00A1, 0x0003, // MOVE.B $A10003, D1
                0xD339, 0x00FF, 0x0000, // ADD.B D1, $FF0000
                0x6000, 0xFFF2, // BRA.w 0x418
            ],
        )
        .build()
}

fn rom() -> Vec<u8> {
    program("MACHINE CONFORMANCE")
}

fn other_rom() -> Vec<u8> {
    program("MACHINE CONFORMANCE B")
}

fn build() -> GenesisEmulator {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(&rom()).expect("the program loads");
    emu.set_audio_enabled(true);
    emu.set_execution_mode(ExecutionMode::Running);
    emu
}

fn input(frame: u64) -> u32 {
    match frame {
        120..=239 => BTN_C,
        240..=359 => BTN_B,
        _ => 0,
    }
}

fn machine() -> MachineUnderTest<GenesisEmulator> {
    MachineUnderTest {
        build,
        rom,
        other_rom,
        input,
        work_ram: (0x00FF_0000, 16),
        drift_seconds: 30,
    }
}

retro_core::emulator_conformance_tests!(machine());
