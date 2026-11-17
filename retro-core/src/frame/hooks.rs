//! The points a frame loop's work attaches to: what a core's machine calls
//! around its step and the events it handles ([`StepHooks`]), and what the
//! driver calls around a frame call ([`Tier`]).

/// What [`StepHooks::before_instruction`] decided about the instruction
/// the step is about to run.
pub enum Flow<S> {
    /// Run it; `S` is what the hooks keep for
    /// [`StepHooks::after_instruction`].
    Run(S),
    /// Stop before it: the instruction does not run, and the next step
    /// goes on from it.
    StopBefore,
}

/// The points a core's machine calls inside
/// [`super::FrameMachine::machine_step`] and [`super::FrameMachine::handle_due`].
/// Every body but `before_instruction` defaults to empty.
pub trait StepHooks<M> {
    /// What the hooks keep from before an instruction to after it.
    type Start;

    /// Before the instruction at `pc` runs.
    fn before_instruction(&mut self, m: &mut M, pc: u32) -> Flow<Self::Start>;

    /// Right after it ran. `cycles` is the instruction's cost as the
    /// core's trace or history entry records it; a core whose entries
    /// record none passes the step's time.
    #[inline(always)]
    fn after_instruction(&mut self, _m: &mut M, _start: Self::Start, _cycles: u32) {}

    /// The CPU took an interrupt, leaving the instruction at
    /// `interrupted_pc`, which it runs when the handler returns.
    #[inline(always)]
    fn interrupt_taken(&mut self, _m: &mut M, _interrupted_pc: u32) {}

    /// Something the machine did raised events the debugger checks:
    /// `check` checks them (it may drain a buffer) and says whether a
    /// breakpoint matched. The play tier never calls it.
    #[inline(always)]
    fn raised<F: FnOnce(&mut M) -> bool>(&mut self, _m: &mut M, _check: F) {}
}

/// No hooks at all: a step that records nothing and checks nothing.
impl<M> StepHooks<M> for () {
    type Start = ();

    #[inline(always)]
    fn before_instruction(&mut self, _m: &mut M, _pc: u32) -> super::Flow<()> {
        Flow::Run(())
    }
}

/// The part of a frame call the driver is in, for the profiler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// Handling what fell due.
    Due,
    /// Running steps to the next deadline.
    Run,
    /// Closing the frame.
    Close,
}

/// A frame loop's work at the points only the driver sees. Every body
/// defaults to empty, and [`Self::stop_requested`] to `false`.
pub trait Tier<M>: StepHooks<M> {
    /// A frame call begins.
    #[inline(always)]
    fn enter(&mut self, _m: &mut M) {}

    /// The frame call moves to `part`.
    #[inline(always)]
    fn part(&mut self, _m: &mut M, _part: Part) {}

    /// Whether a check asked the frame call to stop.
    #[inline(always)]
    fn stop_requested(&self, _m: &M) -> bool {
        false
    }

    /// The frame call stopped in front of the capture boundary.
    #[inline(always)]
    fn held(&mut self, _m: &mut M) {}

    /// The frame call stopped part of the way through its frame.
    #[inline(always)]
    fn stopped(&mut self, _m: &mut M) {}

    /// The frame call ran its frame to the end.
    #[inline(always)]
    fn leave(&mut self, _m: &mut M) {}
}
