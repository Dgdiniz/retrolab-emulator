//! The machine into and out of its savestate payload, whose layout is
//! `crate::savestate`, and the state hash netplay and the golden records
//! compare.

#[cfg(doc)]
use retro_core::EmulatorCore;

use super::SnesEmulator;

impl SnesEmulator {
    /// Build the full deterministic machine snapshot shared by
    /// [`EmulatorCore::save_state`] (which wraps it in the cross-core
    /// envelope) and [`Self::state_hash`] (which hashes it). The payload
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

    /// Load a savestate, refusing one captured against another ROM unless
    /// `check` is `RomCheck::AnyRom`. [`EmulatorCore::load_state`] is this
    /// with `RomCheck::SameRom` and its refusal as text; a host that asked
    /// the player first loads a state made on another build of the game
    /// (a translation in progress, say) with `RomCheck::AnyRom`. The
    /// refusal says whether the ROM was the reason.
    pub fn load_state_with_rom_check(
        &mut self,
        data: &[u8],
        check: retro_core::savestate::RomCheck,
    ) -> Result<(), retro_core::savestate::LoadStateError> {
        use crate::savestate::{SnesSaveState, SNES_SAVESTATE_VERSION};
        use retro_core::savestate::{check_rom_hash, deserialize_compressed, LoadStateError};

        let env = deserialize_compressed::<SnesSaveState>(data)?;
        // Refuse a state captured against a different ROM unless the host
        // asked for any ROM (an empty hash in the state is a wildcard,
        // matching the envelope contract).
        check_rom_hash(&env, self.image_hash(), check)?;
        // Guard the envelope's own version too, so a tampered wrapper
        // can't smuggle a future schema past the payload check below.
        if env.core_version > SNES_SAVESTATE_VERSION {
            return Err(LoadStateError::Unreadable(format!(
                "SNES save state core_version {} is newer than supported version {}",
                env.core_version, SNES_SAVESTATE_VERSION
            )));
        }
        let payload = env.payload;
        // Reject a newer payload schema and validate the big-memory
        // lengths before mutating any live state.
        payload.validate()?;

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
        self.audio_ring.clear();
        self.resampler.reset();
        self.apu_drain_buf.clear();
        self.audio_resample_buf.clear();
        self.audio_pending_drain.clear();

        // The restored PPU register file may imply a different active
        // display size (hires / overscan); resync the cached DisplayInfo
        // so the next host poll matches.
        self.sync_display_info();
        Ok(())
    }

    /// Deterministic FNV-1a-64 hash of the full machine state, for
    /// netplay desync detection and the determinism harness. Hashes the
    /// exact payload [`EmulatorCore::save_state`] serializes and
    /// [`EmulatorCore::load_state`] restores (raw bincode, no
    /// compression), so a save/load round-trip is hash-preserving by
    /// construction, two emulators in the same logical state hash equal,
    /// and [`Self::run_frame_headless`] leaves the same hash as a
    /// presented frame.
    pub fn state_hash(&self) -> u64 {
        let bytes = bincode::serialize(&self.capture_save_state()).unwrap_or_default();
        retro_core::fnv1a64(&bytes)
    }

    /// Per-subsystem FNV-1a-64 hashes of the canonical savestate payload,
    /// for netplay desync triage: when two instances (or a save/load
    /// round-trip) disagree on [`Self::state_hash`], this pinpoints which
    /// subsystem's serialized state diverged. `frame_count` is reported
    /// verbatim (not hashed) so a frame-alignment mismatch is obvious.
    pub fn state_hash_parts(&self) -> Vec<(&'static str, u64)> {
        let s = self.capture_save_state();
        let h = |bytes: Vec<u8>| retro_core::fnv1a64(&bytes);
        vec![
            ("cpu", h(bincode::serialize(&s.cpu).unwrap_or_default())),
            (
                "wram",
                h(bincode::serialize(&s.bus.wram).unwrap_or_default()),
            ),
            ("ppu", h(bincode::serialize(&s.bus.ppu).unwrap_or_default())),
            ("apu", h(bincode::serialize(&s.bus.apu).unwrap_or_default())),
            ("dma", h(bincode::serialize(&s.bus.dma).unwrap_or_default())),
            (
                "system_regs",
                h(bincode::serialize(&s.bus.system_regs).unwrap_or_default()),
            ),
            (
                "controllers",
                h(bincode::serialize(&s.bus.controllers).unwrap_or_default()),
            ),
            (
                "sram",
                h(bincode::serialize(&s.bus.sram).unwrap_or_default()),
            ),
            (
                "coprocessors",
                h(bincode::serialize(&s.bus.coprocessors).unwrap_or_default()),
            ),
            ("bus", h(bincode::serialize(&s.bus).unwrap_or_default())),
            (
                "scheduler",
                h(bincode::serialize(&s.scheduler).unwrap_or_default()),
            ),
            (
                "timing",
                h(bincode::serialize(&s.timing).unwrap_or_default()),
            ),
            ("frame_count", s.frame_count),
        ]
    }
}
