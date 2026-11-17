//! The SNES's timetable: the console's timing profile and the events a
//! frame arms.
//!
//! The frame runs on a queue of events keyed on absolute master-clock
//! cycles, retro-core's [`EventQueue`] over this core's [`EventKind`].
//! Every cycle in it is expressed in **master clocks** (NTSC
//! `21_477_270` Hz, PAL `21_281_370` Hz), the same unit the rest of the
//! emulator counts in `master_cycles`.
//!
//! The intent is to model events that are known in advance (per
//! scanline H-blank, HDMA and render position, V-blank entry/exit,
//! frame end) by pre-scheduling them once per frame and draining the
//! queue at convenient boundaries. Events that react to the game
//! during the frame (the H/V IRQ events, re-armed after an
//! `NMITIMEN` / `HTIME` / `VTIME` write, and `AutoJoypadDone`, armed
//! when an auto-joypad read starts) get added on demand via
//! [`EventQueue::schedule`].
//!
//! ## Frame skeleton
//!
//! [`schedule_frame_events`] populates the queue for an entire
//! NTSC/PAL frame at `base_cycle`:
//!
//! - one [`EventKind::HBlankStart`] per scanline at dot 274 (= 1096
//!   master clocks into the scanline);
//! - one [`EventKind::RenderPos`] at dot 22 (= 88 master clocks)
//!   on each of lines 1..=240, whatever `vblank_start_line` is (the
//!   PPU drops the lines past the active region);
//! - [`EventKind::VBlankStart`] at the beginning of `vblank_start_line`;
//! - [`EventKind::VBlankEnd`] at the top of the frame (line 0 / V:0),
//!   clearing the VBlank/NMI flags that the *previous* frame's
//!   `VBlankStart` set, they stay raised across the frame boundary,
//!   mirroring the hardware's V-counter-wrap clear;
//! - [`EventKind::FrameEnd`] one scanline past the end (so the run
//!   loop sees a sentinel even if it pre-empts on master-cycle budget
//!   alone).
//!
//! The event handlers themselves live in
//! `SnesEmulator::handle_event_at` (`system/frame.rs`).

use retro_core::frame::EventQueue;

/// Region-specific timing constants (master-clock rate, scanline
/// length, frame layout).
///
/// `cycles_per_scanline` is identical for NTSC and PAL on real
/// hardware (1364 master clocks); only the frame height and master
/// clock differ. `vblank_start_line` is the first scanline of the
/// vertical blank, visible scanlines are `0..vblank_start_line`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimingProfile {
    /// Master clock rate in Hz. Used as the denominator for the
    /// audio rate converter so 1 second of master clocks resolves to
    /// exactly `output_rate` samples.
    pub master_clock_hz: u32,
    /// Master clocks per scanline. 1364 on both NTSC and PAL, the
    /// real difference is in the height of the frame.
    pub cycles_per_scanline: u32,
    /// Number of scanlines per frame. 262 NTSC, 312 PAL.
    pub scanlines_per_frame: u16,
    /// First scanline of vertical blank. Scanlines `0..vblank_start_line`
    /// are visible; the rest are blank.
    pub vblank_start_line: u16,
}

impl TimingProfile {
    /// NTSC profile, Japan, USA, Korea, most Asian/American markets.
    pub const NTSC: Self = Self {
        master_clock_hz: 21_477_270,
        cycles_per_scanline: 1364,
        scanlines_per_frame: 262,
        vblank_start_line: 225,
    };

    /// PAL profile, Europe, Australia, most non-USA Western markets.
    pub const PAL: Self = Self {
        master_clock_hz: 21_281_370,
        cycles_per_scanline: 1364,
        scanlines_per_frame: 312,
        vblank_start_line: 240,
    };

    /// Total master clocks in a full frame (`cycles_per_scanline *
    /// scanlines_per_frame`).
    pub const fn cycles_per_frame(&self) -> u64 {
        self.cycles_per_scanline as u64 * self.scanlines_per_frame as u64
    }

    /// Frames per second this profile produces: the master clock over
    /// the master clocks in one frame (NTSC 60.0985 Hz, PAL 50.0070 Hz).
    /// Derived from the same arithmetic that schedules the frame, so the
    /// rate the host paces against can never drift from the frame it
    /// actually runs.
    pub fn refresh_rate(&self) -> f32 {
        (self.master_clock_hz as f64 / self.cycles_per_frame() as f64) as f32
    }

    /// Map a SNES header destination byte (`$xxFFD9`) to a timing
    /// profile.
    ///
    /// The destination code is a 4-bit market identifier. The full
    /// table is documented inside the function. Unknown values fall
    /// through to NTSC, which matches the behaviour of real hardware
    /// shipped without region lockout (Japanese and USA consoles all
    /// produce NTSC video regardless of cartridge region).
    pub fn from_destination(byte: u8) -> Self {
        // Destination byte (`$FFD9`) layout per superfamicom.org
        // (checked against bsnes-plus and Mesen):
        //
        // | Byte | Region                                    | TV  |
        // |------|-------------------------------------------|-----|
        // | 0x00 | Japan                                     | NTSC|
        // | 0x01 | USA / Canada                              | NTSC|
        // | 0x02 | Europe (most PAL countries)               | PAL |
        // | 0x03 | Sweden                                    | PAL |
        // | 0x04 | Finland                                   | PAL |
        // | 0x05 | Denmark                                   | PAL |
        // | 0x06 | France                                    | PAL |
        // | 0x07 | Netherlands                               | PAL |
        // | 0x08 | Spain                                     | PAL |
        // | 0x09 | Germany / Austria / Switzerland           | PAL |
        // | 0x0A | Italy                                     | PAL |
        // | 0x0B | China / Hong Kong                         | NTSC|
        // | 0x0C | Indonesia                                 | NTSC|
        // | 0x0D | Korea                                     | NTSC|
        // | 0x0F | Canada (alt)                              | NTSC|
        // | 0x10 | Brazil (PAL-M, but 60 Hz, treated NTSC) | NTSC|
        // | 0x11 | Australia                                 | PAL |
        match byte {
            0x02..=0x0A | 0x11 => Self::PAL,
            _ => Self::NTSC,
        }
    }
}

/// Categories of events the scheduler can dispatch.
///
/// Payload-carrying variants (`IrqV`, `IrqH`, `IrqHV`) carry the
/// matching target so the handler doesn't have to consult system
/// registers separately. The remaining variants are infra signals
/// (begin/end of blank periods, frame boundary, APU resync hook).
///
/// Event semantics live in `handle_event_at` inside the SnesEmulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// Start of horizontal blank for the current scanline.
    HBlankStart,
    /// Mid-scanline "render position" sample point used by the PPU
    /// renderer (dot 22).
    RenderPos,
    /// Top-of-frame HDMA setup (reload table pointers + line counters
    /// for every HDMA-enabled channel). Fires once per frame.
    HdmaInit,
    /// HDMA channel trigger for the current scanline.
    HdmaTrigger,
    /// First scanline of vertical blank, NMI candidate point.
    VBlankStart,
    /// Top of frame (V:0), clears the RDNMI/HVBJOY flags that the
    /// previous frame's `VBlankStart` raised, matching the hardware's
    /// V-counter-wrap clear (the flags stay set through the last
    /// scanline).
    VBlankEnd,
    /// V-counter IRQ at target scanline.
    IrqV {
        /// Target scanline that triggered the schedule.
        v_target: u16,
    },
    /// H-counter IRQ at target dot.
    IrqH {
        /// Target horizontal dot.
        h_target: u16,
    },
    /// Combined H+V counter IRQ.
    IrqHV {
        /// Target scanline.
        v: u16,
        /// Target horizontal dot.
        h: u16,
    },
    /// "Catch up the APU"; the host run loop should advance the
    /// SPC700 / S-DSP up to the current master cycle.
    ApuCatchup,
    /// Auto-joypad read finished, lower the HVBJOY busy flag
    /// (`$4212` bit 0). Scheduled on demand at V-blank when an
    /// auto-read started, a few scanlines after the latch.
    AutoJoypadDone,
    /// End of frame sentinel, `run_frame` uses it as the natural
    /// place to catch the APU and the coprocessor up and drain the
    /// frame's audio (the framebuffer is finished at `VBlankStart`).
    FrameEnd,
}

impl EventKind {
    /// Whether this is one of the H/V IRQ triggers (`IrqV`, `IrqH`,
    /// `IrqHV`).
    pub fn is_irq(&self) -> bool {
        matches!(
            self,
            EventKind::IrqV { .. } | EventKind::IrqH { .. } | EventKind::IrqHV { .. }
        )
    }
}

/// Convert a [`TimingProfile`] to its savestate mirror. `pub(crate)` so
/// `system.rs` can persist the emulator's own timing field too.
pub(crate) fn timing_to_state(t: &TimingProfile) -> crate::savestate::TimingState {
    crate::savestate::TimingState {
        master_clock_hz: t.master_clock_hz,
        cycles_per_scanline: t.cycles_per_scanline,
        scanlines_per_frame: t.scanlines_per_frame,
        vblank_start_line: t.vblank_start_line,
    }
}

/// Inverse of [`timing_to_state`].
pub(crate) fn timing_from_state(s: &crate::savestate::TimingState) -> TimingProfile {
    TimingProfile {
        master_clock_hz: s.master_clock_hz,
        cycles_per_scanline: s.cycles_per_scanline,
        scanlines_per_frame: s.scanlines_per_frame,
        vblank_start_line: s.vblank_start_line,
    }
}

/// Convert an [`EventKind`] to its savestate mirror.
fn event_kind_to_state(kind: &EventKind) -> crate::savestate::EventKindState {
    use crate::savestate::EventKindState as S;
    match *kind {
        EventKind::HBlankStart => S::HBlankStart,
        EventKind::RenderPos => S::RenderPos,
        EventKind::HdmaInit => S::HdmaInit,
        EventKind::HdmaTrigger => S::HdmaTrigger,
        EventKind::VBlankStart => S::VBlankStart,
        EventKind::VBlankEnd => S::VBlankEnd,
        EventKind::IrqV { v_target } => S::IrqV { v_target },
        EventKind::IrqH { h_target } => S::IrqH { h_target },
        EventKind::IrqHV { v, h } => S::IrqHV { v, h },
        EventKind::ApuCatchup => S::ApuCatchup,
        EventKind::AutoJoypadDone => S::AutoJoypadDone,
        EventKind::FrameEnd => S::FrameEnd,
    }
}

/// Inverse of [`event_kind_to_state`].
fn event_kind_from_state(s: &crate::savestate::EventKindState) -> EventKind {
    use crate::savestate::EventKindState as S;
    match *s {
        S::HBlankStart => EventKind::HBlankStart,
        S::RenderPos => EventKind::RenderPos,
        S::HdmaInit => EventKind::HdmaInit,
        S::HdmaTrigger => EventKind::HdmaTrigger,
        S::VBlankStart => EventKind::VBlankStart,
        S::VBlankEnd => EventKind::VBlankEnd,
        S::IrqV { v_target } => EventKind::IrqV { v_target },
        S::IrqH { h_target } => EventKind::IrqH { h_target },
        S::IrqHV { v, h } => EventKind::IrqHV { v, h },
        S::ApuCatchup => EventKind::ApuCatchup,
        S::AutoJoypadDone => EventKind::AutoJoypadDone,
        S::FrameEnd => EventKind::FrameEnd,
    }
}

/// The queue as the savestate writes it: its cursor, its sequence, the
/// timing profile, and its events in the heap's storage order, which the
/// state hash covers. `timing` is the emulator's own profile, the one the
/// queue's events were armed with: the two were always set together.
pub(crate) fn queue_to_state(
    queue: &EventQueue<EventKind>,
    timing: &TimingProfile,
) -> crate::savestate::SchedulerState {
    crate::savestate::SchedulerState {
        current_cycle: queue.cursor(),
        next_seq: queue.next_seq(),
        timing: timing_to_state(timing),
        events: queue
            .stored()
            .map(
                |(at_cycle, seq, kind)| crate::savestate::ScheduledEventState {
                    at_cycle,
                    seq,
                    kind: event_kind_to_state(kind),
                },
            )
            .collect(),
    }
}

/// The queue back from a savestate, its events pushed in the stored order
/// they were written in, which rebuilds the same storage. The timing
/// profile the state carries is the emulator's, restored with it.
pub(crate) fn queue_from_state(
    queue: &mut EventQueue<EventKind>,
    state: &crate::savestate::SchedulerState,
) {
    queue.restore(
        state.current_cycle,
        state.next_seq,
        state
            .events
            .iter()
            .map(|ev| (ev.at_cycle, ev.seq, event_kind_from_state(&ev.kind))),
    );
}

/// Pre-schedule the per-frame skeleton: HBlank starts, RenderPos
/// markers on visible scanlines, VBlank start/end, and a FrameEnd
/// sentinel. All cycles are absolute (built from `base_cycle`).
///
/// Called once per frame, at the frame boundary: by `reset` for
/// frame 0 and by `end_of_frame_bookkeeping` for each frame after.
/// Events that land on the same cycle come out in schedule order;
/// no two of the events armed here share a cycle in either timing
/// profile.
///
/// ## Overscan unification
///
/// RenderPos is always scheduled for SNES lines 1..=240, regardless
/// of the timing profile's `vblank_start_line` (= 225 NTSC, 240
/// PAL). The PPU's [`crate::ppu::Ppu::in_active_scanline`] gate
/// reads the live `$2133` bit 2 (overscan) and drops scanlines past
/// the active region, 225 (overscan off) or 240 (overscan on).
/// Schedule-then-gate keeps the H-event timeline stable even when
/// the game toggles overscan mid-frame, where rescheduling would
/// require careful cycle accounting to avoid double-firing.
///
/// ## Overscan shifts the VBlank boundary (NMI + HDMA range)
///
/// `$2133` bit 2 (overscan) doesn't just extend the visible area to
/// 239: it pushes the **first VBlank scanline** from 225 to 240, so
/// VBlankStart/NMI fire later and per-line HDMA triggers extend to
/// line 239: the VRAM/CGRAM/OAM access gate and the NMI both move
/// with the bit (checked against bsnes-plus). The caller
/// passes the live bit (`Ppu::overscan`); we read it at frame build
/// time (line 0), which is correct because games set overscan during
/// the previous frame's VBlank and leave it stable for the frame.
/// Without this shift the NMI handler's VBlank DMA runs at V~230,
/// inside the overscan-active region (1..239), and the access gate
/// silently drops it, freezing per-frame VRAM uploads (e.g. the text
/// of the Color Test in Nintendo's SNES Test Program) while ungated
/// register writes keep animating.
pub fn schedule_frame_events(
    scheduler: &mut EventQueue<EventKind>,
    timing: TimingProfile,
    base_cycle: u64,
    overscan: bool,
) {
    let cps = timing.cycles_per_scanline as u64;
    let lines = timing.scanlines_per_frame;
    // Overscan moves the first VBlank scanline to 240, matching the
    // PPU's access gate (`in_active_scanline` uses 240 when overscan is
    // on). Non-overscan keeps the region default (225 NTSC / 240 PAL).
    let vbl_start = if overscan {
        240
    } else {
        timing.vblank_start_line
    };

    // HDMA setup runs once near the top of the frame (line 0, dot ~6 =
    // 24 master clocks), ahead of every per-line trigger (the
    // position was checked against bsnes-plus).
    scheduler.schedule(base_cycle + 6 * 4, EventKind::HdmaInit);

    for line in 0..lines {
        let line_base = base_cycle + (line as u64) * cps;

        // RenderPos lands at dot 22 (= 88 master clocks) for every
        // SNES line that *could* be visible with overscan enabled
        // (lines 1..=240 covers both NTSC overscan and the PAL
        // visible region). The PPU itself drops the extra lines
        // when overscan is off via `in_active_scanline`.
        if (1..=240).contains(&line) {
            scheduler.schedule(line_base + 22 * 4, EventKind::RenderPos);
        }

        // HBlankStart at dot 274 (= 1096 master clocks).
        scheduler.schedule(line_base + 274 * 4, EventKind::HBlankStart);

        // HDMA per-line trigger at dot 276 (= 1104 master clocks;
        // checked against bsnes-plus), just after HBlank begins, for every
        // scanline that drives the visible raster (`0..vblank_start`).
        if line < vbl_start {
            scheduler.schedule(line_base + 276 * 4, EventKind::HdmaTrigger);
        }

        // VBlankStart on the first VBL scanline.
        if line == vbl_start {
            scheduler.schedule(line_base, EventKind::VBlankStart);
        }
    }

    // VBlankEnd at the top of the frame (line 0 / V:0). On hardware the
    // RDNMI ($4210) NMI flag and the HVBJOY ($4212) VBlank flag are set
    // at VBlankStart (line 225/240) and stay set through the last
    // scanline, clearing only when the V-counter wraps back to 0. A
    // clear on the *last* scanline (V:261) would drop $4212 bit 7 one
    // line early, so a "wait for VBlank to end" poll would latch the
    // H/V counter at V:261 instead of V:0; the counter-latch sub-test of
    // the Electronics Test in Nintendo's SNES Test Program reads OPVCT
    // after such a poll and requires 0. The flag set by *this* frame's
    // VBlankStart is therefore cleared by the *next* frame's line-0
    // VBlankEnd (events are armed per frame; the flag persists across
    // the boundary): the flags stay set through the last line and clear
    // when the V counter wraps (checked against snes9x).
    scheduler.schedule(base_cycle, EventKind::VBlankEnd);

    // FrameEnd sentinel one scanline past the visible/blank tail,
    // mirrors `master_cycles >= base + cycles_per_frame`.
    scheduler.schedule(base_cycle + timing.cycles_per_frame(), EventKind::FrameEnd);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kinds of the events due by `cycle`, taken off the queue.
    fn drain_kinds(q: &mut EventQueue<EventKind>, cycle: u64) -> Vec<EventKind> {
        let batch = q.take_due(cycle);
        let kinds = batch.iter().map(|(_, kind)| kind).collect();
        q.give_back(batch);
        kinds
    }

    #[test]
    fn timing_ntsc_constants_match_spec() {
        let t = TimingProfile::NTSC;
        assert_eq!(t.master_clock_hz, 21_477_270);
        assert_eq!(t.cycles_per_scanline, 1364);
        assert_eq!(t.scanlines_per_frame, 262);
        assert_eq!(t.vblank_start_line, 225);
        assert_eq!(t.cycles_per_frame(), 1364 * 262);
    }

    #[test]
    fn timing_pal_constants_match_spec() {
        let t = TimingProfile::PAL;
        assert_eq!(t.master_clock_hz, 21_281_370);
        assert_eq!(t.cycles_per_scanline, 1364);
        assert_eq!(t.scanlines_per_frame, 312);
        assert_eq!(t.vblank_start_line, 240);
        assert_eq!(t.cycles_per_frame(), 1364 * 312);
    }

    #[test]
    fn from_destination_japan_and_us_are_ntsc() {
        assert_eq!(TimingProfile::from_destination(0x00), TimingProfile::NTSC);
        assert_eq!(TimingProfile::from_destination(0x01), TimingProfile::NTSC);
    }

    #[test]
    fn from_destination_europe_block_is_pal() {
        for byte in 0x02..=0x0A_u8 {
            assert_eq!(
                TimingProfile::from_destination(byte),
                TimingProfile::PAL,
                "byte 0x{byte:02X} should be PAL"
            );
        }
        assert_eq!(TimingProfile::from_destination(0x11), TimingProfile::PAL);
    }

    #[test]
    fn from_destination_asian_and_brazil_are_ntsc() {
        // China/HK, Indonesia, Korea, Canada-alt, Brazil
        for byte in [0x0B_u8, 0x0C, 0x0D, 0x0F, 0x10] {
            assert_eq!(
                TimingProfile::from_destination(byte),
                TimingProfile::NTSC,
                "byte 0x{byte:02X} should be NTSC"
            );
        }
    }

    #[test]
    fn from_destination_unknown_falls_back_to_ntsc() {
        for byte in [0xFFu8, 0x80, 0x42, 0xAB] {
            assert_eq!(TimingProfile::from_destination(byte), TimingProfile::NTSC);
        }
    }

    #[test]
    fn is_irq_names_the_three_irq_triggers() {
        assert!(EventKind::IrqV { v_target: 0 }.is_irq());
        assert!(EventKind::IrqH { h_target: 0 }.is_irq());
        assert!(EventKind::IrqHV { v: 0, h: 0 }.is_irq());
        assert!(!EventKind::VBlankStart.is_irq());
        assert!(!EventKind::FrameEnd.is_irq());
    }

    #[test]
    fn schedule_frame_events_ntsc_counts_match_spec() {
        let mut s = EventQueue::new();
        let timing = TimingProfile::NTSC;
        schedule_frame_events(&mut s, timing, 0, false);

        // RenderPos always covers lines 1..=240 (overscan-aware
        // gate lives inside the PPU): 240 RenderPos events
        // regardless of NTSC vs PAL.
        // 262 HBlankStart + 240 RenderPos + 1 VBlankStart + 1
        // VBlankEnd + 1 FrameEnd + 1 HdmaInit + 225 HdmaTrigger
        // (one per visible line 0..225).
        assert_eq!(s.len(), 262 + 240 + 1 + 1 + 1 + 1 + 225);

        let drained = drain_kinds(&mut s, u64::MAX);
        let count = |needle: &EventKind| drained.iter().filter(|e| *e == needle).count();
        assert_eq!(count(&EventKind::HBlankStart), 262);
        assert_eq!(count(&EventKind::RenderPos), 240);
        assert_eq!(count(&EventKind::VBlankStart), 1);
        assert_eq!(count(&EventKind::VBlankEnd), 1);
        assert_eq!(count(&EventKind::FrameEnd), 1);
        assert_eq!(count(&EventKind::HdmaInit), 1);
        assert_eq!(count(&EventKind::HdmaTrigger), 225);
    }

    #[test]
    fn schedule_frame_events_pal_counts_match_spec() {
        let mut s = EventQueue::new();
        let timing = TimingProfile::PAL;
        schedule_frame_events(&mut s, timing, 0, false);
        // RenderPos always covers lines 1..=240, same as NTSC.
        // 312 HBlankStart + 240 RenderPos + 1 VBlankStart +
        // 1 VBlankEnd + 1 FrameEnd + 1 HdmaInit + 240 HdmaTrigger
        // (one per visible line 0..240).
        assert_eq!(s.len(), 312 + 240 + 1 + 1 + 1 + 1 + 240);

        let drained = drain_kinds(&mut s, u64::MAX);
        let count = |needle: &EventKind| drained.iter().filter(|e| *e == needle).count();
        assert_eq!(count(&EventKind::HdmaInit), 1);
        assert_eq!(count(&EventKind::HdmaTrigger), 240);
    }

    #[test]
    fn schedule_frame_events_overscan_shifts_vblank_to_240() {
        // With overscan on (NTSC) the first VBlank scanline moves from
        // 225 to 240: VBlankStart lands on line 240 and per-line HDMA
        // triggers extend to line 239 (240 triggers, one per line
        // 0..=239). RenderPos/HBlank/VBlankEnd/FrameEnd are unchanged.
        let cps = TimingProfile::NTSC.cycles_per_scanline as u64;
        let mut s = EventQueue::new();
        let timing = TimingProfile::NTSC;
        schedule_frame_events(&mut s, timing, 0, true);
        // 262 HBlank + 240 RenderPos + 1 VBlankStart + 1 VBlankEnd +
        // 1 FrameEnd + 1 HdmaInit + 240 HdmaTrigger.
        assert_eq!(s.len(), 262 + 240 + 1 + 1 + 1 + 1 + 240);

        let mut vbl_start_cycle = None;
        let mut hdma_triggers = 0usize;
        while let Some(cyc) = s.peek_cycle() {
            for ev in drain_kinds(&mut s, cyc) {
                match ev {
                    EventKind::VBlankStart => vbl_start_cycle = Some(cyc),
                    EventKind::HdmaTrigger => hdma_triggers += 1,
                    _ => {}
                }
            }
        }
        // VBlankStart at the top of line 240 (vs 225 without overscan).
        assert_eq!(vbl_start_cycle, Some(240 * cps));
        assert_eq!(hdma_triggers, 240);
    }

    #[test]
    fn schedule_frame_events_render_pos_covers_overscan_lines() {
        // Sanity check: RenderPos fires for SNES line 240 too, so
        // the PPU gate can decide at runtime whether to render
        // (overscan on) or skip (overscan off).
        let cps = TimingProfile::NTSC.cycles_per_scanline as u64;
        let mut s = EventQueue::new();
        let timing = TimingProfile::NTSC;
        schedule_frame_events(&mut s, timing, 0, false);
        // Collect (cycle, kind) tuples by repeatedly peeking +
        // draining.
        let mut render_pos_cycles: Vec<u64> = Vec::new();
        while let Some(cyc) = s.peek_cycle() {
            for ev in drain_kinds(&mut s, cyc) {
                if matches!(ev, EventKind::RenderPos) {
                    render_pos_cycles.push(cyc);
                }
            }
        }
        assert_eq!(render_pos_cycles.len(), 240);
        // First RenderPos at line 1, last at line 240.
        assert_eq!(render_pos_cycles[0], cps + 22 * 4);
        assert_eq!(render_pos_cycles[239], 240 * cps + 22 * 4);
    }

    #[test]
    fn schedule_frame_events_hdma_positions() {
        let cps = TimingProfile::NTSC.cycles_per_scanline as u64;
        let mut s = EventQueue::new();
        let timing = TimingProfile::NTSC;
        schedule_frame_events(&mut s, timing, 0, false);
        let mut init_cycles: Vec<u64> = Vec::new();
        let mut trigger_cycles: Vec<u64> = Vec::new();
        while let Some(cyc) = s.peek_cycle() {
            for ev in drain_kinds(&mut s, cyc) {
                match ev {
                    EventKind::HdmaInit => init_cycles.push(cyc),
                    EventKind::HdmaTrigger => trigger_cycles.push(cyc),
                    _ => {}
                }
            }
        }
        // One HdmaInit at line 0, dot 6 (= 24 master clocks).
        assert_eq!(init_cycles, vec![6 * 4]);
        // HdmaTrigger on lines 0..225 at dot 276 (= 1104 master clocks).
        assert_eq!(trigger_cycles.len(), 225);
        assert_eq!(trigger_cycles[0], 276 * 4);
        assert_eq!(trigger_cycles[224], 224 * cps + 276 * 4);
    }

    #[test]
    fn schedule_frame_events_vblank_start_lands_on_correct_cycle() {
        let mut s = EventQueue::new();
        let timing = TimingProfile::NTSC;
        schedule_frame_events(&mut s, timing, 0, false);
        let drained = drain_kinds(&mut s, u64::MAX);
        // First VBlankStart event must be at cycle 225 * 1364, the
        // pre-schedule loop placed it on the line_base of vbl_start.
        // Since drain_up_to flattens to kinds, we can't easily
        // re-derive the cycle. So instead, schedule into an empty
        // scheduler, then peek before drain:
        let mut s2 = EventQueue::new();
        let timing_s2 = TimingProfile::NTSC;
        schedule_frame_events(&mut s2, timing_s2, 1_000_000, false);
        let mut found_vbl = None;
        while let Some(cyc) = s2.peek_cycle() {
            let evs = drain_kinds(&mut s2, cyc);
            for ev in evs {
                if let EventKind::VBlankStart = ev {
                    found_vbl = Some(cyc);
                    break;
                }
            }
            if found_vbl.is_some() {
                break;
            }
        }
        assert_eq!(found_vbl, Some(1_000_000 + 225 * 1364));
        // Silence "drained never read", we only use it for type inference above.
        let _ = drained.len();
    }

    #[test]
    fn schedule_frame_events_vblank_end_lands_on_line_zero() {
        // The VBlank/NMI flags raised at VBlankStart (line 225/240) must
        // stay set through the last scanline and clear only when V wraps
        // to 0. We model that by arming VBlankEnd at the *top* of the
        // frame (base_cycle / line 0), so it clears the previous frame's
        // VBlank. Firing it on the last scanline instead would drop the
        // $4212 VBlank flag one line early and fail the H/V-latch sub-test
        // of the Electronics Test in Nintendo's SNES Test Program.
        let base = 1_000_000u64;
        let mut s = EventQueue::new();
        let timing = TimingProfile::NTSC;
        schedule_frame_events(&mut s, timing, base, false);
        let mut vbl_end = None;
        while let Some(cyc) = s.peek_cycle() {
            for ev in drain_kinds(&mut s, cyc) {
                if let EventKind::VBlankEnd = ev {
                    vbl_end = Some(cyc);
                }
            }
        }
        assert_eq!(
            vbl_end,
            Some(base),
            "VBlankEnd must be armed at line 0 (base_cycle), not the last scanline"
        );
    }

    #[test]
    fn schedule_frame_events_frame_end_is_strictly_after_vblank_end() {
        let mut s = EventQueue::new();
        let timing = TimingProfile::NTSC;
        schedule_frame_events(&mut s, timing, 0, false);
        // Pull all events into an order list, tagged with their cycle
        // by interleaving peek + drain_up_to(peek_cycle).
        let mut sequence: Vec<(u64, EventKind)> = Vec::new();
        while let Some(cyc) = s.peek_cycle() {
            for ev in drain_kinds(&mut s, cyc) {
                sequence.push((cyc, ev));
            }
        }
        let vbl_end_cycle = sequence
            .iter()
            .find_map(|(c, e)| matches!(e, EventKind::VBlankEnd).then_some(*c))
            .expect("VBlankEnd missing");
        let frame_end_cycle = sequence
            .iter()
            .find_map(|(c, e)| matches!(e, EventKind::FrameEnd).then_some(*c))
            .expect("FrameEnd missing");
        assert!(
            frame_end_cycle > vbl_end_cycle,
            "FrameEnd ({frame_end_cycle}) must come strictly after VBlankEnd ({vbl_end_cycle})"
        );
    }

    #[test]
    fn timing_profile_refresh_rates_are_cycle_derived() {
        // 21_477_270 / (1364 x 262 = 357_368) and 21_281_370 / (1364 x 312 = 425_568).
        assert!((TimingProfile::NTSC.refresh_rate() - 60.0985).abs() < 1e-4);
        assert!((TimingProfile::PAL.refresh_rate() - 50.0070).abs() < 1e-4);
        assert_eq!(
            TimingProfile::NTSC.refresh_rate(),
            (21_477_270f64 / 357_368f64) as f32
        );
    }
}
