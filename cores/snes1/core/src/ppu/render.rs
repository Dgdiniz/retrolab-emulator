//! Per-scanline background renderer for SNES Modes 0-7.
//!
//! Hosts the public [`Ppu::render_scanline`] entry point and the
//! per-mode helpers that walk the four BG layers, the Mode 3/4
//! direct-colour path, the Mode 7 affine plane, and the priority
//! compositor. The Mode 7 coefficient state lives in
//! [`super::mode7`]; the affine raster ([`Ppu::render_mode7_rows`])
//! reuses the same mosaic / snapshot / composite pipeline as the
//! tile modes, only Step 1 (the BG fill) differs.
//!
//! The renderer is driven by the H-event scheduler: each
//! [`crate::scheduler::EventKind::RenderPos`] event in
//! `SnesEmulator::handle_event_at` (`system.rs`) calls
//! [`Ppu::render_scanline`] with the freshly-snapshotted scanline
//! and the host framebuffer. The PPU never reaches into the
//! scheduler or the bus, the framebuffer is borrowed as a `&mut`
//! argument so [`crate::system::SnesEmulator`] keeps owning it.
//!
//! ## Hires (Modes 5/6) compositing
//!
//! Modes 5 and 6 render at 512 display pixels wide. The pipeline
//! walks each enabled BG twice (once into the main-screen buffer,
//! once into the sub-screen buffer) and the final scanline
//! interleaves them: even display columns come from sub, odd
//! columns from main. Internal row buffers are 512 wide for every
//! mode, non-hires modes only ever write into the leading 256
//! slots so the shared allocation keeps the cold path cheap. The
//! host framebuffer is sized to fit the worst case (512x480) at
//! emulator construction; [`Ppu::active_display_size`] reports the
//! live (width, height) that the host should crop to.
//!
//! ## Overscan
//!
//! With `$2133` bit 2 set the visible region extends from 224 to
//! 240 lines. The scheduler always fires `RenderPos` for SNES
//! lines `1..=240` and [`Ppu::in_active_scanline`] gates the lower
//! end based on the live overscan bit, so flipping overscan
//! mid-frame switches the renderer on/off for the extra rows
//! without rescheduling.

use retro_core::Framebuffer;

use crate::ppu::colormath::{color_add, color_sub};
use crate::ppu::sprite;
use crate::ppu::tile::{
    build_tilemap_row, compute_offset_per_tile, decode_tile_2bpp_row, decode_tile_4bpp_row,
    decode_tile_8bpp_row, fetch_bg_tilemap_entry, hires_adjacent_tile_offset, mode7_clip,
    mode7_sample, sign_extend_13, tilemap_masks, TilemapEntry,
};
use crate::ppu::window::{col_region, window_inside};
use crate::ppu::window_layer;
use crate::ppu::{Ppu, LAYER_OBJ};

/// One BG layer's contribution to a single scanline, retained
/// between scanlines so vertical mosaic can replay the previous
/// row. 2 KiB per BG x 4 BGs = 8 KiB total (boxed inside the PPU
/// struct so the inline size stays bounded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BgRowPixel {
    /// 15-bit BGR colour ready for the brightness step. Meaningless
    /// when `opaque == false`.
    pub(crate) color: u16,
    /// `true` if the tile pixel was non-zero. A zero tile pixel is
    /// transparent regardless of the CGRAM byte at that index.
    pub(crate) opaque: bool,
    /// Per-tile priority bit (`tile_word & 0x2000`).
    pub(crate) priority: bool,
}

impl BgRowPixel {
    pub(crate) const TRANSPARENT: Self = Self {
        color: 0,
        opaque: false,
        priority: false,
    };
}

/// Per-scanline per-BG pixel buffer.
///
/// 512 entries wide so the same allocation services both non-hires
/// (only slots 0..=255 are written) and hires Modes 5/6 (every
/// slot 0..=511 is written). The leading 256 slots are the "main"
/// view; in non-hires they are also the only meaningful slots.
pub(crate) type BgRowBuffer = [BgRowPixel; 512];

/// Maximum SNES output width in display pixels (Mode 5/6 hires).
pub(crate) const MAX_SCANLINE_WIDTH: usize = 512;
/// Non-hires output width in display pixels (Modes 0-4 + Mode 7).
pub(crate) const NORMAL_SCANLINE_WIDTH: usize = 256;
/// Maximum host output rows produced in one frame: 240 (overscan) x 2
/// (Mode 5/6 interlace weave) = 480. Bounds the per-row `line_hires`
/// record consumed by the lo-res->512 normalization in
/// [`Ppu::finalize_frame`].
pub(crate) const MAX_OUTPUT_ROWS: usize = 480;

/// One sprite (OBJ) pixel resolved for a single scanline column.
///
/// Sprites collapse to a single per-column layer carrying the colour
/// plus the owning sprite's 2-bit priority; the compositor interleaves
/// that priority with the BG layers via [`priority_layers`]. OBJ is
/// always lo-res (256 wide) even in hires modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ObjRowPixel {
    /// 15-bit BGR colour, meaningful only when `opaque`.
    pub(crate) color: u16,
    /// `true` when a non-zero sprite pixel landed here.
    pub(crate) opaque: bool,
    /// Owning sprite's 2-bit priority (`0..=3`).
    pub(crate) prio: u8,
    /// Owning sprite's 3-bit palette (`0..=7`). Sprites in palettes
    /// 0-3 (CGRAM index < 192) are exempt from colour math, so the
    /// compositor records the palette to derive that flag.
    pub(crate) palette: u8,
}

impl ObjRowPixel {
    pub(crate) const TRANSPARENT: Self = Self {
        color: 0,
        opaque: false,
        prio: 0,
        palette: 0,
    };
}

/// Per-scanline sprite output buffer. Always 256 columns wide, OBJ
/// never renders at hires resolution; the hires compositor maps each
/// OBJ column into both display sub-pixels.
pub(crate) type ObjRowBuffer = [ObjRowPixel; NORMAL_SCANLINE_WIDTH];

/// Which layer won a composited column. Drives the per-source colour-
/// math enable lookup (`color_math_bg_enable` / `color_math_oam_enable`
/// / `color_math_back_enable`) in the colour-math stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PixelSource {
    /// Nothing opaque landed here, the backdrop (CGRAM 0) shows
    /// through, or for the sub-screen the fixed colour stands in.
    Backdrop,
    /// Background layer `id` (`0..=3`).
    Bg(u8),
    /// The sprite layer.
    Obj,
}

/// One fully-resolved column of a single screen (main or sub): the
/// final colour plus the metadata the colour-math stage needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScreenPixel {
    /// 15-bit BGR colour after priority resolution + window clip.
    color: u16,
    /// Layer that produced `color`.
    source: PixelSource,
    /// `true` for an OBJ pixel in palettes 0-3, those never take
    /// part in colour math regardless of the CGADSUB OBJ bit.
    color_exempt: bool,
}

/// A composited screen. Sized at the worst-case 512 columns so the
/// same type serves both the 256-wide normal modes and the 512-wide
/// hires Modes 5/6. Non-hires compositing fills only `0..256`; the
/// hires path composites all 512 BG positions and the output picks
/// the sub screen on even columns and the main screen on odd columns
/// (the order the chip emits them in).
type ScreenRow = [ScreenPixel; MAX_SCANLINE_WIDTH];

/// Per-screen window-clip tables for the five clippable layers
/// (BG1-4 + OBJ), indexed `0..=3` for BG and `4` for OBJ. `true` at
/// column `x` means "clip this layer here" (the layer's TMW/TSW bit
/// is set and column `x` is inside the layer's window region).
type LayerClips = [[bool; NORMAL_SCANLINE_WIDTH]; 5];

/// All-false clip tables: no layer is window-clipped.
const NO_LAYER_CLIPS: LayerClips = [[false; NORMAL_SCANLINE_WIDTH]; 5];

impl Ppu {
    /// Render one SNES scanline into the host framebuffer's RGBA8
    /// plane.
    ///
    /// Skips line 0 (top pre-render) and any scanline beyond the
    /// active region. The active region is 224 lines normally and
    /// 240 lines with overscan (`$2133` bit 2); the live bit is
    /// consulted via [`Ppu::in_active_scanline`] so that
    /// mid-frame overscan toggles flip the gate without rescheduling
    /// the H-event timeline.
    ///
    /// In Mode 5/6 with interlace (`$2133` bit 0) the vertical
    /// resolution doubles: each scanline emits two woven output rows
    /// (the even/odd field lines) so the full 512x448 image is built
    /// in a single frame. The per-row work lives in
    /// [`Ppu::render_output_line`].
    pub fn render_scanline(&mut self, scanline: u16, fb: &mut Framebuffer) {
        if scanline == 0 || !self.in_active_scanline(scanline) {
            return;
        }

        // Per-frame reset of the sprite-overflow flags. They accumulate
        // across the visible scanlines (`|=` in `render_obj_row`) and
        // are read via STAT77 ($213E) during VBlank, so the reset
        // happens at the frame top, and before the forced-blank
        // short-circuit in `render_output_line` so a blanked first line
        // still drops last frame's result.
        //
        // The frame-global display-size state resets here too:
        // `frame_hires` ORs every visible scanline's hires flag (so the
        // reported width is 512 if *any* line was Mode 5/6, which is
        // also how bsnes-plus reports it), `render_interlace` latches the
        // interlace bit for the whole frame (so the weave row count and
        // the reported height can never disagree mid-frame), and
        // `line_hires` is cleared so the lo-res normalization post-pass
        // only doubles rows this frame actually rendered lo-res.
        if scanline == 1 {
            self.sprite_range_over = false;
            self.sprite_time_over = false;
            self.frame_hires = false;
            self.render_interlace = self.interlace;
            self.line_hires = [false; MAX_OUTPUT_ROWS];
        }

        // Sprite range/time-overflow evaluation ($213E STAT77 bits 6/7)
        // runs on every active scanline independent of the OBJ
        // display-enable bits ($212C/$212D) AND forced blank. On hardware
        // the sprite range/tile eval always walks OAM; only the OBJ
        // *compositing* is gated by the enable bits and forced blank
        // (snes9x evaluates them whatever the display state too, forced
        // blank included). Gated on display state, STAT77 would read back
        // 0, and the Electronics Test of Nintendo's SNES Test Program,
        // which overflows a line and then reads $213E bit 6 (range over)
        // while the screen is blanked / OBJ-off, would fail its final gate.
        // This is the single source of truth for the two flags; the
        // painted path in `render_obj_row` does not set them.
        self.eval_obj_overflow(scanline);

        // Accumulate the frame-global hires flag regardless of forced
        // blank, a blanked hires line is still emitted 512 wide.
        let hires = is_hires_mode(self.bgmode);
        self.frame_hires |= hires;

        // Mode 5/6 interlace weaves both fields into a single complete
        // frame. `vy` is the BG vertical source (the value
        // `render_bg_row` adds the per-BG V-scroll to); with interlace
        // it advances one BG line per output row, the weave equivalent
        // of `py = (vcounter << 1) | field`. Sprites stay at the
        // visible scanline (`obj_line = scanline`) and are line-doubled
        // across the field pair. The non-interlace path keeps the
        // plain mapping: output row N-1 <- scanline N (vy = N). The
        // weave is gated on the latched `render_interlace`, not the live
        // bit, so the whole frame agrees with `active_display_size`.
        if hires && self.render_interlace {
            let r0 = u32::from(scanline - 1) * 2;
            self.mark_line_hires(r0, true);
            self.mark_line_hires(r0 + 1, true);
            self.render_output_line(scanline * 2 - 1, scanline, r0, fb);
            self.render_output_line(scanline * 2, scanline, r0 + 1, fb);
        } else {
            let fb_row = u32::from(scanline - 1);
            self.mark_line_hires(fb_row, hires);
            self.render_output_line(scanline, scanline, fb_row, fb);
        }
    }

    /// Record whether output row `fb_row` was produced by a hires
    /// scanline (already 512 wide). Lo-res rows (`false`) are expanded
    /// to 512 by [`Self::finalize_frame`] when the frame is hires.
    fn mark_line_hires(&mut self, fb_row: u32, hires: bool) {
        if let Some(slot) = self.line_hires.get_mut(fb_row as usize) {
            *slot = hires;
        }
    }

    /// Render one output row into the host framebuffer's RGBA8 plane.
    ///
    /// `vy` is the BG vertical coordinate, `obj_line` the visible
    /// scanline used for sprite evaluation, and `fb_row` the
    /// destination row. The split from [`Ppu::render_scanline`] lets
    /// the Mode 5/6 interlace path emit two woven rows per scanline
    /// (doubled `vy`, shared `obj_line`) while the normal path emits
    /// one.
    fn render_output_line(&mut self, vy: u16, obj_line: u16, fb_row: u32, fb: &mut Framebuffer) {
        // The host framebuffer must be at least as tall as the active
        // region (448 with hires interlace) or the trailing rows clip
        // here.
        if fb_row >= fb.height {
            return;
        }

        let hires = is_hires_mode(self.bgmode);
        let output_width = if hires {
            MAX_SCANLINE_WIDTH
        } else {
            NORMAL_SCANLINE_WIDTH
        };

        // Forced blank or zero-brightness collapses to a black row.
        // The vertical-mosaic buffer also gets cleared so a later
        // unmask doesn't pull stale colour back into a mosaic block.
        if self.display_disable || self.display_brightness == 0 {
            write_black_scanline(fb_row, fb, output_width);
            for bg in 0..4 {
                self.previous_bg_rows[bg] = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
            }
            return;
        }

        // Colour math or a main-screen-black colour window can pull
        // the output away from a plain main-screen composite. When
        // neither is configured (and we're not hires, which always
        // interleaves a sub plane) the fast path below skips the
        // sub-screen build and the per-pixel colour-math pass, that's
        // the common case and keeps the Modes 0-7 BG/OBJ path as cheap as
        // a plain composite. Window *layer* clipping (TMW/TSW) is independent
        // of colour math and is always honoured in the composite.
        let color_math_possible = self.col_main_mask != 0
            || self.color_math_back_enable
            || self.color_math_oam_enable
            || self.color_math_bg_enable.iter().any(|&b| b);
        let need_sub = hires || color_math_possible;

        // Per-BG row buffers. Always full-width (512) so the same
        // allocation services hires and non-hires; the cold tail
        // stays transparent unless a hires renderer fills it.
        let mut main_rows: [BgRowBuffer; 4] = [[BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH]; 4];
        let mut sub_rows: [BgRowBuffer; 4] = [[BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH]; 4];

        // Step 1: render each BG into the main / sub buffers per
        // its TM ($212C) / TS ($212D) enable bits. Mode 7 replaces
        // the tile walk with a single affine plane (BG1 + optional
        // EXTBG BG2) and fills the main plane only. BG3 in Modes
        // 2/4/6 is **not** rendered (it's an offset source) so it
        // stays transparent here even though both enable bits may be
        // set.
        if self.bgmode == 7 {
            self.render_mode7_rows(vy, &mut main_rows);
        } else {
            for bg_id in 0..4 {
                if !bg_visible_in_mode(self.bgmode, bg_id) {
                    continue;
                }
                if self.bg_main_enable[bg_id] {
                    self.render_bg_row(bg_id, vy, &mut main_rows[bg_id], hires);
                }
                // The sub plane feeds the hires interleave and the
                // colour-math addend; it is only walked when one of
                // those needs it, so the no-colour-math non-hires path
                // never pays for the second render.
                if self.bg_sub_enable[bg_id] && need_sub {
                    self.render_bg_row(bg_id, vy, &mut sub_rows[bg_id], hires);
                }
            }
        }

        // Debugger layer-disable mask: blank any BG the debugger turned
        // off (works for every mode, Mode 7 included) by clearing its
        // rendered rows so it drops out of the composite below.
        for bg_id in 0..4 {
            if self.layer_debug_disabled(bg_id) {
                main_rows[bg_id] = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
                sub_rows[bg_id] = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
            }
        }

        // Step 2: per-BG horizontal + vertical mosaic on the main
        // plane. The sub plane is intentionally not mosaicked, the
        // vertical-mosaic replay buffer tracks the main plane only and
        // mosaicked-sub-screen colour math is vanishingly rare.
        for (bg_id, row) in main_rows.iter_mut().enumerate() {
            self.apply_mosaic_for_bg(bg_id, vy, row, output_width);
        }

        // Step 3: snapshot each main BG row for next-line vmosaic.
        // We always copy the full 512 cols so the previous-row
        // buffer stays consistent when the mode toggles between
        // hires and non-hires across frames.
        self.previous_bg_rows.copy_from_slice(&main_rows);

        // Step 3.5: evaluate sprites once into a 256-wide OBJ row. The
        // walk is skipped entirely when OBJ is off on both screens.
        let mut obj_row = [ObjRowPixel::TRANSPARENT; NORMAL_SCANLINE_WIDTH];
        if (self.obj_main_enable || self.obj_sub_enable) && !self.layer_debug_disabled(LAYER_OBJ) {
            self.render_obj_row(obj_line, &mut obj_row);
        }

        // Step 4: composite each enabled screen by the per-mode
        // priority list, then run the colour-math stage. Modes 0-6
        // pass the `$2105` bit-3 BG3-priority flag as the priority
        // variant; Mode 7 reuses the same slot for the EXTBG flag
        // (`$2133` bit 6). OBJ is gated per-plane by `obj_main_enable`
        // / `obj_sub_enable`.
        let priority_variant = if self.bgmode == 7 {
            self.mode7_extbg
        } else {
            self.bg3_priority
        };
        let backdrop = self.cgram()[0];

        // Main screen. Window layer-clip is applied whether or not
        // colour math runs.
        let main_clips = self.build_layer_clips(false);
        let main_composite = composite_screen(
            &main_rows,
            &obj_row,
            self.bgmode,
            priority_variant,
            self.obj_main_enable,
            &main_clips,
            backdrop,
            output_width,
            hires,
        );

        // Fast path: non-hires with no colour math -> emit the main
        // composite straight to the framebuffer (identical to the
        // BG/OBJ-only renderer plus window clipping).
        if !need_sub {
            let mut row = [0u16; MAX_SCANLINE_WIDTH];
            for (dst, px) in row.iter_mut().zip(main_composite.iter()) {
                *dst = px.color;
            }
            self.write_scanline_normal(fb_row, &row, fb);
            return;
        }

        // Sub screen. Its backdrop default is the fixed colour in the
        // normal modes (checked against bsnes-plus); hires uses the
        // real backdrop because the sub plane is a displayed image.
        let sub_backdrop = if hires { backdrop } else { self.fixed_color };
        let sub_clips = self.build_layer_clips(true);
        let sub_composite = composite_screen(
            &sub_rows,
            &obj_row,
            self.bgmode,
            priority_variant,
            self.obj_sub_enable,
            &sub_clips,
            sub_backdrop,
            output_width,
            hires,
        );

        // Colour-window regions: `col_main` gates main-screen-black,
        // `col_sub` gates whether colour math runs at all.
        let (col_main, col_sub) = self.build_col_tables(output_width);

        if hires {
            // Hires output: every one of the 512 columns is composited
            // from its own BG position. Even columns show the sub screen
            // with the main screen as the addend, odd columns the main
            // screen with the sub screen as the addend. There is no
            // 2i/2i+1 doubling: the compositor already produced the full
            // 512-wide line.
            let mut line = [0u16; MAX_SCANLINE_WIDTH];
            for (k, cell) in line.iter_mut().enumerate() {
                *cell = if k & 1 == 1 {
                    self.resolve_pixel(k, &main_composite, &sub_composite, &col_main, &col_sub)
                } else {
                    self.resolve_pixel(k, &sub_composite, &main_composite, &col_main, &col_sub)
                };
            }
            self.write_scanline_hires(fb_row, &line, fb);
        } else {
            let mut row = [0u16; MAX_SCANLINE_WIDTH];
            for (x, cell) in row.iter_mut().enumerate().take(NORMAL_SCANLINE_WIDTH) {
                *cell = self.resolve_pixel(x, &main_composite, &sub_composite, &col_main, &col_sub);
            }
            self.write_scanline_normal(fb_row, &row, fb);
        }
    }

    /// Build the per-layer window-clip tables for one screen.
    ///
    /// `sub == false` selects the main screen (TMW `$212E`), `true`
    /// the sub screen (TSW `$212F`). Only layers whose disable bit is
    /// set get a geometry table; the rest stay all-`false` (never
    /// clipped). Index `0..=3` is BG1-4, index `4` is OBJ.
    fn build_layer_clips(&self, sub: bool) -> LayerClips {
        let mut clips = NO_LAYER_CLIPS;
        let disables = if sub {
            [
                self.bg_sub_window_disable[0],
                self.bg_sub_window_disable[1],
                self.bg_sub_window_disable[2],
                self.bg_sub_window_disable[3],
                self.obj_sub_window_disable,
            ]
        } else {
            [
                self.bg_main_window_disable[0],
                self.bg_main_window_disable[1],
                self.bg_main_window_disable[2],
                self.bg_main_window_disable[3],
                self.obj_main_window_disable,
            ]
        };
        let layers = [
            window_layer::BG1,
            window_layer::BG2,
            window_layer::BG3,
            window_layer::BG4,
            window_layer::OBJ,
        ];
        for (slot, (&disabled, &layer)) in clips.iter_mut().zip(disables.iter().zip(layers.iter()))
        {
            if !disabled {
                continue;
            }
            let we = self.window_enable[layer];
            let combine = self.window_combine[layer];
            for (x, cell) in slot.iter_mut().enumerate() {
                *cell = window_inside(
                    x as u16,
                    we,
                    combine,
                    self.window_one_left,
                    self.window_one_right,
                    self.window_two_left,
                    self.window_two_right,
                );
            }
        }
        clips
    }

    /// Build the colour-window region tables for both screens from the
    /// COL window config (`$2125` high nibble / `$212B` bits 2-3) and
    /// the CGWSEL masks (`col_main_mask` / `col_sub_mask`).
    fn build_col_tables(
        &self,
        width: usize,
    ) -> ([bool; MAX_SCANLINE_WIDTH], [bool; MAX_SCANLINE_WIDTH]) {
        let we = self.window_enable[window_layer::COL];
        let combine = self.window_combine[window_layer::COL];
        let mut col_main = [false; MAX_SCANLINE_WIDTH];
        let mut col_sub = [false; MAX_SCANLINE_WIDTH];
        // Colour windows are defined in 256-dot space; in hires
        // (`width == 512`) each dot covers two output columns, so the
        // region for output column `x` is sampled at dot `x >> 1`.
        let hires = width > NORMAL_SCANLINE_WIDTH;
        for x in 0..width {
            let dot = if hires { (x >> 1) as u16 } else { x as u16 };
            let inside = window_inside(
                dot,
                we,
                combine,
                self.window_one_left,
                self.window_one_right,
                self.window_two_left,
                self.window_two_right,
            );
            col_main[x] = col_region(self.col_main_mask, inside);
            col_sub[x] = col_region(self.col_sub_mask, inside);
        }
        (col_main, col_sub)
    }

    /// CGADSUB colour-math enable bit for a winning source layer.
    fn color_math_enabled_for(&self, source: PixelSource) -> bool {
        match source {
            PixelSource::Backdrop => self.color_math_back_enable,
            PixelSource::Bg(id) => self.color_math_bg_enable[id as usize],
            PixelSource::Obj => self.color_math_oam_enable,
        }
    }

    /// The colour that leaves the chip for column `x`: the pixel of the
    /// screen that is shown, with the other screen's pixel, or the fixed
    /// colour, added to or subtracted from it where colour math applies.
    ///
    /// The colour window decides twice: outside its main region the shown
    /// pixel is black, and only inside its sub region does colour math
    /// run at all. It runs for a pixel whose layer takes part (sprites
    /// from the first four palettes never do); the result is halved only
    /// inside the main region, and never when the addend is the sub
    /// screen's transparent backdrop.
    ///
    /// Normally the main screen is shown and the sub screen is the
    /// addend; in a hires line the even columns show the sub screen with
    /// the main screen as the addend, which is the same rule with the
    /// two rows swapped.
    fn resolve_pixel(
        &self,
        x: usize,
        shown: &ScreenRow,
        addend: &ScreenRow,
        col_main: &[bool],
        col_sub: &[bool],
    ) -> u16 {
        let pixel = shown[x];
        let colour = if col_main[x] { pixel.color } else { 0 };
        let takes_part =
            col_sub[x] && !pixel.color_exempt && self.color_math_enabled_for(pixel.source);
        if !takes_part {
            return colour;
        }
        let addend_is_backdrop =
            self.addsub_mode && matches!(addend[x].source, PixelSource::Backdrop);
        let addend_colour = if self.addsub_mode {
            addend[x].color
        } else {
            self.fixed_color
        };
        let halve = self.color_math_halve && col_main[x] && !addend_is_backdrop;
        if self.color_math_subtract {
            color_sub(colour, addend_colour, halve)
        } else {
            color_add(colour, addend_colour, halve)
        }
    }

    /// Fill a row buffer with a single BG's tile output. `hires`
    /// selects between the 256-wide normal path (Modes 0-4) and
    /// the 512-wide hires path (Modes 5/6); in hires every tilemap
    /// entry covers 16 BG-pixels and is paired with its right-side
    /// neighbour via [`hires_adjacent_tile_offset`].
    fn render_bg_row(&mut self, bg_id: usize, scanline: u16, row: &mut BgRowBuffer, hires: bool) {
        let bpp = bpp_for_bg(self.bgmode, bg_id);
        let palette_offset = palette_offset_for_bg(self.bgmode, bg_id);
        let base_hoffset = self.bg_hoffset[bg_id];
        let base_voffset = self.bg_voffset[bg_id];
        // Mode 5/6 fetch the BG on the 512-dot hires grid (`x` runs
        // 0..511), so the horizontal scroll is doubled to live in that
        // space (the scroll register counts the BG's own pixels). At
        // scroll 0 (e.g. RPM Racing's static logo) this is a no-op; once
        // the camera scrolls an undoubled value would run the BG at half
        // speed, breaking the track and sliding sprites onto the grass.
        // Vertical scroll is unaffected, the interlace weave already
        // supplies the doubled `vy`. The offset-per-tile path (Mode 6)
        // consumes the same doubled value (`opt_x`/`hoffset`).
        let h_scroll = if hires {
            base_hoffset << 1
        } else {
            base_hoffset
        };
        let screen_addr = self.bg_screen_addr[bg_id];
        let screen_size = self.bg_screen_size[bg_id];
        let tile_data_addr = self.bg_tiledata_addr[bg_id];
        // 16x16-tile BGs (`$2105` bits 4-7). Each tilemap entry then
        // covers a 2x2 block of 8-px characters; the renderer fetches the
        // entry on a 16-px grid and picks the right/bottom quadrant below.
        // Honoured in every mode, hires included: a Mode 5/6 BG with the
        // size bit set (the PPU bus activity test's Layer 1, Air Strike
        // Patrol's menu text) lays its tilemap out as 16x16 blocks across
        // the 512-dot grid. The 16-px horizontal pick coincides with
        // `hires_adjacent_tile_offset`
        // (`tile` for the left 8 px, `tile+1` for the right), so the two
        // are mutually exclusive per BG: an 8x8 hires BG keeps taking the
        // `else if hires` arm below, a 16x16 one takes the `tile16` arm
        // which additionally selects the bottom characters (`+16`) the
        // 8x8-only hires path can never reach. With interlace the doubled
        // `screen_y` makes that bottom-char pick follow the field
        // (`py = (y << 1) | field`).
        let tile16 = self.bg_tile_size[bg_id];
        let direct_color =
            bg_id == 0 && (self.bgmode == 3 || self.bgmode == 4) && self.direct_color && bpp == 8;
        let row_width: u16 = if hires {
            MAX_SCANLINE_WIDTH as u16
        } else {
            NORMAL_SCANLINE_WIDTH as u16
        };
        let (mask_x, mask_y) = tilemap_masks(screen_size, hires, tile16);

        // Tilemap-row cache (Modes 0/1/3). Offset-per-tile (2/4/6) has
        // a per-column voffset, hires (5/6) a 16-px stride, and 16x16
        // tiles span two 8-px tile rows per entry, so all three keep the
        // direct fetch. For the cached modes the voffset is constant
        // across the scanline, so the whole row sits in a single tile
        // band and one cached row serves all 8 lines.
        let use_cache = !hires && !tile16 && !matches!(self.bgmode, 2 | 4 | 6);
        let cached_row = if use_cache {
            let screen_y = scanline.wrapping_add(base_voffset);
            let ty = (screen_y & mask_y) >> 3;
            Some(self.cached_tilemap_row(bg_id, screen_addr, screen_size, ty))
        } else {
            None
        };

        // Per-scanline decoded-tile memo ("char row" reuse). Runs of
        // the same (tile, flips, row_in_tile), solid fills, repeated
        // patterns, decode once. Local to this call, so it reads live
        // VRAM and needs no invalidation.
        let mut last_decode: Option<(u16, bool, bool, u8, [u8; 8])> = None;

        // Resolve per-pixel by walking the output columns. We
        // group on tile boundaries inside the loop so the tilemap
        // entry + decoded row are reused for each 8-pixel block
        // (and to make the offset-per-tile lookup cheap).
        let mut x: u16 = 0;
        while x < row_width {
            // Per-tile-column offset override (Modes 2/4/6). Mode 6
            // is hires with offset-per-tile; it reuses the same
            // algorithm as Mode 2 (`compute_offset_per_tile` already
            // documents that the BG3 lookup is identical).
            // Tile coordinates within the BG's virtual screen, in
            // *absolute* space. For the offset-per-tile modes
            // `compute_offset_per_tile` already folds the base scroll into
            // the column / scanline; the other modes
            // add it here. Either branch yields absolute coordinates that
            // must not be offset by `x` / `scanline` again.
            let (screen_x, screen_y) = if matches!(self.bgmode, 2 | 4 | 6) {
                let bg3_screen_addr = self.bg_screen_addr[2];
                let bg3_screen_size = self.bg_screen_size[2];
                let bg3_hoffset = self.bg_hoffset[2];
                let bg3_voffset = self.bg_voffset[2];
                compute_offset_per_tile(
                    self.vram(),
                    bg3_screen_addr,
                    bg3_screen_size,
                    bg3_hoffset,
                    bg3_voffset,
                    self.bgmode,
                    bg_id as u8,
                    x,
                    h_scroll,
                    base_voffset,
                    scanline,
                )
            } else {
                (
                    x.wrapping_add(h_scroll),
                    scanline.wrapping_add(base_voffset),
                )
            };

            // Cached modes index the prebuilt row by absolute tile
            // column; the others fetch directly. Both produce the same
            // entry, `build_tilemap_row` is built from this very
            // function (see `BgTilemapRowCache`).
            let entry = match cached_row {
                Some(ref entries) => entries[((screen_x & mask_x) >> 3) as usize],
                None => fetch_bg_tilemap_entry(
                    self.vram(),
                    screen_addr,
                    screen_size,
                    screen_x,
                    screen_y,
                    hires,
                    tile16,
                ),
            };

            // A 16x16 entry names a 2x2 block of 8-px characters laid
            // out in VRAM as TL=N, TR=N+1, BL=N+16, BR=N+17 (the char
            // table is 16 tiles wide, so "down one tile" is +16). Pick
            // the quadrant from the low scroll bits: bit 3 of X selects
            // the right column, bit 3 of Y the bottom row, each
            // inverted by the corresponding flip so a flipped 16x16 tile
            // still reads its own four characters in mirror order. This
            // arm serves 16x16 BGs in every mode, hires included: the
            // 512-dot `screen_x` makes bit 3 land on the right 8-px half
            // exactly like `hires_adjacent_tile_offset`, while bit 3 of
            // the (interlace-doubled) `screen_y` adds the +16 the 8x8
            // hires arm below can't reach. The `else if hires` arm is the
            // 8x8 hires case: one entry per 16-px slot, neighbour tile for
            // the second half. Both mask back to 10 bits to mirror the
            // decoders' internal wrap.
            let effective_tile = if tile16 {
                let mut t = entry.tile;
                if ((screen_x & 8) != 0) ^ entry.flip_h {
                    t = t.wrapping_add(1);
                }
                if ((screen_y & 8) != 0) ^ entry.flip_v {
                    t = t.wrapping_add(16);
                }
                t & 0x03FF
            } else if hires {
                entry
                    .tile
                    .wrapping_add(hires_adjacent_tile_offset(screen_x, entry.flip_h))
                    & 0x03FF
            } else {
                entry.tile
            };

            let row_in_tile = (screen_y & 7) as u8;
            let pixels = match last_decode {
                Some((t, fh, fv, r, px))
                    if t == effective_tile
                        && fh == entry.flip_h
                        && fv == entry.flip_v
                        && r == row_in_tile =>
                {
                    px
                }
                _ => {
                    let decoded = match bpp {
                        2 => decode_tile_2bpp_row(
                            self.vram(),
                            effective_tile,
                            tile_data_addr,
                            row_in_tile,
                            entry.flip_h,
                            entry.flip_v,
                        ),
                        4 => decode_tile_4bpp_row(
                            self.vram(),
                            effective_tile,
                            tile_data_addr,
                            row_in_tile,
                            entry.flip_h,
                            entry.flip_v,
                        ),
                        _ => decode_tile_8bpp_row(
                            self.vram(),
                            effective_tile,
                            tile_data_addr,
                            row_in_tile,
                            entry.flip_h,
                            entry.flip_v,
                        ),
                    };
                    last_decode = Some((
                        effective_tile,
                        entry.flip_h,
                        entry.flip_v,
                        row_in_tile,
                        decoded,
                    ));
                    decoded
                }
            };

            // Skip into the tile to consume the fractional scroll
            // (`screen_x & 7` pixels into the decoded row).
            let skip = (screen_x & 7) as usize;
            for &pixel in &pixels[skip..8] {
                if x >= row_width {
                    break;
                }
                if pixel == 0 {
                    row[x as usize] = BgRowPixel {
                        color: 0,
                        opaque: false,
                        priority: entry.priority,
                    };
                } else {
                    let color = if direct_color {
                        direct_color_word(pixel, entry.palette)
                    } else {
                        let cgram_idx = cgram_index(palette_offset, entry.palette, pixel, bpp);
                        self.cgram()[cgram_idx as usize]
                    };
                    row[x as usize] = BgRowPixel {
                        color,
                        opaque: true,
                        priority: entry.priority,
                    };
                }
                x = x.wrapping_add(1);
            }
        }
    }

    /// Look up, or rebuild, the cached tilemap row for `bg_id` at
    /// tile row `ty`, returning a copy of the 64 entries. Invoked only
    /// for the cache-eligible modes (0/1/3). The returned array is
    /// `Copy`, so the `&mut self` borrow ends here and the caller can
    /// freely re-borrow `self.vram()` while indexing the row. See
    /// [`crate::ppu::BgTilemapRowCache`] for the coherence contract.
    fn cached_tilemap_row(
        &mut self,
        bg_id: usize,
        screen_addr: u32,
        screen_size: u8,
        ty: u16,
    ) -> [TilemapEntry; 64] {
        let generation = self.vram_generation;
        if !self.bg_tilemap_row_cache[bg_id].matches(generation, screen_addr, screen_size, ty) {
            let entries = build_tilemap_row(self.vram(), screen_addr, screen_size, ty);
            self.bg_tilemap_row_cache[bg_id].store(
                generation,
                screen_addr,
                screen_size,
                ty,
                entries,
            );
        }
        *self.bg_tilemap_row_cache[bg_id].entries()
    }

    /// Evaluate OAM for one visible `scanline` and update the STAT77
    /// ($213E) sprite range-over (bit 6) / time-over (bit 7) flags
    /// **without painting**.
    ///
    /// On hardware the sprite range/tile evaluation walks OAM on every
    /// active scanline regardless of whether OBJ is enabled on either
    /// screen ($212C/$212D bit 4) or whether the screen is in forced
    /// blank, only the *compositing* is gated (checked against
    /// snes9x, which evaluates them whatever the display state).
    /// Keeping this as a flag-only twin
    /// of [`Self::render_obj_row`]'s two passes lets the painted path
    /// stay display-gated while STAT77 reads back correctly when OBJ is
    /// off-screen or the line is blanked.
    ///
    /// The two passes use the identical range/tile-cap logic as
    /// `render_obj_row` (33rd sprite -> range-over, 35th tile column ->
    /// time-over, with the same off-screen tile-column skip) so the flags
    /// agree exactly with what the painted path would have reported.
    fn eval_obj_overflow(&mut self, scanline: u16) {
        let line = scanline.wrapping_sub(1);
        let obj_interlace = self.obj_interlace;
        let base_size = self.obj_base_size;

        // Pass 1: range eval, gather up to 32 in-range sprites; the
        // 33rd sets range-over.
        let mut items: [u8; 32] = [0xFF; 32];
        let mut item_count = 0usize;
        for i in 0..128usize {
            let idx = (usize::from(self.oam_first_sprite) + i) & 0x7F;
            let s = sprite::parse_sprite(self.oam(), idx, base_size, obj_interlace);
            if !sprite::sprite_on_scanline(&s, line, obj_interlace) {
                continue;
            }
            if item_count >= 32 {
                self.sprite_range_over = true;
                break;
            }
            items[item_count] = idx as u8;
            item_count += 1;
        }

        // Pass 2: tile-budget eval, count 8px tile columns in the same
        // reverse order as the painter; the 35th sets time-over. The
        // off-screen 256..512 skip mirrors `render_obj_row` so the tile
        // count matches the painted path exactly.
        let mut tile_count = 0usize;
        'items: for i in (0..item_count).rev() {
            let idx = usize::from(items[i]);
            let s = sprite::parse_sprite(self.oam(), idx, base_size, obj_interlace);
            let tiles_wide = u16::from(s.width >> 3);
            for tx in 0..tiles_wide {
                let sx0 = (s.x + (tx << 3)) & 0x1FF;
                if s.x != 256 && sx0 >= 256 && (sx0 + 7) < 512 {
                    continue;
                }
                if tile_count >= 34 {
                    self.sprite_time_over = true;
                    break 'items;
                }
                tile_count += 1;
            }
        }
    }

    /// Evaluate OAM for `scanline` and rasterise the visible sprites
    /// into `obj_row`.
    ///
    /// Two passes, the hardware's evaluation and its painting (checked
    /// against bsnes-plus):
    ///
    /// 1. **Range eval**: walk all 128 sprites starting at
    ///    [`Ppu::oam_first_sprite`]; gather up to 32 that fall on the
    ///    line. The 33rd onward are dropped (capped at 32).
    /// 2. **Tile build + raster**: replay the gathered sprites in
    ///    reverse so the highest-priority sprite paints last (and thus
    ///    wins on overlap). Each 8-px tile column consumes one of the
    ///    34 per-scanline tile slots; the 35th caps the loop and drops
    ///    the remaining (higher-priority) tiles, exactly the hardware
    ///    "too many sprites vanish" quirk.
    ///
    /// This path only *paints* and so runs solely when OBJ is displayed
    /// (not forced-blank, OBJ enabled on a screen). The STAT77 ($213E)
    /// range/time-overflow flags are owned by [`Self::eval_obj_overflow`],
    /// which runs every active scanline regardless of display state, so
    /// the caps here intentionally leave the flags alone.
    ///
    /// `scanline` is the 1-indexed vcounter; the OBJ Y comparison runs
    /// against `scanline - 1` so a sprite with OAM Y = N lands with its
    /// top row at output row N (the one-line OBJ pipeline latency is
    /// already absorbed by the vcounter being 1-indexed, see the
    /// Mode 7 note in [`Self::render_mode7_rows`] for the same offset
    /// reasoning).
    fn render_obj_row(&mut self, scanline: u16, obj_row: &mut ObjRowBuffer) {
        *obj_row = [ObjRowPixel::TRANSPARENT; NORMAL_SCANLINE_WIDTH];

        let line = scanline.wrapping_sub(1);
        let obj_interlace = self.obj_interlace;
        let field = self.field;
        let base_size = self.obj_base_size;

        // Pass 1: range eval.
        let mut items: [u8; 32] = [0xFF; 32];
        let mut item_count = 0usize;
        for i in 0..128usize {
            let idx = (usize::from(self.oam_first_sprite) + i) & 0x7F;
            let s = sprite::parse_sprite(self.oam(), idx, base_size, obj_interlace);
            if !sprite::sprite_on_scanline(&s, line, obj_interlace) {
                continue;
            }
            if item_count >= 32 {
                // Cap the painted set at 32; the range-over flag is set
                // by `eval_obj_overflow` (runs even when OBJ is off).
                break;
            }
            items[item_count] = idx as u8;
            item_count += 1;
        }

        // The active-scan OAM read pointer tracks the last in-range
        // sprite's high-table word.
        if item_count > 0 {
            self.oam_iaddr = 0x0200 + (u16::from(items[item_count - 1]) >> 2);
        }

        // Pass 2: reverse tile build + raster.
        let mut tile_count = 0usize;
        'items: for i in (0..item_count).rev() {
            let idx = usize::from(items[i]);
            let s = sprite::parse_sprite(self.oam(), idx, base_size, obj_interlace);
            let y_row = sprite::sprite_row_in_sprite(&s, line, field, obj_interlace);
            let row_in_tile = (y_row & 7) as usize;
            let tiles_wide = u16::from(s.width >> 3);

            for tx in 0..tiles_wide {
                let sx0 = (s.x + (tx << 3)) & 0x1FF;
                // Skip tile columns that sit entirely in the off-screen
                // 256..512 band (unless x == 256, which loads them all).
                if s.x != 256 && sx0 >= 256 && (sx0 + 7) < 512 {
                    continue;
                }
                if tile_count >= 34 {
                    // Cap painting at 34 tile columns (the "too many
                    // sprites vanish" quirk); the time-over flag is set
                    // by `eval_obj_overflow`.
                    break 'items;
                }
                tile_count += 1;

                let tile_number = sprite::sprite_tile_number(
                    self.obj_tiledata_addr,
                    self.obj_name_select,
                    s.character,
                    s.nameselect,
                    s.hflip,
                    s.width,
                    tx as u8,
                    y_row,
                );
                let pixels = self.cached_sprite_tile(tile_number);
                for px in 0..8u16 {
                    let sx = (sx0 + px) & 0x1FF;
                    if sx >= 256 {
                        continue;
                    }
                    let col = if s.hflip {
                        7 - px as usize
                    } else {
                        px as usize
                    };
                    let pixel = pixels[row_in_tile * 8 + col];
                    if pixel != 0 {
                        let cgram_idx = 128 + usize::from(s.palette) * 16 + usize::from(pixel);
                        obj_row[sx as usize] = ObjRowPixel {
                            color: self.cgram()[cgram_idx],
                            opaque: true,
                            prio: s.priority,
                            palette: s.palette,
                        };
                    }
                }
            }
        }
    }

    /// Look up, or decode, a 4bpp sprite tile, returning all eight
    /// rows (`[u8; 64]`, row-major). The returned array is `Copy`, so
    /// the `&mut self` borrow ends here and the caller can re-borrow
    /// `self.cgram()` while reading pixels. Coherence rides
    /// [`Ppu::vram_generation`], the same contract as
    /// [`Self::cached_tilemap_row`].
    fn cached_sprite_tile(&mut self, tile_number: u16) -> [u8; 64] {
        let generation = self.vram_generation;
        if !self.sprite_tile_cache.matches(tile_number, generation) {
            let base_byte = u32::from(tile_number) << 5;
            let mut pixels = [0u8; 64];
            for row in 0..8usize {
                let decoded =
                    decode_tile_4bpp_row(self.vram(), 0, base_byte, row as u8, false, false);
                pixels[row * 8..row * 8 + 8].copy_from_slice(&decoded);
            }
            self.sprite_tile_cache
                .store(tile_number, generation, pixels);
        }
        *self.sprite_tile_cache.pixels(tile_number)
    }

    /// Fill the Mode 7 BG row buffers from the affine plane.
    ///
    /// Mode 7 replaces the four tile BGs with one affine layer
    /// (BG1). With EXTBG (`$2133` bit 6) the same sampled byte also
    /// feeds BG2: its low 7 bits are the colour index and bit 7 is
    /// the priority bit. Only `main_rows` is filled, Mode 7 is
    /// non-hires so the sub plane never reaches the composite.
    ///
    /// The matrix math is the hardware's as Anomie's Mode 7 notes give
    /// it (checked against bsnes-plus). Coefficients A-D are 8.8 fixed
    /// point; the `& !63` truncation on the three base terms of each
    /// axis is the chip's (the `(cx << 8)` centre term and the
    /// per-pixel `A*xf` / `C*xf` terms are not truncated).
    /// [`sign_extend_13`] reads the centre / scroll registers;
    /// [`mode7_clip`] is the separate clip applied to the
    /// scroll-minus-centre differences.
    fn render_mode7_rows(&self, scanline: u16, main_rows: &mut [BgRowBuffer; 4]) {
        let a = self.m7a as i16 as i32;
        let b = self.m7b as i16 as i32;
        let c = self.m7c as i16 as i32;
        let d = self.m7d as i16 as i32;
        let cx = sign_extend_13(self.m7x);
        let cy = sign_extend_13(self.m7y);
        let hofs = sign_extend_13(self.mode7_hoffset);
        let vofs = sign_extend_13(self.mode7_voffset);

        // World line = the 1-indexed SNES vcounter (1 on the first
        // visible scanline).
        // The tile renderer feeds `scanline` straight into `screen_y`
        // for the same reason, so Mode 7 must NOT subtract 1 or it
        // would sit one row above every tile-mode BG. Vertical flip
        // mirrors around 255, matching the hardware.
        let line = i32::from(scanline);
        let y = if self.mode7_vflip { 255 - line } else { line };

        let sx0 = mode7_clip(hofs - cx);
        let sy0 = mode7_clip(vofs - cy);
        let base_x = (a.wrapping_mul(sx0) & !63)
            .wrapping_add(b.wrapping_mul(sy0) & !63)
            .wrapping_add(b.wrapping_mul(y) & !63)
            .wrapping_add(cx << 8);
        let base_y = (c.wrapping_mul(sx0) & !63)
            .wrapping_add(d.wrapping_mul(sy0) & !63)
            .wrapping_add(d.wrapping_mul(y) & !63)
            .wrapping_add(cy << 8);

        let extbg = self.mode7_extbg;
        let direct = self.direct_color;
        let bg1_main = self.bg_main_enable[0];
        let bg2_main = extbg && self.bg_main_enable[1];

        // Two independent row buffers (BG1 + EXTBG BG2) are written at the
        // same x; zipping both through iterators would obscure the mode-7
        // coordinate math for no gain.
        #[allow(clippy::needless_range_loop)]
        for x in 0..NORMAL_SCANLINE_WIDTH {
            let xf = if self.mode7_hflip {
                255 - x as i32
            } else {
                x as i32
            };
            let px = base_x.wrapping_add(a.wrapping_mul(xf)) >> 8;
            let py = base_y.wrapping_add(c.wrapping_mul(xf)) >> 8;

            let Some(byte) = mode7_sample(self.vram(), px, py, self.mode7_repeat) else {
                continue;
            };

            // BG1: the full 8-bit value indexes CGRAM, or runs
            // through the direct-colour decode when CGWSEL bit 0 is
            // set. Pixel 0 is transparent.
            if bg1_main && byte != 0 {
                let color = if direct {
                    direct_color_word(byte, 0)
                } else {
                    self.cgram()[byte as usize]
                };
                main_rows[0][x] = BgRowPixel {
                    color,
                    opaque: true,
                    priority: false,
                };
            }

            // BG2 (EXTBG): low 7 bits = colour index, bit 7 =
            // priority. Direct colour never applies, BG2 has only
            // 128 colours.
            if bg2_main {
                let idx = byte & 0x7F;
                if idx != 0 {
                    main_rows[1][x] = BgRowPixel {
                        color: self.cgram()[idx as usize],
                        opaque: true,
                        priority: byte & 0x80 != 0,
                    };
                }
            }
        }
    }

    /// Apply both horizontal + vertical mosaic for one BG.
    ///
    /// The hardware's V-mosaic counter starts at 1 on
    /// scanline 1 and is decremented every line; when it hits 0 it
    /// resets to `mosaic_size` and the current
    /// row becomes the new V-mosaic source. The equivalent
    /// stateless formulation is `(scanline - 1) % N == 0`, fresh
    /// at lines 1, 1+N, 1+2N, ...; replayed everywhere else. The
    /// `scanline % N == 0` formulation would mis-classify line 1
    /// (the first visible line) as "replay" and pull the empty
    /// previous-row buffer through, which manifests as black BGs
    /// for the first mosaic block of the frame.
    ///
    /// The size N is `(($2106 >> 4) & 0xF)
    /// + 1`, stored already-adjusted in [`Ppu::mosaic_size`] (range
    /// `1..=16`). Size 1 is a no-op even when the enable bit is set.
    ///
    /// `width` lets the same path serve both 256-wide (non-hires)
    /// and 512-wide (hires) rows. The vertical replay copies the
    /// full 512-wide previous-row buffer regardless so the cold
    /// tail stays consistent if the mode flips mid-frame.
    fn apply_mosaic_for_bg(
        &mut self,
        bg_id: usize,
        scanline: u16,
        row: &mut BgRowBuffer,
        width: usize,
    ) {
        if !self.bg_mosaic_enable[bg_id] || self.mosaic_size <= 1 {
            return;
        }
        let n = self.mosaic_size as u16;
        let is_fresh_row = scanline.wrapping_sub(1).is_multiple_of(n);
        if !is_fresh_row {
            *row = self.previous_bg_rows[bg_id];
            return;
        }
        // H-mosaic on the fresh row: replicate the leftmost pixel
        // of each N-wide block over its siblings.
        let n_us = n as usize;
        let mut x = 0usize;
        while x < width {
            let src = row[x];
            let end = (x + n_us).min(width);
            for slot in &mut row[x..end] {
                *slot = src;
            }
            x += n_us;
        }
    }

    /// Convert a composited BGR15 row to RGBA8 with brightness and
    /// write it into the host framebuffer at `fb_row` (0-indexed
    /// host row, already adjusted for the `scanline -> scanline-1`
    /// SNES-to-host mapping by the caller). Only the leading 256
    /// columns are written, `fb.width` clamps further if the host
    /// allocated less.
    fn write_scanline_normal(
        &self,
        fb_row: u32,
        row: &[u16; MAX_SCANLINE_WIDTH],
        fb: &mut Framebuffer,
    ) {
        let stride = (fb.width * 4) as usize;
        let base = stride * fb_row as usize;
        let visible_w = (fb.width as usize).min(NORMAL_SCANLINE_WIDTH);
        let brightness = self.display_brightness;
        for (x, &color) in row.iter().take(visible_w).enumerate() {
            let [r, g, b] = bgr15_to_rgb8_with_brightness(color, brightness);
            let p = base + x * 4;
            fb.data[p] = r;
            fb.data[p + 1] = g;
            fb.data[p + 2] = b;
            fb.data[p + 3] = 0xFF;
        }
    }

    /// Hires writeout: emit the fully-composited 512-pixel line where
    /// even columns already carry the sub-screen colour and odd columns
    /// the main-screen colour (see [`Ppu::render_output_line`]).
    /// `line[k]` maps straight to host
    /// display column `k`, there is no 2i/2i+1 doubling because the
    /// compositor already produced every one of the 512 columns from
    /// its own BG position.
    ///
    /// A full 512-wide host writes the whole line. A 256-wide host
    /// can't fit it, so we write the leading 256 columns (a
    /// degraded-but-sane default for a non-hires-aware host; the
    /// CLI/WASM hosts are 512-wide).
    fn write_scanline_hires(
        &self,
        fb_row: u32,
        line: &[u16; MAX_SCANLINE_WIDTH],
        fb: &mut Framebuffer,
    ) {
        let stride = (fb.width * 4) as usize;
        let base = stride * fb_row as usize;
        let brightness = self.display_brightness;
        let visible_w = (fb.width as usize).min(MAX_SCANLINE_WIDTH);
        for (x, &color) in line.iter().take(visible_w).enumerate() {
            let [r, g, b] = bgr15_to_rgb8_with_brightness(color, brightness);
            let p = base + x * 4;
            fb.data[p] = r;
            fb.data[p + 1] = g;
            fb.data[p + 2] = b;
            fb.data[p + 3] = 0xFF;
        }
    }

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
    pub(crate) fn finalize_frame(&self, fb: &mut Framebuffer) {
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

/// `true` if BG `bg_id` participates in the visible composite under
/// the given mode. BG3 in Modes 2 / 4 / 6 is used as an offset
/// table only and **does not** draw pixels.
fn bg_visible_in_mode(bgmode: u8, bg_id: usize) -> bool {
    matches!(
        (bgmode, bg_id),
        (0, 0..=3) | (1, 0..=2) | (2, 0..=1) | (3, 0..=1) | (4, 0..=1) | (5, 0..=1) | (6, 0)
    )
}

/// Bits per pixel for `bg_id` under the given mode. Pre-condition:
/// the caller already filtered via [`bg_visible_in_mode`].
pub(crate) fn bpp_for_bg(bgmode: u8, bg_id: usize) -> u8 {
    match (bgmode, bg_id) {
        (0, _) => 2,
        (1, 0) | (1, 1) => 4,
        (1, 2) => 2,
        (2, _) => 4,
        (3, 0) => 8,
        (3, 1) => 4,
        (4, 0) => 8,
        (4, 1) => 2,
        (5, 0) => 4,
        (5, 1) => 2,
        (6, 0) => 4,
        _ => 0, // unreachable given bg_visible_in_mode
    }
}

/// `true` for the hires bg modes (5 and 6). Drives the 512-pixel
/// rendering path and the active display-size reporter.
#[inline]
#[must_use]
pub(crate) fn is_hires_mode(bgmode: u8) -> bool {
    bgmode == 5 || bgmode == 6
}

/// Palette-region base for `bg_id` under the given mode. Mode 0
/// splits the 256-colour CGRAM into four 32-colour quadrants
/// (BG1=0, BG2=32, BG3=64, BG4=96); every other mode starts at 0
/// for every BG.
pub(crate) fn palette_offset_for_bg(bgmode: u8, bg_id: usize) -> u8 {
    if bgmode == 0 {
        (bg_id as u8) * 32
    } else {
        0
    }
}

/// CGRAM byte index for `pixel != 0`. Pre-condition: caller has
/// already filtered the transparent (pixel == 0) case.
///
/// 8bpp pixels bypass the per-tile palette_number: on the chip the 3
/// palette bits collide with the upper colour indices and the index
/// wraps at 256 (checked against bsnes-plus).
fn cgram_index(palette_offset: u8, palette_number: u8, pixel: u8, bpp: u8) -> u8 {
    match bpp {
        2 => palette_offset
            .wrapping_add(palette_number * 4)
            .wrapping_add(pixel),
        4 => palette_offset
            .wrapping_add(palette_number * 16)
            .wrapping_add(pixel),
        _ => pixel, // 8bpp ignores both palette_offset and palette_number
    }
}

/// Mode 3/4 BG1 direct colour: the 8-bit pixel is `BBGGGRRR` and the
/// tilemap entry's palette number lends one more bit to each channel,
/// below the pixel's; the lowest bit of every channel stays zero. So
/// red and green have four effective bits, blue three, out of the
/// five each channel has in a CGRAM colour.
#[must_use]
pub(crate) fn direct_color_word(pixel: u8, palette_number: u8) -> u16 {
    let pixel = u16::from(pixel);
    let palette = u16::from(palette_number);
    let red = ((pixel & 7) << 2) | ((palette & 1) << 1);
    let green = (((pixel >> 3) & 7) << 2) | (((palette >> 1) & 1) << 1);
    let blue = ((pixel >> 6) << 3) | (((palette >> 2) & 1) << 2);
    red | (green << 5) | (blue << 10)
}

/// One entry in a per-mode priority list: either a BG layer (split by
/// its per-pixel priority bit) or the OBJ layer at a given 2-bit
/// sprite priority. The compositor walks the list top-to-bottom and
/// keeps the first opaque pixel per column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layer {
    /// A background layer, gated on its high/low priority bit.
    Bg { id: u8, high: bool },
    /// The sprite layer at one of the four OBJ priorities.
    Obj { prio: u8 },
}

impl Layer {
    const fn bg(id: u8, high: bool) -> Self {
        Self::Bg { id, high }
    }
    const fn obj(prio: u8) -> Self {
        Self::Obj { prio }
    }
}

/// Per-mode priority list in highest-to-lowest order, interleaving the
/// four OBJ priorities with the BG layers as fullsnes's priority table
/// gives it (checked against bsnes-plus). The BG layers keep their relative
/// order; the four `Obj` slots sit in between.
///
/// Mode 1's `bg3_priority` (`$2105` bit 3) lifts BG3's high-prio
/// pixels above everything (including OBJ3), used by menu overlays
/// that draw text on top. Modes 2-5 share the "two BG" interleave;
/// the hires composite reuses the same table for both planes.
///
/// Mode 7 overloads `bg3_priority` to carry the EXTBG flag
/// (`$2133` bit 6). Without EXTBG the plane is BG1 (sandwiched by the
/// four OBJ priorities); with it, BG2 splits by its per-pixel priority
/// bit around BG1.
fn priority_layers(bgmode: u8, bg3_priority: bool) -> &'static [Layer] {
    static MODE0: &[Layer] = &[
        Layer::obj(3),
        Layer::bg(0, true),
        Layer::bg(1, true),
        Layer::obj(2),
        Layer::bg(0, false),
        Layer::bg(1, false),
        Layer::obj(1),
        Layer::bg(2, true),
        Layer::bg(3, true),
        Layer::obj(0),
        Layer::bg(2, false),
        Layer::bg(3, false),
    ];
    static MODE1_NORMAL: &[Layer] = &[
        Layer::obj(3),
        Layer::bg(0, true),
        Layer::bg(1, true),
        Layer::obj(2),
        Layer::bg(0, false),
        Layer::bg(1, false),
        Layer::obj(1),
        Layer::bg(2, true),
        Layer::obj(0),
        Layer::bg(2, false),
    ];
    static MODE1_BG3_LIFTED: &[Layer] = &[
        Layer::bg(2, true),
        Layer::obj(3),
        Layer::bg(0, true),
        Layer::bg(1, true),
        Layer::obj(2),
        Layer::bg(0, false),
        Layer::bg(1, false),
        Layer::obj(1),
        Layer::obj(0),
        Layer::bg(2, false),
    ];
    static MODE_TWO_BG: &[Layer] = &[
        Layer::obj(3),
        Layer::bg(0, true),
        Layer::obj(2),
        Layer::bg(1, true),
        Layer::obj(1),
        Layer::bg(0, false),
        Layer::obj(0),
        Layer::bg(1, false),
    ];
    static MODE_ONE_BG: &[Layer] = &[
        Layer::obj(3),
        Layer::bg(0, true),
        Layer::obj(2),
        Layer::obj(1),
        Layer::bg(0, false),
        Layer::obj(0),
    ];
    static MODE7_NORMAL: &[Layer] = &[
        Layer::obj(3),
        Layer::obj(2),
        Layer::obj(1),
        Layer::bg(0, false),
        Layer::obj(0),
    ];
    static MODE7_EXTBG: &[Layer] = &[
        Layer::obj(3),
        Layer::obj(2),
        Layer::bg(1, true),
        Layer::obj(1),
        Layer::bg(0, false),
        Layer::obj(0),
        Layer::bg(1, false),
    ];
    match bgmode {
        0 => MODE0,
        1 => {
            if bg3_priority {
                MODE1_BG3_LIFTED
            } else {
                MODE1_NORMAL
            }
        }
        2..=5 => MODE_TWO_BG,
        6 => MODE_ONE_BG,
        7 => {
            if bg3_priority {
                MODE7_EXTBG
            } else {
                MODE7_NORMAL
            }
        }
        _ => &[],
    }
}

/// Expand a 5-bit channel to 8 bits and apply brightness inline.
/// `(channel5 << 3) | (channel5 >> 2)` is the usual 5->8 expansion,
/// then `(x * (brightness + 1)) >> 4` scales to the
/// 16-step brightness ramp.
#[inline]
fn expand_channel_with_brightness(channel5: u8, brightness: u8) -> u8 {
    let c5 = channel5 & 0x1F;
    let c8 = (c5 << 3) | (c5 >> 2);
    ((u16::from(c8) * (u16::from(brightness) + 1)) >> 4) as u8
}

#[inline]
fn bgr15_to_rgb8_with_brightness(bgr15: u16, brightness: u8) -> [u8; 3] {
    let r5 = (bgr15 & 0x1F) as u8;
    let g5 = ((bgr15 >> 5) & 0x1F) as u8;
    let b5 = ((bgr15 >> 10) & 0x1F) as u8;
    [
        expand_channel_with_brightness(r5, brightness),
        expand_channel_with_brightness(g5, brightness),
        expand_channel_with_brightness(b5, brightness),
    ]
}

/// Compose the 4 BG row buffers plus the OBJ row into the first
/// `width` columns of one screen's [`ScreenRow`], recording per column
/// the winning source layer and the OBJ colour-math exemption alongside
/// the colour.
///
/// The result is the SNES "pixel cache" for a single screen:
/// both the non-hires output and (per screen) the hires interleave
/// read it. Each enabled layer is gated by its window-clip table:
/// `clips[id][x]` (or `clips[4][x]` for OBJ) skips column `x` when the
/// layer's TMW/TSW bit set it inside the window region. Unwritten
/// columns fall through to the backdrop (source [`PixelSource::Backdrop`]).
///
/// `obj_enabled` gates the OBJ slots, pass `obj_main_enable` for the
/// main composite and `obj_sub_enable` for the sub composite. OBJ
/// colour-math exemption is `palette < 4` (CGRAM index < 192).
///
/// Pure helper so it serves the main and sub composites without
/// re-borrowing `Ppu` state.
///
/// `width` is the output column count (256 or 512); `hires` selects
/// the 512-wide BG sampling where the 256-dot OBJ/clip tables are
/// indexed by `x >> 1`.
#[allow(clippy::too_many_arguments)]
fn composite_screen(
    rows: &[BgRowBuffer; 4],
    obj_row: &ObjRowBuffer,
    bgmode: u8,
    bg3_priority: bool,
    obj_enabled: bool,
    clips: &LayerClips,
    backdrop: u16,
    width: usize,
    hires: bool,
) -> ScreenRow {
    // Window-clip and OBJ tables are always 256 dots wide. In hires
    // each lo-res dot covers two output columns, so they're indexed by
    // `x >> 1`; the BG rows are full-resolution (512 in hires) and are
    // indexed by `x` directly.
    let lo = |x: usize| if hires { x >> 1 } else { x };
    let mut out = [ScreenPixel {
        color: backdrop,
        source: PixelSource::Backdrop,
        color_exempt: false,
    }; MAX_SCANLINE_WIDTH];
    let mut written = [false; MAX_SCANLINE_WIDTH];
    for layer in priority_layers(bgmode, bg3_priority) {
        match *layer {
            Layer::Bg { id, high } => {
                let row = &rows[id as usize];
                let clip = &clips[id as usize];
                for x in 0..width {
                    if written[x] || clip[lo(x)] {
                        continue;
                    }
                    if row[x].opaque && row[x].priority == high {
                        out[x] = ScreenPixel {
                            color: row[x].color,
                            source: PixelSource::Bg(id),
                            color_exempt: false,
                        };
                        written[x] = true;
                    }
                }
            }
            Layer::Obj { prio } => {
                if !obj_enabled {
                    continue;
                }
                let clip = &clips[4];
                for x in 0..width {
                    if written[x] || clip[lo(x)] {
                        continue;
                    }
                    let o = &obj_row[lo(x)];
                    if o.opaque && o.prio == prio {
                        out[x] = ScreenPixel {
                            color: o.color,
                            source: PixelSource::Obj,
                            color_exempt: o.palette < 4,
                        };
                        written[x] = true;
                    }
                }
            }
        }
    }
    out
}

/// Write an all-zero RGB row with full alpha (the user-facing
/// "black" the SNES draws during forced blank or brightness 0).
/// `fb_row` is the 0-indexed host row, already SNES-to-host
/// adjusted by the caller. `output_width` selects how many of the
/// framebuffer's columns to blacken, the cold tail past the
/// active display width is left untouched: nothing shows it, since
/// [`Ppu::finalize_frame`] widens a lo-res row to 512 in a hires
/// frame and the host crops a lo-res frame to 256.
fn write_black_scanline(fb_row: u32, fb: &mut Framebuffer, output_width: usize) {
    let stride = (fb.width * 4) as usize;
    let base = stride * fb_row as usize;
    let visible_w = (fb.width as usize).min(output_width);
    for x in 0..visible_w {
        let p = base + x * 4;
        fb.data[p] = 0;
        fb.data[p + 1] = 0;
        fb.data[p + 2] = 0;
        fb.data[p + 3] = 0xFF;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgr15_zero_is_black() {
        assert_eq!(bgr15_to_rgb8_with_brightness(0, 15), [0, 0, 0]);
    }

    #[test]
    fn bgr15_max_at_full_brightness_is_white() {
        // 0x7FFF = all five-bit channels at 0x1F (31). 5->8 expansion:
        // (31 << 3) | (31 >> 2) = 248 | 7 = 255. Brightness 15 is no
        // attenuation -> 255.
        assert_eq!(
            bgr15_to_rgb8_with_brightness(0x7FFF, 15),
            [0xFF, 0xFF, 0xFF]
        );
    }

    #[test]
    fn bgr15_zero_brightness_is_black() {
        assert_eq!(bgr15_to_rgb8_with_brightness(0x7FFF, 0), [0x0F, 0x0F, 0x0F]);
        // Note: brightness 0 != forced black at the renderer level,
        // the caller has its own short-circuit. This test pins the
        // arithmetic: (255 * 1) >> 4 = 15.
    }

    #[test]
    fn bgr15_red_channel_isolated() {
        // pure red, R5 = 0x1F, others 0
        let rgb = bgr15_to_rgb8_with_brightness(0x001F, 15);
        assert_eq!(rgb, [0xFF, 0, 0]);
    }

    #[test]
    fn cgram_index_2bpp_uses_palette_times_4() {
        assert_eq!(cgram_index(0, 3, 2, 2), 3 * 4 + 2);
        assert_eq!(cgram_index(32, 3, 2, 2), 32 + 14); // Mode 0 BG2 offset
    }

    #[test]
    fn cgram_index_4bpp_uses_palette_times_16() {
        assert_eq!(cgram_index(0, 5, 9, 4), 5 * 16 + 9);
    }

    #[test]
    fn cgram_index_8bpp_ignores_palette_number() {
        assert_eq!(cgram_index(0, 7, 200, 8), 200);
        assert_eq!(cgram_index(255, 7, 200, 8), 200);
    }

    #[test]
    fn direct_color_word_zero_is_black() {
        assert_eq!(direct_color_word(0, 0), 0);
    }

    #[test]
    fn direct_color_word_red_bits_land_in_low_nibble() {
        // pixel = 0x07 (R bits = 111) -> R5 from pixel goes to bits 4-2.
        // palette_number = 0 -> no LSB contribution.
        // Result R5 = 0b11100 = 0x1C, G5/B5 = 0.
        assert_eq!(direct_color_word(0x07, 0), 0x001C);
    }

    #[test]
    fn direct_color_word_full_white_with_max_palette() {
        // pixel = 0xFF (BBGGGRRR all 1), palette_number = 7 (bgr all 1)
        let actual = direct_color_word(0xFF, 7);
        // R5 = (RRR << 2) | r_bit = (0b111 << 2) | 1 << 1 = 0b11110 = 0x1E. Wait,
        // let me redo via formula. Output:
        //   (0xFF << 7) & 0x6000 = 0x7F80 & 0x6000 = 0x6000
        //   tile_word = 7 << 10 = 0x1C00; (0x1C00) & 0x1000 = 0x1000
        //   (0xFF << 4) & 0x0380 = 0xFF0 & 0x380 = 0x380
        //   (0x1C00 >> 5) & 0x0040 = 0xE0 & 0x40 = 0x40
        //   (0xFF << 2) & 0x001C = 0x3FC & 0x1C = 0x1C
        //   (0x1C00 >> 9) & 0x0002 = 0xE & 0x02 = 0x02
        // Sum = 0x6000 | 0x1000 | 0x380 | 0x40 | 0x1C | 0x02 = 0x73DE
        assert_eq!(actual, 0x73DE);
    }

    #[test]
    fn priority_layers_mode0_interleaves_obj_with_four_bgs() {
        let l = priority_layers(0, false);
        assert_eq!(l.len(), 12);
        // OBJ3 sits at the very top, BG4-low at the very bottom.
        assert_eq!(l[0], Layer::obj(3));
        assert_eq!(l[1], Layer::bg(0, true));
        assert_eq!(l[11], Layer::bg(3, false));
        // The four OBJ priorities are all present, descending.
        let objs: Vec<u8> = l
            .iter()
            .filter_map(|x| match x {
                Layer::Obj { prio } => Some(*prio),
                Layer::Bg { .. } => None,
            })
            .collect();
        assert_eq!(objs, [3, 2, 1, 0]);
    }

    #[test]
    fn priority_layers_mode1_default() {
        let l = priority_layers(1, false);
        assert_eq!(
            l,
            [
                Layer::obj(3),
                Layer::bg(0, true),
                Layer::bg(1, true),
                Layer::obj(2),
                Layer::bg(0, false),
                Layer::bg(1, false),
                Layer::obj(1),
                Layer::bg(2, true),
                Layer::obj(0),
                Layer::bg(2, false),
            ]
        );
    }

    #[test]
    fn priority_layers_mode1_bg3_lifted() {
        let l = priority_layers(1, true);
        // BG3 high lifts above even OBJ3.
        assert_eq!(l[0], Layer::bg(2, true));
        assert_eq!(l[1], Layer::obj(3));
        assert_eq!(l[2], Layer::bg(0, true));
    }

    #[test]
    fn priority_layers_modes_2_3_4_5_only_two_bgs() {
        for mode in [2u8, 3, 4, 5] {
            let l = priority_layers(mode, false);
            // 2 BGs x 2 priorities + 4 OBJ slots = 8 entries.
            assert_eq!(l.len(), 8, "mode {mode}");
            assert!(l.iter().all(|x| match x {
                Layer::Bg { id, .. } => *id == 0 || *id == 1,
                Layer::Obj { .. } => true,
            }));
        }
    }

    #[test]
    fn priority_layers_mode6_single_bg_pair() {
        let l = priority_layers(6, false);
        assert_eq!(
            l,
            [
                Layer::obj(3),
                Layer::bg(0, true),
                Layer::obj(2),
                Layer::obj(1),
                Layer::bg(0, false),
                Layer::obj(0),
            ]
        );
    }

    #[test]
    fn bg_visible_table() {
        assert!(bg_visible_in_mode(0, 3));
        assert!(bg_visible_in_mode(1, 2));
        assert!(!bg_visible_in_mode(1, 3));
        assert!(!bg_visible_in_mode(2, 2)); // BG3 is offset table, not drawn
        assert!(!bg_visible_in_mode(4, 2));
        assert!(bg_visible_in_mode(3, 1));
        assert!(!bg_visible_in_mode(3, 2));
        // Hires modes
        assert!(bg_visible_in_mode(5, 0));
        assert!(bg_visible_in_mode(5, 1));
        assert!(!bg_visible_in_mode(5, 2));
        assert!(bg_visible_in_mode(6, 0));
        assert!(
            !bg_visible_in_mode(6, 1),
            "Mode 6 only renders BG1 (BG3 is offset source, BG2/4 unused)"
        );
        assert!(!bg_visible_in_mode(6, 2));
    }

    #[test]
    fn bpp_table_matches_modes() {
        assert_eq!(bpp_for_bg(0, 0), 2);
        assert_eq!(bpp_for_bg(1, 0), 4);
        assert_eq!(bpp_for_bg(1, 2), 2);
        assert_eq!(bpp_for_bg(3, 0), 8);
        assert_eq!(bpp_for_bg(3, 1), 4);
        assert_eq!(bpp_for_bg(4, 0), 8);
        assert_eq!(bpp_for_bg(4, 1), 2);
        // Hires modes
        assert_eq!(bpp_for_bg(5, 0), 4);
        assert_eq!(bpp_for_bg(5, 1), 2);
        assert_eq!(bpp_for_bg(6, 0), 4);
    }

    #[test]
    fn is_hires_mode_true_only_for_modes_5_and_6() {
        for m in 0..=4u8 {
            assert!(!is_hires_mode(m), "mode {m} must be non-hires");
        }
        assert!(is_hires_mode(5));
        assert!(is_hires_mode(6));
        assert!(!is_hires_mode(7));
    }

    #[test]
    fn composite_screen_writes_backdrop_on_empty_rows() {
        let rows = [[BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH]; 4];
        let obj = [ObjRowPixel::TRANSPARENT; NORMAL_SCANLINE_WIDTH];
        let out = composite_screen(
            &rows,
            &obj,
            1,
            false,
            true,
            &NO_LAYER_CLIPS,
            0x1234,
            NORMAL_SCANLINE_WIDTH,
            false,
        );
        for px in &out {
            assert_eq!(
                px.color, 0x1234,
                "every empty column falls through to backdrop"
            );
            assert_eq!(px.source, PixelSource::Backdrop);
            assert!(!px.color_exempt);
        }
    }

    #[test]
    fn composite_screen_obj_interleaves_by_priority() {
        // BG1 low-priority opaque pixel at column 0. In Mode 1 the
        // order is OBJ3, BG1H, BG2H, OBJ2, BG1L, ..., OBJ1, ..., OBJ0:
        // OBJ2 sits *above* BG1-low, OBJ0 sits *below* it.
        let mut rows = [[BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH]; 4];
        rows[0][0] = BgRowPixel {
            color: 0x1111,
            opaque: true,
            priority: false,
        };
        let obj_at = |prio: u8| {
            let mut obj = [ObjRowPixel::TRANSPARENT; NORMAL_SCANLINE_WIDTH];
            obj[0] = ObjRowPixel {
                color: 0x2222,
                opaque: true,
                prio,
                palette: 4,
            };
            obj
        };
        // Sprite priority 2 beats BG1-low.
        let high = composite_screen(
            &rows,
            &obj_at(2),
            1,
            false,
            true,
            &NO_LAYER_CLIPS,
            0,
            NORMAL_SCANLINE_WIDTH,
            false,
        );
        assert_eq!(high[0].color, 0x2222, "OBJ2 sits above BG1-low");
        assert_eq!(high[0].source, PixelSource::Obj);
        // Sprite priority 0 loses to BG1-low.
        let low = composite_screen(
            &rows,
            &obj_at(0),
            1,
            false,
            true,
            &NO_LAYER_CLIPS,
            0,
            NORMAL_SCANLINE_WIDTH,
            false,
        );
        assert_eq!(low[0].color, 0x1111, "OBJ0 sits below BG1-low");
        assert_eq!(low[0].source, PixelSource::Bg(0));
        // OBJ disabled -> BG1 shows regardless of sprite priority.
        let off = composite_screen(
            &rows,
            &obj_at(2),
            1,
            false,
            false,
            &NO_LAYER_CLIPS,
            0,
            NORMAL_SCANLINE_WIDTH,
            false,
        );
        assert_eq!(off[0].color, 0x1111, "disabled OBJ never composites");
    }

    #[test]
    fn composite_screen_obj_palette_exemption() {
        let rows = [[BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH]; 4];
        let obj_pal = |palette: u8| {
            let mut obj = [ObjRowPixel::TRANSPARENT; NORMAL_SCANLINE_WIDTH];
            obj[0] = ObjRowPixel {
                color: 0x2222,
                opaque: true,
                prio: 0,
                palette,
            };
            obj
        };
        // Palette 3 (CGRAM index < 192) -> exempt from colour math.
        let exempt = composite_screen(
            &rows,
            &obj_pal(3),
            1,
            false,
            true,
            &NO_LAYER_CLIPS,
            0,
            NORMAL_SCANLINE_WIDTH,
            false,
        );
        assert!(
            exempt[0].color_exempt,
            "OBJ palette 0-3 is colour-math exempt"
        );
        // Palette 4 (CGRAM index >= 192) -> eligible.
        let eligible = composite_screen(
            &rows,
            &obj_pal(4),
            1,
            false,
            true,
            &NO_LAYER_CLIPS,
            0,
            NORMAL_SCANLINE_WIDTH,
            false,
        );
        assert!(
            !eligible[0].color_exempt,
            "OBJ palette 4-7 takes colour math"
        );
    }

    #[test]
    fn composite_screen_window_clip_skips_layer() {
        // BG1 opaque at columns 0 and 1; clip BG1 only at column 0.
        let mut rows = [[BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH]; 4];
        for px in rows[0].iter_mut().take(2) {
            *px = BgRowPixel {
                color: 0x1111,
                opaque: true,
                priority: false,
            };
        }
        let obj = [ObjRowPixel::TRANSPARENT; NORMAL_SCANLINE_WIDTH];
        let mut clips = NO_LAYER_CLIPS;
        clips[0][0] = true; // BG1 clipped at column 0 only
        let out = composite_screen(
            &rows,
            &obj,
            1,
            false,
            true,
            &clips,
            0x0BAD,
            NORMAL_SCANLINE_WIDTH,
            false,
        );
        assert_eq!(out[0].color, 0x0BAD, "clipped column shows backdrop");
        assert_eq!(out[0].source, PixelSource::Backdrop);
        assert_eq!(out[1].color, 0x1111, "unclipped column shows BG1");
        assert_eq!(out[1].source, PixelSource::Bg(0));
    }

    #[test]
    fn palette_offset_mode0_splits_cgram_in_quadrants() {
        assert_eq!(palette_offset_for_bg(0, 0), 0);
        assert_eq!(palette_offset_for_bg(0, 1), 32);
        assert_eq!(palette_offset_for_bg(0, 2), 64);
        assert_eq!(palette_offset_for_bg(0, 3), 96);
    }

    #[test]
    fn palette_offset_zero_for_other_modes() {
        for mode in 1u8..=4 {
            for bg in 0..4 {
                assert_eq!(palette_offset_for_bg(mode, bg), 0, "mode {mode} bg {bg}");
            }
        }
    }

    // -- Mode 7 ------------------------------------------------

    #[test]
    fn priority_layers_mode7_normal_sandwiches_bg1_in_obj() {
        assert_eq!(
            priority_layers(7, false),
            [
                Layer::obj(3),
                Layer::obj(2),
                Layer::obj(1),
                Layer::bg(0, false),
                Layer::obj(0),
            ]
        );
    }

    #[test]
    fn priority_layers_mode7_extbg_orders_bg2_high_bg1_bg2_low() {
        assert_eq!(
            priority_layers(7, true),
            [
                Layer::obj(3),
                Layer::obj(2),
                Layer::bg(1, true),
                Layer::obj(1),
                Layer::bg(0, false),
                Layer::obj(0),
                Layer::bg(1, false),
            ]
        );
    }

    /// Identity-matrix Mode 7 PPU (A = D = 1.0 in 8.8 fixed point).
    fn mode7_identity_ppu() -> Ppu {
        let mut ppu = Ppu::new();
        ppu.bgmode = 7;
        ppu.m7a = 0x0100;
        ppu.m7d = 0x0100;
        ppu
    }

    #[test]
    fn render_mode7_identity_fills_bg1_from_vram() {
        let mut ppu = mode7_identity_ppu();
        ppu.bg_main_enable[0] = true;
        // Scanline 1 with the identity matrix samples world row 1
        // (the vcounter is 1-indexed), so screen (0,0) maps to
        // world (px=0, py=1): tilemap (0,0) = vram[0] low byte (tile
        // 0), and the character pixel (cx=0, cy=1) lives at
        // vram[tile*64 + 1*8 + 0] = vram[8] high byte = 0x42.
        ppu.vram[8] = 0x4200;
        ppu.cgram[0x42] = 0x1234;

        let mut rows = [[BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH]; 4];
        ppu.render_mode7_rows(1, &mut rows);

        assert!(rows[0][0].opaque);
        assert_eq!(rows[0][0].color, 0x1234);
        assert!(!rows[0][0].priority);
    }

    #[test]
    fn render_mode7_extbg_splits_byte_into_bg1_and_bg2() {
        let mut ppu = mode7_identity_ppu();
        ppu.mode7_extbg = true;
        ppu.bg_main_enable[0] = true;
        ppu.bg_main_enable[1] = true;
        // Scanline 1 samples world (0,1) -> tile 0 pixel (cx=0, cy=1)
        // at vram[8] high byte. Sampled byte 0x83: bit 7 set (high
        // priority), index 0x03.
        ppu.vram[8] = 0x8300;
        ppu.cgram[0x83] = 0xAAAA;
        ppu.cgram[0x03] = 0x5555;

        let mut rows = [[BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH]; 4];
        ppu.render_mode7_rows(1, &mut rows);

        // BG1 uses the full 8-bit value at low priority.
        assert!(rows[0][0].opaque);
        assert_eq!(rows[0][0].color, 0xAAAA);
        assert!(!rows[0][0].priority);
        // BG2 uses the low 7 bits as colour + bit 7 as priority.
        assert!(rows[1][0].opaque);
        assert_eq!(rows[1][0].color, 0x5555);
        assert!(rows[1][0].priority);
    }

    #[test]
    fn render_mode7_direct_color_bg1_bypasses_cgram() {
        let mut ppu = mode7_identity_ppu();
        ppu.direct_color = true;
        ppu.bg_main_enable[0] = true;
        // Scanline 1 samples world (0,1) -> tile 0 pixel (cx=0, cy=1)
        // at vram[8] high byte. Pixel byte 0x07 -> direct-colour
        // decode (palette_number 0).
        ppu.vram[8] = 0x0700;
        // Poison cgram[0x07] so an accidental lookup would show.
        ppu.cgram[0x07] = 0x7FFF;

        let mut rows = [[BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH]; 4];
        ppu.render_mode7_rows(1, &mut rows);

        assert!(rows[0][0].opaque);
        assert_eq!(rows[0][0].color, direct_color_word(0x07, 0));
    }

    // -- Tilemap-row cache -------------------------------------

    /// Mode 1 BG1 (4bpp) with two distinct tiles in tile-row 0
    /// (columns 0/1) and a third tile in tile-row 1 (column 0), plus a
    /// palette. `bg_voffset = 0xFFFF` so `scanline` maps to
    /// `screen_y = scanline - 1` (tile row 0 covers scanlines 1..=8).
    fn mode1_cache_ppu() -> Ppu {
        let mut ppu = Ppu::new();
        ppu.bgmode = 1;
        ppu.bg_screen_addr[0] = 0x0000; // tilemap word base 0
        ppu.bg_tiledata_addr[0] = 0x2000; // char word base 0x1000
        ppu.bg_screen_size[0] = 0;
        ppu.bg_voffset[0] = 0xFFFF;

        // Tilemap: (col0,row0)=tile1, (col1,row0)=tile2, (col0,row1)=tile2.
        ppu.vram[0] = 0x0001;
        ppu.vram[1] = 0x0002;
        ppu.vram[32] = 0x0002;

        // Tile 1 row 0: plane0 = 0xFF -> every pixel = colour index 1.
        ppu.vram[0x1000 + 16] = 0x00FF;
        // Tile 2 row 0: plane1 = 0xFF -> every pixel = colour index 2.
        ppu.vram[0x1000 + 32] = 0xFF00;

        ppu.cgram[1] = 0x1111;
        ppu.cgram[2] = 0x2222;
        ppu
    }

    /// Fill all 8 rows of a 4bpp character (char word base 0x1000) with
    /// the same plane0/1 and plane2/3 words, so its colour index is
    /// constant across every row and column, handy when a flip would
    /// otherwise pick an unpopulated row/column.
    fn fill_solid_4bpp_tile(ppu: &mut Ppu, tile: usize, plane01: u16, plane23: u16) {
        let base = 0x1000 + tile * 16;
        for r in 0..8 {
            ppu.vram[base + r] = plane01;
            ppu.vram[base + 8 + r] = plane23;
        }
    }

    #[test]
    fn render_bg_row_cache_indexes_columns_correctly() {
        let mut ppu = mode1_cache_ppu();
        let mut row = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        // Scanline 1 -> screen_y 0 -> tile row 0.
        ppu.render_bg_row(0, 1, &mut row, false);
        // Column 0 (x 0..7) = tile 1 = colour index 1 = cgram[1].
        assert!(row[0].opaque && row[7].opaque);
        assert_eq!(row[0].color, 0x1111);
        assert_eq!(row[7].color, 0x1111);
        // Column 1 (x 8..15) = tile 2 = colour index 2 = cgram[2].
        assert_eq!(row[8].color, 0x2222);
        assert_eq!(row[15].color, 0x2222);
    }

    #[test]
    fn render_bg_row_cache_hit_matches_miss_and_forced_rebuild() {
        let mut ppu = mode1_cache_ppu();
        let mut cold = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        let mut warm = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        let mut rebuilt = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];

        // First render builds the cache (cold miss).
        ppu.render_bg_row(0, 1, &mut cold, false);
        // Second render of the same scanline hits the warm cache.
        ppu.render_bg_row(0, 1, &mut warm, false);
        // Forcing a miss must reproduce the identical output.
        ppu.invalidate_tilemap_caches();
        ppu.render_bg_row(0, 1, &mut rebuilt, false);

        assert_eq!(&cold[..256], &warm[..256], "cache hit diverged from build");
        assert_eq!(
            &cold[..256],
            &rebuilt[..256],
            "forced rebuild diverged from build"
        );
    }

    #[test]
    fn render_bg_row_cache_rebuilds_on_tile_row_change() {
        let mut ppu = mode1_cache_ppu();
        let mut band0 = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        let mut band1 = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        // Scanline 1 -> tile row 0 -> column 0 = tile 1 = cgram[1].
        ppu.render_bg_row(0, 1, &mut band0, false);
        // Scanline 9 -> screen_y 8 -> tile row 1 -> column 0 = tile 2 =
        // cgram[2]. A stale ty-0 row would still report cgram[1].
        ppu.render_bg_row(0, 9, &mut band1, false);
        assert_eq!(band0[0].color, 0x1111);
        assert_eq!(band1[0].color, 0x2222, "ty rollover must rebuild the row");
    }

    #[test]
    fn render_bg_row_offset_mode_bypasses_cache() {
        let mut ppu = mode1_cache_ppu();
        ppu.bgmode = 2; // offset-per-tile -> direct fetch, no caching
        let mut row = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        ppu.render_bg_row(0, 1, &mut row, false);
        assert!(
            !ppu.bg_tilemap_row_cache[0].valid,
            "offset-per-tile must not populate the tilemap-row cache"
        );
    }

    /// Mode 1 BG1 with 16x16 tiles: a single tilemap entry must expand to
    /// the 2x2 block of 8-px characters TL=N, TR=N+1, BL=N+16, BR=N+17.
    /// Contra III's Konami logo needs it: BG1 there is a 16x16 layer
    /// that, (mis)read as 8x8, paints opaque garbage (a pink band + a
    /// white wash) over the BG2 logo instead of staying transparent.
    #[test]
    fn render_bg_row_tile16_selects_four_quadrants() {
        let mut ppu = Ppu::new();
        ppu.bgmode = 1;
        ppu.bg_screen_addr[0] = 0x0000;
        ppu.bg_tiledata_addr[0] = 0x2000; // char word base 0x1000
        ppu.bg_screen_size[0] = 0;
        ppu.bg_voffset[0] = 0xFFFF; // screen_y = scanline - 1
        ppu.bg_tile_size[0] = true; // 16x16 tiles

        // One tilemap entry at word 0 names base tile N = 4 (palette 0).
        ppu.vram[0] = 0x0004;

        // Fill all 8 rows of each quadrant's character with a solid
        // colour index so the assertion holds regardless of which row a
        // flip would select. 4bpp: plane0/1 at (0x1000 + T*16 + row),
        // plane2/3 at +8.
        fill_solid_4bpp_tile(&mut ppu, 4, 0x00FF, 0x0000); // TL -> index 1
        fill_solid_4bpp_tile(&mut ppu, 5, 0xFF00, 0x0000); // TR -> index 2
        fill_solid_4bpp_tile(&mut ppu, 20, 0xFFFF, 0x0000); // BL -> index 3
        fill_solid_4bpp_tile(&mut ppu, 21, 0x0000, 0x00FF); // BR -> index 4

        ppu.cgram[1] = 0x1111;
        ppu.cgram[2] = 0x2222;
        ppu.cgram[3] = 0x3333;
        ppu.cgram[4] = 0x4444;

        let mut top = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        let mut bot = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        ppu.render_bg_row(0, 1, &mut top, false); // screen_y 0 -> top half
        ppu.render_bg_row(0, 9, &mut bot, false); // screen_y 8 -> bottom half

        assert_eq!(top[0].color, 0x1111, "x0..7,y0..7 -> TL tile N");
        assert_eq!(top[8].color, 0x2222, "x8..15,y0..7 -> TR tile N+1");
        assert_eq!(bot[0].color, 0x3333, "x0..7,y8..15 -> BL tile N+16");
        assert_eq!(bot[8].color, 0x4444, "x8..15,y8..15 -> BR tile N+17");

        assert!(
            !ppu.bg_tilemap_row_cache[0].valid,
            "16x16 tiles must bypass the 8x8 tilemap-row cache"
        );
    }

    /// H/V flips on a 16x16 entry mirror *which* quadrant each screen
    /// position reads: with both flips set, the top-left screen corner
    /// must resolve to the bottom-right character (N+17), etc.
    #[test]
    fn render_bg_row_tile16_flips_swap_quadrants() {
        let mut ppu = Ppu::new();
        ppu.bgmode = 1;
        ppu.bg_screen_addr[0] = 0x0000;
        ppu.bg_tiledata_addr[0] = 0x2000;
        ppu.bg_screen_size[0] = 0;
        ppu.bg_voffset[0] = 0xFFFF;
        ppu.bg_tile_size[0] = true;

        // Entry: tile N = 4 with flip_h (0x4000) + flip_v (0x8000).
        ppu.vram[0] = 0x0004 | 0x4000 | 0x8000;

        // Same four characters as the non-flipped test, every row solid
        // so the per-quadrant colour index is unaffected by the decoder's
        // pixel mirroring, only the quadrant *selection* flips.
        fill_solid_4bpp_tile(&mut ppu, 4, 0x00FF, 0x0000); // tile 4  -> index 1
        fill_solid_4bpp_tile(&mut ppu, 5, 0xFF00, 0x0000); // tile 5  -> index 2
        fill_solid_4bpp_tile(&mut ppu, 20, 0xFFFF, 0x0000); // tile 20 -> index 3
        fill_solid_4bpp_tile(&mut ppu, 21, 0x0000, 0x00FF); // tile 21 -> index 4

        ppu.cgram[1] = 0x1111;
        ppu.cgram[2] = 0x2222;
        ppu.cgram[3] = 0x3333;
        ppu.cgram[4] = 0x4444;

        let mut top = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        let mut bot = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        ppu.render_bg_row(0, 1, &mut top, false);
        ppu.render_bg_row(0, 9, &mut bot, false);

        // Both flips set -> screen TL reads BR char (N+17, idx 4), screen
        // TR reads BL (N+16, idx 3), screen BL reads TR (N+1, idx 2),
        // screen BR reads TL (N, idx 1).
        assert_eq!(top[0].color, 0x4444, "flip: screen-TL -> char N+17");
        assert_eq!(top[8].color, 0x3333, "flip: screen-TR -> char N+16");
        assert_eq!(bot[0].color, 0x2222, "flip: screen-BL -> char N+1");
        assert_eq!(bot[8].color, 0x1111, "flip: screen-BR -> char N");
    }

    /// Mode 5 (hires) BG1 with 16x16 tiles must still expand each entry
    /// to the 2x2 block of 8-px characters across the 512-dot grid. The
    /// PPU bus activity test's Mode 6 text and Air Strike Patrol's menu
    /// need it: read as 8x8, the bottom characters (`+16`) are never
    /// selected and tall text shows only its top half.
    #[test]
    fn render_bg_row_hires_tile16_selects_four_quadrants() {
        let mut ppu = Ppu::new();
        ppu.bgmode = 5; // hires
        ppu.bg_screen_addr[0] = 0x0000;
        ppu.bg_tiledata_addr[0] = 0x2000; // char word base 0x1000
        ppu.bg_screen_size[0] = 0;
        ppu.bg_voffset[0] = 0xFFFF; // screen_y = scanline - 1
        ppu.bg_tile_size[0] = true; // 16x16 tiles

        ppu.vram[0] = 0x0004; // entry names base tile N = 4

        fill_solid_4bpp_tile(&mut ppu, 4, 0x00FF, 0x0000); // TL -> index 1
        fill_solid_4bpp_tile(&mut ppu, 5, 0xFF00, 0x0000); // TR -> index 2
        fill_solid_4bpp_tile(&mut ppu, 20, 0xFFFF, 0x0000); // BL -> index 3
        fill_solid_4bpp_tile(&mut ppu, 21, 0x0000, 0x00FF); // BR -> index 4

        ppu.cgram[1] = 0x1111;
        ppu.cgram[2] = 0x2222;
        ppu.cgram[3] = 0x3333;
        ppu.cgram[4] = 0x4444;

        let mut top = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        let mut bot = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        ppu.render_bg_row(0, 1, &mut top, true); // screen_y 0 -> top half
        ppu.render_bg_row(0, 9, &mut bot, true); // screen_y 8 -> bottom half

        // 512-dot grid: cols 0..7 = left char, cols 8..15 = right char.
        assert_eq!(top[0].color, 0x1111, "x0..7,y0..7 -> TL tile N");
        assert_eq!(top[8].color, 0x2222, "x8..15,y0..7 -> TR tile N+1");
        assert_eq!(bot[0].color, 0x3333, "x0..7,y8..15 -> BL tile N+16");
        assert_eq!(bot[8].color, 0x4444, "x8..15,y8..15 -> BR tile N+17");
    }

    /// In hires a 16x16 entry spans 16 BG rows: `screen_y >> 4` advances
    /// the tilemap row and `screen_y & 8` picks top vs bottom char within
    /// the block. This also guards the interlace weave, which feeds a
    /// *doubled* `screen_y` into the same path.
    #[test]
    fn render_bg_row_hires_tile16_vertical_stride_is_16() {
        let mut ppu = Ppu::new();
        ppu.bgmode = 5;
        ppu.bg_screen_addr[0] = 0x0000;
        ppu.bg_tiledata_addr[0] = 0x2000;
        ppu.bg_screen_size[0] = 0;
        ppu.bg_voffset[0] = 0; // screen_y = scanline
        ppu.bg_tile_size[0] = true;

        // Block row 0 = tilemap word 0 -> tile 4; block row 1 = word 32
        // (one 16-px-tall row down) -> tile 8.
        ppu.vram[0] = 0x0004;
        ppu.vram[32] = 0x0008;
        fill_solid_4bpp_tile(&mut ppu, 4, 0x00FF, 0x0000); // block0 TL -> idx 1
        fill_solid_4bpp_tile(&mut ppu, 20, 0xFFFF, 0x0000); // block0 BL (4+16) -> idx 3
        fill_solid_4bpp_tile(&mut ppu, 8, 0xFF00, 0x0000); // block1 TL -> idx 2
        ppu.cgram[1] = 0x1111;
        ppu.cgram[2] = 0x2222;
        ppu.cgram[3] = 0x3333;

        let top_color = |ppu: &mut Ppu, sy: u16| {
            let mut row = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
            ppu.render_bg_row(0, sy, &mut row, true);
            row[0].color
        };
        assert_eq!(top_color(&mut ppu, 0), 0x1111, "y0 -> block0 top char");
        assert_eq!(
            top_color(&mut ppu, 8),
            0x3333,
            "y8 -> block0 bottom char (+16), same entry"
        );
        assert_eq!(
            top_color(&mut ppu, 16),
            0x2222,
            "y16 -> block1 top char (next 16-px entry row)"
        );
    }

    /// Honouring 16x16 in hires must not disturb 8x8 hires BGs (size bit
    /// clear): those keep pairing `tile` (left 8 px) with the adjacent
    /// `tile + 1` (right 8 px) via `hires_adjacent_tile_offset`, never the
    /// 16x16 `+16`.
    #[test]
    fn render_bg_row_hires_8x8_still_pairs_adjacent_tiles() {
        let mut ppu = Ppu::new();
        ppu.bgmode = 5;
        ppu.bg_screen_addr[0] = 0x0000;
        ppu.bg_tiledata_addr[0] = 0x2000;
        ppu.bg_screen_size[0] = 0;
        ppu.bg_voffset[0] = 0xFFFF; // screen_y = scanline - 1
        ppu.bg_tile_size[0] = false; // 8x8

        ppu.vram[0] = 0x0004; // entry tile N = 4
        fill_solid_4bpp_tile(&mut ppu, 4, 0x00FF, 0x0000); // tile N -> index 1
        fill_solid_4bpp_tile(&mut ppu, 5, 0xFF00, 0x0000); // tile N+1 -> index 2
        ppu.cgram[1] = 0x1111;
        ppu.cgram[2] = 0x2222;

        let mut row = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        ppu.render_bg_row(0, 1, &mut row, true); // screen_y 0

        assert_eq!(row[0].color, 0x1111, "left 8 px -> tile N");
        assert_eq!(row[8].color, 0x2222, "right 8 px -> adjacent tile N+1");
    }

    // -- Sprite (OBJ) rasteriser -------------------------------

    /// Bare 8x8-small sprite PPU with the OBJ tile data at VRAM 0 and
    /// every sprite parked at OAM Y = 240 (below the visible test
    /// lines), so individual tests can place exactly the sprites they
    /// care about.
    fn obj_test_ppu() -> Ppu {
        let mut ppu = Ppu::new();
        ppu.obj_base_size = 0;
        ppu.obj_tiledata_addr = 0;
        ppu.obj_name_select = 0;
        for i in 0..128 {
            ppu.oam[i * 4 + 1] = 0xF0;
        }
        ppu
    }

    /// Program sprite `idx`'s four low-table bytes.
    fn set_sprite(ppu: &mut Ppu, idx: usize, x: u8, y: u8, character: u8, attr: u8) {
        let base = idx * 4;
        ppu.oam[base] = x;
        ppu.oam[base + 1] = y;
        ppu.oam[base + 2] = character;
        ppu.oam[base + 3] = attr;
    }

    #[test]
    fn render_obj_row_single_8x8_sprite() {
        let mut ppu = obj_test_ppu();
        set_sprite(&mut ppu, 0, 0, 0, 0, 0);
        // Tile 0 row 0: plane0 = 0xFF -> colour index 1 across the row.
        ppu.vram[0] = 0x00FF;
        ppu.cgram[129] = 0x1234; // 128 + palette 0 * 16 + index 1

        let mut obj = [ObjRowPixel::TRANSPARENT; NORMAL_SCANLINE_WIDTH];
        // Scanline 1 -> line 0 -> sprite row 0.
        ppu.render_obj_row(1, &mut obj);

        for (x, px) in obj.iter().enumerate().take(8) {
            assert!(px.opaque, "col {x} opaque");
            assert_eq!(px.color, 0x1234, "col {x} colour");
            assert_eq!(px.prio, 0);
        }
        assert!(!obj[8].opaque, "column past the sprite is transparent");
    }

    #[test]
    fn render_obj_row_palette_and_priority_from_attr() {
        let mut ppu = obj_test_ppu();
        // attr: priority 3 (0x30) | palette 5 (0x0A) = 0x3A.
        set_sprite(&mut ppu, 0, 0, 0, 0, 0x3A);
        ppu.vram[0] = 0x00FF; // index 1
        ppu.cgram[128 + 5 * 16 + 1] = 0x7ABC;

        let mut obj = [ObjRowPixel::TRANSPARENT; NORMAL_SCANLINE_WIDTH];
        ppu.render_obj_row(1, &mut obj);
        assert!(obj[0].opaque);
        assert_eq!(obj[0].color, 0x7ABC);
        assert_eq!(obj[0].prio, 3);
    }

    #[test]
    fn render_obj_row_hflip_reverses_row() {
        let mut ppu = obj_test_ppu();
        set_sprite(&mut ppu, 0, 0, 0, 0, 0x40); // hflip
                                                // plane0 bit 7 set -> only column 0 opaque before flipping.
        ppu.vram[0] = 0x0080;
        ppu.cgram[129] = 0x1234;

        let mut obj = [ObjRowPixel::TRANSPARENT; NORMAL_SCANLINE_WIDTH];
        ppu.render_obj_row(1, &mut obj);
        assert!(obj[7].opaque, "hflip moves the leftmost pixel to column 7");
        assert_eq!(obj[7].color, 0x1234);
        assert!(!obj[0].opaque);
    }

    #[test]
    fn resolved_render_order_top_matches_obj_row_winner() {
        // Parity guard: the sprite the resolved order reports on top
        // (last) must win the overlap pixel the renderer actually paints.
        use retro_core::video::VideoDebugger;
        let mut ppu = obj_test_ppu();
        // Two 8x8 sprites stacked at (0,0): slot 0 palette 0, slot 1
        // palette 1, both lit on row 0 -> they overlap at column 0.
        set_sprite(&mut ppu, 0, 0, 0, 0, 0x00); // palette 0
        set_sprite(&mut ppu, 1, 0, 0, 0, 0x02); // palette 1 (attr bit 1)
        ppu.vram[0] = 0x00FF; // tile 0 row 0 -> index 1 across
        ppu.cgram[128 + 1] = 0xAAAA; // palette 0 index 1 (slot 0)
        ppu.cgram[128 + 16 + 1] = 0x5555; // palette 1 index 1 (slot 1)

        let mut obj = [ObjRowPixel::TRANSPARENT; NORMAL_SCANLINE_WIDTH];
        ppu.render_obj_row(1, &mut obj);

        let order = ppu.resolved_sprite_render_order();
        let top = order.last().expect("non-empty order");
        assert_eq!(top.index, 0, "OAM slot 0 (no rotation) is highest priority");
        assert!(obj[0].opaque);
        assert_eq!(
            obj[0].color, 0xAAAA,
            "overlap shows the top sprite's colour"
        );
        assert_ne!(obj[0].color, 0x5555, "lower sprite did not win the overlap");
    }

    #[test]
    fn eval_obj_overflow_range_over_past_32_on_line() {
        let mut ppu = obj_test_ppu();
        // 33 sprites all on line 0 at x = 0 -> range overflow.
        ppu.vram[0] = 0x00FF;
        ppu.cgram[129] = 0x1234;
        for i in 0..33 {
            set_sprite(&mut ppu, i, 0, 0, 0, 0);
        }
        ppu.eval_obj_overflow(1);
        assert!(
            ppu.sprite_range_over,
            "33 sprites on one line sets range-over"
        );
        assert!(
            !ppu.sprite_time_over,
            "33 single-tile sprites stay under 34 tiles"
        );
    }

    #[test]
    fn eval_obj_overflow_time_over_past_34_tiles() {
        let mut ppu = obj_test_ppu();
        ppu.obj_base_size = 2; // small 8x8, large 64x64
        ppu.vram[0] = 0x00FF;
        ppu.cgram[129] = 0x1234;
        // Five 64-wide large sprites = 5 x 8 = 40 tile columns > 34.
        // Spread their X so the tile columns are on-screen.
        for i in 0..5 {
            // high-table large bit for sprite i.
            ppu.oam[0x200 + (i >> 2)] |= 0x02 << ((i & 3) * 2);
            set_sprite(&mut ppu, i, 0, 0, 0, 0);
        }
        ppu.eval_obj_overflow(1);
        assert!(ppu.sprite_time_over, ">34 tiles on one line sets time-over");
    }

    #[test]
    fn render_scanline_sets_overflow_flags_with_obj_off_and_forced_blank() {
        // The Electronics Test of Nintendo's SNES Test Program needs this:
        // STAT77 ($213E) range/time-over must be set by sprite evaluation even
        // when OBJ is disabled on both screens AND the screen is in
        // forced blank, only compositing is display-gated on hardware
        // (checked against snes9x). render_obj_row (the
        // painted path) would be skipped in both states, so the flags
        // have to come from the unconditional eval in render_scanline.
        let mut ppu = obj_test_ppu();
        ppu.vram[0] = 0x00FF;
        ppu.cgram[129] = 0x1234;
        for i in 0..33 {
            set_sprite(&mut ppu, i, 0, 0, 0, 0);
        }
        // OBJ off on both screens + forced blank: the painter never runs.
        ppu.obj_main_enable = false;
        ppu.obj_sub_enable = false;
        ppu.display_disable = true;

        let mut fb = Framebuffer::new(NORMAL_SCANLINE_WIDTH as u32, 224);
        ppu.render_scanline(1, &mut fb);
        assert!(
            ppu.sprite_range_over,
            "range-over must be set even with OBJ off + forced blank"
        );
    }

    #[test]
    fn cached_sprite_tile_reuses_until_generation_bumps() {
        let mut ppu = obj_test_ppu();
        ppu.vram[0] = 0x00FF; // tile 0 row 0 -> index 1 across

        let first = ppu.cached_sprite_tile(0);
        assert_eq!(first[0], 1);
        let second = ppu.cached_sprite_tile(0);
        assert_eq!(first, second, "same generation -> identical cache hit");

        // Poke VRAM without bumping the generation: the cache must keep
        // serving the prior decode (coherence is generation-keyed, and
        // real writes only land while the generation can move).
        ppu.vram[0] = 0x0000;
        assert_eq!(
            ppu.cached_sprite_tile(0)[0],
            1,
            "stale VRAM poke under a frozen generation stays cached"
        );

        // A landed VRAM write bumps the generation -> re-decode.
        ppu.vram_generation += 1;
        assert_eq!(
            ppu.cached_sprite_tile(0)[0],
            0,
            "generation bump forces a fresh decode"
        );
    }

    #[test]
    fn render_scanline_clears_sprite_overflow_at_frame_top() {
        let mut ppu = Ppu::new();
        ppu.display_disable = false;
        ppu.display_brightness = 15;
        // Push all 128 sprites off the tested scanlines (Y=224, the Y
        // byte is OAM offset +1): an all-zero OAM otherwise parks 128 8x8
        // sprites at Y=0, which the unconditional `eval_obj_overflow`
        // would (correctly) count as a line-0 range overflow and re-set
        // the flag right after the frame-top clear we're trying to
        // observe.
        for i in 0..128 {
            ppu.oam[i * 4 + 1] = 0xE0;
        }
        ppu.sprite_range_over = true;
        ppu.sprite_time_over = true;
        let mut fb = Framebuffer::new(256, 224);

        // Scanline 1 is the frame top -> both flags reset.
        ppu.render_scanline(1, &mut fb);
        assert!(!ppu.sprite_range_over);
        assert!(!ppu.sprite_time_over);

        // A mid-frame scanline must NOT touch them (they accumulate).
        ppu.sprite_range_over = true;
        ppu.render_scanline(50, &mut fb);
        assert!(ppu.sprite_range_over, "only the frame top clears the flags");
    }

    #[test]
    fn render_scanline_clears_overflow_even_in_forced_blank() {
        let mut ppu = Ppu::new();
        ppu.display_disable = true; // forced blank short-circuits below
                                    // Off-screen OAM (Y byte at offset +1; see frame-top-clear test)
                                    // so the unconditional overflow eval doesn't re-flag a line-0
                                    // all-zero OAM.
        for i in 0..128 {
            ppu.oam[i * 4 + 1] = 0xE0;
        }
        ppu.sprite_range_over = true;
        ppu.sprite_time_over = true;
        let mut fb = Framebuffer::new(256, 224);
        ppu.render_scanline(1, &mut fb);
        assert!(
            !ppu.sprite_range_over,
            "frame-top clear precedes forced blank"
        );
        assert!(!ppu.sprite_time_over);
    }

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
