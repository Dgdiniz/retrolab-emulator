//! The machine step: one 65C816 instruction and what its time does to the
//! rest of the machine (its bus costs, DMA, NMI and IRQ delivery, the
//! cartridge chips' catch-up), and the step a host asks for.

use retro_core::*;

use super::SnesEmulator;
use crate::debugger::{SnesEvent, MAIN_CPU};

impl SnesEmulator {
    /// Run one 65C816 instruction, returning the master clocks it
    /// consumed (the sum of each memory access's 6/8/12 mclk cost plus
    /// internal cycles at the 6 mclk fast rate, plus any general-DMA
    /// stall).
    pub(super) fn step_cpu_internal(&mut self) -> u64 {
        // Mirror the level-triggered IRQ line into the CPU before the
        // instruction. The scheduler's `Irq*` handlers raise the line;
        // a `$4211` (TIMEUP) read inside the previous instruction
        // lowered it again. The CPU's own `I` flag decides whether a
        // raised line is actually serviced. A burst-CPU coprocessor (the
        // SA-1, the Super FX or the Cx4) ORs its own S-CPU-directed IRQ
        // line in on top, gated so only such a cart pays for the call.
        self.cpu.irq_pending = self.bus.system_regs().irq_line();
        if self.bus.cart_runs_burst_cpu() {
            self.cpu.irq_pending |= self.bus.cart_has_irq();
        }

        // VBlank NMI delivery is decided *after* the instruction runs (see
        // below), so a `$4210` poll the instruction performs is observed
        // before the NMI is vectored.

        // Diagnostic traces: stamp the PC of the instruction about to
        // run so any watched synchronisation-register access it makes is
        // attributed to it, and feed the PC ring buffer. Gated on a
        // trace being enabled, so the fast path never computes or stores
        // it.
        if self.bus.diag_trace_enabled() {
            let pc24 = ((self.cpu.pbr as u32) << 16) | self.cpu.pc as u32;
            self.bus.set_trace_pc(pc24);
        }

        // Charge the instruction its real per-access master-cycle cost.
        // The bus accumulates 6/8/12 mclk for each memory access the CPU
        // makes (via `cycles_for`); the remaining `cpu_cycles - accesses`
        // are internal (non-bus) cycles, each 6 mclk on hardware. A flat
        // `cpu_cycles * 6` would assume every cycle is a FastROM access
        // and run the CPU ~6-7% too fast in master-cycle terms, enough to
        // put the 5A22 and the SPC700 in the wrong relative phase and
        // desync the `$2140` mailbox handshake. `saturating_sub`
        // guards the unexpected case of fewer reported cycles than
        // accesses.
        //
        // A `$4211` read inside the instruction sees an IRQ that rises
        // before it, so the bus is told where the next one falls.
        self.bus
            .set_next_irq_cycle(self.irq_cycles.front().copied());
        self.bus.begin_cpu_instruction();
        let cpu_cycles = self.cpu.step(&mut self.bus);
        let (access_mclks, accesses) = self.bus.take_cpu_instruction_cost();
        if let Some(at) = self.bus.take_irq_read_early() {
            self.irq_read_early = Some(at);
        }
        let internal = (cpu_cycles as u64).saturating_sub(accesses as u64);
        let mut delta = access_mclks + internal * 6;
        // General DMA is deferred to the instruction boundary: a `$420B`
        // write only flagged a pending channel mask. Run the whole block
        // now and fold the stolen master-cycles into this step's cost so
        // the scheduler timeline and the frame budget both observe the
        // CPU stall.
        let dma_mask = self.bus.take_pending_general_dma();
        if dma_mask != 0 {
            delta += self.bus.run_general_dma(dma_mask);
            // A general-DMA block just ran: match a `DmaStart` event
            // breakpoint for the whole `$420B` write, and a `DmaDone` for
            // each channel it ran, by the channel's B-bus port (only armed
            // when one exists).
            if self.debug_armed {
                if self.debugger.check_event(SnesEvent::DmaStart) {
                    self.pending_break = true;
                }
                for channel in (0..8).filter(|c| dma_mask & (1 << c) != 0) {
                    let port = self.bus.dma().channels[channel].b_bus_port;
                    if self.debugger.check_event(SnesEvent::DmaDone(port)) {
                        self.pending_break = true;
                    }
                }
            }
        }
        // A `$4200` NMI-enable 0->1 edge while the `$4210` flag is still
        // latched re-triggers NMI. The bus flagged it because it cannot
        // reach the CPU from inside the store that set it.
        if self.bus.take_pending_nmi() {
            self.cpu.nmi_pending = true;
        }
        // VBlank NMI delivery (poll-aware): once armed at `VBlankStart`,
        // hold off vectoring until the main thread has *observed* the
        // `$4210` flag via a read, or the fallback window expires. This
        // lets a `lda $4210 / bpl` vblank-wait see bit 7 before the
        // handler's own `$4210` ack clears it, regardless of which
        // instruction was in flight when VBlank began. The handler
        // preserves A/X/Y, so vectoring right after the poll's read is
        // correct, its `BPL` tests the restored accumulator and exits.
        if self.nmi_deliver_armed {
            let observed = self.bus.system_regs_mut().take_nmi_flag_observed();
            self.nmi_deliver_countdown = self.nmi_deliver_countdown.saturating_sub(1);
            if observed || self.nmi_deliver_countdown == 0 {
                self.cpu.nmi_pending = true;
                self.nmi_deliver_armed = false;
            }
        }
        // A `$4200`/`$4207-$420A` write changed the IRQ configuration:
        // drop the stale H/V IRQ events and re-arm them from the current
        // frame base, keeping only the still-future triggers.
        if self.bus.take_pending_irq_reschedule() {
            self.reschedule_irq_events();
        }
        self.master_cycles += delta;
        // SystemRegs::tick takes u32 and saturates internally to 255,
        // a single instruction's master-cycle cost never approaches u32
        // anyway.
        self.bus
            .system_regs_mut()
            .tick(delta.min(u32::MAX as u64) as u32);
        // Refresh the bus's raster snapshot so the next PPU access
        // (OAM/CGRAM active-scan gate, $2137 SLHV latch, WRIO
        // falling-edge latch) sees an up-to-1-instruction-stale
        // `(h_dot, v_scanline)`. Decoded against `frame_base_cycle`
        // (see `raster_position`) so the `$213D` value a game latches
        // agrees with the line its scheduled NMI/IRQ events fire on;
        // the bus expects the pair in `(h, v)` order.
        let (v, h) = self.raster_position(self.master_cycles);
        self.bus.set_raster_snapshot(h, v);
        // The APU is caught up lazily on a mailbox access; hand it the
        // instruction boundary it should advance to, mirroring the
        // raster snapshot above. Staleness is bounded by one
        // instruction.
        self.bus.set_apu_cycle(self.master_cycles);
        // A burst-driven coprocessor CPU (the SA-1's second 65816, the Super FX's
        // GSU or the Cx4) must advance every instruction, not just at `FrameEnd`,
        // so it keeps step with the S-CPU across code that never touches shared
        // memory. Gated so plain / DSP-1 / S-DD1 / SPC7110 carts keep their exact
        // current cadence. The chip's S-CPU-directed IRQ line is ORed into
        // `cpu.irq_pending` at the head of the next instruction (see
        // `step_cpu_internal`).
        if self.bus.cart_runs_burst_cpu() {
            self.bus.catch_up_coprocessor(self.master_cycles);
        }
        delta
    }

    /// Run one 65C816 instruction and nothing else: no scheduler event is
    /// handled, no frame closed, and the debugger records nothing. For
    /// looking at one instruction in isolation (register-level tests); a
    /// host steps through [`EmulatorCore::step_cpu`], which the trait's
    /// `step` calls for the focused CPU. From a capture boundary, the
    /// events the frame loop handles before the next instruction come
    /// first, so the instruction runs against the state it would in the
    /// loop.
    pub fn step_without_events(&mut self) -> StepResult {
        self.leave_capture_boundary();
        let delta = self.step_cpu_internal();
        StepResult {
            cycles: delta.min(u32::MAX as u64) as u32,
            breakpoint_hit: false,
            instruction_executed: true,
        }
    }

    /// Advance exactly one 65C816 instruction with full instrumentation
    /// (coverage, trace, call stack, history, state refresh), draining any
    /// scheduler events that fall at the new master-cycle boundary so the
    /// PPU stays consistent. The step a host asks for
    /// ([`EmulatorCore::step_cpu`] on the 65C816) and the step modes both
    /// run it. The history records the instruction at the PC as the step
    /// starts. The step runs the instruction whatever execute breakpoint
    /// sits at it, so the next execute check is made afresh.
    pub(super) fn debug_single_step_65816(&mut self) -> StepResult {
        self.enter_frame_for_step();
        let pc24 = ((self.cpu.pbr as u32) << 16) | self.cpu.pc as u32;
        self.debugger.record_coverage(MAIN_CPU, pc24);
        if self.debugger.tracing(MAIN_CPU) {
            let entry = self.capture_trace_65816();
            self.debugger.record_trace(MAIN_CPU, &entry);
        }
        let history = self.history_entry_65816(pc24);
        self.debugger.record_history(MAIN_CPU, history);
        // Drop what was recorded between two instructions so the
        // watchpoint scan sees only this instruction.
        if self.debug_armed {
            self.bus.clear_debug_events();
        }
        let start = self.step_start_65816(pc24);
        let delta = self.step_cpu_internal();
        self.maintain_call_stack_65816(start);
        self.debugger.forget_checked(MAIN_CPU);
        self.drain_events_and_maybe_close_frame();
        // A single step always pauses, so the hit result isn't acted on,
        // but the recorders are still drained to keep watchpoint/event hit
        // counts accurate and to clear the buffers for the next step.
        if self.debug_armed {
            let _ = self.drain_bus_debug_events();
            let _ = self.drain_apu_dsp_events();
            self.pending_break = false;
        }
        self.drain_spc_trace();
        self.drain_cartridge_trace();
        self.refresh_debugger_state();
        StepResult {
            cycles: delta.min(u32::MAX as u64) as u32,
            breakpoint_hit: false,
            instruction_executed: true,
        }
    }

    /// Before a debugger step: leave the capture boundary if the core is
    /// held there (see [`Self::leave_capture_boundary`]), then go on with
    /// the frame in progress or open the next one, as a frame call does.
    ///
    /// Opening the next frame here matters after a frame call that ran its
    /// frame to the end: the frame base would otherwise still be that
    /// frame's, so the first step would close a frame at once and arm a
    /// second set of events over the ones already armed.
    fn enter_frame_for_step(&mut self) {
        self.leave_capture_boundary();
        self.enter_frame();
    }
}
