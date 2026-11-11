//! Header parser infrastructure
//!
//! This module contains ONLY generic traits and structures.
//! Console-specific implementations should be in their respective crates:
//! - GenesisHeaderParser -> cores/sg1/core/src/cartridge/parsers.rs

// Re-export the HeaderParser trait from detection module
pub use super::detection::HeaderParser;
