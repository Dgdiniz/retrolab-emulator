//! Per-scanline background renderer for SNES Modes 0-7.
//!
//! Hosts the public [`Ppu::render_scanline`] entry point, the
//! per-mode walk of the four BG layers, the priority compositor with
//! its window clips and colour math, and the rows written into the
//! host framebuffer. The layers' rows are filled beside it: a tile
//! BG's in `backgrounds.rs` (with the tilemap-row cache and the
//! mosaic), the sprites' in `sprite.rs`, and the Mode 7 affine
//! plane's in `mode7.rs`, with the coefficient state; the affine
//! raster reuses the same mosaic / snapshot / composite pipeline as
//! the tile modes, only Step 1 (the BG fill) differs. The picture's
//! size, its normalisation and the interlace field are in
//! `picture.rs`, and the redraws in `redraw.rs`.
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
//! ## Pseudo-hires
//!
//! With the pseudo-hires bit (`$2133` bit 3) set in Modes 0-4 and 7 a
//! line is 512 columns wide as well, two a dot. The backgrounds, the
//! sprites and the windows stay at their 256 dots; each dot's even
//! column shows the sub screen, with the main screen as the addend, and
//! its odd column the main screen, the hires rule at the dot the two
//! columns share. The sub screen's empty pixels show colour 0, as in
//! hires. A frame with such a line is 512 wide and 448 high, as a hires
//! frame is.
//!
//! ## Overscan
//!
//! With `$2133` bit 2 set a frame draws 239 lines (scanlines 1 to
//! 239) where it draws 224 without. The scheduler always fires
//! `RenderPos` for SNES lines `1..=240` and [`Ppu::in_active_scanline`]
//! gates the lower end based on the live overscan bit, so flipping
//! overscan mid-frame switches the renderer on or off for the extra
//! lines without rescheduling. They draw only before the vertical
//! blank, which the bit as it stood when the frame began places:
//! line 240 with overscan, else 225 on NTSC and 240 on PAL.

use retro_core::capture::{draw_order_runs, DrawOrder, DrawSlot, NoTrace, PixelTrace};
use retro_core::video::ColourLevels;
use retro_core::Framebuffer;

use crate::ppu::colormath::{color_add, color_sub};
use crate::ppu::picture::WovenRow;
use crate::ppu::redraw::FIRST_LINE_PAST_OVERSCAN;
use crate::ppu::trace::{pixel_layer, source_code, FrameTrace, MathOutcome, RowColumns};
use crate::ppu::window::{col_region, window_inside};
use crate::ppu::{window_layer, Ppu, LAYER_OBJ};

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

/// One of the two screens a line composites: the main screen, shown, and
/// the sub screen, the colour math's addend (shown on the even columns of
/// a hires or pseudo-hires line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Screen {
    Main,
    Sub,
}

/// Per-scanline sprite output buffer. Always 256 columns wide, OBJ
/// never renders at hires resolution; the hires compositor maps each
/// OBJ column into both display sub-pixels.
pub(crate) type ObjRowBuffer = [ObjRowPixel; NORMAL_SCANLINE_WIDTH];

/// Which layer won a composited column. Drives the per-source colour-
/// math enable lookup (`color_math_bg_enable` / `color_math_oam_enable`
/// / `color_math_back_enable`) in the colour-math stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PixelSource {
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
pub(crate) struct ScreenPixel {
    /// 15-bit BGR colour after priority resolution + window clip.
    pub(crate) color: u16,
    /// Layer that produced `color`.
    pub(crate) source: PixelSource,
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
pub(crate) type ScreenRow = [ScreenPixel; MAX_SCANLINE_WIDTH];

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
    /// active region. The active region is 224 lines normally
    /// (scanlines 1 to 224) and 239 with overscan (`$2133` bit 2,
    /// scanlines 1 to 239); the live bit is
    /// consulted via [`Ppu::in_active_scanline`] so that
    /// mid-frame overscan toggles flip the gate without rescheduling
    /// the H-event timeline.
    ///
    /// In Mode 5/6 with interlace (`$2133` bit 0) the vertical
    /// resolution doubles: each scanline emits two woven output rows
    /// (the even/odd field lines) so the full 512x448 image is built
    /// in a single frame; a pseudo-hires line emits two rows as well.
    /// The per-row work lives in [`Ppu::render_output_line`].
    pub fn render_scanline(&mut self, scanline: u16, fb: &mut Framebuffer) {
        self.draw_scanline(scanline, fb, &mut NoTrace::<FrameTrace>::new());
    }

    /// [`Self::render_scanline`], noting where each pixel came from in
    /// `trace` when it takes notes. The frame loops draw with [`NoTrace`],
    /// whose `ON` is false, so every note sits behind `if T::ON` and their
    /// code is the renderer's alone.
    pub(crate) fn draw_scanline<T: PixelTrace<Notes = FrameTrace>>(
        &mut self,
        scanline: u16,
        fb: &mut Framebuffer,
        trace: &mut T,
    ) {
        if scanline == 0 || !self.in_active_scanline(scanline) {
            return;
        }
        // The line table takes the settings the line is drawn with, when
        // one is kept (see `record`).
        if self.recorders.lines.is_some() {
            self.record_line_state(scanline);
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
        // blank, a blanked hires line is still emitted 512 wide. A
        // pseudo-hires line is 512 wide too.
        let hires = is_hires_mode(self.bgmode);
        let wide = hires || self.pseudo_hires;
        self.frame_hires |= wide;

        // Mode 5/6 interlace weaves both fields into a single complete
        // frame, as bsnes and ares weave them: each scanline gives two
        // rows, its even field on the first and its odd field on the
        // second, on the rows the picture's grid gives them (`WovenRow`).
        // `vy`, the BG vertical source (the value `render_bg_row` adds
        // the per-BG V-scroll to), is that field's line of the doubled
        // frame, `py = (vcounter << 1) | field`, and the sprites are the
        // line's, with that field's rows. A pseudo-hires line of an
        // interlaced frame gives its two rows too; its backgrounds keep
        // their 256 dots and their lines. The non-interlace path keeps
        // the plain mapping: output row N-1 <- scanline N (vy = N), its
        // sprites drawn for the frame's field. The weave is gated on the
        // latched `render_interlace`, not the live bit, so the whole
        // frame agrees with `active_display_size`.
        if wide && self.render_interlace {
            let grid = self.picture_grid();
            for grid_field in 0..2u8 {
                let row = grid.row(u32::from(scanline - 1), grid_field);
                let shown = WovenRow::of(scanline, grid_field);
                self.mark_line_hires(row, true);
                self.render_output_line(
                    shown.background_line(hires),
                    shown.line,
                    shown.field,
                    row,
                    fb,
                    trace,
                );
            }
        } else {
            let fb_row = u32::from(scanline - 1);
            self.mark_line_hires(fb_row, wide);
            let field = self.field;
            self.render_output_line(scanline, scanline, field, fb_row, fb, trace);
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
    /// scanline used for sprite evaluation, `field` the interlace field
    /// whose rows OBJ interlace draws, and `fb_row` the destination
    /// row. The split from [`Ppu::render_scanline`] lets the interlace
    /// path emit two woven rows per scanline, each for its own field,
    /// while the normal path emits one.
    fn render_output_line<T: PixelTrace<Notes = FrameTrace>>(
        &mut self,
        vy: u16,
        obj_line: u16,
        field: bool,
        fb_row: u32,
        fb: &mut Framebuffer,
        trace: &mut T,
    ) {
        // The host framebuffer must be at least as tall as the active
        // region (448 with hires interlace) or the trailing rows clip
        // here.
        if fb_row >= fb.height {
            return;
        }
        // A single-layer render draws its one layer instead of the
        // composite (see `render_layer`); never set during play.
        if let Some(layer) = self.isolated_layer {
            self.render_isolated_line(layer, vy, obj_line, field, fb_row, fb, trace);
            return;
        }

        let hires = is_hires_mode(self.bgmode);
        let output_width = if hires {
            MAX_SCANLINE_WIDTH
        } else {
            NORMAL_SCANLINE_WIDTH
        };
        // A pseudo-hires line keeps its rows, composites and windows at
        // the 256 dots of `output_width` and is written 512 columns wide,
        // two a dot.
        let pseudo = self.pseudo_hires && !hires;
        let line_width = if hires || pseudo {
            MAX_SCANLINE_WIDTH
        } else {
            NORMAL_SCANLINE_WIDTH
        };

        // Forced blank or zero-brightness collapses to a black row.
        // The vertical-mosaic buffer also gets cleared so a later
        // unmask doesn't pull stale colour back into a mosaic block.
        if self.display_disable || self.display_brightness == 0 {
            write_black_scanline(fb_row, fb, line_width);
            for bg in 0..4 {
                self.previous_bg_rows[bg] = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
            }
            self.sub_rows_kept = false;
            if T::ON {
                trace
                    .notes()
                    .note_blank_row(fb_row, (fb.width as usize).min(line_width));
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
        // of colour math and is always honoured in the composite. A
        // pseudo-hires line shows the sub plane, as a hires one does.
        let color_math_possible = self.col_main_mask != 0
            || self.color_math_back_enable
            || self.color_math_oam_enable
            || self.color_math_bg_enable.iter().any(|&b| b);
        let need_sub = hires || pseudo || color_math_possible;

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
            let bg1 = self.bg_main_enable[0];
            let bg2 = self.mode7_extbg && self.bg_main_enable[1];
            self.render_mode7_rows(vy, &mut main_rows, bg1, bg2, trace);
        } else {
            for bg_id in 0..4 {
                if !bg_visible_in_mode(self.bgmode, bg_id) {
                    continue;
                }
                if self.bg_main_enable[bg_id] {
                    self.render_bg_row(bg_id, vy, &mut main_rows[bg_id], hires, trace);
                }
                // The sub plane feeds the hires interleave and the
                // colour-math addend; it is only walked when one of
                // those needs it, so the no-colour-math non-hires path
                // never pays for the second render.
                if self.bg_sub_enable[bg_id] && need_sub {
                    self.render_bg_row(bg_id, vy, &mut sub_rows[bg_id], hires, trace);
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
        // plane.
        if T::ON {
            trace.notes().begin_main_screen();
        }
        for (bg_id, row) in main_rows.iter_mut().enumerate() {
            self.apply_mosaic_for_bg(Screen::Main, bg_id, vy, row, output_width, trace);
        }

        // Step 3: snapshot each main BG row for next-line vmosaic.
        // We always copy the full 512 cols so the previous-row
        // buffer stays consistent when the mode toggles between
        // hires and non-hires across frames.
        self.previous_bg_rows.copy_from_slice(&main_rows);
        if T::ON {
            trace.notes().keep_previous();
        }

        // Steps 2 and 3 on the sub plane, when the line builds it: the
        // same mosaic, its vertical replay from rows of its own, which a
        // line keeps only while the mosaic is on (`sub_rows_kept`).
        if need_sub && T::ON {
            trace.notes().begin_sub_screen();
        }
        let keep_sub_rows =
            need_sub && self.mosaic_size > 1 && self.bg_mosaic_enable.iter().any(|&on| on);
        if keep_sub_rows {
            for (bg_id, row) in sub_rows.iter_mut().enumerate() {
                self.apply_mosaic_for_bg(Screen::Sub, bg_id, vy, row, output_width, trace);
            }
            self.previous_sub_rows.copy_from_slice(&sub_rows);
            if T::ON {
                trace.notes().keep_previous_sub();
            }
        }
        self.sub_rows_kept = keep_sub_rows;

        // Step 3.5: evaluate sprites once into a 256-wide OBJ row. The
        // walk is skipped entirely when OBJ is off on both screens.
        let mut obj_row = [ObjRowPixel::TRANSPARENT; NORMAL_SCANLINE_WIDTH];
        if (self.obj_main_enable || self.obj_sub_enable) && !self.layer_debug_disabled(LAYER_OBJ) {
            self.render_obj_row(obj_line, field, &mut obj_row, trace);
        }

        // Step 4: composite each enabled screen by the per-mode
        // priority list, then run the colour-math stage. Modes 0-6
        // pass the `$2105` bit-3 BG3-priority flag as the priority
        // variant; Mode 7 reuses the same slot for the EXTBG flag
        // (`$2133` bit 6). OBJ is gated per-plane by `obj_main_enable`
        // / `obj_sub_enable`.
        let priority_variant = priority_variant(self.bgmode, self.bg3_priority, self.mode7_extbg);
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
            if T::ON {
                let width = (fb.width as usize).min(NORMAL_SCANLINE_WIDTH);
                trace.notes().note_row(
                    fb_row,
                    width,
                    RowColumns::Dots,
                    &main_composite,
                    None,
                    false,
                );
            }
            return;
        }

        // Sub screen. Its backdrop default is the fixed colour in the
        // normal modes (checked against bsnes-plus); hires and
        // pseudo-hires use the real backdrop, colour 0, because the sub
        // plane is a displayed image.
        let sub_backdrop = if hires || pseudo {
            backdrop
        } else {
            self.fixed_color
        };
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
                *cell = if T::ON {
                    let (colour, outcome) = if k & 1 == 1 {
                        self.resolve_pixel_explained(
                            k,
                            &main_composite,
                            &sub_composite,
                            &col_main,
                            &col_sub,
                        )
                    } else {
                        self.resolve_pixel_explained(
                            k,
                            &sub_composite,
                            &main_composite,
                            &col_main,
                            &col_sub,
                        )
                    };
                    trace.notes().math[k] = outcome;
                    colour
                } else if k & 1 == 1 {
                    self.resolve_pixel(k, &main_composite, &sub_composite, &col_main, &col_sub)
                } else {
                    self.resolve_pixel(k, &sub_composite, &main_composite, &col_main, &col_sub)
                };
            }
            self.write_scanline_hires(fb_row, &line, fb);
            if T::ON {
                let width = (fb.width as usize).min(MAX_SCANLINE_WIDTH);
                trace.notes().note_row(
                    fb_row,
                    width,
                    RowColumns::Hires,
                    &main_composite,
                    Some(&sub_composite),
                    true,
                );
            }
        } else if pseudo {
            // Pseudo-hires output: two columns a dot, both composited at
            // the dot. The even column shows the sub screen with the main
            // screen as the addend, the odd column the main screen with
            // the sub screen as the addend: the hires rule, with the
            // backgrounds at 256 dots.
            let mut line = [0u16; MAX_SCANLINE_WIDTH];
            let (pairs, _) = line.as_chunks_mut::<2>();
            for (x, pair) in pairs.iter_mut().enumerate() {
                if T::ON {
                    let (even, even_outcome) = self.resolve_pixel_explained(
                        x,
                        &sub_composite,
                        &main_composite,
                        &col_main,
                        &col_sub,
                    );
                    let (odd, odd_outcome) = self.resolve_pixel_explained(
                        x,
                        &main_composite,
                        &sub_composite,
                        &col_main,
                        &col_sub,
                    );
                    let notes = trace.notes();
                    notes.math[2 * x] = even_outcome;
                    notes.math[2 * x + 1] = odd_outcome;
                    pair[0] = even;
                    pair[1] = odd;
                } else {
                    pair[0] =
                        self.resolve_pixel(x, &sub_composite, &main_composite, &col_main, &col_sub);
                    pair[1] =
                        self.resolve_pixel(x, &main_composite, &sub_composite, &col_main, &col_sub);
                }
            }
            self.write_scanline_hires(fb_row, &line, fb);
            if T::ON {
                let width = (fb.width as usize).min(MAX_SCANLINE_WIDTH);
                trace.notes().note_row(
                    fb_row,
                    width,
                    RowColumns::PseudoHires,
                    &main_composite,
                    Some(&sub_composite),
                    true,
                );
            }
        } else {
            let mut row = [0u16; MAX_SCANLINE_WIDTH];
            for (x, cell) in row.iter_mut().enumerate().take(NORMAL_SCANLINE_WIDTH) {
                *cell = if T::ON {
                    let (colour, outcome) = self.resolve_pixel_explained(
                        x,
                        &main_composite,
                        &sub_composite,
                        &col_main,
                        &col_sub,
                    );
                    trace.notes().math[x] = outcome;
                    colour
                } else {
                    self.resolve_pixel(x, &main_composite, &sub_composite, &col_main, &col_sub)
                };
            }
            self.write_scanline_normal(fb_row, &row, fb);
            if T::ON {
                let width = (fb.width as usize).min(NORMAL_SCANLINE_WIDTH);
                trace.notes().note_row(
                    fb_row,
                    width,
                    RowColumns::Dots,
                    &main_composite,
                    Some(&sub_composite),
                    true,
                );
            }
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
    /// addend; in a hires or pseudo-hires line the even columns show the
    /// sub screen with the main screen as the addend, which is the same
    /// rule with the two rows swapped.
    fn resolve_pixel(
        &self,
        x: usize,
        shown: &ScreenRow,
        addend: &ScreenRow,
        col_main: &[bool],
        col_sub: &[bool],
    ) -> u16 {
        self.resolve_pixel_explained(x, shown, addend, col_main, col_sub)
            .0
    }

    /// [`Self::resolve_pixel`], saying too what colour math did: the one
    /// rule the picture and a traced redraw share.
    #[inline(always)]
    fn resolve_pixel_explained(
        &self,
        x: usize,
        shown: &ScreenRow,
        addend: &ScreenRow,
        col_main: &[bool],
        col_sub: &[bool],
    ) -> (u16, MathOutcome) {
        let pixel = shown[x];
        let colour = if col_main[x] { pixel.color } else { 0 };
        let blacked = !col_main[x];
        let takes_part =
            col_sub[x] && !pixel.color_exempt && self.color_math_enabled_for(pixel.source);
        if !takes_part {
            return (
                colour,
                MathOutcome {
                    blacked,
                    ..MathOutcome::NONE
                },
            );
        }
        let addend_is_backdrop =
            self.addsub_mode && matches!(addend[x].source, PixelSource::Backdrop);
        let addend_colour = if self.addsub_mode {
            addend[x].color
        } else {
            self.fixed_color
        };
        let halve = self.color_math_halve && col_main[x] && !addend_is_backdrop;
        let result = if self.color_math_subtract {
            color_sub(colour, addend_colour, halve)
        } else {
            color_add(colour, addend_colour, halve)
        };
        let addend_code = if self.addsub_mode {
            source_code(addend[x].source)
        } else {
            pixel_layer::ADDEND_FIXED_COLOUR
        };
        (
            result,
            MathOutcome {
                blacked,
                applied: true,
                halved: halve,
                subtracted: self.color_math_subtract,
                addend: addend_code as u8,
            },
        )
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
}

/// `true` if BG `bg_id` participates in the visible composite under
/// the given mode. BG3 in Modes 2 / 4 / 6 is used as an offset
/// table only and **does not** draw pixels.
pub(super) fn bg_visible_in_mode(bgmode: u8, bg_id: usize) -> bool {
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
pub(super) fn cgram_index(palette_offset: u8, palette_number: u8, pixel: u8, bpp: u8) -> u8 {
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

/// A background's pixels in a priority list: BG `id` (`0..=3`, the
/// layer list's ids) of priority bit `high`.
const fn bg(id: u8, high: bool) -> DrawSlot {
    DrawSlot::new(id as u32, high as u8)
}

/// The sprites' pixels in a priority list, of sprite priority `prio`
/// (`0..=3`).
const fn obj(prio: u8) -> DrawSlot {
    DrawSlot::new(LAYER_OBJ as u32, prio)
}

/// Per-mode priority list in highest-to-lowest order, interleaving the
/// four OBJ priorities with the BG layers as fullsnes's priority table
/// gives it (checked against bsnes-plus). The BG layers keep their relative
/// order, each split by its per-pixel priority bit; the four OBJ slots sit
/// in between. The compositor walks the list top-to-bottom and keeps the
/// first opaque pixel per column, and [`Ppu::draw_order`] reports it as it
/// stands, front to back.
///
/// Mode 1's `bg3_priority` (`$2105` bit 3) lifts BG3's high-prio
/// pixels above everything (including OBJ3), used by menu overlays
/// that draw text on top. Modes 2-5 share the "two BG" interleave;
/// the hires composite reuses the same table for both planes.
///
/// Mode 7 overloads `bg3_priority` to carry the EXTBG flag
/// (`$2133` bit 6). Without EXTBG the plane is BG1 (sandwiched by the
/// four OBJ priorities), whose pixels have no priority bit (0); with it,
/// BG2 splits by its pixel's top bit around BG1.
fn priority_layers(bgmode: u8, bg3_priority: bool) -> &'static [DrawSlot] {
    static MODE0: &[DrawSlot] = &[
        obj(3),
        bg(0, true),
        bg(1, true),
        obj(2),
        bg(0, false),
        bg(1, false),
        obj(1),
        bg(2, true),
        bg(3, true),
        obj(0),
        bg(2, false),
        bg(3, false),
    ];
    static MODE1_NORMAL: &[DrawSlot] = &[
        obj(3),
        bg(0, true),
        bg(1, true),
        obj(2),
        bg(0, false),
        bg(1, false),
        obj(1),
        bg(2, true),
        obj(0),
        bg(2, false),
    ];
    static MODE1_BG3_LIFTED: &[DrawSlot] = &[
        bg(2, true),
        obj(3),
        bg(0, true),
        bg(1, true),
        obj(2),
        bg(0, false),
        bg(1, false),
        obj(1),
        obj(0),
        bg(2, false),
    ];
    static MODE_TWO_BG: &[DrawSlot] = &[
        obj(3),
        bg(0, true),
        obj(2),
        bg(1, true),
        obj(1),
        bg(0, false),
        obj(0),
        bg(1, false),
    ];
    static MODE_ONE_BG: &[DrawSlot] = &[obj(3), bg(0, true), obj(2), obj(1), bg(0, false), obj(0)];
    static MODE7_NORMAL: &[DrawSlot] = &[obj(3), obj(2), obj(1), bg(0, false), obj(0)];
    static MODE7_EXTBG: &[DrawSlot] = &[
        obj(3),
        obj(2),
        bg(1, true),
        obj(1),
        bg(0, false),
        obj(0),
        bg(1, false),
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

/// The variant of mode `bgmode`'s priority list a line takes
/// ([`priority_layers`]): Mode 7's EXTBG bit `extbg` in Mode 7, Mode 1's
/// BG3 priority bit `bg3_priority` in every other mode (only Mode 1 has
/// two lists).
fn priority_variant(bgmode: u8, bg3_priority: bool, extbg: bool) -> bool {
    if bgmode == 7 {
        extbg
    } else {
        bg3_priority
    }
}

impl Ppu {
    /// The order the held frame's lines put the layers in
    /// ([`DrawOrder`]): each line's priority list ([`priority_layers`]),
    /// front to back, the one the compositor walks. A line's mode, its
    /// BG3 priority bit and its EXTBG bit are read from the settings it was
    /// drawn with where the line table holds the frame
    /// ([`Self::draws_from_line_states`]), else from the registers as they
    /// stand, for every line the frame draws. Lines are counted from the
    /// first drawn, scanline 1 line 0, as the picture's grid counts them.
    pub fn draw_order(&self) -> Vec<DrawOrder> {
        let layers = |bgmode: u8, bg3_priority: bool, extbg: bool| {
            priority_layers(bgmode, priority_variant(bgmode, bg3_priority, extbg))
        };
        match self.recorders.lines.as_deref() {
            Some(table) if self.draws_from_line_states() => {
                draw_order_runs(table.lines().iter().map(|(scanline, state)| {
                    (
                        u32::from(*scanline).saturating_sub(1),
                        layers(state.bgmode, state.bg3_priority, state.mode7_extbg),
                    )
                }))
            }
            _ => {
                let slots = layers(self.bgmode, self.bg3_priority, self.mode7_extbg);
                draw_order_runs(
                    (1..FIRST_LINE_PAST_OVERSCAN)
                        .filter(|&scanline| self.in_active_scanline(scanline))
                        .map(|scanline| (u32::from(scanline) - 1, slots)),
                )
            }
        }
    }
}

/// The levels a channel of a colour the picture draws takes at full
/// brightness: a CGRAM colour's 5 bits a channel, each level the bits moved
/// up three places with their top two repeated below them
/// ([`expand_channel_with_brightness`]). The master brightness scales the
/// picture's colours down from these levels.
pub(super) const COLOUR_LEVELS: ColourLevels = ColourLevels {
    name: "snes-32",
    levels: &[
        0, 8, 16, 24, 33, 41, 49, 57, 66, 74, 82, 90, 99, 107, 115, 123, 132, 140, 148, 156, 165,
        173, 181, 189, 198, 206, 214, 222, 231, 239, 247, 255,
    ],
};

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
pub(super) fn bgr15_to_rgb8_with_brightness(bgr15: u16, brightness: u8) -> [u8; 3] {
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
    for slot in priority_layers(bgmode, bg3_priority) {
        if slot.layer == LAYER_OBJ as u32 {
            if !obj_enabled {
                continue;
            }
            let prio = slot.priority;
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
        } else {
            let id = slot.layer as u8;
            let high = slot.priority != 0;
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
    fn the_colour_levels_are_those_a_cgram_channel_converts_to() {
        assert_eq!(COLOUR_LEVELS.name, "snes-32");
        assert_eq!(COLOUR_LEVELS.levels.len(), 32);
        for (channel, &level) in COLOUR_LEVELS.levels.iter().enumerate() {
            let bits = channel as u16;
            let grey = bits | (bits << 5) | (bits << 10);
            assert_eq!(
                bgr15_to_rgb8_with_brightness(grey, 15),
                [level; 3],
                "channel value {channel}"
            );
        }
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
        assert_eq!(l[0], obj(3));
        assert_eq!(l[1], bg(0, true));
        assert_eq!(l[11], bg(3, false));
        // The four OBJ priorities are all present, descending.
        let objs: Vec<u8> = l
            .iter()
            .filter_map(|x| (x.layer == LAYER_OBJ as u32).then_some(x.priority))
            .collect();
        assert_eq!(objs, [3, 2, 1, 0]);
    }

    #[test]
    fn priority_layers_mode1_default() {
        let l = priority_layers(1, false);
        assert_eq!(
            l,
            [
                obj(3),
                bg(0, true),
                bg(1, true),
                obj(2),
                bg(0, false),
                bg(1, false),
                obj(1),
                bg(2, true),
                obj(0),
                bg(2, false),
            ]
        );
    }

    #[test]
    fn priority_layers_mode1_bg3_lifted() {
        let l = priority_layers(1, true);
        // BG3 high lifts above even OBJ3.
        assert_eq!(l[0], bg(2, true));
        assert_eq!(l[1], obj(3));
        assert_eq!(l[2], bg(0, true));
    }

    #[test]
    fn priority_layers_modes_2_3_4_5_only_two_bgs() {
        for mode in [2u8, 3, 4, 5] {
            let l = priority_layers(mode, false);
            // 2 BGs x 2 priorities + 4 OBJ slots = 8 entries.
            assert_eq!(l.len(), 8, "mode {mode}");
            assert!(l
                .iter()
                .all(|x| x.layer == LAYER_OBJ as u32 || x.layer <= 1));
        }
    }

    #[test]
    fn priority_layers_mode6_single_bg_pair() {
        let l = priority_layers(6, false);
        assert_eq!(
            l,
            [obj(3), bg(0, true), obj(2), obj(1), bg(0, false), obj(0),]
        );
    }

    #[test]
    fn the_draw_order_reads_the_registers_for_every_line_drawn_with_no_table() {
        let mut ppu = Ppu::new();
        ppu.bgmode = 1;
        ppu.bg3_priority = true;
        let order = |ppu: &Ppu| -> Vec<(u32, u32, &'static [DrawSlot])> {
            ppu.draw_order()
                .iter()
                .map(|run| {
                    let slots = priority_layers(
                        ppu.bgmode,
                        priority_variant(ppu.bgmode, ppu.bg3_priority, ppu.mode7_extbg),
                    );
                    assert_eq!(run.slots, slots);
                    (run.first_line, run.lines, slots)
                })
                .collect()
        };
        assert_eq!(order(&ppu), [(0, 224, priority_layers(1, true))]);
        ppu.overscan = true;
        assert_eq!(order(&ppu), [(0, 239, priority_layers(1, true))]);
        ppu.bgmode = 7;
        ppu.mode7_extbg = true;
        assert_eq!(
            order(&ppu),
            [(0, 239, priority_layers(7, true))],
            "Mode 7 reads EXTBG, not the BG3 priority bit"
        );
    }

    #[test]
    fn the_draw_order_follows_each_line_s_settings_where_the_table_holds_the_frame() {
        use retro_core::capture::CaptureRecording;
        let mut ppu = Ppu::new();
        ppu.set_recording_frame(1);
        ppu.set_capture_recording(CaptureRecording {
            line_states: true,
            ..Default::default()
        });
        ppu.bgmode = 1;
        for scanline in 1..=224 {
            if scanline == 101 {
                ppu.bgmode = 7;
                ppu.mode7_extbg = true;
            }
            ppu.record_line_state(scanline);
        }
        ppu.note_picture_finished();
        ppu.bgmode = 3;
        let runs: Vec<(u32, u32, Vec<DrawSlot>)> = ppu
            .draw_order()
            .into_iter()
            .map(|run| (run.first_line, run.lines, run.slots))
            .collect();
        assert_eq!(
            runs,
            [
                (0, 100, priority_layers(1, false).to_vec()),
                (100, 124, priority_layers(7, true).to_vec())
            ]
        );
        // A register the debugger edits makes the frame drawn from the
        // registers as they stand.
        ppu.note_registers_edited();
        let runs = ppu.draw_order();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].slots, priority_layers(3, false));
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
            [obj(3), obj(2), obj(1), bg(0, false), obj(0),]
        );
    }

    #[test]
    fn priority_layers_mode7_extbg_orders_bg2_high_bg1_bg2_low() {
        assert_eq!(
            priority_layers(7, true),
            [
                obj(3),
                obj(2),
                bg(1, true),
                obj(1),
                bg(0, false),
                obj(0),
                bg(1, false),
            ]
        );
    }
}
