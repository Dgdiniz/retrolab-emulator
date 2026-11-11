//! YM2612 timers A and B.
//!
//! Two counters that raise status flags; a sound driver polls the flags to
//! keep time. Both count the chip's internal samples (one every 144 input
//! clocks, about 53.3 kHz):
//!
//! - **Timer A** has a ten-bit period register TA (`$24` holds its upper
//!   eight bits, `$25` the lower two) and overflows every `1024 - TA`
//!   samples.
//! - **Timer B** has an eight-bit period register TB (`$26`) behind a
//!   divide-by-sixteen, so it overflows every `(256 - TB) * 16` samples.
//!
//! `$27` controls both: bits 0 and 1 run the counters (a counter reloads
//! when its bit goes from 0 to 1), bits 2 and 3 let an overflow set the
//! status flag, bits 4 and 5 clear the flags, and bits 6 and 7 are the
//! channel-3 mode. The status register's bit 0 is timer A's flag and bit 1
//! timer B's.
//!
//! Written from the chip's documented behaviour (Yamaha's application
//! manual, ymfm); checked against Genesis Plus GX.

/// `$27` bits: the counters run.
const RUN_A: u8 = 0x01;
const RUN_B: u8 = 0x02;
/// `$27` bits: an overflow sets the status flag.
const FLAG_ON_OVERFLOW_A: u8 = 0x04;
const FLAG_ON_OVERFLOW_B: u8 = 0x08;
/// The status register's two flags.
const STATUS_A: u8 = 0x01;
const STATUS_B: u8 = 0x02;

/// Samples in timer A's longest period (TA = 0).
const TIMER_A_SPAN: i32 = 1024;
/// Timer B's longest count (TB = 0), and the samples one count lasts.
const TIMER_B_SPAN: i32 = 256;
const TIMER_B_SAMPLES_PER_COUNT: i32 = 16;

/// The two timers, their control byte and the status flags.
///
/// The timers are clocked per internal sample (by `Ym2612::clock_timers`,
/// every 144 input clocks), not per input clock.
#[derive(Debug, Clone)]
pub struct Timers {
    // --- Timer A state ---
    /// Timer A period register (10-bit: 0-1023)
    ta_period: u16,
    /// Samples in one timer A period: `1024 - ta_period`
    ta_load: i32,
    /// Samples left in the current timer A period (counts down from `ta_load`)
    ta_counter: i32,

    // --- Timer B state ---
    /// Timer B period register (8-bit: 0-255)
    tb_period: u8,
    /// Samples in one timer B period: `(256 - tb_period) * 16`
    tb_load: i32,
    /// Samples left in the current timer B period (counts down from `tb_load`)
    tb_counter: i32,

    // --- Control state (register 0x27) ---
    /// The last byte written to register 0x27.
    /// Bits 0-1: run, bits 2-3: flag on overflow, bits 4-5: clear, bits 6-7: CH3 mode
    mode: u8,

    /// Status register (bits 0-1: Timer A/B overflow flags)
    status: u8,

    /// Tracks whether timer A overflowed since last check (for CSM mode).
    timer_a_overflowed_since_check: bool,
}

impl Timers {
    pub fn new() -> Self {
        Self {
            ta_period: 0,
            ta_load: TIMER_A_SPAN,
            ta_counter: TIMER_A_SPAN,
            tb_period: 0,
            tb_load: TIMER_B_SPAN * TIMER_B_SAMPLES_PER_COUNT,
            tb_counter: TIMER_B_SPAN * TIMER_B_SAMPLES_PER_COUNT,
            mode: 0,
            status: 0,
            timer_a_overflowed_since_check: false,
        }
    }

    // ========================
    // Register writes
    // ========================

    /// Register 0x24: the upper eight bits of timer A's period.
    pub fn set_timer_a_high(&mut self, value: u8) {
        self.set_timer_a_period((u16::from(value) << 2) | (self.ta_period & 0x003));
    }

    /// Register 0x25: the lower two bits of timer A's period.
    pub fn set_timer_a_low(&mut self, value: u8) {
        self.set_timer_a_period((self.ta_period & 0x3FC) | u16::from(value & 0x03));
    }

    fn set_timer_a_period(&mut self, period: u16) {
        self.ta_period = period;
        self.ta_load = TIMER_A_SPAN - i32::from(period);
    }

    /// Register 0x26: timer B's period.
    pub fn set_timer_b_period(&mut self, value: u8) {
        self.tb_period = value;
        self.tb_load = (TIMER_B_SPAN - i32::from(value)) * TIMER_B_SAMPLES_PER_COUNT;
    }

    /// Register 0x27: run and flag bits, flag clears, and the CH3 mode.
    pub fn set_control(&mut self, value: u8) {
        // A counter starts a fresh period when its run bit goes from 0 to 1.
        let started = value & !self.mode;
        if started & RUN_A != 0 {
            self.ta_counter = self.ta_load;
        }
        if started & RUN_B != 0 {
            self.tb_counter = self.tb_load;
        }
        // Bits 4 and 5 clear timer A's and timer B's flag.
        self.status &= !((value >> 4) & (STATUS_A | STATUS_B));
        self.mode = value;
    }

    // ========================
    // Timer clocking (per internal sample)
    // ========================

    /// Clock Timer A for one internal sample.
    /// Called by `Ym2612::clock_timers` once per internal sample (144 input clocks).
    /// Returns true if Timer A overflowed this sample (for CSM mode).
    pub fn clock_timer_a(&mut self) -> bool {
        if self.mode & RUN_A == 0 {
            return false;
        }
        self.ta_counter -= 1;
        if self.ta_counter > 0 {
            return false;
        }
        // The period ran out: start the next one and report it.
        self.ta_counter = self.ta_load;
        if self.mode & FLAG_ON_OVERFLOW_A != 0 {
            self.status |= STATUS_A;
        }
        self.timer_a_overflowed_since_check = true;
        true
    }

    /// Clock Timer B for `samples` internal samples.
    /// Called once per sample batch in the YM2612 clock cycle.
    pub fn clock_timer_b(&mut self, samples: i32) {
        if self.mode & RUN_B == 0 {
            return;
        }
        self.tb_counter -= samples;
        if self.tb_counter > 0 {
            return;
        }
        if self.mode & FLAG_ON_OVERFLOW_B != 0 {
            self.status |= STATUS_B;
        }
        // A batch can run past more than one period: move on by whole
        // periods until samples are left in the current one.
        let period = self.tb_load.max(1);
        self.tb_counter += period * (1 + (-self.tb_counter) / period);
    }

    // ========================
    // Status and mode queries
    // ========================

    /// Get status register value.
    /// Bit 0: Timer A overflow, Bit 1: Timer B overflow.
    pub fn get_status(&self) -> u8 {
        self.status
    }

    /// Get CH3 mode (bits 7-6 of control register)
    pub fn get_ch3_mode(&self) -> u8 {
        self.mode & 0xC0
    }

    /// Check if CSM mode is active (bit 7 of mode)
    pub fn is_csm_mode(&self) -> bool {
        (self.mode & 0x80) != 0
    }

    /// Check and clear timer A overflow (for CSM mode checking).
    /// Returns true if timer A has overflowed since last check.
    pub fn take_timer_a_overflow(&mut self) -> bool {
        let overflowed = self.timer_a_overflowed_since_check;
        self.timer_a_overflowed_since_check = false;
        overflowed
    }

    // ========================
    // Batch clock (a no-op), display queries, reset
    // ========================

    /// Batch-clock the timers: a no-op, since they are clocked per internal
    /// sample (see `clock_timers` in `mod.rs`), not per input clock.
    pub fn clock_cycles(&mut self, _cycles: u32) {
        // The timers are clocked in `clock_timers` via clock_timer_a/b.
        // This method is kept for API compatibility but does nothing.
    }

    /// Get Timer A period register value (for audio-info display)
    pub fn get_timer_a_period(&self) -> u16 {
        self.ta_period
    }

    /// Get Timer B period register value (for audio-info display)
    pub fn get_timer_b_period(&self) -> u8 {
        self.tb_period
    }

    /// Check if Timer A is running (bit 0 of mode)
    pub fn is_timer_a_running(&self) -> bool {
        self.mode & RUN_A != 0
    }

    /// Check if Timer B is running (bit 1 of mode)
    pub fn is_timer_b_running(&self) -> bool {
        self.mode & RUN_B != 0
    }

    /// Reset all timers
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

impl Default for Timers {
    fn default() -> Self {
        Self::new()
    }
}

impl Timers {
    pub fn to_savestate(&self) -> crate::system::savestate::Ym2612TimerState {
        crate::system::savestate::Ym2612TimerState {
            ta_period: self.ta_period,
            ta_load: self.ta_load,
            ta_counter: self.ta_counter,
            tb_period: self.tb_period,
            tb_load: self.tb_load,
            tb_counter: self.tb_counter,
            mode: self.mode,
            status: self.status,
            timer_a_overflowed_since_check: self.timer_a_overflowed_since_check,
        }
    }

    pub fn from_savestate(&mut self, s: &crate::system::savestate::Ym2612TimerState) {
        self.ta_period = s.ta_period;
        self.ta_load = s.ta_load;
        self.ta_counter = s.ta_counter;
        self.tb_period = s.tb_period;
        self.tb_load = s.tb_load;
        self.tb_counter = s.tb_counter;
        self.mode = s.mode;
        self.status = s.status;
        self.timer_a_overflowed_since_check = s.timer_a_overflowed_since_check;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timers_creation() {
        let timers = Timers::new();
        assert_eq!(timers.get_timer_a_period(), 0);
        assert_eq!(timers.get_timer_b_period(), 0);
        assert_eq!(timers.get_ch3_mode(), 0);
        assert_eq!(timers.get_status(), 0);
    }

    #[test]
    fn test_timer_a_period_registers() {
        let mut timers = Timers::new();

        // Set Timer A to 0x2AB (high=0xAA, low=0x03)
        // high bits: value << 2 = 0xAA << 2 = 0x2A8
        // low bits: value & 0x03 = 0x03
        // TA = 0x2A8 | 0x03 = 0x2AB
        timers.set_timer_a_high(0xAA);
        timers.set_timer_a_low(0x03);
        assert_eq!(timers.get_timer_a_period(), 0x2AB);
    }

    #[test]
    fn test_timer_a_overflow() {
        let mut timers = Timers::new();

        // Timer A with period 1023 -> load = 1024 - 1023 = 1
        // Should overflow after 1 sample
        timers.set_timer_a_high(0xFF); // high = 0xFF << 2 = 0x3FC
        timers.set_timer_a_low(0x03); // low = 0x03 -> TA = 0x3FF = 1023
        assert_eq!(timers.get_timer_a_period(), 1023);

        // Enable Timer A run (bit 0) + status enable (bit 2)
        timers.set_control(0x05);

        // Clock one sample -> should overflow
        let overflowed = timers.clock_timer_a();
        assert!(
            overflowed,
            "Timer A should overflow after 1 sample with period 1023"
        );
        assert_eq!(
            timers.get_status() & 0x01,
            0x01,
            "Status bit 0 should be set"
        );
    }

    #[test]
    fn test_timer_a_period_0() {
        let mut timers = Timers::new();

        // Timer A with period 0 -> load = 1024
        // Should overflow after 1024 samples
        // Enable Timer A run (bit 0) + status enable (bit 2)
        timers.set_control(0x05);

        let mut overflowed = false;
        for i in 0..1025 {
            if timers.clock_timer_a() {
                assert_eq!(i, 1023, "Timer A should overflow at sample 1024");
                overflowed = true;
                break;
            }
        }
        assert!(overflowed, "Timer A should have overflowed");
        assert_eq!(timers.get_status() & 0x01, 0x01);
    }

    #[test]
    fn test_timer_b_overflow() {
        let mut timers = Timers::new();

        // Timer B with period 255 -> load = (256-255) << 4 = 16
        // Should overflow after 16 sample-steps
        timers.set_timer_b_period(255);

        // Enable Timer B run (bit 1) + status enable (bit 3)
        timers.set_control(0x0A);

        // Clock 15 samples -> no overflow yet
        timers.clock_timer_b(15);
        assert_eq!(timers.get_status() & 0x02, 0x00);

        // Clock 1 more -> overflow
        timers.clock_timer_b(1);
        assert_eq!(
            timers.get_status() & 0x02,
            0x02,
            "Status bit 1 should be set"
        );
    }

    #[test]
    fn test_timer_b_period_0() {
        let mut timers = Timers::new();

        // Timer B with period 0 -> load = 256 << 4 = 4096
        timers.set_timer_b_period(0);

        // Enable Timer B run + status enable
        timers.set_control(0x0A);

        // Clock 4095 samples -> no overflow yet
        timers.clock_timer_b(4095);
        assert_eq!(timers.get_status() & 0x02, 0x00);

        // Clock 1 more -> overflow
        timers.clock_timer_b(1);
        assert_eq!(timers.get_status() & 0x02, 0x02);
    }

    #[test]
    fn test_control_register_bits() {
        let mut timers = Timers::new();

        // Set period first
        timers.set_timer_a_high(0xFF);
        timers.set_timer_a_low(0x03); // TA = 1023, load = 1

        // Only run (bit 0), no status enable (bit 2 clear)
        timers.set_control(0x01);
        timers.clock_timer_a(); // should overflow
        assert_eq!(
            timers.get_status() & 0x01,
            0x00,
            "Status should NOT be set without enable"
        );

        // Now enable status (bit 2)
        timers.set_control(0x05);
        timers.clock_timer_a(); // should overflow and set status
        assert_eq!(
            timers.get_status() & 0x01,
            0x01,
            "Status SHOULD be set with enable"
        );
    }

    #[test]
    fn test_status_reset() {
        let mut timers = Timers::new();

        // Force both status flags
        timers.set_timer_a_high(0xFF);
        timers.set_timer_a_low(0x03);
        timers.set_timer_b_period(255);
        timers.set_control(0x0F); // run + enable both
        timers.clock_timer_a();
        timers.clock_timer_b(16);
        assert_eq!(timers.get_status(), 0x03, "Both flags should be set");

        // Reset Timer A flag (bit 4)
        timers.set_control(0x1F); // 0x10 | 0x0F
        assert_eq!(
            timers.get_status() & 0x01,
            0x00,
            "Timer A flag should be cleared"
        );
        assert_eq!(
            timers.get_status() & 0x02,
            0x02,
            "Timer B flag should remain"
        );

        // Reset Timer B flag (bit 5)
        timers.set_control(0x2F); // 0x20 | 0x0F
        assert_eq!(
            timers.get_status() & 0x02,
            0x00,
            "Timer B flag should be cleared"
        );
    }

    #[test]
    fn test_timer_reload_on_rising_edge() {
        let mut timers = Timers::new();

        timers.set_timer_a_high(0xFF);
        timers.set_timer_a_low(0x03); // TA = 1023, load = 1

        // Start with run + enable
        timers.set_control(0x05);
        timers.clock_timer_a(); // overflow, counter reloaded to 1

        // Clear and re-set run bit -> rising edge should reload
        timers.set_control(0x04); // clear bit 0
        timers.set_control(0x05); // set bit 0 -> rising edge -> reload

        // Counter should be freshly reloaded
        let overflowed = timers.clock_timer_a();
        assert!(
            overflowed,
            "Timer A should overflow immediately after reload"
        );
    }

    #[test]
    fn test_ch3_mode() {
        let mut timers = Timers::new();

        timers.set_control(0x00);
        assert_eq!(timers.get_ch3_mode(), 0x00);
        assert!(!timers.is_csm_mode());

        timers.set_control(0x80);
        assert_eq!(timers.get_ch3_mode(), 0x80);
        assert!(timers.is_csm_mode());

        timers.set_control(0x40);
        assert_eq!(timers.get_ch3_mode(), 0x40);
        assert!(!timers.is_csm_mode());
    }

    #[test]
    fn test_csm_overflow_tracking() {
        let mut timers = Timers::new();
        timers.set_timer_a_high(0xFF);
        timers.set_timer_a_low(0x03);
        timers.set_control(0x81); // CSM mode + Timer A run

        timers.clock_timer_a();
        assert!(timers.take_timer_a_overflow());
        assert!(!timers.take_timer_a_overflow()); // cleared after take
    }

    #[test]
    fn test_timer_not_running_doesnt_count() {
        let mut timers = Timers::new();
        timers.set_timer_a_high(0xFF);
        timers.set_timer_a_low(0x03); // load = 1

        // Don't set run bit (bit 0)
        timers.set_control(0x04); // only status enable, no run

        for _ in 0..100 {
            timers.clock_timer_a();
        }
        assert_eq!(
            timers.get_status(),
            0x00,
            "Timer should not count without run bit"
        );
    }

    #[test]
    fn test_reset() {
        let mut timers = Timers::new();
        timers.set_timer_a_high(0xFF);
        timers.set_timer_b_period(128);
        timers.set_control(0x8F);

        timers.reset();

        assert_eq!(timers.get_timer_a_period(), 0);
        assert_eq!(timers.get_timer_b_period(), 0);
        assert_eq!(timers.get_ch3_mode(), 0);
        assert_eq!(timers.get_status(), 0);
    }

    #[test]
    fn timer_b_moves_on_by_whole_periods_as_it_would_one_period_at_a_time() {
        for period in [0u8, 1, 128, 254, 255] {
            for samples in [1, 7, 16, 100, 4096, 10_000] {
                let mut timers = Timers::new();
                timers.set_timer_b_period(period);
                timers.set_control(RUN_B);
                let mut expected = timers.tb_counter;
                for _ in 0..200 {
                    timers.clock_timer_b(samples);
                    expected -= samples;
                    while expected <= 0 {
                        expected += timers.tb_load;
                    }
                    assert_eq!(timers.tb_counter, expected, "TB={period} batch={samples}");
                }
            }
        }
    }

    #[test]
    fn the_control_byte_reloads_on_a_start_and_clears_only_the_flags_it_names() {
        let mut timers = Timers::new();
        timers.set_timer_a_high(0xFF);
        timers.set_timer_a_low(0x03); // TA = 1023: one sample a period
        timers.set_timer_b_period(0xFF); // TB = 255: sixteen samples a period
        timers.set_control(RUN_A | RUN_B | FLAG_ON_OVERFLOW_A | FLAG_ON_OVERFLOW_B);
        assert!(timers.clock_timer_a());
        timers.clock_timer_b(16);
        assert_eq!(timers.get_status(), STATUS_A | STATUS_B);
        // Bit 4 clears timer A's flag and leaves timer B's; the CH3 mode
        // bits (6-7) clear nothing.
        timers.set_control(0xC0 | 0x10 | RUN_A | RUN_B);
        assert_eq!(timers.get_status(), STATUS_B);
        timers.set_control(0x20 | RUN_A | RUN_B);
        assert_eq!(timers.get_status(), 0);
        // Still running: no reload. Stopped and started again: a fresh period.
        timers.set_timer_b_period(0xF0);
        timers.clock_timer_b(3);
        let mid_period = timers.tb_counter;
        timers.set_control(RUN_A | RUN_B);
        assert_eq!(timers.tb_counter, mid_period);
        timers.set_control(RUN_A);
        timers.set_control(RUN_A | RUN_B);
        assert_eq!(timers.tb_counter, 16 * 16);
    }
}
