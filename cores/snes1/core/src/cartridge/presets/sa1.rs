//! SA-1 static map (checked against bsnes-plus's mapping of the board).
//!
//! The SA-1 shares ROM, I-RAM, and BW-RAM with the console's S-CPU through a
//! set of windows that no stock preset knows about, so it owns its whole plan.
//! Every window routes to the one [`Sa1Chip`](crate::cartridge::Sa1Chip) via
//! [`MapTarget::ChipMmio`], split apart by the `base` sentinel the slot carries
//! (see `cartridge/sa1/mod.rs` for the sentinel contract):
//!
//! - **MMIO** `$00-3F/$80-BF:2200-23FF` -> `SA1_MMIO_BASE` (`0` bank stride, so
//!   a register access arrives as `SA1_MMIO_BASE + (reg - 0x2200)`). The
//!   page-table walker can't promote these (the `$2000` page is the B-bus
//!   window), so the bus's `$2200-23FF` carve-out resolves them here.
//! - **I-RAM** `$00-3F/$80-BF:3000-37FF` -> `SA1_IRAM_BASE` (`0` stride, every
//!   bank aliases the same 2 KiB). The S-CPU only sees I-RAM here; the SA-1's
//!   low `$0000-07FF` view is on the SA-1's own bus.
//! - **BW-RAM window** `$00-3F/$80-BF:6000-7FFF` -> `SA1_BWRAM_BASE` (`0`
//!   stride; the chip offsets it by the `$2224` block select), plus the
//!   **linear** view `$40-4F:0000-FFFF` -> `SA1_BWRAM_LINEAR_BASE`. Present only
//!   when the cart declares BW-RAM.
//! - **ROM** `$00-3F:8000-FFFF`, `$80-BF:8000-FFFF`, `$C0-FF:0000-FFFF` ->
//!   `SA1_ROM_BASE + window_start`. Each ROM slot carries a `0x10000` bank
//!   stride and no mask, so `base + compute_target_offset(addr)` reconstructs
//!   `SA1_ROM_BASE + addr24`; the chip subtracts the base to get the full
//!   24-bit address and applies the Super MMC per bank. `$80-BF` is
//!   FastROM-eligible.
//!
//! Routing ROM/BW-RAM/I-RAM through the chip (rather than as plain `Rom`/`Sram`
//! targets) is deliberate: the Super MMC remaps ROM dynamically, and every
//! S-CPU access to shared memory must force a SA-1 catch-up. Because a
//! dedicated preset owns the entire map, the cartridge round-trips through
//! the manifest (`preset: "sa1"` reproduces this plan).

use crate::cartridge::{
    BusSpeed, MapPlan, MapSlot, MapTarget, SA1_BWRAM_BASE, SA1_BWRAM_LINEAR_BASE, SA1_CHIP_ID,
    SA1_IRAM_BASE, SA1_MMIO_BASE, SA1_ROM_BASE,
};

/// Per-bank stride for the Super MMC ROM windows, one bank step crosses an
/// entire 64 KiB, so the chip receives the true 24-bit address (not a
/// LoROM-compacted one) and can apply the MMC math itself.
const ROM_BANK_STRIDE: u32 = 0x10000;

/// Build the SA-1 plan.
///
/// `rom` is the padded ROM buffer (unused for slot construction, the chip
/// folds out-of-range reads modulo its own length). `bwram_size` is the BW-RAM
/// size in bytes (0 for "no RAM"); `fastrom` toggles the `$80-BF` ROM window
/// between [`BusSpeed::Slow`] and [`BusSpeed::Fast`].
pub fn sa1_preset(rom: &[u8], bwram_size: usize, fastrom: bool) -> MapPlan {
    let _ = rom; // padded buffer unused: the chip mirrors OOB reads itself.
    let mut plan = MapPlan::new(if fastrom { "sa1-fastrom" } else { "sa1" });

    let chip = |base: u32| MapTarget::ChipMmio {
        chip_id: SA1_CHIP_ID,
        base,
    };

    // MMIO $2200-23FF in both system-bank halves. The `0` bank stride hands the
    // chip a bare `reg - 0x2200` index; the bus's $2200-23FF carve-out resolves
    // the access through this plan (the B-bus page won't promote it).
    for banks in [0x00..=0x3F_u8, 0x80..=0xBF_u8] {
        plan.push(
            MapSlot::new(banks, 0x2200..=0x23FF, chip(SA1_MMIO_BASE), BusSpeed::Slow)
                .with_bank_stride(0),
        );
    }

    // I-RAM $3000-37FF in both halves; `0` stride aliases every bank to the
    // same 2 KiB.
    for banks in [0x00..=0x3F_u8, 0x80..=0xBF_u8] {
        plan.push(
            MapSlot::new(banks, 0x3000..=0x37FF, chip(SA1_IRAM_BASE), BusSpeed::Slow)
                .with_bank_stride(0),
        );
    }

    // BW-RAM, only when the cart declares it: the 8 KiB block-selected window
    // ($6000-7FFF, `0` stride) plus the linear view ($40-4F, default full-bank
    // stride so the chip sees a contiguous 0..0xFFFFF offset).
    if bwram_size > 0 {
        for banks in [0x00..=0x3F_u8, 0x80..=0xBF_u8] {
            plan.push(
                MapSlot::new(banks, 0x6000..=0x7FFF, chip(SA1_BWRAM_BASE), BusSpeed::Slow)
                    .with_bank_stride(0),
            );
        }
        plan.push(MapSlot::new(
            0x40..=0x4F,
            0x0000..=0xFFFF,
            chip(SA1_BWRAM_LINEAR_BASE),
            BusSpeed::Slow,
        ));
    }

    // ROM via the Super MMC. Each window carries base = SA1_ROM_BASE + its own
    // start SNES address; with the 0x10000 bank stride and no mask,
    // `base + compute_target_offset` == SA1_ROM_BASE + addr24, so the chip
    // recovers the full 24-bit address. $80-BF is FastROM-eligible.
    plan.push(
        MapSlot::new(
            0x00..=0x3F,
            0x8000..=0xFFFF,
            chip(SA1_ROM_BASE + 0x00_8000),
            BusSpeed::Slow,
        )
        .with_bank_stride(ROM_BANK_STRIDE),
    );
    plan.push(
        MapSlot::new(
            0x80..=0xBF,
            0x8000..=0xFFFF,
            chip(SA1_ROM_BASE + 0x80_8000),
            if fastrom {
                BusSpeed::Fast
            } else {
                BusSpeed::Slow
            },
        )
        .with_bank_stride(ROM_BANK_STRIDE),
    );
    plan.push(MapSlot::new(
        0xC0..=0xFF,
        0x0000..=0xFFFF,
        chip(SA1_ROM_BASE + 0xC0_0000),
        BusSpeed::Slow,
    ));

    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4 MiB padded SA-1 ROM (Super Mario RPG-sized).
    fn sa1_rom() -> Vec<u8> {
        vec![0u8; 0x40_0000]
    }

    fn slots_with_base(plan: &MapPlan, base: u32) -> Vec<&MapSlot> {
        plan.slots
            .iter()
            .filter(|s| matches!(s.target, MapTarget::ChipMmio { base: b, .. } if b == base))
            .collect()
    }

    /// The chip-address the chip's `read`/`write` receives for `addr24`: the
    /// slot's base plus the resolved slot-relative offset, exactly what the bus
    /// forms in `read_cartridge` (`base + target_offset`).
    fn chip_addr(plan: &MapPlan, addr24: u32) -> u32 {
        let r = plan.resolve(addr24).expect("address maps");
        match plan.slots[r.slot_index as usize].target {
            MapTarget::ChipMmio { base, .. } => base + r.target_offset,
            other => panic!("expected ChipMmio, got {other:?}"),
        }
    }

    #[test]
    fn preset_name_tracks_fastrom() {
        assert_eq!(sa1_preset(&sa1_rom(), 0, false).name, "sa1");
        assert_eq!(sa1_preset(&sa1_rom(), 0, true).name, "sa1-fastrom");
    }

    #[test]
    fn mmio_window_covers_both_halves_with_folded_bank() {
        let plan = sa1_preset(&sa1_rom(), 0, false);
        let mmio = slots_with_base(&plan, SA1_MMIO_BASE);
        assert_eq!(mmio.len(), 2, "one MMIO slot per system-bank half");
        assert_eq!(mmio[0].bank_range, 0x00..=0x3F);
        assert_eq!(mmio[1].bank_range, 0x80..=0xBF);
        for s in &mmio {
            assert_eq!(s.offset_range, 0x2200..=0x23FF);
            assert_eq!(s.bank_stride, Some(0), "bank folded away");
        }
        // $00:2220 and its $80 mirror both land on chip byte MMIO_BASE + 0x20.
        assert_eq!(chip_addr(&plan, 0x00_2220), SA1_MMIO_BASE + 0x20);
        assert_eq!(chip_addr(&plan, 0x80_2220), SA1_MMIO_BASE + 0x20);
        assert_eq!(chip_addr(&plan, 0x3F_23FF), SA1_MMIO_BASE + 0x1FF);
    }

    #[test]
    fn iram_window_aliases_every_bank_to_2kib() {
        let plan = sa1_preset(&sa1_rom(), 0, false);
        let iram = slots_with_base(&plan, SA1_IRAM_BASE);
        assert_eq!(iram.len(), 2);
        for s in &iram {
            assert_eq!(s.offset_range, 0x3000..=0x37FF);
            assert_eq!(s.bank_stride, Some(0));
        }
        // $00:3000 -> base+0; $05:3000 (any bank) -> base+0; $00:37FF -> base+0x7FF.
        assert_eq!(chip_addr(&plan, 0x00_3000), SA1_IRAM_BASE);
        assert_eq!(chip_addr(&plan, 0x05_3000), SA1_IRAM_BASE);
        assert_eq!(chip_addr(&plan, 0x00_37FF), SA1_IRAM_BASE + 0x7FF);
    }

    #[test]
    fn bwram_windows_present_only_with_ram() {
        let with = sa1_preset(&sa1_rom(), 0x8000, false);
        assert_eq!(slots_with_base(&with, SA1_BWRAM_BASE).len(), 2);
        assert_eq!(slots_with_base(&with, SA1_BWRAM_LINEAR_BASE).len(), 1);

        let without = sa1_preset(&sa1_rom(), 0, false);
        assert!(slots_with_base(&without, SA1_BWRAM_BASE).is_empty());
        assert!(slots_with_base(&without, SA1_BWRAM_LINEAR_BASE).is_empty());
    }

    #[test]
    fn bwram_window_folds_bank_and_linear_spans_banks() {
        let plan = sa1_preset(&sa1_rom(), 0x1_0000, false);
        // 8 KiB window: $00:6000 -> base+0, $00:7FFF -> base+0x1FFF, and the $80
        // mirror + any bank alias the same 8 KiB (chip adds the sbm select).
        assert_eq!(chip_addr(&plan, 0x00_6000), SA1_BWRAM_BASE);
        assert_eq!(chip_addr(&plan, 0x00_7FFF), SA1_BWRAM_BASE + 0x1FFF);
        assert_eq!(chip_addr(&plan, 0x80_6000), SA1_BWRAM_BASE);
        // Linear view: $40:0000 -> base+0, $41:0000 -> base+0x10000, top of $4F.
        assert_eq!(chip_addr(&plan, 0x40_0000), SA1_BWRAM_LINEAR_BASE);
        assert_eq!(
            chip_addr(&plan, 0x41_0000),
            SA1_BWRAM_LINEAR_BASE + 0x1_0000
        );
        assert_eq!(
            chip_addr(&plan, 0x4F_FFFF),
            SA1_BWRAM_LINEAR_BASE + 0xF_FFFF
        );
    }

    #[test]
    fn rom_windows_reconstruct_the_full_24bit_address() {
        let plan = sa1_preset(&sa1_rom(), 0, false);
        let rom: Vec<&MapSlot> = plan
            .slots
            .iter()
            .filter(
                |s| matches!(s.target, MapTarget::ChipMmio { base, .. } if base >= SA1_ROM_BASE),
            )
            .collect();
        assert_eq!(rom.len(), 3, "three Super MMC ROM windows");

        // Window 1 ($00-3F:8000-FFFF): chip receives SA1_ROM_BASE + addr24.
        assert_eq!(chip_addr(&plan, 0x00_8000), SA1_ROM_BASE + 0x00_8000);
        assert_eq!(chip_addr(&plan, 0x01_8000), SA1_ROM_BASE + 0x01_8000);
        assert_eq!(chip_addr(&plan, 0x3F_FFFF), SA1_ROM_BASE + 0x3F_FFFF);
        // Window 2 ($80-BF:8000-FFFF).
        assert_eq!(chip_addr(&plan, 0x80_8000), SA1_ROM_BASE + 0x80_8000);
        assert_eq!(chip_addr(&plan, 0xBF_FFFF), SA1_ROM_BASE + 0xBF_FFFF);
        // Window 3 ($C0-FF:0000-FFFF), default full-bank stride.
        assert_eq!(chip_addr(&plan, 0xC0_0000), SA1_ROM_BASE + 0xC0_0000);
        assert_eq!(chip_addr(&plan, 0xC1_0000), SA1_ROM_BASE + 0xC1_0000);
        assert_eq!(chip_addr(&plan, 0xFF_FFFF), SA1_ROM_BASE + 0xFF_FFFF);
    }

    #[test]
    fn rom_windows_carry_bank_stride_for_full_address() {
        let plan = sa1_preset(&sa1_rom(), 0, false);
        for s in &plan.slots {
            if let MapTarget::ChipMmio { base, .. } = s.target {
                if base >= SA1_ROM_BASE && *s.offset_range.start() == 0x8000 {
                    assert_eq!(
                        s.bank_stride,
                        Some(ROM_BANK_STRIDE),
                        "the $8000 ROM windows need a 64 KiB stride"
                    );
                }
            }
        }
    }

    #[test]
    fn fastrom_flag_drives_upper_rom_window_speed() {
        let slow = sa1_preset(&sa1_rom(), 0, false);
        let fast = sa1_preset(&sa1_rom(), 0, true);
        let upper_speed = |p: &MapPlan| {
            p.slots
                .iter()
                .find(|s| s.bank_range == (0x80..=0xBF) && *s.offset_range.start() == 0x8000)
                .unwrap()
                .speed
        };
        assert_eq!(upper_speed(&slow), BusSpeed::Slow);
        assert_eq!(upper_speed(&fast), BusSpeed::Fast);
    }
}
