//! SPC7110 RTC-4513 real-time clock (`$4840-$4842`).
//!
//! A byte-faithful port of the RTC half of the SPC7110 emulator by byuu
//! and neviksti (its notice is at the end of `LICENSE`), read in the copy
//! bsnes-plus carries: the clock update, the `$4840`/`$4841` state
//! machine, and the `$4842` status port. Present only on the `$F9`
//! cartridges (Tengai Makyou Zero).
//!
//! ## What this unit owns vs what the chip owns
//!
//! The RTC-4513 is a 20-byte register file (bsnes `memory::cartrtc`,
//! allocated filled with `0xFF`): 16 nibble registers (indices `0-15`) plus a
//! 4-byte host-clock timestamp (indices `16-19`, little-endian). bsnes keeps
//! that file **persistent** (a separate `.rtc` save) and the command-machine
//! fields (`rtc_state`/`rtc_mode`/`rtc_index`, `r4840`/`r4841`/`r4842`)
//! **volatile** (savestate).
//!
//! This module mirrors that split: [`Spc7110Rtc`] holds only the *volatile*
//! command machine; the *persistent* 20-byte file is passed in as a `&[u8]` /
//! `&mut [u8]` slice by the hosting [`Spc7110Chip`](super::Spc7110Chip), which
//! stores it contiguously with the game SRAM so a single
//! [`Coprocessor::battery`](retro_core::cartridge::Coprocessor::battery) blob
//! persists both. Keeping the data out of the struct also makes every method
//! trivially unit-testable against a local `[u8; 20]`.
//!
//! ## Clock injection
//!
//! `update_time` compares the host wall clock to the stored timestamp. bsnes
//! calls `time(0)`; to keep the tick deterministic under test we take an
//! [`RtcClock`] instead of reading `SystemTime` directly.

use crate::savestate::Spc7110RtcState;

/// Number of bytes in the RTC-4513 register file (indices `0-19`).
pub(super) const RTC_FILE_LEN: usize = 20;

/// Days per month (non-leap), bsnes `SPC7110::months`.
const MONTHS: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

/// A source of the host wall-clock time (Unix seconds). Injected so
/// `update_time` is deterministic under test.
pub(super) trait RtcClock: Send + Sync {
    /// Current time in whole seconds since the Unix epoch.
    fn now_unix(&self) -> i64;
}

/// A process-wide pin on the host clock: when set, every production clock
/// reports this time instead of the real one, so a run that reads the
/// RTC is reproducible, the golden harness pins it, and two netplay
/// peers could agree on one. `i64::MIN` means "not pinned".
static FIXED_UNIX_TIME: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(i64::MIN);

/// Pin (`Some(secs)`) or release (`None`) the time every production RTC
/// clock reports, in whole seconds since the Unix epoch.
pub fn set_fixed_unix_time(secs: Option<i64>) {
    FIXED_UNIX_TIME.store(
        secs.unwrap_or(i64::MIN),
        std::sync::atomic::Ordering::SeqCst,
    );
}

/// The current pin, if any.
pub fn fixed_unix_time() -> Option<i64> {
    match FIXED_UNIX_TIME.load(std::sync::atomic::Ordering::SeqCst) {
        i64::MIN => None,
        secs => Some(secs),
    }
}

/// Production clock: reads the real host wall clock, `std::time::SystemTime`
/// natively, `Date.now()` under wasm (where `std::time` panics with "time not
/// implemented on this platform"), unless a time is pinned with
/// [`set_fixed_unix_time`]. A read before the epoch (or a failure)
/// reports `0`.
pub(super) struct SystemClock;

impl RtcClock for SystemClock {
    fn now_unix(&self) -> i64 {
        if let Some(pinned) = fixed_unix_time() {
            return pinned;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        }

        // `std::time::SystemTime::now()` is unimplemented on
        // `wasm32-unknown-unknown` and panics; the browser clock
        // (`Date.now()`, milliseconds since the Unix epoch) is the wasm
        // equivalent.
        #[cfg(target_arch = "wasm32")]
        {
            (js_sys::Date::now() / 1000.0) as i64
        }
    }
}

/// The RTC command-machine state (bsnes `RTC_State`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum RtcState {
    /// Disabled; `$4841` reads/writes are inert.
    Inactive,
    /// Awaiting the Linear/Indexed mode byte on `$4841`.
    ModeSelect,
    /// Awaiting the register index on `$4841`.
    IndexSelect,
    /// Sequential-write mode (Linear only); each `$4841` write stores a
    /// nibble and auto-increments the index.
    Write,
}

/// The RTC access mode (bsnes `RTC_Mode`: `Linear = 0x03`, `Indexed = 0x0c`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum RtcMode {
    /// Sequential write (index-select advances to [`RtcState::Write`]).
    Linear,
    /// Random-access read (index-select stays put; reads auto-increment).
    Indexed,
}

/// The RTC-4513 command machine (`$4840`/`$4841`/`$4842`). The 20-byte data
/// file it reads and writes is owned by the chip and passed in per call.
#[derive(Clone)]
pub(super) struct Spc7110Rtc {
    /// `$4840` enable/latch (last written value).
    r4840: u8,
    /// `$4841` index/data port (last written value; the read side is computed).
    r4841: u8,
    /// `$4842` status (bit 7 = ready, cleared on read).
    r4842: u8,
    state: RtcState,
    mode: RtcMode,
    /// Current register index (`0-15`), auto-incrementing on read/write.
    index: u8,
}

impl Default for Spc7110Rtc {
    fn default() -> Self {
        Self {
            r4840: 0,
            r4841: 0,
            r4842: 0,
            state: RtcState::Inactive,
            mode: RtcMode::Linear,
            index: 0,
        }
    }
}

impl Spc7110Rtc {
    /// Return the command machine to its power-on state. The 20-byte data file
    /// is *not* touched here, it is persistent hardware state the chip keeps
    /// across a reset (bsnes clears only `r4840-r4842` + the machine fields).
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    /// CPU read of `$4840` (the enable latch).
    pub(super) fn read_4840(&self) -> u8 {
        self.r4840
    }

    /// CPU read of `$4841`: the index/data port. Inert (returns `0`) while
    /// `Inactive`/`ModeSelect`; otherwise sets the ready flag, returns the
    /// current register, and auto-increments the index.
    pub(super) fn read_4841(&mut self, regs: &[u8]) -> u8 {
        if self.state == RtcState::Inactive || self.state == RtcState::ModeSelect {
            return 0x00;
        }
        self.r4842 = 0x80;
        let data = regs.get(self.index as usize).copied().unwrap_or(0);
        self.index = (self.index + 1) & 15;
        data
    }

    /// Debugger read of `$4841`, the current register with **no** side
    /// effects (no ready-flag set, no index advance).
    pub(super) fn debug_read_4841(&self, regs: &[u8]) -> u8 {
        if self.state == RtcState::Inactive || self.state == RtcState::ModeSelect {
            return 0x00;
        }
        regs.get(self.index as usize).copied().unwrap_or(0)
    }

    /// CPU read of `$4842`: the status byte, clearing bit 7 (read-to-clear).
    pub(super) fn read_4842(&mut self) -> u8 {
        let status = self.r4842;
        self.r4842 &= 0x7f;
        status
    }

    /// `$4842` status without the read-to-clear side effect (debugger view).
    pub(super) fn status(&self) -> u8 {
        self.r4842
    }

    /// CPU write of `$4840`. Bit 0 clear disables the RTC (and syncs the clock
    /// one last time); bit 0 set arms it into `ModeSelect`.
    pub(super) fn write_4840(&mut self, regs: &mut [u8], data: u8, clock: &dyn RtcClock) {
        self.r4840 = data;
        if data & 1 == 0 {
            self.state = RtcState::Inactive;
            self.update_time(regs, clock, 0);
        } else {
            self.r4842 = 0x80;
            self.state = RtcState::ModeSelect;
        }
    }

    /// CPU write of `$4841`. Drives the command machine: pick the mode, then
    /// the index, then (Linear) stream nibbles, applying the CR0/CR2 control
    /// side effects on indices 13/15.
    pub(super) fn write_4841(&mut self, regs: &mut [u8], data: u8, clock: &dyn RtcClock) {
        self.r4841 = data;
        match self.state {
            RtcState::ModeSelect => {
                if data == 0x03 || data == 0x0c {
                    self.r4842 = 0x80;
                    self.state = RtcState::IndexSelect;
                    self.mode = if data == 0x03 {
                        RtcMode::Linear
                    } else {
                        RtcMode::Indexed
                    };
                    self.index = 0;
                }
            }
            RtcState::IndexSelect => {
                self.r4842 = 0x80;
                self.index = data & 15;
                if self.mode == RtcMode::Linear {
                    self.state = RtcState::Write;
                }
            }
            RtcState::Write => {
                self.r4842 = 0x80;
                self.apply_control_side_effects(regs, data, clock);
                if let Some(cell) = regs.get_mut(self.index as usize) {
                    *cell = data & 15;
                }
                self.index = (self.index + 1) & 15;
            }
            RtcState::Inactive => {}
        }
    }

    /// The CR0 (index 13) and CR2 (index 15) write side effects, applied
    /// *before* the nibble is stored (bsnes order).
    fn apply_control_side_effects(&mut self, regs: &mut [u8], data: u8, clock: &dyn RtcClock) {
        // Control register 0.
        if self.index == 13 {
            // Increment second counter.
            if data & 2 != 0 {
                self.update_time(regs, clock, 1);
            }
            // Round minute counter.
            if data & 8 != 0 {
                self.update_time(regs, clock, 0);
                let second = bcd2(regs, 0);
                regs[0] = 0;
                regs[1] = 0;
                if second >= 30 {
                    self.update_time(regs, clock, 60);
                }
            }
        }
        // Control register 2.
        if self.index == 15 {
            // Disable timer and clear the second counter.
            if (data & 1 != 0) && (regs[15] & 1 == 0) {
                self.update_time(regs, clock, 0);
                regs[0] = 0;
                regs[1] = 0;
            }
            // Disable timer.
            if (data & 2 != 0) && (regs[15] & 2 == 0) {
                self.update_time(regs, clock, 0);
            }
        }
    }

    /// Advance the BCD calendar by the seconds elapsed since the stored
    /// timestamp, then restamp it, bsnes `SPC7110::update_time`. `offset` is
    /// *subtracted* from the host clock (the CR0/CR2 side effects pass `1`/`60`
    /// to nudge the reference point). No-op advance when the timer is disabled
    /// (CR0 bit0 / CR2 bits0-1) or when no time has elapsed; the timestamp is
    /// always refreshed.
    pub(super) fn update_time(&self, regs: &mut [u8], clock: &dyn RtcClock, offset: i64) {
        let rtc_time = i64::from(regs[16])
            | (i64::from(regs[17]) << 8)
            | (i64::from(regs[18]) << 16)
            | (i64::from(regs[19]) << 24);
        let current_time = clock.now_unix() - offset;

        // Match bsnes' overflow/underflow-tolerant elapsed-seconds calc: a
        // stored timestamp in the "future" (e.g. the 0xFF power-on fill) yields
        // a huge diff that the half-range clamp collapses to zero.
        let mut diff = if current_time >= rtc_time {
            current_time - rtc_time
        } else {
            i64::MAX - rtc_time + current_time + 1
        };
        if diff > i64::MAX / 2 {
            diff = 0;
        }

        let mut update = true;
        if regs[13] & 1 != 0 {
            update = false; // CR0 timer-disable
        }
        if regs[15] & 3 != 0 {
            update = false; // CR2 timer-disable
        }

        if diff > 0 && update {
            let mut second = bcd2(regs, 0);
            let mut minute = bcd2(regs, 2);
            let mut hour = bcd2(regs, 4);
            let mut day = bcd2(regs, 6);
            let mut month = bcd2(regs, 8);
            let mut year = bcd2(regs, 10);
            let mut weekday = u32::from(regs[12]);

            day = day.wrapping_sub(1);
            month = month.wrapping_sub(1);
            year += if year >= 90 { 1900 } else { 2000 };

            // bsnes computes `second += diff` in 32-bit unsigned (wrapping).
            second = second.wrapping_add(diff as u32);
            while second >= 60 {
                second -= 60;

                minute += 1;
                if minute < 60 {
                    continue;
                }
                minute = 0;

                hour += 1;
                if hour < 24 {
                    continue;
                }
                hour = 0;

                day += 1;
                weekday = (weekday + 1) % 7;
                let mut days = MONTHS[(month % 12) as usize];
                if days == 28 {
                    let mut leapyear = false;
                    if year.is_multiple_of(4) {
                        leapyear = true;
                        if year.is_multiple_of(100) && !year.is_multiple_of(400) {
                            leapyear = false;
                        }
                    }
                    if leapyear {
                        days += 1;
                    }
                }
                if day < days {
                    continue;
                }
                day = 0;

                month += 1;
                if month < 12 {
                    continue;
                }
                month = 0;

                year += 1;
            }

            day += 1;
            month += 1;
            year %= 100;

            regs[0] = (second % 10) as u8;
            regs[1] = (second / 10) as u8;
            regs[2] = (minute % 10) as u8;
            regs[3] = (minute / 10) as u8;
            regs[4] = (hour % 10) as u8;
            regs[5] = (hour / 10) as u8;
            regs[6] = (day % 10) as u8;
            regs[7] = (day / 10) as u8;
            regs[8] = (month % 10) as u8;
            regs[9] = (month / 10) as u8;
            regs[10] = (year % 10) as u8;
            regs[11] = ((year / 10) % 10) as u8;
            regs[12] = (weekday % 7) as u8;
        }

        regs[16] = current_time as u8;
        regs[17] = (current_time >> 8) as u8;
        regs[18] = (current_time >> 16) as u8;
        regs[19] = (current_time >> 24) as u8;
    }

    /// Command-machine state (for the debug view).
    pub(super) fn state(&self) -> RtcState {
        self.state
    }

    /// Access mode (for the debug view).
    pub(super) fn mode(&self) -> RtcMode {
        self.mode
    }

    /// Current register index (for the debug view).
    pub(super) fn index(&self) -> u8 {
        self.index
    }

    /// Capture the volatile command machine for the savestate (the 20-byte
    /// data file persists separately via the chip's battery).
    pub(super) fn snapshot(&self) -> Spc7110RtcState {
        Spc7110RtcState {
            r4840: self.r4840,
            r4841: self.r4841,
            r4842: self.r4842,
            state: state_code(self.state),
            mode: mode_code(self.mode),
            index: self.index,
        }
    }

    /// Restore the command machine from a savestate.
    pub(super) fn restore_state(&mut self, s: &Spc7110RtcState) {
        self.r4840 = s.r4840;
        self.r4841 = s.r4841;
        self.r4842 = s.r4842;
        self.state = state_from_code(s.state);
        self.mode = mode_from_code(s.mode);
        self.index = s.index & 15;
    }
}

/// Encode [`RtcState`] as the small integer used by the debug view + savestate.
pub(super) fn state_code(state: RtcState) -> u8 {
    match state {
        RtcState::Inactive => 0,
        RtcState::ModeSelect => 1,
        RtcState::IndexSelect => 2,
        RtcState::Write => 3,
    }
}

/// Inverse of [`state_code`]; an unknown code decodes to `Inactive`.
fn state_from_code(code: u8) -> RtcState {
    match code {
        1 => RtcState::ModeSelect,
        2 => RtcState::IndexSelect,
        3 => RtcState::Write,
        _ => RtcState::Inactive,
    }
}

/// Encode [`RtcMode`] as its `$4841` wire value (`0x03` Linear / `0x0c` Indexed).
pub(super) fn mode_code(mode: RtcMode) -> u8 {
    match mode {
        RtcMode::Linear => 0x03,
        RtcMode::Indexed => 0x0c,
    }
}

/// Inverse of [`mode_code`]; only `0x0c` decodes to Indexed, everything else to
/// Linear (the power-on default).
fn mode_from_code(code: u8) -> RtcMode {
    if code == 0x0c {
        RtcMode::Indexed
    } else {
        RtcMode::Linear
    }
}

/// Decode a 2-nibble BCD field (`regs[i]` ones, `regs[i+1]` tens) into a
/// plain integer, bsnes `read(i) + read(i+1) * 10`.
fn bcd2(regs: &[u8], i: usize) -> u32 {
    u32::from(regs[i]) + u32::from(regs[i + 1]) * 10
}

/// A deterministic, advanceable clock for tests (atomic so it satisfies the
/// `Send + Sync` bound on [`RtcClock`]).
#[cfg(test)]
use std::sync::atomic::{AtomicI64, Ordering};

#[cfg(test)]
pub(super) struct FixedClock {
    now: AtomicI64,
}

#[cfg(test)]
impl FixedClock {
    pub(super) fn new(now: i64) -> Self {
        Self {
            now: AtomicI64::new(now),
        }
    }

    pub(super) fn advance(&self, secs: i64) {
        self.now.fetch_add(secs, Ordering::SeqCst);
    }
}

#[cfg(test)]
impl RtcClock for FixedClock {
    fn now_unix(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pinned_time_replaces_the_host_clock_until_released() {
        let real = SystemClock.now_unix();
        assert!(real > 1_600_000_000, "the host clock reads the present");
        set_fixed_unix_time(Some(1_767_225_600));
        assert_eq!(fixed_unix_time(), Some(1_767_225_600));
        assert_eq!(SystemClock.now_unix(), 1_767_225_600);
        set_fixed_unix_time(None);
        assert_eq!(fixed_unix_time(), None);
        assert!(SystemClock.now_unix() >= real);
    }

    /// A fresh 20-byte file (bsnes fills `cartrtc` with `0xFF`).
    fn file() -> [u8; RTC_FILE_LEN] {
        [0xFF; RTC_FILE_LEN]
    }

    /// Drive the command machine to sequentially write the 13 calendar fields
    /// (Linear mode from index 0), then clear CR0/idx14/CR2 (indices 13-15) so
    /// the timer is *enabled*: a fresh file is `0xFF`-filled, and a non-zero
    /// CR0 bit0 / CR2 bits0-1 would gate every `update_time` advance off.
    fn program_time(rtc: &mut Spc7110Rtc, regs: &mut [u8], clock: &dyn RtcClock, fields: [u8; 13]) {
        rtc.write_4840(regs, 0x01, clock); // enable -> ModeSelect
        rtc.write_4841(regs, 0x03, clock); // Linear -> IndexSelect
        rtc.write_4841(regs, 0x00, clock); // index 0 -> Write
        for nibble in fields {
            rtc.write_4841(regs, nibble, clock);
        }
        // The loop left us at index 13 (Write auto-increment); zero the two
        // control registers (CR0 @13, CR2 @15) so the timer runs.
        rtc.write_4841(regs, 0x00, clock); // idx 13 (CR0) = 0
        rtc.write_4841(regs, 0x00, clock); // idx 14 = 0
        rtc.write_4841(regs, 0x00, clock); // idx 15 (CR2) = 0
    }

    #[test]
    fn state_machine_advances_mode_index_write() {
        let mut rtc = Spc7110Rtc::default();
        let mut regs = file();
        let clock = FixedClock::new(1_000_000);

        assert_eq!(rtc.state(), RtcState::Inactive);
        rtc.write_4840(&mut regs, 0x01, &clock);
        assert_eq!(rtc.state(), RtcState::ModeSelect);
        assert_eq!(rtc.status() & 0x80, 0x80, "enable sets ready");

        rtc.write_4841(&mut regs, 0x03, &clock); // Linear
        assert_eq!(rtc.state(), RtcState::IndexSelect);
        assert_eq!(rtc.mode(), RtcMode::Linear);

        rtc.write_4841(&mut regs, 0x05, &clock); // index 5
        assert_eq!(rtc.state(), RtcState::Write);
        assert_eq!(rtc.index(), 5);

        rtc.write_4841(&mut regs, 0x07, &clock); // store nibble at 5
        assert_eq!(regs[5], 0x07);
        assert_eq!(rtc.index(), 6, "index auto-increments after a write");
    }

    #[test]
    fn indexed_mode_stays_in_index_select_and_reads_autoincrement() {
        let mut rtc = Spc7110Rtc::default();
        let mut regs = file();
        regs[3] = 0x0A;
        regs[4] = 0x0B;
        let clock = FixedClock::new(1_000_000);

        rtc.write_4840(&mut regs, 0x01, &clock);
        rtc.write_4841(&mut regs, 0x0c, &clock); // Indexed
        assert_eq!(rtc.mode(), RtcMode::Indexed);
        rtc.write_4841(&mut regs, 0x03, &clock); // select index 3
        assert_eq!(rtc.state(), RtcState::IndexSelect, "indexed stays put");
        assert_eq!(rtc.index(), 3);

        // Reads auto-increment the index.
        assert_eq!(rtc.read_4841(&regs), 0x0A);
        assert_eq!(rtc.read_4841(&regs), 0x0B);
        assert_eq!(rtc.index(), 5);
    }

    #[test]
    fn port_reads_are_inert_while_inactive() {
        let mut rtc = Spc7110Rtc::default();
        let regs = file();
        assert_eq!(rtc.read_4841(&regs), 0x00);
        // Status port read-to-clear.
        rtc.r4842 = 0x80;
        assert_eq!(rtc.read_4842(), 0x80);
        assert_eq!(rtc.read_4842(), 0x00, "bit7 cleared on read");
    }

    #[test]
    fn update_time_advances_calendar_by_elapsed_seconds() {
        let mut rtc = Spc7110Rtc::default();
        let mut regs = file();
        let clock = FixedClock::new(1_000_000);

        // Program 2000-01-01 00:00:00, Saturday (weekday 6), CR0/CR2 = 0.
        // fields: sec.lo hi, min.lo hi, hr.lo hi, day.lo hi, mon.lo hi,
        //         yr.lo hi, weekday.
        program_time(
            &mut rtc,
            &mut regs,
            &clock,
            [0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 6],
        );
        // Disable -> syncs the timestamp to "now" (1_000_000) without advancing.
        rtc.write_4840(&mut regs, 0x00, &clock);
        assert_eq!(regs[0], 0, "seconds still zero right after sync");

        // 65 seconds later, re-enable and read: the calendar rolled to 00:01:05.
        clock.advance(65);
        rtc.write_4840(&mut regs, 0x01, &clock);
        // A $4841 read isn't needed, the advance happens inside update_time on
        // the disable sync; re-sync via a disable to fold in the 65s.
        rtc.write_4840(&mut regs, 0x00, &clock);
        assert_eq!(regs[0], 5, "seconds ones");
        assert_eq!(regs[1], 0, "seconds tens");
        assert_eq!(regs[2], 1, "minutes ones");
        assert_eq!(regs[3], 0, "minutes tens");
        assert_eq!(regs[6], 1, "day unchanged");
        assert_eq!(regs[8], 1, "month unchanged");
    }

    #[test]
    fn update_time_rolls_over_a_full_day() {
        let mut rtc = Spc7110Rtc::default();
        let mut regs = file();
        let clock = FixedClock::new(2_000_000);

        // 2000-01-01 23:59:59, weekday 6.
        program_time(
            &mut rtc,
            &mut regs,
            &clock,
            [9, 5, 9, 5, 3, 2, 1, 0, 1, 0, 0, 0, 6],
        );
        rtc.write_4840(&mut regs, 0x00, &clock); // sync
        clock.advance(2); // +2s -> 2000-01-02 00:00:01
        rtc.write_4840(&mut regs, 0x00, &clock);

        assert_eq!(regs[0], 1, "seconds -> 1");
        assert_eq!(regs[2], 0, "minutes -> 0");
        assert_eq!(regs[4], 0, "hours -> 0");
        assert_eq!(regs[6], 2, "day -> 2");
        assert_eq!(regs[12], 0, "weekday wrapped 6 -> 0");
    }

    #[test]
    fn disabled_timer_does_not_advance() {
        let mut rtc = Spc7110Rtc::default();
        let mut regs = file();
        let clock = FixedClock::new(3_000_000);

        // CR0 (index 13) bit0 set = timer disabled.
        program_time(
            &mut rtc,
            &mut regs,
            &clock,
            [0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0],
        );
        // Set CR0 bit0 by writing index 13 = 1.
        rtc.write_4840(&mut regs, 0x01, &clock);
        rtc.write_4841(&mut regs, 0x03, &clock); // Linear
        rtc.write_4841(&mut regs, 13, &clock); // index 13
        rtc.write_4841(&mut regs, 0x01, &clock); // CR0 = 1 (disable)
        rtc.write_4840(&mut regs, 0x00, &clock); // sync

        clock.advance(120);
        rtc.write_4840(&mut regs, 0x00, &clock); // sync again
        assert_eq!(regs[0], 0, "seconds must not advance while disabled");
        assert_eq!(regs[2], 0, "minutes must not advance while disabled");
    }

    #[test]
    fn first_sync_with_ff_timestamp_does_not_bogus_advance() {
        // A fresh (0xFF-filled) file has a "future" timestamp; the first sync
        // must clamp the elapsed time to zero, not spew a huge advance.
        let rtc = Spc7110Rtc::default();
        let mut regs = file();
        // Valid BCD calendar + zeroed CR0/CR2 (indices 13/15) so the timer is
        // enabled, this way it is the elapsed-time clamp, not the disable
        // gate, that prevents the bogus advance.
        regs[0..16].copy_from_slice(&[0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0]);
        let clock = FixedClock::new(1_700_000_000); // ~2023

        rtc.update_time(&mut regs, &clock, 0);
        assert_eq!(regs[0], 0, "no bogus second advance on first sync");
        // Timestamp now reflects the clock.
        let stamped = i64::from(regs[16])
            | (i64::from(regs[17]) << 8)
            | (i64::from(regs[18]) << 16)
            | (i64::from(regs[19]) << 24);
        assert_eq!(stamped, 1_700_000_000 & 0xffff_ffff);
    }
}
