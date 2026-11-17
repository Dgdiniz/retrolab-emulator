//! What the debugger's tiers need of a core: its debugger, and the
//! per-CPU work each console does its own way (the execute check's
//! register context, the call classifier, the trace and history entries,
//! the access log, arming the recorders).

use crate::debug::{ConsoleDebug, DebuggerState};
use crate::debugger::DebuggerCore;
use crate::emulator::EmulatorCore;
use crate::types::{ExecutionMode, StackFrame};

use super::machine::FrameMachine;

/// The debugger as the frame's tiers use it. It is implemented once, for
/// every [`DebuggerState`]: the state's CPU count is a const generic,
/// which a core's adapter can only name through a trait.
pub trait FrameDebugger: DebuggerCore {
    /// Record that CPU `cpu` ran an instruction at `address`.
    fn record_coverage(&mut self, cpu: usize, address: u32);
    /// Whether CPU `cpu` is being traced.
    fn tracing(&self, cpu: usize) -> bool;
    /// Forget CPU `cpu`'s execute mark: its next execute check checks.
    fn forget_checked(&mut self, cpu: usize);
    /// CPU `cpu` made a call (or took an interrupt): keep `frame` as its
    /// innermost.
    fn push_call(&mut self, cpu: usize, frame: StackFrame);
    /// A check asks the frame call to stop.
    fn request_stop(&mut self);
    /// Whether a check asked the frame call to stop.
    fn stop_requested(&self) -> bool;
    /// Forget a request to stop.
    fn clear_stop(&mut self);
}

impl<const N: usize, C: ConsoleDebug> FrameDebugger for DebuggerState<N, C> {
    #[inline(always)]
    fn record_coverage(&mut self, cpu: usize, address: u32) {
        DebuggerState::record_coverage(self, cpu, address);
    }

    #[inline(always)]
    fn tracing(&self, cpu: usize) -> bool {
        DebuggerState::tracing(self, cpu)
    }

    #[inline(always)]
    fn forget_checked(&mut self, cpu: usize) {
        DebuggerState::forget_checked(self, cpu);
    }

    #[inline(always)]
    fn push_call(&mut self, cpu: usize, frame: StackFrame) {
        DebuggerState::push_call(self, cpu, frame);
    }

    #[inline(always)]
    fn request_stop(&mut self) {
        DebuggerState::request_stop(self);
    }

    #[inline(always)]
    fn stop_requested(&self) -> bool {
        DebuggerState::stop_requested(self)
    }

    #[inline(always)]
    fn clear_stop(&mut self) {
        DebuggerState::clear_stop(self);
    }
}

/// The per-CPU adapter a core supplies for the debugger's tiers. Each
/// method is the console's own way of doing one piece of the debugger's
/// work around its main CPU's step; the order the pieces run in, and the
/// stops they lead to, are the tiers' ([`super::LightDebug`],
/// [`super::FullDebug`]).
pub trait DebugMachine: FrameMachine + EmulatorCore {
    /// The core's debugger.
    type Debugger: FrameDebugger;

    /// The debugger, to read.
    fn debug_state(&self) -> &Self::Debugger;

    /// The debugger, to change.
    fn debug_state_mut(&mut self) -> &mut Self::Debugger;

    /// Whether the event checks and the access log are armed: some enabled
    /// breakpoint watches memory or an event.
    fn armed(&self) -> bool;

    /// Arm what the debugger's state asks for: the trace capture of the
    /// CPUs no frame loop steps, the access log and the event watches.
    fn arm(&mut self);

    /// Disarm all of it, moving what the traces hold to the debugger
    /// first, so a frame run now leaves nothing for a later check to find.
    fn disarm(&mut self);

    /// The execute breakpoints at `pc`, a condition read in the CPU's own
    /// register context; `true` stops before the instruction.
    fn check_execute(&mut self, pc: u32) -> bool;

    /// What the call classifier reads before an instruction.
    type CallStart;
    /// Read it, for the instruction at `pc`.
    fn call_start(&self, pc: u32) -> Self::CallStart;
    /// Push a call or pop a return the instruction made.
    fn call_finish(&mut self, start: Self::CallStart);

    /// What a trace entry keeps from before an instruction to after it:
    /// `()` for a CPU whose entry is whole before the instruction, which
    /// [`Self::trace_start`] then records there.
    type TraceStart;
    /// Before the instruction at `pc`: read its entry, or record it when it
    /// is whole already.
    fn trace_start(&mut self, pc: u32) -> Self::TraceStart;
    /// Finish and record an entry [`Self::trace_start`] left open, `cycles`
    /// as [`super::StepHooks::after_instruction`] passes them.
    fn trace_finish(&mut self, start: Self::TraceStart, cycles: u32);

    /// What a history entry reads before an instruction.
    type HistoryStart;
    /// Read it, for the instruction at `pc`.
    fn history_start(&self, pc: u32) -> Self::HistoryStart;
    /// Record the entry.
    fn history_finish(&mut self, start: Self::HistoryStart, cycles: u32);

    /// Start the instruction's access log, dropping what was recorded
    /// between two instructions; returns whether it is armed.
    fn arm_access_log(&mut self) -> bool;

    /// Check the accesses the instruction at `pc` made against the
    /// watchpoints, and the events its stores raised; `true` when a
    /// breakpoint matched.
    fn check_access_log(&mut self, pc: u32) -> bool;

    /// Move what the CPUs no frame loop steps traced into the debugger.
    fn flush_traces(&mut self) {}

    /// Pause where a frame call stopped: flush the traces, then the
    /// trait's pause, which writes the CPU states the debugger shows.
    fn pause(&mut self) {
        self.flush_traces();
        EmulatorCore::set_execution_mode(self, ExecutionMode::Paused);
    }
}

/// What the light tier keeps from before an instruction to after it; the
/// full tier keeps the history entry besides.
pub struct InstructionStart<M: DebugMachine> {
    pub(super) pc: u32,
    pub(super) access_log: bool,
    pub(super) call: M::CallStart,
    pub(super) trace: Option<M::TraceStart>,
}
