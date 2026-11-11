//! Genesis-specific debugger implementation.
//!
//! Complete debugging infrastructure with breakpoints, disassembly,
//! memory inspection, and state tracking.

use retro_core::debug::expr::ExprContext;
use retro_core::*;
use std::collections::{HashSet, VecDeque};

/// 68K address space used by the coverage bitmap. The Genesis 68000
/// has a 24-bit address bus -> 16 MiB. We size the bitmap to the full
/// range so any PC the CPU could legally execute from has a slot.
const COVERAGE_68K_SIZE: u32 = 16 * 1024 * 1024;
/// Z80 address space -> 64 KiB.
const COVERAGE_Z80_SIZE: u32 = 64 * 1024;

/// Maximum number of instructions to keep in history.
const MAX_INSTRUCTION_HISTORY: usize = 1000;

/// Maximum call stack depth to track.
const MAX_CALL_STACK_DEPTH: usize = 100;

/// Genesis event-breakpoint codes, carried in the `code` field of
/// [`retro_core::types::BreakpointType::Event`].
///
/// retro-core stays agnostic about these numbers: they are the Genesis
/// core's private taxonomy, paired with the matching [`GenesisEvent`]
/// constructor and advertised through
/// [`GenesisDebugger::available_events`]. The `param` field disambiguates
/// within a class (a scanline); the paramless classes (VBlank/VInt/HInt)
/// report `param == 0`, so a breakpoint stored with `param == 0` or
/// [`retro_core::types::BREAKPOINT_PARAM_ANY`] matches them. Mirrors the
/// SNES core's `snes_events` module.
pub mod genesis_events {
    /// The beam entered vertical blank (start of the VBlank region).
    pub const VBLANK: u32 = 1;
    /// A VBlank interrupt (68000 level 6) was delivered.
    pub const VINT: u32 = 2;
    /// An HBlank interrupt (68000 level 4) was delivered.
    pub const HINT: u32 = 3;
    /// A scanline was reached; `param` is the VDP `V` counter.
    pub const SCANLINE: u32 = 4;
}

/// A Genesis debug event that an `Event` breakpoint can match.
///
/// Each variant maps to a `(code, param)` pair via [`Self::to_code_param`],
/// the same pair the host stores in
/// [`retro_core::types::BreakpointType::Event`]. The hook sites in the
/// `run_frame_debug_light` / `run_frame` tiers construct the appropriate
/// variant and call [`GenesisDebugger::check_event`]. Mirrors the SNES
/// core's `SnesEvent`.
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
}

impl GenesisEvent {
    /// Resolve to the `(code, param)` pair used by
    /// [`retro_core::types::BreakpointType::Event`]. Paramless events
    /// report `param == 0`.
    pub fn to_code_param(self) -> (u32, u32) {
        match self {
            GenesisEvent::VBlank => (genesis_events::VBLANK, 0),
            GenesisEvent::VInt => (genesis_events::VINT, 0),
            GenesisEvent::HInt => (genesis_events::HINT, 0),
            GenesisEvent::Scanline(v) => (genesis_events::SCANLINE, v as u32),
        }
    }
}

/// Size of a single serialized trace entry in bytes.
/// Note: This is the SERIALIZED size (84 bytes), not the in-memory size of TraceEntryBinary
/// which may be larger due to alignment padding with #[repr(C)].
/// Layout: pc(4) + sr(2) + d0-d7(32) + a0-a7(32) + opcode(2) + cycles(2) + total_cycles(8) + scanline(2) = 84 bytes
pub const TRACE_ENTRY_SIZE: usize = 84;

/// Binary trace entry for efficient streaming (84 bytes serialized).
///
/// This struct captures the complete CPU state at each instruction execution,
/// allowing for full trace comparison with other emulators like Exodus.
///
/// Layout:
/// - pc: 4 bytes (Program Counter)
/// - sr: 2 bytes (Status Register)
/// - d0-d7: 32 bytes (Data registers)
/// - a0-a7: 32 bytes (Address registers)
/// - opcode: 2 bytes (First word of instruction)
/// - cycles: 2 bytes (Cycles consumed by this instruction)
/// - total_cycles: 8 bytes (Total cycles executed so far)
/// - scanline: 2 bytes (Current VDP scanline)
#[repr(C)]
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
    /// Create a new trace entry.
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

    /// Convert to bytes for streaming.
    pub fn to_bytes(&self) -> [u8; TRACE_ENTRY_SIZE] {
        let mut bytes = [0u8; TRACE_ENTRY_SIZE];
        let mut offset = 0;

        // PC (4 bytes, little endian)
        bytes[offset..offset + 4].copy_from_slice(&self.pc.to_le_bytes());
        offset += 4;

        // SR (2 bytes, little endian)
        bytes[offset..offset + 2].copy_from_slice(&self.sr.to_le_bytes());
        offset += 2;

        // D0-D7 (32 bytes)
        for &d in &self.d {
            bytes[offset..offset + 4].copy_from_slice(&d.to_le_bytes());
            offset += 4;
        }

        // A0-A7 (32 bytes)
        for &a in &self.a {
            bytes[offset..offset + 4].copy_from_slice(&a.to_le_bytes());
            offset += 4;
        }

        // Opcode (2 bytes, little endian)
        bytes[offset..offset + 2].copy_from_slice(&self.opcode.to_le_bytes());
        offset += 2;

        // Cycles (2 bytes, little endian)
        bytes[offset..offset + 2].copy_from_slice(&self.cycles.to_le_bytes());
        offset += 2;

        // Total cycles (8 bytes, little endian)
        bytes[offset..offset + 8].copy_from_slice(&self.total_cycles.to_le_bytes());
        offset += 8;

        // Scanline (2 bytes, little endian)
        bytes[offset..offset + 2].copy_from_slice(&self.scanline.to_le_bytes());

        bytes
    }

    /// Parse from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < TRACE_ENTRY_SIZE {
            return None;
        }

        let mut offset = 0;

        let pc = u32::from_le_bytes(bytes[offset..offset + 4].try_into().ok()?);
        offset += 4;

        let sr = u16::from_le_bytes(bytes[offset..offset + 2].try_into().ok()?);
        offset += 2;

        let mut d = [0u32; 8];
        for reg in &mut d {
            *reg = u32::from_le_bytes(bytes[offset..offset + 4].try_into().ok()?);
            offset += 4;
        }

        let mut a = [0u32; 8];
        for reg in &mut a {
            *reg = u32::from_le_bytes(bytes[offset..offset + 4].try_into().ok()?);
            offset += 4;
        }

        let opcode = u16::from_le_bytes(bytes[offset..offset + 2].try_into().ok()?);
        offset += 2;

        let cycles = u16::from_le_bytes(bytes[offset..offset + 2].try_into().ok()?);
        offset += 2;

        let total_cycles = u64::from_le_bytes(bytes[offset..offset + 8].try_into().ok()?);
        offset += 8;

        let scanline = u16::from_le_bytes(bytes[offset..offset + 2].try_into().ok()?);

        Some(Self {
            pc,
            sr,
            d,
            a,
            opcode,
            cycles,
            total_cycles,
            scanline,
        })
    }
}

/// Size of a Z80 binary trace entry in bytes.
/// Layout: pc(2) + af(2) + bc(2) + de(2) + hl(2) + ix(2) + iy(2) + sp(2) +
///         i(1) + r(1) + im(1) + iff1(1) + iff2(1) + halt(1) + cycles(2) + total_cycles(8) + scanline(2) + raw_bytes(4) = 38 bytes
pub const Z80_TRACE_ENTRY_SIZE: usize = 38;

/// Binary trace entry for Z80 CPU state (38 bytes serialized).
///
/// Captures the complete Z80 CPU state for trace comparison with MAME.
/// Text format: PC:XXXX AF:XXXX BC:XXXX DE:XXXX HL:XXXX IX:XXXX IY:XXXX SP:XXXX I:XX R:XX IM:X IFF1:X IFF2:X HALT:X
#[repr(C)]
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
    /// Raw instruction bytes at PC (up to 4 bytes, Z80 max instruction length).
    /// Stored at capture time so the converter can disassemble from these bytes
    /// instead of current (potentially changed) Z80 RAM.
    pub raw_bytes: [u8; 4],
}

impl TraceEntryBinaryZ80 {
    /// Convert to bytes for streaming.
    pub fn to_bytes(&self) -> [u8; Z80_TRACE_ENTRY_SIZE] {
        let mut bytes = [0u8; Z80_TRACE_ENTRY_SIZE];
        let mut offset = 0;

        bytes[offset..offset + 2].copy_from_slice(&self.pc.to_le_bytes());
        offset += 2;
        bytes[offset..offset + 2].copy_from_slice(&self.af.to_le_bytes());
        offset += 2;
        bytes[offset..offset + 2].copy_from_slice(&self.bc.to_le_bytes());
        offset += 2;
        bytes[offset..offset + 2].copy_from_slice(&self.de.to_le_bytes());
        offset += 2;
        bytes[offset..offset + 2].copy_from_slice(&self.hl.to_le_bytes());
        offset += 2;
        bytes[offset..offset + 2].copy_from_slice(&self.ix.to_le_bytes());
        offset += 2;
        bytes[offset..offset + 2].copy_from_slice(&self.iy.to_le_bytes());
        offset += 2;
        bytes[offset..offset + 2].copy_from_slice(&self.sp.to_le_bytes());
        offset += 2;
        bytes[offset] = self.i_reg;
        offset += 1;
        bytes[offset] = self.r_reg;
        offset += 1;
        bytes[offset] = self.im;
        offset += 1;
        bytes[offset] = self.iff1;
        offset += 1;
        bytes[offset] = self.iff2;
        offset += 1;
        bytes[offset] = self.halt;
        offset += 1;
        bytes[offset..offset + 2].copy_from_slice(&self.cycles.to_le_bytes());
        offset += 2;
        bytes[offset..offset + 8].copy_from_slice(&self.total_cycles.to_le_bytes());
        offset += 8;
        bytes[offset..offset + 2].copy_from_slice(&self.scanline.to_le_bytes());
        offset += 2;
        bytes[offset..offset + 4].copy_from_slice(&self.raw_bytes);

        bytes
    }

    /// Parse from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Z80_TRACE_ENTRY_SIZE {
            return None;
        }

        let mut offset = 0;
        let pc = u16::from_le_bytes(bytes[offset..offset + 2].try_into().ok()?);
        offset += 2;
        let af = u16::from_le_bytes(bytes[offset..offset + 2].try_into().ok()?);
        offset += 2;
        let bc = u16::from_le_bytes(bytes[offset..offset + 2].try_into().ok()?);
        offset += 2;
        let de = u16::from_le_bytes(bytes[offset..offset + 2].try_into().ok()?);
        offset += 2;
        let hl = u16::from_le_bytes(bytes[offset..offset + 2].try_into().ok()?);
        offset += 2;
        let ix = u16::from_le_bytes(bytes[offset..offset + 2].try_into().ok()?);
        offset += 2;
        let iy = u16::from_le_bytes(bytes[offset..offset + 2].try_into().ok()?);
        offset += 2;
        let sp = u16::from_le_bytes(bytes[offset..offset + 2].try_into().ok()?);
        offset += 2;
        let i_reg = bytes[offset];
        offset += 1;
        let r_reg = bytes[offset];
        offset += 1;
        let im = bytes[offset];
        offset += 1;
        let iff1 = bytes[offset];
        offset += 1;
        let iff2 = bytes[offset];
        offset += 1;
        let halt = bytes[offset];
        offset += 1;
        let cycles = u16::from_le_bytes(bytes[offset..offset + 2].try_into().ok()?);
        offset += 2;
        let total_cycles = u64::from_le_bytes(bytes[offset..offset + 8].try_into().ok()?);
        offset += 8;
        let scanline = u16::from_le_bytes(bytes[offset..offset + 2].try_into().ok()?);
        offset += 2;
        let mut raw_bytes = [0u8; 4];
        raw_bytes.copy_from_slice(&bytes[offset..offset + 4]);

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
            cycles,
            total_cycles,
            scanline,
            raw_bytes,
        })
    }

    /// Format as text matching MAME Z80 trace format.
    /// Format: PC:XXXX AF:XXXX BC:XXXX DE:XXXX HL:XXXX IX:XXXX IY:XXXX SP:XXXX I:XX R:XX IM:X IFF1:X IFF2:X HALT:X
    pub fn to_text(&self, disasm: &str) -> String {
        format!(
            "PC:{:04X} AF:{:04X} BC:{:04X} DE:{:04X} HL:{:04X} IX:{:04X} IY:{:04X} SP:{:04X} I:{:02X} R:{:02X} IM:{} IFF1:{} IFF2:{} HALT:{} | {}",
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
            disasm,
        )
    }
}

// Tracing buffer + stats DTO live in `retro_core::debug::trace`.
// SG1's CPU-specific entry types (`TraceEntryBinary` for 68K,
// `TraceEntryBinaryZ80` for Z80, above) are serialized via their own
// `to_bytes()` and pushed into `TracingState` through the per-CPU
// wrappers on `GenesisDebugger` (`record_trace`, `record_z80_trace`).

/// Build a tracing buffer sized for SG1's 68K entries (8 MiB initial).
fn new_tracing_68k_default() -> TracingState {
    TracingState::new(8, TRACE_ENTRY_SIZE)
}

/// Build a tracing buffer sized for SG1's Z80 entries (8 MiB initial).
fn new_tracing_z80_default() -> TracingState {
    TracingState::new(8, Z80_TRACE_ENTRY_SIZE)
}

/// Genesis debugger.
pub struct GenesisDebugger {
    // Breakpoints and execution control
    breakpoints: BreakpointList,
    execution_mode: ExecutionMode,

    // Instruction history (ring buffer)
    instruction_history: VecDeque<InstructionTrace>,

    // Call stack tracking
    call_stack: Vec<StackFrame>,

    // Cached CPU states (updated before each step)
    cpu_states: Vec<CpuState>,

    // System info reference (populated on creation)
    system_info: SystemInfo,

    // Instruction boundary map: tracks all addresses that have been observed
    // as instruction starts during execution. Used for accurate disassembly
    // around branches/jumps (bsnes-style approach).
    instruction_boundaries: HashSet<u32>,

    /// Per-CPU code-coverage bitmaps. One set-bit per executed
    /// instruction start; consumed by `disassembleWindow` to render
    /// `....` for never-executed regions instead of fake `ori`
    /// instructions, and to walk backward from PC across only real
    /// instruction starts.
    /// In-memory only, cleared when a different ROM is loaded;
    /// survives `reset()` and hot-reload of the same cartridge.
    coverage_68k: CoverageBitmap,
    coverage_z80: CoverageBitmap,

    // Tracing state for 68K binary streaming to the host
    tracing: TracingState,

    // ===== Z80 Debug State =====
    /// Z80 instruction history (ring buffer)
    z80_instruction_history: VecDeque<InstructionTrace>,

    /// Z80 tracing state for binary streaming
    z80_tracing: TracingState,

    /// Z80 total cycles counter (for trace output)
    z80_total_cycles: u64,
}

impl GenesisDebugger {
    /// Create a new debugger with system information.
    pub fn new_with_system_info(system_info: SystemInfo) -> Self {
        Self {
            breakpoints: BreakpointList::new(),
            execution_mode: ExecutionMode::Paused, // Start paused for debugging
            instruction_history: VecDeque::with_capacity(MAX_INSTRUCTION_HISTORY),
            call_stack: Vec::with_capacity(MAX_CALL_STACK_DEPTH),
            cpu_states: Vec::new(),
            system_info,
            instruction_boundaries: HashSet::new(),
            coverage_68k: CoverageBitmap::new(COVERAGE_68K_SIZE),
            coverage_z80: CoverageBitmap::new(COVERAGE_Z80_SIZE),
            tracing: new_tracing_68k_default(),
            z80_instruction_history: VecDeque::with_capacity(MAX_INSTRUCTION_HISTORY),
            z80_tracing: new_tracing_z80_default(),
            z80_total_cycles: 0,
        }
    }

    /// Create a new debugger (stub version for compatibility).
    pub fn new() -> Self {
        // Create minimal system info as placeholder
        let system_info = SystemInfo {
            name: "Sega Genesis".to_string(),
            short_name: "genesis".to_string(),
            manufacturer: "Sega".to_string(),
            year: 1988,
            cpus: Vec::new(),
            display: DisplayInfo {
                width: 320,
                height: 224,
                aspect_ratio: 4.0 / 3.0,
                refresh_rate: crate::timing::VideoRegion::Ntsc.frame_rate(),
                pixel_format: PixelFormat::RGBA8888,
            },
            audio: AudioInfo {
                sample_rate: 44100,
                channels: 2,
                bits_per_sample: 16,
            },
            input: InputInfo {
                max_players: 2,
                button_layout: Vec::new(),
            },
        };

        Self::new_with_system_info(system_info)
    }

    /// Add an instruction to the history.
    pub fn record_instruction(&mut self, trace: InstructionTrace) {
        if self.instruction_history.len() >= MAX_INSTRUCTION_HISTORY {
            self.instruction_history.pop_front();
        }
        self.instruction_history.push_back(trace);
    }

    /// Record an instruction boundary (an address that is known to be the start of an instruction).
    /// Called during execution to build up the boundary map.
    pub fn record_instruction_boundary(&mut self, addr: u32) {
        self.instruction_boundaries.insert(addr);
    }

    /// Check if an address is a known instruction boundary.
    pub fn is_instruction_boundary(&self, addr: u32) -> bool {
        self.instruction_boundaries.contains(&addr)
    }

    /// Get all known instruction boundaries in a range [start, end).
    /// Returns addresses in sorted order.
    pub fn get_boundaries_in_range(&self, start: u32, end: u32) -> Vec<u32> {
        let mut result: Vec<u32> = self
            .instruction_boundaries
            .iter()
            .filter(|&&addr| addr >= start && addr < end)
            .copied()
            .collect();
        result.sort_unstable();
        result
    }

    /// Get the N nearest known boundaries before the given address.
    /// Returns in ascending order (oldest to most recent before addr).
    pub fn get_boundaries_before(&self, addr: u32, count: usize) -> Vec<u32> {
        let mut before: Vec<u32> = self
            .instruction_boundaries
            .iter()
            .filter(|&&a| a < addr)
            .copied()
            .collect();
        before.sort_unstable();
        // Take the last `count` elements (closest to addr)
        if before.len() > count {
            before.split_off(before.len() - count)
        } else {
            before
        }
    }

    /// Clear all instruction boundaries (e.g., on ROM load or reset).
    pub fn clear_instruction_boundaries(&mut self) {
        self.instruction_boundaries.clear();
    }

    // ===== Coverage Bitmap =====

    /// Record that the 68K executed an instruction starting at `pc`.
    /// Cheap (~2 ns); called from every 68K step path.
    #[inline]
    pub fn record_coverage_68k(&mut self, pc: u32) {
        // 68K addresses are 24-bit on Genesis; mask off mirror bits so
        // PC values like 0xFF008000 land in the same slot as their
        // canonical 0x008000 counterpart.
        self.coverage_68k.set(pc & 0x00FF_FFFF);
    }

    /// Record that the Z80 executed an instruction starting at `pc`.
    #[inline]
    pub fn record_coverage_z80(&mut self, pc: u16) {
        self.coverage_z80.set(pc as u32);
    }

    /// Reset every coverage bit for the given CPU (0 = 68K, 1 = Z80).
    /// Called by `load_rom` when the new cartridge differs from the
    /// previously-loaded one; exposed via WASM as a manual user reset.
    pub fn clear_coverage(&mut self, cpu_id: usize) {
        match cpu_id {
            0 => self.coverage_68k.clear(),
            1 => self.coverage_z80.clear(),
            _ => {}
        }
    }

    /// `true` iff `addr` has been observed as an instruction start on
    /// the given CPU. Out-of-range CPU ids return `false`.
    pub fn is_covered(&self, cpu_id: usize, addr: u32) -> bool {
        match cpu_id {
            0 => self.coverage_68k.get(addr & 0x00FF_FFFF),
            1 => self.coverage_z80.get(addr & 0xFFFF),
            _ => false,
        }
    }

    /// `(total_addressable_bytes, covered_bytes)` for the given CPU.
    /// Returns `(0, 0)` for unknown CPU ids.
    pub fn coverage_stats(&self, cpu_id: usize) -> (u32, u32) {
        match cpu_id {
            0 => (self.coverage_68k.size(), self.coverage_68k.count()),
            1 => (self.coverage_z80.size(), self.coverage_z80.count()),
            _ => (0, 0),
        }
    }

    /// Largest covered address `<= addr` on the given CPU, or `None`
    /// when no covered address exists at or below it. `disassemble_window`
    /// does not use it: its walks probe [`Self::is_covered`].
    pub fn coverage_nearest_set_at_or_before(&self, cpu_id: usize, addr: u32) -> Option<u32> {
        match cpu_id {
            0 => self
                .coverage_68k
                .nearest_set_at_or_before(addr & 0x00FF_FFFF),
            1 => self.coverage_z80.nearest_set_at_or_before(addr & 0xFFFF),
            _ => None,
        }
    }

    /// Smallest covered address `>= addr` on the given CPU. `disassemble_window`
    /// does not use it either: its forward walk steps over gaps a byte at a time.
    pub fn coverage_nearest_set_at_or_after(&self, cpu_id: usize, addr: u32) -> Option<u32> {
        match cpu_id {
            0 => self
                .coverage_68k
                .nearest_set_at_or_after(addr & 0x00FF_FFFF),
            1 => self.coverage_z80.nearest_set_at_or_after(addr & 0xFFFF),
            _ => None,
        }
    }

    // ===== Tracing Methods =====

    /// Enable or disable tracing.
    pub fn set_tracing_enabled(&mut self, enabled: bool) {
        self.tracing.set_enabled(enabled);
    }

    /// Check if 68K tracing is enabled.
    pub fn is_68k_tracing_enabled(&self) -> bool {
        self.tracing.is_enabled()
    }

    /// Pick the execution tier the system loop should run next, using
    /// the generic helper in `retro_core::debug::execution_tier`. The
    /// helper centralizes the rule so other cores share the same
    /// decision logic.
    pub fn execution_tier(&self) -> ExecutionTier {
        let traces = [self.tracing.is_enabled(), self.z80_tracing.is_enabled()];
        select_tier(TierSelectorInputs {
            execution_mode: self.execution_mode,
            has_breakpoints: !self.breakpoints.is_empty(),
            tracing_enabled_per_cpu: &traces,
        })
    }

    /// Check if the fast execution path can be used (no debug features active).
    /// Returns true when: execution mode is Running, no breakpoints, and tracing is off.
    pub fn can_use_fast_path(&self) -> bool {
        self.execution_tier() == ExecutionTier::Fast
    }

    /// Check if the lightweight debug path can be used.
    /// Returns true when in Running mode -- breakpoints and/or tracing are active
    /// but we only need breakpoint checks and trace recording, not the full debug
    /// overhead (disassembly, register snapshots, instruction history, state updates).
    pub fn can_use_light_debug_path(&self) -> bool {
        self.execution_mode == ExecutionMode::Running
    }

    /// Record a 68K trace entry. Returns true if buffer needs flush.
    pub fn record_trace(&mut self, entry: &TraceEntryBinary) -> bool {
        self.tracing.push_entry(&entry.to_bytes())
    }

    /// Check if trace buffer needs to be flushed.
    pub fn needs_trace_flush(&self) -> bool {
        self.tracing.needs_flush()
    }

    /// Get trace data ready for flushing.
    pub fn get_trace_flush_data(&self) -> &[u8] {
        self.tracing.get_flush_data()
    }

    /// Clear flushed trace data and reset buffer.
    pub fn clear_trace_flushed(&mut self) {
        self.tracing.clear_flushed();
    }

    /// Get tracing statistics.
    pub fn get_trace_stats(&self) -> TraceStats {
        TraceStats::from_state(&self.tracing)
    }

    // ===== Z80 Tracing =====

    /// Check if Z80 tracing is enabled.
    pub fn is_z80_tracing_enabled(&self) -> bool {
        self.z80_tracing.is_enabled()
    }

    /// Set Z80 tracing enabled/disabled.
    pub fn set_z80_tracing_enabled(&mut self, enabled: bool) {
        self.z80_tracing.set_enabled(enabled);
    }

    /// Record a Z80 trace entry from Z80 CPU state.
    /// `raw_bytes` contains up to 4 instruction bytes at the current PC,
    /// captured before execution so the trace can be accurately disassembled later.
    pub fn record_z80_trace(
        &mut self,
        z80: &crate::z80::Z80,
        cycles: u16,
        scanline: u16,
        raw_bytes: [u8; 4],
    ) {
        let entry = TraceEntryBinaryZ80 {
            pc: z80.pc,
            af: ((z80.a as u16) << 8) | (z80.f as u16),
            bc: ((z80.b as u16) << 8) | (z80.c as u16),
            de: ((z80.d as u16) << 8) | (z80.e as u16),
            hl: ((z80.h as u16) << 8) | (z80.l as u16),
            ix: z80.ix,
            iy: z80.iy,
            sp: z80.sp,
            i_reg: z80.i,
            r_reg: z80.r,
            im: z80.im,
            iff1: if z80.iff1 { 1 } else { 0 },
            iff2: if z80.iff2 { 1 } else { 0 },
            halt: if z80.halted { 1 } else { 0 },
            cycles,
            total_cycles: self.z80_total_cycles,
            scanline,
            raw_bytes,
        };
        self.z80_total_cycles += cycles as u64;
        self.z80_tracing.push_entry(&entry.to_bytes());
    }

    /// Check if Z80 trace buffer needs flushing.
    pub fn needs_z80_trace_flush(&self) -> bool {
        self.z80_tracing.needs_flush()
    }

    /// Get Z80 trace data ready for flushing.
    pub fn get_z80_trace_flush_data(&self) -> &[u8] {
        self.z80_tracing.get_flush_data()
    }

    /// Clear flushed Z80 trace data.
    pub fn clear_z80_trace_flushed(&mut self) {
        self.z80_tracing.clear_flushed();
    }

    /// Record Z80 instruction in history.
    pub fn record_z80_history(&mut self, trace: InstructionTrace) {
        if self.z80_instruction_history.len() >= MAX_INSTRUCTION_HISTORY {
            self.z80_instruction_history.pop_front();
        }
        self.z80_instruction_history.push_back(trace);
    }

    /// Get Z80 instruction history.
    pub fn get_z80_instruction_history(&self, count: u32) -> Vec<InstructionTrace> {
        let count = count as usize;
        let len = self.z80_instruction_history.len();
        let skip = len.saturating_sub(count);
        self.z80_instruction_history
            .iter()
            .skip(skip)
            .cloned()
            .collect()
    }

    /// Check if a Z80 breakpoint is hit at the given address. Z80 is
    /// CPU id 1; the underlying [`BreakpointList::check_execute`]
    /// filters so a 68K-scoped breakpoint (cpu_id=Some(0)) at the same
    /// numeric address never spuriously fires here.
    pub fn check_z80_breakpoint(&mut self, addr: u32) -> bool {
        self.breakpoints.check_execute(1, addr)
    }

    /// Push a stack frame (for JSR/BSR).
    pub fn push_call(&mut self, frame: StackFrame) {
        if self.call_stack.len() < MAX_CALL_STACK_DEPTH {
            self.call_stack.push(frame);
        }
    }

    /// Pop a stack frame (for RTS/RTR).
    pub fn pop_call(&mut self) -> Option<StackFrame> {
        self.call_stack.pop()
    }

    /// Update cached CPU states.
    pub fn update_cpu_states(&mut self, states: Vec<CpuState>) {
        self.cpu_states = states;
    }

    /// Check if execution / memory access should break at the given
    /// address on the 68K (CPU id 0). The probe's [`AccessFlags`] pick
    /// which checks run: `exec` matches execute breakpoints, `read` /
    /// `write` match memory watchpoints in that direction. Only one
    /// probe can fire per call so the hit counter stays accurate.
    ///
    /// 68K-scoping is intentional: the bus master here is the 68K, and
    /// `BreakpointList` filters by `cpu_id` so a Z80-scoped breakpoint
    /// at the same numeric address never spuriously fires.
    pub fn should_break(&mut self, addr: u32, bp_type: BreakpointType) -> bool {
        match bp_type {
            BreakpointType::Access(flags) => {
                (flags.exec && self.breakpoints.check_execute(0, addr))
                    || (flags.read && self.breakpoints.check_memory_access(0, addr, false))
                    || (flags.write && self.breakpoints.check_memory_access(0, addr, true))
            }
            // Event breakpoints are a core-defined hook, matched through `check_event`
            // at the frame loop's raster and interrupt points, so they never fire here.
            BreakpointType::Event { .. } => false,
        }
    }

    /// Like [`Self::should_break`] but supplies an [`ExprContext`] so an
    /// execute breakpoint carrying a conditional expression (`MatchSpec.expr`,
    /// e.g. `d0 == 5 && [0xFF0000].w > 0`) is evaluated against live
    /// registers / memory. Build the context from the 68K CPU + bus at the
    /// fetch site (see [`crate::expr_ctx::M68kExprContext`]). Used on the
    /// execute-breakpoint fetch path; the value-agnostic [`Self::should_break`]
    /// remains for callers without machine state, where conditional
    /// breakpoints fail closed.
    pub fn should_break_ctx(
        &mut self,
        addr: u32,
        bp_type: BreakpointType,
        ctx: &mut dyn ExprContext,
    ) -> bool {
        match bp_type {
            BreakpointType::Access(flags) => {
                (flags.exec && self.breakpoints.check_execute_ctx(0, addr, Some(&mut *ctx)))
                    || (flags.read && self.breakpoints.check_memory_access(0, addr, false))
                    || (flags.write && self.breakpoints.check_memory_access(0, addr, true))
            }
            BreakpointType::Event { .. } => false,
        }
    }

    /// Value-aware memory-watchpoint check on the 68K (CPU id 0): like
    /// [`Self::should_break`] but supplies the byte observed at `addr` so
    /// value-matched watchpoints can fire. Only the read/write probes the
    /// watchpoint drain produces are honoured (execute breakpoints carry
    /// no observed value and are checked on the fetch path instead).
    pub fn should_break_valued(&mut self, addr: u32, bp_type: BreakpointType, value: u8) -> bool {
        match bp_type {
            BreakpointType::Access(flags) => {
                let v = value as u32;
                (flags.read
                    && self
                        .breakpoints
                        .check_memory_access_valued(0, addr, false, v))
                    || (flags.write
                        && self
                            .breakpoints
                            .check_memory_access_valued(0, addr, true, v))
            }
            BreakpointType::Event { .. } => false,
        }
    }

    /// Batch memory-watchpoint check on the 68K (CPU id 0) for all byte
    /// accesses recorded during one instruction. Each watchpoint fires at
    /// most once, and multi-byte watchpoints (`value_size > 1`) assemble
    /// their full big-endian word from memory via `peek` (a
    /// side-effect-free byte reader). The 68000 is big-endian, so the
    /// first watched byte is the most-significant.
    pub fn should_break_watch_batch<F: FnMut(u32) -> u8>(
        &mut self,
        accesses: &[(u32, bool, u8)],
        peek: F,
    ) -> bool {
        self.breakpoints
            .check_memory_access_batch(0, accesses, true, peek)
    }

    /// Like [`Self::should_break_watch_batch`] but threads an
    /// [`ExprContext`] so a conditional watchpoint (`MatchSpec.expr`) is
    /// evaluated against live registers / memory. The context also supplies
    /// the observed value for sized watches (via
    /// [`ExprContext::read_mem`]), so a single 68K bus borrow serves both
    /// the value peek and the expression, with no separate peek closure.
    pub fn should_break_watch_batch_ctx(
        &mut self,
        accesses: &[(u32, bool, u8)],
        ctx: &mut dyn ExprContext,
    ) -> bool {
        self.breakpoints
            .check_memory_access_batch_ctx(0, accesses, ctx)
    }

    /// Whether any enabled breakpoint watches memory reads or writes, or is an
    /// event breakpoint. The system uses this to decide whether to arm 68K
    /// bus-access recording for the next instruction and whether to check the
    /// frame's event breakpoints; when it is false (and the instruction access
    /// log is off) the bus stays on its zero-overhead fast path.
    pub fn has_watchpoints(&self) -> bool {
        self.breakpoints.has_watchpoints_or_events()
    }

    /// Mutable access to the shared breakpoint list, so the cartridge
    /// debugger can append ROM/SRAM breakpoints to the *same* list the CPU
    /// step path checks (no parallel breakpoint state). Mirrors
    /// `SnesDebugger::breakpoints_mut`.
    pub fn breakpoints_mut(&mut self) -> &mut BreakpointList {
        &mut self.breakpoints
    }

    /// Check whether a hardware [`GenesisEvent`] matches an armed `Event`
    /// breakpoint. The frame-loop hook sites (VBlank / VInt / HInt /
    /// scanline) call this so the host can break on a raster/interrupt
    /// event, not just a PC or memory address. Mirrors
    /// `SnesDebugger::check_event`.
    pub fn check_event(&mut self, event: GenesisEvent) -> bool {
        let (code, param) = event.to_code_param();
        self.breakpoints.check_event(code, param)
    }
}

impl Default for GenesisDebugger {
    fn default() -> Self {
        Self::new()
    }
}

impl DebuggerCore for GenesisDebugger {
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

    /// The Genesis event taxonomy (see [`genesis_events`]). The interrupt
    /// classes (VBlank / VInt / HInt) are paramless; `Scanline` carries a
    /// free-form `V`-counter param. The hook sites live in the
    /// `run_frame_debug_light` / `run_frame` tiers.
    fn available_events(&self) -> Vec<EventClass> {
        let paramless = |code: u32, label: &str, description: &str| EventClass {
            code,
            label: label.to_string(),
            description: Some(description.to_string()),
            param_label: None,
            param_options: Vec::new(),
        };
        vec![
            paramless(
                genesis_events::VBLANK,
                "VBlank",
                "The beam entered vertical blank",
            ),
            paramless(
                genesis_events::VINT,
                "VInt",
                "A VBlank interrupt (68000 level 6) was delivered",
            ),
            paramless(
                genesis_events::HINT,
                "HInt",
                "An HBlank interrupt (68000 level 4) was delivered",
            ),
            EventClass {
                code: genesis_events::SCANLINE,
                label: "Scanline".to_string(),
                description: Some("A rendered scanline matched the V counter".to_string()),
                param_label: Some("Scanline (V)".to_string()),
                param_options: Vec::new(),
            },
        ]
    }

    fn get_execution_mode(&self) -> ExecutionMode {
        self.execution_mode
    }

    fn set_execution_mode(&mut self, mode: ExecutionMode) {
        self.execution_mode = mode;
    }

    fn read_memory(&self, _region: MemoryRegion, _addr: u32, _size: u32) -> Vec<u8> {
        // Memory access requires bus reference, which debugger doesn't have
        // This will be delegated through GenesisEmulator
        Vec::new()
    }

    fn write_memory(&mut self, _region: MemoryRegion, _addr: u32, _data: &[u8]) {
        // Memory access requires bus reference, which debugger doesn't have
        // This will be delegated through GenesisEmulator
    }

    fn get_memory_regions(&self) -> Vec<MemoryRegionInfo> {
        vec![
            MemoryRegionInfo {
                region: MemoryRegion::MainRam,
                name: "Work RAM".to_string(),
                start_address: 0xFF0000,
                size: 0x10000, // 64KB
                writable: true,
            },
            MemoryRegionInfo {
                region: MemoryRegion::VideoRam,
                name: "VRAM".to_string(),
                start_address: 0xC00000,
                size: 0x10000, // 64KB
                writable: true,
            },
            MemoryRegionInfo {
                region: MemoryRegion::Rom,
                name: "ROM".to_string(),
                start_address: 0x000000,
                size: 0x400000, // 4MB max
                writable: false,
            },
            MemoryRegionInfo {
                region: MemoryRegion::Custom(1),
                name: "Z80 RAM".to_string(),
                start_address: 0xA00000,
                size: 0x2000, // 8KB
                writable: true,
            },
        ]
    }

    fn disassemble(&self, _cpu_id: usize, _addr: u32, _count: u32) -> Vec<Instruction> {
        // Disassembly requires bus reference to read instructions
        // This will be delegated through GenesisEmulator
        Vec::new()
    }

    fn get_call_stack(&self, cpu_id: usize) -> Vec<StackFrame> {
        // Currently only CPU 0 (68000) has call stack tracking
        if cpu_id == 0 {
            self.call_stack.clone()
        } else {
            Vec::new()
        }
    }

    fn get_instruction_history(&self, cpu_id: usize, count: u32) -> Vec<InstructionTrace> {
        let history = match cpu_id {
            0 => &self.instruction_history,
            1 => &self.z80_instruction_history,
            _ => return Vec::new(),
        };

        let count = count as usize;
        let start = if history.len() > count {
            history.len() - count
        } else {
            0
        };
        // Return in reverse order (most recent first)
        let mut result: Vec<_> = history.range(start..).cloned().collect();
        result.reverse();
        result
    }

    fn video_debugger(&mut self) -> Option<&mut dyn video::VideoDebugger> {
        // The debugger owns no bus reference; the host drives the live video
        // debugger through the inherent `GenesisEmulator::video_debugger()`
        // (which borrows the VDP off the bus), mirroring `SnesDebugger`.
        None
    }

    fn audio_debugger(&mut self) -> Option<&mut dyn audio::AudioDebugger> {
        // As with `video_debugger`, the live audio debugger is reached via
        // the inherent `GenesisEmulator::audio_debugger()` (the YM2612 off
        // the bus); the debugger struct holds no bus reference.
        None
    }

    // ===== Per-CPU Tracing =====

    fn set_tracing_enabled(&mut self, cpu_id: usize, enabled: bool) {
        match cpu_id {
            0 => self.tracing.set_enabled(enabled),
            1 => self.z80_tracing.set_enabled(enabled),
            _ => {}
        }
    }

    fn is_tracing_enabled(&self, cpu_id: usize) -> bool {
        match cpu_id {
            0 => self.tracing.is_enabled(),
            1 => self.z80_tracing.is_enabled(),
            _ => false,
        }
    }

    fn needs_trace_flush(&self) -> bool {
        self.tracing.needs_flush() || self.z80_tracing.needs_flush()
    }

    fn get_trace_flush_data(&mut self, cpu_id: usize) -> Option<Vec<u8>> {
        match cpu_id {
            0 => {
                let data = self.tracing.get_flush_data();
                if data.is_empty() {
                    None
                } else {
                    Some(data.to_vec())
                }
            }
            1 => {
                let data = self.z80_tracing.get_flush_data();
                if data.is_empty() {
                    None
                } else {
                    Some(data.to_vec())
                }
            }
            _ => None,
        }
    }

    fn clear_trace_flushed(&mut self, cpu_id: usize) {
        match cpu_id {
            0 => self.tracing.clear_flushed(),
            1 => self.z80_tracing.clear_flushed(),
            _ => {}
        }
    }

    fn traceable_cpu_count(&self) -> usize {
        2 // 68K and Z80
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_debugger() -> GenesisDebugger {
        let system_info = SystemInfo {
            name: "Test System".to_string(),
            short_name: "test".to_string(),
            manufacturer: "Test".to_string(),
            year: 2024,
            cpus: vec![CpuInfo {
                id: 0,
                name: "Test CPU".to_string(),
                architecture: CpuArchitecture::M68000,
                clock_speed: 1000000,
                registers: vec![],
                location: CpuLocation::Motherboard,
                address_width: 24,
            }],
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
        };

        GenesisDebugger::new_with_system_info(system_info)
    }

    #[test]
    fn test_breakpoint_add_remove() {
        let mut debugger = create_test_debugger();

        // Add breakpoint
        let bp = Breakpoint {
            address: 0x000400,
            bp_type: BreakpointType::execute(),
            cpu_id: Some(0),
            condition: None,
        };

        let id = debugger.add_breakpoint(bp);
        assert_eq!(id, 0);

        // List breakpoints
        let breakpoints = debugger.list_breakpoints();
        assert_eq!(breakpoints.len(), 1);
        assert_eq!(breakpoints[0].id, 0);
        assert_eq!(breakpoints[0].breakpoint.address, 0x000400);
        assert!(breakpoints[0].enabled);

        // Remove breakpoint
        debugger.remove_breakpoint(id);
        let breakpoints = debugger.list_breakpoints();
        assert_eq!(breakpoints.len(), 0);
    }

    #[test]
    fn test_breakpoint_multiple() {
        let mut debugger = create_test_debugger();

        // Add multiple breakpoints
        let bp1 = Breakpoint {
            address: 0x000400,
            bp_type: BreakpointType::execute(),
            cpu_id: Some(0),
            condition: None,
        };
        let bp2 = Breakpoint {
            address: 0x000500,
            bp_type: BreakpointType::read(),
            cpu_id: Some(0),
            condition: None,
        };

        let id1 = debugger.add_breakpoint(bp1);
        let id2 = debugger.add_breakpoint(bp2);

        assert_eq!(id1, 0);
        assert_eq!(id2, 1);

        let breakpoints = debugger.list_breakpoints();
        assert_eq!(breakpoints.len(), 2);
    }

    #[test]
    fn test_breakpoint_enable_disable() {
        let mut debugger = create_test_debugger();

        let bp = Breakpoint {
            address: 0x000400,
            bp_type: BreakpointType::execute(),
            cpu_id: Some(0),
            condition: None,
        };

        let id = debugger.add_breakpoint(bp);

        // Initially enabled
        assert!(debugger.list_breakpoints()[0].enabled);

        // Disable
        debugger.enable_breakpoint(id, false);
        assert!(!debugger.list_breakpoints()[0].enabled);

        // Enable again
        debugger.enable_breakpoint(id, true);
        assert!(debugger.list_breakpoints()[0].enabled);
    }

    #[test]
    fn test_should_break() {
        let mut debugger = create_test_debugger();

        let bp = Breakpoint {
            address: 0x000400,
            bp_type: BreakpointType::execute(),
            cpu_id: Some(0),
            condition: None,
        };

        debugger.add_breakpoint(bp);

        // Should break at 0x000400
        assert!(debugger.should_break(0x000400, BreakpointType::execute()));

        // Should not break at other address
        assert!(!debugger.should_break(0x000500, BreakpointType::execute()));

        // Should not break with different type
        assert!(!debugger.should_break(0x000400, BreakpointType::read()));
    }

    #[test]
    fn test_execution_mode() {
        let mut debugger = create_test_debugger();

        // Initial mode is Paused (for debugging)
        assert!(matches!(
            debugger.get_execution_mode(),
            ExecutionMode::Paused
        ));

        // Set to Paused
        debugger.set_execution_mode(ExecutionMode::Paused);
        assert!(matches!(
            debugger.get_execution_mode(),
            ExecutionMode::Paused
        ));

        // Set to StepInto
        debugger.set_execution_mode(ExecutionMode::StepInto);
        assert!(matches!(
            debugger.get_execution_mode(),
            ExecutionMode::StepInto
        ));
    }

    #[test]
    fn test_instruction_history() {
        let mut debugger = create_test_debugger();

        // Record some instructions
        for i in 0..10 {
            let trace = InstructionTrace {
                address: 0x000400 + i * 2,
                instruction: format!("NOP #{}", i),
                registers_before: vec![],
                cycle_count: 4,
            };
            debugger.record_instruction(trace);
        }

        // Get history
        let history = debugger.get_instruction_history(0, 5);
        assert_eq!(history.len(), 5);

        // Most recent should be at the beginning
        assert_eq!(history[0].address, 0x000400 + 9 * 2);
        assert_eq!(history[4].address, 0x000400 + 5 * 2);
    }

    #[test]
    fn test_instruction_history_ring_buffer() {
        let mut debugger = create_test_debugger();

        // Record more than MAX_INSTRUCTION_HISTORY instructions
        for i in 0..1100 {
            let trace = InstructionTrace {
                address: i * 2,
                instruction: format!("INST #{}", i),
                registers_before: vec![],
                cycle_count: 4,
            };
            debugger.record_instruction(trace);
        }

        // Should only keep the last MAX_INSTRUCTION_HISTORY (1000)
        let history = debugger.get_instruction_history(0, 2000);
        assert_eq!(history.len(), 1000);

        // Should have the most recent ones
        assert_eq!(history[0].address, (1099 * 2));
    }

    #[test]
    fn test_call_stack() {
        let mut debugger = create_test_debugger();

        // Push some stack frames
        let frame1 = StackFrame {
            return_address: 0x000400,
            call_address: 0x000200,
            function_name: Some("main".to_string()),
        };
        let frame2 = StackFrame {
            return_address: 0x000500,
            call_address: 0x000400,
            function_name: Some("sub1".to_string()),
        };

        debugger.push_call(frame1.clone());
        debugger.push_call(frame2.clone());

        let stack = debugger.get_call_stack(0);
        assert_eq!(stack.len(), 2);
        assert_eq!(stack[0].return_address, 0x000400);
        assert_eq!(stack[1].return_address, 0x000500);

        // Pop a frame
        debugger.pop_call();
        let stack = debugger.get_call_stack(0);
        assert_eq!(stack.len(), 1);
        assert_eq!(stack[0].return_address, 0x000400);
    }

    #[test]
    fn test_memory_regions() {
        let debugger = create_test_debugger();

        let regions = debugger.get_memory_regions();
        assert!(!regions.is_empty());

        // Should have at least Work RAM
        let ram_region = regions.iter().find(|r| r.name == "Work RAM");
        assert!(ram_region.is_some());
    }

    #[test]
    fn test_cpu_states() {
        let mut debugger = create_test_debugger();

        let cpu_state = CpuState {
            cpu_id: 0,
            registers: vec![
                RegisterValue {
                    name: "PC".to_string(),
                    value: 0x000400,
                },
                RegisterValue {
                    name: "SR".to_string(),
                    value: 0x2700,
                },
            ],
            pc: 0x000400,
            flags: 0x2700,
        };

        debugger.update_cpu_states(vec![cpu_state]);

        let states = debugger.get_cpu_states();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].pc, 0x000400);
    }

    #[test]
    fn available_events_lists_the_genesis_taxonomy() {
        let debugger = create_test_debugger();
        let events = debugger.available_events();
        let codes: Vec<u32> = events.iter().map(|e| e.code).collect();
        assert_eq!(
            codes,
            vec![
                genesis_events::VBLANK,
                genesis_events::VINT,
                genesis_events::HINT,
                genesis_events::SCANLINE,
            ]
        );
        // Only the scanline class carries a selectable param.
        let scanline = events
            .iter()
            .find(|e| e.code == genesis_events::SCANLINE)
            .unwrap();
        assert!(scanline.param_label.is_some());
        assert!(events
            .iter()
            .filter(|e| e.code != genesis_events::SCANLINE)
            .all(|e| e.param_label.is_none()));
    }

    #[test]
    fn genesis_event_to_code_param_round_trips() {
        assert_eq!(
            GenesisEvent::VBlank.to_code_param(),
            (genesis_events::VBLANK, 0)
        );
        assert_eq!(
            GenesisEvent::VInt.to_code_param(),
            (genesis_events::VINT, 0)
        );
        assert_eq!(
            GenesisEvent::HInt.to_code_param(),
            (genesis_events::HINT, 0)
        );
        assert_eq!(
            GenesisEvent::Scanline(224).to_code_param(),
            (genesis_events::SCANLINE, 224)
        );
    }

    #[test]
    fn check_event_matches_armed_event_breakpoint() {
        let mut debugger = create_test_debugger();
        // Nothing armed -> the hot-path guard is off and no event fires.
        assert!(!debugger.has_watchpoints());
        assert!(!debugger.check_event(GenesisEvent::VBlank));

        debugger.add_breakpoint(Breakpoint {
            address: 0,
            bp_type: BreakpointType::Event {
                code: genesis_events::VBLANK,
                param: 0,
            },
            cpu_id: None,
            condition: None,
        });
        assert!(debugger.has_watchpoints());
        assert!(debugger.check_event(GenesisEvent::VBlank));
        // A different class does not match the VBlank hook.
        assert!(!debugger.check_event(GenesisEvent::HInt));
    }

    #[test]
    fn check_event_scanline_param_must_match() {
        let mut debugger = create_test_debugger();
        debugger.add_breakpoint(Breakpoint {
            address: 0,
            bp_type: BreakpointType::Event {
                code: genesis_events::SCANLINE,
                param: 100,
            },
            cpu_id: None,
            condition: None,
        });
        assert!(debugger.check_event(GenesisEvent::Scanline(100)));
        assert!(!debugger.check_event(GenesisEvent::Scanline(101)));
    }
}
