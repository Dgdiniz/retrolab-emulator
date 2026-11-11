//! Cartridge abstraction.
//!
//! The SNES bus addresses cartridges through a trait, not a fixed
//! mapper enum. A cartridge implementation owns its ROM, its SRAM,
//! any expansion chips, and, crucially, its own [`MapPlan`]: a
//! list of slots describing exactly which bank-and-offset ranges map
//! to which target. The bus calls [`SnesBus::install_cartridge`] once
//! and from then on every access is dispatched through the trait
//! methods.
//!
//! This module declares:
//!
//! - [`SnesCartridge`]: the trait the bus consumes.
//! - [`MapPlan`], [`MapSlot`], [`MapTarget`], [`BusSpeed`],
//!   [`Resolved`]: the data types every plan is built from.
//!
//! Concrete cartridge implementations (the SMC-stripping
//! `CartridgeBuilder`, header parsing, LoROM/HiROM/ExHiROM preset
//! generators, the manifest format, etc.) live in sibling modules.
//!
//! [`SnesBus::install_cartridge`]: crate::bus::SnesBus::install_cartridge

mod builder;
mod chip_ids;
mod chip_support;
mod composable;
#[cfg(feature = "chip-cx4")]
pub mod cx4;
mod debug;
mod detect;
mod dsp1;
mod dsp1_hle;
mod factory;
mod firmware;
mod header;
mod manifest;
mod map_plan;
mod plain;
mod presets;
#[cfg(feature = "chip-sa1")]
mod sa1;
mod sdd1;
mod spc7110;
pub use spc7110::{fixed_unix_time, set_fixed_unix_time};
#[cfg(feature = "chip-superfx")]
mod superfx;

pub use builder::{detected_manifest, rom_sha256, BuildError, CartridgeBuilder};
pub use chip_ids::{
    CX4_CHIP_ID, CX4_MMIO_BASE, CX4_RAM_BASE, CX4_ROM_BASE, SA1_BWRAM_BASE, SA1_BWRAM_LINEAR_BASE,
    SA1_CHIP_ID, SA1_IRAM_BASE, SA1_MMIO_BASE, SA1_ROM_BASE, SUPERFX_CHIP_ID, SUPERFX_MMIO_BASE,
    SUPERFX_RAM_BASE, SUPERFX_ROM_BASE,
};
pub use chip_support::{
    unsupported_chip, unsupported_preset, CHIP_CX4_BUILT, CHIP_SA1_BUILT, CHIP_SUPERFX_BUILT,
};
pub use composable::ComposableCartridge;
#[cfg(feature = "chip-cx4")]
pub use cx4::Cx4Chip;
pub use debug::SnesCartridgeDebugger;
pub use detect::{
    detect_preset, detect_preset_with_threshold, score_header, DetectedPreset,
    DEFAULT_DETECT_THRESHOLD,
};
pub use dsp1::{
    detect_dsp1_layout, CoprocessorMode, Dsp1Cartridge, Dsp1Chip, Dsp1Layout, DSP1_CHIP_ID,
    NEC_LLE_BUILT,
};
pub use dsp1_hle::Dsp1Hle;
pub use firmware::{ByteOrder, Dsp1Firmware, Dsp1Revision, FirmwareError, FIRMWARE_SIZE};
pub use header::{
    try_parse_header, SnesHeader, SnesVectors, HEADER_CANDIDATES, MAP_MODE_EXHIROM, MAP_MODE_HIROM,
    MAP_MODE_LOROM,
};
pub use manifest::{
    manifest_to_map_plan, parse_manifest, CartridgeManifest, ManifestChip, ManifestControllers,
    ManifestError, ManifestMapping, ManifestRom, ManifestSlot, ManifestTarget,
    SUPPORTED_MANIFEST_VERSION,
};
pub use map_plan::{BusSpeed, MapPlan, MapSlot, MapTarget, Resolved};
pub use plain::PlainCartridge;
pub use presets::{
    cx4_preset, exhirom_preset, hirom_preset, lorom_preset, sa1_preset, sdd1_preset,
    spc7110_preset, superfx_preset, PresetKind,
};
#[cfg(feature = "chip-sa1")]
pub use sa1::Sa1Chip;
pub use sdd1::{Sdd1Chip, SDD1_CHIP_ID, SDD1_REG_WINDOW_BASE};
pub use spc7110::{
    Spc7110Chip, SPC7110_CHIP_ID, SPC7110_DCU_BASE, SPC7110_MCU_BASE, SPC7110_REG_BASE,
    SPC7110_SRAM_BASE,
};
#[cfg(feature = "chip-superfx")]
pub use superfx::SuperFxChip;

/// SNES cartridge contract.
///
/// Implementors own their ROM, their SRAM, and any coprocessor
/// chips. They publish a [`MapPlan`] (consumed once by the bus at
/// install time) and answer per-byte read/write calls scoped to one
/// of the slots in that plan.
///
/// All `read_*` methods take `&self` because ROM and most chip read
/// paths are stateless; SRAM is sometimes battery-backed but a read
/// alone doesn't mutate it. `write_*` methods take `&mut self` so
/// SRAM writes and chip register writes can update internal state.
///
/// Cartridges with no coprocessor chips can ignore the default
/// `read_chip`/`write_chip` implementations, they return open bus
/// and drop writes, mirroring how the bus handles unmapped MMIO.
pub trait SnesCartridge: Send {
    /// The static mapping plan the bus walks at install time.
    fn map_plan(&self) -> &MapPlan;

    /// Read a byte from cartridge ROM at the linear offset computed
    /// by [`MapSlot::compute_target_offset`] plus the slot target's
    /// `base_offset`.
    fn read_rom(&self, offset: u32) -> u8;

    /// Read a byte from cartridge SRAM. Same offset model as
    /// [`SnesCartridge::read_rom`].
    fn read_sram(&self, offset: u32) -> u8;

    /// Write a byte to cartridge SRAM.
    fn write_sram(&mut self, offset: u32, value: u8);

    /// Read from the coprocessor chip identified by `chip_id`.
    ///
    /// `master_cycle` is the absolute master cycle of this access; chips
    /// with their own clock (DSP-1, ...) use it to catch their core up to
    /// the exact instant the CPU touches the register, so the read
    /// observes the chip's latest state. The default impl returns 0 (the
    /// bus folds this into MDR / open bus at the caller site).
    fn read_chip(&mut self, _chip_id: u8, _offset: u32, _master_cycle: u64) -> u8 {
        0
    }

    /// Write to the coprocessor chip identified by `chip_id`.
    ///
    /// `master_cycle` carries the same meaning as in
    /// [`SnesCartridge::read_chip`]. The default impl drops the write so
    /// cartridges without chips don't need any boilerplate.
    fn write_chip(&mut self, _chip_id: u8, _offset: u32, _value: u8, _master_cycle: u64) {}

    /// Advance any coprocessor to `master_cycle` without a register
    /// access. Called once per frame (at the scheduler's `FrameEnd`) so a
    /// chip's clock keeps ticking across frames with no register traffic,
    /// bounding catch-up staleness to one frame. Default: no-op.
    fn catch_up_chip(&mut self, _master_cycle: u64) {}

    /// Reset any coprocessor and rebind its clock to `master_clock_hz`.
    /// Called from `SnesEmulator::reset` (next to the APU re-anchor) so a
    /// chip's lazy-catch-up converter tracks `master_cycles` from 0 again
    /// after the console reset zeroes the master clock. Default: no-op.
    fn reset_chip(&mut self, _master_clock_hz: u64) {}

    /// Passive console-bus observation. The bus forwards a write to a
    /// register a chip *observes* but does not *own* (e.g. the S-DD1 spying
    /// on the DMA registers to reconstruct each channel's transfer
    /// parameters). `addr` is the console-meaningful register address; for
    /// the SNES CpuMmio surface that is the bank-agnostic 16-bit offset
    /// (`$4300-$437F` etc.), so a chip's single observed range matches every
    /// bank mirror. Default: no-op, the vast majority of cartridges observe
    /// nothing, and this keeps the DMA engine free of any cartridge coupling.
    fn observe_bus_write(&mut self, _addr: u32, _value: u8) {}

    /// Does any installed chip register a passive bus observer? Lets the bus
    /// gate its `observe_bus_write` wiring at install time so a plain
    /// cartridge never pays for the per-write forward. Default: false.
    fn observes_bus(&self) -> bool {
        false
    }

    /// Is any installed coprocessor asserting an IRQ toward the S-CPU (the
    /// SA-1's message / CC-DMA IRQ)? The bus ORs this into the S-CPU's IRQ line
    /// each instruction (gated by [`crate::bus::SnesBus::cart_runs_burst_cpu`]
    /// so only a burst-CPU cart pays for it). Default: no chip, no IRQ.
    fn has_irq(&self) -> bool {
        false
    }

    /// Human-readable summary of the installed coprocessor (e.g.
    /// `"DSP-1B (LLE)"`), or `None` for a plain cartridge. Surfaced by
    /// the CLI `info` command. Default: no coprocessor.
    fn coprocessor_label(&self) -> Option<&str> {
        None
    }

    /// Count of CPU accesses to the coprocessor's register window since
    /// the last reset. `0` for a plain cartridge (no chip) or a chip the
    /// game never touches; a positive value proves the running game is
    /// actively driving the coprocessor. Default: no coprocessor.
    fn coprocessor_accesses(&self) -> u64 {
        0
    }

    /// Capture every coprocessor's volatile chip state for a savestate,
    /// keyed by `chip_id`. Empty when the cartridge hosts no chips (or the
    /// only chip is the firmware-less idle stub). Mirrors the APU: only the
    /// core registers + the drifting converter anchor are persisted, never
    /// the firmware ROM. Default: no coprocessors.
    fn coprocessors_savestate(&self) -> Vec<(u8, crate::savestate::CoprocessorState)> {
        Vec::new()
    }

    /// Restore each coprocessor's chip state from a savestate, matching a
    /// blob to its chip by `chip_id` and rebinding every lazy-catch-up
    /// converter to `master_clock_hz` (the region clock, the same value the
    /// APU re-derives its ratio from on restore). A blob whose id has no
    /// matching chip, or whose backend the chip can't consume, is dropped.
    /// Default: no-op.
    fn restore_coprocessors_savestate(
        &mut self,
        _states: &[(u8, crate::savestate::CoprocessorState)],
        _master_clock_hz: u64,
    ) {
    }

    /// Snapshot the primary coprocessor's live state for the debugger / CLI
    /// (`get_chip_state`): chip type, named registers, internal RAM, PC,
    /// and cycle count. `None` for a plain cartridge. Default: no chip.
    fn coprocessor_debug_state(&self) -> Option<retro_core::cartridge::ExpansionChipState> {
        None
    }

    /// Snapshot every coprocessor's live state, keyed by `chip_id`, for the
    /// debugger / CLI. Empty for a plain cartridge. Default: no chips.
    fn coprocessors_debug_state(&self) -> Vec<(u8, retro_core::cartridge::ExpansionChipState)> {
        Vec::new()
    }

    /// Human-readable label of every installed coprocessor, keyed by
    /// `chip_id` (e.g. `[(1, "DSP-1B (LLE)")]`). Empty for a plain
    /// cartridge. Surfaced by the CLI `info` command. Default: no chips.
    fn coprocessor_labels(&self) -> Vec<(u8, String)> {
        Vec::new()
    }

    /// Register snapshot of a hosted steppable secondary CPU (the SA-1's second
    /// 65816, the Super FX's GSU or the Cx4), for the debugger's extra CPU slot.
    /// `None` when no installed chip hosts one, the common case, so a plain
    /// cartridge (or a DSP-1 / S-DD1 / SPC7110 cart) stays a two-CPU console.
    /// Default: no chip.
    fn secondary_cpu_state(&self) -> Option<retro_core::types::CpuState> {
        None
    }

    /// Single-step a hosted steppable secondary CPU by one instruction,
    /// returning its [`StepResult`](retro_core::emulator::StepResult). `None`
    /// when no installed chip hosts one. Default: no chip.
    fn step_secondary_cpu(&mut self) -> Option<retro_core::emulator::StepResult> {
        None
    }

    /// Forward-decode `count` instructions of a hosted steppable secondary CPU
    /// from `addr`, side-effect-free (for the disassembly view). Empty when no
    /// installed chip hosts one. Default: no chip.
    fn disassemble_secondary(
        &self,
        _addr: u32,
        _count: u32,
    ) -> Vec<retro_core::types::Instruction> {
        Vec::new()
    }

    /// Enable or disable instruction tracing of a hosted steppable secondary
    /// CPU. Default: no chip (no-op).
    fn set_secondary_trace_enabled(&mut self, _enabled: bool) {}

    /// Drain a hosted secondary CPU's trace records captured since the last
    /// call, as a byte stream of fixed-size entries the console re-parses into
    /// its own trace ring. Empty when no installed chip hosts one.
    fn take_secondary_trace(&mut self) -> Vec<u8> {
        Vec::new()
    }

    /// Bulk borrow of the cartridge's battery SRAM, for `.srm`
    /// persistence and savestate capture. The default is "no SRAM"
    /// (an empty slice); cartridges with battery RAM override it.
    fn sram(&self) -> &[u8] {
        &[]
    }

    /// Bulk restore of the cartridge's battery SRAM from `data` (a
    /// `.srm` image or a savestate's SRAM bytes). Implementations
    /// size-check `data` against their own SRAM region. The default
    /// drops the data, a cartridge without SRAM has nothing to
    /// restore.
    fn restore_sram(&mut self, _data: &[u8]) {}
}
