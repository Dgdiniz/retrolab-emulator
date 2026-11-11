//! Timing constants and utilities for Genesis emulation.
//!
//! The Genesis hardware is built around a master clock that drives all components.

/// Master clock frequency (NTSC) in Hz.
pub const MASTER_CLOCK_NTSC: u64 = 53_693_175;

/// Master clock frequency (PAL) in Hz.
pub const MASTER_CLOCK_PAL: u64 = 53_203_424;

/// 68000 CPU clock frequency (NTSC) in Hz.
/// Derived from master clock divided by 7.
pub const CPU_68K_CLOCK_NTSC: u64 = MASTER_CLOCK_NTSC / 7; // ~7.67 MHz

/// 68000 CPU clock frequency (PAL) in Hz.
pub const CPU_68K_CLOCK_PAL: u64 = MASTER_CLOCK_PAL / 7; // ~7.60 MHz

/// Z80 CPU clock frequency (NTSC) in Hz.
/// Derived from master clock divided by 15.
pub const Z80_CLOCK_NTSC: u64 = MASTER_CLOCK_NTSC / 15; // ~3.58 MHz

/// Z80 CPU clock frequency (PAL) in Hz.
pub const Z80_CLOCK_PAL: u64 = MASTER_CLOCK_PAL / 15; // ~3.55 MHz

/// VDP clock frequency (NTSC) in Hz.
/// Derived from master clock divided by 4.
pub const VDP_CLOCK_NTSC: u64 = MASTER_CLOCK_NTSC / 4; // ~13.42 MHz

/// VDP clock frequency (PAL) in Hz.
pub const VDP_CLOCK_PAL: u64 = MASTER_CLOCK_PAL / 4; // ~13.30 MHz

/// Number of scanlines per frame (NTSC).
pub const SCANLINES_NTSC: u32 = 262;

/// Number of scanlines per frame (PAL).
pub const SCANLINES_PAL: u32 = 313;

/// Number of 68000 CPU cycles per scanline (NTSC).
/// Approximately: 7.67 MHz / 59.92 Hz / 262 scanlines
pub const CYCLES_PER_SCANLINE_NTSC: u32 = 488;

/// 68000 CPU cycles per scanline (PAL), as [`VideoRegion::cycles_per_scanline`]
/// reports it. A PAL line is 3420 master clocks, 488.57 68K cycles as on NTSC.
pub const CYCLES_PER_SCANLINE_PAL: u32 = 487;

/// Video region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoRegion {
    #[default]
    Ntsc,
    Pal,
}

impl VideoRegion {
    /// Get the master clock for this region.
    pub fn master_clock(&self) -> u64 {
        match self {
            VideoRegion::Ntsc => MASTER_CLOCK_NTSC,
            VideoRegion::Pal => MASTER_CLOCK_PAL,
        }
    }

    /// Get the 68000 CPU clock for this region.
    pub fn cpu_clock(&self) -> u64 {
        match self {
            VideoRegion::Ntsc => CPU_68K_CLOCK_NTSC,
            VideoRegion::Pal => CPU_68K_CLOCK_PAL,
        }
    }

    /// Get the number of scanlines per frame for this region.
    pub fn scanlines(&self) -> u32 {
        match self {
            VideoRegion::Ntsc => SCANLINES_NTSC,
            VideoRegion::Pal => SCANLINES_PAL,
        }
    }

    /// Get the number of CPU cycles per scanline for this region (approximate).
    pub fn cycles_per_scanline(&self) -> u32 {
        match self {
            VideoRegion::Ntsc => CYCLES_PER_SCANLINE_NTSC,
            VideoRegion::Pal => CYCLES_PER_SCANLINE_PAL,
        }
    }

    /// Get the exact number of master clocks per scanline (no fractional error).
    /// NTSC/PAL: 3420 master clocks per scanline.
    pub fn master_clocks_per_scanline(&self) -> u32 {
        // Both NTSC and PAL use 3420 master clocks per scanline
        3420
    }

    /// Get the CPU clock divider from master clock.
    /// The 68000 runs at master_clock / 7.
    pub fn cpu_divider(&self) -> u32 {
        7
    }

    /// Frames per second for this region: the master clock over the
    /// master clocks in one frame (`scanlines x 3420`), NTSC 59.9227 Hz,
    /// PAL 49.7015 Hz. Derived from the same arithmetic the VDP runs, so
    /// the rate the host paces against can never drift from the frame
    /// the machine actually produces.
    pub fn frame_rate(&self) -> f32 {
        let clocks_per_frame = self.scanlines() as f64 * self.master_clocks_per_scanline() as f64;
        (self.master_clock() as f64 / clocks_per_frame) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clock_frequencies() {
        // NTSC clocks
        assert_eq!(CPU_68K_CLOCK_NTSC, 7_670_453);
        assert_eq!(Z80_CLOCK_NTSC, 3_579_545);
        assert_eq!(VDP_CLOCK_NTSC, 13_423_293);

        // Verify derivations
        assert_eq!(MASTER_CLOCK_NTSC / 7, CPU_68K_CLOCK_NTSC);
        assert_eq!(MASTER_CLOCK_NTSC / 15, Z80_CLOCK_NTSC);
        assert_eq!(MASTER_CLOCK_NTSC / 4, VDP_CLOCK_NTSC);
    }

    #[test]
    fn test_video_region_methods() {
        let ntsc = VideoRegion::Ntsc;
        assert_eq!(ntsc.scanlines(), 262);
        assert_eq!(ntsc.cycles_per_scanline(), 488);
        assert!((ntsc.frame_rate() - 59.9227).abs() < 1e-4);

        let pal = VideoRegion::Pal;
        assert_eq!(pal.scanlines(), 313);
        assert_eq!(pal.cycles_per_scanline(), 487);
        assert!((pal.frame_rate() - 49.7015).abs() < 1e-4);
    }

    #[test]
    fn frame_rate_is_derived_from_cycle_arithmetic() {
        // 53_693_175 / (262 x 3420 = 896_040) and 53_203_424 / (313 x 3420 = 1_070_460).
        assert_eq!(
            VideoRegion::Ntsc.frame_rate(),
            (53_693_175f64 / 896_040f64) as f32
        );
        assert_eq!(
            VideoRegion::Pal.frame_rate(),
            (53_203_424f64 / 1_070_460f64) as f32
        );
    }

    #[test]
    fn test_default_region() {
        let region = VideoRegion::default();
        assert_eq!(region, VideoRegion::Ntsc);
    }
}
