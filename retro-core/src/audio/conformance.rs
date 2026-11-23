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
//! nothing the machine holds, and it lasts until the host lifts it.

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
