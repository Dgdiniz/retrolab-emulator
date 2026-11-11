//! JSON cartridge manifest (`cartridge-manifest-v1`).
//!
//! The manifest format lets developers describe a cartridge by
//! hand instead of relying on heuristic detection. It is the only
//! way to express custom mappings, coprocessor chips, or non-stock
//! SRAM layouts. Manifests are pure data, they carry no code, no
//! binary blobs, and no version-coupled invariants beyond the
//! advertised `version` string.
//!
//! ## Pipeline
//!
//! 1. [`parse_manifest`] consumes a `&str` of JSON and returns a
//!    typed [`CartridgeManifest`] (or a typed [`ManifestError`]).
//! 2. [`manifest_to_map_plan`] consumes the parsed manifest and
//!    the ROM bytes, then produces a [`MapPlan`] ready for
//!    [`crate::cartridge::PlainCartridge::new`] or a custom
//!    cartridge implementation.
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
//! - **Hex addresses as strings.** Bank and offset ranges arrive
//!   as `"0x80"` / `"0xFF"` strings instead of integers so the
//!   on-disk format is reviewable by humans. The
//!   [`parse_hex_u8`] / [`parse_hex_u16`] helpers handle both
//!   `"0x80"` and `"80"` forms without taking a new dependency.
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
//!   but does not own. The builder parses them with
//!   [`parse_addr_range`] and registers them on the `ChipSet` so the
//!   bus forwards matching writes to the chip's `observe_write` (how an
//!   S-DD1 spies on the DMA registers). Absent for chips that observe
//!   nothing.
//! - **No SHA-256 verification here.** [`parse_manifest`] only
//!   checks the schema; verifying `rom.sha256` against the actual
//!   ROM bytes is the cartridge builder's job, since it
//!   already has the full ROM in hand.

use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::cartridge::map_plan::{BusSpeed, MapPlan, MapSlot, MapTarget};
use crate::cartridge::presets::{
    cx4_preset, exhirom_preset, hirom_preset, lorom_preset, sa1_preset, sdd1_preset,
    spc7110_preset, superfx_preset, PresetKind,
};

/// Manifest format version this module accepts. Bumping the JSON
/// `version` field beyond this string forces callers to upgrade
/// (a hard `ManifestError::UnsupportedVersion`).
pub const SUPPORTED_MANIFEST_VERSION: &str = "1.0";

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
    pub controllers: Option<ManifestControllers>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

/// ROM-image metadata: digest + advertised size + optional human
/// labels. The digest is *not* verified by [`parse_manifest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestRom {
    pub sha256: String,
    pub size: u64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
}

/// Mapping section: chosen preset + per-mapper knobs + extra slots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestMapping {
    pub preset: String,
    #[serde(default)]
    pub fastrom: bool,
    #[serde(default)]
    pub sram_size: usize,
    #[serde(default)]
    pub slots: Vec<ManifestSlot>,
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

/// What lives behind a slot. Tag-by-`kind` so the JSON reads
/// naturally as `{ "kind": "Rom", "base_offset": 0 }`.
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

/// Coprocessor chip declaration. Each chip carries its own mapping
/// slot (the bus dispatch fires `read_chip` / `write_chip` with
/// the `chip_id` and the slot-relative offset), plus an optional list
/// of bus ranges it passively *observes*.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestChip {
    pub id: u8,
    pub kind: String,
    pub mapping: ManifestSlot,
    /// Bus address ranges the chip watches but does not own, each a
    /// `"lo-hi"` 24-bit hex range like `"0x4300-0x437F"`. The builder
    /// parses these with [`parse_addr_range`] and registers them on the
    /// `ChipSet` (`add_observer`) so a matching bus write is forwarded to
    /// the chip's `observe_write`, how an S-DD1 reconstructs the DMA
    /// parameters without coupling the DMA engine. Empty (the default) for
    /// chips that observe nothing, e.g. the DSP-1.
    #[serde(default)]
    pub observes: Vec<String>,
}

/// Optional controller hints. Reserved for future use; carried
/// verbatim today.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestControllers {
    #[serde(default)]
    pub port1: Option<String>,
    #[serde(default)]
    pub port2: Option<String>,
}

/// All the ways [`parse_manifest`] and [`manifest_to_map_plan`]
/// can fail.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// JSON syntax / shape error from `serde_json`.
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// `manifest.version` is something other than
    /// [`SUPPORTED_MANIFEST_VERSION`].
    #[error("unsupported manifest version: {0}")]
    UnsupportedVersion(String),
    /// `mapping.preset` is not one of `"lorom"`, `"hirom"`, `"exhirom"`,
    /// `"sdd1"`, `"spc7110"`, `"sa1"`, `"superfx"`, `"cx4"` or `"custom"`.
    #[error("unsupported preset: {0}")]
    UnknownPreset(String),
    /// `slot.speed` is not one of `"Fast" | "Slow" | "XSlow"`.
    #[error("unsupported speed: {0}")]
    UnknownSpeed(String),
    /// A hex string field couldn't be parsed. `field` is a
    /// dotted path (`"slots[0].bank_range[0]"`), `value` is the
    /// offending input.
    #[error("invalid hex in field '{field}': {value}")]
    InvalidHex { field: String, value: String },
}

/// Parse a JSON string into a typed [`CartridgeManifest`]. Returns
/// [`ManifestError::Json`] on syntax / shape errors, or
/// [`ManifestError::UnsupportedVersion`] if the `version` field
/// disagrees with [`SUPPORTED_MANIFEST_VERSION`].
pub fn parse_manifest(json: &str) -> Result<CartridgeManifest, ManifestError> {
    let manifest: CartridgeManifest = serde_json::from_str(json)?;
    if manifest.version != SUPPORTED_MANIFEST_VERSION {
        return Err(ManifestError::UnsupportedVersion(manifest.version));
    }
    Ok(manifest)
}

/// Turn a parsed manifest into a [`MapPlan`]. The plan is built in
/// this order:
///
/// 1. If `mapping.preset` is one of `"lorom"` / `"hirom"` /
///    `"exhirom"` / `"sdd1"` / `"spc7110"` / `"sa1"` / `"superfx"` /
///    `"cx4"`, seed the plan with that preset's slots.
/// 2. Append each `mapping.slots[i]` as a fresh [`MapSlot`].
/// 3. Append each `chips[i].mapping` (with the slot's
///    [`ManifestTarget`] forced to [`MapTarget::ChipMmio`] using
///    the chip's `id` if the target wasn't already `ChipMmio`).
///
/// The first-wins resolution policy in [`MapPlan::resolve`] means
/// preset slots beat manifest slots on overlap. Use
/// `preset: "custom"` to start with an empty plan.
pub fn manifest_to_map_plan(
    manifest: &CartridgeManifest,
    rom: &[u8],
) -> Result<MapPlan, ManifestError> {
    let preset_kind = parse_preset(&manifest.mapping.preset)?;
    let mut plan = match preset_kind {
        PresetKind::LoRom => {
            lorom_preset(rom, manifest.mapping.sram_size, manifest.mapping.fastrom)
        }
        PresetKind::HiRom => {
            hirom_preset(rom, manifest.mapping.sram_size, manifest.mapping.fastrom)
        }
        PresetKind::ExHiRom => {
            exhirom_preset(rom, manifest.mapping.sram_size, manifest.mapping.fastrom)
        }
        PresetKind::Sdd1 => sdd1_preset(rom, manifest.mapping.sram_size, manifest.mapping.fastrom),
        PresetKind::Spc7110 => {
            // The manifest lacks the header's unpadded length and RTC flag, so
            // derive the wrap size from `rom.size` and map the RTC ports
            // unconditionally (the chip decides whether they are live). The
            // builder's own path threads the real header values instead.
            spc7110_preset(
                rom,
                manifest.rom.size as usize,
                manifest.mapping.sram_size,
                manifest.mapping.fastrom,
                true,
            )
        }
        PresetKind::Sa1 => sa1_preset(rom, manifest.mapping.sram_size, manifest.mapping.fastrom),
        PresetKind::SuperFx => superfx_preset(rom, manifest.mapping.sram_size),
        PresetKind::Cx4 => cx4_preset(rom, manifest.mapping.sram_size),
        PresetKind::Custom => MapPlan::new("custom"),
    };

    append_manifest_extras(&mut plan, manifest)?;

    Ok(plan)
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

/// Append the manifest's custom slots and chip mappings to an
/// already-built plan. Used by [`manifest_to_map_plan`] and by the
/// cartridge builder when a `force_preset` overrides the manifest's
/// preset string but the manifest still contributes extras.
///
/// The slots and chips append in source order; the bus's first-wins
/// resolver means preset slots beat manifest slots on overlap.
pub(crate) fn append_manifest_extras(
    plan: &mut MapPlan,
    manifest: &CartridgeManifest,
) -> Result<(), ManifestError> {
    for (i, slot) in manifest.mapping.slots.iter().enumerate() {
        let map_slot = manifest_slot_to_map_slot(slot, &format!("slots[{}]", i), None)?;
        plan.push(map_slot);
    }

    for (i, chip) in manifest.chips.iter().enumerate() {
        let path = format!("chips[{}].mapping", i);
        let map_slot = manifest_slot_to_map_slot(&chip.mapping, &path, Some(chip.id))?;
        plan.push(map_slot);
    }

    Ok(())
}

fn parse_speed(s: &str) -> Result<BusSpeed, ManifestError> {
    match s {
        "Fast" => Ok(BusSpeed::Fast),
        "Slow" => Ok(BusSpeed::Slow),
        "XSlow" => Ok(BusSpeed::XSlow),
        other => Err(ManifestError::UnknownSpeed(other.to_string())),
    }
}

fn manifest_slot_to_map_slot(
    slot: &ManifestSlot,
    path: &str,
    chip_id_override: Option<u8>,
) -> Result<MapSlot, ManifestError> {
    let bank_lo = parse_hex_u8(&slot.bank_range[0], &format!("{}.bank_range[0]", path))?;
    let bank_hi = parse_hex_u8(&slot.bank_range[1], &format!("{}.bank_range[1]", path))?;
    let off_lo = parse_hex_u16(&slot.offset_range[0], &format!("{}.offset_range[0]", path))?;
    let off_hi = parse_hex_u16(&slot.offset_range[1], &format!("{}.offset_range[1]", path))?;

    let mut target = manifest_target_to_map_target(&slot.target);
    if let Some(chip_id) = chip_id_override {
        // Chips published in the `chips[]` array always dispatch
        // through ChipMmio regardless of what the inline target
        // looked like. The override keeps the manifest concise,
        // a chip block doesn't need to repeat its own id on the
        // inner slot.
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

    let speed = parse_speed(&slot.speed)?;
    let mut map_slot = MapSlot::new(bank_lo..=bank_hi, off_lo..=off_hi, target, speed);
    if let Some(mask_str) = slot.mask.as_deref() {
        let mask = parse_hex_u32(mask_str, &format!("{}.mask", path))?;
        map_slot = map_slot.with_mask(mask);
    }
    if let Some(stride_str) = slot.bank_stride.as_deref() {
        let stride = parse_hex_u32(stride_str, &format!("{}.bank_stride", path))?;
        map_slot = map_slot.with_bank_stride(stride);
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

fn parse_hex_u8(s: &str, field: &str) -> Result<u8, ManifestError> {
    let v = parse_hex_inner(s).ok_or_else(|| ManifestError::InvalidHex {
        field: field.to_string(),
        value: s.to_string(),
    })?;
    u8::try_from(v).map_err(|_| ManifestError::InvalidHex {
        field: field.to_string(),
        value: s.to_string(),
    })
}

fn parse_hex_u16(s: &str, field: &str) -> Result<u16, ManifestError> {
    let v = parse_hex_inner(s).ok_or_else(|| ManifestError::InvalidHex {
        field: field.to_string(),
        value: s.to_string(),
    })?;
    u16::try_from(v).map_err(|_| ManifestError::InvalidHex {
        field: field.to_string(),
        value: s.to_string(),
    })
}

fn parse_hex_u32(s: &str, field: &str) -> Result<u32, ManifestError> {
    parse_hex_inner(s).ok_or_else(|| ManifestError::InvalidHex {
        field: field.to_string(),
        value: s.to_string(),
    })
}

/// Parse an inclusive 24-bit address range like `"0x4300-0x437F"` (or
/// `"4300-437F"`) into a [`RangeInclusive<u32>`], for the per-chip
/// `observes[]` bus ranges. Fails with [`ManifestError::InvalidHex`] on a
/// malformed bound, a missing `-` separator, or an inverted range
/// (`lo > hi`). Whitespace around each bound is tolerated.
pub(crate) fn parse_addr_range(s: &str, field: &str) -> Result<RangeInclusive<u32>, ManifestError> {
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

/// Strip an optional `0x` / `0X` prefix and parse the rest as
/// hex into a `u32`. Returns `None` on any malformed input
/// (non-hex characters, empty string, or > 8 hex digits).
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
    fn parse_error_unsupported_version() {
        let json = r#"{
            "core": "snes1",
            "version": "9.9",
            "rom": { "sha256": "x", "size": 1 },
            "mapping": { "preset": "lorom" }
        }"#;
        let err = parse_manifest(json).unwrap_err();
        assert!(matches!(err, ManifestError::UnsupportedVersion(ref s) if s == "9.9"));
    }

    #[test]
    fn parse_error_invalid_json() {
        let err = parse_manifest("{ this is not json }").unwrap_err();
        assert!(matches!(err, ManifestError::Json(_)));
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
    fn parse_addr_range_valid() {
        assert_eq!(
            parse_addr_range("0x4300-0x437F", "observes[0]").unwrap(),
            0x4300..=0x437F
        );
        // Bare hex (no 0x) and surrounding whitespace are tolerated.
        assert_eq!(
            parse_addr_range(" 4300 - 437f ", "observes[0]").unwrap(),
            0x4300..=0x437F
        );
        // A single-address range (lo == hi) is legal.
        assert_eq!(
            parse_addr_range("0x4801-0x4801", "observes[0]").unwrap(),
            0x4801..=0x4801
        );
    }

    #[test]
    fn parse_addr_range_rejects_malformed() {
        // Missing separator, non-hex bound, and inverted range all fail
        // with InvalidHex carrying the field path + offending value.
        for bad in ["0x4300", "0x4300-nope", "0x4400-0x4300"] {
            match parse_addr_range(bad, "observes[2]") {
                Err(ManifestError::InvalidHex { field, value }) => {
                    assert_eq!(field, "observes[2]");
                    assert_eq!(value, bad);
                }
                other => panic!("expected InvalidHex for {bad:?}, got {other:?}"),
            }
        }
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
}
