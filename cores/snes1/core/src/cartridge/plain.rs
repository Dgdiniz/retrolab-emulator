//! Plain ROM-and-SRAM cartridge implementation.
//!
//! [`PlainCartridge`] is the default backing for cartridges that
//! only carry ROM and (optionally) battery-backed SRAM, no
//! coprocessor chips. It is the natural target for LoROM, HiROM,
//! and ExHiROM presets, plus any homebrew manifest whose `slots`
//! only reference `MapTarget::Rom` and `MapTarget::Sram`.
//!
//! A cartridge with coprocessor chips (SA-1, SDD-1, DSP-1, ...)
//! would either wrap `PlainCartridge` and override `read_chip`/
//! `write_chip`, or be implemented as its own [`SnesCartridge`]
//! impl with whatever state the chip requires.

use super::{MapPlan, SnesCartridge};

/// A ROM image, an SRAM region, and a [`MapPlan`] describing how
/// they project onto the SNES address space.
///
/// Out-of-bounds ROM reads return `0` rather than panicking: the
/// bus walker may resolve a slot offset that lies past the end of
/// the ROM image when a mask is set permissively or when the slot
/// is intentionally larger than the underlying chip (LoROM cart
/// with a 32 KiB ROM mapped into the full 4 MiB LoROM window, for
/// example). The same safety net applies to SRAM access.
pub struct PlainCartridge {
    rom: Vec<u8>,
    sram: Vec<u8>,
    plan: MapPlan,
}

impl PlainCartridge {
    /// Build a cartridge with the given ROM bytes, a zeroed SRAM
    /// region of `sram_size` bytes (use 0 for "no SRAM"), and the
    /// supplied map plan.
    pub fn new(rom: Vec<u8>, sram_size: usize, plan: MapPlan) -> Self {
        Self {
            rom,
            sram: vec![0u8; sram_size],
            plan,
        }
    }

    /// Build a cartridge with pre-populated SRAM: the builder calls it
    /// for a buffer handed to `CartridgeBuilder::with_sram`. `.srm` files
    /// and savestates restore SRAM through `restore_sram` instead.
    pub fn with_sram(rom: Vec<u8>, sram: Vec<u8>, plan: MapPlan) -> Self {
        Self { rom, sram, plan }
    }

    /// ROM image length in bytes.
    pub fn rom_len(&self) -> usize {
        self.rom.len()
    }

    /// SRAM image length in bytes. Zero means no SRAM.
    pub fn sram_len(&self) -> usize {
        self.sram.len()
    }

    /// Read-only borrow of the SRAM buffer (for save persistence).
    pub fn sram(&self) -> &[u8] {
        &self.sram
    }

    /// Consume the cartridge, returning its `(rom, sram, plan)`. A composite
    /// cartridge (e.g. `ComposableCartridge`) uses this to take ownership of
    /// a plain cartridge's storage and re-home it behind a chip set without
    /// re-reading or cloning the ROM image.
    pub fn into_parts(self) -> (Vec<u8>, Vec<u8>, MapPlan) {
        (self.rom, self.sram, self.plan)
    }
}

impl SnesCartridge for PlainCartridge {
    fn map_plan(&self) -> &MapPlan {
        &self.plan
    }

    fn read_rom(&self, offset: u32) -> u8 {
        self.rom.get(offset as usize).copied().unwrap_or(0)
    }

    fn read_sram(&self, offset: u32) -> u8 {
        self.sram.get(offset as usize).copied().unwrap_or(0)
    }

    fn write_sram(&mut self, offset: u32, value: u8) {
        if let Some(slot) = self.sram.get_mut(offset as usize) {
            *slot = value;
        }
    }

    fn sram(&self) -> &[u8] {
        &self.sram
    }

    fn restore_sram(&mut self, data: &[u8]) {
        // Copy up to the smaller of the two lengths so a `.srm` or
        // savestate that disagrees on size degrades gracefully (extra
        // bytes ignored, missing bytes left untouched) instead of
        // panicking.
        let n = data.len().min(self.sram.len());
        self.sram[..n].copy_from_slice(&data[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cartridge::{BusSpeed, MapPlan, MapSlot, MapTarget};

    fn lorom_plan() -> MapPlan {
        let mut plan = MapPlan::new("lorom-test");
        plan.push(MapSlot::new(
            0x80..=0x80,
            0x8000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            BusSpeed::Fast,
        ));
        plan
    }

    #[test]
    fn plain_cartridge_constructs_with_rom_and_sram() {
        let cart = PlainCartridge::new(vec![0xAAu8; 0x8000], 2048, lorom_plan());
        assert_eq!(cart.rom_len(), 0x8000);
        assert_eq!(cart.sram_len(), 2048);
        assert_eq!(cart.map_plan().name, "lorom-test");
        // SRAM is zero-initialised.
        assert_eq!(cart.read_sram(0), 0);
        assert_eq!(cart.read_sram(2047), 0);
    }

    #[test]
    fn plain_cartridge_read_rom_within_bounds() {
        let rom: Vec<u8> = (0..=255).cycle().take(0x8000).collect();
        let cart = PlainCartridge::new(rom, 0, lorom_plan());
        assert_eq!(cart.read_rom(0), 0);
        assert_eq!(cart.read_rom(1), 1);
        assert_eq!(cart.read_rom(0xFF), 0xFF);
        assert_eq!(cart.read_rom(0x100), 0); // pattern wraps every 256.
        assert_eq!(cart.read_rom(0x7FFF), 0xFF);
    }

    #[test]
    fn plain_cartridge_read_rom_out_of_bounds_returns_zero() {
        let cart = PlainCartridge::new(vec![0x55u8; 0x100], 0, lorom_plan());
        assert_eq!(cart.read_rom(0x100), 0);
        assert_eq!(cart.read_rom(0xFFFF_FFFF), 0);
    }

    #[test]
    fn plain_cartridge_sram_round_trip() {
        let mut cart = PlainCartridge::new(vec![0u8; 0x100], 2048, lorom_plan());
        cart.write_sram(0, 0x42);
        cart.write_sram(2047, 0xAB);
        assert_eq!(cart.read_sram(0), 0x42);
        assert_eq!(cart.read_sram(2047), 0xAB);
        assert_eq!(cart.read_sram(1), 0);
        // The SRAM accessor reflects the same bytes.
        assert_eq!(cart.sram()[0], 0x42);
        assert_eq!(cart.sram()[2047], 0xAB);
    }

    #[test]
    fn plain_cartridge_write_sram_out_of_bounds_no_op() {
        let mut cart = PlainCartridge::new(vec![0u8; 0x100], 16, lorom_plan());
        cart.write_sram(16, 0xCC); // first byte past the SRAM end.
        cart.write_sram(0xFFFF_FFFF, 0xDD);
        // Existing bytes untouched.
        assert_eq!(cart.read_sram(0), 0);
        assert_eq!(cart.read_sram(15), 0);
        // Out-of-bounds read still returns 0.
        assert_eq!(cart.read_sram(16), 0);
    }
}
