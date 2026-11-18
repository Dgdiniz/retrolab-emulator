use super::*;

/// Where the program's two subroutines start.
const COUNT: u32 = 0x500;

/// A program for the debugger's readers. It puts `JR $0000` at the
/// end of 64 bytes of the Z80's RAM, which holds zeros, so the Z80
/// runs 64 NOPs and jumps back, again and again; then the 68000
/// calls count, which counts a word of work RAM up, with `JSR
/// abs.L`, and colour, which stores that word as colour 1, with
/// `BSR.W`, again and again, the display off and no interrupt on.
fn debugger_shapes_rom() -> Vec<u8> {
    let mut rom = synth_genesis_rom("DEBUGGER SHAPES");
    #[rustfmt::skip]
            place(&mut rom, 0x400, &[
                0x46FC, 0x2700,                 // $400: MOVE.W #$2700,SR
                0x33FC, 0x0100, 0x00A1, 0x1100, // $404: the 68000 takes the Z80's bus
                0x13FC, 0x0018, 0x00A0, 0x0040, // $40C: MOVE.B #$18,($A00040).L
                0x13FC, 0x00BE, 0x00A0, 0x0041, // $414: MOVE.B #$BE,($A00041).L: JR $0000
                0x33FC, 0x0100, 0x00A1, 0x1200, // $41C: the Z80 out of reset
                0x33FC, 0x0000, 0x00A1, 0x1100, // $424: and its bus given back
                0x41F9, 0x00C0, 0x0004,         // $42C: LEA $C00004,A0: control port
                0x43F9, 0x00C0, 0x0000,         // $432: LEA $C00000,A1: data port
                0x30BC, 0x8104,                 // $438: register 1: mode 5, display off
                0x4EB9, 0x0000, COUNT as u16,   // $43C: JSR count
                0x6100, 0x00DC,                 // $442: BSR.W colour
                0x60F4,                         // $446: BRA.S $43C
            ]);
    #[rustfmt::skip]
            place(&mut rom, COUNT as usize, &[
                0x5279, 0x00FF, 0x0000,         // $500: ADDQ.W #1,($FF0000).L
                0x4E75,                         // $506: RTS
            ]);
    #[rustfmt::skip]
            place(&mut rom, 0x520, &[
                0x20BC, 0xC002, 0x0000,         // $520: CRAM colour 1
                0x32B9, 0x00FF, 0x0000,         // $526: MOVE.W ($FF0000).L,(A1)
                0x4E75,                         // $52C: RTS
            ]);
    rom
}

/// The program with the Z80 traced from the first frame, one frame
/// run, then an execute breakpoint on count and the 68000 traced,
/// and a frame call that stops at the breakpoint. Then a write watch
/// on the counter and an event breakpoint on colour 1 are added.
fn stopped_at_count() -> RetroEmulator {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    emu.load_rom(&debugger_shapes_rom()).unwrap();
    emu.set_z80_tracing_enabled(true);
    emu.set_execution_mode("Running".to_string());
    emu.run_frame();
    emu.add_breakpoint(COUNT, "execute".to_string(), None);
    emu.set_tracing_enabled(true);
    emu.run_frame();
    emu.add_breakpoint(0xFF0000, "write".to_string(), None);
    emu.add_event_breakpoint(6, 1);
    emu
}

/// [`stopped_at_count`] stepped six 68000 instructions with `step`
/// (count's ADDQ and RTS, the BSR, colour's two stores, its RTS)
/// and two Z80 instructions with `stepCpu(1)`, and what each step
/// returned.
fn stepped() -> (RetroEmulator, Vec<JsValue>) {
    let mut emu = stopped_at_count();
    let mut returned = Vec::new();
    for _ in 0..6 {
        returned.push(emu.step());
    }
    for _ in 0..2 {
        returned.push(emu.step_cpu(1));
    }
    (emu, returned)
}

/// The program with only the Z80 traced, run until the Z80's trace
/// ring asks to be flushed.
fn with_the_z80_ring_full() -> RetroEmulator {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    emu.load_rom(&debugger_shapes_rom()).unwrap();
    emu.set_z80_tracing_enabled(true);
    emu.set_execution_mode("Running".to_string());
    for _ in 0..60 {
        emu.run_frame();
        if emu.needs_z80_trace_flush() {
            return emu;
        }
    }
    panic!("the Z80's trace ring never asked to be flushed");
}

/// `bytes` as lowercase hexadecimal, two digits a byte.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The bytes `text` gives in hexadecimal.
fn from_hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}

/// A trace chunk's length and its first entry's bytes.
fn chunk_head(data: &[u8], entry_size: usize) -> Vec<(&'static str, JsValue)> {
    vec![
        ("length", JsValue::from(data.len() as u32)),
        (
            "first entry",
            JsValue::from_str(&hex(&data[..entry_size.min(data.len())])),
        ),
    ]
}

/// A text as its lines, each a value, and whether it ends in a line
/// break.
fn lines_of(text: &str) -> Vec<(String, JsValue)> {
    let mut found: Vec<(String, JsValue)> = text
        .lines()
        .enumerate()
        .map(|(at, line)| (format!("line {}", at + 1), JsValue::from_str(line)))
        .collect();
    found.push((
        "ends with a line break".to_string(),
        JsValue::from_bool(text.ends_with('\n')),
    ));
    found
}

/// The Z80's state after the steps is the one its own last step wrote:
/// each step a host asks for writes the states, the two Z80 steps, which
/// ran the NOPs at $27 and $28, as well as the 68000's.
#[wasm_bindgen_test]
fn what_the_cpu_states_hold() {
    let mut stopped = stopped_at_count();
    let at_the_stop = stopped.get_cpu_states();
    let (mut emu, _) = stepped();
    assert_shapes(
        &[
            ("at the stop", at_the_stop),
            ("after the steps", emu.get_cpu_states()),
        ],
        &[
            (
                "at the stop",
                "[{cpu_id: 0, flags: 9984, pc: 1280, registers: [{name: \"D0\", \
                         value: 0}, {name: \"D1\", value: 0}, {name: \"D2\", value: 0}, \
                         {name: \"D3\", value: 0}, {name: \"D4\", value: 0}, {name: \"D5\", \
                         value: 0}, {name: \"D6\", value: 0}, {name: \"D7\", value: 0}, \
                         {name: \"A0\", value: 12582916}, {name: \"A1\", value: 12582912}, \
                         {name: \"A2\", value: 0}, {name: \"A3\", value: 0}, {name: \"A4\", \
                         value: 0}, {name: \"A5\", value: 0}, {name: \"A6\", value: 0}, \
                         {name: \"A7\", value: 4294967292}, {name: \"PC\", value: 1280}, \
                         {name: \"SR\", value: 9984}]}, {cpu_id: 1, flags: 0, pc: 26, \
                         registers: [{name: \"A\", value: 0}, {name: \"F\", value: 0}, {name: \
                         \"B\", value: 0}, {name: \"C\", value: 0}, {name: \"D\", value: 0}, \
                         {name: \"E\", value: 0}, {name: \"H\", value: 0}, {name: \"L\", \
                         value: 0}, {name: \"PC\", value: 26}, {name: \"SP\", value: 65535}, \
                         {name: \"IX\", value: 0}, {name: \"IY\", value: 0}]}]",
            ),
            (
                "after the steps",
                "[{cpu_id: 0, flags: 9984, pc: 1094, registers: [{name: \"D0\", \
                         value: 0}, {name: \"D1\", value: 0}, {name: \"D2\", value: 0}, \
                         {name: \"D3\", value: 0}, {name: \"D4\", value: 0}, {name: \"D5\", \
                         value: 0}, {name: \"D6\", value: 0}, {name: \"D7\", value: 0}, \
                         {name: \"A0\", value: 12582916}, {name: \"A1\", value: 12582912}, \
                         {name: \"A2\", value: 0}, {name: \"A3\", value: 0}, {name: \"A4\", \
                         value: 0}, {name: \"A5\", value: 0}, {name: \"A6\", value: 0}, \
                         {name: \"A7\", value: 0}, {name: \"PC\", value: 1094}, {name: \
                         \"SR\", value: 9984}]}, {cpu_id: 1, flags: 0, pc: 41, registers: \
                         [{name: \"A\", value: 0}, {name: \"F\", value: 0}, {name: \"B\", \
                         value: 0}, {name: \"C\", value: 0}, {name: \"D\", value: 0}, {name: \
                         \"E\", value: 0}, {name: \"H\", value: 0}, {name: \"L\", value: 0}, \
                         {name: \"PC\", value: 41}, {name: \"SP\", value: 65535}, {name: \
                         \"IX\", value: 0}, {name: \"IY\", value: 0}]}]",
            ),
        ],
    );
}

#[wasm_bindgen_test]
fn what_the_breakpoint_list_holds() {
    let mut stopped = stopped_at_count();
    let at_the_stop = stopped.list_breakpoints();
    let (mut emu, _) = stepped();
    assert_shapes(
        &[
            ("at the stop", at_the_stop),
            ("after the steps", emu.list_breakpoints()),
        ],
        &[
            (
                "at the stop",
                "[{breakpoint: {address: 1280, bp_type: {Access: {exec: true, read: \
                         false, write: false}}, condition: undefined, cpu_id: 0}, enabled: \
                         true, hit_count: 1, id: 0, match_spec: {action: \"break\", \
                         address_end: undefined, expr: undefined, hit_skip: 0, one_shot: \
                         false, value: undefined, value_compare: \"eq\", value_mask: 0, \
                         value_size: 0}}, {breakpoint: {address: 16711680, bp_type: {Access: \
                         {exec: false, read: false, write: true}}, condition: undefined, \
                         cpu_id: 0}, enabled: true, hit_count: 0, id: 1, match_spec: {action: \
                         \"break\", address_end: undefined, expr: undefined, hit_skip: 0, \
                         one_shot: false, value: undefined, value_compare: \"eq\", \
                         value_mask: 0, value_size: 0}}, {breakpoint: {address: 0, bp_type: \
                         {Event: {code: 6, param: 1}}, condition: undefined, cpu_id: \
                         undefined}, enabled: true, hit_count: 0, id: 2, match_spec: {action: \
                         \"break\", address_end: undefined, expr: undefined, hit_skip: 0, \
                         one_shot: false, value: undefined, value_compare: \"eq\", \
                         value_mask: 0, value_size: 0}}]",
            ),
            (
                "after the steps",
                "[{breakpoint: {address: 1280, bp_type: {Access: {exec: true, read: \
                         false, write: false}}, condition: undefined, cpu_id: 0}, enabled: \
                         true, hit_count: 1, id: 0, match_spec: {action: \"break\", \
                         address_end: undefined, expr: undefined, hit_skip: 0, one_shot: \
                         false, value: undefined, value_compare: \"eq\", value_mask: 0, \
                         value_size: 0}}, {breakpoint: {address: 16711680, bp_type: {Access: \
                         {exec: false, read: false, write: true}}, condition: undefined, \
                         cpu_id: 0}, enabled: true, hit_count: 1, id: 1, match_spec: {action: \
                         \"break\", address_end: undefined, expr: undefined, hit_skip: 0, \
                         one_shot: false, value: undefined, value_compare: \"eq\", \
                         value_mask: 0, value_size: 0}}, {breakpoint: {address: 0, bp_type: \
                         {Event: {code: 6, param: 1}}, condition: undefined, cpu_id: \
                         undefined}, enabled: true, hit_count: 1, id: 2, match_spec: {action: \
                         \"break\", address_end: undefined, expr: undefined, hit_skip: 0, \
                         one_shot: false, value: undefined, value_compare: \"eq\", \
                         value_mask: 0, value_size: 0}}]",
            ),
        ],
    );
}

/// Each event names its kind beside its code.
#[wasm_bindgen_test]
fn what_the_event_catalogue_holds() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    assert_shapes(
        &[("catalogue", emu.get_available_events())],
        &[(
            "catalogue",
            "[{code: 1, description: \"The beam entered vertical blank\", kind: \
                     \"vertical_blank\", label: \"VBlank\", param_label: undefined, \
                     param_options: []}, {code: 2, description: \"A VBlank interrupt \
                     (68000 level 6) was delivered\", kind: \"vertical_blank_interrupt\", \
                     label: \"VInt\", param_label: undefined, param_options: []}, {code: \
                     3, description: \"An HBlank interrupt (68000 level 4) was \
                     delivered\", kind: \"raster_interrupt\", label: \"HInt\", \
                     param_label: undefined, param_options: []}, {code: 4, description: \
                     \"A rendered scanline matched the V counter\", kind: \"line\", \
                     label: \"Scanline\", param_label: \"Scanline (V)\", param_options: \
                     []}, {code: 5, description: \"A DMA from 68000 memory, a fill or a \
                     copy has written all its data\", kind: \"transfer_done\", label: \
                     \"Transfer complete\", param_label: \"Memory\", param_options: \
                     [{label: \"VRAM\", value: 0}, {label: \"CRAM\", value: 1}, {label: \
                     \"VSRAM\", value: 2}]}, {code: 6, description: \"A CRAM colour was \
                     written, through the data port, by a DMA or by a fill\", kind: \
                     \"palette_write\", label: \"Palette write\", param_label: \"Colour \
                     (0-63)\", param_options: []}, {code: 7, description: \"A DMA from \
                     68000 memory, a fill or a copy wrote into the sprite table\", kind: \
                     \"sprite_table_write\", label: \"Sprite table transfer\", \
                     param_label: undefined, param_options: []}, {code: 8, description: \
                     \"A write changed the bytes of a tile in VRAM (writing the same bytes \
                     again does not count)\", kind: \"tile_write\", label: \"Tile \
                     rewritten\", param_label: \"Tile (0-2047)\", param_options: []}]",
        )],
    );
}

/// A new emulator runs and a load leaves the mode as it was; a name the
/// binding does not know sets the running mode.
#[wasm_bindgen_test]
fn what_the_execution_mode_reads() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    let new = emu.get_execution_mode();
    emu.load_rom(&debugger_shapes_rom()).unwrap();
    let loaded = emu.get_execution_mode();
    emu.set_execution_mode("Running".to_string());
    let running = emu.get_execution_mode();
    emu.set_execution_mode("StepOver".to_string());
    let step_over = emu.get_execution_mode();
    emu.set_execution_mode("no such mode".to_string());
    let unknown = emu.get_execution_mode();
    let mut stopped = stopped_at_count();
    let (mut after_the_steps, _) = stepped();
    let found: Vec<(&str, JsValue)> = [
        ("a new emulator", new),
        ("after a load", loaded),
        ("set to Running", running),
        ("set to StepOver", step_over),
        ("set to a name it does not know", unknown),
        ("at the stop", stopped.get_execution_mode()),
        ("after the steps", after_the_steps.get_execution_mode()),
    ]
    .into_iter()
    .map(|(what, mode)| (what, JsValue::from_str(&mode)))
    .collect();
    assert_shapes(
        &found,
        &[
            ("a new emulator", "\"Running\""),
            ("after a load", "\"Running\""),
            ("set to Running", "\"Running\""),
            ("set to StepOver", "\"StepOver\""),
            ("set to a name it does not know", "\"Running\""),
            ("at the stop", "\"Paused\""),
            ("after the steps", "\"Paused\""),
        ],
    );
}

/// The Z80's RAM is the sound CPU's RAM, `AudioRam`.
#[wasm_bindgen_test]
fn what_the_memory_regions_hold() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    assert_shapes(
        &[("regions", emu.get_memory_regions())],
        &[(
            "regions",
            "[{name: \"Work RAM\", region: \"MainRam\", size: 65536, \
                     start_address: 16711680, writable: true}, {name: \"VRAM\", region: \
                     \"VideoRam\", size: 65536, start_address: 12582912, writable: true}, \
                     {name: \"ROM\", region: \"Rom\", size: 4194304, start_address: 0, \
                     writable: false}, {name: \"Z80 RAM\", region: \"AudioRam\", size: \
                     8192, start_address: 10485760, writable: true}]",
        )],
    );
}

/// The light tier that ran to the stop keeps no history; the steps' entries
/// come back the newest first, and the Z80's carry no registers.
#[wasm_bindgen_test]
fn what_the_instruction_history_holds() {
    let mut stopped = stopped_at_count();
    let at_the_stop = stopped.get_instruction_history(0, 8);
    let (mut emu, _) = stepped();
    assert_shapes(
        &[
            ("68000 at the stop", at_the_stop),
            (
                "68000 after the steps, 8",
                emu.get_instruction_history(0, 8),
            ),
            (
                "68000 after the steps, 3",
                emu.get_instruction_history(0, 3),
            ),
            (
                "68000 after the steps, 0",
                emu.get_instruction_history(0, 0),
            ),
            ("Z80 after the steps", emu.get_instruction_history(1, 8)),
            ("CPU 2", emu.get_instruction_history(2, 8)),
        ],
        &[
            ("68000 at the stop", "[]"),
            (
                "68000 after the steps, 8",
                "[{address: 1324, cycle_count: 16, instruction: \"RTS\", \
                         registers_before: [{name: \"PC\", value: 1324}, {name: \"SR\", \
                         value: 9984}]}, {address: 1318, cycle_count: 20, instruction: \
                         \"MOVE.W  $00FF0000, (A1)\", registers_before: [{name: \"PC\", \
                         value: 1318}, {name: \"SR\", value: 9992}]}, {address: 1312, \
                         cycle_count: 20, instruction: \"MOVE.L  #$C0020000, (A0)\", \
                         registers_before: [{name: \"PC\", value: 1312}, {name: \"SR\", \
                         value: 9984}]}, {address: 1090, cycle_count: 18, instruction: \
                         \"BSR     $00000520\", registers_before: [{name: \"PC\", value: \
                         1090}, {name: \"SR\", value: 9984}]}, {address: 1286, cycle_count: \
                         16, instruction: \"RTS\", registers_before: [{name: \"PC\", value: \
                         1286}, {name: \"SR\", value: 9984}]}, {address: 1280, cycle_count: \
                         20, instruction: \"ADDQ.W #1, $00FF0000\", registers_before: [{name: \
                         \"PC\", value: 1280}, {name: \"SR\", value: 9984}]}]",
            ),
            (
                "68000 after the steps, 3",
                "[{address: 1324, cycle_count: 16, instruction: \"RTS\", \
                         registers_before: [{name: \"PC\", value: 1324}, {name: \"SR\", \
                         value: 9984}]}, {address: 1318, cycle_count: 20, instruction: \
                         \"MOVE.W  $00FF0000, (A1)\", registers_before: [{name: \"PC\", \
                         value: 1318}, {name: \"SR\", value: 9992}]}, {address: 1312, \
                         cycle_count: 20, instruction: \"MOVE.L  #$C0020000, (A0)\", \
                         registers_before: [{name: \"PC\", value: 1312}, {name: \"SR\", \
                         value: 9984}]}]",
            ),
            ("68000 after the steps, 0", "[]"),
            (
                "Z80 after the steps",
                "[{address: 40, cycle_count: 4, instruction: \"NOP\", \
                         registers_before: []}, {address: 39, cycle_count: 4, instruction: \
                         \"NOP\", registers_before: []}]",
            ),
            ("CPU 2", "[]"),
        ],
    );
}

/// At the light tier's stop the JSR's frame holds the address after the
/// six-byte JSR at $43C, $442, as its return address; count's RTS takes
/// the frame off, the BSR.W pushes colour's and colour's RTS takes it
/// off. The Z80 has no call stack.
#[wasm_bindgen_test]
fn what_the_call_stack_holds() {
    let mut stopped = stopped_at_count();
    let at_the_stop = stopped.get_call_stack(0);
    let (mut emu, _) = stepped();
    assert_shapes(
        &[
            ("68000 at the stop", at_the_stop),
            ("68000 after the steps", emu.get_call_stack(0)),
            ("Z80 after the steps", emu.get_call_stack(1)),
            ("CPU 2", emu.get_call_stack(2)),
        ],
        &[
            (
                "68000 at the stop",
                "[{call_address: 1084, function_name: undefined, return_address: \
                         1090}]",
            ),
            ("68000 after the steps", "[]"),
            ("Z80 after the steps", "[]"),
            ("CPU 2", "[]"),
        ],
    );
}

/// A CPU past the two this console has is not taken.
#[wasm_bindgen_test]
fn what_the_focused_cpu_reads() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    let new = emu.get_focused_cpu();
    emu.set_focused_cpu(1);
    let one = emu.get_focused_cpu();
    emu.set_focused_cpu(2);
    let two = emu.get_focused_cpu();
    emu.set_focused_cpu(0);
    let zero = emu.get_focused_cpu();
    let found: Vec<(&str, JsValue)> = [
        ("a new emulator", new),
        ("set to 1", one),
        ("then set to 2", two),
        ("then set to 0", zero),
    ]
    .into_iter()
    .map(|(what, cpu)| (what, JsValue::from(cpu as u32)))
    .collect();
    assert_shapes(
        &found,
        &[
            ("a new emulator", "0"),
            ("set to 1", "1"),
            ("then set to 2", "1"),
            ("then set to 0", "0"),
        ],
    );
}

/// An address is masked to the CPU's bus (24 bits, 16 bits) before it is
/// looked up.
#[wasm_bindgen_test]
fn what_the_coverage_reads() {
    let (emu, _) = stepped();
    let found: Vec<(String, JsValue)> = [
        (0, COUNT),
        (0, 0x506),
        (0, 0x520),
        (0, 0x600),
        (0, 0xFF00_0500),
        (1, 0x0000),
        (1, 0x0040),
        (1, 0x0042),
        (1, 0x1_0040),
        (2, 0x0000),
    ]
    .into_iter()
    .map(|(cpu, address)| {
        (
            format!("CPU {cpu} at ${address:X}"),
            JsValue::from_bool(emu.is_covered(cpu, address)),
        )
    })
    .collect();
    assert_shapes(
        &found,
        &[
            ("CPU 0 at $500", "true"),
            ("CPU 0 at $506", "true"),
            ("CPU 0 at $520", "true"),
            ("CPU 0 at $600", "false"),
            ("CPU 0 at $FF000500", "true"),
            ("CPU 1 at $0", "true"),
            ("CPU 1 at $40", "true"),
            ("CPU 1 at $42", "false"),
            ("CPU 1 at $10040", "true"),
            ("CPU 2 at $0", "false"),
        ],
    );
}

#[wasm_bindgen_test]
fn what_the_coverage_stats_hold() {
    let fresh = RetroEmulator::new("genesis").unwrap();
    let (emu, _) = stepped();
    assert_shapes(
        &[
            ("68000 on a new emulator", fresh.get_coverage_stats(0)),
            ("68000", emu.get_coverage_stats(0)),
            ("Z80", emu.get_coverage_stats(1)),
            ("CPU 2", emu.get_coverage_stats(2)),
        ],
        &[
            (
                "68000 on a new emulator",
                "{coveredBytes: 0, percent: 0, totalBytes: 16777216}",
            ),
            (
                "68000",
                "{coveredBytes: 17, percent: 0.00010132789611816406, totalBytes: \
                         16777216}",
            ),
            (
                "Z80",
                "{coveredBytes: 65, percent: 0.09918212890625, totalBytes: 65536}",
            ),
            ("CPU 2", "{coveredBytes: 0, percent: 0, totalBytes: 0}"),
        ],
    );
}

/// The flag reads every CPU's trace ring: with only the Z80's ring full
/// it is true.
#[wasm_bindgen_test]
fn what_the_trace_flush_flag_reads() {
    let (emu, _) = stepped();
    let full = with_the_z80_ring_full();
    assert_shapes(
        &[
            (
                "after the steps",
                JsValue::from_bool(emu.needs_trace_flush()),
            ),
            (
                "with only the Z80's ring full",
                JsValue::from_bool(full.needs_trace_flush()),
            ),
        ],
        &[
            ("after the steps", "false"),
            ("with only the Z80's ring full", "true"),
        ],
    );
}

#[wasm_bindgen_test]
fn what_the_z80_trace_flush_flag_reads() {
    let (emu, _) = stepped();
    let full = with_the_z80_ring_full();
    assert_shapes(
        &[
            (
                "after the steps",
                JsValue::from_bool(emu.needs_z80_trace_flush()),
            ),
            (
                "with only the Z80's ring full",
                JsValue::from_bool(full.needs_z80_trace_flush()),
            ),
        ],
        &[
            ("after the steps", "false"),
            ("with only the Z80's ring full", "true"),
        ],
    );
}

#[wasm_bindgen_test]
fn what_the_trace_flush_data_holds() {
    let (emu, _) = stepped();
    let data = emu.get_trace_flush_data();
    let fresh = RetroEmulator::new("genesis").unwrap();
    let mut found = chunk_head(&data, sg1_core::debugger::TRACE_ENTRY_SIZE);
    found.push((
        "length on a new emulator",
        JsValue::from(fresh.get_trace_flush_data().len() as u32),
    ));
    assert_shapes(&found, &[
                    (
                        "length",
                        "588",
                    ),
                    (
                        "first entry",
                        "\"3c040000002700000000000000000000000000000000000000000000000000000000000000000400c0000000c000000000000000000000000000000000000000000000000000b94e140030f40100000000000000\"",
                    ),
                    (
                        "length on a new emulator",
                        "0",
                    ),
                ]);
}

#[wasm_bindgen_test]
fn what_the_trace_entry_size_reads() {
    let emu = RetroEmulator::new("genesis").unwrap();
    assert_shapes(
        &[("size", JsValue::from(emu.get_trace_entry_size() as u32))],
        &[("size", "84")],
    );
}

/// Three 68000 trace entries, field by field, and ten bytes past
/// them, in bytes.
const FIXED_68000_ENTRIES: [&str; 3] = [
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

/// The fixed entries' lines are the text the core's `debugger_baseline`
/// test holds for the same entries.
#[wasm_bindgen_test]
fn what_a_trace_chunk_reads_as_text() {
    let (mut emu, _) = stepped();
    let data = emu.get_trace_flush_data();
    let head = sg1_core::debugger::TRACE_ENTRY_SIZE * 2;
    let traced = emu.convert_trace_chunk_to_text(&data[..head]);

    // The fixed entries, on a program that holds the instructions
    // they name at $400: MOVE.L #$12345678,D0, RTS, JSR $400.
    let mut rom = synth_genesis_rom("TRACE TEXT");
    #[rustfmt::skip]
            place(&mut rom, 0x400, &[
                0x203C, 0x1234, 0x5678,
                0x4E75,
                0x4EB9, 0x0000, 0x0400,
            ]);
    let mut fixed = RetroEmulator::new("genesis").unwrap();
    fixed.load_rom(&rom).unwrap();
    let mut chunk: Vec<u8> = FIXED_68000_ENTRIES
        .iter()
        .flat_map(|entry| from_hex(entry))
        .collect();
    chunk.extend_from_slice(&[0xAA; 10]);
    let fixed_text = fixed.convert_trace_chunk_to_text(&chunk);

    let mut found: Vec<(String, JsValue)> = lines_of(&traced)
        .into_iter()
        .map(|(what, value)| (format!("traced, {what}"), value))
        .collect();
    found.extend(
        lines_of(&fixed_text)
            .into_iter()
            .map(|(what, value)| (format!("fixed, {what}"), value)),
    );
    assert_shapes(
        &found,
        &[
            (
                "traced, line 1",
                "\"PC:00043C SR:2700 D0:00000000 D1:00000000 D2:00000000 D3:00000000 \
                         D4:00000000 D5:00000000 D6:00000000 D7:00000000 A0:00C00004 \
                         A1:00C00000 A2:00000000 A3:00000000 A4:00000000 A5:00000000 \
                         A6:00000000 A7:00000000 | JSR $00000500\"",
            ),
            (
                "traced, line 2",
                "\"PC:000500 SR:2700 D0:00000000 D1:00000000 D2:00000000 D3:00000000 \
                         D4:00000000 D5:00000000 D6:00000000 D7:00000000 A0:00C00004 \
                         A1:00C00000 A2:00000000 A3:00000000 A4:00000000 A5:00000000 \
                         A6:00000000 A7:FFFFFFFC | ADDQ.W #1, $00FF0000\"",
            ),
            ("traced, ends with a line break", "true"),
            (
                "fixed, line 1",
                "\"PC:000400 SR:2700 D0:00000000 D1:12345678 D2:FFFFFFFF D3:80000001 \
                         D4:000000FF D5:00FF0000 D6:7FFFFFFF D7:01020304 A0:00FF0000 \
                         A1:00C00004 A2:00C00000 A3:00A00000 A4:00000400 A5:00FFFFFE \
                         A6:00000000 A7:00FFFE00 | MOVE.L #$12345678, D0\"",
            ),
            (
                "fixed, line 2",
                "\"PC:000406 SR:2015 D0:00000001 D1:00000002 D2:00000003 D3:00000004 \
                         D4:00000005 D5:00000006 D6:00000007 D7:00000008 A0:00000009 \
                         A1:0000000A A2:0000000B A3:0000000C A4:0000000D A5:0000000E \
                         A6:0000000F A7:00FFFDFC | RTS \"",
            ),
            (
                "fixed, line 3",
                "\"PC:000408 SR:0000 D0:00000000 D1:00000000 D2:00000000 D3:00000000 \
                         D4:00000000 D5:00000000 D6:00000000 D7:00000000 A0:00000000 \
                         A1:00000000 A2:00000000 A3:00000000 A4:00000000 A5:00000000 \
                         A6:00000000 A7:00000000 | JSR $00000400\"",
            ),
            ("fixed, ends with a line break", "true"),
        ],
    );
}

/// A new emulator has no ring yet, so its capacity reads 0; the first
/// enabling of the trace makes it, at 8 MiB.
#[wasm_bindgen_test]
fn what_the_trace_stats_hold() {
    let fresh = RetroEmulator::new("genesis").unwrap();
    let (emu, _) = stepped();
    assert_shapes(
        &[
            ("on a new emulator", fresh.get_trace_stats()),
            ("after the steps", emu.get_trace_stats()),
        ],
        &[
            (
                "on a new emulator",
                "{buffer_bytes: 0, buffer_capacity: 0, buffered_entries: 0, \
                         enabled: false, flushed_entries: 0, total_entries: 0}",
            ),
            (
                "after the steps",
                "{buffer_bytes: 588, buffer_capacity: 8388608, buffered_entries: 7, \
                         enabled: true, flushed_entries: 0, total_entries: 7}",
            ),
        ],
    );
}

#[wasm_bindgen_test]
fn what_the_tracing_flag_reads() {
    let fresh = RetroEmulator::new("genesis").unwrap();
    let (emu, _) = stepped();
    assert_shapes(
        &[
            (
                "on a new emulator",
                JsValue::from_bool(fresh.is_tracing_enabled()),
            ),
            (
                "after the steps",
                JsValue::from_bool(emu.is_tracing_enabled()),
            ),
        ],
        &[("on a new emulator", "false"), ("after the steps", "true")],
    );
}

/// A host step reports no hit: the watch and the palette write are counted.
#[wasm_bindgen_test]
fn what_a_step_returns() {
    let (_, returned) = stepped();
    let found: Vec<(String, JsValue)> = returned
        .into_iter()
        .enumerate()
        .map(|(at, value)| {
            let what = if at < 6 {
                format!("step {}", at + 1)
            } else {
                format!("stepCpu(1) {}", at - 5)
            };
            (what, value)
        })
        .collect();
    assert_shapes(
        &found,
        &[
            (
                "step 1",
                "{breakpoint_hit: false, cycles: 20, instruction_executed: true}",
            ),
            (
                "step 2",
                "{breakpoint_hit: false, cycles: 16, instruction_executed: true}",
            ),
            (
                "step 3",
                "{breakpoint_hit: false, cycles: 18, instruction_executed: true}",
            ),
            (
                "step 4",
                "{breakpoint_hit: false, cycles: 20, instruction_executed: true}",
            ),
            (
                "step 5",
                "{breakpoint_hit: false, cycles: 22, instruction_executed: true}",
            ),
            (
                "step 6",
                "{breakpoint_hit: false, cycles: 16, instruction_executed: true}",
            ),
            (
                "stepCpu(1) 1",
                "{breakpoint_hit: false, cycles: 4, instruction_executed: true}",
            ),
            (
                "stepCpu(1) 2",
                "{breakpoint_hit: false, cycles: 4, instruction_executed: true}",
            ),
        ],
    );
}

#[wasm_bindgen_test]
fn what_a_step_of_each_cpu_returns() {
    let mut emu = stopped_at_count();
    assert_shapes(
        &[
            ("stepCpu(0)", emu.step_cpu(0)),
            ("stepCpu(1)", emu.step_cpu(1)),
            ("stepCpu(2)", emu.step_cpu(2)),
        ],
        &[
            (
                "stepCpu(0)",
                "{breakpoint_hit: false, cycles: 20, instruction_executed: true}",
            ),
            (
                "stepCpu(1)",
                "{breakpoint_hit: false, cycles: 4, instruction_executed: true}",
            ),
            (
                "stepCpu(2)",
                "{breakpoint_hit: false, cycles: 0, instruction_executed: false}",
            ),
        ],
    );
}

#[wasm_bindgen_test]
fn what_turning_z80_tracing_on_and_off_does() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    emu.load_rom(&debugger_shapes_rom()).unwrap();
    emu.set_execution_mode("Running".to_string());
    let mut found: Vec<(&str, JsValue)> = Vec::new();
    emu.set_z80_tracing_enabled(true);
    emu.run_frame();
    found.push((
        "on, a frame run: bytes",
        JsValue::from(emu.get_z80_trace_flush_data().len() as u32),
    ));
    emu.set_z80_tracing_enabled(false);
    emu.run_frame();
    found.push((
        "off, a frame run: bytes",
        JsValue::from(emu.get_z80_trace_flush_data().len() as u32),
    ));
    emu.set_z80_tracing_enabled(true);
    found.push((
        "on again: bytes",
        JsValue::from(emu.get_z80_trace_flush_data().len() as u32),
    ));
    found.push((
        "on again: flag",
        JsValue::from_bool(emu.is_z80_tracing_enabled()),
    ));
    assert_shapes(
        &found,
        &[
            ("on, a frame run: bytes", "556624"),
            ("off, a frame run: bytes", "556624"),
            ("on again: bytes", "0"),
            ("on again: flag", "true"),
        ],
    );
}

#[wasm_bindgen_test]
fn what_the_z80_tracing_flag_reads() {
    let fresh = RetroEmulator::new("genesis").unwrap();
    let (emu, _) = stepped();
    assert_shapes(
        &[
            (
                "on a new emulator",
                JsValue::from_bool(fresh.is_z80_tracing_enabled()),
            ),
            (
                "after the steps",
                JsValue::from_bool(emu.is_z80_tracing_enabled()),
            ),
        ],
        &[("on a new emulator", "false"), ("after the steps", "true")],
    );
}

/// The frames of the Z80, the Z80 instructions the six 68000 steps ran,
/// and the two Z80 steps.
#[wasm_bindgen_test]
fn what_the_z80_trace_flush_data_holds() {
    let (emu, _) = stepped();
    let data = emu.get_z80_trace_flush_data();
    assert_shapes(
        &chunk_head(&data, sg1_core::debugger::Z80_TRACE_ENTRY_SIZE),
        &[
            ("length", "557308"),
            (
                "first entry",
                "\"0000000000000000000000000000ffff00000000000000000000000000000000000000000000\"",
            ),
        ],
    );
}

#[wasm_bindgen_test]
fn what_clearing_the_flushed_z80_trace_does() {
    let (mut emu, _) = stepped();
    let mut full = with_the_z80_ring_full();
    emu.clear_z80_trace_flushed();
    full.clear_z80_trace_flushed();
    assert_shapes(
        &[
            (
                "after the steps: bytes",
                JsValue::from(emu.get_z80_trace_flush_data().len() as u32),
            ),
            (
                "with the ring full: bytes",
                JsValue::from(full.get_z80_trace_flush_data().len() as u32),
            ),
            (
                "with the ring full: flag",
                JsValue::from_bool(full.needs_z80_trace_flush()),
            ),
        ],
        &[
            ("after the steps: bytes", "0"),
            ("with the ring full: bytes", "0"),
            ("with the ring full: flag", "false"),
        ],
    );
}

#[wasm_bindgen_test]
fn what_the_z80_trace_entry_size_reads() {
    let emu = RetroEmulator::new("genesis").unwrap();
    assert_shapes(
        &[("size", JsValue::from(emu.get_z80_trace_entry_size() as u32))],
        &[("size", "38")],
    );
}

/// Three Z80 trace entries, field by field, in bytes.
const FIXED_Z80_ENTRIES: [&str; 3] = [
    "000034127856bc9af0de02010403ff1f007f010101000a000000000001000000050101d91f00",
    "3412ffff000001000080ffff00000000ff8002000101110000000000000000000000cd000100",
    "3800440010000010000100000000f01f00000000000015006300000000000000e000edb00000",
];

/// The fixed entries' lines are the text the core's `debugger_baseline`
/// test holds for the same entries.
#[wasm_bindgen_test]
fn what_a_z80_trace_chunk_reads_as_text() {
    let (emu, _) = stepped();
    let data = emu.get_z80_trace_flush_data();
    let head = sg1_core::debugger::Z80_TRACE_ENTRY_SIZE * 2;
    let traced = emu.convert_z80_trace_chunk_to_text(&data[..head]);
    let mut chunk: Vec<u8> = FIXED_Z80_ENTRIES
        .iter()
        .flat_map(|entry| from_hex(entry))
        .collect();
    chunk.extend_from_slice(&[0xAA; 10]);
    let fixed_text = emu.convert_z80_trace_chunk_to_text(&chunk);

    let mut found: Vec<(String, JsValue)> = lines_of(&traced)
        .into_iter()
        .map(|(what, value)| (format!("traced, {what}"), value))
        .collect();
    found.extend(
        lines_of(&fixed_text)
            .into_iter()
            .map(|(what, value)| (format!("fixed, {what}"), value)),
    );
    assert_shapes(
        &found,
        &[
            (
                "traced, line 1",
                "\"PC:0000 AF:0000 BC:0000 DE:0000 HL:0000 IX:0000 IY:0000 SP:FFFF \
                         I:00 R:00 IM:0 IFF1:0 IFF2:0 HALT:0 | nop\"",
            ),
            (
                "traced, line 2",
                "\"PC:0001 AF:0000 BC:0000 DE:0000 HL:0000 IX:0000 IY:0000 SP:FFFF \
                         I:00 R:01 IM:0 IFF1:0 IFF2:0 HALT:0 | nop\"",
            ),
            ("traced, ends with a line break", "true"),
            (
                "fixed, line 1",
                "\"PC:0000 AF:1234 BC:5678 DE:9ABC HL:DEF0 IX:0102 IY:0304 SP:1FFF \
                         I:00 R:7F IM:1 IFF1:1 IFF2:1 HALT:0 | ld bc,$1FD9\"",
            ),
            (
                "fixed, line 2",
                "\"PC:1234 AF:FFFF BC:0000 DE:0001 HL:8000 IX:FFFF IY:0000 SP:0000 \
                         I:FF R:80 IM:2 IFF1:0 IFF2:1 HALT:1 | call $0100\"",
            ),
            (
                "fixed, line 3",
                "\"PC:0038 AF:0044 BC:0010 DE:1000 HL:0100 IX:0000 IY:0000 SP:1FF0 \
                         I:00 R:00 IM:0 IFF1:0 IFF2:0 HALT:0 | ldir\"",
            ),
            ("fixed, ends with a line break", "true"),
        ],
    );
}

/// Each event's kind, in the catalogue's order: the same words on
/// every console, so a host finds an event by what it is.
#[wasm_bindgen_test]
fn each_event_names_its_kind() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    let catalogue: js_sys::Array = emu.get_available_events().into();
    let kinds: Vec<String> = catalogue
        .iter()
        .map(|event| field(&event, "kind").as_string().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "vertical_blank",
            "vertical_blank_interrupt",
            "raster_interrupt",
            "line",
            "transfer_done",
            "palette_write",
            "sprite_table_write",
            "tile_write",
        ]
    );
}

/// The Z80's RAM is named as the sound CPU's RAM.
#[wasm_bindgen_test]
fn the_z80_ram_is_the_sound_cpus_ram() {
    let mut emu = RetroEmulator::new("genesis").unwrap();
    let regions: js_sys::Array = emu.get_memory_regions().into();
    let z80_ram = regions
        .iter()
        .find(|region| field(region, "name").as_string().as_deref() == Some("Z80 RAM"))
        .expect("the Z80's RAM is listed");
    assert_eq!(
        field(&z80_ram, "region").as_string().as_deref(),
        Some("AudioRam")
    );
}

/// A call's frame returns to the address after the call: the six-byte
/// JSR at $43C to $442, the four-byte BSR.W at $442 to $446.
#[wasm_bindgen_test]
fn a_call_returns_to_the_address_after_it() {
    let frames = |emu: &mut RetroEmulator| -> Vec<(u32, u32)> {
        let stack: js_sys::Array = emu.get_call_stack(0).into();
        stack
            .iter()
            .map(|frame| {
                (
                    field(&frame, "call_address").as_f64().unwrap() as u32,
                    field(&frame, "return_address").as_f64().unwrap() as u32,
                )
            })
            .collect()
    };
    let mut emu = stopped_at_count();
    assert_eq!(frames(&mut emu), [(0x43C, 0x442)]);
    // count's ADDQ and RTS, then the BSR.W into colour.
    for _ in 0..3 {
        emu.step();
    }
    assert_eq!(frames(&mut emu), [(0x442, 0x446)]);
}

/// A reset, a load of the ROM and a load of a savestate each empty the
/// call stack, whose frames belonged to the machine replaced.
#[wasm_bindgen_test]
fn a_reset_or_a_load_empties_the_call_stack() {
    let empty = |emu: &mut RetroEmulator| js_sys::Array::from(&emu.get_call_stack(0)).length() == 0;
    let mut emu = stopped_at_count();
    let state = emu.save_state();
    assert!(!empty(&mut emu));
    emu.reset();
    assert!(empty(&mut emu), "after a reset");

    let mut emu = stopped_at_count();
    emu.load_rom(&debugger_shapes_rom()).unwrap();
    assert!(empty(&mut emu), "after a ROM load");

    let mut emu = stopped_at_count();
    emu.load_state(&state).unwrap();
    assert!(empty(&mut emu), "after a savestate load");
}

/// Each step a host asks for writes the CPU states, the Z80's too.
#[wasm_bindgen_test]
fn a_step_of_the_z80_writes_the_cpu_states() {
    let mut emu = stopped_at_count();
    for _ in 0..2 {
        emu.step_cpu(1);
        let states: js_sys::Array = emu.get_cpu_states().into();
        let z80_pc = field(&states.get(1), "pc").as_f64().unwrap() as u16;
        assert_eq!(z80_pc, emu.genesis.z80().pc);
    }
}

/// The trace flush flag asks whether any CPU's ring wants flushing;
/// the Z80's own flag reads the Z80's ring alone.
#[wasm_bindgen_test]
fn the_trace_flush_flag_reads_every_cpus_ring() {
    let full = with_the_z80_ring_full();
    assert!(full.needs_z80_trace_flush());
    assert!(full.needs_trace_flush());
    let (stepped, _) = stepped();
    assert!(!stepped.needs_z80_trace_flush());
    assert!(!stepped.needs_trace_flush());
}

/// A CPU's trace ring is made the first time its trace is enabled.
#[wasm_bindgen_test]
fn a_trace_ring_is_made_when_its_trace_is_first_enabled() {
    let capacity = |emu: &RetroEmulator| {
        field(&emu.get_trace_stats(), "buffer_capacity")
            .as_f64()
            .unwrap()
    };
    let mut emu = RetroEmulator::new("genesis").unwrap();
    assert_eq!(capacity(&emu), 0.0);
    emu.set_tracing_enabled(true);
    assert_eq!(capacity(&emu), 8.0 * 1024.0 * 1024.0);
}

/// What the debugger checks a CPU's breakpoints for: everything on the
/// 68000; nothing on the Z80, whose breakpoints no frame loop checks,
/// nor past the two CPUs.
#[wasm_bindgen_test]
fn what_each_cpus_breakpoints_are_checked_for() {
    let emu = RetroEmulator::new("genesis").unwrap();
    assert_shapes(
        &[
            ("68000", emu.get_breakpoint_checks(0)),
            ("Z80", emu.get_breakpoint_checks(1)),
            ("CPU 2", emu.get_breakpoint_checks(2)),
        ],
        &[
            ("68000", "{conditions: true, execute: true, memory: true}"),
            ("Z80", "{conditions: false, execute: false, memory: false}"),
            (
                "CPU 2",
                "{conditions: false, execute: false, memory: false}",
            ),
        ],
    );
}
