//! The machine step: one 68000 instruction and what its time does to the
//! rest of the machine (the Z80, the YM2612, the PSG, and the VDP's FIFO,
//! transfers and vertical interrupt delay), on the play loop and the
//! debugger's, and the Z80's own step.

use super::GenesisEmulator;
use crate::debugger::{record_z80_trace, TraceEntryBinary, MAIN_CPU, SOUND_CPU};
#[cfg(doc)]
use retro_core::capture::CaptureCore;

use retro_core::*;

impl GenesisEmulator {
    /// Step the Z80 CPU one instruction (debug path, 68K frozen).
    pub(super) fn step_z80(&mut self) -> StepResult {
        // Handle Z80 reset-release transition
        if self.bus.take_z80_reset_release() {
            self.z80.pc = 0x0000;
            self.z80.sp = 0xFFFF;
            self.z80.iff1 = false;
            self.z80.iff2 = false;
            self.z80.im = 0;
            self.z80.halted = false;
            self.z80.ei = 0;
        }

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
        // and disassembled before the step.
        let raw_bytes = z80_bus.instruction_bytes(pre_pc);
        let (disasm_text, _) = crate::z80::disasm::disassemble_from_bytes(pre_pc, &raw_bytes);

        // Record Z80 trace BEFORE step (captures pre-execution state, matching run_frame)
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
        self.debugger.record_history(
            SOUND_CPU,
            InstructionTrace {
                address: u32::from(pre_pc),
                instruction: disasm_text,
                registers_before: Vec::new(),
                cycle_count: total_cycles,
            },
        );

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

    /// Fast step: execute one 68K instruction with minimal overhead.
    /// Skips: disassembly, register snapshots, instruction history, boundary recording,
    /// breakpoint checks, and refresh_debugger_state(). Used by run_frame_fast() when
    /// no debug features are active.
    /// Returns the number of CPU cycles consumed (including FIFO wait).
    pub(super) fn step_fast(&mut self) -> u32 {
        // Tick VDP FIFO drain based on previous step's cycles
        if self.last_step_cycles > 0 {
            let elapsed_mclks = (self.last_step_cycles as u64) * (self.region.cpu_divider() as u64);
            self.bus.vdp_mut().tick_fifo(elapsed_mclks);
        }

        // Process pending interrupts between instructions (68000 checks IPL
        // at every instruction boundary, not just at scanline boundaries).
        // This is critical for instructions like ANDI/ORI/EORI to SR and RTE
        // that change the interrupt mask mid-scanline.
        self.refresh_vdp_interrupt_level();
        let pending_level = self.cpu.pending_irq_level;
        let irq_cycles = self.cpu.process_pending_interrupts(&mut self.bus);
        if irq_cycles > 0 {
            self.handle_interrupt_ack(pending_level);
        }

        // Reset bus cycles and VDP wait state
        self.bus.cycles_executed = 0;
        self.bus.vdp_wait_cycles = 0;

        // Set YM2612 CPU cycle for timestamp-based busy flag
        self.bus.ym2612_mut().current_cpu_cycle = self.master_cycles;
        self.bus.m68k_cycle = self.master_cycles;

        // Record coverage BEFORE stepping so the bitmap reflects every
        // PC the CPU actually executed from, regardless of whether the
        // instruction succeeded or trapped.
        self.debugger.record_coverage(MAIN_CPU, self.cpu.pc);

        // Execute one CPU instruction
        let cycles = self.cpu.step(&mut self.bus);

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
        let total_cycles = irq_cycles + cycles + fifo_wait + refresh + z80_wait;
        self.master_cycles += total_cycles as u64;

        // Accumulate master clocks for VDP timing
        let total_master_clocks = (total_cycles as u64) * (self.region.cpu_divider() as u64);
        self.step_master_clocks_accumulated += total_master_clocks;

        // Update DMA progress
        self.bus.vdp_mut().tick_dma(total_master_clocks);
        // Advance the deferred-VINT countdown (the request lands part-way into the line)
        self.bus.vdp_mut().tick_vint_delay(total_master_clocks);

        // Save total cycles for FIFO drain timing in next step
        self.last_step_cycles = total_cycles;

        total_cycles
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

    /// Lightweight debug step: execute one 68K instruction with breakpoint checking
    /// and optional trace recording, but WITHOUT the heavy per-instruction allocations
    /// (no disassembly, no register snapshots, no instruction history, no debugger state updates).
    /// Returns a StepResult indicating cycles consumed and whether a breakpoint was hit.
    pub(super) fn step_debug_light(&mut self) -> StepResult {
        // Tick VDP FIFO drain based on previous step's cycles
        if self.last_step_cycles > 0 {
            let elapsed_mclks = (self.last_step_cycles as u64) * (self.region.cpu_divider() as u64);
            self.bus.vdp_mut().tick_fifo(elapsed_mclks);
        }

        self.refresh_vdp_interrupt_level();
        let interrupted = self.cpu.pc;
        let pending_level = self.cpu.pending_irq_level;
        let irq_cycles = self.cpu.process_pending_interrupts(&mut self.bus);
        if irq_cycles > 0 {
            self.handle_interrupt_ack(pending_level);
            self.master_cycles += irq_cycles as u64;
            self.step_master_clocks_accumulated +=
                (irq_cycles as u64) * (self.region.cpu_divider() as u64);
            self.push_interrupt_frame(interrupted);
        }
        let interrupt_event = irq_cycles > 0 && self.interrupt_event_hit(pending_level);

        // A taken interrupt wakes the CPU, so a stopped one took none here.
        if self.cpu.stopped {
            let idle_cycles = self.idle_stopped_68000();
            return StepResult {
                cycles: idle_cycles,
                breakpoint_hit: self.video_event_hit(),
                instruction_executed: false,
            };
        }

        let pc = self.cpu.pc;
        let opcode = self.bus.peek_word(pc);

        // Breakpoint check (before execution)
        let breakpoint_hit_before = self.check_execute_breakpoint(pc);

        // Capture lightweight state for trace (stack-only, zero heap allocations)
        let sr_before = self.cpu.sr;
        let d_before = self.cpu.d;
        let a_before = self.cpu.a;
        let total_cycles_before = self.master_cycles;
        let scanline_before = self.bus.vdp().timing().scanline;

        self.bus.cycles_executed = 0;
        self.bus.vdp_wait_cycles = 0;
        self.bus.ym2612_mut().current_cpu_cycle = self.master_cycles;
        self.bus.m68k_cycle = self.master_cycles;

        self.debugger.record_coverage(MAIN_CPU, pc);

        let watch_armed = self.arm_watchpoints();
        let cycles = self.cpu.step(&mut self.bus);
        let watchpoint_hit = self.drain_watchpoints(watch_armed, pc);

        if self.bus.vdp().should_trigger_vint() {
            let vint_enabled = (self.bus.vdp().registers()[1] & 0x20) != 0;
            if vint_enabled && !self.bus.vdp().vint_request_delayed() {
                self.cpu.request_interrupt(6);
            }
            self.bus.vdp_mut().ack_vint();
        }

        let fifo_wait = self.bus.vdp_wait_cycles;
        self.bus.vdp_wait_cycles = 0;

        // Add 68K stall cycles for Z80-bus ($A00000-$A0FFFF) accesses this step.
        let z80_wait = self.bus.z80_bus_wait_cycles;
        self.bus.z80_bus_wait_cycles = 0;

        let refresh = self.bus_refresh_penalty();
        let total_cycles = irq_cycles + cycles + fifo_wait + refresh + z80_wait;
        self.master_cycles += total_cycles as u64;

        // Trace recording (conditional, uses pre-allocated buffer -- zero heap allocs)
        if self.debugger.tracing(MAIN_CPU) {
            let trace_entry = TraceEntryBinary::new(
                pc,
                sr_before,
                d_before,
                a_before,
                opcode,
                cycles as u16,
                total_cycles_before,
                scanline_before,
            );
            self.debugger.record_trace(MAIN_CPU, &trace_entry);
        }

        self.maintain_call_stack(pc, opcode);

        let total_master_clocks = (total_cycles as u64) * (self.region.cpu_divider() as u64);
        self.step_master_clocks_accumulated += total_master_clocks;
        self.bus.vdp_mut().tick_dma(total_master_clocks);
        // Advance the deferred-VINT countdown (the request lands part-way into the line)
        self.bus.vdp_mut().tick_vint_delay(total_master_clocks);
        let video_event = self.video_event_hit();

        // Breakpoint check (after execution), for the instruction that runs
        // next
        let new_pc = self.cpu.pc;
        let breakpoint_hit_after = if new_pc != pc {
            self.check_next_execute_breakpoint(new_pc)
        } else {
            self.debugger.forget_checked(MAIN_CPU);
            false
        };

        self.last_step_cycles = total_cycles;

        StepResult {
            cycles: total_cycles,
            breakpoint_hit: breakpoint_hit_before
                || breakpoint_hit_after
                || watchpoint_hit
                || interrupt_event
                || video_event,
            instruction_executed: true,
        }
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

    /// One step of a stopped 68000 on a debugger step, doing to the rest of
    /// the machine what [`Self::step_fast`] does for one: the bus counters
    /// cleared and stamped, the CPU's idle cycles (`M68k::step` gives 4 and
    /// changes nothing else), the DRAM refresh, and the transfers and
    /// the delay before the vertical interrupt's request counted down by
    /// that time. Without the countdown the request a program waits for in
    /// `STOP` never comes, and the program never wakes. Returns the cycles
    /// charged.
    fn idle_stopped_68000(&mut self) -> u32 {
        self.bus.cycles_executed = 0;
        self.bus.vdp_wait_cycles = 0;
        self.bus.ym2612_mut().current_cpu_cycle = self.master_cycles;
        self.bus.m68k_cycle = self.master_cycles;
        let idle_cycles = self.cpu.step(&mut self.bus);
        let total_cycles = idle_cycles + self.bus_refresh_penalty();
        self.master_cycles += total_cycles as u64;
        let total_master_clocks = (total_cycles as u64) * (self.region.cpu_divider() as u64);
        self.step_master_clocks_accumulated += total_master_clocks;
        self.bus.vdp_mut().tick_dma(total_master_clocks);
        self.bus.vdp_mut().tick_vint_delay(total_master_clocks);
        self.last_step_cycles = total_cycles;
        total_cycles
    }
}

// Private helper methods for CPU stepping (used by EmulatorCore trait impl)
impl GenesisEmulator {
    /// Step the 68K CPU one instruction (full debug path).
    ///
    /// A frame held part of the way through ([`CaptureCore::run_to_capture_boundary`],
    /// an event breakpoint) keeps the beam where it stopped: the instruction
    /// runs there, its cycles come out of the rest of the held frame
    /// ([`Self::charge_held_frame`]), and the Z80 and the sound chips, which
    /// the frame gives their whole line when it goes on, are not clocked
    /// here. The frame then finishes as a whole frame would, with the
    /// stepped instructions where it stopped; driven by this path's own
    /// beam instead, the frame loop would go on from a line the beam had
    /// left, running lines twice.
    pub(super) fn step_68k_internal(&mut self) -> StepResult {
        use retro_core::cpu::Clockable;

        let held = self.held_frame.is_some();
        // A step the host asks for (not one of the full loop's own, which
        // armed the watch as its pass began) arms it from the breakpoints
        // as they are now.
        if !self.frame_runner_owns_beam {
            self.arm_video_events();
        }

        // Advance VDP timing BEFORE instruction executes
        // This ensures VDP state is up-to-date when instruction reads VDP registers
        //
        // We use master clocks (3420 per scanline) instead of CPU cycles (488 per scanline)
        // to avoid fractional drift. CPU cycles/scanline = 3420/7 = 488.571..., which
        // causes ~0.3 scanlines of drift per frame when truncated to 488. Using master
        // clocks eliminates this entirely.
        let master_clocks_per_scanline = self.region.master_clocks_per_scanline() as u64;
        while self.step_master_clocks_accumulated >= master_clocks_per_scanline {
            self.step_master_clocks_accumulated -= master_clocks_per_scanline;
            // `run_frame` advances the beam once per outer iteration; burning
            // the credit here without a second `scanline()` keeps free-run
            // phase stable (see `frame_runner_owns_beam`). A held frame's
            // loop owns the beam the same way.
            if self.frame_runner_owns_beam || held {
                continue;
            }
            let current_scanline = self.bus.vdp().timing().scanline;
            let next_scanline = (current_scanline + 1) % self.region.scanlines() as u16;
            self.bus.vdp_mut().scanline(next_scanline);

            // Check for VBLANK interrupt (VINT) when entering VBLANK
            // The VDP sets vint_irq_pending when transitioning to VBLANK scanline.
            // After requesting the interrupt, ack_vint() clears vint_irq_pending
            // to prevent the same VINT from being re-requested on every cycle.
            // The F flag (vblank_pending) is cleared by IACK when the 68K takes
            // the interrupt, not by status register reads.
            if self.bus.vdp().should_trigger_vint() {
                let vint_enabled = (self.bus.vdp().registers()[1] & 0x20) != 0;
                if vint_enabled && !self.bus.vdp().vint_request_delayed() {
                    self.cpu.request_interrupt(6);
                }
                self.z80_irq_pending = true;
                self.bus.vdp_mut().ack_vint();
            }

            // Clear Z80 IRQ when no longer in VBLANK (level-triggered)
            if !self.bus.vdp().in_vblank() {
                self.z80_irq_pending = false;
            }

            // Check for HBLANK interrupt (HINT)
            if self.bus.vdp().should_trigger_hint() {
                self.cpu.request_interrupt(4); // HBLANK = IRQ level 4
                self.bus.vdp_mut().ack_hint();
            }

            // Scanline boundary: top up YM2612 to full scanline, clock PSG and I/O,
            // then reset per-scanline accumulators.
            {
                let cycles_per_scanline = self.region.cycles_per_scanline();
                let psg_target = self.region.master_clocks_per_scanline() / 15;

                // Top up YM2612 for any remaining cycles in the scanline.
                if self.step_ym_cycles_clocked < cycles_per_scanline {
                    self.bus
                        .ym2612_mut()
                        .clock(cycles_per_scanline - self.step_ym_cycles_clocked);
                }
                self.step_ym_cycles_clocked = 0;

                // Clock PSG once per scanline
                self.bus.psg_mut().clock(psg_target);

                // I/O clocking
                self.bus.io_mut().clock(cycles_per_scanline);

                // Reset Z80 target/executed for the new scanline
                self.step_z80_target = 0;
                self.step_z80_executed = 0;
                self.step_z80_credit_numer = 0;
            }
        }

        // Refresh VINT level from VDP state (level-triggered)
        self.refresh_vdp_interrupt_level();

        // Process any pending interrupts BEFORE the next instruction
        let interrupted = self.cpu.pc;
        let pending_level = self.cpu.pending_irq_level;
        let irq_cycles = self.cpu.process_pending_interrupts(&mut self.bus);
        if irq_cycles > 0 {
            self.handle_interrupt_ack(pending_level);
            self.master_cycles += irq_cycles as u64;
            self.step_master_clocks_accumulated +=
                (irq_cycles as u64) * (self.region.cpu_divider() as u64);
            self.push_interrupt_frame(interrupted);
        }
        let interrupt_event = irq_cycles > 0 && self.interrupt_event_hit(pending_level);

        // A CPU stopped by STOP idles; a taken interrupt wakes it, so a
        // stopped one took none here.
        if self.cpu.stopped {
            let idle_cycles = self.idle_stopped_68000();
            if held {
                self.charge_held_frame(idle_cycles);
            }
            return StepResult {
                cycles: idle_cycles,
                breakpoint_hit: self.video_event_hit(),
                instruction_executed: false,
            };
        }

        // Capture PC AFTER interrupt processing (interrupt may have changed PC)
        let pc = self.cpu.pc;

        // Read the opcode for the call stack and the trace, without side
        // effects
        let opcode = self.bus.peek_word(pc);

        // Check for an execute breakpoint at the current PC before the
        // instruction; it passes where a stop left the CPU, or where the
        // previous step's check after its instruction looked already
        let breakpoint_hit_before = self.check_execute_breakpoint(pc);

        // Disassemble current instruction for history, without side effects
        use crate::cpu68k::disasm;
        let (disasm_str, _) = disasm::disassemble_one(&mut self.bus.peek_view(), pc);

        // Capture registers before execution
        let registers_before = vec![
            RegisterValue {
                name: "PC".to_string(),
                value: self.cpu.pc as u64,
            },
            RegisterValue {
                name: "SR".to_string(),
                value: self.cpu.sr as u64,
            },
        ];

        // Capture CPU and VDP state BEFORE execution for trace
        // (SR, D registers, A registers, timing must be captured before step modifies them)
        let sr_before = self.cpu.sr;
        let d_before = self.cpu.d;
        let a_before = self.cpu.a;
        let total_cycles_before = self.master_cycles;
        let scanline_before = self.bus.vdp().timing().scanline;

        // Tick VDP FIFO: drain entries based on master clocks elapsed since last instruction.
        // This must happen before cpu.step() so the FIFO state is current when the CPU
        // accesses VDP ports. We use the previous step's total cycles (including any
        // IRQ processing cycles that were added to step_master_clocks_accumulated above).
        if self.last_step_cycles > 0 {
            let elapsed_mclks = (self.last_step_cycles as u64) * (self.region.cpu_divider() as u64);
            self.bus.vdp_mut().tick_fifo(elapsed_mclks);
        }

        // Reset bus cycles and VDP wait state AFTER all pre-step reads (opcode, disassembly)
        // This ensures only the actual instruction execution cycles are counted
        self.bus.cycles_executed = 0;
        self.bus.vdp_wait_cycles = 0;

        // Set YM2612 CPU cycle for timestamp-based busy flag.
        // Uses pre-instruction cycle count, matching MAME where machine().time()
        // during a bus access reflects the time before the instruction started.
        self.bus.ym2612_mut().current_cpu_cycle = self.master_cycles;
        self.bus.m68k_cycle = self.master_cycles;

        // Record coverage BEFORE stepping so the bitmap mirrors the
        // step_fast / step_debug_light paths and stays consistent
        // regardless of which execution mode the user landed in.
        self.debugger.record_coverage(MAIN_CPU, pc);

        // Execute one CPU instruction
        let watch_armed = self.arm_watchpoints();
        let cycles = self.cpu.step(&mut self.bus);
        let watchpoint_hit = self.drain_watchpoints(watch_armed, pc);

        // Check if a VDP register write during the instruction caused VINT to be
        // re-asserted (e.g., enabling VINT while F flag was already set from a
        // previous VBlank). On real hardware, the VDP's /INT output is combinatorial:
        //   /INT = F_FLAG AND VINT_ENABLE
        // So enabling VINT while F flag is set immediately asserts the interrupt line.
        if self.bus.vdp().should_trigger_vint() {
            let vint_enabled = (self.bus.vdp().registers()[1] & 0x20) != 0;
            if vint_enabled && !self.bus.vdp().vint_request_delayed() {
                self.cpu.request_interrupt(6);
            }
            self.bus.vdp_mut().ack_vint();
        }

        // Add VDP FIFO wait cycles (CPU stalled when FIFO is full during active display)
        let fifo_wait = self.bus.vdp_wait_cycles;
        self.bus.vdp_wait_cycles = 0;

        // Add 68K stall cycles for Z80-bus ($A00000-$A0FFFF) accesses this step.
        let z80_wait = self.bus.z80_bus_wait_cycles;
        self.bus.z80_bus_wait_cycles = 0;

        let refresh = self.bus_refresh_penalty();
        let total_cycles = cycles + fifo_wait + refresh + z80_wait;
        self.master_cycles += total_cycles as u64;

        // Z80 interleaving: run Z80 proportional to 68K cycles when bus is free.
        // This ensures the Z80 executes between 68K instructions, allowing games
        // that check Z80 status flags mid-scanline (e.g. Gaiares) to work.
        let ym_before_z80 = self.step_ym_cycles_clocked;
        {
            // Handle Z80 reset-release transition (per-instruction, not per-scanline)
            if self.bus.take_z80_reset_release() {
                self.z80.pc = 0x0000;
                self.z80.sp = 0xFFFF;
                self.z80.iff1 = false;
                self.z80.iff2 = false;
                self.z80.im = 0;
                self.z80.halted = false;
                self.z80.ei = 0;
            }

            // A held frame gives the Z80 and the YM2612 this time when it goes
            // on; the instruction's cycles come out of the rest of the frame.
            if held {
                self.charge_held_frame(irq_cycles + total_cycles);
            } else {
                // Accumulate absolute Z80 target from this 68K instruction
                // Z80 clock = master / 15, 68K clock = master / 7
                // So Z80 cycles = 68K cycles * 7 / 15
                if !self.bus.is_z80_reset() && !self.bus.is_z80_bus_requested() {
                    self.step_z80_credit_numer += total_cycles * 7;
                    self.step_z80_target += self.step_z80_credit_numer / 15;
                    self.step_z80_credit_numer %= 15;
                }

                // Run Z80 when target is sufficiently ahead of executed.
                // Absolute target means overshoot from previous bursts naturally
                // prevents Z80 from running until the target catches up.
                if self.step_z80_target > self.step_z80_executed + 3
                    && !self.bus.is_z80_reset()
                    && !self.bus.is_z80_bus_requested()
                    && self.bus.cartridge().is_some()
                {
                    let mut z80_irq = self.z80_irq_pending;
                    let cycles_per_scanline = self.region.cycles_per_scanline();
                    let ym_cap = ym_before_z80 + total_cycles;
                    let mut z80_bus = self.bus.create_z80_bus();

                    while self.step_z80_executed < self.step_z80_target {
                        let (processed, irq_cycles) =
                            self.z80.check_interrupts(&mut z80_bus, z80_irq, false);
                        if processed {
                            z80_irq = false;
                            self.step_z80_executed += irq_cycles;
                            let ym_c = (irq_cycles * 15 / 7)
                                .min(
                                    cycles_per_scanline.saturating_sub(self.step_ym_cycles_clocked),
                                )
                                .min(ym_cap.saturating_sub(self.step_ym_cycles_clocked));
                            z80_bus.clock_ym2612(ym_c);
                            self.step_ym_cycles_clocked += ym_c;
                        }
                        self.debugger
                            .record_coverage(SOUND_CPU, u32::from(self.z80.pc));
                        let zc = self.z80.step(&mut z80_bus);
                        self.step_z80_executed += zc;
                        let ym_c = (zc * 15 / 7)
                            .min(cycles_per_scanline.saturating_sub(self.step_ym_cycles_clocked))
                            .min(ym_cap.saturating_sub(self.step_ym_cycles_clocked));
                        z80_bus.clock_ym2612(ym_c);
                        self.step_ym_cycles_clocked += ym_c;
                    }

                    self.z80_irq_pending = z80_irq;
                }

                // Ensure YM2612 is clocked by at least the 68K instruction's cycle count.
                // The YM2612 has its own clock (master/7, same as 68K) independent of the Z80.
                // When Z80 is running, it partially clocks the YM2612; we fill in the remainder.
                // When Z80 is held/reset, we clock the full amount. Without this, the busy flag
                // only decrements at scanline boundaries in bulk, causing games that poll
                // YM2612 status with Z80 bus held to see stale values.
                {
                    let cycles_per_scanline = self.region.cycles_per_scanline();
                    let ym_target = ym_before_z80 + total_cycles;
                    if self.step_ym_cycles_clocked < ym_target {
                        let ym_remaining = (ym_target - self.step_ym_cycles_clocked)
                            .min(cycles_per_scanline.saturating_sub(self.step_ym_cycles_clocked));
                        if ym_remaining > 0 {
                            self.bus.ym2612_mut().clock(ym_remaining);
                            self.step_ym_cycles_clocked += ym_remaining;
                        }
                    }
                }
            }
        }

        // Record trace entry (if tracing is enabled)
        // Uses state captured BEFORE execution to match reference trace format
        if self.debugger.tracing(MAIN_CPU) {
            let trace_entry = TraceEntryBinary::new(
                pc,
                sr_before,
                d_before,
                a_before,
                opcode,
                cycles as u16,
                total_cycles_before,
                scanline_before,
            );
            self.debugger.record_trace(MAIN_CPU, &trace_entry);
        }

        self.maintain_call_stack(pc, opcode);

        // Record instruction boundary (for bsnes-style disassembly)
        self.debugger.console_mut().record_instruction_boundary(pc);

        // Record instruction in history
        let trace = InstructionTrace {
            address: pc,
            instruction: disasm_str,
            registers_before,
            cycle_count: cycles,
        };
        self.debugger.record_history(MAIN_CPU, trace);

        // Accumulate master clocks for VDP timing (will be processed at start of next step)
        // Convert total CPU cycles (instruction + refresh penalty + FIFO wait) to master clocks
        // by multiplying by CPU divider (7). This includes bus contention effects.
        let total_master_clocks = (total_cycles as u64) * (self.region.cpu_divider() as u64);
        self.step_master_clocks_accumulated += total_master_clocks;

        // Update DMA progress: the VDP DMA continues in the background while
        // the CPU is running (for VRAM Fill and VRAM Copy - the CPU is NOT halted).
        // We tick the DMA progress by the same master clocks the CPU consumed.
        self.bus.vdp_mut().tick_dma(total_master_clocks);
        // Advance the deferred-VINT countdown (the request lands part-way into the line)
        self.bus.vdp_mut().tick_vint_delay(total_master_clocks);
        let video_event = self.video_event_hit();

        // Update debugger state AFTER step to reflect new register values
        self.refresh_debugger_state();

        // Check for breakpoint at NEW PC after execution (only if different
        // from old PC), for the instruction that runs next
        let new_pc = self.cpu.pc;
        let breakpoint_hit_after = if new_pc != pc {
            self.check_next_execute_breakpoint(new_pc)
        } else {
            self.debugger.forget_checked(MAIN_CPU);
            false
        };

        // Save total cycles for FIFO drain timing in next step
        self.last_step_cycles = total_cycles;

        StepResult {
            // Report the full charge (instruction + FIFO stall + DRAM refresh),
            // not the bare instruction cycles. run_frame() accumulates this
            // against the per-scanline budget; charging only `cycles` would let
            // the refresh/FIFO time run "for free", fitting ~1.6% extra
            // instructions per frame and drifting the CPU ahead of the beam
            // (which desyncs Contra's attract-mode demo). step_fast() and
            // step_debug_light() both already return total_cycles, so this keeps
            // the three execution tiers paced identically.
            cycles: total_cycles,
            breakpoint_hit: breakpoint_hit_before
                || breakpoint_hit_after
                || watchpoint_hit
                || interrupt_event
                || video_event,
            instruction_executed: true,
        }
    }
}
