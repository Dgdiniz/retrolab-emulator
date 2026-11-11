//! JSON cartridge manifest (`cartridge-manifest-v1`, Genesis flavor).
//!
//! The manifest lets a developer describe a cartridge by hand instead of
//! relying on heuristic detection. It is the only way to express the special
//! mappers, coprocessors, or non-stock SRAM layouts the Genesis header
//! cannot advertise. It is pure data: no code, no blobs, no version-coupled
//! invariants beyond the advertised `version` string.
//!
//! This is the Genesis counterpart of the SNES manifest. The one shape
//! difference is the address model: the 68000 has a flat 24-bit space, so a
//! slot is a single `addr_range` (`["0x000000", "0x3FFFFF"]`) instead of the
//! SNES `bank_range x offset_range` rectangle. The [`MapTarget`] vocabulary
//! (`Rom`/`Sram`/`ChipMmio`/`OpenBus`) is the shared `retro-core` type,
//! identical across both consoles.
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
//! - **Hex addresses as strings.** Address ranges arrive as `"0x300000"`
//!   strings so the on-disk format is reviewable by humans; the helpers
//!   parse both `"0x..."` and bare-hex forms.
//! - **Presets.** `mapping.preset` seeds the plan with a stock shape
//!   (`plain` / `sega` / `ssf2` / `codemasters` / `eeprom` / `jcart` /
//!   `svp`), or `custom` for an empty plan the `slots`/`chips` fill in. A
//!   chip preset uses the first `chips[]` entry's id for its MMIO window.
//! - **Slot order.** Preset slots come first, then manifest custom slots,
//!   then chip slots. [`MapPlan::resolve`] is first-wins, so a manifest slot
//!   that overlaps a preset slot loses the overlap, use `preset: "custom"`
//!   to shadow a preset slot deliberately.
//! - **`chips[].observes`.** Optional `"lo-hi"` 24-bit hex ranges a chip
//!   passively watches (e.g. `"0xA130F0-0xA130FF"` for a SEGA mapper's bank
//!   registers). The builder registers them on the `ChipSet`.
//! - **No SHA-256 verification.** [`parse_manifest`] checks the schema, the
//!   version and the SRAM size; nothing compares `rom.sha256` with the ROM
//!   (`load_rom` takes a staged manifest as trusted metadata).

use serde::{Deserialize, Serialize};

use super::map_plan::{MapPlan, MapSlot, MapTarget};
use super::presets::{
    bank_mapper_plan, eeprom_plan, jcart_plan, plain_plan, svp_plan, PresetKind,
    EEPROM_CONTROL_ADDR,
};
use super::savestate::BankMapperKind;
use crate::cartridge::parsers::SRAM_SIZE_LIMIT;

/// Manifest format version this module accepts.
pub const SUPPORTED_MANIFEST_VERSION: &str = "1.0";

/// The default stock-SRAM window base for the `sram_size` shorthand, when a
/// manifest does not spell out an explicit `Sram` slot.
pub const DEFAULT_SRAM_BASE: u32 = 0x200000;

/// Top-level manifest. Mirrors the on-disk JSON 1:1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CartridgeManifest {
    #[serde(rename = "$schema", default)]
    pub schema: Option<String>,
    pub core: String,
    pub version: String,
    pub rom: ManifestRom,
    pub mapping: ManifestMapping,
    #[serde(default)]
    pub chips: Vec<ManifestChip>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

/// ROM-image metadata: digest + advertised size + optional labels. The
/// digest is *not* verified by [`parse_manifest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestRom {
    pub sha256: String,
    pub size: u64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
}

/// Mapping section: chosen preset + stock-SRAM size + extra slots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestMapping {
    pub preset: String,
    #[serde(default)]
    pub sram_size: usize,
    #[serde(default)]
    pub slots: Vec<ManifestSlot>,
}

/// One extra linear slot. Hex addresses arrive as strings so the manifest
/// stays reviewable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestSlot {
    pub addr_range: [String; 2],
    pub target: ManifestTarget,
    #[serde(default)]
    pub mask: Option<String>,
}

/// What lives behind a slot. Tag-by-`kind` so the JSON reads naturally.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ManifestTarget {
    Rom {
        #[serde(default)]
        base_offset: u32,
    },
    Sram {
        #[serde(default)]
        base_offset: u32,
    },
    ChipMmio {
        chip_id: u8,
        #[serde(default)]
        base: u32,
    },
    OpenBus,
}

/// Coprocessor chip declaration. Each chip carries its own mapping slot plus
/// an optional list of bus ranges it passively observes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestChip {
    pub id: u8,
    pub kind: String,
    pub mapping: ManifestSlot,
    #[serde(default)]
    pub observes: Vec<String>,
}

/// All the ways [`parse_manifest`] / [`manifest_to_map_plan`] can fail.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported manifest version: {0}")]
    UnsupportedVersion(String),
    #[error("unsupported preset: {0}")]
    UnknownPreset(String),
    #[error("invalid hex in field '{field}': {value}")]
    InvalidHex { field: String, value: String },
    #[error("sram_size {size} is larger than the {limit}-byte limit")]
    SramTooLarge { size: usize, limit: u32 },
}

/// Parse a JSON string into a typed [`CartridgeManifest`].
pub fn parse_manifest(json: &str) -> Result<CartridgeManifest, ManifestError> {
    let manifest: CartridgeManifest = serde_json::from_str(json)?;
    if manifest.version != SUPPORTED_MANIFEST_VERSION {
        return Err(ManifestError::UnsupportedVersion(manifest.version));
    }
    if manifest.mapping.sram_size > SRAM_SIZE_LIMIT as usize {
        return Err(ManifestError::SramTooLarge {
            size: manifest.mapping.sram_size,
            limit: SRAM_SIZE_LIMIT,
        });
    }
    Ok(manifest)
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

/// Turn a parsed manifest into a [`MapPlan`]. The preset seeds the plan; the
/// manifest's custom slots and chip mappings append in source order. A chip
/// preset (sega/ssf2/...) uses the first `chips[]` entry's id for its MMIO
/// window.
pub fn manifest_to_map_plan(
    manifest: &CartridgeManifest,
    rom: &[u8],
) -> Result<MapPlan, ManifestError> {
    let kind = parse_preset(&manifest.mapping.preset)?;
    let chip0_id = manifest.chips.first().map(|c| c.id).unwrap_or(0);
    let sram = manifest_sram_window(manifest);

    let mut plan = match kind {
        PresetKind::Plain => plain_plan(rom.len(), sram),
        PresetKind::Sega => bank_mapper_plan(BankMapperKind::Sega, chip0_id, sram),
        PresetKind::Ssf2 => bank_mapper_plan(BankMapperKind::Ssf2, chip0_id, sram),
        PresetKind::Codemasters => bank_mapper_plan(BankMapperKind::Codemasters, chip0_id, sram),
        PresetKind::Eeprom => eeprom_plan(chip0_id, rom.len(), EEPROM_CONTROL_ADDR),
        PresetKind::JCart => jcart_plan(chip0_id, rom.len(), sram),
        PresetKind::Svp => svp_plan(chip0_id, rom.len()),
        PresetKind::Custom => MapPlan::new("custom"),
    };

    append_manifest_extras(&mut plan, manifest)?;
    Ok(plan)
}

/// Append the manifest's custom slots and chip mappings to a plan. The bus's
/// first-wins resolver means preset slots beat manifest slots on overlap.
pub fn append_manifest_extras(
    plan: &mut MapPlan,
    manifest: &CartridgeManifest,
) -> Result<(), ManifestError> {
    for (i, slot) in manifest.mapping.slots.iter().enumerate() {
        let map_slot = manifest_slot_to_map_slot(slot, &format!("slots[{i}]"), None)?;
        plan.push(map_slot);
    }
    for (i, chip) in manifest.chips.iter().enumerate() {
        let path = format!("chips[{i}].mapping");
        let map_slot = manifest_slot_to_map_slot(&chip.mapping, &path, Some(chip.id))?;
        plan.push(map_slot);
    }
    Ok(())
}

fn manifest_slot_to_map_slot(
    slot: &ManifestSlot,
    path: &str,
    chip_id_override: Option<u8>,
) -> Result<MapSlot, ManifestError> {
    let lo = parse_hex_u32(&slot.addr_range[0], &format!("{path}.addr_range[0]"))?;
    let hi = parse_hex_u32(&slot.addr_range[1], &format!("{path}.addr_range[1]"))?;

    let mut target = manifest_target_to_map_target(&slot.target);
    if let Some(chip_id) = chip_id_override {
        // A chip published in `chips[]` always dispatches through ChipMmio,
        // regardless of the inline target, so the block need not repeat its
        // own id on the inner slot.
        target = MapTarget::ChipMmio {
            chip_id,
            base: match slot.target {
                ManifestTarget::ChipMmio { base, .. } => base,
                ManifestTarget::Rom { base_offset } | ManifestTarget::Sram { base_offset } => {
                    base_offset
                }
                ManifestTarget::OpenBus => 0,
            },
        };
    }

    let mut map_slot = MapSlot::new(lo..=hi, target);
    if let Some(mask_str) = slot.mask.as_deref() {
        let mask = parse_hex_u32(mask_str, &format!("{path}.mask"))?;
        map_slot = map_slot.with_mask(mask);
    }
    Ok(map_slot)
}

fn manifest_target_to_map_target(t: &ManifestTarget) -> MapTarget {
    match *t {
        ManifestTarget::Rom { base_offset } => MapTarget::Rom { base_offset },
        ManifestTarget::Sram { base_offset } => MapTarget::Sram { base_offset },
        ManifestTarget::ChipMmio { chip_id, base } => MapTarget::ChipMmio { chip_id, base },
        ManifestTarget::OpenBus => MapTarget::OpenBus,
    }
}

/// Parse an inclusive 24-bit range like `"0xA130F0-0xA130FF"` into a
/// [`std::ops::RangeInclusive<u32>`], for the per-chip `observes[]` ranges.
pub fn parse_addr_range(
    s: &str,
    field: &str,
) -> Result<std::ops::RangeInclusive<u32>, ManifestError> {
    let bounds = s
        .split_once('-')
        .and_then(|(lo, hi)| Some((parse_hex_inner(lo.trim())?, parse_hex_inner(hi.trim())?)));
    match bounds {
        Some((lo, hi)) if lo <= hi => Ok(lo..=hi),
        _ => Err(ManifestError::InvalidHex {
            field: field.to_string(),
            value: s.to_string(),
        }),
    }
}

fn parse_hex_u32(s: &str, field: &str) -> Result<u32, ManifestError> {
    parse_hex_inner(s).ok_or_else(|| ManifestError::InvalidHex {
        field: field.to_string(),
        value: s.to_string(),
    })
}

fn parse_hex_inner(s: &str) -> Option<u32> {
    let stripped = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if stripped.is_empty() || stripped.len() > 8 {
        return None;
    }
    u32::from_str_radix(stripped, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

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
                base: super::super::presets::SVP_MAILBOX_BASE
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
    fn error_unsupported_version() {
        let json = r#"{
            "core": "sg1",
            "version": "9.9",
            "rom": { "sha256": "x", "size": 1 },
            "mapping": { "preset": "plain" }
        }"#;
        let err = parse_manifest(json).unwrap_err();
        assert!(matches!(err, ManifestError::UnsupportedVersion(ref s) if s == "9.9"));
    }

    #[test]
    fn parse_addr_range_valid_and_malformed() {
        assert_eq!(
            parse_addr_range("0xA130F0-0xA130FF", "observes[0]").unwrap(),
            0xA130F0..=0xA130FF
        );
        assert_eq!(
            parse_addr_range(" A130F0 - A130FF ", "observes[0]").unwrap(),
            0xA130F0..=0xA130FF
        );
        for bad in ["0xA130F0", "0xA130F0-nope", "0xA130FF-0xA130F0"] {
            assert!(matches!(
                parse_addr_range(bad, "observes[1]"),
                Err(ManifestError::InvalidHex { .. })
            ));
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
