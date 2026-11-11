//! Golden runs of the Mega Drive core: the shared harness of
//! [`retro_core::golden`] with what is the Mega Drive's own: the state
//! hash, the pad's button names, the `six-button` manifest flag and the
//! record format's tag.
//!
//! A golden test uses it; the records it makes are kept outside this tree,
//! beside the manifest `genesis.txt`, in the folder
//! [`retro_core::golden::GoldenDir`] finds. They are made on the frame
//! loop a player runs ([`FrameLoop::Fast`]).

pub use retro_core::golden::{
    crop_framebuffer_rgb, first_divergence, hash_samples, Divergence, FrameRecord, GoldenEntry,
    GoldenSpec,
};
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode};

use crate::io::devices::Genesis6ButtonDevice as Pad;
use crate::GenesisEmulator;

/// Version tag on the first line of a Mega Drive record file.
pub const RECORD_FORMAT: &str = "sg1-golden 1";

/// The manifest flag that plugs six-button pads in, for a ROM whose
/// header does not ask for them.
pub const SIX_BUTTON: &str = "six-button";

/// Button names with their bit in `InputState::buttons`, the pad's own
/// layout. `x`, `y`, `z` and `mode` only reach a game through a
/// six-button pad.
pub const BUTTONS: [(&str, u32); 12] = [
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

/// Which of the core's three frame loops a golden run drives. A host
/// picks one per frame from what the debugger is doing; a record is made
/// with one of them throughout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameLoop {
    /// What a player runs: no debugger work at all.
    Fast,
    /// Breakpoints and tracing only.
    LightDebug,
    /// The full debugger loop (`EmulatorCore::run_frame`).
    FullDebug,
}

impl FrameLoop {
    /// Advance `emu` one presented frame on this loop.
    pub fn run_frame(self, emu: &mut GenesisEmulator) {
        match self {
            FrameLoop::Fast => emu.run_frame_fast(),
            FrameLoop::LightDebug => emu.run_frame_debug_light(),
            FrameLoop::FullDebug => <GenesisEmulator as EmulatorCore>::run_frame(emu),
        }
    }
}

/// Drive `emu` through `spec` on `frame_loop`, calling `on_shot(frame,
/// width, height, rgb)` at each listed shot and `on_audio(samples)` with
/// every frame's samples, and return one record per frame. `emu` must
/// have its ROM loaded; the run puts the machine in the running mode and
/// arms audio capture.
pub fn run_golden(
    emu: &mut GenesisEmulator,
    spec: &GoldenSpec,
    frame_loop: FrameLoop,
    on_shot: impl FnMut(u64, u32, u32, &[u8]),
    on_audio: impl FnMut(&[f32]),
) -> Vec<FrameRecord> {
    // The frame loops only advance a machine that is running.
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    retro_core::golden::run_golden(
        emu,
        spec,
        |emu| frame_loop.run_frame(emu),
        GenesisEmulator::state_hash,
        on_shot,
        on_audio,
    )
}

/// Render records as the text file the test writes and reads.
pub fn format_records(header: &[String], records: &[FrameRecord]) -> String {
    retro_core::golden::format_records(RECORD_FORMAT, header, records)
}

/// Parse a Mega Drive record file back; the format line must be
/// [`RECORD_FORMAT`].
pub fn parse_records(text: &str) -> Result<(Vec<String>, Vec<FrameRecord>), String> {
    retro_core::golden::parse_records(RECORD_FORMAT, text)
}

/// Parse a Mega Drive manifest (`genesis.txt`): the shared
/// grammar with the pad's buttons and the [`SIX_BUTTON`] flag.
pub fn parse_manifest(text: &str) -> Result<Vec<GoldenEntry>, String> {
    retro_core::golden::parse_manifest(text, &BUTTONS, &[SIX_BUTTON])
}

/// The run spec a manifest entry describes, its presses read with the
/// pad's buttons.
pub fn entry_spec(entry: &GoldenEntry) -> Result<GoldenSpec, String> {
    entry.spec(&BUTTONS)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let text = format_records(&["rom=sonic1.bin".to_string()], &records);
        assert!(text.starts_with("# sg1-golden 1\n# rom=sonic1.bin\n"));
        assert_eq!(parse_records(&text).unwrap().1, records);
        assert!(parse_records("# snes1-golden 1\n").is_err());

        let entries = parse_manifest(
            "name=a rom=a.bin frames=10 press-at=3:start;5:a+c;7:z six-button\nname=b rom=b.bin frames=5\n",
        )
        .unwrap();
        assert!(entries[0].has_flag(SIX_BUTTON));
        assert!(!entries[1].has_flag(SIX_BUTTON));
        let presses = entry_spec(&entries[0]).unwrap().presses.unwrap();
        assert_eq!(presses.mask_at(3), Some(1 << 7));
        assert_eq!(presses.mask_at(5), Some((1 << 6) | (1 << 5)));
        assert_eq!(presses.mask_at(7), Some(1 << 8));
        // A SNES button or flag is not a Mega Drive one.
        assert!(parse_manifest("name=a rom=a.bin frames=10 press-at=3:select").is_err());
        assert!(parse_manifest("name=a rom=a.bin frames=10 force-detect").is_err());
    }
}
