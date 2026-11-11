// Cartridge Integration Tests
// Tests for cartridge loading, SRAM, ROM mirroring, and integration
//
// NOTE: These tests use the MiniCartridgeBuilder to create test ROMs
// and validate the cartridge system via the GenesisEmulator.
// Drafts of bank-mapper, EEPROM and J-Cart tests are commented out at the end.

#[path = "helpers/mod.rs"]
mod helpers;

use helpers::MiniCartridgeBuilder;
use retro_core::EmulatorCore;
use sg1_core::system::GenesisEmulator;

#[test]
fn test_load_standard_rom() {
    let rom = MiniCartridgeBuilder::new()
        .with_size(0x80000) // 512KB
        .with_title("Standard ROM Test")
        .build();

    let mut emulator = GenesisEmulator::new();
    let result = emulator.load_rom(&rom);

    assert!(
        result.is_ok(),
        "Failed to load standard ROM: {:?}",
        result.err()
    );

    // Verify ROM info
    let rom_info = emulator.get_rom_info();
    assert!(rom_info.is_some(), "ROM info should be available");

    if let Some(info) = rom_info {
        assert!(info.title.contains("Standard ROM Test"));
    }
}

#[test]
fn test_load_rom_read_different_addresses() {
    // Create ROM with known data at different addresses
    let rom = MiniCartridgeBuilder::new()
        .with_size(0x100000) // 1MB
        .with_title("Address Test")
        .with_code_at(0x1000, &[0xAA, 0xBB, 0xCC, 0xDD])
        .with_code_at(0x50000, &[0x11, 0x22, 0x33, 0x44])
        .with_code_at(0xF0000, &[0xEE, 0xFF, 0x00, 0x11])
        .build();

    let mut emulator = GenesisEmulator::new();
    emulator.load_rom(&rom).unwrap();

    // Read from different addresses
    let val1 = emulator.read_memory_debug(0x1000, 1)[0];
    let val2 = emulator.read_memory_debug(0x50000, 1)[0];
    let val3 = emulator.read_memory_debug(0xF0000, 1)[0];

    assert_eq!(val1, 0xAA);
    assert_eq!(val2, 0x11);
    assert_eq!(val3, 0xEE);
}

#[test]
fn test_rom_write_protected() {
    let rom = MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("Write Protection Test")
        .build();

    let mut emulator = GenesisEmulator::new();
    emulator.load_rom(&rom).unwrap();

    // Try to write to ROM space
    let original_value = emulator.read_memory_debug(0x100, 1)[0];
    emulator.write_memory_debug(0x100, &[0xFF]);
    let after_write = emulator.read_memory_debug(0x100, 1)[0];

    // ROM should not change
    assert_eq!(original_value, after_write, "ROM should be write-protected");
}

#[test]
fn test_sram_roundtrip() {
    let rom = MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("SRAM Test")
        .with_sram(0x2000) // 8KB SRAM
        .build();

    let mut emulator = GenesisEmulator::new();
    emulator.load_rom(&rom).unwrap();

    // Write to SRAM (Genesis uses odd-byte addressing)
    emulator.write_memory_debug(0x200001, &[0x42]); // Odd address
    emulator.write_memory_debug(0x200003, &[0x43]); // Odd address
    emulator.write_memory_debug(0x201FFF, &[0x99]); // Odd address

    // Read back
    assert_eq!(emulator.read_memory_debug(0x200001, 1)[0], 0x42);
    assert_eq!(emulator.read_memory_debug(0x200003, 1)[0], 0x43);
    assert_eq!(emulator.read_memory_debug(0x201FFF, 1)[0], 0x99);

    // Get SRAM data
    let sram = emulator.get_save_ram();
    assert!(sram.is_some(), "SRAM should be available");

    if let Some(sram_data) = sram {
        // The SRAM is stored as a flat buffer covering the full address
        // range (as Genesis Plus GX's save files are), so the physical offset is just
        // `addr - sram_start` where sram_start is aligned to the even
        // boundary 0x200000. There is no parity compression.
        assert_eq!(sram_data[0x0001], 0x42); // addr 0x200001
        assert_eq!(sram_data[0x0003], 0x43); // addr 0x200003
        assert_eq!(sram_data[0x1FFF], 0x99); // addr 0x201FFF
    }
}

#[test]
fn test_sram_get_set_roundtrip() {
    let rom = MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("SRAM Persistence Test")
        .with_sram(0x1000) // 4KB
        .build();

    let mut emulator = GenesisEmulator::new();
    emulator.load_rom(&rom).unwrap();

    // Write some data (odd-byte addressing)
    emulator.write_memory_debug(0x200001, &[0xAA]);
    emulator.write_memory_debug(0x200101, &[0xBB]);

    // Get SRAM
    let sram1 = emulator.get_save_ram().unwrap().to_vec();

    // Create new emulator and load same ROM
    let mut emulator2 = GenesisEmulator::new();
    emulator2.load_rom(&rom).unwrap();

    // Set SRAM from first emulator
    emulator2.set_save_ram(&sram1);

    // Verify data was restored (odd-byte addressing)
    assert_eq!(emulator2.read_memory_debug(0x200001, 1)[0], 0xAA);
    assert_eq!(emulator2.read_memory_debug(0x200101, 1)[0], 0xBB);
}

#[test]
fn test_rom_mirroring() {
    // Create small ROM (256KB)
    let rom = MiniCartridgeBuilder::new()
        .with_size(0x40000) // 256KB
        .with_title("Mirror Test")
        .with_code_at(0x1000, &[0xDE, 0xAD, 0xBE, 0xEF])
        .build();

    let mut emulator = GenesisEmulator::new();
    emulator.load_rom(&rom).unwrap();

    // Read from original location
    let val1 = emulator.read_memory_debug(0x1000, 1)[0];

    // Read from mirrored location (256KB later)
    let val2 = emulator.read_memory_debug(0x1000 + 0x40000, 1)[0];

    // Should be the same due to mirroring
    assert_eq!(val1, val2, "ROM should be mirrored");
    assert_eq!(val1, 0xDE);
}

#[test]
fn test_multiple_rom_loads() {
    let mut emulator = GenesisEmulator::new();

    // Load first ROM
    let rom1 = MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("First ROM")
        .build();

    emulator.load_rom(&rom1).unwrap();
    let info1 = emulator.get_rom_info().unwrap();
    assert!(info1.title.contains("First ROM"));

    // Load second ROM (should replace first)
    let rom2 = MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("Second ROM")
        .build();

    emulator.load_rom(&rom2).unwrap();
    let info2 = emulator.get_rom_info().unwrap();
    assert!(info2.title.contains("Second ROM"));
}

#[test]
fn test_rom_with_executable_code() {
    // Create ROM with actual 68000 instructions
    let rom = MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("Executable Test")
        .with_instructions(
            0x400,
            &[
                0x4E71, // NOP
                0x7001, // MOVEQ #1, D0
                0x7202, // MOVEQ #2, D1
                0xD240, // ADD.W D0, D1
                0x4E75, // RTS
            ],
        )
        .build();

    let mut emulator = GenesisEmulator::new();
    emulator.load_rom(&rom).unwrap();
    emulator.reset();

    // Execute a few instructions
    for _ in 0..5 {
        emulator.step();
    }

    // Nothing is checked: the five steps only have to run (D1 would be 3
    // after the ADD).
}

#[test]
fn test_save_state_with_cartridge() {
    let rom = MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("Save State Test")
        .with_sram(0x1000)
        .build();

    let mut emulator = GenesisEmulator::new();
    emulator.load_rom(&rom).unwrap();

    // Write to SRAM (odd-byte addressing)
    emulator.write_memory_debug(0x200001, &[0x42]);

    // Get cartridge save state
    let save_state = emulator.get_cartridge_save_state(100);
    assert!(save_state.is_ok(), "Failed to get save state");

    // Create new emulator and load same ROM
    let mut emulator2 = GenesisEmulator::new();
    emulator2.load_rom(&rom).unwrap();

    // Load save state
    let result = emulator2.load_cartridge_save_state(&save_state.unwrap());
    assert!(result.is_ok(), "Failed to load save state");

    // Verify SRAM was restored (odd-byte addressing)
    assert_eq!(emulator2.read_memory_debug(0x200001, 1)[0], 0x42);
}

#[test]
fn test_invalid_rom_rejected() {
    let mut emulator = GenesisEmulator::new();

    // Try to load invalid ROM (too small)
    let invalid_rom = vec![0u8; 10];
    let result = emulator.load_rom(&invalid_rom);

    assert!(result.is_err(), "Invalid ROM should be rejected");
}

#[test]
fn test_rom_too_small() {
    let mut emu = GenesisEmulator::new();
    let small_rom = vec![0u8; 100]; // Too small

    let result = emu.load_rom(&small_rom);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("too small"));
}

#[test]
fn test_rom_hash_consistency() {
    let rom = MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("Hash Test")
        .build();

    let mut emulator = GenesisEmulator::new();
    emulator.load_rom(&rom).unwrap();

    let hash1 = emulator.get_cartridge_hash();
    let hash2 = emulator.get_cartridge_hash();

    assert!(hash1.is_some());
    assert!(hash2.is_some());
    assert_eq!(hash1, hash2, "Hash should be consistent");
}

#[test]
fn test_sram_dirty_flag() {
    let rom = MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("Dirty Flag Test")
        .with_sram(0x1000)
        .build();

    let mut emulator = GenesisEmulator::new();
    emulator.load_rom(&rom).unwrap();

    // Initially not dirty
    assert!(!emulator.is_cartridge_sram_dirty());

    // Write to SRAM (odd-byte addressing)
    emulator.write_memory_debug(0x200001, &[0x42]);

    // Should be dirty now
    assert!(emulator.is_cartridge_sram_dirty());

    // Clear dirty flag
    emulator.clear_cartridge_sram_dirty();

    // Should not be dirty anymore
    assert!(!emulator.is_cartridge_sram_dirty());
}

// TODO: tests through GenesisEmulator for the bank mapper, the EEPROM and the J-Cart.

/*
#[test]
fn test_sega_mapper_bank_switching() {
    let rom = MiniCartridgeBuilder::new()
        .with_size(0x400000) // 4MB (requires mapper)
        .with_title("SEGA Mapper Test")
        .with_mapper("SEGA")
        .with_code_at(0x000000, &[0xAA])  // Bank 0
        .with_code_at(0x080000, &[0xBB])  // Bank 1
        .with_code_at(0x180000, &[0xCC])  // Bank 3
        .build();

    let mut emulator = GenesisEmulator::new();
    emulator.load_rom(&rom).unwrap();

    // Initially bank 0 should be mapped
    assert_eq!(emulator.read_memory(0x000000), 0xAA);

    // Switch bank 0 to point to bank 3
    emulator.write_memory(0xA130F0, 3);

    // Now reading from 0x000000 should give data from bank 3
    assert_eq!(emulator.read_memory(0x000000), 0xCC);
}

#[test]
fn test_eeprom_cartridge() {
    let rom = MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("EEPROM Test")
        .with_chip("EEPROM")
        .build();

    let mut emulator = GenesisEmulator::new();
    emulator.load_rom(&rom).unwrap();

    // TODO: Test EEPROM serial protocol
}

#[test]
fn test_jcart_io() {
    let rom = MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("J-Cart Test")
        .with_chip("JCart")
        .build();

    let mut emulator = GenesisEmulator::new();
    emulator.load_rom(&rom).unwrap();

    // TODO: Test extra I/O ports (0x38FFFE, 0x3FFFFE)
}
*/
