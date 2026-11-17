//! The rules the debugger keeps on the Mega Drive that are the console's
//! own, each shown on a small program: what the 68000's calls of each
//! length and their returns do to the call stack, what the Z80's own step
//! leaves in the debugger, and that the debugger's own reads of every
//! chip's window leave the machine as it was.
//!
//! The rules of the frame loops and the host step (running on from a stop,
//! what a host step records, a program waiting in `STOP` waking on every
//! tier and under host steps, what a reset or a load empties) are the frame
//! driver's conformance suite, run on this core in `frame_conformance.rs`;
//! the history's order and the trace rings' are `retro_core`'s own tests.

mod helpers;

use helpers::M68kProgram;
use retro_core::cpu::Addressable;
use retro_core::debug::TraceEntry;
use retro_core::frame::run_frame_on;
use retro_core::types::{Breakpoint, BreakpointType, StackFrame};
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode, ExecutionTier};
use sg1_core::cartridge::composable::parse_manifest;
use sg1_core::debugger::{TraceEntryBinary, MAIN_CPU, SOUND_CPU};
use sg1_core::GenesisEmulator;

fn execute_at(address: u32) -> Breakpoint {
    Breakpoint {
        address,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(MAIN_CPU),
        condition: None,
    }
}

/// Run frames on the debugger's light tier from the running mode until it
/// pauses.
fn run_to_a_stop(emu: &mut GenesisEmulator) {
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    for _ in 0..10 {
        run_frame_on(emu, ExecutionTier::LightDebug);
        if emu.debugger_ref().get_execution_mode() == ExecutionMode::Paused {
            return;
        }
    }
    panic!("no breakpoint stopped the program");
}

/// The 68000's call stack as `(call address, return address)` pairs, the
/// outermost first.
fn calls(emu: &GenesisEmulator) -> Vec<(u32, u32)> {
    emu.debugger_ref()
        .get_call_stack(MAIN_CPU)
        .iter()
        .map(|frame: &StackFrame| (frame.call_address, frame.return_address))
        .collect()
}

/// The addresses of the newest `count` entries of a CPU's history, as the
/// debugger lists them.
fn history(emu: &GenesisEmulator, cpu: usize, count: u32) -> Vec<u32> {
    emu.debugger_ref()
        .get_instruction_history(cpu, count)
        .iter()
        .map(|entry| entry.address)
        .collect()
}

fn loaded(rom: &[u8]) -> GenesisEmulator {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(rom).expect("the program loads");
    emu
}

// ===== Calls and returns =====

/// main calls outer with `JSR abs.L` (6 bytes), outer calls leaf with
/// `BSR.W` (4 bytes), leaf returns, outer returns, main calls leaf with
/// `BSR.B` (2 bytes) and goes idle.
fn calls_program() -> Vec<u8> {
    let mut program = M68kProgram::new(0x1000, 0x200);
    #[rustfmt::skip]
    program.org(0x200).words(&[
        0x46FC, 0x2700,                 // $200: MOVE.W #$2700,SR
        0x4EB9, 0x0000, 0x0240,         // $204: JSR $240
        0x6124,                         // $20A: BSR.B $230
        0x60FE,                         // $20C: BRA.S $20C
    ]);
    #[rustfmt::skip]
    program.org(0x230).words(&[
        0x4E71,                         // $230: NOP
        0x4E75,                         // $232: RTS
    ]);
    #[rustfmt::skip]
    program.org(0x240).words(&[
        0x6100, 0xFFEE,                 // $240: BSR.W $230
        0x4E71,                         // $244: NOP
        0x4E75,                         // $246: RTS
    ]);
    program.build()
}

#[test]
fn a_call_returns_past_itself() {
    let stops = [0x230, 0x244, 0x230, 0x20C];
    let expected: [&[(u32, u32)]; 4] = [
        // In leaf, called by outer, called by main.
        &[(0x204, 0x20A), (0x240, 0x244)],
        // Back in outer: leaf's RTS took the BSR.W's frame off.
        &[(0x204, 0x20A)],
        // In leaf again, called by main's BSR.B.
        &[(0x20A, 0x20C)],
        // Idle: every call returned.
        &[],
    ];
    let mut emu = loaded(&calls_program());
    for &stop in &stops {
        emu.debugger().add_breakpoint(execute_at(stop));
    }
    for (stop, frames) in stops.iter().zip(expected) {
        run_to_a_stop(&mut emu);
        assert_eq!(emu.cpu().pc, *stop, "the light tier stops at ${stop:X}");
        assert_eq!(calls(&emu), frames, "at ${stop:X}");
    }
}

// ===== The Z80's own step =====

/// A Z80 program: `LD SP,$2000`, then `JR` to itself.
const Z80_PROGRAM: [u8; 5] = [0x31, 0x00, 0x20, 0x18, 0xFE];

#[test]
fn stepping_the_z80_leaves_its_state_in_the_debugger() {
    let mut emu = loaded(&calls_program());
    emu.set_z80_bus_request(true);
    emu.write_memory_debug(0xA00000, &Z80_PROGRAM);
    emu.set_z80_bus_request(false);
    emu.set_z80_reset(false);
    for expected_pc in [3, 3] {
        let step = emu.step_cpu(SOUND_CPU);
        assert!(step.instruction_executed);
        let states = emu.debugger_ref().get_cpu_states();
        assert_eq!(states[SOUND_CPU].pc, expected_pc);
        assert_eq!(states[SOUND_CPU].pc, u32::from(emu.z80().pc));
    }
    assert_eq!(history(&emu, SOUND_CPU, 10), [3, 0]);
}

// ===== The debugger's own reads =====

/// main counts in D0 round a loop whose second instruction, a NOP at
/// `$206`, takes the breakpoint.
fn counting_program() -> Vec<u8> {
    let mut program = M68kProgram::new(0x1000, 0x200);
    #[rustfmt::skip]
    program.org(0x200).words(&[
        0x46FC, 0x2700,                 // $200: MOVE.W #$2700,SR
        0x5240,                         // $204: ADDQ.W #1,D0
        0x4E71,                         // $206: NOP
        0x60FA,                         // $208: BRA.S $204
    ]);
    program.build()
}

/// What a debugger read could change: the state hash, the cartridge's own
/// saved state (its RAM and every coprocessor's state, which the hash
/// leaves out) and the reads the cartridge's chip has counted.
fn machine(emu: &GenesisEmulator) -> (u64, Vec<u8>, u64) {
    (
        emu.state_hash(),
        emu.get_cartridge_save_state(0)
            .expect("a cartridge is loaded"),
        emu.coprocessor_accesses(),
    )
}

/// The counting program on a cartridge of `preset`, stopped by the light
/// tier part of the way through its second frame, the first frame run in
/// full.
fn stopped_on(preset: &str) -> GenesisEmulator {
    let manifest = parse_manifest(&format!(
        r#"{{
            "core": "sg1",
            "version": "1.0",
            "rom": {{ "sha256": "x", "size": 4096, "title": "TEST PROGRAM" }},
            "mapping": {{ "preset": "{preset}", "sram_size": 0 }}
        }}"#
    ))
    .expect("the manifest parses");
    let mut emu = GenesisEmulator::new();
    emu.set_cartridge_manifest_override(Some(manifest));
    emu.load_rom(&counting_program())
        .expect("the program loads");
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    run_frame_on(&mut emu, ExecutionTier::Play);
    emu.debugger().add_breakpoint(execute_at(0x206));
    run_to_a_stop(&mut emu);
    emu
}

/// Read every byte of each window through the debugger: the memory view,
/// the disassembly, and a 68000 trace entry's text at each window's start.
fn read_through_the_debugger(emu: &mut GenesisEmulator, windows: &[(u32, u32)]) {
    for &(start, length) in windows {
        let bytes = emu.read_memory_debug(start, length);
        assert_eq!(bytes.len(), length as usize);
        emu.disassemble_at(start, length.div_ceil(2));
        let entry = TraceEntryBinary {
            pc: start,
            ..TraceEntryBinary::default()
        };
        emu.trace_to_text(MAIN_CPU, &entry.to_bytes())
            .expect("the 68000 has a trace text");
    }
}

#[test]
fn reading_a_chip_window_through_the_debugger_leaves_the_machine_as_it_was() {
    let cases: [(&str, &[(u32, u32)]); 6] = [
        // The DRAM and its two cell-arranged views.
        (
            "svp",
            &[(0x300000, 0x100), (0x390000, 0x100), (0x3A0000, 0x100)],
        ),
        // The ROM window through the mapper's banks.
        ("sega", &[(0x000000, 0x400)]),
        ("ssf2", &[(0x000000, 0x400)]),
        ("codemasters", &[(0x000000, 0x400)]),
        // The control byte.
        ("eeprom", &[(0x200000, 4)]),
        // The two extra controller ports.
        ("jcart", &[(0x38FFFC, 4), (0x3FFFFC, 4)]),
    ];
    for (preset, windows) in cases {
        let mut emu = stopped_on(preset);
        let before = machine(&emu);
        read_through_the_debugger(&mut emu, windows);
        assert!(
            machine(&emu) == before,
            "{preset}: a debugger read changed the machine"
        );
    }
}

#[test]
fn a_live_read_of_the_svp_dram_runs_the_dsp_and_a_debugger_read_does_not() {
    let mut emu = stopped_on("svp");
    let before = machine(&emu);
    let peeked = emu.read_memory_debug(0x300000, 2);
    assert!(machine(&emu) == before);

    // The 68000's own read catches the DSP up to the present first.
    let read = [emu.bus_mut().read(0x300000), emu.bus_mut().read(0x300001)];
    assert_eq!(peeked, read, "the peek reads the DRAM the 68000 reads");
    assert!(machine(&emu) != before, "the 68000's read ran the DSP");
}
