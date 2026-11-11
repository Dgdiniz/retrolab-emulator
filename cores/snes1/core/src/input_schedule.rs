//! Scripted controller input for a SNES run: the shared press and reset
//! grammar of [`retro_core::input_schedule`] with the SNES pad's button
//! names. The CLI's `--press-at` / `--reset-at` and the golden-run
//! manifests use it.

pub use retro_core::input_schedule::{parse_reset_frames, PressSchedule};

/// Button names in the order of the generic input bits
/// (`InputState::buttons`), matching the `BTN_*` constants of
/// [`crate::controller`].
pub const BUTTON_NAMES: [&str; 12] = [
    "up", "down", "left", "right", "a", "b", "x", "y", "l", "r", "start", "select",
];

/// The same names with their bit, as the shared grammar takes them.
pub const BUTTONS: [(&str, u32); 12] = {
    let mut table = [("", 0u32); 12];
    let mut i = 0;
    while i < 12 {
        table[i] = (BUTTON_NAMES[i], i as u32);
        i += 1;
    }
    table
};

/// Parse a button list into the generic input bitmask. Returns
/// `Ok(None)` when no spec was given; an unknown name is an error so a
/// typo fails loudly instead of silently pressing nothing.
pub fn parse_button_mask(spec: Option<&str>) -> Result<Option<u32>, String> {
    retro_core::input_schedule::parse_button_mask(spec, &BUTTONS)
}

/// Parse a `"F:buttons;F:buttons"` spec into a [`PressSchedule`]. Returns
/// `Ok(None)` when no spec (or only separators) was given. A malformed
/// segment, a non-numeric frame or an unknown button name is an error.
pub fn parse_press_schedule(spec: Option<&str>) -> Result<Option<PressSchedule>, String> {
    retro_core::input_schedule::parse_press_schedule(spec, &BUTTONS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_names_follow_the_controller_constants() {
        use crate::controller::*;
        let expected = [
            BTN_UP, BTN_DOWN, BTN_LEFT, BTN_RIGHT, BTN_A, BTN_B, BTN_X, BTN_Y, BTN_L, BTN_R,
            BTN_START, BTN_SELECT,
        ];
        for (i, bit) in expected.iter().enumerate() {
            assert_eq!(
                parse_button_mask(Some(BUTTON_NAMES[i])).unwrap(),
                Some(1 << bit),
                "{}",
                BUTTON_NAMES[i]
            );
        }
    }

    #[test]
    fn the_snes_names_drive_the_shared_grammar() {
        let mask = parse_button_mask(Some(" Start , A ,b")).unwrap().unwrap();
        assert_eq!(mask, (1 << 10) | (1 << 4) | (1 << 5));
        assert!(parse_button_mask(Some("turbo")).is_err());
        let sched = parse_press_schedule(Some("186:;180:start"))
            .unwrap()
            .unwrap();
        assert_eq!(sched.points(), &[(180, 1 << 10), (186, 0)]);
        assert!(
            parse_press_schedule(Some("12:c")).is_err(),
            "no C on a SNES pad"
        );
    }
}
