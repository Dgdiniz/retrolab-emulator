//! What the debugger reports today, held as literals: the event catalogue a
//! host lists, each trace entry's bytes and text line, and the instruction
//! history and the call stack of programs that call and return, on the
//! debugger's light tier and under the steps a host asks for. A change to
//! any of them fails here, and the message shows the value found beside the
//! one recorded, so a change made on purpose shows exactly what moved.

mod helpers;

use helpers::M68kProgram;
use retro_core::debug::TraceEntry;
use retro_core::frame::run_frame_on;
use retro_core::types::{Breakpoint, BreakpointType, InstructionTrace, StackFrame};
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode, ExecutionTier};
use sg1_core::debugger::{
    TraceEntryBinary, TraceEntryBinaryZ80, MAIN_CPU, SOUND_CPU, TRACE_ENTRY_SIZE,
    Z80_TRACE_ENTRY_SIZE,
};
use sg1_core::GenesisEmulator;

/// Hold `found` to `expected`. When they differ, `found` is written out
/// as it is, between two marker lines, so it can be read or copied.
fn assert_literal(what: &str, found: &str, expected: &str) {
    if found != expected {
        eprintln!("---- {what}, found:\n{found}\n---- end of {what}");
    }
    assert!(
        found == expected,
        "{what} differs from the recorded literal"
    );
}

/// `bytes` as lowercase hexadecimal, two digits a byte.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

// ===== The event catalogue =====

/// The event classes a host lists for event breakpoints, as JSON: eight
/// codes, 1 to 8, in that order, each with the kind of event it is, the
/// same words on every console. Codes 1 to 3 and 7 take no parameter;
/// code 4 takes the line as the V counter counts it, code 5 the memory a
/// transfer wrote (three named choices), code 6 a colour and code 8 a tile.
const EVENT_CATALOGUE: &str = r#"[
  {
    "code": 1,
    "kind": "vertical_blank",
    "label": "VBlank",
    "description": "The beam entered vertical blank",
    "param_label": null,
    "param_options": []
  },
  {
    "code": 2,
    "kind": "vertical_blank_interrupt",
    "label": "VInt",
    "description": "A VBlank interrupt (68000 level 6) was delivered",
    "param_label": null,
    "param_options": []
  },
  {
    "code": 3,
    "kind": "raster_interrupt",
    "label": "HInt",
    "description": "An HBlank interrupt (68000 level 4) was delivered",
    "param_label": null,
    "param_options": []
  },
  {
    "code": 4,
    "kind": "line",
    "label": "Scanline",
    "description": "A rendered scanline matched the V counter",
    "param_label": "Scanline (V)",
    "param_options": []
  },
  {
    "code": 5,
    "kind": "transfer_done",
    "label": "Transfer complete",
    "description": "A DMA from 68000 memory, a fill or a copy has written all its data",
    "param_label": "Memory",
    "param_options": [
      {
        "value": 0,
        "label": "VRAM"
      },
      {
        "value": 1,
        "label": "CRAM"
      },
      {
        "value": 2,
        "label": "VSRAM"
      }
    ]
  },
  {
    "code": 6,
    "kind": "palette_write",
    "label": "Palette write",
    "description": "A CRAM colour was written, through the data port, by a DMA or by a fill",
    "param_label": "Colour (0-63)",
    "param_options": []
  },
  {
    "code": 7,
    "kind": "sprite_table_write",
    "label": "Sprite table transfer",
    "description": "A DMA from 68000 memory, a fill or a copy wrote into the sprite table",
    "param_label": null,
    "param_options": []
  },
  {
    "code": 8,
    "kind": "tile_write",
    "label": "Tile rewritten",
    "description": "A write changed the bytes of a tile in VRAM (writing the same bytes again does not count)",
    "param_label": "Tile (0-2047)",
    "param_options": []
  }
]"#;

#[test]
fn the_event_catalogue_a_host_lists() {
    let emu = GenesisEmulator::new();
    let catalogue = emu.debugger_ref().available_events();
    let json = serde_json::to_string_pretty(&catalogue).expect("the catalogue serialises");
    assert_literal("the event catalogue", &json, EVENT_CATALOGUE);
}

// ===== Trace entries =====

/// The three 68000 instructions the trace text below disassembles, at
/// `$000400`.
fn trace_text_program() -> Vec<u8> {
    let mut program = M68kProgram::new(0x1000, 0x400);
    #[rustfmt::skip]
    program.org(0x400).words(&[
        0x203C, 0x1234, 0x5678,         // $400: MOVE.L #$12345678,D0
        0x4E75,                         // $406: RTS
        0x4EB9, 0x0000, 0x0400,         // $408: JSR $00000400
    ]);
    program.build()
}

/// Three 68000 trace entries, built field by field: the first at the MOVE,
/// the second at the RTS, the third at the JSR with an opcode field that
/// names a NOP.
fn trace_entries_68000() -> [TraceEntryBinary; 3] {
    [
        TraceEntryBinary {
            pc: 0x0000_0400,
            sr: 0x2700,
            d: [
                0x0000_0000,
                0x1234_5678,
                0xFFFF_FFFF,
                0x8000_0001,
                0x0000_00FF,
                0x00FF_0000,
                0x7FFF_FFFF,
                0x0102_0304,
            ],
            a: [
                0x00FF_0000,
                0x00C0_0004,
                0x00C0_0000,
                0x00A0_0000,
                0x0000_0400,
                0x00FF_FFFE,
                0x0000_0000,
                0x00FF_FE00,
            ],
            opcode: 0x203C,
            cycles: 12,
            total_cycles: 0x0000_0001_0000_0022,
            scanline: 223,
        },
        TraceEntryBinary {
            pc: 0x0000_0406,
            sr: 0x2015,
            d: [1, 2, 3, 4, 5, 6, 7, 8],
            a: [9, 10, 11, 12, 13, 14, 15, 0x00FF_FDFC],
            opcode: 0x4E75,
            cycles: 16,
            total_cycles: 34,
            scanline: 0,
        },
        TraceEntryBinary {
            pc: 0x0000_0408,
            sr: 0x0000,
            d: [0; 8],
            a: [0; 8],
            opcode: 0x4E71,
            cycles: 20,
            total_cycles: u64::MAX,
            scanline: 0xFFFF,
        },
    ]
}

/// Each 68000 entry's bytes: 84 a entry, every field little-endian, in the
/// order PC, SR, D0-D7, A0-A7, opcode, cycles, total cycles, line.
const TRACE_BYTES_68000: [&str; 3] = [
    "0004000000270000000078563412ffffffff01000080ff0000000000ff00ffffff7f04030201\
     0000ff000400c0000000c0000000a00000040000feffff000000000000feff003c200c002200\
     000001000000df00",
    "0604000015200100000002000000030000000400000005000000060000000700000008000000\
     090000000a0000000b0000000c0000000d0000000e0000000f000000fcfdff00754e10002200\
     0000000000000000",
    "0804000000000000000000000000000000000000000000000000000000000000000000000000\
     0000000000000000000000000000000000000000000000000000000000000000714e1400ffff\
     ffffffffffffffff",
];

/// The chunk of the three 68000 entries and ten more bytes, as text (the
/// emulator's trace text, which a host asks for a chunk at a time): a line
/// per whole entry, each followed by a line break, with the PC, SR and the
/// sixteen registers, then the instruction; the cycles, the total cycles
/// and the line are not in it. The instruction comes from memory as it is
/// now: the third line shows the JSR memory holds although the entry's
/// opcode field names a NOP, and the RTS line ends in a space after the
/// mnemonic. The ten bytes past the third entry make no line.
const TRACE_TEXT_68000: &str = "\
    PC:000400 SR:2700 D0:00000000 D1:12345678 D2:FFFFFFFF D3:80000001 D4:000000FF \
    D5:00FF0000 D6:7FFFFFFF D7:01020304 A0:00FF0000 A1:00C00004 A2:00C00000 \
    A3:00A00000 A4:00000400 A5:00FFFFFE A6:00000000 A7:00FFFE00 | MOVE.L #$12345678, D0\n\
    PC:000406 SR:2015 D0:00000001 D1:00000002 D2:00000003 D3:00000004 D4:00000005 \
    D5:00000006 D6:00000007 D7:00000008 A0:00000009 A1:0000000A A2:0000000B \
    A3:0000000C A4:0000000D A5:0000000E A6:0000000F A7:00FFFDFC | RTS \n\
    PC:000408 SR:0000 D0:00000000 D1:00000000 D2:00000000 D3:00000000 D4:00000000 \
    D5:00000000 D6:00000000 D7:00000000 A0:00000000 A1:00000000 A2:00000000 \
    A3:00000000 A4:00000000 A5:00000000 A6:00000000 A7:00000000 | JSR $00000400\n";

#[test]
fn the_68000_trace_entry_in_bytes_and_text() {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(&trace_text_program())
        .expect("the program loads");
    let entries = trace_entries_68000();

    let mut chunk = Vec::new();
    for (entry, expected) in entries.iter().zip(TRACE_BYTES_68000) {
        let bytes = entry.to_bytes();
        assert_eq!(bytes.len(), TRACE_ENTRY_SIZE);
        assert_literal("a 68000 entry's bytes", &hex(&bytes), expected);
        let read_back = TraceEntryBinary::from_bytes(&bytes).expect("a whole entry reads back");
        assert_eq!(format!("{read_back:?}"), format!("{entry:?}"));
        chunk.extend_from_slice(&bytes);
    }
    assert!(TraceEntryBinary::from_bytes(&chunk[..TRACE_ENTRY_SIZE - 1]).is_none());
    chunk.extend_from_slice(&[0xAA; 10]);

    let text = emu
        .trace_to_text(MAIN_CPU, &chunk)
        .expect("the 68000 has a trace text");
    assert_literal("the 68000 trace text", &text, TRACE_TEXT_68000);
}

/// Three Z80 trace entries, built field by field, each with the bytes of
/// one instruction: `LD BC,$1FD9`, `CALL $0100` and the prefixed `LDIR`.
fn trace_entries_z80() -> [TraceEntryBinaryZ80; 3] {
    [
        TraceEntryBinaryZ80 {
            pc: 0x0000,
            af: 0x1234,
            bc: 0x5678,
            de: 0x9ABC,
            hl: 0xDEF0,
            ix: 0x0102,
            iy: 0x0304,
            sp: 0x1FFF,
            i_reg: 0x00,
            r_reg: 0x7F,
            im: 1,
            iff1: 1,
            iff2: 1,
            halt: 0,
            cycles: 10,
            total_cycles: 0x0000_0001_0000_0000,
            scanline: 261,
            raw_bytes: [0x01, 0xD9, 0x1F, 0x00],
        },
        TraceEntryBinaryZ80 {
            pc: 0x1234,
            af: 0xFFFF,
            bc: 0x0000,
            de: 0x0001,
            hl: 0x8000,
            ix: 0xFFFF,
            iy: 0x0000,
            sp: 0x0000,
            i_reg: 0xFF,
            r_reg: 0x80,
            im: 2,
            iff1: 0,
            iff2: 1,
            halt: 1,
            cycles: 17,
            total_cycles: 0,
            scanline: 0,
            raw_bytes: [0xCD, 0x00, 0x01, 0x00],
        },
        TraceEntryBinaryZ80 {
            pc: 0x0038,
            af: 0x0044,
            bc: 0x0010,
            de: 0x1000,
            hl: 0x0100,
            ix: 0x0000,
            iy: 0x0000,
            sp: 0x1FF0,
            i_reg: 0x00,
            r_reg: 0x00,
            im: 0,
            iff1: 0,
            iff2: 0,
            halt: 0,
            cycles: 21,
            total_cycles: 99,
            scanline: 0x00E0,
            raw_bytes: [0xED, 0xB0, 0x00, 0x00],
        },
    ]
}

/// Each Z80 entry's bytes: 38 a entry, the 16-bit fields little-endian, in
/// the order PC, AF, BC, DE, HL, IX, IY, SP, then I, R, IM, IFF1, IFF2 and
/// HALT a byte each, cycles, total cycles, line and the instruction's
/// four bytes.
const TRACE_BYTES_Z80: [&str; 3] = [
    "000034127856bc9af0de02010403ff1f007f010101000a000000000001000000050101d91f00",
    "3412ffff000001000080ffff00000000ff8002000101110000000000000000000000cd000100",
    "3800440010000010000100000000f01f00000000000015006300000000000000e000edb00000",
];

/// The chunk of the three Z80 entries and ten more bytes, as text: a line
/// per whole entry, each followed by a line break, with the registers and
/// flags, then the instruction disassembled from the entry's own bytes,
/// lowercase; the cycles, the total cycles and the line are not in it. The
/// ten bytes past the third entry make no line.
const TRACE_TEXT_Z80: &str = "\
    PC:0000 AF:1234 BC:5678 DE:9ABC HL:DEF0 IX:0102 IY:0304 SP:1FFF I:00 R:7F IM:1 IFF1:1 \
    IFF2:1 HALT:0 | ld bc,$1FD9\n\
    PC:1234 AF:FFFF BC:0000 DE:0001 HL:8000 IX:FFFF IY:0000 SP:0000 I:FF R:80 IM:2 IFF1:0 \
    IFF2:1 HALT:1 | call $0100\n\
    PC:0038 AF:0044 BC:0010 DE:1000 HL:0100 IX:0000 IY:0000 SP:1FF0 I:00 R:00 IM:0 IFF1:0 \
    IFF2:0 HALT:0 | ldir\n";

#[test]
fn the_z80_trace_entry_in_bytes_and_text() {
    let entries = trace_entries_z80();
    let mut chunk = Vec::new();
    for (entry, expected) in entries.iter().zip(TRACE_BYTES_Z80) {
        let bytes = entry.to_bytes();
        assert_eq!(bytes.len(), Z80_TRACE_ENTRY_SIZE);
        assert_literal("a Z80 entry's bytes", &hex(&bytes), expected);
        let read_back = TraceEntryBinaryZ80::from_bytes(&bytes).expect("a whole entry reads back");
        assert_eq!(format!("{read_back:?}"), format!("{entry:?}"));
        chunk.extend_from_slice(&bytes);
    }
    assert!(TraceEntryBinaryZ80::from_bytes(&chunk[..Z80_TRACE_ENTRY_SIZE - 1]).is_none());
    chunk.extend_from_slice(&[0xAA; 10]);

    let text = GenesisEmulator::new()
        .trace_to_text(SOUND_CPU, &chunk)
        .expect("the Z80 has a trace text");
    assert_literal("the Z80 trace text", &text, TRACE_TEXT_Z80);
}

// ===== History and call stack =====

/// The call stack in the order the debugger returns it, the outermost
/// frame first: each frame's call address, the return address recorded for
/// it and its name.
fn describe_stack(frames: &[StackFrame]) -> String {
    let frames: Vec<String> = frames
        .iter()
        .map(|frame| {
            format!(
                "{{call ${:06X}, return ${:06X}, name {:?}}}",
                frame.call_address, frame.return_address, frame.function_name
            )
        })
        .collect();
    format!("[{}]", frames.join(", "))
}

/// The history in the order the debugger returns it, an entry a line,
/// every field of every entry: its address, its text, its cycles and the
/// registers it carries.
fn describe_history(entries: &[InstructionTrace]) -> String {
    if entries.is_empty() {
        return " none\n".to_string();
    }
    let mut text = String::from("\n");
    for entry in entries {
        let registers: Vec<String> = entry
            .registers_before
            .iter()
            .map(|register| format!("{}=${:X}", register.name, register.value))
            .collect();
        let registers = if registers.is_empty() {
            "none".to_string()
        } else {
            registers.join(" ")
        };
        text.push_str(&format!(
            "    ${:06X} {:?} {} cycles, registers {}\n",
            entry.address, entry.instruction, entry.cycle_count, registers
        ));
    }
    text
}

/// Run `frame` until the debugger pauses, from the running mode.
fn run_to_a_stop(emu: &mut GenesisEmulator, frame: fn(&mut GenesisEmulator)) {
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    for _ in 0..10 {
        frame(emu);
        if emu.debugger_ref().get_execution_mode() == ExecutionMode::Paused {
            return;
        }
    }
    panic!("no breakpoint stopped the program");
}

/// Load `rom`, put an execute breakpoint on the 68000 at each of `stops`,
/// and run it on `frame` from one stop to the next, `stops.len()` times.
/// Each stop is written as its PC, the 68000's call stack and the newest
/// six entries of its instruction history, as the debugger returns them.
fn stops_of(rom: &[u8], stops: &[u32], frame: fn(&mut GenesisEmulator)) -> String {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(rom).expect("the program loads");
    for &address in stops {
        emu.debugger().add_breakpoint(Breakpoint {
            address,
            bp_type: BreakpointType::execute(),
            cpu_id: Some(0),
            condition: None,
        });
    }
    let mut text = String::new();
    for _ in stops {
        run_to_a_stop(&mut emu, frame);
        let pc = emu.cpu().pc;
        let stack = emu.debugger_ref().get_call_stack(0);
        let history = emu.debugger_ref().get_instruction_history(0, 6);
        text.push_str(&format!(
            "stopped at ${pc:06X}\n  call stack {}\n  history:{}",
            describe_stack(&stack),
            describe_history(&history)
        ));
    }
    text
}

/// The light debugger tier, as a frame call.
fn light_frame(emu: &mut GenesisEmulator) {
    run_frame_on(emu, ExecutionTier::LightDebug);
}

/// Host steps of the 68000 until its program counter is `address`, at
/// least one.
fn step_to(emu: &mut GenesisEmulator, address: u32) {
    for _ in 0..1_000_000 {
        emu.step_cpu(MAIN_CPU);
        if emu.cpu().pc == address {
            return;
        }
    }
    panic!("the 68000 never reached ${address:06X}");
}

/// Load `rom` and step it with the host's steps to each of `stops` in
/// turn, written as [`stops_of`] writes its stops.
fn steps_to(rom: &[u8], stops: &[u32]) -> String {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(rom).expect("the program loads");
    let mut text = String::new();
    for &address in stops {
        step_to(&mut emu, address);
        let stack = emu.debugger_ref().get_call_stack(0);
        let history = emu.debugger_ref().get_instruction_history(0, 6);
        text.push_str(&format!(
            "stopped at ${address:06X}\n  call stack {}\n  history:{}",
            describe_stack(&stack),
            describe_history(&history)
        ));
    }
    text
}

/// Where the calls program puts its code.
const LEAF: u32 = 0x230;
const OUTER: u32 = 0x240;

/// A program that calls and returns: main calls outer with `JSR abs.L`,
/// outer calls leaf with `BSR.W`, leaf returns, outer returns, main calls
/// leaf again with `BSR.B` and goes idle.
fn calls_program() -> Vec<u8> {
    let mut program = M68kProgram::new(0x1000, 0x200);
    #[rustfmt::skip]
    program.org(0x200).words(&[
        0x46FC, 0x2700,                 // $200: MOVE.W #$2700,SR
        0x4EB9, 0x0000, OUTER as u16,   // $204: JSR outer
        0x6100 | (LEAF as u16 - 0x20C), // $20A: BSR.B leaf
    ]).label("idle").words(&[
        0x60FE,                         // $20C: BRA.S idle
    ]);
    #[rustfmt::skip]
    program.org(LEAF as usize).label("leaf").words(&[
        0x4E71,                         // $230: NOP
        0x4E75,                         // $232: RTS
    ]);
    program.org(OUTER as usize).branch(0x6100, "leaf"); // $240: BSR.W leaf
    #[rustfmt::skip]
    program.label("back_in_outer").words(&[
        0x4E71,                         // $244: NOP
        0x4E75,                         // $246: RTS
    ]);
    let rom = program.build();
    assert_eq!(program.address("idle"), 0x20C);
    assert_eq!(program.address("back_in_outer"), 0x244);
    rom
}

/// Where the calls program is stopped: in leaf called from outer, back in
/// outer, in leaf called from main, idle.
const CALLS_STOPS: [u32; 4] = [LEAF, 0x244, LEAF, 0x20C];

/// The calls program on the light tier. Each call, the JSR and both BSRs,
/// pushes a frame whose return address is the address after the call, and
/// each RTS takes its own call's frame off. The loop keeps no instruction
/// history.
const CALLS_ON_THE_LIGHT_LOOP: &str = r#"stopped at $000230
  call stack [{call $000204, return $00020A, name None}, {call $000240, return $000244, name None}]
  history: none
stopped at $000244
  call stack [{call $000204, return $00020A, name None}]
  history: none
stopped at $000230
  call stack [{call $00020A, return $00020C, name None}]
  history: none
stopped at $00020C
  call stack []
  history: none
"#;

/// The calls program stepped by the host to the same places: the frames of
/// the light tier, and the history, which lists the instructions run, the
/// newest first.
const CALLS_UNDER_HOST_STEPS: &str = r#"stopped at $000230
  call stack [{call $000204, return $00020A, name None}, {call $000240, return $000244, name None}]
  history:
    $000240 "BSR     $00000230" 18 cycles, registers PC=$240 SR=$2700
    $000204 "JSR     $00000240" 20 cycles, registers PC=$204 SR=$2700
    $000200 "MOVE    #$2700, SR" 16 cycles, registers PC=$200 SR=$2700
stopped at $000244
  call stack [{call $000204, return $00020A, name None}]
  history:
    $000232 "RTS" 16 cycles, registers PC=$232 SR=$2700
    $000230 "NOP" 4 cycles, registers PC=$230 SR=$2700
    $000240 "BSR     $00000230" 18 cycles, registers PC=$240 SR=$2700
    $000204 "JSR     $00000240" 20 cycles, registers PC=$204 SR=$2700
    $000200 "MOVE    #$2700, SR" 16 cycles, registers PC=$200 SR=$2700
stopped at $000230
  call stack [{call $00020A, return $00020C, name None}]
  history:
    $00020A "BSR     $00000230" 18 cycles, registers PC=$20A SR=$2700
    $000246 "RTS" 16 cycles, registers PC=$246 SR=$2700
    $000244 "NOP" 4 cycles, registers PC=$244 SR=$2700
    $000232 "RTS" 16 cycles, registers PC=$232 SR=$2700
    $000230 "NOP" 4 cycles, registers PC=$230 SR=$2700
    $000240 "BSR     $00000230" 18 cycles, registers PC=$240 SR=$2700
stopped at $00020C
  call stack []
  history:
    $000232 "RTS" 16 cycles, registers PC=$232 SR=$2700
    $000230 "NOP" 4 cycles, registers PC=$230 SR=$2700
    $00020A "BSR     $00000230" 18 cycles, registers PC=$20A SR=$2700
    $000246 "RTS" 16 cycles, registers PC=$246 SR=$2700
    $000244 "NOP" 4 cycles, registers PC=$244 SR=$2700
    $000232 "RTS" 16 cycles, registers PC=$232 SR=$2700
"#;

#[test]
fn the_history_and_call_stack_through_calls_on_the_light_loop() {
    let found = stops_of(&calls_program(), &CALLS_STOPS, light_frame);
    assert_literal(
        "the calls on the light tier",
        &found,
        CALLS_ON_THE_LIGHT_LOOP,
    );
}

#[test]
fn the_history_and_call_stack_through_calls_under_host_steps() {
    let found = steps_to(&calls_program(), &CALLS_STOPS);
    assert_literal("the calls under host steps", &found, CALLS_UNDER_HOST_STEPS);
}

/// Where the interrupt program puts its code.
const WAIT: u32 = 0x240;
const HANDLER: u32 = 0x280;

/// A program that waits in a subroutine for the vertical interrupt: main
/// turns the interrupt on in the video chip and calls wait with
/// `JSR abs.L`; wait unmasks interrupts and polls a counter the handler
/// counts up, then returns; main goes idle.
fn interrupt_program() -> Vec<u8> {
    let mut program = M68kProgram::new(0x1000, 0x200);
    program.vector(30, HANDLER);
    #[rustfmt::skip]
    program.org(0x200).words(&[
        0x46FC, 0x2700,                 // $200: MOVE.W #$2700,SR
        0x41F9, 0x00C0, 0x0004,         // $204: LEA $C00004,A0
        0x30BC, 0x8164,                 // $20A: register 1: display, vertical interrupt, mode 5
        0x4EB9, 0x0000, WAIT as u16,    // $20E: JSR wait
    ]).label("idle").words(&[
        0x60FE,                         // $214: BRA.S idle
    ]);
    #[rustfmt::skip]
    program.org(WAIT as usize).words(&[
        0x46FC, 0x2000,                 // $240: MOVE.W #$2000,SR
    ]).label("poll").words(&[
        0x4A79, 0x00FF, 0x0000,         // $244: TST.W ($FF0000).L
    ]).branch(0x6700, "poll")           // $24A: BEQ.W poll
    .label("wait_returns").words(&[
        0x4E75,                         // $24E: RTS
    ]);
    #[rustfmt::skip]
    program.org(HANDLER as usize).words(&[
        0x5279, 0x00FF, 0x0000,         // $280: ADDQ.W #1,($FF0000).L
        0x4E73,                         // $286: RTE
    ]);
    let rom = program.build();
    assert_eq!(program.address("idle"), 0x214);
    assert_eq!(program.address("wait_returns"), 0x24E);
    rom
}

/// Where the interrupt program is stopped: in front of the handler's first
/// instruction, at wait's RTS, idle.
const INTERRUPT_STOPS: [u32; 3] = [HANDLER, 0x24E, 0x214];

/// Where host steps stop in it: after the handler's first instruction (the
/// step that takes the interrupt runs it), at wait's RTS, idle.
const INTERRUPT_STEPS: [u32; 3] = [HANDLER + 6, 0x24E, 0x214];

/// The interrupt program on the light tier: taking the interrupt pushes a
/// frame whose call and return address are both the address the interrupt
/// came at (the BEQ the handler returns to), the handler's RTE takes that
/// frame off, and wait's RTS takes main's call of wait off. The stop at the
/// handler comes in front of its first instruction: the step took the
/// interrupt, and the breakpoint stopped it before the instruction.
const INTERRUPT_ON_THE_LIGHT_LOOP: &str = r#"stopped at $000280
  call stack [{call $00020E, return $000214, name None}, {call $00024A, return $00024A, name None}]
  history: none
stopped at $00024E
  call stack [{call $00020E, return $000214, name None}]
  history: none
stopped at $000214
  call stack []
  history: none
"#;

/// The interrupt program stepped by the host: the frames of the light tier,
/// and the history. The history has no entry for taking the interrupt: the
/// handler's first instruction is listed with the status register the
/// interrupt set.
const INTERRUPT_UNDER_HOST_STEPS: &str = r#"stopped at $000286
  call stack [{call $00020E, return $000214, name None}, {call $00024A, return $00024A, name None}]
  history:
    $000280 "ADDQ.W #1, $00FF0000" 20 cycles, registers PC=$280 SR=$2604
    $000244 "TST.W  $00FF0000" 16 cycles, registers PC=$244 SR=$2004
    $00024A "BEQ     $00000244" 10 cycles, registers PC=$24A SR=$2004
    $000244 "TST.W  $00FF0000" 16 cycles, registers PC=$244 SR=$2004
    $00024A "BEQ     $00000244" 10 cycles, registers PC=$24A SR=$2004
    $000244 "TST.W  $00FF0000" 16 cycles, registers PC=$244 SR=$2004
stopped at $00024E
  call stack [{call $00020E, return $000214, name None}]
  history:
    $00024A "BEQ     $00000244" 12 cycles, registers PC=$24A SR=$2000
    $000244 "TST.W  $00FF0000" 16 cycles, registers PC=$244 SR=$2004
    $00024A "BEQ     $00000244" 10 cycles, registers PC=$24A SR=$2004
    $000286 "RTE" 20 cycles, registers PC=$286 SR=$2600
    $000280 "ADDQ.W #1, $00FF0000" 20 cycles, registers PC=$280 SR=$2604
    $000244 "TST.W  $00FF0000" 16 cycles, registers PC=$244 SR=$2004
stopped at $000214
  call stack []
  history:
    $00024E "RTS" 16 cycles, registers PC=$24E SR=$2000
    $00024A "BEQ     $00000244" 12 cycles, registers PC=$24A SR=$2000
    $000244 "TST.W  $00FF0000" 16 cycles, registers PC=$244 SR=$2004
    $00024A "BEQ     $00000244" 10 cycles, registers PC=$24A SR=$2004
    $000286 "RTE" 20 cycles, registers PC=$286 SR=$2600
    $000280 "ADDQ.W #1, $00FF0000" 20 cycles, registers PC=$280 SR=$2604
"#;

#[test]
fn the_call_stack_through_an_interrupt_on_the_light_loop() {
    let found = stops_of(&interrupt_program(), &INTERRUPT_STOPS, light_frame);
    assert_literal(
        "the interrupt on the light tier",
        &found,
        INTERRUPT_ON_THE_LIGHT_LOOP,
    );
}

#[test]
fn the_call_stack_through_an_interrupt_under_host_steps() {
    let found = steps_to(&interrupt_program(), &INTERRUPT_STEPS);
    assert_literal(
        "the interrupt under host steps",
        &found,
        INTERRUPT_UNDER_HOST_STEPS,
    );
}

/// The calls program stepped by the host into leaf, then a reset, the same
/// ROM loaded again, a savestate loaded (one a twin took at its first
/// frame's end: a frame in progress has none) and another ROM loaded. Each
/// of them empties the call stack, whose frames belonged to the machine it
/// replaced; the history stays, since it records what ran.
const ACROSS_A_RESET_AND_A_LOAD: &str = r#"stopped in leaf
  call stack [{call $000204, return $00020A, name None}, {call $000240, return $000244, name None}]
  history:
    $000240 "BSR     $00000230" 18 cycles, registers PC=$240 SR=$2700
    $000204 "JSR     $00000240" 20 cycles, registers PC=$204 SR=$2700
after a reset
  call stack []
  history:
    $000240 "BSR     $00000230" 18 cycles, registers PC=$240 SR=$2700
    $000204 "JSR     $00000240" 20 cycles, registers PC=$204 SR=$2700
after the same ROM loaded again
  call stack []
  history:
    $000240 "BSR     $00000230" 18 cycles, registers PC=$240 SR=$2700
    $000204 "JSR     $00000240" 20 cycles, registers PC=$204 SR=$2700
after the savestate loaded
  call stack []
  history:
    $000240 "BSR     $00000230" 18 cycles, registers PC=$240 SR=$2700
    $000204 "JSR     $00000240" 20 cycles, registers PC=$204 SR=$2700
after another ROM loaded
  call stack []
  history:
    $000240 "BSR     $00000230" 18 cycles, registers PC=$240 SR=$2700
    $000204 "JSR     $00000240" 20 cycles, registers PC=$204 SR=$2700
"#;

#[test]
fn the_call_stack_and_history_across_a_reset_and_a_load() {
    let rom = calls_program();
    let mut emu = GenesisEmulator::new();
    emu.load_rom(&rom).expect("the program loads");
    step_to(&mut emu, LEAF);
    assert!(
        emu.save_state().is_empty(),
        "a frame in progress has no savestate"
    );
    let mut twin = GenesisEmulator::new();
    twin.load_rom(&rom).expect("the program loads");
    run_frame_on(&mut twin, ExecutionTier::Play);
    let state = twin.save_state();
    assert!(!state.is_empty(), "a frame's end has one");

    let describe = |what: &str, emu: &GenesisEmulator| {
        format!(
            "{what}\n  call stack {}\n  history:{}",
            describe_stack(&emu.debugger_ref().get_call_stack(0)),
            describe_history(&emu.debugger_ref().get_instruction_history(0, 2))
        )
    };
    let mut text = describe("stopped in leaf", &emu);
    emu.reset();
    text.push_str(&describe("after a reset", &emu));
    emu.load_rom(&rom).expect("the program loads again");
    text.push_str(&describe("after the same ROM loaded again", &emu));
    emu.load_state(&state).expect("the savestate loads");
    text.push_str(&describe("after the savestate loaded", &emu));
    emu.load_rom(&interrupt_program())
        .expect("another program loads");
    text.push_str(&describe("after another ROM loaded", &emu));
    assert_literal(
        "across a reset and a load",
        &text,
        ACROSS_A_RESET_AND_A_LOAD,
    );
}

/// A Z80 program that calls and returns: `LD SP,$2000`, `CALL $0008`, then
/// `JR` to itself; at `$0008` a `NOP` and a `RET`.
const Z80_CALLS: [u8; 10] = [
    0x31, 0x00, 0x20, // $0000: LD SP,$2000
    0xCD, 0x08, 0x00, // $0003: CALL $0008
    0x18, 0xFE, // $0006: JR $0006
    0x00, // $0008: NOP
    0xC9, // $0009: RET
];

/// The Z80 program stepped seven instructions with the Z80's own step:
/// what each step returned, then the Z80's history, the newest first, and
/// its call stack, which nothing fills.
const Z80_STEPPED: &str = r#"step: 10 cycles, breakpoint false, executed true
step: 17 cycles, breakpoint false, executed true
step: 4 cycles, breakpoint false, executed true
step: 10 cycles, breakpoint false, executed true
step: 12 cycles, breakpoint false, executed true
step: 12 cycles, breakpoint false, executed true
step: 12 cycles, breakpoint false, executed true
history:
    $000006 "JR $0006" 12 cycles, registers none
    $000006 "JR $0006" 12 cycles, registers none
    $000006 "JR $0006" 12 cycles, registers none
    $000009 "RET" 10 cycles, registers none
    $000008 "NOP" 4 cycles, registers none
    $000003 "CALL $0008" 17 cycles, registers none
    $000000 "LD SP, $2000" 10 cycles, registers none
call stack []
"#;

#[test]
fn the_z80_history_and_call_stack_when_stepped() {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(&calls_program()).expect("the program loads");
    emu.set_z80_bus_request(true);
    emu.write_memory_debug(0xA00000, &Z80_CALLS);
    emu.set_z80_bus_request(false);
    emu.set_z80_reset(false);

    let mut text = String::new();
    for _ in 0..7 {
        let step = emu.step_cpu(1);
        text.push_str(&format!(
            "step: {} cycles, breakpoint {}, executed {}\n",
            step.cycles, step.breakpoint_hit, step.instruction_executed
        ));
    }
    let history = emu.debugger_ref().get_instruction_history(1, 10);
    let stack = emu.debugger_ref().get_call_stack(1);
    text.push_str(&format!(
        "history:{}call stack {}\n",
        describe_history(&history),
        describe_stack(&stack)
    ));
    assert_literal("the Z80 stepped", &text, Z80_STEPPED);
}
