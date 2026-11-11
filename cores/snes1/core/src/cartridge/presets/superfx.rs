//! Super FX (GSU) static map (checked against bsnes-plus's mapping of
//! the GSU-2 board).
//!
//! The Super FX shares Game Pak ROM and Game Pak RAM with the console's S-CPU
//! through a set of windows a stock LoROM cart doesn't declare. ROM, the chip
//! registers (`$3000-$34FF`), and Game Pak RAM all route to the one
//! [`SuperFxChip`](crate::cartridge::SuperFxChip) via
//! [`MapTarget::ChipMmio`], split apart by the `base` sentinel each slot
//! carries (see `cartridge/superfx/mod.rs` for the sentinel contract). ROM is
//! chip-routed so RON/RAN arbitration can hand the S-CPU open bus while the
//! GSU owns the ROM/RAM buses; while the GSU is idle the chip returns the real
//! ROM byte, so this is transparent for a booting game.
//!
//! The S-CPU's view:
//!
//! - **ROM** (`base = SUPERFX_ROM_BASE`): the LoROM windows
//!   `$00-3F:8000-FFFF` + `$80-BF:8000-FFFF` (32 KiB/bank) and the linear
//!   windows `$40-5F:0000-FFFF` + `$C0-DF:0000-FFFF` (64 KiB/bank). The slot
//!   geometry is a plain-`Rom` clone, so `offset - SUPERFX_ROM_BASE` is the ROM
//!   byte offset; all four address the same image from offset 0, the LoROM and
//!   linear banks are two views of the same 2 MiB, so a `< 2 MiB` ROM carries a
//!   fold mask.
//! - **MMIO** (`base = SUPERFX_MMIO_BASE`): `$00-3F/$80-BF:3000-34FF`, `0`
//!   bank stride, so a register access arrives as
//!   `SUPERFX_MMIO_BASE + (reg - 0x3000)`. The `$3000-3FFF` page defaults to
//!   open bus, so the bus's page-table walker promotes it to the cartridge,
//!   no B-bus-style carve-out is needed (unlike the SA-1's `$2200-23FF`).
//! - **Game Pak RAM** (`base = SUPERFX_RAM_BASE`, only when `ram_size > 0`):
//!   the 8 KiB `$00-3F/$80-BF:6000-7FFF` peephole (`0` bank stride, first
//!   8 KiB) plus the linear `$60-7D/$E0-FF:0000-FFFF` views. All fold modulo
//!   the RAM size inside the chip, so the chip owns the buffer for plot +
//!   battery (the same reason the SA-1 routes BW-RAM through its chip).

use crate::cartridge::{
    BusSpeed, MapPlan, MapSlot, MapTarget, SUPERFX_MMIO_BASE, SUPERFX_RAM_BASE, SUPERFX_ROM_BASE,
};

/// Super FX ROM window capacity: the LoROM (`$00-3F`, 64 x 32 KiB) and linear
/// (`$40-5F`, 32 x 64 KiB) windows both span 2 MiB, Doom (the largest Super
/// FX game) fills it exactly.
const SUPERFX_ROM_BYTES: u32 = 0x20_0000;

/// Per-bank stride for the linear (`$40-5F`/`$C0-DF`) ROM windows, one bank
/// crosses a full 64 KiB, so the window reads the ROM linearly.
const LINEAR_BANK_STRIDE: u32 = 0x10000;

/// Build the Super FX plan.
///
/// `rom` is the padded ROM buffer (used only to size the fold mask). `ram_size`
/// is the Game Pak RAM size in bytes (0 for "no RAM").
pub fn superfx_preset(rom: &[u8], ram_size: usize) -> MapPlan {
    let mut plan = MapPlan::new("superfx");
    let mask = rom_mask(rom.len());

    let chip = |base: u32| MapTarget::ChipMmio {
        chip_id: crate::cartridge::SUPERFX_CHIP_ID,
        base,
    };

    // ROM routed through the chip (`base = SUPERFX_ROM_BASE`) so RON
    // arbitration can hand the S-CPU open bus while the GSU owns the ROM bus;
    // the geometry is identical to a plain `Rom` slot, so
    // `offset - SUPERFX_ROM_BASE` is exactly the ROM byte offset a `Rom` target
    // would have produced. LoROM windows (32 KiB/bank) and linear windows
    // (64 KiB/bank) are two views of the same image, both from offset 0.
    let push_rom = |plan: &mut MapPlan,
                    banks: std::ops::RangeInclusive<u8>,
                    offsets: std::ops::RangeInclusive<u16>,
                    linear: bool| {
        let mut slot = MapSlot::new(banks, offsets, chip(SUPERFX_ROM_BASE), BusSpeed::Slow);
        if linear {
            slot = slot.with_bank_stride(LINEAR_BANK_STRIDE);
        }
        if let Some(m) = mask {
            slot = slot.with_mask(m);
        }
        plan.push(slot);
    };
    push_rom(&mut plan, 0x00..=0x3F, 0x8000..=0xFFFF, false);
    push_rom(&mut plan, 0x40..=0x5F, 0x0000..=0xFFFF, true);
    push_rom(&mut plan, 0x80..=0xBF, 0x8000..=0xFFFF, false);
    push_rom(&mut plan, 0xC0..=0xDF, 0x0000..=0xFFFF, true);

    // Chip MMIO $3000-34FF in both system-bank halves. The `0` bank stride
    // hands the chip a bare `reg - 0x3000` index; the page-table walker
    // promotes the open-bus `$3000` page to the cartridge (no carve-out).
    for banks in [0x00..=0x3F_u8, 0x80..=0xBF_u8] {
        plan.push(
            MapSlot::new(
                banks,
                0x3000..=0x34FF,
                chip(SUPERFX_MMIO_BASE),
                BusSpeed::Slow,
            )
            .with_bank_stride(0),
        );
    }

    // Game Pak RAM, only when the cart declares it: the 8 KiB $6000-7FFF
    // peephole (`0` stride -> first 8 KiB in every bank) plus the linear
    // $60-7D / $E0-FF views (default full-bank stride).
    if ram_size > 0 {
        for banks in [0x00..=0x3F_u8, 0x80..=0xBF_u8] {
            plan.push(
                MapSlot::new(
                    banks,
                    0x6000..=0x7FFF,
                    chip(SUPERFX_RAM_BASE),
                    BusSpeed::Slow,
                )
                .with_bank_stride(0),
            );
        }
        plan.push(MapSlot::new(
            0x60..=0x7D,
            0x0000..=0xFFFF,
            chip(SUPERFX_RAM_BASE),
            BusSpeed::Slow,
        ));
        plan.push(MapSlot::new(
            0xE0..=0xFF,
            0x0000..=0xFFFF,
            chip(SUPERFX_RAM_BASE),
            BusSpeed::Slow,
        ));
    }

    plan
}

/// Fold mask for ROMs smaller than the 2 MiB window capacity, so the LoROM and
/// linear windows both wrap back into the real image. `None` for ROMs `>= 2
/// MiB` (the raw linear offset already lands inside the buffer). Assumes a
/// power-of-two ROM length (the builder pads before constructing).
fn rom_mask(rom_len: usize) -> Option<u32> {
    if rom_len == 0 || rom_len as u32 >= SUPERFX_ROM_BYTES {
        None
    } else {
        Some((rom_len as u32) - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cartridge::{
        SUPERFX_CHIP_ID, SUPERFX_MMIO_BASE, SUPERFX_RAM_BASE, SUPERFX_ROM_BASE,
    };

    /// A 1 MiB padded Super FX ROM (Star Fox-sized).
    fn starfox_rom() -> Vec<u8> {
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

    /// The ROM byte offset the chip recovers for `addr24`: the chip-routed ROM
    /// window hands the chip `SUPERFX_ROM_BASE + offset`, so the offset is the
    /// chip address minus the base (identical to what a plain `Rom` slot with
    /// `base_offset 0` would have produced).
    fn rom_offset(plan: &MapPlan, addr24: u32) -> u32 {
        chip_addr(plan, addr24) - SUPERFX_ROM_BASE
    }

    #[test]
    fn preset_is_named_superfx() {
        assert_eq!(superfx_preset(&starfox_rom(), 0).name, "superfx");
    }

    #[test]
    fn rom_windows_are_chip_routed_for_ron_arbitration() {
        let plan = superfx_preset(&starfox_rom(), 0);
        // The four ROM windows route through the chip (base = SUPERFX_ROM_BASE)
        // so RON arbitration can intercept the S-CPU read while the GSU owns the
        // ROM bus; the geometry still matches a plain-`Rom` slot.
        for addr in [0x00_8000, 0x40_0000, 0x80_8000, 0xC0_0000] {
            let r = plan.resolve(addr).expect("ROM maps");
            assert!(
                matches!(
                    plan.slots[r.slot_index as usize].target,
                    MapTarget::ChipMmio {
                        chip_id: SUPERFX_CHIP_ID,
                        base: SUPERFX_ROM_BASE,
                    }
                ),
                "addr {addr:#08x} must be chip-routed ROM"
            );
        }
    }

    #[test]
    fn lorom_and_linear_windows_are_two_views_of_the_same_image() {
        // Use a 2 MiB ROM so no fold mask hides the arithmetic.
        let plan = superfx_preset(&vec![0u8; 0x20_0000], 0);
        // LoROM: $00:8000 -> 0, $01:8000 -> 0x8000, $3F:FFFF -> 0x1FFFFF.
        assert_eq!(rom_offset(&plan, 0x00_8000), 0);
        assert_eq!(rom_offset(&plan, 0x01_8000), 0x8000);
        assert_eq!(rom_offset(&plan, 0x3F_FFFF), 0x1F_FFFF);
        // Linear: $40:0000 -> 0, $41:0000 -> 0x10000, $5F:FFFF -> 0x1FFFFF.
        assert_eq!(rom_offset(&plan, 0x40_0000), 0);
        assert_eq!(rom_offset(&plan, 0x41_0000), 0x10000);
        assert_eq!(rom_offset(&plan, 0x5F_FFFF), 0x1F_FFFF);
        // The $80-BF / $C0-DF mirrors match their $00-3F / $40-5F twins.
        assert_eq!(rom_offset(&plan, 0x80_8000), rom_offset(&plan, 0x00_8000));
        assert_eq!(rom_offset(&plan, 0xC0_0000), rom_offset(&plan, 0x40_0000));
    }

    #[test]
    fn small_rom_folds_back_into_the_image() {
        // 1 MiB ROM: the 2 MiB windows wrap via a 0xFFFFF mask.
        let plan = superfx_preset(&starfox_rom(), 0);
        // $20:8000 -> linear 0x100000 -> masked to 0 (wraps the 1 MiB image).
        assert_eq!(rom_offset(&plan, 0x20_8000), 0);
        // $50:0000 -> linear 0x100000 -> masked to 0.
        assert_eq!(rom_offset(&plan, 0x50_0000), 0);
    }

    #[test]
    fn no_rom_mask_for_2mib_rom() {
        let plan = superfx_preset(&vec![0u8; 0x20_0000], 0);
        let rom_slots = slots_with_base(&plan, SUPERFX_ROM_BASE);
        assert_eq!(rom_slots.len(), 4, "four chip-routed ROM windows");
        for s in rom_slots {
            assert!(s.mask.is_none(), "2 MiB ROM needs no fold mask");
        }
    }

    #[test]
    fn mmio_window_covers_both_halves_with_folded_bank() {
        let plan = superfx_preset(&starfox_rom(), 0);
        let mmio = slots_with_base(&plan, SUPERFX_MMIO_BASE);
        assert_eq!(mmio.len(), 2, "one MMIO slot per system-bank half");
        assert_eq!(mmio[0].bank_range, 0x00..=0x3F);
        assert_eq!(mmio[1].bank_range, 0x80..=0xBF);
        for s in &mmio {
            assert_eq!(s.offset_range, 0x3000..=0x34FF);
            assert_eq!(s.bank_stride, Some(0), "bank folded away");
            assert!(matches!(
                s.target,
                MapTarget::ChipMmio {
                    chip_id: SUPERFX_CHIP_ID,
                    ..
                }
            ));
        }
        // $00:3000 and its $80 mirror both land on chip byte MMIO_BASE + 0.
        assert_eq!(chip_addr(&plan, 0x00_3000), SUPERFX_MMIO_BASE);
        assert_eq!(chip_addr(&plan, 0x80_3000), SUPERFX_MMIO_BASE);
        // $00:301F (R15 high) -> base + 0x1F; $00:34FF (top) -> base + 0x4FF.
        assert_eq!(chip_addr(&plan, 0x00_301F), SUPERFX_MMIO_BASE + 0x1F);
        assert_eq!(chip_addr(&plan, 0x3F_34FF), SUPERFX_MMIO_BASE + 0x4FF);
    }

    #[test]
    fn game_pak_ram_windows_present_only_with_ram() {
        let with = superfx_preset(&starfox_rom(), 0x8000);
        // 2 peephole slots ($00-3F, $80-BF) + 2 linear ($60-7D, $E0-FF).
        assert_eq!(slots_with_base(&with, SUPERFX_RAM_BASE).len(), 4);

        let without = superfx_preset(&starfox_rom(), 0);
        assert!(slots_with_base(&without, SUPERFX_RAM_BASE).is_empty());
    }

    #[test]
    fn ram_peephole_aliases_first_8kib_and_linear_spans_banks() {
        let plan = superfx_preset(&starfox_rom(), 0x8000);
        // Peephole: $00:6000 -> base+0, $00:7FFF -> base+0x1FFF, and any bank
        // (plus the $80 mirror) aliases the same first 8 KiB.
        assert_eq!(chip_addr(&plan, 0x00_6000), SUPERFX_RAM_BASE);
        assert_eq!(chip_addr(&plan, 0x00_7FFF), SUPERFX_RAM_BASE + 0x1FFF);
        assert_eq!(chip_addr(&plan, 0x10_6000), SUPERFX_RAM_BASE);
        assert_eq!(chip_addr(&plan, 0x80_6000), SUPERFX_RAM_BASE);
        // Linear: $60:0000 -> base+0, $61:0000 -> base+0x10000.
        assert_eq!(chip_addr(&plan, 0x60_0000), SUPERFX_RAM_BASE);
        assert_eq!(chip_addr(&plan, 0x61_0000), SUPERFX_RAM_BASE + 0x1_0000);
        // The $E0-FF linear mirror starts at base+0 again.
        assert_eq!(chip_addr(&plan, 0xE0_0000), SUPERFX_RAM_BASE);
    }
}
