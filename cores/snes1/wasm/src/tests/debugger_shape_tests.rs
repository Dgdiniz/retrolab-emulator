use super::*;
use snes1_core::TraceEntryBinary65816;

/// Bytes of one entry of each trace.
const TRACE_ENTRY_SIZE_65816: usize = <TraceEntryBinary65816 as TraceEntry>::SIZE;
const TRACE_ENTRY_SIZE_SPC700: usize = <TraceEntryBinarySpc700 as TraceEntry>::SIZE;
#[cfg(feature = "chip-superfx")]
const TRACE_ENTRY_SIZE_GSU: usize = <TraceEntryBinaryGsu as TraceEntry>::SIZE;
#[cfg(feature = "chip-cx4")]
const TRACE_ENTRY_SIZE_CX4: usize = <TraceEntryBinaryCx4 as TraceEntry>::SIZE;

/// A number as a host reads it.
fn number(value: usize) -> JsValue {
    JsValue::from_f64(value as f64)
}

/// Bytes as a host reads them: a `Uint8Array` copy.
fn byte_array(bytes: &[u8]) -> JsValue {
    js_sys::Uint8Array::from(bytes).into()
}

/// The length of a trace chunk, its first entry and the whole chunk.
fn chunk(data: &[u8], entry_size: usize) -> Vec<JsValue> {
    vec![
        number(data.len()),
        byte_array(&data[..entry_size.min(data.len())]),
        byte_array(data),
    ]
}

/// A `JSR` to a routine that makes a `JSL` to another, which loads a
/// value, stores it to work RAM at `$0010` and returns with `RTL`;
/// the first returns with `RTS` to a loop.
fn debugger_program_rom() -> Vec<u8> {
    lorom_running(
        "DEBUGGER PROGRAM",
        &[0x20, 0x10, 0x80, 0x80, 0xFE], // JSR $8010, BRA $8003
        &[
            (0x8010, &[0x22, 0x20, 0x80, 0x00, 0x60]), // JSL $00:8020, RTS
            (0x8020, &[0xA9, 0x12, 0x8D, 0x10, 0x00, 0x6B]), // LDA #$12, STA $0010, RTL
        ],
    )
}

/// The program with its three breakpoints (an execute breakpoint at
/// `$8020`, a write watch on `$0010`, a VBlank event breakpoint) and
/// the main CPU's trace on, run until the execute breakpoint stops it
/// in front of the load: on the instrumented loop, through the `JSR`
/// and the `JSL`.
fn stopped_at_the_breakpoint() -> RetroEmulator {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&debugger_program_rom()).unwrap();
    emu.add_breakpoint(0x00_8020, "execute".to_string(), None);
    emu.add_breakpoint(0x00_0010, "write".to_string(), None);
    emu.add_event_breakpoint(1, 0xFFFF_FFFF);
    emu.set_tracing_enabled(true);
    emu.run_frame();
    emu
}

/// One instruction on the full loop: the step mode, then a frame.
fn step_into(emu: &mut RetroEmulator) {
    emu.set_execution_mode("StepInto".to_string());
    emu.run_frame();
}

/// The program stopped at its breakpoint, then stepped twice on the
/// full loop: the load, and the store the write watch sees. It waits
/// in front of the `RTL`.
fn debugging_the_program() -> RetroEmulator {
    let mut emu = stopped_at_the_breakpoint();
    step_into(&mut emu);
    step_into(&mut emu);
    emu
}

/// Right after the stop on the instrumented loop, the states where it
/// stopped: the main CPU in front of the load at `$8020`. After the two
/// steps, the main CPU in front of the `RTL`.
#[wasm_bindgen_test]
fn the_cpu_states_read_as_before() {
    let mut emu = stopped_at_the_breakpoint();
    let mut states = vec![emu.get_cpu_states()];
    step_into(&mut emu);
    step_into(&mut emu);
    states.push(emu.get_cpu_states());
    let expected = [
        concat!(
            r#"[{cpu_id: 0, flags: 52, pc: 32800, registers: [{name: "A", value: 0}, "#,
            r#"{name: "X", value: 0}, {name: "Y", value: 0}, {name: "S", value: 504}, "#,
            r#"{name: "D", value: 0}, {name: "DB", value: 0}, {name: "PB", value: 0}, "#,
            r#"{name: "P", value: 52}, {name: "E", value: 1}]}, {cpu_id: 1, flags: 2, "#,
            r#"pc: 65472, registers: [{name: "A", value: 0}, {name: "X", value: 0}, "#,
            r#"{name: "Y", value: 0}, {name: "SP", value: 239}, {name: "YA", value: 0},"#,
            r#" {name: "PSW", value: 2}]}]"#,
        ),
        concat!(
            r#"[{cpu_id: 0, flags: 52, pc: 32805, registers: [{name: "A", value: 18}, "#,
            r#"{name: "X", value: 0}, {name: "Y", value: 0}, {name: "S", value: 504}, "#,
            r#"{name: "D", value: 0}, {name: "DB", value: 0}, {name: "PB", value: 0}, "#,
            r#"{name: "P", value: 52}, {name: "E", value: 1}]}, {cpu_id: 1, flags: 2, "#,
            r#"pc: 65472, registers: [{name: "A", value: 0}, {name: "X", value: 0}, "#,
            r#"{name: "Y", value: 0}, {name: "SP", value: 239}, {name: "YA", value: 0},"#,
            r#" {name: "PSW", value: 2}]}]"#,
        ),
    ];
    assert_eq!(shapes(&states), expected);
}

/// The three breakpoints once the program ran (the execute breakpoint and
/// the write watch hit once each, the event breakpoint never); then with
/// the write watch watching reads too, the event breakpoint disabled and the
/// execute breakpoint given a value and a hit skip; then with the write
/// watch removed.
#[wasm_bindgen_test]
fn the_breakpoint_list_reads_as_before() {
    let mut emu = debugging_the_program();
    let mut lists = vec![emu.list_breakpoints()];
    emu.set_breakpoint_access(1, false, true, true);
    emu.enable_breakpoint(2, false);
    let spec = MatchSpec {
        value: Some(0x12),
        hit_skip: 1,
        ..Default::default()
    };
    emu.set_breakpoint_match_spec(0, serde_wasm_bindgen::to_value(&spec).unwrap())
        .unwrap();
    lists.push(emu.list_breakpoints());
    emu.remove_breakpoint(1);
    lists.push(emu.list_breakpoints());
    let expected = [
        concat!(
            r#"[{breakpoint: {address: 32800, bp_type: {Access: {exec: true, read: "#,
            r#"false, write: false}}, condition: undefined, cpu_id: 0}, enabled: true, "#,
            r#"hit_count: 1, id: 0, match_spec: {action: "break", address_end: "#,
            r#"undefined, expr: undefined, hit_skip: 0, one_shot: false, value: "#,
            r#"undefined, value_compare: "eq", value_mask: 0, value_size: 0}}, "#,
            r#"{breakpoint: {address: 16, bp_type: {Access: {exec: false, read: false, "#,
            r#"write: true}}, condition: undefined, cpu_id: 0}, enabled: true, "#,
            r#"hit_count: 1, id: 1, match_spec: {action: "break", address_end: "#,
            r#"undefined, expr: undefined, hit_skip: 0, one_shot: false, value: "#,
            r#"undefined, value_compare: "eq", value_mask: 0, value_size: 0}}, "#,
            r#"{breakpoint: {address: 0, bp_type: {Event: {code: 1, param: 4294967295}}"#,
            r#", condition: undefined, cpu_id: undefined}, enabled: true, hit_count: 0,"#,
            r#" id: 2, match_spec: {action: "break", address_end: undefined, expr: "#,
            r#"undefined, hit_skip: 0, one_shot: false, value: undefined, "#,
            r#"value_compare: "eq", value_mask: 0, value_size: 0}}]"#,
        ),
        concat!(
            r#"[{breakpoint: {address: 32800, bp_type: {Access: {exec: true, read: "#,
            r#"false, write: false}}, condition: undefined, cpu_id: 0}, enabled: true, "#,
            r#"hit_count: 1, id: 0, match_spec: {action: "break", address_end: "#,
            r#"undefined, expr: undefined, hit_skip: 1, one_shot: false, value: 18, "#,
            r#"value_compare: "eq", value_mask: 0, value_size: 0}}, {breakpoint: "#,
            r#"{address: 16, bp_type: {Access: {exec: false, read: true, write: true}},"#,
            r#" condition: undefined, cpu_id: 0}, enabled: true, hit_count: 1, id: 1, "#,
            r#"match_spec: {action: "break", address_end: undefined, expr: undefined, "#,
            r#"hit_skip: 0, one_shot: false, value: undefined, value_compare: "eq", "#,
            r#"value_mask: 0, value_size: 0}}, {breakpoint: {address: 0, bp_type: "#,
            r#"{Event: {code: 1, param: 4294967295}}, condition: undefined, cpu_id: "#,
            r#"undefined}, enabled: false, hit_count: 0, id: 2, match_spec: {action: "#,
            r#""break", address_end: undefined, expr: undefined, hit_skip: 0, "#,
            r#"one_shot: false, value: undefined, value_compare: "eq", value_mask: 0, "#,
            r#"value_size: 0}}]"#,
        ),
        concat!(
            r#"[{breakpoint: {address: 32800, bp_type: {Access: {exec: true, read: "#,
            r#"false, write: false}}, condition: undefined, cpu_id: 0}, enabled: true, "#,
            r#"hit_count: 1, id: 0, match_spec: {action: "break", address_end: "#,
            r#"undefined, expr: undefined, hit_skip: 1, one_shot: false, value: 18, "#,
            r#"value_compare: "eq", value_mask: 0, value_size: 0}}, {breakpoint: "#,
            r#"{address: 0, bp_type: {Event: {code: 1, param: 4294967295}}, condition: "#,
            r#"undefined, cpu_id: undefined}, enabled: false, hit_count: 0, id: 2, "#,
            r#"match_spec: {action: "break", address_end: undefined, expr: undefined, "#,
            r#"hit_skip: 0, one_shot: false, value: undefined, value_compare: "eq", "#,
            r#"value_mask: 0, value_size: 0}}]"#,
        ),
    ];
    assert_eq!(shapes(&lists), expected);
}

/// The twelve event classes as a host reads them, each with its kind.
#[wasm_bindgen_test]
fn the_event_catalogue_reads_as_before() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    let expected = [concat!(
        r#"[{code: 1, description: "Entering vertical blank", kind: "#,
        r#""vertical_blank", label: "VBlank", param_label: undefined, "#,
        r#"param_options: []}, {code: 2, description: "V-blank NMI delivered to "#,
        r#"the 65C816", kind: "vertical_blank_interrupt", label: "NMI", "#,
        r#"param_label: undefined, param_options: []}, {code: 3, description: "#,
        r#""H/V-timer IRQ raised", kind: "raster_interrupt", label: "IRQ", "#,
        r#"param_label: undefined, param_options: []}, {code: 4, description: "#,
        r#""General-purpose DMA block started ($420B)", kind: "transfer_start", "#,
        r#"label: "DMA start", param_label: undefined, param_options: []}, {code: "#,
        r#"5, description: "Top-of-frame HDMA initialisation", kind: "#,
        r#""line_transfer_set_up", label: "HDMA start", param_label: undefined, "#,
        r#"param_options: []}, {code: 6, description: "A rendered scanline matched "#,
        r#"the V counter", kind: "line", label: "Scanline", param_label: "Scanline "#,
        r#"(V)", param_options: []}, {code: 7, description: "Write to a PPU "#,
        r#"register ($2100-$213F)", kind: "video_register_write", label: "PPU "#,
        r#"register write", param_label: "Register ($00-$3F)", param_options: []}, "#,
        r#"{code: 8, description: "65C816 write to an APU mailbox port "#,
        r#"($2140-$2143)", kind: "audio_port_write", label: "APU port write", "#,
        r#"param_label: "Port", param_options: [{label: "Port 0", value: 0}, "#,
        r#"{label: "Port 1", value: 1}, {label: "Port 2", value: 2}, {label: "Port "#,
        r#"3", value: 3}]}, {code: 9, description: "SPC700 write to an S-DSP "#,
        r#"register", kind: "audio_register_write", label: "S-DSP register write", "#,
        r#"param_label: "DSP register ($00-$7F)", param_options: []}, {code: 10, "#,
        r#"description: "A general DMA channel finished its block, by the B-bus "#,
        r#"port it wrote", kind: "transfer_done", label: "DMA done", param_label: "#,
        r#""B-bus port", param_options: [{label: "VRAM ($2118)", value: 24}, "#,
        r#"{label: "Palette ($2122)", value: 34}, {label: "Sprite table ($2104)", "#,
        r#"value: 4}, {label: "Work RAM ($2180)", value: 128}]}, {code: 11, "#,
        r#"description: "A colour written into CGRAM by the processor, a DMA or an "#,
        r#"HDMA", kind: "palette_write", label: "Palette write", param_label: "#,
        r#""Colour (0-255)", param_options: []}, {code: 12, description: "A write "#,
        r#"into VRAM changed a tile's bytes (the same bytes written again do not "#,
        r#"count)", kind: "tile_write", label: "Tile changed", param_label: "Tile "#,
        r#"(4bpp, 0-2047)", param_options: []}]"#,
    )];
    assert_eq!(shapes(&[emu.get_available_events()]), expected);
}

/// Running at first, paused by the execute breakpoint, paused after a step,
/// then each mode set by its name; a name the core does not know reads as
/// `Running`.
#[wasm_bindgen_test]
fn the_execution_mode_reads_as_before() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&debugger_program_rom()).unwrap();
    emu.add_breakpoint(0x00_8020, "execute".to_string(), None);
    let mut modes = vec![emu.get_execution_mode()];
    emu.run_frame();
    modes.push(emu.get_execution_mode());
    step_into(&mut emu);
    modes.push(emu.get_execution_mode());
    for mode in ["Running", "StepOut", "StepOver", "Unknown"] {
        emu.set_execution_mode(mode.to_string());
        modes.push(emu.get_execution_mode());
    }
    let modes: Vec<JsValue> = modes.iter().map(|mode| JsValue::from_str(mode)).collect();
    let expected = [
        r#""Running""#,
        r#""Paused""#,
        r#""Paused""#,
        r#""Running""#,
        r#""StepOut""#,
        r#""StepOver""#,
        r#""Running""#,
    ];
    assert_eq!(shapes(&modes), expected);
}

/// With no game, then with the program loaded (the ROM has a size). OAM
/// is named the sprite table and CGRAM the palette.
#[wasm_bindgen_test]
fn the_memory_regions_read_as_before() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    let mut regions = vec![emu.get_memory_regions()];
    emu.load_rom(&debugger_program_rom()).unwrap();
    regions.push(emu.get_memory_regions());
    let expected = [
        concat!(
            r#"[{name: "WRAM", region: "MainRam", size: 131072, start_address: 8257536,"#,
            r#" writable: true}, {name: "VRAM", region: "VideoRam", size: 65536, "#,
            r#"start_address: 0, writable: true}, {name: "OAM", region: "SpriteTable", "#,
            r#"size: 544, start_address: 0, writable: true}, {name: "CGRAM", region: "#,
            r#""PaletteRam", size: 512, start_address: 0, writable: true}, {name: "#,
            r#""ARAM", region: "AudioRam", size: 65536, start_address: 0, writable: "#,
            r#"true}, {name: "ROM", region: "Rom", size: 0, start_address: 0, "#,
            r#"writable: false}, {name: "SRAM", region: "SaveRam", size: 0, "#,
            r#"start_address: 0, writable: true}]"#,
        ),
        concat!(
            r#"[{name: "WRAM", region: "MainRam", size: 131072, start_address: 8257536,"#,
            r#" writable: true}, {name: "VRAM", region: "VideoRam", size: 65536, "#,
            r#"start_address: 0, writable: true}, {name: "OAM", region: "SpriteTable", "#,
            r#"size: 544, start_address: 0, writable: true}, {name: "CGRAM", region: "#,
            r#""PaletteRam", size: 512, start_address: 0, writable: true}, {name: "#,
            r#""ARAM", region: "AudioRam", size: 65536, start_address: 0, writable: "#,
            r#"true}, {name: "ROM", region: "Rom", size: 131072, start_address: 0, "#,
            r#"writable: false}, {name: "SRAM", region: "SaveRam", size: 0, "#,
            r#"start_address: 0, writable: true}]"#,
        ),
    ];
    assert_eq!(shapes(&regions), expected);
}

/// The two steps' records, the newest first; the newest alone; then the
/// sound CPU's and a cartridge CPU's, which keep none.
#[wasm_bindgen_test]
fn the_instruction_history_reads_as_before() {
    let mut emu = debugging_the_program();
    let histories = [
        emu.get_instruction_history(0, 16),
        emu.get_instruction_history(0, 1),
        emu.get_instruction_history(1, 16),
        emu.get_instruction_history(2, 16),
    ];
    let expected = [
        concat!(
            r#"[{address: 32802, cycle_count: 4, instruction: "sta $0010", "#,
            r#"registers_before: [{name: "A", value: 18}, {name: "X", value: 0}, "#,
            r#"{name: "Y", value: 0}, {name: "S", value: 504}, {name: "D", value: 0}, "#,
            r#"{name: "DB", value: 0}, {name: "PB", value: 0}, {name: "P", value: 52}, "#,
            r#"{name: "E", value: 1}]}, {address: 32800, cycle_count: 2, instruction: "#,
            r#""lda #$12", registers_before: [{name: "A", value: 0}, {name: "X", "#,
            r#"value: 0}, {name: "Y", value: 0}, {name: "S", value: 504}, {name: "D", "#,
            r#"value: 0}, {name: "DB", value: 0}, {name: "PB", value: 0}, {name: "P", "#,
            r#"value: 52}, {name: "E", value: 1}]}]"#,
        ),
        concat!(
            r#"[{address: 32802, cycle_count: 4, instruction: "sta $0010", "#,
            r#"registers_before: [{name: "A", value: 18}, {name: "X", value: 0}, "#,
            r#"{name: "Y", value: 0}, {name: "S", value: 504}, {name: "D", value: 0}, "#,
            r#"{name: "DB", value: 0}, {name: "PB", value: 0}, {name: "P", value: 52}, "#,
            r#"{name: "E", value: 1}]}]"#,
        ),
        "[]",
        "[]",
    ];
    assert_eq!(shapes(&histories), expected);
}

/// The two calls the program made before its two steps, the outermost
/// first; then the sound CPU's and a cartridge CPU's, which keep none.
#[wasm_bindgen_test]
fn the_call_stack_reads_as_before() {
    let mut emu = debugging_the_program();
    let stacks = [
        emu.get_call_stack(0),
        emu.get_call_stack(1),
        emu.get_call_stack(2),
    ];
    let expected = [
        concat!(
            "[{call_address: 32768, function_name: undefined, return_address: 32771},",
            " {call_address: 32784, function_name: undefined, return_address: 32788}]",
        ),
        "[]",
        "[]",
    ];
    assert_eq!(shapes(&stacks), expected);
}

/// The focus at first, then after asking for the sound CPU, for a cartridge
/// CPU the program does not have (left as it was), and for the main CPU.
#[wasm_bindgen_test]
fn the_focused_cpu_reads_as_before() {
    let mut emu = debugging_the_program();
    let mut focus = vec![number(emu.get_focused_cpu())];
    for cpu in [1, 2, 0] {
        emu.set_focused_cpu(cpu);
        focus.push(number(emu.get_focused_cpu()));
    }
    let expected = ["0", "1", "1", "0"];
    assert_eq!(shapes(&focus), expected);
}

/// The main CPU at `$8000`, `$8003`, `$8010`, `$8020`, `$8022` and `$8025`
/// (the program ran four of them), the sound CPU at its boot ROM's first
/// instruction, which it has not run yet, and a cartridge CPU the program
/// does not have; then the sound CPU at the same address after a frame in
/// which it ran its boot ROM.
#[wasm_bindgen_test]
fn the_coverage_reads_as_before() {
    let emu = debugging_the_program();
    let mut covered: Vec<JsValue> = [
        (0, 0x00_8000),
        (0, 0x00_8003),
        (0, 0x00_8010),
        (0, 0x00_8020),
        (0, 0x00_8022),
        (0, 0x00_8025),
        (1, 0xFFC0),
        (2, 0x00_8000),
    ]
    .iter()
    .map(|&(cpu, address)| JsValue::from_bool(emu.is_covered(cpu, address)))
    .collect();
    covered.push(JsValue::from_bool(spc700_traced().is_covered(1, 0xFFC0)));
    let expected = [
        "true", "false", "true", "true", "true", "false", "false", "false", "false",
    ];
    assert_eq!(shapes(&covered), expected);
}

/// The main CPU's four addresses, the sound CPU's none, and a cartridge CPU
/// the program does not have.
#[wasm_bindgen_test]
fn the_coverage_stats_read_as_before() {
    let emu = debugging_the_program();
    let stats = [
        emu.get_coverage_stats(0),
        emu.get_coverage_stats(1),
        emu.get_coverage_stats(2),
    ];
    let expected = [
        "{coveredBytes: 4, percent: 0.00002384185791015625, totalBytes: 16777216}",
        "{coveredBytes: 0, percent: 0, totalBytes: 65536}",
        "{coveredBytes: 0, percent: 0, totalBytes: 0}",
    ];
    assert_eq!(shapes(&stats), expected);
}

/// The flag after the program's four traced instructions, then on a
/// program that loops on `NOP`s with the main CPU's trace on: each
/// instruction is traced, and ten frames pass the trace's first
/// eight MiB.
#[wasm_bindgen_test]
fn the_trace_flush_flag_reads_as_before() {
    let emu = debugging_the_program();
    let mut flags = vec![JsValue::from_bool(emu.needs_trace_flush())];
    let nops: Vec<u8> = [0xEA; 124].into_iter().chain([0x80, 0x82]).collect();
    let mut looping = RetroEmulator::new("snes").unwrap();
    let rom = lorom_running("LOOPING", &nops, &[]); // 124 NOPs, BRA $8000
    looping.load_rom(&rom).unwrap();
    looping.set_tracing_enabled(true);
    for _ in 0..10 {
        looping.run_frame();
    }
    flags.push(JsValue::from_bool(looping.needs_trace_flush()));
    looping.clear_trace_flushed();
    flags.push(JsValue::from_bool(looping.needs_trace_flush()));
    let expected = ["false", "true", "false"];
    assert_eq!(shapes(&flags), expected);
}

/// Off at first, on once set (the sound CPU's stays off), off once cleared.
#[wasm_bindgen_test]
fn the_trace_switch_reads_as_before() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    let mut switch = vec![JsValue::from_bool(emu.is_tracing_enabled())];
    emu.set_tracing_enabled(true);
    switch.push(JsValue::from_bool(emu.is_tracing_enabled()));
    switch.push(JsValue::from_bool(emu.is_spc700_tracing_enabled()));
    emu.set_tracing_enabled(false);
    switch.push(JsValue::from_bool(emu.is_tracing_enabled()));
    let expected = ["false", "true", "false", "false"];
    assert_eq!(shapes(&switch), expected);
}

/// The chunk after the program's four traced instructions (the `JSR`
/// and the `JSL` on the instrumented loop, the load and the store on
/// the full loop), then what is left once it is cleared.
#[wasm_bindgen_test]
fn the_trace_flush_data_reads_as_before() {
    let mut emu = debugging_the_program();
    let mut data = chunk(&emu.get_trace_flush_data(), TRACE_ENTRY_SIZE_65816);
    emu.clear_trace_flushed();
    data.push(byte_array(&emu.get_trace_flush_data()));
    let expected = [
        "176",
        "Uint8Array(len 44, fnv1a64 0xc518fa71d9f9da00)",
        "Uint8Array(len 176, fnv1a64 0xe3be799a7b74468b)",
        "Uint8Array(len 0, fnv1a64 0xcbf29ce484222325)",
    ];
    assert_eq!(shapes(&data), expected);
}

/// The main CPU's entry size in bytes.
#[wasm_bindgen_test]
fn the_trace_entry_size_reads_as_before() {
    let emu = RetroEmulator::new("snes").unwrap();
    let expected = ["44"];
    assert_eq!(shapes(&[number(emu.get_trace_entry_size())]), expected);
}

/// The program's four entries as text, the same chunk with ten bytes
/// of a fifth entry after it, and nothing.
#[wasm_bindgen_test]
fn the_trace_text_reads_as_before() {
    let mut emu = debugging_the_program();
    let data = emu.get_trace_flush_data();
    let mut longer = data.clone();
    longer.extend_from_slice(&data[..10]);
    let texts: Vec<JsValue> = [&data[..], &longer[..], &[]]
        .iter()
        .map(|chunk| JsValue::from_str(&emu.convert_trace_chunk_to_text(chunk)))
        .collect();
    let expected = [
        concat!(
            r#""008000 jsr $8010      [008010] A:0000 X:0000 Y:0000 S:01fd D:0000 "#,
            r#"DB:00 ..1B.I.. V:  0 H:  0 F: 0\n008010 jsl $008020    [008020] A:0000 "#,
            r#"X:0000 Y:0000 S:01fb D:0000 DB:00 ..1B.I.. V:  0 H: 11 F: 0\n008020 lda "#,
            r#"#$12                A:0000 X:0000 Y:0000 S:01f8 D:0000 DB:00 ..1B.I.. "#,
            r#"V:  0 H: 27 F: 0\n008022 sta $0010      [000010] A:0012 X:0000 Y:0000 "#,
            r#"S:01f8 D:0000 DB:00 ..1B.I.. V:  0 H: 31 F: 0\n""#,
        ),
        concat!(
            r#""008000 jsr $8010      [008010] A:0000 X:0000 Y:0000 S:01fd D:0000 "#,
            r#"DB:00 ..1B.I.. V:  0 H:  0 F: 0\n008010 jsl $008020    [008020] A:0000 "#,
            r#"X:0000 Y:0000 S:01fb D:0000 DB:00 ..1B.I.. V:  0 H: 11 F: 0\n008020 lda "#,
            r#"#$12                A:0000 X:0000 Y:0000 S:01f8 D:0000 DB:00 ..1B.I.. "#,
            r#"V:  0 H: 27 F: 0\n008022 sta $0010      [000010] A:0012 X:0000 Y:0000 "#,
            r#"S:01f8 D:0000 DB:00 ..1B.I.. V:  0 H: 31 F: 0\n""#,
        ),
        r#""""#,
    ];
    assert_eq!(shapes(&texts), expected);
}

/// From in front of the `RTL`: `step()`, then `stepCpu` for the main
/// CPU, the sound CPU, a cartridge CPU the program does not have and
/// a CPU past the list.
#[wasm_bindgen_test]
fn a_step_returns_as_before() {
    let mut emu = debugging_the_program();
    let results = [
        emu.step(),
        emu.step_cpu(0),
        emu.step_cpu(1),
        emu.step_cpu(2),
        emu.step_cpu(7),
    ];
    let expected = [
        "{breakpoint_hit: false, cycles: 44, instruction_executed: true}",
        "{breakpoint_hit: false, cycles: 42, instruction_executed: true}",
        "{breakpoint_hit: false, cycles: 0, instruction_executed: false}",
        "{breakpoint_hit: false, cycles: 0, instruction_executed: false}",
        "{breakpoint_hit: false, cycles: 0, instruction_executed: false}",
    ];
    assert_eq!(shapes(&results), expected);
}

/// The program with only the sound CPU's trace on, run one frame:
/// the SPC700 runs its boot ROM.
fn spc700_traced() -> RetroEmulator {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&debugger_program_rom()).unwrap();
    emu.set_spc700_tracing_enabled(true);
    emu.run_frame();
    emu
}

/// Set, the sound CPU's switch reads on and the main CPU's stays off; then
/// cleared.
#[wasm_bindgen_test]
fn setting_the_spc700_trace_reads_as_before() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.set_spc700_tracing_enabled(true);
    let mut switch = vec![
        JsValue::from_bool(emu.is_spc700_tracing_enabled()),
        JsValue::from_bool(emu.is_tracing_enabled()),
    ];
    emu.set_spc700_tracing_enabled(false);
    switch.push(JsValue::from_bool(emu.is_spc700_tracing_enabled()));
    let expected = ["true", "false", "false"];
    assert_eq!(shapes(&switch), expected);
}

/// Off at first, and on in a run with it set.
#[wasm_bindgen_test]
fn the_spc700_trace_switch_reads_as_before() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    let switch = [
        JsValue::from_bool(emu.is_spc700_tracing_enabled()),
        JsValue::from_bool(spc700_traced().is_spc700_tracing_enabled()),
    ];
    let expected = ["false", "true"];
    assert_eq!(shapes(&switch), expected);
}

/// The sound CPU's chunk after one frame of its boot ROM, then the main
/// CPU's, empty: its trace is off.
#[wasm_bindgen_test]
fn the_spc700_trace_data_reads_as_before() {
    let mut emu = spc700_traced();
    let mut data = chunk(&emu.get_spc700_trace_flush_data(), TRACE_ENTRY_SIZE_SPC700);
    data.push(byte_array(&emu.get_trace_flush_data()));
    let expected = [
        "95376",
        "Uint8Array(len 24, fnv1a64 0x9587a08e0a5703c5)",
        "Uint8Array(len 95376, fnv1a64 0x8a6d5972165b0b8d)",
        "Uint8Array(len 0, fnv1a64 0xcbf29ce484222325)",
    ];
    assert_eq!(shapes(&data), expected);
}

/// Once cleared, the chunk is empty and the switch stays on.
#[wasm_bindgen_test]
fn clearing_the_spc700_trace_reads_as_before() {
    let mut emu = spc700_traced();
    emu.clear_spc700_trace_flushed();
    let data = [
        byte_array(&emu.get_spc700_trace_flush_data()),
        JsValue::from_bool(emu.is_spc700_tracing_enabled()),
    ];
    let expected = ["Uint8Array(len 0, fnv1a64 0xcbf29ce484222325)", "true"];
    assert_eq!(shapes(&data), expected);
}

/// The sound CPU's entry size in bytes.
#[wasm_bindgen_test]
fn the_spc700_trace_entry_size_reads_as_before() {
    let emu = RetroEmulator::new("snes").unwrap();
    let size = [number(emu.get_spc700_trace_entry_size())];
    let expected = ["24"];
    assert_eq!(shapes(&size), expected);
}

/// The boot ROM's first three entries as text, the same with ten
/// bytes of a fourth after them, and nothing.
#[wasm_bindgen_test]
fn the_spc700_trace_text_reads_as_before() {
    let mut emu = spc700_traced();
    let data = emu.get_spc700_trace_flush_data();
    let three = &data[..3 * TRACE_ENTRY_SIZE_SPC700];
    let longer = &data[..3 * TRACE_ENTRY_SIZE_SPC700 + 10];
    let texts: Vec<JsValue> = [three, longer, &[]]
        .iter()
        .map(|chunk| JsValue::from_str(&emu.convert_spc700_trace_chunk_to_text(chunk)))
        .collect();
    let expected = [
        concat!(
            r#""..ffc0 mov   x, #$ef           A:00 X:00 Y:00 SP:01ef YA:0000 "#,
            r#"......Z.\n..ffc2 mov   sp, x             A:00 X:ef Y:00 SP:01ef YA:0000 "#,
            r#"N.......\n..ffc3 mov   a, #$00           A:00 X:ef Y:00 SP:01ef YA:0000 "#,
            r#"N.......\n""#,
        ),
        concat!(
            r#""..ffc0 mov   x, #$ef           A:00 X:00 Y:00 SP:01ef YA:0000 "#,
            r#"......Z.\n..ffc2 mov   sp, x             A:00 X:ef Y:00 SP:01ef YA:0000 "#,
            r#"N.......\n..ffc3 mov   a, #$00           A:00 X:ef Y:00 SP:01ef YA:0000 "#,
            r#"N.......\n""#,
        ),
        r#""""#,
    ];
    assert_eq!(shapes(&texts), expected);
}

/// A Super FX cartridge whose main CPU masks the GSU's interrupt,
/// starts the GSU on `IWT R0,#$1234`, `IWT R1,#$5678`, `STOP` at
/// `$0100`, then waits in a loop.
#[cfg(feature = "chip-superfx")]
fn super_fx_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x10_0000];
    let start: [u8; 18] = [
        0xA9, 0x80, 0x8D, 0x37, 0x30, // LDA #$80, STA $3037: mask the interrupt
        0x58, // CLI
        0xA9, 0x00, 0x8D, 0x1E, 0x30, // LDA #$00, STA $301E: R15 low
        0xA9, 0x01, 0x8D, 0x1F, 0x30, // LDA #$01, STA $301F: R15 high, the start
        0x80, 0xFE, // BRA -2
    ];
    rom[..start.len()].copy_from_slice(&start);
    let kernel = [0xF0, 0x34, 0x12, 0xF1, 0x78, 0x56, 0x00];
    rom[0x0100..0x0100 + kernel.len()].copy_from_slice(&kernel);
    rom[0x7FC0..0x7FC0 + 21].copy_from_slice(b"SUPER FX TRACE       ");
    rom[0x7FD5] = 0x20; // LoROM
    rom[0x7FD6] = 0x13; // a Super FX cartridge
    rom[0x7FD7] = 0x0A; // 1 MiB
    rom[0x7FD9] = 0x01; // USA, NTSC
    rom[0x7FDC..0x7FDE].copy_from_slice(&(!0x1234u16).to_le_bytes());
    rom[0x7FDE..0x7FE0].copy_from_slice(&0x1234u16.to_le_bytes());
    rom[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    rom[0x7FFE..0x8000].copy_from_slice(&0x8000u16.to_le_bytes());
    rom
}

/// The Super FX cartridge with the cartridge CPU's trace on, run
/// three frames.
#[cfg(feature = "chip-superfx")]
fn super_fx_traced() -> RetroEmulator {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&super_fx_rom()).unwrap();
    emu.set_superfx_tracing_enabled(true);
    for _ in 0..3 {
        emu.run_frame();
    }
    emu
}

/// The switch on the program, which has no cartridge CPU: set, it stays
/// off under both names of the cartridge CPU's trace; then cleared.
#[wasm_bindgen_test]
fn setting_the_super_fx_trace_reads_as_before() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&debugger_program_rom()).unwrap();
    emu.set_superfx_tracing_enabled(true);
    let mut switch = vec![
        JsValue::from_bool(emu.is_superfx_tracing_enabled()),
        JsValue::from_bool(emu.is_hg51b_tracing_enabled()),
    ];
    emu.set_superfx_tracing_enabled(false);
    switch.push(JsValue::from_bool(emu.is_superfx_tracing_enabled()));
    let expected = ["false", "false", "false"];
    assert_eq!(shapes(&switch), expected);
}

/// Off at first.
#[wasm_bindgen_test]
fn the_super_fx_trace_switch_reads_as_before() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    let switch = [JsValue::from_bool(emu.is_superfx_tracing_enabled())];
    let expected = ["false"];
    assert_eq!(shapes(&switch), expected);
}

/// What the Super FX cartridge's GSU traced, then the same reader on
/// the program, which has no cartridge CPU, with the trace on.
#[cfg(feature = "chip-superfx")]
#[wasm_bindgen_test]
fn the_super_fx_trace_data_reads_as_before() {
    let mut emu = super_fx_traced();
    let mut data = chunk(&emu.get_superfx_trace_flush_data(), TRACE_ENTRY_SIZE_GSU);
    let mut plain = RetroEmulator::new("snes").unwrap();
    plain.load_rom(&debugger_program_rom()).unwrap();
    plain.set_superfx_tracing_enabled(true);
    plain.run_frame();
    data.push(byte_array(&plain.get_superfx_trace_flush_data()));
    let expected = [
        "208",
        "Uint8Array(len 52, fnv1a64 0xfa42a3a7c400d865)",
        "Uint8Array(len 208, fnv1a64 0x89b43a09edfd5a65)",
        "Uint8Array(len 0, fnv1a64 0xcbf29ce484222325)",
    ];
    assert_eq!(shapes(&data), expected);
}

/// Once cleared, the chunk is empty and the switch stays on.
#[cfg(feature = "chip-superfx")]
#[wasm_bindgen_test]
fn clearing_the_super_fx_trace_reads_as_before() {
    let mut emu = super_fx_traced();
    emu.clear_superfx_trace_flushed();
    let data = [
        byte_array(&emu.get_superfx_trace_flush_data()),
        JsValue::from_bool(emu.is_superfx_tracing_enabled()),
    ];
    let expected = ["Uint8Array(len 0, fnv1a64 0xcbf29ce484222325)", "true"];
    assert_eq!(shapes(&data), expected);
}

/// The Super FX's entry size in bytes.
#[wasm_bindgen_test]
fn the_super_fx_trace_entry_size_reads_as_before() {
    let emu = RetroEmulator::new("snes").unwrap();
    let size = [number(emu.get_superfx_trace_entry_size())];
    let expected = ["52"];
    assert_eq!(shapes(&size), expected);
}

/// Two entries built field by field (an `IWT` with only the running
/// flag set, a `STOP` with every status flag the line shows set) as
/// text, the same with ten bytes of a third after them, and nothing.
#[cfg(feature = "chip-superfx")]
#[wasm_bindgen_test]
fn the_super_fx_trace_text_reads_as_before() {
    let mut registers = [0u16; 16];
    registers[0] = 0x1234;
    registers[15] = 0x0104;
    let first = TraceEntryBinaryGsu {
        master_cycle: 0,
        pbr_pc: 0x00_0100,
        sfr: 0x0020,
        rombr: 0,
        rambr: 0,
        bytes_len: 3,
        bytes: [0xF0, 0x34, 0x12],
        r: registers,
    };
    let second = TraceEntryBinaryGsu {
        master_cycle: 0x0123_4567_89AB_CDEF,
        pbr_pc: 0x5F_FFFF,
        sfr: 0x9F7E,
        rombr: 0x7E,
        rambr: 1,
        bytes_len: 1,
        bytes: [0x00, 0x00, 0x00],
        r: std::array::from_fn(|i| (0x1000 + i * 0x111) as u16),
    };
    let mut data = first.to_bytes().to_vec();
    data.extend_from_slice(&second.to_bytes());
    let mut longer = data.clone();
    longer.extend_from_slice(&data[..10]);
    let emu = RetroEmulator::new("snes").unwrap();
    let texts: Vec<JsValue> = [&data[..], &longer[..], &[]]
        .iter()
        .map(|chunk| JsValue::from_str(&emu.convert_superfx_trace_chunk_to_text(chunk)))
        .collect();
    let expected = [
        concat!(
            r#""000100 iwt r0,#$1234        R0:1234 R1:0000 R2:0000 R3:0000 R4:0000 "#,
            r#"R5:0000 R6:0000 R7:0000 R8:0000 R9:0000 R10:0000 R11:0000 R12:0000 "#,
            r#"R13:0000 R14:0000 R15:0104 SFR:0020 .......g....\n5fffff stop           "#,
            r#"      R0:1000 R1:1111 R2:1222 R3:1333 R4:1444 R5:1555 R6:1666 R7:1777 "#,
            r#"R8:1888 R9:1999 R10:1aaa R11:1bbb R12:1ccc R13:1ddd R14:1eee R15:1fff "#,
            r#"SFR:9f7e ibhl21rgvscz\n""#,
        ),
        concat!(
            r#""000100 iwt r0,#$1234        R0:1234 R1:0000 R2:0000 R3:0000 R4:0000 "#,
            r#"R5:0000 R6:0000 R7:0000 R8:0000 R9:0000 R10:0000 R11:0000 R12:0000 "#,
            r#"R13:0000 R14:0000 R15:0104 SFR:0020 .......g....\n5fffff stop           "#,
            r#"      R0:1000 R1:1111 R2:1222 R3:1333 R4:1444 R5:1555 R6:1666 R7:1777 "#,
            r#"R8:1888 R9:1999 R10:1aaa R11:1bbb R12:1ccc R13:1ddd R14:1eee R15:1fff "#,
            r#"SFR:9f7e ibhl21rgvscz\n""#,
        ),
        r#""""#,
    ];
    assert_eq!(shapes(&texts), expected);
}

/// A Cx4 cartridge whose main CPU points the Cx4 at a program at
/// `$01:8000` (`LD A,#$12`, `ST RAMDATA,A`, `WRRAML`, `HALT`),
/// starts it, then waits in a loop.
#[cfg(feature = "chip-cx4")]
fn cx4_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x20_0000];
    let start: [u8; 20] = [
        0xA9, 0x00, 0x8D, 0x49, 0x7F, // LDA #$00, STA $7F49: program offset, low
        0xA9, 0x80, 0x8D, 0x4A, 0x7F, // LDA #$80, STA $7F4A: middle
        0xA9, 0x01, 0x8D, 0x4B, 0x7F, // LDA #$01, STA $7F4B: bank
        0x9C, 0x4F, 0x7F, // STZ $7F4F: start at the program's first word
        0x80, 0xFE, // BRA -2
    ];
    rom[..start.len()].copy_from_slice(&start);
    for (i, word) in [0x6412u16, 0xE00C, 0xEC00, 0xFC00].iter().enumerate() {
        rom[0x8000 + 2 * i..0x8000 + 2 * i + 2].copy_from_slice(&word.to_le_bytes());
    }
    let header = 0x7FB0;
    rom[header + 0x10..header + 0x25].copy_from_slice(b"CX4 TRACE            ");
    rom[header + 0x25] = 0x20; // LoROM
    rom[header + 0x26] = 0xF3; // a Cx4 cartridge
    rom[header + 0x27] = 0x0B; // 2 MiB
    rom[header + 0x29] = 0x01; // USA, NTSC
    rom[header + 0x2A] = 0x01;
    rom[header + 0x2C..header + 0x2E].copy_from_slice(&(!0x1234u16).to_le_bytes());
    rom[header + 0x2E..header + 0x30].copy_from_slice(&0x1234u16.to_le_bytes());
    rom[header + 0x4C..header + 0x4E].copy_from_slice(&0x8000u16.to_le_bytes());
    rom
}

/// The Cx4 cartridge with the cartridge CPU's trace on, run two
/// frames.
#[cfg(feature = "chip-cx4")]
fn cx4_traced() -> RetroEmulator {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&cx4_rom()).unwrap();
    emu.set_hg51b_tracing_enabled(true);
    for _ in 0..2 {
        emu.run_frame();
    }
    emu
}

/// The switch on the program, which has no cartridge CPU: set, it stays
/// off under both names of the cartridge CPU's trace; then cleared.
#[wasm_bindgen_test]
fn setting_the_hg51b_trace_reads_as_before() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&debugger_program_rom()).unwrap();
    emu.set_hg51b_tracing_enabled(true);
    let mut switch = vec![
        JsValue::from_bool(emu.is_hg51b_tracing_enabled()),
        JsValue::from_bool(emu.is_superfx_tracing_enabled()),
    ];
    emu.set_hg51b_tracing_enabled(false);
    switch.push(JsValue::from_bool(emu.is_hg51b_tracing_enabled()));
    let expected = ["false", "false", "false"];
    assert_eq!(shapes(&switch), expected);
}

/// Off at first.
#[wasm_bindgen_test]
fn the_hg51b_trace_switch_reads_as_before() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    let switch = [JsValue::from_bool(emu.is_hg51b_tracing_enabled())];
    let expected = ["false"];
    assert_eq!(shapes(&switch), expected);
}

/// What the Cx4 cartridge's chip traced, the chip running as it runs
/// untraced, then the same reader on the program, which has no
/// cartridge CPU, with the trace on.
#[cfg(feature = "chip-cx4")]
#[wasm_bindgen_test]
fn the_hg51b_trace_data_reads_as_before() {
    let mut emu = cx4_traced();
    let mut data = chunk(&emu.get_hg51b_trace_flush_data(), TRACE_ENTRY_SIZE_CX4);
    let mut plain = RetroEmulator::new("snes").unwrap();
    plain.load_rom(&debugger_program_rom()).unwrap();
    plain.set_hg51b_tracing_enabled(true);
    plain.run_frame();
    data.push(byte_array(&plain.get_hg51b_trace_flush_data()));
    let expected = [
        "372",
        "Uint8Array(len 93, fnv1a64 0xfd09fdf8879cc4b8)",
        "Uint8Array(len 372, fnv1a64 0x80484c1efe13ed24)",
        "Uint8Array(len 0, fnv1a64 0xcbf29ce484222325)",
    ];
    assert_eq!(shapes(&data), expected);
}

/// Once cleared, the chunk is empty and the switch stays on.
#[cfg(feature = "chip-cx4")]
#[wasm_bindgen_test]
fn clearing_the_hg51b_trace_reads_as_before() {
    let mut emu = cx4_traced();
    emu.clear_hg51b_trace_flushed();
    let data = [
        byte_array(&emu.get_hg51b_trace_flush_data()),
        JsValue::from_bool(emu.is_hg51b_tracing_enabled()),
    ];
    let expected = ["Uint8Array(len 0, fnv1a64 0xcbf29ce484222325)", "true"];
    assert_eq!(shapes(&data), expected);
}

/// The Cx4's entry size in bytes.
#[wasm_bindgen_test]
fn the_hg51b_trace_entry_size_reads_as_before() {
    let emu = RetroEmulator::new("snes").unwrap();
    let size = [number(emu.get_hg51b_trace_entry_size())];
    let expected = ["93"];
    assert_eq!(shapes(&size), expected);
}

/// Two entries built field by field (a load with every register
/// clear, a `HALT` with every flag set) as text, the same with ten
/// bytes of a third after them, and nothing.
#[cfg(feature = "chip-cx4")]
#[wasm_bindgen_test]
fn the_hg51b_trace_text_reads_as_before() {
    let first = TraceEntryBinaryCx4 {
        cycles: 0,
        pc: 0,
        opcode: 0x6412,
        p: 0,
        flags: 0,
        a: 0,
        acch: 0,
        accl: 0,
        gpr: [0; 16],
    };
    let second = TraceEntryBinaryCx4 {
        cycles: 0x00FF_FFFF_FFFF,
        pc: 0x7F_FFFF,
        opcode: 0xFC00,
        p: 0x7FFF,
        flags: 0xF,
        a: 0xFF_FFFF,
        acch: 0x12_3456,
        accl: 0x65_4321,
        gpr: std::array::from_fn(|i| 0xFF_FFFF - i as u32),
    };
    let mut data = first.to_bytes().to_vec();
    data.extend_from_slice(&second.to_bytes());
    let mut longer = data.clone();
    longer.extend_from_slice(&data[..10]);
    let emu = RetroEmulator::new("snes").unwrap();
    let texts: Vec<JsValue> = [&data[..], &longer[..], &[]]
        .iter()
        .map(|chunk| JsValue::from_str(&emu.convert_hg51b_trace_chunk_to_text(chunk)))
        .collect();
    let expected = [
        concat!(
            r#""000000 ld a,#$12            A:000000 P:0000 R0:000000 R1:000000 "#,
            r#"R2:000000 R3:000000 R4:000000 R5:000000 R6:000000 R7:000000 R8:000000 "#,
            r#"R9:000000 R10:000000 R11:000000 R12:000000 R13:000000 R14:000000 "#,
            r#"R15:000000 ....\n7fffff halt                 A:ffffff P:7fff R0:ffffff "#,
            r#"R1:fffffe R2:fffffd R3:fffffc R4:fffffb R5:fffffa R6:fffff9 R7:fffff8 "#,
            r#"R8:fffff7 R9:fffff6 R10:fffff5 R11:fffff4 R12:fffff3 R13:fffff2 "#,
            r#"R14:fffff1 R15:fffff0 nzvc\n""#,
        ),
        concat!(
            r#""000000 ld a,#$12            A:000000 P:0000 R0:000000 R1:000000 "#,
            r#"R2:000000 R3:000000 R4:000000 R5:000000 R6:000000 R7:000000 R8:000000 "#,
            r#"R9:000000 R10:000000 R11:000000 R12:000000 R13:000000 R14:000000 "#,
            r#"R15:000000 ....\n7fffff halt                 A:ffffff P:7fff R0:ffffff "#,
            r#"R1:fffffe R2:fffffd R3:fffffc R4:fffffb R5:fffffa R6:fffff9 R7:fffff8 "#,
            r#"R8:fffff7 R9:fffff6 R10:fffff5 R11:fffff4 R12:fffff3 R13:fffff2 "#,
            r#"R14:fffff1 R15:fffff0 nzvc\n""#,
        ),
        r#""""#,
    ];
    assert_eq!(shapes(&texts), expected);
}

/// The property `key` of `value`.
fn property(value: &JsValue, key: &str) -> JsValue {
    js_sys::Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

/// Each event's code with its kind, the name a host finds the event
/// by whatever the console numbers it.
#[wasm_bindgen_test]
fn every_event_names_its_kind() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    let kinds: Vec<(u32, String)> = js_sys::Array::from(&emu.get_available_events())
        .iter()
        .map(|event| {
            let code = property(&event, "code").as_f64().unwrap() as u32;
            (code, property(&event, "kind").as_string().unwrap())
        })
        .collect();
    let expected = [
        (1, "vertical_blank"),
        (2, "vertical_blank_interrupt"),
        (3, "raster_interrupt"),
        (4, "transfer_start"),
        (5, "line_transfer_set_up"),
        (6, "line"),
        (7, "video_register_write"),
        (8, "audio_port_write"),
        (9, "audio_register_write"),
        (10, "transfer_done"),
        (11, "palette_write"),
        (12, "tile_write"),
    ]
    .map(|(code, kind)| (code, kind.to_string()));
    assert_eq!(kinds, expected);
}

/// The history reads the newest record first: after the two steps,
/// the store, then the load; one step more puts the `RTL` first.
#[wasm_bindgen_test]
fn the_history_reads_newest_first() {
    let mut emu = debugging_the_program();
    let addresses = |emu: &mut RetroEmulator| -> Vec<u32> {
        js_sys::Array::from(&emu.get_instruction_history(0, 16))
            .iter()
            .map(|entry| property(&entry, "address").as_f64().unwrap() as u32)
            .collect()
    };
    assert_eq!(addresses(&mut emu), [0x00_8022, 0x00_8020]);
    step_into(&mut emu);
    assert_eq!(addresses(&mut emu), [0x00_8025, 0x00_8022, 0x00_8020]);
}

/// A stop on the instrumented loop refreshes the CPU states: the
/// main CPU reads where it stopped.
#[wasm_bindgen_test]
fn the_cpu_states_are_fresh_after_a_stop() {
    let mut emu = stopped_at_the_breakpoint();
    let states = js_sys::Array::from(&emu.get_cpu_states());
    let pc = property(&states.get(0), "pc").as_f64().unwrap() as u32;
    assert_eq!(pc, 0x00_8020);
}

/// OAM reads as the sprite table and CGRAM as the palette.
#[wasm_bindgen_test]
fn the_sprite_table_and_the_palette_are_named_by_kind() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    let regions: Vec<(String, String)> = js_sys::Array::from(&emu.get_memory_regions())
        .iter()
        .map(|region| {
            (
                property(&region, "name").as_string().unwrap(),
                property(&region, "region").as_string().unwrap_or_default(),
            )
        })
        .collect();
    assert!(regions.contains(&("OAM".to_string(), "SpriteTable".to_string())));
    assert!(regions.contains(&("CGRAM".to_string(), "PaletteRam".to_string())));
}

/// On the program, which has no cartridge CPU, the cartridge CPU's
/// trace switches stay off under both names, and a frame runs on
/// the fast loop.
#[wasm_bindgen_test]
fn a_cartridge_without_a_cpu_that_traces_keeps_its_switches_off() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&debugger_program_rom()).unwrap();
    emu.set_superfx_tracing_enabled(true);
    emu.set_hg51b_tracing_enabled(true);
    assert!(!emu.is_superfx_tracing_enabled());
    assert!(!emu.is_hg51b_tracing_enabled());
    emu.run_frame();
    let tier = emu.snes.debugger_ref().execution_tier();
    assert_eq!(tier, retro_core::ExecutionTier::Play);
}

/// On a Super FX cartridge the switch is on.
#[cfg(feature = "chip-superfx")]
#[wasm_bindgen_test]
fn a_super_fx_cartridge_takes_the_cartridge_cpu_trace() {
    let mut emu = super_fx_traced();
    assert!(emu.is_superfx_tracing_enabled());
}

/// A traced Cx4 runs as an untraced one: the machine after the two
/// frames is the same.
#[cfg(feature = "chip-cx4")]
#[wasm_bindgen_test]
fn tracing_the_hg51b_leaves_the_machine_as_it_runs_untraced() {
    let traced = cx4_traced();
    let mut untraced = RetroEmulator::new("snes").unwrap();
    untraced.load_rom(&cx4_rom()).unwrap();
    for _ in 0..2 {
        untraced.run_frame();
    }
    assert_eq!(traced.get_state_hash(), untraced.get_state_hash());
}

/// `step()` is `stepCpu` of the focused CPU: the same result, the
/// same machine, the same history; focused on the sound CPU, which
/// no host steps, it runs nothing.
#[wasm_bindgen_test]
fn a_step_is_a_step_of_the_focused_cpu() {
    let mut by_step = debugging_the_program();
    let mut by_cpu = debugging_the_program();
    let focused = by_cpu.get_focused_cpu();
    let results = [by_step.step(), by_cpu.step_cpu(focused)];
    assert_eq!(shapes(&results[..1]), shapes(&results[1..]));
    assert_eq!(by_step.get_state_hash(), by_cpu.get_state_hash());
    assert_eq!(
        shapes(&[by_step.get_instruction_history(0, 16)]),
        shapes(&[by_cpu.get_instruction_history(0, 16)])
    );
    by_step.set_focused_cpu(1);
    let idle = property(&by_step.step(), "instruction_executed");
    assert_eq!(idle.as_bool(), Some(false));
}

/// A run from a stop at an execute breakpoint goes on: the
/// instruction at the breakpoint runs, the program returns to its
/// loop, and the frame ends with the debugger still running.
#[wasm_bindgen_test]
fn a_run_goes_on_from_an_execute_breakpoint() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&debugger_program_rom()).unwrap();
    emu.add_breakpoint(0x00_8020, "execute".to_string(), None);
    emu.run_frame();
    assert_eq!(emu.get_execution_mode(), "Paused");
    emu.set_execution_mode("Running".to_string());
    emu.run_frame();
    assert_eq!(emu.get_execution_mode(), "Running");
}

/// A host's step keeps the history and the call stack: two steps
/// through the `JSR` and the `JSL` leave two records and two calls.
#[wasm_bindgen_test]
fn a_host_step_fills_the_history_and_the_call_stack() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&debugger_program_rom()).unwrap();
    emu.step_cpu(0);
    emu.step_cpu(0);
    let history = js_sys::Array::from(&emu.get_instruction_history(0, 16));
    let stack = js_sys::Array::from(&emu.get_call_stack(0));
    assert_eq!((history.length(), stack.length()), (2, 2));
}

/// The call stack is empty after a reset, a ROM load and a state
/// load; the history, which records what ran, stays.
#[wasm_bindgen_test]
fn the_call_stack_is_empty_after_a_reset_or_a_load() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&debugger_program_rom()).unwrap();
    let state = emu.save_state();
    let into_calls = |emu: &mut RetroEmulator| {
        emu.step_cpu(0);
        emu.step_cpu(0);
        assert_eq!(js_sys::Array::from(&emu.get_call_stack(0)).length(), 2);
    };
    let lengths = |emu: &mut RetroEmulator| {
        (
            js_sys::Array::from(&emu.get_call_stack(0)).length(),
            js_sys::Array::from(&emu.get_instruction_history(0, 64)).length(),
        )
    };
    into_calls(&mut emu);
    emu.reset();
    assert_eq!(lengths(&mut emu), (0, 2));
    into_calls(&mut emu);
    emu.load_rom(&debugger_program_rom()).unwrap();
    assert_eq!(lengths(&mut emu), (0, 4));
    into_calls(&mut emu);
    emu.load_state(&state).unwrap();
    assert_eq!(lengths(&mut emu), (0, 6));
}

/// What each CPU checks: the main CPU everything, the sound CPU and
/// the cartridge CPU nothing.
#[wasm_bindgen_test]
fn the_breakpoint_checks_read_what_each_cpu_checks() {
    let emu = RetroEmulator::new("snes").unwrap();
    let checks = [
        emu.get_breakpoint_checks(0),
        emu.get_breakpoint_checks(1),
        emu.get_breakpoint_checks(2),
        emu.get_breakpoint_checks(7),
    ];
    let expected = [
        "{conditions: true, execute: true, memory: true}",
        "{conditions: false, execute: false, memory: false}",
        "{conditions: false, execute: false, memory: false}",
        "{conditions: false, execute: false, memory: false}",
    ];
    assert_eq!(shapes(&checks), expected);
}

/// The main CPU's trace has no ring, so no capacity, until its
/// trace is first turned on.
#[wasm_bindgen_test]
fn the_trace_stats_read_no_capacity_until_traced() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&debugger_program_rom()).unwrap();
    let before = property(&emu.get_trace_stats(), "buffer_capacity");
    assert_eq!(before.as_f64(), Some(0.0));
    emu.set_tracing_enabled(true);
    emu.run_frame();
    let after = property(&emu.get_trace_stats(), "buffer_capacity");
    assert!(after.as_f64().unwrap() > 0.0);
}

/// Clearing a CPU's coverage forgets every address it ran.
#[wasm_bindgen_test]
fn clearing_the_coverage_forgets_it() {
    let mut emu = RetroEmulator::new("snes").unwrap();
    emu.load_rom(&debugger_program_rom()).unwrap();
    emu.step_cpu(0);
    emu.step_cpu(0);
    let covered = |emu: &RetroEmulator| {
        property(&emu.get_coverage_stats(0), "coveredBytes")
            .as_f64()
            .unwrap()
    };
    assert_eq!(covered(&emu), 2.0);
    emu.clear_coverage(0);
    assert_eq!(covered(&emu), 0.0);
}
