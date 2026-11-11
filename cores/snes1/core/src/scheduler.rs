//! H-event scheduler and console timing profile.
//!
//! Drives the SNES system loop around a min-heap of pre-scheduled
//! events keyed on absolute master-clock cycles. Every cycle in the
//! scheduler (`current_cycle`, `at_cycle`, `peek_cycle`,
//! `drain_up_to`) is expressed in **master clocks** (NTSC
//! `21_477_270` Hz, PAL `21_281_370` Hz), the same unit the rest of
//! the emulator counts in `master_cycles`.
//!
//! The intent is to model events that are known in advance (per
//! scanline H-blank, HDMA and render position, V-blank entry/exit,
//! frame end) by pre-scheduling them once per frame and draining the
//! queue at convenient boundaries. Events that react to the game
//! during the frame (the H/V IRQ events, re-armed after an
//! `NMITIMEN` / `HTIME` / `VTIME` write, and `AutoJoypadDone`, armed
//! when an auto-joypad read starts) get added on demand via
//! [`HEventScheduler::schedule`].
//!
//! ## Frame skeleton
//!
//! [`schedule_frame_events`] populates the scheduler for an entire
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
//! `SnesEmulator::handle_event_at` (`system.rs`).

use std::cmp::Reverse;
use std::collections::BinaryHeap;

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
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// One scheduled event. Ordered by `(at_cycle, seq)` so the
/// scheduler can use `BinaryHeap<Reverse<_>>` as a min-heap with
/// stable FIFO order on cycle ties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledEvent {
    /// Absolute master-clock cycle at which the event fires.
    pub at_cycle: u64,
    /// Sequence number assigned at schedule time. Breaks ties on
    /// `at_cycle` so events scheduled first fire first.
    pub seq: u64,
    /// The event itself.
    pub kind: EventKind,
}

impl Ord for ScheduledEvent {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Sort by (at_cycle, seq) ascending so that wrapped in
        // `Reverse` the BinaryHeap pops the lowest cycle first, and
        // ties are broken by insertion order.
        self.at_cycle
            .cmp(&other.at_cycle)
            .then(self.seq.cmp(&other.seq))
    }
}

impl PartialOrd for ScheduledEvent {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
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

/// Min-heap scheduler of [`ScheduledEvent`]s keyed on master cycles.
///
/// See module docs for the unit contract and the typical event
/// catalog. The scheduler owns nothing the rest of the system
/// doesn't already own, it's a pure data structure with a small
/// helper API around `BinaryHeap`.
pub struct HEventScheduler {
    events: BinaryHeap<Reverse<ScheduledEvent>>,
    current_cycle: u64,
    timing: TimingProfile,
    next_seq: u64,
}

impl HEventScheduler {
    /// Build a fresh scheduler for the given timing profile. The
    /// heap is empty and `current_cycle` is zero.
    pub fn new(timing: TimingProfile) -> Self {
        Self {
            events: BinaryHeap::new(),
            current_cycle: 0,
            timing,
            next_seq: 0,
        }
    }

    /// Clear every queued event and re-anchor `current_cycle` to
    /// `base_cycle`. Used at console reset and at savestate restore.
    pub fn reset(&mut self, base_cycle: u64) {
        self.events.clear();
        self.current_cycle = base_cycle;
        self.next_seq = 0;
    }

    /// Capture the cursor, the sequence counter, the active timing
    /// profile, and the pending events (the heap flattened to an
    /// order-independent `Vec`) into a snapshot.
    pub fn to_savestate(&self) -> crate::savestate::SchedulerState {
        crate::savestate::SchedulerState {
            current_cycle: self.current_cycle,
            next_seq: self.next_seq,
            timing: timing_to_state(&self.timing),
            events: self
                .events
                .iter()
                .map(|Reverse(ev)| crate::savestate::ScheduledEventState {
                    at_cycle: ev.at_cycle,
                    seq: ev.seq,
                    kind: event_kind_to_state(&ev.kind),
                })
                .collect(),
        }
    }

    /// Restore the scheduler from a snapshot, rebuilding the heap from
    /// the flattened event list (order is irrelevant, the heap re-sorts
    /// by `(at_cycle, seq)`).
    pub fn from_savestate(&mut self, s: &crate::savestate::SchedulerState) {
        self.current_cycle = s.current_cycle;
        self.next_seq = s.next_seq;
        self.timing = timing_from_state(&s.timing);
        self.events.clear();
        for ev in &s.events {
            self.events.push(Reverse(ScheduledEvent {
                at_cycle: ev.at_cycle,
                seq: ev.seq,
                kind: event_kind_from_state(&ev.kind),
            }));
        }
    }

    /// Schedule `kind` to fire at absolute master cycle `at_cycle`.
    /// Multiple events at the same cycle fire in schedule order.
    pub fn schedule(&mut self, at_cycle: u64, kind: EventKind) {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.events.push(Reverse(ScheduledEvent {
            at_cycle,
            seq,
            kind,
        }));
    }

    /// Cycles of the queued events `pred` accepts, earliest first.
    pub fn cycles_of(&self, pred: impl Fn(&EventKind) -> bool) -> Vec<u64> {
        let mut cycles: Vec<u64> = self
            .events
            .iter()
            .filter(|Reverse(ev)| pred(&ev.kind))
            .map(|Reverse(ev)| ev.at_cycle)
            .collect();
        cycles.sort_unstable();
        cycles
    }

    /// Cycle of the earliest queued event, or `None` if no events
    /// are pending.
    pub fn peek_cycle(&self) -> Option<u64> {
        self.events.peek().map(|Reverse(ev)| ev.at_cycle)
    }

    /// Pop every event whose `at_cycle <= cycle`, in `(at_cycle,
    /// seq)` order, and return the kinds. Updates `current_cycle`
    /// to `cycle`.
    ///
    /// `cycle` must not go backward across calls; the scheduler is
    /// monotonically advancing.
    pub fn drain_up_to(&mut self, cycle: u64) -> Vec<EventKind> {
        debug_assert!(
            cycle >= self.current_cycle,
            "drain_up_to ({cycle}) must not go backward below current_cycle ({})",
            self.current_cycle
        );
        let mut out = Vec::new();
        while let Some(Reverse(top)) = self.events.peek() {
            if top.at_cycle > cycle {
                break;
            }
            let Reverse(ev) = self.events.pop().expect("peek above guaranteed an element");
            out.push(ev.kind);
        }
        self.current_cycle = cycle;
        out
    }

    /// Like [`Self::drain_up_to`] but pairs each drained event with its
    /// scheduled `at_cycle`, in `(at_cycle, seq)` order.
    ///
    /// The scheduled cycle is the raster point the event was armed for.
    /// When a single instruction or DMA overshoots the cursor by many
    /// scanlines (a general DMA folds its whole transfer into one step),
    /// draining against the post-jump `master_cycles` would attribute
    /// every deferred event to the same late raster position, misrendering
    /// the deferred `RenderPos` scanlines and pushing per-line HDMA CGRAM/OAM
    /// writes into active display (where the active-scan gate redirects them
    /// to colour 0). Callers that care about the raster position of each
    /// event use this so a deferred batch replays in cycle order at the
    /// dot each event was scheduled for.
    pub fn drain_up_to_with_cycles(&mut self, cycle: u64) -> Vec<(u64, EventKind)> {
        debug_assert!(
            cycle >= self.current_cycle,
            "drain_up_to_with_cycles ({cycle}) must not go backward below current_cycle ({})",
            self.current_cycle
        );
        let mut out = Vec::new();
        while let Some(Reverse(top)) = self.events.peek() {
            if top.at_cycle > cycle {
                break;
            }
            let Reverse(ev) = self.events.pop().expect("peek above guaranteed an element");
            out.push((ev.at_cycle, ev.kind));
        }
        self.current_cycle = cycle;
        out
    }

    /// Drop every queued event for which `pred(&kind)` returns
    /// `true`. Used to cancel IRQs when the corresponding timer
    /// disables. O(n log n) because `BinaryHeap` has no efficient
    /// in-place removal, fine because the call rate is bounded by
    /// register writes, not by per-cycle traffic.
    pub fn cancel_matching(&mut self, pred: impl Fn(&EventKind) -> bool) {
        let kept: Vec<_> = self
            .events
            .drain()
            .filter(|Reverse(ev)| !pred(&ev.kind))
            .collect();
        self.events = BinaryHeap::from(kept);
    }

    /// Decompose `current_cycle` into `(scanline, h_dot)`.
    ///
    /// `scanline` is taken modulo `scanlines_per_frame` so the
    /// position rolls over at frame boundary even if the caller
    /// never resets the scheduler. `h_dot` is in PPU dot units (4
    /// master clocks per dot).
    pub fn current_position(&self) -> (u16, u16) {
        let cps = self.timing.cycles_per_scanline as u64;
        let line = (self.current_cycle / cps) % self.timing.scanlines_per_frame as u64;
        let dot = (self.current_cycle % cps) / 4;
        (line as u16, dot as u16)
    }

    /// Timing profile this scheduler was built with.
    pub fn timing(&self) -> TimingProfile {
        self.timing
    }

    /// Current absolute master cycle (last value passed to
    /// `drain_up_to`, or `reset` baseline).
    pub fn current_cycle(&self) -> u64 {
        self.current_cycle
    }

    /// Number of events currently queued. Diagnostics-only.
    pub fn pending_count(&self) -> usize {
        self.events.len()
    }
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
pub fn schedule_frame_events(scheduler: &mut HEventScheduler, base_cycle: u64, overscan: bool) {
    let timing = scheduler.timing();
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
    fn schedule_then_peek_returns_earliest_cycle() {
        let mut s = HEventScheduler::new(TimingProfile::NTSC);
        s.schedule(500, EventKind::HBlankStart);
        s.schedule(100, EventKind::RenderPos);
        s.schedule(900, EventKind::FrameEnd);
        assert_eq!(s.peek_cycle(), Some(100));
        assert_eq!(s.pending_count(), 3);
    }

    #[test]
    fn peek_cycle_none_when_empty() {
        let s = HEventScheduler::new(TimingProfile::NTSC);
        assert_eq!(s.peek_cycle(), None);
    }

    #[test]
    fn drain_up_to_returns_events_in_order_and_removes_them() {
        let mut s = HEventScheduler::new(TimingProfile::NTSC);
        s.schedule(500, EventKind::HBlankStart);
        s.schedule(100, EventKind::RenderPos);
        s.schedule(900, EventKind::FrameEnd);

        let drained = s.drain_up_to(600);
        assert_eq!(drained, vec![EventKind::RenderPos, EventKind::HBlankStart]);
        assert_eq!(s.pending_count(), 1);
        assert_eq!(s.peek_cycle(), Some(900));
        assert_eq!(s.current_cycle(), 600);

        let drained2 = s.drain_up_to(1000);
        assert_eq!(drained2, vec![EventKind::FrameEnd]);
        assert_eq!(s.pending_count(), 0);
    }

    #[test]
    fn drain_up_to_preserves_fifo_on_tie() {
        let mut s = HEventScheduler::new(TimingProfile::NTSC);
        s.schedule(1000, EventKind::HBlankStart);
        s.schedule(1000, EventKind::RenderPos);
        s.schedule(1000, EventKind::HdmaTrigger);
        let drained = s.drain_up_to(2000);
        assert_eq!(
            drained,
            vec![
                EventKind::HBlankStart,
                EventKind::RenderPos,
                EventKind::HdmaTrigger
            ],
            "tied events must fire in insertion order"
        );
    }

    #[test]
    fn drain_up_to_with_cycles_pairs_each_event_with_its_scheduled_cycle() {
        // Placing each event at its own raster point (Yoshi's Island's HDMA
        // water gradient) needs each drained event tagged with the cycle it
        // was armed for, not the post-drain cursor: a batch deferred behind
        // a long general DMA must replay at each event's own scanline/dot.
        // This is the only difference from `drain_up_to`, which discards
        // the cycle.
        let mut s = HEventScheduler::new(TimingProfile::NTSC);
        s.schedule(100, EventKind::HBlankStart);
        s.schedule(300, EventKind::RenderPos);
        s.schedule(300, EventKind::HdmaTrigger);
        let drained = s.drain_up_to_with_cycles(1000);
        assert_eq!(
            drained,
            vec![
                (100, EventKind::HBlankStart),
                (300, EventKind::RenderPos),
                (300, EventKind::HdmaTrigger),
            ],
            "each event carries its own scheduled cycle, FIFO on tie"
        );
        // The cursor advanced to the drain point, so a re-drain is empty.
        assert!(s.drain_up_to_with_cycles(1000).is_empty());
    }

    #[test]
    fn drain_up_to_excludes_events_strictly_after_cycle() {
        let mut s = HEventScheduler::new(TimingProfile::NTSC);
        s.schedule(100, EventKind::HBlankStart);
        s.schedule(101, EventKind::RenderPos);
        let drained = s.drain_up_to(100);
        assert_eq!(drained, vec![EventKind::HBlankStart]);
        assert_eq!(s.pending_count(), 1);
    }

    #[test]
    fn cancel_matching_drops_only_matching_kinds() {
        let mut s = HEventScheduler::new(TimingProfile::NTSC);
        s.schedule(100, EventKind::IrqV { v_target: 10 });
        s.schedule(200, EventKind::HBlankStart);
        s.schedule(300, EventKind::IrqV { v_target: 20 });
        s.schedule(400, EventKind::VBlankStart);

        s.cancel_matching(|kind| matches!(kind, EventKind::IrqV { .. }));

        assert_eq!(s.pending_count(), 2);
        let drained = s.drain_up_to(1000);
        assert_eq!(
            drained,
            vec![EventKind::HBlankStart, EventKind::VBlankStart]
        );
    }

    #[test]
    fn cycles_of_lists_matching_events_earliest_first() {
        let mut s = HEventScheduler::new(TimingProfile::NTSC);
        s.schedule(300, EventKind::IrqH { h_target: 5 });
        s.schedule(100, EventKind::IrqV { v_target: 10 });
        s.schedule(50, EventKind::HBlankStart);
        s.schedule(200, EventKind::IrqHV { v: 1, h: 2 });
        assert_eq!(s.cycles_of(EventKind::is_irq), vec![100, 200, 300]);
        assert!(s.cycles_of(|k| matches!(k, EventKind::FrameEnd)).is_empty());
        // Listing leaves the queue as it was.
        assert_eq!(s.pending_count(), 4);
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
    fn current_position_computes_scanline_and_hdot() {
        let mut s = HEventScheduler::new(TimingProfile::NTSC);
        // Move into scanline 3, dot 22 (= 22*4 = 88 master clocks into line)
        s.drain_up_to(3 * 1364 + 88);
        assert_eq!(s.current_position(), (3, 22));

        // Move into scanline 225 (VBlank start), dot 0
        s.drain_up_to(225 * 1364);
        assert_eq!(s.current_position(), (225, 0));
    }

    #[test]
    fn current_position_wraps_at_frame_boundary() {
        let mut s = HEventScheduler::new(TimingProfile::NTSC);
        // 1.5 frames forward, scanline must wrap modulo 262.
        let frames = TimingProfile::NTSC.cycles_per_frame();
        s.drain_up_to(frames + 5 * 1364);
        assert_eq!(s.current_position(), (5, 0));
    }

    #[test]
    fn reset_clears_events_and_anchors_cycle() {
        let mut s = HEventScheduler::new(TimingProfile::NTSC);
        s.schedule(500, EventKind::HBlankStart);
        s.schedule(700, EventKind::VBlankStart);
        s.drain_up_to(100); // advance current_cycle, leaves events queued

        s.reset(0);
        assert_eq!(s.pending_count(), 0);
        assert_eq!(s.peek_cycle(), None);
        assert_eq!(s.current_cycle(), 0);
    }

    #[test]
    fn reset_to_nonzero_anchor() {
        let mut s = HEventScheduler::new(TimingProfile::PAL);
        s.reset(12_345);
        assert_eq!(s.current_cycle(), 12_345);
        let (line, dot) = s.current_position();
        // 12345 / 1364 = 9, 12345 % 1364 = 69 -> dot 17
        assert_eq!(line, 9);
        assert_eq!(dot, 17);
    }

    #[test]
    fn schedule_frame_events_ntsc_counts_match_spec() {
        let mut s = HEventScheduler::new(TimingProfile::NTSC);
        schedule_frame_events(&mut s, 0, false);

        // RenderPos always covers lines 1..=240 (overscan-aware
        // gate lives inside the PPU): 240 RenderPos events
        // regardless of NTSC vs PAL.
        // 262 HBlankStart + 240 RenderPos + 1 VBlankStart + 1
        // VBlankEnd + 1 FrameEnd + 1 HdmaInit + 225 HdmaTrigger
        // (one per visible line 0..225).
        assert_eq!(s.pending_count(), 262 + 240 + 1 + 1 + 1 + 1 + 225);

        let drained = s.drain_up_to(u64::MAX);
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
        let mut s = HEventScheduler::new(TimingProfile::PAL);
        schedule_frame_events(&mut s, 0, false);
        // RenderPos always covers lines 1..=240, same as NTSC.
        // 312 HBlankStart + 240 RenderPos + 1 VBlankStart +
        // 1 VBlankEnd + 1 FrameEnd + 1 HdmaInit + 240 HdmaTrigger
        // (one per visible line 0..240).
        assert_eq!(s.pending_count(), 312 + 240 + 1 + 1 + 1 + 1 + 240);

        let drained = s.drain_up_to(u64::MAX);
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
        let mut s = HEventScheduler::new(TimingProfile::NTSC);
        schedule_frame_events(&mut s, 0, true);
        // 262 HBlank + 240 RenderPos + 1 VBlankStart + 1 VBlankEnd +
        // 1 FrameEnd + 1 HdmaInit + 240 HdmaTrigger.
        assert_eq!(s.pending_count(), 262 + 240 + 1 + 1 + 1 + 1 + 240);

        let mut vbl_start_cycle = None;
        let mut hdma_triggers = 0usize;
        while let Some(cyc) = s.peek_cycle() {
            for ev in s.drain_up_to(cyc) {
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
        let mut s = HEventScheduler::new(TimingProfile::NTSC);
        schedule_frame_events(&mut s, 0, false);
        // Collect (cycle, kind) tuples by repeatedly peeking +
        // draining.
        let mut render_pos_cycles: Vec<u64> = Vec::new();
        while let Some(cyc) = s.peek_cycle() {
            for ev in s.drain_up_to(cyc) {
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
        let mut s = HEventScheduler::new(TimingProfile::NTSC);
        schedule_frame_events(&mut s, 0, false);
        let mut init_cycles: Vec<u64> = Vec::new();
        let mut trigger_cycles: Vec<u64> = Vec::new();
        while let Some(cyc) = s.peek_cycle() {
            for ev in s.drain_up_to(cyc) {
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
        let mut s = HEventScheduler::new(TimingProfile::NTSC);
        schedule_frame_events(&mut s, 0, false);
        let drained = s.drain_up_to(u64::MAX);
        // First VBlankStart event must be at cycle 225 * 1364, the
        // pre-schedule loop placed it on the line_base of vbl_start.
        // Since drain_up_to flattens to kinds, we can't easily
        // re-derive the cycle. So instead, schedule into an empty
        // scheduler, then peek before drain:
        let mut s2 = HEventScheduler::new(TimingProfile::NTSC);
        schedule_frame_events(&mut s2, 1_000_000, false);
        let mut found_vbl = None;
        while let Some(cyc) = s2.peek_cycle() {
            let evs = s2.drain_up_to(cyc);
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
        let mut s = HEventScheduler::new(TimingProfile::NTSC);
        schedule_frame_events(&mut s, base, false);
        let mut vbl_end = None;
        while let Some(cyc) = s.peek_cycle() {
            for ev in s.drain_up_to(cyc) {
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
        let mut s = HEventScheduler::new(TimingProfile::NTSC);
        schedule_frame_events(&mut s, 0, false);
        // Pull all events into an order list, tagged with their cycle
        // by interleaving peek + drain_up_to(peek_cycle).
        let mut sequence: Vec<(u64, EventKind)> = Vec::new();
        while let Some(cyc) = s.peek_cycle() {
            for ev in s.drain_up_to(cyc) {
                sequence.push((cyc, ev.clone()));
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

    #[test]
    fn timing_getter_round_trips() {
        let s = HEventScheduler::new(TimingProfile::PAL);
        assert_eq!(s.timing(), TimingProfile::PAL);
    }

    #[test]
    fn schedule_after_drain_keeps_unique_sequence_numbers() {
        // Regression: if next_seq wasn't preserved across schedules
        // that come after a drain, two new events at the same cycle
        // could collide on their ScheduledEvent Ord. This test guards
        // that drains never reset next_seq mid-frame.
        let mut s = HEventScheduler::new(TimingProfile::NTSC);
        s.schedule(100, EventKind::HBlankStart);
        s.drain_up_to(100);
        s.schedule(200, EventKind::RenderPos);
        s.schedule(200, EventKind::HdmaTrigger);
        let drained = s.drain_up_to(300);
        assert_eq!(
            drained,
            vec![EventKind::RenderPos, EventKind::HdmaTrigger],
            "post-drain schedules must keep FIFO order on tie"
        );
    }
}
