//! Generic save state and SRAM management
//!
//! This module provides generic structures and utilities for:
//! - Save state serialization/deserialization
//! - SRAM auto-save with debouncing
//! - Console-agnostic save state format

use super::*;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Current save state format version
pub const SAVE_STATE_VERSION: u32 = 1;

/// Generic cartridge save state
///
/// This structure serializes the entire state of a cartridge including:
/// - Cartridge metadata (for validation)
/// - SRAM data
/// - Mapper state
/// - Metadata (timestamp, version, etc.)
///
/// Coprocessor state is NOT carried here: composable cartridges snapshot
/// their chips through the console-specific typed `CoprocessorState` path
/// ([`Coprocessor`]), not this generic blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CartridgeSaveState {
    /// Format version for compatibility
    pub version: u32,

    /// Cartridge ROM hash (SHA-256) for validation
    pub rom_hash: String,

    /// Cartridge title
    pub title: String,

    /// SRAM data (if present)
    pub sram: Option<Vec<u8>>,

    /// Mapper state (if present)
    pub mapper_state: Option<Vec<u8>>,

    /// Metadata
    pub metadata: SaveStateMetadata,
}

/// Save state metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveStateMetadata {
    /// Creation timestamp (ISO 8601)
    pub created_at: String,

    /// Emulator version
    pub emulator_version: String,

    /// Frame count at save time
    pub frame_count: u64,

    /// Compressed flag
    pub compressed: bool,

    /// Screenshot data (optional, base64 PNG)
    pub screenshot: Option<String>,
}

impl CartridgeSaveState {
    /// Create a new save state from a cartridge
    pub fn from_cartridge(
        cartridge: &dyn Cartridge,
        frame_count: u64,
        screenshot: Option<String>,
    ) -> Result<Self, String> {
        let info = cartridge.info();

        // Get SRAM
        let sram = cartridge.get_save_ram().map(|s| s.to_vec());

        // Get mapper state
        let mapper_state = if let Some(mapper) = cartridge.mapper() {
            Some(mapper.serialize_mapper()?)
        } else {
            None
        };

        Ok(CartridgeSaveState {
            version: SAVE_STATE_VERSION,
            rom_hash: info.rom_hash.clone(),
            title: info.title.clone(),
            sram,
            mapper_state,
            metadata: SaveStateMetadata {
                created_at: current_timestamp(),
                emulator_version: env!("CARGO_PKG_VERSION").to_string(),
                frame_count,
                compressed: false,
                screenshot,
            },
        })
    }

    /// Apply save state to a cartridge
    pub fn apply_to_cartridge(&self, cartridge: &mut dyn Cartridge) -> Result<(), String> {
        // Validate version
        if self.version != SAVE_STATE_VERSION {
            return Err(format!(
                "Incompatible save state version: {} (expected {})",
                self.version, SAVE_STATE_VERSION
            ));
        }

        // Validate ROM hash
        let info = cartridge.info();
        if self.rom_hash != info.rom_hash {
            return Err("Save state is for a different ROM (hash mismatch)".to_string());
        }

        // Restore SRAM
        if let Some(ref sram) = self.sram {
            cartridge.set_save_ram(sram)?;
        }

        // Restore mapper state
        if let Some(ref mapper_data) = self.mapper_state {
            if let Some(mapper) = cartridge.mapper_mut() {
                mapper.deserialize_mapper(mapper_data)?;
            }
        }

        Ok(())
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        bincode::serialize(self).map_err(|e| e.to_string())
    }

    /// Deserialize from bytes
    pub fn from_bytes(data: &[u8]) -> Result<Self, String> {
        bincode::deserialize(data).map_err(|e| e.to_string())
    }
}

/// SRAM Manager with auto-save and debouncing
///
/// Handles automatic saving of SRAM with debouncing to avoid
/// excessive writes when SRAM is being modified frequently.
pub struct SramManager {
    /// Time since last SRAM modification
    last_dirty_time: Option<Instant>,

    /// Debounce delay (how long to wait after last write before auto-saving)
    debounce_delay: Duration,

    /// Whether auto-save is enabled
    auto_save_enabled: bool,

    /// Pending save flag
    pending_save: bool,
}

impl SramManager {
    /// Create a new SRAM manager
    ///
    /// # Arguments
    /// * `debounce_ms` - Debounce delay in milliseconds (default: 1000)
    pub fn new(debounce_ms: u64) -> Self {
        SramManager {
            last_dirty_time: None,
            debounce_delay: Duration::from_millis(debounce_ms),
            auto_save_enabled: true,
            pending_save: false,
        }
    }

    /// Notify that SRAM was modified
    pub fn mark_dirty(&mut self) {
        self.last_dirty_time = Some(Instant::now());
        self.pending_save = true;
    }

    /// Check if SRAM should be saved now (debounce period elapsed)
    pub fn should_save(&self) -> bool {
        if !self.auto_save_enabled || !self.pending_save {
            return false;
        }

        if let Some(last_time) = self.last_dirty_time {
            Instant::now().duration_since(last_time) >= self.debounce_delay
        } else {
            false
        }
    }

    /// Mark save as completed
    pub fn mark_saved(&mut self) {
        self.pending_save = false;
        self.last_dirty_time = None;
    }

    /// Enable/disable auto-save
    pub fn set_auto_save_enabled(&mut self, enabled: bool) {
        self.auto_save_enabled = enabled;
    }

    /// Force save immediately (bypass debounce)
    pub fn force_save(&mut self) {
        self.pending_save = true;
    }

    /// Get debounce delay in milliseconds
    pub fn debounce_delay_ms(&self) -> u64 {
        self.debounce_delay.as_millis() as u64
    }

    /// Set debounce delay
    pub fn set_debounce_delay(&mut self, ms: u64) {
        self.debounce_delay = Duration::from_millis(ms);
    }
}

impl Default for SramManager {
    fn default() -> Self {
        Self::new(1000) // 1 second default debounce
    }
}

/// Get current timestamp as ISO 8601 string
fn current_timestamp() -> String {
    // Placeholder - will be replaced with proper timestamp in console-specific code
    "1970-01-01T00:00:00Z".to_string()
}
