//! The frame driver's conformance suite: checks a core's
//! [`super::FrameMachine`] and [`super::DebugMachine`] on a program of the
//! core's own, the way [`crate::capture::conformance`] checks its capture.
//!
//! A core describes itself in a [`FrameCoreUnderTest`]: how to build its
//! emulator with its program loaded, its state hash and the parts the hash
//! is made of, its frame count, and where its program can be stopped. Its
//! test file then calls
//! [`frame_conformance_tests!`](crate::frame_conformance_tests), which
//! writes one test per check, so each check passes or fails on its own;
//! each check is also a public function of this module.
//!
//! The program must, from its first frame on: run one instruction several
//! times a frame, the first time before its first interrupt; make a call
//! and return from it before its first interrupt; wait for an interrupt
//! once a frame, well before the interrupt comes; and take that interrupt
//! every frame from outside any call. A check that compares two machines
//! compares their state hashes, so every check holds the debugger to
//! changing nothing of the machine.

use crate::debug::{ExecutionTier, MAIN_CPU};
use crate::golden::arm_everything;
use crate::types::{
    Breakpoint, BreakpointId, BreakpointType, ExecutionMode, MatchSpec, BREAKPOINT_PARAM_ANY,
};
use crate::EmulatorCore;

use super::{finish_held_frame, run_frame_headless, run_to_capture_boundary, DebugMachine};

/// Frames a check that looks at a run of frames runs.
const FRAMES: u64 = 6;

/// Host steps or frame calls past which a check that waits for the program
/// to get somewhere gives up: several frames' worth of the shortest step.
const PATIENCE: u64 = 1_000_000;

/// What the suite needs of a core: its emulator with a program of its own,
/// the hashes no trait gives, and the program's landmarks.
pub struct FrameCoreUnderTest<E> {
    /// A new emulator with the program loaded and nothing run, its debugger
    /// running with nothing instrumented.
    pub build: fn() -> E,
    /// The core's hash of everything its savestate holds.
    pub state_hash: fn(&E) -> u64,
    /// The hashes of the parts the state hash is made of, each named, so a
    /// failure says which part two machines differ in.
    pub state_parts: fn(&E) -> Vec<(&'static str, u64)>,
    /// The frames the core has finished.
    pub frame_count: fn(&E) -> u64,
    /// An instruction of the main CPU (CPU 0) the program runs several
    /// times in every frame, the first time before its first interrupt,
    /// and that does not branch to itself.
    pub repeated_address: u32,
    /// The first instruction of the interrupt handler the program enters
    /// every frame, from outside any call.
    pub handler_address: u32,
    /// A call the program makes before its first interrupt: the address of
    /// the call instruction and the address its return comes back to.
    pub call: (u32, u32),
    /// An event breakpoint that stops a frame part of the way through its
    /// picture.
    pub stop_in_picture: Breakpoint,
    /// Where the main CPU waits for its interrupt once a frame: its program
    /// counter while it waits, the instruction it runs when the handler
    /// returns.
    pub waiting_at: u32,
}

/// Write one test per check of the frame conformance suite
/// ([`crate::frame::conformance`]) in a core's test file, each running its
/// check on the [`FrameCoreUnderTest`] the expression gives:
///
/// ```text
/// fn my_core() -> FrameCoreUnderTest<MyEmulator> { ... }
///
/// retro_core::frame_conformance_tests!(my_core());
/// ```
///
/// The core's test crate enables `retro-core`'s `frame-conformance`
/// feature, among its dev-dependencies.
#[macro_export]
macro_rules! frame_conformance_tests {
    ($core:expr) => {
        $crate::frame_conformance_tests!(@each $core;
            every_debug_loop_runs_the_fast_loops_machine,
            a_stop_resumes_into_the_frame_run_whole,
            the_execute_check_stops_before_the_instruction,
            a_run_goes_on_from_a_stop_and_stops_at_its_next_hit,
            a_stop_refreshes_the_cpu_states,
            an_interrupt_pushes_a_frame_its_return_pops_on_every_debug_loop,
            a_step_of_the_focused_cpu_is_the_step_the_trait_makes,
            a_host_step_records_each_instruction,
            host_steps_keep_the_timeline,
            a_step_from_the_capture_boundary_is_the_loops_next_step,
            finishing_a_held_frame_stops_nowhere_and_counts_nothing,
            a_headless_frame_is_the_presented_machine_and_counts_nothing,
            run_full_refreshes_when_paused_and_steps_once,
            the_play_tier_records_coverage,
            a_cpu_that_cannot_run_calls_no_hook,
            profiled_runs_the_play_machine,
        );
    };
    (@each $core:expr; $($check:ident),+ $(,)?) => {
        $(
            #[test]
            fn $check() {
                $crate::frame::conformance::$check(&$core);
            }
        )+
    };
}

// -- Helpers -------------------------------------------------------------

/// An execute breakpoint at `address`.
fn execute_at(address: u32) -> Breakpoint {
    Breakpoint {
        address,
        bp_type: BreakpointType::execute(),
        cpu_id: None,
        condition: None,
    }
}

/// Add `breakpoint`, with `spec` when one is given.
fn add<E: EmulatorCore>(
    e: &mut E,
    breakpoint: Breakpoint,
    spec: Option<MatchSpec>,
) -> BreakpointId {
    let id = e.debugger().add_breakpoint(breakpoint);
    if let Some(spec) = spec {
        e.debugger().set_breakpoint_match_spec(id, spec);
    }
    id
}

/// How many times breakpoint `id` matched.
fn hits<E: EmulatorCore>(e: &E, id: BreakpointId) -> u32 {
    e.debugger_ref()
        .list_breakpoints()
        .iter()
        .find(|info| info.id == id)
        .map(|info| info.hit_count)
        .expect("the breakpoint is listed")
}

/// Every breakpoint's hit count.
fn all_hits<E: EmulatorCore>(e: &E) -> Vec<u32> {
    e.debugger_ref()
        .list_breakpoints()
        .iter()
        .map(|info| info.hit_count)
        .collect()
}

/// Every check the debugger has, set to stop: an execute breakpoint and a
/// read and write watchpoint over the whole address space, and every event
/// of the console's catalogue with any parameter; and tracing on every CPU.
fn arm_to_stop<E: EmulatorCore>(e: &mut E) {
    let everywhere = MatchSpec {
        address_end: Some(u32::MAX),
        ..MatchSpec::default()
    };
    add(e, execute_at(0), Some(everywhere.clone()));
    let watch = Breakpoint {
        bp_type: BreakpointType::read_write(),
        ..execute_at(0)
    };
    add(e, watch, Some(everywhere));
    let codes: Vec<u32> = e
        .debugger_ref()
        .available_events()
        .iter()
        .map(|event| event.code)
        .collect();
    for code in codes {
        let event = Breakpoint {
            bp_type: BreakpointType::Event {
                code,
                param: BREAKPOINT_PARAM_ANY,
            },
            ..execute_at(0)
        };
        add(e, event, None);
    }
    trace_every_cpu(e);
}

/// Tracing on every CPU.
fn trace_every_cpu<E: EmulatorCore>(e: &mut E) {
    for cpu in 0..e.system_info().cpus.len() {
        e.debugger().set_tracing_enabled(cpu, true);
    }
}

/// How many entries each CPU's trace has taken.
fn traced<E: EmulatorCore>(e: &E) -> Vec<u64> {
    (0..e.system_info().cpus.len())
        .map(|cpu| {
            e.debugger_ref()
                .trace_stats(cpu)
                .map_or(0, |stats| stats.total_entries)
        })
        .collect()
}

/// Drop what each CPU's trace holds, so a long run does not fill it.
fn drop_traces<E: EmulatorCore>(e: &mut E) {
    for cpu in 0..e.system_info().cpus.len() {
        e.debugger().clear_trace_flushed(cpu);
    }
}

/// The main CPU's call stack, each frame as `(call, return)`.
fn call_stack<E: EmulatorCore>(e: &E) -> Vec<(u32, u32)> {
    e.debugger_ref()
        .get_call_stack(MAIN_CPU)
        .iter()
        .map(|frame| (frame.call_address, frame.return_address))
        .collect()
}

/// The addresses of the main CPU's history, the newest first.
fn history<E: EmulatorCore>(e: &E) -> Vec<u32> {
    e.debugger_ref()
        .get_instruction_history(MAIN_CPU, u32::MAX)
        .iter()
        .map(|entry| entry.address)
        .collect()
}

/// The main CPU's program counter as the debugger shows it, the states
/// written first.
fn pc<E: EmulatorCore>(e: &mut E) -> u32 {
    e.refresh_debugger_state();
    shown_pc(e)
}

/// The main CPU's program counter as the debugger shows it, without
/// writing the states.
fn shown_pc<E: EmulatorCore>(e: &E) -> u32 {
    e.debugger_ref().get_cpu_states()[MAIN_CPU].pc
}

/// Whether the debugger is paused.
fn paused<E: EmulatorCore>(e: &E) -> bool {
    e.debugger_ref().get_execution_mode() == ExecutionMode::Paused
}

/// Host steps of the main CPU until its program counter is `address`.
fn step_to<E: EmulatorCore>(e: &mut E, address: u32, what: &str) {
    let mut steps = 0;
    while pc(e) != address {
        e.step_cpu(MAIN_CPU);
        steps += 1;
        assert!(
            steps < PATIENCE,
            "{what}: the main CPU never reached ${address:06X}"
        );
    }
}

/// Frame calls until the core has finished `frame` frames, each stop
/// resumed. Returns how many frame calls stopped.
fn run_resuming<E: EmulatorCore>(
    e: &mut E,
    frame_count: fn(&E) -> u64,
    frame: u64,
    what: &str,
) -> u64 {
    let mut stops = 0;
    while frame_count(e) < frame {
        e.run_frame();
        if paused(e) {
            stops += 1;
            assert!(stops < PATIENCE, "{what}: frame {frame} never ends");
            e.set_execution_mode(ExecutionMode::Running);
        }
    }
    stops
}

/// The parts two machines' states differ in.
fn parted<E>(core: &FrameCoreUnderTest<E>, a: &E, b: &E) -> Vec<&'static str> {
    (core.state_parts)(a)
        .iter()
        .zip((core.state_parts)(b).iter())
        .filter(|(x, y)| x.1 != y.1)
        .map(|(x, _)| x.0)
        .collect()
}

/// The ways a debug tier is armed, as [`every_debug_loop_runs_the_fast_loops_machine`] runs it.
#[derive(Clone, Copy, Debug)]
enum Instrumented {
    /// One disabled execute breakpoint: the debug tier runs, and checks
    /// nothing that matches.
    Light,
    /// Every check, each set to log ([`arm_everything`]).
    Armed,
    /// Every CPU traced.
    Traced,
}

fn instrument<E: EmulatorCore>(e: &mut E, how: Instrumented, core: &FrameCoreUnderTest<E>) {
    match how {
        Instrumented::Light => {
            let id = add(e, execute_at(core.repeated_address), None);
            e.debugger().enable_breakpoint(id, false);
        }
        Instrumented::Armed => arm_everything(e),
        Instrumented::Traced => trace_every_cpu(e),
    }
}

// -- The checks ----------------------------------------------------------

/// The debugger's tier while it runs makes the machine the play tier makes,
/// frame by frame: with one disabled breakpoint, with every check armed to
/// log, and with every CPU traced. Nothing stops it.
pub fn every_debug_loop_runs_the_fast_loops_machine<E: DebugMachine>(core: &FrameCoreUnderTest<E>) {
    for how in [
        Instrumented::Light,
        Instrumented::Armed,
        Instrumented::Traced,
    ] {
        let mut play = (core.build)();
        let mut debug = (core.build)();
        instrument(&mut debug, how, core);
        assert_eq!(
            debug.debugger_ref().execution_tier(),
            ExecutionTier::LightDebug,
            "{how:?}: the frames run on the debug tier"
        );
        for frame in 1..=FRAMES {
            play.run_frame();
            debug.run_frame();
            assert!(!paused(&debug), "{how:?}: frame {frame} stopped");
            assert_eq!(
                (core.state_hash)(&debug),
                (core.state_hash)(&play),
                "{how:?}: frame {frame}: the debug tier parts from the play tier in {:?}",
                parted(core, &debug, &play)
            );
            drop_traces(&mut debug);
        }
    }
}

/// A frame stopped part of the way through, and resumed, ends as the frame
/// run whole: stopped at an execute breakpoint several times a frame, and
/// at an event breakpoint in its picture.
pub fn a_stop_resumes_into_the_frame_run_whole<E: DebugMachine>(core: &FrameCoreUnderTest<E>) {
    let stops: [(&str, Breakpoint); 2] = [
        ("an execute breakpoint", execute_at(core.repeated_address)),
        ("an event breakpoint", core.stop_in_picture.clone()),
    ];
    for (what, breakpoint) in stops {
        let mut whole = (core.build)();
        let mut stopped = (core.build)();
        add(&mut stopped, breakpoint, None);
        let mut calls_stopped = 0;
        for frame in 1..=FRAMES {
            whole.run_frame();
            calls_stopped += run_resuming(&mut stopped, core.frame_count, frame, what);
            assert_eq!(
                (core.state_hash)(&stopped),
                (core.state_hash)(&whole),
                "{what}: frame {frame}: the frame stopped parts from the frame run whole in {:?}",
                parted(core, &stopped, &whole)
            );
        }
        assert!(
            calls_stopped >= FRAMES,
            "{what}: it stops every frame ({calls_stopped} stops in {FRAMES} frames)"
        );
    }
}

/// An execute breakpoint stops a frame in front of its instruction: where
/// the main CPU, host-stepped, first reaches the instruction, and one host
/// step from either runs it the same way.
pub fn the_execute_check_stops_before_the_instruction<E: DebugMachine>(
    core: &FrameCoreUnderTest<E>,
) {
    let address = core.repeated_address;
    let mut looped = (core.build)();
    let id = add(&mut looped, execute_at(address), None);
    looped.run_frame();
    assert!(paused(&looped), "the frame call stopped");
    assert_eq!(pc(&mut looped), address, "in front of the instruction");
    assert_eq!(hits(&looped, id), 1);

    let mut stepped = (core.build)();
    step_to(&mut stepped, address, "host steps");
    let from_stop = looped.step_cpu(MAIN_CPU);
    let from_steps = stepped.step_cpu(MAIN_CPU);
    assert!(from_stop.instruction_executed && from_steps.instruction_executed);
    assert_eq!(from_stop.cycles, from_steps.cycles);
    assert_eq!(
        (core.state_hash)(&looped),
        (core.state_hash)(&stepped),
        "the instruction ran from the stop as from the host steps; they part in {:?}",
        parted(core, &looped, &stepped)
    );
    assert_eq!(hits(&looped, id), 1, "a host step checks no breakpoint");
}

/// A run goes on from an execute breakpoint, running the instruction it
/// stopped at, and stops at its next hit; so does a run after a host step
/// from the stop.
pub fn a_run_goes_on_from_a_stop_and_stops_at_its_next_hit<E: DebugMachine>(
    core: &FrameCoreUnderTest<E>,
) {
    let address = core.repeated_address;
    let mut e = (core.build)();
    let id = add(&mut e, execute_at(address), None);
    e.run_frame();
    assert!(paused(&e));
    assert_eq!(pc(&mut e), address);
    assert_eq!(hits(&e, id), 1);
    let at_first = (core.state_hash)(&e);

    e.set_execution_mode(ExecutionMode::Running);
    e.run_frame();
    assert!(paused(&e));
    assert_eq!(pc(&mut e), address, "it stops at the next pass");
    assert_eq!(hits(&e, id), 2);
    assert_ne!(
        (core.state_hash)(&e),
        at_first,
        "the instruction at the stop and the ones after it ran"
    );

    let step = e.step_cpu(MAIN_CPU);
    assert!(
        step.instruction_executed,
        "the host step runs the instruction at the stop"
    );
    assert_ne!(pc(&mut e), address);
    e.set_execution_mode(ExecutionMode::Running);
    e.run_frame();
    assert!(paused(&e));
    assert_eq!(pc(&mut e), address);
    assert_eq!(hits(&e, id), 3);
}

/// A frame call that stops writes the CPU states the debugger shows.
pub fn a_stop_refreshes_the_cpu_states<E: DebugMachine>(core: &FrameCoreUnderTest<E>) {
    let address = core.repeated_address;
    let mut e = (core.build)();
    assert_ne!(shown_pc(&e), address, "the program starts elsewhere");
    add(&mut e, execute_at(address), None);
    e.run_frame();
    assert!(paused(&e));
    assert_eq!(shown_pc(&e), address, "the states show where it stopped");
}

/// Taking an interrupt pushes a frame whose call and return are both the
/// interrupted address, on the debug tier and on host steps alike, and the
/// handler's return pops it.
pub fn an_interrupt_pushes_a_frame_its_return_pops_on_every_debug_loop<E: DebugMachine>(
    core: &FrameCoreUnderTest<E>,
) {
    let handler = core.handler_address;
    let mut running = (core.build)();
    add(&mut running, execute_at(handler), None);
    running.run_frame();
    assert!(paused(&running));
    assert_eq!(pc(&mut running), handler);
    let stack = call_stack(&running);
    let (call, ret) = *stack.last().expect("the interrupt pushed a frame");
    assert_eq!(
        call, ret,
        "an interrupt's frame names the interrupted address twice"
    );

    let mut stepped = (core.build)();
    step_to(&mut stepped, handler, "host steps to the handler");
    assert_eq!(
        call_stack(&stepped),
        stack,
        "the debug tier and the host steps keep one stack"
    );
    let depth = stack.len();
    let mut steps = 0;
    while call_stack(&stepped).len() >= depth {
        stepped.step_cpu(MAIN_CPU);
        steps += 1;
        assert!(steps < PATIENCE, "the handler never returns");
    }
    assert_eq!(
        call_stack(&stepped),
        stack[..depth - 1],
        "the return pops it"
    );
}

/// The trait's `step` is a step of the focused CPU: the same results, the
/// same machine, stack and history.
pub fn a_step_of_the_focused_cpu_is_the_step_the_trait_makes<E: DebugMachine>(
    core: &FrameCoreUnderTest<E>,
) {
    let mut by_trait = (core.build)();
    let mut by_cpu = (core.build)();
    for step in 0..64 {
        let a = EmulatorCore::step(&mut by_trait);
        let focused = by_cpu.focused_cpu();
        let b = by_cpu.step_cpu(focused);
        assert_eq!(
            (a.cycles, a.instruction_executed),
            (b.cycles, b.instruction_executed),
            "step {step}"
        );
    }
    assert_eq!((core.state_hash)(&by_trait), (core.state_hash)(&by_cpu));
    assert_eq!(call_stack(&by_trait), call_stack(&by_cpu));
    assert_eq!(history(&by_trait), history(&by_cpu));
}

/// Each host step that runs an instruction records it in the history,
/// newest first; the call pushes its frame and the return pops it.
pub fn a_host_step_records_each_instruction<E: DebugMachine>(core: &FrameCoreUnderTest<E>) {
    let (site, back) = core.call;
    let mut e = (core.build)();
    let step_one = |e: &mut E| {
        let from = pc(e);
        let before = history(e).len();
        let step = e.step_cpu(MAIN_CPU);
        let after = history(e);
        if step.instruction_executed {
            assert_eq!(
                after.len(),
                before + 1,
                "one record for the instruction at ${from:06X}"
            );
            assert_eq!(
                after[0], from,
                "the newest record is the instruction just run"
            );
        } else {
            assert_eq!(
                after.len(),
                before,
                "a step that ran nothing records nothing"
            );
        }
    };
    let mut steps = 0;
    while pc(&mut e) != site {
        step_one(&mut e);
        steps += 1;
        assert!(steps < PATIENCE, "the program never calls");
    }
    let depth = call_stack(&e).len();
    step_one(&mut e);
    assert_eq!(call_stack(&e).len(), depth + 1, "the call pushes a frame");
    assert_eq!(call_stack(&e).last(), Some(&(site, back)));
    while pc(&mut e) != back {
        step_one(&mut e);
        steps += 1;
        assert!(steps < PATIENCE, "the call never returns");
    }
    assert_eq!(call_stack(&e).len(), depth, "the return pops it");
}

/// Host steps keep the frames' timeline: stepped to the end of each frame,
/// the machine is the machine the frames make, part by part. A host step
/// handles what is due before its instruction as a pass of the frame loop
/// handles it.
pub fn host_steps_keep_the_timeline<E: DebugMachine>(core: &FrameCoreUnderTest<E>) {
    let mut frames = (core.build)();
    let mut stepped = (core.build)();
    let mut steps = 0;
    for frame in 1..=FRAMES {
        frames.run_frame();
        while (core.frame_count)(&stepped) < frame {
            stepped.step_cpu(MAIN_CPU);
            steps += 1;
            assert!(steps < PATIENCE * FRAMES, "frame {frame} never ends");
        }
        assert!(
            parted(core, &stepped, &frames).is_empty(),
            "frame {frame}: host steps part from the frames in {:?}",
            parted(core, &stepped, &frames)
        );
        assert_eq!((core.state_hash)(&stepped), (core.state_hash)(&frames));
    }
}

/// Host steps from the capture boundary go on as the frame loop goes on:
/// the hold left, what is due handled, then the steps to the first
/// instruction, whether the CPU is running or waiting there.
pub fn a_step_from_the_capture_boundary_is_the_loops_next_step<E: DebugMachine>(
    core: &FrameCoreUnderTest<E>,
) {
    let mut stepped = (core.build)();
    let mut looped = (core.build)();
    for e in [&mut stepped, &mut looped] {
        run_to_capture_boundary(e);
        assert!(e.held_at_capture_boundary(), "held at the capture boundary");
    }
    // The loop: a frame call stopped in front of the first instruction it
    // reaches, then the host step that runs it.
    let everywhere = MatchSpec {
        address_end: Some(u32::MAX),
        ..MatchSpec::default()
    };
    add(&mut looped, execute_at(0), Some(everywhere));
    looped.run_frame();
    assert!(paused(&looped));
    assert!(
        !looped.held_at_capture_boundary(),
        "the frame call left the hold"
    );
    assert!(looped.step_cpu(MAIN_CPU).instruction_executed);
    // Host steps until one runs an instruction.
    let mut steps = 0;
    while !stepped.step_cpu(MAIN_CPU).instruction_executed {
        steps += 1;
        assert!(steps < PATIENCE, "no instruction ever runs");
    }
    assert!(
        !stepped.held_at_capture_boundary(),
        "the host step left the hold"
    );
    assert_eq!(
        (core.state_hash)(&stepped),
        (core.state_hash)(&looped),
        "they part in {:?}",
        parted(core, &stepped, &looped)
    );
}

/// A frame held part of the way through is finished, before a save, as
/// the frame run whole, with no breakpoint stopping it and nothing of it
/// counted or traced; the debugger is armed again after.
pub fn finishing_a_held_frame_stops_nowhere_and_counts_nothing<E: DebugMachine>(
    core: &FrameCoreUnderTest<E>,
) {
    let mut whole = (core.build)();
    whole.run_frame();
    let mut held = (core.build)();
    run_to_capture_boundary(&mut held);
    assert!(
        held.frame_held(),
        "the hold at the capture boundary is a held frame"
    );
    arm_to_stop(&mut held);
    let traced_before = traced(&held);
    let stack_before = call_stack(&held);
    let history_before = history(&held);

    assert!(finish_held_frame(&mut held), "there was a frame to finish");
    assert!(!paused(&held), "nothing stopped it");
    assert!(!held.frame_held() && !held.frame_in_progress());
    assert_eq!((core.frame_count)(&held), 1);
    assert_eq!(
        (core.state_hash)(&held),
        (core.state_hash)(&whole),
        "the finished frame parts from the frame run whole in {:?}",
        parted(core, &held, &whole)
    );
    assert!(
        all_hits(&held).iter().all(|&n| n == 0),
        "no breakpoint counted"
    );
    assert_eq!(traced(&held), traced_before, "nothing traced");
    assert_eq!(call_stack(&held), stack_before);
    assert_eq!(history(&held), history_before);
    assert!(!finish_held_frame(&mut held), "nothing left to finish");

    // Armed again: the next frame call stops at once.
    held.run_frame();
    assert!(paused(&held), "the debugger is armed again after");
}

/// A headless frame is the presented frame's machine, and the debugger
/// counts and traces nothing of it; it is armed again after.
pub fn a_headless_frame_is_the_presented_machine_and_counts_nothing<E: DebugMachine>(
    core: &FrameCoreUnderTest<E>,
) {
    let mut presented = (core.build)();
    let mut headless = (core.build)();
    arm_everything(&mut headless);
    for frame in 1..=FRAMES {
        presented.run_frame();
        run_frame_headless(&mut headless);
        assert_eq!(
            (core.state_hash)(&headless),
            (core.state_hash)(&presented),
            "frame {frame}: the headless frame parts from the presented one in {:?}",
            parted(core, &headless, &presented)
        );
    }
    assert!(!paused(&headless));
    assert!(
        all_hits(&headless).iter().all(|&n| n == 0),
        "no breakpoint counted"
    );
    assert!(traced(&headless).iter().all(|&n| n == 0), "nothing traced");
    assert!(call_stack(&headless).is_empty());
    assert!(history(&headless).is_empty());

    headless.run_frame();
    assert!(
        all_hits(&headless).iter().any(|&n| n > 0),
        "the debugger is armed again after"
    );
}

/// The debugger's tier while it is paused writes the CPU states it shows
/// and runs nothing; in a step mode it makes one host step, then pauses.
pub fn run_full_refreshes_when_paused_and_steps_once<E: DebugMachine>(
    core: &FrameCoreUnderTest<E>,
) {
    // Two machines one frame in on the play tier, which writes no state the
    // debugger shows.
    let mut e = (core.build)();
    let mut twin = (core.build)();
    e.run_frame();
    twin.run_frame();
    twin.refresh_debugger_state();
    let live = shown_pc(&twin);
    assert_ne!(shown_pc(&e), live, "the play tier wrote no state");

    e.debugger().set_execution_mode(ExecutionMode::Paused);
    let before = (core.state_hash)(&e);
    e.run_frame();
    assert_eq!(
        (core.state_hash)(&e),
        before,
        "paused, a frame call runs nothing"
    );
    assert_eq!(shown_pc(&e), live, "and writes the states");

    e.debugger().set_execution_mode(ExecutionMode::StepInto);
    e.run_frame();
    twin.step_cpu(MAIN_CPU);
    assert!(paused(&e), "a step mode pauses after its step");
    assert_eq!(
        (core.state_hash)(&e),
        (core.state_hash)(&twin),
        "one host step"
    );
    assert_eq!(shown_pc(&e), shown_pc(&twin));
    assert_eq!(history(&e), history(&twin));
}

/// The play tier records the main CPU's coverage, as do a headless frame
/// and the finish of a held frame, which run on it.
pub fn the_play_tier_records_coverage<E: DebugMachine>(core: &FrameCoreUnderTest<E>) {
    let address = core.repeated_address;
    let mut played = (core.build)();
    assert_eq!(played.debugger_ref().execution_tier(), ExecutionTier::Fast);
    played.run_frame();
    assert!(played.debugger_ref().is_covered(MAIN_CPU, address));
    assert!(played.debugger_ref().coverage_stats(MAIN_CPU).1 > 0);

    let mut headless = (core.build)();
    run_frame_headless(&mut headless);
    assert!(headless.debugger_ref().is_covered(MAIN_CPU, address));

    let mut held = (core.build)();
    run_to_capture_boundary(&mut held);
    held.debugger().clear_coverage(MAIN_CPU);
    finish_held_frame(&mut held);
    assert!(
        held.debugger_ref().coverage_stats(MAIN_CPU).1 > 0,
        "the finish records coverage"
    );
}

/// A CPU that waits runs no instruction and calls no hook: an execute
/// breakpoint where it waits stops once a frame, when the instruction there
/// runs after the handler, never while it waits nor as it takes its
/// interrupt there; a host step while it waits records nothing.
pub fn a_cpu_that_cannot_run_calls_no_hook<E: DebugMachine>(core: &FrameCoreUnderTest<E>) {
    let waiting_at = core.waiting_at;
    let mut e = (core.build)();
    let id = add(&mut e, execute_at(waiting_at), None);
    for frame in 1..=FRAMES {
        let stops = run_resuming(&mut e, core.frame_count, frame, "waiting");
        assert_eq!(stops, 1, "frame {frame}: one stop where the CPU waits");
    }
    assert_eq!(hits(&e, id), FRAMES as u32);

    let mut stepped = (core.build)();
    step_to(&mut stepped, waiting_at, "host steps to the wait");
    let stack = call_stack(&stepped);
    let records = history(&stepped);
    let step = stepped.step_cpu(MAIN_CPU);
    assert!(
        !step.instruction_executed,
        "a waiting CPU runs no instruction"
    );
    assert_eq!(pc(&mut stepped), waiting_at);
    assert_eq!(call_stack(&stepped), stack);
    assert_eq!(history(&stepped), records, "and records nothing");
}

/// The profiler runs the play tier's machine, and measures it.
pub fn profiled_runs_the_play_machine<E: DebugMachine>(core: &FrameCoreUnderTest<E>) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut play = (core.build)();
        let mut profiled = (core.build)();
        for frame in 1..=FRAMES {
            play.run_frame();
            let profile = super::run_frame_profiled(&mut profiled);
            assert_eq!(
                (core.state_hash)(&profiled),
                (core.state_hash)(&play),
                "frame {frame}: the profiled frame parts from the played one in {:?}",
                parted(core, &profiled, &play)
            );
            assert_eq!((core.frame_count)(&profiled), frame);
            assert!(
                profile.instructions > 0,
                "frame {frame}: it counted the instructions"
            );
            assert!(profile.elapsed > 0);
            assert!(
                profile.due_ns + profile.run_ns + profile.close_ns <= profile.total_ns,
                "frame {frame}: the parts fit in the whole"
            );
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = core;
}
