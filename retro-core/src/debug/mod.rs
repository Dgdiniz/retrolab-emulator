//! The debugger every console core shares.
//!
//! [`DebuggerState`] is a core's whole debugger: the breakpoints, the
//! execution mode, the focused CPU, and for each CPU its coverage, trace,
//! instruction history and call stack; it is the one implementation of
//! [`crate::DebuggerCore`]. A core describes its CPUs and its console to it
//! and keeps only what its hardware makes different: its CPUs' decoders,
//! trace entries and register snapshots, its events' codes, its memory map,
//! and where its frame loops record and check.
//!
//! - [`state`]: [`DebuggerState`] and what it keeps of each CPU.
//! - [`breakpoints`]: [`BreakpointList`] and its checks.
//! - [`access`]: the bytes an instruction accessed, for the watchpoints.
//! - [`events`]: the event catalogue, the events a chip raises, the params
//!   a breakpoint watches.
//! - [`expr`]: the condition language and a CPU's context for it.
//! - [`trace`]: the trace ring, a trace entry and its text.
//! - [`coverage`], [`disasm_window`], [`execution_tier`]: what the
//!   disassembly view and the frame-loop choice read.

pub mod access;
pub mod breakpoints;
pub mod coverage;
pub mod disasm_window;
pub mod events;
pub mod execution_tier;
pub mod expr;
pub mod state;
pub mod trace;

pub use access::{Access, AccessLog};
pub use breakpoints::BreakpointList;
pub use coverage::CoverageBitmap;
pub use disasm_window::{disassemble_window, DisasmDriver};
pub use events::{
    check_catalogue, event_catalog, DebugEvent, EventParam, EventSpec, ParamSet, RaisedEvents,
};
pub use execution_tier::{select_tier, ExecutionTier, TierSelectorInputs};
pub use expr::{eval_str, ByteOrder, CpuExprContext, Expr, ExprContext, ExprError};
pub use state::{
    BreakpointChecks, CallStack, ConsoleDebug, CpuDebug, CpuDebugSpec, DebuggerState, HistoryRing,
    CALL_STACK_DEPTH, HISTORY_CAPACITY,
};
pub use trace::{
    trace_to_text, TraceEntry, TraceStats, TraceText, TracingState, MAX_TRACE_BUFFER_SIZE,
};
