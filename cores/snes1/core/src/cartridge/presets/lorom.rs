//! LoROM stock map (mode 20h).
//!
//! Banks `$00-$7D` and `$80-$FF` each expose 32 KiB of ROM in their
//! upper half (`$8000-$FFFF`); the lower half (`$0000-$7FFF`) is
//! left to system RAM, MMIO, and the optional battery-backed SRAM
//! window in `$70-$7D`. The two ROM windows are mirrors of the
//! same chip image, so the cartridge fills 4 MiB of address space
//! with just `(126 + 128) x 32 KiB = ~8 MiB` of accessible address
//! reads pointing at up to 4 MiB of real chip. FastROM, when the
//! cartridge supports it (header bit `$25.4`) and the CPU enables
//! it (MEMSEL `$420D.0`), upgrades the upper mirror (`$80-$FF`)
//! from 8 to 6 master cycles per access, the lower mirror stays
//! slow because address line A23 still gates the fast bus on real
//! hardware.

use crate::cartridge::{BusSpeed, MapPlan, MapSlot, MapTarget};

/// LoROM linear capacity: 4 MiB. ROMs strictly smaller than this
/// are masked so the slots fold their linear offset back into the
/// real chip buffer.
const LOROM_LINEAR_BYTES: u32 = 0x40_0000;

/// Build the stock LoROM plan.
///
/// `sram_size` is the SRAM size in bytes (0 for "no SRAM").
/// `fastrom` toggles the upper-mirror slot between
/// [`BusSpeed::Slow`] and [`BusSpeed::Fast`].
///
/// The returned plan contains 2 or 3 slots in this order:
///
/// 1. `$00-$7D : $8000-$FFFF`: ROM (slow, never FastROM).
/// 2. `$80-$FF : $8000-$FFFF`: ROM mirror, FastROM-eligible.
/// 3. `$70-$7D : $0000-$7FFF`: SRAM (only when `sram_size > 0`).
///
/// SRAM and ROM slots do not overlap in `(bank, offset)`, so the
/// first-wins resolver picks the right target regardless of order.
pub fn lorom_preset(rom: &[u8], sram_size: usize, fastrom: bool) -> MapPlan {
    let mut plan = MapPlan::new(if fastrom { "lorom-fastrom" } else { "lorom" });
    let mask = lorom_mask(rom.len());

    let mut slot_lo = MapSlot::new(
        0x00..=0x7D,
        0x8000..=0xFFFF,
        MapTarget::Rom { base_offset: 0 },
        BusSpeed::Slow,
    );
    let mut slot_hi = MapSlot::new(
        0x80..=0xFF,
        0x8000..=0xFFFF,
        MapTarget::Rom { base_offset: 0 },
        if fastrom {
            BusSpeed::Fast
        } else {
            BusSpeed::Slow
        },
    );
    if let Some(m) = mask {
        slot_lo = slot_lo.with_mask(m);
        slot_hi = slot_hi.with_mask(m);
    }
    plan.push(slot_lo);
    plan.push(slot_hi);

    if sram_size > 0 {
        plan.push(
            MapSlot::new(
                0x70..=0x7D,
                0x0000..=0x7FFF,
                MapTarget::Sram { base_offset: 0 },
                BusSpeed::Slow,
            )
            .with_mask(super::sram_mirror_mask(sram_size)),
        );
    }

    plan
}

/// Pick the mask that folds slot-relative linear offsets back into
/// the actual ROM buffer when the ROM is smaller than the 4 MiB
/// LoROM window. Returns `None` for ROMs >= 4 MiB (the slot's
/// raw linear offset already lands inside the chip image).
///
/// The mask assumes `rom.len()` is a power of two; the cartridge
/// builder is responsible for padding the buffer to its next
/// power of two before constructing the cartridge.
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

    #[test]
    fn lorom_preset_3_slots_for_1mib_with_sram() {
        let rom = vec![0u8; 0x10_0000]; // 1 MiB
        let plan = lorom_preset(&rom, 2048, false);
        assert_eq!(plan.name, "lorom");
        assert_eq!(plan.slots.len(), 3);

        let s0 = &plan.slots[0];
        assert_eq!(s0.bank_range, 0x00..=0x7D);
        assert_eq!(s0.offset_range, 0x8000..=0xFFFF);
        assert!(matches!(s0.target, MapTarget::Rom { base_offset: 0 }));
        assert_eq!(s0.speed, BusSpeed::Slow);
        assert_eq!(s0.mask, Some(0x0F_FFFF)); // 1 MiB - 1

        let s1 = &plan.slots[1];
        assert_eq!(s1.bank_range, 0x80..=0xFF);
        assert_eq!(s1.offset_range, 0x8000..=0xFFFF);
        assert_eq!(s1.speed, BusSpeed::Slow);
        assert_eq!(s1.mask, Some(0x0F_FFFF));

        let s2 = &plan.slots[2];
        assert_eq!(s2.bank_range, 0x70..=0x7D);
        assert_eq!(s2.offset_range, 0x0000..=0x7FFF);
        assert!(matches!(s2.target, MapTarget::Sram { base_offset: 0 }));
        assert_eq!(s2.speed, BusSpeed::Slow);
        // The SRAM slot carries a *mirror* mask (size-1), not the
        // ROM-size mask: the 2 KiB chip wraps across the window.
        assert_eq!(s2.mask, Some(0x7FF));
    }

    #[test]
    fn lorom_fastrom_flag_drives_upper_half_speed() {
        let rom = vec![0u8; 0x20_0000]; // 2 MiB
        let plan_slow = lorom_preset(&rom, 0, false);
        let plan_fast = lorom_preset(&rom, 0, true);

        assert_eq!(plan_slow.name, "lorom");
        assert_eq!(plan_fast.name, "lorom-fastrom");
        assert_eq!(plan_slow.slots[1].speed, BusSpeed::Slow);
        assert_eq!(plan_fast.slots[1].speed, BusSpeed::Fast);
        // The lower mirror is always Slow even when FastROM is set,
        // address line A23 still gates the fast bus on real hardware.
        assert_eq!(plan_slow.slots[0].speed, BusSpeed::Slow);
        assert_eq!(plan_fast.slots[0].speed, BusSpeed::Slow);
    }

    #[test]
    fn lorom_no_sram_slot_when_size_zero() {
        let rom = vec![0u8; 0x10_0000];
        let plan = lorom_preset(&rom, 0, false);
        assert_eq!(plan.slots.len(), 2);
        assert!(plan
            .slots
            .iter()
            .all(|s| !matches!(s.target, MapTarget::Sram { .. })));
    }

    #[test]
    fn lorom_no_mask_for_4mib_rom() {
        // Right at the LoROM linear capacity, no mask needed.
        let rom = vec![0u8; 0x40_0000];
        let plan = lorom_preset(&rom, 0, false);
        assert!(plan.slots[0].mask.is_none());
        assert!(plan.slots[1].mask.is_none());
    }

    #[test]
    fn lorom_mask_for_512kib_rom() {
        let rom = vec![0u8; 0x8_0000];
        let plan = lorom_preset(&rom, 0, false);
        assert_eq!(plan.slots[0].mask, Some(0x7_FFFF));
        assert_eq!(plan.slots[1].mask, Some(0x7_FFFF));
    }

    #[test]
    fn lorom_sram_slot_carries_size_mask() {
        // The SRAM slot mirror-mask is size-1 regardless of ROM size.
        let rom = vec![0u8; 0x10_0000];
        assert_eq!(
            lorom_preset(&rom, 2048, false).slots.last().unwrap().mask,
            Some(0x7FF)
        );
        assert_eq!(
            lorom_preset(&rom, 8 * 1024, false)
                .slots
                .last()
                .unwrap()
                .mask,
            Some(0x1FFF)
        );
        assert_eq!(
            lorom_preset(&rom, 32 * 1024, false)
                .slots
                .last()
                .unwrap()
                .mask,
            Some(0x7FFF)
        );
    }
}
