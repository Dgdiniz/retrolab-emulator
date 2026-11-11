//! Cartridge detection system
//!
//! Provides intelligent ROM detection using multiple strategies:
//! - Header parsing (console-specific)
//! - SHA-256 hash database lookup
//! - JSON manifest files
//! - Heuristic detection

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

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
// ROM Database
// ============================================================================

/// ROM database entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RomDatabaseEntry {
    /// SHA-256 hash of ROM
    pub hash: String,

    /// Title
    pub title: String,

    /// Serial number
    pub serial: Option<String>,

    /// Regions
    pub regions: Vec<Region>,

    /// Mapper
    pub mapper: MapperId,

    /// Expansion chip
    pub expansion_chip: Option<ExpansionChipType>,

    /// SRAM size
    pub sram_size: usize,

    /// SRAM type
    pub sram_type: SRAMType,

    /// Notes
    pub notes: Option<String>,

    /// Fixes needed
    pub fixes: Option<Vec<RomFix>>,
}

/// ROM fix/patch
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RomFix {
    /// Patch a byte at offset
    PatchByte { offset: usize, value: u8 },

    /// Fix checksum
    FixChecksum,

    /// Trim overdump
    TrimOverdump { new_size: usize },
}

/// ROM database
pub struct RomDatabase {
    entries: HashMap<String, RomDatabaseEntry>,
}

impl RomDatabase {
    /// Create empty database
    pub fn new() -> Self {
        RomDatabase {
            entries: HashMap::new(),
        }
    }

    /// Load database from embedded JSON
    pub fn load_embedded() -> Self {
        // For now, return empty database
        // TODO: Load from embedded JSON file
        RomDatabase::new()
    }

    /// Load database from JSON string
    pub fn from_json(json: &str) -> Result<Self, String> {
        let entries: Vec<RomDatabaseEntry> = serde_json::from_str(json)
            .map_err(|e| format!("Failed to parse database JSON: {}", e))?;

        let mut map = HashMap::new();
        for entry in entries {
            map.insert(entry.hash.clone(), entry);
        }

        Ok(RomDatabase { entries: map })
    }

    /// Add entry to database
    pub fn add_entry(&mut self, entry: RomDatabaseEntry) {
        self.entries.insert(entry.hash.clone(), entry);
    }

    /// Lookup ROM by hash
    pub fn lookup(&self, hash: &str) -> Option<&RomDatabaseEntry> {
        self.entries.get(hash)
    }

    /// Get number of entries
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if database is empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for RomDatabase {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Manifest
// ============================================================================

/// Manifest file structure (JSON)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Format identifier
    pub format: String,

    /// Version
    pub version: String,

    /// Console system
    pub system: String,

    /// Title
    pub title: String,

    /// Serial number
    pub serial: Option<String>,

    /// Regions
    pub regions: Vec<String>,

    /// Mapper configuration
    pub mapper: Option<ManifestMapper>,

    /// SRAM configuration
    pub sram: Option<ManifestSram>,

    /// Expansion chip configuration
    pub expansion_chip: Option<ManifestExpansionChip>,

    /// Notes
    pub notes: Option<String>,

    /// Overrides
    pub overrides: Option<ManifestOverrides>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestMapper {
    #[serde(rename = "type")]
    pub mapper_type: String,
    pub banks: Option<u32>,
    pub bank_size: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestSram {
    pub size: String,
    #[serde(rename = "type")]
    pub sram_type: String,
    pub address_range: Option<Vec<String>>,
    pub odd_byte: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestExpansionChip {
    #[serde(rename = "type")]
    pub chip_type: String,
    pub rom_start: Option<String>,
    pub rom_size: Option<String>,
    pub chip_ram_size: Option<String>,
    pub chip_rom_size: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestOverrides {
    pub force_detection: Option<bool>,
    pub ignore_header: Option<bool>,
}

impl Manifest {
    /// Load manifest from JSON string
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| format!("Failed to parse manifest: {}", e))
    }

    /// Convert to JSON string
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| format!("Failed to serialize manifest: {}", e))
    }
}

// ============================================================================
// Hash Utilities
// ============================================================================

/// Calculate SHA-256 hash of ROM data
pub fn calculate_rom_hash(rom_data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(rom_data);
    let result = hasher.finalize();
    format!("{:x}", result)
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
    fn test_calculate_rom_hash() {
        let data = b"test data";
        let hash = calculate_rom_hash(data);

        // SHA-256 of "test data"
        assert_eq!(hash.len(), 64); // SHA-256 is 64 hex chars
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_rom_database() {
        let mut db = RomDatabase::new();
        assert!(db.is_empty());

        let entry = RomDatabaseEntry {
            hash: "test_hash".to_string(),
            title: "Test Game".to_string(),
            serial: Some("TEST-001".to_string()),
            regions: vec![Region::USA],
            mapper: MapperId::GenesisNone,
            expansion_chip: None,
            sram_size: 0,
            sram_type: SRAMType::None,
            notes: None,
            fixes: None,
        };

        db.add_entry(entry.clone());
        assert_eq!(db.len(), 1);

        let found = db.lookup("test_hash");
        assert!(found.is_some());
        assert_eq!(found.unwrap().title, "Test Game");
    }

    #[test]
    fn test_manifest_json() {
        let json = r#"{
            "format": "retro-manifest",
            "version": "1.0",
            "system": "genesis",
            "title": "Test Game",
            "serial": null,
            "regions": ["USA"],
            "mapper": null,
            "sram": null,
            "expansion_chip": null,
            "notes": null,
            "overrides": null
        }"#;

        let manifest = Manifest::from_json(json).unwrap();
        assert_eq!(manifest.system, "genesis");
        assert_eq!(manifest.title, "Test Game");
    }
}
