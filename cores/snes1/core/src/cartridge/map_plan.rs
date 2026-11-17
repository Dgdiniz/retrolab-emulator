//! The SNES's map slot.
//!
//! The SNES bus does not know what a "LoROM" or "HiROM" cartridge is.
//! Every cartridge presents the bus with a [`MapPlan`], an ordered
//! list of [`MapSlot`]s. Each slot covers a contiguous rectangle of
//! `(bank, offset)` pairs and points at a [`MapTarget`] (ROM, SRAM,
//! a per-chip MMIO region, or open bus). The bus walks the plan once
//! at `install_cartridge()` time to populate its page table, then
//! never inspects the mapping again, the per-access hot path looks
//! at a single page-table entry, indexes a single slot, and dispatches
//! to the cartridge.
//!
//! This means:
//!
//! - LoROM, HiROM, ExHiROM, and any custom homebrew layout produce
//!   the same data structure; they are just different *preset
//!   generators*.
//! - Coprocessor chips (SA-1, SDD-1, DSP-1, SuperFX, Cx4, ...) appear
//!   to the bus only as `MapTarget::ChipMmio { chip_id, base }` slots
//!   carved into the cartridge plan, with the actual chip behaviour
//!   living inside the cartridge implementation.
//! - Bus accesses are bounded by the slot count, not the cartridge
//!   variant count. A linear walk through `slots` is fine: commercial
//!   cartridges have 2-6 slots; even adventurous homebrew rarely
//!   crosses 20.
//!
//! The plan, its resolution and [`Resolved`] are retro-core's,
//! monomorphised on this slot; what stays here is the slot's shape, its
//! access speed and its page arithmetic.

use std::ops::RangeInclusive;

use retro_core::cartridge::SlotShape;

/// Memory access speed (master cycles per byte).
///
/// The SNES bus runs at three speeds: 6 mclk (FastROM areas + WRAM
/// mirror), 8 mclk (most of the memory map), and 12 mclk (manual
/// joypad registers in `$00-$3F:$4000-$41FF`). Cartridges declare
/// their per-slot speed; the bus speed table reads it back at
/// `install_cartridge()` time.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BusSpeed {
    /// 6 master cycles per byte.
    Fast,
    /// 8 master cycles per byte.
    Slow,
    /// 12 master cycles per byte (joypad MMIO).
    XSlow,
}

impl BusSpeed {
    /// Master cycles consumed by a single read or write.
    pub fn master_cycles(self) -> u8 {
        match self {
            BusSpeed::Fast => 6,
            BusSpeed::Slow => 8,
            BusSpeed::XSlow => 12,
        }
    }
}

/// Where the bytes addressed by a slot actually live.
///
/// Defined in `retro-core` so every console shares one mapping
/// vocabulary (`Rom`/`Sram`/`ChipMmio`/`OpenBus`); the slot's *shape*
/// (the bank:offset rectangle) stays here, console-owned. Re-exported so
/// `map_plan::MapTarget` names it too.
pub use retro_core::cartridge::MapTarget;
pub use retro_core::cartridge::Resolved;

/// A SNES cartridge's map plan.
pub type MapPlan = retro_core::cartridge::MapPlan<MapSlot>;

/// One rectangular slot in a cartridge map plan.
///
/// `bank_range` x `offset_range` is the matching window. `mask`, if
/// present, is bitwise-AND'd against the slot-relative linear offset
/// before being added to the `MapTarget` base, this is how a
/// LoROM-style 32 KiB-per-bank mapping folds the upper bits of the
/// linear offset back inside the chip-rom window, and how HiROM-style
/// mirrors of a smaller ROM still resolve to addresses inside the
/// real ROM image.
///
/// `bank_stride` overrides the bytes-per-bank step used by
/// [`SlotShape::compute_target_offset`]. The default (None) means
/// "one bank = exactly `offset_range.len()` bytes", which is right
/// for slots that cover full or contiguous chip slices (LoROM ROM,
/// LoROM upper mirror, the primary HiROM `$C0-$FF:0000-FFFF`
/// window, all SRAM windows). HiROM-style *partial-window* mirrors
/// (`$00-$3F:8000-FFFF` mirroring the upper halves of
/// `$C0-$FF:0000-FFFF`) need `bank_stride = 0x10000` even though
/// the offset window is only `$8000` wide, so the per-bank step
/// crosses an entire 64 KiB chip bank and the mirror lands on the
/// *upper half* of the matching chip bank instead of the *next*
/// chip bank. The matching `MapTarget::Rom { base_offset: 0x8000 }`
/// then shifts the result up into the upper half. See the HiROM
/// preset in `super::presets::hirom` for the full math.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapSlot {
    pub bank_range: RangeInclusive<u8>,
    pub offset_range: RangeInclusive<u16>,
    pub mask: Option<u32>,
    pub bank_stride: Option<u32>,
    pub target: MapTarget,
    pub speed: BusSpeed,
}

impl MapSlot {
    /// Build a slot with no mask and the default bank stride
    /// (`offset_range.len()`).
    pub fn new(
        bank_range: RangeInclusive<u8>,
        offset_range: RangeInclusive<u16>,
        target: MapTarget,
        speed: BusSpeed,
    ) -> Self {
        Self {
            bank_range,
            offset_range,
            mask: None,
            bank_stride: None,
            target,
            speed,
        }
    }

    /// Attach a mask that wraps the slot-relative offset before it
    /// reaches the target. Useful for cartridges whose ROM is
    /// smaller than the address window they live in.
    pub fn with_mask(mut self, mask: u32) -> Self {
        self.mask = Some(mask);
        self
    }

    /// Override the bank stride used by
    /// [`SlotShape::compute_target_offset`]. See the type-level docs
    /// for when this matters (HiROM / ExHiROM upper-half mirrors).
    pub fn with_bank_stride(mut self, stride: u32) -> Self {
        self.bank_stride = Some(stride);
        self
    }

    /// Iterate over every 4 KiB page that this slot covers, expressed
    /// as page indices in the 0..0x1000 range used by
    /// [`crate::bus::SnesBus`]'s page table.
    ///
    /// A page is the 4 KiB chunk addressed by the top 12 bits of a
    /// 24-bit address (`addr >> 12`); equivalently
    /// `(bank << 4) | (offset >> 12)`. The walker uses this to map
    /// every covered page to `PageHandler::Cartridge { slot_index }`.
    ///
    /// Slots whose offset range does not start on a 4 KiB boundary
    /// (e.g. a hypothetical `$0500-$0FFF` slot) cover the entire
    /// page that contains either endpoint. The runtime
    /// [`SlotShape::contains`] check used by the bus dispatch handles
    /// the per-byte filtering inside such a page.
    pub fn covered_pages(&self) -> impl Iterator<Item = u16> + '_ {
        let bank_start = u16::from(*self.bank_range.start());
        let bank_end = u16::from(*self.bank_range.end());
        let page_start = *self.offset_range.start() >> 12;
        let page_end = *self.offset_range.end() >> 12;
        (bank_start..=bank_end)
            .flat_map(move |bank| (page_start..=page_end).map(move |p| (bank << 4) | p))
    }
}

impl SlotShape for MapSlot {
    type Target = MapTarget;

    /// `true` iff `addr24` falls inside this slot's rectangle.
    #[inline]
    fn contains(&self, addr24: u32) -> bool {
        let bank = ((addr24 >> 16) & 0xFF) as u8;
        let offset = (addr24 & 0xFFFF) as u16;
        self.bank_range.contains(&bank) && self.offset_range.contains(&offset)
    }

    /// The bank's distance from the first bank times the stride, plus the
    /// offset's distance from the first offset.
    #[inline]
    fn linear_offset(&self, addr24: u32) -> u32 {
        let bank = ((addr24 >> 16) & 0xFF) as u8;
        let offset = (addr24 & 0xFFFF) as u16;
        let bank_norm = u32::from(bank - self.bank_range.start());
        let offset_norm = u32::from(offset - self.offset_range.start());
        let stride = self.bank_stride.unwrap_or_else(|| {
            (u32::from(*self.offset_range.end()) - u32::from(*self.offset_range.start())) + 1
        });
        bank_norm * stride + offset_norm
    }

    #[inline]
    fn mask(&self) -> Option<u32> {
        self.mask
    }

    #[inline]
    fn target(&self) -> MapTarget {
        self.target
    }

    /// The rectangle's corners, packed as `(bank << 16) | offset`.
    fn bounds(&self) -> (u32, u32) {
        let bank_lo = u32::from(*self.bank_range.start());
        let bank_hi = u32::from(*self.bank_range.end());
        let off_lo = u32::from(*self.offset_range.start());
        let off_hi = u32::from(*self.offset_range.end());
        ((bank_lo << 16) | off_lo, (bank_hi << 16) | off_hi)
    }

    /// Total bytes the rectangle covers (`banks x offset-span`).
    fn size(&self) -> usize {
        let banks = usize::from(*self.bank_range.end()) - usize::from(*self.bank_range.start()) + 1;
        let span =
            usize::from(*self.offset_range.end()) - usize::from(*self.offset_range.start()) + 1;
        banks * span
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lorom_like_slot() -> MapSlot {
        MapSlot::new(
            0x80..=0xFF,
            0x8000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            BusSpeed::Fast,
        )
    }

    fn hirom_like_slot() -> MapSlot {
        MapSlot::new(
            0xC0..=0xFF,
            0x0000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            BusSpeed::Slow,
        )
    }

    #[test]
    fn resolve_lorom_like_slot() {
        let mut plan = MapPlan::new("lorom-like");
        plan.push(lorom_like_slot());

        let resolved = plan.resolve(0x80_8000).expect("matches LoROM-like slot");
        assert_eq!(resolved.slot_index, 0);
        assert_eq!(resolved.target_offset, 0);
        assert_eq!(
            plan.slots[resolved.slot_index as usize].speed,
            BusSpeed::Fast
        );

        // Bank $81, offset $9000 -> linear offset = 1*$8000 + $1000 = $9000.
        let resolved = plan.resolve(0x81_9000).expect("matches LoROM-like slot");
        assert_eq!(resolved.target_offset, 0x9000);
    }

    #[test]
    fn resolve_hirom_like_slot() {
        let mut plan = MapPlan::new("hirom-like");
        plan.push(hirom_like_slot());

        let resolved = plan.resolve(0xC0_1234).expect("matches HiROM-like slot");
        assert_eq!(resolved.slot_index, 0);
        assert_eq!(resolved.target_offset, 0x1234);
        assert_eq!(
            plan.slots[resolved.slot_index as usize].speed,
            BusSpeed::Slow
        );

        // Bank $C1, offset $0000 -> linear offset = 1 * $10000 = $10000.
        let resolved = plan.resolve(0xC1_0000).expect("matches HiROM-like slot");
        assert_eq!(resolved.target_offset, 0x10000);
    }

    #[test]
    fn resolve_mask_applied() {
        // LoROM-like slot whose ROM is only 32 KiB: the mask folds
        // banks above $80 back onto the first 32 KiB of the chip.
        let slot = MapSlot::new(
            0x80..=0xFF,
            0x8000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            BusSpeed::Fast,
        )
        .with_mask(0x7FFF);
        let mut plan = MapPlan::new("masked");
        plan.push(slot);

        // Bank $80, offset $8000 -> linear 0 -> masked 0.
        let resolved = plan.resolve(0x80_8000).unwrap();
        assert_eq!(resolved.target_offset, 0);
        // Bank $81, offset $8000 -> linear $8000 -> masked to 0
        // (upper bits stripped by the 0x7FFF mask).
        let resolved = plan.resolve(0x81_8000).unwrap();
        assert_eq!(resolved.target_offset, 0);
        // Bank $80, offset $FFFF -> linear $7FFF -> masked $7FFF.
        let resolved = plan.resolve(0x80_FFFF).unwrap();
        assert_eq!(resolved.target_offset, 0x7FFF);
    }

    #[test]
    fn resolve_outside_slot_returns_none() {
        let mut plan = MapPlan::new("lorom-like");
        plan.push(lorom_like_slot());

        // WRAM lives at $7E:0000, outside any cartridge slot.
        assert!(plan.resolve(0x7E_0000).is_none());
        // Bank $00 + offset $8000 is the LoROM low-half mirror; the
        // slot we registered only covers $80-$FF, so the linear walk
        // returns None.
        assert!(plan.resolve(0x00_8000).is_none());
    }

    #[test]
    fn resolve_chip_mmio_slot() {
        // DSP-1 LoROM register window: $30-$3F : $8000-$BFFF.
        let mut plan = MapPlan::new("dsp1-lorom");
        plan.push(MapSlot::new(
            0x30..=0x3F,
            0x8000..=0xBFFF,
            MapTarget::ChipMmio {
                chip_id: 1,
                base: 0,
            },
            BusSpeed::Fast,
        ));

        let resolved = plan.resolve(0x30_8000).expect("matches DSP-1 slot");
        assert_eq!(resolved.slot_index, 0);
        assert_eq!(resolved.target_offset, 0);
        assert_eq!(
            plan.slots[resolved.slot_index as usize].speed,
            BusSpeed::Fast
        );

        // Verify the slot's target is what we put in.
        let slot = &plan.slots[resolved.slot_index as usize];
        assert!(matches!(
            slot.target,
            MapTarget::ChipMmio {
                chip_id: 1,
                base: 0
            }
        ));
    }

    #[test]
    fn bus_speed_master_cycles() {
        assert_eq!(BusSpeed::Fast.master_cycles(), 6);
        assert_eq!(BusSpeed::Slow.master_cycles(), 8);
        assert_eq!(BusSpeed::XSlow.master_cycles(), 12);
    }

    #[test]
    fn covered_pages_lorom_like_slot_count() {
        let slot = lorom_like_slot();
        let pages: Vec<u16> = slot.covered_pages().collect();
        // 128 banks ($80-$FF) x 8 pages per bank ($8000-$FFFF =
        // pages $8..$F) = 1024 pages.
        assert_eq!(pages.len(), 128 * 8);
        // First page covered: bank $80, offset page $8 -> index $808.
        assert_eq!(pages.first().copied(), Some(0x808));
        // Last page covered: bank $FF, offset page $F -> index $FFF.
        assert_eq!(pages.last().copied(), Some(0xFFF));
        // Page just before the first cart page must NOT appear.
        assert!(!pages.contains(&0x807));
    }

    #[test]
    fn covered_pages_single_4kib_page() {
        // A slot that fits inside a single 4 KiB page (offset
        // $1000-$1FFF aligns exactly to page index 0x1 within bank
        // $00). Single bank x single page = one entry.
        let slot = MapSlot::new(
            0x00..=0x00,
            0x1000..=0x1FFF,
            MapTarget::Rom { base_offset: 0 },
            BusSpeed::Slow,
        );
        let pages: Vec<u16> = slot.covered_pages().collect();
        assert_eq!(pages, vec![0x001]);
    }

    #[test]
    fn bank_stride_overrides_window_for_hirom_mirror() {
        // HiROM mirror at $00-$3F:$8000-$FFFF mirrors the upper
        // halves of $C0-$FF:$0000-$FFFF. The slot's window is only
        // 32 KiB per bank but a one-bank step in source addresses
        // crosses a 64 KiB chip bank, so bank_stride must be
        // overridden to 0x10000. Combined with
        // MapTarget::Rom { base_offset: 0x8000 } at the call site,
        // this lands $00:8000 on chip byte $8000 (matching $C0:8000
        // in the primary slot).
        let slot = MapSlot::new(
            0x00..=0x3F,
            0x8000..=0xFFFF,
            MapTarget::Rom { base_offset: 0 },
            BusSpeed::Slow,
        )
        .with_bank_stride(0x10000);
        let mut plan = MapPlan::new("hirom-mirror");
        plan.push(slot);

        // $00:8000 -> bank_norm=0, offset_norm=0, stride=$10000
        //   linear = 0 * $10000 + 0 = 0
        let r = plan.resolve(0x00_8000).unwrap();
        assert_eq!(r.target_offset, 0);
        // $00:FFFF -> linear = 0 * $10000 + $7FFF = $7FFF
        let r = plan.resolve(0x00_FFFF).unwrap();
        assert_eq!(r.target_offset, 0x7FFF);
        // $01:8000 -> linear = 1 * $10000 + 0 = $10000
        // (Not $8000, which is what the default stride would have
        // produced, that would have collided with $00:FFFF+1 and
        // skipped the chip's upper-half of bank 1 entirely.)
        let r = plan.resolve(0x01_8000).unwrap();
        assert_eq!(r.target_offset, 0x10000);
        // $3F:FFFF -> linear = $3F * $10000 + $7FFF = $3F_7FFF
        let r = plan.resolve(0x3F_FFFF).unwrap();
        assert_eq!(r.target_offset, 0x3F_7FFF);
    }

    #[test]
    fn covered_pages_sram_8_pages() {
        // HiROM-style SRAM at $20-$20 : $6000-$7FFF spans 2 pages
        // (offset pages $6 and $7) in one bank.
        let single_bank = MapSlot::new(
            0x20..=0x20,
            0x6000..=0x7FFF,
            MapTarget::Sram { base_offset: 0 },
            BusSpeed::Slow,
        );
        let pages: Vec<u16> = single_bank.covered_pages().collect();
        assert_eq!(pages, vec![0x206, 0x207]);

        // LoROM-style SRAM at $70-$7D : $0000-$7FFF covers 14 banks
        // x 8 pages = 112 pages.
        let lorom_sram = MapSlot::new(
            0x70..=0x7D,
            0x0000..=0x7FFF,
            MapTarget::Sram { base_offset: 0 },
            BusSpeed::Slow,
        );
        let pages: Vec<u16> = lorom_sram.covered_pages().collect();
        assert_eq!(pages.len(), 14 * 8);
        assert_eq!(pages.first().copied(), Some(0x700));
        assert_eq!(pages.last().copied(), Some(0x7D7));
    }

    #[test]
    fn a_slot_reports_its_corners_and_its_area() {
        let slot = lorom_like_slot();
        assert_eq!(slot.bounds(), (0x80_8000, 0xFF_FFFF));
        assert_eq!(slot.size(), 128 * 0x8000);
        let sram = MapSlot::new(
            0x20..=0x20,
            0x6000..=0x7FFF,
            MapTarget::Sram { base_offset: 0 },
            BusSpeed::Slow,
        );
        assert_eq!(sram.bounds(), (0x20_6000, 0x20_7FFF));
        assert_eq!(sram.size(), 0x2000);
    }
}
