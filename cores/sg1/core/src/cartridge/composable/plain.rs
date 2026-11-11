//! Plain Genesis cartridge: ROM + optional battery SRAM, no coprocessor.
//!
//! The cartridge keeps SRAM in one flat buffer over the whole `[start, end)`
//! window, indexed by `addr - start` (the layout Genesis Plus GX's save files
//! have), and models the `0xA130F1` SRAM-enable toggle.
//!
//! SRAM handling has two regimes, as on hardware (checked against Genesis
//! Plus GX):
//! - **Permanent**: SRAM whose window starts *beyond* the mirrored ROM
//!   image is always mapped; `0xA130F1` writes do not gate it.
//! - **Overlapping**: SRAM whose window overlaps the ROM image starts
//!   *disabled* (the ROM shows through) and is toggled on/off by
//!   `0xA130F1` bit 0. When disabled, a read of the SRAM window returns the
//!   underlying mirrored ROM byte; a write is dropped.
//!
//! The `0xA130F1` write is delivered through [`observe_bus_write`]: the
//! cartridge registers as a bus observer only when it hosts an overlapping
//! SRAM, so a plain ROM (or permanent-SRAM) cartridge pays nothing.
//!
//! [`observe_bus_write`]: super::GenesisCartridge::observe_bus_write

use std::sync::Arc;

use retro_core::cartridge::CartridgeInfo;

use super::map_plan::{rom_mirror_mask, MapPlan, MapSlot, MapTarget};
use super::GenesisCartridge;

/// The `/TIME` register the SEGA mapper protocol uses for the SRAM-enable
/// bit (`0xA130F1`, bit 0).
pub const SRAM_CONTROL_ADDR: u32 = 0xA130F1;

/// A cartridge with stock ROM and optional battery SRAM, no coprocessor.
pub struct PlainCartridge {
    /// ROM image, unpadded, shared via `Arc` (a future ROM-consuming chip
    /// would take a clone). Mirroring is done by the plan's ROM-slot mask
    /// plus the `0xFF` out-of-bounds fold in [`read_rom`], so the buffer is
    /// never physically padded.
    rom: Arc<[u8]>,
    /// Power-of-two mirror mask for the ROM window.
    rom_mask: u32,
    /// Flat SRAM buffer over `[sram_start, sram_start + sram.len())`; empty
    /// when the cartridge has no SRAM.
    sram: Vec<u8>,
    /// Absolute 68000 start address of the SRAM window.
    sram_start: u32,
    /// Whether SRAM is currently readable/writable (see the regime docs).
    sram_enabled: bool,
    /// Permanent SRAM ignores the `0xA130F1` toggle.
    sram_permanent: bool,
    /// SRAM written since the last `.srm` flush.
    sram_dirty: bool,
    /// The combined map plan (SRAM slot first when present, then ROM).
    plan: MapPlan,
    /// Cartridge metadata.
    info: CartridgeInfo,
}

impl PlainCartridge {
    /// A ROM-only cartridge (no SRAM).
    pub fn new(rom: Arc<[u8]>, info: CartridgeInfo) -> Self {
        let rom_mask = rom_mirror_mask(rom.len());
        let mut plan = MapPlan::new("plain");
        plan.push(
            MapSlot::new(0x000000..=0x3FFFFF, MapTarget::Rom { base_offset: 0 })
                .with_mask(rom_mask),
        );
        Self {
            rom,
            rom_mask,
            sram: Vec::new(),
            sram_start: 0,
            sram_enabled: false,
            sram_permanent: false,
            sram_dirty: false,
            plan,
            info,
        }
    }

    /// A cartridge with battery SRAM over `[sram_start, sram_start + sram_len)`.
    ///
    /// SRAM whose start is at or beyond the mirrored ROM image (`rom_mask + 1`
    /// bytes) is permanent and starts enabled; otherwise it overlaps ROM and
    /// starts disabled, gated by `0xA130F1`.
    pub fn with_sram(rom: Arc<[u8]>, info: CartridgeInfo, sram_start: u32, sram_len: u32) -> Self {
        let rom_mask = rom_mirror_mask(rom.len());
        let sram_start = sram_start & !1; // word-align
        let padded_rom_len = rom_mask.wrapping_add(1);
        let sram_permanent = sram_start >= padded_rom_len;

        // The SRAM slot is pushed first so it wins over the overlapping ROM
        // window; `read_sram`/`write_sram` then apply the enable gate.
        let mut plan = MapPlan::new("plain");
        let sram_end = sram_start.saturating_add(sram_len.max(1));
        plan.push(MapSlot::new(
            sram_start..=sram_end.saturating_sub(1),
            MapTarget::Sram { base_offset: 0 },
        ));
        plan.push(
            MapSlot::new(0x000000..=0x3FFFFF, MapTarget::Rom { base_offset: 0 })
                .with_mask(rom_mask),
        );

        Self {
            rom,
            rom_mask,
            sram: vec![0xFF; sram_len as usize],
            sram_start,
            sram_enabled: sram_permanent,
            sram_permanent,
            sram_dirty: false,
            plan,
            info,
        }
    }

    /// Whether the SRAM enable bit is currently set (for the debugger and
    /// the `0xA130F1` read hook the bus installs).
    pub fn sram_enabled(&self) -> bool {
        self.sram_enabled
    }
}

impl GenesisCartridge for PlainCartridge {
    fn map_plan(&self) -> &MapPlan {
        &self.plan
    }

    fn info(&self) -> &CartridgeInfo {
        &self.info
    }

    fn read_rom(&self, offset: u32) -> u8 {
        self.rom.get(offset as usize).copied().unwrap_or(0xFF)
    }

    fn read_sram(&self, offset: u32) -> u8 {
        if self.sram.is_empty() {
            return 0xFF;
        }
        if self.sram_enabled {
            self.sram.get(offset as usize).copied().unwrap_or(0xFF)
        } else {
            // Overlapping SRAM disabled: the mirrored ROM shows through.
            let abs = self.sram_start.wrapping_add(offset);
            self.rom
                .get((abs & self.rom_mask) as usize)
                .copied()
                .unwrap_or(0xFF)
        }
    }

    fn write_sram(&mut self, offset: u32, value: u8) {
        if !self.sram_enabled {
            return; // disabled overlap -> write hits read-only ROM, dropped
        }
        if let Some(slot) = self.sram.get_mut(offset as usize) {
            *slot = value;
            self.sram_dirty = true;
        }
    }

    fn observe_bus_write(&mut self, addr: u32, value: u8) {
        // The SRAM-enable register only exists for overlapping SRAM.
        if addr == SRAM_CONTROL_ADDR && !self.sram_permanent && !self.sram.is_empty() {
            self.sram_enabled = value & 0x01 != 0;
        }
    }

    fn observes_bus(&self) -> bool {
        !self.sram.is_empty() && !self.sram_permanent
    }

    fn sram(&self) -> &[u8] {
        &self.sram
    }

    fn restore_sram(&mut self, data: &[u8]) {
        let n = data.len().min(self.sram.len());
        self.sram[..n].copy_from_slice(&data[..n]);
    }

    fn is_sram_dirty(&self) -> bool {
        self.sram_dirty
    }

    fn clear_sram_dirty(&mut self) {
        self.sram_dirty = false;
    }

    fn read_time_register(&self, addr: u32) -> Option<u8> {
        if addr == SRAM_CONTROL_ADDR && !self.sram.is_empty() {
            Some(u8::from(self.sram_enabled))
        } else {
            None
        }
    }

    fn reset(&mut self) {
        self.sram_enabled = self.sram_permanent;
        self.sram_dirty = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::cartridge::{ConsoleSystem, DetectionSource, SRAMType};

    fn test_info(rom_size: usize, sram_size: usize) -> CartridgeInfo {
        CartridgeInfo {
            system: ConsoleSystem::Genesis,
            title: "TEST".to_string(),
            serial: None,
            regions: Vec::new(),
            rom_size,
            sram_size,
            sram_type: if sram_size > 0 {
                SRAMType::Battery
            } else {
                SRAMType::None
            },
            mapper_info: None,
            expansion_chip_info: None,
            header_checksum: None,
            calculated_checksum: None,
            rom_hash: String::new(),
            detection_source: DetectionSource::Header,
        }
    }

    fn rom_with_marker(len: usize) -> Arc<[u8]> {
        let mut rom = vec![0u8; len];
        rom[0x100] = b'S';
        rom[0x123] = 0x42;
        Arc::from(rom.into_boxed_slice())
    }

    #[test]
    fn rom_only_reads_and_mirrors() {
        let rom = rom_with_marker(0x80000); // 512 KiB
        let mut c = PlainCartridge::new(rom, test_info(0x80000, 0));
        assert_eq!(c.map_plan().name, "plain");
        assert_eq!(c.read_addr(0x000100), b'S');
        assert_eq!(c.read_addr(0x000123), 0x42);
        // 512 KiB ROM mirrors across the 4 MiB window: 0x080123 folds to 0x000123.
        assert_eq!(c.read_addr(0x080123), 0x42);
        // No SRAM -> not a bus observer, empty sram image.
        assert!(!c.observes_bus());
        assert!(c.sram().is_empty());
    }

    #[test]
    fn rom_writes_are_dropped() {
        let rom = rom_with_marker(0x80000);
        let mut c = PlainCartridge::new(rom, test_info(0x80000, 0));
        c.write_addr(0x000123, 0x99); // ROM is read-only
        assert_eq!(c.read_addr(0x000123), 0x42);
    }

    #[test]
    fn permanent_sram_beyond_rom_is_always_mapped() {
        // 2 MiB ROM, SRAM at 0x200000 (right past the mirrored image).
        let rom = rom_with_marker(0x200000);
        let mut c = PlainCartridge::with_sram(rom, test_info(0x200000, 0x8000), 0x200000, 0x8000);
        assert!(!c.observes_bus(), "permanent SRAM is not toggled");
        assert!(c.sram_enabled(), "permanent SRAM starts enabled");

        c.write_addr(0x200000, 0xAB);
        c.write_addr(0x207FFF, 0xCD);
        assert_eq!(c.read_addr(0x200000), 0xAB);
        assert_eq!(c.read_addr(0x207FFF), 0xCD);
        assert!(c.is_sram_dirty());
        assert_eq!(c.sram().len(), 0x8000);

        c.clear_sram_dirty();
        assert!(!c.is_sram_dirty());
    }

    #[test]
    fn overlapping_sram_is_gated_by_a130f1() {
        // 4 MiB ROM with SRAM overlapping at 0x200000 (inside the image).
        let mut rom = vec![0u8; 0x400000];
        rom[0x200000] = 0x11; // the ROM byte behind the SRAM window
        let rom: Arc<[u8]> = Arc::from(rom.into_boxed_slice());
        let mut c = PlainCartridge::with_sram(rom, test_info(0x400000, 0x8000), 0x200000, 0x8000);

        assert!(c.observes_bus(), "overlapping SRAM must observe 0xA130F1");
        assert!(!c.sram_enabled(), "overlapping SRAM starts disabled");
        // Disabled: the ROM shows through, writes are dropped.
        assert_eq!(c.read_addr(0x200000), 0x11);
        c.write_addr(0x200000, 0x22);
        assert_eq!(c.read_addr(0x200000), 0x11, "write dropped while disabled");

        // Enable via 0xA130F1 bit 0.
        c.write_addr(SRAM_CONTROL_ADDR, 0x01);
        assert!(c.sram_enabled());
        c.write_addr(0x200000, 0x22);
        assert_eq!(c.read_addr(0x200000), 0x22, "SRAM now readable/writable");

        // Disable again: ROM shows through, the SRAM contents are retained.
        c.write_addr(SRAM_CONTROL_ADDR, 0x00);
        assert!(!c.sram_enabled());
        assert_eq!(c.read_addr(0x200000), 0x11);
        c.write_addr(SRAM_CONTROL_ADDR, 0x01);
        assert_eq!(
            c.read_addr(0x200000),
            0x22,
            "contents preserved across toggle"
        );
    }

    #[test]
    fn restore_sram_degrades_on_size_mismatch() {
        let rom = rom_with_marker(0x200000);
        let mut c = PlainCartridge::with_sram(rom, test_info(0x200000, 0x8000), 0x200000, 0x8000);
        c.restore_sram(&[0x01, 0x02, 0x03]);
        assert_eq!(c.read_addr(0x200000), 0x01);
        assert_eq!(c.read_addr(0x200002), 0x03);
    }

    #[test]
    fn reset_reanchors_enable_to_permanent() {
        let mut rom = vec![0u8; 0x400000];
        rom[0x200000] = 0x11;
        let rom: Arc<[u8]> = Arc::from(rom.into_boxed_slice());
        let mut c = PlainCartridge::with_sram(rom, test_info(0x400000, 0x8000), 0x200000, 0x8000);
        c.write_addr(SRAM_CONTROL_ADDR, 0x01);
        c.write_addr(0x200000, 0x22);
        assert!(c.sram_enabled());
        c.reset();
        assert!(!c.sram_enabled(), "reset disables overlapping SRAM again");
        assert!(!c.is_sram_dirty());
    }

    #[test]
    fn no_coprocessor_surface_on_plain_cart() {
        let rom = rom_with_marker(0x80000);
        let c = PlainCartridge::new(rom, test_info(0x80000, 0));
        assert!(c.coprocessor_label().is_none());
        assert_eq!(c.coprocessor_accesses(), 0);
        assert!(c.coprocessors_savestate().is_empty());
        assert!(c.coprocessor_debug_state().is_none());
        assert!(!c.has_chip_irq());
    }
}
