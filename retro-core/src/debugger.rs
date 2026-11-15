//! Generic debugging interface for emulators.

use crate::debug::state::BreakpointChecks;
use crate::debug::trace::TraceStats;
use crate::types::*;

/// What a host reads and changes of a core's debugger. One implementation
/// serves every core: [`crate::debug::DebuggerState`].
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

    /// Catalogue of event classes this core can break on, used to populate
    /// the UI's event-breakpoint picker: each code, its [`EventKind`], its
    /// param's meaning and enumerable param options. The default is empty:
    /// a core with no events advertises nothing.
    fn available_events(&self) -> Vec<EventClass> {
        Vec::new()
    }

    /// Get current execution mode.
    fn get_execution_mode(&self) -> ExecutionMode;

    /// Set execution mode.
    fn set_execution_mode(&mut self, mode: ExecutionMode);

    /// Get available memory regions.
    fn get_memory_regions(&self) -> Vec<MemoryRegionInfo>;

    /// The calls a CPU made and has not returned from, the outermost first;
    /// empty for a CPU that keeps no call stack.
    fn get_call_stack(&self, cpu_id: usize) -> Vec<StackFrame>;

    /// The newest `count` records of a CPU's instruction history, newest
    /// first; empty for a CPU that keeps none.
    fn get_instruction_history(&self, cpu_id: usize, count: u32) -> Vec<InstructionTrace>;

    // ===== Per-CPU coverage =====

    /// Whether the CPU has run an instruction from `address` (masked to its
    /// address space); `false` for a CPU that keeps no coverage.
    fn is_covered(&self, cpu_id: usize, address: u32) -> bool;

    /// `(addresses the coverage spans, addresses covered)` for a CPU;
    /// `(0, 0)` for one that keeps no coverage.
    fn coverage_stats(&self, cpu_id: usize) -> (u32, u32);

    /// Forget a CPU's coverage.
    fn clear_coverage(&mut self, cpu_id: usize);

    // ===== Per-CPU tracing =====

    /// Enable or disable tracing for a specific CPU. A CPU with no trace
    /// ignores it.
    fn set_tracing_enabled(&mut self, cpu_id: usize, enabled: bool);

    /// Check if tracing is enabled for a specific CPU.
    fn is_tracing_enabled(&self, cpu_id: usize) -> bool;

    /// Whether a CPU's trace ring asks to be flushed.
    fn needs_trace_flush(&self, cpu_id: usize) -> bool;

    /// Whether any CPU's trace ring asks to be flushed.
    fn any_trace_needs_flush(&self) -> bool;

    /// Get trace data for a specific CPU (binary, for flushing).
    /// Returns None if no data or cpu_id is invalid.
    fn get_trace_flush_data(&self, cpu_id: usize) -> Option<Vec<u8>>;

    /// Clear flushed trace data for a specific CPU.
    fn clear_trace_flushed(&mut self, cpu_id: usize);

    /// The size of a CPU's trace entries, `None` for a CPU with no trace.
    fn trace_entry_size(&self, cpu_id: usize) -> Option<usize>;

    /// A CPU's trace statistics (all 0 until its tracing is first enabled),
    /// `None` for a CPU with no trace.
    fn trace_stats(&self, cpu_id: usize) -> Option<TraceStats>;

    /// How many CPUs keep a trace.
    fn traceable_cpu_count(&self) -> usize;

    // ===== What is checked =====

    /// What a CPU's breakpoints are checked for.
    fn breakpoint_checks(&self, cpu_id: usize) -> BreakpointChecks;
}
