//! S-DD1 static map (checked against snes9x's mapping of the board).
//!
//! The S-DD1 presents a LoROM program map plus three extra windows the
//! stock LoROM preset has no notion of, so it owns its whole plan rather
//! than front-inserting slots onto `lorom_preset`:
//!
//! - a **static linear ROM window** at `$60-$7F:0000-FFFF` (the first
//!   2 MiB of ROM, HiROM-style full-bank stride),
//! - the **MMC ROM window** at `$C0-$FF:0000-FFFF`, routed to the chip as
//!   `MapTarget::ChipMmio { base: 0 }` so the super-bank latches steer it
//!   (and the decompressor intercepts armed transfers),
//! - the chip's own **registers** at `$4800-$480F` in both CpuMmio bank
//!   halves, routed as `ChipMmio { base: SDD1_REG_WINDOW_BASE }` so the
//!   chip tells a register access from an MMC access by magnitude.
//!
//! Because a dedicated preset owns the entire map, the cartridge round-trips
//! through the manifest: `detected_manifest` emits `preset: "sdd1"` and
//! re-applying it reproduces this exact plan.

use crate::cartridge::{BusSpeed, MapPlan, MapSlot, MapTarget, SDD1_CHIP_ID, SDD1_REG_WINDOW_BASE};

/// LoROM linear capacity: 4 MiB. The program window folds ROMs strictly
/// smaller than this back into the real buffer (same rule as the stock
/// LoROM preset); S-DD1 ROMs are far larger, so the mask is `None` in
/// practice.
const LOROM_LINEAR_BYTES: u32 = 0x40_0000;

/// Build the S-DD1 plan.
///
/// `sram_size` is the SRAM size in bytes (0 for "no SRAM", SFA2/Zero 2).
/// `fastrom` toggles the `$80-$BF` program mirror between [`BusSpeed::Slow`]
/// and [`BusSpeed::Fast`], matching the LoROM preset.
///
/// Slots are pushed in first-wins order. The SRAM windows precede the
/// static linear window so SRAM wins the overlapping `$70-$7F:0000-7FFF`
/// pages; the register windows precede everything so the `$4800-$480F`
/// carve-out resolves to the chip.
pub fn sdd1_preset(rom: &[u8], sram_size: usize, fastrom: bool) -> MapPlan {
    let mut plan = MapPlan::new(if fastrom { "sdd1-fastrom" } else { "sdd1" });
    let program_mask = lorom_mask(rom.len());

    // Registers $4800-$480F, in the $00-$3F and $80-$BF CpuMmio halves.
    // The page-table walker can't promote these (the whole $4000 page is
    // CpuMmio), so the bus's $4800-$48FF carve-out resolves them through
    // this plan. `base = SDD1_REG_WINDOW_BASE` + a `0xF` mask hands the
    // chip a bare 0..0xF register index regardless of the source bank.
    for banks in [0x00..=0x3F_u8, 0x80..=0xBF_u8] {
        plan.push(
            MapSlot::new(
                banks,
                0x4800..=0x480F,
                MapTarget::ChipMmio {
                    chip_id: SDD1_CHIP_ID,
                    base: SDD1_REG_WINDOW_BASE,
                },
                BusSpeed::Slow,
            )
            .with_mask(0xF),
        );
    }

    // Program (LoROM) + FastROM-eligible upper mirror. Direct ROM, not
    // through the MMC, the S-DD1 passes $00-$3F/$80-$BF straight to the
    // first 2 MiB, like a plain LoROM board.
    let mut prog_lo = MapSlot::new(
        0x00..=0x3F,
        0x8000..=0xFFFF,
        MapTarget::Rom { base_offset: 0 },
        BusSpeed::Slow,
    );
    let mut prog_hi = MapSlot::new(
        0x80..=0xBF,
        0x8000..=0xFFFF,
        MapTarget::Rom { base_offset: 0 },
        if fastrom {
            BusSpeed::Fast
        } else {
            BusSpeed::Slow
        },
    );
    if let Some(m) = program_mask {
        prog_lo = prog_lo.with_mask(m);
        prog_hi = prog_hi.with_mask(m);
    }
    plan.push(prog_lo);
    plan.push(prog_hi);

    // SRAM (only Star Ocean). Pushed *before* the linear window so it wins
    // the overlapping $70-$7F:0000-7FFF pages under first-wins.
    if sram_size > 0 {
        let mask = super::sram_mirror_mask(sram_size);
        plan.push(
            MapSlot::new(
                0x70..=0x7F,
                0x0000..=0x7FFF,
                MapTarget::Sram { base_offset: 0 },
                BusSpeed::Slow,
            )
            .with_mask(mask),
        );
        plan.push(
            MapSlot::new(
                0xA0..=0xBF,
                0x6000..=0x7FFF,
                MapTarget::Sram { base_offset: 0 },
                BusSpeed::Slow,
            )
            .with_mask(mask),
        );
    }

    // Static linear ROM window: the first 2 MiB mapped HiROM-style (a full
    // 64 KiB per bank, so the default bank stride is correct).
    plan.push(MapSlot::new(
        0x60..=0x7F,
        0x0000..=0xFFFF,
        MapTarget::Rom { base_offset: 0 },
        BusSpeed::Slow,
    ));

    // MMC ROM window. `base = 0` hands the chip a linear 0..0x3F_FFFF
    // offset; the four super-bank latches steer each 1 MiB quadrant.
    plan.push(MapSlot::new(
        0xC0..=0xFF,
        0x0000..=0xFFFF,
        MapTarget::ChipMmio {
            chip_id: SDD1_CHIP_ID,
            base: 0,
        },
        BusSpeed::Slow,
    ));

    plan
}

/// Pick the mask that folds the LoROM program window back into a
/// sub-4 MiB ROM buffer. `None` for ROMs >= 4 MiB (every S-DD1 ROM). Same
/// rule as the stock LoROM preset; assumes a power-of-two `rom_len` (the
/// builder pads the buffer before constructing the cartridge).
fn lorom_mask(rom_len: usize) -> Option<u32> {
    if rom_len == 0 || rom_len as u32 >= LOROM_LINEAR_BYTES {
        None
    } else {
        Some((rom_len as u32) - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Star-Ocean-shaped ROM: 8 MiB (6 MiB padded to a power of two).
    fn big_rom() -> Vec<u8> {
        vec![0u8; 0x80_0000]
    }

    fn find_reg_slots(plan: &MapPlan) -> Vec<&MapSlot> {
        plan.slots
            .iter()
            .filter(|s| {
                matches!(
                    s.target,
                    MapTarget::ChipMmio {
                        base: SDD1_REG_WINDOW_BASE,
                        ..
                    }
                )
            })
            .collect()
    }

    #[test]
    fn preset_name_tracks_fastrom() {
        assert_eq!(sdd1_preset(&big_rom(), 0, false).name, "sdd1");
        assert_eq!(sdd1_preset(&big_rom(), 0, true).name, "sdd1-fastrom");
    }

    #[test]
    fn register_windows_cover_both_bank_halves() {
        let plan = sdd1_preset(&big_rom(), 0, false);
        let regs = find_reg_slots(&plan);
        assert_eq!(regs.len(), 2, "one register slot per CpuMmio bank half");
        assert_eq!(regs[0].bank_range, 0x00..=0x3F);
        assert_eq!(regs[1].bank_range, 0x80..=0xBF);
        for s in regs {
            assert_eq!(s.offset_range, 0x4800..=0x480F);
            assert_eq!(s.mask, Some(0xF), "register index masked to 0..0xF");
            assert!(matches!(
                s.target,
                MapTarget::ChipMmio {
                    chip_id: SDD1_CHIP_ID,
                    base: SDD1_REG_WINDOW_BASE
                }
            ));
        }
    }

    #[test]
    fn mmc_window_is_chip_mmio_over_c0_ff() {
        let plan = sdd1_preset(&big_rom(), 0, false);
        let mmc = plan
            .slots
            .iter()
            .find(|s| matches!(s.target, MapTarget::ChipMmio { base: 0, .. }))
            .expect("MMC window present");
        assert_eq!(mmc.bank_range, 0xC0..=0xFF);
        assert_eq!(mmc.offset_range, 0x0000..=0xFFFF);
        assert!(matches!(
            mmc.target,
            MapTarget::ChipMmio {
                chip_id: SDD1_CHIP_ID,
                base: 0
            }
        ));
    }

    #[test]
    fn mmc_window_resolves_to_quadrant_preserving_offset() {
        let plan = sdd1_preset(&big_rom(), 0, false);
        // $C0:0000 -> quadrant 0, within 0.
        let r = plan.resolve(0xC0_0000).unwrap();
        assert_eq!(r.target_offset, 0);
        // $D0:0000 -> quadrant 1 ($D0-$C0 = 0x10 banks x 0x10000 = 0x10_0000).
        let r = plan.resolve(0xD0_0000).unwrap();
        assert_eq!(r.target_offset, 0x10_0000);
        // $FF:FFFF -> the very top of quadrant 3.
        let r = plan.resolve(0xFF_FFFF).unwrap();
        assert_eq!(r.target_offset, 0x3F_FFFF);
    }

    #[test]
    fn register_window_resolves_to_masked_index() {
        let plan = sdd1_preset(&big_rom(), 0, false);
        // $00:4804 -> register index 4, regardless of bank.
        let r = plan.resolve(0x00_4804).unwrap();
        assert_eq!(r.target_offset, 4);
        // $12:4807 -> still index 7 (bank folded away by the 0xF mask).
        let r = plan.resolve(0x12_4807).unwrap();
        assert_eq!(r.target_offset, 7);
        // $80:4800 -> index 0 in the upper mirror.
        let r = plan.resolve(0x80_4800).unwrap();
        assert_eq!(r.target_offset, 0);
    }

    #[test]
    fn linear_window_maps_first_two_mib() {
        let plan = sdd1_preset(&big_rom(), 0, false);
        // $60:0000 -> ROM offset 0.
        let r = plan.resolve(0x60_0000).unwrap();
        assert_eq!(r.target_offset, 0);
        // $61:1234 -> 1 * 0x10000 + 0x1234.
        let r = plan.resolve(0x61_1234).unwrap();
        assert_eq!(r.target_offset, 0x1_1234);
    }

    #[test]
    fn sram_precedes_linear_window_and_wins_overlap() {
        let plan = sdd1_preset(&big_rom(), 8 * 1024, false);
        // $70:0000 is inside both the SRAM window and the linear window;
        // first-wins must pick SRAM.
        let r = plan.resolve(0x70_0000).unwrap();
        let slot = &plan.slots[r.slot_index as usize];
        assert!(
            matches!(slot.target, MapTarget::Sram { .. }),
            "SRAM must win the overlap at $70:0000"
        );
        // $70:8000 is above the SRAM window -> linear ROM window wins.
        let r = plan.resolve(0x70_8000).unwrap();
        let slot = &plan.slots[r.slot_index as usize];
        assert!(matches!(slot.target, MapTarget::Rom { .. }));
    }

    #[test]
    fn sram_windows_present_only_with_sram() {
        let with = sdd1_preset(&big_rom(), 8 * 1024, false);
        assert_eq!(
            with.slots
                .iter()
                .filter(|s| matches!(s.target, MapTarget::Sram { .. }))
                .count(),
            2,
            "primary + mirror SRAM windows"
        );
        let without = sdd1_preset(&big_rom(), 0, false);
        assert!(without
            .slots
            .iter()
            .all(|s| !matches!(s.target, MapTarget::Sram { .. })));
    }

    #[test]
    fn sram_windows_carry_mirror_mask() {
        let plan = sdd1_preset(&big_rom(), 8 * 1024, false);
        for s in plan
            .slots
            .iter()
            .filter(|s| matches!(s.target, MapTarget::Sram { .. }))
        {
            assert_eq!(s.mask, Some(0x1FFF), "8 KiB mirror mask");
        }
    }

    #[test]
    fn program_window_unmasked_for_large_rom() {
        let plan = sdd1_preset(&big_rom(), 0, false);
        let prog: Vec<&MapSlot> = plan
            .slots
            .iter()
            .filter(|s| {
                matches!(s.target, MapTarget::Rom { .. }) && *s.offset_range.start() == 0x8000
            })
            .collect();
        assert_eq!(prog.len(), 2, "program + mirror");
        for s in prog {
            assert!(s.mask.is_none(), "8 MiB ROM needs no LoROM fold");
        }
    }

    #[test]
    fn fastrom_flag_drives_program_mirror_speed() {
        let slow = sdd1_preset(&big_rom(), 0, false);
        let fast = sdd1_preset(&big_rom(), 0, true);
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

    #[test]
    fn small_rom_program_window_is_masked() {
        // A hypothetical sub-4 MiB S-DD1 image still folds correctly.
        let rom = vec![0u8; 0x20_0000]; // 2 MiB
        let plan = sdd1_preset(&rom, 0, false);
        let prog = plan
            .slots
            .iter()
            .find(|s| matches!(s.target, MapTarget::Rom { .. }) && s.bank_range == (0x00..=0x3F))
            .unwrap();
        assert_eq!(prog.mask, Some(0x1F_FFFF));
    }
}
