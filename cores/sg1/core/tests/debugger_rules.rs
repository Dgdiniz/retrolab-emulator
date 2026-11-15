//! The rules the debugger keeps on the Mega Drive, each shown on a small
//! program: what a call and a return do to the call stack on both debugger
//! frame loops, the order of the history, which trace asks to be flushed,
//! where running on from a stop goes, what a step a host asks for records,
//! what a reset or a load empties, and that the debugger's own reads leave
//! the machine as it was.

mod helpers;

use helpers::M68kProgram;
use retro_core::cpu::Addressable;
use retro_core::debug::TraceEntry;
use retro_core::types::{Breakpoint, BreakpointId, BreakpointType, StackFrame};
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode};
use sg1_core::cartridge::composable::parse_manifest;
use sg1_core::debugger::{TraceEntryBinary, TraceEntryBinaryZ80, MAIN_CPU, SOUND_CPU};
use sg1_core::GenesisEmulator;

/// A frame of one of the debugger's frame loops.
type FrameLoop = fn(&mut GenesisEmulator);

/// The light debugger loop, as a frame call.
fn light_frame(emu: &mut GenesisEmulator) {
    emu.run_frame_debug_light();
}

/// The full debugger loop, as a frame call.
fn full_frame(emu: &mut GenesisEmulator) {
    <GenesisEmulator as EmulatorCore>::run_frame(emu);
}

const BOTH_LOOPS: [(&str, FrameLoop); 2] = [("light", light_frame), ("full", full_frame)];

fn execute_at(address: u32) -> Breakpoint {
    Breakpoint {
        address,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(MAIN_CPU),
        condition: None,
    }
}

fn hits(emu: &GenesisEmulator, id: BreakpointId) -> u32 {
    emu.debugger_ref()
        .list_breakpoints()
        .iter()
        .find(|info| info.id == id)
        .map(|info| info.hit_count)
        .expect("the breakpoint is listed")
}

/// Run frames of `frame` from the running mode until the debugger pauses.
fn run_to_a_stop(emu: &mut GenesisEmulator, frame: FrameLoop) {
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    for _ in 0..10 {
        frame(emu);
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
fn a_call_returns_past_itself_and_both_loops_keep_the_same_stack() {
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
    for (name, frame) in BOTH_LOOPS {
        let mut emu = loaded(&calls_program());
        for &stop in &stops {
            emu.debugger().add_breakpoint(execute_at(stop));
        }
        for (stop, frames) in stops.iter().zip(expected) {
            run_to_a_stop(&mut emu, frame);
            assert_eq!(emu.cpu().pc, *stop, "the {name} loop stops at ${stop:X}");
            assert_eq!(calls(&emu), frames, "the {name} loop at ${stop:X}");
        }
    }
}

// ===== The history =====

#[test]
fn the_history_lists_the_newest_instruction_first() {
    let mut emu = loaded(&calls_program());
    for _ in 0..4 {
        emu.step_cpu(MAIN_CPU);
    }
    // MOVE at $200, JSR at $204, BSR.W at $240, NOP at $230.
    assert_eq!(history(&emu, MAIN_CPU, 10), [0x230, 0x240, 0x204, 0x200]);
    assert_eq!(history(&emu, MAIN_CPU, 2), [0x230, 0x240]);
}

// ===== A host's step =====

#[test]
fn a_step_over_a_call_and_its_return_records_each_and_keeps_the_stack() {
    let mut emu = loaded(&calls_program());
    let mut recorded = Vec::new();
    let mut stacks = Vec::new();
    for _ in 0..6 {
        let pc = emu.cpu().pc;
        let step = emu.step_cpu(MAIN_CPU);
        assert!(step.instruction_executed);
        recorded.push(pc);
        // One entry a step, the newest the instruction just stepped.
        assert_eq!(
            history(&emu, MAIN_CPU, 100),
            recorded.iter().rev().copied().collect::<Vec<_>>()
        );
        stacks.push(calls(&emu));
    }
    assert_eq!(recorded, [0x200, 0x204, 0x240, 0x230, 0x232, 0x244]);
    let expected: [&[(u32, u32)]; 6] = [
        &[],
        &[(0x204, 0x20A)],
        &[(0x204, 0x20A), (0x240, 0x244)],
        &[(0x204, 0x20A), (0x240, 0x244)],
        &[(0x204, 0x20A)],
        &[(0x204, 0x20A)],
    ];
    assert_eq!(stacks, expected);
}

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

// ===== Running on from a stop =====

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

#[test]
fn running_on_from_a_stop_runs_the_instruction_there_and_stops_at_the_next_hit() {
    for (name, frame) in BOTH_LOOPS {
        let mut emu = loaded(&counting_program());
        let id = emu.debugger().add_breakpoint(execute_at(0x206));

        run_to_a_stop(&mut emu, frame);
        assert_eq!(
            (emu.cpu().pc, emu.cpu().d[0], hits(&emu, id)),
            (0x206, 1, 1),
            "{name}: the first stop"
        );

        // Running on runs the NOP the stop left the 68000 at, goes round
        // the loop once and stops at the breakpoint's next hit.
        run_to_a_stop(&mut emu, frame);
        assert_eq!(
            (emu.cpu().pc, emu.cpu().d[0], hits(&emu, id)),
            (0x206, 2, 2),
            "{name}: the next stop"
        );
    }
}

#[test]
fn a_savestate_loaded_at_a_stop_is_checked_where_it_resumes() {
    for (name, frame) in BOTH_LOOPS {
        let mut emu = loaded(&counting_program());
        let id = emu.debugger().add_breakpoint(execute_at(0x206));
        run_to_a_stop(&mut emu, frame);
        let state = emu.save_state();
        run_to_a_stop(&mut emu, frame);
        assert_eq!(emu.cpu().d[0], 2, "{name}");

        // The savestate puts the 68000 back on the NOP with D0 at 1. The
        // breakpoint there is checked again: it stops the run before the
        // loop goes round, where the stop just left behind would have let
        // the NOP run unchecked and stopped only at D0 = 2.
        emu.load_state(&state).expect("the savestate loads");
        assert_eq!((emu.cpu().pc, emu.cpu().d[0]), (0x206, 1), "{name}");
        run_to_a_stop(&mut emu, frame);
        assert_eq!(emu.cpu().d[0], 1, "{name}: stopped in the same round");
        assert_eq!(hits(&emu, id), 3, "{name}");
    }
}

// ===== A reset or a load =====

#[test]
fn a_reset_or_a_load_empties_the_call_stack_and_keeps_the_history() {
    let rom = calls_program();
    let stopped_in_leaf = || {
        let mut emu = loaded(&rom);
        emu.debugger().add_breakpoint(execute_at(0x230));
        run_to_a_stop(&mut emu, full_frame);
        assert_eq!(calls(&emu), [(0x204, 0x20A), (0x240, 0x244)]);
        emu
    };
    let state = stopped_in_leaf().save_state();

    type Replace = fn(&mut GenesisEmulator, &[u8], &[u8]);
    let replacements: [(&str, Replace); 3] = [
        ("a reset", |emu, _, _| emu.reset()),
        ("a ROM load", |emu, rom, _| {
            emu.load_rom(rom).expect("the program loads again")
        }),
        ("a savestate load", |emu, _, state| {
            emu.load_state(state).expect("the savestate loads")
        }),
    ];
    for (what, replace) in replacements {
        let mut emu = stopped_in_leaf();
        let ran = history(&emu, MAIN_CPU, 3);
        assert_eq!(ran, [0x240, 0x204, 0x200]);
        replace(&mut emu, &rom, &state);
        assert!(calls(&emu).is_empty(), "{what} empties the call stack");
        assert_eq!(history(&emu, MAIN_CPU, 3), ran, "{what} keeps the history");
    }
}

// ===== The traces =====

#[test]
fn a_trace_ring_holds_nothing_until_its_tracing_is_first_enabled() {
    let mut emu = GenesisEmulator::new();
    for cpu in [MAIN_CPU, SOUND_CPU] {
        let stats = emu.debugger_ref().trace_stats(cpu).expect("a traced CPU");
        assert_eq!(stats.buffer_capacity, 0, "cpu {cpu}");
    }
    emu.debugger().set_tracing_enabled(MAIN_CPU, true);
    let stats = emu.debugger_ref().trace_stats(MAIN_CPU).unwrap();
    assert_eq!(stats.buffer_capacity, 8 * 1024 * 1024);
    assert_eq!(
        emu.debugger_ref()
            .trace_stats(SOUND_CPU)
            .unwrap()
            .buffer_capacity,
        0
    );
}

#[test]
fn each_trace_asks_to_be_flushed_for_itself_and_any_one_for_the_debugger() {
    let mut emu = GenesisEmulator::new();
    emu.debugger().set_tracing_enabled(MAIN_CPU, true);
    emu.debugger().set_tracing_enabled(SOUND_CPU, true);
    assert!(!emu.debugger_ref().any_trace_needs_flush());

    // Fill the Z80's ring, the 68000's holding one entry.
    emu.debugger()
        .record_trace(MAIN_CPU, &TraceEntryBinary::default());
    let entry = TraceEntryBinaryZ80::default();
    let mut recorded = 0;
    while !emu.debugger_ref().needs_trace_flush(SOUND_CPU) {
        emu.debugger().record_trace(SOUND_CPU, &entry);
        recorded += 1;
        assert!(recorded < 1_000_000, "the ring never asked to be flushed");
    }
    assert!(!emu.debugger_ref().needs_trace_flush(MAIN_CPU));
    assert!(emu.debugger_ref().any_trace_needs_flush());

    // Once the Z80's data is taken, nothing asks.
    let data = emu
        .debugger_ref()
        .get_trace_flush_data(SOUND_CPU)
        .expect("the Z80's entries");
    assert_eq!(data.len() % TraceEntryBinaryZ80::SIZE, 0);
    emu.debugger().clear_trace_flushed(SOUND_CPU);
    assert!(!emu.debugger_ref().any_trace_needs_flush());
}

// ===== The debugger's own reads =====

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
/// loop part of the way through its second frame, the first frame run in
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
    emu.run_frame_fast();
    emu.debugger().add_breakpoint(execute_at(0x206));
    run_to_a_stop(&mut emu, light_frame);
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
