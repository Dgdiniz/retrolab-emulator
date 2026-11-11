//! SNES debugger + bsnes-plus 65C816 trace tests.
//!
//! The headline tests are the **byte-exact** ones in [`format_tests`]:
//! they build a [`TraceEntryBinary65816`] from the literal register /
//! instruction values of the first lines of bsnes-plus traces of the Super
//! Mario World (SMW) and Chrono Trigger boots, and assert that
//! [`format_trace_line_65816`] reproduces those lines character-for-character.
//! If this drifts, the trace-diff CLI can no longer compare our output
//! against bsnes-plus byte-for-byte.

use std::collections::HashMap;

use retro_core::cpu::Addressable;
use retro_core::{
    AudioInfo, Breakpoint, BreakpointType, CpuArchitecture, CpuInfo, CpuLocation, DebuggerCore,
    DisplayInfo, EmulatorCore, ExecutionMode, ExecutionTier, InputInfo, InstructionTrace,
    MatchSpec, PixelFormat, StackFrame, SystemInfo,
};
use wdc65816::addressing::AddressingMode;

use snes1_core::{
    effective_address, format_trace_line_65816, format_trace_line_spc700, EffRegs, SnesDebugger,
    SnesEmulator, TraceEntryBinary65816, TraceEntryBinarySpc700, TRACE_ENTRY_SIZE_65816,
    TRACE_ENTRY_SIZE_SPC700,
};
#[cfg(feature = "chip-superfx")]
use snes1_core::{format_trace_line_gsu, TraceEntryBinaryGsu, TRACE_ENTRY_SIZE_GSU};

/// Minimal two-CPU `SystemInfo` for constructing a standalone
/// [`SnesDebugger`] in unit tests.
fn test_sysinfo() -> SystemInfo {
    SystemInfo {
        name: "Test".to_string(),
        short_name: "T".to_string(),
        manufacturer: "T".to_string(),
        year: 1990,
        cpus: vec![
            CpuInfo {
                id: 0,
                name: "65C816".to_string(),
                architecture: CpuArchitecture::Mos65816,
                clock_speed: 3_580_000,
                registers: Vec::new(),
                location: CpuLocation::Motherboard,
                address_width: 24,
            },
            CpuInfo {
                id: 1,
                name: "SPC700".to_string(),
                architecture: CpuArchitecture::Spc700,
                clock_speed: 1_024_000,
                registers: Vec::new(),
                location: CpuLocation::Motherboard,
                address_width: 16,
            },
        ],
        display: DisplayInfo {
            width: 256,
            height: 224,
            aspect_ratio: 8.0 / 7.0,
            refresh_rate: 60.0,
            pixel_format: PixelFormat::RGBA8888,
        },
        audio: AudioInfo {
            sample_rate: 44_100,
            channels: 2,
            bits_per_sample: 32,
        },
        input: InputInfo {
            max_players: 2,
            button_layout: Vec::new(),
        },
    }
}

mod format_tests {
    use super::*;

    /// Implied addressing (no operand, no `[effective]` bracket) in
    /// emulation mode, SMW reference line 1.
    #[test]
    fn implied_emulation_sei() {
        let e = TraceEntryBinary65816 {
            master_cycle: 0,
            pbr_pc: 0x00_8000,
            a: 0x0280,
            x: 0x000a,
            y: 0x0001,
            s: 0x01f1,
            d: 0x0000,
            vcounter: 0,
            hdot: 46,
            framecounter: 0,
            eff_addr: 0,
            dbr: 0x00,
            p: 0x34,
            e: 1,
            eff_present: 0,
            bytes_len: 1,
            cycles: 0,
            bytes: [0x78, 0, 0, 0],
        };
        assert_eq!(
            format_trace_line_65816(&e),
            "008000 sei                     A:0280 X:000a Y:0001 S:01f1 D:0000 DB:00 ..1B.I.. V:  0 H: 46 F: 0"
        );
    }

    /// Absolute addressing with an `[effective]` bracket, SMW line 2.
    #[test]
    fn absolute_with_effective_bracket_stz() {
        let e = TraceEntryBinary65816 {
            master_cycle: 0,
            pbr_pc: 0x00_8001,
            a: 0x0280,
            x: 0x000a,
            y: 0x0001,
            s: 0x01f1,
            d: 0x0000,
            vcounter: 0,
            hdot: 50,
            framecounter: 0,
            eff_addr: 0x00_4200,
            dbr: 0x00,
            p: 0x34,
            e: 1,
            eff_present: 1,
            bytes_len: 3,
            cycles: 0,
            bytes: [0x9c, 0x00, 0x42, 0],
        };
        assert_eq!(
            format_trace_line_65816(&e),
            "008001 stz $4200      [004200] A:0280 X:000a Y:0001 S:01f1 D:0000 DB:00 ..1B.I.. V:  0 H: 50 F: 0"
        );
    }

    /// Immediate addressing (8-bit accumulator, no bracket), SMW line 9.
    #[test]
    fn immediate_no_bracket_lda() {
        let e = TraceEntryBinary65816 {
            master_cycle: 0,
            pbr_pc: 0x00_8016,
            a: 0x0280,
            x: 0x000a,
            y: 0x0001,
            s: 0x01f1,
            d: 0x0000,
            vcounter: 0,
            hdot: 102,
            framecounter: 0,
            eff_addr: 0,
            dbr: 0x00,
            p: 0x34,
            e: 1,
            eff_present: 0,
            bytes_len: 2,
            cycles: 0,
            bytes: [0xa9, 0x80, 0, 0],
        };
        assert_eq!(
            format_trace_line_65816(&e),
            "008016 lda #$80                A:0280 X:000a Y:0001 S:01f1 D:0000 DB:00 ..1B.I.. V:  0 H:102 F: 0"
        );
    }

    /// 24-bit long jump with bracket, native mode (`..MX.I.C`),
    /// Chrono Trigger reference line 4.
    #[test]
    fn long_jump_native_flags_jml() {
        let e = TraceEntryBinary65816 {
            master_cycle: 0,
            pbr_pc: 0x00_ff03,
            a: 0x001d,
            x: 0x0022,
            y: 0x0052,
            s: 0x01ed,
            d: 0x0000,
            vcounter: 0,
            hdot: 57,
            framecounter: 0,
            eff_addr: 0xfd_c000,
            dbr: 0x00,
            p: 0x35,
            e: 0,
            eff_present: 1,
            bytes_len: 4,
            cycles: 0,
            bytes: [0x5c, 0x00, 0xc0, 0xfd],
        };
        assert_eq!(
            format_trace_line_65816(&e),
            "00ff03 jml $fdc000    [fdc000] A:001d X:0022 Y:0052 S:01ed D:0000 DB:00 ..MX.I.C V:  0 H: 57 F: 0"
        );
    }

    /// Native mode with N + C set and a wide H column (3 digits),
    /// SMW reference line 19 (`sta $7f8000`).
    #[test]
    fn native_n_c_flags_sta_long() {
        let e = TraceEntryBinary65816 {
            master_cycle: 0,
            pbr_pc: 0x00_802a,
            a: 0xf0a9,
            x: 0x000a,
            y: 0x0001,
            s: 0x01ff,
            d: 0x0000,
            vcounter: 0,
            hdot: 161,
            framecounter: 0,
            eff_addr: 0x7f_8000,
            dbr: 0x00,
            p: 0x85,
            e: 0,
            eff_present: 1,
            bytes_len: 4,
            cycles: 0,
            bytes: [0x8f, 0x00, 0x80, 0x7f],
        };
        assert_eq!(
            format_trace_line_65816(&e),
            "00802a sta $7f8000    [7f8000] A:f0a9 X:000a Y:0001 S:01ff D:0000 DB:00 N....I.C V:  0 H:161 F: 0"
        );
    }

    /// Non-zero frame counter widens the `F` field to 2 digits, matching
    /// the bsnes `%2d` format.
    #[test]
    fn frame_counter_two_digits() {
        let e = TraceEntryBinary65816 {
            master_cycle: 0,
            pbr_pc: 0x00_8000,
            a: 0,
            x: 0,
            y: 0,
            s: 0x01ff,
            d: 0,
            vcounter: 12,
            hdot: 5,
            framecounter: 42,
            eff_addr: 0,
            dbr: 0,
            p: 0x34,
            e: 1,
            eff_present: 0,
            bytes_len: 1,
            cycles: 0,
            bytes: [0x78, 0, 0, 0],
        };
        let line = format_trace_line_65816(&e);
        assert!(line.ends_with("V: 12 H:  5 F:42"), "got: {line}");
    }

    #[test]
    fn entry_round_trips_through_bytes() {
        let e = TraceEntryBinary65816 {
            master_cycle: 0x0123_4567_89ab_cdef,
            pbr_pc: 0x12_3456,
            a: 0x1234,
            x: 0x5678,
            y: 0x9abc,
            s: 0x01ff,
            d: 0x4000,
            vcounter: 200,
            hdot: 339,
            framecounter: 7,
            eff_addr: 0x7e_1234,
            dbr: 0x7e,
            p: 0xa5,
            e: 0,
            eff_present: 1,
            bytes_len: 3,
            cycles: 5,
            bytes: [0xad, 0x34, 0x12, 0],
        };
        let bytes = e.to_bytes();
        assert_eq!(bytes.len(), TRACE_ENTRY_SIZE_65816);
        let back = TraceEntryBinary65816::from_bytes(&bytes).unwrap();
        assert_eq!(back, e);
        // Short buffers are rejected.
        assert!(TraceEntryBinary65816::from_bytes(&bytes[..TRACE_ENTRY_SIZE_65816 - 1]).is_none());
    }
}

/// Byte-exact SPC700 trace-line tests, built from the literal register /
/// instruction values of the first lines of a bsnes-plus SPC700 trace of
/// the SMW boot (the deterministic IPL boot). If
/// [`format_trace_line_spc700`] drifts from these, the SPC700 trace can
/// no longer be diffed against bsnes-plus.
mod format_tests_spc700 {
    use super::*;

    /// Immediate `mov x, #$ef`, IPL boot line 1 (Z set at reset).
    #[test]
    fn immediate_mov_x() {
        let e = TraceEntryBinarySpc700 {
            master_cycle: 0,
            pc: 0xffc0,
            a: 0x00,
            x: 0x00,
            y: 0x00,
            sp: 0xef,
            psw: 0x02, // ......Z.
            bytes_len: 2,
            bytes: [0xcd, 0xef, 0],
        };
        assert_eq!(
            format_trace_line_spc700(&e),
            "..ffc0 mov   x, #$ef           A:00 X:00 Y:00 SP:01ef YA:0000 ......Z."
        );
    }

    /// Register-to-register `mov sp, x`, IPL boot line 2 (N set).
    #[test]
    fn implied_mov_sp_x() {
        let e = TraceEntryBinarySpc700 {
            master_cycle: 0,
            pc: 0xffc2,
            a: 0x00,
            x: 0xef,
            y: 0x00,
            sp: 0xef,
            psw: 0x80, // N.......
            bytes_len: 1,
            bytes: [0xbd, 0, 0],
        };
        assert_eq!(
            format_trace_line_spc700(&e),
            "..ffc2 mov   sp, x             A:00 X:ef Y:00 SP:01ef YA:0000 N......."
        );
    }

    /// Indirect store `mov (x), a`, IPL boot line 4.
    #[test]
    fn indirect_store_mov() {
        let e = TraceEntryBinarySpc700 {
            master_cycle: 0,
            pc: 0xffc5,
            a: 0x00,
            x: 0xef,
            y: 0x00,
            sp: 0xef,
            psw: 0x02,
            bytes_len: 1,
            bytes: [0xc6, 0, 0],
        };
        assert_eq!(
            format_trace_line_spc700(&e),
            "..ffc5 mov   (x), a            A:00 X:ef Y:00 SP:01ef YA:0000 ......Z."
        );
    }

    /// Relative branch `bne $ffc5`, IPL boot line 6; the target is
    /// resolved from `pc + 2 + (i8)offset`.
    #[test]
    fn relative_branch_bne() {
        let e = TraceEntryBinarySpc700 {
            master_cycle: 0,
            pc: 0xffc7,
            a: 0x00,
            x: 0xee,
            y: 0x00,
            sp: 0xef,
            psw: 0x80,
            bytes_len: 2,
            bytes: [0xd0, 0xfc, 0], // offset -4 -> $ffc5
        };
        assert_eq!(
            format_trace_line_spc700(&e),
            "..ffc7 bne   $ffc5             A:00 X:ee Y:00 SP:01ef YA:0000 N......."
        );
    }

    /// `YA` is the packed `Y:A` pair and every flag bit maps to its
    /// `NVPBHIZC` column.
    #[test]
    fn ya_pair_and_all_flags() {
        let e = TraceEntryBinarySpc700 {
            master_cycle: 0,
            pc: 0x0500,
            a: 0x34,
            x: 0x11,
            y: 0x12,
            sp: 0xcc,
            psw: 0xff, // NVPBHIZC all set
            bytes_len: 1,
            bytes: [0x00, 0, 0], // nop
        };
        let line = format_trace_line_spc700(&e);
        assert!(line.starts_with("..0500 nop  "), "got: {line}");
        assert!(
            line.contains(" A:34 X:11 Y:12 SP:01cc YA:1234 "),
            "got: {line}"
        );
        assert!(line.ends_with("NVPBHIZC"), "got: {line}");
    }

    #[test]
    fn entry_round_trips_through_bytes() {
        let e = TraceEntryBinarySpc700 {
            master_cycle: 0x0123_4567_89ab_cdef,
            pc: 0x1234,
            a: 0xaa,
            x: 0xbb,
            y: 0xcc,
            sp: 0xdd,
            psw: 0xa5,
            bytes_len: 3,
            bytes: [0x8f, 0x77, 0x20],
        };
        let bytes = e.to_bytes();
        assert_eq!(bytes.len(), TRACE_ENTRY_SIZE_SPC700);
        let back = TraceEntryBinarySpc700::from_bytes(&bytes).unwrap();
        assert_eq!(back, e);
        // Short buffers are rejected.
        assert!(
            TraceEntryBinarySpc700::from_bytes(&bytes[..TRACE_ENTRY_SIZE_SPC700 - 1]).is_none()
        );
    }
}

/// Super FX GSU trace-line tests. There is no bsnes-plus GSU trace to diff
/// against, so these pin our own layout (address / disasm / the 16-register
/// file / SFR + status flags) so [`format_trace_line_gsu`] and the entry
/// (de)serialization stay in lockstep with the `TRACE_ENTRY_SIZE_GSU` = 52
/// contract shared with `debugger-core`'s `SUPERFX` `TraceCpuSpec`.
#[cfg(feature = "chip-superfx")]
mod format_tests_gsu {
    use super::*;

    /// `iwt r0,#$1234`: the immediate-load kernel opcode the synthetic Super
    /// FX carts start with. `pbr_pc` is the 24-bit `PBR:(R15-1)` fetch address.
    #[test]
    fn immediate_iwt_line_layout() {
        let mut r = [0u16; 16];
        r[0] = 0x1234;
        r[15] = 0x0104;
        let e = TraceEntryBinaryGsu {
            master_cycle: 0,
            pbr_pc: 0x00_0100,
            sfr: 0x0020, // G (running)
            rombr: 0,
            rambr: 0,
            bytes_len: 3,
            bytes: [0xf0, 0x34, 0x12], // iwt r0,#$1234
            r,
        };
        let line = format_trace_line_gsu(&e);
        assert!(line.starts_with("000100 "), "24-bit address prefix: {line}");
        assert!(line.contains("iwt"), "decoded mnemonic: {line}");
        // The full register file is printed, R0..R15.
        assert!(line.contains("R0:1234 "), "R0 column: {line}");
        assert!(line.contains("R15:0104 "), "R15 column: {line}");
        assert!(line.contains("SFR:0020 "), "SFR column: {line}");
        // Only the `G` (running) flag lights in the 12-char status field
        // (`ibhl21rgvscz`, high->low): 7 dots, `g`, 4 dots.
        assert!(line.ends_with(".......g...."), "G flag only: {line}");
    }

    /// Every SFR status bit maps to its column in the `fmt_flags_gsu` field
    /// (`i b h l 2 1 r g v s c z`, high->low), exercised via a `stop` line.
    #[test]
    fn all_status_flags_render() {
        let e = TraceEntryBinaryGsu {
            master_cycle: 0,
            pbr_pc: 0x00_8000,
            // IRQ|B|IH|IL|ALT2|ALT1|R|G|OV|S|CY|Z all set.
            sfr: 0x8000
                | 0x1000
                | 0x0800
                | 0x0400
                | 0x0200
                | 0x0100
                | 0x0040
                | 0x0020
                | 0x0010
                | 0x0008
                | 0x0004
                | 0x0002,
            rombr: 0,
            rambr: 0,
            bytes_len: 1,
            bytes: [0x00, 0, 0], // stop
            r: [0u16; 16],
        };
        let line = format_trace_line_gsu(&e);
        assert!(line.ends_with("ibhl21rgvscz"), "all flags set: {line}");
    }

    #[test]
    fn entry_round_trips_through_bytes() {
        let r: [u16; 16] = std::array::from_fn(|i| (0x1000 + i * 0x111) as u16);
        let e = TraceEntryBinaryGsu {
            master_cycle: 0x0123_4567_89ab_cdef,
            pbr_pc: 0x0012_3456,
            sfr: 0xa5c3,
            rombr: 0x7e,
            rambr: 1,
            bytes_len: 3,
            bytes: [0xf0, 0x34, 0x12],
            r,
        };
        let bytes = e.to_bytes();
        assert_eq!(bytes.len(), TRACE_ENTRY_SIZE_GSU);
        let back = TraceEntryBinaryGsu::from_bytes(&bytes).unwrap();
        assert_eq!(back, e);
        // Short buffers are rejected.
        assert!(TraceEntryBinaryGsu::from_bytes(&bytes[..TRACE_ENTRY_SIZE_GSU - 1]).is_none());
    }
}

mod effective_address_tests {
    use super::*;

    /// Bus that returns 0 for every unset address (and lets indirect
    /// pointers be pre-seeded).
    struct FakeBus {
        mem: HashMap<u32, u8>,
    }

    impl FakeBus {
        fn new() -> Self {
            Self {
                mem: HashMap::new(),
            }
        }
        fn with(pairs: &[(u32, u8)]) -> Self {
            let mut b = Self::new();
            for &(a, v) in pairs {
                b.mem.insert(a & 0x00FF_FFFF, v);
            }
            b
        }
    }

    impl Addressable for FakeBus {
        fn read(&mut self, addr: u32) -> u8 {
            *self.mem.get(&(addr & 0x00FF_FFFF)).unwrap_or(&0)
        }
        fn write(&mut self, addr: u32, value: u8) {
            self.mem.insert(addr & 0x00FF_FFFF, value);
        }
    }

    fn regs() -> EffRegs {
        EffRegs {
            d: 0,
            x: 0,
            y: 0,
            s: 0,
            db: 0,
            pbr_pc: 0,
        }
    }

    #[test]
    fn implied_and_immediate_have_no_bracket() {
        let mut bus = FakeBus::new();
        assert_eq!(
            effective_address(&mut bus, AddressingMode::Implied, &[0xea], &regs()),
            None
        );
        assert_eq!(
            effective_address(&mut bus, AddressingMode::Immediate, &[0xa9, 0x12], &regs()),
            None
        );
        assert_eq!(
            effective_address(
                &mut bus,
                AddressingMode::BlockMove,
                &[0x54, 0x00, 0x7e],
                &regs()
            ),
            None
        );
    }

    #[test]
    fn direct_adds_direct_page() {
        let r = EffRegs {
            d: 0x1200,
            ..regs()
        };
        let mut bus = FakeBus::new();
        assert_eq!(
            effective_address(&mut bus, AddressingMode::Direct, &[0xa5, 0x34], &r),
            Some(0x00_1234)
        );
    }

    #[test]
    fn absolute_data_uses_data_bank() {
        let r = EffRegs { db: 0x7e, ..regs() };
        let mut bus = FakeBus::new();
        assert_eq!(
            effective_address(&mut bus, AddressingMode::Absolute, &[0xad, 0x34, 0x12], &r),
            Some(0x7e_1234)
        );
    }

    #[test]
    fn absolute_jmp_uses_program_bank_not_data_bank() {
        // JMP $1234 from bank $12 with DB=$7e must resolve to $121234
        // (program bank), not $7e1234 (data bank).
        let r = EffRegs {
            db: 0x7e,
            pbr_pc: 0x12_abcd,
            ..regs()
        };
        let mut bus = FakeBus::new();
        assert_eq!(
            effective_address(&mut bus, AddressingMode::Absolute, &[0x4c, 0x34, 0x12], &r),
            Some(0x12_1234)
        );
    }

    #[test]
    fn absolute_long_is_the_operand() {
        let mut bus = FakeBus::new();
        assert_eq!(
            effective_address(
                &mut bus,
                AddressingMode::AbsoluteLong,
                &[0x8f, 0x00, 0x80, 0x7f],
                &regs()
            ),
            Some(0x7f_8000)
        );
    }

    #[test]
    fn direct_indirect_dereferences_pointer_in_bank0() {
        // (dp) at D=0, operand $10 -> pointer at $0010/$0011 = $8000,
        // banked through DB=$7e -> $7e8000.
        let r = EffRegs { db: 0x7e, ..regs() };
        let mut bus = FakeBus::with(&[(0x0010, 0x00), (0x0011, 0x80)]);
        assert_eq!(
            effective_address(&mut bus, AddressingMode::DirectIndirect, &[0xb2, 0x10], &r),
            Some(0x7e_8000)
        );
    }

    #[test]
    fn direct_indirect_long_reads_24_bit_pointer() {
        // [dp] at D=0, operand $20 -> 24-bit pointer at $0020.. = $7f1234.
        let mut bus = FakeBus::with(&[(0x0020, 0x34), (0x0021, 0x12), (0x0022, 0x7f)]);
        assert_eq!(
            effective_address(
                &mut bus,
                AddressingMode::DirectIndirectLong,
                &[0xa7, 0x20],
                &regs()
            ),
            Some(0x7f_1234)
        );
    }

    #[test]
    fn relative_branch_targets_within_bank() {
        // BRA *-2 at $00:8010: PC after operand is $8012, +(-2) = $8010.
        let r = EffRegs {
            pbr_pc: 0x00_8010,
            ..regs()
        };
        let mut bus = FakeBus::new();
        assert_eq!(
            effective_address(&mut bus, AddressingMode::Relative, &[0x80, 0xfe], &r),
            Some(0x00_8010)
        );
    }

    #[test]
    fn relative_branch_keeps_program_bank() {
        // A forward branch from bank $12 stays in bank $12.
        let r = EffRegs {
            pbr_pc: 0x12_8000,
            ..regs()
        };
        let mut bus = FakeBus::new();
        assert_eq!(
            effective_address(&mut bus, AddressingMode::Relative, &[0x80, 0x10], &r),
            Some(0x12_8012)
        );
    }

    /// One state for the whole table below: every register at a value
    /// that makes a wrong term show, an instruction two bytes from the
    /// end of its bank, and the three pointers the indirect modes read.
    fn table_state() -> (FakeBus, EffRegs) {
        let bus = FakeBus::with(&[
            // (dp) / [dp] at D + $F4 = $2074.
            (0x2074, 0x34),
            (0x2075, 0xe5),
            (0x2076, 0x96),
            // (dp,x) at D + X + $F4 = $2197.
            (0x2197, 0x67),
            (0x2198, 0x18),
            // (sr,s),y at S + $F4 = $02E4.
            (0x02e4, 0xa4),
            (0x02e5, 0x55),
        ]);
        let regs = EffRegs {
            d: 0x1f80,
            x: 0x0123,
            y: 0xfedc,
            s: 0x01f0,
            db: 0x7e,
            pbr_pc: 0xc0_fffe,
        };
        (bus, regs)
    }

    #[test]
    fn the_bracket_of_every_addressing_mode() {
        use AddressingMode as Mode;
        // (opcode, mode, bracket) with the operand bytes F4 FF 7F.
        let table: &[(u8, AddressingMode, Option<u32>)] = &[
            (0xea, Mode::Implied, None),
            (0x0a, Mode::Accumulator, None),
            (0xa9, Mode::Immediate, None),
            (0xa2, Mode::ImmediateX, None),
            (0xc2, Mode::ImmediateByte, None),
            (0x54, Mode::BlockMove, None),
            // D + offset (+ index), a 16-bit sum in bank 0.
            (0xa5, Mode::Direct, Some(0x00_2074)),
            (0xb5, Mode::DirectX, Some(0x00_2197)),
            (0xb6, Mode::DirectY, Some(0x00_1f50)),
            // The pointer found there, in the data bank (+ Y after).
            (0xb2, Mode::DirectIndirect, Some(0x7e_e534)),
            (0xa1, Mode::DirectXIndirect, Some(0x7e_1867)),
            (0xb1, Mode::DirectIndirectY, Some(0x7f_e410)),
            // A 24-bit pointer is the whole address (+ Y after).
            (0xa7, Mode::DirectIndirectLong, Some(0x96_e534)),
            (0xb7, Mode::DirectIndirectLongY, Some(0x97_e410)),
            // S + offset; the pointer there, in the data bank, + Y.
            (0xa3, Mode::StackRelative, Some(0x00_02e4)),
            (0xb3, Mode::StackRelativeIndirectY, Some(0x7f_5480)),
            // A data address: the data bank, the index added to all 24 bits.
            (0xad, Mode::Absolute, Some(0x7e_fff4)),
            (0xbd, Mode::AbsoluteX, Some(0x7f_0117)),
            (0xb9, Mode::AbsoluteY, Some(0x7f_fed0)),
            (0xaf, Mode::AbsoluteLong, Some(0x7f_fff4)),
            (0xbf, Mode::AbsoluteLongX, Some(0x80_0117)),
            // A jump's address: the program bank.
            (0x4c, Mode::Absolute, Some(0xc0_fff4)),
            (0x20, Mode::Absolute, Some(0xc0_fff4)),
            // The indirect jumps show where the pointer is.
            (0x6c, Mode::AbsoluteIndirect, Some(0xc0_fff4)),
            (0x7c, Mode::AbsoluteXIndirect, Some(0xc0_0117)),
            (0xfc, Mode::AbsoluteXIndirect, Some(0xc0_0117)),
            (0xdc, Mode::AbsoluteIndirectLong, Some(0x00_fff4)),
            // A branch: the next instruction (which wraps inside the bank)
            // plus the distance (which does not).
            (0x80, Mode::Relative, Some(0xbf_fff4)),
            (0x82, Mode::RelativeLong, Some(0xbf_fff5)),
        ];
        let (mut bus, regs) = table_state();
        for &(opcode, mode, bracket) in table {
            assert_eq!(
                wdc65816::decode::OPCODE_TABLE[usize::from(opcode)].mode,
                mode,
                "the table names opcode {opcode:02x} with the wrong mode"
            );
            assert_eq!(
                effective_address(&mut bus, mode, &[opcode, 0xf4, 0xff, 0x7f], &regs),
                bracket,
                "opcode {opcode:02x} ({mode:?})"
            );
        }
    }

    #[test]
    fn the_bracket_of_all_256_opcodes_is_the_recorded_one() {
        // Recorded from the implementation the reference logs were checked
        // with; a change here is a change to every trace line.
        let (mut bus, regs) = table_state();
        let mut hash: u32 = 0x811c_9dc5;
        let mut with_bracket = 0;
        for opcode in 0..=255u8 {
            let mode = wdc65816::decode::OPCODE_TABLE[usize::from(opcode)].mode;
            let bracket = effective_address(&mut bus, mode, &[opcode, 0xf4, 0xff, 0x7f], &regs);
            with_bracket += usize::from(bracket.is_some());
            for byte in bracket.unwrap_or(0xffff_ffff).to_le_bytes() {
                hash ^= u32::from(byte);
                hash = hash.wrapping_mul(0x0100_0193);
            }
        }
        assert_eq!((with_bracket, hash), (187, 0xe620_a157));
    }

    #[test]
    fn a_short_slice_reads_the_missing_operand_bytes_as_zero() {
        let (mut bus, regs) = table_state();
        assert_eq!(
            effective_address(&mut bus, AddressingMode::AbsoluteLong, &[0xaf, 0xf4], &regs),
            Some(0x00_00f4)
        );
        assert_eq!(
            effective_address(&mut bus, AddressingMode::Absolute, &[], &regs),
            Some(0x7e_0000)
        );
    }
}

mod debugger_state_tests {
    use super::*;

    fn exec_bp(addr: u32) -> Breakpoint {
        Breakpoint {
            address: addr,
            bp_type: BreakpointType::execute(),
            cpu_id: Some(0),
            condition: None,
        }
    }

    #[test]
    fn coverage_records_executed_addresses() {
        let mut d = SnesDebugger::new(test_sysinfo());
        assert_eq!(d.coverage_count_65816(), 0);
        d.record_coverage_65816(0x00_8000);
        d.record_coverage_65816(0x00_8000); // idempotent
        d.record_coverage_65816(0x00_8001);
        assert!(d.is_covered_65816(0x00_8000));
        assert!(d.is_covered_65816(0x00_8001));
        assert!(!d.is_covered_65816(0x00_8002));
        assert_eq!(d.coverage_count_65816(), 2);
    }

    #[test]
    fn execute_breakpoint_matches_pc() {
        let mut d = SnesDebugger::new(test_sysinfo());
        let id = d.add_breakpoint(exec_bp(0x00_8000));
        assert_eq!(d.list_breakpoints().len(), 1);
        assert!(d.check_execute_65816(0x00_8000));
        assert!(!d.check_execute_65816(0x00_8001));
        d.enable_breakpoint(id, false);
        assert!(!d.check_execute_65816(0x00_8000));
        d.remove_breakpoint(id);
        assert!(d.list_breakpoints().is_empty());
    }

    fn write_bp(addr: u32) -> Breakpoint {
        Breakpoint {
            address: addr,
            bp_type: BreakpointType::write(),
            cpu_id: Some(0),
            condition: None,
        }
    }

    #[test]
    fn value_matched_watchpoint_fires_only_on_matching_byte() {
        let mut d = SnesDebugger::new(test_sysinfo());
        let id = d.add_breakpoint(write_bp(0x7E_0000));
        d.set_breakpoint_match_spec(
            id,
            MatchSpec {
                value: Some(0xAB),
                ..Default::default()
            },
        );
        // Wrong byte: silent. Value-agnostic path: also silent.
        assert!(!d.check_memory_access_valued_65816(0x7E_0000, true, 0x00));
        assert!(!d.check_memory_access_65816(0x7E_0000, true));
        // Matching byte: fires.
        assert!(d.check_memory_access_valued_65816(0x7E_0000, true, 0xAB));
        let info = d
            .list_breakpoints()
            .into_iter()
            .find(|b| b.id == id)
            .unwrap();
        assert_eq!(info.hit_count, 1);
        assert_eq!(info.match_spec.value, Some(0xAB));
    }

    #[test]
    fn available_events_lists_the_snes_taxonomy() {
        let d = SnesDebugger::new(test_sysinfo());
        let events = d.available_events();
        assert_eq!(events.len(), 9, "all nine SNES event classes advertised");
        // VBlank is paramless.
        let vblank = events.iter().find(|e| e.label == "VBlank").unwrap();
        assert!(vblank.param_label.is_none());
        assert!(vblank.param_options.is_empty());
        // APU port write enumerates its four mailboxes.
        let apu = events.iter().find(|e| e.label == "APU port write").unwrap();
        assert_eq!(apu.param_options.len(), 4);
        assert_eq!(apu.param_options[0].value, 0);
        // PPU register write takes a free-form numeric param.
        let ppu = events
            .iter()
            .find(|e| e.label == "PPU register write")
            .unwrap();
        assert!(ppu.param_label.is_some());
        assert!(ppu.param_options.is_empty());
    }

    #[test]
    fn tier_follows_breakpoints_tracing_and_mode() {
        let mut d = SnesDebugger::new(test_sysinfo());
        // Running + nothing instrumented -> Fast.
        assert_eq!(d.execution_tier(), ExecutionTier::Fast);
        // Tracing on any CPU -> LightDebug.
        d.set_tracing_enabled(0, true);
        assert_eq!(d.execution_tier(), ExecutionTier::LightDebug);
        d.set_tracing_enabled(0, false);
        assert_eq!(d.execution_tier(), ExecutionTier::Fast);
        // A breakpoint -> LightDebug.
        d.add_breakpoint(exec_bp(0x1234));
        assert_eq!(d.execution_tier(), ExecutionTier::LightDebug);
        // Any non-Running mode -> FullDebug regardless.
        d.set_execution_mode(ExecutionMode::Paused);
        assert_eq!(d.execution_tier(), ExecutionTier::FullDebug);
    }

    #[test]
    fn record_trace_then_flush_returns_entry_bytes() {
        let mut d = SnesDebugger::new(test_sysinfo());
        // Tracing must be enabled for the buffer to accept entries.
        assert!(d.get_trace_flush_data(0).is_none());
        d.set_tracing_enabled(0, true);
        let entry = TraceEntryBinary65816 {
            pbr_pc: 0x00_8000,
            s: 0x01ff,
            p: 0x34,
            e: 1,
            bytes_len: 1,
            bytes: [0x78, 0, 0, 0],
            ..Default::default()
        };
        d.record_trace_65816(&entry);
        let data = d.get_trace_flush_data(0).expect("buffered entry");
        assert_eq!(data.len(), TRACE_ENTRY_SIZE_65816);
        let back = TraceEntryBinary65816::from_bytes(&data).unwrap();
        assert_eq!(back, entry);
        d.clear_trace_flushed(0);
        assert!(d.get_trace_flush_data(0).is_none());
    }

    #[test]
    fn cpu_states_cover_both_cpus() {
        let d = SnesDebugger::new(test_sysinfo());
        let states = d.get_cpu_states();
        assert_eq!(states.len(), 2);
        assert_eq!(states[0].cpu_id, 0);
        assert_eq!(states[1].cpu_id, 1);
        // The 65C816 snapshot exposes A/X/Y/S/D/DB/PB/P/E.
        assert!(states[0].registers.iter().any(|r| r.name == "A"));
        assert!(states[0].registers.iter().any(|r| r.name == "E"));
        // The SPC700 snapshot exposes the YA pseudo-register.
        assert!(states[1].registers.iter().any(|r| r.name == "YA"));
        assert_eq!(d.traceable_cpu_count(), 2);
    }

    #[test]
    fn memory_regions_include_wram_and_aram() {
        let d = SnesDebugger::new(test_sysinfo());
        let regions = d.get_memory_regions();
        assert!(regions.iter().any(|r| r.name == "WRAM"));
        assert!(regions.iter().any(|r| r.name == "ARAM"));
        assert!(regions.iter().any(|r| r.name == "VRAM"));
    }
}

/// 65C816 call-stack and instruction-history tests. The
/// storage and ring-buffer behaviour is checked directly on
/// [`SnesDebugger`]; the opcode-detection wiring (`maintain_call_stack_65816`)
/// is checked end-to-end by single-stepping a synthetic LoROM that calls
/// and returns.
mod callstack_history_tests {
    use super::*;

    /// Stamp the minimal SNES header LoROM detection needs to score a
    /// confident LoROM match (mirrors `boot_test::stamp_header`).
    fn stamp_lorom_header(chip: &mut [u8], reset_vector: u16) {
        let header = 0x7FB0;
        chip[header + 0x10..header + 0x25].copy_from_slice(b"CALLSTACK TEST       ");
        chip[header + 0x25] = 0x20; // map_mode = LoROM
        chip[header + 0x27] = 0x0A; // rom_size_code = 1 MiB
        chip[header + 0x29] = 0x01; // destination = USA (NTSC)
        chip[header + 0x2A] = 0x01; // developer_id
        let checksum: u16 = 0x1234;
        chip[header + 0x2C..header + 0x2E].copy_from_slice(&(!checksum).to_le_bytes());
        chip[header + 0x2E..header + 0x30].copy_from_slice(&checksum.to_le_bytes());
        chip[header + 0x4C..header + 0x4E].copy_from_slice(&reset_vector.to_le_bytes());
    }

    /// 1 MiB LoROM whose `$00:8000` code is `code` and whose reset vector
    /// points at `$8000`.
    fn synth_lorom(code: &[u8]) -> Vec<u8> {
        let mut chip = vec![0u8; 0x10_0000];
        chip[..code.len()].copy_from_slice(code);
        stamp_lorom_header(&mut chip, 0x8000);
        chip
    }

    /// Boot `code` and single-step the 65C816 `steps` instructions, one per
    /// frame run (through the full-debug tier, which is where the call
    /// stack + history are maintained).
    fn boot_and_step(code: &[u8], steps: usize) -> SnesEmulator {
        let mut emu = SnesEmulator::new();
        emu.load_rom(&synth_lorom(code))
            .expect("load synthetic LoROM");
        for _ in 0..steps {
            emu.debugger().set_execution_mode(ExecutionMode::StepInto);
            emu.run_frame();
        }
        emu
    }

    #[test]
    fn call_stack_push_pop_jsr_rts() {
        // $8000 JSR $8005 ; $8003 BRA -2 ; $8005 RTS
        let code = [0x20, 0x05, 0x80, 0x80, 0xFE, 0x60];

        // Step 1: the JSR pushes a frame.
        let mut emu = boot_and_step(&code, 1);
        let stack = emu.debugger().get_call_stack(0);
        assert_eq!(stack.len(), 1, "JSR should push one frame");
        assert_eq!(stack[0].call_address, 0x00_8000);
        assert_eq!(stack[0].return_address, 0x00_8003);
        assert!(stack[0].function_name.is_none());

        // Step 2: now at $8005, the RTS pops it.
        emu.debugger().set_execution_mode(ExecutionMode::StepInto);
        emu.run_frame();
        assert!(
            emu.debugger().get_call_stack(0).is_empty(),
            "RTS should pop the frame"
        );
    }

    #[test]
    fn call_stack_push_pop_jsl_rtl() {
        // $8000 JSL $00:8006 ; $8004 BRA -2 ; $8006 RTL
        let code = [0x22, 0x06, 0x80, 0x00, 0x80, 0xFE, 0x6B];

        let mut emu = boot_and_step(&code, 1);
        let stack = emu.debugger().get_call_stack(0);
        assert_eq!(stack.len(), 1, "JSL should push one frame");
        assert_eq!(stack[0].call_address, 0x00_8000);
        // JSL is 4 bytes, so the return address is $8004.
        assert_eq!(stack[0].return_address, 0x00_8004);

        emu.debugger().set_execution_mode(ExecutionMode::StepInto);
        emu.run_frame();
        assert!(
            emu.debugger().get_call_stack(0).is_empty(),
            "RTL should pop the frame"
        );
    }

    #[test]
    fn call_stack_storage_and_depth_cap() {
        let mut d = SnesDebugger::new(test_sysinfo());
        assert!(d.get_call_stack(0).is_empty());
        // Push well past the cap; growth must stop at the bound.
        for i in 0..400u32 {
            d.push_call_65816(StackFrame {
                call_address: 0x00_8000 + i,
                return_address: 0x00_9000 + i,
                function_name: None,
            });
        }
        let depth = d.get_call_stack(0).len();
        assert_eq!(depth, 256, "call stack is bounded at MAX_CALL_STACK_DEPTH");
        // cpu 1 (SPC700) has no call stack.
        assert!(d.get_call_stack(1).is_empty());
        // A pop shrinks it by one.
        d.pop_call_65816();
        assert_eq!(d.get_call_stack(0).len(), depth - 1);
    }

    #[test]
    fn instruction_history_ring_keeps_last_1024() {
        let mut d = SnesDebugger::new(test_sysinfo());
        assert!(d.get_instruction_history(0, 10).is_empty());
        for i in 0..1100u32 {
            d.record_instruction_65816(InstructionTrace {
                address: i,
                instruction: format!("nop #{i}"),
                registers_before: Vec::new(),
                cycle_count: 2,
            });
        }
        let all = d.get_instruction_history(0, 5000);
        assert_eq!(all.len(), 1024, "ring keeps the last 1024 records");
        // Newest at the back; oldest retained is 1100 - 1024 = 76.
        assert_eq!(all.first().unwrap().address, 76);
        assert_eq!(all.last().unwrap().address, 1099);
        // A bounded request returns only the most recent N.
        let last3 = d.get_instruction_history(0, 3);
        assert_eq!(last3.len(), 3);
        assert_eq!(last3[0].address, 1097);
        assert_eq!(last3[2].address, 1099);
        // cpu 1 has no history.
        assert!(d.get_instruction_history(1, 10).is_empty());
    }

    #[test]
    fn instruction_history_populated_by_single_step() {
        // $8000 LDA #$12 (8-bit M at reset) ; $8002 BRA -2
        let code = [0xA9, 0x12, 0x80, 0xFE];
        let mut emu = boot_and_step(&code, 1);
        let hist = emu.debugger().get_instruction_history(0, 10);
        assert_eq!(hist.len(), 1, "one single-step records one instruction");
        assert_eq!(hist[0].address, 0x00_8000);
        assert!(
            hist[0].instruction.to_lowercase().starts_with("lda"),
            "instruction text: {}",
            hist[0].instruction
        );
        // The pre-execution register snapshot is captured (A/X/Y/.. present).
        assert!(hist[0].registers_before.iter().any(|r| r.name == "A"));
    }
}

mod integration {
    use super::*;

    /// Running a frame with 65C816 tracing enabled drives the
    /// `LightDebug` tier and fills the trace buffer end-to-end. ROM-free
    /// (the CPU executes open-bus) so it is safe for CI: it exercises the
    /// capture -> buffer -> flush -> format plumbing, not specific opcodes.
    #[test]
    fn run_frame_with_tracing_fills_buffer() {
        let mut emu = SnesEmulator::new();
        emu.debugger().set_tracing_enabled(0, true);
        emu.run_frame();
        let data = emu
            .debugger()
            .get_trace_flush_data(0)
            .expect("trace data after a traced frame");
        assert!(!data.is_empty());
        assert_eq!(data.len() % TRACE_ENTRY_SIZE_65816, 0);
        // Every entry renders to a well-formed line without panicking.
        for chunk in data.as_chunks::<TRACE_ENTRY_SIZE_65816>().0.iter().take(64) {
            let entry = TraceEntryBinary65816::from_bytes(chunk).unwrap();
            let line = format_trace_line_65816(&entry);
            assert!(line.contains(" A:"));
            assert!(line.contains(" V:"));
        }
    }

    #[test]
    fn fast_tier_leaves_trace_buffer_empty() {
        let mut emu = SnesEmulator::new();
        // No tracing, no breakpoints -> Fast tier -> no capture.
        emu.run_frame();
        assert!(emu.debugger().get_trace_flush_data(0).is_none());
    }

    #[cfg(feature = "chip-superfx")]
    /// Build a minimal Super FX LoROM whose S-CPU reset code masks the STOP
    /// interrupt (`CFGR.irq`), starts the GSU on `kernel` at chip `$0100`, then
    /// idles in `BRA -2`. The masked IRQ lets the GSU run the kernel once and
    /// halt without an S-CPU IRQ storm. Kept local (test binaries don't share
    /// helpers with `superfx_irq_test.rs`); enough to drive `run_frame`
    /// end-to-end for cpu-2 trace capture. CI-safe: no ROM file needed.
    fn build_superfx_trace_rom(kernel: &[u8]) -> Vec<u8> {
        const SIZE: usize = 0x10_0000;
        let mut chip = vec![0u8; SIZE];
        // S-CPU reset (emulation / 8-bit): LDA #$80 : STA $3037 (CFGR.irq) ;
        // CLI ; LDA #$00 : STA $301E (R15 low) ; LDA #$01 : STA $301F
        // (R15 high = $0100 -> GO) ; BRA -2.
        let reset: [u8; 18] = [
            0xA9, 0x80, 0x8D, 0x37, 0x30, // LDA #$80 : STA $3037 (mask STOP IRQ)
            0x58, // CLI
            0xA9, 0x00, 0x8D, 0x1E, 0x30, // LDA #$00 : STA $301E (R15 low)
            0xA9, 0x01, 0x8D, 0x1F, 0x30, // LDA #$01 : STA $301F (R15 high -> GO)
            0x80, 0xFE, // BRA -2
        ];
        chip[..reset.len()].copy_from_slice(&reset);
        chip[0x0100..0x0100 + kernel.len()].copy_from_slice(kernel);
        // Super FX LoROM header at $7FB0 (title at +$10, fields at +$25..).
        chip[0x7FC0..0x7FC0 + 21].copy_from_slice(b"SUPERFX TRACE TEST   ");
        chip[0x7FD5] = 0x20; // LoROM map mode (Super FX shares it)
        chip[0x7FD6] = 0x13; // cartridge_type $13 -> Super FX (Star Fox)
        chip[0x7FD7] = 0x0A; // 1 MiB
        chip[0x7FD9] = 0x01; // USA -> NTSC
        let checksum: u16 = 0x1234;
        chip[0x7FDC..0x7FDE].copy_from_slice(&(!checksum).to_le_bytes());
        chip[0x7FDE..0x7FE0].copy_from_slice(&checksum.to_le_bytes());
        // Emulation vectors: reset -> $8000 (also the inert IRQ target).
        chip[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
        chip[0x7FFE..0x8000].copy_from_slice(&0x8000u16.to_le_bytes());
        chip
    }

    /// End-to-end Super FX GSU (cpu 2) trace capture through the whole
    /// [`SnesEmulator`]: a synthetic cart arms the GSU, cpu-2 tracing is
    /// enabled, and `run_frame` runs the chip's `catch_up` capture ->
    /// `drain_gsu_trace` -> the debugger's cpu-2 ring. CI-safe (no ROM file);
    /// exercises the `sync_gsu_trace_enable`/`drain_gsu_trace` wiring the
    /// chip-level tests can't reach.
    #[cfg(feature = "chip-superfx")]
    #[test]
    fn superfx_capture_traces_gsu_kernel_end_to_end() {
        // iwt r0,#$1234 ; iwt r1,#$5678 ; stop.
        let kernel = [0xF0, 0x34, 0x12, 0xF1, 0x78, 0x56, 0x00];
        let rom = build_superfx_trace_rom(&kernel);
        let mut emu = SnesEmulator::new();
        emu.load_rom(&rom).expect("synthetic Super FX ROM loads");
        assert_eq!(
            emu.coprocessor_label(),
            Some("Super FX"),
            "the GSU chip installed"
        );

        emu.debugger().set_tracing_enabled(2, true);
        // The GSU is armed by the reset code and advanced by the frame catch-up;
        // a few frames give it time to run the kernel to `stop`. Accumulate the
        // ring across frames (draining each) so a multi-frame run is captured.
        let mut data = Vec::new();
        for _ in 0..6 {
            emu.run_frame();
            if let Some(chunk) = emu.debugger().get_trace_flush_data(2) {
                data.extend_from_slice(&chunk);
                emu.debugger().clear_trace_flushed(2);
            }
        }

        assert!(!data.is_empty(), "cpu-2 tracing captured GSU instructions");
        assert_eq!(data.len() % TRACE_ENTRY_SIZE_GSU, 0, "whole records only");
        let mut saw_iwt = false;
        for chunk in data.as_chunks::<TRACE_ENTRY_SIZE_GSU>().0 {
            let entry = TraceEntryBinaryGsu::from_bytes(chunk).unwrap();
            let line = format_trace_line_gsu(&entry);
            assert!(line.contains("R0:"), "R0 column: {line}");
            assert!(line.contains("SFR:"), "SFR column: {line}");
            if line.contains("iwt") {
                saw_iwt = true;
            }
        }
        assert!(saw_iwt, "the end-to-end cpu-2 trace decoded the iwt kernel");
    }

    /// Enabling SPC700 (cpu 1) tracing and running a frame captures the
    /// APU's IPL boot via `Apu::catch_up_to` and bridges it into the
    /// debugger. CI-safe and byte-exact: the IPL ROM and the SPC700 reset
    /// state (`SP=$ef`, `PSW` Z-set) are fixed, and the 64-byte IPL clears
    /// zero page before touching the mailbox, so the head of the trace is
    /// independent of the (ROM-free, open-bus) 65C816. These are the
    /// literal first six lines of a bsnes-plus SPC700 trace of the Super
    /// Mario World boot.
    #[test]
    fn spc700_capture_traces_ipl_boot() {
        let mut emu = SnesEmulator::new();
        // `reset()` schedules the per-frame `FrameEnd` event that drives the
        // APU catch-up (a fresh `new()` leaves the scheduler empty) and
        // reboots the SPC700 onto its `$ffc0` IPL vector.
        emu.reset();
        emu.debugger().set_tracing_enabled(1, true);
        emu.run_frame();

        let data = emu
            .debugger()
            .get_trace_flush_data(1)
            .expect("SPC700 trace data after a traced frame");
        assert_eq!(data.len() % TRACE_ENTRY_SIZE_SPC700, 0);

        let lines: Vec<String> = data
            .as_chunks::<TRACE_ENTRY_SIZE_SPC700>()
            .0
            .iter()
            .take(6)
            .map(|c| format_trace_line_spc700(&TraceEntryBinarySpc700::from_bytes(c).unwrap()))
            .collect();

        let expected = [
            "..ffc0 mov   x, #$ef           A:00 X:00 Y:00 SP:01ef YA:0000 ......Z.",
            "..ffc2 mov   sp, x             A:00 X:ef Y:00 SP:01ef YA:0000 N.......",
            "..ffc3 mov   a, #$00           A:00 X:ef Y:00 SP:01ef YA:0000 N.......",
            "..ffc5 mov   (x), a            A:00 X:ef Y:00 SP:01ef YA:0000 ......Z.",
            "..ffc6 dec   x                 A:00 X:ef Y:00 SP:01ef YA:0000 ......Z.",
            "..ffc7 bne   $ffc5             A:00 X:ee Y:00 SP:01ef YA:0000 N.......",
        ];
        assert!(
            lines.len() >= expected.len(),
            "expected >= {} SPC700 lines, got {}",
            expected.len(),
            lines.len()
        );
        for (i, want) in expected.iter().enumerate() {
            assert_eq!(&lines[i], want, "SPC700 IPL boot line {i}");
        }
    }
}
