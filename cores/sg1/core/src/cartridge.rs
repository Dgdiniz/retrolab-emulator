//! The Mega Drive's cartridges.
//!
//! A cartridge is retro-core's composable cartridge
//! ([`retro_core::cartridge::ComposableCartridge`]): the ROM, an optional
//! battery SRAM, a [`MapPlan`] the 68000's cartridge window resolves
//! through, and a [`ChipSet`](retro_core::cartridge::ChipSet) of
//! coprocessors (the bank mappers, the EEPROM, the J-Cart, the SVP). What
//! this core keeps is what the Mega Drive makes different: a slot is one
//! linear range of the 24-bit address space ([`map_plan`]); the board
//! ([`board`]) holds the SRAM switch at `$A130F1` and its /TIME register;
//! the presets ([`presets`]), the header and what is detected from it
//! ([`header`], [`detect`]) and the builder that picks a preset
//! ([`builder`]); the chips ([`chips`]) and the factory that builds them
//! ([`factory`]); the manifest's slot and presets ([`manifest`]); and the
//! typed list of chip states the savestate holds ([`savestate`]).

pub mod board;
pub mod builder;
pub mod chips;
pub mod detect;
pub mod factory;
pub mod header;
pub mod manifest;
pub mod map_plan;
pub mod presets;

pub use board::{GenesisBoard, GenesisCartridge, SRAM_CONTROL_ADDR};
pub use builder::{build_genesis_cartridge, build_with_manifest, detected_manifest};
pub use chips::{
    BankMapperChip, EepromChip, JCartChip, SvpCoprocessor, BANK_MAPPER_CHIP_ID, EEPROM_CHIP_ID,
    JCART_CHIP_ID, SVP_CHIP_ID,
};
pub use detect::*;
pub use factory::Sg1ChipFactory;
pub use header::*;
pub use manifest::{parse_manifest, CartridgeManifest};
pub use map_plan::{MapPlan, MapSlot, MapTarget, Resolved};
pub use presets::PresetKind;
