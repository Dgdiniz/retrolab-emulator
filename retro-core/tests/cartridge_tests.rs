//! Tests of the cartridge types a host is told about: their equality and
//! their serialized form.

use retro_core::cartridge::*;

// ============================================================================
// Enum Tests
// ============================================================================

#[test]
fn test_console_system_eq() {
    assert_eq!(ConsoleSystem::Genesis, ConsoleSystem::Genesis);
    assert_ne!(ConsoleSystem::Genesis, ConsoleSystem::NES);
}

#[test]
fn test_region_eq() {
    assert_eq!(Region::USA, Region::USA);
    assert_ne!(Region::USA, Region::Japan);
}

#[test]
fn test_sram_type_eq() {
    assert_eq!(SRAMType::Battery, SRAMType::Battery);
    assert_ne!(SRAMType::Battery, SRAMType::EEPROM);
}

#[test]
fn test_mapper_id_eq() {
    assert_eq!(MapperId::GenesisSEGA, MapperId::GenesisSEGA);
    assert_ne!(MapperId::GenesisSEGA, MapperId::GenesisSSF2);

    let nes1 = MapperId::NES(4);
    let nes2 = MapperId::NES(4);
    let nes3 = MapperId::NES(5);
    assert_eq!(nes1, nes2);
    assert_ne!(nes1, nes3);
}

#[test]
fn test_mapper_id_custom() {
    let custom1 = MapperId::Custom("MyMapper".to_string());
    let custom2 = MapperId::Custom("MyMapper".to_string());
    let custom3 = MapperId::Custom("OtherMapper".to_string());

    assert_eq!(custom1, custom2);
    assert_ne!(custom1, custom3);
}

#[test]
fn test_memory_type_eq() {
    assert_eq!(MemoryType::ROM, MemoryType::ROM);
    assert_ne!(MemoryType::ROM, MemoryType::SRAM);
}

#[test]
fn test_expansion_chip_type_eq() {
    assert_eq!(ExpansionChipType::SVP, ExpansionChipType::SVP);
    assert_ne!(ExpansionChipType::SVP, ExpansionChipType::SuperFX);
}

// ============================================================================
// Struct Serialization Tests
// ============================================================================

#[test]
fn test_cartridge_info_serialize() {
    let info = CartridgeInfo {
        system: ConsoleSystem::Genesis,
        title: "Test".to_string(),
        serial: Some("TEST-001".to_string()),
        regions: vec![Region::USA, Region::Europe],
        rom_size: 0x100000,
        sram_size: 0x2000,
        sram_type: SRAMType::Battery,
        mapper_info: None,
        expansion_chip_info: None,
        header_checksum: Some(0x1234),
        calculated_checksum: Some(0x1234),
        rom_hash: "hash".to_string(),
        detection_source: DetectionSource::Header,
    };

    let serialized = serde_json::to_string(&info).unwrap();
    let deserialized: CartridgeInfo = serde_json::from_str(&serialized).unwrap();

    assert_eq!(deserialized.system, info.system);
    assert_eq!(deserialized.title, info.title);
    assert_eq!(deserialized.rom_size, info.rom_size);
}

#[test]
fn test_bank_configuration_serialize() {
    let config = BankConfiguration {
        banks: vec![
            BankInfo {
                cpu_address_start: 0,
                cpu_address_end: 0x7FFF,
                physical_offset: 0,
                size: 0x8000,
                memory_type: MemoryType::ROM,
                bank_number: 0,
            },
            BankInfo {
                cpu_address_start: 0x8000,
                cpu_address_end: 0xFFFF,
                physical_offset: 0x8000,
                size: 0x8000,
                memory_type: MemoryType::ROM,
                bank_number: 1,
            },
        ],
    };

    let serialized = serde_json::to_string(&config).unwrap();
    let deserialized: BankConfiguration = serde_json::from_str(&serialized).unwrap();

    assert_eq!(deserialized.banks.len(), 2);
    assert_eq!(deserialized.banks[0].bank_number, 0);
    assert_eq!(deserialized.banks[1].bank_number, 1);
}
