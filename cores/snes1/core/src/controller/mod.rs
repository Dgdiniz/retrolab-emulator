//! Controller subsystem.
//!
//! Two controller ports, read by the auto-joypad read (into `$4218-$421B`)
//! and by the manual serial protocol via `$4016`/`$4017`. The host feeds
//! a per-port bitmask through [`retro_core::input::GenericInputManager`];
//! [`input_state_to_snes_pad`] converts that generic mask into the
//! 16-bit SNES hardware layout the [`StandardPad`] shift register
//! exposes, and [`pad_defaults`] is the keyboard and pad map a new
//! manager starts with.
//!
//! ## Hardware bit layout
//!
//! The SNES controller serialises its buttons MSB-first in this fixed
//! order (one bit per clock):
//!
//! ```text
//! bit:  15 14 13 12 11 10  9  8  7  6  5  4  3  2  1  0
//! btn:   B  Y  Se St Up Dn Lf Rt  A  X  L  R  0  0  0  0
//! ```
//!
//! Auto-read latches all 16 bits at once into `$4218/$4219` (JOY1) and
//! `$421A/$421B` (JOY2); the low nibble (bits 0-3) is the controller ID
//! and reads as 0 for a standard pad.

use retro_core::input::PadDefaults;
use std::collections::HashMap;

/// The standard 12-button SNES controller (SNS-005): retro-core's serial
/// pad, 16 bits long. The latch line resets the bit cursor and each read
/// clocks the next bit out of the shift register (checked against
/// bsnes-plus); the buttons are taken into the register on the latch's
/// high-to-low edge rather than polled per bit, which is the same because
/// the host samples input once per frame. After the auto-joypad read the
/// register is left exhausted (`exhaust_serial`), so a manual `$4016` read
/// without a new latch gives the idle `1`: Dark Law polls `$4016` that
/// way, with the hardware auto-read on.
pub type StandardPad = retro_core::input::SerialPad<16>;

// -- Generic input bitmask layout -----------------------------------
//
// Bit indices of the per-port mask carried by
// `retro_core::InputState::buttons`. The host (and the emulator's
// `InputInfo::button_layout`) agree on these so the frontend can map
// physical devices uniformly. D-pad first (0-3) matches the SG1 core's
// layout; the remaining indices cover the SNES-specific face/shoulder
// buttons.

/// D-pad up.
pub const BTN_UP: u32 = 0;
/// D-pad down.
pub const BTN_DOWN: u32 = 1;
/// D-pad left.
pub const BTN_LEFT: u32 = 2;
/// D-pad right.
pub const BTN_RIGHT: u32 = 3;
/// Right face button (`A`).
pub const BTN_A: u32 = 4;
/// Bottom face button (`B`).
pub const BTN_B: u32 = 5;
/// Top face button (`X`).
pub const BTN_X: u32 = 6;
/// Left face button (`Y`).
pub const BTN_Y: u32 = 7;
/// Left shoulder (`L`).
pub const BTN_L: u32 = 8;
/// Right shoulder (`R`).
pub const BTN_R: u32 = 9;
/// `Start`.
pub const BTN_START: u32 = 10;
/// `Select`.
pub const BTN_SELECT: u32 = 11;

/// Convert a generic per-port input bitmask (see the `BTN_*` indices)
/// into the 16-bit SNES hardware layout consumed by [`StandardPad`].
///
/// Opposing directions (`Up`+`Down`, `Left`+`Right`) are passed through
/// unfiltered, matching real hardware, a few games rely on the glitchy
/// behaviour and the controller itself does not arbitrate.
pub fn input_state_to_snes_pad(mask: u32) -> u16 {
    let on = |index: u32, bit: u16| if mask & (1 << index) != 0 { bit } else { 0 };
    on(BTN_B, 0x8000)
        | on(BTN_Y, 0x4000)
        | on(BTN_SELECT, 0x2000)
        | on(BTN_START, 0x1000)
        | on(BTN_UP, 0x0800)
        | on(BTN_DOWN, 0x0400)
        | on(BTN_LEFT, 0x0200)
        | on(BTN_RIGHT, 0x0100)
        | on(BTN_A, 0x0080)
        | on(BTN_X, 0x0040)
        | on(BTN_L, 0x0020)
        | on(BTN_R, 0x0010)
}

/// The maps a new input manager starts with: [`default_keyboard_mapping`]
/// for the keyboard, [`default_gamepad_mapping`] for every pad.
pub fn pad_defaults() -> PadDefaults {
    PadDefaults {
        keyboard: default_keyboard_mapping(),
        gamepad: default_gamepad_mapping(),
    }
}

/// Default host-keyboard mapping for the standard pad, keyed by
/// `KeyboardEvent.code` and valued by the generic `BTN_*` bit index.
///
/// Mirrors the RetroPad keyboard layout the platform's LibRetro path
/// ships, so the native snes1 core and the snes9x
/// LibRetro core feel identical out of the box: arrows drive the D-pad,
/// `Z`/`X` are the bottom/right face buttons (`B`/`A`), `A`/`S` are the
/// left/top face buttons (`Y`/`X`), `Q`/`W` are the shoulders, and
/// `Enter`/`RightShift` are Start/Select.
pub fn default_keyboard_mapping() -> HashMap<String, u32> {
    HashMap::from([
        ("ArrowUp".to_string(), BTN_UP),
        ("ArrowDown".to_string(), BTN_DOWN),
        ("ArrowLeft".to_string(), BTN_LEFT),
        ("ArrowRight".to_string(), BTN_RIGHT),
        ("KeyZ".to_string(), BTN_B),
        ("KeyX".to_string(), BTN_A),
        ("KeyA".to_string(), BTN_Y),
        ("KeyS".to_string(), BTN_X),
        ("KeyQ".to_string(), BTN_L),
        ("KeyW".to_string(), BTN_R),
        ("Enter".to_string(), BTN_START),
        ("ShiftRight".to_string(), BTN_SELECT),
    ])
}

/// Default standard-gamepad mapping, keyed by Web Gamepad API button
/// index and valued by the generic `BTN_*` bit index.
///
/// Mirrors the RetroPad gamepad layout the platform's LibRetro path
/// ships: the bottom/right face buttons
/// (`0`/`1`) are `B`/`A`, the left/top face buttons (`2`/`3`) are
/// `Y`/`X`, shoulders (`4`/`5`) are `L`/`R`, `Select`/`Start` are
/// `8`/`9`, and the HID D-pad (`12`-`15`) drives the directions. The
/// SNES pad has no analog triggers or stick clicks, so RetroPad's
/// `L2`/`R2`/`L3`/`R3` (`6`/`7`/`10`/`11`) are intentionally unmapped.
pub fn default_gamepad_mapping() -> HashMap<u32, u32> {
    HashMap::from([
        (0, BTN_B),
        (1, BTN_A),
        (2, BTN_Y),
        (3, BTN_X),
        (4, BTN_L),
        (5, BTN_R),
        (8, BTN_SELECT),
        (9, BTN_START),
        (12, BTN_UP),
        (13, BTN_DOWN),
        (14, BTN_LEFT),
        (15, BTN_RIGHT),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mapping_each_button_sets_the_right_hardware_bit() {
        assert_eq!(input_state_to_snes_pad(1 << BTN_B), 0x8000);
        assert_eq!(input_state_to_snes_pad(1 << BTN_Y), 0x4000);
        assert_eq!(input_state_to_snes_pad(1 << BTN_SELECT), 0x2000);
        assert_eq!(input_state_to_snes_pad(1 << BTN_START), 0x1000);
        assert_eq!(input_state_to_snes_pad(1 << BTN_UP), 0x0800);
        assert_eq!(input_state_to_snes_pad(1 << BTN_DOWN), 0x0400);
        assert_eq!(input_state_to_snes_pad(1 << BTN_LEFT), 0x0200);
        assert_eq!(input_state_to_snes_pad(1 << BTN_RIGHT), 0x0100);
        assert_eq!(input_state_to_snes_pad(1 << BTN_A), 0x0080);
        assert_eq!(input_state_to_snes_pad(1 << BTN_X), 0x0040);
        assert_eq!(input_state_to_snes_pad(1 << BTN_L), 0x0020);
        assert_eq!(input_state_to_snes_pad(1 << BTN_R), 0x0010);
    }

    #[test]
    fn mapping_empty_and_combined_masks() {
        assert_eq!(input_state_to_snes_pad(0), 0x0000);
        // B + Y + A held simultaneously.
        let mask = (1 << BTN_B) | (1 << BTN_Y) | (1 << BTN_A);
        assert_eq!(input_state_to_snes_pad(mask), 0x8000 | 0x4000 | 0x0080);
        // Unused high bits of the host mask are ignored.
        assert_eq!(input_state_to_snes_pad(0xFFFF_0000), 0x0000);
    }

    #[test]
    fn default_keyboard_mapping_lands_on_the_right_snes_bits() {
        let map = default_keyboard_mapping();
        // Every entry, routed through the hardware translation, must hit
        // exactly the SNES button it is labelled for.
        let pad = |code: &str| input_state_to_snes_pad(1 << map[code]);
        assert_eq!(pad("ArrowUp"), 0x0800);
        assert_eq!(pad("ArrowDown"), 0x0400);
        assert_eq!(pad("ArrowLeft"), 0x0200);
        assert_eq!(pad("ArrowRight"), 0x0100);
        assert_eq!(pad("KeyZ"), 0x8000); // B
        assert_eq!(pad("KeyX"), 0x0080); // A
        assert_eq!(pad("KeyA"), 0x4000); // Y
        assert_eq!(pad("KeyS"), 0x0040); // X
        assert_eq!(pad("KeyQ"), 0x0020); // L
        assert_eq!(pad("KeyW"), 0x0010); // R
        assert_eq!(pad("Enter"), 0x1000); // Start
        assert_eq!(pad("ShiftRight"), 0x2000); // Select
    }

    #[test]
    fn default_keyboard_mapping_covers_all_twelve_buttons_once() {
        let map = default_keyboard_mapping();
        assert_eq!(map.len(), 12, "one host key per SNES button");
        let mut bits: Vec<u32> = map.values().copied().collect();
        bits.sort_unstable();
        assert_eq!(
            bits,
            (0..12).collect::<Vec<_>>(),
            "the twelve bindings must cover BTN_UP..=BTN_SELECT with no gaps or dupes"
        );
    }

    #[test]
    fn default_gamepad_mapping_lands_on_the_right_snes_bits() {
        let map = default_gamepad_mapping();
        let pad = |btn: u32| input_state_to_snes_pad(1 << map[&btn]);
        assert_eq!(pad(0), 0x8000); // B
        assert_eq!(pad(1), 0x0080); // A
        assert_eq!(pad(2), 0x4000); // Y
        assert_eq!(pad(3), 0x0040); // X
        assert_eq!(pad(4), 0x0020); // L
        assert_eq!(pad(5), 0x0010); // R
        assert_eq!(pad(8), 0x2000); // Select
        assert_eq!(pad(9), 0x1000); // Start
        assert_eq!(pad(12), 0x0800); // Up
        assert_eq!(pad(13), 0x0400); // Down
        assert_eq!(pad(14), 0x0200); // Left
        assert_eq!(pad(15), 0x0100); // Right
    }

    #[test]
    fn default_gamepad_mapping_omits_snes_absent_inputs() {
        // The SNES pad has no L2/R2 triggers or L3/R3 stick clicks, so
        // those RetroPad indices must be left unmapped.
        let map = default_gamepad_mapping();
        assert_eq!(map.len(), 12);
        for absent in [6u32, 7, 10, 11] {
            assert!(
                !map.contains_key(&absent),
                "button {absent} must be unmapped"
            );
        }
    }
}
