//! The tiers: the play tier, and the debugger's work around a step,
//! written once over a core's [`DebugMachine`].

use crate::debug::MAIN_CPU;
use crate::types::StackFrame;

use super::adapter::{DebugMachine, FrameDebugger, InstructionStart};
use super::hooks::{Flow, StepHooks, Tier};

/// What a player runs. Its one piece of work is the main CPU's coverage,
/// so the debugger's coverage view fills while a game plays; every other
/// hook is empty, and a check handed to [`StepHooks::raised`] never runs.
#[derive(Clone, Copy, Debug, Default)]
pub struct Play;

impl<M: DebugMachine> StepHooks<M> for Play {
    type Start = ();

    #[inline(always)]
    fn before_instruction(&mut self, m: &mut M, pc: u32) -> Flow<()> {
        m.debug_state_mut().record_coverage(MAIN_CPU, pc);
        Flow::Run(())
    }
}

impl<M: DebugMachine> Tier<M> for Play {}

/// The debugger's tier while it runs: execute breakpoints, watchpoints,
/// events, coverage, traces and the call stack.
#[derive(Clone, Copy, Debug, Default)]
pub struct LightDebug;

impl<M: DebugMachine> StepHooks<M> for LightDebug {
    type Start = InstructionStart<M>;

    fn before_instruction(&mut self, m: &mut M, pc: u32) -> Flow<Self::Start> {
        if m.check_execute(pc) {
            return Flow::StopBefore;
        }
        Flow::Run(start_instruction(m, pc))
    }

    fn after_instruction(&mut self, m: &mut M, start: Self::Start, cycles: u32) {
        finish_instruction(m, start, cycles);
    }

    fn interrupt_taken(&mut self, m: &mut M, interrupted_pc: u32) {
        push_interrupt_frame(m, interrupted_pc);
    }

    fn raised<F: FnOnce(&mut M) -> bool>(&mut self, m: &mut M, check: F) {
        if m.armed() && check(m) {
            m.debug_state_mut().request_stop();
        }
    }
}

impl<M: DebugMachine> Tier<M> for LightDebug {
    /// A frame call clears a stop asked for before it, unless the frame is
    /// held at its capture boundary: an event in the batch that reached
    /// the boundary waits for the rest of that batch, which the call
    /// handles first.
    fn enter(&mut self, m: &mut M) {
        if !m.held_at_capture_boundary() {
            m.debug_state_mut().clear_stop();
        }
    }

    fn stop_requested(&self, m: &M) -> bool {
        m.armed() && m.debug_state().stop_requested()
    }

    fn held(&mut self, m: &mut M) {
        m.flush_traces();
    }

    fn stopped(&mut self, m: &mut M) {
        m.pause();
    }

    fn leave(&mut self, m: &mut M) {
        m.flush_traces();
    }
}

/// The step a host asks for: no execute check (the step runs whatever
/// breakpoint sits at its instruction), and the history entry besides the
/// light tier's work. What its checks ask for is counted, and the host
/// step drops the request to stop: a host step always pauses.
#[derive(Clone, Copy, Debug, Default)]
pub struct FullDebug;

impl<M: DebugMachine> StepHooks<M> for FullDebug {
    type Start = (InstructionStart<M>, M::HistoryStart);

    fn before_instruction(&mut self, m: &mut M, pc: u32) -> Flow<Self::Start> {
        let start = start_instruction(m, pc);
        Flow::Run((start, m.history_start(pc)))
    }

    fn after_instruction(&mut self, m: &mut M, (start, history): Self::Start, cycles: u32) {
        finish_instruction(m, start, cycles);
        m.history_finish(history, cycles);
    }

    fn interrupt_taken(&mut self, m: &mut M, interrupted_pc: u32) {
        push_interrupt_frame(m, interrupted_pc);
    }

    fn raised<F: FnOnce(&mut M) -> bool>(&mut self, m: &mut M, check: F) {
        if m.armed() && check(m) {
            m.debug_state_mut().request_stop();
        }
    }
}

/// Before an instruction the debug tiers run: coverage, the trace entry
/// when the main CPU is traced, the access log and the call classifier's
/// start.
fn start_instruction<M: DebugMachine>(m: &mut M, pc: u32) -> InstructionStart<M> {
    m.debug_state_mut().record_coverage(MAIN_CPU, pc);
    let trace = if m.debug_state().tracing(MAIN_CPU) {
        Some(m.trace_start(pc))
    } else {
        None
    };
    let access_log = m.arm_access_log();
    let call = m.call_start(pc);
    InstructionStart {
        pc,
        access_log,
        call,
        trace,
    }
}

/// After it: the call stack, the trace entry, and the instruction's
/// accesses against the watchpoints.
fn finish_instruction<M: DebugMachine>(m: &mut M, start: InstructionStart<M>, cycles: u32) {
    m.call_finish(start.call);
    if let Some(trace) = start.trace {
        m.trace_finish(trace, cycles);
    }
    if start.access_log && m.check_access_log(start.pc) {
        m.debug_state_mut().request_stop();
    }
}

/// An interrupt's call frame: the interrupted address as both the call
/// and the return, which the handler's return pops.
fn push_interrupt_frame<M: DebugMachine>(m: &mut M, interrupted_pc: u32) {
    m.debug_state_mut().push_call(
        MAIN_CPU,
        StackFrame {
            call_address: interrupted_pc,
            return_address: interrupted_pc,
            function_name: None,
        },
    );
}
