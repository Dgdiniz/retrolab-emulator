//! Core emulator trait that all console emulators must implement.

use crate::audio::{AudioDebugger, HostAudioRing};
use crate::cartridge::{CartridgeDebugger, CartridgeInfo};
use crate::debugger::DebuggerCore;
use crate::input::GenericInputManager;
use crate::savestate::{LoadStateError, PayloadUse, RomCheck, RomIdentity, SavestatePayload};
use crate::types::*;
use crate::video::VideoDebugger;
use serde::{Deserialize, Serialize};

#[cfg(feature = "emulator-conformance")]
pub mod conformance;

/// The extension of a core's savestate payload.
pub type ExtensionOf<E> = <<E as EmulatorCore>::Savestate as SavestatePayload>::Extension;

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
    // ---- the savestate

    /// The payload this console's savestates carry; its bincode layout
    /// never moves.
    type Savestate: SavestatePayload;

    /// The core id written in envelopes and the build id (`"sg1"`).
    const CORE_ID: &'static str = <Self::Savestate as SavestatePayload>::FORMAT.core_id;

    /// Get system information and metadata.
    fn system_info(&self) -> &SystemInfo;

    /// The image hash and title of the cartridge plugged in; `None` with
    /// none.
    fn rom_identity(&self) -> Option<RomIdentity<'_>>;

    /// The machine as a payload. [`PayloadUse::StateHash`] leaves out what
    /// is not machine state (a time stamped, a title).
    fn savestate_payload(&self, purpose: PayloadUse) -> Self::Savestate;

    /// What the machine holds that the payload's layout has no field for;
    /// the default when it holds nothing of the kind.
    fn savestate_extension(&self) -> ExtensionOf<Self>;

    /// Make the machine the state's, its payload and its extension: the
    /// frame in progress the state's (none for a state taken where a frame
    /// ends), the call stacks and the host's queued samples emptied, the
    /// region the state's.
    fn restore_savestate(&mut self, payload: Self::Savestate, extension: ExtensionOf<Self>);

    /// The envelope's `created_at`: 0 unless the console stamps a clock.
    fn savestate_created_at(&self) -> u64 {
        0
    }

    /// Finish a frame held where a savestate cannot hold it (the frame
    /// driver's `finish_held_frame`); returns whether there was one.
    fn finish_held_frame(&mut self) -> bool {
        false
    }

    /// The machine sealed where it stands, after a held frame is finished;
    /// no bytes if it cannot be sealed.
    fn save_state(&mut self) -> Vec<u8> {
        self.finish_held_frame();
        let payload = self.savestate_payload(PayloadUse::Savestate);
        let extension = self.savestate_extension();
        let rom_sha256 = self
            .rom_identity()
            .map(|rom| rom.sha256.to_string())
            .unwrap_or_default();
        payload
            .seal(&extension, &rom_sha256, self.savestate_created_at())
            .unwrap_or_default()
    }

    /// Open a state and make the machine its: another core's, a newer
    /// build's and, with [`RomCheck::SameRom`], another ROM's are refused
    /// with the machine as it was ([`savestate::open`]).
    fn load_state(&mut self, data: &[u8], check: RomCheck) -> Result<(), LoadStateError> {
        let (payload, extension) = Self::Savestate::open(data, self.rom_identity(), check)?;
        self.restore_savestate(payload, extension);
        Ok(())
    }

    /// FNV-1a-64 of the state a savestate holds: two machines in the same
    /// state hash equal, and a save loaded again hashes as it did.
    fn state_hash(&self) -> u64 {
        self.savestate_payload(PayloadUse::StateHash)
            .state_hash(&self.savestate_extension())
    }

    /// The payload's parts hashed by name, then the extension's when the
    /// machine holds one, for finding where two machines differ.
    fn state_hash_parts(&self) -> Vec<(&'static str, u64)> {
        let mut parts = self.savestate_payload(PayloadUse::StateHash).hash_parts();
        if let Some(hash) = Self::Savestate::extension_hash(&self.savestate_extension()) {
            parts.push(("extension", hash));
        }
        parts
    }

    // ---- resets: none changes the debugger's mode

    /// The console's reset button: what its reset line resets. Work RAM,
    /// video memory, the save, the region and the host's settings are
    /// kept.
    fn reset(&mut self);

    /// Every chip as a fresh load of the cartridge plugged in leaves it,
    /// in the region the machine is in, its save RAM blank. The host's
    /// settings and the debugger are kept; the input lets go of nothing.
    fn power_cycle(&mut self);

    /// A power cycle that keeps the battery save: the app's Reset.
    fn hard_reset(&mut self) {
        let save = self.get_save_ram().map(|s| s.to_vec());
        self.power_cycle();
        if let Some(save) = save {
            self.set_save_ram(&save);
        }
    }

    /// Netplay's baseline: a power cycle with nothing of the debugger
    /// armed (no breakpoint, watchpoint or event, every trace off) and
    /// nothing the input holds still pressed.
    fn power_on_reset(&mut self) {
        self.power_cycle();
        let debugger = self.debugger();
        let ids: Vec<_> = debugger.list_breakpoints().iter().map(|b| b.id).collect();
        for id in ids {
            debugger.remove_breakpoint(id);
        }
        for cpu in 0..debugger.traceable_cpu_count() {
            debugger.set_tracing_enabled(cpu, false);
        }
        self.input_manager_mut().release_all();
    }

    // ---- frames: what a host reads of them

    /// Frames finished since the last reset or load.
    fn frame_count(&self) -> u64;

    /// Master clocks run since the last reset or load; a load restores them.
    fn master_cycles(&self) -> u64;

    /// The master clock of the region the machine is in, in Hz.
    fn master_clock_hz(&self) -> u64;

    // ---- region

    /// The region the machine runs in.
    fn region(&self) -> VideoRegion;

    /// The regions the console has.
    fn supported_regions(&self) -> &'static [VideoRegion] {
        &[VideoRegion::Ntsc, VideoRegion::Pal]
    }

    /// Run in `region`: a power cycle in it that keeps the save; the
    /// region the machine is in already changes nothing.
    fn set_region(&mut self, region: VideoRegion) -> Result<(), String> {
        if !self.supported_regions().contains(&region) {
            return Err(
                VideoRegion::parse_among(region.name(), self.supported_regions()).unwrap_err(),
            );
        }
        if region == self.region() {
            return Ok(());
        }
        let save = self.get_save_ram().map(|s| s.to_vec());
        self.apply_region(region);
        self.power_cycle();
        if let Some(save) = save {
            self.set_save_ram(&save);
        }
        Ok(())
    }

    /// Retime the machine for `region` (its clocks, its line count) and
    /// nothing else: [`Self::set_region`]'s power cycle, a ROM's load and a
    /// state's load call it.
    fn apply_region(&mut self, region: VideoRegion);

    /// [`Self::set_region`] by name (`"ntsc"`, `"pal"`, any case).
    fn set_region_str(&mut self, name: &str) -> Result<(), String> {
        let region = VideoRegion::parse_among(name, self.supported_regions())?;
        self.set_region(region)
    }

    /// The region's name.
    fn region_str(&self) -> &'static str {
        self.region().name()
    }

    // ---- the host's audio

    /// The samples made and not yet taken by the host.
    fn host_audio(&self) -> &HostAudioRing;

    /// The samples made and not yet taken by the host.
    fn host_audio_mut(&mut self) -> &mut HostAudioRing;

    /// Every queued sample, interleaved left and right, in [-1.0, 1.0].
    fn get_audio_samples(&mut self) -> &[f32] {
        self.host_audio_mut().drain_all()
    }

    /// Drop sound made but not yet queued for the host (after a rollback's
    /// frames run again).
    fn discard_pending_audio(&mut self) {}

    // ---- input

    /// The host's input: providers, ports and overrides.
    fn input_manager(&self) -> &GenericInputManager;

    /// The host's input: providers, ports and overrides.
    fn input_manager_mut(&mut self) -> &mut GenericInputManager;

    // ---- the debugger's views

    /// The video chip, for the debugger's views.
    fn video_debugger(&mut self) -> &mut dyn VideoDebugger;

    /// Sound chips the debugger opens: 0 the console's main chip.
    fn audio_chip_count(&self) -> usize;

    /// Sound chip `chip`, `None` past [`Self::audio_chip_count`].
    fn audio_debugger(&mut self, chip: usize) -> Option<&mut dyn AudioDebugger>;

    /// The last `samples` of the console's mixed sound, mono, in
    /// [-1.0, 1.0], oldest first, read without taking them from the host's
    /// ring: `samples` values, silence where none was made yet.
    fn output_waveform(&self, samples: u32) -> Vec<f32>;

    /// The cartridge's map, chips and breakpoints; an empty plan with no
    /// cartridge.
    fn cartridge_debugger(&mut self) -> Box<dyn CartridgeDebugger + '_>;

    /// Write `data` from `addr` as the main CPU's stores would; addresses
    /// wrap at 2^32.
    fn write_memory_debug(&mut self, addr: u32, data: &[u8]);

    /// Rows of disassembly around `pc` on CPU `cpu_id`.
    fn disassemble_window(
        &mut self,
        cpu_id: usize,
        pc: u32,
        before_count: usize,
        after_count: usize,
    ) -> Vec<DisasmRow>;

    /// The debugger's mode.
    fn execution_mode(&self) -> ExecutionMode {
        self.debugger_ref().get_execution_mode()
    }

    // ---- running

    /// Step one instruction on the focused CPU: `step_cpu(focused_cpu())`.
    fn step(&mut self) -> StepResult {
        self.step_cpu(self.focused_cpu())
    }

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

    /// A frame with the host's picture and sound left out, for a
    /// rollback's re-simulation: the frame driver's
    /// [`run_frame_headless`](crate::frame::run_frame_headless), the same
    /// machine as a presented frame.
    fn run_frame_headless(&mut self);

    /// Run until VBLANK interrupt.
    fn run_until_vblank(&mut self);

    /// Load a ROM file.
    fn load_rom(&mut self, data: &[u8]) -> Result<(), String>;

    /// Get information about the loaded ROM.
    fn get_rom_info(&self) -> Option<RomInfo>;

    // ---- the cartridge plugged in

    /// The console's cartridge manifest: `cartridge-manifest-v1` with the
    /// console's own keys.
    type Manifest: Serialize;

    /// Read and check a manifest a host stages; the error says what is
    /// wrong with it.
    fn parse_manifest(json: &str) -> Result<Self::Manifest, String>;

    /// The manifest detection gives a file's bytes, with no staged manifest
    /// applied; builds and installs nothing.
    fn detected_manifest(&self, rom: &[u8]) -> Self::Manifest;

    /// The manifest the next [`Self::load_rom`] applies in place of
    /// detection, `None` while detection decides.
    fn cartridge_manifest_override(&self) -> Option<&Self::Manifest>;

    /// Stage a manifest for the next [`Self::load_rom`], or `None` for
    /// detection; the cartridge plugged in now is left as it is.
    fn set_cartridge_manifest_override(&mut self, manifest: Option<Self::Manifest>);

    /// The file the last successful [`Self::load_rom`] received; empty
    /// before one.
    fn last_rom(&self) -> &[u8];

    /// The SHA-256 of that file, in lowercase hex; `None` before a load.
    fn get_cartridge_hash(&self) -> Option<String>;

    /// What the cartridge plugged in declares; `None` with none.
    fn cartridge_info(&self) -> Option<&CartridgeInfo>;

    /// The game wrote its battery save since the flag was last cleared.
    fn is_cartridge_sram_dirty(&self) -> bool;

    /// Clear the battery save's dirty flag.
    fn clear_cartridge_sram_dirty(&mut self);

    /// The chip a refused [`Self::load_rom`] needed and this build has no
    /// module for; `None` on a console whose builds carry every chip.
    fn last_unsupported_chip(&self) -> Option<&'static str> {
        None
    }

    // ---- the picture

    /// Show the lines a console draws past its picture's usual height (a
    /// host's display preference, never part of a savestate); a console
    /// that draws none keeps the default, which changes nothing.
    fn set_show_overscan(&mut self, show: bool) {
        let _ = show;
    }

    /// Whether those lines are shown; `false` on a console that draws none.
    fn show_overscan_enabled(&self) -> bool {
        false
    }

    /// Get the current framebuffer for display.
    fn get_framebuffer(&self) -> &Framebuffer;

    /// Get display information.
    fn get_display_info(&self) -> &DisplayInfo;

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

    /// Get save RAM (battery backup) if present.
    fn get_save_ram(&self) -> Option<&[u8]>;

    /// Set save RAM (battery backup).
    fn set_save_ram(&mut self, data: &[u8]);

    /// Get mutable reference to the debugger.
    fn debugger(&mut self) -> &mut dyn DebuggerCore;

    /// The debugger, to read.
    fn debugger_ref(&self) -> &dyn DebuggerCore;

    /// Write the CPU states the debugger reports
    /// ([`DebuggerCore::get_cpu_states`]) from the CPUs as they are now.
    fn refresh_debugger_state(&mut self);

    /// Set the debugger's execution mode. A pause also writes the CPU
    /// states the debugger reports, wherever the game stopped: a loop that
    /// runs with nothing armed never writes them.
    fn set_execution_mode(&mut self, mode: ExecutionMode) {
        self.debugger().set_execution_mode(mode);
        if mode == ExecutionMode::Paused {
            self.refresh_debugger_state();
        }
    }

    /// A chunk of CPU `cpu_id`'s trace as text, one line per whole entry
    /// ([`crate::debug::trace::trace_to_text`]); `None` for a CPU whose
    /// trace the core does not write as text.
    fn trace_to_text(&mut self, _cpu_id: usize, _data: &[u8]) -> Option<String> {
        None
    }

    // ===== Additional debugging methods that need bus access =====

    /// Read memory for debugging purposes.
    fn read_memory_debug(&mut self, addr: u32, size: u32) -> Vec<u8>;

    /// Disassemble instructions starting at address (main CPU).
    fn disassemble_at(&mut self, addr: u32, count: u32) -> Vec<Instruction>;

    /// Disassemble instructions for a specific CPU at address.
    fn disassemble_cpu(&mut self, cpu_id: usize, addr: u32, count: u32) -> Vec<Instruction>;
}
