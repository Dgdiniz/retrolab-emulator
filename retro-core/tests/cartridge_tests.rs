//! Integration tests for cartridge traits

use retro_core::cartridge::*;

// ============================================================================
// Mock Implementations
// ============================================================================

/// Mock cartridge for testing
struct MockCartridge {
    info: CartridgeInfo,
    rom: Vec<u8>,
    sram: Option<Vec<u8>>,
    sram_dirty: bool,
}

impl MockCartridge {
    fn new_with_rom(rom: Vec<u8>) -> Self {
        Self {
            info: CartridgeInfo {
                system: ConsoleSystem::Genesis,
                title: "Test ROM".to_string(),
                serial: Some("TEST-001".to_string()),
                regions: vec![Region::USA],
                rom_size: rom.len(),
                sram_size: 0,
                sram_type: SRAMType::None,
                mapper_info: None,
                expansion_chip_info: None,
                header_checksum: Some(0x1234),
                calculated_checksum: Some(0x1234),
                rom_hash: "test_hash".to_string(),
                detection_source: DetectionSource::Header,
            },
            rom,
            sram: None,
            sram_dirty: false,
        }
    }

    fn new_with_sram(rom: Vec<u8>, sram_size: usize) -> Self {
        let mut cart = Self::new_with_rom(rom);
        cart.sram = Some(vec![0xFF; sram_size]);
        cart.info.sram_size = sram_size;
        cart.info.sram_type = SRAMType::Battery;
        cart
    }
}

impl Cartridge for MockCartridge {
    fn info(&self) -> &CartridgeInfo {
        &self.info
    }

    fn read(&mut self, addr: u32) -> u8 {
        let addr = addr as usize;
        if addr < self.rom.len() {
            self.rom[addr]
        } else {
            0xFF
        }
    }

    fn write(&mut self, addr: u32, value: u8) {
        if let Some(ref mut sram) = self.sram {
            let offset = addr as usize;
            if offset < sram.len() {
                sram[offset] = value;
                self.sram_dirty = true;
            }
        }
    }

    fn get_save_ram(&self) -> Option<&[u8]> {
        self.sram.as_deref()
    }

    fn set_save_ram(&mut self, data: &[u8]) -> Result<(), String> {
        if let Some(ref mut sram) = self.sram {
            if data.len() != sram.len() {
                return Err(format!(
                    "Size mismatch: expected {}, got {}",
                    sram.len(),
                    data.len()
                ));
            }
            sram.copy_from_slice(data);
            self.sram_dirty = false;
            Ok(())
        } else {
            Err("No SRAM".to_string())
        }
    }

    fn is_save_ram_dirty(&self) -> bool {
        self.sram_dirty
    }

    fn clear_save_ram_dirty(&mut self) {
        self.sram_dirty = false;
    }

    fn serialize_state(&self) -> Result<Vec<u8>, String> {
        bincode::serialize(&self.sram).map_err(|e| e.to_string())
    }

    fn deserialize_state(&mut self, data: &[u8]) -> Result<(), String> {
        self.sram = bincode::deserialize(data).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn reset(&mut self) {
        if let Some(ref mut sram) = self.sram {
            sram.fill(0xFF);
        }
        self.sram_dirty = false;
    }
}

// ============================================================================
// Cartridge Trait Tests
// ============================================================================

#[test]
fn test_cartridge_read() {
    let rom = vec![0x12, 0x34, 0x56, 0x78];
    let mut cart = MockCartridge::new_with_rom(rom);

    assert_eq!(cart.read(0), 0x12);
    assert_eq!(cart.read(1), 0x34);
    assert_eq!(cart.read(2), 0x56);
    assert_eq!(cart.read(3), 0x78);
}

#[test]
fn test_cartridge_read_word() {
    let rom = vec![0x12, 0x34, 0x56, 0x78];
    let mut cart = MockCartridge::new_with_rom(rom);

    assert_eq!(cart.read_word(0), 0x1234); // Big endian
    assert_eq!(cart.read_word(2), 0x5678);
}

#[test]
fn test_cartridge_write_word() {
    let rom = vec![0; 4];
    let mut cart = MockCartridge::new_with_sram(rom, 4);

    cart.write_word(0, 0xABCD);
    assert_eq!(cart.sram.as_ref().unwrap()[0], 0xAB);
    assert_eq!(cart.sram.as_ref().unwrap()[1], 0xCD);
}

#[test]
fn test_cartridge_info() {
    let rom = vec![0; 100];
    let cart = MockCartridge::new_with_rom(rom);
    let info = cart.info();

    assert_eq!(info.system, ConsoleSystem::Genesis);
    assert_eq!(info.title, "Test ROM");
    assert_eq!(info.rom_size, 100);
    assert_eq!(info.sram_size, 0);
}

#[test]
fn test_sram_read_write() {
    let rom = vec![0; 100];
    let mut cart = MockCartridge::new_with_sram(rom, 8);

    // Write to SRAM
    cart.write(0, 0x42);
    cart.write(1, 0x43);

    // Verify dirty flag
    assert!(cart.is_save_ram_dirty());

    // Get SRAM directly
    let sram = cart.get_save_ram().unwrap();
    assert_eq!(sram[0], 0x42);
    assert_eq!(sram[1], 0x43);
}

#[test]
fn test_sram_set() {
    let rom = vec![0; 100];
    let mut cart = MockCartridge::new_with_sram(rom, 8);

    let new_sram = vec![1, 2, 3, 4, 5, 6, 7, 8];
    cart.set_save_ram(&new_sram).unwrap();

    assert!(!cart.is_save_ram_dirty());

    let sram = cart.get_save_ram().unwrap();
    assert_eq!(sram, &new_sram[..]);
}

#[test]
fn test_sram_set_size_mismatch() {
    let rom = vec![0; 100];
    let mut cart = MockCartridge::new_with_sram(rom, 8);

    let wrong_size = vec![1, 2, 3];
    assert!(cart.set_save_ram(&wrong_size).is_err());
}

#[test]
fn test_cartridge_reset() {
    let rom = vec![0; 100];
    let mut cart = MockCartridge::new_with_sram(rom, 8);

    cart.write(0, 0x42);
    assert!(cart.is_save_ram_dirty());

    cart.reset();
    assert!(!cart.is_save_ram_dirty());

    let sram = cart.get_save_ram().unwrap();
    assert_eq!(sram[0], 0xFF); // Reset to 0xFF
}

#[test]
fn test_cartridge_serialize() {
    let rom = vec![0; 100];
    let mut cart = MockCartridge::new_with_sram(rom, 8);

    cart.write(0, 0x42);
    cart.write(1, 0x43);

    let state = cart.serialize_state().unwrap();

    // Create new cartridge and load state
    let rom2 = vec![0; 100];
    let mut cart2 = MockCartridge::new_with_sram(rom2, 8);
    cart2.deserialize_state(&state).unwrap();

    let sram = cart2.get_save_ram().unwrap();
    assert_eq!(sram[0], 0x42);
    assert_eq!(sram[1], 0x43);
}

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

// ============================================================================
// Mock Mapper Tests
// ============================================================================

struct MockMapper {
    info: MapperInfo,
    bank: u8,
}

impl MockMapper {
    fn new() -> Self {
        Self {
            info: MapperInfo {
                mapper_id: MapperId::GenesisSEGA,
                name: "Mock Mapper".to_string(),
                description: "Test mapper".to_string(),
                features: vec!["banking".to_string()],
            },
            bank: 0,
        }
    }
}

impl Mapper for MockMapper {
    fn info(&self) -> &MapperInfo {
        &self.info
    }

    fn map_read(&self, addr: u32) -> MappedAddress {
        let offset = (self.bank as usize * 0x8000) + (addr as usize & 0x7FFF);
        MappedAddress {
            physical_offset: offset,
            memory_type: MemoryType::ROM,
            writable: false,
        }
    }

    fn map_write(&mut self, _addr: u32, _value: u8) -> Option<MappedAddress> {
        None
    }

    fn write_register(&mut self, _addr: u32, value: u8) -> bool {
        self.bank = value;
        true
    }

    fn get_bank_config(&self) -> BankConfiguration {
        BankConfiguration {
            banks: vec![BankInfo {
                cpu_address_start: 0,
                cpu_address_end: 0x7FFF,
                physical_offset: (self.bank as usize) * 0x8000,
                size: 0x8000,
                memory_type: MemoryType::ROM,
                bank_number: self.bank as u32,
            }],
        }
    }

    fn reset(&mut self) {
        self.bank = 0;
    }

    fn serialize_mapper(&self) -> Result<Vec<u8>, String> {
        Ok(vec![self.bank])
    }

    fn deserialize_mapper(&mut self, data: &[u8]) -> Result<(), String> {
        if data.is_empty() {
            return Err("Empty data".to_string());
        }
        self.bank = data[0];
        Ok(())
    }
}

#[test]
fn test_mapper_info() {
    let mapper = MockMapper::new();
    let info = mapper.info();

    assert_eq!(info.mapper_id, MapperId::GenesisSEGA);
    assert_eq!(info.name, "Mock Mapper");
}

#[test]
fn test_mapper_bank_switching() {
    let mut mapper = MockMapper::new();

    // Initial bank
    let mapped = mapper.map_read(0x1000);
    assert_eq!(mapped.physical_offset, 0x1000);
    assert_eq!(mapped.memory_type, MemoryType::ROM);
    assert!(!mapped.writable);

    // Switch to bank 1
    mapper.write_register(0x8000, 1);
    let mapped = mapper.map_read(0x1000);
    assert_eq!(mapped.physical_offset, 0x9000); // Bank 1 * 0x8000 + 0x1000
}

#[test]
fn test_mapper_get_bank_config() {
    let mut mapper = MockMapper::new();
    mapper.write_register(0x8000, 2);

    let config = mapper.get_bank_config();
    assert_eq!(config.banks.len(), 1);
    assert_eq!(config.banks[0].bank_number, 2);
    assert_eq!(config.banks[0].physical_offset, 0x10000); // Bank 2 * 0x8000
}

#[test]
fn test_mapper_reset() {
    let mut mapper = MockMapper::new();
    mapper.write_register(0x8000, 5);

    mapper.reset();
    let config = mapper.get_bank_config();
    assert_eq!(config.banks[0].bank_number, 0);
}

#[test]
fn test_mapper_serialize() {
    let mut mapper = MockMapper::new();
    mapper.write_register(0x8000, 3);

    let data = mapper.serialize_mapper().unwrap();
    assert_eq!(data, vec![3]);

    let mut mapper2 = MockMapper::new();
    mapper2.deserialize_mapper(&data).unwrap();

    let config = mapper2.get_bank_config();
    assert_eq!(config.banks[0].bank_number, 3);
}
