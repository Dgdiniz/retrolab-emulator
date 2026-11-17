//! The debugger every console core keeps: one [`DebuggerState`] holding the
//! breakpoints, the execution mode, the focused CPU, the cached CPU states
//! and, for each CPU, its coverage, its trace ring, its instruction history
//! and its call stack.
//!
//! A core describes its CPUs as [`CpuDebugSpec`]s (how much address space
//! coverage spans, how wide a trace entry is, whether a history is kept,
//! what its breakpoints are checked for) and the rest of its console as a
//! [`ConsoleDebug`] (its event catalogue, its memory regions, anything of
//! its own the debugger keeps). Everything else, the state and the
//! [`DebuggerCore`] a host reads, is written once here. A core's own code
//! keeps what its hardware makes different: decoding its CPUs, building
//! their trace entries and register snapshots, and where in its frame loops
//! it calls the checks.

use std::collections::VecDeque;
use std::hash::Hash;

use serde::{Deserialize, Serialize};

use crate::debug::access::Access;
use crate::debug::breakpoints::BreakpointList;
use crate::debug::coverage::CoverageBitmap;
use crate::debug::events::{event_catalog, DebugEvent, EventSpec, RaisedEvents};
use crate::debug::execution_tier::{select_tier, ExecutionTier, TierSelectorInputs};
use crate::debug::expr::ExprContext;
use crate::debug::trace::{TraceEntry, TraceStats, TracingState};
use crate::debugger::DebuggerCore;
use crate::types::{
    AccessFlags, Breakpoint, BreakpointId, BreakpointInfo, CpuInfo, CpuState, EventClass,
    ExecutionMode, InstructionTrace, MatchSpec, MemoryRegionInfo, StackFrame, SystemInfo,
};

/// What a CPU's breakpoints are checked for, as the core's loops check
/// them: a host offers a user only what is checked, and says why the rest
/// is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BreakpointChecks {
    /// Execute breakpoints stop the CPU before the instruction at their
    /// address.
    pub execute: bool,
    /// Read and write watchpoints stop it after an instruction that
    /// touched their bytes.
    pub memory: bool,
    /// A breakpoint's condition is evaluated against the CPU's registers
    /// and memory.
    pub conditions: bool,
}

impl BreakpointChecks {
    /// Everything is checked.
    pub const ALL: Self = Self {
        execute: true,
        memory: true,
        conditions: true,
    };

    /// Nothing is checked.
    pub const NONE: Self = Self {
        execute: false,
        memory: false,
        conditions: false,
    };
}

/// How the debugger keeps one CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuDebugSpec {
    /// Coverage spans addresses `0..coverage_size`; 0 keeps no coverage.
    pub coverage_size: u32,
    /// Applied to an address before coverage records or reads it, so a
    /// mirror of the CPU's space lands on the address it mirrors.
    pub address_mask: u32,
    /// The size of the CPU's trace entries; 0 keeps no trace.
    pub trace_entry_size: usize,
    /// The CPU keeps an instruction history and a call stack.
    pub history: bool,
    /// What its breakpoints are checked for.
    pub checks: BreakpointChecks,
}

impl CpuDebugSpec {
    /// A slot with no CPU in it: no coverage, no trace, no history, nothing
    /// checked.
    pub const NO_CPU: Self = Self {
        coverage_size: 0,
        address_mask: 0,
        trace_entry_size: 0,
        history: false,
        checks: BreakpointChecks::NONE,
    };
}

/// The most instructions a CPU's history keeps.
pub const HISTORY_CAPACITY: usize = 1024;

/// The deepest a CPU's call stack goes; calls past it are not kept.
pub const CALL_STACK_DEPTH: usize = 256;

/// The size, in MiB, a trace ring starts at when its CPU's tracing is first
/// enabled; it grows as the trace needs.
const TRACE_RING_START_MIB: usize = 8;

/// A ring of the newest entries pushed, at most `capacity` of them: a push
/// past it forgets the oldest. A ring of capacity 0 keeps nothing.
#[derive(Debug, Clone, Default)]
pub struct HistoryRing<T> {
    entries: VecDeque<T>,
    capacity: usize,
}

impl<T> HistoryRing<T> {
    /// An empty ring keeping at most `capacity` entries.
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            capacity,
        }
    }

    /// Keep `entry` as the newest, forgetting the oldest when full.
    pub fn push(&mut self, entry: T) {
        if self.capacity == 0 {
            return;
        }
        if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    /// The newest `count` entries, the newest first.
    pub fn newest_first(&self, count: usize) -> Vec<T>
    where
        T: Clone,
    {
        self.entries.iter().rev().take(count).cloned().collect()
    }

    /// How many entries the ring holds.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the ring holds nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Forget every entry.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// The calls a CPU made and has not returned from, at most `depth` of
/// them: a call past the depth is not kept (a return still pops). A stack
/// of depth 0 keeps nothing.
#[derive(Debug, Clone, Default)]
pub struct CallStack {
    frames: Vec<StackFrame>,
    depth: usize,
}

impl CallStack {
    /// An empty stack keeping at most `depth` frames.
    pub fn new(depth: usize) -> Self {
        Self {
            frames: Vec::new(),
            depth,
        }
    }

    /// Keep `frame` as the innermost call, unless the stack is full.
    pub fn push(&mut self, frame: StackFrame) {
        if self.frames.len() < self.depth {
            self.frames.push(frame);
        }
    }

    /// Take the innermost call off, `None` when there is none.
    pub fn pop(&mut self) -> Option<StackFrame> {
        self.frames.pop()
    }

    /// The calls, the outermost first.
    pub fn frames(&self) -> &[StackFrame] {
        &self.frames
    }

    /// Forget every call.
    pub fn clear(&mut self) {
        self.frames.clear();
    }
}

/// What the debugger keeps of one CPU, as its [`CpuDebugSpec`] says.
pub struct CpuDebug {
    spec: CpuDebugSpec,
    coverage: CoverageBitmap,
    trace: Option<TracingState>,
    history: HistoryRing<InstructionTrace>,
    call_stack: CallStack,
}

impl CpuDebug {
    /// A CPU's slot, empty: no coverage recorded, no trace ring yet (one is
    /// made when its tracing is first enabled), no history, no calls.
    pub fn new(spec: CpuDebugSpec) -> Self {
        let (history, depth) = if spec.history {
            (HISTORY_CAPACITY, CALL_STACK_DEPTH)
        } else {
            (0, 0)
        };
        Self {
            spec,
            coverage: CoverageBitmap::new(spec.coverage_size),
            trace: None,
            history: HistoryRing::new(history),
            call_stack: CallStack::new(depth),
        }
    }

    /// The spec the slot was made from.
    pub fn spec(&self) -> CpuDebugSpec {
        self.spec
    }

    /// Whether the CPU is being traced.
    #[inline]
    fn tracing(&self) -> bool {
        self.trace.as_ref().is_some_and(TracingState::is_enabled)
    }

    /// The trace statistics, `None` for a CPU with no trace.
    fn trace_stats(&self) -> Option<TraceStats> {
        if self.spec.trace_entry_size == 0 {
            return None;
        }
        Some(
            self.trace
                .as_ref()
                .map(TraceStats::from_state)
                .unwrap_or_default(),
        )
    }
}

/// The parts of a console's debugger that are not its CPUs: the events its
/// breakpoints can stop on, its memory regions, and whatever else of its
/// own the core keeps in the debugger.
pub trait ConsoleDebug {
    /// The console's event catalogue ([`crate::debug::events`]).
    const EVENTS: &'static [EventSpec];

    /// The memory regions a host can view.
    fn memory_regions(&self) -> Vec<MemoryRegionInfo>;
}

/// A console with no events and no memory regions.
impl ConsoleDebug for () {
    const EVENTS: &'static [EventSpec] = &[];

    fn memory_regions(&self) -> Vec<MemoryRegionInfo> {
        Vec::new()
    }
}

/// The slot of the CPU a frame loop steps: CPU 0 in every core.
pub const MAIN_CPU: usize = 0;

/// A console's debugger: `N` CPU slots and the console's own part `C`.
///
/// It starts running (`ExecutionMode::Running`) with CPU 0 focused. The
/// core fills the cached CPU states (`set_cpu_states`) and the system info
/// (`set_system_info`, whose CPU list bounds the focus) and calls the
/// recording and checking methods from its frame loops; a host reads it
/// through [`DebuggerCore`].
pub struct DebuggerState<const N: usize, C: ConsoleDebug> {
    breakpoints: BreakpointList,
    execution_mode: ExecutionMode,
    focused_cpu: usize,
    cpu_states: Vec<CpuState>,
    system_info: SystemInfo,
    cpus: [CpuDebug; N],
    /// For each CPU, the address its last execute check covered, when the
    /// next check there is to be skipped ([`Self::check_execute`]).
    checked_at: [Option<u32>; N],
    /// A check asked the frame call to stop: an event, or a watchpoint the
    /// instruction's accesses matched. The frame loop pauses after the step
    /// or the events that asked ([`crate::frame`]).
    stop_requested: bool,
    console: C,
}

impl<const N: usize, C: ConsoleDebug> DebuggerState<N, C> {
    /// A debugger for CPUs described by `cpus`, slot `i` for CPU id `i`.
    pub fn new(cpus: [CpuDebugSpec; N], system_info: SystemInfo, console: C) -> Self {
        Self {
            breakpoints: BreakpointList::new(),
            execution_mode: ExecutionMode::Running,
            focused_cpu: 0,
            cpu_states: Vec::new(),
            system_info,
            cpus: cpus.map(CpuDebug::new),
            checked_at: [None; N],
            stop_requested: false,
            console,
        }
    }

    /// Make CPU `cpu`'s slot anew from `spec` (a cartridge brought another
    /// CPU), forgetting what the slot kept; nothing changes when the slot
    /// already has this spec, or for an id past the slots.
    pub fn configure_cpu(&mut self, cpu: usize, spec: CpuDebugSpec) {
        if let Some(slot) = self.cpus.get_mut(cpu) {
            if slot.spec != spec {
                *slot = CpuDebug::new(spec);
                self.checked_at[cpu] = None;
            }
        }
    }

    /// The slot of CPU `cpu`, `None` past the slots.
    pub fn cpu(&self, cpu: usize) -> Option<&CpuDebug> {
        self.cpus.get(cpu)
    }

    /// CPU `cpu`'s coverage, `None` past the slots.
    pub fn coverage(&self, cpu: usize) -> Option<&CoverageBitmap> {
        self.cpus.get(cpu).map(|slot| &slot.coverage)
    }

    /// Replace the system info: the CPUs it lists answer
    /// [`DebuggerCore::get_cpu_info`] and bound the focus.
    pub fn set_system_info(&mut self, info: SystemInfo) {
        self.system_info = info;
    }

    /// Replace the cached CPU states [`DebuggerCore::get_cpu_states`]
    /// returns.
    pub fn set_cpu_states(&mut self, states: Vec<CpuState>) {
        self.cpu_states = states;
    }

    /// The CPU the debugger's views and steps follow.
    pub fn focused_cpu(&self) -> usize {
        self.focused_cpu
    }

    /// Focus CPU `cpu`; an id past the CPUs the system info lists is
    /// ignored.
    pub fn set_focused_cpu(&mut self, cpu: usize) {
        if cpu < self.system_info.cpus.len() {
            self.focused_cpu = cpu;
        }
    }

    /// The console's own part.
    pub fn console(&self) -> &C {
        &self.console
    }

    /// The console's own part, to change.
    pub fn console_mut(&mut self) -> &mut C {
        &mut self.console
    }

    /// The breakpoints.
    pub fn breakpoints(&self) -> &BreakpointList {
        &self.breakpoints
    }

    /// The breakpoints, to change (a cartridge debugger adding its own to
    /// the list the CPUs check).
    pub fn breakpoints_mut(&mut self) -> &mut BreakpointList {
        &mut self.breakpoints
    }

    /// Whether an enabled breakpoint watches memory or an event, so the
    /// core arms its access log and its event hooks.
    pub fn has_watchpoints_or_events(&self) -> bool {
        self.breakpoints.has_watchpoints_or_events()
    }

    /// The frame loop to run next, by the execution mode, the breakpoints
    /// and the traces ([`select_tier`]).
    pub fn execution_tier(&self) -> ExecutionTier {
        self.tier_in(self.execution_mode)
    }

    /// The frame loop the breakpoints and the traces would pick if the
    /// debugger were running, whatever its mode.
    pub fn running_tier(&self) -> ExecutionTier {
        self.tier_in(ExecutionMode::Running)
    }

    fn tier_in(&self, execution_mode: ExecutionMode) -> ExecutionTier {
        let tracing: [bool; N] = std::array::from_fn(|cpu| self.cpus[cpu].tracing());
        select_tier(TierSelectorInputs {
            execution_mode,
            has_breakpoints: !self.breakpoints.is_empty(),
            tracing_enabled_per_cpu: &tracing,
        })
    }

    /// Record that CPU `cpu` ran an instruction at `address`, masked to its
    /// space. Every tier calls it for every instruction: with a constant
    /// `cpu` it is the bitmap's store and the mask's load.
    #[inline(always)]
    pub fn record_coverage(&mut self, cpu: usize, address: u32) {
        if let Some(slot) = self.cpus.get_mut(cpu) {
            slot.coverage.set(address & slot.spec.address_mask);
        }
    }

    /// Whether CPU `cpu` is being traced.
    #[inline]
    pub fn tracing(&self, cpu: usize) -> bool {
        self.cpus.get(cpu).is_some_and(CpuDebug::tracing)
    }

    /// Append `entry` to CPU `cpu`'s trace while it is traced. Returns
    /// whether the ring asks to be flushed.
    #[inline]
    pub fn record_trace<E: TraceEntry>(&mut self, cpu: usize, entry: &E) -> bool {
        let Some(slot) = self.cpus.get_mut(cpu) else {
            return false;
        };
        debug_assert_eq!(
            E::SIZE,
            slot.spec.trace_entry_size,
            "a trace entry of another size"
        );
        match slot.trace.as_mut() {
            Some(trace) if trace.is_enabled() => trace.push_entry(entry.to_bytes().as_ref()),
            _ => false,
        }
    }

    /// Append entries already in bytes (a chip's own trace, drained) to CPU
    /// `cpu`'s trace while it is traced: each whole entry, a trailing part
    /// of one left out. Returns whether the ring asks to be flushed.
    pub fn record_trace_bytes(&mut self, cpu: usize, entries: &[u8]) -> bool {
        let Some(slot) = self.cpus.get_mut(cpu) else {
            return false;
        };
        let size = slot.spec.trace_entry_size;
        let Some(trace) = slot.trace.as_mut() else {
            return false;
        };
        if size == 0 || !trace.is_enabled() {
            return false;
        }
        let mut flush = false;
        for entry in entries.chunks_exact(size) {
            flush |= trace.push_entry(entry);
        }
        flush
    }

    /// CPU `cpu` made a call: keep `frame` as its innermost.
    pub fn push_call(&mut self, cpu: usize, frame: StackFrame) {
        if let Some(slot) = self.cpus.get_mut(cpu) {
            slot.call_stack.push(frame);
        }
    }

    /// CPU `cpu` returned: take its innermost call off.
    pub fn pop_call(&mut self, cpu: usize) {
        if let Some(slot) = self.cpus.get_mut(cpu) {
            slot.call_stack.pop();
        }
    }

    /// Keep `entry` as the newest instruction of CPU `cpu`'s history.
    pub fn record_history(&mut self, cpu: usize, entry: InstructionTrace) {
        if let Some(slot) = self.cpus.get_mut(cpu) {
            slot.history.push(entry);
        }
    }

    /// Forget every CPU's coverage (a different program was loaded).
    pub fn clear_all_coverage(&mut self) {
        for slot in &mut self.cpus {
            slot.coverage.clear();
        }
    }

    /// Check the execute breakpoints of CPU `cpu` before the instruction at
    /// `pc` runs; `true` stops.
    ///
    /// After a stop at an address, the next instruction the CPU runs is the
    /// one there, unchecked, so running on goes on from the stop: a hit
    /// marks the address, and the next check there clears the mark and
    /// passes. A check anywhere else clears the mark too.
    pub fn check_execute(
        &mut self,
        cpu: usize,
        pc: u32,
        ctx: Option<&mut dyn ExprContext>,
    ) -> bool {
        if let Some(mark) = self.checked_at.get_mut(cpu) {
            if mark.take() == Some(pc) {
                return false;
            }
        }
        let hit = self.breakpoints.check_execute(cpu, pc, ctx);
        if hit {
            if let Some(mark) = self.checked_at.get_mut(cpu) {
                *mark = Some(pc);
            }
        }
        hit
    }

    /// Check the execute breakpoints of CPU `cpu` at `next_pc`, after an
    /// instruction, for the instruction that runs next; `true` stops. The
    /// address is marked whether or not a breakpoint hits, so the check
    /// [`Self::check_execute`] makes there before that instruction passes:
    /// one check per instruction.
    pub fn check_execute_next(
        &mut self,
        cpu: usize,
        next_pc: u32,
        ctx: Option<&mut dyn ExprContext>,
    ) -> bool {
        if let Some(mark) = self.checked_at.get_mut(cpu) {
            *mark = Some(next_pc);
        }
        self.breakpoints.check_execute(cpu, next_pc, ctx)
    }

    /// A check asks the frame call to stop.
    pub fn request_stop(&mut self) {
        self.stop_requested = true;
    }

    /// Whether a check asked the frame call to stop.
    pub fn stop_requested(&self) -> bool {
        self.stop_requested
    }

    /// Forget a request to stop.
    pub fn clear_stop(&mut self) {
        self.stop_requested = false;
    }

    /// Forget CPU `cpu`'s mark: its next execute check checks.
    pub fn forget_checked(&mut self, cpu: usize) {
        if let Some(mark) = self.checked_at.get_mut(cpu) {
            *mark = None;
        }
    }

    /// The machine was replaced (a reset, a program or a savestate loaded):
    /// empty every CPU's call stack, whose calls belonged to the machine
    /// that is gone, and forget every mark. The histories stay: they
    /// record what ran.
    pub fn machine_replaced(&mut self) {
        for slot in &mut self.cpus {
            slot.call_stack.clear();
        }
        self.checked_at = [None; N];
    }

    /// Check the watchpoints of CPU `cpu` against the bytes one instruction
    /// accessed ([`BreakpointList::check_accesses`]); `true` stops.
    pub fn check_accesses(
        &mut self,
        cpu: usize,
        accesses: &[Access],
        ctx: &mut dyn ExprContext,
    ) -> bool {
        self.breakpoints.check_accesses(cpu, accesses, ctx)
    }

    /// Check the event breakpoints against `event`; `true` stops.
    pub fn check_event<E: DebugEvent>(&mut self, event: E) -> bool {
        let (code, param) = event.code_param();
        self.breakpoints.check_event(code, param)
    }

    /// Drain `raised` and check the event breakpoints against each event,
    /// every one of them (a breakpoint further down still counts its hit);
    /// `true` stops.
    pub fn check_raised<E: DebugEvent + Eq + Hash>(
        &mut self,
        raised: &mut RaisedEvents<E>,
    ) -> bool {
        let breakpoints = &mut self.breakpoints;
        let mut hit = false;
        raised.drain(|event| {
            let (code, param) = event.code_param();
            hit |= breakpoints.check_event(code, param);
        });
        hit
    }
}

impl<const N: usize, C: ConsoleDebug> DebuggerCore for DebuggerState<N, C> {
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

    fn available_events(&self) -> Vec<EventClass> {
        event_catalog(C::EVENTS)
    }

    fn get_execution_mode(&self) -> ExecutionMode {
        self.execution_mode
    }

    fn set_execution_mode(&mut self, mode: ExecutionMode) {
        self.execution_mode = mode;
    }

    fn execution_tier(&self) -> ExecutionTier {
        DebuggerState::execution_tier(self)
    }

    fn get_memory_regions(&self) -> Vec<MemoryRegionInfo> {
        self.console.memory_regions()
    }

    fn get_call_stack(&self, cpu_id: usize) -> Vec<StackFrame> {
        self.cpus
            .get(cpu_id)
            .map(|slot| slot.call_stack.frames().to_vec())
            .unwrap_or_default()
    }

    fn get_instruction_history(&self, cpu_id: usize, count: u32) -> Vec<InstructionTrace> {
        self.cpus
            .get(cpu_id)
            .map(|slot| slot.history.newest_first(count as usize))
            .unwrap_or_default()
    }

    fn set_tracing_enabled(&mut self, cpu_id: usize, enabled: bool) {
        let Some(slot) = self.cpus.get_mut(cpu_id) else {
            return;
        };
        let size = slot.spec.trace_entry_size;
        if size == 0 {
            return;
        }
        match slot.trace.as_mut() {
            Some(trace) => trace.set_enabled(enabled),
            None if enabled => {
                let mut trace = TracingState::new(TRACE_RING_START_MIB, size);
                trace.set_enabled(true);
                slot.trace = Some(trace);
            }
            None => {}
        }
    }

    fn is_tracing_enabled(&self, cpu_id: usize) -> bool {
        self.tracing(cpu_id)
    }

    fn get_trace_flush_data(&self, cpu_id: usize) -> Option<Vec<u8>> {
        let trace = self.cpus.get(cpu_id)?.trace.as_ref()?;
        trace.has_data().then(|| trace.get_flush_data().to_vec())
    }

    fn clear_trace_flushed(&mut self, cpu_id: usize) {
        if let Some(trace) = self
            .cpus
            .get_mut(cpu_id)
            .and_then(|slot| slot.trace.as_mut())
        {
            trace.clear_flushed();
        }
    }

    fn traceable_cpu_count(&self) -> usize {
        self.cpus
            .iter()
            .filter(|slot| slot.spec.trace_entry_size > 0)
            .count()
    }

    fn is_covered(&self, cpu_id: usize, address: u32) -> bool {
        self.cpus
            .get(cpu_id)
            .is_some_and(|slot| slot.coverage.get(address & slot.spec.address_mask))
    }

    fn coverage_stats(&self, cpu_id: usize) -> (u32, u32) {
        self.cpus
            .get(cpu_id)
            .map(|slot| (slot.coverage.size(), slot.coverage.count()))
            .unwrap_or((0, 0))
    }

    fn clear_coverage(&mut self, cpu_id: usize) {
        if let Some(slot) = self.cpus.get_mut(cpu_id) {
            slot.coverage.clear();
        }
    }

    fn needs_trace_flush(&self, cpu_id: usize) -> bool {
        self.cpus
            .get(cpu_id)
            .and_then(|slot| slot.trace.as_ref())
            .is_some_and(TracingState::needs_flush)
    }

    fn any_trace_needs_flush(&self) -> bool {
        self.cpus
            .iter()
            .filter_map(|slot| slot.trace.as_ref())
            .any(TracingState::needs_flush)
    }

    fn trace_entry_size(&self, cpu_id: usize) -> Option<usize> {
        self.cpus
            .get(cpu_id)
            .map(|slot| slot.spec.trace_entry_size)
            .filter(|&size| size > 0)
    }

    fn trace_stats(&self, cpu_id: usize) -> Option<TraceStats> {
        self.cpus.get(cpu_id)?.trace_stats()
    }

    fn breakpoint_checks(&self, cpu_id: usize) -> BreakpointChecks {
        self.cpus
            .get(cpu_id)
            .map_or(BreakpointChecks::NONE, |slot| slot.spec.checks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debug::events::{EventParam, RaisedEvents};
    use crate::debug::expr::{ByteOrder, CpuExprContext};
    use crate::types::{
        AudioInfo, BreakpointType, CpuArchitecture, CpuLocation, DisplayInfo, EventKind, InputInfo,
        MemoryRegion, PixelFormat, BREAKPOINT_PARAM_ANY,
    };

    /// A main CPU: 16 MiB of coverage, a 24-bit mask, 4-byte trace entries,
    /// a history, everything checked.
    const MAIN: CpuDebugSpec = CpuDebugSpec {
        coverage_size: 0x0100_0000,
        address_mask: 0x00FF_FFFF,
        trace_entry_size: 4,
        history: true,
        checks: BreakpointChecks::ALL,
    };

    /// A sound CPU: 64 KiB of coverage, 2-byte trace entries, no history,
    /// execute breakpoints only.
    const SOUND: CpuDebugSpec = CpuDebugSpec {
        coverage_size: 0x1_0000,
        address_mask: 0xFFFF,
        trace_entry_size: 2,
        history: false,
        checks: BreakpointChecks {
            execute: true,
            memory: false,
            conditions: false,
        },
    };

    fn system(cpus: usize) -> SystemInfo {
        SystemInfo {
            name: "Test".to_string(),
            short_name: "test".to_string(),
            manufacturer: "Nobody".to_string(),
            year: 1990,
            cpus: (0..cpus)
                .map(|id| CpuInfo {
                    id,
                    name: format!("CPU {id}"),
                    architecture: CpuArchitecture::M68000,
                    clock_speed: 1,
                    registers: Vec::new(),
                    location: CpuLocation::Motherboard,
                    address_width: 24,
                })
                .collect(),
            display: DisplayInfo {
                width: 1,
                height: 1,
                aspect_ratio: 1.0,
                refresh_rate: 60.0,
                pixel_format: PixelFormat::RGBA8888,
            },
            audio: AudioInfo {
                sample_rate: 1,
                channels: 1,
                bits_per_sample: 16,
            },
            input: InputInfo {
                max_players: 1,
                button_layout: Vec::new(),
            },
        }
    }

    /// Two CPUs and a third slot with no CPU in it.
    fn state() -> DebuggerState<3, ()> {
        DebuggerState::new([MAIN, SOUND, CpuDebugSpec::NO_CPU], system(2), ())
    }

    /// A 4-byte trace entry.
    #[derive(Debug, Clone, Copy, PartialEq)]
    struct Entry(u32);

    impl TraceEntry for Entry {
        const SIZE: usize = 4;
        type Bytes = [u8; 4];

        fn to_bytes(&self) -> [u8; 4] {
            self.0.to_le_bytes()
        }

        fn from_bytes(bytes: &[u8]) -> Option<Self> {
            Some(Self(u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?)))
        }
    }

    fn execute_at(address: u32, cpu: usize) -> Breakpoint {
        Breakpoint {
            address,
            bp_type: BreakpointType::execute(),
            cpu_id: Some(cpu),
            condition: None,
        }
    }

    fn frame(call: u32) -> StackFrame {
        StackFrame {
            call_address: call,
            return_address: call + 4,
            function_name: None,
        }
    }

    fn instruction(address: u32) -> InstructionTrace {
        InstructionTrace {
            address,
            instruction: format!("at {address}"),
            registers_before: Vec::new(),
            cycle_count: 4,
        }
    }

    fn hits(state: &DebuggerState<3, ()>, id: BreakpointId) -> u32 {
        state
            .list_breakpoints()
            .into_iter()
            .find(|bp| bp.id == id)
            .unwrap()
            .hit_count
    }

    // -- The slots ------------------------------------------------------

    #[test]
    fn a_new_debugger_runs_with_the_first_cpu_focused() {
        let state = state();
        assert_eq!(state.get_execution_mode(), ExecutionMode::Running);
        assert_eq!(state.focused_cpu(), 0);
        assert!(state.get_cpu_states().is_empty());
        assert_eq!(state.cpu(0).map(CpuDebug::spec), Some(MAIN));
        assert_eq!(state.cpu(2).map(CpuDebug::spec), Some(CpuDebugSpec::NO_CPU));
        assert!(state.cpu(3).is_none());
    }

    #[test]
    fn the_focus_is_ignored_past_the_cpus_the_system_lists() {
        let mut state = state();
        state.set_focused_cpu(1);
        assert_eq!(state.focused_cpu(), 1);
        state.set_focused_cpu(2);
        assert_eq!(state.focused_cpu(), 1, "the system lists two CPUs");
        state.set_system_info(system(3));
        state.set_focused_cpu(2);
        assert_eq!(state.focused_cpu(), 2);
        state.set_focused_cpu(0);
        assert_eq!(state.focused_cpu(), 0);
    }

    #[test]
    fn the_cpu_states_and_infos_are_the_ones_the_core_set() {
        let mut state = state();
        state.set_cpu_states(vec![CpuState {
            cpu_id: 0,
            registers: Vec::new(),
            pc: 0x400,
            flags: 0,
        }]);
        assert_eq!(state.get_cpu_states()[0].pc, 0x400);
        assert_eq!(state.get_cpu_info(1).map(|info| info.id), Some(1));
        assert!(state.get_cpu_info(2).is_none());
    }

    #[test]
    fn each_cpu_records_coverage_masked_to_its_space() {
        let mut state = state();
        state.record_coverage(0, 0xFF00_0500);
        state.record_coverage(1, 0x1_0040);
        assert!(state.is_covered(0, 0x500));
        assert!(state.is_covered(0, 0xFF00_0500), "a read is masked too");
        assert!(!state.is_covered(0, 0x502));
        assert!(state.is_covered(1, 0x40));
        assert!(!state.is_covered(0, 0x40), "each CPU its own");
        assert_eq!(state.coverage_stats(0), (0x0100_0000, 1));
        assert_eq!(state.coverage_stats(1), (0x1_0000, 1));
        assert!(state.coverage(1).unwrap().get(0x40));
    }

    #[test]
    fn a_cpu_with_no_coverage_records_and_reports_none() {
        let mut state = state();
        state.record_coverage(2, 0x10);
        state.record_coverage(7, 0x10);
        assert!(!state.is_covered(2, 0x10));
        assert!(!state.is_covered(7, 0x10));
        assert_eq!(state.coverage_stats(2), (0, 0));
        assert_eq!(state.coverage_stats(7), (0, 0));
        assert!(state.coverage(7).is_none());
    }

    #[test]
    fn coverage_is_cleared_for_one_cpu_or_for_all() {
        let mut state = state();
        state.record_coverage(0, 0x10);
        state.record_coverage(1, 0x10);
        state.clear_coverage(1);
        assert!(state.is_covered(0, 0x10) && !state.is_covered(1, 0x10));
        state.record_coverage(1, 0x10);
        state.clear_all_coverage();
        assert_eq!(state.coverage_stats(0).1 + state.coverage_stats(1).1, 0);
        state.clear_coverage(9);
    }

    #[test]
    fn configuring_a_cpu_makes_its_slot_anew_unless_its_spec_is_unchanged() {
        let mut state = state();
        state.record_coverage(1, 0x10);
        state.set_tracing_enabled(1, true);
        state.configure_cpu(1, SOUND);
        assert!(state.is_covered(1, 0x10), "the same spec keeps the slot");
        assert!(state.is_tracing_enabled(1));

        let other = CpuDebugSpec {
            trace_entry_size: 6,
            ..SOUND
        };
        state.configure_cpu(1, other);
        assert!(!state.is_covered(1, 0x10));
        assert!(
            !state.is_tracing_enabled(1),
            "a new ring waits for a new enable"
        );
        assert_eq!(state.trace_entry_size(1), Some(6));

        assert_eq!(state.traceable_cpu_count(), 2);
        state.configure_cpu(2, SOUND);
        assert_eq!(state.traceable_cpu_count(), 3);
        state.configure_cpu(2, CpuDebugSpec::NO_CPU);
        assert_eq!(state.traceable_cpu_count(), 2);
        state.configure_cpu(9, MAIN);
    }

    #[test]
    fn breakpoint_checks_are_each_cpus_own() {
        let state = state();
        assert_eq!(state.breakpoint_checks(0), BreakpointChecks::ALL);
        assert_eq!(state.breakpoint_checks(1), SOUND.checks);
        assert_eq!(state.breakpoint_checks(2), BreakpointChecks::NONE);
        assert_eq!(state.breakpoint_checks(9), BreakpointChecks::NONE);
    }

    // -- Traces ---------------------------------------------------------

    #[test]
    fn a_trace_ring_is_made_the_first_time_its_tracing_is_enabled() {
        let mut state = state();
        assert_eq!(state.trace_stats(0), Some(TraceStats::default()));
        assert_eq!(state.trace_stats(0).unwrap().buffer_capacity, 0);
        assert!(!state.is_tracing_enabled(0));
        state.set_tracing_enabled(0, false);
        assert_eq!(
            state.trace_stats(0).unwrap().buffer_capacity,
            0,
            "off makes none"
        );

        state.set_tracing_enabled(0, true);
        let stats = state.trace_stats(0).unwrap();
        assert!(stats.enabled);
        assert_eq!(stats.buffer_capacity, TRACE_RING_START_MIB * 1024 * 1024);
        assert!(state.is_tracing_enabled(0) && state.tracing(0));
        assert!(!state.tracing(1));

        state.set_tracing_enabled(0, false);
        let stats = state.trace_stats(0).unwrap();
        assert!(!stats.enabled);
        assert_eq!(stats.buffer_capacity, TRACE_RING_START_MIB * 1024 * 1024);
    }

    #[test]
    fn a_cpu_with_no_trace_ignores_its_switch() {
        let mut state = state();
        state.set_tracing_enabled(2, true);
        state.set_tracing_enabled(9, true);
        assert!(!state.is_tracing_enabled(2));
        assert!(!state.tracing(9));
        assert_eq!(state.trace_stats(2), None);
        assert_eq!(state.trace_entry_size(2), None);
        assert_eq!(state.trace_entry_size(9), None);
        assert_eq!(state.trace_entry_size(0), Some(4));
        assert_eq!(state.get_trace_flush_data(2), None);
        assert_eq!(state.execution_tier(), ExecutionTier::Play);
    }

    #[test]
    fn traced_entries_reach_their_cpu_ring_while_it_is_traced() {
        let mut state = state();
        assert!(!state.record_trace(0, &Entry(1)), "no ring yet");
        state.set_tracing_enabled(0, true);
        state.record_trace(0, &Entry(0x0403_0201));
        state.record_trace(0, &Entry(0x0807_0605));
        assert!(!state.record_trace(9, &Entry(1)));
        assert_eq!(
            state.get_trace_flush_data(0),
            Some(vec![1, 2, 3, 4, 5, 6, 7, 8])
        );
        assert_eq!(state.trace_stats(0).unwrap().total_entries, 2);
        state.clear_trace_flushed(0);
        assert_eq!(state.get_trace_flush_data(0), None);
        state.set_tracing_enabled(0, false);
        state.record_trace(0, &Entry(9));
        assert_eq!(state.get_trace_flush_data(0), None, "off records nothing");
    }

    #[test]
    fn entries_in_bytes_are_recorded_whole() {
        let mut state = state();
        assert!(!state.record_trace_bytes(1, &[1, 2]));
        state.set_tracing_enabled(1, true);
        state.record_trace_bytes(1, &[1, 2, 3, 4, 5]);
        assert_eq!(
            state.get_trace_flush_data(1),
            Some(vec![1, 2, 3, 4]),
            "a trailing part of an entry is left out"
        );
        assert_eq!(state.trace_stats(1).unwrap().total_entries, 2);
        assert!(!state.record_trace_bytes(2, &[1, 2]));
    }

    #[test]
    fn the_flush_flags_read_one_ring_or_any() {
        let mut state = state();
        assert!(!state.any_trace_needs_flush());
        state.set_tracing_enabled(1, true);
        // Fill the sound CPU's ring past its first size, so it asks to be
        // flushed.
        let chunk = vec![0u8; 1024 * 1024];
        for _ in 0..TRACE_RING_START_MIB + 1 {
            state.record_trace_bytes(1, &chunk);
        }
        assert!(state.needs_trace_flush(1));
        assert!(!state.needs_trace_flush(0));
        assert!(!state.needs_trace_flush(9));
        assert!(state.any_trace_needs_flush());
        state.clear_trace_flushed(1);
        assert!(!state.any_trace_needs_flush());
    }

    #[test]
    fn the_tier_follows_the_mode_the_breakpoints_and_the_traces() {
        let mut state = state();
        assert_eq!(state.execution_tier(), ExecutionTier::Play);
        state.set_tracing_enabled(1, true);
        assert_eq!(state.execution_tier(), ExecutionTier::LightDebug);
        state.set_tracing_enabled(1, false);
        let id = state.add_breakpoint(execute_at(0x10, 0));
        assert_eq!(state.execution_tier(), ExecutionTier::LightDebug);
        state.remove_breakpoint(id);
        state.set_execution_mode(ExecutionMode::Paused);
        assert_eq!(state.execution_tier(), ExecutionTier::FullDebug);
        assert_eq!(state.running_tier(), ExecutionTier::Play);
    }

    // -- History and calls ---------------------------------------------

    #[test]
    fn a_history_ring_keeps_the_newest_and_gives_them_newest_first() {
        let mut ring = HistoryRing::new(3);
        assert!(ring.is_empty());
        for value in 1..=5 {
            ring.push(value);
        }
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.newest_first(10), [5, 4, 3]);
        assert_eq!(ring.newest_first(2), [5, 4]);
        assert!(ring.newest_first(0).is_empty());
        ring.clear();
        assert!(ring.is_empty());

        let mut none = HistoryRing::new(0);
        none.push(1);
        assert!(none.is_empty());
    }

    #[test]
    fn a_call_stack_keeps_its_depth_and_lists_the_outermost_first() {
        let mut stack = CallStack::new(2);
        stack.push(frame(0x10));
        stack.push(frame(0x20));
        stack.push(frame(0x30));
        let calls: Vec<u32> = stack.frames().iter().map(|f| f.call_address).collect();
        assert_eq!(calls, [0x10, 0x20], "a call past the depth is not kept");
        assert_eq!(stack.pop().map(|f| f.call_address), Some(0x20));
        stack.clear();
        assert!(stack.pop().is_none());
    }

    #[test]
    fn the_history_and_calls_hold_their_limits() {
        let mut state = state();
        for address in 0..(HISTORY_CAPACITY as u32 + 5) {
            state.record_history(0, instruction(address));
        }
        let history = state.get_instruction_history(0, u32::MAX);
        assert_eq!(history.len(), HISTORY_CAPACITY);
        assert_eq!(
            history[0].address,
            HISTORY_CAPACITY as u32 + 4,
            "newest first"
        );
        assert_eq!(history[HISTORY_CAPACITY - 1].address, 5);
        assert_eq!(state.get_instruction_history(0, 2).len(), 2);

        for call in 0..(CALL_STACK_DEPTH as u32 + 3) {
            state.push_call(0, frame(call));
        }
        let calls = state.get_call_stack(0);
        assert_eq!(calls.len(), CALL_STACK_DEPTH);
        assert_eq!(calls[0].call_address, 0, "the outermost first");
        state.pop_call(0);
        assert_eq!(state.get_call_stack(0).len(), CALL_STACK_DEPTH - 1);
    }

    #[test]
    fn a_cpu_with_no_history_keeps_neither_history_nor_calls() {
        let mut state = state();
        state.record_history(1, instruction(1));
        state.push_call(1, frame(1));
        state.record_history(9, instruction(1));
        state.push_call(9, frame(1));
        state.pop_call(9);
        assert!(state.get_instruction_history(1, 8).is_empty());
        assert!(state.get_call_stack(1).is_empty());
        assert!(state.get_instruction_history(9, 8).is_empty());
        assert!(state.get_call_stack(9).is_empty());
    }

    // -- Running on from a stop ----------------------------------------

    #[test]
    fn a_hit_marks_its_address_and_the_next_check_there_passes_once() {
        let mut state = state();
        let id = state.add_breakpoint(execute_at(0x10, 0));
        assert!(state.check_execute(0, 0x10, None), "the stop");
        assert!(
            !state.check_execute(0, 0x10, None),
            "running on: the instruction runs"
        );
        assert!(
            state.check_execute(0, 0x10, None),
            "the next time round stops again"
        );
        assert_eq!(hits(&state, id), 2, "the skipped check counts no hit");
    }

    #[test]
    fn a_check_at_another_address_forgets_the_mark() {
        let mut state = state();
        state.add_breakpoint(execute_at(0x10, 0));
        assert!(state.check_execute(0, 0x10, None));
        assert!(!state.check_execute(0, 0x14, None));
        assert!(state.check_execute(0, 0x10, None), "the mark is gone");
    }

    #[test]
    fn a_check_with_no_hit_marks_nothing() {
        let mut state = state();
        let id = state.add_breakpoint(execute_at(0x10, 0));
        assert!(!state.check_execute(0, 0x20, None));
        assert!(state.check_execute(0, 0x10, None));
        assert_eq!(hits(&state, id), 1);
    }

    #[test]
    fn checking_the_next_address_marks_it_whether_or_not_it_hits() {
        let mut state = state();
        let id = state.add_breakpoint(execute_at(0x10, 0));
        assert!(
            state.check_execute_next(0, 0x10, None),
            "the stop, after an instruction"
        );
        assert!(
            !state.check_execute(0, 0x10, None),
            "the instruction at the stop runs"
        );
        assert_eq!(hits(&state, id), 1);

        assert!(!state.check_execute_next(0, 0x20, None));
        assert!(
            !state.check_execute(0, 0x20, None),
            "one check an instruction: the address was checked after the one before"
        );
        assert!(!state.check_execute_next(0, 0x20, None));
        state.forget_checked(0);
        assert!(!state.check_execute(0, 0x20, None));
        assert_eq!(hits(&state, id), 1);
    }

    #[test]
    fn forgetting_the_mark_makes_the_next_check_check() {
        let mut state = state();
        let id = state.add_breakpoint(execute_at(0x10, 0));
        assert!(state.check_execute(0, 0x10, None));
        state.forget_checked(0);
        assert!(state.check_execute(0, 0x10, None));
        assert_eq!(hits(&state, id), 2);
        state.forget_checked(9);
    }

    #[test]
    fn each_cpu_keeps_its_own_mark() {
        let mut state = state();
        state.add_breakpoint(execute_at(0x10, 0));
        state.add_breakpoint(execute_at(0x10, 1));
        assert!(state.check_execute(0, 0x10, None));
        assert!(state.check_execute(1, 0x10, None), "the other CPU checks");
        assert!(!state.check_execute(0, 0x10, None));
        assert!(!state.check_execute(1, 0x10, None));
        state.add_breakpoint(execute_at(0x10, 9));
        assert!(
            state.check_execute(9, 0x10, None),
            "a CPU past the slots, unmarked"
        );
        assert!(state.check_execute(9, 0x10, None));
    }

    #[test]
    fn a_replaced_machine_empties_the_calls_forgets_the_marks_and_keeps_the_history() {
        let mut state = state();
        state.add_breakpoint(execute_at(0x10, 0));
        state.push_call(0, frame(0x100));
        state.record_history(0, instruction(0x100));
        assert!(state.check_execute(0, 0x10, None));
        state.machine_replaced();
        assert!(state.get_call_stack(0).is_empty());
        assert_eq!(state.get_instruction_history(0, 8).len(), 1);
        assert!(
            state.check_execute(0, 0x10, None),
            "after a load, the instruction at the address is checked"
        );
    }

    #[test]
    fn conditions_and_watches_go_through_the_context() {
        let mut state = state();
        let mut spec_bp = execute_at(0x10, 0);
        spec_bp.bp_type = BreakpointType::write();
        let watch = state.add_breakpoint(spec_bp);
        let mut ctx = CpuExprContext::new(|_| None, |_| 0, ByteOrder::BigEndian);
        let write = Access {
            address: 0x10,
            write: true,
            value: 1,
        };
        assert!(state.check_accesses(0, &[write], &mut ctx));
        assert_eq!(hits(&state, watch), 1);

        let conditional = state.add_breakpoint(execute_at(0x20, 0));
        state.set_breakpoint_match_spec(
            conditional,
            MatchSpec {
                expr: Some("a == 7".into()),
                ..MatchSpec::default()
            },
        );
        let mut seven = CpuExprContext::new(
            |name| (name == "a").then_some(7),
            |_| 0,
            ByteOrder::BigEndian,
        );
        assert!(!state.check_execute(0, 0x20, None), "no context: closed");
        assert!(state.check_execute(0, 0x20, Some(&mut seven)));
    }

    // -- Events and the console -----------------------------------------

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    enum Event {
        VerticalBlank,
        Colour(u8),
    }

    impl DebugEvent for Event {
        fn code_param(self) -> (u32, u32) {
            match self {
                Self::VerticalBlank => (1, 0),
                Self::Colour(colour) => (6, u32::from(colour)),
            }
        }
    }

    fn event_at(code: u32, param: u32) -> Breakpoint {
        Breakpoint {
            address: 0,
            bp_type: BreakpointType::Event { code, param },
            cpu_id: None,
            condition: None,
        }
    }

    #[test]
    fn an_event_reaches_its_breakpoints() {
        let mut state = state();
        let id = state.add_breakpoint(event_at(6, 3));
        assert!(!state.check_event(Event::Colour(2)));
        assert!(state.check_event(Event::Colour(3)));
        assert!(!state.check_event(Event::VerticalBlank));
        assert_eq!(hits(&state, id), 1);
    }

    #[test]
    fn the_raised_events_are_all_checked_and_drained() {
        let mut state = state();
        let any = state.add_breakpoint(event_at(6, BREAKPOINT_PARAM_ANY));
        let blank = state.add_breakpoint(event_at(1, BREAKPOINT_PARAM_ANY));
        let mut raised = RaisedEvents::new(16);
        raised.raise(Event::Colour(1));
        raised.raise(Event::VerticalBlank);
        raised.raise(Event::Colour(2));
        assert!(state.check_raised(&mut raised));
        assert!(raised.is_empty());
        assert_eq!(
            hits(&state, any),
            2,
            "the check does not stop at the first hit"
        );
        assert_eq!(hits(&state, blank), 1);
        assert!(!state.check_raised(&mut raised), "nothing left");
    }

    #[test]
    fn the_watched_params_and_the_events_come_from_the_list() {
        let mut state = state();
        state.add_breakpoint(event_at(6, 5));
        assert!(state.has_watchpoints_or_events());
        let watched = state.breakpoints().watched_params::<1>(6).unwrap();
        assert!(watched.contains(5));
        state.breakpoints_mut().add(event_at(6, 7));
        assert_eq!(state.breakpoints().enabled_events().count(), 2);
    }

    /// A console with two events and a region, and a counter of its own.
    struct Console {
        lines: u32,
    }

    impl ConsoleDebug for Console {
        const EVENTS: &'static [EventSpec] = &[
            EventSpec {
                code: 1,
                kind: EventKind::VerticalBlank,
                label: "VBlank",
                description: "The beam entered vertical blank",
                param: EventParam::None,
            },
            EventSpec {
                code: 4,
                kind: EventKind::Line,
                label: "Scanline",
                description: "A line was reached",
                param: EventParam::Number { label: "Line" },
            },
        ];

        fn memory_regions(&self) -> Vec<MemoryRegionInfo> {
            vec![MemoryRegionInfo {
                region: MemoryRegion::MainRam,
                name: format!("RAM after {} lines", self.lines),
                start_address: 0,
                size: 0x2000,
                writable: true,
            }]
        }
    }

    #[test]
    fn the_catalogue_and_the_regions_are_the_console_s() {
        let mut state = DebuggerState::new([MAIN], system(1), Console { lines: 0 });
        let catalogue = state.available_events();
        assert_eq!(
            catalogue
                .iter()
                .map(|class| (class.code, class.kind))
                .collect::<Vec<_>>(),
            [(1, EventKind::VerticalBlank), (4, EventKind::Line)]
        );
        assert_eq!(catalogue[1].param_label.as_deref(), Some("Line"));
        state.console_mut().lines = 3;
        assert_eq!(state.console().lines, 3);
        assert_eq!(state.get_memory_regions()[0].name, "RAM after 3 lines");
        assert_eq!(check_console(&state), Ok(()));

        let bare = DebuggerState::new([MAIN], system(1), ());
        assert!(bare.available_events().is_empty());
        assert!(bare.get_memory_regions().is_empty());
    }

    /// A console's catalogue passes the catalogue check.
    fn check_console<const N: usize, C: ConsoleDebug>(
        _state: &DebuggerState<N, C>,
    ) -> Result<(), String> {
        crate::debug::events::check_catalogue(C::EVENTS)
    }

    #[test]
    fn the_debugger_is_read_through_the_trait_object() {
        let mut state = state();
        let debugger: &mut dyn DebuggerCore = &mut state;
        debugger.set_tracing_enabled(0, true);
        assert!(debugger.is_tracing_enabled(0));
        assert_eq!(debugger.traceable_cpu_count(), 2);
        let id = debugger.add_breakpoint(execute_at(0x10, 0));
        debugger.enable_breakpoint(id, false);
        debugger.set_breakpoint_access(id, AccessFlags::WRITE);
        assert_eq!(debugger.list_breakpoints().len(), 1);
        assert!(!debugger.list_breakpoints()[0].enabled);
    }
}
