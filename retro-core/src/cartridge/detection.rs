//! What a ROM says of itself: the header-parser seam each console
//! implements, the ROM hash, and the heuristic that tells the consoles'
//! images apart.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

use super::*;

// ============================================================================
// Header Parser Trait
// ============================================================================

/// Header parser trait for console-specific ROM headers
pub trait HeaderParser: Send + Sync {
    /// Parse ROM header and extract information
    fn parse(&self, rom_data: &[u8]) -> Result<HeaderInfo, String>;

    /// Validate if this ROM belongs to this console
    fn validate(&self, rom_data: &[u8]) -> bool;

    /// Get console system this parser handles
    fn console_system(&self) -> ConsoleSystem;
}

/// Header information extracted from ROM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderInfo {
    /// ROM title
    pub title: String,

    /// Serial number (if available)
    pub serial: Option<String>,

    /// Region(s)
    pub regions: Vec<Region>,

    /// ROM size (from header, may differ from actual)
    pub rom_size: usize,

    /// SRAM size
    pub sram_size: usize,

    /// SRAM type
    pub sram_type: SRAMType,

    /// Checksum from header
    pub checksum: Option<u16>,

    /// Mapper hint (if determinable from header)
    pub mapper_hint: Option<MapperId>,

    /// Expansion chip hint
    pub expansion_chip_hint: Option<ExpansionChipType>,

    /// I/O support (Genesis-specific)
    pub io_support: Vec<String>,
}

// ============================================================================
// Hash Utilities
// ============================================================================

/// The SHA-256 of `bytes`, in lowercase hex: the hash of a ROM image
/// (`CartridgeInfo::rom_hash`, a manifest's `rom.sha256`, the savestate
/// envelope's check) or of the file a host loaded, as the caller chooses
/// what to hash. It hashes what it is given.
pub fn rom_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(out, "{byte:02x}").expect("writing to a String does not fail");
    }
    out
}

// ============================================================================
// Heuristics
// ============================================================================

/// Detect console system from ROM data using heuristics
pub fn detect_console_system(rom_data: &[u8]) -> Option<ConsoleSystem> {
    if rom_data.len() < 0x200 {
        return None;
    }

    // Check Genesis signature
    if rom_data.len() >= 0x110 {
        let console_name = String::from_utf8_lossy(&rom_data[0x100..0x110]);
        if console_name.contains("SEGA") {
            return Some(ConsoleSystem::Genesis);
        }
    }

    // Check NES signature (iNES header)
    if rom_data.len() >= 4 && &rom_data[0..4] == b"NES\x1A" {
        return Some(ConsoleSystem::NES);
    }

    // Check SNES signature (more complex, check at different offsets)
    // LoROM: 0x7FB0, HiROM: 0xFFB0
    if rom_data.len() >= 0xFFB0 + 32 {
        // Try HiROM
        let checksum_comp = u16::from_le_bytes([rom_data[0xFFDC], rom_data[0xFFDD]]);
        let checksum = u16::from_le_bytes([rom_data[0xFFDE], rom_data[0xFFDF]]);
        if checksum_comp == !checksum {
            return Some(ConsoleSystem::SNES);
        }
    }

    if rom_data.len() >= 0x7FB0 + 32 {
        // Try LoROM
        let checksum_comp = u16::from_le_bytes([rom_data[0x7FDC], rom_data[0x7FDD]]);
        let checksum = u16::from_le_bytes([rom_data[0x7FDE], rom_data[0x7FDF]]);
        if checksum_comp == !checksum {
            return Some(ConsoleSystem::SNES);
        }
    }

    None
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rom_sha256_gives_the_standard_vectors_in_lowercase_hex() {
        assert_eq!(
            rom_sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            rom_sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
