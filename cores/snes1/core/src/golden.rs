//! Golden runs of the SNES core: the shared harness of
//! [`retro_core::golden`] with what is the SNES's own: the state hash,
//! the pad's button names, the `force-detect` manifest flag, the record
//! format's tag, and a pinned real-time clock.
//!
//! The CLI's `golden` commands and the golden tests use it; the records it
//! makes are kept outside this tree, beside the manifest `snes.txt`, in the
//! folder [`retro_core::golden::GoldenDir`] finds.

pub use retro_core::golden::{
    crop_framebuffer_rgb, first_divergence, hash_samples, Divergence, FrameRecord, GoldenEntry,
    GoldenSpec,
};

use retro_core::EmulatorCore;

use crate::cartridge::{fixed_unix_time, set_fixed_unix_time};
use crate::input_schedule::BUTTONS;
use crate::SnesEmulator;

/// Version tag on the first line of a SNES record file.
pub const RECORD_FORMAT: &str = "snes1-golden 1";

/// The manifest flag that loads a low-confidence ROM by forcing its
/// vector-implied mapping.
pub const FORCE_DETECT: &str = "force-detect";

/// The time every real-time clock reads during a golden run:
/// 2026-01-01 00:00:00 UTC.
pub const GOLDEN_UNIX_TIME: i64 = 1_767_225_600;

/// Drive `emu` through `spec`, calling `on_shot(frame, width, height,
/// rgb)` at each listed shot and `on_audio(samples)` with every frame's
/// samples, and return one record per frame. `emu` must have its ROM
/// loaded; audio capture is armed by the run.
pub fn run_golden(
    emu: &mut SnesEmulator,
    spec: &GoldenSpec,
    on_shot: impl FnMut(u64, u32, u32, &[u8]),
    on_audio: impl FnMut(&[f32]),
) -> Vec<FrameRecord> {
    // A cartridge with a real-time clock would read the host's time into
    // its state; pin the clock for the run so the record is reproducible.
    let previous_time = fixed_unix_time();
    set_fixed_unix_time(Some(GOLDEN_UNIX_TIME));
    let records = retro_core::golden::run_golden(
        emu,
        spec,
        <SnesEmulator as EmulatorCore>::run_frame,
        SnesEmulator::state_hash,
        on_shot,
        on_audio,
    );
    set_fixed_unix_time(previous_time);
    records
}

/// Render records as the text file the CLI writes and the test reads.
pub fn format_records(header: &[String], records: &[FrameRecord]) -> String {
    retro_core::golden::format_records(RECORD_FORMAT, header, records)
}

/// Parse a SNES record file back; the format line must be
/// [`RECORD_FORMAT`].
pub fn parse_records(text: &str) -> Result<(Vec<String>, Vec<FrameRecord>), String> {
    retro_core::golden::parse_records(RECORD_FORMAT, text)
}

/// Parse a SNES manifest (`snes.txt`): the shared grammar
/// with the SNES pad's buttons and the [`FORCE_DETECT`] flag.
pub fn parse_manifest(text: &str) -> Result<Vec<GoldenEntry>, String> {
    retro_core::golden::parse_manifest(text, &BUTTONS, &[FORCE_DETECT])
}

/// The run spec a manifest entry describes, its presses read with the
/// SNES pad's buttons.
pub fn entry_spec(entry: &GoldenEntry) -> Result<GoldenSpec, String> {
    entry.spec(&BUTTONS)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let text = format_records(&["rom=smw.sfc".to_string()], &records);
        assert!(text.starts_with("# snes1-golden 1\n# rom=smw.sfc\n"));
        assert_eq!(parse_records(&text).unwrap().1, records);
        assert!(parse_records("# sg1-golden 1\n").is_err());

        let entries = parse_manifest(
            "name=a rom=a.sfc frames=10 press-at=3:select force-detect\nname=b rom=b.sfc frames=5\n",
        )
        .unwrap();
        assert!(entries[0].has_flag(FORCE_DETECT));
        assert!(!entries[1].has_flag(FORCE_DETECT));
        let spec = entry_spec(&entries[0]).unwrap();
        assert_eq!(spec.presses.unwrap().mask_at(3), Some(1 << 11));
        // A Mega Drive button or flag is not a SNES one.
        assert!(parse_manifest("name=a rom=a.sfc frames=10 press-at=3:c").is_err());
        assert!(parse_manifest("name=a rom=a.sfc frames=10 six-button").is_err());
    }
}
