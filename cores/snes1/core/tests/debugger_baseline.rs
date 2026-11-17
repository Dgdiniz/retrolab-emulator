//! What the debugger reports today, held as literals, so a change to the
//! debugger shows exactly what moved: the event catalogue, the bytes and the
//! text line of each trace entry type, and the instruction history and call
//! stack that a program with a `JSR` and a `JSL` leaves on each path that
//! steps the 65C816.
//!
//! A literal that stops matching means a value a host reads, or a file it
//! stores, changed: record a new one only for a change that is meant.

use retro_core::debug::TraceEntry;
use retro_core::{
    Breakpoint, BreakpointType, EmulatorCore, ExecutionMode, InstructionTrace, StackFrame,
};
#[cfg(feature = "chip-cx4")]
use snes1_core::format_trace_line_cx4;
#[cfg(feature = "chip-superfx")]
use snes1_core::format_trace_line_gsu;
use snes1_core::{
    format_trace_line_65816, format_trace_line_spc700, SnesEmulator, TraceEntryBinary65816,
    TraceEntryBinaryCx4, TraceEntryBinaryGsu, TraceEntryBinarySpc700,
};

/// The bytes as lower-case hex, two digits a byte.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A 128 KiB LoROM that starts at `$00:8000`, with each `(address, bytes)`
/// of `blocks` at that address of bank `$00` and, when `nmi` names one, the
/// emulation-mode NMI vector pointing at it.
fn lorom(blocks: &[(u16, &[u8])], nmi: Option<u16>) -> Vec<u8> {
    let mut rom = vec![0u8; 0x2_0000];
    for (address, bytes) in blocks {
        let at = usize::from(address - 0x8000);
        rom[at..at + bytes.len()].copy_from_slice(bytes);
    }
    let header = 0x7FB0;
    rom[header + 0x10..header + 0x25].copy_from_slice(b"DEBUGGER BASELINE    ");
    rom[header + 0x25] = 0x20; // LoROM
    rom[header + 0x27] = 0x07; // 128 KiB
    rom[header + 0x29] = 0x01; // USA, NTSC
    rom[header + 0x2C..header + 0x2E].copy_from_slice(&(!0x1234u16).to_le_bytes());
    rom[header + 0x2E..header + 0x30].copy_from_slice(&0x1234u16.to_le_bytes());
    if let Some(handler) = nmi {
        rom[header + 0x4A..header + 0x4C].copy_from_slice(&handler.to_le_bytes());
    }
    rom[header + 0x4C..header + 0x4E].copy_from_slice(&0x8000u16.to_le_bytes());
    rom
}

/// A call and its return, nested: `JSR` to a routine that makes a `JSL`
/// to another, which returns with `RTL`, then `RTS`, then the program waits
/// in a loop. `$8030` holds an `RTI`, the NMI handler when a test gives the
/// program one.
const CALLS: &[(u16, &[u8])] = &[
    (0x8000, &[0x20, 0x10, 0x80]),       // JSR $8010
    (0x8003, &[0x80, 0xFE]),             // BRA $8003
    (0x8010, &[0x22, 0x20, 0x80, 0x00]), // JSL $00:8020
    (0x8014, &[0x60]),                   // RTS
    (0x8020, &[0xEA]),                   // NOP
    (0x8021, &[0x6B]),                   // RTL
    (0x8030, &[0x40]),                   // RTI
];

/// The emulator with the call program loaded and nothing run.
fn calls_loaded(nmi: Option<u16>) -> SnesEmulator {
    let mut emu = SnesEmulator::new();
    emu.load_rom(&lorom(CALLS, nmi)).expect("the program loads");
    emu
}

/// One 65C816 instruction on the full loop: the step mode, then a frame
/// call, which runs one instruction and pauses.
fn step_into(emu: &mut SnesEmulator) {
    emu.debugger().set_execution_mode(ExecutionMode::StepInto);
    emu.run_frame();
}

/// `call->return`, both 24-bit, and the name when the frame has one.
fn frame_text(frame: &StackFrame) -> String {
    let mut text = format!("{:06X}->{:06X}", frame.call_address, frame.return_address);
    if let Some(name) = &frame.function_name {
        text.push(' ');
        text.push_str(name);
    }
    text
}

/// The 65C816's call stack, in the order the debugger returns it.
fn call_stack(emu: &mut SnesEmulator) -> String {
    let frames: Vec<String> = emu
        .debugger()
        .get_call_stack(0)
        .iter()
        .map(frame_text)
        .collect();
    format!("[{}]", frames.join(", "))
}

/// One history record: its address, its text, its cycle count and the
/// registers before it.
fn history_text(entry: &InstructionTrace) -> String {
    let registers: Vec<String> = entry
        .registers_before
        .iter()
        .map(|register| format!("{}={:X}", register.name, register.value))
        .collect();
    format!(
        "{:06X} {} ({} cycles) {}",
        entry.address,
        entry.instruction,
        entry.cycle_count,
        registers.join(" ")
    )
}

/// The 65C816's address as `PBR:PC`.
fn pc24(emu: &SnesEmulator) -> u32 {
    (u32::from(emu.cpu().pbr) << 16) | u32::from(emu.cpu().pc)
}

/// The twelve event classes, in the order the debugger lists them, as JSON:
/// each code with its kind (the name a host finds the event by, whatever
/// the console), its label, its description, the label of its param (none
/// for the five that take none) and its param's options (the four APU ports,
/// and the four ports a DMA fills).
#[test]
fn the_event_catalogue_reads_as_recorded() {
    let mut emu = SnesEmulator::new();
    let catalogue = serde_json::to_string(&emu.debugger().available_events()).unwrap();
    assert_eq!(
        catalogue,
        concat!(
            r#"[{"code":1,"kind":"vertical_blank","label":"VBlank","description":"Entering vertical blank","#,
            r#""param_label":null,"param_options":[]},{"code":2,"kind":"vertical_blank_interrupt","label":"NMI","#,
            r#""description":"V-blank NMI delivered to the 65C816","param_label":null,"#,
            r#""param_options":[]},{"code":3,"kind":"raster_interrupt","label":"IRQ","#,
            r#""description":"H/V-timer IRQ raised","param_label":null,"param_options":[]},"#,
            r#"{"code":4,"kind":"transfer_start","label":"DMA start","#,
            r#""description":"General-purpose DMA block started ($420B)","#,
            r#""param_label":null,"param_options":[]},{"code":5,"kind":"line_transfer_set_up","label":"HDMA start","#,
            r#""description":"Top-of-frame HDMA initialisation","param_label":null,"#,
            r#""param_options":[]},{"code":6,"kind":"line","label":"Scanline","#,
            r#""description":"A rendered scanline matched the V counter","#,
            r#""param_label":"Scanline (V)","param_options":[]},{"code":7,"kind":"video_register_write","#,
            r#""label":"PPU register write","#,
            r#""description":"Write to a PPU register ($2100-$213F)","#,
            r#""param_label":"Register ($00-$3F)","param_options":[]},{"code":8,"kind":"audio_port_write","#,
            r#""label":"APU port write","#,
            r#""description":"65C816 write to an APU mailbox port ($2140-$2143)","#,
            r#""param_label":"Port","param_options":[{"value":0,"label":"Port 0"},"#,
            r#"{"value":1,"label":"Port 1"},{"value":2,"label":"Port 2"},{"value":3,"#,
            r#""label":"Port 3"}]},{"code":9,"kind":"audio_register_write","label":"S-DSP register write","#,
            r#""description":"SPC700 write to an S-DSP register","#,
            r#""param_label":"DSP register ($00-$7F)","param_options":[]},{"code":10,"kind":"transfer_done","#,
            r#""label":"DMA done","#,
            r#""description":"A general DMA channel finished its block, by the B-bus port "#,
            r#"it wrote","#,
            r#""param_label":"B-bus port","param_options":[{"value":24,"#,
            r#""label":"VRAM ($2118)"},{"value":34,"label":"Palette ($2122)"},{"value":4,"#,
            r#""label":"Sprite table ($2104)"},{"value":128,"label":"Work RAM ($2180)"}]},"#,
            r#"{"code":11,"kind":"palette_write","label":"Palette write","#,
            r#""description":"A colour written into CGRAM by the processor, a DMA or an "#,
            r#"HDMA","#,
            r#""param_label":"Colour (0-255)","param_options":[]},{"code":12,"kind":"tile_write","#,
            r#""label":"Tile changed","#,
            r#""description":"A write into VRAM changed a tile's bytes (the same bytes "#,
            r#"written again do not count)","#,
            r#""param_label":"Tile (4bpp, 0-2047)","param_options":[]}]"#,
        )
    );
}

/// Three 65C816 entries: a `JSR` in emulation mode, a long indexed load in
/// native mode with sixteen-bit registers, and an `RTL` in native mode with
/// eight-bit registers whose three bytes past the opcode are set: the bytes
/// keep them and the line leaves them out.
#[test]
fn the_65816_trace_entries_read_as_recorded() {
    let entries = [
        TraceEntryBinary65816 {
            master_cycle: 1_234_567,
            pbr_pc: 0x00_8000,
            a: 0x0012,
            x: 0x0034,
            y: 0x0056,
            s: 0x01FF,
            d: 0x0000,
            vcounter: 12,
            hdot: 123,
            framecounter: 7,
            eff_addr: 0x00_8010,
            dbr: 0x00,
            p: 0x34,
            e: 1,
            eff_present: 1,
            bytes_len: 3,
            cycles: 6,
            bytes: [0x20, 0x10, 0x80, 0x00],
        },
        TraceEntryBinary65816 {
            master_cycle: 0x0123_4567_89AB_CDEF,
            pbr_pc: 0x01_8123,
            a: 0xBEEF,
            x: 0x0010,
            y: 0xFFFF,
            s: 0x1FF0,
            d: 0x0300,
            vcounter: 261,
            hdot: 339,
            framecounter: 123_456,
            eff_addr: 0x7E_2010,
            dbr: 0x7E,
            p: 0xC3,
            e: 0,
            eff_present: 1,
            bytes_len: 4,
            cycles: 5,
            bytes: [0xBF, 0x00, 0x20, 0x7E],
        },
        TraceEntryBinary65816 {
            master_cycle: 89_478_485,
            pbr_pc: 0xC0_FFFF,
            a: 0x00FF,
            x: 0x0001,
            y: 0x0002,
            s: 0x0100,
            d: 0x2100,
            vcounter: 225,
            hdot: 0,
            framecounter: 59,
            eff_addr: 0,
            dbr: 0x80,
            p: 0x30,
            e: 0,
            eff_present: 0,
            bytes_len: 1,
            cycles: 6,
            bytes: [0x6B, 0xAA, 0xBB, 0xCC],
        },
    ];
    let mut seen = Vec::new();
    for entry in &entries {
        let bytes = entry.to_bytes();
        assert_eq!(TraceEntryBinary65816::from_bytes(&bytes), Some(*entry));
        seen.push(hex(&bytes));
        seen.push(format_trace_line_65816(entry));
    }
    assert_eq!(
        seen,
        [
            concat!(
                "87d612000000000000800000120034005600ff0100000c007b00070000001080000000340101",
                "030620108000",
            ),
            concat!(
                "008000 jsr $8010      [008010] A:0012 X:0034 Y:0056 S:01ff D:0000 DB:00 ",
                "..1B.I.. V: 12 H:123 F: 7",
            ),
            concat!(
                "efcdab896745230123810100efbe1000fffff01f00030501530140e2010010207e007ec30001",
                "0405bf00207e",
            ),
            concat!(
                "018123 lda $7e2000,x  [7e2010] A:beef X:0010 Y:ffff S:1ff0 D:0300 DB:7e ",
                "NV....ZC V:261 H:339 F:123456",
            ),
            concat!(
                "5555550500000000ffffc000ff000100020000010021e10000003b0000000000000080300000",
                "01066baabbcc",
            ),
            concat!(
                "c0ffff rtl                     A:00ff X:0001 Y:0002 S:0100 D:2100 DB:80 ",
                "..MX.... V:225 H:  0 F:59",
            ),
        ]
    );
}

/// Three SPC700 entries: the first instruction of the boot ROM, a `CALL`
/// with three flags set, and a direct-page indexed load that the
/// direct-page flag moves to page 1, with a third byte set that the bytes
/// keep and the line leaves out.
#[test]
fn the_spc700_trace_entries_read_as_recorded() {
    let entries = [
        TraceEntryBinarySpc700 {
            master_cycle: 0,
            pc: 0xFFC0,
            a: 0x00,
            x: 0x00,
            y: 0x00,
            sp: 0xEF,
            psw: 0x02,
            bytes_len: 2,
            bytes: [0xCD, 0xEF, 0x00],
        },
        TraceEntryBinarySpc700 {
            master_cycle: 0xFEDC_BA98_7654_3210,
            pc: 0x0A00,
            a: 0x80,
            x: 0x01,
            y: 0xFF,
            sp: 0xCF,
            psw: 0xA1,
            bytes_len: 3,
            bytes: [0x3F, 0x34, 0x12],
        },
        TraceEntryBinarySpc700 {
            master_cycle: 1_024_000,
            pc: 0x1234,
            a: 0x42,
            x: 0x10,
            y: 0x20,
            sp: 0xFF,
            psw: 0x20,
            bytes_len: 2,
            bytes: [0xF4, 0x12, 0x99],
        },
    ];
    let mut seen = Vec::new();
    for entry in &entries {
        let bytes = entry.to_bytes();
        assert_eq!(TraceEntryBinarySpc700::from_bytes(&bytes), Some(*entry));
        seen.push(hex(&bytes));
        seen.push(format_trace_line_spc700(entry));
    }
    assert_eq!(
        seen,
        [
            "0000000000000000c0ff000000ef0202cdef000000000000",
            "..ffc0 mov   x, #$ef           A:00 X:00 Y:00 SP:01ef YA:0000 ......Z.",
            "1032547698badcfe000a8001ffcfa1033f34120000000000",
            "..0a00 call  $1234             A:80 X:01 Y:ff SP:01cf YA:ff80 N.P....C",
            "00a00f00000000003412421020ff2002f412990000000000",
            "..1234 mov   a, $112+x         A:42 X:10 Y:20 SP:01ff YA:2042 ..P.....",
        ]
    );
}

/// Three Super FX entries: an `IWT` with only the running flag set, the
/// opcode `$50` with the `ALT1` prefix flag set (read as `ADC`), and a `STOP`
/// with every status flag the line shows set, its bank registers and every
/// register non-zero.
fn super_fx_entries() -> [TraceEntryBinaryGsu; 3] {
    let mut first = [0u16; 16];
    first[0] = 0x1234;
    first[15] = 0x0104;
    let mut second = [0u16; 16];
    second[0] = 0x00FF;
    second[1] = 0x0001;
    second[15] = 0x8001;
    let third: [u16; 16] = std::array::from_fn(|i| (0x1000 + i * 0x111) as u16);
    [
        TraceEntryBinaryGsu {
            master_cycle: 0,
            pbr_pc: 0x00_0100,
            sfr: 0x0020,
            rombr: 0,
            rambr: 0,
            bytes_len: 3,
            bytes: [0xF0, 0x34, 0x12],
            r: first,
        },
        TraceEntryBinaryGsu {
            master_cycle: 21_477_272,
            pbr_pc: 0x01_8000,
            sfr: 0x0120,
            rombr: 0x14,
            rambr: 0,
            bytes_len: 1,
            bytes: [0x50, 0x00, 0x00],
            r: second,
        },
        TraceEntryBinaryGsu {
            master_cycle: 0x0123_4567_89AB_CDEF,
            pbr_pc: 0x5F_FFFF,
            sfr: 0x9F7E,
            rombr: 0x7E,
            rambr: 1,
            bytes_len: 1,
            bytes: [0x00, 0x00, 0x00],
            r: third,
        },
    ]
}

#[test]
fn the_super_fx_trace_entries_read_as_recorded() {
    let mut seen = Vec::new();
    for entry in &super_fx_entries() {
        let bytes = entry.to_bytes();
        assert_eq!(TraceEntryBinaryGsu::from_bytes(&bytes), Some(*entry));
        seen.push(hex(&bytes));
    }
    assert_eq!(
        seen,
        [
            concat!(
                "0000000000000000000100002000000003f03412341200000000000000000000000000000000",
                "0000000000000000000000000401",
            ),
            concat!(
                "98b7470100000000008001002001140001500000ff0001000000000000000000000000000000",
                "0000000000000000000000000180",
            ),
            concat!(
                "efcdab8967452301ffff5f007e9f7e0101000000001011112212331344145515661677178818",
                "9919aa1abb1bcc1cdd1dee1eff1f",
            ),
        ]
    );
}

#[cfg(feature = "chip-superfx")]
#[test]
fn the_super_fx_trace_lines_read_as_recorded() {
    let lines: Vec<String> = super_fx_entries()
        .iter()
        .map(format_trace_line_gsu)
        .collect();
    assert_eq!(
        lines,
        [
            concat!(
                "000100 iwt r0,#$1234        R0:1234 R1:0000 R2:0000 R3:0000 R4:0000 R5:0000 ",
                "R6:0000 R7:0000 R8:0000 R9:0000 R10:0000 R11:0000 R12:0000 R13:0000 ",
                "R14:0000 R15:0104 SFR:0020 .......g....",
            ),
            concat!(
                "018000 adc r0               R0:00ff R1:0001 R2:0000 R3:0000 R4:0000 R5:0000 ",
                "R6:0000 R7:0000 R8:0000 R9:0000 R10:0000 R11:0000 R12:0000 R13:0000 ",
                "R14:0000 R15:8001 SFR:0120 .....1.g....",
            ),
            concat!(
                "5fffff stop                 R0:1000 R1:1111 R2:1222 R3:1333 R4:1444 R5:1555 ",
                "R6:1666 R7:1777 R8:1888 R9:1999 R10:1aaa R11:1bbb R12:1ccc R13:1ddd ",
                "R14:1eee R15:1fff SFR:9f7e ibhl21rgvscz",
            ),
        ]
    );
}

/// Three Cx4 entries: a load of the accumulator with every register clear, a
/// store with the negative and carry flags set and the registers and the
/// page non-zero, and a `HALT` with both halves of the product set.
fn cx4_entries() -> [TraceEntryBinaryCx4; 3] {
    let second: [u32; 16] = std::array::from_fn(|i| (i as u32) * 0x01_0101);
    let third: [u32; 16] = std::array::from_fn(|i| 0xFF_FFFF - i as u32);
    [
        TraceEntryBinaryCx4 {
            cycles: 0,
            pc: 0,
            opcode: 0x6412,
            p: 0,
            flags: 0,
            a: 0,
            acch: 0,
            accl: 0,
            gpr: [0; 16],
        },
        TraceEntryBinaryCx4 {
            cycles: 20_000_000,
            pc: 0x00_0001,
            opcode: 0xE00C,
            p: 0x1234,
            flags: 0x9,
            a: 0x00_0012,
            acch: 0,
            accl: 0,
            gpr: second,
        },
        TraceEntryBinaryCx4 {
            cycles: 0x00FF_FFFF_FFFF,
            pc: 0x7F_FFFF,
            opcode: 0xFC00,
            p: 0x7FFF,
            flags: 0xF,
            a: 0xFF_FFFF,
            acch: 0x12_3456,
            accl: 0x65_4321,
            gpr: third,
        },
    ]
}

#[test]
fn the_cx4_trace_entries_read_as_recorded() {
    let mut seen = Vec::new();
    for entry in &cx4_entries() {
        let bytes = entry.to_bytes();
        assert_eq!(TraceEntryBinaryCx4::from_bytes(&bytes), Some(*entry));
        seen.push(hex(&bytes));
    }
    assert_eq!(
        seen,
        [
            concat!(
                "0000000000000000000000001264000000000000000000000000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000000000000000",
                "0000000000000000000000000000000000",
            ),
            concat!(
                "002d310100000000010000000ce0341209120000000000000000000000000000000101010002",
                "020200030303000404040005050500060606000707070008080800090909000a0a0a000b0b0b",
                "000c0c0c000d0d0d000e0e0e000f0f0f00",
            ),
            concat!(
                "ffffffffff000000ffff7f0000fcff7f0fffffff005634120021436500ffffff00feffff00fd",
                "ffff00fcffff00fbffff00faffff00f9ffff00f8ffff00f7ffff00f6ffff00f5ffff00f4ffff",
                "00f3ffff00f2ffff00f1ffff00f0ffff00",
            ),
        ]
    );
}

#[cfg(feature = "chip-cx4")]
#[test]
fn the_cx4_trace_lines_read_as_recorded() {
    let lines: Vec<String> = cx4_entries().iter().map(format_trace_line_cx4).collect();
    assert_eq!(
        lines,
        [
            concat!(
                "000000 ld a,#$12            A:000000 P:0000 R0:000000 R1:000000 R2:000000 ",
                "R3:000000 R4:000000 R5:000000 R6:000000 R7:000000 R8:000000 R9:000000 ",
                "R10:000000 R11:000000 R12:000000 R13:000000 R14:000000 R15:000000 ....",
            ),
            concat!(
                "000001 st ramdata,a         A:000012 P:1234 R0:000000 R1:010101 R2:020202 ",
                "R3:030303 R4:040404 R5:050505 R6:060606 R7:070707 R8:080808 R9:090909 ",
                "R10:0a0a0a R11:0b0b0b R12:0c0c0c R13:0d0d0d R14:0e0e0e R15:0f0f0f n..c",
            ),
            concat!(
                "7fffff halt                 A:ffffff P:7fff R0:ffffff R1:fffffe R2:fffffd ",
                "R3:fffffc R4:fffffb R5:fffffa R6:fffff9 R7:fffff8 R8:fffff7 R9:fffff6 ",
                "R10:fffff5 R11:fffff4 R12:fffff3 R13:fffff2 R14:fffff1 R15:fffff0 nzvc",
            ),
        ]
    );
}

/// The call program stepped on the full loop, one instruction a frame call.
/// After each step: the address the step started from and the call stack,
/// outermost frame first. Then the whole history and the last three records
/// of it, as the debugger returns them, the newest first.
#[test]
fn a_call_and_its_return_on_the_full_loop_read_as_recorded() {
    let mut emu = calls_loaded(None);
    let mut seen = Vec::new();
    for _ in 0..6 {
        let from = pc24(&emu);
        step_into(&mut emu);
        seen.push(format!("{from:06X} {}", call_stack(&mut emu)));
    }
    for entry in emu.debugger().get_instruction_history(0, 16) {
        seen.push(history_text(&entry));
    }
    for entry in emu.debugger().get_instruction_history(0, 3) {
        seen.push(history_text(&entry));
    }
    assert_eq!(
        seen,
        [
            "008000 [008000->008003]",
            "008010 [008000->008003, 008010->008014]",
            "008020 [008000->008003, 008010->008014]",
            "008021 [008000->008003]",
            "008014 []",
            "008003 []",
            "008003 bra $8003 (3 cycles) A=0 X=0 Y=0 S=1FD D=0 DB=0 PB=0 P=34 E=1",
            "008014 rts (6 cycles) A=0 X=0 Y=0 S=1FB D=0 DB=0 PB=0 P=34 E=1",
            "008021 rtl (6 cycles) A=0 X=0 Y=0 S=1F8 D=0 DB=0 PB=0 P=34 E=1",
            "008020 nop (2 cycles) A=0 X=0 Y=0 S=1F8 D=0 DB=0 PB=0 P=34 E=1",
            "008010 jsl $008020 (8 cycles) A=0 X=0 Y=0 S=1FB D=0 DB=0 PB=0 P=34 E=1",
            "008000 jsr $8010 (6 cycles) A=0 X=0 Y=0 S=1FD D=0 DB=0 PB=0 P=34 E=1",
            "008003 bra $8003 (3 cycles) A=0 X=0 Y=0 S=1FD D=0 DB=0 PB=0 P=34 E=1",
            "008014 rts (6 cycles) A=0 X=0 Y=0 S=1FB D=0 DB=0 PB=0 P=34 E=1",
            "008021 rtl (6 cycles) A=0 X=0 Y=0 S=1F8 D=0 DB=0 PB=0 P=34 E=1",
        ]
    );
}

/// The call program on the instrumented loop: an execute breakpoint inside
/// the routine the `JSL` calls stops the frame there. The call stack holds
/// the two calls made so far, as the full loop's does after the same two
/// instructions, and the history holds nothing: this loop keeps no history.
/// A reset empties the call stack, whose calls belonged to the machine it
/// replaced; the history, which records what ran, stays.
#[test]
fn a_call_on_the_instrumented_loop_reads_as_recorded() {
    let mut emu = calls_loaded(None);
    emu.debugger().add_breakpoint(Breakpoint {
        address: 0x00_8020,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    });
    emu.run_frame();
    let mut seen = vec![
        format!(
            "{:?} at {:06X}",
            emu.debugger().get_execution_mode(),
            pc24(&emu)
        ),
        call_stack(&mut emu),
        format!(
            "history {}",
            emu.debugger().get_instruction_history(0, 16).len()
        ),
    ];
    emu.reset();
    seen.push(call_stack(&mut emu));
    seen.push(format!(
        "history {}",
        emu.debugger().get_instruction_history(0, 16).len()
    ));
    assert_eq!(
        seen,
        [
            "Paused at 008020",
            "[008000->008003, 008010->008014]",
            "history 0",
            "[]",
            "history 0",
        ]
    );
}

/// The call program stepped with `step_cpu(0)`, the step a host makes for
/// one instruction of the main CPU: after each, the address it started from,
/// the call stack and how many history records there are. This step keeps
/// both, as the full loop's steps do: each call pushes its frame and each
/// return pops it, and each step adds its instruction to the history.
#[test]
fn a_call_stepped_one_instruction_at_a_time_reads_as_recorded() {
    let mut emu = calls_loaded(None);
    let mut seen = Vec::new();
    for _ in 0..6 {
        let from = pc24(&emu);
        emu.step_cpu(0);
        seen.push(format!(
            "{from:06X} {} history {}",
            call_stack(&mut emu),
            emu.debugger().get_instruction_history(0, 16).len()
        ));
    }
    assert_eq!(
        seen,
        [
            "008000 [008000->008003] history 1",
            "008010 [008000->008003, 008010->008014] history 2",
            "008020 [008000->008003, 008010->008014] history 3",
            "008021 [008000->008003] history 4",
            "008014 [] history 5",
            "008003 [] history 6",
        ]
    );
}

/// The call program on the full loop with an NMI made pending three times:
/// before the `JSR`, before the `JSL` and before the `NOP` inside the
/// routine the `JSL` calls. The handler is one `RTI`. After each step: the
/// address the step started from, whether an NMI was pending, and the call
/// stack. Then the whole history, address and text, the newest first.
///
/// The step that takes the NMI runs no instruction: it pushes a frame whose
/// call and return addresses are both the interrupted address, which the
/// handler's `RTI` pops. The call it preempted runs after the `RTI`, and
/// only then pushes its frame, once. Every return pops its own call's frame.
/// The history records the instructions that ran, so a call the NMI
/// preempted is in it once, and the step that took the NMI is in it not at
/// all.
#[test]
fn a_call_an_interrupt_meets_reads_as_recorded() {
    let mut emu = calls_loaded(Some(0x8030));
    let mut seen = Vec::new();
    for step in 0..12 {
        let interrupted = matches!(step, 0 | 3 | 6);
        if interrupted {
            emu.cpu_mut().nmi_pending = true;
        }
        let from = pc24(&emu);
        step_into(&mut emu);
        let pending = if interrupted { " NMI" } else { "" };
        seen.push(format!("{from:06X}{pending} {}", call_stack(&mut emu)));
    }
    for entry in emu.debugger().get_instruction_history(0, 16) {
        seen.push(format!("{:06X} {}", entry.address, entry.instruction));
    }
    assert_eq!(
        seen,
        [
            "008000 NMI [008000->008000]",
            "008030 []",
            "008000 [008000->008003]",
            "008010 NMI [008000->008003, 008010->008010]",
            "008030 [008000->008003]",
            "008010 [008000->008003, 008010->008014]",
            "008020 NMI [008000->008003, 008010->008014, 008020->008020]",
            "008030 [008000->008003, 008010->008014]",
            "008020 [008000->008003, 008010->008014]",
            "008021 [008000->008003]",
            "008014 []",
            "008003 []",
            "008003 bra $8003",
            "008014 rts",
            "008021 rtl",
            "008020 nop",
            "008030 rti",
            "008010 jsl $008020",
            "008030 rti",
            "008000 jsr $8010",
            "008030 rti",
        ]
    );
}
