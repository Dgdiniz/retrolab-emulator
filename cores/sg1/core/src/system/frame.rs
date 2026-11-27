//! The frame: the line timetable (a 68000 budget a line, with its 4/7
//! carry), what a line's start and end do, the frame's end, and the frame
//! driver's view of them (`retro_core::frame::FrameMachine`): a frame held
//! part of the way through goes on from the line it stopped on.

use super::GenesisEmulator;
use crate::debugger::GenesisEvent;
use crate::timing::GenesisTiming;
#[cfg(doc)]
use retro_core::capture::CaptureCore;
use retro_core::cartridge::Cartridge;
use retro_core::cpu::Clockable;
use retro_core::frame::{Due, FrameMachine, StepHooks, Stepped};
use retro_core::sub_clock::{FixedRatio, SubClock};

/// The Z80's clock against the 68000's: the Z80 runs at a fifteenth of the
/// master clock and the 68000 at a seventh, so 7 Z80 cycles in each 15 of
/// the 68000's, in both regions. The PSG's input clock is the Z80's.
pub(super) type Z80Ratio = FixedRatio<7, 15>;

/// The YM2612's clock against the 68000's: both run at a seventh of the
/// master clock.
pub(super) type YmRatio = FixedRatio<1, 1>;

/// A line's counters, which each 68000 step moves: in local variables for
/// a run of steps to the line's end ([`FrameMachine::begin_run`] copies them
/// out of the machine, [`FrameMachine::end_run`] writes them back), and in
/// the machine between runs, where the line's end reads them.
#[derive(Clone, Copy, Debug, Default)]
pub struct LineRun {
    /// `master_cycles` less the 68000 cycles the line has spent. The line's
    /// start and every step add the same time to both, so it is constant
    /// within a line: `master_cycles - grid` is the line's spent cycles and
    /// `grid + budget` its end.
    pub(super) grid: u64,
    /// The line's 68000 cycles, 488 or 489 (the 4/7 carry).
    pub(super) budget: u32,
    /// The Z80's clock against the 68000's, which goes on from line to
    /// line and frame to frame: the Z80 runs the cycles it owes in bursts,
    /// its last instruction's overshoot past the present kept as its lead.
    /// The PSG, on the same clock, runs at once for every whole clock of it.
    pub(super) z80_clock: SubClock<Z80Ratio>,
    /// The YM2612's clock against the 68000's, which goes on from line to
    /// line: the YM2612 follows the Z80 where the Z80 writes to it, and is
    /// brought to the present after each step; its lead is where the Z80
    /// took it past the present. It runs at the 68000's rate, so it
    /// carries no credit.
    pub(super) ym_clock: SubClock<YmRatio>,
    /// The PSG's, the Z80's and the YM2612's clocks given in the line,
    /// which a frame stopped part of the way through keeps for a build that
    /// gives them the rest of their line at its end.
    pub(super) psg_clocked: u32,
    pub(super) z80_executed: u32,
    pub(super) ym_clocked: u32,
    /// The Z80's interrupt line as the run has it (copied from and back to
    /// `z80_irq_pending`), and whether the Z80 is traced.
    pub(super) z80_irq: bool,
    pub(super) z80_tracing: bool,
    /// Whether a cartridge is loaded: with none, no 68000 step runs.
    pub(super) has_rom: bool,
    /// The step in progress stopped before its instruction: its first part
    /// (the FIFO's tick, the interrupt) has run, and the next call goes on
    /// from there.
    pub(super) resume: bool,
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

    /// The sound clocks a reset and a load start from: in step with the
    /// master clock since the machine's power-on, so neither keeps a
    /// credit of its own; the Z80 `z80_lead` cycles past the present, as
    /// a savestate keeps it (0 at a reset), and the YM2612 where the Z80
    /// took it, or at the present.
    pub(super) fn derive_sound_clocks(&mut self, z80_lead: i64) {
        self.line.z80_clock =
            SubClock::at(Z80Ratio::default(), self.master_cycles).with_lead(z80_lead);
        let ym_lead = self.line.z80_clock.master_lead().max(0);
        self.line.ym_clock =
            SubClock::at(YmRatio::default(), self.master_cycles).with_lead(ym_lead);
    }

    /// Advance every cartridge coprocessor to the current master cycle. Run
    /// once per frame from every tier so a coprocessor that runs decoupled
    /// from the 68000 (the SSP1601) makes progress even on the play tier.
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
    /// on every line. The debug tiers reach it only while an event or watch
    /// breakpoint is armed ([`retro_core::frame::StepHooks::raised`]).
    fn check_frame_events(&mut self, scanline: u16, vblank: bool, vint: bool, hint: bool) -> bool {
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

    /// The start of the line the frame is on: the VDP steps onto it, the
    /// line's 68000 budget is drawn, the interrupts it raises are requested
    /// and the one the 68000's mask lets through is taken, the line's
    /// events are raised for the debugger, and up to a line of a transfer's
    /// stall is charged before the first step, the PSG clocked and the Z80
    /// run through the interrupt and the stall. The line's counters start
    /// from zero (the sound clock goes on), and the beam's count from what
    /// the start took, so the line's first instruction sees its own line.
    fn start_line<H: StepHooks<Self>>(&mut self, hooks: &mut H) {
        let line = self.frame_line;
        hooks.chip_start("VDP");
        self.bus.vdp_mut().scanline(line as u16);
        hooks.chip_finish(0);
        self.step_master_clocks_accumulated = 0;
        // Exact per-line 68K budget; carries the 4/7-cycle remainder.
        let budget = self.next_scanline_cycles();

        // The vertical interrupt (level 6), and the Z80's interrupt on the
        // same /INT line.
        let vblank = self.bus.vdp().should_trigger_vint();
        if vblank {
            let vint_enabled = (self.bus.vdp().registers()[1] & 0x20) != 0;
            if vint_enabled && !self.bus.vdp().vint_request_delayed() {
                self.cpu.request_interrupt(6);
            }
            self.z80_irq_pending = true;
            self.bus.vdp_mut().ack_vint();
        }

        // The Z80's line falls outside vertical blanking (level-triggered).
        if !self.bus.vdp().in_vblank() {
            self.z80_irq_pending = false;
        }

        // The horizontal interrupt (level 4).
        if self.bus.vdp().should_trigger_hint() {
            self.cpu.request_interrupt(4);
            self.bus.vdp_mut().ack_hint();
        }

        // Refresh VINT level from VDP state (level-triggered)
        self.refresh_vdp_interrupt_level();

        // The interrupt the mask lets through is taken here, its cycles out
        // of the line's budget: the CPU has one cycle count for instructions
        // and interrupts alike.
        let line_start = self.master_cycles;
        let interrupted = self.cpu.pc;
        let level = self.cpu.pending_irq_level;
        let irq_cycles = self.cpu.process_pending_interrupts(&mut self.bus);
        if irq_cycles > 0 {
            self.handle_interrupt_ack(level);
            self.master_cycles += u64::from(irq_cycles);
            hooks.interrupt_taken(self, interrupted);
        }
        // The interrupt events are the interrupts taken here, not those
        // requested: a request the 68000's mask holds back, or the vertical
        // interrupt's that lands later in the line, is taken in a step, which
        // raises its own.
        let vint = irq_cycles > 0 && level == 6;
        let hint = irq_cycles > 0 && level == 4;
        hooks.raised(self, move |m| {
            m.check_frame_events(line as u16, vblank, vint, hint)
        });

        // A pending 68k-bus DMA freezes the CPU: up to one line of it comes
        // out of this line's budget, so the transfer meters line by line.
        self.drain_dma_stall_for_line();
        self.line = LineRun {
            // The overshoot the previous line ran past its budget comes out
            // of this line's.
            grid: line_start.saturating_sub(u64::from(self.frame_cycle_overshoot)),
            budget,
            z80_clock: self.line.z80_clock,
            ym_clock: self.line.ym_clock,
            ..LineRun::default()
        };
        // The interrupt's entry and the transfer's stall are the sound
        // clocks' time too, and the Z80 runs through them (the 68000 is the
        // one held), so its writes there land in their time: the YM2612
        // owes them before the Z80 runs, and then follows it.
        let mut run = self.run_cursor();
        if self.sound_time_passes(&mut run, hooks) && run.z80_clock.owes() {
            self.catch_up_z80(&mut run, hooks);
        }
        self.ym_to_present(&mut run, hooks);
        self.z80_irq_pending = run.z80_irq;
        self.line = run;
        // The beam counts the line from its start: the cycles the line
        // before ran past its end, the interrupt's entry and the transfer's
        // stall are spent already. Each step sets it again after its
        // instruction.
        let spent = (self.master_cycles - self.line.grid) as u32;
        self.bus.vdp_mut().set_hblank_approx(spent, budget);
    }

    /// The end of the line: the Z80 run to the present, the 68000's
    /// overshoot carried into the next line, and the I/O clocked by the
    /// line. The PSG and the YM2612 have had their time at each step.
    fn end_line<H: StepHooks<Self>>(&mut self, hooks: &mut H) {
        let mut run = self.run_cursor();
        let budget = run.budget;

        // The Z80 run to the present, which the 68000's last instruction may
        // have taken past the line's end, so the next line's start finds it
        // there. A held Z80 owes nothing.
        if run.has_rom
            && !self.bus.is_z80_reset()
            && !self.bus.is_z80_bus_requested()
            && run.z80_clock.owes()
        {
            self.catch_up_z80(&mut run, hooks);
        }
        self.z80_irq_pending = run.z80_irq;

        // The 68000's overshoot past the budget, which the next line's
        // budget pays (a line with no ROM to run spends nothing).
        let spent = self.master_cycles - run.grid;
        self.frame_cycle_overshoot = spent.saturating_sub(u64::from(budget)) as u32;

        // I/O clocking
        self.bus.io_mut().clock(budget);
        self.line = run;
    }

    /// The frame's end, after its last line: the cartridge's chips caught
    /// up, the frame's sound made (the converter and the chips' sample
    /// phase advance on a headless frame too: they are machine state; only
    /// the mix and the host's ring wait on presenting), and the picture
    /// handed to the host when presenting.
    fn end_frame<H: StepHooks<Self>>(&mut self, hooks: &mut H) {
        // Advance cartridge coprocessors to end-of-frame (SSP1601 seam).
        hooks.chip_start("cartridge");
        self.catch_up_cartridge();
        hooks.chip_finish(0);
        hooks.chip_start("audio mix");
        self.generate_and_mix_audio(self.present);
        self.end_stem_frame();
        self.end_sound_log_frame();
        hooks.chip_finish(0);
        if self.present {
            hooks.chip_start("framebuffer");
            self.update_cached_framebuffer();
            hooks.chip_finish(0);
        }
    }

    /// The line's counters as a run of steps starts from them: the Z80's
    /// interrupt line and trace flag as the machine has them now (a host's
    /// step of the Z80 may have taken its interrupt since), and whether a
    /// cartridge is loaded.
    #[inline(always)]
    fn run_cursor(&self) -> LineRun {
        let mut run = self.line;
        run.z80_irq = self.z80_irq_pending;
        run.z80_tracing = self.z80_trace_armed;
        run.has_rom = self.bus.cartridge().is_some();
        run
    }

    /// Drop the frame in progress: a reset starts the machine over, and a
    /// load first, before it brings back the state's own.
    pub(super) fn drop_frame(&mut self) {
        self.frame_open = false;
        self.frame_line = 0;
        self.line_started = false;
        self.line = LineRun::default();
        self.held_at_capture_boundary = false;
        self.capture_boundary_requested = false;
        self.interrupt_cycles = 0;
    }
}

/// The Mega Drive's frame is a count of lines: each line's start takes the
/// interrupts it raises and gives the 68000 its budget, steps run until the
/// budget is spent, and the line's end gives the Z80 and the sound chips
/// the rest of their line. The frame ends after its last line.
impl FrameMachine for GenesisEmulator {
    type Run = LineRun;

    #[inline(always)]
    fn now(&self) -> u64 {
        self.master_cycles
    }

    /// Go on with the frame in progress, or open the next one: the players'
    /// input taken once a frame, as a whole frame always has (a frame
    /// stopped part of the way through keeps the input it started with),
    /// and the frame being drawn numbered (it is counted once it ends).
    fn enter_frame(&mut self) -> u64 {
        if !self.frame_open {
            // Sync input state from the input manager into I/O devices
            let p0 = self.input_manager.get_port_state(0);
            let p1 = self.input_manager.get_port_state(1);
            self.bus.io_mut().sync_input(p0, p1);
            self.bus
                .vdp_mut()
                .set_frame_being_drawn(self.frame_count + 1);
            self.frame_line = 0;
            self.line_started = false;
            self.frame_open = true;
        }
        // The frame ends on its line count.
        u64::MAX
    }

    /// One move a call: the line's end once its budget is spent (and the
    /// frame's end after its last line); then a hold in front of the first
    /// line of vertical blanking when a capture boundary was asked for; else
    /// the next line's start.
    fn handle_due<H: StepHooks<Self>>(&mut self, _frame_end: u64, hooks: &mut H) -> Due {
        let lines = self.region.scanlines();
        if self.line_started {
            // A line whose time is not spent: its steps go on (none run
            // without a ROM, so then it is always spent).
            if self.bus.cartridge().is_some()
                && self.master_cycles < self.line.grid + u64::from(self.line.budget)
            {
                return Due::Continue;
            }
            self.end_line(hooks);
            self.line_started = false;
            self.frame_line += 1;
            if self.frame_line >= lines {
                self.end_frame(hooks);
            }
        }
        if self.frame_line >= lines {
            return Due::FrameEnd;
        }
        let boundary = u32::from(self.bus.vdp().first_vblank_line());
        if self.capture_boundary_requested
            && !self.held_at_capture_boundary
            && self.frame_line == boundary
        {
            // The picture is drawn: the host gets it now; the cartridge, the
            // sound and the frame count wait for the frame's end.
            self.held_at_capture_boundary = true;
            if self.present {
                self.update_cached_framebuffer();
            }
            return Due::Held;
        }
        self.held_at_capture_boundary = false;
        self.start_line(hooks);
        self.line_started = true;
        Due::Continue
    }

    /// The line's end, once its line has started; now while the next line
    /// has not (a frame just opened, a frame held at its boundary), so a
    /// host step starts it before its instruction. With no ROM loaded no
    /// step runs, so there is no deadline: each pass of the loop moves one
    /// line, and a host step's handling runs once on each side of its step.
    #[inline(always)]
    fn next_deadline(&self, _frame_end: u64) -> Option<u64> {
        if self.bus.cartridge().is_none() {
            None
        } else if self.line_started {
            Some(self.line.grid + u64::from(self.line.budget))
        } else {
            Some(self.master_cycles)
        }
    }

    #[inline(always)]
    fn begin_run(&mut self) -> LineRun {
        self.run_cursor()
    }

    #[inline(always)]
    fn end_run(&mut self, run: LineRun) {
        self.z80_irq_pending = run.z80_irq;
        self.line = run;
    }

    #[inline(always)]
    fn machine_step<H: StepHooks<Self>>(
        &mut self,
        run: &mut LineRun,
        hooks: &mut H,
    ) -> Option<Stepped> {
        self.step_main_cpu(run, hooks)
    }

    fn close_frame(&mut self) {
        self.frame_count += 1;
        self.frame_open = false;
        self.frame_line = 0;
    }

    /// A hold at the capture boundary is left by the next
    /// [`FrameMachine::handle_due`], which starts the line the hold stood in
    /// front of, as a frame call leaves it.
    fn enter_step<H: StepHooks<Self>>(&mut self, _hooks: &mut H) -> u64 {
        FrameMachine::enter_frame(self)
    }

    fn frame_in_progress(&self) -> bool {
        self.frame_open
    }

    fn held_at_capture_boundary(&self) -> bool {
        self.held_at_capture_boundary
    }

    fn request_capture_boundary(&mut self, on: bool) {
        self.capture_boundary_requested = on;
    }

    fn set_presenting(&mut self, on: bool) {
        self.present = on;
    }

    /// The VDP draws into its own buffer; the host reads a copy of it,
    /// made at a stop as at a frame's end.
    fn present_stop(&mut self) {
        self.update_cached_framebuffer();
    }
}
