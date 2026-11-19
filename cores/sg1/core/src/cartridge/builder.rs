//! Cartridge builder: ROM bytes (+ optional manifest) -> a boxed
//! [`GenesisCartridge`].
//!
//! Two entry points mirror the SNES builder:
//! - [`build_genesis_cartridge`] runs heuristic detection (header hints +
//!   ROM size), then assembles the matching cartridge.
//! - [`build_with_manifest`] takes a parsed [`CartridgeManifest`] and builds
//!   exactly what it describes, resolving chips through [`Sg1ChipFactory`].
//!
//! Both put the cartridge together the same way (`assemble`), so a
//! `plain` cartridge built by detection and one built from a
//! `"preset": "plain"` manifest are identical.
//!
//! ## Detection ordering
//!
//! The SVP check runs **before** the mapper match, because the SVP carts
//! (Virtua Racing) are <= 4 MiB and would otherwise fall through to the plain
//! path and never build their coprocessor.

use std::ops::RangeInclusive;
use std::sync::Arc;

use retro_core::cartridge::detection::{HeaderInfo, HeaderParser};
use retro_core::cartridge::manifest::{NoFields, MANIFEST_SCHEMA};
use retro_core::cartridge::{
    install_chips, rom_sha256, CartridgeInfo, ChipDecl, ChipSet, ConsoleSystem, DetectionSource,
    ExpansionChipInfo, ExpansionChipType, MapperId, MapperInfo, Region,
};

use crate::cartridge::detect::{calculate_genesis_checksum, detect_genesis_mapper};
use crate::cartridge::header::{GenesisHeaderParser, SRAM_SIZE_LIMIT};

use super::board::{GenesisBoard, GenesisCartridge};
use super::factory::{Sg1ChipFactory, DEFAULT_EEPROM_SIZE};
use super::manifest::{
    manifest_sram_window, manifest_to_map_plan, parse_preset, CartridgeManifest, ManifestMapping,
    ManifestRom, SUPPORTED_MANIFEST_VERSION,
};
use super::map_plan::MapPlan;
use super::presets::{self, PresetKind};
use crate::savestate::BankMapperKind;

/// Genesis NTSC master clock. The composable chips are re-anchored to the
/// real region clock by `reset_chip` once the system is up; this is only the
/// build-time default the SVP's constructor stores.
pub const DEFAULT_MASTER_CLOCK_HZ: u64 = 53_693_175;

/// The `0xA130Fx` bank-register window a SEGA / SSF2 mapper observes.
const SEGA_BANK_OBSERVE: RangeInclusive<u32> = 0xA130F0..=0xA130FF;

/// Detect the cartridge kind from the ROM and assemble the matching
/// cartridge.
pub fn build_genesis_cartridge(rom_data: Vec<u8>) -> Result<Box<GenesisCartridge>, String> {
    if rom_data.len() < 0x200 {
        return Err("ROM too small (must be at least 512 bytes)".to_string());
    }

    let header_info = GenesisHeaderParser::new().parse(&rom_data)?;

    // SVP before the mapper match (see "Detection ordering" above).
    if header_info.expansion_chip_hint == Some(ExpansionChipType::SVP) {
        return Ok(build_svp(rom_data, &header_info));
    }

    let mapper_id = header_info
        .mapper_hint
        .clone()
        .unwrap_or_else(|| detect_genesis_mapper(rom_data.len(), &header_info));

    let cart = match mapper_id {
        MapperId::GenesisSEGA => build_bank_mapper(rom_data, &header_info, BankMapperKind::Sega),
        MapperId::GenesisSSF2 => build_bank_mapper(rom_data, &header_info, BankMapperKind::Ssf2),
        MapperId::GenesisCodemasters => {
            build_bank_mapper(rom_data, &header_info, BankMapperKind::Codemasters)
        }
        MapperId::GenesisEEPROM => build_eeprom(rom_data, &header_info),
        MapperId::GenesisJCart => build_jcart(rom_data, &header_info),
        // GenesisNone and anything else fall back to a plain cartridge.
        _ => build_plain(rom_data, &header_info),
    };
    Ok(cart)
}

/// Build the exact cartridge a manifest describes.
///
/// The manifest's `mapping.preset` seeds the plan and, for a chip preset
/// the manifest does *not* itself declare in `chips[]`, the matching
/// coprocessor is auto-instantiated at the preset's `chip_id`, exactly as
/// [`build_genesis_cartridge`] would. This is what lets a bare
/// `detected_manifest` (which carries only the preset, no `chips[]`)
/// round-trip back to the same cartridge, mirroring the SNES builder.
pub fn build_with_manifest(
    rom_data: Vec<u8>,
    manifest: &CartridgeManifest,
) -> Result<Box<GenesisCartridge>, String> {
    let header_info = GenesisHeaderParser::new().parse(&rom_data).ok();
    let preset = parse_preset(&manifest.mapping.preset).map_err(|e| e.to_string())?;
    let plan = manifest_to_map_plan(manifest, &rom_data).map_err(|e| e.to_string())?;
    let sram = manifest_sram_window(manifest);
    // The preset seeds its MMIO windows at the first declared chip's id (or
    // 0 when none is declared), the same id `manifest_to_map_plan` uses.
    let preset_chip_id = manifest.chips.first().map_or(0, |c| c.id);

    // The chips the manifest declares, then the preset's implied chip when
    // the manifest left the preset's `chip_id` empty (the common
    // `detected_manifest` case).
    let mut decls = manifest
        .chips
        .iter()
        .enumerate()
        .map(|(i, chip)| ChipDecl::from_manifest(chip, i))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    if let Some(kind) = preset_chip_kind(preset) {
        if decls.iter().all(|decl| decl.id != preset_chip_id) {
            decls.push(preset_chip(preset_chip_id, kind));
        }
    }

    let info = make_info(
        &rom_data,
        header_info.as_ref(),
        preset_mapper_info(preset),
        preset_expansion_info(preset),
        DetectionSource::Manifest,
    );
    let rom: Arc<[u8]> = Arc::from(rom_data.into_boxed_slice());
    let mut chips = ChipSet::default();
    install_chips(
        &Sg1ChipFactory::default(),
        decls,
        &rom,
        DEFAULT_MASTER_CLOCK_HZ,
        &mut chips,
    )
    .map_err(|e| e.to_string())?;
    Ok(assemble(rom, plan, chips, info, sram))
}

/// Build the `cartridge-manifest-v1` that [`build_genesis_cartridge`]'s
/// heuristics resolve for `rom`, with no override applied, the "core
/// detects and generates a manifest" entry point (mirrors the SNES
/// `detected_manifest`).
///
/// Fed straight back through [`build_with_manifest`], the returned manifest
/// reproduces the same cartridge: it captures the detected preset, the
/// header's SRAM size, the ROM SHA-256 + size, and the title/region. When
/// the ROM is too small to carry a header it falls back to a bare `plain`
/// shell the user can correct.
pub fn detected_manifest(rom: &[u8]) -> CartridgeManifest {
    let header = GenesisHeaderParser::new().parse(rom).ok();

    // Resolve the same preset the builder would install.
    let preset = match header.as_ref() {
        Some(h) if h.expansion_chip_hint == Some(ExpansionChipType::SVP) => PresetKind::Svp,
        Some(h) => {
            let mapper = h
                .mapper_hint
                .clone()
                .unwrap_or_else(|| detect_genesis_mapper(rom.len(), h));
            mapper_to_preset(mapper)
        }
        None => PresetKind::Plain,
    };

    // The EEPROM save lives in the chip; other carts advertise a stock SRAM
    // window whose length is the manifest's `sram_size` shorthand.
    let sram_size = match (preset, header.as_ref()) {
        (PresetKind::Eeprom, _) => 0,
        (_, Some(h)) => sram_window(rom, h)
            .map(|(_, len)| len as usize)
            .unwrap_or(0),
        (_, None) => 0,
    };

    let title = header
        .as_ref()
        .map(|h| h.title.trim().to_string())
        .filter(|t| !t.is_empty());
    let region = header
        .as_ref()
        .and_then(|h| h.regions.first().copied())
        .map(region_label);

    CartridgeManifest {
        schema: Some(MANIFEST_SCHEMA.to_string()),
        core: "sg1".to_string(),
        version: SUPPORTED_MANIFEST_VERSION.to_string(),
        rom: ManifestRom {
            sha256: rom_sha256(rom),
            size: rom.len() as u64,
            title,
            region,
        },
        mapping: ManifestMapping {
            preset: preset_to_str(preset).to_string(),
            knobs: NoFields {},
            sram_size,
            slots: Vec::new(),
        },
        chips: Vec::new(),
        console: NoFields {},
        metadata: None,
    }
}

// --- assembly ----------------------------------------------------------------

/// Put a cartridge together: its plan and chips over `rom`, the SRAM
/// window `sram` filled with `0xFF` when it has one, and, when it has none,
/// the chip with a battery as the save (the EEPROM). `info` says the save
/// the cartridge keeps, the window's length or the chip's battery: a
/// header counts its range from its first address to its last, so an
/// odd-byte range ($200001-$20FFFF, 65,535 bytes) is a byte short of the
/// window it describes (65,536).
fn assemble(
    rom: Arc<[u8]>,
    plan: MapPlan,
    chips: ChipSet,
    mut info: CartridgeInfo,
    sram: Option<(u32, u32)>,
) -> Box<GenesisCartridge> {
    let board = GenesisBoard::new(rom.len(), sram);
    let battery_chip = if sram.is_none() {
        chips.battery_id()
    } else {
        None
    };
    info.sram_size = match sram {
        Some((_, len)) => len as usize,
        None => chips.battery().map_or(0, <[u8]>::len),
    };
    let mut cart = GenesisCartridge::new(rom, plan, chips, info, board);
    if let Some((_, len)) = sram {
        cart = cart.with_sram(vec![0xFF; len as usize]);
    }
    if let Some(id) = battery_chip {
        cart = cart.with_battery_chip(id);
    }
    Box::new(cart)
}

/// The chip a preset implies, at `id`: a SEGA or SSF2 mapper watches its
/// `0xA130Fx` bank registers, outside the ROM window its slot covers.
fn preset_chip(id: u8, kind: &str) -> ChipDecl {
    let observes = match kind {
        "sega" | "ssf2" => vec![SEGA_BANK_OBSERVE],
        _ => Vec::new(),
    };
    ChipDecl {
        id,
        kind: kind.to_string(),
        observes,
    }
}

/// The chips of a cartridge whose one chip is `kind`, at id 0.
fn one_chip(factory: &Sg1ChipFactory, kind: &str, rom: &Arc<[u8]>) -> ChipSet {
    let mut chips = ChipSet::default();
    install_chips(
        factory,
        [preset_chip(0, kind)],
        rom,
        DEFAULT_MASTER_CLOCK_HZ,
        &mut chips,
    )
    .expect("the factory builds every preset's chip");
    chips
}

fn build_plain(rom_data: Vec<u8>, header_info: &HeaderInfo) -> Box<GenesisCartridge> {
    let mapper_info = header_info.mapper_hint.clone().map(|id| MapperInfo {
        mapper_id: id,
        name: "None".to_string(),
        description: "Standard ROM with no mapper".to_string(),
        features: Vec::new(),
    });
    let info = make_info(
        &rom_data,
        Some(header_info),
        mapper_info,
        None,
        DetectionSource::Header,
    );
    let sram = sram_window(&rom_data, header_info);
    let rom: Arc<[u8]> = Arc::from(rom_data.into_boxed_slice());
    let plan = presets::plain_plan(rom.len(), sram);
    assemble(rom, plan, ChipSet::default(), info, sram)
}

fn build_bank_mapper(
    rom_data: Vec<u8>,
    header_info: &HeaderInfo,
    kind: BankMapperKind,
) -> Box<GenesisCartridge> {
    let info = make_info(
        &rom_data,
        Some(header_info),
        Some(bank_mapper_info(kind)),
        None,
        DetectionSource::Header,
    );
    let sram = sram_window(&rom_data, header_info);
    let rom: Arc<[u8]> = Arc::from(rom_data.into_boxed_slice());
    // SEGA and SSF2 program their banks through 0xA130Fx (outside the ROM
    // window), so the cartridge forwards those writes to the chip.
    // Codemasters uses ROM-window writes and needs no observer.
    let chips = one_chip(&Sg1ChipFactory::default(), bank_mapper_kind_str(kind), &rom);
    let plan = presets::bank_mapper_plan(kind, 0, sram);
    assemble(rom, plan, chips, info, sram)
}

fn build_eeprom(rom_data: Vec<u8>, header_info: &HeaderInfo) -> Box<GenesisCartridge> {
    let info = make_info(
        &rom_data,
        Some(header_info),
        Some(MapperInfo {
            mapper_id: MapperId::GenesisEEPROM,
            name: "EEPROM".to_string(),
            description: "Serial EEPROM save".to_string(),
            features: Vec::new(),
        }),
        None,
        DetectionSource::Header,
    );
    let rom: Arc<[u8]> = Arc::from(rom_data.into_boxed_slice());
    let factory = Sg1ChipFactory {
        eeprom_size: DEFAULT_EEPROM_SIZE,
    };
    let chips = one_chip(&factory, "eeprom", &rom);
    let plan = presets::eeprom_plan(0, rom.len(), presets::EEPROM_CONTROL_ADDR);
    // The save lives inside the EEPROM chip ([`assemble`] names it).
    assemble(rom, plan, chips, info, None)
}

fn build_jcart(rom_data: Vec<u8>, header_info: &HeaderInfo) -> Box<GenesisCartridge> {
    let info = make_info(
        &rom_data,
        Some(header_info),
        Some(MapperInfo {
            mapper_id: MapperId::GenesisJCart,
            name: "J-Cart".to_string(),
            description: "J-Cart extra controller ports".to_string(),
            features: Vec::new(),
        }),
        None,
        DetectionSource::Header,
    );
    let sram = sram_window(&rom_data, header_info);
    let rom: Arc<[u8]> = Arc::from(rom_data.into_boxed_slice());
    let chips = one_chip(&Sg1ChipFactory::default(), "jcart", &rom);
    let plan = presets::jcart_plan(0, rom.len(), sram);
    assemble(rom, plan, chips, info, sram)
}

fn build_svp(rom_data: Vec<u8>, header_info: &HeaderInfo) -> Box<GenesisCartridge> {
    let info = make_info(
        &rom_data,
        Some(header_info),
        None,
        Some(ExpansionChipInfo {
            name: "SVP".to_string(),
            chip_type: ExpansionChipType::SVP,
            description: "Sega Virtua Processor".to_string(),
            features: Vec::new(),
        }),
        DetectionSource::Header,
    );
    let rom: Arc<[u8]> = Arc::from(rom_data.into_boxed_slice());
    let chips = one_chip(&Sg1ChipFactory::default(), "svp", &rom);
    let plan = presets::svp_plan(0, rom.len());
    assemble(rom, plan, chips, info, None)
}

// --- metadata helpers ------------------------------------------------------

fn bank_mapper_kind_str(kind: BankMapperKind) -> &'static str {
    match kind {
        BankMapperKind::Sega => "sega",
        BankMapperKind::Ssf2 => "ssf2",
        BankMapperKind::Codemasters => "codemasters",
    }
}

fn bank_mapper_info(kind: BankMapperKind) -> MapperInfo {
    let (mapper_id, name) = match kind {
        BankMapperKind::Sega => (MapperId::GenesisSEGA, "SEGA"),
        BankMapperKind::Ssf2 => (MapperId::GenesisSSF2, "SSF2"),
        BankMapperKind::Codemasters => (MapperId::GenesisCodemasters, "Codemasters"),
    };
    MapperInfo {
        mapper_id,
        name: name.to_string(),
        description: "Genesis ROM bank mapper".to_string(),
        features: Vec::new(),
    }
}

/// Map a detected [`MapperId`] to the composable preset it builds.
fn mapper_to_preset(mapper: MapperId) -> PresetKind {
    match mapper {
        MapperId::GenesisSEGA => PresetKind::Sega,
        MapperId::GenesisSSF2 => PresetKind::Ssf2,
        MapperId::GenesisCodemasters => PresetKind::Codemasters,
        MapperId::GenesisEEPROM => PresetKind::Eeprom,
        MapperId::GenesisJCart => PresetKind::JCart,
        _ => PresetKind::Plain,
    }
}

/// Stable lowercase token for a preset, matching the strings
/// [`parse_preset`] accepts so a detected manifest round-trips.
fn preset_to_str(preset: PresetKind) -> &'static str {
    match preset {
        PresetKind::Plain => "plain",
        PresetKind::Sega => "sega",
        PresetKind::Ssf2 => "ssf2",
        PresetKind::Codemasters => "codemasters",
        PresetKind::Eeprom => "eeprom",
        PresetKind::JCart => "jcart",
        PresetKind::Svp => "svp",
        PresetKind::Custom => "custom",
    }
}

/// The chip-factory kind a preset implies, or `None` for a plain / custom
/// preset that carries no coprocessor.
fn preset_chip_kind(preset: PresetKind) -> Option<&'static str> {
    match preset {
        PresetKind::Sega => Some("sega"),
        PresetKind::Ssf2 => Some("ssf2"),
        PresetKind::Codemasters => Some("codemasters"),
        PresetKind::Eeprom => Some("eeprom"),
        PresetKind::JCart => Some("jcart"),
        PresetKind::Svp => Some("svp"),
        PresetKind::Plain | PresetKind::Custom => None,
    }
}

/// The [`MapperInfo`] a manifest-built cartridge should advertise for its
/// preset (so the debug panels label the mapper), or `None`.
fn preset_mapper_info(preset: PresetKind) -> Option<MapperInfo> {
    match preset {
        PresetKind::Sega => Some(bank_mapper_info(BankMapperKind::Sega)),
        PresetKind::Ssf2 => Some(bank_mapper_info(BankMapperKind::Ssf2)),
        PresetKind::Codemasters => Some(bank_mapper_info(BankMapperKind::Codemasters)),
        PresetKind::Eeprom => Some(MapperInfo {
            mapper_id: MapperId::GenesisEEPROM,
            name: "EEPROM".to_string(),
            description: "Serial EEPROM save".to_string(),
            features: Vec::new(),
        }),
        PresetKind::JCart => Some(MapperInfo {
            mapper_id: MapperId::GenesisJCart,
            name: "J-Cart".to_string(),
            description: "J-Cart extra controller ports".to_string(),
            features: Vec::new(),
        }),
        PresetKind::Plain | PresetKind::Svp | PresetKind::Custom => None,
    }
}

/// The [`ExpansionChipInfo`] a manifest-built cartridge should advertise for
/// its preset (only the SVP today), or `None`.
fn preset_expansion_info(preset: PresetKind) -> Option<ExpansionChipInfo> {
    match preset {
        PresetKind::Svp => Some(ExpansionChipInfo {
            name: "SVP".to_string(),
            chip_type: ExpansionChipType::SVP,
            description: "Sega Virtua Processor".to_string(),
            features: Vec::new(),
        }),
        _ => None,
    }
}

/// Human region label for the manifest's `rom.region`, from the first
/// header region.
fn region_label(region: Region) -> String {
    match region {
        Region::Japan => "Japan",
        Region::USA => "USA",
        Region::Europe => "Europe",
        Region::Asia => "Asia",
        Region::Brazil => "Brazil",
        Region::Korea => "Korea",
        Region::China => "China",
        Region::World => "World",
    }
    .to_string()
}

/// The SRAM window `(start, len)` a header advertises, word-aligned, or `None`
/// when the cart has no SRAM. The length is cut to [`SRAM_SIZE_LIMIT`].
fn sram_window(rom_data: &[u8], header_info: &HeaderInfo) -> Option<(u32, u32)> {
    if header_info.sram_size == 0 {
        return None;
    }
    let header = &rom_data[0x100..0x200];
    let (start_addr, end_addr) = if header[0xB0] == b'R' && header[0xB1] == b'A' {
        (
            u32::from_be_bytes([header[0xB4], header[0xB5], header[0xB6], header[0xB7]]),
            u32::from_be_bytes([header[0xB8], header[0xB9], header[0xBA], header[0xBB]]),
        )
    } else if header[0xA8] & 0x40 != 0 {
        (
            u32::from_be_bytes([0, header[0xA9], header[0xAA], header[0xAB]]),
            u32::from_be_bytes([header[0xAC], header[0xAD], header[0xAE], header[0xAF]]),
        )
    } else {
        (0x200001, 0x200001 + header_info.sram_size as u32 - 1)
    };
    let start = start_addr & !1;
    // Round the end up to an even, exclusive boundary, in 64 bits: an end of
    // $FFFFFFFF rounds up past what a `u32` holds.
    let end = (u64::from(end_addr) | 1) + 1;
    let len = end
        .saturating_sub(u64::from(start))
        .min(u64::from(SRAM_SIZE_LIMIT));
    (len > 0).then_some((start, len as u32))
}

/// What the cartridge is: the image's size and hash, the header's fields
/// when there is a header, the mapper and chip the builder chose. The size
/// of its save is [`assemble`]'s, which knows what the cartridge keeps.
fn make_info(
    rom_data: &[u8],
    header_info: Option<&HeaderInfo>,
    mapper_info: Option<MapperInfo>,
    expansion_chip_info: Option<ExpansionChipInfo>,
    detection_source: DetectionSource,
) -> CartridgeInfo {
    let (title, serial, regions, sram_type, header_checksum) = match header_info {
        Some(h) => (
            h.title.clone(),
            h.serial.clone(),
            h.regions.clone(),
            h.sram_type,
            h.checksum,
        ),
        None => (
            String::new(),
            None,
            Vec::new(),
            retro_core::cartridge::SRAMType::None,
            None,
        ),
    };
    CartridgeInfo {
        title,
        serial,
        regions,
        sram_type,
        mapper_info,
        expansion_chip_info,
        header_checksum,
        calculated_checksum: Some(calculate_genesis_checksum(rom_data)),
        detection_source,
        ..CartridgeInfo::new(ConsoleSystem::Genesis, rom_data)
    }
}

#[cfg(test)]
mod tests {
    use retro_core::cartridge::Cartridge;

    use super::*;
    use crate::cartridge::manifest::parse_manifest;

    /// Every byte a NOP, the header included: its RAM field reads as an SRAM
    /// range of $4E000001 bytes, which the cartridge must not allocate whole.
    #[test]
    fn a_garbage_header_gets_no_more_sram_than_the_limit() {
        let mut rom = [0x4Eu8, 0x71].repeat(0x10000);
        rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
        let cart = build_genesis_cartridge(rom).unwrap();
        assert_eq!(cart.sram().len(), SRAM_SIZE_LIMIT as usize);
    }

    #[test]
    fn a_real_sram_window_is_not_cut() {
        let rom = genesis_rom(0x80000, "SAVES", true);
        let header = GenesisHeaderParser::new().parse(&rom).unwrap();
        let (_, len) = sram_window(&rom, &header).expect("the header names SRAM");
        assert!(len < SRAM_SIZE_LIMIT);
        assert_eq!(
            build_genesis_cartridge(rom).unwrap().sram().len(),
            len as usize
        );
    }

    /// What the cartridge declares of its save is the save it keeps: the
    /// window the header's odd-byte range $200001-$20FFFF describes is
    /// 65,536 bytes, where the header counts 65,535; a manifest's window is
    /// its own size; a cartridge with no save declares none.
    #[test]
    fn the_cartridge_declares_the_save_it_keeps() {
        let rom = genesis_rom(0x80000, "SAVES", true);
        let header = GenesisHeaderParser::new().parse(&rom).unwrap();
        assert_eq!(header.sram_size, 0xFFFF, "the header's count");
        let cart = build_genesis_cartridge(rom.clone()).unwrap();
        assert_eq!(cart.sram().len(), 0x10000);
        assert_eq!(cart.info().sram_size, cart.sram().len());
        let mut manifest = detected_manifest(&rom);
        assert_eq!(manifest.mapping.sram_size, cart.sram().len());
        manifest.mapping.sram_size = 0x8000;
        let cart = build_with_manifest(rom, &manifest).unwrap();
        assert_eq!(cart.sram().len(), 0x8000);
        assert_eq!(cart.info().sram_size, 0x8000);
        let none = build_genesis_cartridge(genesis_rom(0x80000, "NO SAVE", false)).unwrap();
        assert_eq!(none.info().sram_size, 0);
    }

    /// Build a synthetic Genesis ROM with a valid header.
    fn genesis_rom(size: usize, title: &str, with_sram: bool) -> Vec<u8> {
        let mut rom = vec![0u8; size.max(0x200)];
        rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
        rom[0x110..0x120].copy_from_slice(b"(C)TEST 2025.JAN");
        let title = format!("{title:<48}");
        rom[0x120..0x150].copy_from_slice(&title.as_bytes()[..48]);
        rom[0x150..0x180].copy_from_slice(&title.as_bytes()[..48]);
        rom[0x183..0x190].copy_from_slice(b"GM T-00000-00");
        rom[0x190..0x1A0].copy_from_slice(b"J               ");
        rom[0x1A0..0x1A4].copy_from_slice(&0u32.to_be_bytes());
        rom[0x1A4..0x1A8].copy_from_slice(&((size - 1) as u32).to_be_bytes());
        if with_sram {
            rom[0x1A8] = 0xE0;
            rom[0x1A9..0x1AC].copy_from_slice(&0x200001u32.to_be_bytes()[1..]);
            rom[0x1AC..0x1B0].copy_from_slice(&0x20FFFFu32.to_be_bytes());
        }
        rom[0x1F0..0x1F3].copy_from_slice(b"JUE");
        // A recognizable ROM byte.
        rom[0x200] = 0xA5;
        rom
    }

    #[test]
    fn detects_plain_small_rom() {
        let rom = genesis_rom(0x80000, "PLAIN GAME", false);
        let mut cart = build_genesis_cartridge(rom).unwrap();
        assert_eq!(cart.map_plan().name, "plain");
        assert_eq!(cart.info().title, "PLAIN GAME");
        assert_eq!(cart.read_bus(0x000200, 0, 0xFF), 0xA5);
        assert!(cart.coprocessor_label().is_none());
    }

    #[test]
    fn detects_sega_mapper_for_large_rom() {
        // 5 MiB ROM -> SEGA mapper. Mark bank 9's first byte so a banked read
        // through the observer-programmed table is provable.
        let mut rom = genesis_rom(0x500000, "BIG GAME", false);
        rom[9 * 0x80000] = 0x9B;
        let mut cart = build_genesis_cartridge(rom).unwrap();
        assert_eq!(cart.map_plan().name, "sega");
        assert_eq!(cart.coprocessor_label(), Some("SEGA mapper"));
        // Bank-switch slot 0 to bank 9 via 0xA130F0 (through the observer),
        // then a ROM-window read at 0x000000 comes from physical bank 9.
        cart.write_bus(0xA130F0, 9, 0);
        assert_eq!(cart.read_bus(0x000000, 0, 0xFF), 0x9B);
        assert!(cart.coprocessor_accesses() > 0);
    }

    #[test]
    fn detects_svp_before_mapper_match() {
        // Virtua Racing is 2 MiB (<= 4 MiB): without the SVP check ahead of the
        // mapper match it would build a plain cart and never make the SVP.
        let rom = genesis_rom(0x200000, "VIRTUA RACING", false);
        let cart = build_genesis_cartridge(rom).unwrap();
        assert_eq!(cart.map_plan().name, "svp");
        assert_eq!(cart.coprocessor_label(), Some("SVP"));
        assert_eq!(
            cart.info()
                .expansion_chip_info
                .as_ref()
                .map(|c| c.chip_type),
            Some(ExpansionChipType::SVP)
        );
    }

    #[test]
    fn plain_cart_with_permanent_sram() {
        let rom = genesis_rom(0x200000, "RPG WITH SAVE", true);
        let mut cart = build_genesis_cartridge(rom).unwrap();
        assert_eq!(cart.map_plan().name, "plain");
        // SRAM at 0x200000 is beyond the 2 MiB image -> permanent, writable.
        cart.write_bus(0x200000, 0x5A, 0);
        assert_eq!(cart.read_bus(0x200000, 0, 0xFF), 0x5A);
        assert!(cart.is_sram_dirty());
    }

    #[test]
    fn rejects_undersized_rom() {
        assert!(build_genesis_cartridge(vec![0u8; 0x100]).is_err());
    }

    #[test]
    fn builds_svp_from_manifest() {
        let rom = genesis_rom(0x200000, "VIRTUA RACING", false);
        let json = r#"{
            "core": "sg1",
            "version": "1.0",
            "rom": { "sha256": "x", "size": 2097152, "title": "VIRTUA RACING" },
            "mapping": { "preset": "svp", "sram_size": 0 },
            "chips": [
                {
                    "id": 0,
                    "kind": "svp",
                    "mapping": {
                        "addr_range": ["0x300000", "0x31FFFF"],
                        "target": { "kind": "ChipMmio", "chip_id": 0, "base": 0 }
                    }
                }
            ]
        }"#;
        let manifest = parse_manifest(json).unwrap();
        let cart = build_with_manifest(rom, &manifest).unwrap();
        assert_eq!(cart.coprocessor_label(), Some("SVP"));
        assert_eq!(cart.info().detection_source, DetectionSource::Manifest);
    }

    #[test]
    fn builds_eeprom_from_manifest_and_routes_battery() {
        let rom = genesis_rom(0x100000, "NBA JAM", false);
        let json = r#"{
            "core": "sg1",
            "version": "1.0",
            "rom": { "sha256": "x", "size": 1048576 },
            "mapping": { "preset": "eeprom", "sram_size": 0 },
            "chips": [
                {
                    "id": 0,
                    "kind": "eeprom",
                    "mapping": {
                        "addr_range": ["0x200001", "0x200001"],
                        "target": { "kind": "ChipMmio", "chip_id": 0, "base": 0 }
                    }
                }
            ]
        }"#;
        let manifest = parse_manifest(json).unwrap();
        let cart = build_with_manifest(rom, &manifest).unwrap();
        // The EEPROM's battery is the cartridge's `.srm`, and what it
        // declares of its save.
        assert_eq!(cart.sram().len(), DEFAULT_EEPROM_SIZE);
        assert_eq!(cart.info().sram_size, DEFAULT_EEPROM_SIZE);
    }

    #[test]
    fn manifest_unknown_chip_kind_errors() {
        let rom = genesis_rom(0x80000, "X", false);
        let json = r#"{
            "core": "sg1",
            "version": "1.0",
            "rom": { "sha256": "x", "size": 524288 },
            "mapping": { "preset": "custom" },
            "chips": [
                {
                    "id": 0,
                    "kind": "supervChipZZZ",
                    "mapping": {
                        "addr_range": ["0x300000", "0x30FFFF"],
                        "target": { "kind": "ChipMmio", "chip_id": 0, "base": 0 }
                    }
                }
            ]
        }"#;
        let manifest = parse_manifest(json).unwrap();
        assert_eq!(
            build_with_manifest(rom, &manifest).err().as_deref(),
            Some("unknown coprocessor chip kind: supervChipZZZ")
        );
    }

    // -- detected_manifest exporter -----------------------------------------

    #[test]
    fn detected_manifest_plain_round_trips() {
        let rom = genesis_rom(0x80000, "PLAIN GAME", false);
        let manifest = detected_manifest(&rom);
        assert_eq!(manifest.core, "sg1");
        assert_eq!(manifest.version, "1.0");
        assert_eq!(manifest.mapping.preset, "plain");
        assert_eq!(manifest.rom.size, 0x80000);
        assert_eq!(manifest.rom.title.as_deref(), Some("PLAIN GAME"));
        assert_eq!(manifest.rom.sha256, rom_sha256(&rom));
        assert_eq!(manifest.rom.region.as_deref(), Some("Japan"));

        // Fed back, it reproduces the same cartridge shape.
        let mut cart = build_with_manifest(rom.clone(), &manifest).unwrap();
        assert_eq!(cart.map_plan().name, "plain");
        assert_eq!(cart.read_bus(0x000200, 0, 0xFF), 0xA5);
        assert_eq!(cart.info().detection_source, DetectionSource::Manifest);
    }

    #[test]
    fn detected_manifest_svp_round_trips() {
        let rom = genesis_rom(0x200000, "VIRTUA RACING", false);
        let manifest = detected_manifest(&rom);
        assert_eq!(manifest.mapping.preset, "svp");
        // No explicit chips, the builder auto-instantiates the SVP.
        assert!(manifest.chips.is_empty());

        let cart = build_with_manifest(rom, &manifest).unwrap();
        assert_eq!(cart.map_plan().name, "svp");
        assert_eq!(cart.coprocessor_label(), Some("SVP"));
        assert_eq!(
            cart.info()
                .expansion_chip_info
                .as_ref()
                .map(|c| c.chip_type),
            Some(ExpansionChipType::SVP)
        );
    }

    #[test]
    fn detected_manifest_sega_round_trips_and_auto_builds_chip() {
        let mut rom = genesis_rom(0x500000, "BIG GAME", false);
        rom[9 * 0x80000] = 0x9B;
        let manifest = detected_manifest(&rom);
        assert_eq!(manifest.mapping.preset, "sega");
        assert!(manifest.chips.is_empty());

        // The bare manifest (no chips[]) still builds a working mapper: the
        // preset chip is auto-instantiated with its 0xA130Fx observer.
        let mut cart = build_with_manifest(rom, &manifest).unwrap();
        assert_eq!(cart.map_plan().name, "sega");
        assert_eq!(cart.coprocessor_label(), Some("SEGA mapper"));
        cart.write_bus(0xA130F0, 9, 0);
        assert_eq!(cart.read_bus(0x000000, 0, 0xFF), 0x9B);
    }

    #[test]
    fn detected_manifest_captures_sram_and_round_trips() {
        let rom = genesis_rom(0x200000, "RPG WITH SAVE", true);
        let manifest = detected_manifest(&rom);
        assert_eq!(manifest.mapping.preset, "plain");
        assert!(manifest.mapping.sram_size > 0);

        let mut cart = build_with_manifest(rom, &manifest).unwrap();
        cart.write_bus(0x200000, 0x5A, 0);
        assert_eq!(cart.read_bus(0x200000, 0, 0xFF), 0x5A);
        assert!(cart.is_sram_dirty());
    }

    #[test]
    fn detected_manifest_falls_back_to_plain_without_header() {
        // A blob too small for a header falls back to a bare plain shell.
        let manifest = detected_manifest(&[0u8; 0x100]);
        assert_eq!(manifest.mapping.preset, "plain");
        assert_eq!(manifest.mapping.sram_size, 0);
        assert!(manifest.rom.title.is_none());
    }
}
