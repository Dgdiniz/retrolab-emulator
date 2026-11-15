//! The capture contract's conformance suite: checks a core's
//! [`CaptureCore`] implementation on a program of the core's own, the way
//! [`crate::golden`] is a harness both cores run.
//!
//! A core describes itself in a [`CoreUnderTest`]: how to build its emulator
//! with its program loaded, and the hooks no trait gives (its state hash,
//! its frame count, its sprite order, its debugger's writes into video
//! memory, and where its program can be stopped). Its test file then calls
//! [`capture_conformance_tests!`](crate::capture_conformance_tests), which
//! writes one test per check, so each check passes or fails on its own; each
//! check is also a public function of this module.
//!
//! The program must draw a background and sprites, move the picture every
//! frame, write video memory every frame both by transfer and through the
//! ports, and show its sprites in some frames and none in others. The first
//! check holds it to that, so that no other check passes for want of
//! anything to see.
//!
//! A check that compares two machines runs both on the same frame loops in
//! the same order: a core's loops may part by a sample of sound over a long
//! run, which is no concern of the capture contract.

use std::collections::BTreeSet;

use crate::capture::{
    drawn_by, layer, line_flags, planes, CaptureCore, CaptureRecording, FrameWrite, LineSprites,
    WriteKind, WriteOrigin, Writer, LINE_SPRITES_STRIDE, NO_PALETTE,
};
use crate::types::{Breakpoint, BreakpointId, BreakpointType, ExecutionMode};
use crate::video::{PixelImage, SpriteOrderEntry};

/// Every recorder of [`CaptureRecording`] armed.
pub const EVERY_RECORDER: CaptureRecording = CaptureRecording {
    line_states: true,
    pixel_sources: true,
    line_sprites: true,
    video_memory_sources: true,
    frame_writes: true,
};

/// Frames a check runs before it looks, so that the program has set itself
/// up.
const WARM_UP: u64 = 4;

/// Frames a check that looks at a run of frames runs.
const FRAMES: u64 = 8;

/// Layer ids past which [`CaptureCore::render_layer`] must have stopped
/// answering: no core's layer list is this long.
const MOST_LAYERS: u32 = 16;

/// What the suite needs of a core: its emulator with a program of its own,
/// and what no trait gives yet.
pub struct CoreUnderTest<E> {
    /// A new emulator with the program loaded, ready to run frames: its
    /// debugger running, its audio on.
    pub build: fn() -> E,
    /// The program's ROM, for a check that loads it again.
    pub rom: Vec<u8>,
    /// One whole frame on the loop a player's host calls, the loop
    /// [`CaptureCore::run_to_capture_boundary`] runs while nothing of the
    /// debugger is armed.
    pub run_frame: fn(&mut E),
    /// The core's hash of everything its savestate holds.
    pub state_hash: fn(&E) -> u64,
    /// The frames the core has finished.
    pub frame_count: fn(&E) -> u64,
    /// The sprites the held frame drew, back to front
    /// ([`crate::video::VideoDebugger::resolved_sprite_render_order`]).
    pub sprite_order: fn(&mut E) -> Vec<SpriteOrderEntry>,
    /// The id of the sprites in the core's layer list.
    pub sprite_layer: u32,
    /// An address the main processor (CPU 0) executes in every frame after
    /// the frame's capture boundary: the program's vertical blank handler.
    pub rest_of_frame: u32,
    /// An event breakpoint that stops a frame part of the way through its
    /// picture, before its capture boundary.
    pub stop_in_picture: Breakpoint,
    /// An event breakpoint that stops a frame in its vertical blank, after
    /// its capture boundary and after the program's vertical blank handler
    /// ran.
    pub stop_in_vertical_blank: Breakpoint,
    /// Writes the core's debugger makes into video memory, one for each
    /// memory it writes (tiles and maps, the palette, the scroll or sprite
    /// memory).
    pub video_memory_edits: Vec<VideoMemoryEdit<E>>,
}

/// A write the core's debugger makes into one of its video memories, as a
/// host's editor makes it while a frame is held: one that changes what a
/// frame showing sprites shows.
pub struct VideoMemoryEdit<E> {
    /// The memory written, as a failure names it.
    pub memory: &'static str,
    /// Make the write.
    pub write: fn(&mut E),
}

/// Write one test per check of the capture conformance suite
/// ([`crate::capture::conformance`]) in a core's test file, each running its
/// check on the [`CoreUnderTest`] the expression gives:
///
/// ```text
/// fn my_core() -> CoreUnderTest<MyEmulator> { ... }
///
/// retro_core::capture_conformance_tests!(my_core());
/// ```
///
/// The core's test crate enables `retro-core`'s `capture-conformance`
/// feature, among its dev-dependencies.
#[macro_export]
macro_rules! capture_conformance_tests {
    ($core:expr) => {
        $crate::capture_conformance_tests!(@each $core;
            the_program_draws_and_writes_video_memory_every_frame,
            each_boundary_call_runs_one_frame,
            a_save_at_the_boundary_finishes_the_frame,
            a_save_at_the_boundary_finishes_the_frame_past_a_breakpoint,
            a_reset_a_state_load_and_a_rom_load_drop_the_hold,
            run_until_vblank_stops_at_the_boundary,
            drawing_the_frame_again_moves_nothing,
            recording_changes_nothing,
            the_recorders_start_empty_and_a_reset_or_a_load_empties_them,
            the_sprite_order_names_the_sprites_that_drew,
            a_debugger_edit_makes_the_line_table_inexact_until_the_next_frame,
            the_picture_from_the_line_table_needs_the_whole_frame,
            the_frame_number_names_the_picture_the_host_holds,
        );
    };
    (@each $core:expr; $($check:ident),+ $(,)?) => {
        $(
            #[test]
            fn $check() {
                $crate::capture::conformance::$check(&$core);
            }
        )+
    };
}

// -- The checks ----------------------------------------------------------

/// The program does what the other checks need: every frame it writes video
/// memory by transfer and through the ports, draws a background, and moves
/// the picture, and it shows sprites in some frames and none in others.
pub fn the_program_draws_and_writes_video_memory_every_frame<E: CaptureCore>(
    core: &CoreUnderTest<E>,
) {
    let mut emu = core.warmed_up(EVERY_RECORDER);
    emu.run_to_capture_boundary();
    let _ = emu.take_frame_writes();
    let sprite = drawn_by(core.sprite_layer as u16);
    let mut previous: Option<Picture> = None;
    let (mut with_sprites, mut without_sprites) = (0, 0);
    for _ in 0..FRAMES {
        emu.run_to_capture_boundary();
        let frame = emu.capture_frame_snapshot().frame;
        let writes: Vec<FrameWrite> = emu
            .take_frame_writes()
            .expect("frame writes are recorded")
            .iter()
            .collect();
        assert!(
            writes
                .iter()
                .any(|write| write.kind == WriteKind::Transfer as u8),
            "frame {frame}: the program wrote no video memory by transfer ({} records taken)",
            writes.len()
        );
        assert!(
            writes
                .iter()
                .any(|write| write.kind == WriteKind::PaletteStore as u8
                    && write.writer == Writer::Processor as u8),
            "frame {frame}: the processor stored no colour through the palette port ({} records \
             taken)",
            writes.len()
        );
        let sources = emu
            .video_memory_sources()
            .expect("video memory sources are recorded");
        for writer in [Writer::Transfer, Writer::Processor] {
            let lately =
                sources
                    .origins()
                    .iter()
                    .zip(sources.frames())
                    .any(|(&origin, &written_in)| {
                        WriteOrigin(origin).writer() == writer as u32
                            && u64::from(written_in) + 1 >= frame
                    });
            assert!(
                lately,
                "frame {frame}: no byte of video memory was written by {writer:?} in this frame \
                 or the one before"
            );
        }
        let traced = emu
            .pixel_sources(planes::LAYER)
            .expect("a program is loaded");
        let drew = traced.layer.expect("the layer plane was asked for");
        let what = |value: u16| value & layer::WHAT_DREW;
        let not_a_layer = [
            layer::BACKDROP,
            layer::UNLAYERED_COLOUR,
            layer::NOTHING,
            sprite,
        ];
        assert!(
            drew.iter()
                .any(|&value| !not_a_layer.contains(&what(value))),
            "frame {frame}: no pixel of the picture came from a background layer"
        );
        if drew.iter().any(|&value| what(value) == sprite) {
            with_sprites += 1;
        } else {
            without_sprites += 1;
        }
        let picture = Picture::shown(&emu);
        if let Some(previous) = &previous {
            assert!(
                *previous != picture,
                "frame {frame}: the picture is the frame before's, nothing moved"
            );
        }
        previous = Some(picture);
    }
    assert!(
        with_sprites > 0 && without_sprites > 0,
        "the program showed sprites in {with_sprites} of {FRAMES} frames: it must show them in \
         some and none in others"
    );
}

/// Each [`CaptureCore::run_to_capture_boundary`] call runs one frame and
/// holds it at its boundary, numbered one past the frames finished, with
/// the picture the same frame shows once run whole; finished, the frames
/// leave the machine and the sound of frames run whole.
pub fn each_boundary_call_runs_one_frame<E: CaptureCore>(core: &CoreUnderTest<E>) {
    let mut held = core.warmed_up(CaptureRecording::default());
    let mut whole = core.warmed_up(CaptureRecording::default());
    take_audio(&mut held);
    take_audio(&mut whole);
    let first = core.frames(&held) + 1;
    let (mut held_audio, mut whole_audio) = (Vec::new(), Vec::new());
    for call in 0..FRAMES {
        held.run_to_capture_boundary();
        assert!(
            held.at_capture_boundary(),
            "call {call}: not held at a capture boundary after the call"
        );
        let snapshot = held.capture_frame_snapshot();
        assert!(
            snapshot.at_boundary,
            "call {call}: the snapshot does not say it was taken at the boundary"
        );
        assert_eq!(
            snapshot.frame,
            first + call,
            "call {call}: the snapshot's frame number does not rise by one each call"
        );
        assert_eq!(
            snapshot.frame,
            core.frames(&held) + 1,
            "call {call}: the held frame must be counted only once it ends"
        );
        held_audio.extend(take_audio(&mut held));

        (core.run_frame)(&mut whole);
        whole_audio.extend(take_audio(&mut whole));
        let after = whole.capture_frame_snapshot();
        assert_eq!(
            after.frame, snapshot.frame,
            "frame {}: a snapshot of the frame run whole carries another number",
            snapshot.frame
        );
        let (at_boundary, run_whole) = (
            Picture::of_snapshot(&snapshot),
            Picture::of_snapshot(&after),
        );
        assert!(
            at_boundary == run_whole,
            "frame {}: the picture at the boundary is not the picture of the frame run whole: {}",
            snapshot.frame,
            at_boundary.difference(&run_whole)
        );
    }
    (core.run_frame)(&mut held);
    assert!(
        !held.at_capture_boundary(),
        "a frame call left the held frame held"
    );
    held_audio.extend(take_audio(&mut held));
    assert_eq!(
        core.frames(&held),
        core.frames(&whole),
        "the frames split at their boundaries were not all counted"
    );
    assert_eq!(
        core.hash(&held),
        core.hash(&whole),
        "frames split at their boundaries end in another state than frames run whole"
    );
    assert!(
        held_audio == whole_audio,
        "frames split at their boundaries sound otherwise than frames run whole: {}",
        sound_difference(&held_audio, &whole_audio)
    );
}

/// [`CaptureCore::save_state_at_frame_end`] at the boundary finishes the
/// frame first, with the debugger running or paused: the core is held no
/// more, and its state and its savestate are those of a twin that ran the
/// same frames whole. Where no frame is held it runs nothing.
pub fn a_save_at_the_boundary_finishes_the_frame<E: CaptureCore>(core: &CoreUnderTest<E>) {
    for paused in [false, true] {
        let mode = if paused { "paused" } else { "running" };
        let mut held = core.warmed_up(CaptureRecording::default());
        let mut whole = core.warmed_up(CaptureRecording::default());
        (core.run_frame)(&mut whole);
        held.run_to_capture_boundary();
        assert!(held.at_capture_boundary(), "{mode}: no frame held to save");
        if paused {
            held.debugger().set_execution_mode(ExecutionMode::Paused);
        }
        let saved = held.save_state_at_frame_end();
        core.holds_the_frame_run_whole(&held, &whole, &saved, &format!("debugger {mode}"));
    }

    let mut emu = core.warmed_up(CaptureRecording::default());
    let (frames, hash) = (core.frames(&emu), core.hash(&emu));
    assert!(
        !emu.save_state_at_frame_end().is_empty(),
        "with no frame held, the save gave no bytes"
    );
    assert_eq!(
        core.frames(&emu),
        frames,
        "with no frame held, the save ran a frame"
    );
    assert_eq!(
        core.hash(&emu),
        hash,
        "with no frame held, the save moved the state"
    );
}

/// The save at the boundary finishes the frame on the loop no breakpoint
/// can stop: an execute breakpoint in the rest of the frame (which a frame
/// call from the boundary stops at) neither stops the finish nor pauses the
/// debugger, and the savestate holds the frame's end.
pub fn a_save_at_the_boundary_finishes_the_frame_past_a_breakpoint<E: CaptureCore>(
    core: &CoreUnderTest<E>,
) {
    let breakpoint = Breakpoint {
        address: core.rest_of_frame,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    };
    let mut probe = core.warmed_up(CaptureRecording::default());
    probe.run_to_capture_boundary();
    probe.debugger().add_breakpoint(breakpoint.clone());
    probe.run_frame();
    assert_eq!(
        probe.debugger().get_execution_mode(),
        ExecutionMode::Paused,
        "no frame call from the boundary stopped at {:#X}: the address is not run in the rest \
         of the frame",
        core.rest_of_frame
    );

    let mut held = core.warmed_up(CaptureRecording::default());
    let mut whole = core.warmed_up(CaptureRecording::default());
    (core.run_frame)(&mut whole);
    held.run_to_capture_boundary();
    assert!(held.at_capture_boundary(), "no frame held to save");
    held.debugger().add_breakpoint(breakpoint);
    let saved = held.save_state_at_frame_end();
    assert_eq!(
        held.debugger().get_execution_mode(),
        ExecutionMode::Running,
        "the execute breakpoint at {:#X}, in the rest of the frame, stopped the save's finish",
        core.rest_of_frame
    );
    core.holds_the_frame_run_whole(
        &held,
        &whole,
        &saved,
        &format!("execute breakpoint at {:#X}", core.rest_of_frame),
    );
}

/// A reset, a savestate load and a ROM load each drop the frame held at its
/// boundary; after the load the machine runs on as one that loaded the same
/// savestate.
pub fn a_reset_a_state_load_and_a_rom_load_drop_the_hold<E: CaptureCore>(core: &CoreUnderTest<E>) {
    let mut emu = core.warmed_up(CaptureRecording::default());
    let saved = emu.save_state_at_frame_end();

    emu.run_to_capture_boundary();
    assert!(emu.at_capture_boundary(), "no frame held before the reset");
    emu.reset();
    assert!(
        !emu.at_capture_boundary(),
        "a reset kept the frame held at its boundary"
    );

    run_debugger(&mut emu);
    emu.run_to_capture_boundary();
    assert!(
        emu.at_capture_boundary(),
        "no frame held before the state load"
    );
    emu.load_state(&saved)
        .expect("the program's own savestate loads");
    assert!(
        !emu.at_capture_boundary(),
        "a savestate load kept the frame held at its boundary"
    );
    let mut loaded = (core.build)();
    loaded
        .load_state(&saved)
        .expect("the program's own savestate loads");
    for machine in [&mut emu, &mut loaded] {
        run_debugger(machine);
        (core.run_frame)(machine);
    }
    assert_eq!(
        core.hash(&emu),
        core.hash(&loaded),
        "after a load dropped the held frame, the next frame ends otherwise than on a machine \
         that loaded the same savestate"
    );

    emu.run_to_capture_boundary();
    assert!(
        emu.at_capture_boundary(),
        "no frame held before the ROM load"
    );
    emu.load_rom(&core.rom).expect("the program loads again");
    assert!(
        !emu.at_capture_boundary(),
        "a ROM load kept the frame held at its boundary"
    );
}

/// [`crate::EmulatorCore::run_until_vblank`] stops at the capture boundary,
/// one frame a call, and a frame call then finishes the held frame as a
/// whole frame ends.
pub fn run_until_vblank_stops_at_the_boundary<E: CaptureCore>(core: &CoreUnderTest<E>) {
    let mut emu = core.warmed_up(CaptureRecording::default());
    let mut whole = core.warmed_up(CaptureRecording::default());
    let frames = core.frames(&emu);
    for call in 0..2 {
        emu.run_until_vblank();
        assert!(
            emu.at_capture_boundary(),
            "call {call}: run_until_vblank did not stop at the capture boundary"
        );
        assert_eq!(
            core.frames(&emu),
            frames + call,
            "call {call}: run_until_vblank did not run one frame a call"
        );
        assert_eq!(
            emu.capture_frame_snapshot().frame,
            frames + call + 1,
            "call {call}: the held frame's number"
        );
    }
    emu.run_frame();
    assert!(
        !emu.at_capture_boundary(),
        "a frame call after run_until_vblank left the frame held"
    );
    whole.run_frame();
    whole.run_frame();
    assert_eq!(core.frames(&emu), core.frames(&whole));
    assert_eq!(
        core.hash(&emu),
        core.hash(&whole),
        "frames stopped by run_until_vblank end in another state than frames run whole"
    );
}

/// A repaint, every layer render, the picture from the line table, the
/// pixel sources, the line sprites, the chosen sprites drawn and the sprite
/// order leave the state hash where it was, at the boundary with every
/// recorder armed or none, after a whole frame, and at an event stop part
/// of the way through the picture or in the vertical blank. Only the
/// repaint draws into the host's framebuffer, and drawn every line from a
/// line table whose memory is exact it draws the picture the frame drew.
/// The machine then runs on as a twin whose frame nothing drew again.
pub fn drawing_the_frame_again_moves_nothing<E: CaptureCore>(core: &CoreUnderTest<E>) {
    core.draws_again_moving_nothing(
        EVERY_RECORDER,
        Place::AtBoundary,
        "at the capture boundary, every recorder armed",
    );
    core.draws_again_moving_nothing(
        EVERY_RECORDER,
        Place::Finished,
        "after a whole frame, every recorder armed",
    );
    core.draws_again_moving_nothing(
        CaptureRecording::default(),
        Place::AtBoundary,
        "at the capture boundary, nothing recorded",
    );
    core.draws_again_moving_nothing(
        EVERY_RECORDER,
        Place::InPicture,
        "stopped part of the way through the picture, every recorder armed",
    );
    core.draws_again_moving_nothing(
        EVERY_RECORDER,
        Place::InVerticalBlank,
        "stopped in the vertical blank, every recorder armed",
    );
}

/// Recording changes nothing: a twin with every recorder armed (its frame
/// writes taken every frame, as a host takes them) has the same state, the
/// same picture and the same sound every frame as one with none, frames
/// split at their boundaries or run whole.
pub fn recording_changes_nothing<E: CaptureCore>(core: &CoreUnderTest<E>) {
    for split in [false, true] {
        let how = if split {
            "split at the boundary"
        } else {
            "run whole"
        };
        let mut plain = (core.build)();
        let mut recorded = (core.build)();
        recorded.set_capture_recording(EVERY_RECORDER);
        let mut writes_taken = 0;
        for _ in 0..WARM_UP + FRAMES {
            if split {
                plain.run_to_capture_boundary();
                recorded.run_to_capture_boundary();
                let frame = plain.capture_frame_snapshot().frame;
                assert_eq!(
                    core.hash(&recorded),
                    core.hash(&plain),
                    "frame {frame}, at its boundary: recording moved the state"
                );
                let pictures = (Picture::shown(&recorded), Picture::shown(&plain));
                assert!(
                    pictures.0 == pictures.1,
                    "frame {frame}, at its boundary: recording changed the picture: {}",
                    pictures.0.difference(&pictures.1)
                );
            }
            (core.run_frame)(&mut plain);
            (core.run_frame)(&mut recorded);
            writes_taken += recorded
                .take_frame_writes()
                .expect("frame writes are recorded")
                .records
                .len();
            let frame = core.frames(&plain);
            assert_eq!(
                core.hash(&recorded),
                core.hash(&plain),
                "frame {frame}, {how}: recording moved the state"
            );
            let pictures = (Picture::shown(&recorded), Picture::shown(&plain));
            assert!(
                pictures.0 == pictures.1,
                "frame {frame}, {how}: recording changed the picture: {}",
                pictures.0.difference(&pictures.1)
            );
            let sounds = (take_audio(&mut recorded), take_audio(&mut plain));
            assert!(
                sounds.0 == sounds.1,
                "frame {frame}, {how}: recording changed the sound: {}",
                sound_difference(&sounds.0, &sounds.1)
            );
        }
        assert!(
            writes_taken > 0 && table_and_sources_hold_something(&recorded),
            "{how}: the recorders armed for the whole run hold nothing"
        );
    }
}

/// A recorder turned on starts empty, one asked for again keeps what it
/// holds, and a reset, a savestate load and a ROM load empty them and keep
/// the setting; turned off, the recorders and the readers of the line table
/// answer nothing.
pub fn the_recorders_start_empty_and_a_reset_or_a_load_empties_them<E: CaptureCore>(
    core: &CoreUnderTest<E>,
) {
    let mut emu = core.warmed_up(CaptureRecording::default());
    let saved = emu.save_state_at_frame_end();
    emu.set_capture_recording(EVERY_RECORDER);
    assert_eq!(
        emu.capture_recording(),
        EVERY_RECORDER,
        "the core does not record what it was asked to"
    );
    assert_recorders_empty(&mut emu, "turned on after frames ran");
    core.fill_recorders(&mut emu, "turned on");
    emu.set_capture_recording(EVERY_RECORDER);
    assert!(
        table_and_sources_hold_something(&emu),
        "asked again for the recorders already on, the core emptied the line table or the \
         video memory sources"
    );
    assert!(
        !emu.take_frame_writes()
            .expect("frame writes are recorded")
            .records
            .is_empty(),
        "asked again for the recorders already on, the core emptied the frame writes"
    );

    core.fill_recorders(&mut emu, "before the reset");
    emu.reset();
    assert_eq!(
        emu.capture_recording(),
        EVERY_RECORDER,
        "a reset changed the recording"
    );
    assert_recorders_empty(&mut emu, "after a reset");

    run_debugger(&mut emu);
    core.fill_recorders(&mut emu, "before the state load");
    emu.load_state(&saved)
        .expect("the program's own savestate loads");
    assert_eq!(
        emu.capture_recording(),
        EVERY_RECORDER,
        "a savestate load changed the recording"
    );
    assert_recorders_empty(&mut emu, "after a savestate load");

    run_debugger(&mut emu);
    core.fill_recorders(&mut emu, "before the ROM load");
    emu.load_rom(&core.rom).expect("the program loads again");
    assert_eq!(
        emu.capture_recording(),
        EVERY_RECORDER,
        "a ROM load changed the recording"
    );
    assert_recorders_empty(&mut emu, "after a ROM load");

    run_debugger(&mut emu);
    core.fill_recorders(&mut emu, "before turning them off");
    emu.set_capture_recording(CaptureRecording::default());
    assert_eq!(emu.capture_recording(), CaptureRecording::default());
    assert!(
        emu.video_memory_sources().is_none(),
        "turned off, the video memory sources still answer"
    );
    assert!(
        emu.take_frame_writes().is_none(),
        "turned off, the frame writes still answer"
    );
    assert!(
        emu.line_states().is_none(),
        "turned off, the line table still answers"
    );
    assert!(
        emu.line_palettes().is_none(),
        "turned off, the line palettes still answer"
    );
    assert!(
        emu.picture_from_line_states().is_none(),
        "turned off, the picture from the line table still answers"
    );
}

/// The sprite order names the sprites that put at least one pixel on the
/// frame's sprite layer, each once: exactly the sprites the line sprites
/// say a line showing sprites drew a column of, so a frame that drew none
/// gives an empty list; and every pixel the pixel sources put down to the
/// sprites comes from one of them. The two readers agree with the line
/// table kept and without it.
pub fn the_sprite_order_names_the_sprites_that_drew<E: CaptureCore>(core: &CoreUnderTest<E>) {
    let mut emu = core.warmed_up(EVERY_RECORDER);
    let (mut with_sprites, mut without_sprites) = (0, 0);
    for _ in 0..FRAMES {
        emu.run_to_capture_boundary();
        if core.order_names_what_drew(&mut emu, "with the line table kept") {
            with_sprites += 1;
        } else {
            without_sprites += 1;
        }
    }
    assert!(
        with_sprites > 0 && without_sprites > 0,
        "the program drew sprites in {with_sprites} of {FRAMES} frames: the check needs frames \
         with sprites and frames without"
    );
    emu.set_capture_recording(CaptureRecording::default());
    for _ in 0..4 {
        emu.run_to_capture_boundary();
        core.order_names_what_drew(&mut emu, "with no line table");
    }
}

/// A debugger write into video memory makes the line table's
/// [`crate::video::LineStateTable::memory_exact`] false at once, while the
/// table still holds the whole frame; a repaint and the picture from the
/// line table both show the edit, alike; the next frame's table is exact
/// again.
pub fn a_debugger_edit_makes_the_line_table_inexact_until_the_next_frame<E: CaptureCore>(
    core: &CoreUnderTest<E>,
) {
    assert!(
        !core.video_memory_edits.is_empty(),
        "the core names no debugger write into video memory"
    );
    for VideoMemoryEdit { memory, write } in &core.video_memory_edits {
        let mut emu = core.warmed_up(EVERY_RECORDER);
        core.hold_a_frame_with_sprites(&mut emu);
        let table = emu.line_states().expect("the line table is kept");
        assert!(
            table.from_first_line && table.complete && table.memory_exact,
            "{memory}: before the edit, the table of the held frame is not whole and exact \
             (from its first line {}, complete {}, memory exact {})",
            table.from_first_line,
            table.complete,
            table.memory_exact
        );
        let before = Picture::shown(&emu);

        write(&mut emu);
        let table = emu.line_states().expect("the line table is kept");
        assert!(
            !table.memory_exact,
            "{memory}: after a debugger write, the line table still says its memory is exact"
        );
        assert!(
            table.from_first_line && table.complete,
            "{memory}: a debugger write took lines out of the table"
        );
        let redrawn = emu.picture_from_line_states().unwrap_or_else(|| {
            panic!(
                "{memory}: after the write the table still holds the whole frame, and the \
                 picture from it must answer"
            )
        });
        let redrawn = Picture::of_image(&redrawn);
        assert!(
            redrawn != before,
            "{memory}: the write does not show in the picture drawn again from the line table"
        );
        emu.repaint_held_frame();
        let repainted = Picture::shown(&emu);
        assert!(
            repainted != before,
            "{memory}: the write does not show in a repaint"
        );
        assert!(
            repainted == redrawn,
            "{memory}: the repaint and the picture drawn again from the line table differ: {}",
            repainted.difference(&redrawn)
        );

        emu.run_to_capture_boundary();
        let table = emu.line_states().expect("the line table is kept");
        assert!(
            table.from_first_line && table.complete && table.memory_exact,
            "{memory}: the next frame's table is not whole and exact (from its first line {}, \
             complete {}, memory exact {})",
            table.from_first_line,
            table.complete,
            table.memory_exact
        );
    }
}

/// The picture from the line table answers only while the table holds the
/// frame from its first line to its finished picture, where it is the
/// frame's own picture; stopped part of the way through the picture, armed
/// part of the way through a frame, or with no table, it answers `None`,
/// while a layer, the pixel sources and the line sprites still answer and
/// say whether every line was drawn from the table.
pub fn the_picture_from_the_line_table_needs_the_whole_frame<E: CaptureCore>(
    core: &CoreUnderTest<E>,
) {
    let mut emu = core.warmed_up(EVERY_RECORDER);
    let layers = layer_count(&mut emu);

    emu.run_to_capture_boundary();
    let situation = "held at the boundary of a frame the table holds whole";
    core.picture_from_the_table_is_the_frame(&mut emu, situation);
    readers_answer(&mut emu, layers, Some(true), situation);

    (core.run_frame)(&mut emu);
    core.picture_from_the_table_is_the_frame(&mut emu, "after the held frame ran to its end");

    let stop = core.stop(&mut emu, &core.stop_in_picture, "in the picture");
    let situation = "stopped part of the way through the picture";
    assert!(
        emu.picture_from_line_states().is_none(),
        "{situation}: the table does not hold the finished picture, yet the picture from it \
         answered"
    );
    readers_answer(&mut emu, layers, None, situation);
    remove_and_run(&mut emu, stop);

    let stop = core.stop(
        &mut emu,
        &core.stop_in_vertical_blank,
        "in the vertical blank",
    );
    let redrawn = emu
        .picture_from_line_states()
        .map(|image| Picture::of_image(&image));
    let exact = emu.line_states().is_some_and(|table| table.memory_exact);
    remove_and_run(&mut emu, stop);
    emu.run_frame();
    let finished = Picture::shown(&emu);
    let redrawn = redrawn.expect(
        "stopped in the vertical blank, past the frame's boundary: the table holds the frame from \
         its first line to its finished picture, and the picture from it must answer",
    );
    if exact {
        assert!(
            redrawn == finished,
            "stopped in the vertical blank: the picture from the line table is not the picture \
             the frame finished with: {}",
            redrawn.difference(&finished)
        );
    }

    let mut emu = core.warmed_up(CaptureRecording::default());
    let stop = core.stop(&mut emu, &core.stop_in_picture, "in the picture");
    emu.set_capture_recording(EVERY_RECORDER);
    remove_and_run(&mut emu, stop);
    emu.run_to_capture_boundary();
    let situation =
        "held at the boundary of a frame whose table was armed part of the way through it";
    assert!(emu.at_capture_boundary(), "{situation}: not held");
    let table = emu.line_states().expect("the line table is kept");
    assert!(
        !table.from_first_line,
        "{situation}: the table says it holds the frame from its first line"
    );
    assert!(
        !table.complete,
        "{situation}: the table says it is complete, though the lines drawn before \
         it was armed are not in it"
    );
    assert!(
        emu.picture_from_line_states().is_none(),
        "{situation}: the picture from the line table answered"
    );
    readers_answer(&mut emu, layers, Some(false), situation);
    emu.run_to_capture_boundary();
    core.picture_from_the_table_is_the_frame(
        &mut emu,
        "held at the boundary of the frame after the one the table was armed in",
    );

    emu.set_capture_recording(CaptureRecording::default());
    let situation = "held at the boundary with no line table";
    assert!(
        emu.picture_from_line_states().is_none(),
        "{situation}: the picture from the line table answered"
    );
    readers_answer(&mut emu, layers, Some(false), situation);
}

/// A frame snapshot, the pixel sources and the line sprites number the
/// frame whose picture the host's framebuffer shows: the frames finished,
/// plus one while the frame is held at its capture boundary or stopped past
/// it before its end, where the framebuffer shows the held frame's finished
/// picture; a frame stopped part of the way through its picture, whose
/// framebuffer shows the lines drawn so far over the frame before, is
/// numbered by the frames finished. The line table numbers the frame its
/// lines belong to.
pub fn the_frame_number_names_the_picture_the_host_holds<E: CaptureCore>(core: &CoreUnderTest<E>) {
    let mut emu = core.warmed_up(EVERY_RECORDER);
    core.numbers(&mut emu)
        .are_those_of(Place::Finished, "after a whole frame");

    emu.run_to_capture_boundary();
    assert!(emu.at_capture_boundary(), "not held at the boundary");
    core.numbers(&mut emu)
        .are_those_of(Place::AtBoundary, "held at the capture boundary");
    (core.run_frame)(&mut emu);

    let stop = core.stop(&mut emu, &core.stop_in_picture, "in the picture");
    core.numbers(&mut emu).are_those_of(
        Place::InPicture,
        "stopped part of the way through the picture",
    );
    remove_and_run(&mut emu, stop);

    let stop = core.stop(
        &mut emu,
        &core.stop_in_vertical_blank,
        "in the vertical blank",
    );
    let shown = Picture::shown(&emu);
    let numbers = core.numbers(&mut emu);
    remove_and_run(&mut emu, stop);
    emu.run_frame();
    let finished = Picture::shown(&emu);
    assert!(
        shown == finished,
        "stopped in the vertical blank, the host's framebuffer does not show the held frame's \
         finished picture: {}",
        shown.difference(&finished)
    );
    numbers.are_those_of(Place::InVerticalBlank, "stopped in the vertical blank");
    core.numbers(&mut emu)
        .are_those_of(Place::Finished, "after the stopped frame ran to its end");
}

// -- What the checks share -----------------------------------------------

/// Where the frame a check looks at stands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    /// It ran to its end.
    Finished,
    /// Held at its capture boundary.
    AtBoundary,
    /// Stopped by the event breakpoint part of the way through its picture.
    InPicture,
    /// Stopped by the event breakpoint in its vertical blank, past its
    /// capture boundary.
    InVerticalBlank,
}

impl<E: CaptureCore> CoreUnderTest<E> {
    /// A new emulator with `recording` armed from its first frame, run
    /// [`WARM_UP`] frames on the player's loop.
    fn warmed_up(&self, recording: CaptureRecording) -> E {
        let mut emu = (self.build)();
        emu.set_capture_recording(recording);
        for _ in 0..WARM_UP {
            (self.run_frame)(&mut emu);
        }
        emu
    }

    /// The core's state hash.
    fn hash(&self, emu: &E) -> u64 {
        (self.state_hash)(emu)
    }

    /// The frames the core has finished.
    fn frames(&self, emu: &E) -> u64 {
        (self.frame_count)(emu)
    }

    /// Stop `emu` with `breakpoint` through a frame call, which goes on with
    /// a frame stopped before: the debugger paused, part of the way through
    /// a frame it has not counted, held at no boundary. Returns the
    /// breakpoint's id.
    fn stop(&self, emu: &mut E, breakpoint: &Breakpoint, place: &str) -> BreakpointId {
        let frames = self.frames(emu);
        let id = emu.debugger().add_breakpoint(breakpoint.clone());
        emu.run_frame();
        assert_eq!(
            emu.debugger().get_execution_mode(),
            ExecutionMode::Paused,
            "the event breakpoint {place} did not stop the frame"
        );
        assert!(
            !emu.at_capture_boundary(),
            "stopped by the event breakpoint {place}, the core says it is held at its capture \
             boundary"
        );
        assert_eq!(
            self.frames(emu),
            frames,
            "the event breakpoint {place} stopped the frame after its end"
        );
        id
    }

    /// Hold `emu` at the boundary of a frame whose lines showed sprites.
    fn hold_a_frame_with_sprites(&self, emu: &mut E) {
        for _ in 0..FRAMES {
            emu.run_to_capture_boundary();
            let sprites = emu.line_sprites().expect("a program is loaded");
            if !slots_that_drew(&sprites).is_empty() {
                return;
            }
        }
        panic!("no frame of {FRAMES} showed sprites");
    }

    /// Run two frames and hold the recorders to holding something: a take
    /// of the first frame's writes, then the line table and the video
    /// memory sources. The second frame's writes stay in the log.
    fn fill_recorders(&self, emu: &mut E, when: &str) {
        (self.run_frame)(emu);
        let writes = emu.take_frame_writes().expect("frame writes are recorded");
        assert!(
            !writes.records.is_empty(),
            "{when}: a frame of the program left no frame write"
        );
        (self.run_frame)(emu);
        assert!(
            table_and_sources_hold_something(emu),
            "{when}: two frames left the line table or the video memory sources empty"
        );
    }

    /// The saving machine `held` was finished as a frame run whole: no more
    /// held, its frames counted, and its state and the savestate `saved` it
    /// gave hold the machine of `whole`.
    fn holds_the_frame_run_whole(&self, held: &E, whole: &E, saved: &[u8], case: &str) {
        assert!(!saved.is_empty(), "{case}: the save gave no bytes");
        assert!(
            !held.at_capture_boundary(),
            "{case}: still held at the boundary after the save"
        );
        assert_eq!(
            self.frames(held),
            self.frames(whole),
            "{case}: the save did not finish the held frame"
        );
        assert_eq!(
            self.hash(held),
            self.hash(whole),
            "{case}: the machine after the save is not the machine of a twin that ran the frames \
             whole"
        );
        let mut loaded = (self.build)();
        loaded
            .load_state(saved)
            .unwrap_or_else(|error| panic!("{case}: the savestate does not load: {error}"));
        assert_eq!(
            self.hash(&loaded),
            self.hash(whole),
            "{case}: the savestate does not hold the frame's end"
        );
    }

    /// Hold two machines where `place` says, draw one's frame again every
    /// way a host does, and hold the drawing to moving nothing.
    fn draws_again_moving_nothing(
        &self,
        recording: CaptureRecording,
        place: Place,
        situation: &str,
    ) {
        let mut emu = self.warmed_up(recording);
        let mut twin = self.warmed_up(recording);
        let stops = [&mut emu, &mut twin].map(|machine| match place {
            Place::AtBoundary => {
                machine.run_to_capture_boundary();
                assert!(machine.at_capture_boundary(), "{situation}: not held");
                None
            }
            Place::Finished => {
                (self.run_frame)(machine);
                None
            }
            Place::InPicture => Some(self.stop(machine, &self.stop_in_picture, "in the picture")),
            Place::InVerticalBlank => Some(self.stop(
                machine,
                &self.stop_in_vertical_blank,
                "in the vertical blank",
            )),
        });

        let hash = self.hash(&emu);
        let framebuffer = emu.get_framebuffer().data.clone();
        let picture = Picture::shown(&emu);
        let moved_nothing = |emu: &E, reader: &str| {
            assert_eq!(
                self.hash(emu),
                hash,
                "{situation}: {reader} moved the state hash"
            );
            assert!(
                emu.get_framebuffer().data == framebuffer,
                "{situation}: {reader} drew into the host's framebuffer"
            );
        };
        let layers = layer_count(&mut emu);
        assert!(
            self.sprite_layer < layers,
            "{situation}: the sprite layer {} is past the {layers} layers",
            self.sprite_layer
        );
        moved_nothing(&emu, "render_layer");
        let _ = emu.picture_from_line_states();
        moved_nothing(&emu, "picture_from_line_states");
        let _ = emu.pixel_sources(planes::MAIN | planes::SUB_SCREEN);
        moved_nothing(&emu, "pixel_sources");
        let _ = emu.line_sprites();
        moved_nothing(&emu, "line_sprites");
        for selection in [vec![], vec![0, 1], vec![1; 256]] {
            let _ = emu.render_sprites(&selection);
            moved_nothing(&emu, &format!("render_sprites({selection:?})"));
        }
        let _ = (self.sprite_order)(&mut emu);
        moved_nothing(&emu, "the sprite order");

        let every_line = emu.repaint_held_frame();
        assert_eq!(
            self.hash(&emu),
            hash,
            "{situation}: the repaint moved the state hash"
        );
        let exact = emu.line_states().is_some_and(|table| table.memory_exact);
        if every_line && exact {
            let repainted = Picture::shown(&emu);
            assert!(
                repainted == picture,
                "{situation}: the repaint drew every line from a line table whose memory is \
                 exact, yet changed the picture: {}",
                repainted.difference(&picture)
            );
        }

        for (machine, stop) in [&mut emu, &mut twin].into_iter().zip(stops) {
            match stop {
                Some(id) => {
                    remove_and_run(machine, id);
                    machine.run_frame();
                    machine.run_frame();
                }
                None => {
                    (self.run_frame)(machine);
                    (self.run_frame)(machine);
                }
            }
        }
        assert_eq!(
            self.hash(&emu),
            self.hash(&twin),
            "{situation}: the frame drawn again ran on into another state than its twin's"
        );
        let pictures = (Picture::shown(&emu), Picture::shown(&twin));
        assert!(
            pictures.0 == pictures.1,
            "{situation}: the frame after the one drawn again shows another picture than its \
             twin's: {}",
            pictures.0.difference(&pictures.1)
        );
    }

    /// Hold the sprite order of the frame `emu` holds to the sprites that
    /// drew; returns whether any did.
    fn order_names_what_drew(&self, emu: &mut E, table: &str) -> bool {
        let frame = emu.capture_frame_snapshot().frame;
        let order = (self.sprite_order)(emu);
        let named: Vec<u32> = order.iter().map(|entry| entry.index).collect();
        let once: BTreeSet<u32> = named.iter().copied().collect();
        assert_eq!(
            once.len(),
            named.len(),
            "frame {frame}, {table}: the sprite order names a sprite twice: {named:?}"
        );
        let sprites = emu.line_sprites().expect("a program is loaded");
        let drew = slots_that_drew(&sprites);
        assert_eq!(
            once, drew,
            "frame {frame}, {table}: the sprite order names other sprites than those the line \
             sprites say drew a column on a line showing sprites"
        );
        let traced = emu
            .pixel_sources(planes::LAYER | planes::CELL)
            .expect("a program is loaded");
        let sprite = drawn_by(self.sprite_layer as u16);
        let (drew_pixel, cell) = (
            traced.layer.expect("the layer plane was asked for"),
            traced.cell.expect("the cell plane was asked for"),
        );
        for (at, (&value, &slot)) in drew_pixel.iter().zip(&cell).enumerate() {
            if value & layer::WHAT_DREW == sprite && !once.contains(&u32::from(slot)) {
                panic!(
                    "frame {frame}, {table}: pixel ({}, {}) came from sprite {slot}, which the \
                     sprite order does not name ({named:?})",
                    at % traced.width as usize,
                    at / traced.width as usize
                );
            }
        }
        !drew.is_empty()
    }

    /// The picture from the line table of the frame `emu` holds answers
    /// and, its memory exact, is the picture the host holds.
    fn picture_from_the_table_is_the_frame(&self, emu: &mut E, situation: &str) {
        let table = emu.line_states().expect("the line table is kept");
        assert!(
            table.from_first_line && table.complete,
            "{situation}: the table does not hold the frame whole (from its first line {}, \
             complete {})",
            table.from_first_line,
            table.complete
        );
        let redrawn = emu.picture_from_line_states().unwrap_or_else(|| {
            panic!("{situation}: the picture from the line table answered None")
        });
        let (redrawn, shown) = (Picture::of_image(&redrawn), Picture::shown(emu));
        if table.memory_exact {
            assert!(
                redrawn == shown,
                "{situation}: the picture from the line table is not the frame's: {}",
                redrawn.difference(&shown)
            );
        }
    }

    /// The numbers `emu` gives the picture it holds and the lines of its
    /// table.
    fn numbers(&self, emu: &mut E) -> Numbers {
        Numbers {
            finished: self.frames(emu),
            snapshot: emu.capture_frame_snapshot().frame,
            pixel_sources: emu
                .pixel_sources(planes::LAYER)
                .expect("a program is loaded")
                .frame,
            line_sprites: emu.line_sprites().expect("a program is loaded").frame,
            line_table: emu.line_states().expect("the line table is kept").frame,
        }
    }
}

/// The frames a core has finished, and the numbers its readers give the
/// picture the host holds and the lines of the line table.
struct Numbers {
    finished: u64,
    snapshot: u64,
    pixel_sources: u64,
    line_sprites: u64,
    line_table: u64,
}

impl Numbers {
    /// Hold the numbers to those of a frame standing at `place`: the
    /// picture is numbered the frames finished, plus one when the frame is
    /// held at its capture boundary or stopped past it; the line table
    /// numbers the frame its lines belong to, one past the frames finished
    /// while that frame has not ended.
    fn are_those_of(&self, place: Place, situation: &str) {
        let finished = self.finished;
        let picture =
            finished + u64::from(matches!(place, Place::AtBoundary | Place::InVerticalBlank));
        for (reader, number) in [
            ("the snapshot", self.snapshot),
            ("the pixel sources", self.pixel_sources),
            ("the line sprites", self.line_sprites),
        ] {
            assert_eq!(
                number, picture,
                "{situation}: {reader} numbers the picture {number}; with {finished} frames \
                 finished the host's framebuffer shows frame {picture}"
            );
        }
        let lines = finished + u64::from(place != Place::Finished);
        assert_eq!(
            self.line_table, lines,
            "{situation}: the line table numbers its lines' frame {}; with {finished} frames \
             finished they belong to frame {lines}",
            self.line_table
        );
    }
}

/// Run the debugger, as a host does after a reset or a load.
fn run_debugger<E: CaptureCore>(emu: &mut E) {
    emu.debugger().set_execution_mode(ExecutionMode::Running);
}

/// Take the breakpoint `id` out and run the debugger again.
fn remove_and_run<E: CaptureCore>(emu: &mut E, id: BreakpointId) {
    emu.debugger().remove_breakpoint(id);
    run_debugger(emu);
}

/// The layers of the core's layer list: the ids
/// [`CaptureCore::render_layer`] answers, from 0.
fn layer_count<E: CaptureCore>(emu: &mut E) -> u32 {
    let count = (0..MOST_LAYERS)
        .take_while(|&id| emu.render_layer(id).is_some())
        .count() as u32;
    assert!(count > 0, "render_layer answers for no layer");
    assert!(
        count < MOST_LAYERS,
        "render_layer answers for every id up to {MOST_LAYERS}: an id past the layer list must \
         give None"
    );
    count
}

/// Every layer, the pixel sources and the line sprites answer; with
/// `from_table`, each says whether it drew every line from the line table
/// as given.
fn readers_answer<E: CaptureCore>(
    emu: &mut E,
    layers: u32,
    from_table: Option<bool>,
    situation: &str,
) {
    let mut said = Vec::new();
    for id in 0..layers {
        let rendered = emu
            .render_layer(id)
            .unwrap_or_else(|| panic!("{situation}: layer {id} did not answer"));
        said.push((format!("layer {id}"), rendered.from_line_states));
    }
    let traced = emu
        .pixel_sources(planes::MAIN)
        .unwrap_or_else(|| panic!("{situation}: the pixel sources did not answer"));
    said.push(("the pixel sources".to_string(), traced.from_line_states));
    let sprites = emu
        .line_sprites()
        .unwrap_or_else(|| panic!("{situation}: the line sprites did not answer"));
    said.push(("the line sprites".to_string(), sprites.from_line_states));
    if let Some(expected) = from_table {
        for (reader, from_line_states) in said {
            assert_eq!(
                from_line_states, expected,
                "{situation}: {reader} says from_line_states {from_line_states}"
            );
        }
        if expected {
            assert!(
                traced.matches_picture,
                "{situation}: the pixel sources' picture is not the picture the host holds"
            );
        }
    }
}

/// The recorders hold nothing: no byte of video memory written, no frame
/// write, no line of any frame.
fn assert_recorders_empty<E: CaptureCore>(emu: &mut E, when: &str) {
    let sources = emu
        .video_memory_sources()
        .unwrap_or_else(|| panic!("{when}: the video memory sources are not recorded"));
    let written = sources
        .origins()
        .iter()
        .filter(|&&origin| origin != 0)
        .count();
    let stamped = sources.frames().iter().filter(|&&frame| frame != 0).count();
    assert!(
        written == 0 && stamped == 0,
        "{when}: the video memory sources name {written} bytes written and {stamped} frames"
    );
    let writes = emu
        .take_frame_writes()
        .unwrap_or_else(|| panic!("{when}: the frame writes are not recorded"));
    assert!(
        writes.records.is_empty() && writes.dropped == 0,
        "{when}: the frame writes hold {} words and {} dropped records",
        writes.records.len(),
        writes.dropped
    );
    if let Some(table) = emu.line_states() {
        assert!(
            !table.from_first_line && !table.complete,
            "{when}: the line table holds a frame (from its first line {}, complete {})",
            table.from_first_line,
            table.complete
        );
    }
    if let Some(palettes) = emu.line_palettes() {
        let recorded = palettes
            .palette_of_line
            .iter()
            .filter(|&&palette| palette != NO_PALETTE)
            .count();
        assert_eq!(
            recorded, 0,
            "{when}: the line palettes name {recorded} lines"
        );
    }
}

/// Whether the line table holds a line and the video memory sources a
/// byte written.
fn table_and_sources_hold_something<E: CaptureCore>(emu: &E) -> bool {
    let lines = emu.line_palettes().is_some_and(|palettes| {
        palettes
            .palette_of_line
            .iter()
            .any(|&palette| palette != NO_PALETTE)
    });
    let written = emu
        .video_memory_sources()
        .is_some_and(|sources| sources.origins().iter().any(|&origin| origin != 0));
    lines && written
}

/// The slots of the sprites of which a line showing sprites drew at least
/// one visible column.
fn slots_that_drew(sprites: &LineSprites) -> BTreeSet<u32> {
    let mut slots = BTreeSet::new();
    for line in sprites.lines.as_chunks::<LINE_SPRITES_STRIDE>().0 {
        let (first, count, flags) = (usize::from(line[1]), usize::from(line[2]), line[3]);
        if flags & line_flags::NOT_SHOWN != 0 {
            continue;
        }
        for entry in first..first + count {
            if sprites.drawn_width[entry] > 0 {
                slots.insert(u32::from(sprites.slots[entry]));
            }
        }
    }
    slots
}

/// The samples the frames so far left for the host, by their bits.
fn take_audio<E: CaptureCore>(emu: &mut E) -> Vec<u32> {
    emu.get_audio_samples()
        .iter()
        .map(|sample| sample.to_bits())
        .collect()
}

/// Where two runs of samples part, for a failure message.
fn sound_difference(a: &[u32], b: &[u32]) -> String {
    match a.iter().zip(b).position(|(x, y)| x != y) {
        Some(at) => format!(
            "{} samples against {}, the first difference at sample {at}",
            a.len(),
            b.len()
        ),
        None => format!("{} samples against {}", a.len(), b.len()),
    }
}

/// A picture as the host holds it, cut to its size, for comparing.
#[derive(Clone, PartialEq, Eq)]
struct Picture {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Picture {
    /// The picture `emu`'s host holds, as a frame snapshot cuts it.
    fn shown<E: CaptureCore>(emu: &E) -> Self {
        Self::of_snapshot(&emu.capture_frame_snapshot())
    }

    /// A snapshot's picture.
    fn of_snapshot<C>(snapshot: &crate::capture::FrameSnapshot<C>) -> Self {
        Self {
            width: snapshot.width,
            height: snapshot.height,
            rgba: snapshot.framebuffer.clone(),
        }
    }

    /// An image's picture.
    fn of_image(image: &PixelImage) -> Self {
        Self {
            width: image.width,
            height: image.height,
            rgba: image.rgba.clone(),
        }
    }

    /// How `self` and `other` differ, for a failure message.
    fn difference(&self, other: &Picture) -> String {
        if (self.width, self.height) != (other.width, other.height) {
            return format!(
                "{}x{} against {}x{}",
                self.width, self.height, other.width, other.height
            );
        }
        let pixels = self
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .zip(other.rgba.as_chunks::<4>().0);
        let mut differing = 0;
        let mut first = None;
        for (at, (a, b)) in pixels.enumerate() {
            if a != b {
                differing += 1;
                first.get_or_insert((at, a, b));
            }
        }
        match first {
            Some((at, a, b)) => format!(
                "{differing} pixels differ, the first at ({}, {}): {a:?} against {b:?}",
                at % self.width.max(1) as usize,
                at / self.width.max(1) as usize
            ),
            None if self.rgba.len() != other.rgba.len() => {
                format!("{} bytes against {}", self.rgba.len(), other.rgba.len())
            }
            None => "the same picture".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Line sprites of three lines: a line showing sprites, with one drawn
    /// and one not; a line showing none, whose sprite drew; and a line with
    /// another sprite drawn.
    fn line_sprites() -> LineSprites {
        LineSprites {
            frame: 1,
            from_line_states: true,
            lines: vec![
                10,
                0,
                2,
                0, //
                11,
                2,
                1,
                line_flags::NOT_SHOWN, //
                12,
                3,
                1,
                line_flags::OVER_LIMIT,
            ],
            slots: vec![4, 7, 9, 2],
            fates: vec![0, 5, 0, 1],
            x: vec![0, 300, 8, 20],
            width: vec![8, 8, 8, 16],
            drawn_width: vec![8, 0, 8, 3],
        }
    }

    #[test]
    fn the_sprites_that_drew_are_those_a_line_showing_sprites_drew_a_column_of() {
        assert_eq!(
            slots_that_drew(&line_sprites()),
            BTreeSet::from([2, 4]),
            "slot 7 drew no column, slot 9 drew on a line showing no sprite"
        );
        assert!(slots_that_drew(&LineSprites::default()).is_empty());
    }

    fn picture(pixels: &[[u8; 4]]) -> Picture {
        Picture {
            width: 2,
            height: (pixels.len() / 2) as u32,
            rgba: pixels.concat(),
        }
    }

    #[test]
    fn a_picture_difference_names_the_first_pixel_apart_and_how_many() {
        let a = picture(&[[1, 2, 3, 255], [4, 5, 6, 255], [7, 8, 9, 255], [0, 0, 0, 0]]);
        let mut b = a.clone();
        assert_eq!(a.difference(&b), "the same picture");
        b.rgba[9] = 99;
        b.rgba[13] = 99;
        assert_eq!(
            a.difference(&b),
            "2 pixels differ, the first at (0, 1): [7, 8, 9, 255] against [7, 99, 9, 255]"
        );
        let c = Picture {
            width: 4,
            height: 1,
            rgba: a.rgba.clone(),
        };
        assert_eq!(a.difference(&c), "2x2 against 4x1");
    }

    #[test]
    fn a_sound_difference_names_the_first_sample_apart() {
        assert_eq!(
            sound_difference(&[1, 2, 3], &[1, 2, 3]),
            "3 samples against 3"
        );
        assert_eq!(
            sound_difference(&[1, 2, 3], &[1, 5, 3, 4]),
            "3 samples against 4, the first difference at sample 1"
        );
    }

    #[test]
    fn every_recorder_is_every_switch() {
        let all = EVERY_RECORDER;
        assert!(
            all.line_states
                && all.pixel_sources
                && all.line_sprites
                && all.video_memory_sources
                && all.frame_writes
        );
    }
}
