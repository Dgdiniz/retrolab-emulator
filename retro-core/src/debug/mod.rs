//! Debugger building blocks shared across console cores.
//!
//! Each console core (Genesis, SNES, ...) builds its own `DebuggerCore`
//! implementation on top of these primitives so the wasm bindings,
//! disassembly view, and tracing pipeline can be reused without
//! per-console reinvention.

pub mod breakpoints;
pub mod coverage;
pub mod disasm_window;
pub mod execution_tier;
pub mod expr;
pub mod trace;

pub use breakpoints::{BreakpointList, BusWatchpointHook};
pub use coverage::CoverageBitmap;
pub use disasm_window::{disassemble_window, DisasmDriver};
pub use execution_tier::{select_tier, ExecutionTier, TierSelectorInputs};
pub use expr::{eval_str, Expr, ExprContext, ExprError};
pub use trace::{TraceStats, TracingState, MAX_TRACE_BUFFER_SIZE};
