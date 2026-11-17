//! The cartridge a console's bus holds: one [`ComposableCartridge`] over the
//! console's [`Board`], reached through the [`Cartridge`] trait.
//!
//! A cartridge is a ROM image, a stock battery RAM (empty when it has
//! none), a [`MapPlan`] its address space resolves through, and a
//! [`ChipSetOf`] of coprocessors, each at its own `chip_id`. Everything
//! between an access and the byte it reaches is written once, here, as the
//! trait's provided methods, monomorphised on the concrete cartridge: the
//! resolution through the plan, the dispatch to the ROM, the RAM or a chip,
//! the debugger's side-effect-free peek, where a byte comes from, the
//! forwarders over the chip set, and the bridge between the chips' opaque
//! savestates and a console's typed chip-state list. A console says what
//! its hardware makes different through its [`Board`]: what an address that
//! holds nothing reads, how its battery RAM answers (the Mega Drive's SRAM
//! switch), a register it answers outside the plan, writes it watches.
//!
//! The builder's chip loop is here too: [`ChipDecl`] declares a chip and
//! [`install_chips`] builds each one through the console's
//! [`ChipFactory`] into the chip set, with the bus ranges it observes.

use std::ops::RangeInclusive;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;

use super::manifest::{parse_addr_range, ManifestChip, ManifestError};
use super::map_plan::{MapPlan, SlotShape};
use super::{
    CartridgeInfo, ChipFactory, ChipSetOf, Coprocessor, ExpansionChipState, MapTarget, SourcePlace,
};
use crate::emulator::StepResult;
use crate::types::{CpuState, Instruction};

/// The console's part of a cartridge. Every hook defaults to the plain
/// behaviour, so a board overrides only what its console's cartridges do
/// differently.
pub trait Board: Send + 'static {
    /// The console's map slot.
    type Slot: SlotShape<Target = MapTarget> + Send;
    /// The chip object the chip set holds: `dyn Coprocessor`, or a console's
    /// subtrait of it.
    type Chip: ?Sized + Coprocessor;
    /// The element of the console's savestate chip list (its typed enum of
    /// chip states).
    type ChipState: Serialize + DeserializeOwned;

    /// What the cartridge answers where it holds nothing (ROM past its
    /// image, battery RAM past its size, a chip id with no chip). `None`
    /// answers the bus's open-bus value.
    const MISSING: Option<u8>;

    /// The byte a read of the battery RAM at `offset` gives, `missing` where
    /// nothing is. The default reads `sram`.
    fn read_sram(&self, rom: &[u8], sram: &[u8], offset: u32, missing: u8) -> u8 {
        let _ = rom;
        sram.get(offset as usize).copied().unwrap_or(missing)
    }

    /// Whether a write to the battery RAM lands now. The default: always.
    fn sram_writable(&self) -> bool {
        true
    }

    /// Where a read of the battery RAM at `offset` comes from, for a ROM of
    /// `rom_len` bytes. The default: the cartridge's RAM.
    fn sram_source_place(&self, rom_len: usize, offset: u32) -> SourcePlace {
        let _ = (rom_len, offset);
        SourcePlace::CartridgeRam
    }

    /// A write on the console's bus the board watches, delivered before the
    /// chips' observers. The default watches nothing.
    fn observe_write(&mut self, addr: u32, value: u8) {
        let _ = (addr, value);
    }

    /// Whether the board watches any write ([`observe_write`](Self::observe_write)).
    fn observes_bus(&self) -> bool {
        false
    }

    /// A register the board answers outside the plan, `None` where it
    /// answers none.
    fn read_register(&self, addr: u32) -> Option<u8> {
        let _ = addr;
        None
    }

    /// After any write a chip received, through its window or by observing
    /// the bus: a board whose own state follows a chip's registers updates
    /// it here.
    fn after_chip_write(&mut self, chips: &ChipSetOf<Self::Chip>) {
        let _ = chips;
    }

    /// The board's part of the cartridge's reset.
    fn reset(&mut self) {}
}

/// A cartridge as a console's bus and debugger see it. The required
/// methods are the storage; the provided ones are the access path, written
/// once for every console, and a console calls them on its concrete
/// cartridge, so none of them is a dynamic call. `open_bus` is the
/// caller's open-bus value: a constant on a console whose undriven bus
/// reads one, the memory data register on one whose bus keeps the last
/// byte.
pub trait Cartridge: Send {
    /// The console's map slot.
    type Slot: SlotShape<Target = MapTarget>;
    /// The chip object the chip set holds.
    type Chip: ?Sized + Coprocessor;
    /// The element of the console's savestate chip list.
    type ChipState: Serialize + DeserializeOwned;

    /// The plan every access resolves through.
    fn map_plan(&self) -> &MapPlan<Self::Slot>;
    /// What the cartridge is (title, sizes, hash, mapper, chip).
    fn info(&self) -> &CartridgeInfo;
    /// The ROM image as the cartridge maps it.
    fn rom(&self) -> &[u8];
    /// The coprocessors.
    fn chips(&self) -> &ChipSetOf<Self::Chip>;
    /// The coprocessors, mutably.
    fn chips_mut(&mut self) -> &mut ChipSetOf<Self::Chip>;
    /// What a read of nothing gives, given the caller's open-bus value.
    fn missing(&self, open_bus: u8) -> u8;
    /// A read of the battery RAM at `offset` (the slot's base added).
    fn read_sram(&self, offset: u32, open_bus: u8) -> u8;
    /// A write of the battery RAM at `offset`.
    fn write_sram(&mut self, offset: u32, value: u8);
    /// The battery image a host saves (`.srm`, a savestate's RAM).
    fn sram(&self) -> &[u8];
    /// Restore the battery image, copying the prefix both have.
    fn restore_sram(&mut self, data: &[u8]);
    /// Whether the battery image changed since the flag was last cleared.
    fn is_sram_dirty(&self) -> bool;
    /// Clear the battery-changed flag (after a save).
    fn clear_sram_dirty(&mut self);

    /// A read of `target` at the slot-relative `offset`: the ROM byte (the
    /// missing byte past the image), the battery RAM, the chip's window (the
    /// missing byte with no chip), or open bus.
    #[inline]
    fn read_target(
        &mut self,
        target: MapTarget,
        offset: u32,
        master_cycle: u64,
        open_bus: u8,
    ) -> u8 {
        match target {
            MapTarget::Rom { base_offset } => {
                match self.rom().get(base_offset.wrapping_add(offset) as usize) {
                    Some(&byte) => byte,
                    None => self.missing(open_bus),
                }
            }
            MapTarget::Sram { base_offset } => {
                self.read_sram(base_offset.wrapping_add(offset), open_bus)
            }
            MapTarget::ChipMmio { chip_id, base } => {
                self.read_chip(chip_id, base.wrapping_add(offset), master_cycle, open_bus)
            }
            MapTarget::OpenBus => open_bus,
        }
    }

    /// A write of `target` at the slot-relative `offset`. A ROM or open-bus
    /// write is dropped; a chip's observers are not called (a write a chip
    /// observes is the bus's to forward).
    #[inline]
    fn write_target(&mut self, target: MapTarget, offset: u32, value: u8, master_cycle: u64) {
        match target {
            MapTarget::Rom { .. } | MapTarget::OpenBus => {}
            MapTarget::Sram { base_offset } => {
                self.write_sram(base_offset.wrapping_add(offset), value)
            }
            MapTarget::ChipMmio { chip_id, base } => {
                self.write_chip(chip_id, base.wrapping_add(offset), value, master_cycle)
            }
        }
    }

    /// The byte a read of `target` at `offset` would give, read without a
    /// side effect, for the debugger: the ROM and the battery RAM as a read
    /// gives them, a chip's window as its [`Coprocessor::peek`] gives it,
    /// and the open-bus value where the chip cannot answer without acting
    /// or there is no chip.
    fn peek_target(&self, target: MapTarget, offset: u32, open_bus: u8) -> u8 {
        match target {
            MapTarget::Rom { base_offset } => {
                match self.rom().get(base_offset.wrapping_add(offset) as usize) {
                    Some(&byte) => byte,
                    None => self.missing(open_bus),
                }
            }
            MapTarget::Sram { base_offset } => {
                self.read_sram(base_offset.wrapping_add(offset), open_bus)
            }
            MapTarget::ChipMmio { chip_id, base } => self
                .peek_chip(chip_id, base.wrapping_add(offset))
                .unwrap_or(open_bus),
            MapTarget::OpenBus => open_bus,
        }
    }

    /// Where the byte a read of `target` at `offset` gives comes from: the
    /// ROM image's offset (`Unknown` past its end), the battery RAM as the
    /// board says, the chip's own answer (`Coprocessor` with no chip), or
    /// `Unknown` for open bus. Reads the mapping and changes nothing.
    fn target_source_place(&self, target: MapTarget, offset: u32) -> SourcePlace {
        match target {
            MapTarget::Rom { base_offset } => {
                let offset = base_offset.wrapping_add(offset);
                if (offset as usize) < self.rom().len() {
                    SourcePlace::Rom(offset)
                } else {
                    SourcePlace::Unknown
                }
            }
            MapTarget::Sram { base_offset } => {
                self.sram_source_place(base_offset.wrapping_add(offset))
            }
            MapTarget::ChipMmio { chip_id, base } => self
                .chips()
                .source_place(chip_id, base.wrapping_add(offset))
                .unwrap_or(SourcePlace::Coprocessor),
            MapTarget::OpenBus => SourcePlace::Unknown,
        }
    }

    /// Where a read of the battery RAM at `offset` comes from. The default:
    /// the cartridge's RAM.
    fn sram_source_place(&self, offset: u32) -> SourcePlace {
        let _ = offset;
        SourcePlace::CartridgeRam
    }

    /// A read of the bus address `addr` through the plan: the open-bus
    /// value where no slot covers it.
    #[inline]
    fn read_bus(&mut self, addr: u32, master_cycle: u64, open_bus: u8) -> u8 {
        let Some(resolved) = self.map_plan().resolve(addr) else {
            return open_bus;
        };
        let target = self.map_plan().slots[resolved.slot_index as usize].target();
        self.read_target(target, resolved.target_offset, master_cycle, open_bus)
    }

    /// A write of the bus address `addr`: the observers first (the board's,
    /// then the chips'), then the slot that covers it.
    #[inline]
    fn write_bus(&mut self, addr: u32, value: u8, master_cycle: u64) {
        if self.observes_bus() {
            self.observe_bus_write(addr, value);
        }
        let Some(resolved) = self.map_plan().resolve(addr) else {
            return;
        };
        let target = self.map_plan().slots[resolved.slot_index as usize].target();
        self.write_target(target, resolved.target_offset, value, master_cycle);
    }

    /// The byte a read of the bus address `addr` would give, read without a
    /// side effect ([`peek_target`](Self::peek_target)); the open-bus value
    /// where no slot covers it.
    fn peek_bus(&self, addr: u32, open_bus: u8) -> u8 {
        let Some(resolved) = self.map_plan().resolve(addr) else {
            return open_bus;
        };
        let target = self.map_plan().slots[resolved.slot_index as usize].target();
        self.peek_target(target, resolved.target_offset, open_bus)
    }

    /// Where the byte a read of the bus address `addr` gives comes from
    /// ([`target_source_place`](Self::target_source_place)); `Unknown` where
    /// no slot covers it.
    fn source_place(&self, addr: u32) -> SourcePlace {
        let Some(resolved) = self.map_plan().resolve(addr) else {
            return SourcePlace::Unknown;
        };
        let target = self.map_plan().slots[resolved.slot_index as usize].target();
        self.target_source_place(target, resolved.target_offset)
    }

    /// A register the cartridge answers outside the plan, `None` where it
    /// answers none.
    fn read_register(&self, addr: u32) -> Option<u8> {
        let _ = addr;
        None
    }

    /// Forward a write the console's bus makes to the ranges the cartridge
    /// observes. The default: the chips' observers.
    fn observe_bus_write(&mut self, addr: u32, value: u8) {
        self.chips_mut().observe_write(addr, value);
    }

    /// Whether anything on the cartridge observes the bus, so a console can
    /// skip the forward for a cartridge that observes nothing. The default:
    /// whether a chip registered an observer.
    fn observes_bus(&self) -> bool {
        self.chips().has_observers()
    }

    /// The cartridge's reset (its chips are reset by
    /// [`reset_chip`](Self::reset_chip), which needs the region's clock).
    fn reset(&mut self) {}

    /// A read of chip `chip_id`'s window at `offset` (the slot's base
    /// added), with the console's open-bus value; the missing byte when
    /// there is no chip at that id.
    #[inline]
    fn read_chip(&mut self, chip_id: u8, offset: u32, master_cycle: u64, open_bus: u8) -> u8 {
        match self
            .chips_mut()
            .read_with_open_bus(chip_id, offset, master_cycle, open_bus)
        {
            Some(value) => value,
            None => self.missing(open_bus),
        }
    }

    /// A write of chip `chip_id`'s window. Dropped when there is no chip.
    #[inline]
    fn write_chip(&mut self, chip_id: u8, offset: u32, value: u8, master_cycle: u64) {
        self.chips_mut().write(chip_id, offset, value, master_cycle);
    }

    /// The byte a read of chip `chip_id`'s window would give, read without
    /// a side effect; `None` with no chip, or when the chip cannot answer
    /// without acting.
    fn peek_chip(&self, chip_id: u8, offset: u32) -> Option<u8> {
        self.chips().peek(chip_id, offset)
    }

    /// Advance every chip's clock to `master_cycle`.
    fn catch_up_chip(&mut self, master_cycle: u64) {
        self.chips_mut().catch_up_all(master_cycle);
    }

    /// Reset every chip, re-anchoring its clock to `master_clock_hz`.
    fn reset_chip(&mut self, master_clock_hz: u64) {
        self.chips_mut().reset_all(master_clock_hz);
    }

    /// Whether any chip asserts its interrupt line toward the console's CPU.
    fn has_irq(&self) -> bool {
        self.chips().has_irq()
    }

    /// Whether any chip ticks on every cycle of the console's CPU.
    fn needs_tick(&self) -> bool {
        self.chips().needs_tick()
    }

    /// One cycle of the console's CPU for every chip that ticks.
    fn tick(&mut self, master_cycle: u64) {
        self.chips_mut().tick(master_cycle);
    }

    /// Whether any chip makes sound the console mixes.
    fn produces_audio(&self) -> bool {
        self.chips().produces_audio()
    }

    /// The chips' sound output now, summed.
    fn audio_level(&self) -> f32 {
        self.chips().audio_level()
    }

    /// The label of the primary chip (the lowest id), for a summary.
    fn coprocessor_label(&self) -> Option<&str> {
        let id = self.chips().first_id()?;
        self.chips().get(id).map(|chip| chip.label())
    }

    /// The window accesses the primary chip has counted since its reset.
    fn coprocessor_accesses(&self) -> u64 {
        self.chips()
            .first_id()
            .and_then(|id| self.chips().get(id))
            .map_or(0, |chip| chip.accesses())
    }

    /// The primary chip's live state for the debugger.
    fn coprocessor_debug_state(&self) -> Option<ExpansionChipState> {
        let id = self.chips().first_id()?;
        self.chips().get(id).map(|chip| chip.debug_state())
    }

    /// Every chip's live state, keyed by `chip_id`.
    fn coprocessors_debug_state(&self) -> Vec<(u8, ExpansionChipState)> {
        self.chips().debug_states()
    }

    /// Every chip's label, keyed by `chip_id`.
    fn coprocessor_labels(&self) -> Vec<(u8, String)> {
        self.chips().labels()
    }

    /// Every chip's savestate as the console's typed list: each chip's
    /// opaque bytes decoded as one [`Self::ChipState`]. A chip with nothing
    /// to keep (an empty blob) is left out.
    fn coprocessors_savestate(&self) -> Vec<(u8, Self::ChipState)> {
        self.chips()
            .serialize()
            .into_iter()
            .filter(|(_, bytes)| !bytes.is_empty())
            .filter_map(|(id, bytes)| {
                bincode::deserialize::<Self::ChipState>(&bytes)
                    .ok()
                    .map(|state| (id, state))
            })
            .collect()
    }

    /// Restore every chip from the console's typed list, each state encoded
    /// back to the bytes its chip reads, matched by `chip_id`; the clocks
    /// re-anchored to `master_clock_hz`. A state with no chip at its id is
    /// dropped.
    fn restore_coprocessors_savestate(
        &mut self,
        states: &[(u8, Self::ChipState)],
        master_clock_hz: u64,
    ) {
        let blobs: Vec<(u8, Vec<u8>)> = states
            .iter()
            .filter_map(|(id, state)| bincode::serialize(state).ok().map(|bytes| (*id, bytes)))
            .collect();
        self.chips_mut().restore(&blobs, master_clock_hz);
    }

    /// The registers of a chip's own CPU, for the debugger's extra CPU slot;
    /// `None` when no chip has one.
    fn secondary_cpu_state(&self) -> Option<CpuState> {
        self.chips().secondary_cpu_state()
    }

    /// One instruction of a chip's own CPU; `None` when no chip has one.
    fn step_secondary_cpu(&mut self) -> Option<StepResult> {
        self.chips_mut().step_secondary_cpu()
    }

    /// `count` instructions of a chip's own CPU from `addr`, decoded without
    /// a side effect; empty when no chip has one.
    fn disassemble_secondary(&self, addr: u32, count: u32) -> Vec<Instruction> {
        self.chips().disassemble_secondary(addr, count)
    }

    /// Turn the trace of a chip's own CPU on or off.
    fn set_secondary_trace_enabled(&mut self, enabled: bool) {
        self.chips_mut().set_secondary_trace_enabled(enabled);
    }

    /// The trace records of a chip's own CPU since the last call.
    fn take_secondary_trace(&mut self) -> Vec<u8> {
        self.chips_mut().take_secondary_trace()
    }
}

/// A cartridge composed of a ROM image, a stock battery RAM (empty when it
/// has none or its save lives in a chip), a map plan and a chip set, over a
/// console's [`Board`]. A cartridge with no chip is this with an empty set.
pub struct ComposableCartridge<B: Board> {
    rom: Arc<[u8]>,
    sram: Vec<u8>,
    sram_dirty: bool,
    /// The chip whose battery is the cartridge's save, when one was named.
    battery_chip: Option<u8>,
    plan: MapPlan<B::Slot>,
    chips: ChipSetOf<B::Chip>,
    info: CartridgeInfo,
    board: B,
}

impl<B: Board> ComposableCartridge<B> {
    /// A cartridge with no battery RAM of its own; [`with_sram`](Self::with_sram)
    /// gives it one, [`with_battery_chip`](Self::with_battery_chip) names the
    /// chip its save lives in.
    pub fn new(
        rom: Arc<[u8]>,
        plan: MapPlan<B::Slot>,
        chips: ChipSetOf<B::Chip>,
        info: CartridgeInfo,
        board: B,
    ) -> Self {
        Self {
            rom,
            sram: Vec::new(),
            sram_dirty: false,
            battery_chip: None,
            plan,
            chips,
            info,
            board,
        }
    }

    /// The battery RAM, as given (each console's builder chooses what a new
    /// one holds).
    pub fn with_sram(mut self, sram: Vec<u8>) -> Self {
        self.sram = sram;
        self
    }

    /// Name the chip whose battery is the cartridge's save: the battery
    /// image a host saves and restores is that chip's, while it has one.
    pub fn with_battery_chip(mut self, chip_id: u8) -> Self {
        self.battery_chip = Some(chip_id);
        self
    }

    /// The ROM image, for a chip that shares it.
    pub fn rom_arc(&self) -> &Arc<[u8]> {
        &self.rom
    }

    /// The console's part of the cartridge.
    pub fn board(&self) -> &B {
        &self.board
    }

    /// The console's part of the cartridge, mutably.
    pub fn board_mut(&mut self) -> &mut B {
        &mut self.board
    }

    /// The named chip's battery, while the chip has one.
    fn chip_battery(&self) -> Option<&[u8]> {
        self.chips.get(self.battery_chip?)?.battery()
    }

    /// The named chip, while it has a battery.
    fn battery_chip_mut(&mut self) -> Option<&mut B::Chip> {
        let chip = self.chips.get_mut(self.battery_chip?)?;
        chip.battery().is_some().then_some(chip)
    }
}

impl<B: Board> Cartridge for ComposableCartridge<B> {
    type Slot = B::Slot;
    type Chip = B::Chip;
    type ChipState = B::ChipState;

    #[inline]
    fn map_plan(&self) -> &MapPlan<B::Slot> {
        &self.plan
    }

    fn info(&self) -> &CartridgeInfo {
        &self.info
    }

    #[inline]
    fn rom(&self) -> &[u8] {
        &self.rom
    }

    #[inline]
    fn chips(&self) -> &ChipSetOf<B::Chip> {
        &self.chips
    }

    #[inline]
    fn chips_mut(&mut self) -> &mut ChipSetOf<B::Chip> {
        &mut self.chips
    }

    #[inline(always)]
    fn missing(&self, open_bus: u8) -> u8 {
        B::MISSING.unwrap_or(open_bus)
    }

    #[inline]
    fn read_sram(&self, offset: u32, open_bus: u8) -> u8 {
        self.board
            .read_sram(&self.rom, &self.sram, offset, self.missing(open_bus))
    }

    #[inline]
    fn write_sram(&mut self, offset: u32, value: u8) {
        if !self.board.sram_writable() {
            return;
        }
        if let Some(byte) = self.sram.get_mut(offset as usize) {
            *byte = value;
            self.sram_dirty = true;
        }
    }

    fn sram(&self) -> &[u8] {
        match self.chip_battery() {
            Some(battery) => battery,
            None => &self.sram,
        }
    }

    fn restore_sram(&mut self, data: &[u8]) {
        if let Some(chip) = self.battery_chip_mut() {
            chip.restore_battery(data);
            return;
        }
        let n = data.len().min(self.sram.len());
        self.sram[..n].copy_from_slice(&data[..n]);
    }

    fn is_sram_dirty(&self) -> bool {
        match self.battery_chip.and_then(|id| self.chips.get(id)) {
            Some(chip) if chip.battery().is_some() => chip.battery_dirty(),
            _ => self.sram_dirty,
        }
    }

    fn clear_sram_dirty(&mut self) {
        match self.battery_chip_mut() {
            Some(chip) => chip.clear_battery_dirty(),
            None => self.sram_dirty = false,
        }
    }

    fn sram_source_place(&self, offset: u32) -> SourcePlace {
        self.board.sram_source_place(self.rom.len(), offset)
    }

    fn read_register(&self, addr: u32) -> Option<u8> {
        self.board.read_register(addr)
    }

    fn observe_bus_write(&mut self, addr: u32, value: u8) {
        self.board.observe_write(addr, value);
        if self.chips.observe_write(addr, value) {
            self.board.after_chip_write(&self.chips);
        }
    }

    #[inline]
    fn observes_bus(&self) -> bool {
        self.chips.has_observers() || self.board.observes_bus()
    }

    fn reset(&mut self) {
        self.board.reset();
        self.sram_dirty = false;
    }

    #[inline]
    fn write_chip(&mut self, chip_id: u8, offset: u32, value: u8, master_cycle: u64) {
        self.chips.write(chip_id, offset, value, master_cycle);
        self.board.after_chip_write(&self.chips);
    }
}

/// A chip a builder installs: its id, the factory's name for it, and the
/// bus ranges it observes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChipDecl {
    pub id: u8,
    pub kind: String,
    pub observes: Vec<RangeInclusive<u32>>,
}

impl ChipDecl {
    /// The chip a manifest's `chips[index]` declares. A malformed `observes`
    /// entry is named `chips[index].observes[j]`.
    pub fn from_manifest<S>(chip: &ManifestChip<S>, index: usize) -> Result<Self, ManifestError> {
        let observes = chip
            .observes
            .iter()
            .enumerate()
            .map(|(j, range)| parse_addr_range(range, &format!("chips[{index}].observes[{j}]")))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            id: chip.id,
            kind: chip.kind.clone(),
            observes,
        })
    }
}

/// A chip kind the console's factory does not know.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown coprocessor chip kind: {kind}")]
pub struct UnknownChip {
    pub kind: String,
}

/// Build every declared chip through `factory` into `chips`, in order, at
/// its id, with the bus ranges it observes, each given a share of `rom`
/// and `master_clock_hz` as its first clock anchor. Stops at the first kind
/// the factory does not know.
pub fn install_chips<C, F>(
    factory: &F,
    decls: impl IntoIterator<Item = ChipDecl>,
    rom: &Arc<[u8]>,
    master_clock_hz: u64,
    chips: &mut ChipSetOf<C>,
) -> Result<(), UnknownChip>
where
    C: ?Sized + Coprocessor,
    F: ChipFactory<C> + ?Sized,
{
    for decl in decls {
        let chip = factory
            .build(&decl.kind, decl.id, Arc::clone(rom), master_clock_hz)
            .ok_or_else(|| UnknownChip {
                kind: decl.kind.clone(),
            })?;
        chips.insert(decl.id, chip);
        for range in decl.observes {
            chips.add_observer(range, decl.id);
        }
    }
    Ok(())
}
