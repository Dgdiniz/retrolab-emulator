//! The Mega Drive's map slot.
//!
//! The 68000 has a flat 24-bit address space, so a slot of a cartridge's
//! [`MapPlan`] is one contiguous range of it, pointing at a [`MapTarget`]
//! (ROM, SRAM, a chip's window, or open bus) and folded by an optional
//! mask (a ROM smaller than its window mirrors). The plan, its resolution
//! and [`Resolved`] are retro-core's, monomorphised on this slot; the bus's
//! page table sends the cartridge window to the cartridge, whose `read_bus`
//! and `write_bus` resolve every access through the plan, the first slot
//! that covers it winning.

use std::ops::RangeInclusive;

use retro_core::cartridge::SlotShape;

/// Where the bytes addressed by a slot actually live. Shared with every
/// console through `retro-core`; re-exported so `map_plan::MapTarget`
/// resolves at every call site.
pub use retro_core::cartridge::MapTarget;
pub use retro_core::cartridge::Resolved;

/// A Mega Drive cartridge's map plan.
pub type MapPlan = retro_core::cartridge::MapPlan<MapSlot>;

/// Smallest `power-of-two - 1` mask that covers `rom_len` (minimum 64 KiB),
/// used to mirror an under-sized ROM across its 4 MiB address window, the
/// Mega Drive behaviour where a small ROM appears repeated. Combined with a
/// ROM read that answers `0xFF` past the image, it reproduces "mirror,
/// padding to open bus" without physically padding the ROM buffer.
pub fn rom_mirror_mask(rom_len: usize) -> u32 {
    if rom_len == 0 {
        return 0;
    }
    let mut size = 0x1_0000usize; // minimum 64 KiB
    while size < rom_len {
        size <<= 1;
    }
    (size - 1) as u32
}

/// One linear slot in a Mega Drive cartridge's map plan.
///
/// `addr_range` is the matching window (a contiguous 24-bit range).
/// `mask`, if present, is ANDed with the slot-relative offset before it is
/// added to the [`MapTarget`] base: how a ROM smaller than its window
/// folds back inside the real image (power-of-two mirroring).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapSlot {
    pub addr_range: RangeInclusive<u32>,
    pub mask: Option<u32>,
    pub target: MapTarget,
}

impl MapSlot {
    /// Build a slot with no mask.
    pub fn new(addr_range: RangeInclusive<u32>, target: MapTarget) -> Self {
        Self {
            addr_range,
            mask: None,
            target,
        }
    }

    /// Attach a mask that wraps the slot-relative offset before it
    /// reaches the target. Used for cartridges whose ROM is smaller than
    /// the address window they live in (mirroring).
    pub fn with_mask(mut self, mask: u32) -> Self {
        self.mask = Some(mask);
        self
    }

    /// Iterate over every 64 KiB page this slot covers, as page indices
    /// (`addr >> 16`) in the `0..=0xFF` range the bus page table uses. A
    /// slot whose range does not start or end on a 64 KiB boundary still
    /// yields the whole page containing either endpoint; the per-access
    /// [`SlotShape::contains`] check filters byte by byte inside such a
    /// page.
    pub fn covered_pages(&self) -> impl Iterator<Item = u8> + '_ {
        let start = (*self.addr_range.start() >> 16) as u8;
        let end = (*self.addr_range.end() >> 16) as u8;
        start..=end
    }
}

impl SlotShape for MapSlot {
    type Target = MapTarget;

    #[inline]
    fn contains(&self, addr: u32) -> bool {
        self.addr_range.contains(&addr)
    }

    #[inline]
    fn linear_offset(&self, addr: u32) -> u32 {
        addr.wrapping_sub(*self.addr_range.start())
    }

    #[inline]
    fn mask(&self) -> Option<u32> {
        self.mask
    }

    #[inline]
    fn target(&self) -> MapTarget {
        self.target
    }

    fn bounds(&self) -> (u32, u32) {
        (*self.addr_range.start(), *self.addr_range.end())
    }

    fn size(&self) -> usize {
        (*self.addr_range.end() as usize) - (*self.addr_range.start() as usize) + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom_slot() -> MapSlot {
        MapSlot::new(0x000000..=0x3FFFFF, MapTarget::Rom { base_offset: 0 })
    }

    #[test]
    fn a_slot_is_its_range_its_mask_and_its_target() {
        let slot = rom_slot().with_mask(0x7FFFF);
        assert!(slot.contains(0x3FFFFF) && !slot.contains(0x400000));
        assert_eq!(slot.linear_offset(0x180001), 0x180001);
        assert_eq!(slot.compute_target_offset(0x180001), 0x00001);
        assert_eq!(slot.bounds(), (0x000000, 0x3FFFFF));
        assert_eq!(slot.size(), 0x400000);
        assert_eq!(slot.target(), MapTarget::Rom { base_offset: 0 });
        let mailbox = MapSlot::new(0xA15000..=0xA1500B, MapTarget::OpenBus);
        assert_eq!(mailbox.linear_offset(0xA15004), 4);
        assert_eq!(mailbox.size(), 12);
    }

    #[test]
    fn a_chip_window_ahead_of_the_rom_wins_its_addresses() {
        let mut plan = MapPlan::new("svp-like");
        plan.push(MapSlot::new(
            0x300000..=0x31FFFF,
            MapTarget::ChipMmio {
                chip_id: 0,
                base: 0,
            },
        ));
        plan.push(rom_slot());
        let resolved = plan.resolve(0x300010).expect("the chip window wins");
        assert_eq!((resolved.slot_index, resolved.target_offset), (0, 0x10));
        assert!(plan.resolve(0xFF0000).is_none(), "work RAM is no slot's");
    }

    #[test]
    fn covered_pages_rom_window() {
        let pages: Vec<u8> = rom_slot().covered_pages().collect();
        // 0x000000-0x3FFFFF spans pages 0x00..=0x3F (64 pages).
        assert_eq!(pages.len(), 0x40);
        assert_eq!(pages.first().copied(), Some(0x00));
        assert_eq!(pages.last().copied(), Some(0x3F));
    }

    #[test]
    fn covered_pages_single_page_window() {
        // The SVP mailbox lives entirely inside page 0xA1.
        let slot = MapSlot::new(0xA15000..=0xA1500B, MapTarget::OpenBus);
        let pages: Vec<u8> = slot.covered_pages().collect();
        assert_eq!(pages, vec![0xA1]);
    }

    #[test]
    fn rom_mirror_mask_rounds_up_to_power_of_two() {
        assert_eq!(rom_mirror_mask(0), 0);
        // Under the 64 KiB floor still yields the 64 KiB mask.
        assert_eq!(rom_mirror_mask(1), 0xFFFF);
        assert_eq!(rom_mirror_mask(0x1_0000), 0xFFFF);
        // Exact powers of two.
        assert_eq!(rom_mirror_mask(0x8_0000), 0x7_FFFF);
        assert_eq!(rom_mirror_mask(0x40_0000), 0x3F_FFFF);
        // Non-power-of-two rounds up (1.25 MiB -> 2 MiB mask).
        assert_eq!(rom_mirror_mask(0x14_0000), 0x1F_FFFF);
    }

    #[test]
    fn covered_pages_dram_two_pages() {
        // The SVP DRAM at 0x300000-0x31FFFF spans pages 0x30 and 0x31.
        let slot = MapSlot::new(
            0x300000..=0x31FFFF,
            MapTarget::ChipMmio {
                chip_id: 0,
                base: 0,
            },
        );
        let pages: Vec<u8> = slot.covered_pages().collect();
        assert_eq!(pages, vec![0x30, 0x31]);
    }
}
