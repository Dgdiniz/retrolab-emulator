//! Where a byte the 68000 reads in the cartridge window comes from: the
//! offset in the ROM file behind an address, through the cartridge's
//! mapping as it stands (a bank mapper's banks, the SRAM switch), for a
//! debugger that names the ROM bytes a transfer read
//! ([`GenesisCartridge::byte_sources`](super::GenesisCartridge::byte_sources)).
//!
//! A ROM file offset or a negative code ([`SourcePlace::code`]) is what
//! `AddressBus::resolve_sources` gives for each address of the 68000's
//! whole space. Here, [`SourcePlace::CartridgeRam`] is the battery SRAM
//! switched in, [`SourcePlace::Coprocessor`] what a coprocessor answers from
//! its own memory or registers (the SVP's DRAM, an EEPROM, a J-Cart's
//! ports), and [`SourcePlace::Unknown`] open bus or a read past the end of
//! the ROM.

use retro_core::cartridge::SourcePlace;

use super::map_plan::{MapPlan, MapTarget};

/// What a read of `address` reaches through `plan`: a ROM slot's offset
/// is given to `rom`, an SRAM slot's to `sram`, a chip slot's chip and
/// offset to `chip`; no slot and an open-bus slot reach nothing.
pub(crate) fn through_plan(
    plan: &MapPlan,
    address: u32,
    rom: impl Fn(u32) -> SourcePlace,
    sram: impl Fn(u32) -> SourcePlace,
    chip: impl Fn(u8, u32) -> SourcePlace,
) -> SourcePlace {
    let Some(resolved) = plan.resolve(address) else {
        return SourcePlace::Unknown;
    };
    match plan.slots[resolved.slot_index as usize].target {
        MapTarget::Rom { base_offset } => rom(base_offset.wrapping_add(resolved.target_offset)),
        MapTarget::Sram { base_offset } => sram(base_offset.wrapping_add(resolved.target_offset)),
        MapTarget::ChipMmio { chip_id, base } => {
            chip(chip_id, base.wrapping_add(resolved.target_offset))
        }
        MapTarget::OpenBus => SourcePlace::Unknown,
    }
}

/// The ROM file offset `offset`, when a ROM of `len` bytes holds it; a
/// read past its end is open bus.
pub(crate) fn rom_byte(offset: u32, len: usize) -> SourcePlace {
    if (offset as usize) < len {
        SourcePlace::Rom(offset)
    } else {
        SourcePlace::Unknown
    }
}

/// Where a read of an SRAM slot at `offset` comes from, as `read_sram`
/// answers it: nothing without an SRAM buffer, the cartridge RAM while it
/// is switched in, else the ROM showing through at the slot's address
/// (`start + offset`, mirrored by `rom_mask`).
pub(crate) fn sram_byte(
    has_sram: bool,
    enabled: bool,
    start: u32,
    offset: u32,
    rom_mask: u32,
    rom_len: usize,
) -> SourcePlace {
    if !has_sram {
        SourcePlace::Unknown
    } else if enabled {
        SourcePlace::CartridgeRam
    } else {
        rom_byte(start.wrapping_add(offset) & rom_mask, rom_len)
    }
}
