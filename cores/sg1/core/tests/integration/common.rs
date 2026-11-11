// Helpers shared by all the integration tests

use sg1_core::bus::AddressBus;
use sg1_core::cartridge::composable::build_genesis_cartridge;
use sg1_core::cpu68k::M68k;

/// Creates a CPU and a bus with a ROM
pub fn setup_cpu_with_rom(rom: &[u8]) -> (M68k, AddressBus) {
    let mut cpu = M68k::new();
    let mut bus = AddressBus::new();

    // Build a full ROM with reset vectors and a valid Genesis header
    let mut full_rom = vec![0u8; (0x400 + rom.len()).max(512)];

    // Reset vectors at 0x000000
    full_rom[0] = 0x00;
    full_rom[1] = 0x00;
    full_rom[2] = 0xFF;
    full_rom[3] = 0x00; // SSP = 0x0000FF00 (default)
    full_rom[4] = 0x00;
    full_rom[5] = 0x00;
    full_rom[6] = 0x04;
    full_rom[7] = 0x00; // PC = 0x00000400 (default)

    // A valid Genesis header, for the factory function
    full_rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
    full_rom[0x120..0x138].copy_from_slice(b"TEST ROM                ");

    // The tests, though, run from PC=0 directly: copy the ROM to offset 0,
    // over the vectors above as far as it reaches (the tests set PC themselves)
    for (i, &byte) in rom.iter().enumerate() {
        full_rom[i] = byte;
    }

    // Build the cartridge with the factory function
    let cartridge = build_genesis_cartridge(full_rom).expect("Failed to create test cartridge");
    bus.set_cartridge(cartridge);

    cpu.reset(&mut bus);

    (cpu, bus)
}
