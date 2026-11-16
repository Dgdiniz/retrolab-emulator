//! The frame: the raster events armed each frame and what each does, the
//! frame loops that run them, and a frame held part of the way through.

use retro_core::*;
use wdc65816::instructions::FLAG_I;

use super::debug::expr_context;
use super::{SnesEmulator, NMI_DELIVERY_MAX_INSNS};
use crate::debugger::{SnesEvent, MAIN_CPU};
use crate::irq::schedule_irq_events;
use crate::scheduler::{schedule_frame_events, EventKind};

/// Master clocks the auto-joypad read holds the HVBJOY busy flag
/// (`$4212` bit 0) after it starts at V-blank. Hardware spends roughly
/// the first few scanlines of V-blank shifting the 16 bits; we latch
/// the snapshot at once (see `SnesBus::auto_joypad_read`) and clear the
/// busy flag this many cycles later so a polling game still observes
/// the window. ~3.9 scanlines at 1364 mclks/line.
const AUTO_JOYPAD_CYCLES: u64 = 5360;

impl SnesEmulator {
    /// Decompose a master cycle into the `(v_scanline, h_dot)` raster
    /// position **relative to the current frame base**.
    ///
    /// The scheduler arms every per-frame event, `VBlankStart`/NMI,
    /// `RenderPos`, `HBlankStart`, the H/V IRQ triggers, at
    /// `frame_base_cycle + line * cycles_per_scanline + dot * 4`. The
    /// CPU-visible raster counters (`$213D` OPVCT via the `$2137` SLHV
    /// latch, plus the trace V/H columns) MUST be decoded against the
    /// same `frame_base_cycle` anchor. `frame_base_cycle` re-anchors to
    /// the live `master_cycles` at each frame boundary and is *not* a
    /// multiple of `cycles_per_frame`, it carries the overshoot of the
    /// last instruction or DMA stall that crossed the boundary. Decoding
    /// the vcounter from an absolute `master_cycles % cycles_per_frame`
    /// grid therefore drifts away from the event timeline by that
    /// accumulated overshoot, so the scanline a game reads from `$213D`
    /// disagrees with the line its NMI/IRQ actually fired on. A
    /// vblank-paced raster routine (e.g. Chrono Trigger's per-scanline
    /// queue walk) then sees the wrong vcounter, takes the wrong branch,
    /// and the `RenderPos` handler writes the framebuffer at the wrong
    /// row. Anchoring here keeps the readout and the event timeline on
    /// one clock, as on the hardware, where both come from one counter.
    #[inline]
    pub(super) fn raster_position(&self, master_cycle: u64) -> (u16, u16) {
        let cps = self.timing.cycles_per_scanline as u64;
        let scanlines = self.timing.scanlines_per_frame as u64;
        let rel = master_cycle.saturating_sub(self.frame_base_cycle);
        let v = ((rel / cps) % scanlines) as u16;
        let h = ((rel % cps) / 4) as u16;
        (v, h)
    }

    /// Test-only convenience wrapper that dispatches an event at the live
    /// `master_cycles`. The frame loops call [`Self::handle_event_at`]
    /// directly with each event's true scheduled `at_cycle`; unit tests use
    /// this shorthand when the scheduled cycle is irrelevant to the case.
    #[cfg(test)]
    pub(super) fn handle_event(&mut self, kind: EventKind) {
        self.handle_event_at(self.master_cycles, kind);
    }

    /// Per-event dispatcher. Each variant drives the subsystem that
    /// owns it (PPU rendering, the HVBJOY flags, the DMA/HDMA engine,
    /// NMI/IRQ delivery, the APU catch-up):
    ///
    /// - `HBlankStart`: raise the HVBJOY HBlank flag ($4212 bit 6) at
    ///   dot 274; `RenderPos` lowers it when the active display resumes.
    /// - `RenderPos`: render the current scanline. The scheduler
    ///   fires this at dot 22 of lines 1..=240; the PPU drops the
    ///   lines past the active region.
    /// - `VBlankStart`: raise NMI through SystemRegs when NMITIMEN
    ///   bit 7 is set; flip the HVBJOY VBlank flag; sample the
    ///   controllers and run the auto-joypad read when NMITIMEN bit 0
    ///   is set (scheduling `AutoJoypadDone` to clear the busy flag).
    /// - `VBlankEnd`: clear the RDNMI/HVBJOY VBlank flags at the top of
    ///   the frame (V:0), not on the last scanline, so a poll that waits
    ///   for VBlank to end sees the hardware-correct edge.
    /// - `AutoJoypadDone`: lower the HVBJOY auto-joypad busy flag.
    /// - `HdmaInit`: reload every HDMA-enabled channel's
    ///   table pointer + line counter at the top of the frame.
    /// - `HdmaTrigger`: drive a single line of HDMA per
    ///   active channel through the DMA engine.
    /// - `IrqV / IrqH / IrqHV`: raise the 65C816's IRQ line. The events
    ///   are armed for each frame and re-armed after an IRQ register
    ///   write, not here.
    /// - `FrameEnd`: catch the APU up to the frame boundary so the
    ///   SPC700 and sample clock advance even across a frame with no
    ///   mailbox traffic. (`ApuCatchup` is defined but unscheduled,
    ///   port access plus this hook already bound staleness to one
    ///   frame.)
    ///
    /// Raster-dependent behaviour (`RenderPos` scanline, `HdmaInit` /
    /// `HdmaTrigger` active-scan gate snapshot) is decoded from `at_cycle`,
    /// the cycle the event was scheduled for, not the live
    /// `master_cycles`. The two agree to within one instruction on the hot
    /// path; they diverge only when a general DMA (or any single step) folds
    /// many scanlines into one jump, deferring a batch of events. Cycle
    /// *accounting* (HDMA stolen cycles, APU/IRQ bookkeeping) still uses
    /// `master_cycles`: only the raster attribution reads `at_cycle`.
    pub(super) fn handle_event_at(&mut self, at_cycle: u64, kind: EventKind) {
        match kind {
            EventKind::RenderPos => {
                // The line that owns this RenderPos, decoded against
                // `frame_base_cycle` (the same anchor the event was
                // armed on) from the event's scheduled cycle, so a
                // RenderPos deferred behind a long mid-screen DMA still
                // renders its own scanline instead of the late line the
                // post-DMA `master_cycles` points at.
                // The PPU itself drops any out-of-range scanline via
                // `Ppu::render_scanline`'s in_active_scanline gate,
                // overscan toggles flip the gate without a reschedule.
                let (scanline, _h_dot) = self.raster_position(at_cycle);
                // RenderPos lands at dot 22, the active display has
                // resumed, so the line is no longer in horizontal blank.
                self.bus.system_regs_mut().set_hblank(false);
                self.bus
                    .ppu_mut()
                    .render_scanline(scanline, &mut self.framebuffer);
                if self.debug_armed && self.debugger.check_event(SnesEvent::Scanline(scanline)) {
                    self.pending_break = true;
                }
            }
            EventKind::HBlankStart => {
                // Dot 274: raise the HVBJOY HBlank flag ($4212 bit 6).
                // Cleared at the next RenderPos (active display resumes).
                // Games that gate work on HBlank (Chrono Trigger spins
                // on `lda $4212 / bit #$40 / beq` during its NMI handler)
                // depend on this edge to make progress.
                self.bus.system_regs_mut().set_hblank(true);
            }
            EventKind::HdmaInit => {
                // HDMA runs at a fixed raster dot (init at dot 6, per-line
                // at dot 276, both in H-blank), independent of where the
                // CPU last updated the cached raster snapshot. Re-anchor it
                // from the event's scheduled cycle so any PPU-register write
                // the transfer performs gates its active-scan CGRAM/OAM
                // redirect against the true H dot, even when a long general
                // DMA deferred this trigger and the live `master_cycles`
                // has since walked deep into active display. Without this a
                // transfer firing while the snapshot sits inside dots
                // 22..274 is wrongly redirected to the internal address
                // (colour 0), which would corrupt Yoshi's Island's per-line
                // water palette into the backdrop.
                let (v, h) = self.raster_position(at_cycle);
                self.bus.set_raster_snapshot(h, v);
                // Top-of-frame HDMA setup: reload table pointers and
                // line counters for every HDMA-enabled channel. The
                // stolen cycles fold into the frame budget like a CPU
                // stall.
                let stolen = self.bus.hdma_init_frame();
                self.master_cycles += stolen;
                self.bus
                    .system_regs_mut()
                    .tick(stolen.min(u32::MAX as u64) as u32);
                if self.debug_armed
                    && (self.debugger.check_event(SnesEvent::HdmaStart)
                        | self.drain_ppu_debug_events())
                {
                    self.pending_break = true;
                }
            }
            EventKind::HdmaTrigger => {
                // Re-anchor the raster snapshot to this trigger's own
                // scheduled dot (276, in H-blank) so per-line HDMA writes to
                // CGRAM/OAM gate the active-scan redirect against the real H
                // dot rather than the live `master_cycles`, which a long
                // general DMA can push a batch of deferred triggers deep into
                // active display. See the HdmaInit arm above for the Yoshi's
                // Island failure this cures.
                let (v, h) = self.raster_position(at_cycle);
                self.bus.set_raster_snapshot(h, v);
                // Per-scanline HDMA: one transfer pattern per active
                // channel, then advance the line counters.
                let stolen = self.bus.hdma_run_line();
                self.master_cycles += stolen;
                self.bus
                    .system_regs_mut()
                    .tick(stolen.min(u32::MAX as u64) as u32);
                // A palette colour or a tile the line's HDMA wrote: the
                // break waits for the rest of the batch.
                if self.debug_armed && self.drain_ppu_debug_events() {
                    self.pending_break = true;
                }
            }
            EventKind::VBlankStart => {
                // A frame held at the capture boundary finished its
                // picture when it stopped in front of this event; only
                // the start of the vertical blank is left.
                if self.held_at_capture_boundary {
                    self.held_at_capture_boundary = false;
                } else {
                    self.finish_picture();
                }
                // Interlace field toggle, alternates once per frame
                // even when the interlace bit itself is off, which
                // mirrors how real silicon drives `$213F` bit 7.
                self.bus.ppu_mut().toggle_field();
                // Entering VBlank re-anchors the OAM address pointer to
                // its base (a side effect of sprite eval finishing on
                // real hardware) when the display is active.
                self.bus.ppu_mut().oam_reset_at_vblank();
                // Latch the NMI flag ($4210 bit 7) regardless of the
                // enable bit and raise the HVBJOY VBlank flag. The CPU
                // NMI signal itself is gated by NMITIMEN bit 7.
                let regs = self.bus.system_regs_mut();
                regs.set_nmi_flag(true);
                regs.set_vblank(true);
                let nmi_enabled = regs.nmi_enabled();
                if nmi_enabled {
                    // Arm poll-aware delivery rather than firing the NMI
                    // straight away. The flag is already latched above with
                    // `nmi_flag_observed` reset, so the next main-thread
                    // `$4210` read this vblank is what releases the NMI (or
                    // the fallback window expires). See `nmi_deliver_armed`.
                    self.nmi_deliver_armed = true;
                    self.nmi_deliver_countdown = NMI_DELIVERY_MAX_INSNS;
                }
                // Event breakpoints: entering V-blank, and the NMI when
                // it's actually delivered. Only checked when armed.
                if self.debug_armed {
                    if self.debugger.check_event(SnesEvent::VBlank) {
                        self.pending_break = true;
                    }
                    if nmi_enabled && self.debugger.check_event(SnesEvent::Nmi) {
                        self.pending_break = true;
                    }
                }
                // Sample the controllers into the pads here, at the
                // start of VBlank, where the hardware's auto-read does.
                // This keeps manual-serial games fresh too, not just
                // auto-read.
                self.refresh_controllers_from_input();
                // Auto-joypad read (NMITIMEN bit 0): latch both pads
                // into JOY1/JOY2 and hold the busy flag for the read
                // window via a scheduled AutoJoypadDone.
                if self.bus.system_regs().nmitimen() & 0x01 != 0 {
                    self.bus.auto_joypad_read();
                    self.scheduler.schedule(
                        self.master_cycles + AUTO_JOYPAD_CYCLES,
                        EventKind::AutoJoypadDone,
                    );
                }
            }
            EventKind::VBlankEnd => {
                // Clears the RDNMI ($4210 bit 7) NMI flag latch and the
                // HVBJOY ($4212 bit 7) VBlank flag. Armed at the top of
                // the frame (line 0 / V:0), so it clears the flags that
                // the *previous* frame's VBlankStart raised: they stay
                // set through the last scanline and drop only when the
                // V-counter wraps to 0, matching hardware. (Clearing on
                // the last scanline instead would drop the VBlank flag one
                // line early and break the H/V-counter-latch sub-test of
                // the Electronics Test in Nintendo's SNES Test Program,
                // which waits for VBlank to end and then expects to latch
                // V=0.)
                let regs = self.bus.system_regs_mut();
                regs.set_nmi_flag(false);
                regs.set_vblank(false);
            }
            EventKind::IrqV { .. } | EventKind::IrqH { .. } | EventKind::IrqHV { .. } => {
                while self.irq_cycles.front().is_some_and(|&at| at <= at_cycle) {
                    self.irq_cycles.pop_front();
                }
                // Raise the level-triggered IRQ line. `step_cpu_internal`
                // mirrors it into the CPU each step; the handler acks by
                // reading $4211 (TIMEUP), which lowers the line again.
                //
                // A `$4211` read inside the instruction during which this
                // IRQ rose has seen and acknowledged it already. The 5A22
                // latched the interrupt before that read lowered the line,
                // so the line still goes up when the I flag lets the CPU
                // take it (the handler then reads bit 7 set, where the chip
                // would give it clear). With I set the read was all the IRQ
                // left, and a later read must not see it a second time.
                let seen_early = self.irq_read_early == Some(at_cycle);
                if seen_early {
                    self.irq_read_early = None;
                }
                if !seen_early || (self.cpu.p & FLAG_I) == 0 {
                    self.bus.system_regs_mut().set_irq_flag(true);
                }
                if self.debug_armed && self.debugger.check_event(SnesEvent::Irq) {
                    self.pending_break = true;
                }
            }
            EventKind::AutoJoypadDone => {
                // The auto-joypad read window elapsed: drop the HVBJOY
                // busy flag ($4212 bit 0) so a polling game proceeds.
                self.bus.auto_joypad_finish();
            }
            EventKind::FrameEnd => {
                // Bound APU staleness to one frame even when no mailbox
                // access happened this frame: advance the SPC700 and the
                // 32 kHz sample clock to the frame boundary, synthesising
                // any S-DSP samples that came due. Then pull those 32 kHz
                // samples through the resampler into the host audio ring.
                self.bus.apu_mut().catch_up_to(self.master_cycles);
                // The APU catch-up above advances all serialized audio
                // state; only the host-ring drain (resample) is
                // presentation, so a headless frame skips just that.
                if self.present {
                    self.drain_apu_to_host_audio();
                }
                // Keep a cartridge coprocessor (DSP-1, ...) ticking across
                // a frame with no register traffic, bounding its catch-up
                // staleness to one frame just like the APU above.
                self.bus.catch_up_coprocessor(self.master_cycles);
            }
            _ => {}
        }
    }

    /// Finish the frame's picture once its active display is drawn: the
    /// first work of `VBlankStart`, which a frame held at the capture
    /// boundary has already done when it stopped in front of that event.
    ///
    /// Normalize the frame for host display: when any line was hires the
    /// reported width is 512, so lo-res lines (HDMA mid-frame mode sweeps)
    /// are doubled in place to fill the full width. It runs before the
    /// display-size sync so the framebuffer and `DisplayInfo` agree. It is
    /// purely a framebuffer post-process (no serialized state), so a
    /// headless frame skips it. The sync then mirrors the live PPU
    /// dimensions into the cached `DisplayInfo`, so the next host poll of
    /// `get_display_info` reflects any hires / overscan toggles of the
    /// frame just drawn. Neither step reads the interlace field or the OAM
    /// address the rest of `VBlankStart` moves.
    fn finish_picture(&mut self) {
        if self.present {
            self.bus.ppu().finalize_frame(&mut self.framebuffer);
        }
        self.bus.ppu_mut().note_picture_finished();
        self.sync_display_info();
    }

    /// Cancel the pending H/V IRQ events and re-arm them from the
    /// current frame base, keeping only triggers at or after the
    /// current master cycle (so a mid-frame HTIME / VTIME / NMITIMEN
    /// rewrite never re-fires a trigger for a scanline already drawn).
    pub(super) fn reschedule_irq_events(&mut self) {
        self.scheduler.cancel_matching(EventKind::is_irq);
        self.arm_irq_events(self.frame_base_cycle, self.master_cycles);
    }

    /// Push the H/V IRQ events for a frame into the scheduler from the
    /// live NMITIMEN / HTIME / VTIME register state. `min_cycle`
    /// filters out triggers whose raster point has already passed.
    pub(super) fn arm_irq_events(&mut self, base_cycle: u64, min_cycle: u64) {
        let (nmitimen, htime, vtime) = {
            let regs = self.bus.system_regs();
            (regs.nmitimen(), regs.htime(), regs.vtime())
        };
        schedule_irq_events(
            &mut self.scheduler,
            base_cycle,
            nmitimen,
            htime,
            vtime,
            self.timing,
            min_cycle,
        );
        self.collect_irq_cycles();
    }

    /// Rebuild [`Self::irq_cycles`] from the H/V IRQ events the
    /// scheduler holds.
    pub(super) fn collect_irq_cycles(&mut self) {
        self.irq_cycles.clear();
        self.irq_cycles
            .extend(self.scheduler.cycles_of(EventKind::is_irq));
    }

    /// Whether a frame is in progress: its end has not been handled yet,
    /// so the `FrameEnd` it was armed with, at `frame_base_cycle` plus one
    /// frame, is still queued. A frame call that stopped early (at a
    /// breakpoint, at a pause, at the capture boundary), a debugger step, a
    /// reset (frame 0 opens at cycle 0) or a savestate taken mid-frame
    /// leaves one in progress; a frame call that ran its frame to the end
    /// does not, since its tail armed the next frame from the cycle it
    /// ended on. Read off the saved scheduler, so a state loaded mid-frame
    /// goes on with its frame too.
    ///
    /// The clock alone cannot tell: one instruction, a long general DMA,
    /// can carry it past both the frame's `VBlankStart` and its end, and a
    /// frame stopped at its capture boundary there is still in progress.
    pub fn frame_in_progress(&self) -> bool {
        self.scheduler.earliest(&EventKind::FrameEnd)
            == Some(self.frame_base_cycle + self.timing.cycles_per_frame())
    }

    /// Go on with the frame in progress, or open the next one, and return
    /// the master cycle at which the frame ends.
    ///
    /// The next frame opens where the tail of the previous one armed its
    /// events: its queued `FrameEnd` less one frame. That is the present
    /// cycle on every path that runs frames, since nothing runs between a
    /// frame's tail and the next call; bare CPU steps in between (which
    /// handle no events) are counted inside the frame, so its raster
    /// positions still decode against the base its events were armed on.
    /// With no `FrameEnd` queued, the frame opens at the present cycle.
    ///
    /// Re-anchoring a frame that is in progress would decode its armed
    /// events against the wrong base (every raster position off by the
    /// cycles already run) and give it a whole frame's budget again, so a
    /// frame stopped at a breakpoint would run long and draw at the wrong
    /// rows; going on with it keeps the frame exactly as one run whole.
    pub(super) fn enter_frame(&mut self) -> u64 {
        let frame_cycles = self.timing.cycles_per_frame();
        let frame_end = self.frame_base_cycle + frame_cycles;
        // Short of the frame's end the frame is in progress: its
        // `FrameEnd` is handled only once the clock reaches it. The
        // debugger's steps come through here once an instruction, and this
        // spares them the walk over the queue.
        if self.master_cycles < frame_end {
            return frame_end;
        }
        match self.scheduler.earliest(&EventKind::FrameEnd) {
            Some(end) if end == frame_end => {}
            Some(end) => self.frame_base_cycle = end.saturating_sub(frame_cycles),
            None => self.frame_base_cycle = self.master_cycles,
        }
        self.frame_base_cycle + frame_cycles
    }

    /// Handle the scheduler events due by the present cycle, in order.
    ///
    /// For a capture run the drain stops in front of the frame's
    /// `VBlankStart`, which stays queued with every event after it: the
    /// picture is finished there and the core is held at the capture
    /// boundary, which the `true` return reports. The events left queued
    /// are the ones the same drain would have handled after
    /// `VBlankStart`; the first drain once the frame goes on handles them
    /// (see [`Self::release_capture_boundary`]). No event a handler
    /// schedules can fall due before them, since a handler only ever
    /// schedules ahead of the present cycle.
    #[inline]
    fn handle_due_events(&mut self) -> bool {
        if self.capture_boundary_requested | self.held_at_capture_boundary {
            return self.handle_capture_events();
        }
        let events = self.scheduler.drain_up_to_with_cycles(self.master_cycles);
        for (at_cycle, ev) in events {
            self.handle_event_at(at_cycle, ev);
        }
        false
    }

    /// The drain while a capture is under way (see
    /// [`Self::handle_due_events`]): the rest of a split batch when the
    /// core is held at the boundary, else a drain that stops in front of
    /// the frame's `VBlankStart`.
    fn handle_capture_events(&mut self) -> bool {
        if self.held_at_capture_boundary {
            self.release_capture_boundary();
            return false;
        }
        let bound = self.master_cycles;
        let (events, at_vblank) = self
            .scheduler
            .drain_up_to_before(bound, |kind| matches!(kind, EventKind::VBlankStart));
        for (at_cycle, ev) in events {
            self.handle_event_at(at_cycle, ev);
        }
        if at_vblank {
            self.finish_picture();
            self.held_at_capture_boundary = true;
            self.capture_boundary_cycle = bound;
        }
        at_vblank
    }

    /// Handle the rest of the batch the capture boundary split: the
    /// events that were due when the stopping drain ran, up to its own
    /// cycle, the frame's `VBlankStart` first (whose handler ends the
    /// hold). Events that came due since, which an HDMA transfer's cycles
    /// can bring, wait for the next drain, as they would in a frame run
    /// whole: the queue then sees the same pops and pushes in the same
    /// order, and a savestate, which writes the queue in its stored order,
    /// comes out byte for byte the same.
    fn release_capture_boundary(&mut self) {
        let events = self
            .scheduler
            .drain_up_to_with_cycles(self.capture_boundary_cycle);
        for (at_cycle, ev) in events {
            self.handle_event_at(at_cycle, ev);
        }
    }

    /// Before an instruction runs from the capture boundary, handle what
    /// the frame loop handles first there: the rest of the split batch
    /// (the start of the vertical blank first), then every event due by
    /// now, draining again, as the loop does, while the cycles an HDMA
    /// transfer takes bring more events due; then, if the clock is already
    /// past the frame's end (a long general DMA crossed both), close the
    /// frame as the loop would. A step from the boundary is therefore the
    /// step the frame loop would take: the same instruction against the
    /// same state.
    pub(super) fn leave_capture_boundary(&mut self) {
        if !self.held_at_capture_boundary {
            return;
        }
        self.release_capture_boundary();
        while self
            .scheduler
            .peek_cycle()
            .is_some_and(|at| at <= self.master_cycles)
        {
            let events = self.scheduler.drain_up_to_with_cycles(self.master_cycles);
            for (at_cycle, ev) in events {
                self.handle_event_at(at_cycle, ev);
            }
        }
        if self.master_cycles >= self.frame_base_cycle + self.timing.cycles_per_frame() {
            self.end_of_frame_bookkeeping();
            self.frame_base_cycle = self.master_cycles;
        }
    }

    /// Run one frame with no debug instrumentation (the `Fast` tier).
    /// This is the hot path when the debugger panel is idle.
    ///
    /// Each pass handles the events due first, then runs CPU instructions
    /// up to the next event or the frame's end. A frame picked up where an
    /// earlier call stopped may have events due from before the stop, the
    /// clock even past the frame's end, so they are handled before the
    /// frame's end is checked.
    pub(super) fn run_frame_fast(&mut self) {
        let frame_end = self.enter_frame();

        loop {
            if self.handle_due_events() {
                return;
            }
            if self.master_cycles >= frame_end {
                break;
            }
            // Run CPU instructions until either (a) the next
            // scheduler event fires or (b) we've used the whole
            // frame budget. `target` is the lesser of the two.
            let next_event = self.scheduler.peek_cycle().unwrap_or(frame_end);
            let target = next_event.min(frame_end);

            while self.master_cycles < target {
                self.step_cpu_internal();
            }
        }

        self.end_of_frame_bookkeeping();
    }

    /// Headless frame for netplay rollback / catch-up: the full
    /// deterministic simulation with the host-facing presentation steps
    /// (framebuffer post-process + 32 kHz -> 44.1 kHz audio drain)
    /// suppressed. Runs the same `Fast`-tier loop as
    /// [`Self::run_frame_fast`], including the per-scanline PPU render
    /// (which updates the serialized sprite range/time-over flags), the
    /// interlace field toggle, the OAM re-anchor, and the APU + coprocessor
    /// catch-up, so it produces the exact same machine state (and
    /// `frame_count`) as a presented frame. Re-simulating a mispredicted
    /// span therefore leaves the emulator byte-identical to a live run,
    /// which [`Self::state_hash`] verifies.
    pub fn run_frame_headless(&mut self) {
        self.present = false;
        self.run_frame_fast();
        self.present = true;
    }

    /// Whether the frame call just made stopped before its frame's end, at
    /// the capture boundary or at a breakpoint (which pauses the debugger,
    /// even one that hit in the batch that ended the frame).
    pub(super) fn frame_call_stopped(&self) -> bool {
        self.frame_in_progress() || self.debugger.get_execution_mode() != ExecutionMode::Running
    }

    /// Finish the frame held at the capture boundary on the loop a player
    /// runs, whatever the debugger is doing: the fast loop, with nothing of
    /// the debugger armed, as a frame call with no breakpoint and no trace
    /// runs it. No breakpoint, watchpoint or event can stop it before the
    /// frame's end, and nothing of it is traced or counted. The debugger's
    /// hooks are armed again after, as its breakpoints and traces ask, and
    /// its cached CPU states show where the frame ended. Returns whether
    /// there was a frame to finish.
    pub fn finish_held_frame(&mut self) -> bool {
        if !self.held_at_capture_boundary {
            return false;
        }
        self.disarm_debugger_hooks();
        self.run_frame_fast();
        self.sync_spc_trace_enable();
        self.sync_cartridge_trace_enable();
        self.sync_debug_arm();
        self.refresh_debugger_state();
        true
    }

    /// Run the frame in progress, or the next one, on the fast or the
    /// instrumented loop; the tier names which.
    pub(super) fn run_frame_on_tier(&mut self, tier: ExecutionTier) {
        match tier {
            ExecutionTier::Fast => self.run_frame_fast(),
            ExecutionTier::LightDebug => self.run_frame_instrumented(),
            ExecutionTier::FullDebug => self.run_frame_full(),
        }
    }

    /// Run one frame with per-instruction debug hooks (the `LightDebug`
    /// tier). Before each 65C816 instruction we check execute breakpoints,
    /// record coverage, and, when 65C816 tracing is enabled, capture a
    /// binary trace entry; after it, keep the call stack and check the
    /// watchpoints and the events. Everything the debugger reads for
    /// itself goes through the bus's side-effect-free peek, so a frame run
    /// here leaves the machine as the fast loop leaves it.
    ///
    /// A breakpoint hit pauses the debugger (refreshing its CPU states) and
    /// returns mid-frame; the next frame call goes on with the same frame
    /// (see [`Self::enter_frame`]), so a frame broken into pieces ends where
    /// one run whole ends, with the same state. After a stop at an execute
    /// breakpoint, the instruction there runs first, unchecked, so running
    /// on goes on from the stop. Each pass handles the events due first, as
    /// [`Self::run_frame_fast`] does, then runs instructions up to the next
    /// event.
    pub(super) fn run_frame_instrumented(&mut self) {
        // An event in the batch that reached the capture boundary may have
        // matched an event breakpoint. The frame loop honours a match once
        // the whole batch is handled, and the events after the boundary
        // are the rest of that batch, so the match waits for them.
        if !self.held_at_capture_boundary {
            self.pending_break = false;
        }
        let frame_end = self.enter_frame();

        loop {
            if self.handle_due_events() {
                // Held at the capture boundary. The traces the APU and the
                // coprocessor captured so far are flushed as at a frame's
                // end; a breakpoint match waits for the rest of the batch.
                self.drain_spc_trace();
                self.drain_cartridge_trace();
                return;
            }
            // Inline events (VBlank/NMI/IRQ/HDMA/scanline set `pending_break`)
            // and the FrameEnd APU catch-up's DSP-register writes are checked
            // after the event batch drains.
            let hit = self.debug_armed && (self.pending_break | self.drain_apu_dsp_events());
            if self.master_cycles >= frame_end {
                // The batch held the frame's end: the frame closes before a
                // pause, so the next call opens the next frame.
                self.end_of_frame_bookkeeping();
                if hit {
                    self.finish_paused_frame();
                    return;
                }
                break;
            }
            if hit {
                self.finish_paused_frame();
                return;
            }

            let next_event = self.scheduler.peek_cycle().unwrap_or(frame_end);
            let target = next_event.min(frame_end);

            while self.master_cycles < target {
                let pc24 = ((self.cpu.pbr as u32) << 16) | self.cpu.pc as u32;
                let stop = {
                    let mut context = expr_context(&self.cpu, &self.bus);
                    self.debugger
                        .check_execute(MAIN_CPU, pc24, Some(&mut context))
                };
                if stop {
                    self.finish_paused_frame();
                    return;
                }
                self.debugger.record_coverage(MAIN_CPU, pc24);
                if self.debugger.tracing(MAIN_CPU) {
                    let entry = self.capture_trace_65816();
                    self.debugger.record_trace(MAIN_CPU, &entry);
                }
                // Drop what was recorded between two instructions (a
                // host's write while the frame was stopped) so the
                // watchpoints see only this instruction's own accesses.
                if self.debug_armed {
                    self.bus.clear_debug_events();
                }
                let start = self.step_start_65816(pc24);
                self.step_cpu_internal();
                self.maintain_call_stack_65816(start);
                // Watchpoints + PPU/APU write events (recorded by the bus),
                // DSP-register events (recorded by the APU during a mailbox
                // catch-up), and the inline DMA-start event all funnel into
                // a single pause check. Bitwise-or so every match bumps its
                // hit count before we bail.
                if self.debug_armed
                    && (self.drain_bus_debug_events()
                        | self.drain_apu_dsp_events()
                        | self.pending_break)
                {
                    self.finish_paused_frame();
                    return;
                }
            }
        }

        self.drain_spc_trace();
        self.drain_cartridge_trace();
    }

    /// The `FullDebug` tier. Reached only when the execution mode is not
    /// `Running` (paused or stepping). `Paused` just refreshes the cached
    /// state; the step modes advance one 65C816 instruction and pause.
    /// Step-over and step-out are not call-stack-aware: all three step
    /// modes single-step.
    pub(super) fn run_frame_full(&mut self) {
        if self.debugger.get_execution_mode() == ExecutionMode::Paused {
            self.refresh_debugger_state();
            return;
        }
        self.debug_single_step_65816();
        self.debugger.set_execution_mode(ExecutionMode::Paused);
    }

    /// Drain every scheduler event due at the current master cycle and,
    /// when the instruction just executed carried the clock past the frame
    /// boundary, run the end-of-frame tail and re-anchor the frame base.
    ///
    /// Single-stepping shares this with the frame loop so that one stepped
    /// instruction stays event-consistent: the PPU renders, the HVBJOY
    /// flags toggle, NMI/IRQ/HDMA fire. The frame-boundary close is the
    /// load-bearing part, `schedule_frame_events` only arms the *current*
    /// frame, so without re-running it at the boundary a stepped timeline
    /// would fall silent after frame 0 (no further VBlank/NMI) and an
    /// instruction-bounded trace would drift away from a
    /// continuously-running reference.
    pub(super) fn drain_events_and_maybe_close_frame(&mut self) {
        let events = self.scheduler.drain_up_to_with_cycles(self.master_cycles);
        for (at_cycle, ev) in events {
            self.handle_event_at(at_cycle, ev);
        }
        // A general-DMA stall can fold many master cycles into a single
        // instruction. Like `run_frame`, close at most one frame per step
        // and re-anchor the base at the overshoot; the next step absorbs
        // any remaining slack.
        if self.master_cycles >= self.frame_base_cycle + self.timing.cycles_per_frame() {
            self.end_of_frame_bookkeeping();
            self.frame_base_cycle = self.master_cycles;
        }
    }

    /// Per-frame tail shared by every tier: advance the frame counter,
    /// pre-schedule the next frame's events, and re-arm the H/V IRQ
    /// triggers anchored at the current master cycle (which carries the
    /// small overshoot from this frame's last instruction).
    fn end_of_frame_bookkeeping(&mut self) {
        self.frame_count += 1;
        // The lines drawn next belong to the next frame: one store, made
        // whether or not anything records them.
        self.bus.ppu_mut().set_recording_frame(self.frame_count + 1);
        // Read the live overscan bit so the upcoming frame's VBlank/NMI
        // and HDMA range land on the right scanline (240 when overscan
        // is on, else the region default). Games set overscan during
        // VBlank and leave it stable, so the value is settled here.
        let overscan = self.bus.ppu().overscan();
        schedule_frame_events(&mut self.scheduler, self.master_cycles, overscan);
        self.arm_irq_events(self.master_cycles, self.master_cycles);
    }
}
