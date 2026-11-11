//! # SNES1 Core
//!
//! Super Nintendo Entertainment System (Super Famicom) emulator core.
//!
//! Wraps a Ricoh 5A22 (WDC 65C816) main CPU and a Sony SPC700 audio
//! CPU in the H-event scheduler exposed by [`retro_core`], and
//! implements the `EmulatorCore` trait so the emulator can be driven
//! from the platform shell, the standalone CLI, or the WASM bindings.

pub mod apu;
pub mod bus;
pub mod cartridge;
pub mod controller;
pub mod coverage;
pub mod debugger;
pub mod dma;
pub mod golden;
pub mod input_schedule;
pub mod irq;
pub mod ppu;
pub mod savestate;
pub mod scheduler;
pub mod system;
pub mod system_regs;
pub mod trace_format;

pub use bus::{IoAccess, PageHandler, SnesBus};
pub use cartridge::{
    detect_dsp1_layout, detect_preset, detect_preset_with_threshold, detected_manifest,
    exhirom_preset, hirom_preset, lorom_preset, manifest_to_map_plan, parse_manifest, rom_sha256,
    score_header, try_parse_header, unsupported_chip, unsupported_preset, BuildError, BusSpeed,
    CartridgeBuilder, CartridgeManifest, CoprocessorMode, DetectedPreset, Dsp1Cartridge,
    Dsp1Layout, ManifestChip, ManifestControllers, ManifestError, ManifestMapping, ManifestRom,
    ManifestSlot, ManifestTarget, MapPlan, MapSlot, MapTarget, PlainCartridge, PresetKind,
    Resolved, SnesCartridge, SnesCartridgeDebugger, SnesHeader, SnesVectors, CHIP_CX4_BUILT,
    CHIP_SA1_BUILT, CHIP_SUPERFX_BUILT, DEFAULT_DETECT_THRESHOLD, DSP1_CHIP_ID, HEADER_CANDIDATES,
    MAP_MODE_EXHIROM, MAP_MODE_HIROM, MAP_MODE_LOROM, NEC_LLE_BUILT, SUPPORTED_MANIFEST_VERSION,
};
pub use controller::{input_state_to_snes_pad, Controller, StandardPad};
#[cfg(feature = "chip-superfx")]
pub use debugger::cpu_state_gsu;
#[cfg(feature = "chip-cx4")]
pub use debugger::cpu_state_hg51b;
pub use debugger::{
    cpu_state_65816, cpu_state_spc700, snes_events, SnesDebugger, SnesEvent, TraceEntryBinary65816,
    TraceEntryBinaryCx4, TraceEntryBinaryGsu, TraceEntryBinarySpc700, TRACE_ENTRY_SIZE_65816,
    TRACE_ENTRY_SIZE_CX4, TRACE_ENTRY_SIZE_GSU, TRACE_ENTRY_SIZE_SPC700,
};
pub use ppu::tile::TilemapEntry;
pub use ppu::{
    window_layer, Ppu, PpuAccessCtx, SpriteDescriptor, TileSetSource, ViewerImage, WindowEnable,
};
pub use savestate::{SnesSaveState, SNES_SAVESTATE_VERSION};
pub use scheduler::{
    schedule_frame_events, EventKind, HEventScheduler, ScheduledEvent, TimingProfile,
};
pub use system::SnesEmulator;
pub use system_regs::{SystemRegWriteFx, SystemRegs};
#[cfg(feature = "chip-cx4")]
pub use trace_format::format_trace_line_cx4;
#[cfg(feature = "chip-superfx")]
pub use trace_format::format_trace_line_gsu;
pub use trace_format::{
    effective_address, format_trace_line_65816, format_trace_line_spc700, EffRegs,
};
