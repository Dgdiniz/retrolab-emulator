//! Drawing the held frame again on a debugger's request, without
//! advancing emulation: the whole picture into the framebuffer (a
//! repaint), one layer alone, or the picture into a buffer of its own.
//!
//! When the line table holds the frame, each line is drawn with the
//! settings it was drawn with and the video memory it read, so a raster
//! effect comes out as the frame drew it; otherwise every line is drawn
//! from the registers as they stand. Either way the state a savestate
//! holds is left as it was: what drawing moves is taken first and put
//! back after ([`RedrawKeeps`], [`HeldPicture`]).

use retro_core::capture::{NoTrace, PixelTrace};
use retro_core::video::{crop_rgba, ExportError, PixelImage};
use retro_core::Framebuffer;

use super::render::{
    bg_visible_in_mode, bgr15_to_rgb8_with_brightness, is_hires_mode, BgRowBuffer, BgRowPixel,
    ObjRowPixel, Screen, MAX_OUTPUT_ROWS, MAX_SCANLINE_WIDTH, NORMAL_SCANLINE_WIDTH,
};
use super::trace::{FrameTrace, PictureShape};
use super::{record, Ppu, LAYER_COUNT, LAYER_OBJ};

/// One past the last scanline a frame can draw: the overscan frame's 239
/// visible lines end at line 239, and the active-region gate drops the
/// lines past the overscan setting in force.
const FIRST_LINE_PAST_OVERSCAN: u16 = 240;

/// What drawing the frame again changes that is not the picture: the
/// sprite range and time flags STAT77 reads and the address the sprite
/// evaluator leaves for OAM reads, all three part of a savestate and of
/// the state hash. A redraw takes them first and puts them back after,
/// so a repaint or a single-layer render leaves the machine as it was.
#[derive(Clone, Copy)]
struct RedrawKeeps {
    sprite_range_over: bool,
    sprite_time_over: bool,
    oam_iaddr: u16,
}

impl RedrawKeeps {
    fn take(ppu: &Ppu) -> Self {
        Self {
            sprite_range_over: ppu.sprite_range_over,
            sprite_time_over: ppu.sprite_time_over,
            oam_iaddr: ppu.oam_iaddr,
        }
    }

    fn put_back(self, ppu: &mut Ppu) {
        ppu.sprite_range_over = self.sprite_range_over;
        ppu.sprite_time_over = self.sprite_time_over;
        ppu.oam_iaddr = self.oam_iaddr;
    }
}

/// What the held frame's picture leaves in the PPU besides the
/// framebuffer: the frame-global hires and interlace state its display
/// size and normalisation read, and both screens' vertical-mosaic rows.
/// A single-layer render draws into a framebuffer of its own, so it puts
/// these back for the frame the host still shows.
struct HeldPicture {
    frame_hires: bool,
    render_interlace: bool,
    line_hires: [bool; MAX_OUTPUT_ROWS],
    previous_bg_rows: Box<[BgRowBuffer; 4]>,
    previous_sub_rows: Box<[BgRowBuffer; 4]>,
    sub_rows_kept: bool,
}

impl HeldPicture {
    fn take(ppu: &Ppu) -> Self {
        Self {
            frame_hires: ppu.frame_hires,
            render_interlace: ppu.render_interlace,
            line_hires: ppu.line_hires,
            previous_bg_rows: ppu.previous_bg_rows.clone(),
            previous_sub_rows: ppu.previous_sub_rows.clone(),
            sub_rows_kept: ppu.sub_rows_kept,
        }
    }

    fn put_back(self, ppu: &mut Ppu) {
        ppu.frame_hires = self.frame_hires;
        ppu.render_interlace = self.render_interlace;
        ppu.line_hires = self.line_hires;
        ppu.previous_bg_rows = self.previous_bg_rows;
        ppu.previous_sub_rows = self.previous_sub_rows;
        ppu.sub_rows_kept = self.sub_rows_kept;
    }
}

impl Ppu {
    /// Draw the held frame again into `fb` and finish its picture, without
    /// advancing emulation.
    ///
    /// When the line table holds the frame ([`Self::draws_from_line_states`])
    /// each line is drawn with its own settings and the video memory it
    /// read, so a raster effect comes out as the frame drew it. Otherwise
    /// every visible line is drawn from the registers as they stand,
    /// through [`Self::render_scanline`] (from line 1, so the frame's own
    /// resets run; the active-region gate drops the lines past the
    /// overscan setting). Either way [`Self::finalize_frame`] follows.
    ///
    /// Drawing moves state a savestate holds, the sprite range and time
    /// flags and the evaluator's OAM address; they are put back as they
    /// were (see [`RedrawKeeps`]), so the state hash does not move.
    pub(crate) fn redraw_frame(&mut self, fb: &mut Framebuffer) {
        let keeps = RedrawKeeps::take(self);
        let from_table = self.draws_from_line_states();
        self.draw_frame_again(fb, from_table, &mut NoTrace::<FrameTrace>::new());
        keeps.put_back(self);
    }

    /// The held frame drawn again with only `layer` in it, as RGBA the
    /// size of the frame's picture: each pixel the layer covers is opaque,
    /// and every other pixel, the backdrop's included, has alpha 0.
    /// `layer` is a debug layer index (`0..=3` BG1-4, `4` OBJ), the ids
    /// [`VideoDebugger::toggle_layer`](retro_core::video::VideoDebugger)
    /// uses; another index is [`ExportError::OutOfRange`].
    ///
    /// The layer is drawn as the frame draws it: the same fetch (direct
    /// colour, 16x16 tiles, offset-per-tile, hires, the Mode 7 plane),
    /// the same mosaic and the same rows (the interlace weave, the hires
    /// and overscan normalisation of `finalize_frame`), in its
    /// colours at the master brightness. A pixel of the layer is therefore
    /// the composite's pixel wherever the layer is the one shown and no
    /// colour math touches it. It is drawn whatever the main and sub
    /// screen designations, the window clips and the debugger's layer
    /// mask say, which choose what the composite shows of a layer rather
    /// than what the layer holds; a background the mode has no plane for
    /// comes out empty. Colour math is not applied: its result belongs to
    /// two layers, or to a layer and the fixed colour, never to one layer
    /// alone. In forced blank the frame draws no layer, and the image is
    /// empty; at brightness 0 the layer's pixels are black, as the frame's.
    ///
    /// When the line table holds the frame ([`Self::draws_from_line_states`])
    /// each line is drawn with its own settings and the video memory it
    /// read, so a raster effect (an HDMA scroll or mode change down the
    /// frame) comes out as the frame drew it. Otherwise the layer is drawn
    /// from the registers as they stand, and such an effect comes out with
    /// the values of the last line. Emulation does not advance, the host
    /// framebuffer is not touched, and the state a savestate holds stays
    /// as it was, the frame's own display state with it.
    pub fn render_layer(&mut self, layer: usize) -> Result<PixelImage, ExportError> {
        if layer >= LAYER_COUNT {
            return Err(ExportError::OutOfRange);
        }
        let from_table = self.draws_from_line_states();
        self.isolated_layer = Some(layer);
        let image = self.picture_drawn_again(from_table, &mut NoTrace::<FrameTrace>::new());
        self.isolated_layer = None;
        Ok(image)
    }

    /// The frame's picture drawn again from its line table, into a buffer
    /// of its own, cropped as the picture is: each line with the settings
    /// it was drawn with and the video memory it read, then finished as
    /// the picture was. Equal to the picture the frame left, unless the
    /// debugger edited video memory since (the table then reports its
    /// memory as not exact) or the game redrew lines after the picture was
    /// finished.
    ///
    /// `None` when no line table is kept, or it does not hold a frame from
    /// its line 1 to its finished picture. Emulation does not advance, the
    /// host framebuffer is not touched, and the state a savestate holds
    /// stays as it was.
    pub fn picture_from_line_states(&mut self) -> Option<PixelImage> {
        let whole = self
            .recorders
            .lines
            .as_deref()
            .is_some_and(record::LineRecorder::holds_whole_frame);
        whole.then(|| self.picture_drawn_again(true, &mut NoTrace::<FrameTrace>::new()))
    }

    /// Draw the frame again into a buffer of its own and crop it to the
    /// picture's size, leaving the held frame's display state, and the
    /// state a savestate holds, as they were.
    pub(crate) fn picture_drawn_again<T: PixelTrace<Notes = FrameTrace>>(
        &mut self,
        from_table: bool,
        trace: &mut T,
    ) -> PixelImage {
        let keeps = RedrawKeeps::take(self);
        let picture = HeldPicture::take(self);
        let mut fb = Framebuffer::new(MAX_SCANLINE_WIDTH as u32, MAX_OUTPUT_ROWS as u32);
        let (width, height) = self.draw_frame_again(&mut fb, from_table, trace);
        picture.put_back(self);
        keeps.put_back(self);
        PixelImage {
            width,
            height,
            rgba: crop_rgba(&fb.data, fb.width, width, height),
        }
    }

    /// Draw the frame again into `fb` and finish it, from the line table
    /// when `from_table` (the caller has checked that it holds the frame),
    /// else from the registers as they stand; return the size of the
    /// picture. The registers and the line table are left as they were.
    /// A `trace` that takes notes has its planes normalised as the picture
    /// is, while the frame's own display state still stands.
    fn draw_frame_again<T: PixelTrace<Notes = FrameTrace>>(
        &mut self,
        fb: &mut Framebuffer,
        from_table: bool,
        trace: &mut T,
    ) -> (u32, u32) {
        // Taken out for the length of the redraw, so that the lines drawn
        // again are not recorded over the frame's own.
        let mut table = self.recorders.lines.take();
        let live = match table.as_deref_mut() {
            Some(table) if from_table => Some(self.draw_lines_from_table(table, fb, trace)),
            _ => {
                for scanline in 1..FIRST_LINE_PAST_OVERSCAN {
                    self.draw_scanline(scanline, fb, trace);
                }
                None
            }
        };
        self.finalize_frame(fb);
        if T::ON {
            trace.notes().normalize(&PictureShape::of(self));
        }
        let size = self.active_display_size();
        if let Some(live) = live {
            live.apply(self);
        }
        self.recorders.lines = table;
        size
    }

    /// Draw every line of `table` again, each with the settings it was
    /// drawn with: what video memory took since the last line logged
    /// first, then memory wound back to the first line, moved forward as
    /// the frame moved it, line by line, and on to the end of the log,
    /// which leaves it as it was. Returns the settings as they stood
    /// before, for the caller to put back once the picture is finished;
    /// until then the overscan bit is the one the picture was finished
    /// with.
    fn draw_lines_from_table<T: PixelTrace<Notes = FrameTrace>>(
        &mut self,
        table: &mut record::LineRecorder,
        fb: &mut Framebuffer,
        trace: &mut T,
    ) -> record::LineState {
        let live = record::LineState::capture(self);
        table.catch_up(&self.video_memory_now());
        let mut cursor = record::ReplayCursor::default();
        if table.rewind(&mut self.video_memory()) {
            self.vram_moved();
        }
        for index in 0..table.lines().len() {
            if table.replay_through(index, &mut cursor, &mut self.video_memory()) {
                self.vram_moved();
            }
            let (line, state) = table.lines()[index];
            state.apply(self);
            self.draw_scanline(line, fb, trace);
        }
        if table.replay_through(usize::MAX, &mut cursor, &mut self.video_memory()) {
            self.vram_moved();
        }
        // Memory stands as the table's copy holds it again, under a VRAM
        // generation of its own.
        table.memory_put_back(self.vram_generation);
        self.overscan = table.finished_overscan();
        live
    }

    /// VRAM changed outside its port: the rows and tiles decoded from it
    /// are rebuilt the next time they are read.
    fn vram_moved(&mut self) {
        self.vram_generation = self.vram_generation.wrapping_add(1);
    }

    /// One output row of [`Self::render_layer`]: the isolated layer's
    /// pixels, the rest of the row transparent. Mirrors
    /// [`Self::render_output_line`] up to the composite: the same row
    /// fetch, the same mosaic replay (kept in `previous_bg_rows` for the
    /// pass), the same 256-dot sprite row widened to a hires line, and a
    /// pseudo-hires line's dots each two columns wide.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_isolated_line<T: PixelTrace<Notes = FrameTrace>>(
        &mut self,
        layer: usize,
        vy: u16,
        obj_line: u16,
        field: bool,
        fb_row: u32,
        fb: &mut Framebuffer,
        trace: &mut T,
    ) {
        let hires = is_hires_mode(self.bgmode);
        let output_width = if hires {
            MAX_SCANLINE_WIDTH
        } else {
            NORMAL_SCANLINE_WIDTH
        };
        // A pseudo-hires line is written 512 columns wide, two a dot.
        let pseudo = self.pseudo_hires && !hires;
        let line_width = if pseudo {
            MAX_SCANLINE_WIDTH
        } else {
            output_width
        };
        let stride = fb.width as usize * 4;
        let base = stride * fb_row as usize;
        let visible_w = (fb.width as usize).min(line_width);
        fb.data[base..base + visible_w * 4].fill(0);

        if self.display_disable {
            for bg in 0..4 {
                self.previous_bg_rows[bg] = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
            }
            return;
        }
        let brightness = self.display_brightness;
        let mut put = |x: usize, colour: u16| {
            let [r, g, b] = if brightness == 0 {
                [0, 0, 0]
            } else {
                bgr15_to_rgb8_with_brightness(colour, brightness)
            };
            let p = base + x * 4;
            fb.data[p..p + 4].copy_from_slice(&[r, g, b, 0xFF]);
        };

        if layer == LAYER_OBJ {
            let mut obj_row = [ObjRowPixel::TRANSPARENT; NORMAL_SCANLINE_WIDTH];
            self.render_obj_row(obj_line, field, &mut obj_row, trace);
            for x in 0..visible_w {
                let column = if hires || pseudo { x >> 1 } else { x };
                let pixel = obj_row[column];
                // A sprite render draws the chosen sprites alone.
                if pixel.opaque && (!T::ON || trace.notes().draws_sprite_at(column)) {
                    put(x, pixel.color);
                }
            }
            return;
        }

        let mut row = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
        if self.bgmode == 7 {
            // The affine plane is BG1, and BG2 too with EXTBG; there is
            // no BG3 or BG4 in Mode 7.
            let mut rows = [[BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH]; 4];
            self.render_mode7_rows(
                vy,
                &mut rows,
                layer == 0,
                layer == 1 && self.mode7_extbg,
                trace,
            );
            row = rows[layer];
        } else if bg_visible_in_mode(self.bgmode, layer) {
            self.render_bg_row(layer, vy, &mut row, hires, trace);
        }
        if T::ON {
            trace.notes().begin_main_row(layer);
        }
        self.apply_mosaic_for_bg(Screen::Main, layer, vy, &mut row, output_width, trace);
        self.previous_bg_rows[layer] = row;
        if T::ON {
            trace.notes().keep_previous_row(layer);
        }
        for x in 0..visible_w {
            let pixel = row[if pseudo { x >> 1 } else { x }];
            if pixel.opaque {
                put(x, pixel.color);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ppu::sprite::tests::set_sprite;

    // -- Redraws: repaint and single layers ------------------------

    /// Mode 1 with BG1 and sprites on the main screen over a green
    /// backdrop. BG1's tilemap entry (0, 0) is tile 1, solid colour 1
    /// (red); every other entry is tile 0, empty. Sprite 0 is 8x8 at
    /// (16, 0), solid colour 1 of palette 0 (blue); the other 127 sit on
    /// line 100 at x 200, enough to overflow it.
    fn layered_ppu() -> Ppu {
        let mut ppu = Ppu::new();
        ppu.display_disable = false;
        ppu.display_brightness = 15;
        ppu.bgmode = 1;
        ppu.bg_main_enable[0] = true;
        ppu.obj_main_enable = true;
        ppu.bg_screen_addr[0] = 0x0000;
        ppu.bg_tiledata_addr[0] = 0x2000;
        ppu.bg_voffset[0] = 0xFFFF;
        ppu.vram[0] = 0x0001;
        for row in 0..8 {
            // Tile 1 (4bpp, words 0x1010..) and sprite tile 0x40
            // (words 0x400..): plane 0 set, colour 1.
            ppu.vram[0x1010 + row] = 0x00FF;
            ppu.vram[0x400 + row] = 0x00FF;
        }
        ppu.cgram[0] = 0x03E0; // backdrop green
        ppu.cgram[1] = 0x001F; // BG1 red
        ppu.cgram[129] = 0x7C00; // sprite blue
        set_sprite(&mut ppu, 0, 16, 0, 0x40, 0);
        for i in 1..128 {
            set_sprite(&mut ppu, i, 200, 100, 0x40, 0);
        }
        ppu
    }

    /// The alpha of every pixel of an RGBA image.
    fn alphas(rgba: &[u8]) -> impl Iterator<Item = u8> + '_ {
        rgba.iter().skip(3).step_by(4).copied()
    }

    /// The RGBA of pixel `(x, y)` of an image `width` pixels wide.
    fn rgba_at(rgba: &[u8], width: usize, x: usize, y: usize) -> [u8; 4] {
        let at = (y * width + x) * 4;
        [rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]]
    }

    #[test]
    fn render_layer_keys_everything_but_the_layer_to_alpha_0() {
        let mut ppu = layered_ppu();
        let bg1 = ppu.render_layer(0).expect("BG1");
        assert_eq!((bg1.width, bg1.height), (256, 224));
        let w = bg1.width as usize;
        // Tile 1 covers (0..8, 0..8) in BG1's red; nothing else of BG1,
        // and neither the sprite nor the backdrop, shows.
        assert_eq!(rgba_at(&bg1.rgba, w, 0, 0), [0xFF, 0x00, 0x00, 0xFF]);
        assert_eq!(rgba_at(&bg1.rgba, w, 7, 7), [0xFF, 0x00, 0x00, 0xFF]);
        assert_eq!(rgba_at(&bg1.rgba, w, 8, 0)[3], 0, "past the tile");
        assert_eq!(rgba_at(&bg1.rgba, w, 16, 0)[3], 0, "the sprite");
        assert_eq!(rgba_at(&bg1.rgba, w, 100, 100)[3], 0, "the backdrop");

        let obj = ppu.render_layer(4).expect("OBJ");
        assert_eq!(rgba_at(&obj.rgba, w, 16, 0), [0x00, 0x00, 0xFF, 0xFF]);
        assert_eq!(rgba_at(&obj.rgba, w, 0, 0)[3], 0, "BG1's tile");

        // A background the mode has no plane for comes out empty.
        let bg4 = ppu.render_layer(3).expect("BG4");
        assert!(alphas(&bg4.rgba).all(|alpha| alpha == 0));
        assert_eq!(ppu.render_layer(5), Err(ExportError::OutOfRange));
    }

    #[test]
    fn render_layer_is_the_composite_where_the_layer_shows() {
        let mut ppu = layered_ppu();
        let mut fb = Framebuffer::new(256, 224);
        ppu.redraw_frame(&mut fb);
        for layer in [0usize, 4] {
            let image = ppu.render_layer(layer).expect("layer");
            for at in (0..image.rgba.len()).step_by(4) {
                if image.rgba[at + 3] != 0 {
                    // The two layers do not overlap, so where one has a
                    // pixel it is the one shown.
                    assert_eq!(image.rgba[at..at + 4], fb.data[at..at + 4]);
                }
            }
        }
    }

    #[test]
    fn render_layer_draws_a_layer_whatever_the_screens_and_the_debug_mask() {
        let mut ppu = layered_ppu();
        ppu.bg_main_enable[0] = false;
        ppu.set_layer_debug_disabled(0, true);
        let bg1 = ppu.render_layer(0).expect("BG1");
        assert_eq!(rgba_at(&bg1.rgba, 256, 0, 0), [0xFF, 0x00, 0x00, 0xFF]);
        // In forced blank the frame draws no layer.
        ppu.display_disable = true;
        let blank = ppu.render_layer(0).expect("BG1");
        assert!(alphas(&blank.rgba).all(|alpha| alpha == 0));
    }

    #[test]
    fn redraws_put_back_the_sprite_flags_and_the_oam_address() {
        let mut ppu = layered_ppu();
        // As the end of a frame might leave them: flags clear (the game's
        // sprites moved in the vertical blank, say) and the OAM address
        // re-anchored.
        ppu.sprite_range_over = false;
        ppu.sprite_time_over = false;
        ppu.oam_iaddr = 0x0123;
        let mut fb = Framebuffer::new(256, 224);
        ppu.redraw_frame(&mut fb);
        assert!(
            !ppu.sprite_range_over && !ppu.sprite_time_over,
            "the redraw's overflow on line 100 is not kept"
        );
        assert_eq!(ppu.oam_iaddr, 0x0123);

        ppu.render_layer(4).expect("OBJ");
        assert!(!ppu.sprite_range_over && !ppu.sprite_time_over);
        assert_eq!(ppu.oam_iaddr, 0x0123);
    }

    #[test]
    fn render_layer_leaves_the_held_frame_display_state() {
        let mut ppu = layered_ppu();
        ppu.frame_hires = true;
        ppu.render_interlace = true;
        ppu.line_hires[3] = true;
        ppu.previous_bg_rows[0][5] = BgRowPixel {
            color: 0x1234,
            opaque: true,
            priority: true,
        };
        ppu.render_layer(0).expect("BG1");
        assert!(ppu.frame_hires && ppu.render_interlace && ppu.line_hires[3]);
        assert_eq!(ppu.previous_bg_rows[0][5].color, 0x1234);
        assert_eq!(ppu.isolated_layer, None);
    }
}
