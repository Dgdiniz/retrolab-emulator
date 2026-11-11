//! # Retro Core
//!
//! Common traits and types for retro console emulators.
//!
//! This crate provides a generic API that allows different console emulators
//! (Genesis, SNES, NES, etc.) to share a common interface for execution,
//! debugging, and display.

pub mod audio;
pub mod cartridge;
pub mod cpu;
pub mod debug;
pub mod debugger;
pub mod emulator;
pub mod golden;
pub mod hash;
pub mod input;
pub mod input_schedule;
pub mod memory;
pub mod savestate;
pub mod types;
pub mod video;

// Re-export main types
pub use audio::{AudioRingBuffer, ChipSampleBuffer, IntegerRateConverter};
pub use debug::{
    disassemble_window, select_tier, BreakpointList, BusWatchpointHook, CoverageBitmap,
    DisasmDriver, ExecutionTier, TierSelectorInputs, TraceStats, TracingState,
};
pub use debugger::DebuggerCore;
pub use emulator::{EmulatorCore, StepResult};
pub use hash::fnv1a64;
pub use input::manager::{GenericInputManager, InputSnapshot as GenericInputSnapshot};
pub use savestate::{
    check_rom_hash, deserialize_compressed, has_magic, serialize_compressed, verify_rom_hash,
    LoadStateError, RomCheck, SaveStateEnvelope, RETRO_SAVESTATE_ENVELOPE_VERSION,
    RETRO_SAVESTATE_MAGIC,
};
pub use types::*;

// Re-export cartridge types
pub use cartridge::{
    BankConfiguration, BankInfo, Cartridge, CartridgeAudio, CartridgeAudioInfo, CartridgeAudioType,
    CartridgeDebugger, CartridgeInfo, ChipFactory, ChipRegister, ChipSet, ConsoleSystem,
    Coprocessor, DetectionSource, ExpansionChipInfo, ExpansionChipState, ExpansionChipType,
    MapTarget, MappedAddress, Mapper, MapperId, MapperInfo, MemoryType, Region, SRAMType,
};
