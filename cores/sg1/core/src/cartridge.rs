//! Cartridge system for Sega Genesis.
//!
//! Genesis cartridges are built with the composable model in
//! [`composable`]: a ROM + optional SRAM + a `ChipSet` of coprocessors,
//! addressed through a data-driven `MapPlan`. Board types (plain, SEGA /
//! SSF2 / Codemasters banking, EEPROM, J-Cart, SVP) are expressed as map
//! plans + chips, not per-board structs.
//!
//! [`detection`] and [`parsers`] provide the header heuristics the
//! composable builder runs to pick a preset and size ROM/SRAM.

pub mod composable;
pub mod detection;
pub mod parsers;

// Re-export public API
pub use detection::*;
pub use parsers::*;
