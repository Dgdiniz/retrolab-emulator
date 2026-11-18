//! The golden-level savestate check: a console's golden record run as the
//! golden test runs it (the play tier, sound on, the manifest's presses and
//! resets, the real-time clock pinned), with saves taken along the way, each
//! loaded into a new machine that is held to the run's records: at frames'
//! ends, and at stops part of the way through a frame (at an event, then at
//! an execute breakpoint where the main CPU stands).

use super::{main_pc, remove_breakpoints};
use crate::debug::ExecutionTier;
use crate::frame::{run_frame_on, DebugMachine};
use crate::golden::{
    crop_framebuffer_rgb, entry_spec_of, GoldenConsole, GoldenEntry, GoldenSpec, GOLDEN_UNIX_TIME,
};
use crate::savestate::{RomCheck, SavestatePayload};
use crate::types::{Breakpoint, BreakpointType, ExecutionMode, InputState};
use crate::{fnv1a64, EmulatorCore};

/// What [`golden_saves`] takes along a record.
#[derive(Clone, Debug)]
pub struct GoldenSaves {
    /// Every this many frames, the state saved where the frame ends; 0 for
    /// none.
    pub every: u64,
    /// Every this many frames, the next frame stopped part of the way
    /// through and saved there: at each of `events`, then, from that stop,
    /// at an execute breakpoint where the main CPU stands; 0 for none.
    pub stop_every: u64,
    /// The events the stops wait for, as (code, parameter).
    pub events: Vec<(u32, u32)>,
    /// The frames each machine that loaded a save is held to the record.
    pub after: u64,
}

/// What [`golden_saves`] did on a record.
#[derive(Clone, Debug, Default)]
pub struct SavesReport {
    /// States saved where a frame ends.
    pub frame_end_saves: usize,
    /// Of those, the ones that carry state after the payload.
    pub with_extension: usize,
    /// States saved at a stop part of the way through a frame.
    pub stop_saves: usize,
    /// Stops that did not come in the frame they were asked for.
    pub no_stop: usize,
    /// The saves after whose load a machine parted from the record, each
    /// with what parted and where.
    pub parted: Vec<String>,
}

/// What a frame of the run left: the state hash and the picture's.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Frame {
    state: u64,
    picture: u64,
}

fn frame_of<E: EmulatorCore>(emu: &mut E) -> Frame {
    let display = emu.get_display_info();
    let framebuffer = emu.get_framebuffer();
    let (_, _, rgb) = crop_framebuffer_rgb(
        &framebuffer.data,
        framebuffer.width,
        framebuffer.height,
        display.width,
        display.height,
    );
    Frame {
        state: emu.state_hash(),
        picture: fnv1a64(&rgb),
    }
}

/// A new machine with the entry's ROM loaded and its sound on.
fn boot<E: GoldenConsole>(entry: &GoldenEntry, rom: &[u8]) -> Result<E, String> {
    let mut emu = E::new_for_golden();
    emu.load_for_golden(entry, rom)?;
    emu.set_audio_enabled(true);
    Ok(emu)
}

/// What the run does before frame `i`: the reset it presses, the buttons it
/// holds.
fn input_at<E: EmulatorCore>(spec: &GoldenSpec, emu: &mut E, i: u64) {
    if spec.resets.contains(&i) {
        emu.reset();
    }
    if let Some(schedule) = &spec.presses {
        if let Some(buttons) = schedule.mask_at(i) {
            emu.set_input(0, InputState { buttons });
        }
    }
}

/// The buttons the run holds in frame `i`: the last the schedule set at or
/// before it. A loaded state does not hold the host's input, so a machine
/// that loads one is given it.
fn held_at(spec: &GoldenSpec, i: u64) -> u32 {
    let Some(schedule) = &spec.presses else {
        return 0;
    };
    (0..=i)
        .filter_map(|j| schedule.mask_at(j))
        .next_back()
        .unwrap_or(0)
}

/// One frame on the play tier, the host's sound taken as the golden run
/// takes it.
fn play<E: DebugMachine>(emu: &mut E) {
    run_frame_on(emu, ExecutionTier::Play);
    emu.get_audio_samples();
}

/// Run `emu`, the record's frame `from` its next, to frame `to`, holding it
/// to the record's frames; `picture_from` is the first frame whose picture
/// is compared. Returns what parted first, and where.
fn held_to_record<E: GoldenConsole + DebugMachine>(
    emu: &mut E,
    spec: &GoldenSpec,
    record: &[Frame],
    from: u64,
    to: u64,
    picture_from: u64,
) -> Option<String> {
    for i in from..to {
        if i > from || !emu.frame_in_progress() {
            input_at(spec, emu, i);
        }
        play(emu);
        let got = frame_of(emu);
        let want = record[i as usize];
        if got.state != want.state {
            return Some(format!("the state after frame {}", i + 1));
        }
        if i >= picture_from && got.picture != want.picture {
            return Some(format!("the picture after frame {}", i + 1));
        }
    }
    None
}

/// The machine stopped part of the way through the record's frame `at`
/// (the run's own count: a reset the run presses starts the machine's
/// over), saved there, the state loaded into a new machine, which goes on
/// as the record does: the state at once and after every frame, the
/// picture from the first frame drawn whole.
#[allow(clippy::too_many_arguments)]
fn check_the_stop<E: GoldenConsole + DebugMachine>(
    stopped: &mut E,
    at: u64,
    entry: &GoldenEntry,
    rom: &[u8],
    spec: &GoldenSpec,
    record: &[Frame],
    after: u64,
    what: &str,
    report: &mut SavesReport,
) -> Result<(), String> {
    let before = (
        stopped.state_hash(),
        stopped.frame_count(),
        main_pc(stopped),
    );
    let state = stopped.save_state();
    report.stop_saves += 1;
    if (
        stopped.state_hash(),
        stopped.frame_count(),
        main_pc(stopped),
    ) != before
    {
        report.parted.push(format!(
            "{what} in frame {}: the save moved the machine",
            at + 1
        ));
        return Ok(());
    }
    let mut loaded = boot::<E>(entry, rom)?;
    loaded
        .load_state(&state, RomCheck::SameRom)
        .map_err(|e| e.to_string())?;
    loaded.set_input(
        0,
        InputState {
            buttons: held_at(spec, at),
        },
    );
    if loaded.state_hash() != before.0 {
        report
            .parted
            .push(format!("{what} in frame {}: the state loaded", at + 1));
        return Ok(());
    }
    let to = (at + 1 + after).min(record.len() as u64);
    if let Some(parted) = held_to_record(&mut loaded, spec, record, at, to, at + 1) {
        report
            .parted
            .push(format!("{what} in frame {}: {parted}", at + 1));
    }
    Ok(())
}

/// Run the next frames on the light tier, at most three, until a stop;
/// whether one came. `frame` is the record's frame the machine is in, or
/// the next one when none is in progress, and follows the frames that end.
fn run_to_the_stop<E: GoldenConsole + DebugMachine>(
    emu: &mut E,
    spec: &GoldenSpec,
    frame: &mut u64,
) -> bool {
    for _ in 0..3 {
        emu.set_execution_mode(ExecutionMode::Running);
        if !emu.frame_in_progress() {
            input_at(spec, emu, *frame);
        }
        run_frame_on(emu, ExecutionTier::LightDebug);
        emu.get_audio_samples();
        if emu.execution_mode() == ExecutionMode::Paused && emu.frame_in_progress() {
            return true;
        }
        if !emu.frame_in_progress() {
            *frame += 1;
        }
        if emu.execution_mode() == ExecutionMode::Paused {
            return true;
        }
    }
    false
}

/// The golden-level savestate check on one record of a console's golden
/// manifest (`rom` its ROM): the run, then the saves of `plan`, each loaded
/// into a new machine held to the run. An error is a ROM or a state that
/// does not load; a machine that parts from the run is in the report.
pub fn golden_saves<E: GoldenConsole + DebugMachine>(
    entry: &GoldenEntry,
    rom: &[u8],
    plan: &GoldenSaves,
) -> Result<SavesReport, String> {
    let previous_time = E::pin_real_time_clock(Some(GOLDEN_UNIX_TIME));
    let result = check_the_record::<E>(entry, rom, plan);
    E::pin_real_time_clock(previous_time);
    result
}

fn check_the_record<E: GoldenConsole + DebugMachine>(
    entry: &GoldenEntry,
    rom: &[u8],
    plan: &GoldenSaves,
) -> Result<SavesReport, String> {
    let spec = entry_spec_of::<E>(entry)?;
    let sampled = |at: u64, every: u64| every > 0 && at.is_multiple_of(every);

    // The run, its frames and the states saved where the sampled frames end.
    let mut emu = boot::<E>(entry, rom)?;
    let mut record = Vec::with_capacity(spec.frames as usize);
    let mut saves = Vec::new();
    for i in 0..spec.frames {
        input_at(&spec, &mut emu, i);
        play(&mut emu);
        record.push(frame_of(&mut emu));
        let at = i + 1;
        if (sampled(at, plan.every) || sampled(at, plan.stop_every))
            && at + 1 + plan.after <= spec.frames
        {
            saves.push((at, emu.save_state()));
        }
    }

    let mut report = SavesReport::default();
    for (at, state) in &saves {
        let at = *at;
        if sampled(at, plan.every) {
            report.frame_end_saves += 1;
            let (_, extension) =
                E::Savestate::open(state, None, RomCheck::AnyRom).map_err(|e| e.to_string())?;
            if extension != Default::default() {
                report.with_extension += 1;
            }
            let mut loaded = boot::<E>(entry, rom)?;
            loaded
                .load_state(state, RomCheck::SameRom)
                .map_err(|e| e.to_string())?;
            loaded.set_input(
                0,
                InputState {
                    buttons: held_at(&spec, at.saturating_sub(1)),
                },
            );
            if let Some(parted) =
                held_to_record(&mut loaded, &spec, &record, at, at + plan.after, at)
            {
                report
                    .parted
                    .push(format!("the save where frame {at} ends: {parted}"));
            }
        }
        if !sampled(at, plan.stop_every) {
            continue;
        }
        for &(code, param) in &plan.events {
            // The machine at the frame's end again, then stopped at the
            // event in the next frame.
            let mut stopped = boot::<E>(entry, rom)?;
            stopped
                .load_state(state, RomCheck::SameRom)
                .map_err(|e| e.to_string())?;
            stopped.set_input(
                0,
                InputState {
                    buttons: held_at(&spec, at.saturating_sub(1)),
                },
            );
            stopped.debugger().add_breakpoint(Breakpoint {
                address: 0,
                bp_type: BreakpointType::Event { code, param },
                cpu_id: None,
                condition: None,
            });
            let mut frame = at;
            if !run_to_the_stop(&mut stopped, &spec, &mut frame) {
                report.no_stop += 1;
                continue;
            }
            let what = format!("the stop at event {code}:{param}");
            check_the_stop(
                &mut stopped,
                frame,
                entry,
                rom,
                &spec,
                &record,
                plan.after,
                &what,
                &mut report,
            )?;
            // From there, a stop before the instruction the main CPU
            // stands at.
            remove_breakpoints(&mut stopped);
            let pc = main_pc(&stopped);
            stopped.debugger().add_breakpoint(Breakpoint {
                address: pc,
                bp_type: BreakpointType::execute(),
                cpu_id: Some(0),
                condition: None,
            });
            if !run_to_the_stop(&mut stopped, &spec, &mut frame) {
                report.no_stop += 1;
                continue;
            }
            let what = format!("the stop at {pc:06X} after event {code}:{param}");
            check_the_stop(
                &mut stopped,
                frame,
                entry,
                rom,
                &spec,
                &record,
                plan.after,
                &what,
                &mut report,
            )?;
        }
    }
    Ok(report)
}
