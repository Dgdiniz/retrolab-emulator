//! Where each pixel of the held frame came from: a traced redraw.
//!
//! Nothing here records while frames run. A host that asks where the
//! pixels of the held frame came from gets a redraw of it, from its line
//! table when the table holds it (each line with its own settings and the
//! video memory it read, as [`Ppu::picture_from_line_states`] draws), that
//! notes, as each pixel is drawn, which layer drew it and from which
//! character, map entry, sprite and colour. The notes are planes, one value
//! a pixel, normalised and cropped exactly as the picture is, so a plane's
//! value at a pixel describes the picture's pixel there.
//!
//! The renderer is generic over a [`PixelTrace`] whose notes are a
//! `FrameTrace`: the frame loops draw with
//! [`NoTrace`](retro_core::capture::NoTrace), whose additions the compiler
//! removes, and only a redraw draws with a [`FrameTrace`]. A redraw leaves
//! the framebuffer, the video memory and everything a savestate holds as
//! they were.

use retro_core::capture::{drawn_by, layer, planes, tile_value, PixelSources, PixelTrace, NO_CELL};
use retro_core::video::PixelImage;

use super::render::{
    PixelSource, ScreenRow, MAX_OUTPUT_ROWS, MAX_SCANLINE_WIDTH, NORMAL_SCANLINE_WIDTH,
};
use super::tile::Mode7Sample;
use super::{Ppu, LAYER_OBJ};

/// The values and bits of the `layer` plane: bits 0-2 by the shared rule
/// ([`layer`], [`drawn_by`]), bits 3-15 this core's own.
///
/// | Bits | Meaning |
/// |---|---|
/// | 0-2 | What drew the pixel: 0 the backdrop, 1-4 BG1-BG4, 5 the sprites (each layer's id plus one), 7 nothing (forced blank, brightness 0, or a line not drawn) |
/// | 3 | The sub screen's pixel is shown (the even columns of a hires line) |
/// | 4-6 | What colour math combined it with: 0 the other screen's backdrop, 1-4 BG1-BG4, 5 the sprites, 6 the fixed colour, 7 nothing (no colour math) |
/// | 7 | Colour math was applied |
/// | 8 | Its result was halved |
/// | 9 | It subtracted |
/// | 10 | The colour window blacked the shown pixel before colour math |
/// | 11 | Direct colour: the pixel's bits are its colour, no palette read |
/// | 12-13 | Priority: a background tile's priority bit in bit 12; a sprite's priority, 0-3 |
/// | 14 | Mosaic: the pixel repeats its block's first pixel |
/// | 15 | Offset-per-tile replaced the scroll of the pixel's tile column |
pub mod pixel_layer {
    use retro_core::capture::{drawn_by, layer};

    use crate::ppu::LAYER_OBJ;

    /// The sprites drew the pixel.
    pub const SPRITES: u16 = drawn_by(LAYER_OBJ as u16);
    /// The sub screen's pixel is shown.
    pub const SUB_SCREEN_SHOWN: u16 = 1 << 3;
    /// Where the colour math addend sits.
    pub const ADDEND_SHIFT: u16 = 4;
    /// The colour math addend, bits 4-6.
    pub const ADDEND: u16 = 0x0070;
    /// The addend was the fixed colour: a colour no layer drew.
    pub const ADDEND_FIXED_COLOUR: u16 = layer::UNLAYERED_COLOUR;
    /// No colour math ran.
    pub const ADDEND_NONE: u16 = layer::NOTHING;
    /// Colour math was applied.
    pub const MATH_APPLIED: u16 = 1 << 7;
    /// The result was halved.
    pub const HALVED: u16 = 1 << 8;
    /// Colour math subtracted.
    pub const SUBTRACTED: u16 = 1 << 9;
    /// The colour window blacked the shown pixel.
    pub const BLACKED: u16 = 1 << 10;
    /// Direct colour.
    pub const DIRECT_COLOUR: u16 = 1 << 11;
    /// Where the priority sits.
    pub const PRIORITY_SHIFT: u16 = 12;
    /// The priority, bits 12-13.
    pub const PRIORITY: u16 = 0x3000;
    /// The pixel repeats its mosaic block's first pixel.
    pub const MOSAIC_REPEAT: u16 = 1 << 14;
    /// Offset-per-tile replaced the pixel's scroll.
    pub const OFFSET_REPLACED: u16 = 1 << 15;
}

/// A traced redraw of the held frame ([`Ppu::pixel_sources`]): one value a
/// pixel in each plane asked for, row after row, `width` x `height`, the
/// picture's own size and crop, and the picture the pass drew, which the
/// emulator compares with the picture the host holds before it hands the
/// planes out as [`PixelSources`] ([`Self::into_sources`]).
///
/// - `layer`: see [`pixel_layer`].
/// - `colour`: bits 0-7 the CGRAM index the pixel read (0 for the
///   backdrop), or with direct colour the pixel's own eight colour bits;
///   bits 8-10 the palette number of its tile or sprite; bits 11-12 its
///   bits per pixel, 0 none (the backdrop), 1 two, 2 four, 3 eight (Mode 7
///   reads eight-bit characters, its second background included).
/// - `tile`: [`tile_value`] of the character: the VRAM word where it starts
///   (a sprite's 4bpp character at its number times 16; a Mode 7 character
///   at its number times 64, its pixels in the high bytes), the character
///   number read (after a 16x16 or hires pick; a sprite's eleven bits; a
///   Mode 7 character's eight), the row and the column read within it,
///   after flips. 0 for a pixel no character drew.
/// - `cell`: a background pixel's map entry, as its VRAM word; a sprite
///   pixel's slot in the sprite table, 0-127; a Mode 7 pixel's plane cell
///   (`row * 128 + column`, the VRAM word whose low byte names the
///   character) in bits 0-13, with bit 14 set when the pixel lies outside
///   the 1024x1024 plane (the cell is then the wrapped one, or 0 where the
///   screen-over forced character 0); [`NO_CELL`] otherwise.
/// - `sub_layer`: what the sub screen holds at the pixel, with the codes of
///   `layer`'s bits 0-2 (its backdrop is the fixed colour in the normal
///   modes, CGRAM 0 in hires), or 7 where the line built no sub screen
///   (no colour math and not hires, so it took no part in the picture).
/// - `sub_colour`: the sub screen's colour there, BGR555 as CGRAM stores
///   it; 0 where `sub_layer` is 7.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TracedFrame {
    /// The picture's width in pixels.
    pub width: u32,
    /// The picture's height in pixels.
    pub height: u32,
    /// The pass drew from the frame's line table; otherwise from the
    /// registers and the memory as they stand.
    pub from_line_states: bool,
    /// The pass's own picture, RGBA, the size of the planes: equal to the
    /// held picture when the pass describes it.
    pub picture: Vec<u8>,
    /// The `layer` plane, when asked for.
    pub layer: Option<Vec<u16>>,
    /// The `colour` plane, when asked for.
    pub colour: Option<Vec<u16>>,
    /// The `tile` plane, when asked for.
    pub tile: Option<Vec<u32>>,
    /// The `cell` plane, when asked for.
    pub cell: Option<Vec<u16>>,
    /// The `sub_layer` plane, when the sub screen was asked for.
    pub sub_layer: Option<Vec<u8>>,
    /// The `sub_colour` plane, when the sub screen was asked for.
    pub sub_colour: Option<Vec<u16>>,
}

impl TracedFrame {
    /// The planes as a host reads them, for the picture numbered `frame`;
    /// `matches_picture` says the pass's own picture is the one the host
    /// holds, so the planes describe it.
    pub fn into_sources(self, frame: u64, matches_picture: bool) -> PixelSources {
        PixelSources {
            frame,
            width: self.width,
            height: self.height,
            from_line_states: self.from_line_states,
            matches_picture,
            layer: self.layer,
            colour: self.colour,
            tile: self.tile,
            cell: self.cell,
            sub_layer: self.sub_layer,
            sub_colour: self.sub_colour,
        }
    }
}

/// Where one background pixel came from, as it was fetched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct BgOrigin {
    /// The `tile` plane's value.
    pub(crate) tile: u32,
    /// The `cell` plane's value.
    pub(crate) cell: u16,
    /// The `colour` plane's value.
    pub(crate) colour: u16,
    /// The `layer` plane's bits the pixel carries: direct colour, its
    /// priority, mosaic and offset-per-tile.
    pub(crate) flags: u16,
}

/// Where one pixel of the sprite layer came from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ObjOrigin {
    /// The `tile` plane's value.
    pub(crate) tile: u32,
    /// The `colour` plane's value.
    pub(crate) colour: u16,
    /// The sprite's slot in the table.
    pub(crate) slot: u8,
    /// The sprite's priority, 0-3.
    pub(crate) priority: u8,
}

/// What colour math did to one column (see `Ppu::resolve_pixel_explained`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MathOutcome {
    /// The colour window blacked the shown pixel.
    pub(crate) blacked: bool,
    /// Colour math ran.
    pub(crate) applied: bool,
    /// Its result was halved.
    pub(crate) halved: bool,
    /// It subtracted.
    pub(crate) subtracted: bool,
    /// The addend, with the codes of the `layer` plane's bits 4-6.
    pub(crate) addend: u8,
}

impl MathOutcome {
    /// No colour math.
    pub(crate) const NONE: Self = Self {
        blacked: false,
        applied: false,
        halved: false,
        subtracted: false,
        addend: pixel_layer::ADDEND_NONE as u8,
    };

    /// The `layer` plane's bits this outcome sets.
    fn layer_bits(self) -> u16 {
        let mut bits = u16::from(self.addend) << pixel_layer::ADDEND_SHIFT;
        if self.applied {
            bits |= pixel_layer::MATH_APPLIED;
            if self.halved {
                bits |= pixel_layer::HALVED;
            }
            if self.subtracted {
                bits |= pixel_layer::SUBTRACTED;
            }
        }
        if self.blacked {
            bits |= pixel_layer::BLACKED;
        }
        bits
    }
}

/// The `layer` code of a composited pixel's source: 0 the backdrop, 1-4
/// BG1-BG4, 5 the sprites.
pub(crate) fn source_code(source: PixelSource) -> u16 {
    match source {
        PixelSource::Backdrop => layer::BACKDROP,
        PixelSource::Bg(id) => drawn_by(u16::from(id)),
        PixelSource::Obj => pixel_layer::SPRITES,
    }
}

/// The `colour` plane's value: CGRAM index (or direct-colour bits)
/// `index`, palette `palette`, at `bpp` bits per pixel.
#[inline]
pub(crate) fn colour_value(index: u8, palette: u8, bpp: u8) -> u16 {
    let bpp_code: u16 = match bpp {
        2 => 1,
        4 => 2,
        8 => 3,
        _ => 0,
    };
    u16::from(index) | (u16::from(palette & 7) << 8) | (bpp_code << 11)
}

/// Where one Mode 7 pixel came from: `sample` the plane's sample, `index`
/// the colour index the layer read from it (the whole byte for BG1, its
/// low seven bits for the second background), `direct` whether the byte is
/// its colour, `priority` the second background's priority bit.
pub(crate) fn mode7_origin(
    sample: Mode7Sample,
    index: u8,
    direct: bool,
    priority: bool,
) -> BgOrigin {
    let character = u32::from(sample.character);
    let mut flags = 0;
    if direct {
        flags |= pixel_layer::DIRECT_COLOUR;
    }
    if priority {
        flags |= 1 << pixel_layer::PRIORITY_SHIFT;
    }
    BgOrigin {
        tile: tile_value(
            character * 64,
            character,
            u32::from(sample.row),
            u32::from(sample.column),
        ),
        cell: sample.cell | if sample.outside { MODE7_OUTSIDE } else { 0 },
        colour: colour_value(index, 0, 8),
        flags,
    }
}

/// The `cell` bit of a Mode 7 pixel outside the 1024x1024 plane.
pub const MODE7_OUTSIDE: u16 = 1 << 14;

/// Rows of origins, one per background, as wide as the widest line.
type OriginRows = [[BgOrigin; MAX_SCANLINE_WIDTH]; 4];

/// A heap block of `N` copies of `value`, as a fixed-size array.
fn boxed<T: Copy, const N: usize>(value: T) -> Box<[T; N]> {
    vec![value; N]
        .into_boxed_slice()
        .try_into()
        .unwrap_or_else(|_| unreachable!("the vector holds exactly N values"))
}

/// The notes a traced redraw takes: the origins of the line being drawn
/// and the planes of the whole frame, one row per output row, 512 wide,
/// before the picture's normalisation.
pub(crate) struct FrameTrace {
    /// Each background's pixels as the line fetched them, before mosaic:
    /// the sub screen's.
    pub(crate) fetch: Box<OriginRows>,
    /// Each background's pixels on the main screen, after mosaic.
    pub(crate) main: Box<OriginRows>,
    /// The main screen's origins of the line before, which vertical mosaic
    /// repeats.
    previous: Box<OriginRows>,
    /// The sprite layer's pixels, by column.
    pub(crate) obj: Box<[ObjOrigin; NORMAL_SCANLINE_WIDTH]>,
    /// What colour math did to each column of the line.
    pub(crate) math: Box<[MathOutcome; MAX_SCANLINE_WIDTH]>,
    layer: Vec<u16>,
    colour: Vec<u16>,
    tile: Vec<u32>,
    cell: Vec<u16>,
    sub_layer: Vec<u8>,
    sub_colour: Vec<u16>,
    /// The sprites a sprite render draws, by slot; `None` draws them all.
    sprite_filter: Option<[bool; 128]>,
}

impl PixelTrace for FrameTrace {
    type Notes = FrameTrace;
    const ON: bool = true;
    fn notes(&mut self) -> &mut FrameTrace {
        self
    }
}

/// The `layer` value of a pixel nothing drew: no layer, no colour math.
const NOTHING_LAYER: u16 = layer::NOTHING | (pixel_layer::ADDEND_NONE << pixel_layer::ADDEND_SHIFT);

impl FrameTrace {
    /// Notes for the planes in `asked` (a mask of [`planes`] bits); a
    /// sprite render passes the sprites it keeps.
    pub(crate) fn new(asked: u32, sprite_filter: Option<[bool; 128]>) -> Self {
        let size = MAX_SCANLINE_WIDTH * MAX_OUTPUT_ROWS;
        let plane = |bit: u32| asked & bit != 0;
        Self {
            fetch: boxed([BgOrigin::default(); MAX_SCANLINE_WIDTH]),
            main: boxed([BgOrigin::default(); MAX_SCANLINE_WIDTH]),
            previous: boxed([BgOrigin::default(); MAX_SCANLINE_WIDTH]),
            obj: boxed(ObjOrigin::default()),
            math: boxed(MathOutcome::NONE),
            layer: if plane(planes::LAYER) {
                vec![NOTHING_LAYER; size]
            } else {
                Vec::new()
            },
            colour: if plane(planes::COLOUR) {
                vec![0; size]
            } else {
                Vec::new()
            },
            tile: if plane(planes::TILE) {
                vec![0; size]
            } else {
                Vec::new()
            },
            cell: if plane(planes::CELL) {
                vec![NO_CELL; size]
            } else {
                Vec::new()
            },
            sub_layer: if plane(planes::SUB_SCREEN) {
                vec![layer::NOTHING as u8; size]
            } else {
                Vec::new()
            },
            sub_colour: if plane(planes::SUB_SCREEN) {
                vec![0; size]
            } else {
                Vec::new()
            },
            sprite_filter,
        }
    }

    /// Note every plane asked for at `at`.
    #[allow(clippy::too_many_arguments)]
    fn store(
        &mut self,
        at: usize,
        layer: u16,
        colour: u16,
        tile: u32,
        cell: u16,
        sub_layer: u8,
        sub_colour: u16,
    ) {
        if let Some(value) = self.layer.get_mut(at) {
            *value = layer;
        }
        if let Some(value) = self.colour.get_mut(at) {
            *value = colour;
        }
        if let Some(value) = self.tile.get_mut(at) {
            *value = tile;
        }
        if let Some(value) = self.cell.get_mut(at) {
            *value = cell;
        }
        if let Some(value) = self.sub_layer.get_mut(at) {
            *value = sub_layer;
        }
        if let Some(value) = self.sub_colour.get_mut(at) {
            *value = sub_colour;
        }
    }

    /// Output row `fb_row` was drawn black, `width` wide: forced blank or
    /// brightness 0, nothing drew it.
    pub(crate) fn note_blank_row(&mut self, fb_row: u32, width: usize) {
        let base = fb_row as usize * MAX_SCANLINE_WIDTH;
        for x in 0..width {
            self.store(
                base + x,
                NOTHING_LAYER,
                0,
                0,
                NO_CELL,
                layer::NOTHING as u8,
                0,
            );
        }
    }

    /// The backgrounds are fetched: the main screen starts from the
    /// fetched origins, which its mosaic then rearranges.
    pub(crate) fn begin_main_screen(&mut self) {
        *self.main = *self.fetch;
    }

    /// The main screen's rows of this line are what vertical mosaic
    /// repeats on the next.
    pub(crate) fn keep_previous(&mut self) {
        *self.previous = *self.main;
    }

    /// The main screen of background `bg` repeats the line before's,
    /// every pixel of it a repeat.
    pub(crate) fn repeat_previous(&mut self, bg: usize) {
        for (origin, previous) in self.main[bg].iter_mut().zip(self.previous[bg].iter()) {
            *origin = *previous;
            origin.flags |= pixel_layer::MOSAIC_REPEAT;
        }
    }

    /// The pixels `start + 1..end` of background `bg` on the main screen
    /// repeat the one at `start`.
    pub(crate) fn repeat_block(&mut self, bg: usize, start: usize, end: usize) {
        let mut first = self.main[bg][start];
        first.flags |= pixel_layer::MOSAIC_REPEAT;
        for origin in &mut self.main[bg][start + 1..end] {
            *origin = first;
        }
    }

    /// One background row of a single-layer render is fetched: its main
    /// screen starts from it.
    pub(crate) fn begin_main_row(&mut self, bg: usize) {
        self.main[bg] = self.fetch[bg];
    }

    /// A single-layer render's row of background `bg` is what vertical
    /// mosaic repeats on the next line.
    pub(crate) fn keep_previous_row(&mut self, bg: usize) {
        self.previous[bg] = self.main[bg];
    }

    /// Whether a sprite render draws the sprite pixel at `column`.
    pub(crate) fn draws_sprite_at(&self, column: usize) -> bool {
        self.sprite_filter
            .is_none_or(|filter| filter[usize::from(self.obj[column].slot & 0x7F)])
    }

    /// Note output row `fb_row`, `width` wide: `main` the main screen's
    /// composite, `sub` the sub screen's when the line built one (a hires
    /// line shows it on its even columns), `math` whether colour math ran
    /// (its outcome per column in [`Self::math`]).
    pub(crate) fn note_row(
        &mut self,
        fb_row: u32,
        width: usize,
        hires: bool,
        main: &ScreenRow,
        sub: Option<&ScreenRow>,
        math: bool,
    ) {
        let base = fb_row as usize * MAX_SCANLINE_WIDTH;
        for x in 0..width {
            let sub_shown = hires && x & 1 == 0 && sub.is_some();
            let (mut layer, colour, tile, cell) = match (sub_shown, sub) {
                (true, Some(sub)) => {
                    let (layer, colour, tile, cell) =
                        self.pixel_values(sub[x].source, x, hires, false);
                    (layer | pixel_layer::SUB_SCREEN_SHOWN, colour, tile, cell)
                }
                _ => self.pixel_values(main[x].source, x, hires, true),
            };
            layer |= if math {
                self.math[x].layer_bits()
            } else {
                MathOutcome::NONE.layer_bits()
            };
            let (sub_layer, sub_colour) = match sub {
                Some(sub) => (source_code(sub[x].source) as u8, sub[x].color),
                None => (layer::NOTHING as u8, 0),
            };
            self.store(base + x, layer, colour, tile, cell, sub_layer, sub_colour);
        }
    }

    /// The `layer` (its source and the pixel's own bits), `colour`, `tile`
    /// and `cell` of a composited pixel from `source` at column `x`, of the
    /// main screen (`on_main`, its backgrounds mosaicked) or the sub.
    fn pixel_values(
        &self,
        source: PixelSource,
        x: usize,
        hires: bool,
        on_main: bool,
    ) -> (u16, u16, u32, u16) {
        match source {
            PixelSource::Backdrop => (layer::BACKDROP, 0, 0, NO_CELL),
            PixelSource::Bg(id) => {
                let origin = if on_main {
                    self.main[usize::from(id)][x]
                } else {
                    self.fetch[usize::from(id)][x]
                };
                (
                    source_code(source) | origin.flags,
                    origin.colour,
                    origin.tile,
                    origin.cell,
                )
            }
            PixelSource::Obj => {
                let origin = self.obj[if hires { x >> 1 } else { x }];
                (
                    pixel_layer::SPRITES
                        | (u16::from(origin.priority) << pixel_layer::PRIORITY_SHIFT),
                    origin.colour,
                    origin.tile,
                    u16::from(origin.slot),
                )
            }
        }
    }

    /// Normalise every plane as the picture was (see [`normalize_rows`]).
    pub(crate) fn normalize(&mut self, shape: &PictureShape) {
        normalize_rows(&mut self.layer, shape);
        normalize_rows(&mut self.colour, shape);
        normalize_rows(&mut self.tile, shape);
        normalize_rows(&mut self.cell, shape);
        normalize_rows(&mut self.sub_layer, shape);
        normalize_rows(&mut self.sub_colour, shape);
    }

    /// The planes cropped to `picture`'s size, beside it.
    fn into_traced(self, picture: PixelImage, from_line_states: bool) -> TracedFrame {
        let (width, height) = (picture.width as usize, picture.height as usize);
        fn crop<T: Copy>(plane: Vec<T>, width: usize, height: usize) -> Option<Vec<T>> {
            if plane.is_empty() {
                return None;
            }
            let mut out = Vec::with_capacity(width * height);
            for row in plane.chunks(MAX_SCANLINE_WIDTH).take(height) {
                out.extend_from_slice(&row[..width]);
            }
            Some(out)
        }
        TracedFrame {
            width: picture.width,
            height: picture.height,
            from_line_states,
            layer: crop(self.layer, width, height),
            colour: crop(self.colour, width, height),
            tile: crop(self.tile, width, height),
            cell: crop(self.cell, width, height),
            sub_layer: crop(self.sub_layer, width, height),
            sub_colour: crop(self.sub_colour, width, height),
            picture: picture.rgba,
        }
    }
}

/// What the picture's normalisation reads, taken once the frame is drawn:
/// the frame-global hires and interlace state, which output rows a hires
/// line drew, and the overscan crop.
pub(crate) struct PictureShape {
    frame_hires: bool,
    render_interlace: bool,
    line_hires: [bool; MAX_OUTPUT_ROWS],
    overscan: bool,
    show_overscan: bool,
}

impl PictureShape {
    /// The shape the PPU's last drawn frame has, as `finalize_frame` reads
    /// it.
    pub(crate) fn of(ppu: &Ppu) -> Self {
        Self {
            frame_hires: ppu.frame_hires,
            render_interlace: ppu.render_interlace,
            line_hires: ppu.line_hires,
            overscan: ppu.overscan,
            show_overscan: ppu.show_overscan,
        }
    }
}

/// Normalise a plane drawn line by line, 512 values a row and 480 rows,
/// exactly as `Ppu::finalize_frame` normalises the RGBA picture, so each
/// value lands where its pixel does: in a hires frame each row a lo-res
/// line drew is widened (value `x` to columns `2x` and `2x + 1`), a
/// non-interlaced hires frame's rows are doubled down, and an overscan
/// frame is cropped to the TV window unless the host shows the overscan.
/// An empty plane, one not asked for, is left alone.
pub(crate) fn normalize_rows<T: Copy>(data: &mut [T], shape: &PictureShape) {
    let stride = MAX_SCANLINE_WIDTH;
    if data.len() < stride * MAX_OUTPUT_ROWS {
        return;
    }
    if shape.frame_hires {
        let base_h = if shape.overscan { 240 } else { 224 };
        let source_rows = if shape.render_interlace {
            base_h * 2
        } else {
            base_h
        }
        .min(MAX_OUTPUT_ROWS);
        for row in 0..source_rows {
            if shape.line_hires[row] {
                continue;
            }
            let base = row * stride;
            for x in (0..NORMAL_SCANLINE_WIDTH).rev() {
                let value = data[base + x];
                data[base + x * 2 + 1] = value;
                data[base + x * 2] = value;
            }
        }
        if !shape.render_interlace {
            let target_rows = (base_h * 2).min(MAX_OUTPUT_ROWS);
            for k in (0..base_h).rev() {
                let d0 = 2 * k;
                if d0 >= target_rows {
                    continue;
                }
                let src = k * stride;
                let d1 = d0 + 1;
                if d1 < target_rows {
                    data.copy_within(src..src + stride, d1 * stride);
                }
                data.copy_within(src..src + stride, d0 * stride);
            }
        }
    }
    if shape.overscan && !shape.show_overscan {
        let (offset_rows, shown_rows) = if shape.frame_hires {
            (14, 448)
        } else {
            (7, 224)
        };
        let copy_rows = shown_rows.min(MAX_OUTPUT_ROWS - offset_rows);
        data.copy_within(offset_rows * stride..(offset_rows + copy_rows) * stride, 0);
    }
}

impl Ppu {
    /// Where each pixel of the held frame came from: the frame drawn again
    /// with notes taken, the planes in `asked` (a mask of [`planes`] bits;
    /// other bits are ignored) filled, normalised and cropped as the picture
    /// is (see [`TracedFrame`]).
    ///
    /// The frame is drawn as [`Self::picture_from_line_states`] draws it
    /// when the line table holds it ([`Self::draws_from_line_states`]):
    /// each line with its own settings and the video memory it read, the
    /// debugger's layer mask applied as it applies to the picture. With no
    /// such table it is drawn from the registers and the memory as they
    /// stand. The pass's own picture comes back with the planes, so a host
    /// can tell whether they describe the picture it holds. Emulation does
    /// not advance, the host framebuffer is not touched, and the state a
    /// savestate holds stays as it was.
    pub fn pixel_sources(&mut self, asked: u32) -> TracedFrame {
        let from_table = self.draws_from_line_states();
        let mut trace = FrameTrace::new(asked, None);
        let picture = self.picture_drawn_again(from_table, &mut trace);
        trace.into_traced(picture, from_table)
    }

    /// The chosen sprites of the held frame drawn alone, as RGBA the size of
    /// the frame's picture, alpha 0 wherever none of them drew a pixel.
    /// `selection` holds one flag a slot of the sprite table.
    ///
    /// The sprites are drawn as the frame drew them: after each line's
    /// sprite and tile limits, and after priority among sprites, so a
    /// chosen sprite shows only where it won its column of the sprite
    /// layer. Like [`Self::render_layer`]'s sprite layer, it is drawn at
    /// the master brightness, without colour math, whatever the screens,
    /// the windows and the debugger's layer mask say, each line from its
    /// own settings when the line table holds the frame. Emulation does not
    /// advance, the host framebuffer is not touched, and the state a
    /// savestate holds stays as it was.
    pub fn render_sprites(&mut self, selection: &[bool; 128]) -> PixelImage {
        let from_table = self.draws_from_line_states();
        let mut trace = FrameTrace::new(0, Some(*selection));
        self.isolated_layer = Some(LAYER_OBJ);
        let image = self.picture_drawn_again(from_table, &mut trace);
        self.isolated_layer = None;
        image
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::Framebuffer;

    /// A small deterministic generator, so a failure repeats.
    struct Numbers(u64);

    impl Numbers {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    #[test]
    fn the_planes_are_normalised_as_the_picture_is_in_every_case() {
        let mut numbers = Numbers(0x0DD5_EED5);
        for case in 0..16u32 {
            for _ in 0..4 {
                let mut ppu = Ppu::new();
                ppu.frame_hires = case & 1 != 0;
                ppu.render_interlace = case & 2 != 0;
                ppu.overscan = case & 4 != 0;
                ppu.show_overscan = case & 8 != 0;
                for row in ppu.line_hires.iter_mut() {
                    *row = numbers.next() & 1 != 0;
                }
                // Every pixel a value of its own, in the picture and in a
                // plane alike.
                let mut fb = Framebuffer::new(MAX_SCANLINE_WIDTH as u32, MAX_OUTPUT_ROWS as u32);
                let mut plane: Vec<u32> =
                    (0..(MAX_SCANLINE_WIDTH * MAX_OUTPUT_ROWS) as u32).collect();
                for (pixel, value) in fb.data.chunks_mut(4).zip(plane.iter()) {
                    pixel.copy_from_slice(&value.to_le_bytes());
                }
                ppu.finalize_frame(&mut fb);
                normalize_rows(&mut plane, &PictureShape::of(&ppu));
                let picture: Vec<u32> = fb
                    .data
                    .chunks(4)
                    .map(|pixel| u32::from_le_bytes(pixel.try_into().unwrap()))
                    .collect();
                assert_eq!(picture, plane, "case {case:04b}");
            }
        }
    }

    #[test]
    fn a_plane_not_asked_for_is_left_alone() {
        let ppu = Ppu::new();
        let mut empty: Vec<u16> = Vec::new();
        normalize_rows(&mut empty, &PictureShape::of(&ppu));
        assert!(empty.is_empty());
    }

    #[test]
    fn the_values_pack_as_documented() {
        assert_eq!(colour_value(0xAB, 5, 4), 0xAB | (5 << 8) | (2 << 11));
        assert_eq!(colour_value(0x12, 0, 0), 0x12);
        assert_eq!(
            MathOutcome {
                blacked: true,
                applied: true,
                halved: true,
                subtracted: true,
                addend: 3,
            }
            .layer_bits(),
            (3 << 4)
                | pixel_layer::MATH_APPLIED
                | pixel_layer::HALVED
                | pixel_layer::SUBTRACTED
                | pixel_layer::BLACKED
        );
        assert_eq!(MathOutcome::NONE.layer_bits(), 0x70);
    }
}
