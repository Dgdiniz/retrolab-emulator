//! [`SoundCore`], the sound hooks a host calls on a machine, and
//! [`render_frames`], the render a host runs over them.

use serde::Serialize;

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
