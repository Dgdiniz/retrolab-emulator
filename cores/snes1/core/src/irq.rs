//! 5A22 interrupt scheduling.
//!
//! NMI is edge-triggered at the start of VBlank and is driven
//! directly from the `VBlankStart` scheduler event in `system.rs`,
//! so it needs no per-frame pre-scheduling. The H/V/HV IRQ sources,
//! by contrast, fire at raster targets programmed through HTIME /
//! VTIME (`$4207-$420A`) and gated by NMITIMEN (`$4200`) bits 5/4,
//! so they are pre-scheduled into the H-event scheduler each frame
//! (and re-armed whenever those registers change).
//!
//! This module owns the pure, bus-free scheduling math:
//! [`schedule_irq_events`] decodes the IRQ mode and pushes the right
//! `EventKind::Irq*` events for a frame. Keeping it separate from
//! [`crate::scheduler::schedule_frame_events`] means the per-frame
//! event-count assertions on that function stay stable, and the IRQ
//! placement is unit-testable against a bare scheduler.
//!
//! The placement is **instruction-granular**, matching the rest of
//! the CPU timing model (whole instructions, with the events handled
//! between them): V-IRQ lands at dot 0 of the target
//! scanline, H-IRQ at `htime * 4` master clocks into each scanline,
//! and HV-IRQ at the single (scanline, dot) point. This is coarser
//! than a per-cycle compare (what bsnes-plus does) but is sufficient
//! for the scanline-window HUD effects that drive the IRQ feature.

use crate::scheduler::{EventKind, HEventScheduler, TimingProfile};

/// Which raster IRQ source is armed, decoded from NMITIMEN
/// (`$4200`) bit 5 (V) and bit 4 (H).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrqMode {
    /// Neither V nor H IRQ enabled.
    Disabled,
    /// V-counter IRQ: fires once per frame on the target scanline.
    V,
    /// H-counter IRQ: fires every scanline at the target dot.
    H,
    /// HV IRQ: fires once per frame at the (scanline, dot) point.
    Hv,
}

/// Decode the armed IRQ source from NMITIMEN (`$4200`).
pub fn irq_mode(nmitimen: u8) -> IrqMode {
    let v = (nmitimen & 0x20) != 0;
    let h = (nmitimen & 0x10) != 0;
    match (v, h) {
        (false, false) => IrqMode::Disabled,
        (true, false) => IrqMode::V,
        (false, true) => IrqMode::H,
        (true, true) => IrqMode::Hv,
    }
}

/// Master cycle of a V-IRQ trigger: dot 0 of scanline `vtime`.
pub fn v_irq_cycle(base_cycle: u64, vtime: u16, cycles_per_scanline: u64) -> u64 {
    base_cycle + (vtime as u64) * cycles_per_scanline
}

/// Master cycle of an H-IRQ trigger: dot `htime` of scanline `line`
/// (4 master clocks per dot).
pub fn h_irq_cycle(base_cycle: u64, line: u16, htime: u16, cycles_per_scanline: u64) -> u64 {
    base_cycle + (line as u64) * cycles_per_scanline + (htime as u64) * 4
}

/// Pre-schedule the H/V/HV IRQ events for one frame.
///
/// `base_cycle` is the master clock at the top of the current frame;
/// `min_cycle` filters out any trigger whose raster point has already
/// passed (so a mid-frame HTIME/VTIME/NMITIMEN rewrite re-arms only
/// the still-future triggers rather than re-firing one for a scanline
/// the beam already crossed).
pub fn schedule_irq_events(
    scheduler: &mut HEventScheduler,
    base_cycle: u64,
    nmitimen: u8,
    htime: u16,
    vtime: u16,
    timing: TimingProfile,
    min_cycle: u64,
) {
    let cps = timing.cycles_per_scanline as u64;
    let lines = timing.scanlines_per_frame;
    // A dot beyond the scanline length never matches on that line, so
    // an out-of-range HTIME disables H/HV entirely.
    let htime_in_line = (htime as u64) * 4 < cps;

    match irq_mode(nmitimen) {
        IrqMode::Disabled => {}
        IrqMode::V => {
            if vtime < lines {
                let at = v_irq_cycle(base_cycle, vtime, cps);
                if at >= min_cycle {
                    scheduler.schedule(at, EventKind::IrqV { v_target: vtime });
                }
            }
        }
        IrqMode::H => {
            if htime_in_line {
                for line in 0..lines {
                    let at = h_irq_cycle(base_cycle, line, htime, cps);
                    if at >= min_cycle {
                        scheduler.schedule(at, EventKind::IrqH { h_target: htime });
                    }
                }
            }
        }
        IrqMode::Hv => {
            if vtime < lines && htime_in_line {
                let at = h_irq_cycle(base_cycle, vtime, htime, cps);
                if at >= min_cycle {
                    scheduler.schedule(at, EventKind::IrqHV { v: vtime, h: htime });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NTSC: TimingProfile = TimingProfile::NTSC;
    const CPS: u64 = 1364;

    fn fresh() -> HEventScheduler {
        HEventScheduler::new(NTSC)
    }

    #[test]
    fn mode_decode_from_nmitimen() {
        assert_eq!(irq_mode(0x00), IrqMode::Disabled);
        assert_eq!(irq_mode(0x80), IrqMode::Disabled); // NMI bit irrelevant
        assert_eq!(irq_mode(0x20), IrqMode::V);
        assert_eq!(irq_mode(0x10), IrqMode::H);
        assert_eq!(irq_mode(0x30), IrqMode::Hv);
        assert_eq!(irq_mode(0xB0), IrqMode::Hv); // NMI + V + H
    }

    #[test]
    fn disabled_schedules_nothing() {
        let mut s = fresh();
        schedule_irq_events(&mut s, 0, 0x00, 0x10, 0x20, NTSC, 0);
        assert_eq!(s.pending_count(), 0);
    }

    #[test]
    fn v_only_schedules_one_event_on_target_line() {
        let mut s = fresh();
        // V-IRQ, vtime = 0x50 (80).
        schedule_irq_events(&mut s, 0, 0x20, 0x00, 0x50, NTSC, 0);
        assert_eq!(s.pending_count(), 1);
        let at = 0x50 * CPS;
        assert_eq!(s.peek_cycle(), Some(at));
        assert_eq!(s.drain_up_to(at), vec![EventKind::IrqV { v_target: 0x50 }]);
    }

    #[test]
    fn v_only_skips_when_target_out_of_range() {
        let mut s = fresh();
        // vtime past the 262-line frame never fires.
        schedule_irq_events(&mut s, 0, 0x20, 0x00, 300, NTSC, 0);
        assert_eq!(s.pending_count(), 0);
    }

    #[test]
    fn h_only_schedules_every_scanline() {
        let mut s = fresh();
        // H-IRQ, htime = 0x20 (32 dots).
        schedule_irq_events(&mut s, 0, 0x10, 0x20, 0x00, NTSC, 0);
        assert_eq!(s.pending_count(), NTSC.scanlines_per_frame as usize);
        // First trigger is on line 0 at dot 32 = 128 master clocks.
        assert_eq!(s.peek_cycle(), Some(0x20 * 4));
    }

    #[test]
    fn h_only_out_of_range_dot_disables() {
        let mut s = fresh();
        // htime * 4 >= cps (1364) -> 341 dots is past the line length.
        schedule_irq_events(&mut s, 0, 0x10, 341, 0x00, NTSC, 0);
        assert_eq!(s.pending_count(), 0);
    }

    #[test]
    fn hv_schedules_one_event_at_point() {
        let mut s = fresh();
        // HV: vtime = 100, htime = 0x40 (64 dots).
        schedule_irq_events(&mut s, 0, 0x30, 0x40, 100, NTSC, 0);
        assert_eq!(s.pending_count(), 1);
        let at = 100 * CPS + 0x40 * 4;
        assert_eq!(s.peek_cycle(), Some(at));
        assert_eq!(
            s.drain_up_to(at),
            vec![EventKind::IrqHV { v: 100, h: 0x40 }]
        );
    }

    #[test]
    fn min_cycle_filters_past_triggers() {
        let mut s = fresh();
        // H-IRQ every line at dot 0, but we are already 10 lines in:
        // only the future lines (10..262) are armed.
        let min = 10 * CPS;
        schedule_irq_events(&mut s, 0, 0x10, 0x00, 0x00, NTSC, min);
        assert_eq!(s.pending_count(), (NTSC.scanlines_per_frame as usize) - 10);
        // The earliest armed trigger is line 10.
        assert_eq!(s.peek_cycle(), Some(min));
    }

    #[test]
    fn base_cycle_offsets_all_triggers() {
        let mut s = fresh();
        let base = 1_000_000;
        schedule_irq_events(&mut s, base, 0x20, 0x00, 0x50, NTSC, base);
        assert_eq!(s.peek_cycle(), Some(base + 0x50 * CPS));
    }
}
