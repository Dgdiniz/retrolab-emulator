//! The `cartridge-manifest-v1` format: a cartridge described by hand, as
//! JSON, instead of by header detection.
//!
//! The format is one for every console. A manifest names a `preset` (a
//! console's stock map plan, or `"custom"` for an empty one), a battery RAM
//! size, extra `slots` written in the console's own slot shape, and `chips`,
//! each with its own slot (forced to dispatch to the chip) and the bus
//! ranges it observes. A console adds keys of its own through
//! [`ManifestConsole`]: keys inside `mapping` ([`ManifestConsole::MappingKnobs`],
//! written between `preset` and `sram_size`) and keys at the top
//! ([`ManifestConsole::Extras`], written between `chips` and `metadata`).
//! Both are flattened, so a console's manifest reads and writes the same
//! JSON it did before the format was shared, key for key. Unknown keys are
//! ignored, so a manifest a host stored keeps parsing.
//!
//! Hex addresses travel as strings (`"0x8000"`) so a manifest stays
//! reviewable; the parsers here read `"0x..."` and bare hex alike and name
//! a malformed field by its path (`slots[0].addr_range[1]`).

use std::fmt::{self, Debug};
use std::ops::RangeInclusive;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::composable::Board;
use super::map_plan::MapPlan;
use super::MapTarget;

/// The format's name, a manifest's `$schema`.
pub const MANIFEST_SCHEMA: &str = "cartridge-manifest-v1";

/// The format version this module reads; any other is refused.
pub const SUPPORTED_MANIFEST_VERSION: &str = "1.0";

/// What a console adds to the format: its keys, its slot as the file writes
/// it, and its presets.
pub trait ManifestConsole: Board {
    /// Keys added to `mapping`, written between `preset` and `sram_size`.
    type MappingKnobs: Serialize + DeserializeOwned + Clone + Debug + Default;
    /// Keys added at the top, written between `chips` and `metadata`.
    type Extras: Serialize + DeserializeOwned + Clone + Debug + Default;
    /// A slot as the file writes it.
    type ManifestSlot: Serialize + DeserializeOwned + Clone + Debug;

    /// The map slot a file slot describes, `path` naming it in an error.
    /// With `chip`, the slot dispatches to that chip whatever target it
    /// writes ([`chip_target`]). `None` for a slot of another of the
    /// console's buses, which the plan of this one leaves out.
    fn map_slot(
        slot: &Self::ManifestSlot,
        path: &str,
        chip: Option<u8>,
    ) -> Result<Option<Self::Slot>, ManifestError>;

    /// The plan the manifest's `preset` seeds, before its own slots and its
    /// chips' slots are appended.
    fn preset_plan(
        manifest: &CartridgeManifest<Self>,
        rom: &[u8],
    ) -> Result<MapPlan<Self::Slot>, ManifestError>
    where
        Self: Sized;

    /// What the console refuses in a manifest beyond the format's own
    /// checks. The default refuses nothing.
    fn validate(manifest: &CartridgeManifest<Self>) -> Result<(), ManifestError>
    where
        Self: Sized,
    {
        let _ = manifest;
        Ok(())
    }
}

/// A manifest, mirroring the JSON key for key.
#[derive(Serialize, Deserialize)]
#[serde(bound(serialize = "", deserialize = ""))]
pub struct CartridgeManifest<C: ManifestConsole> {
    #[serde(rename = "$schema", default)]
    pub schema: Option<String>,
    pub core: String,
    pub version: String,
    pub rom: ManifestRom,
    pub mapping: ManifestMapping<C>,
    #[serde(default)]
    pub chips: Vec<ManifestChip<C::ManifestSlot>>,
    #[serde(flatten)]
    pub console: C::Extras,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

impl<C: ManifestConsole> Clone for CartridgeManifest<C> {
    fn clone(&self) -> Self {
        Self {
            schema: self.schema.clone(),
            core: self.core.clone(),
            version: self.version.clone(),
            rom: self.rom.clone(),
            mapping: self.mapping.clone(),
            chips: self.chips.clone(),
            console: self.console.clone(),
            metadata: self.metadata.clone(),
        }
    }
}

impl<C: ManifestConsole> Debug for CartridgeManifest<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CartridgeManifest")
            .field("schema", &self.schema)
            .field("core", &self.core)
            .field("version", &self.version)
            .field("rom", &self.rom)
            .field("mapping", &self.mapping)
            .field("chips", &self.chips)
            .field("console", &self.console)
            .field("metadata", &self.metadata)
            .finish()
    }
}

/// The image a manifest describes: its digest, its size and its labels.
/// The digest is not checked here; a console's builder checks it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestRom {
    pub sha256: String,
    pub size: u64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
}

/// The mapping: the preset, the console's knobs, the battery RAM size and
/// the extra slots.
#[derive(Serialize, Deserialize)]
#[serde(bound(serialize = "", deserialize = ""))]
pub struct ManifestMapping<C: ManifestConsole> {
    pub preset: String,
    #[serde(flatten)]
    pub knobs: C::MappingKnobs,
    #[serde(default)]
    pub sram_size: usize,
    #[serde(default)]
    pub slots: Vec<C::ManifestSlot>,
}

impl<C: ManifestConsole> Clone for ManifestMapping<C> {
    fn clone(&self) -> Self {
        Self {
            preset: self.preset.clone(),
            knobs: self.knobs.clone(),
            sram_size: self.sram_size,
            slots: self.slots.clone(),
        }
    }
}

impl<C: ManifestConsole> Debug for ManifestMapping<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManifestMapping")
            .field("preset", &self.preset)
            .field("knobs", &self.knobs)
            .field("sram_size", &self.sram_size)
            .field("slots", &self.slots)
            .finish()
    }
}

/// What lives behind a slot, tagged by `kind` so the JSON reads
/// `{ "kind": "Rom", "base_offset": 0 }`.
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

/// A chip a manifest declares: its id, the factory's name for it, its slot
/// in the console's shape, and the bus ranges it observes (`"lo-hi"` hex).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestChip<S> {
    pub id: u8,
    pub kind: String,
    pub mapping: S,
    #[serde(default)]
    pub observes: Vec<String>,
}

/// The controllers a manifest names for the ports, carried as written.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestControllers {
    #[serde(default)]
    pub port1: Option<String>,
    #[serde(default)]
    pub port2: Option<String>,
}

/// The knobs or extras of a console that adds no key of its own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoFields {}

/// Everything parsing or applying a manifest can refuse.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// The JSON's syntax, or a key of the wrong shape.
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// A `version` other than [`SUPPORTED_MANIFEST_VERSION`].
    #[error("unsupported manifest version: {0}")]
    UnsupportedVersion(String),
    /// A `preset` the console does not have.
    #[error("unsupported preset: {0}")]
    UnknownPreset(String),
    /// A slot's access speed the console does not have.
    #[error("unsupported speed: {0}")]
    UnknownSpeed(String),
    /// A hex string that does not parse, `field` its path.
    #[error("invalid hex in field '{field}': {value}")]
    InvalidHex { field: String, value: String },
    /// A battery RAM over the console's limit.
    #[error("sram_size {size} is larger than the {limit}-byte limit")]
    SramTooLarge { size: usize, limit: u32 },
}

/// Parse a manifest: the JSON, then its version, then the console's own
/// checks.
pub fn parse_manifest<C: ManifestConsole>(
    json: &str,
) -> Result<CartridgeManifest<C>, ManifestError> {
    let manifest: CartridgeManifest<C> = serde_json::from_str(json)?;
    if manifest.version != SUPPORTED_MANIFEST_VERSION {
        return Err(ManifestError::UnsupportedVersion(manifest.version));
    }
    C::validate(&manifest)?;
    Ok(manifest)
}

/// The plan a manifest gives for `rom`: the preset's slots, then the
/// manifest's own slots, then its chips' slots. The first slot that covers
/// an address wins, so a preset slot beats a manifest slot that overlaps
/// it; `"custom"` starts from an empty plan.
pub fn manifest_to_map_plan<C: ManifestConsole>(
    manifest: &CartridgeManifest<C>,
    rom: &[u8],
) -> Result<MapPlan<C::Slot>, ManifestError> {
    let mut plan = C::preset_plan(manifest, rom)?;
    append_manifest_extras(&mut plan, manifest)?;
    Ok(plan)
}

/// Append a manifest's own slots and its chips' slots to `plan`, in the
/// order the file writes them.
pub fn append_manifest_extras<C: ManifestConsole>(
    plan: &mut MapPlan<C::Slot>,
    manifest: &CartridgeManifest<C>,
) -> Result<(), ManifestError> {
    for (i, slot) in manifest.mapping.slots.iter().enumerate() {
        if let Some(slot) = C::map_slot(slot, &format!("slots[{i}]"), None)? {
            plan.push(slot);
        }
    }
    for (i, chip) in manifest.chips.iter().enumerate() {
        let path = format!("chips[{i}].mapping");
        if let Some(slot) = C::map_slot(&chip.mapping, &path, Some(chip.id))? {
            plan.push(slot);
        }
    }
    Ok(())
}

/// The map target a file target names.
pub fn map_target(target: &ManifestTarget) -> MapTarget {
    match *target {
        ManifestTarget::Rom { base_offset } => MapTarget::Rom { base_offset },
        ManifestTarget::Sram { base_offset } => MapTarget::Sram { base_offset },
        ManifestTarget::ChipMmio { chip_id, base } => MapTarget::ChipMmio { chip_id, base },
        ManifestTarget::OpenBus => MapTarget::OpenBus,
    }
}

/// The target of a chip's slot: always the chip's window, at the base the
/// file's target names (a chip window's `base`, a ROM or RAM target's
/// `base_offset`, 0 for open bus), so a chip's declaration need not repeat
/// its own id.
pub fn chip_target(target: &ManifestTarget, chip_id: u8) -> MapTarget {
    let base = match *target {
        ManifestTarget::ChipMmio { base, .. } => base,
        ManifestTarget::Rom { base_offset } | ManifestTarget::Sram { base_offset } => base_offset,
        ManifestTarget::OpenBus => 0,
    };
    MapTarget::ChipMmio { chip_id, base }
}

/// A hex byte (`"0x80"` or `"80"`), `field` naming it in an error.
pub fn parse_hex_u8(s: &str, field: &str) -> Result<u8, ManifestError> {
    u8::try_from(parse_hex_u32(s, field)?).map_err(|_| invalid_hex(s, field))
}

/// A hex half-word, `field` naming it in an error.
pub fn parse_hex_u16(s: &str, field: &str) -> Result<u16, ManifestError> {
    u16::try_from(parse_hex_u32(s, field)?).map_err(|_| invalid_hex(s, field))
}

/// A hex word of at most eight digits, `field` naming it in an error.
pub fn parse_hex_u32(s: &str, field: &str) -> Result<u32, ManifestError> {
    parse_hex(s).ok_or_else(|| invalid_hex(s, field))
}

/// An inclusive address range written `"lo-hi"` (`"0x4300-0x437F"`), as a
/// chip's `observes` entries are; refused when a bound does not parse, the
/// `-` is missing, or `lo > hi`. Space around a bound is allowed.
pub fn parse_addr_range(s: &str, field: &str) -> Result<RangeInclusive<u32>, ManifestError> {
    let bounds = s
        .split_once('-')
        .and_then(|(lo, hi)| Some((parse_hex(lo.trim())?, parse_hex(hi.trim())?)));
    match bounds {
        Some((lo, hi)) if lo <= hi => Ok(lo..=hi),
        _ => Err(invalid_hex(s, field)),
    }
}

/// The hex number `s` holds, with or without `0x` / `0X`, of one to eight
/// digits.
fn parse_hex(s: &str) -> Option<u32> {
    let digits = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if digits.is_empty() || digits.len() > 8 {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}

fn invalid_hex(s: &str, field: &str) -> ManifestError {
    ManifestError::InvalidHex {
        field: field.to_string(),
        value: s.to_string(),
    }
}
