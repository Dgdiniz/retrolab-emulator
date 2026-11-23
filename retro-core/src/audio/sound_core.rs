//! [`SoundCore`], the sound hooks a host calls on a machine, and
//! [`render_frames`], the render a host runs over them.

use serde::Serialize;

use crate::audio::sound_log::{SoundChipKind, SoundLog, SoundLogContents};
use crate::audio::stems::{StemLayout, StemSet, StemTake};
use crate::EmulatorCore;

/// The sound hooks a host calls on a machine, beside the capture's
/// [`crate::capture::CaptureCore`]. Each core implements it in its
/// `system/audio.rs`.
pub trait SoundCore: EmulatorCore {
    /// One frame of the loop a player's host runs, whatever the debugger
    /// has armed: no breakpoint, watchpoint, event or trace stops or slows
    /// it. Each core writes it in one line, `run_frame_on(self, Play)`, as
    /// it writes `run_frame`, so the frame loop is compiled in the core's
    /// crate and not again in a binding that calls it.
    fn render_frame(&mut self);

    /// The stems the machine takes, `None` for a console that takes none.
    fn stem_layout(&self) -> Option<StemLayout> {
        None
    }

    /// Record `stems`, each one's samples and the master's, from the next
    /// frame's start, or from now while the machine stands between two
    /// frames; an empty set disarms. Arming another set drops what was
    /// held and not taken.
    fn arm_stems(&mut self, stems: StemSet) {
        let _ = stems;
    }

    /// What was recorded since the last take, by whole frames; `None`
    /// while nothing is armed or held.
    fn take_stems(&mut self) -> Option<StemTake> {
        None
    }

    /// The sound chips a sound log names, an entry's chip their index:
    /// none on a console that logs none.
    fn sound_log_chips(&self) -> Vec<SoundChipKind> {
        Vec::new()
    }

    /// Start a sound log holding `contents`, its first entries, at time 0,
    /// the chips as they stand: their registers in a log of the writes, a
    /// key-on for each channel sounding in a log of the keys. What an
    /// earlier log left is dropped. Contents of nothing stop the log.
    fn start_sound_log(&mut self, contents: SoundLogContents) {
        let _ = contents;
    }

    /// Stop the log; what it logged waits for the next take.
    fn stop_sound_log(&mut self) {}

    /// What the log took down since the last take, by whole frames; `None`
    /// while no log runs and nothing waits.
    fn take_sound_log(&mut self) -> Option<SoundLog> {
        None
    }
}

/// What [`render_frames`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RenderReport {
    /// The frames it ran.
    pub frames: u32,
    /// Whether the sound was off and had to be turned on: it stays on.
    pub sound_turned_on: bool,
}

/// Run `frames` frames of [`SoundCore::render_frame`] with the sound on,
/// turning it on first if it is off. The samples go to the host's ring as
/// a frame call's do: what the caller does not take, the ring drops from
/// its front.
pub fn render_frames<M: SoundCore>(machine: &mut M, frames: u32) -> RenderReport {
    let sound_turned_on = !machine.is_audio_enabled();
    if sound_turned_on {
        machine.set_audio_enabled(true);
    }
    for _ in 0..frames {
        machine.render_frame();
    }
    RenderReport {
        frames,
        sound_turned_on,
    }
}
