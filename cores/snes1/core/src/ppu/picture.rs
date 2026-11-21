//! The picture a frame leaves for the host: its size, the normalisation
//! that finishes it, the overscan crop, and the interlace field.
//!
//! The renderer draws each line into the host framebuffer as it goes.
//! Once the frame's last line is drawn, [`Ppu::finalize_frame`] gives
//! the picture one shape: the lo-res rows of a frame with hires lines
//! widened to 512, the rows of a non-interlace hires frame doubled to
//! 448, and an overscan frame cropped to the NTSC window unless the host
//! shows overscan. [`Ppu::active_display_size`] reports the size the
//! host crops to. The field flips once a frame, at VBlank; STAT78
//! reports it, and OBJ interlace reads it to choose a sprite's rows.

use retro_core::Framebuffer;

use super::render::{MAX_SCANLINE_WIDTH, NORMAL_SCANLINE_WIDTH};
use super::Ppu;

impl Ppu {
    /// Live display resolution in pixels as `(width, height)`.
    ///
    /// Width is 512 when *any* visible scanline of the frame just ended
    /// used a hires mode (Mode 5/6), tracked frame-global via
    /// `frame_hires` so an HDMA mid-frame mode sweep (lo-res + hires
    /// bands in one frame) still reports the 512 the hires bands force,
    /// and the lo-res bands are doubled to match by
    /// [`Self::finalize_frame`]. Otherwise 256.
    ///
    /// Height is 224 by default, doubled to 448 for *any* hires frame.
    /// An overscan frame (`$2133` bit 2) is cropped to the same 224
    /// (448) NTSC TV window unless the host enables `show_overscan`, in
    /// which case the full 240 (480) is reported. Interlace hires
    /// (`render_interlace`)
    /// reaches 448 by weaving both fields into distinct rows during
    /// rendering; non-interlace hires reaches the same 448 by vertically
    /// line-doubling the rendered rows in [`Self::finalize_frame`],
    /// bsnes-plus displays non-interlace hires (e.g. the PPU bus activity test, which
    /// writes `$2133 = 0`) at the same 448 the interlace path produces.
    /// Keying both width and height on the frame-global flags (rather
    /// than the live `bgmode` / interlace bit) keeps the rendered row
    /// count and the reported size in lockstep even when the game
    /// toggles state between the last active line and VBlank. The host
    /// renderer reads this via
    /// [`crate::system::SnesEmulator::get_display_info`].
    pub fn active_display_size(&self) -> (u32, u32) {
        let w = if self.frame_hires {
            MAX_SCANLINE_WIDTH as u32
        } else {
            NORMAL_SCANLINE_WIDTH as u32
        };
        // Default output is the 224-line NTSC "TV window", an overscan
        // frame is cropped to 224 (the renderer still draws all 239
        // lines; [`Self::finalize_frame`] centers the visible window so
        // the host crops from row 0). Only the explicit `show_overscan`
        // toggle exposes the full 240-line frame.
        let base_h = if self.overscan && self.show_overscan {
            240
        } else {
            224
        };
        let h = if self.frame_hires { base_h * 2 } else { base_h };
        (w, h)
    }

    /// Normalize the just-rendered frame so the host sees a uniform
    /// 512x448 image whenever any scanline was hires.
    ///
    /// Two normalizations run, in order, once per frame at VBlankStart
    /// (before the host samples `active_display_size`):
    ///
    /// 1. **Horizontal lo-res -> 512.** In a mixed-mode frame (HDMA
    ///    sweeps BGMODE across lo-res and hires bands) the hires bands
    ///    force the reported width to 512, but the lo-res bands only
    ///    filled columns 0-255. Expand every such row in place
    ///    (`col 2x = col 2x+1 = col x`, walked high->low so each source
    ///    column is read before any write clobbers it). Rows a hires
    ///    scanline already produced (`line_hires[row]`) are 512 wide and
    ///    skipped.
    ///
    /// 2. **Vertical line-double for non-interlace hires.** The renderer
    ///    emits one output row per scanline for non-interlace frames
    ///    (`base_h` rows); the interlace weave already emits two distinct
    ///    rows per scanline (`2*base_h`). bsnes-plus displays
    ///    non-interlace hires at the same 448 the interlace path
    ///    produces, so duplicate each rendered row (`row k -> rows 2k,
    ///    2k+1`), walked high->low so a source row is copied before a
    ///    lower index overwrites it. Skipped for interlace frames, which
    ///    are already `2*base_h` distinct rows.
    ///
    /// 3. **Overscan crop to the NTSC TV window.** Unless the host
    ///    enabled `show_overscan`, an overscan frame is cropped to the
    ///    same 224 (448 hires) lines a non-overscan frame produces, with
    ///    the visible window centered (top 7 / 14 rows dropped) so the
    ///    host keeps cropping from row 0 (the window bsnes-plus's NTSC
    ///    output shows as well).
    ///    Runs for lo-res frames too, many test ROMs enable overscan
    ///    on a menu that only fills 224 lines, so it must sit outside
    ///    the `frame_hires` guard.
    ///
    /// A no-op when the frame was lo-res and overscan is off (or shown).
    /// The emulator runs it as it finishes each frame's picture; a host that
    /// drives the PPU line by line runs it after the frame's last line.
    pub fn finalize_frame(&self, fb: &mut Framebuffer) {
        // Steps 1+2: hires normalization. Guard against a framebuffer
        // narrower than the hires line; the production allocation is
        // always 512 wide, but staying safe avoids an out-of-bounds
        // write if that ever changes.
        if self.frame_hires && (fb.width as usize) >= MAX_SCANLINE_WIDTH {
            let stride = (fb.width * 4) as usize;
            let fb_height = fb.height as usize;
            let base_h = if self.overscan { 240 } else { 224 } as usize;

            // Rows the renderer actually produced this frame: the
            // interlace weave emitted two output rows per scanline
            // (`2*base_h`), the non-interlace path emitted one
            // (`base_h`).
            let source_rows = if self.render_interlace {
                base_h * 2
            } else {
                base_h
            }
            .min(fb_height);

            // Step 1: horizontal lo-res -> 512 over the rendered rows.
            for row in 0..source_rows {
                if self.line_hires.get(row).copied().unwrap_or(false) {
                    continue;
                }
                let base = row * stride;
                for x in (0..NORMAL_SCANLINE_WIDTH).rev() {
                    let src = base + x * 4;
                    let d0 = base + x * 8;
                    let d1 = d0 + 4;
                    for b in 0..4 {
                        let v = fb.data[src + b];
                        fb.data[d1 + b] = v;
                        fb.data[d0 + b] = v;
                    }
                }
            }

            // Step 2: vertical line-double `base_h` -> `2*base_h` for
            // non-interlace hires. Interlace frames already wove
            // distinct rows and are left untouched.
            if !self.render_interlace {
                let target_rows = (base_h * 2).min(fb_height);
                for k in (0..base_h).rev() {
                    let d0 = 2 * k;
                    if d0 >= target_rows {
                        continue;
                    }
                    let src = k * stride;
                    let src_end = src + stride;
                    let d1 = d0 + 1;
                    if d1 < target_rows {
                        fb.data.copy_within(src..src_end, d1 * stride);
                    }
                    // `d0 == k` only when `k == 0` (an identity copy).
                    fb.data.copy_within(src..src_end, d0 * stride);
                }
            }
        }

        // Step 3: crop overscan to the 224-line NTSC TV window.
        if self.overscan && !self.show_overscan {
            self.crop_overscan_window(fb);
        }
    }

    /// Shift the rendered frame up so the visible 224-line (448 hires)
    /// window starts at row 0, dropping the top 7 (14 hires) overscan
    /// scanlines. The host then crops `active_display_size` rows from
    /// the top exactly as it does for a non-overscan frame, the
    /// window bsnes-plus shows for an overscan frame on NTSC. The bottom
    /// overscan lines fall outside the cropped height and are ignored.
    fn crop_overscan_window(&self, fb: &mut Framebuffer) {
        let stride = (fb.width * 4) as usize;
        if stride == 0 {
            return;
        }
        let fb_height = fb.height as usize;
        let (offset_rows, disp_h) = if self.frame_hires {
            (14usize, 448usize)
        } else {
            (7usize, 224usize)
        };
        let copy_rows = disp_h.min(fb_height.saturating_sub(offset_rows));
        if copy_rows == 0 {
            return;
        }
        let src_start = offset_rows * stride;
        let src_end = src_start + copy_rows * stride;
        if src_end <= fb.data.len() {
            fb.data.copy_within(src_start..src_end, 0);
        }
    }

    /// Flip the interlace field. Called once per frame at
    /// VBlankStart by `SnesEmulator::handle_event_at` (`system.rs`).
    /// The field state is observable via [`Self::field`] and surfaces
    /// on `$213F` (STAT78) bit 7. Mode 5/6 hires interlace renders
    /// both fields woven into one 512x448 framebuffer per frame (see
    /// [`Self::render_scanline`]); OBJ interlace ($2133 bit 1) still
    /// consults this field for sprite row selection.
    pub(crate) fn toggle_field(&mut self) {
        self.field = !self.field;
    }

    /// Current interlace field (`false` = first / even,
    /// `true` = second / odd). Read by the bus when composing
    /// STAT78 bit 7.
    pub fn field(&self) -> bool {
        self.field
    }

    /// Whether the just-rendered frame is a *true* interlace weave,
    /// hires with the interlace bit latched at the frame top. Both a
    /// woven interlace frame and a line-doubled non-interlace hires
    /// frame report the same 448-line [`Self::active_display_size`], so
    /// a host that wants to deinterlace (bob/weave) needs this to tell
    /// the two apart: `true` here means rows came from two distinct
    /// fields, `false` (on a 448 frame) means each row is a vertical
    /// duplicate produced by [`Self::finalize_frame`].
    pub fn frame_interlaced(&self) -> bool {
        self.frame_hires && self.render_interlace
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalize_frame_doubles_lores_rows_when_frame_hires() {
        // Non-interlace hires normalization in two passes:
        //   1. horizontal: a lo-res source row (cols 0..255 filled) is
        //      expanded to 512 (`col 2x = col 2x+1 = col x`); a hires
        //      source row (already 512) is skipped.
        //   2. vertical: the base_h (224) source rows are line-doubled
        //      to 448 (`source row k -> output rows 2k, 2k+1`).
        // So source row 0 (lo-res) lands, horizontally doubled, at output
        // rows 0 and 1; source row 1 (hires) lands verbatim at output
        // rows 2 and 3.
        let mut ppu = Ppu::new();
        let mut fb = Framebuffer::new(512, 480);
        ppu.frame_hires = true;
        ppu.render_interlace = false; // non-interlace -> vertical double
        ppu.line_hires[0] = false; // lo-res source row -> horiz-doubled
        ppu.line_hires[1] = true; // hires source row -> left 512-wide

        let stride = 512 * 4usize;
        for x in 0..512usize {
            // Source row 0: distinct content in cols 0..255 (R = column
            // index); cols 256..511 carry a sentinel so we can confirm
            // the horizontal doubling overwrites the stale right half.
            let p0 = x * 4;
            fb.data[p0] = x as u8;
            fb.data[p0 + 1] = 0x00;
            fb.data[p0 + 2] = if x < 256 { 0x00 } else { 0xCC };
            fb.data[p0 + 3] = 0xFF;
            // Source row 1 (hires): a flat pattern.
            let p1 = stride + x * 4;
            fb.data[p1] = 0x11;
            fb.data[p1 + 1] = 0x22;
            fb.data[p1 + 2] = 0x33;
            fb.data[p1 + 3] = 0x44;
        }

        ppu.finalize_frame(&mut fb);

        // Source row 0 -> output rows 0 and 1, each horizontally doubled
        // (col x duplicated into 2x/2x+1, sentinel overwritten).
        for out_row in [0usize, 1] {
            let base = out_row * stride;
            for x in 0..256usize {
                let want = [x as u8, 0x00, 0x00, 0xFF];
                let d0 = base + (x * 2) * 4;
                let d1 = base + (x * 2 + 1) * 4;
                assert_eq!(&fb.data[d0..d0 + 4], &want[..], "row{out_row} col {x} even");
                assert_eq!(&fb.data[d1..d1 + 4], &want[..], "row{out_row} col {x} odd");
            }
        }
        // Source row 1 (hires) -> output rows 2 and 3, verbatim 512-wide.
        for out_row in [2usize, 3] {
            for x in 0..512usize {
                let p = out_row * stride + x * 4;
                assert_eq!(
                    &fb.data[p..p + 4],
                    &[0x11u8, 0x22, 0x33, 0x44][..],
                    "hires source row -> output {out_row} col {x}"
                );
            }
        }
    }

    #[test]
    fn finalize_frame_vertically_doubles_every_non_interlace_hires_row() {
        // Isolate the vertical line-double: mark every source row hires
        // so step 1 (horizontal) is a no-op, tag each source row with a
        // unique R channel, and confirm row k reappears at output rows
        // 2k and 2k+1 across the full 224 -> 448 expansion. The high->low
        // copy order must not clobber a not-yet-read source row.
        let mut ppu = Ppu::new();
        let mut fb = Framebuffer::new(512, 480);
        ppu.frame_hires = true;
        ppu.render_interlace = false;
        let stride = 512 * 4usize;
        for row in 0..224usize {
            ppu.line_hires[row] = true; // skip horizontal pass
            let p = row * stride; // tag col 0 is enough
            fb.data[p] = row as u8;
            fb.data[p + 3] = 0xFF;
        }

        ppu.finalize_frame(&mut fb);

        for k in 0..224usize {
            let r0 = (2 * k) * stride;
            let r1 = (2 * k + 1) * stride;
            assert_eq!(fb.data[r0], k as u8, "source row {k} -> output {}", 2 * k);
            assert_eq!(
                fb.data[r1],
                k as u8,
                "source row {k} -> output {}",
                2 * k + 1
            );
        }
    }

    #[test]
    fn finalize_frame_interlace_hires_does_not_vertically_double() {
        // The interlace weave already emitted 2*base_h (448) distinct
        // rows, so finalize must leave them alone, re-doubling would
        // overwrite the odd field with the even field and collapse the
        // vertical resolution it just produced.
        let mut ppu = Ppu::new();
        let mut fb = Framebuffer::new(512, 480);
        ppu.frame_hires = true;
        ppu.render_interlace = true; // woven 448
        let stride = 512 * 4usize;
        for row in 0..448usize {
            ppu.line_hires[row] = true; // every row already 512-wide
            for x in 0..512usize {
                let p = row * stride + x * 4;
                fb.data[p] = row as u8; // distinct per row (mod 256)
                fb.data[p + 3] = 0xFF;
            }
        }
        let before = fb.data.clone();
        ppu.finalize_frame(&mut fb);
        assert_eq!(
            fb.data, before,
            "interlace weave is already 448, no vertical re-double"
        );
    }

    #[test]
    fn active_display_size_reports_448_for_any_hires_frame() {
        let mut ppu = Ppu::new();
        // Non-hires: 256x224.
        assert_eq!(ppu.active_display_size(), (256, 224));
        // Non-interlace hires: 512x448 (doubled via finalize).
        ppu.frame_hires = true;
        ppu.render_interlace = false;
        assert_eq!(ppu.active_display_size(), (512, 448));
        // Interlace hires: 512x448 (doubled via weave).
        ppu.render_interlace = true;
        assert_eq!(ppu.active_display_size(), (512, 448));
        // Overscan hires, cropped to the TV window by default: 512x448.
        ppu.overscan = true;
        assert_eq!(ppu.active_display_size(), (512, 448));
        // Overscan hires with the toggle on: full 512x480.
        ppu.set_show_overscan(true);
        assert_eq!(ppu.active_display_size(), (512, 480));
    }

    #[test]
    fn active_display_size_crops_overscan_to_224_by_default() {
        let mut ppu = Ppu::new();
        ppu.overscan = true;
        // Default: overscan is cropped to the 224-line NTSC TV window.
        assert_eq!(ppu.active_display_size(), (256, 224));
        // Toggle on: the full 240-line frame is exposed.
        ppu.set_show_overscan(true);
        assert_eq!(ppu.active_display_size(), (256, 240));
    }

    #[test]
    fn finalize_frame_is_noop_when_not_hires() {
        // A wholly lo-res frame with overscan off is reported 256x224,
        // so there is nothing to expand or crop, finalize must leave
        // the buffer untouched.
        let mut ppu = Ppu::new();
        let mut fb = Framebuffer::new(512, 480);
        ppu.frame_hires = false;
        for (i, byte) in fb.data.iter_mut().enumerate() {
            *byte = (i & 0xFF) as u8;
        }
        let before = fb.data.clone();
        ppu.finalize_frame(&mut fb);
        assert_eq!(fb.data, before, "finalize is a no-op when !frame_hires");
    }

    #[test]
    fn finalize_frame_crops_overscan_window_up_seven_rows() {
        // With overscan on and the toggle off, finalize shifts the
        // visible window up 7 rows so the host crops the 224-line TV
        // window from row 0.
        let mut ppu = Ppu::new();
        ppu.frame_hires = false;
        ppu.overscan = true;
        let mut fb = Framebuffer::new(512, 480);
        let stride = (fb.width * 4) as usize;
        // Tag each row's first byte with its row index so the shift is
        // observable.
        for row in 0..fb.height as usize {
            fb.data[row * stride] = (row & 0xFF) as u8;
        }
        ppu.finalize_frame(&mut fb);
        // Output row 0 must now carry source row 7, row 1 source row 8...
        assert_eq!(fb.data[0], 7, "row 0 <- source row 7");
        assert_eq!(fb.data[stride], 8, "row 1 <- source row 8");
        assert_eq!(
            fb.data[223 * stride],
            230,
            "row 223 <- source row 230 (last TV-window line)"
        );
    }

    #[test]
    fn finalize_frame_keeps_full_overscan_when_toggle_on() {
        // With the toggle on, the overscan frame is not cropped.
        let mut ppu = Ppu::new();
        ppu.frame_hires = false;
        ppu.overscan = true;
        ppu.set_show_overscan(true);
        let mut fb = Framebuffer::new(512, 480);
        let stride = (fb.width * 4) as usize;
        for row in 0..fb.height as usize {
            fb.data[row * stride] = (row & 0xFF) as u8;
        }
        let before = fb.data.clone();
        ppu.finalize_frame(&mut fb);
        assert_eq!(fb.data, before, "no crop when show_overscan is set");
    }
}
