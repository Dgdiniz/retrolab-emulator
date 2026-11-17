//! The plan a cartridge's address space resolves through: an ordered list
//! of a console's map slots, the first slot that covers an address winning.
//!
//! What a slot is stays each console's own (a 68000 window is one linear
//! range, a 65C816 window a bank by offset rectangle that also carries its
//! access speed); [`SlotShape`] is what the plan, the cartridge and the
//! debugger need of one. The plan, its resolution and [`Resolved`] are
//! written once here, monomorphised on the console's slot, so a resolution
//! makes no dynamic call.

use std::fmt::Debug;

/// A console's map slot: the window it covers, where its bytes live, and
/// anything else the console's bus reads off a slot (the SNES's speed).
pub trait SlotShape: Clone + Debug + PartialEq + Eq {
    /// Where the slot's bytes live: `MapTarget` on a console's main bus, a
    /// console's own type on a second bus.
    type Target: Copy + Debug + PartialEq + Eq;

    /// Whether `addr` falls inside the slot's window.
    fn contains(&self, addr: u32) -> bool;

    /// Offset of `addr` in the window before the mask (`contains` holds).
    fn linear_offset(&self, addr: u32) -> u32;

    /// The mask the offset is folded by before it reaches the target, if
    /// any: how a window wider than the memory behind it mirrors it.
    fn mask(&self) -> Option<u32>;

    /// Where the slot's bytes live.
    fn target(&self) -> Self::Target;

    /// First and last address of the window, as a debugger prints them.
    fn bounds(&self) -> (u32, u32);

    /// Bytes the window covers.
    fn size(&self) -> usize;

    /// Offset of `addr` in the window with the mask applied: the offset the
    /// target's base is added to. `contains` must hold.
    #[inline(always)]
    fn compute_target_offset(&self, addr: u32) -> u32 {
        debug_assert!(self.contains(addr));
        let linear = self.linear_offset(addr);
        match self.mask() {
            Some(mask) => linear & mask,
            None => linear,
        }
    }
}

/// An ordered list of slots describing a cartridge's view of an address
/// space. An address resolves to the first slot that covers it, so a
/// specific window is pushed ahead of a broader one it is carved out of.
/// The name is for diagnostics and persistence (`"plain"`, `"lorom"`,
/// `"custom"` for a manifest's own plan).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapPlan<S> {
    pub name: String,
    pub slots: Vec<S>,
}

impl<S> Default for MapPlan<S> {
    fn default() -> Self {
        Self {
            name: String::new(),
            slots: Vec::new(),
        }
    }
}

/// An address resolved through a [`MapPlan`]: the index of the slot that
/// covers it, and the slot-relative offset with the mask applied, which a
/// caller adds to the target's base.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub slot_index: u16,
    pub target_offset: u32,
}

impl<S: SlotShape> MapPlan<S> {
    /// An empty plan with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            slots: Vec::new(),
        }
    }

    /// Append a slot. The first slot that covers an address wins.
    pub fn push(&mut self, slot: S) {
        self.slots.push(slot);
    }

    /// The first slot, in push order, that covers `addr`, or `None` when no
    /// slot does (a read of nothing).
    #[inline]
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

    /// A slot over one linear range, as a test console has.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Window {
        start: u32,
        end: u32,
        mask: Option<u32>,
        target: u8,
    }

    impl SlotShape for Window {
        type Target = u8;
        fn contains(&self, addr: u32) -> bool {
            (self.start..=self.end).contains(&addr)
        }
        fn linear_offset(&self, addr: u32) -> u32 {
            addr - self.start
        }
        fn mask(&self) -> Option<u32> {
            self.mask
        }
        fn target(&self) -> u8 {
            self.target
        }
        fn bounds(&self) -> (u32, u32) {
            (self.start, self.end)
        }
        fn size(&self) -> usize {
            (self.end - self.start + 1) as usize
        }
    }

    fn window(start: u32, end: u32, mask: Option<u32>, target: u8) -> Window {
        Window {
            start,
            end,
            mask,
            target,
        }
    }

    #[test]
    fn the_first_slot_that_covers_an_address_wins() {
        let mut plan = MapPlan::new("test");
        plan.push(window(0x100, 0x1FF, None, 1));
        plan.push(window(0x000, 0xFFF, None, 2));
        assert_eq!(
            plan.resolve(0x180),
            Some(Resolved {
                slot_index: 0,
                target_offset: 0x80
            })
        );
        assert_eq!(
            plan.resolve(0x200),
            Some(Resolved {
                slot_index: 1,
                target_offset: 0x200
            })
        );
    }

    #[test]
    fn the_mask_folds_the_offset() {
        let mut plan = MapPlan::new("mirror");
        plan.push(window(0x1000, 0x1FFF, Some(0xFF), 1));
        let resolved = plan.resolve(0x1345).unwrap();
        assert_eq!(resolved.target_offset, 0x45);
        assert_eq!(plan.slots[0].compute_target_offset(0x1345), 0x45);
    }

    #[test]
    fn an_address_no_slot_covers_resolves_to_nothing() {
        let mut plan = MapPlan::new("gap");
        plan.push(window(0x10, 0x1F, None, 1));
        assert_eq!(plan.resolve(0x0F), None);
        assert_eq!(plan.resolve(0x20), None);
        assert_eq!(MapPlan::<Window>::default().resolve(0), None);
        assert!(MapPlan::<Window>::default().name.is_empty());
    }
}
