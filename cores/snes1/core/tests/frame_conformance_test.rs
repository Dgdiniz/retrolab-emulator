//! The frame driver's conformance suite (`retro_core::frame::conformance`)
//! run on the SNES core, with a program of its own.
//!
//! The program turns the NMI on, then loops: a call to a routine of two
//! instructions, sixteen turns of a `DEX` / `BNE` loop, and a `WAI` for the
//! NMI. The NMI handler counts the frames and returns.

use retro_core::frame::conformance::FrameCoreUnderTest;
use retro_core::{Breakpoint, BreakpointType, EmulatorCore};
use snes1_core::debugger::snes_events;
use snes1_core::SnesEmulator;

/// The call and the address it returns to.
const CALL: u32 = 0x00_800A;
const RETURN: u32 = 0x00_800D;
/// The `DEX` the loop turns on.
const REPEATED: u32 = 0x00_800F;
/// The instruction after the `WAI`, where the CPU waits.
const WAITING_AT: u32 = 0x00_8013;
/// The NMI handler.
const HANDLER: u32 = 0x00_8030;

/// The program of the module's notes, in a 32 KiB LoROM.
fn program() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    let code: &[u8] = &[
        0x78, // $8000 SEI
        0x18, // $8001 CLC
        0xFB, // $8002 XCE: native
        0xE2, 0x30, // $8003 SEP #$30
        0xA9, 0x80, // $8005 LDA #$80
        0x8D, 0x00, 0x42, // $8007 STA $4200: the NMI on
        0x20, 0x20, 0x80, // $800A JSR $8020
        0xA2, 0x10, // $800D LDX #$10
        0xCA, // $800F DEX
        0xD0, 0xFD, // $8010 BNE $800F
        0xCB, // $8012 WAI
        0x80, 0xF5, // $8013 BRA $800A
    ];
    rom[..code.len()].copy_from_slice(code);
    rom[0x20] = 0xEA; // $8020 NOP
    rom[0x21] = 0x60; // $8021 RTS
    rom[0x30..0x33].copy_from_slice(&[0xE6, 0x10, 0x40]); // $8030 INC $10 / RTI
    rom[0x7FC0..0x7FD5].copy_from_slice(b"FRAME CONFORMANCE    ");
    rom[0x7FD5] = 0x20; // LoROM
    rom[0x7FD7] = 0x08; // 256 KiB
    rom[0x7FD9] = 0x01; // USA, NTSC
    rom[0x7FDC..0x7FE0].copy_from_slice(&[0xFF, 0xFF, 0x00, 0x00]);
    rom[0x7FEA..0x7FEC].copy_from_slice(&0x8030u16.to_le_bytes()); // native NMI
    rom[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes()); // reset
    rom
}

fn build() -> SnesEmulator {
    let mut emu = SnesEmulator::new();
    emu.load_rom(&program()).expect("the program loads");
    emu
}

/// The SNES core, as the suite takes it.
fn snes() -> FrameCoreUnderTest<SnesEmulator> {
    FrameCoreUnderTest {
        build,
        rom: program(),
        state_hash: SnesEmulator::state_hash,
        state_parts: SnesEmulator::state_hash_parts,
        frame_count: SnesEmulator::frame_count,
        repeated_address: REPEATED,
        handler_address: HANDLER,
        call: (CALL, RETURN),
        stop_in_picture: Breakpoint {
            address: 0,
            bp_type: BreakpointType::Event {
                code: snes_events::SCANLINE,
                param: 100,
            },
            cpu_id: None,
            condition: None,
        },
        waiting_at: WAITING_AT,
    }
}

retro_core::frame_conformance_tests!(snes());
