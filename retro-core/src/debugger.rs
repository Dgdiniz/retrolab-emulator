//! Generic debugging interface for emulators.

use crate::audio::AudioDebugger;
use crate::types::*;
use crate::video::VideoDebugger;

/// Core debugging interface that all emulators must implement.
pub trait DebuggerCore {
    /// Get current CPU states for all CPUs in the system.
    fn get_cpu_states(&self) -> Vec<CpuState>;

    /// Get CPU info for a specific CPU.
    fn get_cpu_info(&self, cpu_id: usize) -> Option<&CpuInfo>;

    /// Add a breakpoint. Returns the breakpoint ID.
    fn add_breakpoint(&mut self, bp: Breakpoint) -> BreakpointId;

    /// Remove a breakpoint by ID.
    fn remove_breakpoint(&mut self, id: BreakpointId);

    /// Enable or disable a breakpoint.
    fn enable_breakpoint(&mut self, id: BreakpointId, enabled: bool);

    /// Replace the access flags (exec/read/write) of an existing
    /// breakpoint in place, preserving its id, enabled state, and hit
    /// count. No-op if the id is unknown or the breakpoint is an event
    /// hook. Backs the per-row R/W/X checkboxes in the debugger UI.
    fn set_breakpoint_access(&mut self, id: BreakpointId, access: AccessFlags);

    /// Replace the advanced [`MatchSpec`] (address range, value predicate,
    /// hit-skip, action, one-shot) of an existing breakpoint in place,
    /// preserving its id, type, enabled state, and accrued hit count.
    /// No-op if the id is unknown. Backs the watchpoints panel's
    /// condition editors.
    fn set_breakpoint_match_spec(&mut self, id: BreakpointId, spec: MatchSpec);

    /// Get list of all breakpoints.
    fn list_breakpoints(&self) -> Vec<BreakpointInfo>;

    /// Catalog of event classes this core can break on, used to populate
    /// the UI's event-breakpoint picker. The default is empty: a core
    /// with no event taxonomy wired advertises nothing. Cores that
    /// support [`BreakpointType::Event`] override this to list their
    /// codes, param semantics, and enumerable param options.
    fn available_events(&self) -> Vec<EventClass> {
        Vec::new()
    }

    /// Get current execution mode.
    fn get_execution_mode(&self) -> ExecutionMode;

    /// Set execution mode.
    fn set_execution_mode(&mut self, mode: ExecutionMode);

    /// Read memory from a region.
    fn read_memory(&self, region: MemoryRegion, addr: u32, size: u32) -> Vec<u8>;

    /// Write memory to a region.
    fn write_memory(&mut self, region: MemoryRegion, addr: u32, data: &[u8]);

    /// Get available memory regions.
    fn get_memory_regions(&self) -> Vec<MemoryRegionInfo>;

    /// Disassemble instructions starting at an address.
    fn disassemble(&self, cpu_id: usize, addr: u32, count: u32) -> Vec<Instruction>;

    /// Get call stack for a CPU.
    fn get_call_stack(&self, cpu_id: usize) -> Vec<StackFrame>;

    /// Get instruction execution history.
    fn get_instruction_history(&self, cpu_id: usize, count: u32) -> Vec<InstructionTrace>;

    /// Get video debugger if available.
    fn video_debugger(&mut self) -> Option<&mut dyn VideoDebugger>;

    /// Get audio debugger if available.
    fn audio_debugger(&mut self) -> Option<&mut dyn AudioDebugger>;

    // ===== Per-CPU Tracing =====

    /// Enable or disable tracing for a specific CPU.
    fn set_tracing_enabled(&mut self, cpu_id: usize, enabled: bool);

    /// Check if tracing is enabled for a specific CPU.
    fn is_tracing_enabled(&self, cpu_id: usize) -> bool;

    /// Check if any CPU's trace buffer needs flushing.
    fn needs_trace_flush(&self) -> bool;

    /// Get trace data for a specific CPU (binary, for flushing).
    /// Returns None if no data or cpu_id is invalid.
    fn get_trace_flush_data(&mut self, cpu_id: usize) -> Option<Vec<u8>>;

    /// Clear flushed trace data for a specific CPU.
    fn clear_trace_flushed(&mut self, cpu_id: usize);

    /// Get the number of CPUs that have tracing support.
    fn traceable_cpu_count(&self) -> usize;
}
