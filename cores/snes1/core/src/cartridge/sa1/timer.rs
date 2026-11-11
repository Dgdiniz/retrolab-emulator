//! The SA-1's H/V timer (`$2210-$2215` written, `$2302-$2305` read).
//!
//! Two counters run on the SA-1's own clock, two master cycles at a time,
//! whether or not the SA-1 CPU is running, and raise the timer interrupt when
//! they reach the position the program set:
//!
//! - **H/V timer** (TMC bit 7 clear): the horizontal counter covers one
//!   scanline (1 364 master cycles, 341 dots of 4 cycles) and the vertical
//!   counter counts lines up to the console's frame (262 NTSC, 312 PAL), a
//!   raster of the SA-1's own, independent of the PPU's.
//! - **Linear timer** (TMC bit 7 set): the two make one free-running counter,
//!   an 11-bit horizontal part carrying into a 9-bit vertical part.
//!
//! The horizontal target (HCNT) is in dots and the counter in master cycles,
//! so the comparison is against four times the target. Bits 0 and 1 of TMC
//! choose what must match: the horizontal position, the line (at its first
//! dot), or both. A target is compared after every two-cycle step, so a
//! position the program asks for is never stepped over.
//!
//! The counting and the comparison follow ares's SA-1 (`SA1::step`,
//! `sfc/coprocessor/sa1/sa1.cpp`, ISC).

use super::registers::{flag, pack, Bytes};
use crate::savestate::Sa1TimerState;

/// One scanline, in master cycles: 341 dots of 4 cycles.
const CLOCKS_PER_LINE: u16 = 1364;

/// Lines per frame on an NTSC console, the default until the region is set.
const NTSC_LINES: u16 = 262;

/// The SA-1's H/V timer.
pub(super) struct Sa1Timer {
    /// The horizontal counter, in master cycles (H/V timer: `0..1364`;
    /// linear timer: `0..2048`). Always even: it moves two cycles at a time.
    h_counter: u16,
    /// The vertical counter (H/V timer: the line, `0..lines_per_frame`;
    /// linear timer: `0..512`).
    v_counter: u16,
    /// Where the H/V timer's vertical counter wraps: the console's lines per
    /// frame (262 NTSC, 312 PAL).
    lines_per_frame: u16,
    /// TMC (`$2210`) bit 7: the linear timer instead of the H/V one.
    linear: bool,
    /// TMC bit 1: the vertical position must match.
    v_compare: bool,
    /// TMC bit 0: the horizontal position must match.
    h_compare: bool,
    /// HCNT (`$2212-$2213`): the horizontal target, in dots.
    h_target: u16,
    /// VCNT (`$2214-$2215`): the vertical target.
    v_target: u16,
    /// HCR (`$2302-$2303`): the horizontal counter in dots, as the last read
    /// of `$2302` found it.
    h_latched: u16,
    /// VCR (`$2304-$2305`): the vertical counter, as the last read of
    /// `$2302` found it.
    v_latched: u16,
}

impl Default for Sa1Timer {
    fn default() -> Self {
        Self {
            h_counter: 0,
            v_counter: 0,
            lines_per_frame: NTSC_LINES,
            linear: false,
            v_compare: false,
            h_compare: false,
            h_target: 0,
            v_target: 0,
            h_latched: 0,
            v_latched: 0,
        }
    }
}

impl Sa1Timer {
    /// Set the lines per frame of the console's region, where the H/V
    /// timer's vertical counter wraps.
    pub(super) fn set_lines_per_frame(&mut self, lines: u16) {
        self.lines_per_frame = lines.max(1);
    }

    /// TMC (`$2210`): which timer, and which positions must match.
    pub(super) fn write_control(&mut self, value: u8) {
        self.linear = flag(value, 7);
        self.v_compare = flag(value, 1);
        self.h_compare = flag(value, 0);
    }

    /// CTR (`$2211`): any write sets both counters back to zero.
    pub(super) fn restart(&mut self) {
        self.h_counter = 0;
        self.v_counter = 0;
    }

    /// One byte of HCNT (`$2212` + `index`).
    pub(super) fn write_h_target(&mut self, index: usize, value: u8) {
        self.h_target = self.h_target.with_byte(index, value);
    }

    /// One byte of VCNT (`$2214` + `index`).
    pub(super) fn write_v_target(&mut self, index: usize, value: u8) {
        self.v_target = self.v_target.with_byte(index, value);
    }

    /// What a read of `$2302` does besides answering: it copies the counters
    /// into HCR (in dots) and VCR. Only the CPU's read calls this; the
    /// debugger reads the counters without moving the copy.
    pub(super) fn latch(&mut self) {
        self.h_latched = self.h_counter / 4;
        self.v_latched = self.v_counter;
    }

    /// HCR, as the last read of `$2302` left it (dots).
    pub(super) fn h_latched(&self) -> u16 {
        self.h_latched
    }

    /// VCR, as the last read of `$2302` left it.
    pub(super) fn v_latched(&self) -> u16 {
        self.v_latched
    }

    /// The horizontal counter now, in master cycles, for the debugger.
    pub(super) fn h_counter(&self) -> u16 {
        self.h_counter
    }

    /// The vertical counter now, for the debugger.
    pub(super) fn v_counter(&self) -> u16 {
        self.v_counter
    }

    /// TMC as written (bits 7, 1 and 0), for the debugger.
    pub(super) fn control_byte(&self) -> u8 {
        pack(
            [(self.linear, 7), (self.v_compare, 1), (self.h_compare, 0)],
            0,
        )
    }

    /// HCNT (dots), for the debugger.
    pub(super) fn h_target(&self) -> u16 {
        self.h_target
    }

    /// VCNT, for the debugger.
    pub(super) fn v_target(&self) -> u16 {
        self.v_target
    }

    /// Run the counters for `cycles` master cycles, two at a time, and say
    /// whether the target was reached at any step. The caller raises the
    /// timer interrupt, which stays raised until the program clears it, so
    /// one answer for the whole span is enough. An odd last cycle is not
    /// counted: the counters only move in steps of two.
    pub(super) fn advance(&mut self, cycles: u64) -> bool {
        let mut reached = false;
        for _ in 0..cycles / 2 {
            reached |= self.step();
        }
        reached
    }

    /// One step of two master cycles; whether the counters now stand on the
    /// target.
    fn step(&mut self) -> bool {
        self.h_counter += 2;
        if self.linear {
            self.v_counter = self.v_counter.wrapping_add(self.h_counter >> 11) & 0x1ff;
            self.h_counter &= 0x7ff;
        } else if self.h_counter >= CLOCKS_PER_LINE {
            self.h_counter = 0;
            self.v_counter += 1;
            if self.v_counter >= self.lines_per_frame {
                self.v_counter = 0;
            }
        }
        self.on_target()
    }

    /// Whether the counters stand where TMC asks. The horizontal target is
    /// widened before it is scaled to cycles, so a large one simply never
    /// matches.
    fn on_target(&self) -> bool {
        let h_matches = u32::from(self.h_counter) == u32::from(self.h_target) * 4;
        let v_matches = self.v_counter == self.v_target;
        match (self.v_compare, self.h_compare) {
            (false, false) => false,
            (false, true) => h_matches,
            (true, false) => v_matches && self.h_counter == 0,
            (true, true) => v_matches && h_matches,
        }
    }

    /// The timer for a savestate.
    pub(super) fn snapshot(&self) -> Sa1TimerState {
        Sa1TimerState {
            hcounter: self.h_counter,
            vcounter: self.v_counter,
            scanlines: self.lines_per_frame,
            hvselb: self.linear,
            ven: self.v_compare,
            hen: self.h_compare,
            hcnt: self.h_target,
            vcnt: self.v_target,
            hcr: self.h_latched,
            vcr: self.v_latched,
        }
    }

    /// The timer from a savestate; a frame of no lines, which no console
    /// has, is read as one line so the counting cannot stall.
    pub(super) fn restore(&mut self, state: &Sa1TimerState) {
        self.h_counter = state.hcounter;
        self.v_counter = state.vcounter;
        self.lines_per_frame = state.scanlines.max(1);
        self.linear = state.hvselb;
        self.v_compare = state.ven;
        self.h_compare = state.hen;
        self.h_target = state.hcnt;
        self.v_target = state.vcnt;
        self.h_latched = state.hcr;
        self.v_latched = state.vcr;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_horizontal_target_is_reached_at_its_dot() {
        let mut t = Sa1Timer::default();
        t.write_h_target(0, 10); // dot 10 = cycle 40
        t.write_control(0x01);
        assert!(!t.advance(38));
        assert_eq!(t.h_counter(), 38);
        assert!(t.advance(2));
        assert_eq!(t.h_counter(), 40);
    }

    #[test]
    fn a_vertical_target_is_reached_at_the_start_of_its_line() {
        let mut t = Sa1Timer::default();
        t.write_v_target(0, 2);
        t.write_control(0x02);
        assert!(t.advance(2 * u64::from(CLOCKS_PER_LINE)));
        assert_eq!((t.h_counter(), t.v_counter()), (0, 2));
    }

    #[test]
    fn both_targets_must_match_together() {
        let mut t = Sa1Timer::default();
        t.write_h_target(0, 5);
        t.write_v_target(0, 1);
        t.write_control(0x03);
        assert!(t.advance(u64::from(CLOCKS_PER_LINE) + 20));
        assert_eq!((t.h_counter(), t.v_counter()), (20, 1));
    }

    #[test]
    fn with_no_comparison_chosen_nothing_is_reached() {
        let mut t = Sa1Timer::default();
        assert!(!t.advance(10 * u64::from(CLOCKS_PER_LINE)));
    }

    #[test]
    fn a_restart_sets_both_counters_to_zero() {
        let mut t = Sa1Timer::default();
        t.advance(5000);
        assert!(t.h_counter() != 0 || t.v_counter() != 0);
        t.restart();
        assert_eq!((t.h_counter(), t.v_counter()), (0, 0));
    }

    #[test]
    fn the_hv_timer_wraps_the_line_and_the_frame() {
        let mut t = Sa1Timer::default();
        t.set_lines_per_frame(3);
        t.advance(u64::from(CLOCKS_PER_LINE));
        assert_eq!((t.h_counter(), t.v_counter()), (0, 1));
        t.advance(2 * u64::from(CLOCKS_PER_LINE));
        assert_eq!((t.h_counter(), t.v_counter()), (0, 0));
    }

    #[test]
    fn the_linear_timer_carries_horizontal_into_vertical() {
        let mut t = Sa1Timer::default();
        t.write_control(0x80);
        t.advance(2048);
        assert_eq!((t.h_counter(), t.v_counter()), (0, 1));
    }

    #[test]
    fn a_read_of_2302_copies_the_counters_in_dots() {
        let mut t = Sa1Timer::default();
        t.advance(400);
        t.latch();
        assert_eq!((t.h_latched(), t.v_latched()), (100, 0));
        t.advance(400);
        assert_eq!(t.h_latched(), 100, "the copy holds until the next read");
        t.latch();
        assert_eq!(t.h_latched(), 200);
    }

    #[test]
    fn the_timer_survives_a_savestate() {
        let mut t = Sa1Timer::default();
        t.set_lines_per_frame(312);
        t.write_control(0x83);
        t.write_h_target(0, 0x34);
        t.write_h_target(1, 0x12);
        t.write_v_target(0, 0x78);
        t.advance(1234);
        t.latch();
        let snap = t.snapshot();
        let mut restored = Sa1Timer::default();
        restored.restore(&snap);
        assert_eq!(restored.snapshot(), snap);
        assert_eq!(restored.control_byte(), 0x83);
        assert_eq!((restored.h_target(), restored.v_target()), (0x1234, 0x0078));
    }
}
