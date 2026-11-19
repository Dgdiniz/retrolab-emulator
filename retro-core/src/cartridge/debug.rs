//! The cartridge debugger, over a cartridge's [`MapPlan`].
//!
//! [`describe_map_plan`] turns a plan into a flat list of regions, the data
//! behind a "cartridge memory map" view. [`MapPlanDebugger`] is the
//! [`CartridgeDebugger`] every console hands a host: a borrowing view that
//! holds a copy of the installed plan, the ROM and battery RAM sizes, the
//! mapper's id and registers and the chips' states, and borrows the
//! emulator's one [`BreakpointList`], so a ROM or RAM breakpoint lands in
//! the list the CPU's step checks.

use serde::Serialize;

use super::map_plan::{MapPlan, SlotShape};
use super::{
    AccessType, BankConfiguration, BankInfo, BreakpointId, CartridgeDebugger, ExpansionChipState,
    MapTarget, MapperId, MapperRegister, MapperState, MemoryMap, MemoryMapRegion, MemoryType,
    RomRegionInfo, SRAMType, SramRegionInfo, WatchType,
};
use crate::debug::BreakpointList;
use crate::types::{Breakpoint, BreakpointType};

/// One region of a cartridge's address space, ready to print.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MapRegionSummary {
    /// First address of the slot.
    pub start: u32,
    /// Last address of the slot.
    pub end: u32,
    /// What answers there (`"ROM"`, `"SRAM"`, `"Chip 0"`, `"OpenBus"`).
    pub label: String,
    /// The target's base and the slot's mask, when they are not plain.
    pub detail: String,
}

/// Every slot of `plan`, in the order addresses resolve through them.
pub fn describe_map_plan<S: SlotShape<Target = MapTarget>>(
    plan: &MapPlan<S>,
) -> Vec<MapRegionSummary> {
    plan.slots
        .iter()
        .map(|slot| {
            let (label, mut detail) = match slot.target() {
                MapTarget::Rom { base_offset } => ("ROM".to_string(), base_detail(base_offset)),
                MapTarget::Sram { base_offset } => ("SRAM".to_string(), base_detail(base_offset)),
                MapTarget::ChipMmio { chip_id, base } => {
                    (format!("Chip {chip_id}"), base_detail(base))
                }
                MapTarget::OpenBus => ("OpenBus".to_string(), String::new()),
            };
            if let Some(mask) = slot.mask() {
                if !detail.is_empty() {
                    detail.push_str(", ");
                }
                detail.push_str(&format!("mask=0x{mask:06X}"));
            }
            let (start, end) = slot.bounds();
            MapRegionSummary {
                start,
                end,
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

/// A slot's target as `(memory_type, access, physical_offset, label)`;
/// `None` for an open-bus window, which the views leave out.
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

/// A [`CartridgeDebugger`] bound to the emulator's breakpoint list, over a
/// copy of the cartridge's plan.
pub struct MapPlanDebugger<'a, S> {
    plan: MapPlan<S>,
    /// The ROM's length in bytes.
    rom_size: u32,
    /// The battery RAM's length in bytes (0 with none).
    sram_size: u32,
    /// The mapper's id, `None` when the cartridge reports none.
    mapper_id: Option<MapperId>,
    /// The mapper's registers, when the console reads them out.
    registers: Vec<MapperRegister>,
    /// Every chip's state, keyed by `chip_id`; empty with no chip.
    chip_states: Vec<(u8, ExpansionChipState)>,
    /// Every chip's label, keyed by `chip_id`; empty with no chip.
    chip_labels: Vec<(u8, String)>,
    breakpoints: &'a mut BreakpointList,
}

impl<'a, S: SlotShape<Target = MapTarget>> MapPlanDebugger<'a, S> {
    /// A debugger over `plan`, with the ROM and battery RAM sizes, the
    /// mapper's id, the chips' states and the emulator's breakpoint list.
    pub fn new(
        plan: MapPlan<S>,
        rom_size: u32,
        sram_size: u32,
        mapper_id: Option<MapperId>,
        chip_states: Vec<(u8, ExpansionChipState)>,
        breakpoints: &'a mut BreakpointList,
    ) -> Self {
        Self {
            plan,
            rom_size,
            sram_size,
            mapper_id,
            registers: Vec::new(),
            chip_states,
            chip_labels: Vec::new(),
            breakpoints,
        }
    }

    /// The mapper's registers, reported with its state.
    pub fn with_mapper_registers(mut self, registers: Vec<MapperRegister>) -> Self {
        self.registers = registers;
        self
    }

    /// The chips' labels, keyed by `chip_id`, reported by
    /// [`CartridgeDebugger::get_chip_labels`].
    pub fn with_chip_labels(mut self, labels: Vec<(u8, String)>) -> Self {
        self.chip_labels = labels;
        self
    }

    /// The battery RAM's kind for the views: battery-backed when there is
    /// any.
    fn sram_kind(&self) -> SRAMType {
        if self.sram_size > 0 {
            SRAMType::Battery
        } else {
            SRAMType::None
        }
    }
}

impl<S: SlotShape<Target = MapTarget>> CartridgeDebugger for MapPlanDebugger<'_, S> {
    fn get_memory_map(&self) -> MemoryMap {
        let regions = self
            .plan
            .slots
            .iter()
            .filter_map(|slot| {
                let (memory_type, access, physical_offset, label) = classify_target(slot.target())?;
                let (start, end) = slot.bounds();
                Some(MemoryMapRegion {
                    cpu_address_start: start,
                    cpu_address_end: end,
                    physical_offset,
                    size: slot.size(),
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
            .filter(|slot| matches!(slot.target(), MapTarget::Rom { .. }))
            .map(|slot| {
                let (start, end) = slot.bounds();
                RomRegionInfo {
                    start,
                    end,
                    // A window can mirror a smaller ROM; the region claims
                    // no more bytes than the image holds.
                    size: if self.rom_size > 0 {
                        slot.size().min(self.rom_size as usize)
                    } else {
                        slot.size()
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
            .filter(|slot| matches!(slot.target(), MapTarget::Sram { .. }))
            .map(|slot| {
                let (start, end) = slot.bounds();
                SramRegionInfo {
                    start,
                    end,
                    // A window can mirror the RAM; the region reports the
                    // RAM's own size when it is known.
                    size: if self.sram_size > 0 {
                        self.sram_size as usize
                    } else {
                        slot.size()
                    },
                    sram_type,
                    description: format!("SRAM ({})", self.plan.name),
                }
            })
            .collect()
    }

    fn get_mapper_state(&self) -> Option<MapperState> {
        let mapper_id = self.mapper_id.clone()?;
        let banks = self
            .plan
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| {
                let (memory_type, _, physical_offset, _) = classify_target(slot.target())?;
                let (start, end) = slot.bounds();
                Some(BankInfo {
                    cpu_address_start: start,
                    cpu_address_end: end,
                    physical_offset,
                    size: slot.size(),
                    memory_type,
                    bank_number: i as u32,
                })
            })
            .collect();
        Some(MapperState {
            mapper_id,
            registers: self.registers.clone(),
            bank_config: BankConfiguration { banks },
        })
    }

    fn get_chip_state(&self) -> Option<ExpansionChipState> {
        // The primary chip, the lowest id.
        self.chip_states.first().map(|(_, state)| state.clone())
    }

    fn get_chip_states(&self) -> Vec<(u8, ExpansionChipState)> {
        self.chip_states.clone()
    }

    fn map_regions(&self) -> Vec<MapRegionSummary> {
        describe_map_plan(&self.plan)
    }

    fn get_chip_labels(&self) -> Vec<(u8, String)> {
        self.chip_labels.clone()
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
