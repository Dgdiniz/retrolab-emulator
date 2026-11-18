//! The frame: the raster events armed each frame and what each does, and
//! this core's side of the frame driver (`retro_core::frame`): what falls
//! due, where a frame ends, and the hold at the capture boundary.

use retro_core::frame::{Due, FrameMachine, StepHooks, Stepped};
use wdc65816::instructions::FLAG_I;

use super::{SnesEmulator, NMI_DELIVERY_MAX_INSNS};
use crate::debugger::SnesEvent;
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
    /// `master_cycles`, with no debugger hooks. The frame calls
    /// [`Self::handle_event_at`] directly with each event's true scheduled
    /// `at_cycle`; unit tests use this shorthand when the scheduled cycle
    /// is irrelevant to the case.
    #[cfg(test)]
    pub(super) fn handle_event(&mut self, kind: EventKind) {
        self.handle_event_at(self.master_cycles, kind, &mut ());
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
    ///
    /// An event a breakpoint can stop on is handed to the debugger through
    /// `hooks.raised`; the play tier's hooks drop it unchecked.
    pub(super) fn handle_event_at<H: StepHooks<Self>>(
        &mut self,
        at_cycle: u64,
        kind: EventKind,
        hooks: &mut H,
    ) {
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
                hooks.chip_start("PPU");
                self.bus
                    .ppu_mut()
                    .render_scanline(scanline, &mut self.framebuffer);
                hooks.chip_finish(0);
                hooks.raised(self, |m| {
                    m.debugger.check_event(SnesEvent::Scanline(scanline))
                });
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
                hooks.chip_start("HDMA");
                let stolen = self.bus.hdma_init_frame();
                hooks.chip_finish(0);
                self.master_cycles += stolen;
                self.bus
                    .system_regs_mut()
                    .tick(stolen.min(u32::MAX as u64) as u32);
                hooks.raised(self, |m| {
                    m.debugger.check_event(SnesEvent::HdmaStart) | m.drain_ppu_debug_events()
                });
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
                hooks.chip_start("HDMA");
                let stolen = self.bus.hdma_run_line();
                hooks.chip_finish(0);
                self.master_cycles += stolen;
                self.bus
                    .system_regs_mut()
                    .tick(stolen.min(u32::MAX as u64) as u32);
                // A palette colour or a tile the line's HDMA wrote: the
                // break waits for the rest of the batch.
                hooks.raised(self, Self::drain_ppu_debug_events);
            }
            EventKind::VBlankStart => {
                // A frame held at the capture boundary finished its
                // picture when it stopped in front of this event; only
                // the start of the vertical blank is left.
                if self.held_at_capture_boundary {
                    self.held_at_capture_boundary = false;
                } else {
                    hooks.chip_start("framebuffer");
                    self.finish_picture();
                    hooks.chip_finish(0);
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
                // Event breakpoints: entering V-blank, and the NMI when its
                // delivery is armed.
                hooks.raised(self, |m| m.debugger.check_event(SnesEvent::VBlank));
                if nmi_enabled {
                    hooks.raised(self, |m| m.debugger.check_event(SnesEvent::Nmi));
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
                // Raise the level-triggered IRQ line. `mirror_irq_line`
                // copies it into the CPU before each step; the handler acks
                // by reading $4211 (TIMEUP), which lowers the line again.
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
                hooks.raised(self, |m| m.debugger.check_event(SnesEvent::Irq));
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
                hooks.chip_start("APU");
                self.bus.apu_mut().catch_up_to(self.master_cycles);
                hooks.chip_finish(0);
                // The APU catch-up above advances all serialized audio
                // state; only the host-ring drain (resample) is
                // presentation, so a headless frame skips just that.
                if self.present {
                    hooks.chip_start("audio mix");
                    self.drain_apu_to_host_audio();
                    hooks.chip_finish(0);
                }
                // Keep a cartridge coprocessor (DSP-1, ...) ticking across
                // a frame with no register traffic, bounding its catch-up
                // staleness to one frame just like the APU above.
                hooks.chip_start("cartridge");
                self.bus.catch_up_coprocessor(self.master_cycles);
                hooks.chip_finish(0);
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
        self.scheduler.earliest(|kind| *kind == EventKind::FrameEnd)
            == Some(self.frame_base_cycle + self.timing.cycles_per_frame())
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
    fn handle_due_events<H: StepHooks<Self>>(&mut self, hooks: &mut H) -> bool {
        if self.capture_boundary_requested | self.held_at_capture_boundary {
            return self.handle_capture_events(hooks);
        }
        let events = self.scheduler.take_due(self.master_cycles);
        for (at_cycle, ev) in events.iter() {
            self.handle_event_at(at_cycle, ev, hooks);
        }
        self.scheduler.give_back(events);
        false
    }

    /// The drain while a capture is under way (see
    /// [`Self::handle_due_events`]): the rest of a split batch when the
    /// core is held at the boundary, else a drain that stops in front of
    /// the frame's `VBlankStart`.
    fn handle_capture_events<H: StepHooks<Self>>(&mut self, hooks: &mut H) -> bool {
        if self.held_at_capture_boundary {
            self.release_capture_boundary(hooks);
            return false;
        }
        let bound = self.master_cycles;
        let (events, at_vblank) = self
            .scheduler
            .take_due_before(bound, |kind| matches!(kind, EventKind::VBlankStart));
        for (at_cycle, ev) in events.iter() {
            self.handle_event_at(at_cycle, ev, hooks);
        }
        self.scheduler.give_back(events);
        if at_vblank {
            hooks.chip_start("framebuffer");
            self.finish_picture();
            hooks.chip_finish(0);
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
    fn release_capture_boundary<H: StepHooks<Self>>(&mut self, hooks: &mut H) {
        let events = self.scheduler.take_due(self.capture_boundary_cycle);
        for (at_cycle, ev) in events.iter() {
            self.handle_event_at(at_cycle, ev, hooks);
        }
        self.scheduler.give_back(events);
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
    pub(super) fn leave_capture_boundary<H: StepHooks<Self>>(&mut self, hooks: &mut H) {
        if !self.held_at_capture_boundary {
            return;
        }
        self.release_capture_boundary(hooks);
        while self
            .scheduler
            .peek_cycle()
            .is_some_and(|at| at <= self.master_cycles)
        {
            let events = self.scheduler.take_due(self.master_cycles);
            for (at_cycle, ev) in events.iter() {
                self.handle_event_at(at_cycle, ev, hooks);
            }
            self.scheduler.give_back(events);
        }
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
        schedule_frame_events(
            &mut self.scheduler,
            self.timing,
            self.master_cycles,
            overscan,
        );
        self.arm_irq_events(self.master_cycles, self.master_cycles);
    }
}

/// The SNES on the frame driver: a queue of timed events is its timetable,
/// a pass runs the 65C816 to the next event or the frame's end, and the
/// frame is anchored on a floating start (the frame's end the previous
/// frame armed).
impl FrameMachine for SnesEmulator {
    type Run = ();

    #[inline(always)]
    fn now(&self) -> u64 {
        self.master_cycles
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
    fn enter_frame(&mut self) -> u64 {
        let frame_cycles = self.timing.cycles_per_frame();
        let frame_end = self.frame_base_cycle + frame_cycles;
        // Short of the frame's end the frame is in progress: its
        // `FrameEnd` is handled only once the clock reaches it. The
        // debugger's steps come through here once an instruction, and this
        // spares them the walk over the queue.
        if self.master_cycles < frame_end {
            return frame_end;
        }
        match self.scheduler.earliest(|kind| *kind == EventKind::FrameEnd) {
            Some(end) if end == frame_end => {}
            Some(end) => self.frame_base_cycle = end.saturating_sub(frame_cycles),
            None => self.frame_base_cycle = self.master_cycles,
        }
        self.frame_base_cycle + frame_cycles
    }

    /// The events due by the present cycle (stopping in front of the
    /// frame's `VBlankStart` for a capture), then the S-DSP register
    /// writes the `FrameEnd` catch-up made, checked as the batch drains;
    /// the frame ends once the clock reaches `frame_end`.
    #[inline]
    fn handle_due<H: StepHooks<Self>>(&mut self, frame_end: u64, hooks: &mut H) -> Due {
        if self.handle_due_events(hooks) {
            return Due::Held;
        }
        hooks.raised(self, Self::drain_apu_dsp_events);
        if self.master_cycles >= frame_end {
            Due::FrameEnd
        } else {
            Due::Continue
        }
    }

    /// The next queued event, or the frame's end if that comes first.
    #[inline(always)]
    fn next_deadline(&self, frame_end: u64) -> Option<u64> {
        Some(
            self.scheduler
                .peek_cycle()
                .unwrap_or(frame_end)
                .min(frame_end),
        )
    }

    #[inline(always)]
    fn begin_run(&mut self) {}

    #[inline(always)]
    fn end_run(&mut self, _run: ()) {}

    #[inline(always)]
    fn machine_step<H: StepHooks<Self>>(
        &mut self,
        _run: &mut (),
        hooks: &mut H,
    ) -> Option<Stepped> {
        self.step_main_cpu(hooks)
    }

    fn close_frame(&mut self) {
        self.end_of_frame_bookkeeping();
    }

    /// Before a host step: leave the capture boundary if the core is held
    /// there (see [`SnesEmulator::leave_capture_boundary`]), then go on
    /// with the frame in progress or open the next one, as a frame call
    /// does. Opening the next frame here matters after a frame call that
    /// ran its frame to the end: the frame base would otherwise still be
    /// that frame's, so the first step would close a frame at once and
    /// arm a second set of events over the ones already armed.
    fn enter_step<H: StepHooks<Self>>(&mut self, hooks: &mut H) -> u64 {
        self.leave_capture_boundary(hooks);
        FrameMachine::enter_frame(self)
    }

    fn frame_in_progress(&self) -> bool {
        SnesEmulator::frame_in_progress(self)
    }

    /// Only the hold at the capture boundary: a savestate holds any other
    /// frame in progress, its queue and its base with it.
    fn frame_held(&self) -> bool {
        self.held_at_capture_boundary
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
}
