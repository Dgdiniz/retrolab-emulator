//! The machine conformance suite: what every console's [`EmulatorCore`]
//! does the same way, checked once here and run by each core on a program
//! of its own (behind the `emulator-conformance` feature).
//!
//! A core's test file names its program in a [`MachineUnderTest`] and
//! expands [`emulator_conformance_tests!`](crate::emulator_conformance_tests),
//! which writes one test per check:
//!
//! ```text
//! fn my_machine() -> MachineUnderTest<MyEmulator> { ... }
//!
//! retro_core::emulator_conformance_tests!(my_machine());
//! ```
//!
//! The checks run frames on the play tier (the machine runs with nothing of
//! the debugger armed) unless one says otherwise. Sound is compared only
//! between machines that never loaded a state: the host's sound path is
//! not machine state, and after a load each machine resamples from where
//! its own last frame left it. A state taken part of the way through a
//! frame holds the machine, not the picture drawn so far: after such a
//! load the pictures are compared from the first frame drawn whole.
//!
//! [`golden_saves`] holds the same rules to a console's golden records:
//! saves at frames' ends and at stops part of the way through, each
//! loaded into a new machine.

mod golden_saves;
pub use golden_saves::{golden_saves, GoldenSaves, SavesReport};

use crate::capture::{CaptureCore, CaptureRecording};
use crate::frame::DebugMachine;
use crate::golden::GoldenConsole;
use crate::savestate::{
    deserialize_compressed, serialize_compressed, LoadStateError, RomCheck, SavestatePayload,
};
use crate::types::{
    AccessFlags, Breakpoint, BreakpointType, ExecutionMode, InputState, MemoryRegion,
};
use crate::EmulatorCore;
use std::collections::HashMap;

/// A console's program and how to build a machine running it.
pub struct MachineUnderTest<E> {
    /// A new machine with the program loaded, its pads plugged in, its
    /// sound on and its debugger running.
    pub build: fn() -> E,
    /// The program: it reads pad 1 into work RAM every frame and keys a
    /// tone, and its cartridge has battery RAM.
    pub rom: fn() -> Vec<u8>,
    /// The same program under another ROM hash.
    pub other_rom: fn() -> Vec<u8>,
    /// The buttons held on pad 1 at each frame, in `InputState::buttons`.
    pub input: fn(u64) -> u32,
    /// Work RAM the program writes, as (address, length) on the main
    /// CPU's bus.
    pub work_ram: (u32, u32),
    /// Seconds the drift check runs.
    pub drift_seconds: u32,
    /// Bring a machine `build` made, the program run a while, to a moment
    /// whose state the payload's layout has no field for (a transfer or a
    /// chip's busy time running at a frame's end, an interrupt on its way
    /// at a stop), and leave it there; `None` for a console whose layout
    /// lacks nothing.
    pub extension_moment: Option<fn(&mut E)>,
    /// An execute breakpoint on the main CPU the program reaches many
    /// times a frame.
    pub mid_frame_stop: Breakpoint,
}

/// Write one test per check of the machine conformance suite in a core's
/// test file, each running its check on the [`MachineUnderTest`] the
/// expression gives. The core's test crate enables `retro-core`'s
/// `emulator-conformance` feature among its dev-dependencies.
#[macro_export]
macro_rules! emulator_conformance_tests {
    ($machine:expr) => {
        $crate::emulator_conformance_tests!(@each $machine;
            two_machines_stay_identical,
            a_loaded_save_goes_on_identically,
            a_save_at_a_moment_the_layout_lacks_goes_on_identically,
            a_save_taken_mid_frame_goes_on_identically,
            a_rollback_resimulation_matches,
            different_input_diverges,
            the_audio_switch_is_machine_state,
            discarding_pending_audio_leaves_the_state,
            a_resimulation_burst_queues_no_old_audio,
            state_hash_parts_name_the_difference,
            power_on_reset_converges,
            power_on_reset_keeps_settings_and_arms_nothing,
            the_reset_button_keeps_work_ram_and_save,
            hard_reset_is_a_fresh_load_with_the_save,
            no_reset_or_load_changes_the_mode,
            a_region_change_power_cycles,
            the_region_survives_a_savestate,
            host_audio_does_not_drift,
            debug_reads_change_nothing,
            a_savestate_opens_only_on_its_core_and_build,
            the_button_layout_names_the_pads_bits,
            audio_debuggers_answer_their_count,
        );
    };
    (@each $machine:expr; $($check:ident),+ $(,)?) => {
        $(
            #[test]
            fn $check() {
                $crate::emulator::conformance::$check(&$machine);
            }
        )+
    };
}

// -- Helpers -------------------------------------------------------------

/// Run `frames` frames from frame `from`, holding the machine's input on
/// pad 1 at each.
fn run<E: EmulatorCore>(m: &MachineUnderTest<E>, emu: &mut E, from: u64, frames: u64) {
    for frame in from..from + frames {
        emu.set_input(
            0,
            InputState {
                buttons: (m.input)(frame),
            },
        );
        emu.run_frame();
    }
}

/// The machine's work RAM as the debugger reads it.
fn work_ram<E: EmulatorCore>(m: &MachineUnderTest<E>, emu: &mut E) -> Vec<u8> {
    emu.read_memory_debug(m.work_ram.0, m.work_ram.1)
}

/// A battery save the program never writes itself.
fn save_pattern<E: EmulatorCore>(emu: &E) -> Vec<u8> {
    let len = emu.get_save_ram().map_or(0, <[u8]>::len);
    assert!(len > 0, "the program's cartridge has battery RAM");
    (0..len)
        .map(|i| (i as u8).wrapping_mul(37) ^ 0x5A)
        .collect()
}

/// Remove every breakpoint, watchpoint and event a machine holds.
fn remove_breakpoints<E: EmulatorCore>(emu: &mut E) {
    for info in emu.debugger_ref().list_breakpoints() {
        emu.debugger().remove_breakpoint(info.id);
    }
}

/// The main CPU's program counter, as the debugger shows it.
fn main_pc<E: EmulatorCore>(emu: &E) -> u32 {
    emu.debugger_ref()
        .get_cpu_states()
        .first()
        .map_or(0, |cpu| cpu.pc)
}

/// The machine that saved (`a`, its breakpoints removed) and a new machine
/// that loaded its state (`b`) run `frames` frames from frame `from`, both
/// running, with the same input: the state equal after every frame, the
/// picture after every frame drawn whole on both (the first frame is not
/// when the state was taken part of the way through it).
fn go_on_together<E: EmulatorCore + DebugMachine>(
    m: &MachineUnderTest<E>,
    a: &mut E,
    b: &mut E,
    from: u64,
    frames: u64,
) {
    let mut whole = !a.frame_in_progress();
    remove_breakpoints(a);
    a.set_execution_mode(ExecutionMode::Running);
    b.set_execution_mode(ExecutionMode::Running);
    for frame in from..from + frames {
        run(m, a, frame, 1);
        run(m, b, frame, 1);
        assert_eq!(a.state_hash(), b.state_hash(), "state at frame {frame}");
        if whole {
            assert!(
                a.get_framebuffer().data == b.get_framebuffer().data,
                "picture at frame {frame}"
            );
        }
        whole = true;
    }
}

/// The supported region the machine is not in, if the console has one.
fn other_region<E: EmulatorCore>(emu: &E) -> Option<crate::types::VideoRegion> {
    emu.supported_regions()
        .iter()
        .copied()
        .find(|&r| r != emu.region())
}

// -- The checks ----------------------------------------------------------

/// Two machines given the same input are the same machine: the state, the
/// picture and the samples the host takes are equal every frame.
pub fn two_machines_stay_identical<E: EmulatorCore>(m: &MachineUnderTest<E>) {
    let mut a = (m.build)();
    let mut b = (m.build)();
    for frame in 0..600 {
        run(m, &mut a, frame, 1);
        run(m, &mut b, frame, 1);
        assert_eq!(a.state_hash(), b.state_hash(), "state at frame {frame}");
        assert!(
            a.get_framebuffer().data == b.get_framebuffer().data,
            "picture at frame {frame}"
        );
        let sound = a.get_audio_samples().to_vec();
        assert_eq!(sound, b.get_audio_samples(), "samples at frame {frame}");
    }
}

/// A state saved after 300 frames and loaded into a new machine is that
/// machine at once, and the two go on as one for 300 frames.
pub fn a_loaded_save_goes_on_identically<E: EmulatorCore>(m: &MachineUnderTest<E>) {
    let mut a = (m.build)();
    run(m, &mut a, 0, 300);
    let state = a.save_state();
    assert!(!state.is_empty(), "the save has bytes");
    let mut b = (m.build)();
    b.load_state(&state, RomCheck::SameRom)
        .expect("the save loads");
    assert_eq!(b.state_hash(), a.state_hash(), "the loaded state");
    assert_eq!(b.frame_count(), a.frame_count(), "the frame count");
    for frame in 300..600 {
        run(m, &mut a, frame, 1);
        run(m, &mut b, frame, 1);
        assert_eq!(a.state_hash(), b.state_hash(), "state at frame {frame}");
        assert!(
            a.get_framebuffer().data == b.get_framebuffer().data,
            "picture at frame {frame}"
        );
    }
}

/// A state saved where the machine holds what the payload's layout has no
/// field for carries it after the payload, and a new machine loading it is
/// that machine at once and goes on as it does.
pub fn a_save_at_a_moment_the_layout_lacks_goes_on_identically<E: EmulatorCore + DebugMachine>(
    m: &MachineUnderTest<E>,
) {
    let Some(moment) = m.extension_moment else {
        return;
    };
    let mut a = (m.build)();
    run(m, &mut a, 0, 60);
    moment(&mut a);
    // The frame the machine is in: the frames it counted, from its load.
    let frame = a.frame_count();
    let state = a.save_state();
    let (_, extension) =
        E::Savestate::open(&state, None, RomCheck::AnyRom).expect("the save opens");
    assert!(
        extension != Default::default(),
        "the save holds what the layout has no field for"
    );
    let mut b = (m.build)();
    b.load_state(&state, RomCheck::SameRom)
        .expect("the save loads");
    assert_eq!(b.state_hash(), a.state_hash(), "the loaded state");
    go_on_together(m, &mut a, &mut b, frame, 120);
}

/// A state saved at a stop part of the way through a frame holds the stop:
/// the save does not move the machine stopped there, and a new machine
/// loading it is that machine, stopped where it stopped, and goes on as it
/// does.
pub fn a_save_taken_mid_frame_goes_on_identically<E: EmulatorCore + DebugMachine>(
    m: &MachineUnderTest<E>,
) {
    // Into the frames the program is given buttons, so the frame stopped in
    // reads buttons held.
    let pressed = (60..600).find(|&f| (m.input)(f) != 0).unwrap_or(60);
    let mut a = (m.build)();
    run(m, &mut a, 0, pressed + 30);
    a.debugger().add_breakpoint(m.mid_frame_stop.clone());
    // A thousand stops at the breakpoint take the machine well into a frame.
    let first = a.frame_count();
    let mut stops = 0;
    while stops < 1000 {
        let frame = a.frame_count();
        assert!(
            frame < first + 60,
            "the program reaches the breakpoint {stops} times in 60 frames"
        );
        a.set_execution_mode(ExecutionMode::Running);
        run(m, &mut a, frame, 1);
        if a.execution_mode() == ExecutionMode::Paused {
            stops += 1;
        }
    }
    assert!(
        a.frame_in_progress(),
        "stopped part of the way through a frame"
    );
    let stopped = (a.state_hash(), a.frame_count(), main_pc(&a));
    let state = a.save_state();
    assert_eq!(
        (a.state_hash(), a.frame_count(), main_pc(&a)),
        stopped,
        "the save moved the stopped machine"
    );
    let mut b = (m.build)();
    b.load_state(&state, RomCheck::SameRom)
        .expect("the save loads");
    assert_eq!(b.state_hash(), stopped.0, "the loaded state");
    assert!(b.frame_in_progress(), "the frame in progress loaded");
    b.refresh_debugger_state();
    assert_eq!(main_pc(&b), stopped.2, "the loaded program counter");
    let frame = a.frame_count();
    go_on_together(m, &mut a, &mut b, frame, 120);
}

/// A rollback: saved at frame 120, run 60 frames on, loaded back and run
/// those frames again headless with the same input, the machine passes
/// through every state it had.
pub fn a_rollback_resimulation_matches<E: EmulatorCore + DebugMachine>(m: &MachineUnderTest<E>) {
    let mut emu = (m.build)();
    run(m, &mut emu, 0, 120);
    let state = emu.save_state();
    let mut hashes = Vec::new();
    for frame in 120..180 {
        run(m, &mut emu, frame, 1);
        hashes.push(emu.state_hash());
    }
    emu.load_state(&state, RomCheck::SameRom)
        .expect("the save loads");
    for (i, frame) in (120..180).enumerate() {
        emu.set_input(
            0,
            InputState {
                buttons: (m.input)(frame),
            },
        );
        emu.run_frame_headless();
        assert_eq!(emu.state_hash(), hashes[i], "state at frame {frame}");
    }
}

/// One press that differs makes another machine: the hashes part within
/// three frames of it.
pub fn different_input_diverges<E: EmulatorCore>(m: &MachineUnderTest<E>) {
    let mut a = (m.build)();
    let mut b = (m.build)();
    run(m, &mut a, 0, 60);
    run(m, &mut b, 0, 60);
    assert_eq!(a.state_hash(), b.state_hash());
    let held = (m.input)(60);
    a.set_input(0, InputState { buttons: held });
    b.set_input(
        0,
        InputState {
            buttons: held ^ 0x1,
        },
    );
    let mut parted = false;
    for _ in 0..3 {
        a.run_frame();
        b.run_frame();
        parted |= a.state_hash() != b.state_hash();
    }
    assert!(parted, "a different press changes the state");
}

/// The sound switch is part of the machine: switching it off changes the
/// state hash.
pub fn the_audio_switch_is_machine_state<E: EmulatorCore>(m: &MachineUnderTest<E>) {
    let mut a = (m.build)();
    let mut b = (m.build)();
    run(m, &mut a, 0, 30);
    run(m, &mut b, 0, 30);
    assert_eq!(a.state_hash(), b.state_hash());
    b.set_audio_enabled(false);
    assert_ne!(a.state_hash(), b.state_hash());
}

/// Dropping the sound made but not yet queued for the host changes no
/// machine state.
pub fn discarding_pending_audio_leaves_the_state<E: EmulatorCore>(m: &MachineUnderTest<E>) {
    let mut emu = (m.build)();
    run(m, &mut emu, 0, 30);
    let before = emu.state_hash();
    emu.discard_pending_audio();
    assert_eq!(emu.state_hash(), before);
}

/// After 30 headless frames and the drop of what they left, one presented
/// frame queues one frame of sound, not 31.
pub fn a_resimulation_burst_queues_no_old_audio<E: EmulatorCore + DebugMachine>(
    m: &MachineUnderTest<E>,
) {
    let mut emu = (m.build)();
    run(m, &mut emu, 0, 10);
    emu.get_audio_samples();
    for frame in 10..40 {
        emu.set_input(
            0,
            InputState {
                buttons: (m.input)(frame),
            },
        );
        emu.run_frame_headless();
    }
    emu.discard_pending_audio();
    run(m, &mut emu, 40, 1);
    let info = emu.get_audio_info();
    let per_frame =
        info.sample_rate as f64 * info.channels as f64 / emu.get_display_info().refresh_rate as f64;
    let queued = emu.get_audio_samples().len() as f64;
    assert!(
        queued <= per_frame * 1.5,
        "{queued} samples queued, one frame is {per_frame:.0}"
    );
}

/// Two equal machines have equal parts; a poke of work RAM moves the hash
/// and some of the parts, not all of them.
pub fn state_hash_parts_name_the_difference<E: EmulatorCore>(m: &MachineUnderTest<E>) {
    let mut a = (m.build)();
    let mut b = (m.build)();
    run(m, &mut a, 0, 60);
    run(m, &mut b, 0, 60);
    assert_eq!(a.state_hash_parts(), b.state_hash_parts());
    let byte = b.read_memory_debug(m.work_ram.0, 1)[0];
    b.write_memory_debug(m.work_ram.0, &[byte ^ 0xFF]);
    assert_ne!(a.state_hash(), b.state_hash(), "the poke moves the hash");
    let (pa, pb) = (a.state_hash_parts(), b.state_hash_parts());
    assert_eq!(
        pa.iter().map(|p| p.0).collect::<Vec<_>>(),
        pb.iter().map(|p| p.0).collect::<Vec<_>>(),
        "the same parts"
    );
    let moved = pa.iter().zip(&pb).filter(|(x, y)| x.1 != y.1).count();
    assert!(
        moved > 0 && moved < pa.len() / 2,
        "{moved} of {} parts moved",
        pa.len()
    );
}

/// Two machines that ran the game for different lengths, with different
/// input, are the same machine after netplay's power cycle, and stay so.
pub fn power_on_reset_converges<E: EmulatorCore>(m: &MachineUnderTest<E>) {
    let mut a = (m.build)();
    let mut b = (m.build)();
    run(m, &mut a, 0, 300);
    for frame in 0..500 {
        b.set_input(
            0,
            InputState {
                buttons: (m.input)(frame + 7) ^ 0x3,
            },
        );
        b.run_frame();
    }
    assert_ne!(a.state_hash(), b.state_hash());
    a.power_on_reset();
    b.power_on_reset();
    assert_eq!(a.state_hash(), b.state_hash(), "after the power cycle");
    for frame in 0..120 {
        run(m, &mut a, frame, 1);
        run(m, &mut b, frame, 1);
        assert_eq!(a.state_hash(), b.state_hash(), "state at frame {frame}");
    }
}

/// Netplay's power cycle keeps every setting of the player and the host
/// (the controls, the sound switch, the region, what a capture records,
/// the coverage, the CPU shown, the run mode), lets go of what the input
/// holds, leaves nothing of the debugger armed (no breakpoint, watchpoint
/// or event, every trace off) and the save blank.
pub fn power_on_reset_keeps_settings_and_arms_nothing<E: EmulatorCore + CaptureCore>(
    m: &MachineUnderTest<E>,
) {
    let mut emu = (m.build)();
    let blank_save = emu.get_save_ram().map(<[u8]>::to_vec);
    let input = emu.input_manager_mut();
    input.set_keyboard_mapping(HashMap::from([("KeyQ".to_string(), 7u32)]));
    input.add_gamepad("pad-1");
    input.set_gamepad_mapping("pad-1", HashMap::from([(0u32, 6u32)]));
    input.set_gamepad_deadzone("pad-1", 0.4);
    let audio = !emu.is_audio_enabled();
    emu.set_audio_enabled(audio);
    let region = other_region(&emu).unwrap_or(emu.region());
    emu.set_region(region).expect("a supported region");
    let recording = CaptureRecording {
        line_states: true,
        ..CaptureRecording::default()
    };
    emu.set_capture_recording(recording);
    run(m, &mut emu, 0, 3);
    let save = save_pattern(&emu);
    emu.set_save_ram(&save);
    emu.set_focused_cpu(1);
    let debugger = emu.debugger();
    debugger.add_breakpoint(Breakpoint {
        address: 0x00FF_FFF0,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    });
    debugger.add_breakpoint(Breakpoint {
        address: m.work_ram.0,
        bp_type: BreakpointType::Access(AccessFlags::WRITE),
        cpu_id: Some(0),
        condition: None,
    });
    debugger.add_breakpoint(Breakpoint {
        address: 0,
        bp_type: BreakpointType::Event { code: 1, param: 0 },
        cpu_id: None,
        condition: None,
    });
    debugger.set_tracing_enabled(0, true);
    emu.input_manager_mut()
        .update_keyboard("KeyQ".to_string(), true);
    emu.set_input(0, InputState { buttons: 0x3 });
    let covered = emu.debugger_ref().coverage_stats(0).1;
    assert!(covered > 0, "the frames ran code");

    emu.power_on_reset();

    let mut wrong = Vec::new();
    let mut check = |what: &str, ok: bool, now: String| {
        if !ok {
            wrong.push(format!("{what}: {now}"));
        }
    };
    check(
        "the sound switch",
        emu.is_audio_enabled() == audio,
        emu.is_audio_enabled().to_string(),
    );
    check(
        "the region",
        emu.region() == region,
        format!("{:?}", emu.region()),
    );
    check(
        "what a capture records",
        emu.capture_recording() == recording,
        format!("{:?}", emu.capture_recording()),
    );
    check(
        "the CPU shown",
        emu.focused_cpu() == 1,
        emu.focused_cpu().to_string(),
    );
    let covered_after = emu.debugger_ref().coverage_stats(0).1;
    check(
        "the coverage",
        covered_after == covered,
        format!("{covered_after} of {covered}"),
    );
    let mode = emu.execution_mode();
    check(
        "the run mode",
        mode == ExecutionMode::Running,
        format!("{mode:?}"),
    );
    let keys = emu.input_manager().get_keyboard_mapping();
    check(
        "the key map",
        keys.get("KeyQ") == Some(&7),
        format!("{:?}", keys.get("KeyQ")),
    );
    let dead_zone = emu.input_manager().get_gamepad_deadzone("pad-1");
    check(
        "the pad's dead zone",
        dead_zone == 0.4,
        dead_zone.to_string(),
    );
    let held = emu.input_manager().get_port_state(0);
    check("what the input held", held == 0, format!("{held:#x}"));
    let armed = emu.debugger_ref().list_breakpoints().len();
    check("the breakpoints left", armed == 0, armed.to_string());
    let tracing = emu.debugger_ref().is_tracing_enabled(0);
    check("the trace", !tracing, tracing.to_string());
    let save_now = emu.get_save_ram().map(<[u8]>::to_vec);
    check(
        "the save",
        save_now == blank_save,
        "not the blank save".to_string(),
    );
    assert!(
        wrong.is_empty(),
        "after the power cycle:\n{}",
        wrong.join("\n")
    );
}

/// The reset button keeps work RAM, the save, the region and the run mode.
/// What else it keeps is the console's: what its reset line resets.
pub fn the_reset_button_keeps_work_ram_and_save<E: EmulatorCore>(m: &MachineUnderTest<E>) {
    for mode in [ExecutionMode::Running, ExecutionMode::Paused] {
        let mut emu = (m.build)();
        run(m, &mut emu, 0, 120);
        let save = save_pattern(&emu);
        emu.set_save_ram(&save);
        emu.set_execution_mode(mode);
        let ram = work_ram(m, &mut emu);
        let region = emu.region();
        emu.reset();
        assert_eq!(work_ram(m, &mut emu), ram, "{mode:?}: work RAM");
        assert_eq!(
            emu.get_save_ram().map(<[u8]>::to_vec),
            Some(save),
            "{mode:?}: the save"
        );
        assert_eq!(emu.region(), region, "{mode:?}: the region");
        assert_eq!(emu.execution_mode(), mode, "{mode:?}: the mode");
    }
}

/// The app's Reset is a fresh load of the cartridge that keeps its save:
/// the machine equals a new one given the save, in the same region, its
/// breakpoints and mode kept.
pub fn hard_reset_is_a_fresh_load_with_the_save<E: EmulatorCore>(m: &MachineUnderTest<E>) {
    let mut emu = (m.build)();
    run(m, &mut emu, 0, 200);
    let save = save_pattern(&emu);
    emu.set_save_ram(&save);
    emu.debugger().add_breakpoint(Breakpoint {
        address: 0x00FF_FFF0,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    });
    let region = emu.region();
    emu.hard_reset();

    let mut fresh = (m.build)();
    fresh.set_save_ram(&save);
    assert_eq!(emu.state_hash(), fresh.state_hash(), "a fresh load's state");
    assert_eq!(emu.region(), region, "the region");
    assert_eq!(
        emu.get_save_ram().map(<[u8]>::to_vec),
        Some(save),
        "the save"
    );
    assert_eq!(
        emu.debugger_ref().list_breakpoints().len(),
        1,
        "the breakpoint"
    );
    assert_eq!(emu.execution_mode(), ExecutionMode::Running, "the mode");
}

/// No reset, ROM load or state load changes the debugger's mode.
pub fn no_reset_or_load_changes_the_mode<E: EmulatorCore>(m: &MachineUnderTest<E>) {
    for mode in [ExecutionMode::Running, ExecutionMode::Paused] {
        let mut emu = (m.build)();
        run(m, &mut emu, 0, 10);
        let state = emu.save_state();
        emu.set_execution_mode(mode);
        emu.reset();
        assert_eq!(emu.execution_mode(), mode, "{mode:?}: the reset button");
        emu.hard_reset();
        assert_eq!(emu.execution_mode(), mode, "{mode:?}: the app's Reset");
        emu.power_on_reset();
        assert_eq!(
            emu.execution_mode(),
            mode,
            "{mode:?}: netplay's power cycle"
        );
        emu.load_rom(&(m.rom)()).expect("the program loads");
        assert_eq!(emu.execution_mode(), mode, "{mode:?}: a ROM load");
        emu.load_state(&state, RomCheck::SameRom)
            .expect("the save loads");
        assert_eq!(emu.execution_mode(), mode, "{mode:?}: a state load");
    }
}

/// Another region is a power cycle in it that keeps the save: the region,
/// the refresh rate and the master clock follow, and the machine equals a
/// new one set to it and given the save. The region the machine is in
/// already changes nothing.
pub fn a_region_change_power_cycles<E: EmulatorCore>(m: &MachineUnderTest<E>) {
    let mut emu = (m.build)();
    run(m, &mut emu, 0, 30);
    let before = emu.state_hash();
    emu.set_region(emu.region()).expect("the machine's region");
    assert_eq!(emu.state_hash(), before, "the same region changes nothing");

    let home = emu.region();
    let Some(region) = other_region(&emu) else {
        return;
    };
    let rate = emu.get_display_info().refresh_rate;
    let save = save_pattern(&emu);
    emu.set_save_ram(&save);
    emu.set_region(region).expect("a supported region");

    let mut fresh = (m.build)();
    fresh.set_region(region).expect("a supported region");
    fresh.set_save_ram(&save);
    assert_eq!(emu.region(), region, "the region");
    assert_ne!(
        emu.get_display_info().refresh_rate,
        rate,
        "the refresh rate"
    );
    assert_eq!(
        emu.get_display_info().refresh_rate,
        fresh.get_display_info().refresh_rate,
        "the refresh rate"
    );
    assert_eq!(emu.master_clock_hz(), fresh.master_clock_hz(), "the clock");
    assert_eq!(emu.state_hash(), fresh.state_hash(), "a power cycle in it");
    assert_eq!(emu.get_save_ram().map(<[u8]>::to_vec), Some(save.clone()));

    emu.set_region(home).expect("the program's region");
    let mut back = (m.build)();
    back.set_save_ram(&save);
    assert_eq!(emu.state_hash(), back.state_hash(), "back in its region");
}

/// A state brings its region: loaded into a machine in another region, it
/// gives the state's region, refresh rate and master clock, and the
/// machine runs on from it.
pub fn the_region_survives_a_savestate<E: EmulatorCore>(m: &MachineUnderTest<E>) {
    let mut saved = (m.build)();
    let Some(region) = other_region(&saved) else {
        return;
    };
    saved.set_region(region).expect("a supported region");
    run(m, &mut saved, 0, 60);
    let state = saved.save_state();
    let mut emu = (m.build)();
    emu.load_state(&state, RomCheck::SameRom)
        .expect("the save loads");
    assert_eq!(emu.region(), region, "the region");
    assert_eq!(
        emu.get_display_info().refresh_rate,
        saved.get_display_info().refresh_rate,
        "the refresh rate"
    );
    assert_eq!(emu.master_clock_hz(), saved.master_clock_hz(), "the clock");
    run(m, &mut emu, 60, 60);
    assert_eq!(emu.region(), region, "the region after frames");
}

/// The host's sound does not drift: the frames of `drift_seconds` seconds
/// give that many seconds of samples within 0.1 %, with no sample dropped
/// and none asked for that was not there.
pub fn host_audio_does_not_drift<E: EmulatorCore>(m: &MachineUnderTest<E>) {
    let mut emu = (m.build)();
    let seconds = m.drift_seconds as f64;
    let frames = (emu.get_display_info().refresh_rate as f64 * seconds).round() as u64;
    let mut total = 0usize;
    for frame in 0..frames {
        run(m, &mut emu, frame, 1);
        total += emu.get_audio_samples().len();
    }
    let info = emu.get_audio_info();
    let expected = info.sample_rate as f64 * seconds * info.channels as f64;
    let off = (total as f64 - expected).abs() / expected;
    assert!(
        off < 0.001,
        "{total} samples for {expected:.0} ({:.3} % off)",
        off * 100.0
    );
    assert_eq!(emu.host_audio().overflows(), 0, "samples dropped");
    assert_eq!(emu.host_audio().underruns(), 0, "samples missing");
}

/// The debugger's reads change nothing: every memory region the debugger
/// lists on the main CPU's bus, and the video chip's memories, read
/// leave the state where it was and the next 60 frames as they would be.
pub fn debug_reads_change_nothing<E: EmulatorCore>(m: &MachineUnderTest<E>) {
    let mut a = (m.build)();
    let mut b = (m.build)();
    run(m, &mut a, 0, 30);
    run(m, &mut b, 0, 30);
    for info in b.debugger_ref().get_memory_regions() {
        if matches!(
            info.region,
            MemoryRegion::MainRam | MemoryRegion::Rom | MemoryRegion::SaveRam
        ) {
            b.read_memory_debug(info.start_address, info.size);
        }
    }
    let video = b.video_debugger();
    video.get_vram();
    video.get_palette_data();
    video.get_sprite_data();
    assert_eq!(a.state_hash(), b.state_hash(), "after the reads");
    for frame in 30..90 {
        run(m, &mut a, frame, 1);
        run(m, &mut b, frame, 1);
        assert_eq!(a.state_hash(), b.state_hash(), "state at frame {frame}");
    }
}

/// A state opens on its own core and build only: one resealed for another
/// core, a newer layout or a newer envelope is refused with the machine as
/// it was; one made on another ROM is refused as such unless the host
/// chose any ROM; bytes that are no state are refused.
pub fn a_savestate_opens_only_on_its_core_and_build<E: EmulatorCore>(m: &MachineUnderTest<E>) {
    let mut a = (m.build)();
    run(m, &mut a, 0, 60);
    let state = a.save_state();
    let reseal = |change: &dyn Fn(&mut crate::savestate::SaveStateEnvelope<E::Savestate>)| {
        let mut envelope = deserialize_compressed::<E::Savestate>(&state).expect("the state opens");
        change(&mut envelope);
        serialize_compressed(&envelope, 3).expect("the state seals")
    };
    let mut emu = (m.build)();
    run(m, &mut emu, 0, 30);
    let before = emu.state_hash();
    let refused: [(&str, Vec<u8>); 5] = [
        ("another core", reseal(&|e| e.core_id = "other".to_string())),
        (
            "a newer layout",
            reseal(&|e| e.core_version = <E::Savestate as SavestatePayload>::FORMAT.version + 1),
        ),
        (
            "a newer envelope",
            reseal(&|e| {
                e.envelope_version = crate::savestate::RETRO_SAVESTATE_ENVELOPE_VERSION + 1
            }),
        ),
        ("bytes that are no state", vec![1, 2, 3]),
        ("no bytes", Vec::new()),
    ];
    for (what, bytes) in refused {
        let refusal = emu.load_state(&bytes, RomCheck::SameRom);
        assert!(
            matches!(refusal, Err(LoadStateError::Unreadable(_))),
            "{what}: {refusal:?}"
        );
        assert_eq!(emu.state_hash(), before, "{what}: the machine as it was");
    }
    emu.load_rom(&(m.other_rom)()).expect("the other ROM loads");
    let before = emu.state_hash();
    let refusal = emu.load_state(&state, RomCheck::SameRom);
    assert!(
        matches!(refusal, Err(LoadStateError::OtherRom { .. })),
        "another ROM's state: {refusal:?}"
    );
    assert_eq!(emu.state_hash(), before, "another ROM's state: as it was");
    emu.load_state(&state, RomCheck::AnyRom)
        .expect("any ROM takes it");
    assert_eq!(emu.frame_count(), a.frame_count());
}

/// Every button the host is told of is a button of the console's pad, its
/// id the bit the core reads it from.
pub fn the_button_layout_names_the_pads_bits<E: GoldenConsole>(m: &MachineUnderTest<E>) {
    let emu = (m.build)();
    let layout = &emu.system_info().input.button_layout;
    assert!(!layout.is_empty());
    for button in layout {
        let pad = E::BUTTONS
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(&button.name));
        assert_eq!(
            pad.map(|p| p.1),
            Some(button.id),
            "{}: the host's id against the pad's bit",
            button.name
        );
    }
}

/// The sound chips the debugger opens are there below their count and not
/// at it.
pub fn audio_debuggers_answer_their_count<E: EmulatorCore>(m: &MachineUnderTest<E>) {
    let mut emu = (m.build)();
    let count = emu.audio_chip_count();
    assert!(count > 0);
    for chip in 0..count {
        assert!(emu.audio_debugger(chip).is_some(), "chip {chip}");
    }
    assert!(emu.audio_debugger(count).is_none(), "past the count");
}
