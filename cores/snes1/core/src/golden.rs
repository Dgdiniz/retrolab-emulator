//! Golden runs of the SNES core: what is this console's own in the shared
//! harness of [`retro_core::golden`] ([`GoldenConsole`]): the record
//! format's tag, the pad's button names, the `force-detect` manifest flag
//! and the real-time clock a golden run pins.
//!
//! The CLI's `golden` commands and the golden tests use it; the records it
//! makes are kept outside this tree, beside the manifest `snes.txt`, in the
//! folder [`retro_core::golden::GoldenDir`] finds.

use retro_core::golden::{GoldenConsole, GoldenEntry};
use retro_core::EmulatorCore;

use crate::cartridge::{fixed_unix_time, set_fixed_unix_time};
use crate::SnesEmulator;

/// The manifest flag that loads a low-confidence ROM by forcing its
/// vector-implied mapping.
pub const FORCE_DETECT: &str = "force-detect";

/// The pad's button names in the order of the generic input bits
/// (`InputState::buttons`), matching the `BTN_*` constants of
/// [`crate::controller`].
const BUTTON_NAMES: [&str; 12] = [
    "up", "down", "left", "right", "a", "b", "x", "y", "l", "r", "start", "select",
];

/// The same names with their bit, as the shared input grammar takes them.
const BUTTONS: [(&str, u32); 12] = {
    let mut table = [("", 0u32); 12];
    let mut i = 0;
    while i < 12 {
        table[i] = (BUTTON_NAMES[i], i as u32);
        i += 1;
    }
    table
};

impl GoldenConsole for SnesEmulator {
    const RECORD_FORMAT: &'static str = "snes1-golden 1";
    const BUTTONS: &'static [(&'static str, u32)] = &BUTTONS;
    const FLAGS: &'static [&'static str] = &[FORCE_DETECT];

    fn new_for_golden() -> Self {
        SnesEmulator::new()
    }

    /// The ROM loaded, its vector-implied mapping forced when the entry
    /// says (the CLI's `--force-detect`).
    fn load_for_golden(&mut self, entry: &GoldenEntry, rom: &[u8]) -> Result<(), String> {
        if entry.has_flag(FORCE_DETECT) {
            let manifest = self.detected_manifest(rom);
            self.set_cartridge_manifest_override(Some(manifest));
        }
        self.load_rom(rom)
    }

    /// A cartridge with a real-time clock (the SPC7110's RTC-4513) would
    /// read the host's time into its state; a golden run pins it.
    fn pin_real_time_clock(unix_time: Option<i64>) -> Option<i64> {
        let previous = fixed_unix_time();
        set_fixed_unix_time(unix_time);
        previous
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::golden::{
        entry_spec_of, format_records_of, parse_manifest_of, parse_records_of, FrameRecord,
    };

    #[test]
    fn button_names_follow_the_controller_constants() {
        use crate::controller::*;
        use retro_core::input_schedule::parse_button_mask;
        let expected = [
            BTN_UP, BTN_DOWN, BTN_LEFT, BTN_RIGHT, BTN_A, BTN_B, BTN_X, BTN_Y, BTN_L, BTN_R,
            BTN_START, BTN_SELECT,
        ];
        for (i, bit) in expected.iter().enumerate() {
            assert_eq!(
                parse_button_mask(Some(BUTTON_NAMES[i]), &BUTTONS).unwrap(),
                Some(1 << bit),
                "{}",
                BUTTON_NAMES[i]
            );
        }
    }

    #[test]
    fn the_snes_layer_reads_its_own_format_buttons_and_flag() {
        let records = vec![FrameRecord {
            frame: 1,
            state: 1,
            video: 2,
            audio: 3,
            samples: 1068,
            width: 256,
            height: 224,
        }];
        let text = format_records_of::<SnesEmulator>(&["rom=smw.sfc".to_string()], &records);
        assert!(text.starts_with("# snes1-golden 1\n# rom=smw.sfc\n"));
        assert_eq!(parse_records_of::<SnesEmulator>(&text).unwrap().1, records);
        assert!(parse_records_of::<SnesEmulator>("# sg1-golden 1\n").is_err());

        let entries = parse_manifest_of::<SnesEmulator>(
            "name=a rom=a.sfc frames=10 press-at=3:select force-detect\nname=b rom=b.sfc frames=5\n",
        )
        .unwrap();
        assert!(entries[0].has_flag(FORCE_DETECT));
        assert!(!entries[1].has_flag(FORCE_DETECT));
        let spec = entry_spec_of::<SnesEmulator>(&entries[0]).unwrap();
        assert_eq!(spec.presses.unwrap().mask_at(3), Some(1 << 11));
        // A Mega Drive button or flag is not a SNES one.
        assert!(
            parse_manifest_of::<SnesEmulator>("name=a rom=a.sfc frames=10 press-at=3:c").is_err()
        );
        assert!(
            parse_manifest_of::<SnesEmulator>("name=a rom=a.sfc frames=10 six-button").is_err()
        );
    }
}
