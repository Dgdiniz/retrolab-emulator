//! Core emulator trait that all console emulators must implement.

use crate::debugger::DebuggerCore;
use crate::types::*;
use serde::{Deserialize, Serialize};

/// Result of stepping one instruction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub cycles: u32,
    pub breakpoint_hit: bool,
    /// True if an actual instruction was executed; false for STOP idle cycles
    pub instruction_executed: bool,
}

/// Main trait that all emulator cores must implement.
///
/// This provides a unified interface for:
/// - Loading and running ROMs
/// - Execution control (step, run frame, etc.)
/// - Display and audio output
/// - Input handling
/// - Save states and battery saves
/// - Debugging
pub trait EmulatorCore {
    /// Get system information and metadata.
    fn system_info(&self) -> &SystemInfo;

    /// Reset the emulator to initial state.
    fn reset(&mut self);

    /// Step one instruction on the focused CPU.
    /// Delegates to `step_cpu(focused_cpu())`.
    fn step(&mut self) -> StepResult;

    /// Step one instruction on a specific CPU.
    /// cpu_id 0 = main CPU, 1+ = secondary CPUs.
    /// When stepping a secondary CPU, other CPUs are frozen.
    fn step_cpu(&mut self, cpu_id: usize) -> StepResult;

    /// Get the currently focused CPU for debugging.
    /// The focused CPU is the one shown in the debugger UI
    /// and targeted by step()/disassemble_at()/trace.
    fn focused_cpu(&self) -> usize;

    /// Set the focused CPU for debugging.
    fn set_focused_cpu(&mut self, cpu_id: usize);

    /// Run until the end of the current frame (VBLANK).
    fn run_frame(&mut self);

    /// Run until VBLANK interrupt.
    fn run_until_vblank(&mut self);

    /// Load a ROM file.
    fn load_rom(&mut self, data: &[u8]) -> Result<(), String>;

    /// Get information about the loaded ROM.
    fn get_rom_info(&self) -> Option<RomInfo>;

    /// Get the current framebuffer for display.
    fn get_framebuffer(&self) -> &Framebuffer;

    /// Get display information.
    fn get_display_info(&self) -> &DisplayInfo;

    /// Get audio samples generated since last call.
    /// Returns a slice of f32 samples in range [-1.0, 1.0].
    /// Returns empty slice if audio is disabled.
    fn get_audio_samples(&mut self) -> &[f32];

    /// Get audio information.
    fn get_audio_info(&self) -> &AudioInfo;

    /// Enable or disable audio generation.
    /// When disabled, audio chips still respond to register reads/writes
    /// (games depend on this), but no samples are generated.
    /// Default: disabled (false) to focus on video/CPU testing.
    fn set_audio_enabled(&mut self, enabled: bool);

    /// Check if audio generation is enabled.
    fn is_audio_enabled(&self) -> bool;

    /// Set input state for a player.
    fn set_input(&mut self, player: u8, input: InputState);

    /// Get input device information.
    fn get_input_info(&self) -> &InputInfo;

    /// Save complete emulator state.
    fn save_state(&self) -> Vec<u8>;

    /// Load emulator state.
    fn load_state(&mut self, data: &[u8]) -> Result<(), String>;

    /// Get save RAM (battery backup) if present.
    fn get_save_ram(&self) -> Option<&[u8]>;

    /// Set save RAM (battery backup).
    fn set_save_ram(&mut self, data: &[u8]);

    /// Get mutable reference to the debugger.
    fn debugger(&mut self) -> &mut dyn DebuggerCore;

    // ===== Additional debugging methods that need bus access =====

    /// Read memory for debugging purposes.
    fn read_memory_debug(&mut self, addr: u32, size: u32) -> Vec<u8>;

    /// Disassemble instructions starting at address (main CPU).
    fn disassemble_at(&mut self, addr: u32, count: u32) -> Vec<Instruction>;

    /// Disassemble instructions for a specific CPU at address.
    fn disassemble_cpu(&mut self, cpu_id: usize, addr: u32, count: u32) -> Vec<Instruction>;
}
