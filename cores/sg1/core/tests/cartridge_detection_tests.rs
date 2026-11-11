//! Integration tests for Genesis cartridge detection

use retro_core::cartridge::detection::HeaderParser;
use retro_core::cartridge::{ConsoleSystem, ExpansionChipType, MapperId, Region, SRAMType};
use sg1_core::cartridge::{calculate_genesis_checksum, detect_genesis_mapper, GenesisHeaderParser};

// ============================================================================
// Helper Functions
// ============================================================================

fn create_genesis_rom_with_header(size: usize, title: &str, has_sram: bool) -> Vec<u8> {
    let mut rom = vec![0; size.max(0x200)];

    // Console name
    rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");

    // Copyright
    rom[0x110..0x120].copy_from_slice(b"(C)TEST 2025.JAN");

    // Title (domestic and overseas)
    let title_str = format!("{:<48}", title);
    rom[0x120..0x150].copy_from_slice(title_str.as_bytes());
    rom[0x150..0x180].copy_from_slice(title_str.as_bytes());

    // Serial
    let serial_str = b"GM T-12345-01";
    rom[0x183..0x183 + serial_str.len()].copy_from_slice(serial_str);

    // Checksum (placeholder)
    rom[0x18E..0x190].copy_from_slice(&[0, 0]);

    // I/O support
    rom[0x190..0x1A0].copy_from_slice(b"J               ");

    // ROM range
    rom[0x1A0..0x1A4].copy_from_slice(&0x00000000u32.to_be_bytes());
    rom[0x1A4..0x1A8].copy_from_slice(&((size - 1) as u32).to_be_bytes());

    // SRAM (if requested)
    if has_sram {
        rom[0x1A8] = 0xE0;
        rom[0x1A9..0x1AC].copy_from_slice(&0x200001u32.to_be_bytes()[1..]);
        rom[0x1AC..0x1B0].copy_from_slice(&0x20FFFFu32.to_be_bytes());
    }

    // Regions
    rom[0x1F0..0x1F3].copy_from_slice(b"JUE");

    rom
}

// ============================================================================
// Console Detection Tests
// ============================================================================

#[test]
fn test_detect_genesis_by_signature() {
    let rom = create_genesis_rom_with_header(0x80000, "Test Game", false);
    let system = retro_core::cartridge::detection::detect_console_system(&rom);

    assert_eq!(system, Some(ConsoleSystem::Genesis));
}

// ============================================================================
// Header Parser Tests
// ============================================================================

#[test]
fn test_genesis_parser_basic() {
    let rom = create_genesis_rom_with_header(0x80000, "SONIC THE HEDGEHOG", false);
    let parser = GenesisHeaderParser::new();

    assert!(parser.validate(&rom));

    let info = parser.parse(&rom).unwrap();
    assert_eq!(info.title, "SONIC THE HEDGEHOG");
    assert_eq!(info.rom_size, 0x80000);
    assert_eq!(info.sram_size, 0);
    assert_eq!(info.sram_type, SRAMType::None);
}

#[test]
fn test_genesis_parser_with_sram() {
    let rom = create_genesis_rom_with_header(0x80000, "Test With SRAM", true);
    let parser = GenesisHeaderParser::new();

    let info = parser.parse(&rom).unwrap();
    assert!(info.sram_size > 0);
    assert_eq!(info.sram_type, SRAMType::Battery);
}

#[test]
fn test_genesis_parser_virtua_racing() {
    let rom = create_genesis_rom_with_header(0x200000, "VIRTUA RACING", false);
    let parser = GenesisHeaderParser::new();

    let info = parser.parse(&rom).unwrap();
    assert_eq!(info.expansion_chip_hint, Some(ExpansionChipType::SVP));
}

#[test]
fn test_genesis_parser_sonic_3() {
    let rom = create_genesis_rom_with_header(0x400000, "SONIC 3", true);
    let parser = GenesisHeaderParser::new();

    let info = parser.parse(&rom).unwrap();
    assert_eq!(info.sram_type, SRAMType::EEPROM);
}

#[test]
fn test_genesis_parser_regions() {
    let rom = create_genesis_rom_with_header(0x80000, "Test", false);
    let parser = GenesisHeaderParser::new();

    let info = parser.parse(&rom).unwrap();
    assert!(!info.regions.is_empty());
    assert!(
        info.regions.contains(&Region::Japan)
            || info.regions.contains(&Region::USA)
            || info.regions.contains(&Region::Europe)
    );
}

#[test]
fn test_genesis_parser_invalid_header() {
    let mut rom = vec![0; 0x200];
    rom[0x100..0x110].copy_from_slice(b"NOT A GENESIS ! ");

    let parser = GenesisHeaderParser::new();
    assert!(!parser.validate(&rom));

    let result = parser.parse(&rom);
    assert!(result.is_err());
}

// ============================================================================
// Mapper Detection Tests
// ============================================================================

#[test]
fn test_detect_no_mapper_small_rom() {
    let rom = create_genesis_rom_with_header(0x80000, "Small ROM", false);
    let parser = GenesisHeaderParser::new();
    let header = parser.parse(&rom).unwrap();

    let mapper = detect_genesis_mapper(rom.len(), &header);
    assert_eq!(mapper, MapperId::GenesisNone);
}

#[test]
fn test_detect_sega_mapper_large_rom() {
    let rom = create_genesis_rom_with_header(0x500000, "Large ROM", false);
    let parser = GenesisHeaderParser::new();
    let header = parser.parse(&rom).unwrap();

    let mapper = detect_genesis_mapper(rom.len(), &header);
    assert_eq!(mapper, MapperId::GenesisSEGA);
}

// ============================================================================
// Checksum Tests
// ============================================================================

#[test]
fn test_calculate_genesis_checksum() {
    let rom = create_genesis_rom_with_header(0x1000, "Test", false);
    let checksum = calculate_genesis_checksum(&rom);

    // Checksum should be deterministic
    let checksum2 = calculate_genesis_checksum(&rom);
    assert_eq!(checksum, checksum2);
}
