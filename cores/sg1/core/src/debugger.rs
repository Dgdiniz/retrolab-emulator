//! The Mega Drive's part of the debugger.
//!
//! The debugger itself (the breakpoints, the execution mode, the focused
//! CPU, and for each CPU its coverage, trace, instruction history and call
//! stack) is retro-core's [`DebuggerState`]. This module gives it what the
//! console makes different: its two CPUs ([`MAIN_CPU`], the 68000, and
//! [`SOUND_CPU`], the Z80) and how each is kept, the events a breakpoint can
//! stop on ([`genesis_events`], [`GenesisEvent`], [`GENESIS_EVENTS`]), the
//! memory regions a host views, and each CPU's trace entry
//! ([`TraceEntryBinary`], [`TraceEntryBinaryZ80`]) with its text line.

use std::collections::HashSet;
use std::fmt::Write as _;

use retro_core::debug::{
    BreakpointChecks, BreakpointList, ConsoleDebug, CpuDebugSpec, DebugEvent, DebuggerState,
    EventParam, EventSpec, ParamSet, TraceEntry, TraceText,
};
use retro_core::types::{EventKind, MemoryRegion, MemoryRegionInfo, SystemInfo};

use crate::system::GenesisEmulator;
use crate::vdp::{VideoEvent, VideoEventWatch, WriteTarget};
use crate::z80::disasm;
use crate::z80::Z80;

/// The 68000's CPU id: the main CPU, the one the frame loop steps.
pub use retro_core::debug::MAIN_CPU;

/// The Z80's CPU id.
pub const SOUND_CPU: usize = 1;

/// How the debugger keeps the 68000: coverage over its whole 24-bit address
/// space (a PC with high bits set lands on the address it mirrors), its
/// trace entries, a history and a call stack, and every breakpoint checked.
/// A host steps it, and its coverage is recorded as it runs.
const MAIN_CPU_SPEC: CpuDebugSpec = CpuDebugSpec {
    coverage_size: 0x0100_0000,
    address_mask: 0x00FF_FFFF,
    trace_entry_size: TRACE_ENTRY_SIZE,
    history: true,
    checks: BreakpointChecks::ALL,
    steppable: true,
    records_coverage: true,
};

/// How the debugger keeps the Z80: coverage over its 64 KiB, its trace
/// entries and a history (nothing pushes onto its call stack). No frame
/// loop checks its breakpoints, so none is offered for it. A host steps it,
/// and its coverage is recorded on every tier.
const SOUND_CPU_SPEC: CpuDebugSpec = CpuDebugSpec {
    coverage_size: 0x1_0000,
    address_mask: 0xFFFF,
    trace_entry_size: Z80_TRACE_ENTRY_SIZE,
    history: true,
    checks: BreakpointChecks::NONE,
    steppable: true,
    records_coverage: true,
};

/// The Mega Drive's debugger: the 68000 in slot [`MAIN_CPU`], the Z80 in
/// slot [`SOUND_CPU`], and the console's own part.
pub type GenesisDebugger = DebuggerState<2, GenesisDebugConsole>;

/// A debugger for a machine described by `system_info`, running, with
/// nothing recorded.
pub fn genesis_debugger(system_info: SystemInfo) -> GenesisDebugger {
    DebuggerState::new(
        [MAIN_CPU_SPEC, SOUND_CPU_SPEC],
        system_info,
        GenesisDebugConsole::default(),
    )
}

/// Genesis event-breakpoint codes, carried in the `code` field of
/// [`retro_core::types::BreakpointType::Event`]. Users store them in their
/// breakpoint files, so a code is never renumbered.
///
/// The `param` field selects within an event (a line, a memory, a colour,
/// a tile); the events with no param report 0, so a breakpoint stored with
/// 0 or [`retro_core::types::BREAKPOINT_PARAM_ANY`] matches them.
pub mod genesis_events {
    /// The beam entered vertical blank (start of the VBlank region).
    pub const VBLANK: u32 = 1;
    /// A VBlank interrupt (68000 level 6) was delivered.
    pub const VINT: u32 = 2;
    /// An HBlank interrupt (68000 level 4) was delivered.
    pub const HINT: u32 = 3;
    /// A scanline was reached; `param` is the VDP `V` counter.
    pub const SCANLINE: u32 = 4;
    /// A transfer from 68000 memory, a fill or a copy has written all its
    /// data; `param` is the memory written: 0 VRAM, 1 CRAM, 2 VSRAM.
    pub const TRANSFER_COMPLETE: u32 = 5;
    /// A colour of CRAM was written, through the data port, by a transfer
    /// or by a fill; `param` is the colour, 0-63.
    pub const PALETTE_WRITE: u32 = 6;
    /// A transfer from 68000 memory, a fill or a copy wrote into the
    /// sprite table (no param).
    pub const SPRITE_TABLE_TRANSFER: u32 = 7;
    /// A write changed a byte of a tile in VRAM; `param` is the tile,
    /// 0-2047, 32 bytes each, as the tile viewer numbers them. Writing the
    /// bytes a tile already holds does not count.
    pub const TILE_REWRITTEN: u32 = 8;
}

/// The events a breakpoint can stop on, in the order a host lists them.
/// The interrupt events and a transfer into the sprite table take no param;
/// a scanline takes the `V` counter, a finished transfer the memory it
/// wrote, a palette write its colour and a rewritten tile its number. Every
/// one is checked on the debugger's tiers and their steps, none on the
/// play tier, so a breakpoint on any of them moves the frame onto the light
/// debug tier.
pub const GENESIS_EVENTS: &[EventSpec] = &[
    EventSpec {
        code: genesis_events::VBLANK,
        kind: EventKind::VerticalBlank,
        label: "VBlank",
        description: "The beam entered vertical blank",
        param: EventParam::None,
    },
    EventSpec {
        code: genesis_events::VINT,
        kind: EventKind::VerticalBlankInterrupt,
        label: "VInt",
        description: "A VBlank interrupt (68000 level 6) was delivered",
        param: EventParam::None,
    },
    EventSpec {
        code: genesis_events::HINT,
        kind: EventKind::RasterInterrupt,
        label: "HInt",
        description: "An HBlank interrupt (68000 level 4) was delivered",
        param: EventParam::None,
    },
    EventSpec {
        code: genesis_events::SCANLINE,
        kind: EventKind::Line,
        label: "Scanline",
        description: "A rendered scanline matched the V counter",
        param: EventParam::Number {
            label: "Scanline (V)",
        },
    },
    EventSpec {
        code: genesis_events::TRANSFER_COMPLETE,
        kind: EventKind::TransferDone,
        label: "Transfer complete",
        description: "A DMA from 68000 memory, a fill or a copy has written all its data",
        param: EventParam::Choice {
            label: "Memory",
            options: &[(0, "VRAM"), (1, "CRAM"), (2, "VSRAM")],
        },
    },
    EventSpec {
        code: genesis_events::PALETTE_WRITE,
        kind: EventKind::PaletteWrite,
        label: "Palette write",
        description: "A CRAM colour was written, through the data port, by a DMA or by a fill",
        param: EventParam::Number {
            label: "Colour (0-63)",
        },
    },
    EventSpec {
        code: genesis_events::SPRITE_TABLE_TRANSFER,
        kind: EventKind::SpriteTableWrite,
        label: "Sprite table transfer",
        description: "A DMA from 68000 memory, a fill or a copy wrote into the sprite table",
        param: EventParam::None,
    },
    EventSpec {
        code: genesis_events::TILE_REWRITTEN,
        kind: EventKind::TileWrite,
        label: "Tile rewritten",
        description: "A write changed the bytes of a tile in VRAM (writing the same bytes again \
                      does not count)",
        param: EventParam::Number {
            label: "Tile (0-2047)",
        },
    },
];

/// A Genesis event that an `Event` breakpoint can match.
///
/// Each variant is the `(code, param)` pair ([`DebugEvent::code_param`])
/// the host stores in [`retro_core::types::BreakpointType::Event`]. The
/// debugger's tiers and their steps make the variant where the event
/// happens and check it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenesisEvent {
    /// The beam entered vertical blank.
    VBlank,
    /// A VBlank interrupt (level 6) was delivered.
    VInt,
    /// An HBlank interrupt (level 4) was delivered.
    HInt,
    /// A scanline was reached (`V` counter).
    Scanline(u16),
    /// A transfer, a fill or a copy has written all its data into the
    /// memory named (0 VRAM, 1 CRAM, 2 VSRAM).
    TransferComplete(u8),
    /// A colour of CRAM (0-63) was written.
    PaletteWrite(u8),
    /// A transfer, a fill or a copy wrote into the sprite table.
    SpriteTableTransfer,
    /// A write changed a byte of the tile (0-2047).
    TileRewritten(u16),
}

impl DebugEvent for GenesisEvent {
    fn code_param(self) -> (u32, u32) {
        match self {
            GenesisEvent::VBlank => (genesis_events::VBLANK, 0),
            GenesisEvent::VInt => (genesis_events::VINT, 0),
            GenesisEvent::HInt => (genesis_events::HINT, 0),
            GenesisEvent::Scanline(v) => (genesis_events::SCANLINE, u32::from(v)),
            GenesisEvent::TransferComplete(target) => {
                (genesis_events::TRANSFER_COMPLETE, u32::from(target))
            }
            GenesisEvent::PaletteWrite(colour) => {
                (genesis_events::PALETTE_WRITE, u32::from(colour))
            }
            GenesisEvent::SpriteTableTransfer => (genesis_events::SPRITE_TABLE_TRANSFER, 0),
            GenesisEvent::TileRewritten(tile) => (genesis_events::TILE_REWRITTEN, u32::from(tile)),
        }
    }
}

impl From<VideoEvent> for GenesisEvent {
    fn from(event: VideoEvent) -> Self {
        match event {
            VideoEvent::TransferComplete(target) => Self::TransferComplete(target as u8),
            VideoEvent::PaletteWrite(colour) => Self::PaletteWrite(colour),
            VideoEvent::SpriteTableTransfer => Self::SpriteTableTransfer,
            VideoEvent::TileRewritten(tile) => Self::TileRewritten(tile),
        }
    }
}

/// An event the VDP raises is the Genesis event it is.
impl DebugEvent for VideoEvent {
    fn code_param(self) -> (u32, u32) {
        GenesisEvent::from(self).code_param()
    }
}

/// What the enabled event breakpoints on writes to video memory ask the VDP
/// to raise (a transfer finished, a palette colour written, a transfer into
/// the sprite table, a tile rewritten), or `None` when none does. A param of
/// [`retro_core::types::BREAKPOINT_PARAM_ANY`] watches every value; a param
/// no event carries watches nothing. Whether a raised event stops is still
/// the breakpoint check's to say (hit counts, conditions).
pub fn video_event_watch(breakpoints: &BreakpointList) -> Option<VideoEventWatch> {
    let asked_targets = breakpoints
        .watched_params::<1>(genesis_events::TRANSFER_COMPLETE)
        .unwrap_or_default();
    let mut transfer_targets = ParamSet::new();
    for target in [WriteTarget::Vram, WriteTarget::Cram, WriteTarget::Vsram] {
        if asked_targets.contains(target as u32) {
            transfer_targets.insert(target as u32);
        }
    }
    let watch = VideoEventWatch {
        transfer_targets,
        palette_colours: breakpoints
            .watched_params(genesis_events::PALETTE_WRITE)
            .unwrap_or_default(),
        sprite_table: breakpoints
            .watched_params::<1>(genesis_events::SPRITE_TABLE_TRANSFER)
            .is_some(),
        tiles: breakpoints
            .watched_params(genesis_events::TILE_REWRITTEN)
            .unwrap_or_default(),
    };
    (!watch.is_empty()).then_some(watch)
}

/// What the Mega Drive's debugger keeps besides its CPUs: the event
/// catalogue and the memory regions ([`ConsoleDebug`]), the addresses seen
/// as instruction starts, and the Z80 trace's running cycle count.
#[derive(Debug, Default)]
pub struct GenesisDebugConsole {
    /// Every address a host step has seen the 68000 start an instruction
    /// at, for [`GenesisEmulator::disassemble_around_pc`].
    instruction_boundaries: HashSet<u32>,
    /// The cycles the Z80's traced instructions have counted: the total an
    /// entry records before its own.
    z80_trace_cycles: u64,
}

impl GenesisDebugConsole {
    /// Note that the 68000 started an instruction at `address`.
    pub fn record_instruction_boundary(&mut self, address: u32) {
        self.instruction_boundaries.insert(address);
    }

    /// The `count` instruction starts seen nearest below `address`, in
    /// ascending order.
    pub fn get_boundaries_before(&self, address: u32, count: usize) -> Vec<u32> {
        let mut before: Vec<u32> = self
            .instruction_boundaries
            .iter()
            .filter(|&&start| start < address)
            .copied()
            .collect();
        before.sort_unstable();
        if before.len() > count {
            before.split_off(before.len() - count)
        } else {
            before
        }
    }

    /// Forget every instruction start seen (the machine was reset).
    pub fn clear_instruction_boundaries(&mut self) {
        self.instruction_boundaries.clear();
    }

    /// The Z80 trace's running cycle count before an instruction of
    /// `cycles` cycles, which it then adds.
    fn advance_z80_trace_cycles(&mut self, cycles: u16) -> u64 {
        let total = self.z80_trace_cycles;
        self.z80_trace_cycles += u64::from(cycles);
        total
    }
}

impl ConsoleDebug for GenesisDebugConsole {
    const EVENTS: &'static [EventSpec] = GENESIS_EVENTS;

    fn memory_regions(&self) -> Vec<MemoryRegionInfo> {
        vec![
            MemoryRegionInfo {
                region: MemoryRegion::MainRam,
                name: "Work RAM".to_string(),
                start_address: 0xFF0000,
                size: 0x10000,
                writable: true,
            },
            MemoryRegionInfo {
                region: MemoryRegion::VideoRam,
                name: "VRAM".to_string(),
                start_address: 0xC00000,
                size: 0x10000,
                writable: true,
            },
            MemoryRegionInfo {
                region: MemoryRegion::Rom,
                name: "ROM".to_string(),
                start_address: 0x000000,
                size: 0x400000,
                writable: false,
            },
            MemoryRegionInfo {
                region: MemoryRegion::AudioRam,
                name: "Z80 RAM".to_string(),
                start_address: 0xA00000,
                size: 0x2000,
                writable: true,
            },
        ]
    }
}

/// The size of a 68000 trace entry's bytes:
/// pc(4) + sr(2) + d0-d7(32) + a0-a7(32) + opcode(2) + cycles(2) +
/// total_cycles(8) + scanline(2).
pub const TRACE_ENTRY_SIZE: usize = 84;

/// One instruction of the 68000's trace: the CPU's state before it, the
/// cycles it took and where the beam was, for comparison with the traces
/// of other emulators.
///
/// Its bytes are every field little-endian, in the order the fields are
/// declared.
#[derive(Clone, Copy, Debug, Default)]
pub struct TraceEntryBinary {
    /// Program Counter before execution
    pub pc: u32,
    /// Status Register before execution
    pub sr: u16,
    /// Data registers D0-D7
    pub d: [u32; 8],
    /// Address registers A0-A7 (A7 is stack pointer)
    pub a: [u32; 8],
    /// First word of the instruction (opcode)
    pub opcode: u16,
    /// CPU cycles consumed by this instruction
    pub cycles: u16,
    /// Total CPU cycles executed before this instruction
    pub total_cycles: u64,
    /// Current VDP scanline
    pub scanline: u16,
}

impl TraceEntryBinary {
    /// An entry of these fields.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pc: u32,
        sr: u16,
        d: [u32; 8],
        a: [u32; 8],
        opcode: u16,
        cycles: u16,
        total_cycles: u64,
        scanline: u16,
    ) -> Self {
        Self {
            pc,
            sr,
            d,
            a,
            opcode,
            cycles,
            total_cycles,
            scanline,
        }
    }
}

impl TraceEntry for TraceEntryBinary {
    const SIZE: usize = TRACE_ENTRY_SIZE;
    type Bytes = [u8; TRACE_ENTRY_SIZE];

    // Each field at a fixed offset, so the compiler can drop the bounds
    // checks: an entry is made per instruction while the 68000 is traced.
    fn to_bytes(&self) -> Self::Bytes {
        let mut b = [0u8; TRACE_ENTRY_SIZE];
        b[0..4].copy_from_slice(&self.pc.to_le_bytes());
        b[4..6].copy_from_slice(&self.sr.to_le_bytes());
        for (i, d) in self.d.iter().enumerate() {
            b[6 + 4 * i..10 + 4 * i].copy_from_slice(&d.to_le_bytes());
        }
        for (i, a) in self.a.iter().enumerate() {
            b[38 + 4 * i..42 + 4 * i].copy_from_slice(&a.to_le_bytes());
        }
        b[70..72].copy_from_slice(&self.opcode.to_le_bytes());
        b[72..74].copy_from_slice(&self.cycles.to_le_bytes());
        b[74..82].copy_from_slice(&self.total_cycles.to_le_bytes());
        b[82..84].copy_from_slice(&self.scanline.to_le_bytes());
        b
    }

    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut fields = FieldReader::new(bytes.get(..TRACE_ENTRY_SIZE)?);
        let pc = u32::from_le_bytes(fields.take());
        let sr = u16::from_le_bytes(fields.take());
        let d = std::array::from_fn(|_| u32::from_le_bytes(fields.take()));
        let a = std::array::from_fn(|_| u32::from_le_bytes(fields.take()));
        Some(Self {
            pc,
            sr,
            d,
            a,
            opcode: u16::from_le_bytes(fields.take()),
            cycles: u16::from_le_bytes(fields.take()),
            total_cycles: u64::from_le_bytes(fields.take()),
            scanline: u16::from_le_bytes(fields.take()),
        })
    }
}

/// The line `PC:XXXXXX SR:XXXX D0:XXXXXXXX ... A7:XXXXXXXX | MNEMONIC`: the
/// registers the entry holds and the instruction disassembled from the
/// emulator's memory at its PC as memory is now (the entry's opcode field
/// names it only when nothing can be disassembled there).
impl TraceText<GenesisEmulator> for TraceEntryBinary {
    fn write_text(&self, emulator: &mut GenesisEmulator, out: &mut String) {
        let instructions = emulator.disassemble_at(self.pc, 1);
        let mnemonic = match instructions.first() {
            Some(instruction) => format!("{} {}", instruction.mnemonic, instruction.operands),
            None => format!("DC.W ${:04X}", self.opcode),
        };
        let _ = write!(
            out,
            "PC:{:06X} SR:{:04X} D0:{:08X} D1:{:08X} D2:{:08X} D3:{:08X} D4:{:08X} D5:{:08X} \
             D6:{:08X} D7:{:08X} A0:{:08X} A1:{:08X} A2:{:08X} A3:{:08X} A4:{:08X} A5:{:08X} \
             A6:{:08X} A7:{:08X} | {}",
            self.pc,
            self.sr,
            self.d[0],
            self.d[1],
            self.d[2],
            self.d[3],
            self.d[4],
            self.d[5],
            self.d[6],
            self.d[7],
            self.a[0],
            self.a[1],
            self.a[2],
            self.a[3],
            self.a[4],
            self.a[5],
            self.a[6],
            self.a[7],
            mnemonic
        );
    }
}

/// The size of a Z80 trace entry's bytes:
/// pc(2) + af(2) + bc(2) + de(2) + hl(2) + ix(2) + iy(2) + sp(2) + i(1) +
/// r(1) + im(1) + iff1(1) + iff2(1) + halt(1) + cycles(2) +
/// total_cycles(8) + scanline(2) + raw_bytes(4).
pub const Z80_TRACE_ENTRY_SIZE: usize = 38;

/// One instruction of the Z80's trace: the CPU's state before it and the
/// instruction's bytes, for comparison with MAME's Z80 traces.
///
/// Its bytes are every field little-endian, in the order the fields are
/// declared.
#[derive(Clone, Copy, Debug, Default)]
pub struct TraceEntryBinaryZ80 {
    pub pc: u16,
    pub af: u16,
    pub bc: u16,
    pub de: u16,
    pub hl: u16,
    pub ix: u16,
    pub iy: u16,
    pub sp: u16,
    pub i_reg: u8,
    pub r_reg: u8,
    pub im: u8,
    pub iff1: u8,
    pub iff2: u8,
    pub halt: u8,
    pub cycles: u16,
    pub total_cycles: u64,
    pub scanline: u16,
    /// The instruction's bytes at PC (up to 4, the longest Z80 instruction),
    /// read when the entry was made, so its line is disassembled from them
    /// rather than from Z80 RAM as it is later.
    pub raw_bytes: [u8; 4],
}

impl TraceEntry for TraceEntryBinaryZ80 {
    const SIZE: usize = Z80_TRACE_ENTRY_SIZE;
    type Bytes = [u8; Z80_TRACE_ENTRY_SIZE];

    // Each field at a fixed offset, as the 68000's entry.
    fn to_bytes(&self) -> Self::Bytes {
        let mut b = [0u8; Z80_TRACE_ENTRY_SIZE];
        let registers = [
            self.pc, self.af, self.bc, self.de, self.hl, self.ix, self.iy, self.sp,
        ];
        for (i, register) in registers.iter().enumerate() {
            b[2 * i..2 * i + 2].copy_from_slice(&register.to_le_bytes());
        }
        b[16..22].copy_from_slice(&[
            self.i_reg, self.r_reg, self.im, self.iff1, self.iff2, self.halt,
        ]);
        b[22..24].copy_from_slice(&self.cycles.to_le_bytes());
        b[24..32].copy_from_slice(&self.total_cycles.to_le_bytes());
        b[32..34].copy_from_slice(&self.scanline.to_le_bytes());
        b[34..38].copy_from_slice(&self.raw_bytes);
        b
    }

    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut fields = FieldReader::new(bytes.get(..Z80_TRACE_ENTRY_SIZE)?);
        let [pc, af, bc, de, hl, ix, iy, sp] =
            std::array::from_fn(|_| u16::from_le_bytes(fields.take()));
        let [i_reg, r_reg, im, iff1, iff2, halt] = fields.take();
        Some(Self {
            pc,
            af,
            bc,
            de,
            hl,
            ix,
            iy,
            sp,
            i_reg,
            r_reg,
            im,
            iff1,
            iff2,
            halt,
            cycles: u16::from_le_bytes(fields.take()),
            total_cycles: u64::from_le_bytes(fields.take()),
            scanline: u16::from_le_bytes(fields.take()),
            raw_bytes: fields.take(),
        })
    }
}

/// The line `PC:XXXX AF:XXXX BC:XXXX DE:XXXX HL:XXXX IX:XXXX IY:XXXX
/// SP:XXXX I:XX R:XX IM:X IFF1:X IFF2:X HALT:X | mnemonic`, MAME's Z80 trace
/// format, the instruction disassembled from the bytes the entry holds.
impl TraceText<()> for TraceEntryBinaryZ80 {
    fn write_text(&self, _: &mut (), out: &mut String) {
        let (instruction, _) = disasm::disassemble_from_bytes(self.pc, &self.raw_bytes);
        let _ = write!(
            out,
            "PC:{:04X} AF:{:04X} BC:{:04X} DE:{:04X} HL:{:04X} IX:{:04X} IY:{:04X} SP:{:04X} \
             I:{:02X} R:{:02X} IM:{} IFF1:{} IFF2:{} HALT:{} | {}",
            self.pc,
            self.af,
            self.bc,
            self.de,
            self.hl,
            self.ix,
            self.iy,
            self.sp,
            self.i_reg,
            self.r_reg,
            self.im,
            self.iff1,
            self.iff2,
            self.halt,
            disasm::to_trace_format(&instruction),
        );
    }
}

/// The trace entry of the Z80's next instruction: its registers as they
/// are, `raw_bytes` the instruction's bytes at its PC, `total_cycles` the
/// trace's running count before it.
pub fn z80_trace_entry(
    z80: &Z80,
    cycles: u16,
    total_cycles: u64,
    scanline: u16,
    raw_bytes: [u8; 4],
) -> TraceEntryBinaryZ80 {
    TraceEntryBinaryZ80 {
        pc: z80.pc,
        af: u16::from_be_bytes([z80.a, z80.f]),
        bc: u16::from_be_bytes([z80.b, z80.c]),
        de: u16::from_be_bytes([z80.d, z80.e]),
        hl: u16::from_be_bytes([z80.h, z80.l]),
        ix: z80.ix,
        iy: z80.iy,
        sp: z80.sp,
        i_reg: z80.i,
        r_reg: z80.r,
        im: z80.im,
        iff1: u8::from(z80.iff1),
        iff2: u8::from(z80.iff2),
        halt: u8::from(z80.halted),
        cycles,
        total_cycles,
        scanline,
        raw_bytes,
    }
}

/// Record the Z80's next instruction in its trace while the Z80 is traced.
/// The entry is made before the instruction runs, so it counts no cycles of
/// its own.
pub fn record_z80_trace(
    debugger: &mut GenesisDebugger,
    z80: &Z80,
    scanline: u16,
    raw_bytes: [u8; 4],
) {
    let cycles = 0;
    let total_cycles = debugger.console_mut().advance_z80_trace_cycles(cycles);
    let entry = z80_trace_entry(z80, cycles, total_cycles, scanline, raw_bytes);
    debugger.record_trace(SOUND_CPU, &entry);
}

/// Reads an entry's fields one after the other from its bytes, which hold
/// at least every field.
struct FieldReader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> FieldReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take<const N: usize>(&mut self) -> [u8; N] {
        let mut field = [0u8; N];
        field.copy_from_slice(&self.bytes[self.at..self.at + N]);
        self.at += N;
        field
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::debug::check_catalogue;
    use retro_core::types::ExecutionMode;
    use retro_core::types::{
        AudioInfo, Breakpoint, BreakpointId, BreakpointType, CpuArchitecture, CpuInfo, CpuLocation,
        CpuState, DisplayInfo, InputInfo, InstructionTrace, PixelFormat, RegisterValue, StackFrame,
        BREAKPOINT_PARAM_ANY,
    };
    use retro_core::DebuggerCore;

    fn cpu_info(id: usize, architecture: CpuArchitecture) -> CpuInfo {
        CpuInfo {
            id,
            name: "Test CPU".to_string(),
            architecture,
            clock_speed: 1000000,
            registers: vec![],
            location: CpuLocation::Motherboard,
            address_width: 24,
        }
    }

    fn create_test_debugger() -> GenesisDebugger {
        genesis_debugger(SystemInfo {
            name: "Test System".to_string(),
            short_name: "test".to_string(),
            manufacturer: "Test".to_string(),
            year: 2024,
            cpus: vec![
                cpu_info(MAIN_CPU, CpuArchitecture::M68000),
                cpu_info(SOUND_CPU, CpuArchitecture::Z80),
            ],
            display: DisplayInfo {
                width: 320,
                height: 224,
                aspect_ratio: 4.0 / 3.0,
                refresh_rate: 60.0,
                pixel_format: PixelFormat::RGBA8888,
            },
            audio: AudioInfo {
                sample_rate: 44100,
                channels: 2,
                bits_per_sample: 16,
            },
            input: InputInfo {
                max_players: 2,
                button_layout: vec![],
            },
        })
    }

    fn execute_at(address: u32) -> Breakpoint {
        Breakpoint {
            address,
            bp_type: BreakpointType::execute(),
            cpu_id: Some(MAIN_CPU),
            condition: None,
        }
    }

    fn instruction(address: u32) -> InstructionTrace {
        InstructionTrace {
            address,
            instruction: format!("NOP at {address:X}"),
            registers_before: vec![],
            cycle_count: 4,
        }
    }

    #[test]
    fn a_new_debugger_is_running_with_the_68000_focused() {
        let debugger = create_test_debugger();
        assert_eq!(debugger.get_execution_mode(), ExecutionMode::Running);
        assert_eq!(debugger.focused_cpu(), MAIN_CPU);
        assert!(debugger.list_breakpoints().is_empty());
    }

    #[test]
    fn each_cpu_covers_its_own_address_space() {
        let mut debugger = create_test_debugger();
        assert_eq!(debugger.coverage_stats(MAIN_CPU), (0x0100_0000, 0));
        assert_eq!(debugger.coverage_stats(SOUND_CPU), (0x1_0000, 0));
        assert_eq!(debugger.coverage_stats(2), (0, 0));

        // A 68000 PC with high bits set lands on the address it mirrors; a
        // Z80 address past 64 KiB on the one it wraps to.
        debugger.record_coverage(MAIN_CPU, 0xFF00_8000);
        debugger.record_coverage(SOUND_CPU, 0x1_0100);
        assert!(debugger.is_covered(MAIN_CPU, 0x8000));
        assert!(debugger.is_covered(SOUND_CPU, 0x100));
        assert!(!debugger.is_covered(MAIN_CPU, 0x100));
        assert!(!debugger.is_covered(2, 0x100));
    }

    #[test]
    fn each_cpu_has_its_trace_entry_and_its_breakpoint_checks() {
        let debugger = create_test_debugger();
        assert_eq!(debugger.traceable_cpu_count(), 2);
        assert_eq!(debugger.trace_entry_size(MAIN_CPU), Some(TRACE_ENTRY_SIZE));
        assert_eq!(
            debugger.trace_entry_size(SOUND_CPU),
            Some(Z80_TRACE_ENTRY_SIZE)
        );
        assert_eq!(debugger.breakpoint_checks(MAIN_CPU), BreakpointChecks::ALL);
        assert_eq!(
            debugger.breakpoint_checks(SOUND_CPU),
            BreakpointChecks::NONE
        );
    }

    #[test]
    fn breakpoints_are_added_listed_disabled_and_removed() {
        let mut debugger = create_test_debugger();
        let first = debugger.add_breakpoint(execute_at(0x400));
        let second = debugger.add_breakpoint(Breakpoint {
            address: 0x500,
            bp_type: BreakpointType::read(),
            cpu_id: Some(MAIN_CPU),
            condition: None,
        });
        assert_eq!((first, second), (0, 1));
        let listed = debugger.list_breakpoints();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].breakpoint.address, 0x400);
        assert!(listed[0].enabled);

        debugger.enable_breakpoint(first, false);
        assert!(!debugger.list_breakpoints()[0].enabled);
        debugger.enable_breakpoint(first, true);
        assert!(debugger.list_breakpoints()[0].enabled);

        debugger.remove_breakpoint(first);
        assert_eq!(debugger.list_breakpoints().len(), 1);
    }

    #[test]
    fn an_execute_breakpoint_stops_the_68000_at_its_address_only() {
        let mut debugger = create_test_debugger();
        debugger.add_breakpoint(execute_at(0x400));
        assert!(!debugger.check_execute(MAIN_CPU, 0x500, None));
        assert!(!debugger.check_execute(SOUND_CPU, 0x400, None));
        assert!(debugger.check_execute(MAIN_CPU, 0x400, None));
    }

    #[test]
    fn the_history_keeps_the_newest_and_gives_them_newest_first() {
        let mut debugger = create_test_debugger();
        for index in 0..1100 {
            debugger.record_history(MAIN_CPU, instruction(index * 2));
        }
        let history = debugger.get_instruction_history(MAIN_CPU, 2000);
        assert_eq!(history.len(), 1024);
        assert_eq!(history[0].address, 1099 * 2);
        assert_eq!(history[1023].address, (1099 - 1023) * 2);

        // The Z80 keeps a history of its own.
        debugger.record_history(SOUND_CPU, instruction(0x38));
        let z80 = debugger.get_instruction_history(SOUND_CPU, 10);
        assert_eq!(z80.len(), 1);
        assert_eq!(z80[0].address, 0x38);
    }

    #[test]
    fn calls_and_returns_build_the_68000_call_stack() {
        let mut debugger = create_test_debugger();
        let frame = |call_address, return_address| StackFrame {
            call_address,
            return_address,
            function_name: None,
        };
        debugger.push_call(MAIN_CPU, frame(0x200, 0x206));
        debugger.push_call(MAIN_CPU, frame(0x400, 0x404));
        let returns = |debugger: &GenesisDebugger| -> Vec<u32> {
            debugger
                .get_call_stack(MAIN_CPU)
                .iter()
                .map(|frame| frame.return_address)
                .collect()
        };
        assert_eq!(returns(&debugger), [0x206, 0x404]);

        debugger.pop_call(MAIN_CPU);
        assert_eq!(returns(&debugger), [0x206]);
        assert!(debugger.get_call_stack(SOUND_CPU).is_empty());
    }

    #[test]
    fn the_cpu_states_are_the_ones_the_emulator_wrote() {
        let mut debugger = create_test_debugger();
        debugger.set_cpu_states(vec![CpuState {
            cpu_id: MAIN_CPU,
            registers: vec![RegisterValue {
                name: "PC".to_string(),
                value: 0x400,
            }],
            pc: 0x400,
            flags: 0x2700,
        }]);
        let states = debugger.get_cpu_states();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].pc, 0x400);
    }

    #[test]
    fn the_memory_regions_name_the_z80_ram_as_audio_ram() {
        let debugger = create_test_debugger();
        let regions: Vec<(MemoryRegion, String, u32, u32, bool)> = debugger
            .get_memory_regions()
            .into_iter()
            .map(|region| {
                (
                    region.region,
                    region.name,
                    region.start_address,
                    region.size,
                    region.writable,
                )
            })
            .collect();
        let expected = [
            (MemoryRegion::MainRam, "Work RAM", 0xFF0000, 0x10000, true),
            (MemoryRegion::VideoRam, "VRAM", 0xC00000, 0x10000, true),
            (MemoryRegion::Rom, "ROM", 0, 0x400000, false),
            (MemoryRegion::AudioRam, "Z80 RAM", 0xA00000, 0x2000, true),
        ]
        .map(|(region, name, start, size, writable)| {
            (region, name.to_string(), start, size, writable)
        });
        assert_eq!(regions, expected);
    }

    #[test]
    fn the_catalogue_lists_every_event_with_its_kind() {
        assert_eq!(check_catalogue(GENESIS_EVENTS), Ok(()));
        let debugger = create_test_debugger();
        let events: Vec<(u32, EventKind)> = debugger
            .available_events()
            .iter()
            .map(|event| (event.code, event.kind))
            .collect();
        assert_eq!(
            events,
            [
                (genesis_events::VBLANK, EventKind::VerticalBlank),
                (genesis_events::VINT, EventKind::VerticalBlankInterrupt),
                (genesis_events::HINT, EventKind::RasterInterrupt),
                (genesis_events::SCANLINE, EventKind::Line),
                (genesis_events::TRANSFER_COMPLETE, EventKind::TransferDone),
                (genesis_events::PALETTE_WRITE, EventKind::PaletteWrite),
                (
                    genesis_events::SPRITE_TABLE_TRANSFER,
                    EventKind::SpriteTableWrite
                ),
                (genesis_events::TILE_REWRITTEN, EventKind::TileWrite),
            ]
        );
        let transfer = debugger
            .available_events()
            .into_iter()
            .find(|event| event.code == genesis_events::TRANSFER_COMPLETE)
            .unwrap();
        let values: Vec<u32> = transfer.param_options.iter().map(|o| o.value).collect();
        assert_eq!(values, [0, 1, 2]);
    }

    #[test]
    fn each_event_is_its_code_and_param() {
        assert_eq!(
            GenesisEvent::VBlank.code_param(),
            (genesis_events::VBLANK, 0)
        );
        assert_eq!(GenesisEvent::VInt.code_param(), (genesis_events::VINT, 0));
        assert_eq!(GenesisEvent::HInt.code_param(), (genesis_events::HINT, 0));
        assert_eq!(
            GenesisEvent::Scanline(224).code_param(),
            (genesis_events::SCANLINE, 224)
        );
        assert_eq!(
            GenesisEvent::TransferComplete(1).code_param(),
            (genesis_events::TRANSFER_COMPLETE, 1)
        );
        assert_eq!(
            GenesisEvent::PaletteWrite(63).code_param(),
            (genesis_events::PALETTE_WRITE, 63)
        );
        assert_eq!(
            GenesisEvent::SpriteTableTransfer.code_param(),
            (genesis_events::SPRITE_TABLE_TRANSFER, 0)
        );
        assert_eq!(
            GenesisEvent::TileRewritten(2047).code_param(),
            (genesis_events::TILE_REWRITTEN, 2047)
        );
        assert_eq!(
            VideoEvent::TransferComplete(WriteTarget::Vsram).code_param(),
            (genesis_events::TRANSFER_COMPLETE, 2)
        );
        assert_eq!(
            VideoEvent::TileRewritten(7).code_param(),
            (genesis_events::TILE_REWRITTEN, 7)
        );
    }

    fn event_breakpoint(debugger: &mut GenesisDebugger, code: u32, param: u32) -> BreakpointId {
        debugger.add_breakpoint(Breakpoint {
            address: 0,
            bp_type: BreakpointType::Event { code, param },
            cpu_id: None,
            condition: None,
        })
    }

    fn params<const W: usize>(values: &[u32]) -> ParamSet<W> {
        let mut set = ParamSet::new();
        for &value in values {
            set.insert(value);
        }
        set
    }

    fn watch(
        targets: &[WriteTarget],
        colours: &[u32],
        sprite_table: bool,
        tiles: &[u32],
    ) -> VideoEventWatch {
        let targets: Vec<u32> = targets.iter().map(|&target| target as u32).collect();
        VideoEventWatch {
            transfer_targets: params(&targets),
            palette_colours: params(colours),
            sprite_table,
            tiles: params(tiles),
        }
    }

    #[test]
    fn the_video_watch_holds_what_the_enabled_breakpoints_ask() {
        let mut debugger = create_test_debugger();
        assert!(video_event_watch(debugger.breakpoints()).is_none());
        // Breakpoints on other events ask the VDP for nothing; nor does a
        // transfer into a memory the VDP does not have.
        event_breakpoint(&mut debugger, genesis_events::VBLANK, 0);
        event_breakpoint(&mut debugger, genesis_events::SCANLINE, 10);
        event_breakpoint(&mut debugger, genesis_events::TRANSFER_COMPLETE, 3);
        assert!(video_event_watch(debugger.breakpoints()).is_none());

        let colour = event_breakpoint(&mut debugger, genesis_events::PALETTE_WRITE, 5);
        assert_eq!(
            video_event_watch(debugger.breakpoints()),
            Some(watch(&[], &[5], false, &[]))
        );

        event_breakpoint(&mut debugger, genesis_events::TRANSFER_COMPLETE, 1);
        event_breakpoint(&mut debugger, genesis_events::TILE_REWRITTEN, 7);
        event_breakpoint(&mut debugger, genesis_events::SPRITE_TABLE_TRANSFER, 0);
        assert_eq!(
            video_event_watch(debugger.breakpoints()),
            Some(watch(&[WriteTarget::Cram], &[5], true, &[7]))
        );

        // A disabled breakpoint asks for nothing; one on any value, for all.
        debugger.enable_breakpoint(colour, false);
        assert_eq!(
            video_event_watch(debugger.breakpoints()),
            Some(watch(&[WriteTarget::Cram], &[], true, &[7]))
        );
        event_breakpoint(
            &mut debugger,
            genesis_events::TRANSFER_COMPLETE,
            BREAKPOINT_PARAM_ANY,
        );
        assert_eq!(
            video_event_watch(debugger.breakpoints()),
            Some(watch(
                &[WriteTarget::Vram, WriteTarget::Cram, WriteTarget::Vsram],
                &[],
                true,
                &[7]
            ))
        );
    }

    #[test]
    fn an_event_reaches_the_breakpoints_on_its_code_and_param() {
        let mut debugger = create_test_debugger();
        assert!(!debugger.has_watchpoints_or_events());
        assert!(!debugger.check_event(GenesisEvent::VBlank));

        event_breakpoint(&mut debugger, genesis_events::VBLANK, 0);
        event_breakpoint(&mut debugger, genesis_events::SCANLINE, 100);
        assert!(debugger.has_watchpoints_or_events());
        assert!(debugger.check_event(GenesisEvent::VBlank));
        assert!(!debugger.check_event(GenesisEvent::HInt));
        assert!(debugger.check_event(GenesisEvent::Scanline(100)));
        assert!(!debugger.check_event(GenesisEvent::Scanline(101)));
    }
}
