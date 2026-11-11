//! SNES debugger.
//!
//! [`SnesDebugger`] is the `EmulatorCore`-facing implementation of
//! [`retro_core::debugger::DebuggerCore`]. It multiplexes breakpoints,
//! code coverage, and instruction tracing across the two CPUs (65C816
//! main + SPC700 audio) and caches a register snapshot of each for the
//! debugger UI. A cartridge with a CPU of its own (the SA-1's second
//! 65816, the Super FX's GSU, the Cx4) adds a third slot: its register
//! snapshot and, for the GSU and the Cx4, a trace ring.
//!
//! The headline feature wired here is the **65C816 instruction trace**:
//! per instruction the emulator captures a compact [`TraceEntryBinary65816`]
//! (registers + raster position + instruction bytes + resolved effective
//! address) into a [`TracingState`] buffer. The host drains the buffer and
//! renders each entry to text with
//! [`crate::trace_format::format_trace_line_65816`], reproducing the
//! bsnes-plus trace format byte-for-byte so the two can be diffed.
//!
//! The SPC700 side has the same shape: coverage, a register snapshot, and
//! a tracing buffer of [`TraceEntryBinarySpc700`] records rendered by
//! [`crate::trace_format::format_trace_line_spc700`]. Because the SPC700
//! runs via lazy catch-up (not instruction-stepped in the 65C816 hot
//! loop), its entries are captured inside `Apu::catch_up_to` and bridged
//! into [`SnesDebugger`] by the emulator each frame.
//!
//! The 65C816 additionally maintains a **call stack** (JSR/JSL/BRK/COP push,
//! RTS/RTL/RTI pop) and a ring buffer of **instruction history**, both
//! surfaced through the [`DebuggerCore`] trait.

use std::collections::VecDeque;

use retro_core::audio::AudioDebugger;
use retro_core::debug::{
    select_tier, BreakpointList, CoverageBitmap, ExecutionTier, TierSelectorInputs, TracingState,
};
use retro_core::debugger::DebuggerCore;
use retro_core::types::{
    AccessFlags, Breakpoint, BreakpointId, BreakpointInfo, CpuArchitecture, CpuInfo, CpuState,
    EventClass, EventParamOption, ExecutionMode, Instruction, InstructionTrace, MatchSpec,
    MemoryRegion, MemoryRegionInfo, RegisterValue, StackFrame, SystemInfo,
};
use retro_core::video::VideoDebugger;
use spc700::Spc700;
#[cfg(feature = "chip-superfx")]
use superfx_gsu::Gsu;

#[cfg(feature = "chip-cx4")]
use crate::cartridge::cx4::hg51b::Hg51b;
use wdc65816::Wdc65816;

/// Serialized size of one [`TraceEntryBinary65816`] (44 bytes).
///
/// Layout (all little-endian): `master_cycle(8) pbr_pc(4) a(2) x(2) y(2)
/// s(2) d(2) vcounter(2) hdot(2) framecounter(4) eff_addr(4) dbr(1) p(1)
/// e(1) eff_present(1) bytes_len(1) cycles(1) bytes(4)`.
pub const TRACE_ENTRY_SIZE_65816: usize = 44;

/// Serialized size of one [`TraceEntryBinarySpc700`] (24 bytes).
///
/// Layout (all little-endian): `master_cycle(8) pc(2) a(1) x(1) y(1)
/// sp(1) psw(1) bytes_len(1) bytes(3)` followed by 5 reserved bytes.
pub const TRACE_ENTRY_SIZE_SPC700: usize = 24;

/// Serialized size of one [`TraceEntryBinaryGsu`] (52 bytes).
///
/// Layout (all little-endian): `master_cycle(8) pbr_pc(4) sfr(2) rombr(1)
/// rambr(1) bytes_len(1) bytes(3) r[16](32)`.
pub const TRACE_ENTRY_SIZE_GSU: usize = 52;

/// Serialized size of one [`TraceEntryBinaryCx4`] (93 bytes).
///
/// Layout (all little-endian): `cycles(8) pc(4) opcode(2) p(2) flags(1)
/// a(4) acch(4) accl(4) gpr[16](64)`.
pub const TRACE_ENTRY_SIZE_CX4: usize = 93;

/// 24-bit address-space size for the 65C816 coverage bitmap (16 MiB).
const COVERAGE_SIZE_65816: u32 = 0x0100_0000;
/// 16-bit address-space size for the SPC700 coverage bitmap (64 KiB).
const COVERAGE_SIZE_SPC700: u32 = 0x0001_0000;

/// Cap on retained 65C816 instruction-history records (ring buffer).
const MAX_INSTRUCTION_HISTORY_65816: usize = 1024;
/// Cap on tracked 65C816 call-stack depth (prevents runaway growth when a
/// program's pushes and pops are unbalanced, e.g. manual stack juggling).
const MAX_CALL_STACK_DEPTH_65816: usize = 256;

/// SNES event-breakpoint codes, carried in the `code` field of
/// [`retro_core::types::BreakpointType::Event`].
///
/// retro-core stays agnostic about these numbers, they are the SNES
/// core's private taxonomy, paired with the matching
/// [`SnesEvent`] constructor. The `param` field disambiguates within a
/// class (a scanline, a register index, a port); paramless classes
/// (VBlank/NMI/IRQ/DMA/HDMA) report `param == 0`, so a breakpoint stored
/// with `param == 0` or [`retro_core::types::BREAKPOINT_PARAM_ANY`]
/// matches them.
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
}

/// A SNES debug event that can be matched by an `Event` breakpoint.
///
/// Each variant maps to a `(code, param)` pair via [`Self::to_code_param`],
/// the same pair the host stores in
/// [`retro_core::types::BreakpointType::Event`]. The hook sites in
/// `system.rs` / `bus.rs` / `apu` construct the appropriate variant and
/// call [`SnesDebugger::check_event`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
}

impl SnesEvent {
    /// Resolve to the `(code, param)` pair used by
    /// [`retro_core::types::BreakpointType::Event`]. Paramless events
    /// report `param == 0`.
    pub fn to_code_param(self) -> (u32, u32) {
        match self {
            SnesEvent::VBlank => (snes_events::VBLANK, 0),
            SnesEvent::Nmi => (snes_events::NMI, 0),
            SnesEvent::Irq => (snes_events::IRQ, 0),
            SnesEvent::DmaStart => (snes_events::DMA_START, 0),
            SnesEvent::HdmaStart => (snes_events::HDMA_START, 0),
            SnesEvent::Scanline(v) => (snes_events::SCANLINE, v as u32),
            SnesEvent::PpuRegisterWrite(reg) => (snes_events::PPU_REG_WRITE, reg as u32),
            SnesEvent::ApuPortWrite(port) => (snes_events::APU_PORT_WRITE, port as u32),
            SnesEvent::DspRegisterWrite(reg) => (snes_events::DSP_REG_WRITE, reg as u32),
        }
    }
}

/// Per-instruction binary trace record for the 65C816.
///
/// Captured **before** the instruction executes (matching bsnes-plus),
/// this carries everything [`crate::trace_format::format_trace_line_65816`]
/// needs to reconstruct the exact bsnes-plus trace line without touching
/// the bus, including the already-resolved effective address (the only
/// field that depends on live memory for indirect addressing modes).
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

impl TraceEntryBinary65816 {
    /// Serialize to a fixed [`TRACE_ENTRY_SIZE_65816`]-byte little-endian
    /// record for streaming through [`TracingState`].
    pub fn to_bytes(&self) -> [u8; TRACE_ENTRY_SIZE_65816] {
        let mut b = [0u8; TRACE_ENTRY_SIZE_65816];
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

    /// Parse one record produced by [`Self::to_bytes`]. Returns `None`
    /// when `bytes` is shorter than [`TRACE_ENTRY_SIZE_65816`].
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < TRACE_ENTRY_SIZE_65816 {
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

impl TraceEntryBinarySpc700 {
    /// Serialize to a fixed [`TRACE_ENTRY_SIZE_SPC700`]-byte little-endian
    /// record for streaming through [`TracingState`].
    pub fn to_bytes(&self) -> [u8; TRACE_ENTRY_SIZE_SPC700] {
        let mut b = [0u8; TRACE_ENTRY_SIZE_SPC700];
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

    /// Parse one record produced by [`Self::to_bytes`]. Returns `None`
    /// when `bytes` is shorter than [`TRACE_ENTRY_SIZE_SPC700`].
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < TRACE_ENTRY_SIZE_SPC700 {
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

impl TraceEntryBinaryGsu {
    /// Serialize to a fixed [`TRACE_ENTRY_SIZE_GSU`]-byte little-endian
    /// record for streaming through [`TracingState`].
    pub fn to_bytes(&self) -> [u8; TRACE_ENTRY_SIZE_GSU] {
        let mut b = [0u8; TRACE_ENTRY_SIZE_GSU];
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

    /// Parse one record produced by [`Self::to_bytes`]. Returns `None`
    /// when `bytes` is shorter than [`TRACE_ENTRY_SIZE_GSU`].
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < TRACE_ENTRY_SIZE_GSU {
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
/// by the pure [`crate::cartridge::cx4::instructions::disassemble`], and the
/// accumulators + 16 general-purpose registers are snapshotted so the register
/// column is exact at the boundary.
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

impl TraceEntryBinaryCx4 {
    /// Serialize to a fixed [`TRACE_ENTRY_SIZE_CX4`]-byte little-endian record
    /// for streaming through [`TracingState`].
    pub fn to_bytes(&self) -> [u8; TRACE_ENTRY_SIZE_CX4] {
        let mut b = [0u8; TRACE_ENTRY_SIZE_CX4];
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

    /// Parse one record produced by [`Self::to_bytes`]. Returns `None` when
    /// `bytes` is shorter than [`TRACE_ENTRY_SIZE_CX4`].
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < TRACE_ENTRY_SIZE_CX4 {
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

/// SNES debugger: breakpoints, coverage, tracing, and cached CPU states
/// for both the 65C816 (cpu 0) and the SPC700 (cpu 1), plus the state of
/// a cartridge's own CPU and the GSU / Cx4 trace ring as cpu 2.
pub struct SnesDebugger {
    breakpoints: BreakpointList,
    execution_mode: ExecutionMode,

    coverage_65816: CoverageBitmap,
    coverage_spc700: CoverageBitmap,

    tracing_65816: TracingState,
    tracing_spc700: TracingState,
    /// GSU (Super FX) trace ring (cpu 2). Fed by the chip's `catch_up` burst
    /// loop when enabled; idle on a non-Super-FX cart.
    tracing_gsu: TracingState,
    /// Cx4 (HG51B169) trace ring (cpu 2). Fed by the chip's `catch_up` burst
    /// loop when enabled; idle on a non-Cx4 cart. A cart hosts at most one
    /// secondary CPU, so only one of `tracing_gsu`/`tracing_cx4` is ever live,
    /// the slot-2 trait methods pick the ring by the installed architecture.
    tracing_cx4: TracingState,

    /// 65C816 call stack (JSR/JSL/BRK/COP push, RTS/RTL/RTI pop). Built
    /// in both the light and full debug tiers (opcode classification is
    /// allocation-free).
    call_stack_65816: Vec<StackFrame>,
    /// 65C816 instruction history ring (newest at the back). Populated
    /// only in the full-debug single-step path, where the disassembly
    /// string is already being built.
    instruction_history_65816: VecDeque<InstructionTrace>,

    /// Register snapshots refreshed by the emulator's full-debug tier /
    /// single-step path. Index 0 = 65C816, index 1 = SPC700, index 2 = the
    /// cartridge's own CPU when it has one (SA-1, Super FX, Cx4).
    cpu_states: Vec<CpuState>,

    system_info: SystemInfo,

    rom_size: u32,
    sram_size: u32,
}

impl SnesDebugger {
    /// Build a debugger for a system described by `system_info` (used by
    /// [`DebuggerCore::get_cpu_info`]). CPU states start at reset defaults.
    pub fn new(system_info: SystemInfo) -> Self {
        Self {
            breakpoints: BreakpointList::new(),
            execution_mode: ExecutionMode::Running,
            coverage_65816: CoverageBitmap::new(COVERAGE_SIZE_65816),
            coverage_spc700: CoverageBitmap::new(COVERAGE_SIZE_SPC700),
            tracing_65816: TracingState::new(8, TRACE_ENTRY_SIZE_65816),
            tracing_spc700: TracingState::new(8, TRACE_ENTRY_SIZE_SPC700),
            tracing_gsu: TracingState::new(8, TRACE_ENTRY_SIZE_GSU),
            tracing_cx4: TracingState::new(8, TRACE_ENTRY_SIZE_CX4),
            call_stack_65816: Vec::with_capacity(MAX_CALL_STACK_DEPTH_65816),
            instruction_history_65816: VecDeque::with_capacity(MAX_INSTRUCTION_HISTORY_65816),
            cpu_states: vec![
                cpu_state_65816(&Wdc65816::new()),
                cpu_state_spc700(&Spc700::new()),
            ],
            system_info,
            rom_size: 0,
            sram_size: 0,
        }
    }

    /// Mark a 65C816 instruction start as executed (coverage bitmap).
    #[inline]
    pub fn record_coverage_65816(&mut self, pc24: u32) {
        self.coverage_65816.set(pc24);
    }

    /// Mark an SPC700 instruction start as executed.
    #[inline]
    pub fn record_coverage_spc700(&mut self, pc: u32) {
        self.coverage_spc700.set(pc);
    }

    /// `true` iff the 65C816 has executed from `addr`.
    pub fn is_covered_65816(&self, addr: u32) -> bool {
        self.coverage_65816.get(addr)
    }

    /// Number of distinct 65C816 instruction starts seen so far.
    pub fn coverage_count_65816(&self) -> u32 {
        self.coverage_65816.count()
    }

    /// Number of distinct SPC700 instruction starts seen so far.
    pub fn coverage_count_spc700(&self) -> u32 {
        self.coverage_spc700.count()
    }

    /// Append a 65C816 trace entry to the buffer. Returns `true` when the
    /// buffer reached its flush threshold.
    #[inline]
    pub fn record_trace_65816(&mut self, entry: &TraceEntryBinary65816) -> bool {
        self.tracing_65816.push_entry(&entry.to_bytes())
    }

    /// Append an SPC700 trace entry to the buffer. Returns `true` when the
    /// buffer reached its flush threshold.
    #[inline]
    pub fn record_trace_spc700(&mut self, entry: &TraceEntryBinarySpc700) -> bool {
        self.tracing_spc700.push_entry(&entry.to_bytes())
    }

    /// Append a batch of pre-serialized GSU trace records (drained from the
    /// Super FX chip's own capture buffer, each already a
    /// [`TRACE_ENTRY_SIZE_GSU`]-byte [`TraceEntryBinaryGsu`]). Returns `true`
    /// when the ring reached its flush threshold. A short trailing fragment
    /// (not a whole entry) is ignored.
    pub fn record_trace_gsu_bytes(&mut self, raw: &[u8]) -> bool {
        let mut flush = false;
        for chunk in raw.as_chunks::<TRACE_ENTRY_SIZE_GSU>().0 {
            flush |= self.tracing_gsu.push_entry(chunk);
        }
        flush
    }

    /// Append a batch of pre-serialized Cx4 trace records (drained from the Cx4
    /// chip's own capture buffer, each already a [`TRACE_ENTRY_SIZE_CX4`]-byte
    /// [`TraceEntryBinaryCx4`]). Returns `true` when the ring reached its flush
    /// threshold. A short trailing fragment (not a whole entry) is ignored.
    pub fn record_trace_cx4_bytes(&mut self, raw: &[u8]) -> bool {
        let mut flush = false;
        for chunk in raw.as_chunks::<TRACE_ENTRY_SIZE_CX4>().0 {
            flush |= self.tracing_cx4.push_entry(chunk);
        }
        flush
    }

    /// Push a 65C816 call-stack frame (on JSR/JSL/BRK/COP). Bounded by
    /// [`MAX_CALL_STACK_DEPTH_65816`] so pathological push/pop imbalance
    /// can't grow the vector without limit.
    #[inline]
    pub fn push_call_65816(&mut self, frame: StackFrame) {
        if self.call_stack_65816.len() < MAX_CALL_STACK_DEPTH_65816 {
            self.call_stack_65816.push(frame);
        }
    }

    /// Pop the most recent 65C816 call-stack frame (on RTS/RTL/RTI).
    #[inline]
    pub fn pop_call_65816(&mut self) -> Option<StackFrame> {
        self.call_stack_65816.pop()
    }

    /// Append a 65C816 instruction-history record, evicting the oldest
    /// when the ring is full.
    #[inline]
    pub fn record_instruction_65816(&mut self, trace: InstructionTrace) {
        if self.instruction_history_65816.len() >= MAX_INSTRUCTION_HISTORY_65816 {
            self.instruction_history_65816.pop_front();
        }
        self.instruction_history_65816.push_back(trace);
    }

    /// Check (and count) an execute breakpoint on the 65C816 at `pc24`.
    #[inline]
    pub fn check_execute_65816(&mut self, pc24: u32) -> bool {
        self.breakpoints.check_execute(0, pc24)
    }

    /// Check (and count) a read/write watchpoint on the 65C816 bus at
    /// `addr`. `write` selects the access direction. Wired in from the
    /// emulator's per-instruction bus-event drain.
    #[inline]
    pub fn check_memory_access_65816(&mut self, addr: u32, write: bool) -> bool {
        self.breakpoints.check_memory_access(0, addr, write)
    }

    /// Value-aware variant of [`Self::check_memory_access_65816`]:
    /// supplies the byte the CPU read/wrote so value-matched watchpoints
    /// (`(observed & mask) <op> target`) can fire. The emulator's bus
    /// drain carries the value alongside every `Read`/`Write` event.
    #[inline]
    pub fn check_memory_access_valued_65816(&mut self, addr: u32, write: bool, value: u8) -> bool {
        self.breakpoints
            .check_memory_access_valued(0, addr, write, value as u32)
    }

    /// Check (and count) an [`SnesEvent`] against the registered
    /// [`retro_core::types::BreakpointType::Event`] breakpoints.
    #[inline]
    pub fn check_event(&mut self, event: SnesEvent) -> bool {
        let (code, param) = event.to_code_param();
        self.breakpoints.check_event(code, param)
    }

    /// `true` when any enabled breakpoint is a watchpoint or event hook
    /// (i.e. not a plain execute breakpoint). The emulator uses this to
    /// arm bus-level access recording and the inline event checks only
    /// when they can actually fire, keeping the fast path branch-free.
    #[inline]
    pub fn has_watchpoints_or_events(&self) -> bool {
        self.breakpoints.has_watchpoints_or_events()
    }

    /// `true` iff the SPC700 has executed from `addr` (coverage bitmap).
    pub fn is_covered_spc700(&self, addr: u32) -> bool {
        self.coverage_spc700.get(addr)
    }

    /// Mutable borrow of the breakpoint list, so the cartridge debugger
    /// can append ROM/SRAM breakpoints into the same list the CPU step
    /// path checks (rather than maintaining a parallel list).
    pub fn breakpoints_mut(&mut self) -> &mut BreakpointList {
        &mut self.breakpoints
    }

    /// Replace the cached register snapshots (index 0 = 65C816, 1 = SPC700,
    /// 2 = the cartridge's own CPU when it has one).
    pub fn set_cpu_states(&mut self, states: Vec<CpuState>) {
        self.cpu_states = states;
    }

    /// Replace the cached [`SystemInfo`] (whose `cpus` list backs
    /// [`DebuggerCore::get_cpu_info`]). Called on every `load_rom` so the CPU
    /// roster tracks the installed cartridge, a plain SNES cart is two CPUs,
    /// an SA-1 cart adds the chip's second 65816 as a third slot.
    pub fn set_system_info(&mut self, system_info: SystemInfo) {
        self.system_info = system_info;
    }

    /// Record the loaded ROM / SRAM sizes so [`DebuggerCore::get_memory_regions`]
    /// reports them.
    pub fn set_memory_sizes(&mut self, rom_size: u32, sram_size: u32) {
        self.rom_size = rom_size;
        self.sram_size = sram_size;
    }

    /// Pick the execution tier for the next frame given the current
    /// breakpoint / tracing / execution-mode state.
    pub fn execution_tier(&self) -> ExecutionTier {
        select_tier(TierSelectorInputs {
            execution_mode: self.execution_mode,
            has_breakpoints: !self.breakpoints.is_empty(),
            tracing_enabled_per_cpu: &[
                self.tracing_65816.is_enabled(),
                self.tracing_spc700.is_enabled(),
                // A cart hosts one secondary CPU, so only one of these is live.
                self.tracing_gsu.is_enabled() || self.tracing_cx4.is_enabled(),
            ],
        })
    }

    /// Whether the installed slot-2 secondary CPU is the Cx4 HG51B169 (vs the
    /// Super FX GSU or the SA-1's 65816). Routes the slot-2 trace trait methods
    /// to the matching ring; a cart never hosts more than one secondary CPU.
    fn secondary_cpu_is_cx4(&self) -> bool {
        matches!(
            self.system_info.cpus.get(2).map(|c| c.architecture),
            Some(CpuArchitecture::Hg51b)
        )
    }

    /// `true` iff GSU (cpu 2) tracing is currently enabled, the chip reads
    /// this each frame to decide whether to capture entries in its `catch_up`
    /// burst.
    pub fn is_gsu_tracing_enabled(&self) -> bool {
        self.tracing_gsu.is_enabled()
    }
}

/// Build the 65C816 register snapshot (`CpuState`, cpu 0). `pc` carries
/// the full 24-bit `PBR:PC`; `flags` is the raw `P` byte.
pub fn cpu_state_65816(cpu: &Wdc65816) -> CpuState {
    CpuState {
        cpu_id: 0,
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
        cpu_id: 1,
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

impl DebuggerCore for SnesDebugger {
    fn get_cpu_states(&self) -> Vec<CpuState> {
        self.cpu_states.clone()
    }

    fn get_cpu_info(&self, cpu_id: usize) -> Option<&CpuInfo> {
        self.system_info.cpus.get(cpu_id)
    }

    fn add_breakpoint(&mut self, bp: Breakpoint) -> BreakpointId {
        self.breakpoints.add(bp)
    }

    fn remove_breakpoint(&mut self, id: BreakpointId) {
        self.breakpoints.remove(id);
    }

    fn enable_breakpoint(&mut self, id: BreakpointId, enabled: bool) {
        self.breakpoints.enable(id, enabled);
    }

    fn set_breakpoint_access(&mut self, id: BreakpointId, access: AccessFlags) {
        self.breakpoints.set_access(id, access);
    }

    fn set_breakpoint_match_spec(&mut self, id: BreakpointId, spec: MatchSpec) {
        self.breakpoints.set_match_spec(id, spec);
    }

    fn list_breakpoints(&self) -> Vec<BreakpointInfo> {
        self.breakpoints.list()
    }

    /// The SNES event taxonomy (see [`snes_events`]). Paramless classes
    /// (VBlank/NMI/IRQ/DMA/HDMA) advertise no param; the rest carry a
    /// free-form numeric param (scanline, register index) except APU
    /// ports, which enumerate their four mailboxes.
    fn available_events(&self) -> Vec<EventClass> {
        let paramless = |code: u32, label: &str, description: &str| EventClass {
            code,
            label: label.to_string(),
            description: Some(description.to_string()),
            param_label: None,
            param_options: Vec::new(),
        };
        let numeric = |code: u32, label: &str, description: &str, param_label: &str| EventClass {
            code,
            label: label.to_string(),
            description: Some(description.to_string()),
            param_label: Some(param_label.to_string()),
            param_options: Vec::new(),
        };
        vec![
            paramless(snes_events::VBLANK, "VBlank", "Entering vertical blank"),
            paramless(
                snes_events::NMI,
                "NMI",
                "V-blank NMI delivered to the 65C816",
            ),
            paramless(snes_events::IRQ, "IRQ", "H/V-timer IRQ raised"),
            paramless(
                snes_events::DMA_START,
                "DMA start",
                "General-purpose DMA block started ($420B)",
            ),
            paramless(
                snes_events::HDMA_START,
                "HDMA start",
                "Top-of-frame HDMA initialisation",
            ),
            numeric(
                snes_events::SCANLINE,
                "Scanline",
                "A rendered scanline matched the V counter",
                "Scanline (V)",
            ),
            numeric(
                snes_events::PPU_REG_WRITE,
                "PPU register write",
                "Write to a PPU register ($2100-$213F)",
                "Register ($00-$3F)",
            ),
            EventClass {
                code: snes_events::APU_PORT_WRITE,
                label: "APU port write".to_string(),
                description: Some("65C816 write to an APU mailbox port ($2140-$2143)".to_string()),
                param_label: Some("Port".to_string()),
                param_options: (0..4)
                    .map(|p| EventParamOption {
                        value: p,
                        label: format!("Port {p}"),
                    })
                    .collect(),
            },
            numeric(
                snes_events::DSP_REG_WRITE,
                "S-DSP register write",
                "SPC700 write to an S-DSP register",
                "DSP register ($00-$7F)",
            ),
        ]
    }

    fn get_execution_mode(&self) -> ExecutionMode {
        self.execution_mode
    }

    fn set_execution_mode(&mut self, mode: ExecutionMode) {
        self.execution_mode = mode;
    }

    // Memory access is served by the emulator (it owns the bus); these
    // return empty / no-op so the trait stays satisfied. The host uses
    // `EmulatorCore::read_memory_debug` for live reads.
    fn read_memory(&self, _region: MemoryRegion, _addr: u32, _size: u32) -> Vec<u8> {
        Vec::new()
    }

    fn write_memory(&mut self, _region: MemoryRegion, _addr: u32, _data: &[u8]) {}

    fn get_memory_regions(&self) -> Vec<MemoryRegionInfo> {
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
                region: MemoryRegion::Custom(0),
                name: "OAM".to_string(),
                start_address: 0,
                size: 0x0220,
                writable: true,
            },
            MemoryRegionInfo {
                region: MemoryRegion::Custom(1),
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

    // Forward disassembly needs the bus; the emulator serves it via
    // `EmulatorCore::disassemble_cpu`, and the coverage-aware window via
    // `SnesEmulator::disassemble_window`.
    fn disassemble(&self, _cpu_id: usize, _addr: u32, _count: u32) -> Vec<Instruction> {
        Vec::new()
    }

    // Call stack / instruction history are tracked for the 65C816 (cpu
    // 0) only; the other CPUs return empty.
    fn get_call_stack(&self, cpu_id: usize) -> Vec<StackFrame> {
        if cpu_id == 0 {
            self.call_stack_65816.clone()
        } else {
            Vec::new()
        }
    }

    fn get_instruction_history(&self, cpu_id: usize, count: u32) -> Vec<InstructionTrace> {
        if cpu_id != 0 {
            return Vec::new();
        }
        let count = count as usize;
        let len = self.instruction_history_65816.len();
        let skip = len.saturating_sub(count);
        self.instruction_history_65816
            .iter()
            .skip(skip)
            .cloned()
            .collect()
    }

    // The video and audio debuggers live on the structs that own the
    // data (`Ppu`, `Apu`), not on this one; the host reaches them
    // through `SnesEmulator::video_debugger()` and `audio_debugger()`.
    fn video_debugger(&mut self) -> Option<&mut dyn VideoDebugger> {
        None
    }

    fn audio_debugger(&mut self) -> Option<&mut dyn AudioDebugger> {
        None
    }

    fn set_tracing_enabled(&mut self, cpu_id: usize, enabled: bool) {
        match cpu_id {
            0 => self.tracing_65816.set_enabled(enabled),
            1 => self.tracing_spc700.set_enabled(enabled),
            2 if self.secondary_cpu_is_cx4() => self.tracing_cx4.set_enabled(enabled),
            2 => self.tracing_gsu.set_enabled(enabled),
            _ => {}
        }
    }

    fn is_tracing_enabled(&self, cpu_id: usize) -> bool {
        match cpu_id {
            0 => self.tracing_65816.is_enabled(),
            1 => self.tracing_spc700.is_enabled(),
            2 if self.secondary_cpu_is_cx4() => self.tracing_cx4.is_enabled(),
            2 => self.tracing_gsu.is_enabled(),
            _ => false,
        }
    }

    fn needs_trace_flush(&self) -> bool {
        self.tracing_65816.needs_flush()
            || self.tracing_spc700.needs_flush()
            || self.tracing_gsu.needs_flush()
            || self.tracing_cx4.needs_flush()
    }

    fn get_trace_flush_data(&mut self, cpu_id: usize) -> Option<Vec<u8>> {
        let cx4 = self.secondary_cpu_is_cx4();
        let ts = match cpu_id {
            0 => &mut self.tracing_65816,
            1 => &mut self.tracing_spc700,
            2 if cx4 => &mut self.tracing_cx4,
            2 => &mut self.tracing_gsu,
            _ => return None,
        };
        if !ts.has_data() {
            return None;
        }
        Some(ts.get_flush_data().to_vec())
    }

    fn clear_trace_flushed(&mut self, cpu_id: usize) {
        match cpu_id {
            0 => self.tracing_65816.clear_flushed(),
            1 => self.tracing_spc700.clear_flushed(),
            2 if self.secondary_cpu_is_cx4() => self.tracing_cx4.clear_flushed(),
            2 => self.tracing_gsu.clear_flushed(),
            _ => {}
        }
    }

    fn traceable_cpu_count(&self) -> usize {
        // Cpu 2 has a trace ring only on a Super FX or Cx4 cart. The SA-1's
        // secondary 65816 is steppable but *not* traced, so any other
        // three-CPU roster (or a bare cartridge) stays at 2.
        if matches!(
            self.system_info.cpus.get(2).map(|c| c.architecture),
            Some(CpuArchitecture::SuperFx | CpuArchitecture::Hg51b)
        ) {
            3
        } else {
            2
        }
    }
}
