//! ExHiROM stock map (mode 25h).
//!
//! ExHiROM doubles the HiROM addressable ROM by carving a second
//! 4 MiB half into `$40-$7D:$0000-$FFFF` (with `base_offset =
//! 0x400000`). The mirrors are swapped relative to plain HiROM:
//!
//! - `$C0-$FF:0000-FFFF`: primary first half (FastROM-eligible,
//!   chip bytes `0..$3F_FFFF`).
//! - `$40-$7D:0000-FFFF`: primary second half (slow, chip
//!   bytes `$40_0000..$7D_FFFF`).
//! - `$00-$3F:8000-FFFF`: slow mirror of the second half.
//!   This is the boot-critical mirror: the cartridge header at
//!   chip `$40FFB0` (where `HEADER_CANDIDATES[2]` points) reads
//!   back at `$00:FFB0`, and the reset vector at chip `$40FFFC`
//!   is what `$00:FFFC` returns when the CPU boots.
//! - `$80-$BF:8000-FFFF`: FastROM-eligible mirror of the
//!   first half.
//!
//! This is the layout of commercial Mode 25 cartridges (Tales of
//! Phantasia among them), checked against snes9x and bsnes-plus.
//! With the mirrors swapped (`$00-$3F` mirroring the first half,
//! `$80-$BF` the second), the SNES would boot from chip byte `$FFFC`
//! instead of `$40FFFC` and land on whatever lives at the start of
//! the ROM: garbage for any commercial ExHiROM dump.
//!
//! Battery SRAM answers at `$20-$3F:6000-7FFF` and again at
//! `$A0-$BF:6000-7FFF`, the same bytes in both halves, as on HiROM
//! (see `super::hirom_sram_slots`).

use crate::cartridge::{BusSpeed, MapPlan, MapSlot, MapTarget};

/// Per-bank stride used by the upper-half mirror slots, one bank
/// step in the address space crosses an entire 64 KiB chip bank.
const EXHIROM_BANK_STRIDE: u32 = 0x10000;

/// Mirror base offset for the first-half mirror (`$80-$BF`).
const FIRST_HALF_MIRROR_BASE: u32 = 0x8000;

/// Mirror base offset for the second-half mirror (`$00-$3F`).
const SECOND_HALF_MIRROR_BASE: u32 = 0x40_8000;

/// Primary slot 1 base, the second 4 MiB chunk lives starting at
/// chip byte `$40_0000`.
const SECOND_HALF_BASE: u32 = 0x40_0000;

/// Build the stock ExHiROM plan.
///
/// `sram_size` is the SRAM size in bytes (0 for "no SRAM").
/// `fastrom` toggles the slots in the FastROM region, the primary
/// first half and the `$80-$BF` mirror, between [`BusSpeed::Slow`]
/// and [`BusSpeed::Fast`].
///
/// The returned plan contains 4 or 6 slots in this order:
///
/// 1. `$C0-$FF : $0000-$FFFF`: primary first-half ROM (FastROM-eligible).
/// 2. `$40-$7D : $0000-$FFFF`: primary second-half ROM
///    (`base = 0x400000`).
/// 3. `$00-$3F : $8000-$FFFF`: slow mirror of the second-half
///    upper bytes (boot-critical: serves the reset vector).
/// 4. `$80-$BF : $8000-$FFFF`: FastROM-eligible mirror of the
///    first-half upper bytes.
/// 5. `$20-$3F : $6000-$7FFF`: SRAM (only when `sram_size > 0`).
/// 6. `$A0-$BF : $6000-$7FFF`: the same SRAM (only when `sram_size > 0`).
///
/// `rom` is accepted for signature parity with the other presets;
/// ExHiROM's stock map
/// has enough slot capacity for the entire 8 MiB linear space, so
/// out-of-bounds reads on smaller-than-canonical dumps fall
/// through to [`PlainCartridge::read_rom`]'s zero default, the
/// cartridge builder pads ROMs to a power of two
/// before construction so commercial 6 MiB Mode 25 cartridges
/// (Tales of Phantasia among them) get well-defined mirrors via
/// the padded buffer.
pub fn exhirom_preset(_rom: &[u8], sram_size: usize, fastrom: bool) -> MapPlan {
    let mut plan = MapPlan::new(if fastrom {
        "exhirom-fastrom"
    } else {
        "exhirom"
    });

    let primary_first = MapSlot::new(
        0xC0..=0xFF,
        0x0000..=0xFFFF,
        MapTarget::Rom { base_offset: 0 },
        if fastrom {
            BusSpeed::Fast
        } else {
            BusSpeed::Slow
        },
    );
    let primary_second = MapSlot::new(
        0x40..=0x7D,
        0x0000..=0xFFFF,
        MapTarget::Rom {
            base_offset: SECOND_HALF_BASE,
        },
        BusSpeed::Slow,
    );
    let mirror_second_low = MapSlot::new(
        0x00..=0x3F,
        0x8000..=0xFFFF,
        MapTarget::Rom {
            base_offset: SECOND_HALF_MIRROR_BASE,
        },
        BusSpeed::Slow,
    )
    .with_bank_stride(EXHIROM_BANK_STRIDE);
    let mirror_first_fast = MapSlot::new(
        0x80..=0xBF,
        0x8000..=0xFFFF,
        MapTarget::Rom {
            base_offset: FIRST_HALF_MIRROR_BASE,
        },
        if fastrom {
            BusSpeed::Fast
        } else {
            BusSpeed::Slow
        },
    )
    .with_bank_stride(EXHIROM_BANK_STRIDE);

    plan.push(primary_first);
    plan.push(primary_second);
    plan.push(mirror_second_low);
    plan.push(mirror_first_fast);

    if sram_size > 0 {
        for slot in super::hirom_sram_slots(sram_size) {
            plan.push(slot);
        }
    }

    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhirom_preset_second_half_at_base_400000() {
        let rom = vec![0u8; 0x60_0000]; // 6 MiB (Tales-like)
        let plan = exhirom_preset(&rom, 0, false);
        assert_eq!(plan.name, "exhirom");
        assert_eq!(plan.slots.len(), 4);

        let s0 = &plan.slots[0];
        assert_eq!(s0.bank_range, 0xC0..=0xFF);
        assert!(matches!(s0.target, MapTarget::Rom { base_offset: 0 }));

        let s1 = &plan.slots[1];
        assert_eq!(s1.bank_range, 0x40..=0x7D);
        assert!(matches!(
            s1.target,
            MapTarget::Rom {
                base_offset: 0x40_0000
            }
        ));
        assert_eq!(s1.speed, BusSpeed::Slow);

        // $40:0000 -> chip byte $40_0000 (start of second half)
        let r = plan.resolve(0x40_0000).unwrap();
        let final_addr = {
            let base = match s1.target {
                MapTarget::Rom { base_offset } => base_offset,
                _ => unreachable!(),
            };
            base + r.target_offset
        };
        assert_eq!(final_addr, 0x40_0000);
    }

    #[test]
    fn exhirom_low_mirror_targets_second_half_for_boot() {
        // The boot reset vector at $00:FFFC must resolve to chip
        // byte $40_FFFC (where the ExHiROM cartridge header places
        // its reset vector). If the low mirror pointed at the
        // *first* half instead, the SNES would jump to whatever
        // happened to live at chip byte $FFFC, usually garbage.
        let rom = vec![0u8; 0x60_0000];
        let plan = exhirom_preset(&rom, 0, true);

        let r = plan.resolve(0x00_FFFC).unwrap();
        let slot = &plan.slots[r.slot_index as usize];
        let base = match slot.target {
            MapTarget::Rom { base_offset } => base_offset,
            _ => panic!("expected ROM target"),
        };
        let final_addr = base + r.target_offset;
        assert_eq!(
            final_addr, 0x40_FFFC,
            "low mirror must serve second-half reset vector"
        );

        // The header itself at $00:FFB0 -> chip $40_FFB0.
        let r = plan.resolve(0x00_FFB0).unwrap();
        let slot = &plan.slots[r.slot_index as usize];
        let base = match slot.target {
            MapTarget::Rom { base_offset } => base_offset,
            _ => panic!("expected ROM target"),
        };
        assert_eq!(base + r.target_offset, 0x40_FFB0);
    }

    #[test]
    fn exhirom_mirrors_match_first_half_in_80_bf() {
        let rom = vec![0u8; 0x60_0000];
        let plan = exhirom_preset(&rom, 0, true);

        let final_addr = |addr24: u32| -> u32 {
            let r = plan.resolve(addr24).unwrap();
            let slot = &plan.slots[r.slot_index as usize];
            let base = match slot.target {
                MapTarget::Rom { base_offset } => base_offset,
                _ => panic!("expected ROM target at {:06X}", addr24),
            };
            base + r.target_offset
        };

        // $80-$BF FastROM mirror serves the first-half image,
        // same chip byte as $C0-$FF.
        assert_eq!(final_addr(0xC0_8000), 0x8000);
        assert_eq!(final_addr(0x80_8000), 0x8000);

        // ...and the per-bank stride takes a full 64 KiB step.
        assert_eq!(final_addr(0xC1_FFFF), 0x1_FFFF);
        assert_eq!(final_addr(0x81_FFFF), 0x1_FFFF);
    }

    #[test]
    fn exhirom_fastrom_flag_drives_the_fastrom_region_speed() {
        let rom = vec![0u8; 0x60_0000];
        let plan_slow = exhirom_preset(&rom, 0, false);
        let plan_fast = exhirom_preset(&rom, 0, true);

        // The first half (`$C0-$FF`) and its `$80-$BF` mirror are in the
        // FastROM region and follow the flag.
        assert_eq!(plan_slow.slots[0].speed, BusSpeed::Slow);
        assert_eq!(plan_fast.slots[0].speed, BusSpeed::Fast);
        assert_eq!(plan_slow.slots[3].speed, BusSpeed::Slow);
        assert_eq!(plan_fast.slots[3].speed, BusSpeed::Fast);
        // The banks below `$80` are always slow (real hardware: A23 gates
        // the fast bus to the upper-half banks only).
        assert_eq!(plan_slow.slots[1].speed, BusSpeed::Slow);
        assert_eq!(plan_fast.slots[1].speed, BusSpeed::Slow);
        assert_eq!(plan_slow.slots[2].speed, BusSpeed::Slow);
        assert_eq!(plan_fast.slots[2].speed, BusSpeed::Slow);
    }

    #[test]
    fn exhirom_with_sram_6_slots() {
        let rom = vec![0u8; 0x60_0000];
        let plan = exhirom_preset(&rom, 32 * 1024, false);
        assert_eq!(plan.slots.len(), 6);
        let low = &plan.slots[4];
        let high = &plan.slots[5];
        assert_eq!(low.bank_range, 0x20..=0x3F);
        assert_eq!(high.bank_range, 0xA0..=0xBF);
        for sram in [low, high] {
            assert_eq!(sram.offset_range, 0x6000..=0x7FFF);
            assert!(matches!(sram.target, MapTarget::Sram { base_offset: 0 }));
            // SRAM mirror mask = size-1 (32 KiB -> 0x7FFF).
            assert_eq!(sram.mask, Some(0x7FFF));
        }
    }

    #[test]
    fn exhirom_a0_to_bf_reaches_the_same_sram_bytes() {
        // The `$80-$BF` ROM mirror starts at `$8000`, so the RAM window
        // below it is free: on a 32 KiB chip `$A5:7F00` reads the byte
        // that `$21:7F00` and `$25:7F00` read.
        let rom = vec![0u8; 0x60_0000];
        let plan = exhirom_preset(&rom, 32 * 1024, true);

        for (low_addr, high_addr) in [(0x21_7F00, 0xA5_7F00), (0x25_7F00, 0xA5_7F00)] {
            let low = plan.resolve(low_addr).unwrap();
            let high = plan.resolve(high_addr).unwrap();
            assert!(matches!(
                plan.slots[high.slot_index as usize].target,
                MapTarget::Sram { .. }
            ));
            assert_eq!(high.target_offset, low.target_offset);
            assert_eq!(high.target_offset, 0x3F00);
        }
        // The ROM mirror above the window is untouched.
        let rom_byte = plan.resolve(0xA5_8000).unwrap();
        assert!(matches!(
            plan.slots[rom_byte.slot_index as usize].target,
            MapTarget::Rom { .. }
        ));
    }
}
