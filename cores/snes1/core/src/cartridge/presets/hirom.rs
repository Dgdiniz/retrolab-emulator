//! HiROM stock map (mode 21h).
//!
//! HiROM exposes a contiguous 64 KiB-per-bank ROM image in
//! `$C0-$FF:0000-FFFF` (a flat 4 MiB chip view, FastROM-eligible),
//! with two upper-half mirror windows:
//!
//! - `$00-$3F:8000-FFFF`: slow mirror covering the upper halves
//!   of the same banks. Real hardware exposes only the upper half
//!   (`$8000-$FFFF`) of each chip bank here; the lower halves are
//!   not addressable through the mirror.
//! - `$80-$BF:8000-FFFF`: FastROM-eligible mirror with the same
//!   semantics.
//!
//! The mirror windows are partial-bank views over full-bank chip
//! steps. Each bank step in the mirror crosses an entire 64 KiB
//! chip bank, even though the offset window is only 32 KiB wide;
//! the slot uses [`MapSlot::with_bank_stride`] to express this
//! correctly. Combined with `MapTarget::Rom { base_offset: 0x8000 }`
//! the mirrors land on the upper-half bytes of the matching chip
//! bank (so `$00:8000` resolves to chip byte `$8000`, the same
//! byte the primary slot serves at `$C0:8000`).
//!
//! Battery-backed SRAM answers at `$20-$3F:6000-7FFF` in stock HiROM,
//! and again at `$A0-$BF:6000-7FFF`, where the same bytes appear
//! (see `super::hirom_sram_slots`). Cartridges with no SRAM omit both
//! slots.

use crate::cartridge::{BusSpeed, MapPlan, MapSlot, MapTarget};

/// HiROM linear capacity: 4 MiB.
const HIROM_LINEAR_BYTES: u32 = 0x40_0000;

/// Per-bank stride used by the upper-half mirror slots, one bank
/// step in the address space crosses an entire 64 KiB chip bank.
const HIROM_BANK_STRIDE: u32 = 0x10000;

/// Base offset applied to mirror slots so the slot-relative offset
/// (which starts at 0 for `$8000`) lands on the upper-half of each
/// chip bank.
const HIROM_MIRROR_BASE_OFFSET: u32 = 0x8000;

/// Build the stock HiROM plan.
///
/// `sram_size` is the SRAM size in bytes (0 for "no SRAM").
/// `fastrom` toggles the slots in the FastROM region, the primary
/// window and the `$80-$BF` mirror, between [`BusSpeed::Slow`] and
/// [`BusSpeed::Fast`].
///
/// The returned plan contains 3 or 5 slots in this order:
///
/// 1. `$C0-$FF : $0000-$FFFF`: primary ROM (FastROM-eligible), 64 KiB/bank.
/// 2. `$00-$3F : $8000-$FFFF`: slow upper-half mirror.
/// 3. `$80-$BF : $8000-$FFFF`: FastROM-eligible mirror.
/// 4. `$20-$3F : $6000-$7FFF`: SRAM (only when `sram_size > 0`).
/// 5. `$A0-$BF : $6000-$7FFF`: the same SRAM (only when `sram_size > 0`).
pub fn hirom_preset(rom: &[u8], sram_size: usize, fastrom: bool) -> MapPlan {
    let mut plan = MapPlan::new(if fastrom { "hirom-fastrom" } else { "hirom" });
    let mask = hirom_mask(rom.len());

    let mut primary = MapSlot::new(
        0xC0..=0xFF,
        0x0000..=0xFFFF,
        MapTarget::Rom { base_offset: 0 },
        if fastrom {
            BusSpeed::Fast
        } else {
            BusSpeed::Slow
        },
    );
    let mut mirror_low = MapSlot::new(
        0x00..=0x3F,
        0x8000..=0xFFFF,
        MapTarget::Rom {
            base_offset: HIROM_MIRROR_BASE_OFFSET,
        },
        BusSpeed::Slow,
    )
    .with_bank_stride(HIROM_BANK_STRIDE);
    let mut mirror_fast = MapSlot::new(
        0x80..=0xBF,
        0x8000..=0xFFFF,
        MapTarget::Rom {
            base_offset: HIROM_MIRROR_BASE_OFFSET,
        },
        if fastrom {
            BusSpeed::Fast
        } else {
            BusSpeed::Slow
        },
    )
    .with_bank_stride(HIROM_BANK_STRIDE);

    if let Some(m) = mask {
        primary = primary.with_mask(m);
        mirror_low = mirror_low.with_mask(m);
        mirror_fast = mirror_fast.with_mask(m);
    }

    plan.push(primary);
    plan.push(mirror_low);
    plan.push(mirror_fast);

    if sram_size > 0 {
        for slot in super::hirom_sram_slots(sram_size) {
            plan.push(slot);
        }
    }

    plan
}

fn hirom_mask(rom_len: usize) -> Option<u32> {
    if rom_len == 0 || rom_len as u32 >= HIROM_LINEAR_BYTES {
        None
    } else {
        Some((rom_len as u32) - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plan's SRAM slots, in order.
    fn sram_slots(plan: &MapPlan) -> Vec<&MapSlot> {
        plan.slots
            .iter()
            .filter(|s| matches!(s.target, MapTarget::Sram { .. }))
            .collect()
    }

    #[test]
    fn hirom_preset_5_slots_for_4mib_with_sram() {
        let rom = vec![0u8; 0x40_0000];
        let plan = hirom_preset(&rom, 8 * 1024, false);
        assert_eq!(plan.name, "hirom");
        assert_eq!(plan.slots.len(), 5);

        let s0 = &plan.slots[0];
        assert_eq!(s0.bank_range, 0xC0..=0xFF);
        assert_eq!(s0.offset_range, 0x0000..=0xFFFF);
        assert!(matches!(s0.target, MapTarget::Rom { base_offset: 0 }));
        assert_eq!(s0.speed, BusSpeed::Slow);
        assert!(s0.bank_stride.is_none(), "primary slot uses default stride");

        let s1 = &plan.slots[1];
        assert_eq!(s1.bank_range, 0x00..=0x3F);
        assert_eq!(s1.offset_range, 0x8000..=0xFFFF);
        assert_eq!(s1.speed, BusSpeed::Slow);
        assert_eq!(s1.bank_stride, Some(0x10000));
        assert!(matches!(
            s1.target,
            MapTarget::Rom {
                base_offset: 0x8000
            }
        ));

        let s2 = &plan.slots[2];
        assert_eq!(s2.bank_range, 0x80..=0xBF);
        assert_eq!(s2.offset_range, 0x8000..=0xFFFF);
        assert_eq!(s2.speed, BusSpeed::Slow);
        assert_eq!(s2.bank_stride, Some(0x10000));

        let s3 = &plan.slots[3];
        assert_eq!(s3.bank_range, 0x20..=0x3F);
        assert_eq!(s3.offset_range, 0x6000..=0x7FFF);
        assert!(matches!(s3.target, MapTarget::Sram { base_offset: 0 }));

        let s4 = &plan.slots[4];
        assert_eq!(s4.bank_range, 0xA0..=0xBF);
        assert_eq!(s4.offset_range, 0x6000..=0x7FFF);
        assert!(matches!(s4.target, MapTarget::Sram { base_offset: 0 }));
    }

    #[test]
    fn hirom_fastrom_flag_drives_the_fastrom_region_speed() {
        let rom = vec![0u8; 0x40_0000];
        let plan_slow = hirom_preset(&rom, 0, false);
        let plan_fast = hirom_preset(&rom, 0, true);

        assert_eq!(plan_slow.name, "hirom");
        assert_eq!(plan_fast.name, "hirom-fastrom");
        // The primary window (`$C0-$FF`) and the `$80-$BF` mirror are in
        // the FastROM region and follow the flag.
        assert_eq!(plan_slow.slots[0].speed, BusSpeed::Slow);
        assert_eq!(plan_fast.slots[0].speed, BusSpeed::Fast);
        assert_eq!(plan_slow.slots[2].speed, BusSpeed::Slow);
        assert_eq!(plan_fast.slots[2].speed, BusSpeed::Fast);
        // The `$00-$3F` mirror is always slow.
        assert_eq!(plan_slow.slots[1].speed, BusSpeed::Slow);
        assert_eq!(plan_fast.slots[1].speed, BusSpeed::Slow);
    }

    #[test]
    fn hirom_sram_at_20_to_3f_and_a0_to_bf_when_present() {
        let rom = vec![0u8; 0x40_0000];
        let with_sram = hirom_preset(&rom, 8 * 1024, false);
        let no_sram = hirom_preset(&rom, 0, false);

        let windows = sram_slots(&with_sram);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].bank_range, 0x20..=0x3F);
        assert_eq!(windows[1].bank_range, 0xA0..=0xBF);
        for window in windows {
            assert_eq!(window.offset_range, 0x6000..=0x7FFF);
            assert!(matches!(window.target, MapTarget::Sram { base_offset: 0 }));
        }

        assert_eq!(no_sram.slots.len(), 3);
        assert!(sram_slots(&no_sram).is_empty());
    }

    #[test]
    fn hirom_a0_to_bf_reaches_the_same_sram_bytes() {
        // A bank in $A0-$BF reaches the bytes of the bank 128 below it,
        // and $80-$9F holds no RAM at all.
        let rom = vec![0u8; 0x40_0000];
        let plan = hirom_preset(&rom, 32 * 1024, false);

        for addr in [0x20_6000, 0x21_7F00, 0x22_7F00, 0x23_7F00, 0x3F_7FFF] {
            let low = plan.resolve(addr).unwrap();
            let high = plan.resolve(addr | 0x80_0000).unwrap();
            assert!(matches!(
                plan.slots[high.slot_index as usize].target,
                MapTarget::Sram { .. }
            ));
            assert_eq!(high.target_offset, low.target_offset, "{addr:06X}");
        }
        // 32 KiB: four banks of 8 KiB, then the chip repeats.
        assert_eq!(plan.resolve(0xA5_7F00).unwrap().target_offset, 0x3F00);
        assert_eq!(plan.resolve(0xA6_7F00).unwrap().target_offset, 0x5F00);
        assert_eq!(plan.resolve(0xA7_7F00).unwrap().target_offset, 0x7F00);

        assert!(plan.resolve(0x80_6000).is_none());
        assert!(plan.resolve(0x9F_7FFF).is_none());
    }

    #[test]
    fn hirom_size_check_through_both_halves() {
        use crate::cartridge::{PlainCartridge, SnesCartridge};
        // A RAM-size check of the shape games run: three different bytes
        // written through banks $21-$23 must all read back through
        // $A5-$A7. A 32 KiB chip keeps them apart; an 8 KiB one folds the
        // three banks onto one byte, so the check sees the last write.
        let rom = vec![0u8; 0x40_0000];
        let written = [(0x21_7F00, 0xA6), (0x22_7F00, 0xF3), (0x23_7F00, 0x0C)];
        let read_back = [0xA5_7F00, 0xA6_7F00, 0xA7_7F00];

        let check = |sram_size: usize| -> Vec<u8> {
            let plan = hirom_preset(&rom, sram_size, false);
            let mut cart = PlainCartridge::new(rom.clone(), sram_size, plan);
            for (addr, value) in written {
                let offset = cart.map_plan().resolve(addr).unwrap().target_offset;
                cart.write_sram(offset, value);
            }
            read_back
                .iter()
                .map(|&addr| {
                    let offset = cart.map_plan().resolve(addr).unwrap().target_offset;
                    cart.read_sram(offset)
                })
                .collect()
        };

        assert_eq!(check(32 * 1024), vec![0xA6, 0xF3, 0x0C]);
        assert_eq!(check(8 * 1024), vec![0x0C, 0x0C, 0x0C]);
    }

    #[test]
    fn hirom_mirror_resolves_to_chip_upper_halves() {
        // Confirms the bank_stride + base_offset combo produces the
        // correct chip byte for the mirror windows. The primary
        // slot at $C0:8000 should resolve to the same chip byte as
        // both mirror slots at $00:8000 and $80:8000, that's what
        // "mirror" means.
        let rom = vec![0u8; 0x40_0000];
        let plan = hirom_preset(&rom, 0, true);

        let primary = plan.resolve(0xC0_8000).unwrap();
        let mirror_low = plan.resolve(0x00_8000).unwrap();
        let mirror_fast = plan.resolve(0x80_8000).unwrap();

        // All three slots should compute the same final ROM byte
        // (target_offset + base_offset).
        let final_addr = |r: &crate::cartridge::Resolved| -> u32 {
            let slot = &plan.slots[r.slot_index as usize];
            let base = match slot.target {
                MapTarget::Rom { base_offset } => base_offset,
                _ => panic!("expected ROM target"),
            };
            base + r.target_offset
        };
        assert_eq!(final_addr(&primary), 0x8000);
        assert_eq!(final_addr(&mirror_low), 0x8000);
        assert_eq!(final_addr(&mirror_fast), 0x8000);

        // A different mirror byte, just to verify the bank_stride
        // is taking effect for non-bank-0 banks too.
        let p2 = plan.resolve(0xC2_8000).unwrap();
        let m2 = plan.resolve(0x02_8000).unwrap();
        assert_eq!(final_addr(&p2), 0x2_8000);
        assert_eq!(final_addr(&m2), 0x2_8000);
    }

    #[test]
    fn hirom_mask_for_2mib_rom() {
        let rom = vec![0u8; 0x20_0000];
        let plan = hirom_preset(&rom, 0, false);
        assert_eq!(plan.slots[0].mask, Some(0x1F_FFFF));
        assert_eq!(plan.slots[1].mask, Some(0x1F_FFFF));
        assert_eq!(plan.slots[2].mask, Some(0x1F_FFFF));
    }

    #[test]
    fn hirom_sram_slot_carries_size_mask() {
        // Donkey Kong Country: HiROM, 2 KiB battery SRAM. Both SRAM
        // windows must mask the slot-relative offset to size-1 so the
        // 2 KiB chip mirrors across the 8 KiB-per-bank window.
        let rom = vec![0u8; 0x40_0000];
        for (sram_size, mask) in [(2048, 0x7FF), (8 * 1024, 0x1FFF), (32 * 1024, 0x7FFF)] {
            let plan = hirom_preset(&rom, sram_size, false);
            let windows = sram_slots(&plan);
            assert_eq!(windows.len(), 2);
            for window in windows {
                assert_eq!(window.mask, Some(mask), "{sram_size} bytes");
            }
        }
    }

    #[test]
    fn hirom_2kib_sram_mirrors_within_window() {
        // The 2 KiB SRAM repeats every 2 KiB across the whole
        // $20-$3F:$6000-$7FFF window. $20:6000, $20:6800 and $21:6000
        // all resolve to the same chip byte (offset 0).
        let rom = vec![0u8; 0x40_0000];
        let plan = hirom_preset(&rom, 2048, false);

        assert_eq!(plan.resolve(0x20_6000).unwrap().target_offset, 0x000);
        assert_eq!(plan.resolve(0x20_6800).unwrap().target_offset, 0x000);
        assert_eq!(plan.resolve(0x21_6000).unwrap().target_offset, 0x000);
        // A distinct byte inside the first 2 KiB stays distinct.
        assert_eq!(plan.resolve(0x20_6001).unwrap().target_offset, 0x001);
        assert_eq!(plan.resolve(0x20_67FF).unwrap().target_offset, 0x7FF);
    }

    #[test]
    fn hirom_dkc_style_mirror_read_through_cartridge() {
        use crate::cartridge::{PlainCartridge, SnesCartridge};
        // End-to-end DKC anti-piracy shape: write a marker at the
        // base SRAM address, read it back from a mirrored address
        // 0x800 higher; without the mask the mirror read would be 0.
        let rom = vec![0u8; 0x40_0000];
        let plan = hirom_preset(&rom, 2048, false);
        let mut cart = PlainCartridge::new(rom, 2048, plan);

        let base = cart.map_plan().resolve(0x20_6000).unwrap().target_offset;
        let mirror = cart.map_plan().resolve(0x20_6800).unwrap().target_offset;
        cart.write_sram(base, 0x5A);
        assert_eq!(cart.read_sram(mirror), 0x5A, "2 KiB SRAM must mirror");
    }
}
