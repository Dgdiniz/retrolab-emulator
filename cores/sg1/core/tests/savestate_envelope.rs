//! The Mega Drive's savestates from before the envelope: bincode and zstd
//! of the payload alone, with no magic. They still load, through the
//! payload's own reader, and one made on another ROM is refused by the
//! game's title, the only name such a state ever showed. What every
//! savestate does (the round trip, the core, build and ROM checks) is the
//! machine conformance suite's.

#[path = "helpers/mod.rs"]
mod helpers;

use helpers::MiniCartridgeBuilder;
use retro_core::savestate::{LoadStateError, RomCheck};
use retro_core::EmulatorCore;
use sg1_core::savestate::{serialize_and_compress, SystemSaveState};
use sg1_core::system::GenesisEmulator;

fn rom_titled(title: &str) -> Vec<u8> {
    MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title(title)
        .build()
}

/// A state of `rom` after one frame, in the form before the envelope: the
/// payload of a sealed save, written again without it.
fn legacy_state(rom: &[u8]) -> (Vec<u8>, u64) {
    let mut a = GenesisEmulator::new();
    a.load_rom(rom).unwrap();
    a.run_frame();
    let envelope =
        retro_core::savestate::deserialize_compressed::<SystemSaveState>(&a.save_state())
            .expect("the sealed state opens");
    let bytes = serialize_and_compress(&envelope.payload, 3).expect("the old writer writes");
    assert!(
        !retro_core::savestate::has_magic(&bytes),
        "the old form has no magic"
    );
    (bytes, a.frame_count())
}

#[test]
fn a_savestate_from_before_the_envelope_still_loads() {
    let rom = rom_titled("Savestate Envelope Test");
    let (legacy_bytes, frames) = legacy_state(&rom);

    let mut b = GenesisEmulator::new();
    b.load_rom(&rom).unwrap();
    b.load_state(&legacy_bytes, RomCheck::SameRom)
        .expect("the old form loads through the payload's reader");
    assert_eq!(b.frame_count(), frames);
}

#[test]
fn a_legacy_savestate_from_another_rom_is_refused_by_title_unless_any_rom() {
    let (legacy_bytes, _) = legacy_state(&rom_titled("ROM A"));

    let mut b = GenesisEmulator::new();
    b.load_rom(&rom_titled("ROM B")).unwrap();
    match b.load_state(&legacy_bytes, RomCheck::SameRom) {
        Err(LoadStateError::OtherRom {
            state_rom,
            loaded_rom,
        }) => {
            assert!(state_rom.contains("ROM A"), "state_rom = {state_rom}");
            assert!(loaded_rom.contains("ROM B"), "loaded_rom = {loaded_rom}");
        }
        other => panic!("expected an OtherRom refusal, got {other:?}"),
    }
    b.load_state(&legacy_bytes, RomCheck::AnyRom)
        .expect("any ROM takes the legacy state");
}
