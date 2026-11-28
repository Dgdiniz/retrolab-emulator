//! The sound conformance suite: checks a core's sound hooks
//! ([`SoundCore`]) and its sound chips' debugger views on a program of the
//! core's own, the way [`crate::capture::conformance`] checks the capture.
//!
//! A core describes itself in a [`SoundCoreUnderTest`]: how to build its
//! emulator with its program loaded, how to bring every sound chip to play
//! without the program's help, and an address the program runs every
//! frame. Its test file then calls
//! [`sound_conformance_tests!`](crate::sound_conformance_tests), which
//! writes one test per check, so each check passes or fails on its own;
//! each check is also a public function of this module.
//!
//! The first check holds the sound moment to its word: every chip makes
//! sound after it, so that no other check passes for want of anything to
//! hear. A mute, the rules say, is the host's mix's alone: it changes
//! nothing the machine holds, and it lasts until the host lifts it. The
//! sound switch keeps the sound out of the host's ring and nothing else:
//! with it off the machine runs as with it on, and the stems and the log
//! come as they come with it on; switched back on, the next frame queues
//! that frame's sound, not what the frames before made. Stems
//! only read: armed, they change nothing the machine holds or the host
//! hears; they hold whole frames, every stem as many sample frames as the
//! master; and they add up to the master by the layout's rule wherever the
//! take counts no departure. So does the sound log, which starts with the
//! chips' registers at time 0, never goes back in time, holds a frame's end
//! for every frame, keeps the machine's time from frame to frame, says when
//! a load came between two takes, and, holding the keys alone, holds the
//! notes and no write; and, holding the mailbox, holds each value the main
//! processor writes into its sound processor's mailbox, at its port and at
//! its time in its frame.

use crate::audio::sound_log::{SoundEvent, SoundLog, SoundLogContents};
use crate::audio::stems::{SampleKind, StemLayout, StemSet, StemTake};
use crate::audio::{render_frames, SoundCore};
use crate::types::{Breakpoint, BreakpointType, ExecutionMode};

/// Frames a check runs before the sound moment, so that the program has
/// set itself up.
const WARM_UP: u64 = 4;

/// Frames a check listens to.
const FRAMES: u32 = 20;

/// A host sample this loud or louder is sound, not silence.
const SOUND_PEAK: f32 = 1.0e-3;

/// What the suite needs of a core: its emulator with a program of its own,
/// and what no trait gives.
pub struct SoundCoreUnderTest<E> {
    /// A new emulator with the program loaded, ready to run frames: its
    /// debugger running, its sound on.
    pub build: fn() -> E,
    /// Bring every sound chip to play, between two frames and without the
    /// program's help: a note keyed on each chip, and on a chip with an
    /// echo a voice routed to it, with the echo's writes on.
    pub sound_moment: fn(&mut E),
    /// An address the main processor (CPU 0) executes in every frame.
    pub every_frame: u32,
    /// The program's ROM, for a check that loads it again.
    pub rom: Vec<u8>,
    /// Another program, which a check loads in place of the first.
    pub other_rom: Vec<u8>,
    /// A program whose main processor writes the values 0, 1, 2 and on,
    /// wrapping at 256, one after another into its sound processor's
    /// mailbox at `mailbox_port`, through every frame, and makes no sound.
    pub mailbox_rom: Vec<u8>,
    /// The port the mailbox program writes, as the log names it
    /// ([`SoundEvent::Mailbox`]).
    pub mailbox_port: u16,
}

impl<E: SoundCore> SoundCoreUnderTest<E> {
    /// A new machine run through its warm-up, its sound moment past.
    fn playing(&self) -> E {
        let mut emu = (self.build)();
        for _ in 0..WARM_UP {
            emu.run_frame();
        }
        (self.sound_moment)(&mut emu);
        emu
    }
}

/// Write one test per check of the sound conformance suite
/// ([`crate::audio::conformance`]) in a core's test file, each running its
/// check on the [`SoundCoreUnderTest`] the expression gives:
///
/// ```text
/// fn my_core() -> SoundCoreUnderTest<MyEmulator> { ... }
///
/// retro_core::sound_conformance_tests!(my_core());
/// ```
///
/// The core's test crate enables `retro-core`'s `sound-conformance`
/// feature, among its dev-dependencies.
#[macro_export]
macro_rules! sound_conformance_tests {
    ($core:expr) => {
        $crate::sound_conformance_tests!(@each $core;
            every_chip_plays_after_the_sound_moment,
            the_render_runs_the_player_s_loop,
            a_mute_is_the_mixer_s,
            a_mute_is_the_host_s,
            the_sound_switch_keeps_only_the_host_s_copy,
            the_sound_switched_back_on_queues_one_frame,
            stems_change_nothing,
            stems_cover_whole_frames,
            stems_sum_to_the_master,
            the_log_changes_nothing,
            the_log_starts_with_the_image_and_never_goes_back,
            the_log_keeps_the_machine_s_time,
            a_load_interrupts_the_log,
            the_log_takes_the_keys_down,
            the_log_takes_the_mailbox_down,
        );
    };
    (@each $core:expr; $($check:ident),+ $(,)?) => {
        $(
            #[test]
            fn $check() {
                $crate::audio::conformance::$check(&$core);
            }
        )+
    };
}

// -- Helpers -------------------------------------------------------------

/// Mute or unmute every channel of every sound chip but those of `keep`.
fn mute_all_but<E: SoundCore>(emu: &mut E, keep: Option<usize>, muted: bool) {
    for chip in 0..emu.audio_chip_count() {
        if Some(chip) == keep {
            continue;
        }
        let debugger = emu
            .audio_debugger(chip)
            .unwrap_or_else(|| panic!("sound chip {chip} answers its count"));
        for channel in 0..debugger.get_channel_count() {
            debugger.mute_channel(channel, muted);
        }
    }
}

/// Whether every channel of every sound chip is muted (`true`) or none is
/// (`false`), naming the first that is not as `what` leaves it.
fn assert_all_muted<E: SoundCore>(emu: &mut E, muted: bool, what: &str) {
    for chip in 0..emu.audio_chip_count() {
        let debugger = emu
            .audio_debugger(chip)
            .unwrap_or_else(|| panic!("sound chip {chip} answers its count"));
        for channel in 0..debugger.get_channel_count() {
            let state = debugger
                .get_channel_state(channel)
                .unwrap_or_else(|| panic!("chip {chip} channel {channel} reports its state"));
            assert_eq!(
                state.muted, muted,
                "chip {chip} channel {channel}: muted {} after {what}",
                state.muted
            );
        }
    }
}

/// The loudest of the host's samples, taking them.
fn host_peak<E: SoundCore>(emu: &mut E) -> f32 {
    emu.get_audio_samples()
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()))
}

/// The machine's stems, which a core that runs the stem checks has.
fn layout_of<E: SoundCore>(emu: &E) -> StemLayout {
    emu.stem_layout()
        .expect("the console takes stems: its layout is not None")
}

/// Every stem and the master of `take` hold `take.frames` sample frames.
fn assert_whole(take: &StemTake, layout: &StemLayout, what: &str) {
    assert_eq!(
        take.stems.len(),
        take.armed.len(),
        "{what}: a buffer per armed stem"
    );
    for (stem, index) in take.stems.iter().zip(take.armed.iter()) {
        let channel = &layout.channels[index];
        assert_eq!(
            stem.len(),
            take.frames as usize * channel.width(),
            "{what}: stem {} holds the take's sample frames",
            channel.name
        );
    }
    assert_eq!(
        take.master.len(),
        take.frames as usize * 2,
        "{what}: the master holds the take's sample frames"
    );
}

// -- The checks ----------------------------------------------------------

/// Every sound chip makes sound after the sound moment: with every other
/// chip's channels muted, the host's samples are not silence.
pub fn every_chip_plays_after_the_sound_moment<E: SoundCore>(core: &SoundCoreUnderTest<E>) {
    let chips = (core.build)().audio_chip_count();
    assert!(chips > 0, "the console has a sound chip");
    for chip in 0..chips {
        let mut emu = core.playing();
        mute_all_but(&mut emu, Some(chip), true);
        let _ = emu.get_audio_samples();
        let mut peak = 0.0f32;
        for _ in 0..FRAMES {
            emu.render_frame();
            peak = peak.max(host_peak(&mut emu));
        }
        assert!(
            peak >= SOUND_PEAK,
            "sound chip {chip} made no sound after the sound moment (peak {peak})"
        );
    }
}

/// The render runs the loop a player's host runs: with an execute
/// breakpoint armed that the program reaches every frame, which stops a
/// frame call, [`render_frames`] runs each of its frames to its end, and
/// the machine and its sound are those of a machine with nothing armed.
/// A render of a machine whose sound is off turns it on and says so.
pub fn the_render_runs_the_player_s_loop<E: SoundCore>(core: &SoundCoreUnderTest<E>) {
    let breakpoint = Breakpoint {
        address: core.every_frame,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    };
    let mut probe = core.playing();
    probe.debugger().add_breakpoint(breakpoint.clone());
    probe.run_frame();
    assert_eq!(
        probe.debugger().get_execution_mode(),
        ExecutionMode::Paused,
        "no frame call stopped at {:#X}: the address is not run every frame",
        core.every_frame
    );

    let mut armed = core.playing();
    let mut plain = core.playing();
    let start = armed.frame_count();
    armed.debugger().add_breakpoint(breakpoint);
    let report = render_frames(&mut armed, FRAMES);
    for _ in 0..FRAMES {
        plain.run_frame();
    }
    assert_eq!(report.frames, FRAMES);
    assert!(!report.sound_turned_on, "the sound was on");
    assert_eq!(
        armed.frame_count(),
        start + u64::from(FRAMES),
        "the render ran every frame to its end"
    );
    assert_eq!(
        armed.debugger().get_execution_mode(),
        ExecutionMode::Running,
        "the render stopped at the breakpoint"
    );
    assert_eq!(
        armed.state_hash(),
        plain.state_hash(),
        "the render's machine is not the one a frame call with nothing armed runs"
    );
    assert_eq!(
        armed.get_audio_samples().to_vec(),
        plain.get_audio_samples().to_vec(),
        "the render's sound is not the one a frame call with nothing armed makes"
    );

    let mut silent = (core.build)();
    silent.set_audio_enabled(false);
    let report = render_frames(&mut silent, 1);
    assert!(
        report.sound_turned_on,
        "the render says it turned the sound on"
    );
    assert!(silent.is_audio_enabled(), "the sound stays on");
}

/// A mute is the mixer's: with every channel of every chip muted before
/// the sound moment, each frame's state hash is the hash of a machine
/// with nothing muted, which makes sound while the muted one makes none.
pub fn a_mute_is_the_mixer_s<E: SoundCore>(core: &SoundCoreUnderTest<E>) {
    let mut heard = (core.build)();
    let mut muted = (core.build)();
    mute_all_but(&mut muted, None, true);
    for _ in 0..WARM_UP {
        heard.run_frame();
        muted.run_frame();
    }
    (core.sound_moment)(&mut heard);
    (core.sound_moment)(&mut muted);
    let (mut heard_peak, mut muted_peak) = (0.0f32, 0.0f32);
    for frame in 1..=FRAMES {
        heard.render_frame();
        muted.render_frame();
        heard_peak = heard_peak.max(host_peak(&mut heard));
        muted_peak = muted_peak.max(host_peak(&mut muted));
        if heard.state_hash() != muted.state_hash() {
            let parts: Vec<&str> = heard
                .state_hash_parts()
                .iter()
                .zip(muted.state_hash_parts().iter())
                .filter(|(a, b)| a.1 != b.1)
                .map(|(a, _)| a.0)
                .collect();
            panic!(
                "frame {frame} after the sound moment: the muted machine's state parted from the \
                 machine with nothing muted, in {}",
                parts.join(", ")
            );
        }
    }
    assert!(
        heard_peak >= SOUND_PEAK,
        "the machine with nothing muted made no sound (peak {heard_peak})"
    );
    assert!(
        muted_peak < SOUND_PEAK,
        "the machine with every channel muted made sound (peak {muted_peak})"
    );
}

/// A mute is the host's: it lasts across the reset button, a power cycle
/// and the load of another ROM or of the same one again, on every channel
/// of every chip, until the host lifts it.
pub fn a_mute_is_the_host_s<E: SoundCore>(core: &SoundCoreUnderTest<E>) {
    let mut emu = (core.build)();
    emu.run_frame();
    mute_all_but(&mut emu, None, true);
    emu.reset();
    assert_all_muted(&mut emu, true, "the reset button");
    emu.power_cycle();
    assert_all_muted(&mut emu, true, "a power cycle");
    emu.load_rom(&core.other_rom)
        .expect("the other program loads");
    assert_all_muted(&mut emu, true, "a load of another ROM");
    emu.load_rom(&core.rom).expect("the program loads");
    assert_all_muted(&mut emu, true, "a load of the program again");
    emu.load_rom(&core.rom).expect("the program loads");
    assert_all_muted(&mut emu, true, "a load of the same ROM");
    emu.run_frame();
    mute_all_but(&mut emu, None, false);
    assert_all_muted(&mut emu, false, "the host lifted them");
}

/// The sound switch keeps the sound out of the host's ring and nothing
/// else: two machines, one with the sound switched off before the sound
/// moment, each with every stem armed and everything logged from that
/// moment, give the same stems (and master) and the same log after every
/// frame, sample for sample and entry for entry, while the one switched off
/// queues nothing for its host; and switched back on, its state is the
/// other's: its chips ran on.
pub fn the_sound_switch_keeps_only_the_host_s_copy<E: SoundCore>(core: &SoundCoreUnderTest<E>) {
    let mut heard = (core.build)();
    let mut silent = (core.build)();
    for _ in 0..WARM_UP {
        heard.run_frame();
        silent.run_frame();
    }
    let _ = heard.get_audio_samples();
    let _ = silent.get_audio_samples();
    silent.set_audio_enabled(false);
    let layout = layout_of(&heard);
    for emu in [&mut heard, &mut silent] {
        emu.arm_stems(layout.all());
        emu.start_sound_log(EVERYTHING);
        (core.sound_moment)(emu);
    }
    let mut peak = 0.0f32;
    for frame in 1..=FRAMES {
        heard.render_frame();
        silent.render_frame();
        peak = peak.max(host_peak(&mut heard));
        assert!(
            silent.get_audio_samples().is_empty(),
            "frame {frame}: the host got sound from the machine whose sound is off"
        );
        let stems = heard.take_stems().expect("stems armed");
        assert!(stems.frames > 0, "frame {frame}: the stems took the frame");
        assert_eq!(
            silent.take_stems(),
            Some(stems),
            "frame {frame}: the stems with the sound off are not the ones with it on"
        );
        assert_eq!(
            silent.take_sound_log(),
            heard.take_sound_log(),
            "frame {frame}: the log with the sound off is not the one with it on"
        );
    }
    assert!(
        peak >= SOUND_PEAK,
        "the machine with the sound on made no sound (peak {peak})"
    );
    silent.set_audio_enabled(true);
    assert_eq!(
        silent.state_hash(),
        heard.state_hash(),
        "switched back on, the machine is not the one that kept its sound on"
    );
}

/// The sound switched back on queues one frame of sound at the next
/// frame's end, not what the frames run with it off made: as many samples
/// as the same frame queues on a machine that kept its sound on, within a
/// sample frame, and the sound plays.
pub fn the_sound_switched_back_on_queues_one_frame<E: SoundCore>(core: &SoundCoreUnderTest<E>) {
    let mut kept = core.playing();
    let mut switched = core.playing();
    let _ = kept.get_audio_samples();
    let _ = switched.get_audio_samples();
    switched.set_audio_enabled(false);
    for frame in 1..=30 {
        kept.render_frame();
        switched.render_frame();
        let _ = kept.get_audio_samples();
        assert!(
            switched.get_audio_samples().is_empty(),
            "frame {frame}: the host got sound with the sound off"
        );
    }
    switched.set_audio_enabled(true);
    kept.render_frame();
    switched.render_frame();
    let channels = switched.get_audio_info().channels as usize;
    let one_frame = kept.get_audio_samples().len();
    let queued = switched.get_audio_samples().to_vec();
    assert!(
        one_frame > 0,
        "the machine that kept its sound queued a frame"
    );
    assert!(
        queued.len().abs_diff(one_frame) <= channels,
        "{} samples queued, the frame is {one_frame}",
        queued.len()
    );
    let mut peak = queued
        .iter()
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    for _ in 0..FRAMES {
        switched.render_frame();
        peak = peak.max(host_peak(&mut switched));
    }
    assert!(
        peak >= SOUND_PEAK,
        "switched back on, the machine made no sound (peak {peak})"
    );
}

/// Stems change nothing: with every stem armed and taken after each frame,
/// each frame's state hash, the host's samples and their count are those
/// of a machine with no stem armed.
pub fn stems_change_nothing<E: SoundCore>(core: &SoundCoreUnderTest<E>) {
    let mut armed = core.playing();
    let mut plain = core.playing();
    let layout = layout_of(&armed);
    armed.arm_stems(layout.all());
    let mut taken = 0u32;
    for frame in 1..=FRAMES {
        armed.render_frame();
        plain.render_frame();
        taken += armed.take_stems().expect("stems armed").frames;
        assert_eq!(
            armed.state_hash(),
            plain.state_hash(),
            "frame {frame}: the machine with every stem armed parted from the plain one"
        );
        assert_eq!(
            armed.get_audio_samples().to_vec(),
            plain.get_audio_samples().to_vec(),
            "frame {frame}: the host heard something else with every stem armed"
        );
    }
    assert!(taken > 0, "the stems recorded the frames");
}

/// Stems cover whole frames: armed while a frame runs, they start at the
/// next frame's start; every stem and the master hold as many sample
/// frames; and a frame the host is not shown is left out.
pub fn stems_cover_whole_frames<E: SoundCore>(core: &SoundCoreUnderTest<E>) {
    let mut emu = core.playing();
    let layout = layout_of(&emu);
    // A frame begun and left running: the main processor stepped.
    for _ in 0..16 {
        emu.step_cpu(0);
    }
    emu.arm_stems(layout.all());
    let take = emu.take_stems().expect("stems armed");
    assert_eq!(
        take.frames, 0,
        "armed during a frame, nothing before its end"
    );
    emu.render_frame();
    let take = emu.take_stems().expect("stems armed");
    assert_eq!(take.frames, 0, "the frame running when armed is not taken");
    emu.render_frame();
    let take = emu.take_stems().expect("stems armed");
    assert!(take.frames > 0, "a whole frame was taken");
    assert_eq!(take.armed, layout.all());
    assert_whole(&take, &layout, "a frame");
    let one = take.frames;
    emu.run_frame_headless();
    emu.render_frame();
    let take = emu.take_stems().expect("stems armed");
    assert_whole(&take, &layout, "a headless frame, then a frame");
    assert!(
        take.frames <= one + one / 8 && take.frames + take.frames / 8 >= one,
        "a headless frame is left out: {} sample frames after it, {one} for one frame",
        take.frames
    );
    emu.arm_stems(StemSet::EMPTY);
    assert_eq!(emu.take_stems(), None, "disarmed, nothing is held");
}

/// Stems sum to the master: on every sample frame the take counts no
/// departure, the sum of the stems that are the master's parts, a mono
/// stem added to both sides, is the master within the layout's rule; and
/// the check has something to check.
pub fn stems_sum_to_the_master<E: SoundCore>(core: &SoundCoreUnderTest<E>) {
    let mut emu = core.playing();
    let layout = layout_of(&emu);
    emu.arm_stems(layout.all());
    emu.render_frame();
    let _ = emu.take_stems();
    let tolerance = layout.sum.tolerance(layout.kind);
    let unit = match layout.kind {
        SampleKind::I16 => 1.0 / 32768.0,
        SampleKind::F32 => 1.0,
    };
    let (mut checked, mut loudest) = (0u64, 0.0f64);
    for frame in 1..=FRAMES {
        emu.render_frame();
        let take = emu.take_stems().expect("stems armed");
        assert_whole(&take, &layout, "a frame");
        for sample in 0..take.frames {
            if take.departs(sample) {
                continue;
            }
            for side in 0..2usize {
                let mut sum = 0.0f64;
                for (stem, index) in take.stems.iter().zip(take.armed.iter()) {
                    if !layout.channels[index].in_master {
                        continue;
                    }
                    let width = layout.channels[index].width();
                    let at = sample as usize * width + side.min(width - 1);
                    sum += stem.value(at);
                }
                let master = take.master.value(sample as usize * 2 + side);
                loudest = loudest.max(master.abs() * unit);
                assert!(
                    (master - sum).abs() <= tolerance,
                    "frame {frame}, sample frame {sample}, side {side}: the master is {master}, \
                     the stems sum to {sum} ({:?})",
                    layout.sum
                );
                checked += 1;
            }
        }
    }
    assert!(
        checked > 0,
        "every sample frame departed: nothing was checked"
    );
    assert!(
        loudest >= f64::from(SOUND_PEAK),
        "the master was silence (peak {loudest}): the check has nothing to bite on"
    );
}

/// What a log holds in the checks: everything.
const EVERYTHING: SoundLogContents = SoundLogContents {
    writes: true,
    keys: true,
    mailbox: true,
};

/// The log changes nothing: with every write, key and mailbox's write
/// logged and the log taken after each frame, each frame's state hash, the
/// host's samples and their count are those of a machine with no log.
pub fn the_log_changes_nothing<E: SoundCore>(core: &SoundCoreUnderTest<E>) {
    let mut logged = core.playing();
    let mut plain = core.playing();
    logged.start_sound_log(EVERYTHING);
    let mut entries = 0;
    for frame in 1..=FRAMES {
        logged.render_frame();
        plain.render_frame();
        entries += logged.take_sound_log().expect("a log runs").entries.len();
        assert_eq!(
            logged.state_hash(),
            plain.state_hash(),
            "frame {frame}: the machine with the log on parted from the plain one"
        );
        assert_eq!(
            logged.get_audio_samples().to_vec(),
            plain.get_audio_samples().to_vec(),
            "frame {frame}: the host heard something else with the log on"
        );
    }
    assert!(entries > 0, "the log took the frames down");
}

/// What a log holds in the checks of the keys: the keys alone.
const KEYS: SoundLogContents = SoundLogContents {
    writes: false,
    keys: true,
    mailbox: false,
};

/// The log starts with the chips' registers, every one at time 0, before
/// anything else (then a key-on for each channel sounding, none before the
/// sound moment); the writes after them are the sound moment's, at their
/// time; the times never go back; and every frame ends with an entry.
pub fn the_log_starts_with_the_image_and_never_goes_back<E: SoundCore>(
    core: &SoundCoreUnderTest<E>,
) {
    let mut emu = (core.build)();
    for _ in 0..WARM_UP {
        emu.run_frame();
    }
    let chips = emu.sound_log_chips();
    assert!(!chips.is_empty(), "the console logs its sound chips");
    emu.start_sound_log(EVERYTHING);
    (core.sound_moment)(&mut emu);
    let mut logs: Vec<SoundLog> = Vec::new();
    for _ in 0..FRAMES {
        emu.render_frame();
        logs.push(emu.take_sound_log().expect("a log runs"));
    }
    let first = &logs[0];
    assert!(
        first.image_len > 0,
        "the log starts with the chips' registers"
    );
    assert!(first.clock_hz > 0);
    for entry in &first.entries[..first.image_len] {
        assert_eq!(entry.time, 0, "the registers stand at time 0");
        assert!(
            matches!(
                entry.event,
                SoundEvent::Write { .. } | SoundEvent::KeyOn { .. }
            ),
            "the image is the registers and the sounding channels' key-ons"
        );
        assert!(
            usize::from(entry.chip) < chips.len(),
            "an entry names a chip of the list"
        );
    }
    let writes_after = first.entries[first.image_len..]
        .iter()
        .filter(|entry| matches!(entry.event, SoundEvent::Write { .. }))
        .count();
    assert!(writes_after > 0, "the sound moment's writes are logged");
    let mut latest = 0;
    let mut frame_ends = 0;
    for (take, log) in logs.iter().enumerate() {
        assert!(
            take == 0 || log.image_len == 0,
            "only the first take starts with the image"
        );
        assert!(!log.interrupted, "nothing interrupted the log");
        for entry in &log.entries {
            assert!(
                entry.time >= latest,
                "take {take}: the time went back from {latest} to {}",
                entry.time
            );
            latest = entry.time;
            frame_ends += usize::from(entry.event == SoundEvent::FrameEnd);
        }
    }
    assert_eq!(frame_ends, FRAMES as usize, "a frame's end for every frame");
    emu.stop_sound_log();
    assert_eq!(
        emu.take_sound_log(),
        None,
        "stopped and taken, nothing waits"
    );
}

/// The log keeps the machine's time: each frame's end comes as many of the
/// log's ticks after the one before as the frame ran master clocks, to
/// within one tick, however long the log runs. A frame's end at a sound
/// chip's own count would part from the machine's time wherever that chip
/// is clocked a little more or less than its share.
pub fn the_log_keeps_the_machine_s_time<E: SoundCore>(core: &SoundCoreUnderTest<E>) {
    let mut emu = core.playing();
    let master_hz = i128::from(emu.master_clock_hz());
    emu.start_sound_log(EVERYTHING);
    let mut last: Option<(u64, u64)> = None;
    for frame in 0..FRAMES {
        emu.render_frame();
        let master = emu.master_cycles();
        let log = emu.take_sound_log().expect("a log runs");
        let end = log
            .entries
            .iter()
            .rev()
            .find(|entry| entry.event == SoundEvent::FrameEnd)
            .expect("the frame's end")
            .time;
        if let Some((last_end, last_master)) = last {
            // Both in the log's ticks times the master clock's rate.
            let ticks = i128::from(end - last_end) * master_hz;
            let ran = i128::from(master - last_master) * i128::from(log.clock_hz);
            assert!(
                (ticks - ran).abs() < master_hz,
                "frame {frame}: its end came {} ticks after the one before, \
                 where the machine ran {} master clocks ({:.1} ticks)",
                end - last_end,
                master - last_master,
                ran as f64 / master_hz as f64
            );
        }
        last = Some((end, master));
    }
}

/// A load between two takes marks the later one interrupted, and the log
/// goes on, its time still moving forward, the new chips' registers first.
pub fn a_load_interrupts_the_log<E: SoundCore>(core: &SoundCoreUnderTest<E>) {
    let mut emu = core.playing();
    emu.start_sound_log(EVERYTHING);
    emu.render_frame();
    let before = emu.take_sound_log().expect("a log runs");
    assert!(!before.interrupted);
    let latest = before.entries.last().expect("a frame's end").time;
    emu.load_rom(&core.other_rom)
        .expect("the other program loads");
    emu.render_frame();
    let after = emu.take_sound_log().expect("a log runs");
    assert!(after.interrupted, "the load is said");
    assert!(
        after
            .entries
            .first()
            .is_some_and(|entry| entry.time >= latest),
        "the time goes on from {latest}"
    );
    assert!(
        after
            .entries
            .iter()
            .any(|entry| matches!(entry.event, SoundEvent::Write { .. })),
        "the new chips' registers are logged"
    );
    emu.render_frame();
    let next = emu.take_sound_log().expect("a log runs");
    assert!(!next.interrupted, "the take after says nothing came");
}

/// A log of the keys holds no write and no reset. Started before the sound
/// moment, its image is empty (nothing sounds yet) and the moment's notes
/// come after it as key-ons, at no earlier time; started while the notes
/// sound, its image is their key-ons, every one at time 0.
pub fn the_log_takes_the_keys_down<E: SoundCore>(core: &SoundCoreUnderTest<E>) {
    let no_writes = |log: &SoundLog, when: &str| {
        assert!(
            !log.entries
                .iter()
                .any(|entry| matches!(entry.event, SoundEvent::Write { .. } | SoundEvent::Reset)),
            "{when}: a log of the keys took a write down"
        );
    };
    let mut emu = (core.build)();
    for _ in 0..WARM_UP {
        emu.run_frame();
    }
    emu.start_sound_log(KEYS);
    (core.sound_moment)(&mut emu);
    let mut key_ons = 0;
    let mut latest = 0;
    for frame in 0..FRAMES {
        emu.render_frame();
        let log = emu.take_sound_log().expect("a log runs");
        no_writes(&log, "before the sound moment");
        if frame == 0 {
            assert_eq!(log.image_len, 0, "nothing sounded when the log began");
        }
        for entry in &log.entries {
            assert!(entry.time >= latest, "the time went back");
            latest = entry.time;
            key_ons += usize::from(matches!(entry.event, SoundEvent::KeyOn { .. }));
        }
    }
    assert!(key_ons > 0, "the sound moment's notes are logged");

    let mut emu = core.playing();
    emu.render_frame();
    emu.start_sound_log(KEYS);
    emu.render_frame();
    let log = emu.take_sound_log().expect("a log runs");
    no_writes(&log, "while the notes sound");
    assert!(log.image_len > 0, "the notes sounding make the image");
    for entry in &log.entries[..log.image_len] {
        assert!(
            matches!(entry.event, SoundEvent::KeyOn { .. }) && entry.time == 0,
            "the image is key-ons at time 0"
        );
    }
}

/// The mailbox's writes in `log`, as (time, port, value).
fn mailbox_of(log: &SoundLog) -> Vec<(u64, u16, u8)> {
    log.entries
        .iter()
        .filter_map(|entry| match entry.event {
            SoundEvent::Mailbox { port, value } => {
                assert_eq!(entry.chip, 0, "a mailbox's write names the first chip");
                Some((entry.time, port, value))
            }
            _ => None,
        })
        .collect()
}

/// A log of the mailbox takes down each value the main processor writes
/// into its sound processor's mailbox: with everything logged on the
/// mailbox program, each frame's writes are at the program's port, each
/// value one more than the one before, spread through the frame and each
/// at a time between the end of the frame before and its own frame's end;
/// the log changes nothing the machine holds or the host hears; a log of
/// the mailbox alone holds nothing else but the frames' ends and starts
/// with no image; and a log without the mailbox takes none of it down.
pub fn the_log_takes_the_mailbox_down<E: SoundCore>(core: &SoundCoreUnderTest<E>) {
    let running = || {
        let mut emu = (core.build)();
        emu.load_rom(&core.mailbox_rom)
            .expect("the mailbox program loads");
        for _ in 0..WARM_UP {
            emu.run_frame();
        }
        emu
    };
    let mut logged = running();
    let mut plain = running();
    logged.start_sound_log(EVERYTHING);
    let (mut last_end, mut last_value) = (0u64, None::<u8>);
    for frame in 1..=FRAMES {
        logged.render_frame();
        plain.render_frame();
        let log = logged.take_sound_log().expect("a log runs");
        assert_eq!(
            logged.state_hash(),
            plain.state_hash(),
            "frame {frame}: the machine with the mailbox logged parted from the plain one"
        );
        assert_eq!(
            logged.get_audio_samples().to_vec(),
            plain.get_audio_samples().to_vec(),
            "frame {frame}: the host heard something else with the mailbox logged"
        );
        let end = log
            .entries
            .iter()
            .rev()
            .find(|entry| entry.event == SoundEvent::FrameEnd)
            .expect("the frame's end")
            .time;
        let writes = mailbox_of(&log);
        assert!(
            writes.len() >= 2,
            "frame {frame}: {} of the program's writes into the mailbox",
            writes.len()
        );
        let mut latest = last_end;
        for &(time, port, value) in &writes {
            assert_eq!(port, core.mailbox_port, "frame {frame}: the program's port");
            assert!(
                time >= latest && time <= end,
                "frame {frame}: a write at {time}, after {latest}, in a frame ending at {end}"
            );
            if let Some(last) = last_value {
                assert_eq!(
                    value,
                    last.wrapping_add(1),
                    "frame {frame}: each value one more than the one before"
                );
            }
            latest = time;
            last_value = Some(value);
        }
        assert!(
            writes[0].0 < writes[writes.len() - 1].0,
            "frame {frame}: the writes are spread through the frame"
        );
        last_end = end;
    }

    let mut alone = running();
    alone.start_sound_log(SoundLogContents {
        mailbox: true,
        ..SoundLogContents::default()
    });
    alone.render_frame();
    let log = alone.take_sound_log().expect("a log runs");
    assert_eq!(log.image_len, 0, "the mailbox has no image");
    assert!(
        log.entries.iter().all(|entry| matches!(
            entry.event,
            SoundEvent::Mailbox { .. } | SoundEvent::FrameEnd
        )),
        "a log of the mailbox alone took a chip's write or key down"
    );
    assert!(!mailbox_of(&log).is_empty(), "the mailbox alone is logged");

    let mut without = running();
    without.start_sound_log(SoundLogContents {
        mailbox: false,
        ..EVERYTHING
    });
    for frame in 1..=FRAMES {
        without.render_frame();
        let log = without.take_sound_log().expect("a log runs");
        assert!(
            mailbox_of(&log).is_empty(),
            "frame {frame}: a log without the mailbox took a write into it down"
        );
    }
}
