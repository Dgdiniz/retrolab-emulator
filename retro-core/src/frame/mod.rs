//! The frame driver: one loop that runs a console's frames, written once
//! for every core.
//!
//! A core keeps what its hardware makes different: its timetable (a queue
//! of timed events, a line count, a flag its video chip raises), what
//! each of those does when it falls due, and its machine step (the main
//! CPU and what its time does to the rest of the machine). It hands them
//! to the driver through [`FrameMachine`]. The driver owns the rest:
//!
//! - the loop: handle what is due, run steps to the next deadline, close
//!   the frame ([`run_frame`]);
//! - the frame loops a host picks between ([`run_frame_on`]): the play
//!   tier ([`Play`]), the debugger's tier while it runs ([`LightDebug`]),
//!   and its tier while it is paused or stepping ([`run_full`]);
//! - the step a host asks for ([`host_step`]), the headless frame
//!   ([`run_frame_headless`]), a held frame finished before a save
//!   ([`finish_held_frame`]), the capture boundary
//!   ([`run_to_capture_boundary`]) and the profiler
//!   ([`run_frame_profiled`]).
//!
//! The debugger's work around a step is written once too, in the tiers,
//! over the adapter each core supplies ([`DebugMachine`]): the execute
//! check, coverage, the trace entry, the history, the call stack and the
//! access log.
//!
//! # How a frame call stops
//!
//! - An execute breakpoint stops before its instruction runs.
//! - A check made during a step (an event the step raised, the access log
//!   against the watchpoints) stops the frame after that step.
//! - A check made by what fell due stops it before the next step.
//!
//! So no step is ever left half done by a stop, and a frame stopped
//! anywhere goes on from there on the next frame call and ends as one run
//! whole.
//!
//! # Why the play tier costs nothing
//!
//! Every hook takes its tier as a type parameter, so each tier's loop is a
//! function of its own. The play tier's hooks are empty
//! `#[inline(always)]` bodies but for the main CPU's coverage: a check a
//! handler hands to [`StepHooks::raised`] is never run, the stop tests
//! fold to `false`, and nothing is called through a pointer.

mod adapter;
#[cfg(feature = "frame-conformance")]
pub mod conformance;
mod driver;
mod hooks;
mod machine;
#[cfg(not(target_arch = "wasm32"))]
mod profile;
mod queue;
mod tiers;

pub use adapter::{DebugMachine, FrameDebugger, InstructionStart};
#[cfg(not(target_arch = "wasm32"))]
pub use driver::run_frame_profiled;
pub use driver::{
    finish_held_frame, host_step, run_frame, run_frame_headless, run_frame_on, run_full,
    run_to_capture_boundary, run_to_capture_boundary_on, FrameCall,
};
pub use hooks::{Flow, Part, StepHooks, Tier};
pub use machine::{Due, FrameMachine, Stepped};
#[cfg(not(target_arch = "wasm32"))]
pub use profile::{FrameProfile, Profiled};
pub use queue::{DueBatch, EventQueue};
pub use tiers::{FullDebug, LightDebug, Play};
