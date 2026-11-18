//! The Mega Drive's part of a cartridge: the SRAM switch.
//!
//! A cartridge's battery SRAM sits in one flat buffer over its window,
//! indexed by `addr - start` (the layout Genesis Plus GX's save files
//! have). It has two regimes, as on the hardware (checked against Genesis
//! Plus GX):
//!
//! - **Permanent**: SRAM whose window starts at or beyond the mirrored ROM
//!   image is always mapped; `$A130F1` does not gate it.
//! - **Overlapping**: SRAM whose window overlaps the ROM image starts
//!   switched off (the ROM shows through) and is switched by bit 0 of a
//!   write to `$A130F1`. Switched off, a read of the window returns the
//!   ROM byte behind it, mirrored, and a write is dropped.
//!
//! The switch's register reads back through the /TIME window
//! ([`Board::read_register`]): it lives outside the cartridge window, on
//! the `$A1` page. Everything else of a cartridge is retro-core's
//! [`ComposableCartridge`], which this board completes as
//! [`GenesisCartridge`].

use retro_core::cartridge::{Board, ComposableCartridge, Coprocessor, SourcePlace};

use super::map_plan::{rom_mirror_mask, MapSlot};
use crate::savestate::GenesisChipState;

/// The /TIME register the SEGA mapper protocol uses for the SRAM switch
/// (`$A130F1`, bit 0).
pub const SRAM_CONTROL_ADDR: u32 = 0xA130F1;

/// A Mega Drive cartridge: retro-core's composable cartridge over the
/// Mega Drive's board.
pub type GenesisCartridge = ComposableCartridge<GenesisBoard>;

/// The SRAM switch of a Mega Drive cartridge, and the ROM mirror the
/// switched-off window shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GenesisBoard {
    /// The power-of-two mask the ROM mirrors through.
    rom_mask: u32,
    /// The cartridge has an SRAM buffer.
    has_sram: bool,
    /// The 68000 address the SRAM window starts at, word-aligned.
    sram_start: u32,
    /// The SRAM answers its window now.
    sram_enabled: bool,
    /// The SRAM starts past the ROM image, so `$A130F1` does not switch it.
    sram_permanent: bool,
}

impl GenesisBoard {
    /// The board of a cartridge with a ROM of `rom_len` bytes and, when
    /// `sram` names one, an SRAM window `(start, length)`: permanent and
    /// switched on when it starts at or beyond the mirrored image,
    /// overlapping and switched off otherwise.
    pub fn new(rom_len: usize, sram: Option<(u32, u32)>) -> Self {
        let rom_mask = rom_mirror_mask(rom_len);
        let Some((start, len)) = sram else {
            return Self {
                rom_mask,
                has_sram: false,
                sram_start: 0,
                sram_enabled: false,
                sram_permanent: false,
            };
        };
        let sram_start = start & !1;
        let sram_permanent = sram_start >= rom_mask.wrapping_add(1);
        Self {
            rom_mask,
            has_sram: len > 0,
            sram_start,
            sram_enabled: sram_permanent,
            sram_permanent,
        }
    }

    /// Whether the SRAM answers its window now (the debugger, and the
    /// `$A130F1` read).
    pub fn sram_enabled(&self) -> bool {
        self.sram_enabled
    }
}

impl Board for GenesisBoard {
    type Slot = MapSlot;
    type Chip = dyn Coprocessor;
    type ChipState = GenesisChipState;

    /// The 68000's undriven data bus reads `0xFF`.
    const MISSING: Option<u8> = Some(0xFF);

    fn read_sram(&self, rom: &[u8], sram: &[u8], offset: u32, missing: u8) -> u8 {
        if !self.has_sram {
            return missing;
        }
        if self.sram_enabled {
            sram.get(offset as usize).copied().unwrap_or(missing)
        } else {
            // Switched off: the mirrored ROM shows through.
            let addr = self.sram_start.wrapping_add(offset);
            rom.get((addr & self.rom_mask) as usize)
                .copied()
                .unwrap_or(missing)
        }
    }

    fn sram_writable(&self) -> bool {
        self.sram_enabled
    }

    fn sram_source_place(&self, rom_len: usize, offset: u32) -> SourcePlace {
        if !self.has_sram {
            SourcePlace::Unknown
        } else if self.sram_enabled {
            SourcePlace::CartridgeRam
        } else {
            let offset = self.sram_start.wrapping_add(offset) & self.rom_mask;
            if (offset as usize) < rom_len {
                SourcePlace::Rom(offset)
            } else {
                SourcePlace::Unknown
            }
        }
    }

    fn observe_write(&mut self, addr: u32, value: u8) {
        // The switch exists only for overlapping SRAM.
        if addr == SRAM_CONTROL_ADDR && self.has_sram && !self.sram_permanent {
            self.sram_enabled = value & 0x01 != 0;
        }
    }

    fn observes_bus(&self) -> bool {
        self.has_sram && !self.sram_permanent
    }

    fn read_register(&self, addr: u32) -> Option<u8> {
        (addr == SRAM_CONTROL_ADDR && self.has_sram).then_some(u8::from(self.sram_enabled))
    }

    fn reset(&mut self) {
        self.sram_enabled = self.sram_permanent;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use retro_core::cartridge::{
        Cartridge, CartridgeInfo, ChipSet, ConsoleSystem, ExpansionChipInfo, ExpansionChipState,
        ExpansionChipType,
    };

    use super::*;
    use crate::cartridge::chips::BankMapperChip;
    use crate::cartridge::map_plan::{MapPlan, MapTarget};
    use crate::cartridge::presets::{bank_mapper_plan, plain_plan};
    use crate::savestate::BankMapperKind;

    /// A plain cartridge of `rom` with SRAM over `sram`, as the builder
    /// makes one (a fresh SRAM holds `0xFF`).
    fn plain(rom: Vec<u8>, sram: Option<(u32, u32)>) -> GenesisCartridge {
        let len = rom.len();
        let rom: Arc<[u8]> = Arc::from(rom.into_boxed_slice());
        let cart = GenesisCartridge::new(
            Arc::clone(&rom),
            plain_plan(len, sram),
            ChipSet::default(),
            CartridgeInfo::new(ConsoleSystem::Genesis, &rom),
            GenesisBoard::new(len, sram),
        );
        match sram {
            Some((_, size)) => cart.with_sram(vec![0xFF; size as usize]),
            None => cart,
        }
    }

    fn rom_with_marker(len: usize) -> Vec<u8> {
        let mut rom = vec![0u8; len];
        rom[0x100] = b'S';
        rom[0x123] = 0x42;
        rom
    }

    fn read(cart: &mut GenesisCartridge, addr: u32) -> u8 {
        cart.read_bus(addr, 0, 0xFF)
    }

    fn write(cart: &mut GenesisCartridge, addr: u32, value: u8) {
        cart.write_bus(addr, value, 0)
    }

    #[test]
    fn a_rom_reads_mirrors_and_drops_writes() {
        let mut c = plain(rom_with_marker(0x80000), None);
        assert_eq!(c.map_plan().name, "plain");
        assert_eq!(read(&mut c, 0x000100), b'S');
        // A 512 KiB ROM mirrors across the 4 MiB window.
        assert_eq!(read(&mut c, 0x080123), 0x42);
        write(&mut c, 0x000123, 0x99);
        assert_eq!(read(&mut c, 0x000123), 0x42, "the ROM is read-only");
        assert!(!c.observes_bus());
        assert!(c.sram().is_empty());
        assert_eq!(read(&mut c, 0x400000), 0xFF, "no slot");
    }

    #[test]
    fn permanent_sram_beyond_the_rom_is_always_mapped() {
        let mut c = plain(rom_with_marker(0x200000), Some((0x200000, 0x8000)));
        assert!(!c.observes_bus(), "permanent SRAM is not switched");
        assert!(c.board().sram_enabled());
        write(&mut c, 0x200000, 0xAB);
        write(&mut c, 0x207FFF, 0xCD);
        assert_eq!(read(&mut c, 0x200000), 0xAB);
        assert_eq!(read(&mut c, 0x207FFF), 0xCD);
        assert!(c.is_sram_dirty());
        assert_eq!(c.sram().len(), 0x8000);
        c.clear_sram_dirty();
        assert!(!c.is_sram_dirty());
    }

    #[test]
    fn overlapping_sram_is_switched_by_a130f1() {
        let mut rom = vec![0u8; 0x400000];
        rom[0x200000] = 0x11;
        let mut c = plain(rom, Some((0x200000, 0x8000)));
        assert!(c.observes_bus(), "overlapping SRAM watches $A130F1");
        assert!(!c.board().sram_enabled());
        assert_eq!(c.read_register(SRAM_CONTROL_ADDR), Some(0));
        // Switched off: the ROM shows through, a write is dropped.
        assert_eq!(read(&mut c, 0x200000), 0x11);
        write(&mut c, 0x200000, 0x22);
        assert_eq!(read(&mut c, 0x200000), 0x11);
        assert!(!c.is_sram_dirty());

        write(&mut c, SRAM_CONTROL_ADDR, 0x01);
        assert!(c.board().sram_enabled());
        assert_eq!(c.read_register(SRAM_CONTROL_ADDR), Some(1));
        write(&mut c, 0x200000, 0x22);
        assert_eq!(read(&mut c, 0x200000), 0x22);

        // Switched off again: the ROM shows through, the SRAM keeps its bytes.
        write(&mut c, SRAM_CONTROL_ADDR, 0x00);
        assert_eq!(read(&mut c, 0x200000), 0x11);
        write(&mut c, SRAM_CONTROL_ADDR, 0x01);
        assert_eq!(read(&mut c, 0x200000), 0x22);
        assert_eq!(c.read_register(0xA130F3), None, "only $A130F1 answers");
    }

    #[test]
    fn a_reset_puts_the_switch_back() {
        let mut c = plain(vec![0u8; 0x400000], Some((0x200000, 0x8000)));
        write(&mut c, SRAM_CONTROL_ADDR, 0x01);
        write(&mut c, 0x200000, 0x22);
        c.reset();
        assert!(!c.board().sram_enabled());
        assert!(!c.is_sram_dirty());
    }

    #[test]
    fn a_byte_s_source_follows_the_sram_switch_and_the_mirror() {
        let mut overlapping = plain(vec![0u8; 0x400000], Some((0x200000, 0x8000)));
        let places = |c: &GenesisCartridge, addrs: &[u32]| {
            addrs.iter().map(|&a| c.source_place(a)).collect::<Vec<_>>()
        };
        assert_eq!(
            places(&overlapping, &[0x000123, 0x200010, 0x3FFFFF, 0x400000]),
            [
                SourcePlace::Rom(0x123),
                SourcePlace::Rom(0x200010),
                SourcePlace::Rom(0x3FFFFF),
                SourcePlace::Unknown,
            ],
            "switched off, the ROM shows through the SRAM window"
        );
        write(&mut overlapping, SRAM_CONTROL_ADDR, 0x01);
        assert_eq!(
            places(&overlapping, &[0x200010]),
            [SourcePlace::CartridgeRam]
        );

        let permanent = plain(rom_with_marker(0x80000), Some((0x200000, 0x4000)));
        assert_eq!(
            places(&permanent, &[0x080123, 0x200000, 0x204000]),
            [
                SourcePlace::Rom(0x123),
                SourcePlace::CartridgeRam,
                SourcePlace::Rom(0x4000),
            ],
            "the ROM mirrored past its end; the SRAM beyond it"
        );
    }

    #[test]
    fn a_bank_mapper_names_the_rom_through_its_banks_as_they_stand() {
        let rom: Arc<[u8]> = Arc::from(vec![0u8; 0x500000].into_boxed_slice());
        let mut chips = ChipSet::default();
        let id = chips.push(Box::new(BankMapperChip::sega(Arc::clone(&rom))));
        chips.add_observer(0xA130F0..=0xA130FF, id);
        let mut banked = GenesisCartridge::new(
            Arc::clone(&rom),
            bank_mapper_plan(BankMapperKind::Sega, id, None),
            chips,
            CartridgeInfo::new(ConsoleSystem::Genesis, &[]),
            GenesisBoard::new(rom.len(), None),
        );
        assert_eq!(banked.source_place(0x300010), SourcePlace::Rom(0x300010));
        assert_eq!(banked.source_place(0x000020), SourcePlace::Rom(0x20));
        banked.observe_bus_write(0xA130F6, 9);
        assert_eq!(banked.source_place(0x300010), SourcePlace::Rom(0x480010));
        assert_eq!(banked.source_place(0x37FFFF), SourcePlace::Rom(0x4FFFFF));
        assert_eq!(banked.coprocessor_accesses(), 0, "nothing was read");
    }

    #[test]
    fn the_chips_states_are_the_consoles_typed_list() {
        let rom: Arc<[u8]> = Arc::from(vec![0u8; 0x500000].into_boxed_slice());
        let banked = |rom: &Arc<[u8]>| {
            let mut chips = ChipSet::default();
            let id = chips.push(Box::new(BankMapperChip::sega(Arc::clone(rom))));
            chips.add_observer(0xA130F0..=0xA130FF, id);
            GenesisCartridge::new(
                Arc::clone(rom),
                bank_mapper_plan(BankMapperKind::Sega, id, None),
                chips,
                CartridgeInfo::new(ConsoleSystem::Genesis, &[]),
                GenesisBoard::new(rom.len(), None),
            )
        };
        let mut saved_from = banked(&rom);
        saved_from.observe_bus_write(0xA130F6, 9);
        let saved = saved_from.coprocessors_savestate();
        assert!(matches!(
            saved.as_slice(),
            [(0, GenesisChipState::BankMapper(state))] if state.banks[6] == 9
        ));
        let mut restored = banked(&rom);
        restored.restore_coprocessors_savestate(&saved, 53_693_175);
        assert_eq!(restored.source_place(0x300010), SourcePlace::Rom(0x480010));
    }

    #[test]
    fn restore_sram_copies_the_prefix_both_hold() {
        let mut c = plain(rom_with_marker(0x200000), Some((0x200000, 0x8000)));
        c.restore_sram(&[0x01, 0x02, 0x03]);
        assert_eq!(read(&mut c, 0x200000), 0x01);
        assert_eq!(read(&mut c, 0x200002), 0x03);
        assert_eq!(read(&mut c, 0x200003), 0xFF, "the rest kept");
    }

    /// A chip with a battery, for the save's routing.
    struct BatteryChip {
        info: ExpansionChipInfo,
        battery: Vec<u8>,
    }

    impl Coprocessor for BatteryChip {
        fn info(&self) -> &ExpansionChipInfo {
            &self.info
        }
        fn read(&mut self, _offset: u32, _master_cycle: u64) -> u8 {
            0
        }
        fn write(&mut self, _offset: u32, _value: u8, _master_cycle: u64) {}
        fn reset(&mut self, _master_clock_hz: u64) {}
        fn serialize(&self) -> Vec<u8> {
            Vec::new()
        }
        fn restore(&mut self, _data: &[u8], _master_clock_hz: u64) {}
        fn debug_state(&self) -> ExpansionChipState {
            ExpansionChipState {
                chip_type: self.info.chip_type,
                registers: Vec::new(),
                internal_ram: None,
                program_counter: None,
                cycle_count: 0,
            }
        }
        fn label(&self) -> &str {
            "battery"
        }
        fn battery(&self) -> Option<&[u8]> {
            Some(&self.battery)
        }
    }

    #[test]
    fn a_battery_chip_not_named_leaves_the_stock_sram_the_save() {
        let mut chips = ChipSet::default();
        chips.push(Box::new(BatteryChip {
            info: ExpansionChipInfo {
                name: "battery".into(),
                chip_type: ExpansionChipType::Custom(1),
                description: String::new(),
                features: Vec::new(),
            },
            battery: vec![0; 128],
        }));
        let mut plan = MapPlan::new("mapper+sram");
        plan.push(MapSlot::new(
            0x200000..=0x20FFFF,
            MapTarget::Sram { base_offset: 0 },
        ));
        let rom: Arc<[u8]> = Arc::from(vec![0u8; 0x200000].into_boxed_slice());
        let c = GenesisCartridge::new(
            rom,
            plan,
            chips,
            CartridgeInfo::new(ConsoleSystem::Genesis, &[]),
            GenesisBoard::new(0x200000, Some((0x200000, 0x8000))),
        )
        .with_sram(vec![0u8; 0x8000]);
        assert_eq!(c.sram().len(), 0x8000, "the stock buffer is the save");
        assert!(c.board().sram_enabled(), "SRAM past the ROM is permanent");
    }

    #[test]
    fn a_cartridge_with_no_chip_reports_no_coprocessor() {
        let c = plain(rom_with_marker(0x80000), None);
        assert!(c.coprocessor_label().is_none());
        assert_eq!(c.coprocessor_accesses(), 0);
        assert!(c.coprocessors_savestate().is_empty());
        assert!(c.coprocessor_debug_state().is_none());
        assert!(!c.has_irq());
    }
}
