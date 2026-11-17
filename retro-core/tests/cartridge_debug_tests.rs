//! The cartridge debugger's rules, which every console's debugger keeps:
//! the regions a plan describes, the memory map, the ROM and battery RAM
//! regions capped at what the cartridge holds, the mapper's state, the
//! chips' states, and the breakpoints landing in the emulator's own list.
//! Over two test slots: a linear window and a bank by offset rectangle.

use retro_core::cartridge::{
    describe_map_plan, CartridgeDebugger, ExpansionChipState, ExpansionChipType, MapPlan,
    MapPlanDebugger, MapTarget, MapperId, MapperRegister, MemoryType, SRAMType, SlotShape,
    WatchType,
};
use retro_core::debug::BreakpointList;

/// A slot over one linear range.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Linear {
    start: u32,
    end: u32,
    mask: Option<u32>,
    target: MapTarget,
}

impl SlotShape for Linear {
    type Target = MapTarget;
    fn contains(&self, addr: u32) -> bool {
        (self.start..=self.end).contains(&addr)
    }
    fn linear_offset(&self, addr: u32) -> u32 {
        addr - self.start
    }
    fn mask(&self) -> Option<u32> {
        self.mask
    }
    fn target(&self) -> MapTarget {
        self.target
    }
    fn bounds(&self) -> (u32, u32) {
        (self.start, self.end)
    }
    fn size(&self) -> usize {
        (self.end - self.start + 1) as usize
    }
}

fn linear(start: u32, end: u32, mask: Option<u32>, target: MapTarget) -> Linear {
    Linear {
        start,
        end,
        mask,
        target,
    }
}

/// A bank by offset rectangle, as a 65C816's window is.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Rectangle {
    banks: (u8, u8),
    offsets: (u16, u16),
    target: MapTarget,
}

impl SlotShape for Rectangle {
    type Target = MapTarget;
    fn contains(&self, addr: u32) -> bool {
        let bank = (addr >> 16) as u8;
        let offset = addr as u16;
        (self.banks.0..=self.banks.1).contains(&bank)
            && (self.offsets.0..=self.offsets.1).contains(&offset)
    }
    fn linear_offset(&self, addr: u32) -> u32 {
        let span = u32::from(self.offsets.1 - self.offsets.0) + 1;
        u32::from((addr >> 16) as u8 - self.banks.0) * span
            + u32::from(addr as u16 - self.offsets.0)
    }
    fn mask(&self) -> Option<u32> {
        None
    }
    fn target(&self) -> MapTarget {
        self.target
    }
    fn bounds(&self) -> (u32, u32) {
        (
            (u32::from(self.banks.0) << 16) | u32::from(self.offsets.0),
            (u32::from(self.banks.1) << 16) | u32::from(self.offsets.1),
        )
    }
    fn size(&self) -> usize {
        (usize::from(self.banks.1 - self.banks.0) + 1)
            * (usize::from(self.offsets.1 - self.offsets.0) + 1)
    }
}

/// A plan of a chip's mailbox, a chip window at a base, a RAM window, an
/// open-bus window and a ROM window mirrored by its mask.
fn plan() -> MapPlan<Linear> {
    let mut plan = MapPlan::new("test");
    plan.push(linear(
        0xA15000,
        0xA1500B,
        None,
        MapTarget::ChipMmio {
            chip_id: 0,
            base: 0x100000,
        },
    ));
    plan.push(linear(
        0x300000,
        0x31FFFF,
        None,
        MapTarget::ChipMmio {
            chip_id: 0,
            base: 0,
        },
    ));
    plan.push(linear(
        0x200000,
        0x207FFF,
        None,
        MapTarget::Sram { base_offset: 0 },
    ));
    plan.push(linear(0x400000, 0x40FFFF, None, MapTarget::OpenBus));
    plan.push(linear(
        0x000000,
        0x1FFFFF,
        Some(0x7FFFF),
        MapTarget::Rom { base_offset: 0 },
    ));
    plan
}

fn chip_state() -> ExpansionChipState {
    ExpansionChipState {
        chip_type: ExpansionChipType::SVP,
        registers: Vec::new(),
        internal_ram: Some(vec![0u8; 4]),
        program_counter: Some(0x0400),
        cycle_count: 1234,
    }
}

#[test]
fn a_plan_describes_its_slots_in_order_with_their_bases_and_masks() {
    let regions = describe_map_plan(&plan());
    let labels: Vec<_> = regions.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(labels, ["Chip 0", "Chip 0", "SRAM", "OpenBus", "ROM"]);
    assert_eq!((regions[0].start, regions[0].end), (0xA15000, 0xA1500B));
    assert_eq!(regions[0].detail, "base=0x100000");
    assert_eq!(regions[1].detail, "");
    assert_eq!(regions[4].detail, "mask=0x07FFFF");
    assert_eq!(
        serde_json::to_string(&regions[4]).unwrap(),
        r#"{"start":0,"end":2097151,"label":"ROM","detail":"mask=0x07FFFF"}"#
    );

    let mut both = MapPlan::new("both");
    both.push(linear(
        0,
        0xFF,
        Some(0xF),
        MapTarget::Rom { base_offset: 0x10 },
    ));
    assert_eq!(
        describe_map_plan(&both)[0].detail,
        "base=0x10, mask=0x00000F"
    );
    assert!(describe_map_plan(&MapPlan::<Linear>::new("custom")).is_empty());
}

#[test]
fn the_memory_map_lists_every_window_but_open_bus() {
    let mut breakpoints = BreakpointList::new();
    let debugger =
        MapPlanDebugger::new(plan(), 0x80000, 0x8000, None, Vec::new(), &mut breakpoints);
    let map = debugger.get_memory_map();
    assert_eq!(map.regions.len(), 4);
    assert!(map
        .regions
        .iter()
        .any(|r| r.memory_type == MemoryType::ROM && r.cpu_address_start == 0));
    assert!(map
        .regions
        .iter()
        .any(|r| r.memory_type == MemoryType::MappedIO && r.physical_offset == 0x100000));
    assert!(map
        .regions
        .iter()
        .all(|r| r.description.ends_with("(test)")));
}

#[test]
fn the_rom_and_ram_regions_claim_no_more_than_the_cartridge_holds() {
    let mut breakpoints = BreakpointList::new();
    let debugger =
        MapPlanDebugger::new(plan(), 0x80000, 0x2000, None, Vec::new(), &mut breakpoints);
    let rom = debugger.get_rom_regions();
    assert_eq!(rom.len(), 1);
    assert_eq!(rom[0].size, 0x80000, "the window mirrors a smaller image");
    let ram = debugger.get_sram_regions();
    assert_eq!(ram.len(), 1);
    assert_eq!((ram[0].start, ram[0].size), (0x200000, 0x2000));
    assert_eq!(ram[0].sram_type, SRAMType::Battery);

    let mut breakpoints = BreakpointList::new();
    let no_sizes = MapPlanDebugger::new(plan(), 0, 0, None, Vec::new(), &mut breakpoints);
    assert_eq!(no_sizes.get_rom_regions()[0].size, 0x200000);
    assert_eq!(no_sizes.get_sram_regions()[0].size, 0x8000);
    assert_eq!(no_sizes.get_sram_regions()[0].sram_type, SRAMType::None);
}

#[test]
fn the_mapper_state_needs_a_mapper_id_and_carries_its_registers() {
    let mut breakpoints = BreakpointList::new();
    let none = MapPlanDebugger::new(plan(), 0, 0, None, Vec::new(), &mut breakpoints);
    assert!(none.get_mapper_state().is_none());

    let mut breakpoints = BreakpointList::new();
    let register = MapperRegister {
        address: 0xA130F1,
        name: "bank".into(),
        value: 3,
        description: String::new(),
    };
    let named = MapPlanDebugger::new(
        plan(),
        0,
        0,
        Some(MapperId::Custom("test".into())),
        Vec::new(),
        &mut breakpoints,
    )
    .with_mapper_registers(vec![register]);
    let state = named.get_mapper_state().unwrap();
    assert_eq!(state.mapper_id, MapperId::Custom("test".into()));
    assert_eq!(
        state.bank_config.banks.len(),
        4,
        "one bank a window, open bus left out"
    );
    assert_eq!(state.registers.len(), 1);
}

#[test]
fn the_chips_states_are_the_primary_first() {
    let mut breakpoints = BreakpointList::new();
    let mut second = chip_state();
    second.program_counter = Some(0x0800);
    let debugger = MapPlanDebugger::new(
        plan(),
        0,
        0,
        None,
        vec![(0, chip_state()), (3, second)],
        &mut breakpoints,
    );
    assert_eq!(
        debugger.get_chip_state().unwrap().program_counter,
        Some(0x0400)
    );
    assert_eq!(debugger.get_chip_states().len(), 2);

    let mut breakpoints = BreakpointList::new();
    let empty = MapPlanDebugger::new(
        MapPlan::<Linear>::default(),
        0,
        0,
        None,
        Vec::new(),
        &mut breakpoints,
    );
    assert!(empty.get_chip_state().is_none());
    assert!(empty.get_memory_map().regions.is_empty());
}

#[test]
fn breakpoints_land_in_the_emulators_list() {
    let mut breakpoints = BreakpointList::new();
    {
        let mut debugger = MapPlanDebugger::new(plan(), 0, 0, None, Vec::new(), &mut breakpoints);
        let rom = debugger.add_rom_breakpoint(0x400);
        let ram = debugger.add_sram_watchpoint(0x200000, WatchType::Write);
        debugger.add_sram_watchpoint(0x200001, WatchType::Read);
        debugger.add_sram_watchpoint(0x200002, WatchType::ReadWrite);
        assert_ne!(rom, ram);
        debugger.remove_breakpoint(rom);
    }
    assert_eq!(breakpoints.list().len(), 3);
}

#[test]
fn a_rectangle_slot_reports_its_corners_and_its_area() {
    let slot = Rectangle {
        banks: (0x80, 0x9F),
        offsets: (0x8000, 0xFFFF),
        target: MapTarget::Rom { base_offset: 0 },
    };
    assert_eq!(slot.bounds(), (0x80_8000, 0x9F_FFFF));
    assert_eq!(slot.size(), 32 * 0x8000);
    assert_eq!(slot.compute_target_offset(0x81_8001), 0x8001);
    let mut plan = MapPlan::new("rect");
    plan.push(slot);
    let regions = describe_map_plan(&plan);
    assert_eq!((regions[0].start, regions[0].end), (0x80_8000, 0x9F_FFFF));
    let mut breakpoints = BreakpointList::new();
    let debugger = MapPlanDebugger::new(plan, 0x40000, 0, None, Vec::new(), &mut breakpoints);
    assert_eq!(debugger.get_memory_map().regions[0].size, 32 * 0x8000);
    assert_eq!(debugger.get_rom_regions()[0].size, 0x40000);
}
