//! Pure window-mask helpers for the per-scanline compositor.
//!
//! The SNES has two horizontal windows (W1, W2) defined by their
//! left/right edges (`$2126-$2129`). Each layer (BG1-4, OBJ, and the
//! colour-math "COL" pseudo-layer) picks per-window enable + invert
//! bits (`$2123-$2125`) and a 2-bit combine rule (`$212A`/`$212B`)
//! that fuses the two windows. The result is a per-column boolean:
//! for BG/OBJ it means "this column sits inside the configured
//! window region" (used by TMW/TSW to clip the layer); for COL it is
//! post-processed by [`col_region`] into the colour-math /
//! main-screen-black region.
//!
//! Both helpers are `&self`-free so the renderer can fold them into
//! its mutable-state walk and the unit tests can drive them with
//! synthetic register values. Written from fullsnes's window
//! registers; checked against bsnes-plus.

use super::WindowEnable;

/// `true` when column `x` falls inside the fused window region for a
/// layer with the given enable/invert bits, combine rule, and edges.
///
/// - Neither window enabled -> always `false`.
/// - One window enabled -> `(x in range) ^ invert` for that window.
/// - Both enabled -> combine the two `(x in range) ^ invert` masks
///   via OR / AND / XOR / XNOR (`combine & 3`).
///
/// Edge tests are inclusive on both ends (`x >= left && x <= right`),
/// exactly as the hardware compares the dot counter.
#[must_use]
pub fn window_inside(
    x: u16,
    we: WindowEnable,
    combine: u8,
    w1_left: u8,
    w1_right: u8,
    w2_left: u8,
    w2_right: u8,
) -> bool {
    let in_w1 = x >= u16::from(w1_left) && x <= u16::from(w1_right);
    let in_w2 = x >= u16::from(w2_left) && x <= u16::from(w2_right);
    match (we.w1_enable, we.w2_enable) {
        (false, false) => false,
        (true, false) => in_w1 ^ we.w1_invert,
        (false, true) => in_w2 ^ we.w2_invert,
        (true, true) => {
            let m1 = in_w1 ^ we.w1_invert;
            let m2 = in_w2 ^ we.w2_invert;
            match combine & 0x3 {
                0 => m1 | m2,
                1 => m1 & m2,
                2 => m1 ^ m2,
                _ => !(m1 ^ m2),
            }
        }
    }
}

/// Resolve the colour-window region for one screen from the 2-bit
/// CGWSEL mask (`col_main_mask` / `col_sub_mask`) and the geometric
/// `inside` flag from [`window_inside`] using the COL window config.
///
/// - `0` = always in region (regardless of geometry),
/// - `1` = inside the window only,
/// - `2` = outside the window only,
/// - `3` = never in region.
///
/// Combined with [`window_inside`] returning `false` when no COL
/// window is enabled, mask `2` ("outside") yields the whole line and
/// mask `1` ("inside") yields nothing, as the hardware does.
#[must_use]
pub fn col_region(mask_mode: u8, inside: bool) -> bool {
    match mask_mode & 0x3 {
        0 => true,
        1 => inside,
        2 => !inside,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn we(w1_enable: bool, w1_invert: bool, w2_enable: bool, w2_invert: bool) -> WindowEnable {
        WindowEnable {
            w1_enable,
            w1_invert,
            w2_enable,
            w2_invert,
        }
    }

    #[test]
    fn both_disabled_is_never_inside() {
        let cfg = we(false, false, false, false);
        for x in [0u16, 50, 128, 255] {
            assert!(!window_inside(x, cfg, 0, 40, 160, 80, 200));
        }
    }

    #[test]
    fn single_window_one_inclusive_range() {
        let cfg = we(true, false, false, false);
        // Window 1 covers [40, 160] inclusive.
        assert!(!window_inside(39, cfg, 0, 40, 160, 0, 0));
        assert!(window_inside(40, cfg, 0, 40, 160, 0, 0));
        assert!(window_inside(100, cfg, 0, 40, 160, 0, 0));
        assert!(window_inside(160, cfg, 0, 40, 160, 0, 0));
        assert!(!window_inside(161, cfg, 0, 40, 160, 0, 0));
    }

    #[test]
    fn single_window_invert_flips_inside() {
        let cfg = we(true, true, false, false);
        // Inverted W1: inside [40,160] becomes "outside".
        assert!(window_inside(39, cfg, 0, 40, 160, 0, 0));
        assert!(!window_inside(100, cfg, 0, 40, 160, 0, 0));
        assert!(window_inside(161, cfg, 0, 40, 160, 0, 0));
    }

    #[test]
    fn window_two_only_uses_window_two_edges() {
        let cfg = we(false, false, true, false);
        assert!(!window_inside(50, cfg, 0, 0, 0, 80, 200));
        assert!(window_inside(80, cfg, 0, 0, 0, 80, 200));
        assert!(window_inside(200, cfg, 0, 0, 0, 80, 200));
        assert!(!window_inside(201, cfg, 0, 0, 0, 80, 200));
    }

    #[test]
    fn combine_or_is_union() {
        let cfg = we(true, false, true, false);
        // W1 [10,20], W2 [100,110]; OR (combine 0) covers both bands.
        assert!(window_inside(15, cfg, 0, 10, 20, 100, 110));
        assert!(window_inside(105, cfg, 0, 10, 20, 100, 110));
        assert!(!window_inside(50, cfg, 0, 10, 20, 100, 110));
    }

    #[test]
    fn combine_and_is_intersection() {
        let cfg = we(true, false, true, false);
        // W1 [10,120], W2 [100,200]; AND (combine 1) only [100,120].
        assert!(!window_inside(50, cfg, 1, 10, 120, 100, 200));
        assert!(window_inside(110, cfg, 1, 10, 120, 100, 200));
        assert!(!window_inside(150, cfg, 1, 10, 120, 100, 200));
    }

    #[test]
    fn combine_xor_is_symmetric_difference() {
        let cfg = we(true, false, true, false);
        // W1 [10,120], W2 [100,200]; XOR (combine 2) excludes overlap.
        assert!(window_inside(50, cfg, 2, 10, 120, 100, 200), "W1 only");
        assert!(!window_inside(110, cfg, 2, 10, 120, 100, 200), "overlap");
        assert!(window_inside(150, cfg, 2, 10, 120, 100, 200), "W2 only");
    }

    #[test]
    fn combine_xnor_is_complement_of_xor() {
        let cfg = we(true, false, true, false);
        // XNOR (combine 3): inside iff in both or in neither.
        assert!(!window_inside(50, cfg, 3, 10, 120, 100, 200), "W1 only");
        assert!(window_inside(110, cfg, 3, 10, 120, 100, 200), "overlap");
        assert!(window_inside(5, cfg, 3, 10, 120, 100, 200), "neither");
    }

    #[test]
    fn col_region_always_and_never_ignore_geometry() {
        assert!(col_region(0, false));
        assert!(col_region(0, true));
        assert!(!col_region(3, false));
        assert!(!col_region(3, true));
    }

    #[test]
    fn col_region_inside_and_outside_track_geometry() {
        assert!(col_region(1, true));
        assert!(!col_region(1, false));
        assert!(!col_region(2, true));
        assert!(col_region(2, false));
    }
}
