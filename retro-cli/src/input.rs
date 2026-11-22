//! Scripted player-1 input for a command that runs frames, in the grammar
//! of [`retro_core::input_schedule`] over the console's button names
//! ([`GoldenConsole::BUTTONS`]).

use std::collections::BTreeSet;

use anyhow::Result;
use retro_core::capture::CaptureCore;
use retro_core::golden::GoldenConsole;
use retro_core::input_schedule::{self, PressSchedule};
use retro_core::{EmulatorCore, InputState};

/// The input flags of a command that runs frames. Their help names the
/// console's buttons ([`crate::command`] writes it).
#[derive(clap::Args, Clone, Debug, Default)]
pub struct InputArgs {
    /// Frame-scheduled player-1 input: `"F:buttons;F:buttons"`.
    #[arg(long)]
    pub press_at: Option<String>,
    /// Hold these player-1 buttons every frame.
    #[arg(long)]
    pub press_hold: Option<String>,
    /// Pulse these player-1 buttons on and off every 6 frames.
    #[arg(long)]
    pub press: Option<String>,
    /// Reset the console before these frames.
    #[arg(long)]
    pub reset_at: Option<String>,
}

impl InputArgs {
    /// The input these flags describe, the button names `E`'s.
    pub fn plan<E: GoldenConsole>(&self) -> Result<InputPlan> {
        Ok(InputPlan {
            schedule: parse_press_schedule::<E>(self.press_at.as_deref())?,
            hold: parse_buttons::<E>(self.press_hold.as_deref())?,
            pulse: parse_buttons::<E>(self.press.as_deref())?,
            resets: parse_reset_frames(self.reset_at.as_deref())?,
        })
    }
}

/// How a run of frames ends: its last frame run whole, or held at its
/// capture boundary ([`CaptureCore::run_to_capture_boundary`]), its picture
/// drawn and its vertical blank not begun. What a command reads after a run
/// held there (the picture, the video memory, the palette, the video chip's
/// registers) is what the frame shown was drawn from, before the uploads
/// the game makes in its vertical blank for the next frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunEnd {
    /// The last frame runs whole.
    WholeFrame,
    /// The last frame stops at its capture boundary.
    CaptureBoundary,
}

impl RunEnd {
    /// Whether frame `frame` of a run of `frames` (the first is 0) stops at
    /// its capture boundary: the last one, when the run ends there.
    pub fn holds(self, frame: u64, frames: u64) -> bool {
        self == RunEnd::CaptureBoundary && frame + 1 == frames
    }

    /// Run frame `frame` of a run of `frames`: whole, or to its capture
    /// boundary when [`Self::holds`] says so.
    pub fn run_frame<E: CaptureCore>(self, emu: &mut E, frame: u64, frames: u64) {
        if self.holds(frame, frames) {
            emu.run_to_capture_boundary();
        } else {
            emu.run_frame();
        }
    }
}

/// Scripted input, applied before each frame of a run.
#[derive(Clone, Debug, Default)]
pub struct InputPlan {
    schedule: Option<PressSchedule>,
    hold: Option<u32>,
    pulse: Option<u32>,
    resets: BTreeSet<u64>,
}

impl InputPlan {
    /// Apply what falls on `frame` (counted from the run's first frame): a
    /// reset asked for there, then player 1's buttons ([`Self::buttons_at`]).
    pub fn before_frame<E: EmulatorCore>(&self, emu: &mut E, frame: u64) {
        if self.resets.contains(&frame) {
            emu.reset();
        }
        if let Some(buttons) = self.buttons_at(frame) {
            emu.set_input(0, InputState { buttons });
        }
    }

    /// Run `frames` frames of `emu`, each with this input applied before it
    /// ([`Self::before_frame`]), the last one ending as `end` says.
    pub fn run_frames<E: CaptureCore>(&self, emu: &mut E, frames: u64, end: RunEnd) {
        for frame in 0..frames {
            self.before_frame(emu, frame);
            end.run_frame(emu, frame, frames);
        }
    }

    /// The buttons player 1 gets on `frame`, `None` to leave the input as
    /// it is. A schedule rules alone (before its first set-point it leaves
    /// the input as it is); otherwise held buttons, then pulsed ones (held
    /// for 6 frames, released for 6); with none, nothing.
    pub fn buttons_at(&self, frame: u64) -> Option<u32> {
        if let Some(schedule) = &self.schedule {
            return schedule.mask_at(frame);
        }
        match (self.hold, self.pulse) {
            (Some(held), _) => Some(held),
            (None, Some(pulsed)) if (frame / 6).is_multiple_of(2) => Some(pulsed),
            (None, Some(_)) => Some(0),
            (None, None) => None,
        }
    }
}

/// Install the buttons `schedule` gives `frame` on player 1, from its
/// first set-point on.
pub fn apply_schedule<E: EmulatorCore>(emu: &mut E, schedule: &PressSchedule, frame: u64) {
    if let Some(buttons) = schedule.mask_at(frame) {
        emu.set_input(0, InputState { buttons });
    }
}

/// A button list in `E`'s names; `None` when none was given.
pub fn parse_buttons<E: GoldenConsole>(spec: Option<&str>) -> Result<Option<u32>> {
    input_schedule::parse_button_mask(spec, E::BUTTONS).map_err(anyhow::Error::msg)
}

/// A `"F:buttons;F:buttons"` schedule in `E`'s names; `None` when none was
/// given.
pub fn parse_press_schedule<E: GoldenConsole>(spec: Option<&str>) -> Result<Option<PressSchedule>> {
    input_schedule::parse_press_schedule(spec, E::BUTTONS).map_err(anyhow::Error::msg)
}

/// A `"360,720"` list of the frames before which the console is reset.
pub fn parse_reset_frames(spec: Option<&str>) -> Result<BTreeSet<u64>> {
    input_schedule::parse_reset_frames(spec).map_err(anyhow::Error::msg)
}

/// The button names of `buttons`, comma-separated, as a help text lists
/// them.
pub fn button_names(buttons: &[(&str, u32)]) -> String {
    buttons
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAD: [(&str, u32); 3] = [("up", 0), ("a", 4), ("start", 7)];

    fn plan(press_at: Option<&str>, hold: Option<&str>, press: Option<&str>) -> InputPlan {
        InputPlan {
            schedule: input_schedule::parse_press_schedule(press_at, &PAD).unwrap(),
            hold: input_schedule::parse_button_mask(hold, &PAD).unwrap(),
            pulse: input_schedule::parse_button_mask(press, &PAD).unwrap(),
            resets: BTreeSet::new(),
        }
    }

    #[test]
    fn a_schedule_rules_over_held_and_pulsed_buttons() {
        let p = plan(Some("10:start;12:"), Some("a"), Some("up"));
        assert_eq!(p.buttons_at(9), None);
        assert_eq!(p.buttons_at(10), Some(1 << 7));
        assert_eq!(p.buttons_at(12), Some(0));
    }

    #[test]
    fn held_buttons_rule_over_pulsed_ones_and_pulses_take_six_frames() {
        assert_eq!(
            plan(None, Some("a"), Some("up")).buttons_at(7),
            Some(1 << 4)
        );
        let pulsed = plan(None, None, Some("up"));
        let frames: Vec<Option<u32>> = [0, 5, 6, 11, 12]
            .iter()
            .map(|&f| pulsed.buttons_at(f))
            .collect();
        assert_eq!(frames, vec![Some(1), Some(1), Some(0), Some(0), Some(1)]);
        assert_eq!(plan(None, None, None).buttons_at(3), None);
    }

    #[test]
    fn a_run_held_at_its_capture_boundary_holds_its_last_frame_alone() {
        let held = RunEnd::CaptureBoundary;
        assert!(!held.holds(0, 3) && !held.holds(1, 3));
        assert!(held.holds(2, 3), "the last frame");
        assert!(held.holds(0, 1), "a run of one frame");
        assert!(!RunEnd::WholeFrame.holds(2, 3));
    }

    #[test]
    fn a_help_lists_the_button_names() {
        assert_eq!(button_names(&PAD), "up,a,start");
    }
}
