//! Mini Cartridge Builder: a helper that builds test cartridges
//!
//! Shared by several independent test binaries, each of which compiles this
//! module separately and uses a different subset of it, so per-binary
//! dead-code lints fire on items a sibling binary depends on.
#![allow(dead_code)]
// Helper that builds test cartridges programmatically

/// Builder for Genesis test ROMs
pub struct MiniCartridgeBuilder {
    rom_data: Vec<u8>,
    title: String,
    sram_size: usize,
    sram_start: u32,
    mapper_type: Option<String>,
    expansion_chip: Option<String>,
}

impl MiniCartridgeBuilder {
    /// Create a new builder with minimum size for Genesis header
    pub fn new() -> Self {
        MiniCartridgeBuilder {
            rom_data: vec![0xFF; 0x200], // Minimum for header (512 bytes)
            title: "TEST ROM".to_string(),
            sram_size: 0,
            sram_start: 0x200000,
            mapper_type: None,
            expansion_chip: None,
        }
    }

    /// Set ROM size
    pub fn with_size(mut self, size: usize) -> Self {
        if size < 0x200 {
            panic!("ROM size must be at least 512 bytes for Genesis header");
        }
        self.rom_data.resize(size, 0xFF);
        self
    }

    /// Set ROM title (max 48 chars)
    pub fn with_title(mut self, title: &str) -> Self {
        self.title = title.to_string();
        self
    }

    /// Add SRAM support
    pub fn with_sram(mut self, size: usize) -> Self {
        self.sram_size = size;
        self
    }

    /// Set SRAM start address (default: 0x200000)
    pub fn with_sram_start(mut self, addr: u32) -> Self {
        self.sram_start = addr;
        self
    }

    /// Set mapper type (for future implementation)
    pub fn with_mapper(mut self, mapper: &str) -> Self {
        self.mapper_type = Some(mapper.to_string());
        self
    }

    /// Set expansion chip (for future implementation)
    pub fn with_chip(mut self, chip: &str) -> Self {
        self.expansion_chip = Some(chip.to_string());
        self
    }

    /// Insert code at specific offset
    pub fn with_code_at(mut self, offset: usize, code: &[u8]) -> Self {
        if offset + code.len() > self.rom_data.len() {
            self.rom_data.resize(offset + code.len(), 0xFF);
        }
        self.rom_data[offset..offset + code.len()].copy_from_slice(code);
        self
    }

    /// Insert 68000 instruction at offset
    pub fn with_instruction(self, offset: usize, opcode: u16) -> Self {
        let bytes = opcode.to_be_bytes();
        self.with_code_at(offset, &bytes)
    }

    /// Add multiple instructions starting at offset
    pub fn with_instructions(mut self, offset: usize, opcodes: &[u16]) -> Self {
        let mut current_offset = offset;
        for &opcode in opcodes {
            self = self.with_instruction(current_offset, opcode);
            current_offset += 2;
        }
        self
    }

    /// Build the ROM with valid Genesis header
    pub fn build(mut self) -> Vec<u8> {
        self.build_genesis_header();
        self.rom_data
    }

    /// Generate valid Genesis header
    fn build_genesis_header(&mut self) {
        // Calculate values before borrowing
        let checksum = self.calculate_checksum();
        let rom_end = (self.rom_data.len() - 1) as u32;
        let title_bytes = format!("{:<48}", self.title);

        // Set reset vectors (0x000-0x007)
        // Initial SSP (Stack Pointer) at 0x000-0x003
        self.rom_data[0x000..0x004].copy_from_slice(&0x00FFFFFE_u32.to_be_bytes());
        // Initial PC (Program Counter) at 0x004-0x007
        // Default to 0x000400 (after interrupt vectors and header)
        self.rom_data[0x004..0x008].copy_from_slice(&0x00000400_u32.to_be_bytes());

        let header = &mut self.rom_data[0x100..0x200];

        // Console name (0x100-0x10F)
        header[0x00..0x10].copy_from_slice(b"SEGA MEGA DRIVE ");

        // Copyright (0x110-0x11F)
        header[0x10..0x20].copy_from_slice(b"(C)TEST 2025.JAN");

        // Domestic title (0x120-0x14F) - 48 bytes
        header[0x20..0x50].copy_from_slice(title_bytes.as_bytes());

        // Overseas title (0x150-0x17F) - 48 bytes, same as domestic
        header[0x50..0x80].copy_from_slice(title_bytes.as_bytes());

        // Serial (0x180-0x18D)
        header[0x80..0x8E].copy_from_slice(b"GM T-00000-00 ");

        // Checksum (0x18E-0x18F)
        header[0x8E..0x90].copy_from_slice(&checksum.to_be_bytes());

        // I/O support (0x190-0x19F)
        header[0x90..0xA0].copy_from_slice(b"J               ");

        // ROM address range (0x1A0-0x1A7)
        header[0xA0..0xA4].copy_from_slice(&0x00000000u32.to_be_bytes());
        header[0xA4..0xA8].copy_from_slice(&rom_end.to_be_bytes());

        // RAM address range (0x1A8-0x1AF)
        if self.sram_size > 0 {
            header[0xA8] = 0xE0; // RAM indicator + backup

            // With odd-byte addressing, physical SRAM size needs 2x address space
            // (only odd addresses are valid, so each byte takes 2 addresses)
            let sram_start_odd = self.sram_start | 1;
            let sram_end = sram_start_odd + ((self.sram_size as u32) * 2) - 1;

            // Start address (odd byte aligned for Genesis)
            let start_bytes = sram_start_odd.to_be_bytes();
            header[0xA9..0xAC].copy_from_slice(&start_bytes[1..4]);

            // End address
            header[0xAC..0xB0].copy_from_slice(&sram_end.to_be_bytes());
        } else {
            header[0xA8..0xB0].copy_from_slice(&[0; 8]);
        }

        // Modem support (0x1B0-0x1BF)
        header[0xB0..0xC0].copy_from_slice(&[0x20; 16]);

        // Notes (0x1C0-0x1CF)
        header[0xC0..0xD0].copy_from_slice(&[0x20; 16]);

        // Regions (0x1F0-0x1F2)
        header[0xF0..0xF3].copy_from_slice(b"JUE");

        // Reserved (0x1F3-0x1FF) - fill with spaces
        header[0xF3..0x100].fill(0x20);
    }

    /// Calculate ROM checksum (simple sum)
    fn calculate_checksum(&self) -> u16 {
        let mut sum: u32 = 0;

        // Sum all words after 0x200 (skip header)
        for i in (0x200..self.rom_data.len()).step_by(2) {
            if i + 1 < self.rom_data.len() {
                let word = u16::from_be_bytes([self.rom_data[i], self.rom_data[i + 1]]);
                sum = sum.wrapping_add(word as u32);
            }
        }

        (sum & 0xFFFF) as u16
    }
}

impl Default for MiniCartridgeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_basic() {
        let rom = MiniCartridgeBuilder::new()
            .with_size(0x80000) // 512KB
            .with_title("Test Cartridge")
            .build();

        assert_eq!(rom.len(), 0x80000);

        // Check header
        assert_eq!(&rom[0x100..0x110], b"SEGA MEGA DRIVE ");

        // Check title
        let title_bytes = &rom[0x120..0x150];
        let title = String::from_utf8_lossy(title_bytes);
        assert!(title.starts_with("Test Cartridge"));
    }

    #[test]
    fn test_builder_with_sram() {
        let rom = MiniCartridgeBuilder::new()
            .with_size(0x80000)
            .with_sram(0x2000) // 8KB
            .build();

        // Check SRAM indicator
        assert_eq!(rom[0x1A8], 0xE0);

        // Check SRAM addresses
        let sram_start = u32::from_be_bytes([0, rom[0x1A9], rom[0x1AA], rom[0x1AB]]);
        assert_eq!(sram_start, 0x200001); // Odd byte aligned
    }

    #[test]
    fn test_builder_with_code() {
        let rom = MiniCartridgeBuilder::new()
            .with_size(0x400)
            .with_instruction(0x400, 0x4E71) // NOP
            .with_instruction(0x402, 0x4E75) // RTS
            .build();

        assert_eq!(rom[0x400], 0x4E);
        assert_eq!(rom[0x401], 0x71);
        assert_eq!(rom[0x402], 0x4E);
        assert_eq!(rom[0x403], 0x75);
    }

    #[test]
    fn test_builder_with_instructions() {
        let opcodes = vec![
            0x4E71, // NOP
            0x4E71, // NOP
            0x4E75, // RTS
        ];

        let rom = MiniCartridgeBuilder::new()
            .with_size(0x408)
            .with_instructions(0x400, &opcodes)
            .build();

        assert_eq!(u16::from_be_bytes([rom[0x400], rom[0x401]]), 0x4E71);
        assert_eq!(u16::from_be_bytes([rom[0x402], rom[0x403]]), 0x4E71);
        assert_eq!(u16::from_be_bytes([rom[0x404], rom[0x405]]), 0x4E75);
    }

    #[test]
    fn test_checksum_calculation() {
        let rom = MiniCartridgeBuilder::new().with_size(0x400).build();

        // Checksum is at 0x18E-0x18F
        let checksum = u16::from_be_bytes([rom[0x18E], rom[0x18F]]);

        // Printed, not checked: the 16-bit sum of the words from 0x200 on,
        // which depends on the ROM's content
        println!("Checksum: 0x{:04X}", checksum);
    }
}
