//! Cartridge system for multi-console emulation
//!
//! This module provides generic traits and types for cartridge emulation across
//! multiple console systems (Genesis, NES, SNES, etc). It covers mappers,
//! coprocessors (expansion chips) and a cartridge's own audio output.

use serde::{Deserialize, Serialize};
use std::ops::RangeInclusive;
use std::sync::Arc;

pub mod audio;
pub mod detection;
pub mod parsers;
pub mod savestate;

pub use audio::Msu1Audio;
pub use savestate::{CartridgeSaveState, SaveStateMetadata, SramManager, SAVE_STATE_VERSION};

// ============================================================================
// Main Cartridge Trait
// ============================================================================

/// Main cartridge trait - generic across all consoles
pub trait Cartridge: Send + Sync {
    /// Get cartridge metadata
    fn info(&self) -> &CartridgeInfo;

    /// Read from cartridge address space
    /// addr is console-specific (Genesis: 0x000000+, NES: CPU space, etc)
    fn read(&mut self, addr: u32) -> u8;

    /// Write to cartridge address space (for SRAM, mapper registers, etc)
    fn write(&mut self, addr: u32, value: u8);

    /// Read word (16-bit) - default implementation uses read()
    fn read_word(&mut self, addr: u32) -> u16 {
        let hi = self.read(addr) as u16;
        let lo = self.read(addr + 1) as u16;
        (hi << 8) | lo
    }

    /// Write word (16-bit)
    fn write_word(&mut self, addr: u32, value: u16) {
        self.write(addr, (value >> 8) as u8);
        self.write(addr + 1, (value & 0xFF) as u8);
    }

    /// Clock the cartridge (for chips with timing, e.g. SVP, SuperFX)
    fn clock(&mut self, cycles: u32) -> u32 {
        cycles // Default: pass-through
    }

    /// Get SRAM/save data (if any)
    fn get_save_ram(&self) -> Option<&[u8]>;

    /// Set SRAM/save data
    fn set_save_ram(&mut self, data: &[u8]) -> Result<(), String>;

    /// Check if SRAM was modified (dirty flag)
    fn is_save_ram_dirty(&self) -> bool;

    /// Clear dirty flag
    fn clear_save_ram_dirty(&mut self);

    /// Serialize cartridge state (for save states)
    fn serialize_state(&self) -> Result<Vec<u8>, String>;

    /// Deserialize cartridge state
    fn deserialize_state(&mut self, data: &[u8]) -> Result<(), String>;

    /// Reset cartridge to initial state
    fn reset(&mut self);

    /// Get mapper if cartridge has one
    fn mapper(&self) -> Option<&dyn Mapper> {
        None
    }

    /// Get mapper (mutable)
    fn mapper_mut(&mut self) -> Option<&mut dyn Mapper> {
        None
    }

    /// Debugging: get cartridge debugger
    fn debugger(&self) -> Option<&dyn CartridgeDebugger> {
        None
    }

    /// Debugging: get cartridge debugger (mutable)
    fn debugger_mut(&mut self) -> Option<&mut dyn CartridgeDebugger> {
        None
    }

    /// Get cartridge audio interface (if cartridge generates audio)
    fn audio(&self) -> Option<&dyn CartridgeAudio> {
        None
    }

    /// Get cartridge audio interface (mutable)
    fn audio_mut(&mut self) -> Option<&mut dyn CartridgeAudio> {
        None
    }
}

// ============================================================================
// Cartridge Metadata
// ============================================================================

/// Cartridge metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CartridgeInfo {
    /// Console system
    pub system: ConsoleSystem,

    /// ROM title
    pub title: String,

    /// Serial number (if available)
    pub serial: Option<String>,

    /// Region(s)
    pub regions: Vec<Region>,

    /// ROM size (bytes)
    pub rom_size: usize,

    /// SRAM size (bytes), 0 if none
    pub sram_size: usize,

    /// SRAM type
    pub sram_type: SRAMType,

    /// Mapper info
    pub mapper_info: Option<MapperInfo>,

    /// Expansion chip info
    pub expansion_chip_info: Option<ExpansionChipInfo>,

    /// Checksum from header
    pub header_checksum: Option<u16>,

    /// Calculated checksum
    pub calculated_checksum: Option<u16>,

    /// ROM hash (for database lookup)
    pub rom_hash: String, // SHA-256

    /// Detection source
    pub detection_source: DetectionSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsoleSystem {
    Genesis,
    MegaDrive, // Same as Genesis but different region
    MegaCD,
    Sega32X,
    NES,
    Famicom,
    SNES,
    SuperFamicom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Region {
    Japan,
    USA,
    Europe,
    Asia,
    Brazil,
    Korea,
    China,
    World,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SRAMType {
    None,
    Battery, // Battery-backed SRAM
    EEPROM,  // Serial EEPROM (Sonic 3)
    Flash,   // Flash memory
    FRAM,    // Ferroelectric RAM
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DetectionSource {
    Header,       // Detected from ROM header
    Hash,         // Detected from hash database
    Manifest,     // Loaded from manifest file
    Heuristic,    // Guessed via heuristics
    UserOverride, // User manually specified
}

// ============================================================================
// Mapper Trait
// ============================================================================

/// Mapper trait - handles bank switching and memory mapping
pub trait Mapper: Send + Sync {
    /// Get mapper info
    fn info(&self) -> &MapperInfo;

    /// Map address to ROM offset for reading
    /// Returns (physical_offset, is_writable)
    fn map_read(&self, addr: u32) -> MappedAddress;

    /// Map address for write
    /// Returns MappedAddress if writable, None if write-protected
    fn map_write(&mut self, addr: u32, value: u8) -> Option<MappedAddress>;

    /// Handle mapper register write
    /// Returns true if handled, false if should pass through
    fn write_register(&mut self, addr: u32, value: u8) -> bool;

    /// Get current bank configuration
    fn get_bank_config(&self) -> BankConfiguration;

    /// Reset mapper to initial state
    fn reset(&mut self);

    /// Serialize mapper state
    fn serialize_mapper(&self) -> Result<Vec<u8>, String>;

    /// Deserialize mapper state
    fn deserialize_mapper(&mut self, data: &[u8]) -> Result<(), String>;
}

/// Mapped address result
#[derive(Debug, Clone, Copy)]
pub struct MappedAddress {
    /// Physical offset in ROM/RAM
    pub physical_offset: usize,

    /// Memory type
    pub memory_type: MemoryType,

    /// Is writable?
    pub writable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryType {
    ROM,
    SRAM,
    CartridgeRAM,
    ExpansionChipRAM,
    MappedIO, // For mapper registers
}

/// Bank configuration for debugging
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BankConfiguration {
    pub banks: Vec<BankInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BankInfo {
    pub cpu_address_start: u32,
    pub cpu_address_end: u32,
    pub physical_offset: usize,
    pub size: usize,
    pub memory_type: MemoryType,
    pub bank_number: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapperInfo {
    pub mapper_id: MapperId,
    pub name: String,
    pub description: String,
    pub features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MapperId {
    // Genesis
    GenesisNone,
    GenesisSEGA,
    GenesisSSF2,
    GenesisEEPROM,
    GenesisCodemasters,
    GenesisJCart,

    // NES (240+ mappers)
    NES(u16), // iNES mapper number

    // SNES
    SnesLoROM,
    SnesHiROM,
    SnesExHiROM,
    SnesExLoROM,

    // Custom
    Custom(String),
}

// ============================================================================
// Expansion Chip Metadata
// ============================================================================
//
// Chips run through the composable [`Coprocessor`] model below. These metadata
// types (`ExpansionChipInfo` / `ExpansionChipType` / `ExpansionChipState` /
// `ChipRegister`) are how the debugger surface describes a chip.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpansionChipInfo {
    pub name: String,
    pub chip_type: ExpansionChipType,
    pub description: String,
    pub features: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExpansionChipType {
    // Genesis
    SVP, // Virtua Racing

    // SNES
    SuperFX,
    SA1,
    SDD1,
    SPC7110,
    CX4,
    DSP1,
    DSP2,
    DSP3,
    DSP4,
    OBC1,
    ST010,
    ST011,
    SRTC,

    // NES (mostly audio)
    VRC6,
    VRC7,
    MMC5,
    Namco163,
    FME7,

    // Custom/Unknown
    Custom(u16),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChipRegister {
    pub address: u32,
    pub name: String,
    pub value: u32,
    pub description: String,
}

// ============================================================================
// Composable coprocessors (multi-chip cartridge composition)
// ============================================================================

/// A cartridge coprocessor, generic across consoles.
///
/// A chip is an independent entity: it does NOT know the CPU, the DMA engine,
/// or the console scheduler. It only ever sees reads/writes to its own MMIO,
/// *observed* writes on bus ranges it watches but does not own, and the
/// absolute master-cycle of each access (for lazy catch-up timing). A chip
/// that needs the cartridge ROM (an MMC or a decompressor) is handed an
/// `Arc<[u8]>` of it at construction (see [`ChipFactory`]); the trait itself
/// never exposes the ROM.
///
/// This is the composable, multi-chip surface, the single execution model
/// for every console coprocessor. It reuses the [`ExpansionChipInfo`] /
/// [`ExpansionChipType`] / [`ExpansionChipState`] metadata types for the
/// debugger surface.
pub trait Coprocessor: Send {
    /// Metadata (type, name, features).
    fn info(&self) -> &ExpansionChipInfo;

    /// Read the chip MMIO. `offset` is already reduced by the slot mask;
    /// `master_cycle` is the absolute master-cycle of the access (lazy
    /// catch-up).
    fn read(&mut self, offset: u32, master_cycle: u64) -> u8;

    /// Write the chip MMIO.
    fn write(&mut self, offset: u32, value: u8, master_cycle: u64);

    /// Passive bus observation. Called when the console writes a range the
    /// chip *observes* but does not *own* (e.g. the S-DD1 spying on the DMA
    /// registers at `$43xx`). Default no-op: the vast majority of chips
    /// observe nothing. This is the hook that lets a chip reconstruct console
    /// context (like DMA parameters) without coupling the DMA engine to the
    /// cartridge.
    fn observe_write(&mut self, _addr: u32, _value: u8) {}

    /// Advance the chip clock up to `master_cycle` without a register access
    /// (called once per frame so a self-clocked core keeps moving between
    /// frames). No-op for chips with no free-running clock.
    fn catch_up(&mut self, _master_cycle: u64) {}

    /// Hardware reset, re-anchoring any internal rate converter to the region
    /// master clock.
    fn reset(&mut self, master_clock_hz: u64);

    /// Pending IRQ? (SA-1, SuperFX). Default: none.
    fn has_irq(&self) -> bool {
        false
    }

    // -- Steppable secondary CPU ----------------------------------------------
    //
    // A coprocessor that carries its own debuggable CPU (the SA-1's second
    // 65816, the Super FX's GSU, the Cx4's HG51B) exposes it here so the
    // console can surface it as an extra debugger CPU slot without the console
    // knowing the chip's internals. The default impls report "no steppable
    // CPU", so DSP-1 / S-DD1 / SPC7110 / the SVP are unaffected.

    /// Register snapshot of the coprocessor's secondary CPU, for the
    /// debugger's extra CPU slot. `None` when the chip hosts no steppable
    /// CPU (the common case).
    fn secondary_cpu_state(&self) -> Option<crate::types::CpuState> {
        None
    }

    /// Single-step the secondary CPU by one instruction, returning its
    /// [`StepResult`](crate::emulator::StepResult). `None` when the chip
    /// hosts no steppable CPU.
    fn step_secondary_cpu(&mut self) -> Option<crate::emulator::StepResult> {
        None
    }

    /// Forward-decode `count` instructions of the secondary CPU from
    /// `addr`, side-effect-free (for the disassembly view). Empty when the
    /// chip hosts no steppable CPU.
    fn disassemble_secondary(&self, _addr: u32, _count: u32) -> Vec<crate::types::Instruction> {
        Vec::new()
    }

    /// Enable or disable instruction tracing of the coprocessor's secondary
    /// CPU. Default: no-op (chips without a traceable secondary CPU). A chip
    /// that captures a trace should discard any buffered records on disable so
    /// a stale tail can't leak into a later session.
    fn set_secondary_trace_enabled(&mut self, _enabled: bool) {}

    /// Drain the secondary CPU's trace records captured since the last call as
    /// a byte stream of fixed-size entries, the console re-parses them into
    /// its own typed trace ring (the entry type is console-specific, so the
    /// trait boundary stays `Vec<u8>`). Default: empty.
    fn take_secondary_trace(&mut self) -> Vec<u8> {
        Vec::new()
    }

    /// Volatile state serialized to opaque bytes (never the firmware/ROM,
    /// which is re-staged on load just like the ROM image). The trait
    /// boundary is `Vec<u8>` to stay generic; a console may `bincode` a typed
    /// enum inside.
    fn serialize(&self) -> Vec<u8>;

    /// Restore volatile state, re-anchoring any clock to `master_clock_hz`. A
    /// blob incompatible with this backend is dropped silently.
    fn restore(&mut self, data: &[u8], master_clock_hz: u64);

    /// Live snapshot for the debugger / CLI.
    fn debug_state(&self) -> ExpansionChipState;

    /// Human-readable summary ("DSP-1B (LLE)", "S-DD1") for the CLI `info`.
    fn label(&self) -> &str;

    /// CPU accesses to the MMIO since reset (diagnostic: proves the game is
    /// actually driving the chip).
    fn accesses(&self) -> u64 {
        0
    }

    /// Battery-backed persistent data the chip *owns* (e.g. a serial
    /// EEPROM's cell array), for `.srm` persistence. This is distinct from
    /// the volatile [`serialize`](Coprocessor::serialize) savestate: it is
    /// the durable save file, not the emulator snapshot. `None` for chips
    /// with no battery (the vast majority, DSP-1, S-DD1, the SVP). A
    /// cartridge whose save lives inside a chip (Genesis EEPROM carts)
    /// routes its `.srm` load/save through here.
    fn battery(&self) -> Option<&[u8]> {
        None
    }

    /// Restore battery-backed data from a loaded `.srm`. No-op for chips
    /// without a battery, or when the length does not match.
    fn restore_battery(&mut self, _data: &[u8]) {}

    /// Whether the battery data changed since the last flush (autosave
    /// debounce). Always `false` for chips with no battery.
    fn battery_dirty(&self) -> bool {
        false
    }

    /// Clear the battery-dirty flag after a `.srm` flush.
    fn clear_battery_dirty(&mut self) {}
}

/// A collection of independent coprocessors inside one cartridge, indexed by
/// `chip_id` (the same id a console's chip-MMIO map target carries), plus the
/// registry of observed bus ranges. Console-agnostic: it knows nothing about
/// bank:offset address decoding, the console resolves an address to a
/// `(chip_id, offset)` pair and calls in here.
///
/// The backing store is sparse: `index == chip_id`, with empty slots (`None`)
/// allowed. This lets a chip live at a fixed id the console's map plan already
/// carries (e.g. DSP-1 at id 1) without a chip at id 0, and lets a manifest
/// place chips at arbitrary ids. Use [`ChipSet::insert`] for an explicit id,
/// or [`ChipSet::push`] to append at the next free index.
#[derive(Default)]
pub struct ChipSet {
    chips: Vec<Option<Box<dyn Coprocessor>>>, // index = chip_id (holes allowed)
    observers: Vec<(RangeInclusive<u32>, u8)>, // observed range -> chip_id
}

impl ChipSet {
    /// Append a chip at the next free index, returning the `chip_id` it was
    /// assigned (its index).
    pub fn push(&mut self, chip: Box<dyn Coprocessor>) -> u8 {
        let id = self.chips.len() as u8;
        self.chips.push(Some(chip));
        id
    }

    /// Place `chip` at an explicit `id`, growing the backing store with empty
    /// slots as needed so `index == chip_id` holds. Overwrites any chip
    /// already at `id`.
    pub fn insert(&mut self, id: u8, chip: Box<dyn Coprocessor>) {
        let idx = id as usize;
        while self.chips.len() <= idx {
            self.chips.push(None);
        }
        self.chips[idx] = Some(chip);
    }

    /// Register a bus range for a chip to passively observe (see
    /// [`Coprocessor::observe_write`]).
    pub fn add_observer(&mut self, range: RangeInclusive<u32>, chip_id: u8) {
        self.observers.push((range, chip_id));
    }

    /// Number of chips actually present (empty slots are not counted).
    pub fn len(&self) -> usize {
        self.chips.iter().filter(|c| c.is_some()).count()
    }

    /// No chips present at all.
    pub fn is_empty(&self) -> bool {
        self.chips.iter().all(|c| c.is_none())
    }

    /// Any chip registered a bus observer (lets the console gate its
    /// `observe_write` wiring so plain cartridges pay nothing).
    pub fn has_observers(&self) -> bool {
        !self.observers.is_empty()
    }

    /// Shared borrow of the chip at `id`, if present.
    pub fn get(&self, id: u8) -> Option<&dyn Coprocessor> {
        self.chips.get(id as usize)?.as_deref()
    }

    /// Mutable borrow of the chip at `id`, if present.
    pub fn get_mut(&mut self, id: u8) -> Option<&mut (dyn Coprocessor + 'static)> {
        self.chips.get_mut(id as usize)?.as_deref_mut()
    }

    /// The id of the lowest-numbered present chip, if any. Consoles that
    /// still expose a single "primary" coprocessor (one label/accesses/debug
    /// surface, one-slot savestate) use this to pick it out of the set.
    pub fn first_id(&self) -> Option<u8> {
        self.chips.iter().position(Option::is_some).map(|i| i as u8)
    }

    /// Read the chip `id`'s MMIO. `None` if no such chip.
    pub fn read(&mut self, id: u8, offset: u32, master_cycle: u64) -> Option<u8> {
        self.get_mut(id).map(|c| c.read(offset, master_cycle))
    }

    /// Write the chip `id`'s MMIO. No-op if no such chip.
    pub fn write(&mut self, id: u8, offset: u32, value: u8, master_cycle: u64) {
        if let Some(c) = self.get_mut(id) {
            c.write(offset, value, master_cycle);
        }
    }

    /// Forward an observed bus write to every chip whose registered range
    /// covers `addr`. This is the dispatch behind `observe_write`.
    pub fn observe_write(&mut self, addr: u32, value: u8) {
        for (range, id) in &self.observers {
            if range.contains(&addr) {
                if let Some(slot) = self.chips.get_mut(*id as usize) {
                    if let Some(c) = slot.as_deref_mut() {
                        c.observe_write(addr, value);
                    }
                }
            }
        }
    }

    /// Advance every present chip's clock (called once per frame).
    pub fn catch_up_all(&mut self, master_cycle: u64) {
        for c in self.chips.iter_mut().flatten() {
            c.catch_up(master_cycle);
        }
    }

    /// Whether any present chip is asserting its S-CPU IRQ line (the SA-1's
    /// message / CC-DMA IRQ, the Super FX's `stop` IRQ). ORed into the S-CPU's IRQ line
    /// by the console bus each instruction, so a chip with no IRQ source (the
    /// common case) reports `false` and costs nothing.
    pub fn has_irq(&self) -> bool {
        self.chips.iter().flatten().any(|c| c.has_irq())
    }

    /// Register snapshot of the first present chip that hosts a steppable
    /// secondary CPU (the SA-1's second 65816, the Super FX's GSU or the
    /// Cx4's HG51B), for the debugger's extra CPU slot. `None` when no chip hosts one, the common case, so a plain
    /// cartridge stays a two-CPU console.
    pub fn secondary_cpu_state(&self) -> Option<crate::types::CpuState> {
        self.chips
            .iter()
            .flatten()
            .find_map(|c| c.secondary_cpu_state())
    }

    /// Single-step the first present chip's steppable secondary CPU by one
    /// instruction. `None` when no chip hosts one.
    pub fn step_secondary_cpu(&mut self) -> Option<crate::emulator::StepResult> {
        self.chips
            .iter_mut()
            .flatten()
            .find_map(|c| c.step_secondary_cpu())
    }

    /// Forward-decode `count` instructions of the first present chip's
    /// steppable secondary CPU from `addr`, side-effect-free. Empty when no
    /// chip hosts one.
    pub fn disassemble_secondary(&self, addr: u32, count: u32) -> Vec<crate::types::Instruction> {
        self.chips
            .iter()
            .flatten()
            .map(|c| c.disassemble_secondary(addr, count))
            .find(|rows| !rows.is_empty())
            .unwrap_or_default()
    }

    /// Enable/disable secondary-CPU tracing on every present chip (only a chip
    /// that hosts a steppable secondary CPU acts on it).
    pub fn set_secondary_trace_enabled(&mut self, enabled: bool) {
        for c in self.chips.iter_mut().flatten() {
            c.set_secondary_trace_enabled(enabled);
        }
    }

    /// Drain the secondary-CPU trace bytes captured by every present chip,
    /// concatenated in chip order.
    pub fn take_secondary_trace(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        for c in self.chips.iter_mut().flatten() {
            out.extend(c.take_secondary_trace());
        }
        out
    }

    /// Reset every present chip, re-anchoring to `master_clock_hz`.
    pub fn reset_all(&mut self, master_clock_hz: u64) {
        for c in self.chips.iter_mut().flatten() {
            c.reset(master_clock_hz);
        }
    }

    /// Multi-chip savestate: `(chip_id, opaque bytes)` per present chip.
    pub fn serialize(&self) -> Vec<(u8, Vec<u8>)> {
        self.chips
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.as_ref().map(|c| (i as u8, c.serialize())))
            .collect()
    }

    /// Restore multi-chip savestate, matching each blob to its chip by id.
    /// Entries with no matching chip are ignored.
    pub fn restore(&mut self, states: &[(u8, Vec<u8>)], master_clock_hz: u64) {
        for (id, data) in states {
            if let Some(c) = self.get_mut(*id) {
                c.restore(data, master_clock_hz);
            }
        }
    }

    /// Live debug snapshot of every present chip, keyed by id.
    pub fn debug_states(&self) -> Vec<(u8, ExpansionChipState)> {
        self.chips
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.as_ref().map(|c| (i as u8, c.debug_state())))
            .collect()
    }

    /// Human-readable label of every present chip, keyed by id (for the CLI
    /// `info`).
    pub fn labels(&self) -> Vec<(u8, String)> {
        self.chips
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.as_ref().map(|c| (i as u8, c.label().to_string())))
            .collect()
    }

    /// Battery-backed persistent data owned by the first present chip that
    /// has any (e.g. the SPC7110's game SRAM). `None` when no chip owns a
    /// battery, the common case, so a cartridge with no battery chip keeps
    /// using its own SRAM buffer. Only one chip is expected to own the
    /// cartridge's `.srm`, so the first match is authoritative.
    pub fn battery(&self) -> Option<&[u8]> {
        self.chips.iter().flatten().find_map(|c| c.battery())
    }

    /// Whether any present chip owns battery-backed persistent data. Lets the
    /// hosting cartridge route its `.srm` load/save through the chip instead
    /// of its own SRAM buffer.
    pub fn has_battery(&self) -> bool {
        self.chips.iter().flatten().any(|c| c.battery().is_some())
    }

    /// Restore battery data into the first present chip that owns a battery
    /// (from a loaded `.srm` or a savestate's SRAM bytes). No-op when no chip
    /// has one.
    pub fn restore_battery(&mut self, data: &[u8]) {
        for c in self.chips.iter_mut().flatten() {
            if c.battery().is_some() {
                c.restore_battery(data);
                return;
            }
        }
    }
}

/// Builds a concrete chip from a manifest `kind`. The generic layer defines
/// the seam; each console implements it, because the concrete chip types are
/// console-owned; each console's cartridge builder calls it.
pub trait ChipFactory {
    /// Construct the chip named `kind` (e.g. `"sdd1"`), giving it its
    /// `chip_id`, a clone of the cartridge ROM `Arc<[u8]>` (for MMC /
    /// decompression chips), and the initial clock anchor. Returns `None` if
    /// the console does not know the `kind`.
    fn build(
        &self,
        kind: &str,
        chip_id: u8,
        rom: Arc<[u8]>,
        master_clock_hz: u64,
    ) -> Option<Box<dyn Coprocessor>>;
}

/// Where the bytes addressed by a cartridge map slot actually live.
///
/// This is the shared *mapping vocabulary* every console's map plan is
/// built from. `Rom` and `Sram` carry a `base_offset` that is added to
/// the slot-relative target offset before indexing the cartridge's ROM
/// or SRAM buffer. `ChipMmio` dispatches to the coprocessor identified
/// by `chip_id` (the same id a [`ChipSet`] indexes on). `OpenBus` covers
/// reserved or unmapped windows so the bus returns MDR without
/// traversing the cartridge. The map *shape*, how a console's
/// bank:offset windows resolve to one of these targets, stays
/// console-owned; only this target vocabulary is shared.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MapTarget {
    /// Read from the cartridge ROM image starting at `base_offset`.
    Rom { base_offset: u32 },
    /// Access cartridge battery-backed SRAM starting at `base_offset`.
    Sram { base_offset: u32 },
    /// Dispatch to the coprocessor identified by `chip_id`. The `base`
    /// is added to the slot-relative offset to form the chip address.
    ChipMmio { chip_id: u8, base: u32 },
    /// Open bus: reads return MDR, writes are dropped.
    OpenBus,
}

// ============================================================================
// Cartridge Audio Trait
// ============================================================================

/// Cartridge audio output trait
/// For cartridges that generate analog audio (MSU-1, expansion audio chips, etc)
pub trait CartridgeAudio: Send + Sync {
    /// Get audio info
    fn audio_info(&self) -> &CartridgeAudioInfo;

    /// Generate audio samples
    /// Returns number of samples generated (stereo, interleaved: L, R, L, R, ...)
    fn generate_audio(&mut self, output: &mut [f32], sample_rate: u32) -> usize;

    /// Get current volume/gain (0.0 to 1.0)
    fn get_volume(&self) -> f32;

    /// Set volume/gain (0.0 to 1.0)
    fn set_volume(&mut self, volume: f32);

    /// Is audio enabled?
    fn is_enabled(&self) -> bool;

    /// Enable/disable audio
    fn set_enabled(&mut self, enabled: bool);

    /// Get audio debugger (if supported)
    fn audio_debugger(&self) -> Option<&dyn CartridgeAudioDebugger> {
        None
    }

    /// Get audio debugger (mutable)
    fn audio_debugger_mut(&mut self) -> Option<&mut dyn CartridgeAudioDebugger> {
        None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CartridgeAudioInfo {
    /// Audio chip type
    pub chip_type: CartridgeAudioType,

    /// Number of channels (1 = mono, 2 = stereo, etc)
    pub channels: u8,

    /// Native sample rate (if applicable)
    pub native_sample_rate: Option<u32>,

    /// Features
    pub features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CartridgeAudioType {
    // SNES
    MSU1, // SD2SNES/FxPak Pro - CD-quality audio

    // NES expansion audio
    VRC6Audio,
    VRC7Audio,
    MMC5Audio,
    Namco163Audio,
    FME7Audio,

    // Genesis (future)
    GenesisPCM, // Sample playback

    // Custom
    Custom(String),
}

/// Cartridge audio debugger
pub trait CartridgeAudioDebugger {
    /// Get current playback state
    fn get_playback_state(&self) -> AudioPlaybackState;

    /// Get waveform data (for visualization)
    fn get_waveform(&self, samples: usize) -> Vec<f32>;

    /// Get current track info (for MSU-1, etc)
    fn get_track_info(&self) -> Option<AudioTrackInfo>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioPlaybackState {
    pub playing: bool,
    pub position: u64, // In samples
    pub loop_point: Option<u64>,
    pub volume: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioTrackInfo {
    pub track_number: u32,
    pub title: Option<String>,
    pub duration: Option<u64>, // In samples
    pub loop_enabled: bool,
}

// ============================================================================
// Cartridge Debugger Trait
// ============================================================================

/// Cartridge debugging interface
pub trait CartridgeDebugger {
    /// Get current memory map visualization
    fn get_memory_map(&self) -> MemoryMap;

    /// Get ROM region info
    fn get_rom_regions(&self) -> Vec<RomRegionInfo>;

    /// Get SRAM region info
    fn get_sram_regions(&self) -> Vec<SramRegionInfo>;

    /// Get mapper state
    fn get_mapper_state(&self) -> Option<MapperState>;

    /// Get the primary expansion chip's state (the lowest-id chip on a
    /// multi-chip cartridge), or `None` when the cartridge has no chips.
    fn get_chip_state(&self) -> Option<ExpansionChipState>;

    /// Get every expansion chip's state, keyed by `chip_id`. Cartridges that
    /// host more than one coprocessor override this to enumerate them all; the
    /// default wraps [`Self::get_chip_state`] so a single-chip debugger needs
    /// no change and a plain cartridge returns an empty list.
    fn get_chip_states(&self) -> Vec<(u8, ExpansionChipState)> {
        self.get_chip_state()
            .map(|s| vec![(0, s)])
            .unwrap_or_default()
    }

    /// Set breakpoint on ROM address
    fn add_rom_breakpoint(&mut self, addr: u32) -> BreakpointId;

    /// Set watchpoint on SRAM address
    fn add_sram_watchpoint(&mut self, addr: u32, watch_type: WatchType) -> BreakpointId;

    /// Remove breakpoint/watchpoint
    fn remove_breakpoint(&mut self, id: BreakpointId);
}

pub type BreakpointId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WatchType {
    Read,
    Write,
    ReadWrite,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryMap {
    pub regions: Vec<MemoryMapRegion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryMapRegion {
    pub cpu_address_start: u32,
    pub cpu_address_end: u32,
    pub physical_offset: usize,
    pub size: usize,
    pub memory_type: MemoryType,
    pub access: AccessType,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccessType {
    ReadOnly,
    WriteOnly,
    ReadWrite,
    MappedIO,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RomRegionInfo {
    pub start: u32,
    pub end: u32,
    pub size: usize,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SramRegionInfo {
    pub start: u32,
    pub end: u32,
    pub size: usize,
    pub sram_type: SRAMType,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapperState {
    pub mapper_id: MapperId,
    pub registers: Vec<MapperRegister>,
    pub bank_config: BankConfiguration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapperRegister {
    pub address: u32,
    pub name: String,
    pub value: u8,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpansionChipState {
    pub chip_type: ExpansionChipType,
    pub registers: Vec<ChipRegister>,
    pub internal_ram: Option<Vec<u8>>,
    pub program_counter: Option<u32>,
    pub cycle_count: u64,
}
