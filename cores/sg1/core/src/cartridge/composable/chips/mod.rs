//! Genesis coprocessors, each an independent
//! [`Coprocessor`](retro_core::cartridge::Coprocessor).
//!
//! Every Genesis "special cartridge" behaviour is modelled here as a chip
//! the [`ChipSet`](retro_core::cartridge::ChipSet) hosts, reachable at a
//! `chip_id` the map plan carries:
//!
//! - [`BankMapperChip`]: the SEGA / SSF2 / Codemasters dynamic banking
//!   schemes, as a *mapper-as-coprocessor*: it owns the cartridge ROM and
//!   translates ROM-window reads through a per-slot bank table, updated
//!   either by observed `0xA130Fx` writes (SEGA/SSF2) or by ROM-window
//!   writes (Codemasters).
//! - [`EepromChip`]: the serial-EEPROM (X24C01/X24C02) I2C-like protocol,
//!   with its cell array exposed as the cartridge battery.
//! - [`JCartChip`]: the two extra controller ports (ports 3/4) exposed at
//!   `0x38FFFE` / `0x3FFFFE`.
//! - [`SvpCoprocessor`]: the Sega Virtua Processor, an SSP1601 interpreter
//!   plus the DRAM windows and the `0xA15000` mailbox it shares with the 68000.
//!
//! A detected cartridge hosts a single chip, so every id is 0; the
//! constants name them for clarity and give the multi-chip machinery a
//! stable id to key on.

pub mod bank_mapper;
pub mod eeprom;
pub mod jcart;
pub mod svp;

pub use bank_mapper::BankMapperChip;
pub use eeprom::EepromChip;
pub use jcart::JCartChip;
pub use svp::SvpCoprocessor;

/// `chip_id` of the bank-switching mapper in a SEGA/SSF2/Codemasters cart.
pub const BANK_MAPPER_CHIP_ID: u8 = 0;
/// `chip_id` of the serial EEPROM in an EEPROM cart.
pub const EEPROM_CHIP_ID: u8 = 0;
/// `chip_id` of the J-Cart controller-port expander.
pub const JCART_CHIP_ID: u8 = 0;
/// `chip_id` of the SVP coprocessor in a Virtua Racing cart.
pub const SVP_CHIP_ID: u8 = 0;
