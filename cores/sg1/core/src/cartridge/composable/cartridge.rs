//! Composable multi-chip Genesis cartridge.
//!
//! [`ComposableCartridge`] hosts a [`ChipSet`] of independent
//! coprocessors alongside stock ROM + battery SRAM. It is the Genesis
//! counterpart of the SNES `ComposableCartridge`, adapted to the flat
//! 24-bit address space, and carries two Genesis-specific concerns the
//! SNES version does not:
//!
//! - **`CartridgeInfo`**: the Genesis system reads region/title/checksum
//!   off the cartridge, so it is stored and exposed via
//!   [`GenesisCartridge::info`].
//! - **Battery routing**: a cartridge whose save lives *inside* a chip
//!   (a serial-EEPROM cart) has no own SRAM buffer; its `.srm` traffic is
//!   routed to that chip through the generic
//!   [`Coprocessor::battery`](retro_core::cartridge::Coprocessor::battery)
//!   accessors. Cartridges with a stock SRAM buffer (SEGA-mapper saves,
//!   J-Cart saves) keep the flat-buffer + `0xA130F1` toggle model from the
//!   plain cartridge.
//!
//! Savestates bridge the generic opaque-bytes surface
//! ([`Coprocessor::serialize`](retro_core::cartridge::Coprocessor::serialize))
//! to the typed [`GenesisCoprocessorState`]: each chip `bincode`-encodes
//! one variant, and this cartridge decodes it, keyed by `chip_id`.

use std::sync::Arc;

use retro_core::cartridge::{CartridgeInfo, ChipSet, ExpansionChipState};

use super::map_plan::rom_mirror_mask;
use super::plain::SRAM_CONTROL_ADDR;
use super::savestate::GenesisCoprocessorState;
use super::{GenesisCartridge, MapPlan};

/// A cartridge composed of stock ROM + optional battery SRAM and a
/// [`ChipSet`] of coprocessors, each reachable at its own `chip_id`.
pub struct ComposableCartridge {
    rom: Arc<[u8]>,
    rom_mask: u32,
    /// Stock SRAM buffer (empty when the cartridge has none or its save is
    /// chip-owned: see `battery_chip_id`).
    sram: Vec<u8>,
    sram_start: u32,
    sram_enabled: bool,
    sram_permanent: bool,
    sram_dirty: bool,
    /// Chip that owns the battery save (EEPROM), routing `.srm` traffic when
    /// there is no stock SRAM buffer.
    battery_chip_id: Option<u8>,
    plan: MapPlan,
    chips: ChipSet,
    /// Lowest-id chip, reported by the single-value coprocessor accessors.
    coproc_id: Option<u8>,
    info: CartridgeInfo,
}

impl ComposableCartridge {
    /// Assemble a cartridge with no stock SRAM. Use the builder methods to
    /// attach a stock SRAM buffer or designate a battery chip.
    pub fn new(rom: Arc<[u8]>, plan: MapPlan, chips: ChipSet, info: CartridgeInfo) -> Self {
        let rom_mask = rom_mirror_mask(rom.len());
        let coproc_id = chips.first_id();
        Self {
            rom,
            rom_mask,
            sram: Vec::new(),
            sram_start: 0,
            sram_enabled: false,
            sram_permanent: false,
            sram_dirty: false,
            battery_chip_id: None,
            plan,
            chips,
            coproc_id,
            info,
        }
    }

    /// Attach a stock SRAM buffer over `[sram_start, sram_start + len)`. The
    /// plan must already carry the matching `Sram` slot; the
    /// permanent-vs-overlapping regime is derived exactly as the plain
    /// cartridge (start at/beyond the mirrored ROM image => permanent).
    pub fn with_sram(mut self, sram: Vec<u8>, sram_start: u32) -> Self {
        let sram_start = sram_start & !1;
        let padded_rom_len = self.rom_mask.wrapping_add(1);
        self.sram_permanent = sram_start >= padded_rom_len;
        self.sram_enabled = self.sram_permanent;
        self.sram = sram;
        self.sram_start = sram_start;
        self
    }

    /// Designate the chip whose `battery()` backs this cartridge's `.srm`
    /// (used by EEPROM carts, whose save lives in the chip).
    pub fn with_battery_chip(mut self, chip_id: u8) -> Self {
        self.battery_chip_id = Some(chip_id);
        self
    }

    /// Whether the SRAM enable bit is currently set (debugger / `0xA130F1`
    /// read hook).
    pub fn sram_enabled(&self) -> bool {
        self.sram_enabled
    }
}

impl GenesisCartridge for ComposableCartridge {
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
            let abs = self.sram_start.wrapping_add(offset);
            self.rom
                .get((abs & self.rom_mask) as usize)
                .copied()
                .unwrap_or(0xFF)
        }
    }

    fn write_sram(&mut self, offset: u32, value: u8) {
        if !self.sram_enabled {
            return;
        }
        if let Some(slot) = self.sram.get_mut(offset as usize) {
            *slot = value;
            self.sram_dirty = true;
        }
    }

    fn read_chip(&mut self, chip_id: u8, offset: u32, master_cycle: u64) -> u8 {
        // Missing chip -> Genesis open bus (0xFF), matching the trait default.
        self.chips
            .read(chip_id, offset, master_cycle)
            .unwrap_or(0xFF)
    }

    fn write_chip(&mut self, chip_id: u8, offset: u32, value: u8, master_cycle: u64) {
        self.chips.write(chip_id, offset, value, master_cycle);
    }

    fn catch_up_chip(&mut self, master_cycle: u64) {
        self.chips.catch_up_all(master_cycle);
    }

    fn reset_chip(&mut self, master_clock_hz: u64) {
        self.chips.reset_all(master_clock_hz);
    }

    fn observe_bus_write(&mut self, addr: u32, value: u8) {
        // Own overlapping-SRAM enable toggle (0xA130F1 bit 0)...
        if addr == SRAM_CONTROL_ADDR && !self.sram_permanent && !self.sram.is_empty() {
            self.sram_enabled = value & 0x01 != 0;
        }
        // ...then fan out to any chip observers (e.g. a SEGA mapper's
        // 0xA130Fx bank registers).
        self.chips.observe_write(addr, value);
    }

    fn observes_bus(&self) -> bool {
        self.chips.has_observers() || (!self.sram.is_empty() && !self.sram_permanent)
    }

    fn has_chip_irq(&self) -> bool {
        self.coproc_id
            .and_then(|id| self.chips.get(id))
            .is_some_and(|c| c.has_irq())
    }

    fn coprocessor_label(&self) -> Option<&str> {
        self.chips.get(self.coproc_id?).map(|c| c.label())
    }

    fn coprocessor_accesses(&self) -> u64 {
        self.coproc_id
            .and_then(|id| self.chips.get(id))
            .map_or(0, |c| c.accesses())
    }

    fn coprocessors_savestate(&self) -> Vec<(u8, GenesisCoprocessorState)> {
        // Each chip serializes an opaque blob (a bincode-encoded
        // GenesisCoprocessorState). An empty blob means "nothing to
        // persist" and is dropped from the snapshot.
        self.chips
            .serialize()
            .into_iter()
            .filter(|(_, bytes)| !bytes.is_empty())
            .filter_map(|(id, bytes)| {
                bincode::deserialize::<GenesisCoprocessorState>(&bytes)
                    .ok()
                    .map(|state| (id, state))
            })
            .collect()
    }

    fn restore_coprocessors_savestate(
        &mut self,
        states: &[(u8, GenesisCoprocessorState)],
        master_clock_hz: u64,
    ) {
        let blobs: Vec<(u8, Vec<u8>)> = states
            .iter()
            .filter_map(|(id, state)| bincode::serialize(state).ok().map(|bytes| (*id, bytes)))
            .collect();
        self.chips.restore(&blobs, master_clock_hz);
    }

    fn coprocessor_debug_state(&self) -> Option<ExpansionChipState> {
        self.chips.get(self.coproc_id?).map(|c| c.debug_state())
    }

    fn coprocessors_debug_state(&self) -> Vec<(u8, ExpansionChipState)> {
        self.chips.debug_states()
    }

    fn coprocessor_labels(&self) -> Vec<(u8, String)> {
        self.chips.labels()
    }

    fn sram(&self) -> &[u8] {
        if !self.sram.is_empty() {
            return &self.sram;
        }
        // Chip-owned battery (EEPROM): route to the designated chip.
        if let Some(id) = self.battery_chip_id {
            if let Some(data) = self.chips.get(id).and_then(|c| c.battery()) {
                return data;
            }
        }
        &[]
    }

    fn restore_sram(&mut self, data: &[u8]) {
        if !self.sram.is_empty() {
            let n = data.len().min(self.sram.len());
            self.sram[..n].copy_from_slice(&data[..n]);
            return;
        }
        if let Some(id) = self.battery_chip_id {
            if let Some(c) = self.chips.get_mut(id) {
                c.restore_battery(data);
            }
        }
    }

    fn is_sram_dirty(&self) -> bool {
        if !self.sram.is_empty() {
            return self.sram_dirty;
        }
        self.battery_chip_id
            .and_then(|id| self.chips.get(id))
            .is_some_and(|c| c.battery_dirty())
    }

    fn clear_sram_dirty(&mut self) {
        if !self.sram.is_empty() {
            self.sram_dirty = false;
            return;
        }
        if let Some(id) = self.battery_chip_id {
            if let Some(c) = self.chips.get_mut(id) {
                c.clear_battery_dirty();
            }
        }
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
        // Chips are reset via reset_chip (needs the region clock).
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cartridge::composable::map_plan::{MapSlot, MapTarget};
    use crate::cartridge::composable::savestate::{JCartState, SvpState};
    use retro_core::cartridge::{ChipRegister, Coprocessor, ExpansionChipInfo, ExpansionChipType};

    /// A minimal fake chip: `read` answers `0xC0 + offset` and counts
    /// accesses; `write` latches a byte that `serialize` persists inside a
    /// `GenesisCoprocessorState::JCart` (reusing an existing variant so the
    /// typed bridge is exercised without a real chip). Optionally carries a
    /// battery buffer to exercise the EEPROM routing.
    struct FakeChip {
        info: ExpansionChipInfo,
        label: String,
        accesses: u64,
        latched: u8,
        battery: Option<Vec<u8>>,
        battery_dirty: bool,
    }

    impl FakeChip {
        fn new() -> Self {
            Self {
                info: ExpansionChipInfo {
                    name: "FAKE".to_string(),
                    chip_type: ExpansionChipType::SVP,
                    description: String::new(),
                    features: Vec::new(),
                },
                label: "FAKE".to_string(),
                accesses: 0,
                latched: 0,
                battery: None,
                battery_dirty: false,
            }
        }

        fn with_battery(mut self, len: usize) -> Self {
            self.battery = Some(vec![0xFF; len]);
            self
        }
    }

    impl Coprocessor for FakeChip {
        fn info(&self) -> &ExpansionChipInfo {
            &self.info
        }
        fn read(&mut self, offset: u32, _mc: u64) -> u8 {
            self.accesses += 1;
            0xC0u8.wrapping_add(offset as u8)
        }
        fn write(&mut self, offset: u32, value: u8, _mc: u64) {
            self.accesses += 1;
            self.latched = value;
            if let Some(b) = self.battery.as_mut() {
                if let Some(slot) = b.get_mut(offset as usize) {
                    *slot = value;
                    self.battery_dirty = true;
                }
            }
        }
        fn reset(&mut self, _hz: u64) {
            self.accesses = 0;
            self.latched = 0;
        }
        fn serialize(&self) -> Vec<u8> {
            if self.latched == 0 {
                return Vec::new();
            }
            let state = GenesisCoprocessorState::JCart(JCartState {
                port3_data: self.latched,
                ..Default::default()
            });
            bincode::serialize(&state).unwrap_or_default()
        }
        fn restore(&mut self, data: &[u8], _hz: u64) {
            if let Ok(GenesisCoprocessorState::JCart(s)) =
                bincode::deserialize::<GenesisCoprocessorState>(data)
            {
                self.latched = s.port3_data;
            }
        }
        fn debug_state(&self) -> ExpansionChipState {
            ExpansionChipState {
                chip_type: self.info.chip_type,
                registers: vec![ChipRegister {
                    address: 0,
                    name: "latched".to_string(),
                    value: u32::from(self.latched),
                    description: String::new(),
                }],
                internal_ram: None,
                program_counter: None,
                cycle_count: self.accesses,
            }
        }
        fn label(&self) -> &str {
            &self.label
        }
        fn accesses(&self) -> u64 {
            self.accesses
        }
        fn battery(&self) -> Option<&[u8]> {
            self.battery.as_deref()
        }
        fn restore_battery(&mut self, data: &[u8]) {
            if let Some(b) = self.battery.as_mut() {
                let n = data.len().min(b.len());
                b[..n].copy_from_slice(&data[..n]);
            }
        }
        fn battery_dirty(&self) -> bool {
            self.battery_dirty
        }
        fn clear_battery_dirty(&mut self) {
            self.battery_dirty = false;
        }
    }

    fn test_info() -> CartridgeInfo {
        use retro_core::cartridge::{ConsoleSystem, DetectionSource, SRAMType};
        CartridgeInfo {
            system: ConsoleSystem::Genesis,
            title: "COMPOSABLE".to_string(),
            serial: None,
            regions: Vec::new(),
            rom_size: 0x200000,
            sram_size: 0,
            sram_type: SRAMType::None,
            mapper_info: None,
            expansion_chip_info: None,
            header_checksum: None,
            calculated_checksum: None,
            rom_hash: String::new(),
            detection_source: DetectionSource::Header,
        }
    }

    fn rom() -> Arc<[u8]> {
        let mut r = vec![0u8; 0x200000];
        r[0x100] = b'S';
        Arc::from(r.into_boxed_slice())
    }

    fn plan_with_chip_window() -> MapPlan {
        let mut plan = MapPlan::new("fake");
        // A chip MMIO window ahead of the ROM window.
        plan.push(MapSlot::new(
            0x300000..=0x30FFFF,
            MapTarget::ChipMmio {
                chip_id: 0,
                base: 0,
            },
        ));
        plan.push(
            MapSlot::new(0x000000..=0x3FFFFF, MapTarget::Rom { base_offset: 0 })
                .with_mask(0x1FFFFF),
        );
        plan
    }

    #[test]
    fn rom_read_and_chip_dispatch() {
        let mut chips = ChipSet::default();
        chips.push(Box::new(FakeChip::new()));
        let mut c = ComposableCartridge::new(rom(), plan_with_chip_window(), chips, test_info());

        assert_eq!(c.read_addr(0x000100), b'S');
        // Chip window: read returns 0xC0 + offset.
        assert_eq!(c.read_addr(0x300000), 0xC0);
        assert_eq!(c.read_addr(0x300005), 0xC5);
        assert_eq!(c.coprocessor_label(), Some("FAKE"));
        assert!(c.coprocessor_accesses() >= 2);
    }

    #[test]
    fn savestate_bridge_round_trips_typed_state() {
        let mut chips = ChipSet::default();
        chips.push(Box::new(FakeChip::new()));
        let mut a = ComposableCartridge::new(rom(), plan_with_chip_window(), chips, test_info());

        assert!(a.coprocessors_savestate().is_empty(), "nothing latched yet");
        a.write_addr(0x300000, 0x2A);
        let saved = a.coprocessors_savestate();
        assert!(
            matches!(saved.as_slice(), [(0, GenesisCoprocessorState::JCart(s))] if s.port3_data == 0x2A)
        );

        let mut fresh = ChipSet::default();
        fresh.push(Box::new(FakeChip::new()));
        let mut b = ComposableCartridge::new(rom(), plan_with_chip_window(), fresh, test_info());
        b.restore_coprocessors_savestate(&saved, 53_693_175);
        assert_eq!(
            b.coprocessor_debug_state().unwrap().registers[0].value,
            0x2A
        );
    }

    #[test]
    fn battery_routing_to_chip_when_no_stock_sram() {
        let mut chips = ChipSet::default();
        chips.push(Box::new(FakeChip::new().with_battery(128)));
        let mut plan = MapPlan::new("eeprom-like");
        plan.push(MapSlot::new(
            0x200001..=0x200001,
            MapTarget::ChipMmio {
                chip_id: 0,
                base: 0,
            },
        ));
        let mut c = ComposableCartridge::new(rom(), plan, chips, test_info()).with_battery_chip(0);

        // The cartridge's SRAM view is the chip's battery.
        assert_eq!(c.sram().len(), 128);
        assert!(!c.is_sram_dirty());

        // A chip write marks the battery dirty; the cart reports it.
        c.write_addr(0x200001, 0x77);
        assert!(c.is_sram_dirty());
        assert_eq!(c.sram()[0], 0x77);

        c.clear_sram_dirty();
        assert!(!c.is_sram_dirty());

        // Bulk restore routes into the chip battery.
        c.restore_sram(&[0x01, 0x02, 0x03]);
        assert_eq!(c.sram()[0], 0x01);
        assert_eq!(c.sram()[2], 0x03);
    }

    #[test]
    fn stock_sram_buffer_takes_precedence_over_battery_chip() {
        let mut chips = ChipSet::default();
        chips.push(Box::new(FakeChip::new().with_battery(128)));
        let mut plan = MapPlan::new("mapper+sram");
        plan.push(MapSlot::new(
            0x200000..=0x20FFFF,
            MapTarget::Sram { base_offset: 0 },
        ));
        let c = ComposableCartridge::new(rom(), plan, chips, test_info())
            .with_sram(vec![0u8; 0x8000], 0x200000);
        // Own SRAM buffer wins; the chip battery is ignored for `.srm`.
        assert_eq!(c.sram().len(), 0x8000);
        assert!(c.sram_enabled(), "SRAM beyond ROM image is permanent");
    }

    #[test]
    fn irq_and_defaults_delegate_to_primary_chip() {
        let chips = ChipSet::default();
        let c = ComposableCartridge::new(rom(), plan_with_chip_window(), chips, test_info());
        // No chips -> no coprocessor surface, no IRQ.
        assert!(!c.has_chip_irq());
        assert!(c.coprocessor_labels().is_empty());
        assert!(c.coprocessors_debug_state().is_empty());
        // Prove SvpState is a usable savestate variant end-to-end.
        let st = GenesisCoprocessorState::Svp(SvpState::default());
        let bytes = bincode::serialize(&st).unwrap();
        assert!(bincode::deserialize::<GenesisCoprocessorState>(&bytes).is_ok());
    }
}
