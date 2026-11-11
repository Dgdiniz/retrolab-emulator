//! End-to-end cartridge pipeline tests.
//!
//! These exercise the full ROM-bytes -> `CartridgeBuilder` ->
//! `Box<dyn SnesCartridge>` -> `SnesBus::install_cartridge` ->
//! `bus.read_full(addr)` path. Unit-level coverage for individual
//! pieces (header parsing, presets, scoring, manifest parsing,
//! builder helpers) lives in the `snes1-core` `--lib` test suite;
//! this file proves the pieces fit together and that a built
//! cartridge dispatches correctly through the bus.

use sha2::{Digest, Sha256};
use snes1_core::{
    parse_manifest, BuildError, CartridgeBuilder, MapTarget, SnesBus, SnesCartridge,
    MAP_MODE_EXHIROM, MAP_MODE_HIROM, MAP_MODE_LOROM,
};

// ---------------------------------------------------------------
// Synthesizers
// ---------------------------------------------------------------

/// Build a ROM `size` bytes wide, stamp a well-formed header at
/// `offset` with the given `map_mode` / `rom_size_code` /
/// `sram_size_code`, and set the reset vector to `reset_vector`.
/// Title is a printable ASCII string so the detector scores it
/// well above threshold.
fn synth_rom(
    size: usize,
    offset: usize,
    map_mode: u8,
    rom_size_code: u8,
    sram_size_code: u8,
    reset_vector: u16,
) -> Vec<u8> {
    let mut rom = vec![0u8; size];

    let title = b"CARTRIDGE TEST ROM   ";
    rom[offset + 0x10..offset + 0x10 + 21].copy_from_slice(title);

    rom[offset + 0x25] = map_mode;
    rom[offset + 0x26] = 0x00;
    rom[offset + 0x27] = rom_size_code;
    rom[offset + 0x28] = sram_size_code;
    rom[offset + 0x29] = 0x01;
    rom[offset + 0x2A] = 0x01;
    rom[offset + 0x2B] = 0x00;

    let checksum: u16 = 0x1234;
    let complement = !checksum;
    rom[offset + 0x2C..offset + 0x2E].copy_from_slice(&complement.to_le_bytes());
    rom[offset + 0x2E..offset + 0x30].copy_from_slice(&checksum.to_le_bytes());

    rom[offset + 0x4C..offset + 0x4E].copy_from_slice(&reset_vector.to_le_bytes());

    rom
}

fn synth_lorom_rom(size: usize) -> Vec<u8> {
    synth_rom(size, 0x7FB0, MAP_MODE_LOROM, 0x0A, 0x00, 0x8000)
}

fn synth_hirom_rom(size: usize) -> Vec<u8> {
    synth_rom(size, 0xFFB0, MAP_MODE_HIROM, 0x0C, 0x00, 0x8000)
}

fn synth_exhirom_rom(size: usize) -> Vec<u8> {
    // Reset vector at $8000 stays sane for the boot path; the
    // ExHiROM preset's low mirror serves chip $40_FFFC, so the
    // header's reset vector lands at the right logical address.
    synth_rom(size, 0x40_FFB0, MAP_MODE_EXHIROM, 0x0D, 0x00, 0x8000)
}

/// Lower-case hex SHA-256 of `bytes`. Matches the digest the
/// builder calculates against `manifest.rom.sha256`.
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().fold(String::with_capacity(64), |mut acc, b| {
        use std::fmt::Write as _;
        let _ = write!(acc, "{:02x}", b);
        acc
    })
}

/// Install the boxed cartridge in a fresh bus and hand the bus
/// back so the test can drive `read_full` / `cartridge_ref`.
fn install_in_bus(cart: Box<dyn SnesCartridge>) -> SnesBus {
    let mut bus = SnesBus::new();
    bus.install_cartridge(cart);
    bus
}

// ---------------------------------------------------------------
// End-to-end tests
// ---------------------------------------------------------------

#[test]
fn builder_then_install_lorom_routes_reads_correctly() {
    // Stamp a sentinel byte at LoROM chip offset 0xABCD. With the
    // default LoROM bank stride (32 KiB per bank), chip byte
    // 0xABCD lives at bank $01, offset $AB_CD (bank-relative
    // $2BCD + $8000 = $ABCD).
    let mut rom = synth_lorom_rom(0x10_0000);
    rom[0xABCD] = 0xAB;

    let cart = CartridgeBuilder::new(&rom)
        .build()
        .expect("detection LoROM cartridge builds");
    assert_eq!(cart.map_plan().name, "lorom");

    let mut bus = install_in_bus(cart);
    assert_eq!(bus.read_full(0x01_ABCD), 0xAB);
    // Lower mirror at $00-$7D maps the same chip image, so the
    // earlier bank $00 reads MUST hit the same byte for a smaller
    // address. The lower mirror chooses chip byte 0 at $00:8000;
    // confirm it's still 0 (we didn't stamp there).
    assert_eq!(bus.read_full(0x00_8000), 0);
}

#[test]
fn lorom_mirror_works_through_installed_bus() {
    // The LoROM `$80-$FF` slot mirrors the `$00-$7D` slot. Both
    // address spaces resolve to the same chip image, so reads at
    // `$00:8000+k` and `$80:8000+k` must return the same byte.
    let mut rom = synth_lorom_rom(0x10_0000);
    rom[0x5678] = 0x77;

    let cart = CartridgeBuilder::new(&rom).build().expect("builds");
    let mut bus = install_in_bus(cart);

    // Chip byte 0x5678 -> bank 0, offset 0x5678 -> address $00:D678.
    assert_eq!(bus.read_full(0x00_D678), 0x77);
    // Mirror in the upper half: same chip byte via $80:D678.
    assert_eq!(bus.read_full(0x80_D678), 0x77);
}

#[test]
fn hirom_mirror_lands_on_upper_half_after_install() {
    // HiROM's $C0-$FF primary slot maps directly: $C0:8000 -> chip
    // byte 0x8000. The $00-$3F mirror slot reads the same chip
    // byte at $00:8000 via the bank_stride + base_offset trick.
    let mut rom = synth_hirom_rom(0x40_0000);
    rom[0x8000] = 0x55;

    let cart = CartridgeBuilder::new(&rom).build().expect("HiROM builds");
    assert_eq!(cart.map_plan().name, "hirom");

    let mut bus = install_in_bus(cart);

    // Primary slot at $C0:8000.
    assert_eq!(bus.read_full(0xC0_8000), 0x55);
    // Slow mirror at $00:8000 (covers $00-$3F:$8000-$FFFF).
    assert_eq!(bus.read_full(0x00_8000), 0x55);
    // FastROM-eligible mirror at $80:8000 (covers $80-$BF:$8000-$FFFF).
    assert_eq!(bus.read_full(0x80_8000), 0x55);
}

#[test]
fn exhirom_low_mirror_serves_second_half_reset_vector_after_install() {
    // The whole point of the ExHiROM mirror layout: the CPU boot
    // reset vector at $00:FFFC must read the bytes the cartridge
    // wrote into its header at chip $40_FFFC (i.e., $40FFB0+0x4C
    // = $40FFFC). `synth_exhirom_rom` already stamps the reset
    // vector as LE 0x8000 at chip $40_FFFC / $40_FFFD; the test
    // verifies that those same bytes round-trip through the bus
    // when read via the boot path at $00:FFFC.
    //
    // Re-stamping the reset vector to a fancier pattern would
    // push it out of the `$8000-$FFFF` logical range, which costs
    // the detector its +100 reset-vector bonus and drops the
    // overall score below the threshold, proving by accident
    // that the bonus carries weight, but defeating this test.
    let rom = synth_exhirom_rom(0x60_0000);

    let cart = CartridgeBuilder::new(&rom).build().expect("ExHiROM builds");
    assert_eq!(cart.map_plan().name, "exhirom");

    let mut bus = install_in_bus(cart);
    // Both the primary slot and the low mirror must serve the
    // same chip bytes ($40_FFFC / $40_FFFD = LE 0x8000).
    assert_eq!(bus.read_full(0x40_FFFC), 0x00); // LE 0x8000 low byte
    assert_eq!(bus.read_full(0x40_FFFD), 0x80); // LE 0x8000 high byte
    assert_eq!(bus.read_full(0x00_FFFC), 0x00);
    assert_eq!(bus.read_full(0x00_FFFD), 0x80);
}

#[test]
fn manifest_overrides_heuristic() {
    // A clean LoROM ROM that the detector would happily classify
    // as LoROM: the manifest forces HiROM and wins.
    let rom = synth_lorom_rom(0x10_0000);
    let manifest_json = r#"{
        "core": "snes1",
        "version": "1.0",
        "rom": { "sha256": "", "size": 1048576 },
        "mapping": { "preset": "hirom" }
    }"#;
    let manifest = parse_manifest(manifest_json).unwrap();

    let cart = CartridgeBuilder::new(&rom)
        .with_manifest(manifest)
        .build()
        .expect("manifest-driven build succeeds");
    assert!(cart.map_plan().name.starts_with("hirom"));
}

#[test]
fn manifest_custom_slot_appends_to_preset() {
    // LoROM preset generates 2 slots (sram_size = 0); the
    // manifest appends one custom slot for a total of 3.
    let rom = synth_lorom_rom(0x10_0000);
    let manifest_json = r#"{
        "core": "snes1",
        "version": "1.0",
        "rom": { "sha256": "", "size": 1048576 },
        "mapping": {
            "preset": "lorom",
            "sram_size": 0,
            "slots": [
                {
                    "bank_range": ["0x60", "0x6F"],
                    "offset_range": ["0x0000", "0x7FFF"],
                    "target": { "kind": "Sram", "base_offset": 0 },
                    "speed": "Slow"
                }
            ]
        }
    }"#;
    let manifest = parse_manifest(manifest_json).unwrap();

    let cart = CartridgeBuilder::new(&rom)
        .with_manifest(manifest)
        .build()
        .expect("manifest with custom slot builds");
    let plan = cart.map_plan();
    assert_eq!(plan.name, "lorom");
    assert_eq!(
        plan.slots.len(),
        3,
        "2 LoROM ROM slots + 1 custom SRAM slot"
    );
    let appended = plan.slots.last().unwrap();
    assert_eq!(appended.bank_range, 0x60..=0x6F);
    assert!(matches!(appended.target, MapTarget::Sram { .. }));
}

#[test]
fn manifest_custom_chip_dispatches_via_chip_id() {
    // A DSP-1 declared in the manifest `chips[]` must produce a *live*
    // coprocessor built through the console `ChipFactory`, not merely a
    // `MapTarget::ChipMmio` slot. Chip declarations always dispatch through
    // ChipMmio regardless of how the inline target is written.
    let rom = synth_lorom_rom(0x10_0000);
    let manifest_json = r#"{
        "core": "snes1",
        "version": "1.0",
        "rom": { "sha256": "", "size": 1048576 },
        "mapping": { "preset": "lorom" },
        "chips": [
            {
                "id": 1,
                "kind": "dsp1",
                "mapping": {
                    "bank_range": ["0x30", "0x3F"],
                    "offset_range": ["0x8000", "0xBFFF"],
                    "target": { "kind": "Rom", "base_offset": 0 },
                    "speed": "Fast"
                }
            }
        ]
    }"#;
    let manifest = parse_manifest(manifest_json).unwrap();

    let mut cart = CartridgeBuilder::new(&rom)
        .with_manifest(manifest)
        .build()
        .expect("DSP-1 manifest builds");

    // The slot resolves to the chip window...
    {
        let chip_slot = cart
            .map_plan()
            .slots
            .iter()
            .find(|s| matches!(s.target, MapTarget::ChipMmio { chip_id: 1, .. }))
            .expect("DSP-1 slot present");
        assert_eq!(chip_slot.bank_range, 0x30..=0x3F);
        assert_eq!(chip_slot.offset_range, 0x8000..=0xBFFF);
        match chip_slot.target {
            MapTarget::ChipMmio { chip_id, base } => {
                assert_eq!(chip_id, 1);
                assert_eq!(base, 0);
            }
            _ => unreachable!(),
        }
    }

    // ...and a live coprocessor sits behind it. No firmware was staged, so
    // the factory installs the firmware-free HLE. Driving the SR side (the
    // 0x4000 DR/SR-select bit for the LoROM layout) returns the ready
    // status and bumps the chip's access counter, proving the dispatch
    // reaches a real chip rather than open bus.
    let label = cart.coprocessor_label().expect("manifest DSP-1 is live");
    assert!(label.contains("HLE"), "label = {label}");
    assert_eq!(cart.coprocessor_accesses(), 0);
    assert_eq!(cart.read_chip(1, 0x4000, 0), 0x80);
    assert_eq!(cart.coprocessor_accesses(), 1);
}

#[test]
fn smc_header_strip_then_pad_lorom() {
    // Build a 0x10_0200-byte input (= 0x200 SMC prelude + 0x10_0000
    // LoROM image). The strip drops the first 0x200 bytes; after
    // strip the buffer is 0x10_0000 bytes (already a power of two)
    // and the LoROM header lives at $7FB0.
    //
    // Sentinel pattern that only matches when the strip actually
    // happens: stamp 0xCC at SMC-input offset 0x300, which lands
    // at chip byte 0x100 in the stripped image. Without the
    // strip, chip byte 0x100 would read the (zeroed) SMC prelude.
    let lorom_image = synth_lorom_rom(0x10_0000);
    let mut padded = vec![0u8; 0x200 + 0x10_0000];
    padded[0x200..].copy_from_slice(&lorom_image);
    padded[0x300] = 0xCC; // = chip byte 0x100 after strip.

    let cart = CartridgeBuilder::new(&padded)
        .build()
        .expect("SMC-prefixed LoROM strips + builds");
    assert_eq!(cart.map_plan().name, "lorom");

    // Chip byte 0x100 -> bank 0, bank-relative offset 0x100 ->
    // address $00:8100.
    let mut bus = install_in_bus(cart);
    assert_eq!(bus.read_full(0x00_8100), 0xCC);
}

#[test]
fn rom_padding_to_power_of_2_for_6mib_exhirom() {
    // ExHiROM ROMs at 6 MiB (Tales of Phantasia) are not a power
    // of two; the builder must pad to 8 MiB so the preset
    // generators' mask logic and the bus's slot dispatch hold up
    // for the upper-mirror reads. A 6 MiB build that succeeds and
    // exposes a 4-slot ExHiROM plan is strong evidence.
    let rom = synth_exhirom_rom(0x60_0000);
    assert_eq!(rom.len() % 1024, 0, "test input is not SMC-padded");

    let cart = CartridgeBuilder::new(&rom)
        .build()
        .expect("6 MiB ExHiROM builds");
    let plan = cart.map_plan();
    assert_eq!(plan.name, "exhirom");
    assert_eq!(plan.slots.len(), 4, "4 slots without SRAM");

    // The bus dispatches reads through the now-padded buffer:
    // the reset vector path lives at chip $40_FFFC, which is
    // well inside the original 6 MiB region.
    let mut bus = install_in_bus(cart);
    let reset_lo = bus.read_full(0x00_FFFC);
    let reset_hi = bus.read_full(0x00_FFFD);
    let reset = u16::from_le_bytes([reset_lo, reset_hi]);
    assert_eq!(reset, 0x8000, "reset vector survives padding");
}

#[test]
fn hirom_and_exhirom_sram_answer_in_both_halves() {
    // A HiROM board does not decode address line A23 for its RAM, so
    // `$A0-$BF:6000-7FFF` reaches the bytes `$20-$3F:6000-7FFF` does.
    // On a 32 KiB chip, three bytes written through banks $21-$23 read
    // back through $A5-$A7, and a byte written high reads back low.
    let hirom = synth_rom(0x40_0000, 0xFFB0, MAP_MODE_HIROM, 0x0C, 0x05, 0x8000);
    let exhirom = synth_rom(0x60_0000, 0x40_FFB0, MAP_MODE_EXHIROM, 0x0D, 0x05, 0x8000);
    for rom in [hirom, exhirom] {
        let cart = CartridgeBuilder::new(&rom).build().expect("ROM builds");
        assert_eq!(cart.sram().len(), 32 * 1024, "header declares 32 KiB");
        let mut bus = install_in_bus(cart);

        for (addr, value) in [(0x21_7F00, 0xA6), (0x22_7F00, 0xF3), (0x23_7F00, 0x0C)] {
            bus.write_full(addr, value);
        }
        assert_eq!(bus.read_full(0xA5_7F00), 0xA6);
        assert_eq!(bus.read_full(0xA6_7F00), 0xF3);
        assert_eq!(bus.read_full(0xA7_7F00), 0x0C);

        bus.write_full(0xB9_6123, 0x5A);
        assert_eq!(bus.read_full(0x39_6123), 0x5A);
        // 32 KiB is four banks of 8 KiB: bank $39 folds onto $21.
        assert_eq!(bus.read_full(0x21_6123), 0x5A);
    }
}

#[test]
fn sha256_match_succeeds() {
    let rom = synth_lorom_rom(0x10_0000);
    let digest = sha256_hex(&rom);
    let manifest_json = format!(
        r#"{{
            "core": "snes1",
            "version": "1.0",
            "rom": {{ "sha256": "{digest}", "size": 1048576 }},
            "mapping": {{ "preset": "lorom" }}
        }}"#
    );
    let manifest = parse_manifest(&manifest_json).unwrap();

    let cart = CartridgeBuilder::new(&rom)
        .with_manifest(manifest)
        .build()
        .expect("matching SHA-256 builds");
    assert_eq!(cart.map_plan().name, "lorom");
}

#[test]
fn sha256_mismatch_blocks_build() {
    let rom = synth_lorom_rom(0x10_0000);
    let manifest_json = r#"{
        "core": "snes1",
        "version": "1.0",
        "rom": {
            "sha256": "0000000000000000000000000000000000000000000000000000000000000000",
            "size": 1048576
        },
        "mapping": { "preset": "lorom" }
    }"#;
    let manifest = parse_manifest(manifest_json).unwrap();

    let result = CartridgeBuilder::new(&rom).with_manifest(manifest).build();
    match result {
        Ok(_) => panic!("build should have rejected the manifest"),
        Err(BuildError::Sha256Mismatch { expected, actual }) => {
            assert_eq!(
                expected,
                "0000000000000000000000000000000000000000000000000000000000000000"
            );
            assert_eq!(actual.len(), 64);
            assert_ne!(actual, expected);
        }
        Err(other) => panic!("expected Sha256Mismatch, got {other:?}"),
    }
}

#[test]
fn no_header_no_manifest_no_force_returns_no_mapping() {
    // A buffer the detector can't make sense of, no manifest,
    // no forced preset, must surface NoMapping with the
    // configured threshold so the caller knows to prompt for a
    // manifest.
    let rom = vec![0u8; 0x10_0000];
    let result = CartridgeBuilder::new(&rom).build();
    match result {
        Ok(_) => panic!("garbage ROM should not build"),
        Err(BuildError::NoMapping { threshold }) => {
            assert_eq!(threshold, 70, "default threshold");
        }
        Err(other) => panic!("expected NoMapping, got {other:?}"),
    }
}
