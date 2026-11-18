//! Golden runs of the Mega Drive core: what is this console's own in the
//! shared harness of [`retro_core::golden`] ([`GoldenConsole`]): the record
//! format's tag, the pad's button names and the `six-button` manifest flag.
//!
//! A golden test uses it; the records it makes are kept outside this tree,
//! beside the manifest `genesis.txt`, in the folder
//! [`retro_core::golden::GoldenDir`] finds. They are made on the play tier,
//! the loop a player runs ([`retro_core::golden::run_golden`]); every other
//! tier is held to them ([`retro_core::golden::run_golden_on`]).

use retro_core::golden::{GoldenConsole, GoldenEntry};
use retro_core::EmulatorCore;

use crate::io::devices::Genesis6ButtonDevice as Pad;
use crate::GenesisEmulator;

/// The manifest flag that plugs six-button pads in, for a ROM whose
/// header does not ask for them.
pub const SIX_BUTTON: &str = "six-button";

impl GoldenConsole for GenesisEmulator {
    const RECORD_FORMAT: &'static str = "sg1-golden 1";

    /// The pad's own layout, with its bits in `InputState::buttons`. `x`,
    /// `y`, `z` and `mode` reach a game only through a six-button pad.
    const BUTTONS: &'static [(&'static str, u32)] = &[
        ("up", Pad::BTN_UP),
        ("down", Pad::BTN_DOWN),
        ("left", Pad::BTN_LEFT),
        ("right", Pad::BTN_RIGHT),
        ("a", Pad::BTN_A),
        ("b", Pad::BTN_B),
        ("c", Pad::BTN_C),
        ("start", Pad::BTN_START),
        ("x", Pad::BTN_X),
        ("y", Pad::BTN_Y),
        ("z", Pad::BTN_Z),
        ("mode", Pad::BTN_MODE),
    ];

    const FLAGS: &'static [&'static str] = &[SIX_BUTTON];

    fn new_for_golden() -> Self {
        GenesisEmulator::new()
    }

    /// The ROM loaded, and six-button pads plugged in when the entry says.
    fn load_for_golden(&mut self, entry: &GoldenEntry, rom: &[u8]) -> Result<(), String> {
        self.load_rom(rom)?;
        if entry.has_flag(SIX_BUTTON) {
            self.connect_controllers(true);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::golden::{
        entry_spec_of, format_records_of, parse_manifest_of, parse_records_of, FrameRecord,
    };

    #[test]
    fn the_mega_drive_layer_reads_its_own_format_buttons_and_flag() {
        let records = vec![FrameRecord {
            frame: 1,
            state: 1,
            video: 2,
            audio: 3,
            samples: 1470,
            width: 320,
            height: 224,
        }];
        let text = format_records_of::<GenesisEmulator>(&["rom=sonic1.bin".to_string()], &records);
        assert!(text.starts_with("# sg1-golden 1\n# rom=sonic1.bin\n"));
        assert_eq!(
            parse_records_of::<GenesisEmulator>(&text).unwrap().1,
            records
        );
        assert!(parse_records_of::<GenesisEmulator>("# snes1-golden 1\n").is_err());

        let entries = parse_manifest_of::<GenesisEmulator>(
            "name=a rom=a.bin frames=10 press-at=3:start;5:a+c;7:z six-button\nname=b rom=b.bin frames=5\n",
        )
        .unwrap();
        assert!(entries[0].has_flag(SIX_BUTTON));
        assert!(!entries[1].has_flag(SIX_BUTTON));
        let presses = entry_spec_of::<GenesisEmulator>(&entries[0])
            .unwrap()
            .presses
            .unwrap();
        assert_eq!(presses.mask_at(3), Some(1 << 7));
        assert_eq!(presses.mask_at(5), Some((1 << 6) | (1 << 5)));
        assert_eq!(presses.mask_at(7), Some(1 << 8));
        // A SNES button or flag is not a Mega Drive one.
        assert!(parse_manifest_of::<GenesisEmulator>(
            "name=a rom=a.bin frames=10 press-at=3:select"
        )
        .is_err());
        assert!(
            parse_manifest_of::<GenesisEmulator>("name=a rom=a.bin frames=10 force-detect")
                .is_err()
        );
    }
}
