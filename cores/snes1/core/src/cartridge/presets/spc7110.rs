//! SPC7110 static map (checked against bsnes-plus's mapping of the board).
//!
//! The SPC7110 (Tengai Makyou Zero, Momotarou Dentetsu Happy, Super Power
//! League 4) is a HiROM-shaped program map plus several extra windows the
//! stock HiROM preset has no notion of, so it owns its whole plan rather than
//! front-inserting slots onto `hirom_preset`:
//!
//! - **Shadow program ROM** at `$00-0F:8000-FFFF` and `$80-BF:8000-FFFF`,
//!   the HiROM upper-half mirror (full 64 KiB per bank, so `bank_stride =
//!   0x10000` + `MapTarget::Rom { base_offset: 0x8000 }`, exactly like the
//!   stock HiROM mirrors). `$80-BF` is FastROM-eligible; `$00-0F` is slow.
//! - **Linear program ROM** at `$C0-CF:0000-FFFF`, the first 1 MiB mapped
//!   contiguously (default full-bank stride).
//! - **Extra program ROM** at `$40-4F:0000-FFFF @ ROM offset 0x600000`,
//!   present only for ROMs `>= 7 MiB` (the Tengai Makyou Zero English fan
//!   translation); the shipped Japanese carts are 4-5 MiB and omit it.
//! - **MCU data-ROM window** at `$D0-FF:0000-FFFF`, routed to the chip as
//!   `MapTarget::ChipMmio { base: SPC7110_MCU_BASE }` (a linear
//!   `0..0x2F_FFFF` offset) so the chip remaps each 1 MiB quadrant through
//!   the `$4831/$4832/$4833` `dx/ex/fx` bank latches.
//! - **DCU decompress port** at `$50:0000-FFFF` and `$58:0000-FFFF`, routed
//!   with `base = SPC7110_DCU_BASE` and a `0` mask, so every address in the
//!   two banks resolves to a single chip offset that aliases a read of
//!   register `$4800` (the decompression FIFO).
//! - **Write-gated SRAM** at `$00-3F:6000-7FFF` and `$80-BF:6000-7FFF`,
//!   routed with `base = SPC7110_SRAM_BASE` so the chip can enforce the
//!   `$4830 bit7` write-enable gate and own the battery for `.srm`
//!   persistence (the A/B boot self-test writes its result here).
//! - **Chip registers** at `$4800-483F` (both CpuMmio bank halves), plus the
//!   RTC ports `$4840-4842` for `$F9` carts, routed with `base =
//!   SPC7110_REG_BASE` and a `0` bank stride, so a register access arrives as
//!   `SPC7110_REG_BASE + (reg - 0x4800)` regardless of the source bank. The
//!   page-table walker can't promote these (the whole `$4000` page is
//!   CpuMmio), so the bus's `$4800-48FF` carve-out resolves them here.
//!
//! Because a dedicated preset owns the entire map, the cartridge round-trips
//! through the manifest: the detected manifest emits `preset: "spc7110"` and
//! re-applying it reproduces this exact plan.

use crate::cartridge::{
    BusSpeed, MapPlan, MapSlot, MapTarget, SPC7110_CHIP_ID, SPC7110_DCU_BASE, SPC7110_MCU_BASE,
    SPC7110_REG_BASE, SPC7110_SRAM_BASE,
};

/// Threshold (real, unpadded ROM size) at and above which the extra program
/// ROM window `$40-4F:0000-FFFF @ 0x600000` is mapped (the same size
/// bsnes-plus maps it from), only the Tengai Makyou Zero English fan
/// translation is large enough; the shipped carts are 4-5 MiB.
const EXTRA_PRG_THRESHOLD: usize = 0x70_0000;

/// ROM offset the extra program window reads from.
const EXTRA_PRG_OFFSET: u32 = 0x60_0000;

/// Per-bank stride for the HiROM-style shadow program mirrors, one bank step
/// in the address space crosses an entire 64 KiB chip bank.
const SHADOW_BANK_STRIDE: u32 = 0x10000;

/// Base offset applied to the shadow mirrors so the slot-relative offset
/// (which starts at 0 for `$8000`) lands on the upper half of each chip bank.
const SHADOW_BASE_OFFSET: u32 = 0x8000;

/// Build the SPC7110 plan.
///
/// `real_rom_len` is the *unpadded* ROM size (the padded `rom` buffer's length
/// is only used for slot construction); it gates the extra program window.
/// `sram_size` is the battery SRAM size in bytes (0 for "no SRAM"), `fastrom`
/// toggles the `$80-BF` shadow between [`BusSpeed::Slow`] and
/// [`BusSpeed::Fast`], and `has_rtc` extends the register window to cover the
/// RTC ports `$4840-4842` (only `$F9` carts).
pub fn spc7110_preset(
    rom: &[u8],
    real_rom_len: usize,
    sram_size: usize,
    fastrom: bool,
    has_rtc: bool,
) -> MapPlan {
    let _ = rom; // padded buffer unused: bounds-checked reads fold OOB to 0.
    let mut plan = MapPlan::new(if fastrom {
        "spc7110-fastrom"
    } else {
        "spc7110"
    });

    // Chip registers $4800-483F (+ RTC $4840-4842 for $F9 carts), in both
    // CpuMmio bank halves. A `0` bank stride hands the chip a bare
    // `reg - 0x4800` index regardless of the source bank; the bus's
    // $4800-48FF carve-out resolves the access through this plan.
    let reg_end: u16 = if has_rtc { 0x4842 } else { 0x483F };
    for banks in [0x00..=0x3F_u8, 0x80..=0xBF_u8] {
        plan.push(
            MapSlot::new(
                banks,
                0x4800..=reg_end,
                MapTarget::ChipMmio {
                    chip_id: SPC7110_CHIP_ID,
                    base: SPC7110_REG_BASE,
                },
                BusSpeed::Slow,
            )
            .with_bank_stride(0),
        );
    }

    // Write-gated SRAM $00-3F/$80-BF:6000-7FFF, routed through the chip so it
    // enforces the $4830 bit7 write gate and owns the battery. Pushed before
    // the program windows (no overlap, but kept early for clarity).
    if sram_size > 0 {
        let mask = super::sram_mirror_mask(sram_size);
        for banks in [0x00..=0x3F_u8, 0x80..=0xBF_u8] {
            plan.push(
                MapSlot::new(
                    banks,
                    0x6000..=0x7FFF,
                    MapTarget::ChipMmio {
                        chip_id: SPC7110_CHIP_ID,
                        base: SPC7110_SRAM_BASE,
                    },
                    BusSpeed::Slow,
                )
                .with_mask(mask),
            );
        }
    }

    // Shadow program ROM: HiROM upper-half mirror. $00-0F is slow; $80-BF is
    // FastROM-eligible.
    plan.push(
        MapSlot::new(
            0x00..=0x0F,
            0x8000..=0xFFFF,
            MapTarget::Rom {
                base_offset: SHADOW_BASE_OFFSET,
            },
            BusSpeed::Slow,
        )
        .with_bank_stride(SHADOW_BANK_STRIDE),
    );
    plan.push(
        MapSlot::new(
            0x80..=0xBF,
            0x8000..=0xFFFF,
            MapTarget::Rom {
                base_offset: SHADOW_BASE_OFFSET,
            },
            if fastrom {
                BusSpeed::Fast
            } else {
                BusSpeed::Slow
            },
        )
        .with_bank_stride(SHADOW_BANK_STRIDE),
    );

    // Linear program ROM: the first 1 MiB in $C0-CF (default full-bank stride,
    // base offset 0).
    plan.push(MapSlot::new(
        0xC0..=0xCF,
        0x0000..=0xFFFF,
        MapTarget::Rom { base_offset: 0 },
        BusSpeed::Slow,
    ));

    // Extra program ROM at $40-4F:0000-FFFF @ 0x600000, only for >= 7 MiB
    // carts (the TMZ English translation).
    if real_rom_len >= EXTRA_PRG_THRESHOLD {
        plan.push(MapSlot::new(
            0x40..=0x4F,
            0x0000..=0xFFFF,
            MapTarget::Rom {
                base_offset: EXTRA_PRG_OFFSET,
            },
            BusSpeed::Slow,
        ));
    }

    // DCU decompress port $50/$58:0000-FFFF. A `0` mask collapses every
    // address in each bank to a single chip offset (`SPC7110_DCU_BASE`), which
    // the chip aliases to a read of register $4800.
    for bank in [0x50_u8, 0x58_u8] {
        plan.push(
            MapSlot::new(
                bank..=bank,
                0x0000..=0xFFFF,
                MapTarget::ChipMmio {
                    chip_id: SPC7110_CHIP_ID,
                    base: SPC7110_DCU_BASE,
                },
                BusSpeed::Slow,
            )
            .with_mask(0),
        );
    }

    // MCU data-ROM window $D0-FF:0000-FFFF. `base = SPC7110_MCU_BASE` (0) hands
    // the chip a linear 0..0x2F_FFFF offset; the chip remaps each 1 MiB
    // quadrant through the dx/ex/fx bank latches ($4831-4833).
    plan.push(MapSlot::new(
        0xD0..=0xFF,
        0x0000..=0xFFFF,
        MapTarget::ChipMmio {
            chip_id: SPC7110_CHIP_ID,
            base: SPC7110_MCU_BASE,
        },
        BusSpeed::Slow,
    ));

    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TMZ-shaped padded ROM: 5 MiB real -> 8 MiB padded (power of two).
    fn tmz_rom() -> Vec<u8> {
        vec![0u8; 0x80_0000]
    }

    fn reg_slots(plan: &MapPlan) -> Vec<&MapSlot> {
        plan.slots
            .iter()
            .filter(|s| {
                matches!(
                    s.target,
                    MapTarget::ChipMmio {
                        base: SPC7110_REG_BASE,
                        ..
                    }
                )
            })
            .collect()
    }

    #[test]
    fn preset_name_tracks_fastrom() {
        assert_eq!(
            spc7110_preset(&tmz_rom(), 0x50_0000, 0, false, false).name,
            "spc7110"
        );
        assert_eq!(
            spc7110_preset(&tmz_rom(), 0x50_0000, 0, true, false).name,
            "spc7110-fastrom"
        );
    }

    #[test]
    fn register_windows_cover_both_bank_halves() {
        let plan = spc7110_preset(&tmz_rom(), 0x50_0000, 0, true, false);
        let regs = reg_slots(&plan);
        assert_eq!(regs.len(), 2, "one register slot per CpuMmio bank half");
        assert_eq!(regs[0].bank_range, 0x00..=0x3F);
        assert_eq!(regs[1].bank_range, 0x80..=0xBF);
        for s in regs {
            assert_eq!(s.offset_range, 0x4800..=0x483F, "no RTC -> no $4840-4842");
            assert_eq!(s.bank_stride, Some(0), "bank folded away");
            assert!(matches!(
                s.target,
                MapTarget::ChipMmio {
                    chip_id: SPC7110_CHIP_ID,
                    base: SPC7110_REG_BASE
                }
            ));
        }
    }

    #[test]
    fn rtc_extends_register_window() {
        let no_rtc = spc7110_preset(&tmz_rom(), 0x50_0000, 0, false, false);
        let with_rtc = spc7110_preset(&tmz_rom(), 0x50_0000, 0, false, true);
        assert_eq!(reg_slots(&no_rtc)[0].offset_range, 0x4800..=0x483F);
        assert_eq!(reg_slots(&with_rtc)[0].offset_range, 0x4800..=0x4842);
    }

    #[test]
    fn register_window_resolves_to_masked_index() {
        let plan = spc7110_preset(&tmz_rom(), 0x50_0000, 0, false, true);
        // `resolve()` returns the *slot-relative* offset; the bus adds the
        // slot's `SPC7110_REG_BASE` at the call site, and the chip recovers the
        // $48xx address by adding $4800. The `0` bank stride folds the source
        // bank away, so the offset is exactly `reg - 0x4800` in every mirror.
        // Each of these resolves to a register slot whose target carries the
        // register base.
        let resolves_to = |addr24: u32, want_offset: u32| {
            let r = plan.resolve(addr24).unwrap();
            assert!(matches!(
                plan.slots[r.slot_index as usize].target,
                MapTarget::ChipMmio {
                    chip_id: SPC7110_CHIP_ID,
                    base: SPC7110_REG_BASE
                }
            ));
            assert_eq!(r.target_offset, want_offset, "addr {addr24:#08x}");
        };
        // $00:4806 -> reg offset 6 (decomp trigger).
        resolves_to(0x00_4806, 0x06);
        // $12:4831 -> reg offset 0x31 (bank folded away by the 0 stride).
        resolves_to(0x12_4831, 0x31);
        // $80:4840 -> RTC port, reg offset 0x40 in the upper mirror.
        resolves_to(0x80_4840, 0x40);
    }

    #[test]
    fn shadow_program_mirrors_upper_halves() {
        let plan = spc7110_preset(&tmz_rom(), 0x50_0000, 0, true, false);
        let final_addr = |addr24: u32| -> u32 {
            let r = plan.resolve(addr24).unwrap();
            let slot = &plan.slots[r.slot_index as usize];
            match slot.target {
                MapTarget::Rom { base_offset } => base_offset + r.target_offset,
                other => panic!("expected ROM, got {other:?}"),
            }
        };
        // $00:8000 -> chip byte $8000 (upper half of bank 0).
        assert_eq!(final_addr(0x00_8000), 0x8000);
        // $01:8000 -> chip byte $18000 (upper half of bank 1).
        assert_eq!(final_addr(0x01_8000), 0x1_8000);
        // $80:9ABC -> chip byte $9ABC.
        assert_eq!(final_addr(0x80_9ABC), 0x9ABC);
        // $BF:FFFF -> the very top of the 4 MiB shadow.
        assert_eq!(final_addr(0xBF_FFFF), 0x3F_FFFF);
    }

    #[test]
    fn linear_program_maps_first_mib() {
        let plan = spc7110_preset(&tmz_rom(), 0x50_0000, 0, false, false);
        // $C0:0000 -> ROM offset 0.
        assert_eq!(plan.resolve(0xC0_0000).unwrap().target_offset, 0);
        // $C1:1234 -> 1 * 0x10000 + 0x1234.
        assert_eq!(plan.resolve(0xC1_1234).unwrap().target_offset, 0x1_1234);
        // $CF:FFFF -> the top of the 1 MiB linear window.
        assert_eq!(plan.resolve(0xCF_FFFF).unwrap().target_offset, 0xF_FFFF);
    }

    #[test]
    fn extra_prg_only_for_seven_mib_and_up() {
        // 5 MiB real -> no extra PRG window.
        let small = spc7110_preset(&tmz_rom(), 0x50_0000, 0, false, false);
        assert!(small.slots.iter().all(|s| !matches!(
            s.target,
            MapTarget::Rom {
                base_offset: EXTRA_PRG_OFFSET
            }
        )));

        // 7 MiB real -> extra PRG at $40-4F @ 0x600000.
        let big = spc7110_preset(&vec![0u8; 0x80_0000], 0x70_0000, 0, false, false);
        let extra = big
            .slots
            .iter()
            .find(|s| {
                matches!(
                    s.target,
                    MapTarget::Rom {
                        base_offset: EXTRA_PRG_OFFSET
                    }
                )
            })
            .expect("extra PRG window present for >= 7 MiB");
        assert_eq!(extra.bank_range, 0x40..=0x4F);
        assert_eq!(extra.offset_range, 0x0000..=0xFFFF);
        // $40:0000 -> ROM offset 0x600000; $41:0000 -> 0x610000.
        assert_eq!(big.resolve(0x40_0000).unwrap().target_offset, 0);
        assert_eq!(big.resolve(0x41_0000).unwrap().target_offset, 0x1_0000);
    }

    #[test]
    fn mcu_window_is_chip_mmio_over_d0_ff() {
        let plan = spc7110_preset(&tmz_rom(), 0x50_0000, 0, false, false);
        let mcu = plan
            .slots
            .iter()
            .find(|s| {
                matches!(
                    s.target,
                    MapTarget::ChipMmio {
                        base: SPC7110_MCU_BASE,
                        ..
                    }
                )
            })
            .expect("MCU window present");
        assert_eq!(mcu.bank_range, 0xD0..=0xFF);
        assert_eq!(mcu.offset_range, 0x0000..=0xFFFF);
        // $D0:0000 -> chip offset 0 (dx quadrant); $E0:0000 -> 0x100000 (ex);
        // $F0:0000 -> 0x200000 (fx); $FF:FFFF -> 0x2FFFFF (top of fx).
        assert_eq!(plan.resolve(0xD0_0000).unwrap().target_offset, 0);
        assert_eq!(plan.resolve(0xE0_0000).unwrap().target_offset, 0x10_0000);
        assert_eq!(plan.resolve(0xF0_0000).unwrap().target_offset, 0x20_0000);
        assert_eq!(plan.resolve(0xFF_FFFF).unwrap().target_offset, 0x2F_FFFF);
    }

    #[test]
    fn dcu_ports_alias_to_single_offset() {
        let plan = spc7110_preset(&tmz_rom(), 0x50_0000, 0, false, false);
        let dcu: Vec<&MapSlot> = plan
            .slots
            .iter()
            .filter(|s| {
                matches!(
                    s.target,
                    MapTarget::ChipMmio {
                        base: SPC7110_DCU_BASE,
                        ..
                    }
                )
            })
            .collect();
        assert_eq!(dcu.len(), 2, "DCU at banks $50 and $58");
        // Every address in $50/$58 collapses to offset 0 (-> chip DCU_BASE).
        assert_eq!(plan.resolve(0x50_0000).unwrap().target_offset, 0);
        assert_eq!(plan.resolve(0x50_ABCD).unwrap().target_offset, 0);
        assert_eq!(plan.resolve(0x58_FFFF).unwrap().target_offset, 0);
    }

    #[test]
    fn sram_windows_present_only_with_sram_and_gated_via_chip() {
        let with = spc7110_preset(&tmz_rom(), 0x50_0000, 8 * 1024, false, false);
        let sram: Vec<&MapSlot> = with
            .slots
            .iter()
            .filter(|s| {
                matches!(
                    s.target,
                    MapTarget::ChipMmio {
                        base: SPC7110_SRAM_BASE,
                        ..
                    }
                )
            })
            .collect();
        assert_eq!(sram.len(), 2, "SRAM at $00-3F and $80-BF");
        for s in &sram {
            assert_eq!(s.offset_range, 0x6000..=0x7FFF);
            assert_eq!(s.mask, Some(0x1FFF), "8 KiB mirror mask");
        }
        // $00:6000 -> chip offset SRAM_BASE + 0; $00:6001 -> +1; mirror at
        // $01:6000 -> +0 again (8 KiB wraps across banks).
        assert_eq!(with.resolve(0x00_6000).unwrap().target_offset, 0);
        assert_eq!(with.resolve(0x00_6001).unwrap().target_offset, 1);
        assert_eq!(with.resolve(0x01_6000).unwrap().target_offset, 0);

        let without = spc7110_preset(&tmz_rom(), 0x50_0000, 0, false, false);
        assert!(without.slots.iter().all(|s| !matches!(
            s.target,
            MapTarget::ChipMmio {
                base: SPC7110_SRAM_BASE,
                ..
            }
        )));
    }

    #[test]
    fn fastrom_flag_drives_shadow_mirror_speed() {
        let slow = spc7110_preset(&tmz_rom(), 0x50_0000, 0, false, false);
        let fast = spc7110_preset(&tmz_rom(), 0x50_0000, 0, true, false);
        let mirror_speed = |p: &MapPlan| {
            p.slots
                .iter()
                .find(|s| s.bank_range == (0x80..=0xBF) && *s.offset_range.start() == 0x8000)
                .unwrap()
                .speed
        };
        assert_eq!(mirror_speed(&slow), BusSpeed::Slow);
        assert_eq!(mirror_speed(&fast), BusSpeed::Fast);
    }
}
