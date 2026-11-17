//! The profiler: the play tier with the time of each part of a frame call
//! measured. It needs a wall clock, so it exists only off WebAssembly.

use std::time::Instant;

use crate::debug::MAIN_CPU;

use super::adapter::{DebugMachine, FrameDebugger};
use super::hooks::{Flow, Part, StepHooks, Tier};

/// Where one frame's time went.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameProfile {
    /// The whole frame call, in nanoseconds.
    pub total_ns: u64,
    /// Handling what fell due.
    pub due_ns: u64,
    /// Running steps between deadlines.
    pub run_ns: u64,
    /// Closing the frame.
    pub close_ns: u64,
    /// The instructions the main CPU ran.
    pub instructions: u64,
    /// The time the frame took in the machine's own unit
    /// ([`super::FrameMachine::now`]).
    pub elapsed: u64,
}

/// The play tier with the parts of a frame call timed and its
/// instructions counted ([`super::run_frame_profiled`]).
#[derive(Debug)]
pub struct Profiled {
    profile: FrameProfile,
    part: Option<(Part, Instant)>,
    started: Instant,
    clock_at_start: u64,
}

impl Profiled {
    /// A profiler with nothing measured yet.
    pub fn new() -> Self {
        Self {
            profile: FrameProfile::default(),
            part: None,
            started: Instant::now(),
            clock_at_start: 0,
        }
    }

    /// What it measured.
    pub fn into_profile(self) -> FrameProfile {
        self.profile
    }

    fn close_part(&mut self) {
        if let Some((part, since)) = self.part.take() {
            let ns = since.elapsed().as_nanos() as u64;
            match part {
                Part::Due => self.profile.due_ns += ns,
                Part::Run => self.profile.run_ns += ns,
                Part::Close => self.profile.close_ns += ns,
            }
        }
    }

    fn finish<M: DebugMachine>(&mut self, m: &M) {
        self.close_part();
        self.profile.total_ns = self.started.elapsed().as_nanos() as u64;
        self.profile.elapsed = m.now().saturating_sub(self.clock_at_start);
    }
}

impl Default for Profiled {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: DebugMachine> StepHooks<M> for Profiled {
    type Start = ();

    #[inline(always)]
    fn before_instruction(&mut self, m: &mut M, pc: u32) -> Flow<()> {
        m.debug_state_mut().record_coverage(MAIN_CPU, pc);
        self.profile.instructions += 1;
        Flow::Run(())
    }
}

impl<M: DebugMachine> Tier<M> for Profiled {
    fn enter(&mut self, m: &mut M) {
        self.started = Instant::now();
        self.clock_at_start = m.now();
    }

    fn part(&mut self, _m: &mut M, part: Part) {
        self.close_part();
        self.part = Some((part, Instant::now()));
    }

    fn held(&mut self, m: &mut M) {
        self.finish(m);
    }

    fn stopped(&mut self, m: &mut M) {
        self.finish(m);
    }

    fn leave(&mut self, m: &mut M) {
        self.finish(m);
    }
}
