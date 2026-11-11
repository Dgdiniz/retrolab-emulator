//! VDP timing and synchronization.
//!
//! The Genesis VDP operates on a scanline-based rendering model,
//! with different timing for NTSC and PAL systems.

/// Video region (NTSC or PAL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoRegion {
    /// NTSC (North America, Japan): 60 Hz
    Ntsc,
    /// PAL (Europe): 50 Hz
    Pal,
}

/// Timing constants for NTSC region.
pub mod ntsc {
    pub const SCANLINES_VISIBLE: u16 = 224; // V28 mode
    pub const SCANLINES_VBLANK: u16 = 38;
    pub const SCANLINES_TOTAL: u16 = 262;

    /// 68000 cycles per scanline (approximately, for coarse timing)
    pub const CYCLES_PER_SCANLINE: u32 = 488;

    /// Master clocks per scanline (exact - no fractional error!)
    /// This is the precise value: 3420 master clocks = 1 scanline.
    /// Using this instead of CPU cycles (3420/7 = 488.571...) eliminates drift.
    pub const MASTER_CLOCKS_PER_SCANLINE: u32 = 3420;

    /// Active display cycles (not in HBLANK)
    pub const ACTIVE_CYCLES: u32 = 424;

    /// HBLANK cycles
    pub const HBLANK_CYCLES: u32 = 64;

    /// Master clock frequency (Hz)
    pub const MASTER_CLOCK: u64 = 53_693_175;

    /// 68000 clock frequency (Hz)
    pub const CPU_CLOCK: u64 = MASTER_CLOCK / 7; // ~7.67 MHz

    /// CPU clock divider from master clock
    pub const CPU_DIVIDER: u32 = 7;
}

/// Timing constants for PAL region.
pub mod pal {
    pub const SCANLINES_VISIBLE: u16 = 240; // V30 mode
    pub const SCANLINES_VBLANK: u16 = 73;
    pub const SCANLINES_TOTAL: u16 = 313;

    pub const CYCLES_PER_SCANLINE: u32 = 488;

    /// Master clocks per scanline (exact - no fractional error!)
    pub const MASTER_CLOCKS_PER_SCANLINE: u32 = 3420;

    pub const ACTIVE_CYCLES: u32 = 424;
    pub const HBLANK_CYCLES: u32 = 64;

    pub const MASTER_CLOCK: u64 = 53_203_424;
    pub const CPU_CLOCK: u64 = MASTER_CLOCK / 7;

    /// CPU clock divider from master clock
    pub const CPU_DIVIDER: u32 = 7;
}

/// VDP timing state.
#[derive(Debug, Clone)]
pub struct VdpTiming {
    /// Current video region
    pub region: VideoRegion,

    /// Current scanline (0-261 for NTSC, 0-312 for PAL)
    pub scanline: u16,

    /// Horizontal counter (0-487)
    pub h_counter: u16,

    /// Total cycles executed this frame
    pub frame_cycles: u32,

    /// Total frames rendered
    pub frame_count: u64,

    /// Whether we're in VBLANK
    pub in_vblank: bool,

    /// Whether we're in HBLANK
    pub in_hblank: bool,

    /// VBLANK interrupt occurred flag (F bit in status register).
    /// Set when entering VBlank, cleared by interrupt acknowledge (IACK)
    /// when the 68K takes the VINT. NOT cleared by status register reads.
    pub vblank_pending: bool,

    /// VINT interrupt request to CPU.
    /// Set when entering VBlank (with VINT enabled), cleared when the system
    /// delivers the interrupt request to the CPU. This prevents the same VINT
    /// from being re-requested every instruction cycle.
    pub vint_irq_pending: bool,

    /// HBLANK interrupt pending
    pub hblank_pending: bool,

    /// HBLANK counter (for HINT timing)
    pub h_counter_reload: u8,
    pub h_counter_current: u8,

    /// Countdown (in master clocks) that holds off the level-6 68K vertical
    /// interrupt request after entering the VBLANK line. Real hardware
    /// asserts VINT ~770 (H32) / ~788 (H40) master-clocks INTO line 224,
    /// not at the line's start (the offsets Genesis Plus GX uses). The F flag (`vblank_pending`) and the Z80 IRQ
    /// edge are still raised at the line's start; only the 68K IRQ request is
    /// gated by `vint_request_delayed()` until this countdown elapses. A
    /// negative value means "not delayed". Transient within-frame state (always
    /// resolves before frame end), so it is intentionally not part of the
    /// savestate.
    pub vint_request_delay_mclks: i32,

    /// Suppress the very first VBLANK interrupt after a VDP reset. The VDP
    /// comes out of reset already past VBLANK, so real hardware never raises
    /// the F flag or the
    /// level-6 request on the first post-reset frame, and a game receives its
    /// first VINT one frame later than the bare line-224 crossing would imply.
    /// Without this, SG1 would run exactly one logic frame ahead of Genesis Plus GX
    /// from boot (every game's first work-RAM write would land one frame early).
    /// Cleared the instant the first VBLANK is reached. Transient boot state, not
    /// part of the savestate (always false by the time a game is savestated).
    pub first_frame_after_reset: bool,
}

impl VdpTiming {
    /// Create new timing state for given region.
    pub fn new(region: VideoRegion) -> Self {
        // Genesis starts in VBLANK - games expect this
        // Start at first VBLANK scanline (224 for NTSC) to match MAME's initial state
        let initial_scanline = match region {
            VideoRegion::Ntsc => ntsc::SCANLINES_VISIBLE, // 224
            VideoRegion::Pal => pal::SCANLINES_VISIBLE,
        };
        Self {
            region,
            scanline: initial_scanline,
            h_counter: 0,
            frame_cycles: 0,
            frame_count: 0,
            in_vblank: true, // Games expect VBLANK at startup
            in_hblank: false,
            vblank_pending: false,
            vint_irq_pending: false,
            hblank_pending: false,
            h_counter_reload: 0,
            h_counter_current: 0,
            vint_request_delay_mclks: -1,
            first_frame_after_reset: true,
        }
    }

    /// Get total scanlines for current region.
    pub fn total_scanlines(&self) -> u16 {
        match self.region {
            VideoRegion::Ntsc => ntsc::SCANLINES_TOTAL,
            VideoRegion::Pal => pal::SCANLINES_TOTAL,
        }
    }

    /// Get visible scanlines for current region.
    pub fn visible_scanlines(&self) -> u16 {
        match self.region {
            VideoRegion::Ntsc => ntsc::SCANLINES_VISIBLE,
            VideoRegion::Pal => pal::SCANLINES_VISIBLE,
        }
    }

    /// Get cycles per scanline.
    pub fn cycles_per_scanline(&self) -> u32 {
        match self.region {
            VideoRegion::Ntsc => ntsc::CYCLES_PER_SCANLINE,
            VideoRegion::Pal => pal::CYCLES_PER_SCANLINE,
        }
    }

    /// Advance to next scanline.
    pub fn next_scanline(&mut self) {
        self.scanline += 1;
        if self.scanline >= self.total_scanlines() {
            self.scanline = 0;
            self.frame_count += 1;
            self.frame_cycles = 0;
        }

        // Update VBLANK status
        let visible = self.visible_scanlines();
        self.in_vblank = self.scanline >= visible;

        // Reset HBLANK counter for new scanline
        if self.h_counter_current == 0 {
            self.h_counter_current = self.h_counter_reload;
        } else {
            self.h_counter_current -= 1;
        }

        self.h_counter = 0;
    }

    /// Execute cycles on current scanline.
    /// Returns true if scanline completed.
    pub fn execute_cycles(&mut self, cycles: u32) -> bool {
        self.h_counter += cycles as u16;
        self.frame_cycles += cycles;

        // Check HBLANK transition
        let active_cycles = match self.region {
            VideoRegion::Ntsc => ntsc::ACTIVE_CYCLES,
            VideoRegion::Pal => pal::ACTIVE_CYCLES,
        };

        self.in_hblank = self.h_counter >= active_cycles as u16;

        // Check if scanline is complete
        self.h_counter >= self.cycles_per_scanline() as u16
    }

    /// Reset timing to start of frame.
    /// The Genesis starts in VBLANK after reset.
    pub fn reset(&mut self) {
        // Start at first VBLANK scanline (224 for NTSC) to match MAME's initial state
        self.scanline = match self.region {
            VideoRegion::Ntsc => ntsc::SCANLINES_VISIBLE, // 224
            VideoRegion::Pal => pal::SCANLINES_VISIBLE,
        };
        self.h_counter = 0;
        self.frame_cycles = 0;
        // Genesis starts in VBLANK after reset - games expect this
        self.in_vblank = true;
        self.in_hblank = false;
        self.vblank_pending = false;
        self.vint_irq_pending = false;
        self.hblank_pending = false;
        self.vint_request_delay_mclks = -1;
        self.first_frame_after_reset = true;
    }

    /// Check if VBLANK interrupt should trigger (internal helper for timing tests).
    /// In production, Vdp::should_trigger_vint() is used instead, which checks
    /// the vint_irq_pending flag.
    pub fn should_trigger_vint(&self, vint_enabled: bool) -> bool {
        self.in_vblank && vint_enabled && self.vint_irq_pending
    }

    /// Check if HBLANK interrupt should trigger.
    pub fn should_trigger_hint(&self, hint_enabled: bool) -> bool {
        hint_enabled && self.h_counter_current == 0 && !self.hblank_pending
    }

    /// Acknowledge VBLANK interrupt (clears the IRQ request to the CPU).
    /// The F flag in the status register is NOT cleared here - it persists
    /// until the 68K acknowledges the interrupt (`ack_vint_iack`).
    pub fn ack_vint(&mut self) {
        self.vint_irq_pending = false;
    }

    /// Acknowledge HBLANK interrupt.
    pub fn ack_hint(&mut self) {
        self.hblank_pending = false;
    }

    /// Set VBLANK interrupt pending.
    /// Sets both the F flag (status register) and the IRQ request to CPU.
    pub fn set_vint_pending(&mut self) {
        self.vblank_pending = true;
        self.vint_irq_pending = true;
    }

    /// Arm the deferred 68K vertical-interrupt request. The F flag
    /// (`vblank_pending`) and Z80 IRQ edge are set separately at the line's
    /// start via `set_vint_pending()`; this only holds off the level-6 68K IRQ
    /// for `delay_mclks` master clocks so it lands part-way into line 224
    /// rather than at its start, as on hardware. Some VINT-synchronised game
    /// loops (e.g. Gunstar Heroes' intro job queue) depend on this offset.
    pub fn arm_vint_request_delay(&mut self, delay_mclks: u32) {
        self.vint_request_delay_mclks = if delay_mclks == 0 {
            -1
        } else {
            delay_mclks as i32
        };
    }

    /// Advance the 68K vertical-interrupt request delay by `master_clocks`.
    /// No-op once the delay has elapsed (or was never armed).
    pub fn tick_vint_request_delay(&mut self, master_clocks: u64) {
        if self.vint_request_delay_mclks < 0 {
            return;
        }
        self.vint_request_delay_mclks -= master_clocks.min(i32::MAX as u64) as i32;
        if self.vint_request_delay_mclks <= 0 {
            self.vint_request_delay_mclks = -1;
        }
    }

    /// True while the 68K vertical-interrupt request is being held off (the
    /// beam has not yet reached the interrupt's point on the VBLANK line). The F flag is
    /// already set; only the CPU IRQ assertion waits.
    pub fn vint_request_delayed(&self) -> bool {
        self.vint_request_delay_mclks >= 0
    }

    /// Set HBLANK interrupt pending.
    pub fn set_hint_pending(&mut self) {
        self.hblank_pending = true;
    }

    /// Interrupt acknowledge (IACK) for VINT.
    /// Called when the 68K CPU actually takes the level 6 interrupt.
    /// Clears the F flag (vblank_pending) and vint_irq_pending.
    /// The F flag is cleared on IACK, not on VDP status register reads
    /// (MAME and Genesis Plus GX agree).
    pub fn ack_vint_iack(&mut self) {
        self.vblank_pending = false;
        self.vint_irq_pending = false;
        self.vint_request_delay_mclks = -1;
    }
}

impl Default for VdpTiming {
    fn default() -> Self {
        Self::new(VideoRegion::Ntsc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timing_new_ntsc() {
        let timing = VdpTiming::new(VideoRegion::Ntsc);
        assert_eq!(timing.region, VideoRegion::Ntsc);
        // Genesis starts in VBLANK at scanline 224 (first VBLANK scanline)
        assert_eq!(timing.scanline, ntsc::SCANLINES_VISIBLE); // 224
        assert!(timing.in_vblank);
        assert!(!timing.in_hblank);
    }

    #[test]
    fn test_timing_new_pal() {
        let timing = VdpTiming::new(VideoRegion::Pal);
        assert_eq!(timing.region, VideoRegion::Pal);
        assert_eq!(timing.total_scanlines(), pal::SCANLINES_TOTAL);
        assert_eq!(timing.visible_scanlines(), pal::SCANLINES_VISIBLE);
    }

    #[test]
    fn test_scanline_advance() {
        let mut timing = VdpTiming::new(VideoRegion::Ntsc);

        // Starts at scanline 224 (VBLANK)
        assert_eq!(timing.scanline, ntsc::SCANLINES_VISIBLE); // 224
        timing.next_scanline();
        assert_eq!(timing.scanline, 225);

        // Advance remaining scanlines to wrap around (225..262 = 37 more scanlines)
        for _ in 0..36 {
            timing.next_scanline();
        }
        // Should wrap to scanline 0 (new frame)
        assert_eq!(timing.scanline, 261);
        timing.next_scanline();
        assert_eq!(timing.scanline, 0);
        assert_eq!(timing.frame_count, 1);
    }

    #[test]
    fn test_vblank_flag_set() {
        let mut timing = VdpTiming::new(VideoRegion::Ntsc);

        // Before VBLANK
        timing.scanline = 100;
        timing.next_scanline();
        assert!(!timing.in_vblank);

        // Enter VBLANK at scanline 224
        timing.scanline = 223;
        timing.next_scanline();
        assert!(timing.in_vblank);
        assert_eq!(timing.scanline, 224);
    }

    #[test]
    fn test_vblank_flag_clear() {
        let mut timing = VdpTiming::new(VideoRegion::Ntsc);

        // Start in VBLANK
        timing.scanline = 250;
        timing.in_vblank = true;

        // Wrap to scanline 0
        timing.scanline = 261;
        timing.next_scanline();

        assert_eq!(timing.scanline, 0);
        assert!(!timing.in_vblank);
    }

    #[test]
    fn test_hblank_cycles() {
        let mut timing = VdpTiming::new(VideoRegion::Ntsc);

        // Execute active display cycles
        let completed = timing.execute_cycles(ntsc::ACTIVE_CYCLES);
        assert!(!completed);
        assert!(timing.in_hblank);

        // Complete scanline
        let completed = timing.execute_cycles(ntsc::HBLANK_CYCLES);
        assert!(completed);
    }

    #[test]
    fn test_vint_trigger() {
        let mut timing = VdpTiming::new(VideoRegion::Ntsc);

        // Starts in VBLANK at scanline 224, but vint_irq_pending is false initially
        assert!(timing.in_vblank);
        assert!(!timing.should_trigger_vint(true)); // No IRQ pending yet

        // Move to visible area (not in VBLANK)
        timing.scanline = 100;
        timing.in_vblank = false;
        assert!(!timing.should_trigger_vint(true));

        // Enter VBLANK and set VINT pending
        timing.scanline = 224;
        timing.in_vblank = true;
        timing.set_vint_pending(); // Sets both vblank_pending and vint_irq_pending

        // Should trigger when enabled and IRQ pending
        assert!(timing.should_trigger_vint(true));
        assert!(!timing.should_trigger_vint(false));

        // After ack, IRQ is no longer pending (but F flag persists in status register)
        timing.ack_vint();
        assert!(!timing.should_trigger_vint(true));
        assert!(timing.vblank_pending); // F flag still set

        // Setting VINT pending again should trigger
        timing.set_vint_pending();
        assert!(timing.should_trigger_vint(true));
    }

    #[test]
    fn test_hint_counter() {
        let mut timing = VdpTiming::new(VideoRegion::Ntsc);

        // Set HINT counter reload value
        timing.h_counter_reload = 2;
        timing.h_counter_current = 2;

        // First scanline: counter = 2 -> 1
        timing.next_scanline();
        assert_eq!(timing.h_counter_current, 1);
        assert!(!timing.should_trigger_hint(true));

        // Second scanline: counter = 1 -> 0
        timing.next_scanline();
        assert_eq!(timing.h_counter_current, 0);
        assert!(timing.should_trigger_hint(true));

        // Third scanline: counter reloads to 2
        timing.next_scanline();
        assert_eq!(timing.h_counter_current, 2);
    }

    #[test]
    fn test_frame_timing_ntsc() {
        let mut timing = VdpTiming::new(VideoRegion::Ntsc);

        assert_eq!(timing.frame_count, 0);
        // Starts at scanline 224

        // Advance from 224 to 262 (wrap) = 38 scanlines, then 0 to 223 = 224 more
        // Total to complete one frame: 262 scanlines from initial position
        for _ in 0..ntsc::SCANLINES_TOTAL {
            timing.next_scanline();
        }

        // After 262 next_scanline() calls from 224, we should be back at 224
        // 224 + 262 = 486, 486 % 262 = 224... but frame_count increments at wrap
        // Actually: 224->225->...->261->0(frame+1)->1->...->223->224
        // That's 38 + 224 = 262 calls, ending at scanline 224 with frame_count=1
        assert_eq!(timing.frame_count, 1);
        assert_eq!(timing.scanline, ntsc::SCANLINES_VISIBLE); // Back to 224
    }

    #[test]
    fn test_reset() {
        let mut timing = VdpTiming::new(VideoRegion::Ntsc);

        timing.scanline = 100;
        timing.in_vblank = false;
        timing.vblank_pending = true;

        timing.reset();

        // Genesis starts in VBLANK at scanline 224 after reset
        assert_eq!(timing.scanline, ntsc::SCANLINES_VISIBLE); // 224
        assert!(timing.in_vblank);
        assert!(!timing.vblank_pending);
    }
}
