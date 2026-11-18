//! The machine into and out of its savestate payload, whose layout is
//! `crate::savestate`, and the extension after it; the trait seals and
//! opens them, and hashes them for netplay and the golden records.

use super::frame::LineRun;
use super::GenesisEmulator;
use crate::savestate::{GenesisExtension, GenesisFrameState, VdpBeamState};
use crate::timing::{GenesisTiming, VideoRegion};
use retro_core::cartridge::Cartridge;
use retro_core::EmulatorCore;

/// Self-contained cartridge-only savestate payload: the battery-SRAM image
/// plus every coprocessor's typed state (keyed by `chip_id`). Serialized by
/// [`GenesisEmulator::get_cartridge_save_state`].
type CartridgeSavePayload = (Vec<u8>, Vec<(u8, crate::savestate::GenesisChipState)>);

impl GenesisEmulator {
    /// Make the machine the payload's (the trait's `restore_savestate`):
    /// every part restored, the region the payload's first, so the clocks
    /// the converter and the chips are restored against are the state's.
    pub(super) fn restore_payload(&mut self, state: crate::savestate::SystemSaveState) {
        // The machine becomes the savestate's: a frame in progress is
        // dropped (the extension brings the state's back), and the calls
        // the call stack holds belong to the machine replaced.
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

        // The state's region, before the clocks restored against it.
        self.apply_region(match state.vdp.timing.region {
            0 => VideoRegion::Ntsc,
            _ => VideoRegion::Pal,
        });

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
        if conv_state == &retro_core::audio::RateConverterState::default() {
            self.audio_rate_converter.reset(self.master_cycles);
        } else {
            self.audio_rate_converter = retro_core::IntegerRateConverter::from_state(
                self.audio_sample_rate as u64,
                self.region.cpu_clock(),
                conv_state,
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
        self.host_audio.clear();
        self.host_audio.reset_counters();
    }

    /// What the machine holds that the layout has no field for (the trait's
    /// `savestate_extension`): the VDP's and the sound chips' state within
    /// a frame, a Z80 bus request waiting for its grant, and a frame
    /// stopped part of the way through.
    pub(super) fn capture_extension(&self) -> GenesisExtension {
        GenesisExtension {
            vdp: self.bus.vdp().extension(),
            ym2612: self.bus.ym2612().extension(),
            psg: self.bus.psg().extension(),
            z80_bus_grant: self.bus.z80_bus_grant(self.master_cycles),
            frame: self.frame_open.then(|| self.frame_state()),
        }
    }

    /// The frame in progress: the line it stands on, the line's counters,
    /// the step stopped before its instruction, and the input it started
    /// with. What only a line's start or a step's first part reads again is
    /// left at 0: the counters before the line's start, the interrupt's
    /// cycles of a step not stopped.
    fn frame_state(&self) -> GenesisFrameState {
        let run = if self.line_started {
            self.line
        } else {
            LineRun::default()
        };
        GenesisFrameState {
            line: self.frame_line,
            line_started: self.line_started,
            grid: run.grid,
            budget: run.budget,
            psg_credit: run.psg_credit,
            psg_clocked: run.psg_clocked,
            z80_target: run.z80_target,
            z80_credit: run.z80_credit,
            z80_executed: run.z80_executed,
            ym_clocked: run.ym_clocked,
            stopped_step: run.resume.then_some(self.interrupt_cycles),
            input: self.bus.io().synced_input(),
        }
    }

    /// Where the beam stands once a line has ended, from what the payload
    /// holds: the last line's length (its 68000 budget, which the budget's
    /// carry says) and the cycles the line ran past it.
    fn line_end_beam(&self) -> VdpBeamState {
        let clocks = self.region.master_clocks_per_scanline();
        let divider = self.region.cpu_divider().max(1);
        let base = clocks / divider;
        let length = if self.scanline_cycle_carry < clocks % divider {
            base + 1
        } else {
            base
        };
        VdpBeamState {
            into_line: length + self.frame_cycle_overshoot,
            line_length: length,
        }
    }

    /// Make the machine's state the layout lacks `extension`'s (the trait's
    /// `restore_savestate`, after [`Self::restore_payload`]): every part
    /// from the extension, or at its none when the extension holds none of
    /// it, so a state with nothing after its payload leaves no fill, no
    /// busy time and no frame in progress, whatever the loading machine
    /// had.
    pub(super) fn restore_extension(&mut self, extension: GenesisExtension) {
        let line_end = self.line_end_beam();
        self.bus
            .vdp_mut()
            .restore_extension(extension.vdp, line_end);
        self.bus.ym2612_mut().restore_extension(extension.ym2612);
        self.bus.psg_mut().restore_extension(extension.psg);
        self.bus.restore_z80_bus_grant(extension.z80_bus_grant);
        if let Some(frame) = extension.frame {
            self.frame_open = true;
            self.frame_line = frame.line;
            self.line_started = frame.line_started;
            self.line = LineRun {
                grid: frame.grid,
                budget: frame.budget,
                psg_credit: frame.psg_credit,
                psg_clocked: frame.psg_clocked,
                z80_target: frame.z80_target,
                z80_credit: frame.z80_credit,
                z80_executed: frame.z80_executed,
                ym_clocked: frame.ym_clocked,
                z80_irq: self.z80_irq_pending,
                resume: frame.stopped_step.is_some(),
                ..LineRun::default()
            };
            self.interrupt_cycles = frame.stopped_step.unwrap_or(0);
            self.bus.io_mut().sync_input(frame.input[0], frame.input[1]);
            self.bus
                .vdp_mut()
                .set_frame_being_drawn(self.frame_count + 1);
        }
    }

    /// Build the full machine snapshot shared by [`EmulatorCore::save_state`]
    /// (which wraps it in the cross-core envelope) and
    /// [`EmulatorCore::state_hash`] (which hashes it). `metadata` is supplied
    /// by the caller so a save can stamp the wall clock via
    /// `SaveStateMetadata::now` while the hash passes a canonical
    /// timestamp-free value.
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
                audio_rate_converter: self.audio_rate_converter.state(),
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
