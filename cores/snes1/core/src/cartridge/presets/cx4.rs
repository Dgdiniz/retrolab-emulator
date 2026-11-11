//! Cx4 (Hitachi HG51B169) static map (checked against bsnes-plus's
//! mapping of the board).
//!
//! The Cx4 shares cart ROM and its MMIO + internal data RAM window with the
//! console's S-CPU through a LoROM-shaped map. ROM, the `$6000-7FFF` window,
//! and (only when a manifest declares it) cart RAM all route to the one
//! [`Cx4Chip`](crate::cartridge::Cx4Chip) via [`MapTarget::ChipMmio`], split
//! apart by the `base` sentinel each slot carries (see `cartridge/cx4/mod.rs`
//! for the sentinel contract). ROM is chip-routed so the HG51B169's bus
//! arbitration can interpose while the DSP owns the cart bus; while the chip is
//! idle it returns the real ROM byte, so this is transparent for a booting
//! game.
//!
//! The S-CPU's view:
//!
//! - **ROM** (`base = CX4_ROM_BASE`): the LoROM windows `$00-3F:8000-FFFF` +
//!   `$80-BF:8000-FFFF` (32 KiB/bank). Unlike the Super FX, the Cx4 declares
//!   *no* linear windows, only the LoROM half-banks. The slot geometry is a
//!   plain-`Rom` clone, so `offset - CX4_ROM_BASE` is the ROM byte offset; the
//!   two windows are mirrors of the same image, so a `< 2 MiB` ROM carries a
//!   fold mask.
//! - **MMIO + data RAM** (`base = CX4_MMIO_BASE`): `$00-3F/$80-BF:6000-7FFF`,
//!   `0` bank stride, so an access arrives as `CX4_MMIO_BASE + (addr & 0x1FFF)`.
//!   The `$6000-7FFF` page defaults to open bus, so the bus's page-table walker
//!   promotes it to the cartridge, no B-bus-style carve-out is needed (like
//!   the Super FX `$3000` window, unlike the SA-1's `$2200-23FF`). The chip
//!   recovers the `$6000-7FFF` SNES offset and decodes the register file
//!   (`$7F40-7FFF`) vs the internal data RAM (`$6000-6BFF`/`$7000-7BFF`).
//! - **Cart RAM** (`base = CX4_RAM_BASE`, only when `ram_size > 0`): the
//!   `$70-77:0000-7FFF` window. The real MMX2/X3 carts declare no SRAM (they
//!   save by password), so this slot is absent for them; a manifest that sets
//!   `sram_size > 0` gets a battery-backed `.srm`. The chip folds modulo the
//!   RAM size, so it owns the buffer (the same reason the Super FX routes Game
//!   Pak RAM through its chip).

use std::ops::RangeInclusive;

use crate::cartridge::{
    BusSpeed, MapPlan, MapSlot, MapTarget, CX4_CHIP_ID, CX4_MMIO_BASE, CX4_RAM_BASE, CX4_ROM_BASE,
};

/// Cx4 ROM window capacity: the LoROM `$00-3F:8000-FFFF` window spans
/// 64 x 32 KiB = 2 MiB, Mega Man X3 (the larger of the two Cx4 games) fills it
/// exactly; Mega Man X2 (1.5 MiB, padded to 2 MiB) leaves the top quarter as
/// zero padding.
const CX4_ROM_BYTES: u32 = 0x20_0000;

/// Build the Cx4 plan.
///
/// `rom` is the padded ROM buffer (used only to size the fold mask). `ram_size`
/// is the cart RAM size in bytes (0 for "no RAM", the real MMX2/X3 case).
pub fn cx4_preset(rom: &[u8], ram_size: usize) -> MapPlan {
    let mut plan = MapPlan::new("cx4");
    let mask = rom_mask(rom.len());

    let chip = |base: u32| MapTarget::ChipMmio {
        chip_id: CX4_CHIP_ID,
        base,
    };

    // ROM routed through the chip (`base = CX4_ROM_BASE`) so the HG51B169's bus
    // arbitration can interpose while the DSP owns the cart bus; the geometry is
    // identical to a plain `Rom` slot, so `offset - CX4_ROM_BASE` is exactly the
    // ROM byte offset a `Rom` target would have produced. The two LoROM windows
    // are mirrors of the same image, both from offset 0.
    let push_rom = |plan: &mut MapPlan, banks: RangeInclusive<u8>| {
        let mut slot = MapSlot::new(banks, 0x8000..=0xFFFF, chip(CX4_ROM_BASE), BusSpeed::Slow);
        if let Some(m) = mask {
            slot = slot.with_mask(m);
        }
        plan.push(slot);
    };
    push_rom(&mut plan, 0x00..=0x3F);
    push_rom(&mut plan, 0x80..=0xBF);

    // Chip MMIO + internal data RAM $6000-7FFF in both system-bank halves. The
    // `0` bank stride hands the chip a bare `addr - 0x6000` index; the
    // page-table walker promotes the open-bus `$6000` page to the cartridge (no
    // carve-out).
    for banks in [0x00..=0x3F_u8, 0x80..=0xBF_u8] {
        plan.push(
            MapSlot::new(banks, 0x6000..=0x7FFF, chip(CX4_MMIO_BASE), BusSpeed::Slow)
                .with_bank_stride(0),
        );
    }

    // Cart RAM, only when a manifest declares it (the real MMX2/X3 carts have
    // none): the `$70-77:0000-7FFF` window. The chip folds modulo the RAM size.
    if ram_size > 0 {
        plan.push(MapSlot::new(
            0x70..=0x77,
            0x0000..=0x7FFF,
            chip(CX4_RAM_BASE),
            BusSpeed::Slow,
        ));
    }

    plan
}

/// Fold mask for ROMs smaller than the 2 MiB window capacity, so the two LoROM
/// windows both wrap back into the real image. `None` for ROMs `>= 2 MiB` (the
/// raw linear offset already lands inside the buffer). Assumes a power-of-two
/// ROM length (the builder pads before constructing).
fn rom_mask(rom_len: usize) -> Option<u32> {
    if rom_len == 0 || rom_len as u32 >= CX4_ROM_BYTES {
        None
    } else {
        Some((rom_len as u32) - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2 MiB Cx4 ROM (Mega Man X3-sized).
    fn mmx3_rom() -> Vec<u8> {
        vec![0u8; 0x20_0000]
    }

    /// A 1 MiB padded Cx4 ROM (a hypothetical small homebrew).
    fn small_rom() -> Vec<u8> {
        vec![0u8; 0x10_0000]
    }

    fn slots_with_base(plan: &MapPlan, base: u32) -> Vec<&MapSlot> {
        plan.slots
            .iter()
            .filter(|s| matches!(s.target, MapTarget::ChipMmio { base: b, .. } if b == base))
            .collect()
    }

    /// The chip-address the chip's `read`/`write` receives for `addr24`.
    fn chip_addr(plan: &MapPlan, addr24: u32) -> u32 {
        let r = plan.resolve(addr24).expect("address maps");
        match plan.slots[r.slot_index as usize].target {
            MapTarget::ChipMmio { base, .. } => base + r.target_offset,
            other => panic!("expected ChipMmio, got {other:?}"),
        }
    }

    /// The ROM byte offset the chip recovers for `addr24`.
    fn rom_offset(plan: &MapPlan, addr24: u32) -> u32 {
        chip_addr(plan, addr24) - CX4_ROM_BASE
    }

    #[test]
    fn preset_is_named_cx4() {
        assert_eq!(cx4_preset(&mmx3_rom(), 0).name, "cx4");
    }

    #[test]
    fn rom_windows_are_chip_routed() {
        let plan = cx4_preset(&mmx3_rom(), 0);
        for addr in [0x00_8000, 0x80_8000] {
            let r = plan.resolve(addr).expect("ROM maps");
            assert!(
                matches!(
                    plan.slots[r.slot_index as usize].target,
                    MapTarget::ChipMmio {
                        chip_id: CX4_CHIP_ID,
                        base: CX4_ROM_BASE,
                    }
                ),
                "addr {addr:#08x} must be chip-routed ROM"
            );
        }
    }

    #[test]
    fn lorom_windows_are_two_views_of_the_same_image() {
        let plan = cx4_preset(&mmx3_rom(), 0);
        // $00:8000 -> 0, $01:8000 -> 0x8000, $3F:FFFF -> 0x1FFFFF.
        assert_eq!(rom_offset(&plan, 0x00_8000), 0);
        assert_eq!(rom_offset(&plan, 0x01_8000), 0x8000);
        assert_eq!(rom_offset(&plan, 0x3F_FFFF), 0x1F_FFFF);
        // The $80-BF mirror matches its $00-3F twin.
        assert_eq!(rom_offset(&plan, 0x80_8000), rom_offset(&plan, 0x00_8000));
        assert_eq!(rom_offset(&plan, 0xBF_FFFF), rom_offset(&plan, 0x3F_FFFF));
    }

    #[test]
    fn small_rom_folds_back_into_the_image() {
        // 1 MiB ROM: the 2 MiB window wraps via a 0xFFFFF mask.
        let plan = cx4_preset(&small_rom(), 0);
        // $20:8000 -> linear 0x100000 (= 32 x 32 KiB) -> masked to 0: the second
        // MiB of the window wraps back to the start of the 1 MiB image.
        assert_eq!(rom_offset(&plan, 0x20_8000), 0);
        // $21:8000 -> linear 0x108000 -> masked to 0x8000.
        assert_eq!(rom_offset(&plan, 0x21_8000), 0x8000);
        // $3F:FFFF -> linear 0x1FFFFF -> masked to 0xFFFFF.
        assert_eq!(rom_offset(&plan, 0x3F_FFFF), 0xF_FFFF);
    }

    #[test]
    fn no_rom_mask_for_2mib_rom() {
        let plan = cx4_preset(&mmx3_rom(), 0);
        let rom_slots = slots_with_base(&plan, CX4_ROM_BASE);
        assert_eq!(rom_slots.len(), 2, "two chip-routed ROM windows");
        for s in rom_slots {
            assert!(s.mask.is_none(), "2 MiB ROM needs no fold mask");
        }
    }

    #[test]
    fn mmio_window_covers_both_halves_with_folded_bank() {
        let plan = cx4_preset(&mmx3_rom(), 0);
        let mmio = slots_with_base(&plan, CX4_MMIO_BASE);
        assert_eq!(mmio.len(), 2, "one MMIO slot per system-bank half");
        assert_eq!(mmio[0].bank_range, 0x00..=0x3F);
        assert_eq!(mmio[1].bank_range, 0x80..=0xBF);
        for s in &mmio {
            assert_eq!(s.offset_range, 0x6000..=0x7FFF);
            assert_eq!(s.bank_stride, Some(0), "bank folded away");
        }
        // $00:6000 and its $80 mirror both land on chip byte MMIO_BASE + 0.
        assert_eq!(chip_addr(&plan, 0x00_6000), CX4_MMIO_BASE);
        assert_eq!(chip_addr(&plan, 0x80_6000), CX4_MMIO_BASE);
        // $00:7F53 (a status register) -> base + 0x1F53; any bank aliases it.
        assert_eq!(chip_addr(&plan, 0x00_7F53), CX4_MMIO_BASE + 0x1F53);
        assert_eq!(chip_addr(&plan, 0x3F_7F53), CX4_MMIO_BASE + 0x1F53);
    }

    #[test]
    fn cart_ram_window_present_only_with_ram() {
        let with = cx4_preset(&mmx3_rom(), 0x2000);
        let ram = slots_with_base(&with, CX4_RAM_BASE);
        assert_eq!(ram.len(), 1);
        assert_eq!(ram[0].bank_range, 0x70..=0x77);
        assert_eq!(ram[0].offset_range, 0x0000..=0x7FFF);

        let without = cx4_preset(&mmx3_rom(), 0);
        assert!(slots_with_base(&without, CX4_RAM_BASE).is_empty());
    }

    #[test]
    fn cart_ram_spans_banks_linearly() {
        let plan = cx4_preset(&mmx3_rom(), 0x2000);
        // $70:0000 -> base+0, $70:7FFF -> base+0x7FFF, $71:0000 -> base+0x8000.
        assert_eq!(chip_addr(&plan, 0x70_0000), CX4_RAM_BASE);
        assert_eq!(chip_addr(&plan, 0x70_7FFF), CX4_RAM_BASE + 0x7FFF);
        assert_eq!(chip_addr(&plan, 0x71_0000), CX4_RAM_BASE + 0x8000);
    }
}
