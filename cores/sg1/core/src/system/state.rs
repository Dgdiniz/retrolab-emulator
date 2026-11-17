//! The machine into and out of its savestate payload, whose layout is
//! `crate::savestate`, and the state hash netplay and the golden records
//! compare.

use super::GenesisEmulator;
use retro_core::cartridge::Cartridge;
#[cfg(doc)]
use retro_core::EmulatorCore;

/// Self-contained cartridge-only savestate payload: the battery-SRAM image
/// plus every coprocessor's typed state (keyed by `chip_id`). Serialized by
/// [`GenesisEmulator::get_cartridge_save_state`].
type CartridgeSavePayload = (
    Vec<u8>,
    Vec<(u8, crate::cartridge::GenesisCoprocessorState)>,
);

impl GenesisEmulator {
    /// Deterministic hash of the full machine state, for netplay desync
    /// detection and the determinism harness.
    ///
    /// Hashes the same snapshot [`EmulatorCore::save_state`] serializes
    /// (every field [`EmulatorCore::load_state`] restores), with the
    /// non-deterministic, presentation-only metadata (wall-clock
    /// timestamp, host emulator version, cartridge title, screenshot)
    /// canonicalized away. Two emulators in the same logical state
    /// therefore hash equal even if they were created at different
    /// wall-clock times, and a save/load round-trip is hash-preserving
    /// by construction (the hash covers exactly the restored payload).
    pub fn state_hash(&self) -> u64 {
        use crate::savestate::SaveStateMetadata;

        // Canonical, timestamp-free metadata so the hash is a pure
        // function of deterministic machine state.
        let metadata = SaveStateMetadata {
            game_title: String::new(),
            rom_hash: String::new(),
            timestamp: String::new(),
            slot: 0,
            emulator_version: String::new(),
        };
        let state = self.build_save_state(metadata);
        let bytes = bincode::serialize(&state).unwrap_or_default();
        retro_core::fnv1a64(&bytes)
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
        use crate::savestate::decompress_and_deserialize;
        use crate::savestate::SystemSaveState;
        use retro_core::savestate::{
            check_rom_hash, deserialize_compressed, has_magic, LoadStateError, RomCheck,
        };

        // Two on-disk formats coexist:
        // - Envelope: 4-byte magic ("RLSS") + bincode + zstd, the format
        //   `save_state` writes.
        // - Legacy: bincode + zstd of `SystemSaveState` directly,
        //   with no magic prefix.
        // `has_magic` discriminates with a 4-byte memcmp, cheap and
        // unambiguous because the envelope magic is not a valid zstd
        // frame header.
        let state: SystemSaveState = if has_magic(data) {
            let envelope = deserialize_compressed::<SystemSaveState>(data)?;
            if let Some(cart) = self.bus.cartridge() {
                check_rom_hash(&envelope, &cart.info().rom_hash, check)?;
            }
            envelope.payload
        } else {
            let state = decompress_and_deserialize(data)?;
            // A legacy savestate names its ROM by hash and title; the
            // refusal names the titles, the only names it ever showed.
            if let (RomCheck::SameRom, Some(cart)) = (check, self.bus.cartridge()) {
                if !state.metadata.rom_hash.is_empty()
                    && cart.info().rom_hash != state.metadata.rom_hash
                {
                    return Err(LoadStateError::OtherRom {
                        state_rom: state.metadata.game_title.to_string(),
                        loaded_rom: cart.info().title.to_string(),
                    });
                }
            }
            state
        };

        // The machine becomes the savestate's, which ends a frame: a frame
        // in progress is dropped, and the calls the call stack holds belong
        // to the machine replaced.
        self.drop_frame();
        self.debugger.machine_replaced();

        // Apply state to all components
        self.cpu.from_savestate(&state.cpu68k);
        self.z80.from_savestate(&state.z80);
        self.bus.vdp_mut().from_savestate(&state.vdp);
        self.bus.psg_mut().from_savestate(&state.psg);
        self.bus.ym2612_mut().from_savestate(&state.ym2612);
        self.bus.io_mut().from_savestate(&state.io);
        self.bus.from_savestate(&state.bus);

        // Restore RAM
        if state.ram.len() == 65536 {
            self.bus
                .memory_mut()
                .work_ram
                .as_mut_slice()
                .copy_from_slice(&state.ram);
        }
        if state.z80_ram.len() == 8192 {
            self.bus
                .memory_mut()
                .z80_ram
                .as_mut_slice()
                .copy_from_slice(&state.z80_ram);
        }

        // Restore composable cartridge state: battery SRAM + typed
        // coprocessor states (rebinding chip clocks to the region master
        // clock). The pre-v3 opaque `cartridge` blob is ignored.
        let cart_mclk = self.region.master_clock();
        if let Some(cart) = self.bus.cartridge_mut() {
            if !state.cart_sram.is_empty() {
                cart.restore_sram(&state.cart_sram);
            }
            if !state.cart_coprocessors.is_empty() {
                cart.restore_coprocessors_savestate(&state.cart_coprocessors, cart_mclk);
            }
        }

        // Restore timing
        let t = &state.timing;
        self.frame_count = state.frame_count;
        self.master_cycles = t.master_cycles;
        // Restore the rate converter. If `t.audio_rate_converter` is
        // the default-constructed value (a fresh emulator's, before reset()
        // or set_region anchored it; a savestate from before the field cannot
        // be decoded at all, bincode reads every field), anchor a fresh
        // converter at the current `master_cycles`, equivalent to
        // `reset()` aligned to the restored emulation time, which
        // keeps subsequent `cycles_to_samples` deltas correct.
        // `t.audio_sample_accumulator` is intentionally ignored.
        let conv_state = &t.audio_rate_converter;
        if conv_state == &crate::savestate::SerializableConverterState::default() {
            self.audio_rate_converter.reset(self.master_cycles);
        } else {
            self.audio_rate_converter = retro_core::IntegerRateConverter::with_state(
                self.audio_sample_rate as u64,
                self.region.cpu_clock(),
                conv_state.reference_cycles,
                conv_state.samples_at_reference,
                conv_state.remainder,
            );
        }
        self.audio_enabled = t.audio_enabled;
        // Re-apply the host audio gate to the chips (it is not part of chip state).
        self.bus.ym2612_mut().set_rendering_enabled(t.audio_enabled);
        self.bus.psg_mut().set_rendering_enabled(t.audio_enabled);
        self.last_step_cycles = t.last_step_cycles;
        self.z80_irq_pending = t.z80_irq_pending;
        self.step_z80_target = t.step_z80_target;
        self.step_z80_executed = t.step_z80_executed;
        self.step_z80_credit_numer = t.step_z80_credit_numer;
        self.step_ym_cycles_clocked = t.step_ym_cycles_clocked;
        self.step_master_clocks_accumulated = t.step_master_clocks_accumulated;
        self.scanline_cycle_carry = t.scanline_cycle_carry;
        self.frame_cycle_overshoot = t.frame_cycle_overshoot;
        // Restore the refresh-slot target verbatim so a save/load round-trip
        // is bit-exact (netplay rollback depends on this). At a frame boundary
        // the target normally sits just AHEAD of master_cycles, but it can be
        // up to one long instruction BEHIND (the last instruction overshot it
        // and the next, which re-arms it via `+= 128`, has not run yet).
        // Recomputing it whenever target <= master_cycles would rewrite
        // exactly that legitimate behind-case, making the restored instance
        // charge a different bus-refresh penalty on its first instruction:
        // an immediate desync. `bus_refresh_target` boots at 128 and only ever
        // increments, so it is never 0 in a state this build produced (and a
        // savestate from before the field cannot be decoded: bincode reads every
        // field); only a 0 is re-anchored, to avoid `bus_refresh_penalty()`
        // charging a refresh every step until the stale target crawls back up
        // to master_cycles.
        self.bus_refresh_target = if t.bus_refresh_target == 0 {
            (self.master_cycles / 128) * 128 + 128
        } else {
            t.bus_refresh_target
        };

        // Clear transient buffers
        self.audio_ring.clear();
        self.audio_ring.reset_counters();

        Ok(())
    }

    /// Per-part FNV-1a-64 hashes of the canonical savestate payload, for
    /// netplay desync triage: when two instances (or a save/load
    /// round-trip) disagree on [`Self::state_hash`], this pinpoints which
    /// subsystem's serialized state diverged. Same canonicalization as
    /// `state_hash` (volatile metadata nulled), so the parts are a pure
    /// function of deterministic machine state.
    pub fn state_hash_parts(&self) -> Vec<(&'static str, u64)> {
        use crate::savestate::SaveStateMetadata;
        let s = self.build_save_state(SaveStateMetadata {
            game_title: String::new(),
            rom_hash: String::new(),
            timestamp: String::new(),
            slot: 0,
            emulator_version: String::new(),
        });
        let h = |bytes: Vec<u8>| retro_core::fnv1a64(&bytes);
        vec![
            (
                "cpu68k",
                h(bincode::serialize(&s.cpu68k).unwrap_or_default()),
            ),
            ("z80", h(bincode::serialize(&s.z80).unwrap_or_default())),
            ("vdp", h(bincode::serialize(&s.vdp).unwrap_or_default())),
            ("psg", h(bincode::serialize(&s.psg).unwrap_or_default())),
            (
                "ym2612",
                h(bincode::serialize(&s.ym2612).unwrap_or_default()),
            ),
            ("io", h(bincode::serialize(&s.io).unwrap_or_default())),
            ("bus", h(bincode::serialize(&s.bus).unwrap_or_default())),
            (
                "timing",
                h(bincode::serialize(&s.timing).unwrap_or_default()),
            ),
            ("ram", h(bincode::serialize(&s.ram).unwrap_or_default())),
            (
                "z80_ram",
                h(bincode::serialize(&s.z80_ram).unwrap_or_default()),
            ),
            (
                "cartridge",
                h(bincode::serialize(&s.cartridge).unwrap_or_default()),
            ),
            ("frame_count", s.frame_count),
        ]
    }

    /// Build the full machine snapshot shared by [`EmulatorCore::save_state`]
    /// (which wraps it in the cross-core envelope) and [`Self::state_hash`]
    /// (which hashes it). `metadata` is supplied by the caller so
    /// `save_state` can stamp the wall clock via `SaveStateMetadata::now`
    /// while `state_hash` passes a canonical timestamp-free value.
    pub(super) fn build_save_state(
        &self,
        metadata: crate::savestate::SaveStateMetadata,
    ) -> crate::savestate::SystemSaveState {
        use crate::savestate::*;

        // Composable cartridge state: the battery-SRAM image plus each
        // coprocessor's typed savestate (keyed by chip_id). The legacy opaque
        // `cartridge` blob is retired, always written empty.
        let (cart_sram, cart_coprocessors) = self
            .bus
            .cartridge()
            .map(|c| (c.sram().to_vec(), c.coprocessors_savestate()))
            .unwrap_or_default();

        SystemSaveState {
            version: SAVE_STATE_VERSION,
            metadata,
            cpu68k: self.cpu.to_savestate(),
            z80: self.z80.to_savestate(),
            vdp: self.bus.vdp().to_savestate(),
            psg: self.bus.psg().to_savestate(),
            ym2612: self.bus.ym2612().to_savestate(),
            io: self.bus.io().to_savestate(),
            bus: self.bus.to_savestate(),
            timing: SystemTimingState {
                master_cycles: self.master_cycles,
                // Deprecated f64 accumulator: always written as 0.0;
                // ignored on load. See SystemTimingState docs.
                audio_sample_accumulator: 0.0,
                audio_enabled: self.audio_enabled,
                last_step_cycles: self.last_step_cycles,
                z80_irq_pending: self.z80_irq_pending,
                step_z80_target: self.step_z80_target,
                step_z80_executed: self.step_z80_executed,
                step_z80_credit_numer: self.step_z80_credit_numer,
                step_ym_cycles_clocked: self.step_ym_cycles_clocked,
                step_master_clocks_accumulated: self.step_master_clocks_accumulated,
                audio_rate_converter: SerializableConverterState {
                    reference_cycles: self.audio_rate_converter.reference_cycles(),
                    samples_at_reference: self.audio_rate_converter.samples_at_reference(),
                    remainder: self.audio_rate_converter.remainder(),
                },
                scanline_cycle_carry: self.scanline_cycle_carry,
                frame_cycle_overshoot: self.frame_cycle_overshoot,
                bus_refresh_target: self.bus_refresh_target,
            },
            ram: self.bus.memory().work_ram.as_slice().to_vec(),
            z80_ram: self.bus.memory().z80_ram.as_slice().to_vec(),
            cartridge: Vec::new(),
            cart_sram,
            cart_coprocessors,
            frame_count: self.frame_count,
            screenshot: None,
        }
    }

    // ===== Cartridge Save State Methods =====
    /// Serialize just the cartridge's persistent state, the battery-SRAM
    /// image plus every coprocessor's typed savestate, as a self-contained
    /// bincode blob, independent of the full system savestate. `frame_count`
    /// is accepted for API compatibility and not stored.
    pub fn get_cartridge_save_state(&self, frame_count: u64) -> Result<Vec<u8>, String> {
        let _ = frame_count;
        if let Some(cart) = self.bus.cartridge() {
            let payload: CartridgeSavePayload =
                (cart.sram().to_vec(), cart.coprocessors_savestate());
            bincode::serialize(&payload).map_err(|e| e.to_string())
        } else {
            Err("No cartridge loaded".to_string())
        }
    }

    /// Restore the cartridge-only state produced by
    /// [`get_cartridge_save_state`](Self::get_cartridge_save_state).
    pub fn load_cartridge_save_state(&mut self, data: &[u8]) -> Result<(), String> {
        let (sram, coprocessors): CartridgeSavePayload =
            bincode::deserialize(data).map_err(|e| e.to_string())?;
        let mclk = self.region.master_clock();
        if let Some(cart) = self.bus.cartridge_mut() {
            if !sram.is_empty() {
                cart.restore_sram(&sram);
            }
            cart.restore_coprocessors_savestate(&coprocessors, mclk);
            Ok(())
        } else {
            Err("No cartridge loaded".to_string())
        }
    }
}
