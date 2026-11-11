//! CPU trace lines in the format bsnes-plus writes (65C816 + SPC700).
//!
//! [`format_trace_line_65816`] renders a [`TraceEntryBinary65816`] into a
//! line that is byte for byte what bsnes-plus's tracer writes for the same
//! instruction and state, so a trace produced here can be diffed against a
//! reference log captured with bsnes-plus. [`format_trace_line_spc700`]
//! does the same for the SPC700 (see its own doc comment for the line
//! layout). The 65C816 line, in the variant that prints `H` in PPU dots:
//!
//! ```text
//! 008001 stz $4200      [004200] A:0280 X:000a Y:0001 S:01f1 D:0000 DB:00 ..1B.I.. V:  0 H: 50 F: 0
//! ```
//!
//! Pieces (all hex lowercase):
//! - `"{pbr_pc:06x} "`: 24-bit instruction address
//! - `"{disasm:<14} "`: mnemonic + operands, left-justified to 14
//! - 9-char effective-address block: `"[{eff:06x}] "` or 9 spaces when the
//!   addressing mode prints no bracket (Implied / Accumulator / Immediate /
//!   block-move)
//! - `"A:.. X:.. Y:.. S:.. D:.. DB:.. "` (A/X/Y/S/D always 16-bit)
//! - 8 flag chars + space, E-aware (native `NVMXDIZC`; emulation bit5=`1`,
//!   bit4=`B`)
//! - `"V:{v:3} H:{h:3} F:{f:2}"`
//!
//! The mnemonic + operand text is produced by the shared
//! [`wdc65816::disasm`] decoder (which writes the same grammar) over the
//! entry's captured instruction bytes, then lowercased, the only
//! difference between that decoder's UI output and the trace is the hex
//! case. The `[effective]` bracket is computed by [`effective_address`]
//! at capture time (it needs live memory for indirect modes) and stored
//! in the entry, so this formatter stays bus-free and unit-testable.
//!
//! The format is a behaviour: `snes1-cli compare-trace` diffs a live trace
//! against bsnes-plus's logs line by line, and the tests hold lines taken
//! from those logs.

#[cfg(feature = "chip-cx4")]
use crate::cartridge::cx4::instructions::disassemble as disassemble_cx4;
#[cfg(feature = "chip-cx4")]
use crate::debugger::TraceEntryBinaryCx4;
#[cfg(feature = "chip-superfx")]
use crate::debugger::TraceEntryBinaryGsu;
use crate::debugger::{TraceEntryBinary65816, TraceEntryBinarySpc700};
use retro_core::cpu::Addressable;
use wdc65816::addressing::AddressingMode;
use wdc65816::decode::{Mnemonic, OPCODE_TABLE};
use wdc65816::disasm::{disassemble_at, DisasmState};

/// A throwaway [`Addressable`] backed by an instruction's captured bytes,
/// used to re-run the disassembler without an emulator. Reads outside the
/// captured window return `0` (the decoder never reads past the instruction
/// length we recorded).
struct SliceBus {
    base: u32,
    bytes: [u8; 4],
    len: u8,
}

impl Addressable for SliceBus {
    fn read(&mut self, addr: u32) -> u8 {
        let off = addr.wrapping_sub(self.base);
        if off < self.len as u32 {
            self.bytes[off as usize]
        } else {
            0
        }
    }

    fn write(&mut self, _addr: u32, _value: u8) {}
}

/// Render the 8-character processor-status field of the trace line. In
/// emulation mode (`emulation == true`) the M/X positions print the 6502
/// unused (`1`) and break (`B`) flags; in native mode they print `M` / `X`.
fn fmt_flags(p: u8, emulation: bool) -> String {
    let bit = |mask: u8, ch: char| if p & mask != 0 { ch } else { '.' };
    let n = bit(0x80, 'N');
    let v = bit(0x40, 'V');
    let (m, x) = if emulation {
        (
            if p & 0x20 != 0 { '1' } else { '.' },
            if p & 0x10 != 0 { 'B' } else { '.' },
        )
    } else {
        (bit(0x20, 'M'), bit(0x10, 'X'))
    };
    let d = bit(0x08, 'D');
    let i = bit(0x04, 'I');
    let z = bit(0x02, 'Z');
    let c = bit(0x01, 'C');
    format!("{n}{v}{m}{x}{d}{i}{z}{c}")
}

/// Render one trace entry as a line of the trace format (no trailing
/// newline; the caller joins lines).
pub fn format_trace_line_65816(e: &TraceEntryBinary65816) -> String {
    let state = DisasmState {
        emulation: e.e != 0,
        flag_m: e.p & 0x20 != 0,
        flag_x: e.p & 0x10 != 0,
    };
    let mut bus = SliceBus {
        base: e.pbr_pc,
        bytes: e.bytes,
        len: e.bytes_len,
    };
    // The shared decoder emits uppercase hex for the UI; the trace format
    // is lowercase. Only hex digits A-F differ (mnemonics/suffixes are
    // already lowercase), so a blanket lowercase is exact.
    let (disasm, _len) = disassemble_at(&mut bus, e.pbr_pc, &state);
    let disasm = disasm.to_lowercase();

    let mut line = String::with_capacity(80);
    line.push_str(&format!("{:06x} ", e.pbr_pc & 0x00FF_FFFF));
    line.push_str(&format!("{disasm:<14} "));
    if e.eff_present != 0 {
        line.push_str(&format!("[{:06x}] ", e.eff_addr & 0x00FF_FFFF));
    } else {
        line.push_str("         ");
    }
    line.push_str(&format!(
        "A:{:04x} X:{:04x} Y:{:04x} S:{:04x} D:{:04x} DB:{:02x} ",
        e.a, e.x, e.y, e.s, e.d, e.dbr
    ));
    line.push_str(&fmt_flags(e.p, e.e != 0));
    line.push(' ');
    line.push_str(&format!(
        "V:{:3} H:{:3} F:{:2}",
        e.vcounter, e.hdot, e.framecounter
    ));
    line
}

/// Render the 8-character SPC700 status field `NVPBHIZC` (PSW bits 7..0),
/// `.` for a clear bit. Unlike the 65C816 there is no emulation-mode
/// remapping, the SPC700 status layout is fixed.
fn fmt_flags_spc700(psw: u8) -> String {
    let bit = |mask: u8, ch: char| if psw & mask != 0 { ch } else { '.' };
    let n = bit(0x80, 'N');
    let v = bit(0x40, 'V');
    let p = bit(0x20, 'P');
    let b = bit(0x10, 'B');
    let h = bit(0x08, 'H');
    let i = bit(0x04, 'I');
    let z = bit(0x02, 'Z');
    let c = bit(0x01, 'C');
    format!("{n}{v}{p}{b}{h}{i}{z}{c}")
}

/// Render one SPC700 trace entry as a line of the format bsnes-plus
/// writes for its SMP trace (no trailing newline):
///
/// ```text
/// ..ffc0 mov   x, #$ef           A:00 X:00 Y:00 SP:01ef YA:0000 ......Z.
/// ```
///
/// Pieces (all hex lowercase):
/// - `"..{pc:04x} "`: `..` bank filler + flat 16-bit PC
/// - `"{disasm:<23} "`: mnemonic + operands, left-justified to 23
/// - `"A:.. X:.. Y:.. SP:01.. YA:.... "`: 8-bit regs; SP prints `01`+low
///   byte; `YA = (y << 8) | a`
/// - 8 flag chars `NVPBHIZC` (last field, no trailing space)
///
/// The disassembly is produced by the shared [`spc700::disasm`] decoder
/// (which renders its opcode table in this format) over the captured
/// instruction bytes, with `P` taken from `psw & 0x20`, then lowercased,
/// the only difference between that decoder's UI output and the trace is
/// the hex case.
pub fn format_trace_line_spc700(e: &TraceEntryBinarySpc700) -> String {
    let state = spc700::disasm::DisasmState {
        p: e.psw & 0x20 != 0,
    };
    let mut bus = SliceBus {
        base: e.pc as u32,
        bytes: [e.bytes[0], e.bytes[1], e.bytes[2], 0],
        len: e.bytes_len,
    };
    let (disasm, _len) = spc700::disasm::disassemble_at(&mut bus, e.pc as u32, &state);
    let disasm = disasm.to_lowercase();

    let ya = ((e.y as u16) << 8) | e.a as u16;

    let mut line = String::with_capacity(64);
    line.push_str(&format!("..{:04x} ", e.pc));
    line.push_str(&format!("{disasm:<23} "));
    line.push_str(&format!(
        "A:{:02x} X:{:02x} Y:{:02x} SP:01{:02x} YA:{:04x} ",
        e.a, e.x, e.y, e.sp, ya
    ));
    line.push_str(&fmt_flags_spc700(e.psw));
    line
}

/// Render the 12-character Super FX `SFR` status field, `.` for a clear bit.
/// High->low over the flags a GSU trace cares about: `IRQ B IH IL ALT2 ALT1 R G
/// OV S CY Z` (each a single mnemonic char: `ibhl21rgvscz`). There is no
/// bsnes-plus GSU trace to match byte-for-byte, so this is our own stable
/// layout.
#[cfg(feature = "chip-superfx")]
fn fmt_flags_gsu(sfr: u16) -> String {
    let bit = |mask: u16, ch: char| if sfr & mask != 0 { ch } else { '.' };
    let i = bit(0x8000, 'i'); // IRQ
    let b = bit(0x1000, 'b'); // B (immediate byte)
    let h = bit(0x0800, 'h'); // IH (immediate high)
    let l = bit(0x0400, 'l'); // IL (immediate low)
    let a2 = bit(0x0200, '2'); // ALT2
    let a1 = bit(0x0100, '1'); // ALT1
    let r = bit(0x0040, 'r'); // R (R14 ROM prefetch)
    let g = bit(0x0020, 'g'); // G (GO / running)
    let v = bit(0x0010, 'v'); // OV
    let s = bit(0x0008, 's'); // S (sign)
    let c = bit(0x0004, 'c'); // CY
    let z = bit(0x0002, 'z'); // Z
    format!("{i}{b}{h}{l}{a2}{a1}{r}{g}{v}{s}{c}{z}")
}

/// Render one Super FX GSU trace entry as a single line (no trailing newline).
/// There is no bsnes-plus GSU trace to diff against, so this is our own layout,
/// consistent in spirit with the 65C816 / SPC700 lines:
///
/// ```text
/// 008000 iwt   r1,#$beef      R0:0000 R1:beef ... R15:0104 SFR:0120 .....1.g....
/// ```
///
/// - `"{pbr_pc:06x} "`: 24-bit `PBR:(R15-1)` instruction address
/// - `"{disasm:<20} "`: mnemonic + operands, left-justified to 20
/// - `"R0:.... R1:.... ... R15:.... "`: the full 16-register file (16-bit each)
/// - `"SFR:{sfr:04x} "` + the 12-char [`fmt_flags_gsu`] status field
///
/// The disassembly comes from the shared [`superfx_gsu::disasm`] decoder over
/// the captured instruction bytes (with the `ALT1`/`ALT2` prefix state taken
/// from the captured `SFR`), then lowercased, so this formatter stays bus-free.
#[cfg(feature = "chip-superfx")]
pub fn format_trace_line_gsu(e: &TraceEntryBinaryGsu) -> String {
    let base = e.pbr_pc & 0x7F_FFFF;
    let mut bus = SliceBus {
        base,
        bytes: [e.bytes[0], e.bytes[1], e.bytes[2], 0],
        len: e.bytes_len,
    };
    let alt1 = e.sfr & 0x0100 != 0;
    let alt2 = e.sfr & 0x0200 != 0;
    let res = superfx_gsu::disasm::disassemble_one(&mut bus, base, alt1, alt2);
    let disasm = if res.operands.is_empty() {
        res.mnemonic.to_lowercase()
    } else {
        format!("{} {}", res.mnemonic, res.operands).to_lowercase()
    };

    let mut line = String::with_capacity(180);
    line.push_str(&format!("{:06x} ", e.pbr_pc & 0x00FF_FFFF));
    line.push_str(&format!("{disasm:<20} "));
    for (n, reg) in e.r.iter().enumerate() {
        line.push_str(&format!("R{n}:{reg:04x} "));
    }
    line.push_str(&format!("SFR:{:04x} ", e.sfr));
    line.push_str(&fmt_flags_gsu(e.sfr));
    line
}

/// Render the 4-character Cx4 HG51B169 flag field, `.` for a clear bit, in the
/// packed `N Z V C` order the entry's `flags` byte carries
/// (`N=0x8`/`Z=0x4`/`V=0x2`/`C=0x1`).
#[cfg(feature = "chip-cx4")]
fn fmt_flags_cx4(flags: u8) -> String {
    let bit = |mask: u8, ch: char| if flags & mask != 0 { ch } else { '.' };
    let n = bit(0x8, 'n');
    let z = bit(0x4, 'z');
    let v = bit(0x2, 'v');
    let c = bit(0x1, 'c');
    format!("{n}{z}{v}{c}")
}

/// Render one Cx4 HG51B169 trace entry as a single line (no trailing newline).
/// There is no bsnes-plus Cx4 trace to diff against, so this is our own layout,
/// consistent in spirit with the 65C816 / SPC700 / GSU lines:
///
/// ```text
/// 000000 ld a,#$12            A:000012 P:0000 R0:000000 ... R15:000000 nzvc
/// ```
///
/// - `"{pc:06x} "`: 23-bit program address
/// - `"{disasm:<20} "`: mnemonic + operands (from the pure decoder), lowercased
/// - `"A:{a:06x} P:{p:04x} "`: the accumulator + page register
/// - `"R0:.. .. R15:.. "`: the 16 general-purpose registers (24-bit)
/// - 4 flag chars (`nzvc`)
#[cfg(feature = "chip-cx4")]
pub fn format_trace_line_cx4(e: &TraceEntryBinaryCx4) -> String {
    let d = disassemble_cx4(e.opcode);
    let disasm = if d.operands.is_empty() {
        d.mnemonic.to_lowercase()
    } else {
        format!("{} {}", d.mnemonic, d.operands).to_lowercase()
    };

    let mut line = String::with_capacity(180);
    line.push_str(&format!("{:06x} ", e.pc & 0x7F_FFFF));
    line.push_str(&format!("{disasm:<20} "));
    line.push_str(&format!("A:{:06x} P:{:04x} ", e.a & 0xFF_FFFF, e.p));
    for (n, reg) in e.gpr.iter().enumerate() {
        line.push_str(&format!("R{n}:{:06x} ", reg & 0xFF_FFFF));
    }
    line.push_str(&fmt_flags_cx4(e.flags));
    line
}

/// Register snapshot needed to resolve an effective address.
#[derive(Debug, Clone, Copy)]
pub struct EffRegs {
    pub d: u16,
    pub x: u16,
    pub y: u16,
    pub s: u16,
    pub db: u8,
    /// `PBR << 16 | PC` of the instruction (the program bank + counter).
    pub pbr_pc: u32,
}

/// Where an addressing mode measures its operand from.
#[derive(Clone, Copy)]
enum Origin {
    /// The direct-page register: `D` plus an 8-bit offset, a 16-bit sum in
    /// bank 0.
    DirectPage,
    /// The stack pointer: `S` plus an 8-bit offset, a 16-bit sum in bank 0.
    Stack,
    /// A 16-bit address in the data bank. An index is added to the whole
    /// 24-bit address, so it can run into the next bank.
    DataBank,
    /// A 16-bit address in the bank the instruction is in. An index wraps
    /// inside the bank.
    ProgramBank,
    /// A 16-bit address in bank 0.
    BankZero,
    /// The 24-bit address the operand spells out.
    Long,
    /// The address of the next instruction, plus the signed operand.
    NextInstruction,
}

/// An index register a mode adds.
#[derive(Clone, Copy)]
enum Index {
    None,
    X,
    Y,
}

/// The pointer an indirect mode reads at the address formed so far.
#[derive(Clone, Copy)]
enum Pointer {
    /// The mode is not indirect: the address formed is the one shown.
    None,
    /// Two bytes: a 16-bit address in the data bank.
    Word,
    /// Three bytes: a whole 24-bit address.
    Long,
}

/// How the bracket of a trace line is formed for one addressing mode: the
/// operand is measured from `origin` with `index` added; if the mode is
/// indirect, the `pointer` found there is followed and `index_after` is
/// added to what it holds.
#[derive(Clone, Copy)]
struct Bracket {
    origin: Origin,
    index: Index,
    pointer: Pointer,
    index_after: Index,
}

impl Bracket {
    /// A mode that names an address outright.
    const fn at(origin: Origin, index: Index) -> Self {
        Self {
            origin,
            index,
            pointer: Pointer::None,
            index_after: Index::None,
        }
    }

    /// A mode that names the address of a pointer and goes where it leads.
    const fn through(origin: Origin, index: Index, pointer: Pointer, index_after: Index) -> Self {
        Self {
            origin,
            index,
            pointer,
            index_after,
        }
    }
}

/// The bracket of each addressing mode, or `None` for the modes that name
/// no memory address (implied, accumulator, immediate, block move).
///
/// `jump` says the instruction is a `JMP` or `JSR` to an absolute address:
/// its operand names a place in the program bank, where the absolute
/// operand of any other instruction names one in the data bank. The three
/// indirect jumps show where their pointer *is*, not where it leads; that
/// is what the reference trace prints, and it is kept so the two logs
/// agree.
fn bracket_of(mode: AddressingMode, jump: bool) -> Option<Bracket> {
    use AddressingMode as Mode;
    Some(match mode {
        Mode::Implied
        | Mode::Accumulator
        | Mode::Immediate
        | Mode::ImmediateX
        | Mode::ImmediateByte
        | Mode::BlockMove => return None,

        Mode::Direct => Bracket::at(Origin::DirectPage, Index::None),
        Mode::DirectX => Bracket::at(Origin::DirectPage, Index::X),
        Mode::DirectY => Bracket::at(Origin::DirectPage, Index::Y),
        Mode::DirectIndirect => {
            Bracket::through(Origin::DirectPage, Index::None, Pointer::Word, Index::None)
        }
        Mode::DirectXIndirect => {
            Bracket::through(Origin::DirectPage, Index::X, Pointer::Word, Index::None)
        }
        Mode::DirectIndirectY => {
            Bracket::through(Origin::DirectPage, Index::None, Pointer::Word, Index::Y)
        }
        Mode::DirectIndirectLong => {
            Bracket::through(Origin::DirectPage, Index::None, Pointer::Long, Index::None)
        }
        Mode::DirectIndirectLongY => {
            Bracket::through(Origin::DirectPage, Index::None, Pointer::Long, Index::Y)
        }

        Mode::StackRelative => Bracket::at(Origin::Stack, Index::None),
        Mode::StackRelativeIndirectY => {
            Bracket::through(Origin::Stack, Index::None, Pointer::Word, Index::Y)
        }

        Mode::Absolute if jump => Bracket::at(Origin::ProgramBank, Index::None),
        Mode::Absolute => Bracket::at(Origin::DataBank, Index::None),
        Mode::AbsoluteX => Bracket::at(Origin::DataBank, Index::X),
        Mode::AbsoluteY => Bracket::at(Origin::DataBank, Index::Y),
        Mode::AbsoluteLong => Bracket::at(Origin::Long, Index::None),
        Mode::AbsoluteLongX => Bracket::at(Origin::Long, Index::X),

        Mode::AbsoluteIndirect => Bracket::at(Origin::ProgramBank, Index::None),
        Mode::AbsoluteXIndirect => Bracket::at(Origin::ProgramBank, Index::X),
        Mode::AbsoluteIndirectLong => Bracket::at(Origin::BankZero, Index::None),

        Mode::Relative | Mode::RelativeLong => Bracket::at(Origin::NextInstruction, Index::None),
    })
}

/// The instruction's operand: up to three bytes after the opcode, low byte
/// first. A byte the slice does not hold reads as zero.
fn operand_of(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .skip(1)
        .take(3)
        .enumerate()
        .fold(0, |value, (i, &byte)| value | (u32::from(byte) << (8 * i)))
}

/// `count` bytes of memory from `at`, low byte first, read in that order.
fn read_bytes(bus: &mut dyn Addressable, at: u32, count: u32) -> u32 {
    (0..count).fold(0, |value, i| {
        value | (u32::from(bus.read(at.wrapping_add(i))) << (8 * i))
    })
}

/// Resolve the effective address the trace prints in the `[......]`
/// bracket, or `None` for addressing modes that print no bracket
/// (Implied / Accumulator / Immediate / block-move).
///
/// The bracket is the address the operand names, as [`bracket_of`]
/// describes it for each mode, with nothing wrapped that the modes'
/// definitions do not wrap: a branch target or an indexed absolute
/// address may run past the end of its bank, which is how the reference
/// trace prints them. Checked against bsnes-plus's logs.
///
/// `bytes` is the instruction's raw bytes (opcode + operands); indirect
/// modes dereference live memory through `bus`, so this must be called at
/// capture time (the result is then stored in the trace entry).
pub fn effective_address(
    bus: &mut dyn Addressable,
    mode: AddressingMode,
    bytes: &[u8],
    regs: &EffRegs,
) -> Option<u32> {
    let opcode = bytes.first().copied().unwrap_or(0);
    let decoded = &OPCODE_TABLE[usize::from(opcode)];
    let jump = decoded.mode == AddressingMode::Absolute
        && matches!(decoded.mnemonic, Mnemonic::JMP | Mnemonic::JSR);
    let bracket = bracket_of(mode, jump)?;

    let operand = operand_of(bytes);
    let register = |index: Index| match index {
        Index::None => 0,
        Index::X => u32::from(regs.x),
        Index::Y => u32::from(regs.y),
    };
    let index = register(bracket.index);
    let program_bank = regs.pbr_pc & 0x00FF_0000;
    let data_bank = u32::from(regs.db) << 16;

    let formed = match bracket.origin {
        Origin::DirectPage => (u32::from(regs.d) + index + (operand & 0xFF)) & 0xFFFF,
        Origin::Stack => (u32::from(regs.s) + index + (operand & 0xFF)) & 0xFFFF,
        Origin::DataBank => data_bank + (operand & 0xFFFF) + index,
        Origin::ProgramBank => program_bank | ((operand + index) & 0xFFFF),
        Origin::BankZero => operand & 0xFFFF,
        Origin::Long => operand + index,
        Origin::NextInstruction => {
            let operand_bytes = u32::from(mode.operand_size(true, true));
            let next = program_bank | (regs.pbr_pc.wrapping_add(1 + operand_bytes) & 0xFFFF);
            // The operand is a signed distance, one or two bytes wide.
            let unused_bits = 32 - 8 * operand_bytes;
            let distance = ((operand << unused_bits) as i32) >> unused_bits;
            next.wrapping_add(distance as u32)
        }
    };
    let shown = match bracket.pointer {
        Pointer::None => formed,
        Pointer::Word => data_bank.wrapping_add(read_bytes(bus, formed, 2)),
        Pointer::Long => read_bytes(bus, formed, 3),
    };
    Some(shown.wrapping_add(register(bracket.index_after)) & 0x00FF_FFFF)
}
