//! The SNES's part of a cartridge.
//!
//! A SNES cartridge is retro-core's [`ComposableCartridge`] over this board
//! ([`SnesCartridge`]): the ROM image, a battery RAM, the [`MapPlan`] the
//! 65C816's address space resolves through and the chip set. The board adds
//! nothing between an access and the byte it reaches: a SNES cartridge has
//! no RAM switch, no register outside its plan, and no write it watches
//! itself (a chip's observers are the chip set's). What it names is the
//! typed list of chip states the savestate holds ([`CoprocessorState`]) and
//! what the cartridge answers where it holds nothing: `0x00`, for a ROM read
//! past the image, a battery-RAM read past its size and a chip window with
//! no chip, as this core has always answered them; an address no slot
//! covers and an open-bus slot read the bus's memory data register.

use retro_core::cartridge::{Board, ComposableCartridge, Coprocessor};

use super::map_plan::MapSlot;
use crate::savestate::CoprocessorState;

/// A SNES cartridge: retro-core's composable cartridge over the SNES's
/// board.
pub type SnesCartridge = ComposableCartridge<SnesBoard>;

/// The SNES's board: its slot, its chip states, and the byte a cartridge
/// answers where it holds nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SnesBoard;

impl Board for SnesBoard {
    type Slot = MapSlot;
    type Chip = dyn Coprocessor;
    type ChipState = CoprocessorState;

    const MISSING: Option<u8> = Some(0x00);
}

/// A cartridge of `rom` with `sram_size` bytes of zeroed battery RAM over
/// `plan`, and no chip, for the tests that need a cartridge on a bus.
#[cfg(test)]
pub(crate) fn test_cartridge(
    rom: Vec<u8>,
    sram_size: usize,
    plan: super::MapPlan,
) -> SnesCartridge {
    let rom: std::sync::Arc<[u8]> = std::sync::Arc::from(rom.into_boxed_slice());
    let info =
        retro_core::cartridge::CartridgeInfo::new(retro_core::cartridge::ConsoleSystem::SNES, &rom);
    SnesCartridge::new(
        rom,
        plan,
        retro_core::cartridge::ChipSet::default(),
        info,
        SnesBoard,
    )
    .with_sram(vec![0u8; sram_size])
}

/// A cartridge of `rom` over `plan` with `chips` and no battery RAM, for the
/// tests that need a chip on a bus.
#[cfg(test)]
pub(crate) fn test_cartridge_with_chips(
    rom: Vec<u8>,
    plan: super::MapPlan,
    chips: retro_core::cartridge::ChipSet,
) -> SnesCartridge {
    let rom: std::sync::Arc<[u8]> = std::sync::Arc::from(rom.into_boxed_slice());
    let info =
        retro_core::cartridge::CartridgeInfo::new(retro_core::cartridge::ConsoleSystem::SNES, &rom);
    SnesCartridge::new(rom, plan, chips, info, SnesBoard)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use retro_core::cartridge::{Cartridge, ChipSet, MapTarget};

    use super::*;
    use crate::cartridge::{BusSpeed, MapPlan, Sdd1Chip, SDD1_CHIP_ID, SDD1_REG_WINDOW_BASE};

    fn plan() -> MapPlan {
        let mut plan = MapPlan::new("board-test");
        plan.push(MapSlot::new(
            0x80..=0xFF,
            0x8000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            BusSpeed::Fast,
        ));
        plan.push(MapSlot::new(
            0x70..=0x70,
            0x0000..=0x07FF,
            MapTarget::Sram { base_offset: 0 },
            BusSpeed::Slow,
        ));
        plan.push(MapSlot::new(
            0x30..=0x30,
            0x8000..=0x80FF,
            MapTarget::ChipMmio {
                chip_id: 4,
                base: 0,
            },
            BusSpeed::Fast,
        ));
        plan
    }

    #[test]
    fn what_the_cartridge_holds_nothing_for_reads_zero() {
        let mut rom = vec![0u8; 0x8000];
        rom[0x10] = 0x42;
        let mut cart = test_cartridge(rom, 0x800, plan());
        assert_eq!(cart.read_bus(0x80_8010, 0, 0xEE), 0x42);
        // The ROM is 32 KiB; bank $81 reaches past it.
        assert_eq!(cart.read_bus(0x81_8010, 0, 0xEE), 0x00, "past the image");
        // No chip sits at id 4.
        assert_eq!(cart.read_bus(0x30_8000, 0, 0xEE), 0x00, "no chip");
        assert_eq!(cart.peek_bus(0x30_8000, 0xEE), 0xEE, "a peek with no chip");
        // No slot covers bank $00's low half.
        assert_eq!(cart.read_bus(0x00_1000, 0, 0xEE), 0xEE, "no slot");
        cart.write_bus(0x70_0005, 0x5A, 0);
        assert_eq!(cart.read_bus(0x70_0005, 0, 0xEE), 0x5A);
        assert!(!cart.observes_bus(), "the board watches nothing");
        assert_eq!(cart.read_register(0x4800), None);
    }

    /// A *real* `Sdd1Chip` round-trips its mapping state through the
    /// cartridge's typed savestate bridge, exercising the
    /// `CoprocessorState::Sdd1` bincode path (MMC latches + enables + the
    /// `$43xx` DMA shadow).
    #[test]
    fn savestate_bridge_round_trips_real_sdd1_chip() {
        const HZ: u64 = 21_477_272;
        let rom = vec![0u8; 0x8000];
        let rom_arc: Arc<[u8]> = Arc::from(rom.clone().into_boxed_slice());

        let sdd1_chips = || {
            let mut chips = ChipSet::default();
            chips.insert(
                SDD1_CHIP_ID,
                Box::new(Sdd1Chip::new(Arc::clone(&rom_arc), HZ)),
            );
            chips.add_observer(0x4300..=0x437F, SDD1_CHIP_ID);
            chips
        };

        let mut a = test_cartridge_with_chips(rom.clone(), plan(), sdd1_chips());
        // Drive the MMC latch + enables through the register window and one
        // channel's DMA shadow through the $43xx observer.
        a.write_chip(SDD1_CHIP_ID, SDD1_REG_WINDOW_BASE + 0x4, 5, 0); // mmc[0] = 5
        a.write_chip(SDD1_CHIP_ID, SDD1_REG_WINDOW_BASE, 0x81, 0); // $4800 sdd1_enable
        a.write_chip(SDD1_CHIP_ID, SDD1_REG_WINDOW_BASE + 0x1, 0x01, 0); // xfer_enable
        for (reg, val) in [
            (0x2, 0x45),
            (0x3, 0x23),
            (0x4, 0xC1),
            (0x5, 0x34),
            (0x6, 0x12),
        ] {
            a.observe_bus_write(0x4300 + reg, val); // channel 0 src $C1_2345, size $1234
        }

        let saved = a.coprocessors_savestate();
        let orig = match saved.as_slice() {
            [(id, CoprocessorState::Sdd1(s))] if *id == SDD1_CHIP_ID => s.clone(),
            other => panic!("expected one S-DD1 entry, got {other:?}"),
        };
        assert_eq!(orig.mmc[0], 5 << 20);
        assert_eq!(orig.dma_addr[0], 0xC1_2345);
        assert_eq!(orig.dma_size[0], 0x1234);

        // Restore into a fresh cartridge and confirm the state came back.
        let mut b = test_cartridge_with_chips(rom, plan(), sdd1_chips());
        b.restore_coprocessors_savestate(&saved, HZ);
        let round = match b.coprocessors_savestate().as_slice() {
            [(_, CoprocessorState::Sdd1(s))] => s.clone(),
            other => panic!("expected one S-DD1 entry after restore, got {other:?}"),
        };
        assert_eq!(round, orig, "the S-DD1 state round-trips byte-for-byte");

        // The restored mapping surfaces in the debug state.
        let st = b.coprocessor_debug_state().expect("S-DD1 present");
        let mmc0 = st.registers.iter().find(|r| r.name == "MMC0").unwrap();
        assert_eq!(mmc0.value, 5);
        assert!(st
            .registers
            .iter()
            .any(|r| r.name == "DMA0" && r.value == 0xC1_2345));
    }
}
