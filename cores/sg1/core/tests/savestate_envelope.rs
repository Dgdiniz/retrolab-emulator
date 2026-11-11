//! End-to-end savestate tests for the SG1 envelope wrapper.
//!
//! Covers:
//! - The envelope-wrapped format always carries the retro-core MAGIC
//!   prefix.
//! - `save_state` -> `load_state` roundtrip preserves the master cycle
//!   count (the only state the roundtrip test compares).
//! - Legacy bincode+zstd savestates (no magic prefix) still load
//!   through the fallback path in `load_state`.
//! - ROM hash mismatch surfaces a clear error.

#[path = "helpers/mod.rs"]
mod helpers;

use helpers::MiniCartridgeBuilder;
use retro_core::EmulatorCore;
use sg1_core::system::GenesisEmulator;

fn test_rom() -> Vec<u8> {
    MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("Savestate Envelope Test")
        .build()
}

#[test]
fn save_state_starts_with_retro_core_magic() {
    let mut emulator = GenesisEmulator::new();
    emulator.load_rom(&test_rom()).unwrap();

    let bytes = emulator.save_state();
    assert!(
        retro_core::savestate::has_magic(&bytes),
        "save_state output must start with retro-core MAGIC (got prefix {:?})",
        &bytes[..bytes.len().min(8)]
    );
    assert_eq!(&bytes[..4], retro_core::savestate::RETRO_SAVESTATE_MAGIC);
}

#[test]
fn save_state_roundtrip_preserves_master_cycles() {
    let rom = test_rom();
    let mut a = GenesisEmulator::new();
    a.load_rom(&rom).unwrap();

    // Step a handful of CPU instructions to give the snapshot
    // non-trivial state. master_cycles is a simple monotone counter
    // touched on every step, much more reliable than frame_count
    // for a synthetic mini-cartridge that may never finish a frame
    // worth of code.
    for _ in 0..32 {
        a.step();
    }
    let cycles_before = a.master_cycles();
    assert!(
        cycles_before > 34,
        "expected master_cycles > 34 (reset baseline) after stepping, got {cycles_before}"
    );

    let snapshot = a.save_state();

    let mut b = GenesisEmulator::new();
    b.load_rom(&rom).unwrap();
    b.load_state(&snapshot).expect("load_state must succeed");
    assert_eq!(
        b.master_cycles(),
        cycles_before,
        "master cycles must survive roundtrip"
    );
}

#[test]
fn load_state_accepts_legacy_format_without_magic() {
    // Build a savestate in the legacy format (bincode + zstd, no
    // envelope) and make sure load_state still accepts it, so the
    // savestates older builds wrote keep loading.
    use sg1_core::system::savestate::*;
    use sg1_core::system::savestate_impl::serialize_and_compress;

    let rom = test_rom();
    let mut a = GenesisEmulator::new();
    a.load_rom(&rom).unwrap();
    a.run_frame();
    let frames_before = a.frame_count();

    // Reach in for the legacy snapshot. We can't go through `save_state`
    // because that's already on the new format, so we have to manually
    // craft a `SystemSaveState`. Cheating: round-trip the new format
    // to get the payload, then re-serialize without the envelope.
    let new_bytes = a.save_state();
    let envelope = retro_core::savestate::deserialize_compressed::<SystemSaveState>(&new_bytes)
        .expect("new format must roundtrip");
    let legacy_bytes =
        serialize_and_compress(&envelope.payload, 3).expect("legacy serializer must still work");
    assert!(
        !retro_core::savestate::has_magic(&legacy_bytes),
        "legacy serializer must NOT emit the magic prefix"
    );

    let mut b = GenesisEmulator::new();
    b.load_rom(&rom).unwrap();
    b.load_state(&legacy_bytes)
        .expect("legacy savestate must load via fallback path");
    assert_eq!(b.frame_count(), frames_before);
}

#[test]
fn load_state_rejects_envelope_with_mismatched_rom_hash() {
    let rom_a = MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("ROM A")
        .build();
    let rom_b = MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("ROM B")
        .build();

    let mut a = GenesisEmulator::new();
    a.load_rom(&rom_a).unwrap();
    let snapshot = a.save_state();

    let mut b = GenesisEmulator::new();
    b.load_rom(&rom_b).unwrap();
    let err = b
        .load_state(&snapshot)
        .expect_err("load_state must refuse a savestate for a different ROM");
    assert!(
        err.contains("ROM hash mismatch"),
        "expected ROM hash mismatch error, got: {err}"
    );
}

/// Two builds of one game: the same code under another title, so the two
/// ROMs hash differently, as two builds of a translation do.
fn two_builds() -> (Vec<u8>, Vec<u8>) {
    let build = |title: &str| {
        MiniCartridgeBuilder::new()
            .with_size(0x80000)
            .with_title(title)
            .build()
    };
    (build("ROM A"), build("ROM B"))
}

#[test]
fn load_state_with_any_rom_takes_a_state_made_on_another_rom() {
    use retro_core::savestate::{LoadStateError, RomCheck};

    let (rom_a, rom_b) = two_builds();
    let mut a = GenesisEmulator::new();
    a.load_rom(&rom_a).unwrap();
    for _ in 0..3 {
        a.run_frame();
    }
    let frames = a.frame_count();
    let snapshot = a.save_state();

    let mut b = GenesisEmulator::new();
    b.load_rom(&rom_b).unwrap();
    let err = b
        .load_state_with_rom_check(&snapshot, RomCheck::SameRom)
        .expect_err("the same-ROM check refuses another ROM's state");
    assert!(
        matches!(err, LoadStateError::OtherRom { .. }),
        "got {err:?}"
    );
    b.load_state_with_rom_check(&snapshot, RomCheck::AnyRom)
        .expect("any ROM takes the state");
    assert_eq!(b.frame_count(), frames);
}

#[test]
fn a_legacy_savestate_from_another_rom_is_refused_by_title_unless_any_rom() {
    use retro_core::savestate::{LoadStateError, RomCheck};
    use sg1_core::system::savestate::*;
    use sg1_core::system::savestate_impl::serialize_and_compress;

    let (rom_a, rom_b) = two_builds();
    let mut a = GenesisEmulator::new();
    a.load_rom(&rom_a).unwrap();
    a.run_frame();
    let envelope =
        retro_core::savestate::deserialize_compressed::<SystemSaveState>(&a.save_state())
            .expect("new format must roundtrip");
    let legacy_bytes = serialize_and_compress(&envelope.payload, 3).unwrap();

    let mut b = GenesisEmulator::new();
    b.load_rom(&rom_b).unwrap();
    match b.load_state_with_rom_check(&legacy_bytes, RomCheck::SameRom) {
        Err(LoadStateError::OtherRom {
            state_rom,
            loaded_rom,
        }) => {
            assert!(state_rom.contains("ROM A"), "state_rom = {state_rom}");
            assert!(loaded_rom.contains("ROM B"), "loaded_rom = {loaded_rom}");
        }
        other => panic!("expected an OtherRom refusal, got {other:?}"),
    }
    b.load_state_with_rom_check(&legacy_bytes, RomCheck::AnyRom)
        .expect("any ROM takes the legacy state");
}
