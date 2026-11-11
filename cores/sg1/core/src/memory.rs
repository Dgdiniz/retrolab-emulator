//! Genesis memory system.
//!
//! The Genesis has a complex memory map with multiple regions:
//! - Cartridge ROM (up to 4MB)
//! - Work RAM (64KB)
//! - Z80 RAM (8KB)
//! - VDP registers and memory
//! - I/O ports
//!
//! Memory Map:
//! ```text
//! 0x000000-0x3FFFFF: Cartridge ROM (4MB)
//! 0x400000-0x7FFFFF: Reserved for cartridge (mappers)
//! 0x800000-0x9FFFFF: Reserved
//! 0xA00000-0xA0FFFF: Z80 address space
//! 0xA10000-0xA10FFF: I/O area
//! 0xA11000-0xA11FFF: Control area
//! 0xA12000-0xBFFFFF: Reserved
//! 0xC00000-0xC0001F: VDP ports
//! 0xC00020-0xDFFFFF: VDP (mirrors)
//! 0xE00000-0xFFFFFF: Work RAM (64KB, mirrored)
//! ```

use retro_core::{cpu::Addressable, memory::Ram, MemoryRegion, MemoryRegionInfo};

/// Genesis memory system.
pub struct GenesisMemory {
    /// Work RAM (64KB)
    pub work_ram: Ram,

    /// Z80 RAM (8KB)
    pub z80_ram: Ram,
}

impl GenesisMemory {
    /// Create a new Genesis memory system.
    pub fn new() -> Self {
        Self {
            work_ram: Ram::new(0x10000), // 64KB
            z80_ram: Ram::new(0x2000),   // 8KB
        }
    }

    /// Get memory region information for debugging.
    pub fn get_regions(&self) -> Vec<MemoryRegionInfo> {
        vec![
            MemoryRegionInfo {
                region: MemoryRegion::MainRam,
                name: "Work RAM".to_string(),
                start_address: 0xE00000,
                size: 0x10000,
                writable: true,
            },
            MemoryRegionInfo {
                region: MemoryRegion::AudioRam,
                name: "Z80 RAM".to_string(),
                start_address: 0xA00000,
                size: 0x2000,
                writable: true,
            },
        ]
    }

    /// Read a byte from work RAM at the given offset.
    /// Used by Z80 bus for bank switching reads into 68K address space.
    pub fn read_work_ram(&mut self, offset: usize) -> u8 {
        self.work_ram.read(offset as u32)
    }

    /// Write a byte to work RAM at the given offset.
    /// Used by Z80 bus for bank switching writes into 68K address space.
    pub fn write_work_ram(&mut self, offset: usize, value: u8) {
        self.work_ram.write(offset as u32, value);
    }

    /// Load ROM - kept for compatibility (no-op, the cartridge handles the ROM)
    #[deprecated(note = "ROM is handled by the cartridge system")]
    #[allow(dead_code)]
    pub fn load_rom(&mut self, _data: Vec<u8>) {
        // No-op: ROM is handled by the cartridge system in AddressBus
    }

    /// Get ROM - kept for compatibility (returns empty vec)
    #[deprecated(note = "ROM is handled by the cartridge system")]
    #[allow(dead_code)]
    pub fn rom(&self) -> Vec<u8> {
        Vec::new()
    }
}

impl Default for GenesisMemory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::cpu::Addressable;

    #[test]
    fn test_memory_creation() {
        let mem = GenesisMemory::new();
        assert_eq!(mem.work_ram.len(), 0x10000);
        assert_eq!(mem.z80_ram.len(), 0x2000);
        // ROM is handled by the cartridge system in AddressBus
    }

    #[test]
    fn test_work_ram_access() {
        let mut mem = GenesisMemory::new();
        mem.work_ram.write(0x1000, 0x42);
        assert_eq!(mem.work_ram.read(0x1000), 0x42);
    }

    #[test]
    fn test_z80_ram_access() {
        let mut mem = GenesisMemory::new();
        mem.z80_ram.write(0x100, 0xAB);
        assert_eq!(mem.z80_ram.read(0x100), 0xAB);
    }

    #[test]
    fn test_memory_regions() {
        let mem = GenesisMemory::new();

        let regions = mem.get_regions();
        assert_eq!(regions.len(), 2); // Work RAM, Z80 RAM (ROM is handled by cartridge)

        // Check Work RAM region
        assert_eq!(regions[0].start_address, 0xE00000);
        assert_eq!(regions[0].size, 0x10000);
        assert!(regions[0].writable);

        // Check Z80 RAM region
        assert_eq!(regions[1].start_address, 0xA00000);
        assert_eq!(regions[1].size, 0x2000);
        assert!(regions[1].writable);
    }
}
