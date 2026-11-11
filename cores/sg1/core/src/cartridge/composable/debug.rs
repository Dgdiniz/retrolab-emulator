//! Cartridge debugger over a Genesis cartridge's [`MapPlan`].
//!
//! Two layers live here. [`describe_map_plan`] turns a [`MapPlan`] into a
//! flat, human-readable region list, the data behind a "cartridge memory
//! map" view; keeping it pure (no bus, no chips) lets the CLI memory-map
//! dump and the WASM debug bindings share one tested formatter.
//! [`GenesisCartridgeDebugger`] builds on top of it: a borrowing
//! [`CartridgeDebugger`] view mirroring the SNES `SnesCartridgeDebugger`,
//! adapted to the flat 24-bit 68000 address space (a slot is a single
//! linear range, not a `bank x offset` rectangle).

use serde::Serialize;

use retro_core::cartridge::{
    AccessType, BankConfiguration, BankInfo, BreakpointId, CartridgeDebugger, ExpansionChipState,
    MapperId, MapperState, MemoryMap, MemoryMapRegion, MemoryType, RomRegionInfo, SRAMType,
    SramRegionInfo, WatchType,
};
use retro_core::debug::BreakpointList;
use retro_core::types::{Breakpoint, BreakpointType};

use super::map_plan::{MapPlan, MapSlot, MapTarget};

/// One resolved region of a cartridge's address space, ready to print.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MapRegionSummary {
    /// Inclusive 24-bit start address of the slot.
    pub start: u32,
    /// Inclusive 24-bit end address of the slot.
    pub end: u32,
    /// Short target label (`"ROM"`, `"SRAM"`, `"Chip 0"`, `"OpenBus"`).
    pub label: String,
    /// Extra detail (`base`/`mask`) for the region.
    pub detail: String,
}

/// Summarize every slot in a plan, in resolution (first-wins) order.
pub fn describe_map_plan(plan: &MapPlan) -> Vec<MapRegionSummary> {
    plan.slots
        .iter()
        .map(|slot| {
            let (label, mut detail) = match slot.target {
                MapTarget::Rom { base_offset } => ("ROM".to_string(), base_detail(base_offset)),
                MapTarget::Sram { base_offset } => ("SRAM".to_string(), base_detail(base_offset)),
                MapTarget::ChipMmio { chip_id, base } => {
                    (format!("Chip {chip_id}"), base_detail(base))
                }
                MapTarget::OpenBus => ("OpenBus".to_string(), String::new()),
            };
            if let Some(mask) = slot.mask {
                if !detail.is_empty() {
                    detail.push_str(", ");
                }
                detail.push_str(&format!("mask=0x{mask:06X}"));
            }
            MapRegionSummary {
                start: *slot.addr_range.start(),
                end: *slot.addr_range.end(),
                label,
                detail,
            }
        })
        .collect()
}

fn base_detail(base: u32) -> String {
    if base == 0 {
        String::new()
    } else {
        format!("base=0x{base:X}")
    }
}

/// Inclusive linear 24-bit CPU range a slot covers.
fn slot_cpu_range(slot: &MapSlot) -> (u32, u32) {
    (*slot.addr_range.start(), *slot.addr_range.end())
}

/// Total bytes a slot's address window covers.
fn slot_size(slot: &MapSlot) -> usize {
    (*slot.addr_range.end() as usize) - (*slot.addr_range.start() as usize) + 1
}

/// Decode a slot target into `(memory_type, access, physical_offset,
/// label)`. `None` for open-bus windows, which the viewers omit, mirroring
/// the SNES `classify_target`.
fn classify_target(target: MapTarget) -> Option<(MemoryType, AccessType, usize, &'static str)> {
    match target {
        MapTarget::Rom { base_offset } => Some((
            MemoryType::ROM,
            AccessType::ReadOnly,
            base_offset as usize,
            "ROM",
        )),
        MapTarget::Sram { base_offset } => Some((
            MemoryType::SRAM,
            AccessType::ReadWrite,
            base_offset as usize,
            "SRAM",
        )),
        MapTarget::ChipMmio { base, .. } => Some((
            MemoryType::MappedIO,
            AccessType::MappedIO,
            base as usize,
            "Chip MMIO",
        )),
        MapTarget::OpenBus => None,
    }
}

/// A [`CartridgeDebugger`] bound to the emulator's live breakpoint list.
///
/// Construct via `GenesisEmulator::cartridge_debugger()`. Like the SNES
/// peer it is a thin, borrowing view: it owns a cheap snapshot of the
/// installed [`MapPlan`] plus ROM/SRAM sizes and the coprocessor chip
/// snapshots, and borrows the emulator's single [`BreakpointList`] so
/// ROM/SRAM breakpoints land in the *same* list the CPU step path checks,
/// no parallel breakpoint state.
pub struct GenesisCartridgeDebugger<'a> {
    /// Snapshot of the installed map plan (cloned, a handful of slots).
    plan: MapPlan,
    /// ROM image length in bytes.
    rom_size: u32,
    /// Battery SRAM length in bytes (0 when the cart has none).
    sram_size: u32,
    /// Coprocessor chip snapshots captured at construction, keyed by
    /// `chip_id` (SVP, EEPROM, bank mapper, ...). Empty for a plain cart.
    chip_states: Vec<(u8, ExpansionChipState)>,
    /// The emulator's breakpoint list; ROM/SRAM breakpoints append here.
    breakpoints: &'a mut BreakpointList,
}

impl<'a> GenesisCartridgeDebugger<'a> {
    /// Bind a cartridge debugger to a map plan + breakpoint list, with the
    /// coprocessor chip snapshots keyed by `chip_id` (empty for a plain
    /// cartridge).
    pub fn new(
        plan: MapPlan,
        rom_size: u32,
        sram_size: u32,
        chip_states: Vec<(u8, ExpansionChipState)>,
        breakpoints: &'a mut BreakpointList,
    ) -> Self {
        Self {
            plan,
            rom_size,
            sram_size,
            chip_states,
            breakpoints,
        }
    }

    /// SRAM kind reported in `get_sram_regions`, battery-backed when the
    /// cartridge carries any SRAM, else none.
    fn sram_kind(&self) -> SRAMType {
        if self.sram_size > 0 {
            SRAMType::Battery
        } else {
            SRAMType::None
        }
    }
}

impl CartridgeDebugger for GenesisCartridgeDebugger<'_> {
    fn get_memory_map(&self) -> MemoryMap {
        let regions = self
            .plan
            .slots
            .iter()
            .filter_map(|slot| {
                let (memory_type, access, physical_offset, label) = classify_target(slot.target)?;
                let (start, end) = slot_cpu_range(slot);
                Some(MemoryMapRegion {
                    cpu_address_start: start,
                    cpu_address_end: end,
                    physical_offset,
                    size: slot_size(slot),
                    memory_type,
                    access,
                    description: format!("{label} ({})", self.plan.name),
                })
            })
            .collect();
        MemoryMap { regions }
    }

    fn get_rom_regions(&self) -> Vec<RomRegionInfo> {
        self.plan
            .slots
            .iter()
            .filter(|slot| matches!(slot.target, MapTarget::Rom { .. }))
            .map(|slot| {
                let (start, end) = slot_cpu_range(slot);
                RomRegionInfo {
                    start,
                    end,
                    // A slot mirrors a smaller ROM across a wider window;
                    // cap at the physical image size so the region never
                    // claims more bytes than exist.
                    size: if self.rom_size > 0 {
                        slot_size(slot).min(self.rom_size as usize)
                    } else {
                        slot_size(slot)
                    },
                    description: format!("ROM ({})", self.plan.name),
                }
            })
            .collect()
    }

    fn get_sram_regions(&self) -> Vec<SramRegionInfo> {
        let sram_type = self.sram_kind();
        self.plan
            .slots
            .iter()
            .filter(|slot| matches!(slot.target, MapTarget::Sram { .. }))
            .map(|slot| {
                let (start, end) = slot_cpu_range(slot);
                SramRegionInfo {
                    start,
                    end,
                    size: if self.sram_size > 0 {
                        self.sram_size as usize
                    } else {
                        slot_size(slot)
                    },
                    sram_type,
                    description: format!("SRAM ({})", self.plan.name),
                }
            })
            .collect()
    }

    fn get_mapper_state(&self) -> Option<MapperState> {
        // retro-core's `MapperId` enumerates SNES mappers; every Genesis
        // shape (plain / sega / ssf2 / codemasters / eeprom / jcart / svp)
        // is a named custom mapper keyed by the plan name.
        let mapper_id = MapperId::Custom(self.plan.name.clone());
        let banks = self
            .plan
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| {
                let (memory_type, _, physical_offset, _) = classify_target(slot.target)?;
                let (start, end) = slot_cpu_range(slot);
                Some(BankInfo {
                    cpu_address_start: start,
                    cpu_address_end: end,
                    physical_offset,
                    size: slot_size(slot),
                    memory_type,
                    bank_number: i as u32,
                })
            })
            .collect();
        Some(MapperState {
            mapper_id,
            // Software-visible bank registers surface through the bank-mapper
            // coprocessor's `get_chip_states`, not here (parity with snes1).
            registers: Vec::new(),
            bank_config: BankConfiguration { banks },
        })
    }

    fn get_chip_state(&self) -> Option<ExpansionChipState> {
        // The primary (lowest-id) chip, captured from the live cartridge at
        // construction (SVP surfaces its SSP1601 registers); `None` for a
        // plain cart.
        self.chip_states.first().map(|(_, s)| s.clone())
    }

    fn get_chip_states(&self) -> Vec<(u8, ExpansionChipState)> {
        self.chip_states.clone()
    }

    fn add_rom_breakpoint(&mut self, addr: u32) -> BreakpointId {
        self.breakpoints.add(Breakpoint {
            address: addr,
            bp_type: BreakpointType::execute(),
            cpu_id: Some(0),
            condition: None,
        })
    }

    fn add_sram_watchpoint(&mut self, addr: u32, watch_type: WatchType) -> BreakpointId {
        let bp_type = match watch_type {
            WatchType::Read => BreakpointType::read(),
            WatchType::Write => BreakpointType::write(),
            WatchType::ReadWrite => BreakpointType::read_write(),
        };
        self.breakpoints.add(Breakpoint {
            address: addr,
            bp_type,
            cpu_id: Some(0),
            condition: None,
        })
    }

    fn remove_breakpoint(&mut self, id: BreakpointId) {
        self.breakpoints.remove(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cartridge::composable::map_plan::MapPlan;
    use crate::cartridge::composable::presets::{eeprom_plan, svp_plan, EEPROM_CONTROL_ADDR};

    #[test]
    fn describes_svp_plan_regions() {
        let plan = svp_plan(0, 0x200000);
        let regions = describe_map_plan(&plan);
        assert_eq!(regions.len(), 5);
        // First-wins order: mailbox, linear DRAM, cell-arrange 1, cell-arrange
        // 2, then the mirrored ROM.
        assert_eq!(regions[0].label, "Chip 0");
        assert_eq!(regions[0].start, 0xA15000);
        assert_eq!(regions[0].end, 0xA1500B);
        assert_eq!(regions[1].label, "Chip 0");
        assert_eq!(regions[1].start, 0x300000);
        assert_eq!(regions[1].end, 0x31FFFF);
        assert_eq!(regions[2].label, "Chip 0");
        assert_eq!(regions[2].start, 0x390000);
        assert_eq!(regions[2].end, 0x39FFFF);
        assert_eq!(regions[3].label, "Chip 0");
        assert_eq!(regions[3].start, 0x3A0000);
        assert_eq!(regions[3].end, 0x3AFFFF);
        assert_eq!(regions[4].label, "ROM");
        assert!(regions[4].detail.contains("mask="));
    }

    #[test]
    fn describes_eeprom_control_byte() {
        let plan = eeprom_plan(0, 0x80000, EEPROM_CONTROL_ADDR);
        let regions = describe_map_plan(&plan);
        assert_eq!(regions[0].label, "Chip 0");
        assert_eq!(regions[0].start, EEPROM_CONTROL_ADDR);
        assert_eq!(regions[0].end, EEPROM_CONTROL_ADDR);
    }

    #[test]
    fn empty_plan_has_no_regions() {
        let plan = MapPlan::new("custom");
        assert!(describe_map_plan(&plan).is_empty());
    }

    // -- GenesisCartridgeDebugger -----------------------------------------

    use crate::cartridge::composable::presets::plain_plan;
    use retro_core::cartridge::{
        CartridgeDebugger, ExpansionChipState, ExpansionChipType, MapperId, MemoryType, SRAMType,
        WatchType,
    };
    use retro_core::debug::BreakpointList;

    fn svp_chip_state() -> ExpansionChipState {
        ExpansionChipState {
            chip_type: ExpansionChipType::SVP,
            registers: Vec::new(),
            internal_ram: Some(vec![0u8; 4]),
            program_counter: Some(0x0400),
            cycle_count: 1234,
        }
    }

    #[test]
    fn debugger_memory_map_lists_rom_and_chip_windows() {
        let plan = svp_plan(0, 0x200000);
        let mut bps = BreakpointList::new();
        let dbg = GenesisCartridgeDebugger::new(plan, 0x200000, 0, Vec::new(), &mut bps);
        let map = dbg.get_memory_map();
        // The four SVP chip windows + the ROM window; open-bus (none here)
        // would be omitted.
        assert_eq!(map.regions.len(), 5);
        assert!(map
            .regions
            .iter()
            .any(|r| r.memory_type == MemoryType::ROM && r.cpu_address_start == 0x000000));
        assert!(map
            .regions
            .iter()
            .any(|r| r.memory_type == MemoryType::MappedIO && r.cpu_address_start == 0xA15000));
    }

    #[test]
    fn debugger_rom_and_sram_regions_track_sizes() {
        // Plain cart with a 32 KiB SRAM window at 0x200000.
        let plan = plain_plan(0x80000, Some((0x200000, 0x8000)));
        let mut bps = BreakpointList::new();
        let dbg = GenesisCartridgeDebugger::new(plan, 0x80000, 0x8000, Vec::new(), &mut bps);

        let rom = dbg.get_rom_regions();
        assert_eq!(rom.len(), 1);
        assert_eq!(rom[0].start, 0x000000);
        // The ROM slot mirrors across the 4 MiB window; capped at the image.
        assert_eq!(rom[0].size, 0x80000);

        let sram = dbg.get_sram_regions();
        assert_eq!(sram.len(), 1);
        assert_eq!(sram[0].start, 0x200000);
        assert_eq!(sram[0].size, 0x8000);
        assert_eq!(sram[0].sram_type, SRAMType::Battery);
    }

    #[test]
    fn debugger_mapper_state_is_named_after_the_plan() {
        let plan = svp_plan(0, 0x200000);
        let mut bps = BreakpointList::new();
        let dbg = GenesisCartridgeDebugger::new(plan, 0x200000, 0, Vec::new(), &mut bps);
        let mapper = dbg.get_mapper_state().expect("mapper state");
        assert_eq!(mapper.mapper_id, MapperId::Custom("svp".to_string()));
        // One bank entry per non-open-bus slot.
        assert_eq!(mapper.bank_config.banks.len(), 5);
    }

    #[test]
    fn debugger_surfaces_coprocessor_chip_states() {
        let plan = svp_plan(0, 0x200000);
        let mut bps = BreakpointList::new();
        let dbg =
            GenesisCartridgeDebugger::new(plan, 0x200000, 0, vec![(0, svp_chip_state())], &mut bps);
        let primary = dbg.get_chip_state().expect("primary chip");
        assert_eq!(primary.program_counter, Some(0x0400));
        assert_eq!(dbg.get_chip_states().len(), 1);
    }

    #[test]
    fn debugger_breakpoints_land_in_the_shared_list() {
        let plan = plain_plan(0x80000, None);
        let mut bps = BreakpointList::new();
        {
            let mut dbg = GenesisCartridgeDebugger::new(plan, 0x80000, 0, Vec::new(), &mut bps);
            let rom_bp = dbg.add_rom_breakpoint(0x000400);
            let sram_bp = dbg.add_sram_watchpoint(0x200000, WatchType::Write);
            assert_ne!(rom_bp, sram_bp);
            dbg.remove_breakpoint(rom_bp);
        }
        // One breakpoint (the SRAM watchpoint) survives in the shared list.
        assert_eq!(bps.list().len(), 1);
    }

    #[test]
    fn debugger_empty_when_no_cartridge() {
        let plan = MapPlan::default();
        let mut bps = BreakpointList::new();
        let dbg = GenesisCartridgeDebugger::new(plan, 0, 0, Vec::new(), &mut bps);
        assert!(dbg.get_memory_map().regions.is_empty());
        assert!(dbg.get_rom_regions().is_empty());
        assert!(dbg.get_chip_state().is_none());
    }
}
