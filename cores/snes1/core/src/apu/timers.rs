//! The three SPC700 hardware timers (`$FA-$FC` targets, `$FD-$FF`
//! outputs).
//!
//! Each timer is a two-stage divider. A *prescaler* divides the raw
//! SPC700 clock (~1.024 MHz): timers 0 and 1 divide by 128 (an 8 kHz
//! tick), timer 2 divides by 16 (a 64 kHz tick). Every prescaler tick,
//! while the timer is enabled, increments an internal stage
//! counter; when that counter reaches the programmable target the
//! 4-bit visible output increments (wrapping at 16) and the stage
//! resets. A target of 0 means 256.
//!
//! Two hardware quirks the driver code relies on:
//!
//! - Reading the output register (`$FD-$FF`) clears it to 0. Drivers
//!   poll it to pace their mixing, so a non-clearing read would make
//!   them busy-loop forever.
//! - Toggling a timer from disabled to enabled (CONTROL bit 0/1/2
//!   going 0->1) resets both the internal stage and the visible
//!   output, so the first interval after an enable is full-length.

/// Prescaler divisor for the 8 kHz timers (0 and 1).
pub const DIVISOR_8KHZ: u32 = 128;

/// Prescaler divisor for the 64 kHz timer (2).
pub const DIVISOR_64KHZ: u32 = 16;

/// One SPC700 hardware timer.
#[derive(Debug, Clone)]
pub struct ApuTimer {
    enabled: bool,
    /// `$FA`/`$FB`/`$FC` target. 0 is treated as 256.
    target: u8,
    /// SPC-clock prescaler accumulator (counts up to `divisor`).
    prescaler: u32,
    /// Prescaler divisor: 128 for the 8 kHz timers, 16 for the 64 kHz
    /// timer.
    divisor: u32,
    /// Internal stage counter (counts prescaler ticks up to `target`).
    stage: u16,
    /// 4-bit visible output (`$FD`/`$FE`/`$FF`); read-clears.
    output: u8,
}

impl ApuTimer {
    /// Build a timer with the given prescaler divisor
    /// ([`DIVISOR_8KHZ`] or [`DIVISOR_64KHZ`]), disabled, target 0.
    pub fn new(divisor: u32) -> Self {
        Self {
            enabled: false,
            target: 0,
            prescaler: 0,
            divisor,
            stage: 0,
            output: 0,
        }
    }

    /// Advance the timer by `spc_cycles` SPC700 clock cycles. The
    /// prescaler runs whether or not the timer is enabled, matching
    /// the hardware; only the stage/output advance when enabled.
    pub fn tick(&mut self, spc_cycles: u32) {
        self.prescaler += spc_cycles;
        while self.prescaler >= self.divisor {
            self.prescaler -= self.divisor;
            if !self.enabled {
                continue;
            }
            self.stage += 1;
            let target = if self.target == 0 {
                256
            } else {
                self.target as u16
            };
            if self.stage >= target {
                self.stage = 0;
                self.output = self.output.wrapping_add(1) & 0x0F;
            }
        }
    }

    /// Set the enable bit. A 0->1 transition resets the internal stage
    /// and the visible output so the first post-enable interval is
    /// full length (hardware behaviour the timer drivers depend on).
    pub fn set_enabled(&mut self, on: bool) {
        if on && !self.enabled {
            self.stage = 0;
            self.output = 0;
        }
        self.enabled = on;
    }

    /// Whether the timer is currently enabled.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Set the target register (`$FA`/`$FB`/`$FC`).
    pub fn set_target(&mut self, target: u8) {
        self.target = target;
    }

    /// Read the 4-bit output register, clearing it as a side effect
    /// (`$FD`/`$FE`/`$FF` read-clears on hardware).
    pub fn read_output(&mut self) -> u8 {
        let value = self.output;
        self.output = 0;
        value
    }

    /// Peek the output without clearing it (for the debugger / tests).
    pub fn peek_output(&self) -> u8 {
        self.output
    }

    /// Capture the timer (enable, target, prescaler/stage counters,
    /// divisor, visible output) into a snapshot.
    pub fn to_savestate(&self) -> crate::savestate::ApuTimerState {
        crate::savestate::ApuTimerState {
            enabled: self.enabled,
            target: self.target,
            prescaler: self.prescaler,
            divisor: self.divisor,
            stage: self.stage,
            output: self.output,
        }
    }

    /// Restore the timer from a snapshot. The divisor is restored too,
    /// so the timer keeps its 8 kHz / 64 kHz prescaler identity.
    // Mirrors `to_savestate`: restores into `&mut self` rather than
    // constructing, so it intentionally takes a receiver.
    #[allow(clippy::wrong_self_convention)]
    pub fn from_savestate(&mut self, s: &crate::savestate::ApuTimerState) {
        self.enabled = s.enabled;
        self.target = s.target;
        self.prescaler = s.prescaler;
        self.divisor = s.divisor;
        self.stage = s.stage;
        self.output = s.output;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_timer_does_not_count() {
        let mut t = ApuTimer::new(DIVISOR_8KHZ);
        t.set_target(1);
        t.tick(DIVISOR_8KHZ * 10);
        assert_eq!(t.peek_output(), 0);
    }

    #[test]
    fn target_one_increments_every_divisor_ticks_at_8khz() {
        let mut t = ApuTimer::new(DIVISOR_8KHZ);
        t.set_target(1);
        t.set_enabled(true);
        // One prescaler tick (128 SPC cycles) -> stage hits target 1 ->
        // output increments.
        t.tick(DIVISOR_8KHZ);
        assert_eq!(t.peek_output(), 1);
        t.tick(DIVISOR_8KHZ * 4);
        assert_eq!(t.peek_output(), 5);
    }

    #[test]
    fn timer2_uses_the_64khz_divisor() {
        let mut t = ApuTimer::new(DIVISOR_64KHZ);
        t.set_target(1);
        t.set_enabled(true);
        t.tick(DIVISOR_64KHZ * 3);
        assert_eq!(t.peek_output(), 3);
    }

    #[test]
    fn target_governs_the_stage_length() {
        let mut t = ApuTimer::new(DIVISOR_8KHZ);
        t.set_target(4);
        t.set_enabled(true);
        // 3 prescaler ticks: stage 1,2,3, not yet at target 4.
        t.tick(DIVISOR_8KHZ * 3);
        assert_eq!(t.peek_output(), 0);
        // 4th prescaler tick reaches the target -> output 1.
        t.tick(DIVISOR_8KHZ);
        assert_eq!(t.peek_output(), 1);
    }

    #[test]
    fn target_zero_means_256() {
        let mut t = ApuTimer::new(DIVISOR_64KHZ);
        t.set_target(0);
        t.set_enabled(true);
        // 255 prescaler ticks: still short of 256.
        t.tick(DIVISOR_64KHZ * 255);
        assert_eq!(t.peek_output(), 0);
        t.tick(DIVISOR_64KHZ);
        assert_eq!(t.peek_output(), 1);
    }

    #[test]
    fn read_output_clears_it() {
        let mut t = ApuTimer::new(DIVISOR_8KHZ);
        t.set_target(1);
        t.set_enabled(true);
        t.tick(DIVISOR_8KHZ * 3);
        assert_eq!(t.read_output(), 3);
        assert_eq!(t.read_output(), 0);
    }

    #[test]
    fn output_wraps_at_16() {
        let mut t = ApuTimer::new(DIVISOR_8KHZ);
        t.set_target(1);
        t.set_enabled(true);
        // 20 prescaler ticks -> output = 20 & 0x0F = 4.
        t.tick(DIVISOR_8KHZ * 20);
        assert_eq!(t.peek_output(), 4);
    }

    #[test]
    fn enabling_resets_stage_and_output() {
        let mut t = ApuTimer::new(DIVISOR_8KHZ);
        t.set_target(4);
        t.set_enabled(true);
        t.tick(DIVISOR_8KHZ * 3); // stage = 3
        t.set_enabled(false);
        t.set_enabled(true); // 0->1: stage + output reset
        t.tick(DIVISOR_8KHZ * 3); // stage = 3 again, still short of 4
        assert_eq!(t.peek_output(), 0);
        t.tick(DIVISOR_8KHZ);
        assert_eq!(t.peek_output(), 1);
    }

    #[test]
    fn prescaler_phase_is_preserved_across_ticks() {
        let mut t = ApuTimer::new(DIVISOR_8KHZ);
        t.set_target(1);
        t.set_enabled(true);
        // Two sub-divisor ticks that sum to exactly one divisor.
        t.tick(100);
        assert_eq!(t.peek_output(), 0);
        t.tick(28);
        assert_eq!(t.peek_output(), 1);
    }
}
