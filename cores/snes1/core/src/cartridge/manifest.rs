//! The SNES's part of the `cartridge-manifest-v1` format.
//!
//! The manifest format lets developers describe a cartridge by
//! hand instead of relying on heuristic detection. It is the only
//! way to express custom mappings, coprocessor chips, or non-stock
//! SRAM layouts. Manifests are pure data, they carry no code, no
//! binary blobs, and no version-coupled invariants beyond the
//! advertised `version` string. The format is retro-core's
//! ([`retro_core::cartridge::manifest`]), one for every console; what
//! the SNES adds is its slot (a bank by offset rectangle with an access
//! speed), its presets, the `fastrom` key of `mapping` and the
//! `controllers` key at the top.
//!
//! ## Pipeline
//!
//! 1. [`parse_manifest`] consumes a `&str` of JSON and returns a
//!    typed [`CartridgeManifest`] (or a typed [`ManifestError`]).
//! 2. [`manifest_to_map_plan`] consumes the parsed manifest and
//!    the ROM bytes, then produces a [`MapPlan`] ready for the
//!    cartridge builder.
//!
//! ## Schema (v1)
//!
//! ```json
//! {
//!   "$schema": "cartridge-manifest-v1",
//!   "core": "snes1",
//!   "version": "1.0",
//!   "rom": {
//!     "sha256": "abc123...",
//!     "size": 4194304,
//!     "title": "MY GAME",
//!     "region": "USA"
//!   },
//!   "mapping": {
//!     "preset": "lorom",
//!     "fastrom": true,
//!     "sram_size": 8192,
//!     "slots": [
//!       {
//!         "bank_range": ["0x60", "0x6F"],
//!         "offset_range": ["0x0000", "0x7FFF"],
//!         "target": { "kind": "Sram", "base_offset": 0 },
//!         "speed": "Slow"
//!       }
//!     ]
//!   },
//!   "chips": [
//!     {
//!       "id": 5,
//!       "kind": "sdd1",
//!       "mapping": {
//!         "bank_range": ["0xC0", "0xFF"],
//!         "offset_range": ["0x0000", "0xFFFF"],
//!         "target": { "kind": "ChipMmio", "chip_id": 5, "base": 0 },
//!         "speed": "Slow"
//!       },
//!       "observes": ["0x4300-0x437F"]
//!     }
//!   ]
//! }
//! ```
//!
//! ## Design notes
//!
//! - **`preset: "custom"`** skips the stock generator entirely.
//!   The plan starts empty and only the manifest's `slots` + chip
//!   slots end up in it. Use this for one-off mappings that no
//!   preset can express.
//! - **Manifest slot order.** Preset slots come first, then
//!   manifest custom slots, then chip slots. The bus walker
//!   resolves with a first-wins policy, so a manifest slot that
//!   overlaps a preset slot loses the overlap. Authors that need
//!   a custom slot to shadow a preset slot should set
//!   `preset: "custom"` instead of "lorom" / "hirom" / "exhirom".
//! - **`chips[].observes`.** Optional list of `"lo-hi"` 24-bit hex
//!   bus ranges (e.g. `"0x4300-0x437F"`) a chip passively *observes*
//!   but does not own. The builder registers them on the chip set so
//!   the bus forwards matching writes to the chip's `observe_write`
//!   (how an S-DD1 spies on the DMA registers). Absent for chips that
//!   observe nothing.
//! - **No SHA-256 verification here.** [`parse_manifest`] only
//!   checks the schema; verifying `rom.sha256` against the actual
//!   ROM bytes is the cartridge builder's job, since it
//!   already has the full ROM in hand.

use retro_core::cartridge::manifest::{
    chip_target, map_target, parse_hex_u16, parse_hex_u32, parse_hex_u8, ManifestConsole,
};
use serde::{Deserialize, Serialize};

pub use retro_core::cartridge::manifest::{
    append_manifest_extras, manifest_to_map_plan, ManifestControllers, ManifestError, ManifestRom,
    ManifestTarget, SUPPORTED_MANIFEST_VERSION,
};

use crate::cartridge::board::SnesBoard;
use crate::cartridge::map_plan::{BusSpeed, MapPlan, MapSlot};
use crate::cartridge::presets::{
    cx4_preset, exhirom_preset, hirom_preset, lorom_preset, sa1_preset, sdd1_preset,
    spc7110_preset, superfx_preset, PresetKind,
};

/// A SNES manifest. Mirrors the on-disk JSON 1:1.
pub type CartridgeManifest = retro_core::cartridge::manifest::CartridgeManifest<SnesBoard>;

/// A SNES manifest's mapping section: the preset, `fastrom`, the SRAM size
/// and the extra slots.
pub type ManifestMapping = retro_core::cartridge::manifest::ManifestMapping<SnesBoard>;

/// A chip a SNES manifest declares, with its slot.
pub type ManifestChip = retro_core::cartridge::manifest::ManifestChip<ManifestSlot>;

/// What the SNES adds to `mapping`: whether the preset maps its ROM as
/// FastROM.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnesMappingKnobs {
    #[serde(default)]
    pub fastrom: bool,
}

/// What the SNES adds at the top of a manifest: the controllers it names
/// for the ports, carried as written.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SnesManifestExtras {
    #[serde(default)]
    pub controllers: Option<ManifestControllers>,
}

/// One extra slot to append to the preset's plan. Hex addresses
/// arrive as strings so the manifest stays reviewable; the strings
/// are converted to `u8` / `u16` by [`manifest_to_map_plan`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestSlot {
    pub bank_range: [String; 2],
    pub offset_range: [String; 2],
    pub target: ManifestTarget,
    pub speed: String,
    #[serde(default)]
    pub mask: Option<String>,
    #[serde(default)]
    pub bank_stride: Option<String>,
}

/// Parse a JSON string into a typed [`CartridgeManifest`]. Returns
/// [`ManifestError::Json`] on syntax / shape errors, or
/// [`ManifestError::UnsupportedVersion`] if the `version` field
/// disagrees with [`SUPPORTED_MANIFEST_VERSION`].
pub fn parse_manifest(json: &str) -> Result<CartridgeManifest, ManifestError> {
    retro_core::cartridge::manifest::parse_manifest(json)
}

/// Parse the `mapping.preset` string into a [`PresetKind`]. Exposed
/// to the cartridge module so the builder can validate the field
/// upfront, even when `force_preset` will override the chosen kind.
pub(crate) fn parse_preset(s: &str) -> Result<PresetKind, ManifestError> {
    match s {
        "lorom" => Ok(PresetKind::LoRom),
        "hirom" => Ok(PresetKind::HiRom),
        "exhirom" => Ok(PresetKind::ExHiRom),
        "sdd1" => Ok(PresetKind::Sdd1),
        "spc7110" => Ok(PresetKind::Spc7110),
        "sa1" => Ok(PresetKind::Sa1),
        "superfx" => Ok(PresetKind::SuperFx),
        "cx4" => Ok(PresetKind::Cx4),
        "custom" => Ok(PresetKind::Custom),
        other => Err(ManifestError::UnknownPreset(other.to_string())),
    }
}

fn parse_speed(s: &str) -> Result<BusSpeed, ManifestError> {
    match s {
        "Fast" => Ok(BusSpeed::Fast),
        "Slow" => Ok(BusSpeed::Slow),
        "XSlow" => Ok(BusSpeed::XSlow),
        other => Err(ManifestError::UnknownSpeed(other.to_string())),
    }
}

impl ManifestConsole for SnesBoard {
    type MappingKnobs = SnesMappingKnobs;
    type Extras = SnesManifestExtras;
    type ManifestSlot = ManifestSlot;

    fn map_slot(
        slot: &ManifestSlot,
        path: &str,
        chip: Option<u8>,
    ) -> Result<Option<MapSlot>, ManifestError> {
        let bank_lo = parse_hex_u8(&slot.bank_range[0], &format!("{path}.bank_range[0]"))?;
        let bank_hi = parse_hex_u8(&slot.bank_range[1], &format!("{path}.bank_range[1]"))?;
        let off_lo = parse_hex_u16(&slot.offset_range[0], &format!("{path}.offset_range[0]"))?;
        let off_hi = parse_hex_u16(&slot.offset_range[1], &format!("{path}.offset_range[1]"))?;
        // A chip published in `chips[]` always dispatches through its
        // window, whatever the inline target says, so a chip block need
        // not repeat its own id on the inner slot.
        let target = match chip {
            Some(chip_id) => chip_target(&slot.target, chip_id),
            None => map_target(&slot.target),
        };
        let speed = parse_speed(&slot.speed)?;
        let mut map_slot = MapSlot::new(bank_lo..=bank_hi, off_lo..=off_hi, target, speed);
        if let Some(mask) = slot.mask.as_deref() {
            map_slot = map_slot.with_mask(parse_hex_u32(mask, &format!("{path}.mask"))?);
        }
        if let Some(stride) = slot.bank_stride.as_deref() {
            map_slot =
                map_slot.with_bank_stride(parse_hex_u32(stride, &format!("{path}.bank_stride"))?);
        }
        Ok(Some(map_slot))
    }

    /// The preset's plan, from `mapping.preset`, `fastrom` and `sram_size`.
    fn preset_plan(manifest: &CartridgeManifest, rom: &[u8]) -> Result<MapPlan, ManifestError> {
        let mapping = &manifest.mapping;
        let fastrom = mapping.knobs.fastrom;
        Ok(match parse_preset(&mapping.preset)? {
            PresetKind::LoRom => lorom_preset(rom, mapping.sram_size, fastrom),
            PresetKind::HiRom => hirom_preset(rom, mapping.sram_size, fastrom),
            PresetKind::ExHiRom => exhirom_preset(rom, mapping.sram_size, fastrom),
            PresetKind::Sdd1 => sdd1_preset(rom, mapping.sram_size, fastrom),
            PresetKind::Spc7110 => {
                // The manifest lacks the header's unpadded length and RTC
                // flag, so derive the wrap size from `rom.size` and map the
                // RTC ports unconditionally (the chip decides whether they
                // are live). The builder's own path threads the real header
                // values instead.
                spc7110_preset(
                    rom,
                    manifest.rom.size as usize,
                    mapping.sram_size,
                    fastrom,
                    true,
                )
            }
            PresetKind::Sa1 => sa1_preset(rom, mapping.sram_size, fastrom),
            PresetKind::SuperFx => superfx_preset(rom, mapping.sram_size),
            PresetKind::Cx4 => cx4_preset(rom, mapping.sram_size),
            PresetKind::Custom => MapPlan::new("custom"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cartridge::map_plan::MapTarget;

    fn minimal_lorom_manifest() -> &'static str {
        r#"{
            "core": "snes1",
            "version": "1.0",
            "rom": {
                "sha256": "abc",
                "size": 1048576,
                "title": "MY ROM"
            },
            "mapping": {
                "preset": "lorom",
                "fastrom": false,
                "sram_size": 0
            }
        }"#
    }

    #[test]
    fn parse_minimal_lorom_manifest() {
        let manifest = parse_manifest(minimal_lorom_manifest()).expect("parses");
        assert_eq!(manifest.core, "snes1");
        assert_eq!(manifest.version, "1.0");
        assert_eq!(manifest.mapping.preset, "lorom");
        assert!(manifest.mapping.slots.is_empty());
        assert!(manifest.chips.is_empty());
        assert_eq!(manifest.rom.sha256, "abc");
        assert_eq!(manifest.rom.size, 1_048_576);
    }

    #[test]
    fn parse_with_custom_slot_and_chip() {
        let json = r#"{
            "core": "snes1",
            "version": "1.0",
            "rom": { "sha256": "x", "size": 4194304 },
            "mapping": {
                "preset": "hirom",
                "fastrom": true,
                "sram_size": 8192,
                "slots": [
                    {
                        "bank_range": ["0x60", "0x6F"],
                        "offset_range": ["0x0000", "0x7FFF"],
                        "target": { "kind": "Sram", "base_offset": 0 },
                        "speed": "Slow"
                    }
                ]
            },
            "chips": [
                {
                    "id": 1,
                    "kind": "dsp1",
                    "mapping": {
                        "bank_range": ["0x30", "0x3F"],
                        "offset_range": ["0x8000", "0xBFFF"],
                        "target": { "kind": "ChipMmio", "chip_id": 1, "base": 0 },
                        "speed": "Fast"
                    }
                }
            ]
        }"#;
        let manifest = parse_manifest(json).expect("parses");
        assert_eq!(manifest.mapping.slots.len(), 1);
        assert_eq!(manifest.chips.len(), 1);
        assert_eq!(manifest.chips[0].id, 1);
        assert_eq!(manifest.chips[0].kind, "dsp1");
        assert_eq!(manifest.mapping.slots[0].bank_range, ["0x60", "0x6F"]);
    }

    #[test]
    fn manifest_to_plan_lorom_appends_custom_slots() {
        let json = r#"{
            "core": "snes1",
            "version": "1.0",
            "rom": { "sha256": "x", "size": 1048576 },
            "mapping": {
                "preset": "lorom",
                "fastrom": false,
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
        let manifest = parse_manifest(json).unwrap();
        let rom = vec![0u8; 0x10_0000];
        let plan = manifest_to_map_plan(&manifest, &rom).unwrap();
        // LoROM preset (no SRAM, no fastrom) gives 2 slots; the
        // custom SRAM slot is appended.
        assert_eq!(plan.slots.len(), 3);
        assert_eq!(plan.name, "lorom");
        let last = plan.slots.last().unwrap();
        assert_eq!(last.bank_range, 0x60..=0x6F);
        assert!(matches!(last.target, MapTarget::Sram { .. }));
    }

    #[test]
    fn manifest_to_plan_custom_preset_ignores_generators() {
        let json = r#"{
            "core": "snes1",
            "version": "1.0",
            "rom": { "sha256": "x", "size": 524288 },
            "mapping": {
                "preset": "custom",
                "slots": [
                    {
                        "bank_range": ["0x00", "0x3F"],
                        "offset_range": ["0x8000", "0xFFFF"],
                        "target": { "kind": "Rom", "base_offset": 0 },
                        "speed": "Slow"
                    },
                    {
                        "bank_range": ["0x80", "0xBF"],
                        "offset_range": ["0x8000", "0xFFFF"],
                        "target": { "kind": "Rom", "base_offset": 0 },
                        "speed": "Fast"
                    }
                ]
            }
        }"#;
        let manifest = parse_manifest(json).unwrap();
        let rom = vec![0u8; 0x8_0000];
        let plan = manifest_to_map_plan(&manifest, &rom).unwrap();
        assert_eq!(plan.name, "custom");
        assert_eq!(plan.slots.len(), 2);
        assert_eq!(plan.slots[0].speed, BusSpeed::Slow);
        assert_eq!(plan.slots[1].speed, BusSpeed::Fast);
    }

    #[test]
    fn manifest_to_plan_chip_uses_chip_id_dispatch() {
        let json = r#"{
            "core": "snes1",
            "version": "1.0",
            "rom": { "sha256": "x", "size": 1048576 },
            "mapping": { "preset": "custom" },
            "chips": [
                {
                    "id": 5,
                    "kind": "sdd1",
                    "mapping": {
                        "bank_range": ["0xC0", "0xFF"],
                        "offset_range": ["0x0000", "0xFFFF"],
                        "target": { "kind": "Rom", "base_offset": 0 },
                        "speed": "Slow"
                    }
                }
            ]
        }"#;
        let manifest = parse_manifest(json).unwrap();
        let rom = vec![0u8; 0x10_0000];
        let plan = manifest_to_map_plan(&manifest, &rom).unwrap();
        assert_eq!(plan.slots.len(), 1);
        match plan.slots[0].target {
            MapTarget::ChipMmio { chip_id, base } => {
                assert_eq!(chip_id, 5);
                assert_eq!(base, 0);
            }
            other => panic!("expected ChipMmio target, got {:?}", other),
        }
    }

    #[test]
    fn parse_error_unknown_preset() {
        let json = r#"{
            "core": "snes1",
            "version": "1.0",
            "rom": { "sha256": "x", "size": 1 },
            "mapping": { "preset": "exlorom" }
        }"#;
        let manifest = parse_manifest(json).unwrap();
        let err = manifest_to_map_plan(&manifest, &[]).unwrap_err();
        assert!(matches!(err, ManifestError::UnknownPreset(ref s) if s == "exlorom"));
    }

    #[test]
    fn parse_error_invalid_hex_in_bank_range() {
        let json = r#"{
            "core": "snes1",
            "version": "1.0",
            "rom": { "sha256": "x", "size": 1 },
            "mapping": {
                "preset": "custom",
                "slots": [
                    {
                        "bank_range": ["nope", "0xFF"],
                        "offset_range": ["0x0000", "0xFFFF"],
                        "target": { "kind": "OpenBus" },
                        "speed": "Slow"
                    }
                ]
            }
        }"#;
        let manifest = parse_manifest(json).unwrap();
        let err = manifest_to_map_plan(&manifest, &[]).unwrap_err();
        match err {
            ManifestError::InvalidHex { field, value } => {
                assert_eq!(field, "slots[0].bank_range[0]");
                assert_eq!(value, "nope");
            }
            other => panic!("expected InvalidHex, got {:?}", other),
        }
    }

    #[test]
    fn round_trip_serialize_deserialize() {
        let original = parse_manifest(minimal_lorom_manifest()).unwrap();
        let serialized = serde_json::to_string(&original).unwrap();
        let round = parse_manifest(&serialized).unwrap();
        assert_eq!(original.mapping.preset, round.mapping.preset);
        assert_eq!(original.rom.sha256, round.rom.sha256);
    }

    #[test]
    fn manifest_slot_with_mask_and_bank_stride() {
        let json = r#"{
            "core": "snes1",
            "version": "1.0",
            "rom": { "sha256": "x", "size": 1 },
            "mapping": {
                "preset": "custom",
                "slots": [
                    {
                        "bank_range": ["0x00", "0x3F"],
                        "offset_range": ["0x8000", "0xFFFF"],
                        "target": { "kind": "Rom", "base_offset": 32768 },
                        "speed": "Slow",
                        "mask": "0x1FFFFF",
                        "bank_stride": "0x10000"
                    }
                ]
            }
        }"#;
        let manifest = parse_manifest(json).unwrap();
        let plan = manifest_to_map_plan(&manifest, &[]).unwrap();
        assert_eq!(plan.slots[0].mask, Some(0x1F_FFFF));
        assert_eq!(plan.slots[0].bank_stride, Some(0x10000));
    }

    #[test]
    fn chip_observes_parses_and_defaults() {
        // A chip with `observes` carries the raw strings; a chip without
        // the key defaults to an empty list (serde default).
        let json = r#"{
            "core": "snes1",
            "version": "1.0",
            "rom": { "sha256": "x", "size": 1048576 },
            "mapping": { "preset": "custom" },
            "chips": [
                {
                    "id": 5,
                    "kind": "sdd1",
                    "mapping": {
                        "bank_range": ["0xC0", "0xFF"],
                        "offset_range": ["0x0000", "0xFFFF"],
                        "target": { "kind": "ChipMmio", "chip_id": 5, "base": 0 },
                        "speed": "Slow"
                    },
                    "observes": ["0x4300-0x437F"]
                },
                {
                    "id": 1,
                    "kind": "dsp1",
                    "mapping": {
                        "bank_range": ["0x30", "0x3F"],
                        "offset_range": ["0x8000", "0xBFFF"],
                        "target": { "kind": "ChipMmio", "chip_id": 1, "base": 0 },
                        "speed": "Fast"
                    }
                }
            ]
        }"#;
        let manifest = parse_manifest(json).unwrap();
        assert_eq!(
            manifest.chips[0].observes,
            vec!["0x4300-0x437F".to_string()]
        );
        assert!(manifest.chips[1].observes.is_empty());
    }

    #[test]
    fn fastrom_and_controllers_take_their_places() {
        let json = r#"{
            "core": "snes1",
            "version": "1.0",
            "rom": { "sha256": "x", "size": 1048576 },
            "mapping": { "preset": "lorom", "fastrom": true, "sram_size": 0 },
            "controllers": { "port1": "pad", "port2": "mouse" }
        }"#;
        let manifest = parse_manifest(json).unwrap();
        assert!(manifest.mapping.knobs.fastrom);
        let controllers = manifest.console.controllers.as_ref().unwrap();
        assert_eq!(controllers.port1.as_deref(), Some("pad"));
        assert_eq!(controllers.port2.as_deref(), Some("mouse"));
        let written = serde_json::to_string(&manifest).unwrap();
        assert!(written.contains(r#""mapping":{"preset":"lorom","fastrom":true,"sram_size":0"#));
        assert!(written.contains(
            r#""chips":[],"controllers":{"port1":"pad","port2":"mouse"},"metadata":null"#
        ));
        let plan = manifest_to_map_plan(&manifest, &vec![0u8; 0x10_0000]).unwrap();
        assert!(plan.slots.iter().any(|slot| slot.speed == BusSpeed::Fast));
    }
}
