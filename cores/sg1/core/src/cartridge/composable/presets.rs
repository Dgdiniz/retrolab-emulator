//! Stock map-plan generators for the composable Genesis cartridges.
//!
//! A preset builds the [`MapPlan`] for one class of cartridge; the
//! [`builder`](super::builder) then attaches the matching chips,
//! observers, and SRAM configuration to a
//! [`ComposableCartridge`](super::ComposableCartridge). A plain cartridge
//! built by detection needs no preset: [`PlainCartridge`](super::PlainCartridge)
//! builds its own plan ([`plain_plan`] serves a manifest's `plain` preset).
//!
//! Slot ordering matters: [`MapPlan::resolve`] is first-wins, so a preset
//! pushes the *specific* windows (chip MMIO, SRAM) ahead of the broad ROM
//! window they carve out of.

use std::ops::RangeInclusive;

use super::map_plan::{rom_mirror_mask, MapPlan, MapSlot, MapTarget};
use super::savestate::BankMapperKind;

/// The two-wire serial-EEPROM control byte most Genesis EEPROM carts use.
pub const EEPROM_CONTROL_ADDR: u32 = 0x200001;

/// MMIO base assigned to the J-Cart port-4 window (kept in sync with
/// [`chips::jcart::JCART_PORT4_BASE`](super::chips::jcart::JCART_PORT4_BASE)).
pub const JCART_PORT4_BASE: u32 = 0x40;

/// First byte of the SVP 68000-side mailbox (`0xA15000-0xA1500B`: XST, the
/// XST status, and PM0/halt), which lives on the I/O page, *outside* the
/// cartridge ROM window, so it is dispatched to the SVP chip by the bus's
/// `0xA150xx` sub-decode rather than by the ROM page.
pub const SVP_MAILBOX_ADDR: u32 = 0xA15000;

/// Chip-offset base the SVP mailbox window maps to, chosen well past the
/// DRAM image so the chip tells mailbox accesses (`>= SVP_MAILBOX_BASE`)
/// apart from DRAM accesses (`< SVP_DRAM_LEN`).
pub const SVP_MAILBOX_BASE: u32 = 0x10_0000;

/// Chip-offset bases the two SVP cell-arrange views map to. The 68000 reads
/// permuted, read-only "tile" views of DRAM at `0x390000` (view 1) and
/// `0x3A0000` (view 2); each maps to its own chip-offset window so the chip
/// applies the matching address permutation and tells them apart from linear
/// DRAM and from the mailbox. Chosen disjoint from all other SVP windows.
pub const SVP_CELL1_BASE: u32 = 0x20_0000;
/// See [`SVP_CELL1_BASE`]; base for the second (`0x3A0000`) cell-arrange view.
pub const SVP_CELL2_BASE: u32 = 0x30_0000;

/// The full 68000 cartridge ROM window (`0x000000-0x3FFFFF`).
const ROM_WINDOW: RangeInclusive<u32> = 0x000000..=0x3FFFFF;

/// Which stock cartridge shape a build resolves to. Also the value a
/// manifest's `mapping.preset` string maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetKind {
    /// ROM + optional SRAM, no coprocessor.
    Plain,
    /// SEGA bank mapper (ROMs > 4 MiB).
    Sega,
    /// Super Street Fighter II mapper.
    Ssf2,
    /// Codemasters mapper.
    Codemasters,
    /// Serial EEPROM.
    Eeprom,
    /// J-Cart controller ports.
    JCart,
    /// Sega Virtua Processor.
    Svp,
    /// Manifest-defined; start from an empty plan.
    Custom,
}

/// A ROM slot covering the full cartridge window, mirroring an under-sized
/// image via the power-of-two mask.
fn rom_slot(rom_len: usize) -> MapSlot {
    MapSlot::new(ROM_WINDOW, MapTarget::Rom { base_offset: 0 }).with_mask(rom_mirror_mask(rom_len))
}

/// A stock SRAM slot over `[start, start + len)`.
fn sram_slot(start: u32, len: u32) -> MapSlot {
    MapSlot::new(
        start..=start.saturating_add(len.max(1) - 1),
        MapTarget::Sram { base_offset: 0 },
    )
}

/// Plan for a plain cartridge (ROM + optional stock SRAM, no coprocessor).
/// [`PlainCartridge`](super::PlainCartridge) builds this same shape
/// internally; this free function exists so the manifest path can request a
/// plain plan without going through the concrete cartridge.
pub fn plain_plan(rom_len: usize, sram: Option<(u32, u32)>) -> MapPlan {
    let mut plan = MapPlan::new("plain");
    if let Some((start, len)) = sram {
        plan.push(sram_slot(start, len));
    }
    plan.push(rom_slot(rom_len));
    plan
}

/// Plan for a bank-mapped cartridge (SEGA / SSF2 / Codemasters): the whole
/// ROM window dispatches to the mapper chip, which translates through its
/// bank table. An optional stock SRAM window is carved out ahead of it.
pub fn bank_mapper_plan(kind: BankMapperKind, chip_id: u8, sram: Option<(u32, u32)>) -> MapPlan {
    let name = match kind {
        BankMapperKind::Sega => "sega",
        BankMapperKind::Ssf2 => "ssf2",
        BankMapperKind::Codemasters => "codemasters",
    };
    let mut plan = MapPlan::new(name);
    if let Some((start, len)) = sram {
        plan.push(sram_slot(start, len));
    }
    plan.push(MapSlot::new(
        ROM_WINDOW,
        MapTarget::ChipMmio { chip_id, base: 0 },
    ));
    plan
}

/// Plan for a serial-EEPROM cartridge: the single control byte dispatches
/// to the EEPROM chip; the rest of the window is stock ROM.
pub fn eeprom_plan(chip_id: u8, rom_len: usize, control_addr: u32) -> MapPlan {
    let mut plan = MapPlan::new("eeprom");
    plan.push(MapSlot::new(
        control_addr..=control_addr,
        MapTarget::ChipMmio { chip_id, base: 0 },
    ));
    plan.push(rom_slot(rom_len));
    plan
}

/// Plan for a J-Cart cartridge: the two controller-port windows dispatch to
/// the J-Cart chip (port 4 via [`JCART_PORT4_BASE`] so the chip tells the
/// ports apart), an optional SRAM window is carved out, then stock ROM.
pub fn jcart_plan(chip_id: u8, rom_len: usize, sram: Option<(u32, u32)>) -> MapPlan {
    let mut plan = MapPlan::new("jcart");
    plan.push(MapSlot::new(
        0x38FFFE..=0x38FFFF,
        MapTarget::ChipMmio { chip_id, base: 0 },
    ));
    plan.push(MapSlot::new(
        0x3FFFFE..=0x3FFFFF,
        MapTarget::ChipMmio {
            chip_id,
            base: JCART_PORT4_BASE,
        },
    ));
    if let Some((start, len)) = sram {
        plan.push(sram_slot(start, len));
    }
    plan.push(rom_slot(rom_len));
    plan
}

/// Plan for an SVP cartridge: the memory map the SVP presents to the 68000
/// (the map PicoDrive sets up). All coprocessor windows dispatch to the SVP
/// chip, each at a distinct chip-offset base so the chip decodes them:
///
/// | 68000 range         | chip base           | meaning                     |
/// |---------------------|---------------------|-----------------------------|
/// | `0x000000-0x1FFFFF`  | ROM                 | cartridge ROM (SSP program) |
/// | `0x300000-0x31FFFF`  | `0`                 | linear DRAM (banks 30/31)   |
/// | `0x390000-0x39FFFF`  | [`SVP_CELL1_BASE`]  | cell-arrange view 1 (RO)    |
/// | `0x3A0000-0x3AFFFF`  | [`SVP_CELL2_BASE`]  | cell-arrange view 2 (RO)    |
/// | `0xA15000-0xA1500B`  | [`SVP_MAILBOX_BASE`]| XST/PM0 command mailbox     |
///
/// Slot order is specific-before-broad so [`MapPlan::resolve`]'s first-wins
/// picks the coprocessor windows out of the ROM window they sit apart from.
pub fn svp_plan(chip_id: u8, rom_len: usize) -> MapPlan {
    let mut plan = MapPlan::new("svp");
    // Mailbox on the I/O page, dispatched to the chip at its own base.
    plan.push(MapSlot::new(
        SVP_MAILBOX_ADDR..=SVP_MAILBOX_ADDR + 0x0B,
        MapTarget::ChipMmio {
            chip_id,
            base: SVP_MAILBOX_BASE,
        },
    ));
    // Linear DRAM window (banks 0x30/0x31) at chip base 0.
    plan.push(MapSlot::new(
        0x300000..=0x31FFFF,
        MapTarget::ChipMmio { chip_id, base: 0 },
    ));
    // Two read-only cell-arrange DRAM views (banks 0x39/0x3A), each at its
    // own chip base so the chip applies the matching permutation.
    plan.push(MapSlot::new(
        0x390000..=0x39FFFF,
        MapTarget::ChipMmio {
            chip_id,
            base: SVP_CELL1_BASE,
        },
    ));
    plan.push(MapSlot::new(
        0x3A0000..=0x3AFFFF,
        MapTarget::ChipMmio {
            chip_id,
            base: SVP_CELL2_BASE,
        },
    ));
    plan.push(
        MapSlot::new(0x000000..=0x1FFFFF, MapTarget::Rom { base_offset: 0 })
            .with_mask(rom_mirror_mask(rom_len)),
    );
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_plan_rom_only_and_with_sram() {
        let rom_only = plain_plan(0x80000, None);
        assert_eq!(rom_only.name, "plain");
        assert_eq!(rom_only.slots.len(), 1);
        assert!(matches!(
            rom_only.slots[0].target,
            MapTarget::Rom { base_offset: 0 }
        ));

        let with_sram = plain_plan(0x200000, Some((0x200000, 0x8000)));
        assert_eq!(with_sram.slots.len(), 2);
        let s = with_sram.resolve(0x200004).unwrap();
        assert!(matches!(
            with_sram.slots[s.slot_index as usize].target,
            MapTarget::Sram { .. }
        ));
        assert_eq!(s.target_offset, 4);
    }

    #[test]
    fn bank_mapper_plan_routes_rom_window_to_chip() {
        let plan = bank_mapper_plan(BankMapperKind::Sega, 0, None);
        assert_eq!(plan.name, "sega");
        let r = plan.resolve(0x123456).unwrap();
        assert!(matches!(
            plan.slots[r.slot_index as usize].target,
            MapTarget::ChipMmio {
                chip_id: 0,
                base: 0
            }
        ));
        assert_eq!(r.target_offset, 0x123456);
    }

    #[test]
    fn bank_mapper_plan_sram_wins_over_rom_window() {
        let plan = bank_mapper_plan(BankMapperKind::Sega, 0, Some((0x200000, 0x8000)));
        let r = plan.resolve(0x200010).unwrap();
        assert!(matches!(
            plan.slots[r.slot_index as usize].target,
            MapTarget::Sram { .. }
        ));
        assert_eq!(r.target_offset, 0x10);
    }

    #[test]
    fn eeprom_plan_control_byte_wins() {
        let plan = eeprom_plan(0, 0x80000, EEPROM_CONTROL_ADDR);
        let r = plan.resolve(EEPROM_CONTROL_ADDR).unwrap();
        assert!(matches!(
            plan.slots[r.slot_index as usize].target,
            MapTarget::ChipMmio {
                chip_id: 0,
                base: 0
            }
        ));
        // Neighbouring bytes are stock ROM.
        let rr = plan.resolve(0x200000).unwrap();
        assert!(matches!(
            plan.slots[rr.slot_index as usize].target,
            MapTarget::Rom { .. }
        ));
    }

    #[test]
    fn jcart_plan_ports_distinct_bases() {
        let plan = jcart_plan(0, 0x80000, None);
        let p3 = plan.resolve(0x38FFFE).unwrap();
        assert!(matches!(
            plan.slots[p3.slot_index as usize].target,
            MapTarget::ChipMmio {
                chip_id: 0,
                base: 0
            }
        ));
        let p4 = plan.resolve(0x3FFFFE).unwrap();
        assert!(matches!(
            plan.slots[p4.slot_index as usize].target,
            MapTarget::ChipMmio {
                chip_id: 0,
                base: JCART_PORT4_BASE
            }
        ));
        // 0x3FFFFE is intercepted; a ROM address in between resolves to ROM.
        let rom = plan.resolve(0x100000).unwrap();
        assert!(matches!(
            plan.slots[rom.slot_index as usize].target,
            MapTarget::Rom { .. }
        ));
    }

    #[test]
    fn svp_plan_dram_rom_and_mailbox() {
        let plan = svp_plan(0, 0x200000);
        assert_eq!(plan.name, "svp");
        let dram = plan.resolve(0x300010).unwrap();
        assert!(matches!(
            plan.slots[dram.slot_index as usize].target,
            MapTarget::ChipMmio {
                chip_id: 0,
                base: 0
            }
        ));
        assert_eq!(dram.target_offset, 0x10);
        let rom = plan.resolve(0x000100).unwrap();
        assert!(matches!(
            plan.slots[rom.slot_index as usize].target,
            MapTarget::Rom { .. }
        ));
        // Mailbox on the I/O page dispatches to the chip at SVP_MAILBOX_BASE.
        let mbox = plan.resolve(SVP_MAILBOX_ADDR + 4).unwrap();
        assert!(matches!(
            plan.slots[mbox.slot_index as usize].target,
            MapTarget::ChipMmio {
                chip_id: 0,
                base: SVP_MAILBOX_BASE
            }
        ));
        assert_eq!(mbox.target_offset, 4);
    }

    #[test]
    fn svp_plan_maps_cell_arrange_views() {
        let plan = svp_plan(0, 0x200000);
        // View 1 (0x390000) folds into the chip at SVP_CELL1_BASE.
        let c1 = plan.resolve(0x390010).unwrap();
        assert!(matches!(
            plan.slots[c1.slot_index as usize].target,
            MapTarget::ChipMmio {
                chip_id: 0,
                base: SVP_CELL1_BASE
            }
        ));
        assert_eq!(c1.target_offset, 0x10);
        // View 2 (0x3A0000) folds into the chip at SVP_CELL2_BASE.
        let c2 = plan.resolve(0x3A0020).unwrap();
        assert!(matches!(
            plan.slots[c2.slot_index as usize].target,
            MapTarget::ChipMmio {
                chip_id: 0,
                base: SVP_CELL2_BASE
            }
        ));
        assert_eq!(c2.target_offset, 0x20);
    }
}
