//! The machine into and out of its savestate payload, whose layout is
//! `crate::savestate`; the trait seals and opens it, and hashes it for
//! netplay and the golden records.

#[cfg(doc)]
use retro_core::EmulatorCore;

use super::SnesEmulator;

impl SnesEmulator {
    /// Build the full deterministic machine snapshot shared by
    /// [`EmulatorCore::save_state`] (which wraps it in the cross-core
    /// envelope) and [`EmulatorCore::state_hash`] (which hashes it). The payload
    /// carries no wall-clock or host metadata (the envelope owns
    /// `created_at` and `rom_hash`), so it is a pure function of
    /// deterministic machine state.
    pub(super) fn capture_save_state(&self) -> crate::savestate::SnesSaveState {
        use crate::savestate::{Cpu65816State, SnesSaveState, SNES_SAVESTATE_VERSION};
        SnesSaveState {
            version: SNES_SAVESTATE_VERSION,
            cpu: Cpu65816State::capture(&self.cpu),
            bus: self.bus.to_savestate(),
            scheduler: crate::scheduler::queue_to_state(&self.scheduler, &self.timing),
            master_cycles: self.master_cycles,
            frame_base_cycle: self.frame_base_cycle,
            frame_count: self.frame_count,
            timing: crate::scheduler::timing_to_state(&self.timing),
            // Kept for the layout; the focus is the host's, not the machine's.
            focused_cpu: 0,
            audio_enabled: self.audio_enabled,
        }
    }

    /// Make the machine the payload's (the trait's `restore_savestate`),
    /// the trait's opener having checked it.
    pub(super) fn restore_payload(&mut self, payload: crate::savestate::SnesSaveState) {
        // Restore only volatile state. The cartridge, its map plan, and
        // the page/speed tables are deliberately preserved, the ROM is
        // already loaded, and `SnesBus::from_savestate` re-syncs the
        // FastROM speeds via `apply_memsel`.
        payload.cpu.restore(&mut self.cpu);
        self.timing = crate::scheduler::timing_from_state(&payload.timing);
        self.bus
            .from_savestate(&payload.bus, self.timing.master_clock_hz as u64);
        crate::scheduler::queue_from_state(&mut self.scheduler, &payload.scheduler);
        self.collect_irq_cycles();
        self.irq_read_early = None;
        self.master_cycles = payload.master_cycles;
        self.frame_base_cycle = payload.frame_base_cycle;
        self.frame_count = payload.frame_count;
        // The PPU dropped its line table with the memory it described; the
        // lines drawn next belong to the frame after the ones finished.
        self.bus.ppu_mut().set_recording_frame(self.frame_count + 1);
        // Saves are taken at frame boundaries, where NMI delivery has
        // already resolved (it arms and fires within a frame), so it is
        // not part of the wire format, just clear the transient state.
        // `SystemRegs::nmi_flag_observed` is likewise transient and
        // re-derived the next vblank.
        self.nmi_deliver_armed = false;
        self.nmi_deliver_countdown = 0;
        // The restored machine is not stopped at a capture boundary: if
        // its frame is in progress, its `VBlankStart` finishes its picture.
        self.held_at_capture_boundary = false;
        self.capture_boundary_requested = false;
        self.audio_enabled = payload.audio_enabled;
        // The calls the debugger saw belong to the machine the state
        // replaced; the history stays, it records what ran.
        self.debugger.machine_replaced();

        // Host-side audio is not in the wire format: clear the ring and
        // reset the resampler so playback resumes from the restored APU
        // without replaying stale samples.
        self.host_audio.clear();
        self.resampler.reset();
        self.apu_drain_buf.clear();
        self.audio_resample_buf.clear();

        // The restored PPU register file may imply a different active
        // display size (hires / overscan); resync the cached DisplayInfo
        // so the next host poll matches.
        self.sync_display_info();
    }
}
