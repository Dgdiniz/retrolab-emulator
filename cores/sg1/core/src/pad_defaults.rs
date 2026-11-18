//! How the Mega Drive's pad is played from a keyboard and a gamepad out of
//! the box: retro-core's input manager starts with these maps, and the
//! player can change them. The bits are the six-button pad's
//! ([`Genesis6ButtonDevice`]'s `BTN_*`), which the three-button pad shares.

use std::collections::HashMap;

use retro_core::input::PadDefaults;

use crate::io::devices::Genesis6ButtonDevice as Pad;

/// The keyboard: the arrows the directions, A S D the A B C buttons, Q W E
/// the X Y Z buttons, Enter Start, left Shift Mode. The gamepad: the face
/// buttons A B C X (bottom, right, left, top), the shoulders Y Z, Start and
/// Select Start and Mode, the pad's directions the directions.
pub fn pad_defaults() -> PadDefaults {
    let keyboard: HashMap<String, u32> = [
        ("ArrowUp", Pad::BTN_UP),
        ("ArrowDown", Pad::BTN_DOWN),
        ("ArrowLeft", Pad::BTN_LEFT),
        ("ArrowRight", Pad::BTN_RIGHT),
        ("KeyA", Pad::BTN_A),
        ("KeyS", Pad::BTN_B),
        ("KeyD", Pad::BTN_C),
        ("KeyQ", Pad::BTN_X),
        ("KeyW", Pad::BTN_Y),
        ("KeyE", Pad::BTN_Z),
        ("Enter", Pad::BTN_START),
        ("ShiftLeft", Pad::BTN_MODE),
    ]
    .into_iter()
    .map(|(key, bit)| (key.to_string(), bit))
    .collect();
    let gamepad = HashMap::from([
        (0, Pad::BTN_A),
        (1, Pad::BTN_B),
        (2, Pad::BTN_C),
        (3, Pad::BTN_X),
        (4, Pad::BTN_Y),
        (5, Pad::BTN_Z),
        (9, Pad::BTN_START),
        (8, Pad::BTN_MODE),
        (12, Pad::BTN_UP),
        (13, Pad::BTN_DOWN),
        (14, Pad::BTN_LEFT),
        (15, Pad::BTN_RIGHT),
    ]);
    PadDefaults { keyboard, gamepad }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::input::GenericInputManager;

    #[test]
    fn the_keyboard_plays_player_one_with_the_pads_bits() {
        let mut manager = GenericInputManager::new(2, pad_defaults());
        assert_eq!(manager.get_port_assignment(0), Some("keyboard".to_string()));
        assert_eq!(manager.get_port_assignment(1), None);
        manager.update_keyboard("KeyA".to_string(), true);
        assert_eq!(manager.get_port_state(0), 1 << Pad::BTN_A);
        manager.update_keyboard("KeyA".to_string(), false);
        manager.update_keyboard("Enter".to_string(), true);
        assert_eq!(manager.get_port_state(0), 1 << Pad::BTN_START);
    }

    #[test]
    fn a_gamepad_joins_player_one_with_the_pads_bits() {
        let mut manager = GenericInputManager::new(2, pad_defaults());
        manager.update_gamepad_button("gamepad-0".to_string(), 0, true);
        assert_eq!(manager.provider_port("gamepad-0"), Some(0));
        assert_eq!(manager.get_port_state(0), 1 << Pad::BTN_A);
    }
}
