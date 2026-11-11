//! [`CartridgeDebugger`] over a SNES cartridge's [`MapPlan`].
//!
//! The debugger is a thin, borrowing view: it owns a cheap snapshot of
//! the installed map plan (a handful of slots) plus the ROM/SRAM sizes
//! and detected mapper kind, and borrows the emulator's single
//! [`BreakpointList`] so ROM/SRAM breakpoints land in the *same* list
//! the CPU step path checks, no parallel breakpoint state.
//!
//! `SnesEmulator::cartridge_debugger()` constructs it; the trait's
//! inspection methods read the plan, and the breakpoint methods
//! delegate to the borrowed list.

use retro_core::cartridge::{
    AccessType, BankConfiguration, BankInfo, BreakpointId, CartridgeDebugger, ExpansionChipState,
    MapperId, MapperState, MemoryMap, MemoryMapRegion, MemoryType, RomRegionInfo, SRAMType,
    SramRegionInfo, WatchType,
};
use retro_core::debug::BreakpointList;
use retro_core::types::{Breakpoint, BreakpointType};

use crate::cartridge::{MapPlan, MapSlot, MapTarget, PresetKind};

/// A [`CartridgeDebugger`] bound to the emulator's live breakpoint list.
///
/// Construct via `SnesEmulator::cartridge_debugger()`. The lifetime
/// ties the borrowed [`BreakpointList`] to the emulator for the
/// duration of the debug interaction.
pub struct SnesCartridgeDebugger<'a> {
    /// Snapshot of the installed map plan (cloned, a few slots).
    plan: MapPlan,
    /// ROM image length in bytes.
    rom_size: u32,
    /// Battery SRAM length in bytes (0 when the cart has none).
    sram_size: u32,
    /// Detected mapper kind, used to label [`MapperState`].
    preset_kind: Option<PresetKind>,
    /// Coprocessor chip snapshots captured at construction, keyed by
    /// `chip_id` (DSP-1 at id 1, ...). Empty for a plain cartridge. The
    /// full set is returned by [`Self::get_chip_states`]; the primary
    /// (lowest-id) chip by [`Self::get_chip_state`].
    chip_states: Vec<(u8, ExpansionChipState)>,
    /// The emulator's breakpoint list; ROM/SRAM breakpoints append here.
    breakpoints: &'a mut BreakpointList,
}

impl<'a> SnesCartridgeDebugger<'a> {
    /// Bind a cartridge debugger to a map plan + breakpoint list, with the
    /// coprocessor chip snapshots keyed by `chip_id` (empty for a plain
    /// cartridge).
    pub fn new(
        plan: MapPlan,
        rom_size: u32,
        sram_size: u32,
        preset_kind: Option<PresetKind>,
        chip_states: Vec<(u8, ExpansionChipState)>,
        breakpoints: &'a mut BreakpointList,
    ) -> Self {
        Self {
            plan,
            rom_size,
            sram_size,
            preset_kind,
            chip_states,
            breakpoints,
        }
    }

    /// SRAM kind reported in `get_sram_regions`, battery-backed when
    /// the cartridge carries any SRAM, else none.
    fn sram_kind(&self) -> SRAMType {
        if self.sram_size > 0 {
            SRAMType::Battery
        } else {
            SRAMType::None
        }
    }
}

/// Inclusive CPU address range a slot covers, packed as
/// `(bank << 16) | offset`.
fn slot_cpu_range(slot: &MapSlot) -> (u32, u32) {
    let bank_lo = u32::from(*slot.bank_range.start());
    let bank_hi = u32::from(*slot.bank_range.end());
    let off_lo = u32::from(*slot.offset_range.start());
    let off_hi = u32::from(*slot.offset_range.end());
    ((bank_lo << 16) | off_lo, (bank_hi << 16) | off_hi)
}

/// Total bytes a slot's rectangle covers (`banks x offset-span`).
fn slot_size(slot: &MapSlot) -> usize {
    let banks = usize::from(*slot.bank_range.end()) - usize::from(*slot.bank_range.start()) + 1;
    let span = usize::from(*slot.offset_range.end()) - usize::from(*slot.offset_range.start()) + 1;
    banks * span
}

/// Decode a slot target into `(memory_type, access, physical_offset,
/// label)`. `None` for open-bus windows, which the viewers omit.
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

impl CartridgeDebugger for SnesCartridgeDebugger<'_> {
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
                    // A slot can mirror a smaller ROM across a wider
                    // window; cap at the physical image size so the
                    // region never claims more bytes than exist.
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
                    // The plan slot can over-cover the physical SRAM via
                    // mirroring; report the real chip size when known.
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
        let mapper_id = match self.preset_kind? {
            PresetKind::LoRom => MapperId::SnesLoROM,
            PresetKind::HiRom => MapperId::SnesHiROM,
            PresetKind::ExHiRom => MapperId::SnesExHiROM,
            // retro-core has no dedicated S-DD1 mapper id; surface it as a
            // named custom mapper.
            PresetKind::Sdd1 => MapperId::Custom("SDD1".to_string()),
            // Likewise the SPC7110 has no dedicated mapper id.
            PresetKind::Spc7110 => MapperId::Custom("SPC7110".to_string()),
            // The SA-1 has no dedicated mapper id either.
            PresetKind::Sa1 => MapperId::Custom("SA1".to_string()),
            // Likewise the Super FX has no dedicated mapper id.
            PresetKind::SuperFx => MapperId::Custom("Super FX".to_string()),
            // The Cx4 has no dedicated mapper id either.
            PresetKind::Cx4 => MapperId::Custom("Cx4".to_string()),
            PresetKind::Custom => MapperId::Custom(self.plan.name.clone()),
        };
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
            // Plain LoROM/HiROM carts expose no software-visible mapper
            // registers; coprocessor carts would surface theirs here.
            registers: Vec::new(),
            bank_config: BankConfiguration { banks },
        })
    }

    fn get_chip_state(&self) -> Option<ExpansionChipState> {
        // The primary (lowest-id) chip, captured from the live cartridge at
        // construction (DSP-1 surfaces its uPD7725 registers + data RAM);
        // `None` for a plain cart.
        self.chip_states.first().map(|(_, s)| s.clone())
    }

    fn get_chip_states(&self) -> Vec<(u8, ExpansionChipState)> {
        // Every chip captured at construction, keyed by `chip_id`; empty for
        // a plain cart. Enumerates a multi-chip cartridge instead of
        // collapsing to the primary chip.
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
