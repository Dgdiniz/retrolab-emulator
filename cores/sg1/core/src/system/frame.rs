//! The frame: the line timetable (a 68000 budget a line, with its 4/7
//! carry), what a line's start and end do, the frame loops that run it,
//! and a frame held part of the way through.

use super::GenesisEmulator;
use crate::debugger::{record_z80_trace, GenesisEvent, SOUND_CPU};
#[cfg(doc)]
use retro_core::capture::CaptureCore;

use retro_core::*;

/// A frame a frame loop left part of the way through, and where it goes on
/// from. Never part of a savestate: a savestate is always taken where a
/// frame ends ([`CaptureCore::save_state_at_frame_end`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HeldFrame {
    /// The scanline the frame goes on from.
    pub(super) line: u32,
    /// What of that line has run.
    progress: LineProgress,
    /// Why the frame stopped there.
    pub(super) reason: HoldReason,
}

/// How far a held frame's next line has run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineProgress {
    /// None of it: the line starts with the VDP's step onto it. `overshoot`
    /// is the 68000 cycles already run past the previous line's budget,
    /// which come out of this line's.
    NotStarted { overshoot: u32 },
    /// Its start: the VDP stepped onto the line, the interrupts that raised
    /// were taken and the line's event breakpoints checked. `budget` is the
    /// line's 68000 cycles and `spent` the cycles already out of it.
    Started { budget: u32, spent: u32 },
}

/// Why a frame was left part of the way through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HoldReason {
    /// A capture boundary: the picture is drawn and vertical blanking has
    /// not begun.
    CaptureBoundary,
    /// An event breakpoint (VBlank, VInt, HInt, a scanline) fired at the
    /// start of the line.
    EventBreakpoint,
}

/// How one pass of a frame loop ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PassEnd {
    /// At the capture boundary, the frame held there.
    Boundary,
    /// At the end of the frame.
    FrameEnd,
    /// Anywhere else: a breakpoint, an event breakpoint or the debugger's
    /// pause.
    Stopped,
}

/// Where a pass of a frame loop starts and ends ([`GenesisEmulator::start_frame_pass`]).
struct FramePass {
    /// The first line the pass runs.
    first_line: u32,
    /// The line the pass stops before.
    end_line: u32,
    /// 68000 cycles the previous line ran past its budget.
    overshoot: u32,
    /// For a first line whose start has run already: its budget and the
    /// cycles out of it.
    started: Option<(u32, u32)>,
}

impl GenesisEmulator {
    /// [`CaptureCore::run_to_capture_boundary`] on the frame loop
    /// `frame_loop` instead of the one the debugger's state picks.
    pub fn run_to_capture_boundary_with(&mut self, frame_loop: ExecutionTier) {
        if self.run_frame_pass(frame_loop, true) == PassEnd::FrameEnd {
            self.run_frame_pass(frame_loop, true);
        }
    }

    /// Finish a frame left part of the way through, at a capture boundary
    /// or an event breakpoint: its remaining lines run, the cartridge
    /// catches up, its audio is generated, it is counted and the host's
    /// framebuffer refreshed, as at the end of a whole frame. The rest runs
    /// on the loop a player runs, whatever the debugger is doing, so no
    /// breakpoint can stop it before the frame ends. Returns whether there
    /// was a frame to finish.
    pub fn finish_held_frame(&mut self) -> bool {
        if self.held_frame.is_none() {
            return false;
        }
        self.run_frame_fast_inner(true, false);
        // The fast loop writes no CPU state; a paused debugger shows where
        // the frame ended.
        self.refresh_debugger_state();
        true
    }
}

impl GenesisEmulator {
    /// Returns the exact 68K-cycle budget for the next scanline, carrying the
    /// fractional remainder across scanlines and frames.
    ///
    /// A scanline is `master_clocks_per_scanline / cpu_divider = 3420 / 7 =
    /// 488.571...` 68K cycles. Truncating to `488` loses `4/7` of a cycle per
    /// line (~150 cycles/frame, ~0.12% fast-forward drift). Instead we spend
    /// the integer base every line and accumulate the `3420 % 7 = 4` remainder
    /// in [`scanline_cycle_carry`](Self::scanline_cycle_carry), spending one
    /// extra cycle whenever it reaches a full `cpu_divider`. Over a frame this
    /// averages the exact `896,040 / 7` cycles of an NTSC frame, and the carry
    /// surviving across frames keeps the sub-frame phase aligned with hardware.
    fn next_scanline_cycles(&mut self) -> u32 {
        let mclk = self.region.master_clocks_per_scanline();
        let div = self.region.cpu_divider().max(1);
        let base = mclk / div;
        self.scanline_cycle_carry += mclk % div;
        if self.scanline_cycle_carry >= div {
            self.scanline_cycle_carry -= div;
            base + 1
        } else {
            base
        }
    }

    /// Advance every cartridge coprocessor to the current master cycle. Run
    /// once per frame from every tier so a coprocessor that runs decoupled
    /// from the 68000 (the SSP1601) makes progress even on the fast path.
    /// For every other chip it is a no-op.
    fn catch_up_cartridge(&mut self) {
        let mc = self.master_cycles;
        if let Some(cart) = self.bus.cartridge_mut() {
            cart.catch_up_chip(mc);
        }
    }

    /// Check the raster/interrupt event breakpoints for one scanline and
    /// return whether the run loop should pause. `vblank`/`vint`/`hint` say
    /// which interrupt-class events fired on this line; `Scanline` is checked
    /// on every line. Short-circuits to `false` (zero cost) unless an event or
    /// watch breakpoint is armed, so the debug tiers can call it unconditionally.
    fn check_frame_events(&mut self, scanline: u16, vblank: bool, vint: bool, hint: bool) -> bool {
        if !self.debugger.has_watchpoints_or_events() {
            return false;
        }
        let mut brk = self.debugger.check_event(GenesisEvent::Scanline(scanline));
        if vblank {
            brk |= self.debugger.check_event(GenesisEvent::VBlank);
        }
        if vint {
            brk |= self.debugger.check_event(GenesisEvent::VInt);
        }
        if hint {
            brk |= self.debugger.check_event(GenesisEvent::HInt);
        }
        brk
    }

    /// One pass of the frame loop `frame_loop`, from a held frame or a new
    /// one: to the capture boundary when `stop_at_boundary` and the frame
    /// has not reached it, else to the frame's end. A breakpoint, an event
    /// breakpoint or the debugger's pause can end it anywhere.
    fn run_frame_pass(&mut self, frame_loop: ExecutionTier, stop_at_boundary: bool) -> PassEnd {
        match frame_loop {
            ExecutionTier::Fast => self.run_frame_fast_inner(true, stop_at_boundary),
            ExecutionTier::LightDebug => self.run_frame_debug_light_inner(stop_at_boundary),
            ExecutionTier::FullDebug => self.run_frame_full(stop_at_boundary),
        }
    }

    /// Where a pass of a frame loop starts and stops. A held frame goes on
    /// from where it stopped. Otherwise a frame starts at line 0 and takes
    /// the players' input, once a frame, as a whole frame always has: a
    /// frame stopped part of the way through keeps the input it started
    /// with. The pass stops before the capture boundary's line when
    /// `stop_at_boundary` and the frame has not reached it yet, else at the
    /// frame's end.
    fn start_frame_pass(&mut self, scanlines: u32, stop_at_boundary: bool) -> FramePass {
        let (first_line, overshoot, started) = match self.held_frame.take() {
            Some(held) => match held.progress {
                LineProgress::NotStarted { overshoot } => (held.line, overshoot, None),
                LineProgress::Started { budget, spent } => (held.line, 0, Some((budget, spent))),
            },
            None => {
                // Sync input state from InputManager into I/O devices
                let p0 = self.input_manager.get_port_state(0);
                let p1 = self.input_manager.get_port_state(1);
                self.bus.io_mut().sync_input(p0, p1);
                // The frame is counted once it ends: it is the next one.
                self.bus
                    .vdp_mut()
                    .set_frame_being_drawn(self.frame_count + 1);
                (0, self.frame_cycle_overshoot, None)
            }
        };
        let boundary = u32::from(self.bus.vdp().first_vblank_line());
        let end_line = if stop_at_boundary && first_line < boundary {
            boundary
        } else {
            scanlines
        };
        FramePass {
            first_line,
            end_line,
            overshoot,
            started,
        }
    }

    /// Hold the frame at its capture boundary: `line`, the first line of
    /// vertical blanking, has not started, and the 68000 already ran
    /// `overshoot` cycles into it. The picture is drawn, so the host gets
    /// it now (when `present`); the cartridge, the audio and the frame count
    /// wait for the frame to finish, as they do for a whole frame.
    fn hold_at_capture_boundary(&mut self, line: u32, overshoot: u32, present: bool) {
        self.held_frame = Some(HeldFrame {
            line,
            progress: LineProgress::NotStarted { overshoot },
            reason: HoldReason::CaptureBoundary,
        });
        if present {
            self.update_cached_framebuffer();
        }
    }

    /// Hold the frame at line `line`, whose start has run, for an event
    /// breakpoint: `budget` is the line's 68000 cycles and `spent` the
    /// cycles already out of it.
    fn hold_at_event(&mut self, line: u32, budget: u32, spent: u32) {
        self.held_frame = Some(HeldFrame {
            line,
            progress: LineProgress::Started { budget, spent },
            reason: HoldReason::EventBreakpoint,
        });
    }

    /// Charge `cycles` the 68000 ran while a frame is held to the rest of
    /// that frame: they come out of the held line's budget, as an
    /// instruction's overshoot past a line does, so the frame's 68000 time
    /// is the same however many instructions were stepped while it was held.
    pub(super) fn charge_held_frame(&mut self, cycles: u32) {
        if let Some(held) = &mut self.held_frame {
            match &mut held.progress {
                LineProgress::NotStarted { overshoot } => *overshoot += cycles,
                LineProgress::Started { spent, .. } => *spent += cycles,
            }
        }
    }

    /// Shared fast-path frame body: runs a complete frame using
    /// step_fast() with no debug overhead. Skips debugger execution
    /// mode checks, breakpoints, disassembly, instruction history,
    /// register snapshots, and refresh_debugger_state().
    ///
    /// `present` gates the two presentation-only tail steps. When
    /// `false` (a headless netplay rollback/catch-up frame), audio
    /// synthesis (`generate_and_mix_audio`) and the host-visible
    /// framebuffer copy (`update_cached_framebuffer`) are skipped.
    /// Neither feeds back into machine state, so the scanline loop,
    /// Z80, PSG/YM2612 clocking, I/O, and the frame counter advance
    /// byte-identically whether or not the frame is presented.
    ///
    /// One pass of the loop: a frame held part of the way through goes on
    /// from where it stopped ([`Self::start_frame_pass`]), and with
    /// `stop_at_boundary` the pass stops at the capture boundary instead of
    /// the frame's end.
    fn run_frame_fast_inner(&mut self, present: bool, stop_at_boundary: bool) -> PassEnd {
        use retro_core::cpu::Clockable;

        let scanlines = self.region.scanlines();
        let has_rom = self.bus.cartridge().is_some();
        let pass = self.start_frame_pass(scanlines, stop_at_boundary);
        // This loop checks no event breakpoint: the VDP raises none.
        self.bus.vdp_mut().set_event_watch(None);

        // Track 68K overshoot between scanlines. On real hardware, the 68K uses
        // a continuous cycle counter, so if the last instruction of scanline N
        // overshoots by X cycles, scanline N+1 has X fewer cycles available.
        // We carry the overshoot across frames too via frame_cycle_overshoot.
        let mut m68k_overshoot = pass.overshoot;
        let mut started_line = pass.started;

        for scanline in pass.first_line..pass.end_line {
            // A held line whose start ran already goes on with its budget and
            // the cycles already out of it; any other line starts here.
            let (cycles_per_scanline, cycles_spent) = if let Some(started) = started_line.take() {
                started
            } else {
                // Update VDP scanline
                self.bus.vdp_mut().scanline(scanline as u16);
                self.step_master_clocks_accumulated = 0;
                // Exact per-line 68K budget; carries the 4/7-cycle remainder.
                let cycles_per_scanline = self.next_scanline_cycles();

                // Check for VBLANK interrupt (VINT)
                if self.bus.vdp().should_trigger_vint() {
                    // 68K: VINT is level 6 interrupt
                    let vint_enabled = (self.bus.vdp().registers()[1] & 0x20) != 0;
                    if vint_enabled && !self.bus.vdp().vint_request_delayed() {
                        self.cpu.request_interrupt(6);
                    }
                    // Z80: VBLANK also triggers Z80 maskable IRQ (same VDP /INT line)
                    self.z80_irq_pending = true;
                    self.bus.vdp_mut().ack_vint();
                }

                // Clear Z80 IRQ when no longer in VBLANK (level-triggered)
                if !self.bus.vdp().in_vblank() {
                    self.z80_irq_pending = false;
                }

                // Check for HBLANK interrupt (HINT)
                if self.bus.vdp().should_trigger_hint() {
                    self.cpu.request_interrupt(4);
                    self.bus.vdp_mut().ack_hint();
                }

                // Refresh VINT level from VDP state (level-triggered)
                self.refresh_vdp_interrupt_level();

                // Process pending interrupts.
                // Interrupt processing cycles count toward the scanline budget: the
                // CPU has one cycle count for instructions and interrupts alike.
                // Without this, HINT-heavy games get ~7% extra 68K cycles.
                let pending_level = self.cpu.pending_irq_level;
                let irq_cycles = self.cpu.process_pending_interrupts(&mut self.bus);
                if irq_cycles > 0 {
                    self.handle_interrupt_ack(pending_level);
                    self.master_cycles += irq_cycles as u64;
                }
                (cycles_per_scanline, irq_cycles + m68k_overshoot)
            };

            // Interleaved 68K + Z80 + YM2612 execution for this scanline.
            // Running the Z80 after each 68K instruction ensures the Z80 can execute
            // even when the bus is only briefly free mid-scanline (e.g. Gaiares checks
            // a Z80 init flag shortly after releasing the Z80 from reset).
            //
            // Clock ratios (from master clock):
            //   Z80: master / 15, 68K/YM2612: master / 7
            //   So Z80_cycles = 68K_cycles * 7 / 15
            let z80_tracing = self.debugger.tracing(SOUND_CPU);
            let z80_scanline = self.bus.vdp().timing().scanline;
            let psg_target = self.region.master_clocks_per_scanline() / 15;

            // Interrupt cycles and previous overshoot count toward the scanline budget
            // (all of the CPU's time comes out of the same count).
            // A pending 68k-bus DMA freezes the CPU; drain up to one line of it
            // into this scanline's budget so the transfer meters line-by-line.
            let dma_stall = self.drain_dma_stall_for_line();
            let mut cycles_executed = cycles_spent + dma_stall;
            let mut z80_target = 0u32; // absolute Z80 cycle target (monotonically increasing)
            let mut z80_credit_numer = 0u32; // fractional accumulator (numerator, /15)
            let mut z80_cycles_executed = 0u32;
            let mut ym_cycles_clocked = 0u32;
            let mut psg_cycles_clocked = 0u32; // PSG runs at master/15 regardless of Z80 bus state
            let mut psg_credit_numer = 0u32; // fractional accumulator (numerator, /15)
            let mut z80_irq = self.z80_irq_pending;

            while has_rom && cycles_executed < cycles_per_scanline {
                let step_cycles = self.step_fast();
                cycles_executed += step_cycles;

                // Spread the PSG's per-scanline budget across the line so register
                // writes resolve to ~1 output sample via Psg::catch_up instead of
                // collapsing to the line end. The PSG clocks at master/15 regardless
                // of the Z80 bus, so spend 7 numerator per 68K (master/7) cycle and
                // carry the /15 remainder within the line.
                psg_credit_numer += step_cycles * 7;
                let psg_step = psg_credit_numer / 15;
                psg_credit_numer %= 15;
                if psg_step > 0 {
                    self.bus.psg_mut().clock(psg_step);
                    psg_cycles_clocked += psg_step;
                }

                // Update HBLANK approximation based on 68K cycle position
                self.bus
                    .vdp_mut()
                    .set_hblank_approx(cycles_executed, cycles_per_scanline);

                // Handle Z80 reset-release transition per-instruction
                if self.bus.take_z80_reset_release() {
                    self.z80.pc = 0x0000;
                    self.z80.sp = 0xFFFF;
                    self.z80.iff1 = false;
                    self.z80.iff2 = false;
                    self.z80.im = 0;
                    self.z80.halted = false;
                    self.z80.ei = 0;
                }

                // Snapshot YM2612 progress before Z80 interleaving
                let ym_before_z80 = ym_cycles_clocked;

                // Accumulate absolute Z80 target when bus is free (fractional)
                if !self.bus.is_z80_reset() && !self.bus.is_z80_bus_requested() {
                    z80_credit_numer += step_cycles * 7;
                    z80_target += z80_credit_numer / 15;
                    z80_credit_numer %= 15;
                }

                // Run Z80 burst when target is sufficiently ahead of executed.
                // Using absolute target means overshoot from previous bursts
                // naturally prevents the Z80 from running until the target catches up.
                if z80_target > z80_cycles_executed + 3
                    && !self.bus.is_z80_reset()
                    && !self.bus.is_z80_bus_requested()
                {
                    let mut z80_bus = self.bus.create_z80_bus();
                    while z80_cycles_executed < z80_target {
                        if z80_tracing {
                            let raw_bytes = z80_bus.instruction_bytes(self.z80.pc);
                            record_z80_trace(
                                &mut self.debugger,
                                &self.z80,
                                z80_scanline,
                                raw_bytes,
                            );
                        }
                        let (processed, irq_cycles) =
                            self.z80.check_interrupts(&mut z80_bus, z80_irq, false);
                        if processed {
                            z80_irq = false;
                            z80_cycles_executed += irq_cycles;
                            let ym_c = (irq_cycles * 15 / 7)
                                .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                            z80_bus.clock_ym2612(ym_c);
                            ym_cycles_clocked += ym_c;
                        }
                        self.debugger
                            .record_coverage(SOUND_CPU, u32::from(self.z80.pc));
                        let zc = self.z80.step(&mut z80_bus);
                        z80_cycles_executed += zc;
                        let ym_c = (zc * 15 / 7)
                            .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                        z80_bus.clock_ym2612(ym_c);
                        ym_cycles_clocked += ym_c;
                    }
                }

                // Ensure YM2612 is clocked by at least the 68K instruction's cycle count
                // for timer accuracy and audio sample generation.
                let ym_target = ym_before_z80 + step_cycles;
                if ym_cycles_clocked < ym_target {
                    let ym_remaining = (ym_target - ym_cycles_clocked)
                        .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                    if ym_remaining > 0 {
                        self.bus.ym2612_mut().clock(ym_remaining);
                        ym_cycles_clocked += ym_remaining;
                    }
                }
            }

            // Top up Z80 to the scanline's proportional target.
            // z80_target from the accumulator may be slightly under psg_target
            // due to fractional rounding; use psg_target as the true max.
            if !self.bus.is_z80_reset() && !self.bus.is_z80_bus_requested() && has_rom {
                let z80_max = psg_target;
                if z80_cycles_executed < z80_max {
                    let mut z80_bus = self.bus.create_z80_bus();
                    while z80_cycles_executed < z80_max {
                        if z80_tracing {
                            let raw_bytes = z80_bus.instruction_bytes(self.z80.pc);
                            record_z80_trace(
                                &mut self.debugger,
                                &self.z80,
                                z80_scanline,
                                raw_bytes,
                            );
                        }
                        let (processed, irq_cycles) =
                            self.z80.check_interrupts(&mut z80_bus, z80_irq, false);
                        if processed {
                            z80_irq = false;
                            z80_cycles_executed += irq_cycles;
                            let ym_c = (irq_cycles * 15 / 7)
                                .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                            z80_bus.clock_ym2612(ym_c);
                            ym_cycles_clocked += ym_c;
                        }
                        self.debugger
                            .record_coverage(SOUND_CPU, u32::from(self.z80.pc));
                        let zc = self.z80.step(&mut z80_bus);
                        z80_cycles_executed += zc;
                        let ym_c = (zc * 15 / 7)
                            .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                        z80_bus.clock_ym2612(ym_c);
                        ym_cycles_clocked += ym_c;
                    }
                }
            }

            self.z80_irq_pending = z80_irq;

            // Save overshoot for next scanline (68K ran past the budget)
            m68k_overshoot = cycles_executed.saturating_sub(cycles_per_scanline);

            // Top up YM2612 for remaining scanline cycles
            if ym_cycles_clocked < cycles_per_scanline {
                self.bus
                    .ym2612_mut()
                    .clock(cycles_per_scanline - ym_cycles_clocked);
            }

            // Top up PSG to its exact per-scanline budget (covers the time the Z80
            // was halted/reset plus the line's fractional remainder), so the total
            // is still psg_target Z80 cycles per scanline.
            if psg_cycles_clocked < psg_target {
                self.bus.psg_mut().clock(psg_target - psg_cycles_clocked);
            }

            // I/O clocking
            self.bus.io_mut().clock(cycles_per_scanline);
        }

        if pass.end_line < scanlines {
            self.hold_at_capture_boundary(pass.end_line, m68k_overshoot, present);
            return PassEnd::Boundary;
        }

        // Carry the final scanline's instruction overshoot into the next frame.
        self.frame_cycle_overshoot = m68k_overshoot;

        // Advance cartridge coprocessors to end-of-frame (SSP1601 seam).
        self.catch_up_cartridge();

        // Advance the audio subsystem every frame: its converter + chip phase
        // are serialized machine state the netplay hash covers, so a headless
        // frame must advance them identically to an audible one (only the
        // sample mix + ring push inside are gated on `present`). The frame
        // counter is machine state and always advances; the host-visible
        // framebuffer copy is presentation-only and headless-skipped.
        self.generate_and_mix_audio(present);

        self.frame_count += 1;

        if present {
            self.update_cached_framebuffer();
        }
        PassEnd::FrameEnd
    }

    /// Fast frame execution with presentation (audio + framebuffer).
    /// This is the tier the WASM `runFrame` dispatch selects when no
    /// debug features are active. A frame held part of the way through
    /// ([`CaptureCore::run_to_capture_boundary`], an event breakpoint) is finished
    /// instead: the call is the rest of that frame.
    pub fn run_frame_fast(&mut self) {
        self.run_frame_fast_inner(true, false);
    }

    /// Headless frame for netplay rollback/catch-up: the full
    /// deterministic simulation with only the *presentation* outputs
    /// suppressed; the mixed audio is not pushed to the host ring and
    /// the host-visible framebuffer copy is skipped. Audio is still
    /// *synthesized* (the rate converter and each chip's sample-phase
    /// accumulators advance) because that is serialized machine state;
    /// suppressing it would make a re-simulated span hash differently
    /// from a live audible run and trip a spurious netplay resync.
    /// Produces the exact same machine state (including `frame_count`)
    /// as [`Self::run_frame_fast`], so re-simulating a mispredicted
    /// span leaves the emulator byte-identical to a live run. Like it, it
    /// finishes a frame held part of the way through.
    pub fn run_frame_headless(&mut self) {
        self.run_frame_fast_inner(false, false);
    }

    /// Lightweight debug frame execution: uses step_debug_light() which adds only
    /// breakpoint checking and trace recording on top of the fast path, without the
    /// heavy per-instruction allocations of the full debug path (no disassembly,
    /// no register snapshots, no instruction history, no refresh_debugger_state).
    /// This is the middle tier between run_frame_fast() and run_frame().
    /// A frame held part of the way through is finished instead: the call
    /// is the rest of that frame, and an event breakpoint holds the frame
    /// where it fires, so the next call goes on from there.
    pub fn run_frame_debug_light(&mut self) {
        self.run_frame_debug_light_inner(false);
    }

    /// The light debug loop's frame body: one pass of it, from a held
    /// frame or a new one, to the capture boundary when `stop_at_boundary`
    /// and the frame has not reached it, else to the frame's end.
    fn run_frame_debug_light_inner(&mut self, stop_at_boundary: bool) -> PassEnd {
        use retro_core::cpu::Clockable;

        let scanlines = self.region.scanlines();
        let has_rom = self.bus.cartridge().is_some();
        let pass = self.start_frame_pass(scanlines, stop_at_boundary);
        self.arm_video_events();
        let mut m68k_overshoot = pass.overshoot;
        let mut started_line = pass.started;

        for scanline in pass.first_line..pass.end_line {
            // A held line whose start ran already goes on with its budget and
            // the cycles already out of it; any other line starts here.
            let (cycles_per_scanline, cycles_spent) = if let Some(started) = started_line.take() {
                started
            } else {
                self.bus.vdp_mut().scanline(scanline as u16);
                self.step_master_clocks_accumulated = 0;
                // Exact per-line 68K budget; carries the 4/7-cycle remainder.
                let cycles_per_scanline = self.next_scanline_cycles();

                let mut ev_vblank = false;
                if self.bus.vdp().should_trigger_vint() {
                    ev_vblank = true;
                    let vint_enabled = (self.bus.vdp().registers()[1] & 0x20) != 0;
                    if vint_enabled && !self.bus.vdp().vint_request_delayed() {
                        self.cpu.request_interrupt(6);
                    }
                    self.z80_irq_pending = true;
                    self.bus.vdp_mut().ack_vint();
                }

                if !self.bus.vdp().in_vblank() {
                    self.z80_irq_pending = false;
                }

                if self.bus.vdp().should_trigger_hint() {
                    self.cpu.request_interrupt(4);
                    self.bus.vdp_mut().ack_hint();
                }

                self.refresh_vdp_interrupt_level();

                let interrupted = self.cpu.pc;
                let pending_level = self.cpu.pending_irq_level;
                let irq_cycles = self.cpu.process_pending_interrupts(&mut self.bus);
                if irq_cycles > 0 {
                    self.handle_interrupt_ack(pending_level);
                    self.master_cycles += irq_cycles as u64;
                    self.push_interrupt_frame(interrupted);
                }
                // The interrupt events are the interrupts taken here, not those
                // requested: a request the 68000's mask holds back, or the
                // vertical interrupt's that lands later in the line, is taken
                // in a step, which checks them itself.
                let ev_vint = irq_cycles > 0 && pending_level == 6;
                let ev_hint = irq_cycles > 0 && pending_level == 4;

                // Raster/interrupt event breakpoints. Checked after the interrupt is
                // taken (so a VInt break lands with PC at the handler entry) and
                // costs nothing unless an event/watch breakpoint is armed.
                if self.check_frame_events(scanline as u16, ev_vblank, ev_vint, ev_hint) {
                    self.hold_at_event(scanline, cycles_per_scanline, irq_cycles + m68k_overshoot);
                    self.set_execution_mode(ExecutionMode::Paused);
                    self.update_cached_framebuffer();
                    return PassEnd::Stopped;
                }
                (cycles_per_scanline, irq_cycles + m68k_overshoot)
            };

            let z80_tracing = self.debugger.tracing(SOUND_CPU);
            let z80_scanline = self.bus.vdp().timing().scanline;
            let psg_target = self.region.master_clocks_per_scanline() / 15;

            // Drain up to one line of pending 68k-bus DMA stall (freezes the CPU).
            let dma_stall = self.drain_dma_stall_for_line();
            let mut cycles_executed = cycles_spent + dma_stall;
            let mut z80_target = 0u32;
            let mut z80_credit_numer = 0u32;
            let mut z80_cycles_executed = 0u32;
            let mut ym_cycles_clocked = 0u32;
            let mut psg_cycles_clocked = 0u32; // PSG runs at master/15 regardless of Z80 bus state
            let mut psg_credit_numer = 0u32; // fractional accumulator (numerator, /15)
            let mut z80_irq = self.z80_irq_pending;

            while has_rom && cycles_executed < cycles_per_scanline {
                let step_result = self.step_debug_light();

                self.bus
                    .vdp_mut()
                    .set_hblank_approx(cycles_executed + step_result.cycles, cycles_per_scanline);

                if step_result.breakpoint_hit {
                    self.z80_irq_pending = z80_irq;
                    self.set_execution_mode(ExecutionMode::Paused);
                    self.update_cached_framebuffer();
                    return PassEnd::Stopped;
                }

                cycles_executed += step_result.cycles;

                // Spread the PSG's per-scanline budget across the line so register
                // writes resolve to ~1 output sample via Psg::catch_up. The PSG runs
                // at master/15 independent of the Z80 bus, so spend 7 numerator per
                // 68K (master/7) cycle and carry the /15 remainder within the line.
                psg_credit_numer += step_result.cycles * 7;
                let psg_step = psg_credit_numer / 15;
                psg_credit_numer %= 15;
                if psg_step > 0 {
                    self.bus.psg_mut().clock(psg_step);
                    psg_cycles_clocked += psg_step;
                }

                if self.bus.take_z80_reset_release() {
                    self.z80.pc = 0x0000;
                    self.z80.sp = 0xFFFF;
                    self.z80.iff1 = false;
                    self.z80.iff2 = false;
                    self.z80.im = 0;
                    self.z80.halted = false;
                    self.z80.ei = 0;
                }

                let ym_before_z80 = ym_cycles_clocked;

                if !self.bus.is_z80_reset() && !self.bus.is_z80_bus_requested() {
                    z80_credit_numer += step_result.cycles * 7;
                    z80_target += z80_credit_numer / 15;
                    z80_credit_numer %= 15;
                }

                if z80_target > z80_cycles_executed + 3
                    && !self.bus.is_z80_reset()
                    && !self.bus.is_z80_bus_requested()
                {
                    let mut z80_bus = self.bus.create_z80_bus();
                    while z80_cycles_executed < z80_target {
                        if z80_tracing {
                            let raw_bytes = z80_bus.instruction_bytes(self.z80.pc);
                            record_z80_trace(
                                &mut self.debugger,
                                &self.z80,
                                z80_scanline,
                                raw_bytes,
                            );
                        }
                        let (processed, irq_cyc) =
                            self.z80.check_interrupts(&mut z80_bus, z80_irq, false);
                        if processed {
                            z80_irq = false;
                            z80_cycles_executed += irq_cyc;
                            let ym_c = (irq_cyc * 15 / 7)
                                .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                            z80_bus.clock_ym2612(ym_c);
                            ym_cycles_clocked += ym_c;
                        }
                        self.debugger
                            .record_coverage(SOUND_CPU, u32::from(self.z80.pc));
                        let zc = self.z80.step(&mut z80_bus);
                        z80_cycles_executed += zc;
                        let ym_c = (zc * 15 / 7)
                            .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                        z80_bus.clock_ym2612(ym_c);
                        ym_cycles_clocked += ym_c;
                    }
                }

                let ym_target = ym_before_z80 + step_result.cycles;
                if ym_cycles_clocked < ym_target {
                    let ym_remaining = (ym_target - ym_cycles_clocked)
                        .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                    if ym_remaining > 0 {
                        self.bus.ym2612_mut().clock(ym_remaining);
                        ym_cycles_clocked += ym_remaining;
                    }
                }
            }

            // Top up Z80 to the scanline's proportional target
            if !self.bus.is_z80_reset() && !self.bus.is_z80_bus_requested() && has_rom {
                let z80_max = psg_target;
                if z80_cycles_executed < z80_max {
                    let mut z80_bus = self.bus.create_z80_bus();
                    while z80_cycles_executed < z80_max {
                        if z80_tracing {
                            let raw_bytes = z80_bus.instruction_bytes(self.z80.pc);
                            record_z80_trace(
                                &mut self.debugger,
                                &self.z80,
                                z80_scanline,
                                raw_bytes,
                            );
                        }
                        let (processed, irq_cyc) =
                            self.z80.check_interrupts(&mut z80_bus, z80_irq, false);
                        if processed {
                            z80_irq = false;
                            z80_cycles_executed += irq_cyc;
                            let ym_c = (irq_cyc * 15 / 7)
                                .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                            z80_bus.clock_ym2612(ym_c);
                            ym_cycles_clocked += ym_c;
                        }
                        self.debugger
                            .record_coverage(SOUND_CPU, u32::from(self.z80.pc));
                        let zc = self.z80.step(&mut z80_bus);
                        z80_cycles_executed += zc;
                        let ym_c = (zc * 15 / 7)
                            .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                        z80_bus.clock_ym2612(ym_c);
                        ym_cycles_clocked += ym_c;
                    }
                }
            }

            self.z80_irq_pending = z80_irq;
            m68k_overshoot = cycles_executed.saturating_sub(cycles_per_scanline);

            if ym_cycles_clocked < cycles_per_scanline {
                self.bus
                    .ym2612_mut()
                    .clock(cycles_per_scanline - ym_cycles_clocked);
            }

            // Top up PSG to its exact per-scanline budget (covers the time the Z80
            // was halted/reset plus the line's fractional remainder).
            if psg_cycles_clocked < psg_target {
                self.bus.psg_mut().clock(psg_target - psg_cycles_clocked);
            }
            self.bus.io_mut().clock(cycles_per_scanline);
        }

        if pass.end_line < scanlines {
            self.hold_at_capture_boundary(pass.end_line, m68k_overshoot, true);
            return PassEnd::Boundary;
        }

        self.frame_cycle_overshoot = m68k_overshoot;

        // Advance cartridge coprocessors to end-of-frame (SSP1601 seam).
        self.catch_up_cartridge();

        self.generate_and_mix_audio(true);
        self.frame_count += 1;
        self.update_cached_framebuffer();
        PassEnd::FrameEnd
    }

    /// Profiled frame execution: identical to run_frame_fast() but measures time
    /// spent in each subsystem. Returns a FrameProfile with timing breakdowns.
    pub fn run_frame_profiled(&mut self) -> FrameProfile {
        use retro_core::cpu::Clockable;
        use std::time::Instant;

        // A frame held part of the way through is finished, unmeasured: the
        // call is the rest of that frame, as every frame call is.
        if self.held_frame.is_some() {
            self.run_frame_fast_inner(true, false);
            return FrameProfile::default();
        }

        // Sync input state from InputManager into I/O devices
        let p0 = self.input_manager.get_port_state(0);
        let p1 = self.input_manager.get_port_state(1);
        self.bus.io_mut().sync_input(p0, p1);
        self.bus
            .vdp_mut()
            .set_frame_being_drawn(self.frame_count + 1);

        let frame_start = Instant::now();
        let mut profile = FrameProfile::default();

        let scanlines = self.region.scanlines();
        let has_rom = self.bus.cartridge().is_some();
        let mut m68k_overshoot = self.frame_cycle_overshoot;

        for scanline in 0..scanlines {
            // VDP scanline (includes rendering)
            let t = Instant::now();
            self.bus.vdp_mut().scanline(scanline as u16);
            profile.vdp_ns += t.elapsed().as_nanos() as u64;

            self.step_master_clocks_accumulated = 0;
            // Exact per-line 68K budget; carries the 4/7-cycle remainder.
            let cycles_per_scanline = self.next_scanline_cycles();

            // Interrupt checks (cheap, lumped into "other")
            if self.bus.vdp().should_trigger_vint() {
                let vint_enabled = (self.bus.vdp().registers()[1] & 0x20) != 0;
                if vint_enabled && !self.bus.vdp().vint_request_delayed() {
                    self.cpu.request_interrupt(6);
                }
                self.bus.vdp_mut().ack_vint();
            }
            if self.bus.vdp().should_trigger_hint() {
                self.cpu.request_interrupt(4);
                self.bus.vdp_mut().ack_hint();
            }
            self.refresh_vdp_interrupt_level();
            let pending_level = self.cpu.pending_irq_level;
            let irq_cycles = self.cpu.process_pending_interrupts(&mut self.bus);
            if irq_cycles > 0 {
                self.handle_interrupt_ack(pending_level);
                self.master_cycles += irq_cycles as u64;
            }

            // Interleaved 68K + Z80 + YM2612 execution (same approach as run_frame_fast)
            let psg_target = self.region.master_clocks_per_scanline() / 15;
            let t = Instant::now();
            // Drain up to one line of pending 68k-bus DMA stall (freezes the CPU).
            let dma_stall = self.drain_dma_stall_for_line();
            let mut cycles_executed = irq_cycles + m68k_overshoot + dma_stall;
            let mut m68k_instructions = 0u32;
            let mut z80_target = 0u32;
            let mut z80_credit_numer = 0u32;
            let mut z80_cycles_executed = 0u32;
            let mut ym_cycles_clocked = 0u32;
            let mut z80_irq = self.z80_irq_pending;

            while has_rom && cycles_executed < cycles_per_scanline {
                let step_cycles = self.step_fast();
                cycles_executed += step_cycles;
                m68k_instructions += 1;
                self.bus
                    .vdp_mut()
                    .set_hblank_approx(cycles_executed, cycles_per_scanline);

                if self.bus.take_z80_reset_release() {
                    self.z80.pc = 0x0000;
                    self.z80.sp = 0xFFFF;
                    self.z80.iff1 = false;
                    self.z80.iff2 = false;
                    self.z80.im = 0;
                    self.z80.halted = false;
                    self.z80.ei = 0;
                }

                if !self.bus.is_z80_reset() && !self.bus.is_z80_bus_requested() {
                    z80_credit_numer += step_cycles * 7;
                    z80_target += z80_credit_numer / 15;
                    z80_credit_numer %= 15;
                }

                if z80_target > z80_cycles_executed + 3
                    && !self.bus.is_z80_reset()
                    && !self.bus.is_z80_bus_requested()
                {
                    let mut z80_bus = self.bus.create_z80_bus();
                    while z80_cycles_executed < z80_target {
                        let (processed, irq_cycles) =
                            self.z80.check_interrupts(&mut z80_bus, z80_irq, false);
                        if processed {
                            z80_irq = false;
                            z80_cycles_executed += irq_cycles;
                            let ym_c = (irq_cycles * 15 / 7)
                                .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                            z80_bus.clock_ym2612(ym_c);
                            ym_cycles_clocked += ym_c;
                        }
                        self.debugger
                            .record_coverage(SOUND_CPU, u32::from(self.z80.pc));
                        let zc = self.z80.step(&mut z80_bus);
                        z80_cycles_executed += zc;
                        let ym_c = (zc * 15 / 7)
                            .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                        z80_bus.clock_ym2612(ym_c);
                        ym_cycles_clocked += ym_c;
                        profile.z80_instructions += 1;
                    }
                }
            }

            // Top up Z80 to the scanline's proportional target
            if !self.bus.is_z80_reset() && !self.bus.is_z80_bus_requested() && has_rom {
                let z80_max = psg_target;
                if z80_cycles_executed < z80_max {
                    let mut z80_bus = self.bus.create_z80_bus();
                    while z80_cycles_executed < z80_max {
                        let (processed, irq_cycles) =
                            self.z80.check_interrupts(&mut z80_bus, z80_irq, false);
                        if processed {
                            z80_irq = false;
                            z80_cycles_executed += irq_cycles;
                            let ym_c = (irq_cycles * 15 / 7)
                                .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                            z80_bus.clock_ym2612(ym_c);
                            ym_cycles_clocked += ym_c;
                        }
                        self.debugger
                            .record_coverage(SOUND_CPU, u32::from(self.z80.pc));
                        let zc = self.z80.step(&mut z80_bus);
                        z80_cycles_executed += zc;
                        let ym_c = (zc * 15 / 7)
                            .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                        z80_bus.clock_ym2612(ym_c);
                        ym_cycles_clocked += ym_c;
                        profile.z80_instructions += 1;
                    }
                }
            }

            self.z80_irq_pending = z80_irq;

            m68k_overshoot = cycles_executed.saturating_sub(cycles_per_scanline);

            if ym_cycles_clocked < cycles_per_scanline {
                self.bus
                    .ym2612_mut()
                    .clock(cycles_per_scanline - ym_cycles_clocked);
            }

            self.bus.psg_mut().clock(psg_target);

            let interleaved_elapsed = t.elapsed().as_nanos() as u64;
            profile.m68k_ns += interleaved_elapsed / 2;
            profile.m68k_instructions += m68k_instructions as u64;
            profile.m68k_cycles += cycles_executed as u64;
            profile.z80_ns += interleaved_elapsed / 4;
            profile.psg_ns += interleaved_elapsed / 8;
            profile.ym2612_ns += interleaved_elapsed / 8;

            // I/O
            self.bus.io_mut().clock(cycles_per_scanline);
        }

        self.frame_cycle_overshoot = m68k_overshoot;

        // Audio generation
        let t = Instant::now();
        self.generate_and_mix_audio(true);
        profile.audio_mix_ns += t.elapsed().as_nanos() as u64;

        self.frame_count += 1;

        // Framebuffer update
        let t = Instant::now();
        self.update_cached_framebuffer();
        profile.framebuffer_ns += t.elapsed().as_nanos() as u64;

        profile.total_ns = frame_start.elapsed().as_nanos() as u64;
        profile
    }
}

/// Timing breakdown for a single frame's execution.
#[derive(Default, Clone)]
pub struct FrameProfile {
    pub total_ns: u64,
    pub m68k_ns: u64,
    pub z80_ns: u64,
    pub vdp_ns: u64,
    pub ym2612_ns: u64,
    pub psg_ns: u64,
    pub audio_mix_ns: u64,
    pub framebuffer_ns: u64,
    pub m68k_instructions: u64,
    pub m68k_cycles: u64,
    pub z80_instructions: u64,
}

impl FrameProfile {
    pub fn print_summary(&self, label: &str) {
        let total_us = self.total_ns as f64 / 1000.0;
        let m68k_us = self.m68k_ns as f64 / 1000.0;
        let z80_us = self.z80_ns as f64 / 1000.0;
        let vdp_us = self.vdp_ns as f64 / 1000.0;
        let ym2612_us = self.ym2612_ns as f64 / 1000.0;
        let psg_us = self.psg_ns as f64 / 1000.0;
        let audio_us = self.audio_mix_ns as f64 / 1000.0;
        let fb_us = self.framebuffer_ns as f64 / 1000.0;
        let accounted = m68k_us + z80_us + vdp_us + ym2612_us + psg_us + audio_us + fb_us;
        let other_us = total_us - accounted;

        let pct = |v: f64| -> f64 {
            if total_us > 0.0 {
                v / total_us * 100.0
            } else {
                0.0
            }
        };

        println!("  {}", label);
        println!(
            "  Total:       {:>10.1} us ({:.2} ms)",
            total_us,
            total_us / 1000.0
        );
        println!("  -------------------------------------------------------");
        println!(
            "  68K CPU:     {:>10.1} us  ({:>5.1}%)  [{} instr, {} cycles]",
            m68k_us,
            pct(m68k_us),
            self.m68k_instructions,
            self.m68k_cycles
        );
        println!(
            "  Z80 CPU:     {:>10.1} us  ({:>5.1}%)  [{} instr]",
            z80_us,
            pct(z80_us),
            self.z80_instructions
        );
        println!(
            "  VDP render:  {:>10.1} us  ({:>5.1}%)",
            vdp_us,
            pct(vdp_us)
        );
        println!(
            "  YM2612:      {:>10.1} us  ({:>5.1}%)",
            ym2612_us,
            pct(ym2612_us)
        );
        println!(
            "  PSG:         {:>10.1} us  ({:>5.1}%)",
            psg_us,
            pct(psg_us)
        );
        println!(
            "  Audio mix:   {:>10.1} us  ({:>5.1}%)",
            audio_us,
            pct(audio_us)
        );
        println!("  Framebuffer: {:>10.1} us  ({:>5.1}%)", fb_us, pct(fb_us));
        println!(
            "  Other/overhead: {:>7.1} us  ({:>5.1}%)",
            other_us,
            pct(other_us)
        );
    }
}

impl GenesisEmulator {
    /// The full debug loop's frame body: one pass of it, from a held frame
    /// or a new one, to the capture boundary when `stop_at_boundary` and
    /// the frame has not reached it, else to the frame's end.
    ///
    /// While a frame is held, the debugger's pause runs nothing (the frame
    /// stays held) and a step mode steps one instruction where the frame
    /// is held ([`Self::step_68k_internal`]): this loop's first line would
    /// otherwise run, and the rest of the held frame would be lost.
    pub(super) fn run_frame_full(&mut self, stop_at_boundary: bool) -> PassEnd {
        use retro_core::cpu::Clockable;

        if self.held_frame.is_some() {
            match self.debugger.get_execution_mode() {
                ExecutionMode::Running => {}
                ExecutionMode::Paused => return PassEnd::Stopped,
                ExecutionMode::StepInto | ExecutionMode::StepOver | ExecutionMode::StepOut => {
                    self.step();
                    self.debugger.set_execution_mode(ExecutionMode::Paused);
                    self.update_cached_framebuffer();
                    return PassEnd::Stopped;
                }
            }
        }

        let scanlines = self.region.scanlines();
        let pass = self.start_frame_pass(scanlines, stop_at_boundary);
        self.arm_video_events();
        let mut m68k_overshoot = pass.overshoot;
        let mut started_line = pass.started;

        // Own the beam for this frame so step_68k_internal does not also
        // call scanline() when a 489-cycle line crosses 3420 mclks.
        self.frame_runner_owns_beam = true;

        for frame_line in pass.first_line..pass.end_line {
            // A held line whose start ran already goes on with its budget and
            // the cycles already out of it; any other line starts here.
            let (cycles_per_scanline, cycles_spent) = if let Some(started) = started_line.take() {
                started
            } else {
                // Walk the beam 0..scanlines, the same way `run_frame_fast` does.
                // Free-running it from wherever the previous frame left off (the
                // phase `step()` produces when it owns the beam) puts the frame
                // boundary in the middle of VBlank, and a game whose main loop is
                // driven by the vertical interrupt then re-enters its handler
                // without ever leaving it: Dynamite Headdy freezes at $0118DE on
                // frame 44, with the debug tier and the fast tier having agreed on
                // every frame before it. Both runners present the same frame
                // to the program, which is also what makes a screenshot taken
                // here match what the player sees.
                let scanline = frame_line as u16;
                self.bus.vdp_mut().scanline(scanline);

                // Reset step master clock accumulator each line. Combined with
                // `frame_runner_owns_beam`, this keeps beam ownership exclusive to
                // this loop (step()'s scanline advance is for CLI single-step only).
                self.step_master_clocks_accumulated = 0;
                // Exact per-line 68K budget; carries the 4/7-cycle remainder.
                let cycles_per_scanline = self.next_scanline_cycles();

                // Check for VBLANK interrupt (VINT) when entering VBLANK
                let mut ev_vblank = false;
                if self.bus.vdp().should_trigger_vint() {
                    ev_vblank = true;
                    let vint_enabled = (self.bus.vdp().registers()[1] & 0x20) != 0;
                    if vint_enabled && !self.bus.vdp().vint_request_delayed() {
                        self.cpu.request_interrupt(6); // VBLANK = IRQ level 6
                    }
                    // Z80: VBLANK also triggers Z80 maskable IRQ
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

                // Refresh VINT level from VDP state (level-triggered)
                self.refresh_vdp_interrupt_level();

                // Process pending interrupts (cycles count toward scanline budget)
                let interrupted = self.cpu.pc;
                let pending_level = self.cpu.pending_irq_level;
                let irq_cycles = self.cpu.process_pending_interrupts(&mut self.bus);
                if irq_cycles > 0 {
                    self.handle_interrupt_ack(pending_level);
                    self.master_cycles += irq_cycles as u64;
                    self.push_interrupt_frame(interrupted);
                }
                // The interrupt events are the interrupts taken here; one
                // taken later, in a step, is checked there.
                let ev_vint = irq_cycles > 0 && pending_level == 6;
                let ev_hint = irq_cycles > 0 && pending_level == 4;

                // Raster/interrupt event breakpoints. Checked after the interrupt is
                // taken (so a VInt break lands with PC at the handler entry) and
                // costs nothing unless an event/watch breakpoint is armed.
                if self.check_frame_events(scanline, ev_vblank, ev_vint, ev_hint) {
                    self.hold_at_event(
                        frame_line,
                        cycles_per_scanline,
                        irq_cycles + m68k_overshoot,
                    );
                    self.set_execution_mode(ExecutionMode::Paused);
                    self.update_cached_framebuffer();
                    self.frame_runner_owns_beam = false;
                    return PassEnd::Stopped;
                }
                (cycles_per_scanline, irq_cycles + m68k_overshoot)
            };

            // Interleaved 68K + Z80 + YM2612 execution (same approach as run_frame_fast)
            let has_rom = self.bus.cartridge().is_some();
            let z80_tracing = self.debugger.tracing(SOUND_CPU);
            let z80_scanline = self.bus.vdp().timing().scanline;
            let psg_target = self.region.master_clocks_per_scanline() / 15;

            // Drain up to one line of pending 68k-bus DMA stall (freezes the CPU).
            let dma_stall = self.drain_dma_stall_for_line();
            let mut cycles_executed = cycles_spent + dma_stall;
            let mut z80_target = 0u32;
            let mut z80_credit_numer = 0u32;
            let mut z80_cycles_executed = 0u32;
            let mut ym_cycles_clocked = 0u32;
            let mut z80_irq = self.z80_irq_pending;
            let mut early_exit = false;
            // When focused on Z80, step_z80() handles Z80 execution and audio
            // clocking internally; skip the interleaving to avoid double-stepping.
            let z80_focused = self.debugger.focused_cpu() == SOUND_CPU;

            while has_rom && cycles_executed < cycles_per_scanline {
                let exec_mode = self.debugger.get_execution_mode();

                let step_cycles = match exec_mode {
                    ExecutionMode::Running => {
                        let step_result = self.step();
                        self.bus.vdp_mut().set_hblank_approx(
                            cycles_executed + step_result.cycles,
                            cycles_per_scanline,
                        );
                        if step_result.breakpoint_hit {
                            self.z80_irq_pending = z80_irq;
                            self.set_execution_mode(ExecutionMode::Paused);
                            self.update_cached_framebuffer();
                            self.frame_runner_owns_beam = false;
                            return PassEnd::Stopped;
                        }
                        step_result.cycles
                    }
                    ExecutionMode::Paused => {
                        self.z80_irq_pending = z80_irq;
                        self.update_cached_framebuffer();
                        self.frame_runner_owns_beam = false;
                        return PassEnd::Stopped;
                    }
                    ExecutionMode::StepInto | ExecutionMode::StepOver | ExecutionMode::StepOut => {
                        let step_result = self.step();
                        self.bus.vdp_mut().set_hblank_approx(
                            cycles_executed + step_result.cycles,
                            cycles_per_scanline,
                        );
                        self.debugger.set_execution_mode(ExecutionMode::Paused);
                        early_exit = true;
                        step_result.cycles
                    }
                };

                cycles_executed += step_cycles;

                // Handle Z80 reset-release transition per-instruction
                if self.bus.take_z80_reset_release() {
                    self.z80.pc = 0x0000;
                    self.z80.sp = 0xFFFF;
                    self.z80.iff1 = false;
                    self.z80.iff2 = false;
                    self.z80.im = 0;
                    self.z80.halted = false;
                    self.z80.ei = 0;
                }

                // Z80 interleaving only when 68K is the focused CPU.
                // When Z80 is focused, step_z80() already handles Z80+audio.
                if !z80_focused {
                    let ym_before_z80 = ym_cycles_clocked;

                    if !self.bus.is_z80_reset() && !self.bus.is_z80_bus_requested() {
                        z80_credit_numer += step_cycles * 7;
                        z80_target += z80_credit_numer / 15;
                        z80_credit_numer %= 15;
                    }

                    if z80_target > z80_cycles_executed + 3
                        && !self.bus.is_z80_reset()
                        && !self.bus.is_z80_bus_requested()
                    {
                        let mut z80_bus = self.bus.create_z80_bus();
                        while z80_cycles_executed < z80_target {
                            if z80_tracing {
                                let raw_bytes = z80_bus.instruction_bytes(self.z80.pc);
                                record_z80_trace(
                                    &mut self.debugger,
                                    &self.z80,
                                    z80_scanline,
                                    raw_bytes,
                                );
                            }
                            let (processed, irq_cycles) =
                                self.z80.check_interrupts(&mut z80_bus, z80_irq, false);
                            if processed {
                                z80_irq = false;
                                z80_cycles_executed += irq_cycles;
                                let ym_c = (irq_cycles * 15 / 7)
                                    .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                                z80_bus.clock_ym2612(ym_c);
                                ym_cycles_clocked += ym_c;
                            }
                            self.debugger
                                .record_coverage(SOUND_CPU, u32::from(self.z80.pc));
                            let zc = self.z80.step(&mut z80_bus);
                            z80_cycles_executed += zc;
                            let ym_c = (zc * 15 / 7)
                                .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                            z80_bus.clock_ym2612(ym_c);
                            ym_cycles_clocked += ym_c;
                        }
                    }

                    // Ensure YM2612 is clocked by at least the 68K instruction's cycle count
                    let ym_target = ym_before_z80 + step_cycles;
                    if ym_cycles_clocked < ym_target {
                        let ym_remaining = (ym_target - ym_cycles_clocked)
                            .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                        if ym_remaining > 0 {
                            self.bus.ym2612_mut().clock(ym_remaining);
                            ym_cycles_clocked += ym_remaining;
                        }
                    }
                }

                if early_exit {
                    self.z80_irq_pending = z80_irq;
                    self.update_cached_framebuffer();
                    self.frame_runner_owns_beam = false;
                    return PassEnd::Stopped;
                }
            }

            // Top up Z80 to the scanline's proportional target
            if !z80_focused
                && !self.bus.is_z80_reset()
                && !self.bus.is_z80_bus_requested()
                && has_rom
            {
                let z80_max = psg_target;
                if z80_cycles_executed < z80_max {
                    let mut z80_bus = self.bus.create_z80_bus();
                    while z80_cycles_executed < z80_max {
                        if z80_tracing {
                            let raw_bytes = z80_bus.instruction_bytes(self.z80.pc);
                            record_z80_trace(
                                &mut self.debugger,
                                &self.z80,
                                z80_scanline,
                                raw_bytes,
                            );
                        }
                        let (processed, irq_cycles) =
                            self.z80.check_interrupts(&mut z80_bus, z80_irq, false);
                        if processed {
                            z80_irq = false;
                            z80_cycles_executed += irq_cycles;
                            let ym_c = (irq_cycles * 15 / 7)
                                .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                            z80_bus.clock_ym2612(ym_c);
                            ym_cycles_clocked += ym_c;
                        }
                        self.debugger
                            .record_coverage(SOUND_CPU, u32::from(self.z80.pc));
                        let zc = self.z80.step(&mut z80_bus);
                        z80_cycles_executed += zc;
                        let ym_c = (zc * 15 / 7)
                            .min(cycles_per_scanline.saturating_sub(ym_cycles_clocked));
                        z80_bus.clock_ym2612(ym_c);
                        ym_cycles_clocked += ym_c;
                    }
                }
            }

            self.z80_irq_pending = z80_irq;

            m68k_overshoot = cycles_executed.saturating_sub(cycles_per_scanline);

            // Top up YM2612 for remaining scanline cycles
            if ym_cycles_clocked < cycles_per_scanline {
                self.bus
                    .ym2612_mut()
                    .clock(cycles_per_scanline - ym_cycles_clocked);
            }

            // Clock PSG exactly psg_target Z80 cycles per scanline (always)
            self.bus.psg_mut().clock(psg_target);

            // I/O clocking
            // I/O ports run at 68K clock for 6-button timeout tracking (1.5ms = ~11,505 cycles)
            self.bus.io_mut().clock(cycles_per_scanline);
        }

        if pass.end_line < scanlines {
            self.frame_runner_owns_beam = false;
            self.hold_at_capture_boundary(pass.end_line, m68k_overshoot, true);
            return PassEnd::Boundary;
        }

        // Carry the final scanline's instruction overshoot into the next frame.
        self.frame_cycle_overshoot = m68k_overshoot;

        // Advance cartridge coprocessors to end-of-frame (SSP1601 seam).
        self.catch_up_cartridge();

        // Generate audio and mix into output buffer
        self.generate_and_mix_audio(true);

        self.frame_count += 1;
        self.frame_runner_owns_beam = false;

        // Update cached framebuffer from VDP
        self.update_cached_framebuffer();
        PassEnd::FrameEnd
    }
}
