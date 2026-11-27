//! # Retro Core
//!
//! Common traits and types for retro console emulators.
//!
//! This crate provides a generic API that allows different console emulators
//! (Genesis, SNES, NES, etc.) to share a common interface for execution,
//! debugging, and display.

pub mod audio;
pub mod capture;
pub mod cartridge;
pub mod cpu;
pub mod debug;
pub mod debugger;
pub mod emulator;
pub mod frame;
pub mod golden;
pub mod hash;
pub mod input;
pub mod input_schedule;
pub mod memory;
pub mod savestate;
pub mod sub_clock;
pub mod testing;
pub mod types;
pub mod video;

// Re-export main types
pub use audio::{AudioRingBuffer, ChipSampleBuffer, HostAudioRing, IntegerRateConverter};
pub use debug::{
    disassemble_window, select_tier, BreakpointList, CoverageBitmap, DisasmDriver, ExecutionTier,
    TierSelectorInputs, TraceStats, TracingState,
};
pub use debugger::DebuggerCore;
pub use emulator::{EmulatorCore, StepResult};
pub use hash::fnv1a64;
pub use input::manager::{GenericInputManager, InputSnapshot as GenericInputSnapshot};
pub use savestate::{
    deserialize_compressed, has_magic, serialize_compressed, LoadStateError, PayloadUse, RomCheck,
    RomIdentity, SaveStateEnvelope, SavestateExtension, SavestateFormat, SavestatePayload,
    RETRO_SAVESTATE_ENVELOPE_VERSION, RETRO_SAVESTATE_MAGIC,
};
pub use sub_clock::SubClock;
pub use types::*;

// Re-export cartridge types
pub use cartridge::{
    BankConfiguration, BankInfo, Cartridge, CartridgeDebugger, CartridgeInfo, ChipFactory,
    ChipRegister, ChipSet, ConsoleSystem, Coprocessor, DetectionSource, ExpansionChipInfo,
    ExpansionChipState, ExpansionChipType, MapTarget, MapperId, MapperInfo, MemoryType, Region,
    SRAMType,
};
