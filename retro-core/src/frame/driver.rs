//! The driver: the frame loop, and the calls a host makes around it.

use crate::debug::{ExecutionTier, MAIN_CPU};
use crate::debugger::DebuggerCore;
use crate::emulator::StepResult;
use crate::types::ExecutionMode;

use super::adapter::{DebugMachine, FrameDebugger};
use super::hooks::{Part, StepHooks, Tier};
use super::machine::{Due, FrameMachine};
use super::tiers::{FullDebug, LightDebug, Play};

/// How a frame call ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameCall {
    /// The frame ran to its end and is closed.
    Ended,
    /// The frame stopped in front of the capture boundary it was asked to.
    Held,
    /// The frame stopped part of the way through: a breakpoint, a
    /// watchpoint, an event, or the debugger paused or stepping.
    Stopped,
}

/// One frame call on `tier`: the frame in progress, or the next one, to
/// its end, a stop, or the capture boundary. Arms nothing: the caller has
/// armed the debugger as the tier needs.
///
/// Each pass handles what is due first, then runs steps to the next
/// deadline. A frame picked up where an earlier call stopped may have
/// something due from before the stop, so it is handled before the
/// frame's end is checked; the frame closes before a pause, so the next
/// call opens the next frame.
pub fn run_frame<M: FrameMachine, T: Tier<M>>(m: &mut M, tier: &mut T) -> FrameCall {
    tier.enter(m);
    let frame_end = m.enter_frame();
    loop {
        tier.part(m, Part::Due);
        let due = m.handle_due(frame_end, tier);
        if due == Due::Held {
            tier.held(m);
            return FrameCall::Held;
        }
        let stop = tier.stop_requested(m);
        if due == Due::FrameEnd {
            tier.part(m, Part::Close);
            m.close_frame();
            if stop {
                tier.stopped(m);
                return FrameCall::Stopped;
            }
            break;
        }
        if stop {
            m.present_stop();
            tier.stopped(m);
            return FrameCall::Stopped;
        }
        tier.part(m, Part::Run);
        let deadline = m.next_deadline(frame_end);
        let mut run = m.begin_run();
        match deadline {
            Some(target) => {
                while m.now() < target {
                    if m.machine_step(&mut run, tier).is_none() || tier.stop_requested(m) {
                        m.end_run(run);
                        m.present_stop();
                        tier.stopped(m);
                        return FrameCall::Stopped;
                    }
                }
            }
            None => {
                if m.machine_step(&mut run, tier).is_none() || tier.stop_requested(m) {
                    m.end_run(run);
                    m.present_stop();
                    tier.stopped(m);
                    return FrameCall::Stopped;
                }
            }
        }
        m.end_run(run);
    }
    tier.leave(m);
    FrameCall::Ended
}

/// The frame call a host makes: the loop `tier` names (`Play`: the play
/// tier, run with the debugger disarmed for its length; `LightDebug`: the
/// debugger's tier while it runs; `FullDebug`: [`run_full`]), the debugger
/// armed as its state asks for the two debug tiers.
pub fn run_frame_on<M: DebugMachine>(m: &mut M, tier: ExecutionTier) -> FrameCall {
    if tier != ExecutionTier::Play {
        m.arm();
    }
    run_tier(m, tier)
}

/// The loop `tier` names; the debug tiers armed already.
fn run_tier<M: DebugMachine>(m: &mut M, tier: ExecutionTier) -> FrameCall {
    match tier {
        ExecutionTier::Play => play_frame(m),
        ExecutionTier::LightDebug => run_frame(m, &mut LightDebug),
        ExecutionTier::FullDebug => run_full(m),
    }
}

/// A frame call on the play tier, the debugger disarmed for its length and
/// armed again after: the play tier checks nothing, so nothing may be left
/// armed to record what no check will read (a video chip's events raised
/// for a breakpoint the play tier never drains, which the next debug frame
/// would stop on).
fn play_frame<M: DebugMachine>(m: &mut M) -> FrameCall {
    m.disarm();
    let call = run_frame(m, &mut Play);
    m.arm();
    call
}

/// The debugger's tier while it is paused or stepping. Paused, it writes
/// the CPU states the debugger shows and runs nothing; in a step mode it
/// makes one host step, then pauses (the step wrote the states already).
/// Either way the host's picture shows the frame as far as it is drawn
/// ([`FrameMachine::present_stop`]).
pub fn run_full<M: DebugMachine>(m: &mut M) -> FrameCall {
    if m.debug_state().get_execution_mode() == ExecutionMode::Paused {
        m.refresh_debugger_state();
        m.present_stop();
        return FrameCall::Stopped;
    }
    step_on_full(m);
    m.debug_state_mut()
        .set_execution_mode(ExecutionMode::Paused);
    m.present_stop();
    FrameCall::Stopped
}

/// The step a host asks for on the main CPU, the one step of the frame
/// loop: what is due before its instruction is handled first, as a pass
/// of the loop handles it, then the step runs on the [`FullDebug`] tier,
/// then what it made due is handled as the loop's passes that run no step
/// handle it, until a deadline is ahead of the clock or the frame's end
/// is reached, and the frame closed if it is (the next frame opens with
/// the next step or frame call, as after a frame call). The instruction
/// runs whatever execute breakpoint sits at it, so the next execute check
/// is made afresh; what the step's checks find is counted, never acted on.
/// A host step never asks for the capture boundary, and a frame held there
/// is left first, as the loop leaves it.
pub fn host_step<M: DebugMachine>(m: &mut M) -> StepResult {
    m.arm();
    step_on_full(m)
}

/// [`host_step`], armed already.
fn step_on_full<M: DebugMachine>(m: &mut M) -> StepResult {
    let mut tier = FullDebug;
    let frame_end = m.enter_step(&mut tier);
    let frame_end = handle_due_before_step(m, &mut tier, frame_end);
    let mut run = m.begin_run();
    let stepped = m.machine_step(&mut run, &mut tier);
    m.end_run(run);
    m.debug_state_mut().forget_checked(MAIN_CPU);
    handle_due_after_step(m, &mut tier, frame_end);
    if m.armed() {
        m.debug_state_mut().clear_stop();
    }
    m.flush_traces();
    m.refresh_debugger_state();
    // The full tier never stops a step before its instruction.
    let (elapsed, instruction) = stepped.map_or((0, false), |s| (s.elapsed, s.instruction));
    StepResult {
        cycles: elapsed.min(u32::MAX as u64) as u32,
        breakpoint_hit: false,
        instruction_executed: instruction,
    }
}

/// Before a host step's instruction, what a pass of the frame loop handles
/// before it: everything due while the deadline is not ahead of the clock
/// (the events a frame's close armed at the present cycle, the ones an
/// earlier handler's time brought due), once for a core whose step keeps
/// its own timetable; a frame whose end that reaches is closed and the
/// next opened. Returns the frame's end, the new frame's when one opened.
fn handle_due_before_step<M: FrameMachine, H: StepHooks<M>>(
    m: &mut M,
    hooks: &mut H,
    mut frame_end: u64,
) -> u64 {
    let mut once = m.next_deadline(frame_end).is_none();
    while once || m.next_deadline(frame_end).is_some_and(|due| due <= m.now()) {
        once = false;
        match m.handle_due(frame_end, hooks) {
            Due::FrameEnd => {
                m.close_frame();
                frame_end = m.enter_frame();
            }
            Due::Held => break,
            Due::Continue => {}
        }
    }
    frame_end
}

/// After a host step's instruction, what it made due, as the loop's passes
/// that run no step handle it: until a deadline is ahead of the clock
/// (once, for a core whose step keeps its own timetable). A frame whose
/// end that reaches is closed and left so: the next step or frame call
/// opens the next one, as after a frame call that ran its frame to the
/// end, so the machine at a frame's end is the same either way. A line
/// whose time something the step began uses up as soon as it starts (a
/// transfer that stalls the CPU) is due at its start, so the frame's end
/// can need more than one pass.
fn handle_due_after_step<M: FrameMachine, H: StepHooks<M>>(
    m: &mut M,
    hooks: &mut H,
    frame_end: u64,
) {
    loop {
        match m.handle_due(frame_end, hooks) {
            Due::FrameEnd => {
                m.close_frame();
                break;
            }
            Due::Held => break,
            Due::Continue => {}
        }
        if !m.next_deadline(frame_end).is_some_and(|due| due <= m.now()) {
            break;
        }
    }
}

/// Finish a frame [`FrameMachine::frame_held`] reports, before a save: its
/// rest runs on the play tier with the debugger disarmed for its length,
/// so no breakpoint, watchpoint or event stops it and nothing of it is
/// traced or counted (only the main CPU's coverage, as the play tier
/// records it). The debugger is armed again after, and the CPU states it
/// shows are written where the frame ended. Returns whether there was a
/// frame to finish.
pub fn finish_held_frame<M: DebugMachine>(m: &mut M) -> bool {
    if !m.frame_held() {
        return false;
    }
    play_frame(m);
    m.refresh_debugger_state();
    true
}

/// A frame with the host's picture and sound left out, for a rollback's
/// re-simulation: the play tier, with the debugger disarmed for its
/// length so a re-simulated frame records, counts and traces nothing (a
/// rollback would otherwise trace its frames twice); only the main CPU's
/// coverage, as the play tier records it. The same machine as a presented
/// frame.
pub fn run_frame_headless<M: DebugMachine>(m: &mut M) {
    m.set_presenting(false);
    play_frame(m);
    m.set_presenting(true);
}

/// Run to the capture boundary on the loop the debugger's state selects
/// ([`run_to_capture_boundary_on`]).
pub fn run_to_capture_boundary<M: DebugMachine>(m: &mut M) {
    let tier = m.debug_state().execution_tier();
    run_to_capture_boundary_on(m, tier);
}

/// Run to the capture boundary on `tier`: the frame's picture drawn, in
/// front of its vertical blank, where video memory and the picture
/// describe one frame. Each call advances one frame: a frame held there
/// already finishes first; a frame past its boundary (a breakpoint stopped
/// it in its vertical blank) runs to its end and the next one to its
/// boundary. A breakpoint stops it where it stops. `FullDebug` does what
/// [`run_full`] does.
pub fn run_to_capture_boundary_on<M: DebugMachine>(m: &mut M, tier: ExecutionTier) {
    m.arm();
    if tier == ExecutionTier::FullDebug {
        run_full(m);
        return;
    }
    if m.held_at_capture_boundary() {
        run_tier(m, tier);
        if frame_call_stopped(m) {
            return;
        }
    }
    m.request_capture_boundary(true);
    // At most two frames: the rest of a frame already past its boundary,
    // then the next frame up to its own.
    for _ in 0..2 {
        run_tier(m, tier);
        if frame_call_stopped(m) {
            break;
        }
    }
    m.request_capture_boundary(false);
}

/// Whether the frame call just made stopped before its frame's end: at
/// the capture boundary, or at a breakpoint (which pauses the debugger,
/// even one that hit in the batch that ended the frame).
fn frame_call_stopped<M: DebugMachine>(m: &M) -> bool {
    m.frame_in_progress() || m.debug_state().get_execution_mode() != ExecutionMode::Running
}

/// One frame on the play tier, timing the parts of the frame call; the
/// debugger is disarmed for its length, as for a headless frame.
#[cfg(not(target_arch = "wasm32"))]
pub fn run_frame_profiled<M: DebugMachine>(m: &mut M) -> super::FrameProfile {
    m.disarm();
    let mut tier = super::Profiled::new();
    run_frame(m, &mut tier);
    m.arm();
    tier.into_profile()
}
