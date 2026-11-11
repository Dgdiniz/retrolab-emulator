//! Composable Genesis cartridge stack.
//!
//! This is the Genesis counterpart of the SNES `cartridge` module: a
//! cartridge is a ROM image plus a [`MapPlan`] plus a
//! [`ChipSet`](retro_core::cartridge::ChipSet) of independent
//! [`Coprocessor`](retro_core::cartridge::Coprocessor)s, instead of a
//! bespoke `Box<dyn Cartridge>` chosen by a `match`. Every cartridge,
//! plain, SEGA/SSF2/Codemasters banked, EEPROM, J-Cart, SVP, presents the
//! same [`GenesisCartridge`] trait to the bus, and every access the bus sends
//! it resolves through the plan, rather than the bus knowing any cartridge
//! type.

pub mod builder;
pub mod cartridge;
pub mod chips;
pub mod debug;
pub mod factory;
pub mod manifest;
pub mod map_plan;
pub mod plain;
pub mod presets;
pub mod savestate;

pub use builder::{build_genesis_cartridge, build_with_manifest, detected_manifest, rom_sha256};
pub use cartridge::ComposableCartridge;
pub use chips::{
    BankMapperChip, EepromChip, JCartChip, SvpCoprocessor, BANK_MAPPER_CHIP_ID, EEPROM_CHIP_ID,
    JCART_CHIP_ID, SVP_CHIP_ID,
};
pub use debug::{describe_map_plan, GenesisCartridgeDebugger, MapRegionSummary};
pub use factory::Sg1ChipFactory;
pub use manifest::{parse_manifest, CartridgeManifest};
pub use map_plan::{MapPlan, MapSlot, MapTarget, Resolved};
pub use plain::PlainCartridge;
pub use presets::PresetKind;
pub use savestate::{
    BankMapperKind, BankMapperState, EepromChipState, EepromPhase, GenesisCoprocessorState,
    JCartState, SvpState,
};

use retro_core::cartridge::{CartridgeAudio, CartridgeInfo, ExpansionChipState};

/// A Genesis cartridge as seen by the bus.
///
/// Address resolution is data-driven: [`map_plan`](Self::map_plan) returns
/// the [`MapPlan`] every access resolves through, to one of the [`MapTarget`]
/// arms, ROM, SRAM, a coprocessor's MMIO, or open bus. The bus never inspects
/// the concrete cartridge type; it calls [`read_bus`](Self::read_bus) /
/// [`write_bus`](Self::write_bus), which resolve the address and call
/// [`read_rom`](Self::read_rom) / [`read_sram`](Self::read_sram) /
/// [`read_chip`](Self::read_chip) with the already-resolved offset.
///
/// This mirrors the SNES `SnesCartridge` trait, adapted to the flat 24-bit
/// 68000 address space. It adds [`info`](Self::info) (the Genesis system
/// reads region/title/checksum off the cartridge) and SRAM dirty tracking
/// (the `.srm` autosave path debounces on it).
///
/// Unmapped reads return `0xFF` (open bus), not the SNES memory-data-register
/// fold.
pub trait GenesisCartridge: Send {
    /// The cartridge's view of the address space. [`read_bus`](Self::read_bus)
    /// and [`write_bus`](Self::write_bus) resolve every access through it.
    fn map_plan(&self) -> &MapPlan;

    /// Cartridge metadata (title, serial, regions, checksums, mapper /
    /// expansion-chip info, ROM hash). The Genesis system reads region and
    /// header fields from here.
    fn info(&self) -> &CartridgeInfo;

    /// Read a ROM byte at an already-resolved, already-masked offset.
    fn read_rom(&self, offset: u32) -> u8;

    /// Read a battery-SRAM byte. Default: open bus (cartridge has no SRAM).
    fn read_sram(&self, offset: u32) -> u8 {
        let _ = offset;
        0xFF
    }

    /// Write a battery-SRAM byte. Default: no SRAM, dropped.
    fn write_sram(&mut self, offset: u32, value: u8) {
        let _ = (offset, value);
    }

    /// Read coprocessor MMIO. `offset` already has the slot base folded in;
    /// `master_cycle` is the absolute master-cycle for lazy catch-up.
    /// Default: no chip at that id -> open bus.
    fn read_chip(&mut self, chip_id: u8, offset: u32, master_cycle: u64) -> u8 {
        let _ = (chip_id, offset, master_cycle);
        0xFF
    }

    /// Write coprocessor MMIO. Default: no chip, dropped.
    fn write_chip(&mut self, chip_id: u8, offset: u32, value: u8, master_cycle: u64) {
        let _ = (chip_id, offset, value, master_cycle);
    }

    /// Advance every chip's clock to `master_cycle` (called once per frame).
    fn catch_up_chip(&mut self, master_cycle: u64) {
        let _ = master_cycle;
    }

    /// Reset every chip, re-anchoring any rate converter to the region clock.
    fn reset_chip(&mut self, master_clock_hz: u64) {
        let _ = master_clock_hz;
    }

    /// Passive bus observation: forward a write on an *observed* range (a
    /// range a chip watches but does not own, e.g. the SEGA mapper's
    /// `0xA130Fx` bank registers). Default: no observers.
    fn observe_bus_write(&mut self, addr: u32, value: u8) {
        let _ = (addr, value);
    }

    /// Whether any chip registered a bus observer (lets the bus skip the
    /// `observe_bus_write` call entirely for plain cartridges).
    fn observes_bus(&self) -> bool {
        false
    }

    /// Whether a chip has a pending IRQ toward the 68000. Default: none.
    fn has_chip_irq(&self) -> bool {
        false
    }

    /// Label of the primary (lowest-id) coprocessor, for the CLI summary.
    fn coprocessor_label(&self) -> Option<&str> {
        None
    }

    /// MMIO access count of the primary coprocessor (proves the game drives it).
    fn coprocessor_accesses(&self) -> u64 {
        0
    }

    /// Typed savestate of every coprocessor, keyed by `chip_id`.
    fn coprocessors_savestate(&self) -> Vec<(u8, GenesisCoprocessorState)> {
        Vec::new()
    }

    /// Restore every coprocessor's typed savestate, rebinding clocks to
    /// `master_clock_hz` and matching each blob to its chip by `chip_id`.
    fn restore_coprocessors_savestate(
        &mut self,
        states: &[(u8, GenesisCoprocessorState)],
        master_clock_hz: u64,
    ) {
        let _ = (states, master_clock_hz);
    }

    /// Live debug snapshot of the primary coprocessor.
    fn coprocessor_debug_state(&self) -> Option<ExpansionChipState> {
        None
    }

    /// Live debug snapshot of every coprocessor, keyed by `chip_id`.
    fn coprocessors_debug_state(&self) -> Vec<(u8, ExpansionChipState)> {
        Vec::new()
    }

    /// Human-readable label of every coprocessor, keyed by `chip_id`.
    fn coprocessor_labels(&self) -> Vec<(u8, String)> {
        Vec::new()
    }

    /// The full battery-SRAM image, for `.srm` persistence. Empty when the
    /// cartridge has no SRAM.
    fn sram(&self) -> &[u8] {
        &[]
    }

    /// Bulk-restore the battery-SRAM image from a loaded `.srm`. Degrades
    /// gracefully on a size mismatch (copies the overlapping prefix).
    fn restore_sram(&mut self, data: &[u8]) {
        let _ = data;
    }

    /// Whether SRAM has been written since the last `.srm` flush.
    fn is_sram_dirty(&self) -> bool {
        false
    }

    /// Clear the SRAM dirty flag (called after a `.srm` flush).
    fn clear_sram_dirty(&mut self) {}

    /// Cartridge analog-audio source, if the cartridge streams audio (the
    /// MSU-1-style seam). Default: silent; no Genesis cartridge drives it.
    fn audio(&self) -> Option<&dyn CartridgeAudio> {
        None
    }

    /// Mutable cartridge-audio source (see [`audio`](Self::audio)).
    fn audio_mut(&mut self) -> Option<&mut dyn CartridgeAudio> {
        None
    }

    /// Hardware reset of the cartridge (chips + latches; ROM/SRAM contents
    /// are unaffected).
    fn reset(&mut self) {}

    /// Read the value a bus-control register (`0xA130F1` /TIME SRAM-enable)
    /// reports. `None` (the default) means the cartridge does not drive that
    /// address, so the bus returns open bus. A cartridge with SRAM returns its
    /// enable bit here; the hook exists because these registers live *outside*
    /// the map plan (page `0xA1`, not the cartridge window).
    fn read_time_register(&self, addr: u32) -> Option<u8> {
        let _ = addr;
        None
    }

    /// Resolve `addr` through the plan and dispatch the read to ROM / SRAM /
    /// chip / open-bus, threading the absolute `master_cycle` into the chip
    /// so a coprocessor can lazily catch up before answering. This is the
    /// bus's per-access read entry point.
    fn read_bus(&mut self, addr: u32, master_cycle: u64) -> u8 {
        let Some(resolved) = self.map_plan().resolve(addr) else {
            return 0xFF;
        };
        // `MapTarget` is `Copy`, so take an owned copy to drop the plan
        // borrow before calling the `&mut self` chip methods.
        let target = self.map_plan().slots[resolved.slot_index as usize].target;
        match target {
            MapTarget::Rom { base_offset } => {
                self.read_rom(base_offset.wrapping_add(resolved.target_offset))
            }
            MapTarget::Sram { base_offset } => {
                self.read_sram(base_offset.wrapping_add(resolved.target_offset))
            }
            MapTarget::ChipMmio { chip_id, base } => self.read_chip(
                chip_id,
                base.wrapping_add(resolved.target_offset),
                master_cycle,
            ),
            MapTarget::OpenBus => 0xFF,
        }
    }

    /// Write counterpart to [`read_bus`](Self::read_bus): fan the write out to
    /// bus observers first (bank registers, the SRAM-enable toggle), then
    /// dispatch to the resolved target (ROM writes are dropped).
    fn write_bus(&mut self, addr: u32, value: u8, master_cycle: u64) {
        if self.observes_bus() {
            self.observe_bus_write(addr, value);
        }
        let Some(resolved) = self.map_plan().resolve(addr) else {
            return;
        };
        let target = self.map_plan().slots[resolved.slot_index as usize].target;
        match target {
            MapTarget::Rom { .. } | MapTarget::OpenBus => {}
            MapTarget::Sram { base_offset } => {
                self.write_sram(base_offset.wrapping_add(resolved.target_offset), value)
            }
            MapTarget::ChipMmio { chip_id, base } => self.write_chip(
                chip_id,
                base.wrapping_add(resolved.target_offset),
                value,
                master_cycle,
            ),
        }
    }

    /// Test/tooling convenience: a whole-cartridge read at `master_cycle = 0`,
    /// for callers that want a read without standing up a bus.
    fn read_addr(&mut self, addr: u32) -> u8 {
        self.read_bus(addr, 0)
    }

    /// Test/tooling convenience write counterpart to
    /// [`read_addr`](Self::read_addr) (at `master_cycle = 0`).
    fn write_addr(&mut self, addr: u32, value: u8) {
        self.write_bus(addr, value, 0)
    }
}
