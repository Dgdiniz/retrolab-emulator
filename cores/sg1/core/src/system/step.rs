//! The machine step: one 68000 instruction and what its time does to the
//! rest of the machine (the Z80, the YM2612, the PSG, and the VDP's FIFO,
//! transfers and vertical interrupt delay), for every tier of the frame
//! driver and the step a host asks for, and the Z80's own step.

use super::frame::LineRun;
use super::GenesisEmulator;
use crate::debugger::{record_z80_trace, SOUND_CPU};
use crate::timing::GenesisTiming;
use retro_core::cpu::Clockable;
use retro_core::frame::{Flow, StepHooks, Stepped};

use retro_core::*;

impl GenesisEmulator {
    /// Step the Z80 CPU one instruction (debug path, 68K frozen). A frame in
    /// progress gives the Z80 its line's whole share when it goes on,
    /// whatever was stepped here.
    pub(super) fn step_z80(&mut self) -> StepResult {
        self.release_z80_reset();

        // Can't step if Z80 is in reset or bus is requested by 68K
        if self.bus.is_z80_reset() || self.bus.is_z80_bus_requested() {
            self.refresh_debugger_state();
            return StepResult {
                cycles: 0,
                breakpoint_hit: false,
                instruction_executed: false,
            };
        }

        // Capture pre-execution state for trace and history
        let pre_pc = self.z80.pc;
        let z80_tracing = self.debugger.tracing(SOUND_CPU);
        let z80_history = self.debugger.is_history_enabled(SOUND_CPU);
        let scanline = self.bus.vdp().timing().scanline;

        // Check for interrupts before stepping
        let z80_irq = self.z80_irq_pending;
        let mut z80_bus = self.bus.create_z80_bus();
        let (processed, irq_cycles) = self.z80.check_interrupts(&mut z80_bus, z80_irq, false);
        let mut total_cycles = 0u32;
        if processed {
            self.z80_irq_pending = false;
            total_cycles += irq_cycles;
        }

        // The instruction's bytes at PC (up to 4), read without side effects
        // before the step, and disassembled for the history while it is on.
        let raw_bytes = z80_bus.instruction_bytes(pre_pc);
        let disasm_text =
            z80_history.then(|| crate::z80::disasm::disassemble_from_bytes(pre_pc, &raw_bytes).0);

        // Record Z80 trace BEFORE step (captures pre-execution state, as a
        // frame's Z80 runs record it)
        if z80_tracing {
            record_z80_trace(&mut self.debugger, &self.z80, scanline, raw_bytes);
        }

        // Coverage: mark the Z80 PC we are about to execute from.
        self.debugger.record_coverage(SOUND_CPU, u32::from(pre_pc));

        // Execute one Z80 instruction
        let cycles = self.z80.step(&mut z80_bus);
        total_cycles += cycles;

        // Clock YM2612 and PSG proportionally
        let ym_cycles = total_cycles * 15 / 7;
        z80_bus.clock_ym2612(ym_cycles);
        z80_bus.clock_psg(total_cycles);

        // Record Z80 instruction history using pre-execution PC and disassembly
        if let Some(instruction) = disasm_text {
            self.debugger.record_history(
                SOUND_CPU,
                InstructionTrace {
                    address: u32::from(pre_pc),
                    instruction,
                    registers_before: Vec::new(),
                    cycle_count: total_cycles,
                },
            );
        }

        // A breakpoint where the Z80 goes next stops this step: the check its
        // own step makes, the only one any Z80 breakpoint gets.
        let bp_hit = self
            .debugger
            .check_execute_next(SOUND_CPU, u32::from(self.z80.pc), None);
        self.refresh_debugger_state();

        StepResult {
            cycles: total_cycles,
            breakpoint_hit: bp_hit,
            instruction_executed: true,
        }
    }
}

impl GenesisEmulator {
    /// Refresh the 68K pending interrupt level from the current VDP state.
    ///
    /// Refresh the VINT interrupt level from VDP state (level-triggered).
    ///
    /// The Genesis VDP's /INT output is level-triggered:
    ///   /INT_ACTIVE = F_FLAG (vblank_pending) AND VINT_ENABLE (register 1 bit 5)
    ///
    /// The F flag is cleared by IACK (interrupt acknowledge) when the 68K takes
    /// the VINT, NOT by reading the VDP status register (MAME and Genesis Plus
    /// GX agree).
    pub(super) fn refresh_vdp_interrupt_level(&mut self) {
        let vblank_pending = self.bus.vdp().timing().vblank_pending;
        let vint_enabled = (self.bus.vdp().registers()[1] & 0x20) != 0;
        // The F flag may be set while the 68K IRQ request is still being held
        // off part-way into line 224; don't assert level 6
        // until that delay has elapsed.
        let vint_active = vblank_pending && vint_enabled && !self.bus.vdp().vint_request_delayed();
        if vint_active {
            self.cpu.pending_irq_level = self.cpu.pending_irq_level.max(6);
        } else if self.cpu.pending_irq_level == 6 {
            self.cpu.pending_irq_level = 0;
        }
    }

    /// Handle interrupt acknowledge (IACK) after the 68K takes an interrupt.
    /// Clears the appropriate VDP flags based on the acknowledged interrupt level.
    /// (MAME and Genesis Plus GX clear them at the same point.)
    pub(super) fn handle_interrupt_ack(&mut self, level: u8) {
        if level >= 6 {
            self.bus.vdp_mut().ack_vint_iack();
        }
    }

    /// One step of the 68000 and what its time does to the rest of the
    /// machine, for every tier of the frame driver and the step a host asks
    /// for (`retro_core::frame::FrameMachine::machine_step`).
    ///
    /// In its order: the VDP's FIFO drained by the previous step's time and
    /// the pending interrupt taken (once an instruction: a step stopped in
    /// front of its instruction goes on from after them); the bus's counters
    /// cleared and the clock stamped; the instruction, with the hooks around
    /// it, or a stopped 68000's idle step (4 cycles, nothing run, no hook);
    /// the vertical interrupt a register write re-asserted, the FIFO's and
    /// the Z80 bus's waits, the DRAM refresh, the step's total cycles
    /// (the interrupt's among them, once) on the clock, the transfers and
    /// the vertical interrupt's delay counted down by them; then the step's
    /// share of the line for the PSG, the beam, the Z80 and the YM2612.
    ///
    /// Returns `None` when an execute breakpoint stopped it in front of its
    /// instruction.
    #[inline(always)]
    pub(super) fn step_main_cpu<H: StepHooks<Self>>(
        &mut self,
        run: &mut LineRun,
        hooks: &mut H,
    ) -> Option<Stepped> {
        if !run.has_rom {
            return Some(Stepped {
                elapsed: 0,
                instruction: false,
            });
        }

        if !run.resume {
            // Tick VDP FIFO drain based on previous step's cycles
            if self.last_step_cycles > 0 {
                let elapsed_mclks =
                    u64::from(self.last_step_cycles) * u64::from(self.region.cpu_divider());
                self.bus.vdp_mut().tick_fifo(elapsed_mclks);
            }

            // Process pending interrupts between instructions (68000 checks
            // IPL at every instruction boundary, not just at scanline
            // boundaries). This is critical for instructions like
            // ANDI/ORI/EORI to SR and RTE that change the interrupt mask
            // mid-scanline. The interrupt's cycles join the step's total.
            self.refresh_vdp_interrupt_level();
            let interrupted = self.cpu.pc;
            let level = self.cpu.pending_irq_level;
            let irq_cycles = self.cpu.process_pending_interrupts(&mut self.bus);
            self.interrupt_cycles = irq_cycles;
            if irq_cycles > 0 {
                self.handle_interrupt_ack(level);
                hooks.interrupt_taken(self, interrupted);
                hooks.raised(self, move |m| m.interrupt_event_hit(level));
            }
        }
        run.resume = false;

        // Reset bus cycles and VDP wait state
        self.bus.cycles_executed = 0;
        self.bus.vdp_wait_cycles = 0;

        // Set YM2612 CPU cycle for timestamp-based busy flag. The stamp is
        // the time before the instruction, its interrupt not counted yet.
        self.bus.ym2612_mut().current_cpu_cycle = self.master_cycles;
        self.bus.m68k_cycle = self.master_cycles;

        // A taken interrupt wakes the CPU, so a stopped one took none here:
        // it idles, running no instruction.
        let instruction = !self.cpu.stopped;
        let cycles = if instruction {
            let pc = self.cpu.pc;
            let start = match hooks.before_instruction(self, pc) {
                Flow::Run(start) => start,
                Flow::StopBefore => {
                    run.resume = true;
                    return None;
                }
            };
            hooks.chip_start("68000");
            let cycles = self.cpu.step(&mut self.bus);
            hooks.chip_finish(1);
            hooks.after_instruction(self, start, cycles);
            cycles
        } else {
            self.cpu.step(&mut self.bus)
        };

        // Check if a VDP register write caused VINT re-assertion
        if self.bus.vdp().should_trigger_vint() {
            let vint_enabled = (self.bus.vdp().registers()[1] & 0x20) != 0;
            if vint_enabled && !self.bus.vdp().vint_request_delayed() {
                self.cpu.request_interrupt(6);
            }
            self.bus.vdp_mut().ack_vint();
        }

        // Add VDP FIFO wait cycles
        let fifo_wait = self.bus.vdp_wait_cycles;
        self.bus.vdp_wait_cycles = 0;

        // Add 68K stall cycles for Z80-bus ($A00000-$A0FFFF) accesses this step.
        let z80_wait = self.bus.z80_bus_wait_cycles;
        self.bus.z80_bus_wait_cycles = 0;

        let refresh = self.bus_refresh_penalty();
        let total = self.interrupt_cycles + cycles + fifo_wait + refresh + z80_wait;
        self.master_cycles += u64::from(total);

        // Accumulate master clocks for VDP timing
        let total_mclks = u64::from(total) * u64::from(self.region.cpu_divider());
        self.step_master_clocks_accumulated += total_mclks;

        // Update DMA progress
        self.bus.vdp_mut().tick_dma(total_mclks);
        // Advance the deferred-VINT countdown (the request lands part-way into the line)
        self.bus.vdp_mut().tick_vint_delay(total_mclks);

        // Save total cycles for FIFO drain timing in next step
        self.last_step_cycles = total;
        hooks.raised(self, Self::video_event_hit);

        // Spread the PSG's per-scanline budget across the line so register
        // writes resolve to ~1 output sample via Psg::catch_up instead of
        // collapsing to the line end. The PSG clocks at master/15 regardless
        // of the Z80 bus, so spend 7 numerator per 68K (master/7) cycle and
        // carry the /15 remainder within the line.
        run.psg_credit += total * 7;
        let psg_step = run.psg_credit / 15;
        run.psg_credit %= 15;
        if psg_step > 0 {
            hooks.chip_start("PSG");
            self.bus.psg_mut().clock(psg_step);
            hooks.chip_finish(0);
            run.psg_clocked += psg_step;
        }

        // Update HBLANK approximation based on 68K cycle position
        let spent = (self.master_cycles - run.grid) as u32;
        self.bus.vdp_mut().set_hblank_approx(spent, run.budget);

        // Handle Z80 reset-release transition per-instruction
        self.release_z80_reset();

        // Interleaved 68K + Z80 + YM2612 execution for this scanline.
        // Running the Z80 after each 68K instruction ensures the Z80 can
        // execute even when the bus is only briefly free mid-scanline (e.g.
        // Gaiares checks a Z80 init flag shortly after releasing the Z80 from
        // reset).
        //
        // Clock ratios (from master clock):
        //   Z80: master / 15, 68K/YM2612: master / 7
        //   So Z80_cycles = 68K_cycles * 7 / 15

        // Snapshot YM2612 progress before Z80 interleaving
        let ym_before_z80 = run.ym_clocked;

        // Accumulate absolute Z80 target when bus is free (fractional)
        if !self.bus.is_z80_reset() && !self.bus.is_z80_bus_requested() {
            run.z80_credit += total * 7;
            run.z80_target += run.z80_credit / 15;
            run.z80_credit %= 15;
        }

        // Run Z80 burst when target is sufficiently ahead of executed.
        // Using absolute target means overshoot from previous bursts
        // naturally prevents the Z80 from running until the target catches up.
        if run.z80_target > run.z80_executed + 3
            && !self.bus.is_z80_reset()
            && !self.bus.is_z80_bus_requested()
        {
            self.run_z80(run.z80_target, run, hooks);
        }

        // Ensure YM2612 is clocked by at least the 68K instruction's cycle count
        // for timer accuracy and audio sample generation.
        let ym_target = ym_before_z80 + total;
        if run.ym_clocked < ym_target {
            let ym_remaining =
                (ym_target - run.ym_clocked).min(run.budget.saturating_sub(run.ym_clocked));
            if ym_remaining > 0 {
                hooks.chip_start("YM2612");
                self.bus.ym2612_mut().clock(ym_remaining);
                hooks.chip_finish(0);
                run.ym_clocked += ym_remaining;
            }
        }

        Some(Stepped {
            elapsed: u64::from(total),
            instruction,
        })
    }

    /// Run the Z80 until its count for the line reaches `until`: its
    /// interrupt line, its trace while it is traced, its coverage, and the
    /// YM2612 clocked by its time, capped at the line's budget. The Z80's
    /// work after a 68000 instruction and at the line's end.
    #[inline(always)]
    pub(super) fn run_z80<H: StepHooks<Self>>(
        &mut self,
        until: u32,
        run: &mut LineRun,
        hooks: &mut H,
    ) {
        let budget = run.budget;
        let tracing = run.z80_tracing;
        // Only the trace reads the line; the run never crosses one.
        let scanline = if tracing {
            self.bus.vdp().timing().scanline
        } else {
            0
        };
        let mut irq = run.z80_irq;
        let mut executed = run.z80_executed;
        let mut ym_clocked = run.ym_clocked;
        let mut instructions = 0u32;
        hooks.chip_start("Z80");
        {
            let mut z80_bus = self.bus.create_z80_bus();
            while executed < until {
                if tracing {
                    let raw_bytes = z80_bus.instruction_bytes(self.z80.pc);
                    record_z80_trace(&mut self.debugger, &self.z80, scanline, raw_bytes);
                }
                let (processed, irq_cycles) = self.z80.check_interrupts(&mut z80_bus, irq, false);
                if processed {
                    irq = false;
                    executed += irq_cycles;
                    let ym_c = (irq_cycles * 15 / 7).min(budget.saturating_sub(ym_clocked));
                    z80_bus.clock_ym2612(ym_c);
                    ym_clocked += ym_c;
                }
                self.debugger
                    .record_coverage(SOUND_CPU, u32::from(self.z80.pc));
                let zc = self.z80.step(&mut z80_bus);
                executed += zc;
                let ym_c = (zc * 15 / 7).min(budget.saturating_sub(ym_clocked));
                z80_bus.clock_ym2612(ym_c);
                ym_clocked += ym_c;
                instructions += 1;
            }
        }
        hooks.chip_finish(instructions);
        run.z80_irq = irq;
        run.z80_executed = executed;
        run.ym_clocked = ym_clocked;
    }

    /// The Z80 let out of reset by the 68000: it starts at address 0, its
    /// stack at the top, interrupts off, mode 0.
    #[inline(always)]
    fn release_z80_reset(&mut self) {
        if self.bus.take_z80_reset_release() {
            self.z80.pc = 0x0000;
            self.z80.sp = 0xFFFF;
            self.z80.iff1 = false;
            self.z80.iff2 = false;
            self.z80.im = 0;
            self.z80.halted = false;
            self.z80.ei = 0;
        }
    }

    /// Drain up to one scanline's worth of pending 68k-bus DMA stall and return
    /// it as 68K cycles to pre-charge this line's CPU budget. Real hardware
    /// freezes the 68000 for the whole 68k->VRAM/CRAM/VSRAM transfer;
    /// we charge at most one line at a time so a multi-line transfer freezes the
    /// CPU line-by-line while the beam advances, instead of stalling a whole
    /// frame at once. The remainder stays in the VDP and is drained on following
    /// lines (and frames). The global CPU clock advances by the drained amount so
    /// audio/beam timing stays aligned. Returns 0 when no DMA is pending.
    pub(super) fn drain_dma_stall_for_line(&mut self) -> u32 {
        let line_mclks = self.region.master_clocks_per_scanline() as u64;
        let stall_mclks = self.bus.vdp_mut().drain_dma_stall_mclks(line_mclks);
        if stall_mclks == 0 {
            return 0;
        }
        let divider = (self.region.cpu_divider() as u64).max(1);
        let stall_68k = (stall_mclks / divider) as u32;
        self.master_cycles += stall_68k as u64;
        stall_68k
    }

    /// 68K DRAM-refresh bus-steal penalty, in 68K cycles, for the instruction
    /// about to be accounted. The Mega Drive steals the 68000 bus for DRAM
    /// refresh roughly every `128` CPU cycles, stalling the CPU `+2` cycles.
    /// It is checked once per instruction against the absolute cycle counter
    /// (the period and the penalty are the ones Genesis Plus GX uses).
    /// Returns `2` when `master_cycles` has reached the next refresh slot
    /// (advancing the slot), `0` otherwise. Folding the result into the
    /// instruction's `total_cycles` charges it to the line budget, `master_cycles`,
    /// and the VDP master-clock accumulator at once.
    fn bus_refresh_penalty(&mut self) -> u32 {
        const REFRESH_PERIOD_CYCLES: u64 = 128;
        const REFRESH_PENALTY_CYCLES: u32 = 2;
        if self.master_cycles >= self.bus_refresh_target {
            self.bus_refresh_target += REFRESH_PERIOD_CYCLES;
            REFRESH_PENALTY_CYCLES
        } else {
            0
        }
    }
}
