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

/// The Mega Drive's clocks and line counts in each video region (the region
/// itself is retro-core's [`VideoRegion`]).
pub trait GenesisTiming {
    /// The master clock, in Hz.
    fn master_clock(&self) -> u64;
    /// The 68000's clock, the master clock over 7, in Hz.
    fn cpu_clock(&self) -> u64;
    /// Lines in a frame.
    fn scanlines(&self) -> u32;
    /// Master clocks in a line, 3420 in both regions.
    fn master_clocks_per_scanline(&self) -> u32;
    /// The 68000's divider of the master clock, 7.
    fn cpu_divider(&self) -> u32;
    /// Frames per second: the master clock over the master clocks in one
    /// frame (`scanlines x 3420`), NTSC 59.9227 Hz, PAL 49.7015 Hz. Derived
    /// from the same arithmetic the VDP runs, so the rate the host paces
    /// against can never drift from the frame the machine produces.
    fn frame_rate(&self) -> f32;
}

pub use retro_core::types::VideoRegion;

impl GenesisTiming for VideoRegion {
    fn master_clock(&self) -> u64 {
        match self {
            VideoRegion::Ntsc => MASTER_CLOCK_NTSC,
            VideoRegion::Pal => MASTER_CLOCK_PAL,
        }
    }

    fn cpu_clock(&self) -> u64 {
        match self {
            VideoRegion::Ntsc => CPU_68K_CLOCK_NTSC,
            VideoRegion::Pal => CPU_68K_CLOCK_PAL,
        }
    }

    fn scanlines(&self) -> u32 {
        match self {
            VideoRegion::Ntsc => SCANLINES_NTSC,
            VideoRegion::Pal => SCANLINES_PAL,
        }
    }

    fn master_clocks_per_scanline(&self) -> u32 {
        3420
    }

    fn cpu_divider(&self) -> u32 {
        7
    }

    fn frame_rate(&self) -> f32 {
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
        assert!((ntsc.frame_rate() - 59.9227).abs() < 1e-4);

        let pal = VideoRegion::Pal;
        assert_eq!(pal.scanlines(), 313);
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
