//! What a core implements for the frame driver.

use super::hooks::StepHooks;

/// What [`FrameMachine::handle_due`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Due {
    /// Nothing ends the pass: steps run to the next deadline.
    Continue,
    /// Stopped in front of the capture boundary a caller asked for
    /// ([`FrameMachine::request_capture_boundary`]): the picture is drawn
    /// and nothing of the vertical blank has run.
    Held,
    /// The frame's end is reached; the frame is not closed yet.
    FrameEnd,
}

/// What one machine step did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stepped {
    /// The time the step took, in the unit of [`FrameMachine::now`].
    pub elapsed: u64,
    /// Whether the main CPU ran an instruction: false for a step that only
    /// took an interrupt, and for an idle step of a stopped or waiting CPU.
    pub instruction: bool,
}

/// What a core implements so the frame driver can run it: its clock, its
/// timetable's next deadline, the handling of what falls due, one machine
/// step, and the frame's end.
///
/// The core keeps its timetable and its machine step; the driver owns the
/// loop, the tiers, the stops and the capture boundary's control flow.
pub trait FrameMachine: Sized {
    /// The counters a core's step updates every instruction, kept in local
    /// variables for one run of steps to a deadline: copied out of the
    /// machine by [`Self::begin_run`] and back by [`Self::end_run`]. `()`
    /// for a core whose step keeps everything in its fields.
    type Run;

    /// The clock now, in the core's own unit. The driver only compares it
    /// with the deadlines the core gives.
    fn now(&self) -> u64;

    /// Go on with the frame in progress, or open the next one. Returns the
    /// frame's end, which the driver hands back to [`Self::handle_due`]
    /// and [`Self::next_deadline`] unchanged; a core whose frame ends on a
    /// line count or a flag returns `u64::MAX` and ignores it.
    fn enter_frame(&mut self) -> u64;

    /// Handle what is due now: the queued events up to the clock, a line's
    /// end and the next line's start, or the flag that ends a frame. An
    /// event raised for the debugger goes through [`StepHooks::raised`],
    /// an interrupt taken through [`StepHooks::interrupt_taken`]. Returns
    /// [`Due::Held`] when it stopped in front of a capture boundary that
    /// was asked for, [`Due::FrameEnd`] when the frame's end is reached,
    /// else [`Due::Continue`].
    fn handle_due<H: StepHooks<Self>>(&mut self, frame_end: u64, hooks: &mut H) -> Due;

    /// Where the next run of steps stops: before the next event or at the
    /// line's end (`Some`), or after every step (`None`), for a core whose
    /// step keeps its own timetable. A deadline not ahead of
    /// [`Self::now`] means something is due already.
    fn next_deadline(&self, frame_end: u64) -> Option<u64>;

    /// Copy the step's counters out of the machine, before a run of steps.
    fn begin_run(&mut self) -> Self::Run;

    /// Write them back, after a run of steps or a stop.
    fn end_run(&mut self, run: Self::Run);

    /// One step of the main CPU and what its time does to the rest of the
    /// machine.
    ///
    /// The same rule holds for every CPU: a step reports an interrupt it
    /// takes through [`StepHooks::interrupt_taken`], and calls
    /// [`StepHooks::before_instruction`] and
    /// [`StepHooks::after_instruction`] only around an instruction it runs.
    /// A step may take an interrupt, run an instruction, do both (a CPU
    /// that takes its interrupt and runs the handler's first instruction
    /// in one step), or neither (a stopped or waiting CPU's idle step).
    ///
    /// Returns `None` when `before_instruction` stopped the step: the
    /// instruction has not run, and the next call goes on from it.
    fn machine_step<H: StepHooks<Self>>(
        &mut self,
        run: &mut Self::Run,
        hooks: &mut H,
    ) -> Option<Stepped>;

    /// The frame's end: count it and arm the next frame's timetable. The
    /// picture and the sound are finished where a core handles its frame's
    /// end ([`Self::handle_due`]), so the profiler sees that work. The next
    /// [`Self::enter_frame`] opens the next frame.
    fn close_frame(&mut self);

    /// Before a host step: leave a hold at the capture boundary the way the
    /// frame loop leaves it, then [`Self::enter_frame`].
    fn enter_step<H: StepHooks<Self>>(&mut self, hooks: &mut H) -> u64;

    /// Whether a frame is in progress: a frame call that stopped or a host
    /// step leaves one; a frame call that ran its frame to the end does
    /// not. After a reset or a load, a core whose savestate holds a frame
    /// in progress goes on with it, and one whose savestate cannot drops
    /// it.
    fn frame_in_progress(&self) -> bool;

    /// Whether a frame is held part of the way through in a way the
    /// core's savestate cannot hold, so that a save must finish it first
    /// ([`super::finish_held_frame`]).
    fn frame_held(&self) -> bool;

    /// Whether the frame is held at its capture boundary.
    fn held_at_capture_boundary(&self) -> bool;

    /// Ask [`Self::handle_due`] to stop in front of the capture boundary,
    /// or stop asking.
    fn request_capture_boundary(&mut self, on: bool);

    /// Whether a frame's end presents (the host's picture and sound); a
    /// headless frame turns it off.
    fn set_presenting(&mut self, on: bool);

    /// A frame call stopped part of the way through its frame (a
    /// breakpoint, a watchpoint, an event, the debugger paused or
    /// stepping): make the picture the host reads show the frame as far as
    /// it is drawn. A core that draws its lines straight into that picture
    /// has nothing to do.
    #[inline(always)]
    fn present_stop(&mut self) {}
}
