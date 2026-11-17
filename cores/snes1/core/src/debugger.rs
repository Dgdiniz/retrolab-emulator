//! SNES debugger.
//!
//! [`SnesDebugger`] is retro-core's [`DebuggerState`] with the SNES's three
//! CPU slots: the 65C816 ([`MAIN_CPU`]), the SPC700 ([`SOUND_CPU`]) and the
//! cartridge's own CPU ([`CARTRIDGE_CPU`]: the SA-1's second 65816, the
//! Super FX's GSU or the Cx4) when the cartridge has one; and
//! [`SnesDebugConsole`], the console's own part: its event catalogue and
//! its memory regions. The breakpoints, the coverage, the trace rings, the
//! 65C816's instruction history and call stack and the cached CPU states
//! all live in the shared state; the emulator records into it and checks
//! it from its frame loops (`system.rs`).
//!
//! What stays here is what the SNES makes different: the event codes users
//! store ([`snes_events`], [`SnesEvent`]), each CPU's trace entry and the
//! register snapshots of each CPU (`cpu_state_*`).
//!
//! The headline feature is the **65C816 instruction trace**: per
//! instruction the emulator captures a compact [`TraceEntryBinary65816`]
//! (registers + raster position + instruction bytes + resolved effective
//! address) into the 65C816's trace ring. The host drains the ring and
//! renders each entry to text with
//! [`crate::trace_format::format_trace_line_65816`], reproducing the
//! bsnes-plus trace format byte for byte so the two can be diffed.
//!
//! The SPC700 has the same shape: a ring of [`TraceEntryBinarySpc700`]
//! records rendered by [`crate::trace_format::format_trace_line_spc700`].
//! Because the SPC700 runs via lazy catch-up (not instruction-stepped in
//! the 65C816 loop), its entries are captured inside the APU's catch-up and
//! moved into the debugger by the emulator; the Super FX's and the Cx4's
//! entries come out of their chips the same way.

use retro_core::debug::{
    BreakpointChecks, ConsoleDebug, CpuDebugSpec, DebugEvent, DebuggerState, EventParam, EventSpec,
    TraceEntry,
};
use retro_core::types::{
    CpuArchitecture, CpuState, EventKind, MemoryRegion, MemoryRegionInfo, RegisterValue, SystemInfo,
};
use spc700::Spc700;
#[cfg(feature = "chip-superfx")]
use superfx_gsu::Gsu;

#[cfg(feature = "chip-cx4")]
use crate::cartridge::chips::cx4::hg51b::Hg51b;
use crate::ppu::PpuDebugEvent;
use wdc65816::Wdc65816;

/// The 65C816, the 5A22's CPU: the CPU the frame loop steps.
pub use retro_core::debug::MAIN_CPU;

/// The SPC700, the sound CPU.
pub const SOUND_CPU: usize = 1;

/// The cartridge's own CPU, when it has one: the SA-1's second 65816, the
/// Super FX's GSU or the Cx4.
pub const CARTRIDGE_CPU: usize = 2;

/// The 65C816's slot: coverage over its 24-bit space, a trace, an
/// instruction history and a call stack, and every breakpoint check.
const MAIN_CPU_SPEC: CpuDebugSpec = CpuDebugSpec {
    coverage_size: 0x0100_0000,
    address_mask: 0x00FF_FFFF,
    trace_entry_size: <TraceEntryBinary65816 as TraceEntry>::SIZE,
    history: true,
    checks: BreakpointChecks::ALL,
};

/// The SPC700's slot: coverage over its 16-bit space and a trace. Its
/// breakpoints are not checked: it runs inside the APU's catch-up, between
/// two of the 65C816's instructions, where no loop of the emulator stops.
const SOUND_CPU_SPEC: CpuDebugSpec = CpuDebugSpec {
    coverage_size: 0x0001_0000,
    address_mask: 0xFFFF,
    trace_entry_size: <TraceEntryBinarySpc700 as TraceEntry>::SIZE,
    history: false,
    checks: BreakpointChecks::NONE,
};

/// The slot of the cartridge's own CPU, by the architecture it reports: the
/// Super FX's GSU and the Cx4 keep a trace, the SA-1's second 65816 does
/// not, and a cartridge with no CPU of its own (`None`) leaves the slot
/// empty. None keeps coverage or a history, and none has its breakpoints
/// checked: each runs inside its chip's catch-up.
pub fn cartridge_cpu_spec(architecture: Option<CpuArchitecture>) -> CpuDebugSpec {
    let trace_entry_size = match architecture {
        Some(CpuArchitecture::SuperFx) => <TraceEntryBinaryGsu as TraceEntry>::SIZE,
        Some(CpuArchitecture::Hg51b) => <TraceEntryBinaryCx4 as TraceEntry>::SIZE,
        _ => return CpuDebugSpec::NO_CPU,
    };
    CpuDebugSpec {
        trace_entry_size,
        ..CpuDebugSpec::NO_CPU
    }
}

/// The SNES's debugger: the shared state over the console's three CPU
/// slots and its own part.
pub type SnesDebugger = DebuggerState<3, SnesDebugConsole>;

/// The debugger of an SNES described by `system_info`: no cartridge CPU yet
/// (slot 2 empty until a cartridge brings one), running, and the CPU states
/// it reports those of a 65C816 and an SPC700 just made.
pub fn snes_debugger(system_info: SystemInfo) -> SnesDebugger {
    let mut debugger = DebuggerState::new(
        [MAIN_CPU_SPEC, SOUND_CPU_SPEC, CpuDebugSpec::NO_CPU],
        system_info,
        SnesDebugConsole::default(),
    );
    debugger.set_cpu_states(vec![
        cpu_state_65816(&Wdc65816::new()),
        cpu_state_spc700(&Spc700::new()),
    ]);
    debugger
}

/// The SNES's own part of its debugger: the event catalogue, and the sizes
/// of the loaded ROM and of its save RAM, which the memory regions report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SnesDebugConsole {
    rom_size: u32,
    sram_size: u32,
}

impl SnesDebugConsole {
    /// Record the loaded ROM's size and its save RAM's, in bytes.
    pub fn set_memory_sizes(&mut self, rom_size: u32, sram_size: u32) {
        self.rom_size = rom_size;
        self.sram_size = sram_size;
    }
}

impl ConsoleDebug for SnesDebugConsole {
    /// The SNES's events, in the order a host lists them. Five carry no
    /// param (VBlank, NMI, IRQ, DMA start, HDMA start); the APU ports and the
    /// ports a DMA fills are offered as choices, the rest as numbers.
    const EVENTS: &'static [EventSpec] = &[
        EventSpec {
            code: snes_events::VBLANK,
            kind: EventKind::VerticalBlank,
            label: "VBlank",
            description: "Entering vertical blank",
            param: EventParam::None,
        },
        EventSpec {
            code: snes_events::NMI,
            kind: EventKind::VerticalBlankInterrupt,
            label: "NMI",
            description: "V-blank NMI delivered to the 65C816",
            param: EventParam::None,
        },
        EventSpec {
            code: snes_events::IRQ,
            kind: EventKind::RasterInterrupt,
            label: "IRQ",
            description: "H/V-timer IRQ raised",
            param: EventParam::None,
        },
        EventSpec {
            code: snes_events::DMA_START,
            kind: EventKind::TransferStart,
            label: "DMA start",
            description: "General-purpose DMA block started ($420B)",
            param: EventParam::None,
        },
        EventSpec {
            code: snes_events::HDMA_START,
            kind: EventKind::LineTransferSetUp,
            label: "HDMA start",
            description: "Top-of-frame HDMA initialisation",
            param: EventParam::None,
        },
        EventSpec {
            code: snes_events::SCANLINE,
            kind: EventKind::Line,
            label: "Scanline",
            description: "A rendered scanline matched the V counter",
            param: EventParam::Number {
                label: "Scanline (V)",
            },
        },
        EventSpec {
            code: snes_events::PPU_REG_WRITE,
            kind: EventKind::VideoRegisterWrite,
            label: "PPU register write",
            description: "Write to a PPU register ($2100-$213F)",
            param: EventParam::Number {
                label: "Register ($00-$3F)",
            },
        },
        EventSpec {
            code: snes_events::APU_PORT_WRITE,
            kind: EventKind::AudioPortWrite,
            label: "APU port write",
            description: "65C816 write to an APU mailbox port ($2140-$2143)",
            param: EventParam::Choice {
                label: "Port",
                options: &[(0, "Port 0"), (1, "Port 1"), (2, "Port 2"), (3, "Port 3")],
            },
        },
        EventSpec {
            code: snes_events::DSP_REG_WRITE,
            kind: EventKind::AudioRegisterWrite,
            label: "S-DSP register write",
            description: "SPC700 write to an S-DSP register",
            param: EventParam::Number {
                label: "DSP register ($00-$7F)",
            },
        },
        EventSpec {
            code: snes_events::DMA_DONE,
            kind: EventKind::TransferDone,
            label: "DMA done",
            description: "A general DMA channel finished its block, by the B-bus port it wrote",
            param: EventParam::Choice {
                label: "B-bus port",
                options: &[
                    (0x18, "VRAM ($2118)"),
                    (0x22, "Palette ($2122)"),
                    (0x04, "Sprite table ($2104)"),
                    (0x80, "Work RAM ($2180)"),
                ],
            },
        },
        EventSpec {
            code: snes_events::PALETTE_WRITE,
            kind: EventKind::PaletteWrite,
            label: "Palette write",
            description: "A colour written into CGRAM by the processor, a DMA or an HDMA",
            param: EventParam::Number {
                label: "Colour (0-255)",
            },
        },
        EventSpec {
            code: snes_events::TILE_CHANGE,
            kind: EventKind::TileWrite,
            label: "Tile changed",
            description: "A write into VRAM changed a tile's bytes (the same bytes written again do not count)",
            param: EventParam::Number {
                label: "Tile (4bpp, 0-2047)",
            },
        },
    ];

    fn memory_regions(&self) -> Vec<MemoryRegionInfo> {
        vec![
            MemoryRegionInfo {
                region: MemoryRegion::MainRam,
                name: "WRAM".to_string(),
                start_address: 0x7E_0000,
                size: 0x0002_0000,
                writable: true,
            },
            MemoryRegionInfo {
                region: MemoryRegion::VideoRam,
                name: "VRAM".to_string(),
                start_address: 0,
                size: 0x0001_0000,
                writable: true,
            },
            MemoryRegionInfo {
                region: MemoryRegion::SpriteTable,
                name: "OAM".to_string(),
                start_address: 0,
                size: 0x0220,
                writable: true,
            },
            MemoryRegionInfo {
                region: MemoryRegion::PaletteRam,
                name: "CGRAM".to_string(),
                start_address: 0,
                size: 0x0200,
                writable: true,
            },
            MemoryRegionInfo {
                region: MemoryRegion::AudioRam,
                name: "ARAM".to_string(),
                start_address: 0,
                size: 0x0001_0000,
                writable: true,
            },
            MemoryRegionInfo {
                region: MemoryRegion::Rom,
                name: "ROM".to_string(),
                start_address: 0,
                size: self.rom_size,
                writable: false,
            },
            MemoryRegionInfo {
                region: MemoryRegion::SaveRam,
                name: "SRAM".to_string(),
                start_address: 0,
                size: self.sram_size,
                writable: true,
            },
        ]
    }
}

/// SNES event-breakpoint codes, carried in the `code` field of
/// [`retro_core::types::BreakpointType::Event`].
///
/// These numbers are the SNES core's own; users store them in their
/// breakpoint files, so none is ever renumbered. Each is paired with the
/// matching [`SnesEvent`] variant and, in the catalogue, with the
/// [`EventKind`] every console shares. The `param` field disambiguates
/// within a class (a scanline, a register index, a port); paramless classes
/// (VBlank/NMI/IRQ/DMA/HDMA) report `param == 0`, so a breakpoint stored
/// with `param == 0` or [`retro_core::types::BREAKPOINT_PARAM_ANY`] matches
/// them.
pub mod snes_events {
    /// Entering V-blank (the `VBlankStart` scheduler event).
    pub const VBLANK: u32 = 1;
    /// A V-blank NMI delivered to the 65C816 (NMITIMEN bit 7 enabled).
    pub const NMI: u32 = 2;
    /// An H/V-timer IRQ raised on the 65C816.
    pub const IRQ: u32 = 3;
    /// A general-purpose DMA block started (`$420B` write serviced).
    pub const DMA_START: u32 = 4;
    /// Top-of-frame HDMA initialisation.
    pub const HDMA_START: u32 = 5;
    /// A rendered scanline matched; `param` is the `V` counter.
    pub const SCANLINE: u32 = 6;
    /// A write to a PPU register (`$2100-$213F`); `param` is the
    /// register's low byte (`$00-$3F`).
    pub const PPU_REG_WRITE: u32 = 7;
    /// A 65C816 write to an APU mailbox port (`$2140-$2143`); `param`
    /// is the port index (0-3).
    pub const APU_PORT_WRITE: u32 = 8;
    /// An SPC700 write to an S-DSP register (via `$F3` DSPDATA);
    /// `param` is the DSP register index (`$00-$7F`).
    pub const DSP_REG_WRITE: u32 = 9;
    /// A general DMA channel finished its block; `param` is the channel's
    /// B-bus port (`$18` VRAM, `$22` palette, `$04` sprite table, `$80`
    /// work RAM). One event per channel of a `$420B` write, checked after
    /// the whole write's blocks ran, in the same place as [`DMA_START`].
    pub const DMA_DONE: u32 = 10;
    /// A colour was written into CGRAM by the processor, a DMA or an HDMA
    /// (not by the debugger); `param` is the colour, 0-255. An HDMA's
    /// writes pause after the line's batch of events.
    pub const PALETTE_WRITE: u32 = 11;
    /// A write into VRAM changed a byte of a tile; `param` is the tile in
    /// 4bpp tiles, the VRAM word address divided by 16 (0-2047), the tile
    /// viewer's numbering. Writing the same bytes again does not count,
    /// nor do the debugger's writes.
    pub const TILE_CHANGE: u32 = 12;
}

/// A SNES debug event that can be matched by an `Event` breakpoint.
///
/// Each variant names one of the [`snes_events`] codes; its
/// [`DebugEvent::code_param`] is the `(code, param)` pair the host stores in
/// [`retro_core::types::BreakpointType::Event`]. The hook sites in
/// `system.rs` / `bus.rs` / `apu` build the variant and check it against
/// the breakpoints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SnesEvent {
    /// Entering V-blank.
    VBlank,
    /// A V-blank NMI was delivered.
    Nmi,
    /// An H/V-timer IRQ was raised.
    Irq,
    /// A general DMA block started.
    DmaStart,
    /// HDMA was initialised at the top of the frame.
    HdmaStart,
    /// A scanline was rendered (`V` counter).
    Scanline(u16),
    /// A PPU register at `$2100 | reg` was written (`reg` is `$00-$3F`).
    PpuRegisterWrite(u8),
    /// An APU mailbox port (0-3) was written by the 65C816.
    ApuPortWrite(u8),
    /// An S-DSP register (`$00-$7F`) was written by the SPC700.
    DspRegisterWrite(u8),
    /// A general DMA channel finished its block (its B-bus port).
    DmaDone(u8),
    /// A colour was written into CGRAM (the colour, 0-255).
    PaletteWrite(u8),
    /// A write into VRAM changed a tile (in 4bpp tiles, 0-2047).
    TileChange(u16),
}

impl DebugEvent for SnesEvent {
    /// The event's code and param; paramless events report `param == 0`.
    fn code_param(self) -> (u32, u32) {
        match self {
            SnesEvent::VBlank => (snes_events::VBLANK, 0),
            SnesEvent::Nmi => (snes_events::NMI, 0),
            SnesEvent::Irq => (snes_events::IRQ, 0),
            SnesEvent::DmaStart => (snes_events::DMA_START, 0),
            SnesEvent::HdmaStart => (snes_events::HDMA_START, 0),
            SnesEvent::Scanline(v) => (snes_events::SCANLINE, u32::from(v)),
            SnesEvent::PpuRegisterWrite(reg) => (snes_events::PPU_REG_WRITE, u32::from(reg)),
            SnesEvent::ApuPortWrite(port) => (snes_events::APU_PORT_WRITE, u32::from(port)),
            SnesEvent::DspRegisterWrite(reg) => (snes_events::DSP_REG_WRITE, u32::from(reg)),
            SnesEvent::DmaDone(port) => (snes_events::DMA_DONE, u32::from(port)),
            SnesEvent::PaletteWrite(colour) => (snes_events::PALETTE_WRITE, u32::from(colour)),
            SnesEvent::TileChange(tile) => (snes_events::TILE_CHANGE, u32::from(tile)),
        }
    }
}

/// The PPU's events are the palette-write and tile-change events of
/// [`SnesEvent`], with the same codes and params.
impl DebugEvent for PpuDebugEvent {
    fn code_param(self) -> (u32, u32) {
        match self {
            PpuDebugEvent::PaletteWrite(colour) => SnesEvent::PaletteWrite(colour).code_param(),
            PpuDebugEvent::TileChange(tile) => SnesEvent::TileChange(tile).code_param(),
        }
    }
}

/// Per-instruction binary trace record for the 65C816.
///
/// Captured **before** the instruction executes (matching bsnes-plus),
/// this carries everything [`crate::trace_format::format_trace_line_65816`]
/// needs to reconstruct the exact bsnes-plus trace line without touching
/// the bus, including the already-resolved effective address (the only
/// field that depends on memory for indirect addressing modes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TraceEntryBinary65816 {
    /// Master clock at the instruction boundary (not printed; ordering aid).
    pub master_cycle: u64,
    /// `PBR << 16 | PC`: the 24-bit instruction address.
    pub pbr_pc: u32,
    /// Accumulator (full 16-bit `C`).
    pub a: u16,
    pub x: u16,
    pub y: u16,
    /// Stack pointer.
    pub s: u16,
    /// Direct-page register.
    pub d: u16,
    /// PPU scanline (`V`).
    pub vcounter: u16,
    /// PPU dot within the scanline (`H`, 0..=339).
    pub hdot: u16,
    /// Frame counter (`F`).
    pub framecounter: u32,
    /// Resolved effective address (valid only when `eff_present != 0`).
    pub eff_addr: u32,
    /// Data-bank register.
    pub dbr: u8,
    /// Processor status `P`.
    pub p: u8,
    /// Emulation-mode flag (`1` = emulation, `0` = native).
    pub e: u8,
    /// `1` when the addressing mode prints an `[effective]` bracket.
    pub eff_present: u8,
    /// Number of valid bytes in `bytes` (1..=4).
    pub bytes_len: u8,
    /// Instruction byte count cost hint (base cycles; not printed).
    pub cycles: u8,
    /// The instruction's raw bytes (opcode + up to 3 operand bytes).
    pub bytes: [u8; 4],
}

impl TraceEntry for TraceEntryBinary65816 {
    /// 44 bytes, all little-endian: `master_cycle(8) pbr_pc(4) a(2) x(2)
    /// y(2) s(2) d(2) vcounter(2) hdot(2) framecounter(4) eff_addr(4) dbr(1)
    /// p(1) e(1) eff_present(1) bytes_len(1) cycles(1) bytes(4)`.
    const SIZE: usize = 44;
    type Bytes = [u8; 44];

    fn to_bytes(&self) -> [u8; 44] {
        let mut b = [0u8; 44];
        b[0..8].copy_from_slice(&self.master_cycle.to_le_bytes());
        b[8..12].copy_from_slice(&self.pbr_pc.to_le_bytes());
        b[12..14].copy_from_slice(&self.a.to_le_bytes());
        b[14..16].copy_from_slice(&self.x.to_le_bytes());
        b[16..18].copy_from_slice(&self.y.to_le_bytes());
        b[18..20].copy_from_slice(&self.s.to_le_bytes());
        b[20..22].copy_from_slice(&self.d.to_le_bytes());
        b[22..24].copy_from_slice(&self.vcounter.to_le_bytes());
        b[24..26].copy_from_slice(&self.hdot.to_le_bytes());
        b[26..30].copy_from_slice(&self.framecounter.to_le_bytes());
        b[30..34].copy_from_slice(&self.eff_addr.to_le_bytes());
        b[34] = self.dbr;
        b[35] = self.p;
        b[36] = self.e;
        b[37] = self.eff_present;
        b[38] = self.bytes_len;
        b[39] = self.cycles;
        b[40..44].copy_from_slice(&self.bytes);
        b
    }

    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let mut raw = [0u8; 4];
        raw.copy_from_slice(&bytes[40..44]);
        Some(Self {
            master_cycle: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
            pbr_pc: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            a: u16::from_le_bytes(bytes[12..14].try_into().ok()?),
            x: u16::from_le_bytes(bytes[14..16].try_into().ok()?),
            y: u16::from_le_bytes(bytes[16..18].try_into().ok()?),
            s: u16::from_le_bytes(bytes[18..20].try_into().ok()?),
            d: u16::from_le_bytes(bytes[20..22].try_into().ok()?),
            vcounter: u16::from_le_bytes(bytes[22..24].try_into().ok()?),
            hdot: u16::from_le_bytes(bytes[24..26].try_into().ok()?),
            framecounter: u32::from_le_bytes(bytes[26..30].try_into().ok()?),
            eff_addr: u32::from_le_bytes(bytes[30..34].try_into().ok()?),
            dbr: bytes[34],
            p: bytes[35],
            e: bytes[36],
            eff_present: bytes[37],
            bytes_len: bytes[38],
            cycles: bytes[39],
            bytes: raw,
        })
    }
}

/// Per-instruction binary trace record for the SPC700.
///
/// Captured **before** the instruction executes (matching bsnes-plus and
/// the 65C816 model), inside `Apu::catch_up_to`. Everything
/// [`crate::trace_format::format_trace_line_spc700`] needs lives here, so
/// the formatter stays bus-free: the SPC700 registers are all 8-bit and
/// the disassembler reconstructs the operand text from the captured
/// `bytes` plus the `P` flag carried in `psw`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TraceEntryBinarySpc700 {
    /// APU master cycle at the instruction boundary (ordering aid; not printed).
    pub master_cycle: u64,
    /// 16-bit program counter (the flat SPC700 address space).
    pub pc: u16,
    /// Accumulator.
    pub a: u8,
    pub x: u8,
    pub y: u8,
    /// Stack pointer low byte (the stack always lives in page 1).
    pub sp: u8,
    /// Program status word `NVPBHIZC` (also carries `P` for DP folding).
    pub psw: u8,
    /// Number of valid bytes in `bytes` (1..=3).
    pub bytes_len: u8,
    /// The instruction's raw bytes (opcode + up to 2 operand bytes).
    pub bytes: [u8; 3],
}

impl TraceEntry for TraceEntryBinarySpc700 {
    /// 24 bytes, all little-endian: `master_cycle(8) pc(2) a(1) x(1) y(1)
    /// sp(1) psw(1) bytes_len(1) bytes(3)` followed by 5 reserved bytes.
    const SIZE: usize = 24;
    type Bytes = [u8; 24];

    fn to_bytes(&self) -> [u8; 24] {
        let mut b = [0u8; 24];
        b[0..8].copy_from_slice(&self.master_cycle.to_le_bytes());
        b[8..10].copy_from_slice(&self.pc.to_le_bytes());
        b[10] = self.a;
        b[11] = self.x;
        b[12] = self.y;
        b[13] = self.sp;
        b[14] = self.psw;
        b[15] = self.bytes_len;
        b[16..19].copy_from_slice(&self.bytes);
        // bytes[19..24] reserved (zero).
        b
    }

    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let mut raw = [0u8; 3];
        raw.copy_from_slice(&bytes[16..19]);
        Some(Self {
            master_cycle: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
            pc: u16::from_le_bytes(bytes[8..10].try_into().ok()?),
            a: bytes[10],
            x: bytes[11],
            y: bytes[12],
            sp: bytes[13],
            psw: bytes[14],
            bytes_len: bytes[15],
            bytes: raw,
        })
    }
}

/// Per-instruction binary trace record for the Super FX GSU.
///
/// Captured **before** the instruction executes, inside the chip's
/// `catch_up` burst loop. Everything
/// [`crate::trace_format::format_trace_line_gsu`] needs lives here, so the
/// formatter stays bus-free: the disassembler reconstructs the operand text
/// from the captured `bytes` plus the prefix state carried in `sfr`
/// (`ALT1`/`ALT2`), and the full 16-register file is snapshotted so the
/// register column is exact at the boundary.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TraceEntryBinaryGsu {
    /// GSU catch-up anchor (S-CPU master-clock domain) at the instruction
    /// boundary, an ordering aid; not printed.
    pub master_cycle: u64,
    /// `PBR<<16 | R15`: the full 24-bit program address of the instruction.
    pub pbr_pc: u32,
    /// Status flag register (`SFR`); carries `ALT1`/`ALT2` for the decoder.
    pub sfr: u16,
    /// Game Pak ROM bank register.
    pub rombr: u8,
    /// Game Pak RAM bank register (1-bit, stored as `0`/`1`).
    pub rambr: u8,
    /// Number of valid bytes in `bytes` (1..=3).
    pub bytes_len: u8,
    /// The instruction's raw bytes (opcode + up to 2 operand bytes).
    pub bytes: [u8; 3],
    /// The full general-purpose register file `R0..R15` (`R15` = PC).
    pub r: [u16; 16],
}

impl TraceEntry for TraceEntryBinaryGsu {
    /// 52 bytes, all little-endian: `master_cycle(8) pbr_pc(4) sfr(2)
    /// rombr(1) rambr(1) bytes_len(1) bytes(3) r[16](32)`.
    const SIZE: usize = 52;
    type Bytes = [u8; 52];

    fn to_bytes(&self) -> [u8; 52] {
        let mut b = [0u8; 52];
        b[0..8].copy_from_slice(&self.master_cycle.to_le_bytes());
        b[8..12].copy_from_slice(&self.pbr_pc.to_le_bytes());
        b[12..14].copy_from_slice(&self.sfr.to_le_bytes());
        b[14] = self.rombr;
        b[15] = self.rambr;
        b[16] = self.bytes_len;
        b[17..20].copy_from_slice(&self.bytes);
        for (i, reg) in self.r.iter().enumerate() {
            let off = 20 + i * 2;
            b[off..off + 2].copy_from_slice(&reg.to_le_bytes());
        }
        b
    }

    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let mut raw = [0u8; 3];
        raw.copy_from_slice(&bytes[17..20]);
        let mut r = [0u16; 16];
        for (i, reg) in r.iter_mut().enumerate() {
            let off = 20 + i * 2;
            *reg = u16::from_le_bytes(bytes[off..off + 2].try_into().ok()?);
        }
        Some(Self {
            master_cycle: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
            pbr_pc: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            sfr: u16::from_le_bytes(bytes[12..14].try_into().ok()?),
            rombr: bytes[14],
            rambr: bytes[15],
            bytes_len: bytes[16],
            bytes: raw,
            r,
        })
    }
}

/// Per-instruction binary trace record for the Cx4 HG51B169.
///
/// Captured **before** the instruction executes, inside the chip's `catch_up`
/// burst loop. Everything [`crate::trace_format::format_trace_line_cx4`] needs
/// lives here, so the formatter stays bus-free: the 16-bit `opcode` is decoded
/// by the chip's pure disassembler, and the accumulators + 16 general-purpose
/// registers are snapshotted so the register column is exact at the boundary.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TraceEntryBinaryCx4 {
    /// Cumulative 20 MHz chip cycle at the instruction boundary, an ordering
    /// aid; not printed.
    pub cycles: u64,
    /// 23-bit program counter (the instruction's program address).
    pub pc: u32,
    /// The 16-bit instruction word.
    pub opcode: u16,
    /// 15-bit page register.
    pub p: u16,
    /// Packed ALU flags: `N=0x8`/`Z=0x4`/`V=0x2`/`C=0x1`.
    pub flags: u8,
    /// 24-bit accumulator.
    pub a: u32,
    /// High 24 bits of the multiply accumulator.
    pub acch: u32,
    /// Low 24 bits of the multiply accumulator.
    pub accl: u32,
    /// The 16 general-purpose registers `R0..R15` (24-bit each).
    pub gpr: [u32; 16],
}

impl TraceEntry for TraceEntryBinaryCx4 {
    /// 93 bytes, all little-endian: `cycles(8) pc(4) opcode(2) p(2) flags(1)
    /// a(4) acch(4) accl(4) gpr[16](64)`.
    const SIZE: usize = 93;
    type Bytes = [u8; 93];

    fn to_bytes(&self) -> [u8; 93] {
        let mut b = [0u8; 93];
        b[0..8].copy_from_slice(&self.cycles.to_le_bytes());
        b[8..12].copy_from_slice(&self.pc.to_le_bytes());
        b[12..14].copy_from_slice(&self.opcode.to_le_bytes());
        b[14..16].copy_from_slice(&self.p.to_le_bytes());
        b[16] = self.flags;
        b[17..21].copy_from_slice(&self.a.to_le_bytes());
        b[21..25].copy_from_slice(&self.acch.to_le_bytes());
        b[25..29].copy_from_slice(&self.accl.to_le_bytes());
        for (i, reg) in self.gpr.iter().enumerate() {
            let off = 29 + i * 4;
            b[off..off + 4].copy_from_slice(&reg.to_le_bytes());
        }
        b
    }

    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE {
            return None;
        }
        let mut gpr = [0u32; 16];
        for (i, reg) in gpr.iter_mut().enumerate() {
            let off = 29 + i * 4;
            *reg = u32::from_le_bytes(bytes[off..off + 4].try_into().ok()?);
        }
        Some(Self {
            cycles: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
            pc: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            opcode: u16::from_le_bytes(bytes[12..14].try_into().ok()?),
            p: u16::from_le_bytes(bytes[14..16].try_into().ok()?),
            flags: bytes[16],
            a: u32::from_le_bytes(bytes[17..21].try_into().ok()?),
            acch: u32::from_le_bytes(bytes[21..25].try_into().ok()?),
            accl: u32::from_le_bytes(bytes[25..29].try_into().ok()?),
            gpr,
        })
    }
}

/// Build the 65C816 register snapshot (`CpuState`, cpu 0). `pc` carries
/// the full 24-bit `PBR:PC`; `flags` is the raw `P` byte.
pub fn cpu_state_65816(cpu: &Wdc65816) -> CpuState {
    CpuState {
        cpu_id: MAIN_CPU,
        pc: ((cpu.pbr as u32) << 16) | cpu.pc as u32,
        flags: cpu.p as u32,
        registers: vec![
            RegisterValue {
                name: "A".to_string(),
                value: cpu.c as u64,
            },
            RegisterValue {
                name: "X".to_string(),
                value: cpu.x as u64,
            },
            RegisterValue {
                name: "Y".to_string(),
                value: cpu.y as u64,
            },
            RegisterValue {
                name: "S".to_string(),
                value: cpu.sp as u64,
            },
            RegisterValue {
                name: "D".to_string(),
                value: cpu.d as u64,
            },
            RegisterValue {
                name: "DB".to_string(),
                value: cpu.dbr as u64,
            },
            RegisterValue {
                name: "PB".to_string(),
                value: cpu.pbr as u64,
            },
            RegisterValue {
                name: "P".to_string(),
                value: cpu.p as u64,
            },
            RegisterValue {
                name: "E".to_string(),
                value: cpu.emulation as u64,
            },
        ],
    }
}

/// Build the SPC700 register snapshot (`CpuState`, cpu 1). `YA` is the
/// 16-bit pair `Y:A` exposed as a pseudo-register for convenience.
pub fn cpu_state_spc700(smp: &Spc700) -> CpuState {
    CpuState {
        cpu_id: SOUND_CPU,
        pc: smp.pc as u32,
        flags: smp.psw as u32,
        registers: vec![
            RegisterValue {
                name: "A".to_string(),
                value: smp.a as u64,
            },
            RegisterValue {
                name: "X".to_string(),
                value: smp.x as u64,
            },
            RegisterValue {
                name: "Y".to_string(),
                value: smp.y as u64,
            },
            RegisterValue {
                name: "SP".to_string(),
                value: smp.sp as u64,
            },
            RegisterValue {
                name: "YA".to_string(),
                value: ((smp.y as u64) << 8) | smp.a as u64,
            },
            RegisterValue {
                name: "PSW".to_string(),
                value: smp.psw as u64,
            },
        ],
    }
}

/// Build the Super FX GSU register snapshot (`CpuState`). The register
/// *names* are a hard contract with `debugger-core`'s `COLUMN_SPECS.superfx`
/// table (`R0..R15`, then `PBR`/`ROMBR`/`RAMBR`/`CBR`/`SCBR`/`SCMR`/`COLR`/
/// `POR`); `pc` carries `PBR:R15` and `flags` carries `SFR`. The `cpu_id`
/// stays 0, the console stamps the real slot index when it places this in
/// the CPU-state vector.
#[cfg(feature = "chip-superfx")]
pub fn cpu_state_gsu(gsu: &Gsu) -> CpuState {
    let mut registers: Vec<RegisterValue> = (0..16)
        .map(|n| RegisterValue {
            name: format!("R{n}"),
            value: u64::from(gsu.registers[n]),
        })
        .collect();
    let extra: [(&str, u64); 8] = [
        ("PBR", u64::from(gsu.program_bank)),
        ("ROMBR", u64::from(gsu.rom_bank)),
        ("RAMBR", u64::from(gsu.ram_bank)),
        ("CBR", u64::from(gsu.cache_base)),
        ("SCBR", u64::from(gsu.screen_base)),
        ("SCMR", u64::from(gsu.screen_mode)),
        ("COLR", u64::from(gsu.color)),
        ("POR", u64::from(gsu.plot_options)),
    ];
    registers.extend(extra.iter().map(|(name, value)| RegisterValue {
        name: name.to_string(),
        value: *value,
    }));
    CpuState {
        cpu_id: 0,
        pc: (u32::from(gsu.program_bank) << 16) | u32::from(gsu.registers[15]),
        flags: u32::from(gsu.status),
        registers,
    }
}

/// Build the Cx4 HG51B169 register snapshot (`CpuState`). The register
/// *names* are a hard contract with `debugger-core`'s `COLUMN_SPECS.hg51b`
/// table (`A`/`ACCH`/`ACCL`, then `R0..R15`, then `P`); `pc` carries the
/// 23-bit program counter and `flags` packs `N=0x8`/`Z=0x4`/`V=0x2`/`C=0x1`
/// (matching `debugger-core`'s `HG51B_FLAGS`). The `cpu_id` stays 0, the
/// console stamps the real slot index when it places this in the CPU-state
/// vector.
#[cfg(feature = "chip-cx4")]
pub fn cpu_state_hg51b(core: &Hg51b) -> CpuState {
    let mut registers = vec![
        RegisterValue {
            name: "A".to_string(),
            value: u64::from(core.accumulator),
        },
        RegisterValue {
            name: "ACCH".to_string(),
            value: u64::from(core.product_high),
        },
        RegisterValue {
            name: "ACCL".to_string(),
            value: u64::from(core.product_low),
        },
    ];
    registers.extend((0..16).map(|n| RegisterValue {
        name: format!("R{n}"),
        value: u64::from(core.registers[n]),
    }));
    registers.push(RegisterValue {
        name: "P".to_string(),
        value: u64::from(core.page),
    });
    let f = &core.flags;
    let flags = u32::from(f.negative) << 3
        | u32::from(f.zero) << 2
        | u32::from(f.overflow) << 1
        | u32::from(f.carry);
    CpuState {
        cpu_id: 0,
        pc: core.pc,
        flags,
        registers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::debug::check_catalogue;

    #[test]
    fn the_catalogue_names_each_code_with_its_kind() {
        assert_eq!(check_catalogue(SnesDebugConsole::EVENTS), Ok(()));
        let kinds: Vec<(u32, EventKind)> = SnesDebugConsole::EVENTS
            .iter()
            .map(|spec| (spec.code, spec.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                (snes_events::VBLANK, EventKind::VerticalBlank),
                (snes_events::NMI, EventKind::VerticalBlankInterrupt),
                (snes_events::IRQ, EventKind::RasterInterrupt),
                (snes_events::DMA_START, EventKind::TransferStart),
                (snes_events::HDMA_START, EventKind::LineTransferSetUp),
                (snes_events::SCANLINE, EventKind::Line),
                (snes_events::PPU_REG_WRITE, EventKind::VideoRegisterWrite),
                (snes_events::APU_PORT_WRITE, EventKind::AudioPortWrite),
                (snes_events::DSP_REG_WRITE, EventKind::AudioRegisterWrite),
                (snes_events::DMA_DONE, EventKind::TransferDone),
                (snes_events::PALETTE_WRITE, EventKind::PaletteWrite),
                (snes_events::TILE_CHANGE, EventKind::TileWrite),
            ]
        );
    }

    #[test]
    fn each_event_reports_its_code_and_param() {
        assert_eq!(SnesEvent::VBlank.code_param(), (1, 0));
        assert_eq!(SnesEvent::Scanline(100).code_param(), (6, 100));
        assert_eq!(SnesEvent::DmaDone(0x18).code_param(), (10, 0x18));
        assert_eq!(PpuDebugEvent::PaletteWrite(3).code_param(), (11, 3));
        assert_eq!(PpuDebugEvent::TileChange(2047).code_param(), (12, 2047));
    }

    #[test]
    fn the_cartridge_cpu_slot_follows_its_architecture() {
        let gsu = cartridge_cpu_spec(Some(CpuArchitecture::SuperFx));
        assert_eq!(
            gsu.trace_entry_size,
            <TraceEntryBinaryGsu as TraceEntry>::SIZE
        );
        assert_eq!(gsu.checks, BreakpointChecks::NONE);
        let cx4 = cartridge_cpu_spec(Some(CpuArchitecture::Hg51b));
        assert_eq!(
            cx4.trace_entry_size,
            <TraceEntryBinaryCx4 as TraceEntry>::SIZE
        );
        assert_eq!(
            cartridge_cpu_spec(Some(CpuArchitecture::Mos65816)),
            CpuDebugSpec::NO_CPU,
            "the SA-1's 65816 keeps no trace"
        );
        assert_eq!(cartridge_cpu_spec(None), CpuDebugSpec::NO_CPU);
    }

    #[test]
    fn the_memory_regions_name_the_sprite_table_and_the_palette() {
        let mut console = SnesDebugConsole::default();
        console.set_memory_sizes(0x2_0000, 0x2000);
        let regions: Vec<(String, MemoryRegion, u32)> = console
            .memory_regions()
            .into_iter()
            .map(|region| (region.name, region.region, region.size))
            .collect();
        assert!(regions.contains(&("OAM".to_string(), MemoryRegion::SpriteTable, 0x220)));
        assert!(regions.contains(&("CGRAM".to_string(), MemoryRegion::PaletteRam, 0x200)));
        assert!(regions.contains(&("ROM".to_string(), MemoryRegion::Rom, 0x2_0000)));
        assert!(regions.contains(&("SRAM".to_string(), MemoryRegion::SaveRam, 0x2000)));
    }
}
