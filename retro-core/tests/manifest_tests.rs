//! The `cartridge-manifest-v1` format's rules, which every console's
//! manifest keeps: the key order a console's knobs and extras take, the
//! round trip, unknown keys, the version and the console's own check, the
//! errors and the paths they name, the hex and range parsers, a chip's
//! forced target, and the plan's order. Two test consoles: one with keys of
//! its own, one with none.

use retro_core::cartridge::manifest::{
    append_manifest_extras, chip_target, manifest_to_map_plan, map_target, parse_addr_range,
    parse_hex_u16, parse_hex_u32, parse_hex_u8, parse_manifest, CartridgeManifest, ManifestConsole,
    ManifestControllers, ManifestError, ManifestTarget, NoFields, MANIFEST_SCHEMA,
    SUPPORTED_MANIFEST_VERSION,
};
use retro_core::cartridge::{Board, Coprocessor, MapPlan, MapTarget, SlotShape};
use serde::{Deserialize, Serialize};

/// A linear slot.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Slot {
    start: u32,
    end: u32,
    mask: Option<u32>,
    target: MapTarget,
}

impl SlotShape for Slot {
    type Target = MapTarget;
    fn contains(&self, addr: u32) -> bool {
        (self.start..=self.end).contains(&addr)
    }
    fn linear_offset(&self, addr: u32) -> u32 {
        addr - self.start
    }
    fn mask(&self) -> Option<u32> {
        self.mask
    }
    fn target(&self) -> MapTarget {
        self.target
    }
    fn bounds(&self) -> (u32, u32) {
        (self.start, self.end)
    }
    fn size(&self) -> usize {
        (self.end - self.start + 1) as usize
    }
}

/// A slot as the test consoles' files write it; `bus: "second"` names a
/// slot of another bus, which the main plan leaves out.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct FileSlot {
    range: [String; 2],
    target: ManifestTarget,
    #[serde(default)]
    mask: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bus: Option<String>,
}

fn map_file_slot(
    slot: &FileSlot,
    path: &str,
    chip: Option<u8>,
) -> Result<Option<Slot>, ManifestError> {
    if slot.bus.as_deref() == Some("second") {
        return Ok(None);
    }
    let start = parse_hex_u32(&slot.range[0], &format!("{path}.range[0]"))?;
    let end = parse_hex_u32(&slot.range[1], &format!("{path}.range[1]"))?;
    let target = match chip {
        Some(id) => chip_target(&slot.target, id),
        None => map_target(&slot.target),
    };
    let mask = match slot.mask.as_deref() {
        Some(mask) => Some(parse_hex_u32(mask, &format!("{path}.mask"))?),
        None => None,
    };
    Ok(Some(Slot {
        start,
        end,
        mask,
        target,
    }))
}

fn preset(name: &str, rom: &[u8]) -> Result<MapPlan<Slot>, ManifestError> {
    match name {
        "flat" => {
            let mut plan = MapPlan::new("flat");
            plan.push(Slot {
                start: 0,
                end: rom.len().max(1) as u32 - 1,
                mask: None,
                target: MapTarget::Rom { base_offset: 0 },
            });
            Ok(plan)
        }
        "custom" => Ok(MapPlan::new("custom")),
        other => Err(ManifestError::UnknownPreset(other.to_string())),
    }
}

/// A console with a key in `mapping` and one at the top.
struct Keyed;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Knobs {
    #[serde(default)]
    fast: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Extras {
    #[serde(default)]
    controllers: Option<ManifestControllers>,
}

impl Board for Keyed {
    type Slot = Slot;
    type Chip = dyn Coprocessor;
    type ChipState = ();
    const MISSING: Option<u8> = Some(0);
}

impl ManifestConsole for Keyed {
    type MappingKnobs = Knobs;
    type Extras = Extras;
    type ManifestSlot = FileSlot;
    fn map_slot(
        slot: &FileSlot,
        path: &str,
        chip: Option<u8>,
    ) -> Result<Option<Slot>, ManifestError> {
        map_file_slot(slot, path, chip)
    }
    fn preset_plan(
        manifest: &CartridgeManifest<Self>,
        rom: &[u8],
    ) -> Result<MapPlan<Slot>, ManifestError> {
        preset(&manifest.mapping.preset, rom)
    }
    fn validate(manifest: &CartridgeManifest<Self>) -> Result<(), ManifestError> {
        if manifest.mapping.sram_size > 0x100 {
            return Err(ManifestError::SramTooLarge {
                size: manifest.mapping.sram_size,
                limit: 0x100,
            });
        }
        Ok(())
    }
}

/// A console with no key of its own.
struct Bare;

impl Board for Bare {
    type Slot = Slot;
    type Chip = dyn Coprocessor;
    type ChipState = ();
    const MISSING: Option<u8> = Some(0);
}

impl ManifestConsole for Bare {
    type MappingKnobs = NoFields;
    type Extras = NoFields;
    type ManifestSlot = FileSlot;
    fn map_slot(
        slot: &FileSlot,
        path: &str,
        chip: Option<u8>,
    ) -> Result<Option<Slot>, ManifestError> {
        map_file_slot(slot, path, chip)
    }
    fn preset_plan(
        manifest: &CartridgeManifest<Self>,
        rom: &[u8],
    ) -> Result<MapPlan<Slot>, ManifestError> {
        preset(&manifest.mapping.preset, rom)
    }
}

const EVERY_KEY: &str = r#"{"$schema":"cartridge-manifest-v1","core":"test","version":"1.0","rom":{"sha256":"ab","size":16,"title":"T","region":"R"},"mapping":{"preset":"flat","fast":true,"sram_size":32,"slots":[{"range":["0x20","0x2F"],"target":{"kind":"Sram","base_offset":0},"mask":"0xF"},{"range":["0x30","0x3F"],"target":{"kind":"OpenBus"},"mask":null,"bus":"second"}]},"chips":[{"id":3,"kind":"chip","mapping":{"range":["0x40","0x4F"],"target":{"kind":"Rom","base_offset":8},"mask":null},"observes":["0x10-0x1F"]}],"controllers":{"port1":"pad","port2":null},"metadata":{"a":1}}"#;

#[test]
fn a_consoles_keys_take_their_place_and_the_json_comes_back_byte_for_byte() {
    let manifest = parse_manifest::<Keyed>(EVERY_KEY).unwrap();
    assert!(manifest.mapping.knobs.fast);
    assert_eq!(
        manifest
            .console
            .controllers
            .as_ref()
            .unwrap()
            .port1
            .as_deref(),
        Some("pad")
    );
    assert_eq!(serde_json::to_string(&manifest).unwrap(), EVERY_KEY);
    let pretty = serde_json::to_string_pretty(&manifest).unwrap();
    let again = parse_manifest::<Keyed>(&pretty).unwrap();
    assert_eq!(serde_json::to_string(&again).unwrap(), EVERY_KEY);
    let cloned = manifest.clone();
    assert_eq!(serde_json::to_string(&cloned).unwrap(), EVERY_KEY);
    assert!(format!("{manifest:?}").contains("fast: true"));
}

#[test]
fn a_console_with_no_keys_writes_none_and_unknown_keys_are_dropped() {
    let json = r#"{"core":"test","version":"1.0","rom":{"sha256":"","size":4},"mapping":{"preset":"flat","fast":true,"other":[1]},"controllers":{"port1":"pad"},"x":1}"#;
    let manifest = parse_manifest::<Bare>(json).unwrap();
    assert_eq!(
        serde_json::to_string(&manifest).unwrap(),
        r#"{"$schema":null,"core":"test","version":"1.0","rom":{"sha256":"","size":4,"title":null,"region":null},"mapping":{"preset":"flat","sram_size":0,"slots":[]},"chips":[],"metadata":null}"#
    );
    let keyed = parse_manifest::<Keyed>(r#"{"core":"test","version":"1.0","rom":{"sha256":"","size":4},"mapping":{"preset":"flat"}}"#).unwrap();
    assert!(
        !keyed.mapping.knobs.fast,
        "a knob left out takes its default"
    );
    assert!(keyed.console.controllers.is_none());
    assert_eq!(MANIFEST_SCHEMA, "cartridge-manifest-v1");
    assert_eq!(SUPPORTED_MANIFEST_VERSION, "1.0");
}

#[test]
fn the_version_and_the_consoles_check_refuse_and_each_error_reads_as_it_did() {
    let other_version = r#"{"core":"test","version":"1.1","rom":{"sha256":"","size":4},"mapping":{"preset":"flat"}}"#;
    assert_eq!(
        parse_manifest::<Keyed>(other_version)
            .unwrap_err()
            .to_string(),
        "unsupported manifest version: 1.1"
    );
    let too_much_ram = r#"{"core":"test","version":"1.0","rom":{"sha256":"","size":4},"mapping":{"preset":"flat","sram_size":257}}"#;
    assert_eq!(
        parse_manifest::<Keyed>(too_much_ram)
            .unwrap_err()
            .to_string(),
        "sram_size 257 is larger than the 256-byte limit"
    );
    assert!(
        parse_manifest::<Bare>(too_much_ram).is_ok(),
        "only the console that checks refuses"
    );
    assert_eq!(
        parse_manifest::<Bare>("{").unwrap_err().to_string(),
        "invalid JSON: EOF while parsing an object at line 1 column 1"
    );
    assert_eq!(
        parse_manifest::<Bare>("[]").unwrap_err().to_string(),
        "invalid JSON: invalid type: sequence, expected struct CartridgeManifest at line 1 column 0",
        "a manifest is an object"
    );
    assert_eq!(
        ManifestError::UnknownSpeed("Warp".into()).to_string(),
        "unsupported speed: Warp"
    );
    let unknown = r#"{"core":"test","version":"1.0","rom":{"sha256":"","size":4},"mapping":{"preset":"odd"}}"#;
    let manifest = parse_manifest::<Bare>(unknown).unwrap();
    assert_eq!(
        manifest_to_map_plan(&manifest, &[0; 4])
            .unwrap_err()
            .to_string(),
        "unsupported preset: odd"
    );
}

#[test]
fn the_plan_is_the_preset_then_the_slots_then_the_chips_and_a_chip_dispatches_to_itself() {
    let manifest = parse_manifest::<Keyed>(EVERY_KEY).unwrap();
    let plan = manifest_to_map_plan(&manifest, &[0; 16]).unwrap();
    assert_eq!(plan.name, "flat");
    assert_eq!(plan.slots.len(), 3, "the second bus's slot is left out");
    assert_eq!(plan.slots[0].target, MapTarget::Rom { base_offset: 0 });
    assert_eq!(plan.slots[1].target, MapTarget::Sram { base_offset: 0 });
    assert_eq!(plan.slots[1].mask, Some(0xF));
    assert_eq!(
        plan.slots[2].target,
        MapTarget::ChipMmio {
            chip_id: 3,
            base: 8
        },
        "a chip's slot names the chip, at the base its target writes"
    );

    let mut plan = MapPlan::new("custom");
    append_manifest_extras(&mut plan, &manifest).unwrap();
    assert_eq!(plan.slots.len(), 2);
}

#[test]
fn a_bad_hex_names_the_field_it_is_in() {
    let bad_slot = r#"{"core":"test","version":"1.0","rom":{"sha256":"","size":4},"mapping":{"preset":"custom","slots":[{"range":["0x0","0xZZ"],"target":{"kind":"Rom"}}]}}"#;
    let manifest = parse_manifest::<Bare>(bad_slot).unwrap();
    assert_eq!(
        manifest_to_map_plan(&manifest, &[])
            .unwrap_err()
            .to_string(),
        "invalid hex in field 'slots[0].range[1]': 0xZZ"
    );
    let bad_chip = r#"{"core":"test","version":"1.0","rom":{"sha256":"","size":4},"mapping":{"preset":"custom"},"chips":[{"id":1,"kind":"x","mapping":{"range":["0x0","0x1"],"target":{"kind":"OpenBus"},"mask":"q"}}]}"#;
    let manifest = parse_manifest::<Bare>(bad_chip).unwrap();
    assert_eq!(
        manifest_to_map_plan(&manifest, &[])
            .unwrap_err()
            .to_string(),
        "invalid hex in field 'chips[0].mapping.mask': q"
    );
}

#[test]
fn the_hex_parsers_take_either_form_and_refuse_what_does_not_fit() {
    assert_eq!(parse_hex_u32("0x1F", "f").unwrap(), 0x1F);
    assert_eq!(parse_hex_u32("0X1F", "f").unwrap(), 0x1F);
    assert_eq!(parse_hex_u32("1f", "f").unwrap(), 0x1F);
    assert_eq!(parse_hex_u32("FFFFFFFF", "f").unwrap(), u32::MAX);
    for bad in ["", "0x", "123456789", "0xG", "-1"] {
        assert_eq!(
            parse_hex_u32(bad, "f").unwrap_err().to_string(),
            format!("invalid hex in field 'f': {bad}")
        );
    }
    assert_eq!(parse_hex_u8("0xFF", "b").unwrap(), 0xFF);
    assert_eq!(
        parse_hex_u8("0x100", "b").unwrap_err().to_string(),
        "invalid hex in field 'b': 0x100"
    );
    assert_eq!(parse_hex_u16("FFFF", "h").unwrap(), 0xFFFF);
    assert!(parse_hex_u16("10000", "h").is_err());
}

#[test]
fn a_range_is_two_bounds_low_first() {
    assert_eq!(
        parse_addr_range("0x4300-0x437F", "o").unwrap(),
        0x4300..=0x437F
    );
    assert_eq!(parse_addr_range(" 10 - 10 ", "o").unwrap(), 0x10..=0x10);
    for bad in ["0x4300", "0x10-0x0F", "x-1", "1-"] {
        assert_eq!(
            parse_addr_range(bad, "o").unwrap_err().to_string(),
            format!("invalid hex in field 'o': {bad}")
        );
    }
}

#[test]
fn a_file_target_maps_as_written_and_a_chip_target_keeps_the_written_base() {
    assert_eq!(
        map_target(&ManifestTarget::ChipMmio {
            chip_id: 2,
            base: 5
        }),
        MapTarget::ChipMmio {
            chip_id: 2,
            base: 5
        }
    );
    assert_eq!(map_target(&ManifestTarget::OpenBus), MapTarget::OpenBus);
    assert_eq!(
        chip_target(
            &ManifestTarget::ChipMmio {
                chip_id: 9,
                base: 5
            },
            1
        ),
        MapTarget::ChipMmio {
            chip_id: 1,
            base: 5
        }
    );
    assert_eq!(
        chip_target(&ManifestTarget::Sram { base_offset: 7 }, 1),
        MapTarget::ChipMmio {
            chip_id: 1,
            base: 7
        }
    );
    assert_eq!(
        chip_target(&ManifestTarget::OpenBus, 1),
        MapTarget::ChipMmio {
            chip_id: 1,
            base: 0
        }
    );
}
