//! The Mega Drive's part of the `cartridge-manifest-v1` format.
//!
//! The manifest lets a developer describe a cartridge by hand instead of
//! relying on heuristic detection. It is the only way to express the special
//! mappers, coprocessors, or non-stock SRAM layouts the Genesis header
//! cannot advertise. The format is retro-core's
//! ([`retro_core::cartridge::manifest`]), one for every console; what the
//! Mega Drive adds is its slot, its presets and its SRAM rules. The 68000
//! has a flat 24-bit space, so a slot is a single `addr_range`
//! (`["0x000000", "0x3FFFFF"]`), where the SNES writes a
//! `bank_range x offset_range` rectangle. The Mega Drive adds no key of its
//! own to `mapping` or to the top.
//!
//! ## Pipeline
//!
//! 1. [`parse_manifest`] consumes a `&str` of JSON -> a typed
//!    [`CartridgeManifest`] (or a typed [`ManifestError`]).
//! 2. [`manifest_to_map_plan`] consumes the parsed manifest + the ROM bytes
//!    -> a [`MapPlan`]. The cartridge builder pairs that plan with the chips
//!    named in `chips[]` (built through [`Sg1ChipFactory`](super::factory)).
//!
//! ## Schema (v1)
//!
//! ```json
//! {
//!   "$schema": "cartridge-manifest-v1",
//!   "core": "sg1",
//!   "version": "1.0",
//!   "rom": { "sha256": "abc123...", "size": 2097152, "title": "VIRTUA RACING" },
//!   "mapping": { "preset": "svp", "sram_size": 0, "slots": [] },
//!   "chips": [
//!     {
//!       "id": 0,
//!       "kind": "svp",
//!       "mapping": {
//!         "addr_range": ["0x300000", "0x31FFFF"],
//!         "target": { "kind": "ChipMmio", "chip_id": 0, "base": 0 }
//!       },
//!       "observes": []
//!     }
//!   ]
//! }
//! ```
//!
//! ## Design notes
//!
//! - **Presets.** `mapping.preset` seeds the plan with a stock shape
//!   (`plain` / `sega` / `ssf2` / `codemasters` / `eeprom` / `jcart` /
//!   `svp`), or `custom` for an empty plan the `slots`/`chips` fill in. A
//!   chip preset uses the first `chips[]` entry's id for its MMIO window.
//! - **SRAM.** `mapping.sram_size` asks for a stock SRAM window at
//!   [`DEFAULT_SRAM_BASE`]; a size over [`SRAM_SIZE_LIMIT`] is refused.
//! - **`chips[].observes`.** Optional `"lo-hi"` 24-bit hex ranges a chip
//!   passively watches (e.g. `"0xA130F0-0xA130FF"` for a SEGA mapper's bank
//!   registers). The builder registers them on the chip set.
//! - **No SHA-256 verification.** [`parse_manifest`] checks the schema, the
//!   version and the SRAM size; nothing compares `rom.sha256` with the ROM
//!   (`load_rom` takes a staged manifest as trusted metadata).

use retro_core::cartridge::manifest::{
    chip_target, map_target, parse_hex_u32, ManifestConsole, NoFields,
};
use serde::{Deserialize, Serialize};

pub use retro_core::cartridge::manifest::{
    append_manifest_extras, manifest_to_map_plan, parse_addr_range, ManifestError, ManifestRom,
    ManifestTarget, SUPPORTED_MANIFEST_VERSION,
};

use super::board::GenesisBoard;
use super::map_plan::{MapPlan, MapSlot};
use super::presets::{
    bank_mapper_plan, eeprom_plan, jcart_plan, plain_plan, svp_plan, PresetKind,
    EEPROM_CONTROL_ADDR,
};
use super::savestate::BankMapperKind;
use crate::cartridge::header::SRAM_SIZE_LIMIT;

/// The default stock-SRAM window base for the `sram_size` shorthand, when a
/// manifest does not spell out an explicit `Sram` slot.
pub const DEFAULT_SRAM_BASE: u32 = 0x200000;

/// A Mega Drive manifest. Mirrors the on-disk JSON 1:1.
pub type CartridgeManifest = retro_core::cartridge::manifest::CartridgeManifest<GenesisBoard>;

/// A Mega Drive manifest's mapping section: the preset, the stock-SRAM size
/// and the extra slots.
pub type ManifestMapping = retro_core::cartridge::manifest::ManifestMapping<GenesisBoard>;

/// A chip a Mega Drive manifest declares, with its slot.
pub type ManifestChip = retro_core::cartridge::manifest::ManifestChip<ManifestSlot>;

/// One extra linear slot. Hex addresses arrive as strings so the manifest
/// stays reviewable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestSlot {
    pub addr_range: [String; 2],
    pub target: ManifestTarget,
    #[serde(default)]
    pub mask: Option<String>,
}

/// Parse a JSON string into a typed [`CartridgeManifest`].
pub fn parse_manifest(json: &str) -> Result<CartridgeManifest, ManifestError> {
    retro_core::cartridge::manifest::parse_manifest(json)
}

/// Parse the `mapping.preset` string into a [`PresetKind`].
pub fn parse_preset(s: &str) -> Result<PresetKind, ManifestError> {
    match s {
        "plain" => Ok(PresetKind::Plain),
        "sega" => Ok(PresetKind::Sega),
        "ssf2" => Ok(PresetKind::Ssf2),
        "codemasters" => Ok(PresetKind::Codemasters),
        "eeprom" => Ok(PresetKind::Eeprom),
        "jcart" => Ok(PresetKind::JCart),
        "svp" => Ok(PresetKind::Svp),
        "custom" => Ok(PresetKind::Custom),
        other => Err(ManifestError::UnknownPreset(other.to_string())),
    }
}

/// The stock-SRAM window a manifest requests via `sram_size`, if any, cut
/// to [`SRAM_SIZE_LIMIT`] (which [`parse_manifest`] refuses to exceed, so
/// the cut only reaches a manifest built in code).
pub fn manifest_sram_window(manifest: &CartridgeManifest) -> Option<(u32, u32)> {
    if manifest.mapping.sram_size > 0 {
        let len = manifest.mapping.sram_size.min(SRAM_SIZE_LIMIT as usize) as u32;
        Some((DEFAULT_SRAM_BASE, len))
    } else {
        None
    }
}

impl ManifestConsole for GenesisBoard {
    type MappingKnobs = NoFields;
    type Extras = NoFields;
    type ManifestSlot = ManifestSlot;

    fn map_slot(
        slot: &ManifestSlot,
        path: &str,
        chip: Option<u8>,
    ) -> Result<Option<MapSlot>, ManifestError> {
        let lo = parse_hex_u32(&slot.addr_range[0], &format!("{path}.addr_range[0]"))?;
        let hi = parse_hex_u32(&slot.addr_range[1], &format!("{path}.addr_range[1]"))?;
        let target = match chip {
            Some(chip_id) => chip_target(&slot.target, chip_id),
            None => map_target(&slot.target),
        };
        let mut map_slot = MapSlot::new(lo..=hi, target);
        if let Some(mask) = slot.mask.as_deref() {
            map_slot = map_slot.with_mask(parse_hex_u32(mask, &format!("{path}.mask"))?);
        }
        Ok(Some(map_slot))
    }

    /// The preset's plan; a chip preset (sega/ssf2/...) uses the first
    /// `chips[]` entry's id for its MMIO window.
    fn preset_plan(manifest: &CartridgeManifest, rom: &[u8]) -> Result<MapPlan, ManifestError> {
        let kind = parse_preset(&manifest.mapping.preset)?;
        let chip0_id = manifest.chips.first().map_or(0, |c| c.id);
        let sram = manifest_sram_window(manifest);
        Ok(match kind {
            PresetKind::Plain => plain_plan(rom.len(), sram),
            PresetKind::Sega => bank_mapper_plan(BankMapperKind::Sega, chip0_id, sram),
            PresetKind::Ssf2 => bank_mapper_plan(BankMapperKind::Ssf2, chip0_id, sram),
            PresetKind::Codemasters => {
                bank_mapper_plan(BankMapperKind::Codemasters, chip0_id, sram)
            }
            PresetKind::Eeprom => eeprom_plan(chip0_id, rom.len(), EEPROM_CONTROL_ADDR),
            PresetKind::JCart => jcart_plan(chip0_id, rom.len(), sram),
            PresetKind::Svp => svp_plan(chip0_id, rom.len()),
            PresetKind::Custom => MapPlan::new("custom"),
        })
    }

    /// A stock SRAM over [`SRAM_SIZE_LIMIT`] is refused.
    fn validate(manifest: &CartridgeManifest) -> Result<(), ManifestError> {
        if manifest.mapping.sram_size > SRAM_SIZE_LIMIT as usize {
            return Err(ManifestError::SramTooLarge {
                size: manifest.mapping.sram_size,
                limit: SRAM_SIZE_LIMIT,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cartridge::map_plan::MapTarget;
    use crate::cartridge::presets::SVP_MAILBOX_BASE;

    fn minimal_plain() -> &'static str {
        r#"{
            "core": "sg1",
            "version": "1.0",
            "rom": { "sha256": "abc", "size": 524288, "title": "MY ROM" },
            "mapping": { "preset": "plain", "sram_size": 0 }
        }"#
    }

    #[test]
    fn parse_minimal_plain_manifest() {
        let m = parse_manifest(minimal_plain()).expect("parses");
        assert_eq!(m.core, "sg1");
        assert_eq!(m.version, "1.0");
        assert_eq!(m.mapping.preset, "plain");
        assert!(m.mapping.slots.is_empty());
        assert!(m.chips.is_empty());
        assert_eq!(m.rom.sha256, "abc");
        assert_eq!(m.rom.size, 524_288);
    }

    #[test]
    fn a_manifest_cannot_ask_for_more_sram_than_the_limit() {
        let json = |sram_size: u32| {
            format!(
                r#"{{
                    "core": "sg1",
                    "version": "1.0",
                    "rom": {{ "sha256": "x", "size": 524288 }},
                    "mapping": {{ "preset": "plain", "sram_size": {sram_size} }}
                }}"#
            )
        };
        assert!(parse_manifest(&json(SRAM_SIZE_LIMIT)).is_ok());
        let err = parse_manifest(&json(SRAM_SIZE_LIMIT + 1)).unwrap_err();
        assert!(matches!(
            err,
            ManifestError::SramTooLarge { size, limit }
                if size == SRAM_SIZE_LIMIT as usize + 1 && limit == SRAM_SIZE_LIMIT
        ));
    }

    #[test]
    fn plan_plain_with_sram() {
        let json = r#"{
            "core": "sg1",
            "version": "1.0",
            "rom": { "sha256": "x", "size": 2097152 },
            "mapping": { "preset": "plain", "sram_size": 32768 }
        }"#;
        let m = parse_manifest(json).unwrap();
        let plan = manifest_to_map_plan(&m, &vec![0u8; 0x200000]).unwrap();
        assert_eq!(plan.name, "plain");
        // SRAM slot + ROM slot.
        assert_eq!(plan.slots.len(), 2);
        let s = plan.resolve(0x200000).unwrap();
        assert!(matches!(
            plan.slots[s.slot_index as usize].target,
            MapTarget::Sram { .. }
        ));
    }

    #[test]
    fn plan_svp_uses_first_chip_id() {
        let json = r#"{
            "core": "sg1",
            "version": "1.0",
            "rom": { "sha256": "x", "size": 2097152, "title": "VIRTUA RACING" },
            "mapping": { "preset": "svp", "sram_size": 0 },
            "chips": [
                {
                    "id": 3,
                    "kind": "svp",
                    "mapping": {
                        "addr_range": ["0xA15000", "0xA1500B"],
                        "target": { "kind": "ChipMmio", "chip_id": 3, "base": 65536 }
                    }
                }
            ]
        }"#;
        let m = parse_manifest(json).unwrap();
        let plan = manifest_to_map_plan(&m, &vec![0u8; 0x200000]).unwrap();
        assert_eq!(plan.name, "svp");
        // The preset DRAM window uses chip id 3.
        let dram = plan.resolve(0x300000).unwrap();
        assert!(matches!(
            plan.slots[dram.slot_index as usize].target,
            MapTarget::ChipMmio {
                chip_id: 3,
                base: 0
            }
        ));
        // The SVP preset owns the 0xA15000 mailbox window (at
        // SVP_MAILBOX_BASE), propagating the manifest's chip id 3. It is
        // pushed ahead of the redundant chip-declared slot, so first-wins
        // resolution returns the preset's window, the documented
        // "preset beats an overlapping manifest slot" rule.
        let mailbox = plan.resolve(0xA15004).unwrap();
        assert!(matches!(
            plan.slots[mailbox.slot_index as usize].target,
            MapTarget::ChipMmio {
                chip_id: 3,
                base: SVP_MAILBOX_BASE
            }
        ));
    }

    #[test]
    fn plan_custom_appends_slots_and_chip() {
        let json = r#"{
            "core": "sg1",
            "version": "1.0",
            "rom": { "sha256": "x", "size": 4194304 },
            "mapping": {
                "preset": "custom",
                "slots": [
                    {
                        "addr_range": ["0x000000", "0x3FFFFF"],
                        "target": { "kind": "Rom", "base_offset": 0 },
                        "mask": "0x3FFFFF"
                    }
                ]
            },
            "chips": [
                {
                    "id": 1,
                    "kind": "eeprom",
                    "mapping": {
                        "addr_range": ["0x200001", "0x200001"],
                        "target": { "kind": "Rom", "base_offset": 0 }
                    },
                    "observes": []
                }
            ]
        }"#;
        let m = parse_manifest(json).unwrap();
        let plan = manifest_to_map_plan(&m, &vec![0u8; 0x400000]).unwrap();
        assert_eq!(plan.name, "custom");
        // Custom ROM slot first, then the appended chip slot.
        assert_eq!(plan.slots.len(), 2);
        assert!(matches!(
            plan.slots[0].target,
            MapTarget::Rom { base_offset: 0 }
        ));
        // The chip slot is forced to ChipMmio using the chip id, even though
        // its inline `target` said `Rom`.
        assert!(matches!(
            plan.slots[1].target,
            MapTarget::ChipMmio {
                chip_id: 1,
                base: 0
            }
        ));
    }

    #[test]
    fn error_unknown_preset() {
        let json = r#"{
            "core": "sg1",
            "version": "1.0",
            "rom": { "sha256": "x", "size": 1 },
            "mapping": { "preset": "megadrive" }
        }"#;
        let m = parse_manifest(json).unwrap();
        let err = manifest_to_map_plan(&m, &[]).unwrap_err();
        assert!(matches!(err, ManifestError::UnknownPreset(ref s) if s == "megadrive"));
    }

    #[test]
    fn error_invalid_hex_in_addr_range() {
        let json = r#"{
            "core": "sg1",
            "version": "1.0",
            "rom": { "sha256": "x", "size": 1 },
            "mapping": {
                "preset": "custom",
                "slots": [
                    {
                        "addr_range": ["nope", "0x3FFFFF"],
                        "target": { "kind": "OpenBus" }
                    }
                ]
            }
        }"#;
        let m = parse_manifest(json).unwrap();
        let err = manifest_to_map_plan(&m, &[]).unwrap_err();
        match err {
            ManifestError::InvalidHex { field, value } => {
                assert_eq!(field, "slots[0].addr_range[0]");
                assert_eq!(value, "nope");
            }
            other => panic!("expected InvalidHex, got {other:?}"),
        }
    }

    #[test]
    fn round_trip_serialize_deserialize() {
        let original = parse_manifest(minimal_plain()).unwrap();
        let serialized = serde_json::to_string(&original).unwrap();
        let round = parse_manifest(&serialized).unwrap();
        assert_eq!(original.mapping.preset, round.mapping.preset);
        assert_eq!(original.rom.sha256, round.rom.sha256);
    }
}
