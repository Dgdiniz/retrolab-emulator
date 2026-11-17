//! The machine step: one step of the 65C816 and what its time does to the
//! rest of the machine (its bus costs, DMA, NMI and IRQ delivery, the
//! cartridge chips' catch-up), and the bare step register-level tests
//! take.

use retro_core::frame::{Flow, StepHooks, Stepped};
use retro_core::StepResult;
use wdc65816::instructions::FLAG_I;

use super::SnesEmulator;
use crate::debugger::SnesEvent;

impl SnesEmulator {
    /// Mirror the level-triggered IRQ line into the CPU before a step. The
    /// scheduler's `Irq*` handlers raise the line; a `$4211` (TIMEUP) read
    /// inside the previous instruction lowered it again. The CPU's own `I`
    /// flag decides whether a raised line is actually serviced. A
    /// burst-CPU coprocessor (the SA-1, the Super FX or the Cx4) ORs its
    /// own S-CPU-directed IRQ line in on top, gated so only such a cart
    /// pays for the call. Both reads are of state the chips hold: nothing
    /// is caught up.
    #[inline(always)]
    pub(super) fn mirror_irq_line(&mut self) {
        self.cpu.irq_pending = self.bus.system_regs().irq_line();
        if self.bus.cart_runs_burst_cpu() {
            self.cpu.irq_pending |= self.bus.cart_has_irq();
        }
    }

    /// One step of the 65C816, the machine step of the frame driver
    /// (`FrameMachine::machine_step`). What the step does is read from the
    /// CPU before it runs, the IRQ line mirrored once: nothing between
    /// the read and the CPU's step can change the line or `nmi_pending`.
    ///
    /// - A stopped CPU (STP), or one waiting in WAI with no interrupt line
    ///   up, runs nothing: an idle step, with no hook.
    /// - With an NMI pending, or the IRQ line up and the `I` flag clear,
    ///   the CPU vectors to the handler instead of running the
    ///   instruction at its PC, which it runs when the handler returns:
    ///   the step reports the interrupt (`interrupt_taken`) and calls no
    ///   instruction hook.
    /// - Otherwise it runs that instruction, between `before_instruction`
    ///   and `after_instruction`. A CPU woken from WAI by an IRQ the `I`
    ///   flag masks runs it in the same step.
    ///
    /// The S-DSP register writes a mailbox catch-up made during the step
    /// are checked after it, whatever it did.
    #[inline(always)]
    pub(super) fn step_main_cpu<H: StepHooks<Self>>(&mut self, hooks: &mut H) -> Option<Stepped> {
        self.mirror_irq_line();
        let pc24 = ((self.cpu.pbr as u32) << 16) | self.cpu.pc as u32;
        let start = if self.cpu_idles() {
            None
        } else if self.cpu_takes_interrupt() {
            hooks.interrupt_taken(self, pc24);
            None
        } else {
            match hooks.before_instruction(self, pc24) {
                Flow::Run(start) => Some(start),
                Flow::StopBefore => return None,
            }
        };
        let instruction = start.is_some();
        let delta = self.step_cpu_internal(hooks);
        if let Some(start) = start {
            hooks.after_instruction(self, start, delta.min(u32::MAX as u64) as u32);
        }
        hooks.raised(self, Self::drain_apu_dsp_events);
        Some(Stepped {
            elapsed: delta,
            instruction,
        })
    }

    /// Whether the step about to run is idle: the CPU stopped (STP), or
    /// waiting in WAI with no interrupt line up. The IRQ line is mirrored.
    #[inline(always)]
    fn cpu_idles(&self) -> bool {
        let cpu = &self.cpu;
        cpu.stopped || (cpu.waiting && !cpu.nmi_pending && !cpu.irq_pending)
    }

    /// Whether the step about to run takes an interrupt instead of the
    /// instruction at the PC: an NMI pending, or the IRQ line up with the
    /// `I` flag clear. Read after [`Self::cpu_idles`].
    #[inline(always)]
    fn cpu_takes_interrupt(&self) -> bool {
        let cpu = &self.cpu;
        cpu.nmi_pending || (cpu.irq_pending && cpu.p & FLAG_I == 0)
    }

    /// Run one step of the 65C816 and what its time does to the rest of
    /// the machine, the IRQ line already mirrored, returning the master
    /// clocks it consumed (the sum of each memory access's 6/8/12 mclk
    /// cost plus internal cycles at the 6 mclk fast rate, plus any
    /// general-DMA stall).
    pub(super) fn step_cpu_internal<H: StepHooks<Self>>(&mut self, hooks: &mut H) -> u64 {
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
            // each channel it ran, by the channel's B-bus port.
            hooks.raised(self, |m| {
                let mut hit = m.debugger.check_event(SnesEvent::DmaStart);
                for channel in (0..8).filter(|c| dma_mask & (1 << c) != 0) {
                    let port = m.bus.dma().channels[channel].b_bus_port;
                    hit |= m.debugger.check_event(SnesEvent::DmaDone(port));
                }
                hit
            });
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
        // `cpu.irq_pending` before the next step (see `mirror_irq_line`).
        if self.bus.cart_runs_burst_cpu() {
            self.bus.catch_up_coprocessor(self.master_cycles);
        }
        delta
    }

    /// Run one 65C816 step and nothing else: no scheduler event is
    /// handled, no frame closed, and the debugger records and checks
    /// nothing. For looking at one instruction in isolation
    /// (register-level tests); a host steps through
    /// [`retro_core::EmulatorCore::step_cpu`], which the trait's `step`
    /// calls for the focused CPU. From a capture boundary, the events the
    /// frame loop handles before the next instruction come first, so the
    /// instruction runs against the state it would in the loop. As the host
    /// step does, it reports no instruction for an idle step or an
    /// interrupt's entry.
    pub fn step_without_events(&mut self) -> StepResult {
        self.leave_capture_boundary(&mut ());
        self.mirror_irq_line();
        let instruction = !self.cpu_idles() && !self.cpu_takes_interrupt();
        let delta = self.step_cpu_internal(&mut ());
        StepResult {
            cycles: delta.min(u32::MAX as u64) as u32,
            breakpoint_hit: false,
            instruction_executed: instruction,
        }
    }
}
