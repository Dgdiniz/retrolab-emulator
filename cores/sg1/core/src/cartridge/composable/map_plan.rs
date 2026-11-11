//! Slot-based cartridge memory mapping plan (Genesis flavor).
//!
//! The Genesis bus does not know what a "SEGA mapper" or an "SVP"
//! cartridge is. Every cartridge presents the bus with a [`MapPlan`],
//! an ordered list of [`MapSlot`]s. Each slot covers a contiguous
//! *linear* 24-bit address range and points at a [`MapTarget`] (ROM,
//! SRAM, a per-chip MMIO region, or open bus). The bus's own 256-entry page
//! table is fixed and sends the cartridge window to the cartridge, whose
//! `read_bus` / `write_bus` walk the plan's slots on every access (the first
//! match wins) and dispatch to a single trait method on the cartridge.
//!
//! This is the Genesis counterpart of the SNES `map_plan`: the SNES
//! addresses cartridges as `bank:offset`, so its slots are a
//! `bank_range x offset_range` rectangle. The 68000 has a **flat** 24-bit
//! address space, so a Genesis slot is a single `RangeInclusive<u32>`.
//! The [`MapTarget`] vocabulary (`Rom`/`Sram`/`ChipMmio`/`OpenBus`) is the
//! shared `retro-core` type, identical across both consoles.

use std::ops::RangeInclusive;

/// Where the bytes addressed by a slot actually live. Shared with every
/// console through `retro-core`; re-exported so `map_plan::MapTarget`
/// resolves at every call site.
pub use retro_core::cartridge::MapTarget;

/// Smallest `power-of-two - 1` mask that covers `rom_len` (minimum 64 KiB),
/// used to mirror an under-sized ROM across its 4 MiB address window, the
/// Genesis behaviour where a small ROM appears repeated. Combined with a
/// `read_rom` that returns `0xFF` out of bounds, it reproduces "mirror,
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

/// One linear slot in a Genesis cartridge map plan.
///
/// `addr_range` is the matching window (a contiguous 24-bit range).
/// `mask`, if present, is bitwise-AND'd against the slot-relative offset
/// before being added to the [`MapTarget`] base, this is how a ROM
/// smaller than its address window folds the upper offset bits back
/// inside the real ROM image (power-of-two mirroring).
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

    /// `true` iff `addr` falls inside this slot's range.
    pub fn contains(&self, addr: u32) -> bool {
        self.addr_range.contains(&addr)
    }

    /// Linear offset *within* the slot, with the mask (if any) applied.
    /// The caller adds the target's base offset before indexing
    /// ROM/SRAM/chip storage.
    ///
    /// Panics (debug builds) if the address is outside the slot; callers
    /// should gate on [`MapSlot::contains`] first (the bus does, via
    /// [`MapPlan::resolve`]).
    pub fn compute_target_offset(&self, addr: u32) -> u32 {
        debug_assert!(self.addr_range.contains(&addr));
        let linear = addr.wrapping_sub(*self.addr_range.start());
        match self.mask {
            Some(mask) => linear & mask,
            None => linear,
        }
    }

    /// Iterate over every 64 KiB page this slot covers, as page indices
    /// (`addr >> 16`) in the `0..=0xFF` range the Genesis bus page table
    /// uses. A slot whose range does not start/end on a 64 KiB boundary
    /// still yields the whole page containing either endpoint; the
    /// per-access [`MapSlot::contains`] check handles byte-level filtering
    /// inside such a page.
    pub fn covered_pages(&self) -> impl Iterator<Item = u8> + '_ {
        let start = (*self.addr_range.start() >> 16) as u8;
        let end = (*self.addr_range.end() >> 16) as u8;
        start..=end
    }
}

/// Resolution of an address inside a [`MapPlan`].
///
/// `slot_index` is the position of the matching slot in
/// `MapPlan::slots`. `target_offset` is the slot-relative offset
/// (already masked) that callers add to the [`MapTarget`] base to reach
/// the cartridge byte.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub slot_index: u16,
    pub target_offset: u32,
}

/// Ordered list of slots describing a cartridge's view of the address
/// space. Lookup is a linear walk over `slots`; the first match wins, so
/// callers push more-specific slots before more-general ones when the
/// windows overlap. A name is attached for diagnostics and persistence
/// (e.g. `"sega"`, `"plain"`, `"svp"`, or `"custom"` for manifest plans).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MapPlan {
    pub name: String,
    pub slots: Vec<MapSlot>,
}

impl MapPlan {
    /// Build an empty plan with the given diagnostic name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            slots: Vec::new(),
        }
    }

    /// Append a slot. The first slot that covers an address wins.
    pub fn push(&mut self, slot: MapSlot) {
        self.slots.push(slot);
    }

    /// Resolve a 24-bit address. Returns `None` if no slot covers it,
    /// the bus treats that as open bus.
    pub fn resolve(&self, addr: u32) -> Option<Resolved> {
        for (i, slot) in self.slots.iter().enumerate() {
            if slot.contains(addr) {
                return Some(Resolved {
                    slot_index: i as u16,
                    target_offset: slot.compute_target_offset(addr),
                });
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom_slot() -> MapSlot {
        MapSlot::new(0x000000..=0x3FFFFF, MapTarget::Rom { base_offset: 0 })
    }

    #[test]
    fn resolve_rom_slot() {
        let mut plan = MapPlan::new("plain");
        plan.push(rom_slot());

        let r = plan.resolve(0x000000).expect("matches ROM slot");
        assert_eq!(r.slot_index, 0);
        assert_eq!(r.target_offset, 0);

        let r = plan.resolve(0x123456).expect("matches ROM slot");
        assert_eq!(r.target_offset, 0x123456);

        let r = plan.resolve(0x3FFFFF).expect("last ROM byte");
        assert_eq!(r.target_offset, 0x3FFFFF);
    }

    #[test]
    fn resolve_mask_folds_mirror() {
        // 512 KiB ROM mapped into the full 4 MiB window: the mask folds
        // over-large offsets back into the first 512 KiB.
        let slot = rom_slot().with_mask(0x7FFFF);
        let mut plan = MapPlan::new("mirrored");
        plan.push(slot);

        assert_eq!(plan.resolve(0x000000).unwrap().target_offset, 0);
        // 0x080000 wraps back to 0.
        assert_eq!(plan.resolve(0x080000).unwrap().target_offset, 0);
        // 0x07FFFF is the last byte of the real image.
        assert_eq!(plan.resolve(0x07FFFF).unwrap().target_offset, 0x7FFFF);
        // 0x180001 & 0x7FFFF = 0x00001.
        assert_eq!(plan.resolve(0x180001).unwrap().target_offset, 0x00001);
    }

    #[test]
    fn resolve_outside_any_slot_is_none() {
        let mut plan = MapPlan::new("plain");
        plan.push(rom_slot());
        // Genesis RAM lives at 0xFF0000, outside any cartridge slot.
        assert!(plan.resolve(0xFF0000).is_none());
        // The 0xA1 I/O page is not part of a plain cartridge plan.
        assert!(plan.resolve(0xA13000).is_none());
    }

    #[test]
    fn resolve_first_match_wins() {
        // A chip MMIO slot placed ahead of the ROM window overrides it.
        let mut plan = MapPlan::new("svp-like");
        plan.push(MapSlot::new(
            0x300000..=0x31FFFF,
            MapTarget::ChipMmio {
                chip_id: 0,
                base: 0,
            },
        ));
        plan.push(rom_slot());

        let r = plan.resolve(0x300010).expect("chip window wins");
        let slot = &plan.slots[r.slot_index as usize];
        assert!(matches!(
            slot.target,
            MapTarget::ChipMmio {
                chip_id: 0,
                base: 0
            }
        ));
        assert_eq!(r.target_offset, 0x10);
    }

    #[test]
    fn resolve_chip_mmio_with_base() {
        // The mailbox window with a sentinel base, distinguishing it from
        // the DRAM window on the same chip.
        let mut plan = MapPlan::new("svp");
        plan.push(MapSlot::new(
            0xA15000..=0xA1500B,
            MapTarget::ChipMmio {
                chip_id: 0,
                base: 0x10000,
            },
        ));

        let r = plan.resolve(0xA15004).expect("mailbox mapped");
        assert_eq!(r.target_offset, 4);
        let slot = &plan.slots[r.slot_index as usize];
        assert!(matches!(
            slot.target,
            MapTarget::ChipMmio {
                chip_id: 0,
                base: 0x10000
            }
        ));
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
