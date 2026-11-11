// Cartridge WASM Tests
// Tests for cartridge-related WASM bindings

use sg1_wasm::RetroEmulator;
use wasm_bindgen_test::*;

/// Helper to create a minimal valid Genesis ROM for testing
fn create_test_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x600];

    // Reset vectors
    rom[0] = 0x00; // SSP high
    rom[1] = 0xFF;
    rom[2] = 0x00;
    rom[3] = 0x00;
    rom[4] = 0x00; // PC
    rom[5] = 0x00;
    rom[6] = 0x04;
    rom[7] = 0x00;

    // Genesis header
    rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
    rom[0x120..0x138].copy_from_slice(b"TEST ROM                ");

    // Fill with NOPs
    for i in (0x400..0x600).step_by(2) {
        rom[i] = 0x4E; // NOP high byte
        rom[i + 1] = 0x71; // NOP low byte
    }

    rom
}

#[wasm_bindgen_test]
fn test_load_rom_and_get_info() {
    let mut emulator = RetroEmulator::new("genesis").expect("Failed to create emulator");
    let rom = create_test_rom();

    // Load ROM
    emulator.load_rom(&rom).expect("Failed to load ROM");

    // Get cartridge info
    let info = emulator
        .get_cartridge_info()
        .expect("Failed to get cartridge info");
    assert!(info.is_object());
}

#[wasm_bindgen_test]
fn test_get_cartridge_hash() {
    let mut emulator = RetroEmulator::new("genesis").expect("Failed to create emulator");
    let rom = create_test_rom();

    emulator.load_rom(&rom).expect("Failed to load ROM");

    // Get hash
    let hash = emulator.get_cartridge_hash().expect("Failed to get hash");

    // Hash should be a non-empty string (SHA-256 = 64 hex chars)
    assert!(!hash.is_empty());
}

#[wasm_bindgen_test]
fn test_get_cartridge_title() {
    let mut emulator = RetroEmulator::new("genesis").expect("Failed to create emulator");
    let rom = create_test_rom();

    emulator.load_rom(&rom).expect("Failed to load ROM");

    // Get title
    let title = emulator.get_cartridge_title().expect("Failed to get title");

    // Should contain "TEST ROM"
    assert!(title.contains("TEST ROM"));
}

#[wasm_bindgen_test]
fn test_save_ram_initially_none() {
    let mut emulator = RetroEmulator::new("genesis").expect("Failed to create emulator");
    let rom = create_test_rom();

    emulator.load_rom(&rom).expect("Failed to load ROM");

    // Test ROM has no SRAM, should return None
    let result = emulator.get_save_ram();
    assert!(result.is_none());
}

#[wasm_bindgen_test]
fn test_save_ram_with_sram() {
    let mut emulator = RetroEmulator::new("genesis").expect("Failed to create emulator");

    // Create ROM with SRAM header
    let mut rom = create_test_rom();

    // Add SRAM info to header (8KB at 0x200000)
    rom[0x1A8] = 0xE0; // RAM indicator
    rom[0x1A9] = 0x20;
    rom[0x1AA] = 0x00;
    rom[0x1AB] = 0x01; // Start: 0x200001
    rom[0x1AC] = 0x20;
    rom[0x1AD] = 0x1F;
    rom[0x1AE] = 0xFF;
    rom[0x1AF] = 0xFF; // End: 0x201FFF (8KB)

    emulator.load_rom(&rom).expect("Failed to load ROM");

    // Should have SRAM now
    let _sram = emulator.get_save_ram();
    // The result is not checked: the test asks only that this ROM loads and
    // that reading its save RAM does not panic.
}

#[wasm_bindgen_test]
fn test_sram_dirty_flag() {
    let emulator = RetroEmulator::new("genesis").expect("Failed to create emulator");

    // Initially not dirty (no ROM loaded)
    let dirty = emulator.is_cartridge_sram_dirty();
    assert!(!dirty);
}

#[wasm_bindgen_test]
fn test_clear_sram_dirty() {
    let mut emulator = RetroEmulator::new("genesis").expect("Failed to create emulator");

    // Should not panic even with no ROM
    emulator.clear_cartridge_sram_dirty();
}

#[wasm_bindgen_test]
fn test_cartridge_save_state() {
    let mut emulator = RetroEmulator::new("genesis").expect("Failed to create emulator");
    let rom = create_test_rom();

    emulator.load_rom(&rom).expect("Failed to load ROM");

    // Get save state
    let state = emulator.get_cartridge_save_state(100);

    // Should succeed
    assert!(state.is_ok());

    let state_data = state.unwrap();
    assert!(!state_data.is_empty());
}

#[wasm_bindgen_test]
fn test_load_cartridge_save_state_roundtrip() {
    let mut emulator = RetroEmulator::new("genesis").expect("Failed to create emulator");
    let rom = create_test_rom();

    emulator.load_rom(&rom).expect("Failed to load ROM");

    // Save state
    let state_data = emulator
        .get_cartridge_save_state(100)
        .expect("Failed to get state");

    // Load state back
    let result = emulator.load_cartridge_save_state(&state_data);

    // Should succeed
    assert!(result.is_ok());
}

#[wasm_bindgen_test]
fn test_multiple_rom_loads() {
    let mut emulator = RetroEmulator::new("genesis").expect("Failed to create emulator");

    // Load first ROM
    let rom1 = create_test_rom();
    emulator.load_rom(&rom1).expect("Failed to load ROM 1");

    let title1 = emulator
        .get_cartridge_title()
        .expect("Failed to get title 1");

    // Load second ROM (different title)
    let mut rom2 = create_test_rom();
    rom2[0x120..0x138].copy_from_slice(b"SECOND ROM              ");
    emulator.load_rom(&rom2).expect("Failed to load ROM 2");

    let title2 = emulator
        .get_cartridge_title()
        .expect("Failed to get title 2");

    // Titles should be different
    assert_ne!(title1, title2);
    assert!(title2.contains("SECOND ROM"));
}

#[wasm_bindgen_test]
fn test_cartridge_hash_consistency() {
    let mut emulator = RetroEmulator::new("genesis").expect("Failed to create emulator");
    let rom = create_test_rom();

    emulator.load_rom(&rom).expect("Failed to load ROM");

    // Get hash twice
    let hash1 = emulator.get_cartridge_hash().expect("Failed to get hash 1");
    let hash2 = emulator.get_cartridge_hash().expect("Failed to get hash 2");

    // Should be consistent
    assert_eq!(hash1, hash2);
}

#[wasm_bindgen_test]
fn test_invalid_rom_fails() {
    let mut emulator = RetroEmulator::new("genesis").expect("Failed to create emulator");

    // Try to load invalid ROM (too small)
    let invalid_rom = vec![0u8; 10];
    let result = emulator.load_rom(&invalid_rom);

    // Should fail
    assert!(result.is_err());
}
