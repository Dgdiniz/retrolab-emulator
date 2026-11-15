//! Where each pixel of the held frame came from, the sprites each line held
//! and what became of each, and the chosen sprites drawn alone: worked out
//! on demand, by drawing the frame again the way
//! [`Vdp::picture_from_line_states`] draws it (each line from the line table
//! where the table holds it, from the state as it is now otherwise) with a
//! trace that notes what the renderer read for each pixel.
//!
//! The renderer the frame loops run is the same code: `render_scanline`,
//! `composite_scanline` and `render_sprite_line` are generic over a
//! [`PixelTrace`] whose notes are a `LineTrace`, each note sits behind
//! `if T::ON`, and the frame loops pass [`NoTrace`](retro_core::capture::NoTrace),
//! so the renderer they run is built with no trace in it. Nothing is
//! recorded while frames run, and drawing again moves nothing a savestate
//! holds: what drawing changes is kept and put back, the framebuffer
//! included.
//!
//! The shapes and codes are retro-core's ([`PixelSources`], [`LineSprites`],
//! [`SpriteFate`], [`line_flags`]); what this core puts in them:
//!
//! - `layer`: bits 0-2 what drew the pixel ([`layer::BACKDROP`] the
//!   backdrop colour of register 7, [`DRAWN_BY_PLANE_A`],
//!   [`DRAWN_BY_PLANE_B`], [`DRAWN_BY_WINDOW`], [`DRAWN_BY_SPRITE`],
//!   [`layer::UNLAYERED_COLOUR`] a CRAM transfer running with the display
//!   off, which shows each colour it moves, two pixels a colour, and
//!   [`layer::NOTHING`]: the line was drawn with the display off, which
//!   shows the backdrop colour but no layer, or the line table holds no
//!   record of the line, so what drew it is not known); bit 3
//!   [`HIGH_PRIORITY`]; bits 4-5 the [`Shade`]; bits 6-7 the operator sprite
//!   over the pixel ([`OPERATOR_HIGHLIGHT`], [`OPERATOR_SHADOW`]); bit 8
//!   [`FLIPPED_ACROSS`] and bit 9 [`FLIPPED_DOWN`], how the character was
//!   read; bits 10-15 0. An operator sprite is no pixel: the pixel is the
//!   one under it, and bits 6-7 name the operator.
//! - `colour`: the CRAM index the pixel's colour came from, 0-63 (palette
//!   line * 16 + colour); a line drawn with the display off holds the
//!   backdrop's; a pixel a CRAM transfer drew holds the colour word the
//!   transfer moved (`0000 BBB0 GGG0 RRR0`), which no palette index names;
//!   a line the table holds no record of, 0.
//! - `tile`: [`tile_value`] of the character read (which starts at VRAM
//!   word character * 16) and of the row and column read in it, after
//!   flips; 0 where no character drew the pixel.
//! - `cell`: for a plane or window pixel, the VRAM word of its map entry;
//!   for a sprite pixel, the sprite's slot in the sprite table (0-79);
//!   [`NO_CELL`] otherwise.
//! - No sub screen: `sub_layer` and `sub_colour` are always `None`, and
//!   [`planes::SUB_SCREEN`] is ignored.
//! - The fates a line's sprites meet: [`SpriteFate::DrawnWhole`];
//!   [`SpriteFate::DrawnInPart`], the line's dots ran out inside it and only
//!   its first whole cells of the dots left were drawn;
//!   [`SpriteFate::Masked`], a sprite at x 0 of the table (screen x -128)
//!   after a sprite at any other x, or with the mask carried in, hides every
//!   sprite after it on the line; [`SpriteFate::OverLineLimit`], the 21st in
//!   the 40-cell mode, the 17th in the 32-cell mode, and every one after it;
//!   [`SpriteFate::LeftOut`], met after the dots ran out (320 in the 40-cell
//!   mode, 256 in the 32-cell mode); [`SpriteFate::OffScreen`]. The line
//!   flags: [`line_flags::OVER_LIMIT`] (20 sprites in the 40-cell mode, 16
//!   in the 32-cell mode), [`line_flags::OUT_OF_TIME`] (the dots) and
//!   [`line_flags::MASK_CARRIED_IN`] (the line above ran out of dots, which
//!   arms the sprite mask for this one from its first sprite); this core
//!   never sets [`line_flags::NOT_SHOWN`]. A line drawn with the display off
//!   evaluates no sprite: it has no entries.

use super::recording::{LineKind, LineRecord};
use super::sprites::Sprite;
use super::{
    DrawState, Vdp, FRAMEBUFFER_WIDTH, LAYER_COUNT, LAYER_PLANE_A, LAYER_PLANE_B, LAYER_SPRITES,
    LAYER_WINDOW,
};
use retro_core::capture::{
    drawn_by, layer, line_flags, planes, tile_value, LineSprites, PixelSources, PixelTrace,
    RenderedLayer, SpriteFate, LINE_SPRITES_STRIDE, NO_CELL,
};
use retro_core::video::{crop_rgba, equals_cropped_rgba, PixelImage};

/// The `layer` plane's bits 0-2 for a pixel plane A drew.
pub const DRAWN_BY_PLANE_A: u16 = drawn_by(LAYER_PLANE_A as u16);
/// The `layer` plane's bits 0-2 for a pixel plane B drew.
pub const DRAWN_BY_PLANE_B: u16 = drawn_by(LAYER_PLANE_B as u16);
/// The `layer` plane's bits 0-2 for a pixel the window drew.
pub const DRAWN_BY_WINDOW: u16 = drawn_by(LAYER_WINDOW as u16);
/// The `layer` plane's bits 0-2 for a pixel a sprite drew.
pub const DRAWN_BY_SPRITE: u16 = drawn_by(LAYER_SPRITES as u16);
/// The `layer` plane's bit 3: the pixel shown is a high-priority one, as
/// the composite resolved priority (the bit shadow and highlight read).
pub const HIGH_PRIORITY: u16 = 1 << 3;
/// Where the `layer` plane's two bits saying what shadow and highlight did
/// to the pixel start ([`Shade`]).
pub const SHADE_SHIFT: u32 = 4;
/// The `layer` plane's bits 4-5 ([`Shade`]).
pub const SHADE: u16 = 3 << SHADE_SHIFT;
/// Where the `layer` plane's two bits naming the operator sprite over the
/// pixel start: 0 none, [`OPERATOR_HIGHLIGHT`] or [`OPERATOR_SHADOW`].
pub const OPERATOR_SHIFT: u32 = 6;
/// The `layer` plane's bits 6-7: the operator sprite over the pixel.
pub const OPERATOR: u16 = 3 << OPERATOR_SHIFT;
/// The operator: palette line 3's colour 14, the highlight operator.
pub const OPERATOR_HIGHLIGHT: u16 = 1;
/// The operator: palette line 3's colour 15, the shadow operator. This core
/// shows the pixel under it as it is, neither shadowed nor highlighted, so
/// its [`Shade`] is `AsIs`.
pub const OPERATOR_SHADOW: u16 = 2;
/// The `layer` plane's bit 8: the character was read flipped across.
pub const FLIPPED_ACROSS: u16 = 1 << 8;
/// The `layer` plane's bit 9: the character was read flipped down.
pub const FLIPPED_DOWN: u16 = 1 << 9;

/// The slots of the sprite table in the 40-cell mode; the 32-cell mode
/// uses the first 64.
pub const SPRITE_TABLE_SLOTS: usize = 80;

/// The pixels in a line of the line buffers.
const LINE_WIDTH: usize = FRAMEBUFFER_WIDTH as usize;

/// What shadow and highlight did to a pixel's colour: the rule the
/// renderer colours each pixel by (`shade`), in the `layer` plane's bits
/// 4-5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shade {
    /// Its palette colour as it is.
    AsIs = 0,
    /// Halved: shadow.
    Shadowed = 1,
    /// Brightened: highlight.
    Highlighted = 2,
}

/// What shadow and highlight do to a pixel: with the mode on
/// (`shadow_highlight`, register 12 bit 3), the highlight operator
/// (`operator` 14) brightens it, a pixel under the shadow operator (15) or
/// of high priority keeps its colour, and any other is halved; with the
/// mode off every pixel keeps its colour.
#[inline(always)]
pub(super) fn shade(shadow_highlight: bool, operator: u8, high_priority: bool) -> Shade {
    if !shadow_highlight {
        Shade::AsIs
    } else if operator == 14 {
        Shade::Highlighted
    } else if operator == 15 || high_priority {
        Shade::AsIs
    } else {
        Shade::Shadowed
    }
}

/// The `tile` plane's value for a pixel read from character `pattern`, at
/// row `row` and column `column` of it, both after flips: the character
/// starts at VRAM word `pattern * 16` ([`tile_value`]).
fn character_tile(pattern: u16, row: u8, column: u8) -> u32 {
    let character = u32::from(pattern);
    tile_value(character * 16, character, u32::from(row), u32::from(column))
}

/// The flip bits of the `layer` plane for a character read flipped
/// `across` and `down`.
fn flip_bits(across: bool, down: bool) -> u16 {
    (if across { FLIPPED_ACROSS } else { 0 }) | (if down { FLIPPED_DOWN } else { 0 })
}

/// A sprite one line's evaluation met.
#[derive(Debug, Clone, Copy)]
struct MetSprite {
    slot: u8,
    x: i16,
    width: u8,
    masked: bool,
    /// The fate of a sprite met after the evaluation stopped; `None` for one
    /// it took, whose fate is worked out from where it lies and whether the
    /// dots ran out inside it.
    left_out: Option<SpriteFate>,
}

/// A sprite one line's evaluation met, with what became of it and the
/// columns of it the line drew on screen.
#[derive(Debug, Clone, Copy)]
struct SpriteEntry {
    sprite: MetSprite,
    fate: SpriteFate,
    drawn: u8,
}

/// What evaluating one line's sprites found: the line's flags and its
/// entries, in the order the evaluation met them.
#[derive(Debug, Clone, Default)]
struct LineEvaluation {
    flags: u16,
    entries: Vec<SpriteEntry>,
}

/// The origin of a pixel in the line's index buffers: the `layer` plane's
/// bits that come from what drew it (its code and its flips), its `tile`
/// value and its `cell`.
#[derive(Debug, Clone, Copy)]
pub(super) struct PenOrigin {
    layer: u16,
    tile: u32,
    cell: u16,
}

impl PenOrigin {
    /// The backdrop colour: no character, no cell.
    pub(super) const BACKDROP: Self = Self {
        layer: layer::BACKDROP,
        tile: 0,
        cell: NO_CELL,
    };

    /// A pixel of a plane or the window, `drawn_by`, read from the map
    /// entry `word` at byte `address` of VRAM, at column `column` and row
    /// `row` of the character before flips.
    #[inline(always)]
    pub(super) fn character(drawn_by: u16, word: u16, address: usize, column: u8, row: u8) -> Self {
        let across = (word & 0x0800) != 0;
        let down = (word & 0x1000) != 0;
        let column = if across { 7 - column } else { column };
        let row = if down { 7 - row } else { row };
        Self {
            layer: drawn_by | flip_bits(across, down),
            tile: character_tile(word & 0x07FF, row, column),
            cell: ((address >> 1) & 0x7FFF) as u16,
        }
    }
}

/// The notes of one line drawn with a trace ([`PixelTrace`]): where each
/// column's sprite pixel, composited pixel and shown pixel came from, and the
/// sprites the line's evaluation met.
pub(super) struct LineTrace {
    /// The sprite layer, per column, written where the sprite line buffer
    /// is: the slot, the `tile` value and the flip bits of the sprite pixel
    /// in front. Valid where `sprite_line_color` is not 0.
    pub(super) sprite_slot: [u8; LINE_WIDTH],
    sprite_tile: [u32; LINE_WIDTH],
    sprite_flips: [u16; LINE_WIDTH],
    /// Where the pixel the composite left in the line's index buffers came
    /// from, per column.
    pen_layer: [u16; LINE_WIDTH],
    pen_tile: [u32; LINE_WIDTH],
    pen_cell: [u16; LINE_WIDTH],
    /// The planes of the pixel the line shows, per column.
    layer: [u16; LINE_WIDTH],
    colour: [u16; LINE_WIDTH],
    tile: [u32; LINE_WIDTH],
    cell: [u16; LINE_WIDTH],
    /// The sprites the evaluation met, in order.
    sprites: Vec<MetSprite>,
    /// The line's flags ([`line_flags`]).
    flags: u16,
    /// The entry of `sprites` the dots ran out inside, and the screen x
    /// the renderer stops drawing it at.
    clipped: Option<(usize, i16)>,
}

impl PixelTrace for LineTrace {
    type Notes = LineTrace;
    const ON: bool = true;

    fn notes(&mut self) -> &mut LineTrace {
        self
    }
}

impl LineTrace {
    /// A trace with nothing noted.
    pub(super) fn new() -> Box<Self> {
        Box::new(Self {
            sprite_slot: [0; LINE_WIDTH],
            sprite_tile: [0; LINE_WIDTH],
            sprite_flips: [0; LINE_WIDTH],
            pen_layer: [layer::BACKDROP; LINE_WIDTH],
            pen_tile: [0; LINE_WIDTH],
            pen_cell: [NO_CELL; LINE_WIDTH],
            layer: [layer::NOTHING; LINE_WIDTH],
            colour: [0; LINE_WIDTH],
            tile: [0; LINE_WIDTH],
            cell: [NO_CELL; LINE_WIDTH],
            sprites: Vec::with_capacity(SPRITE_TABLE_SLOTS),
            flags: 0,
            clipped: None,
        })
    }

    /// A line is about to be drawn: every column shows nothing until
    /// something draws it.
    fn begin_line(&mut self) {
        self.layer = [layer::NOTHING; LINE_WIDTH];
        self.colour = [0; LINE_WIDTH];
        self.tile = [0; LINE_WIDTH];
        self.cell = [NO_CELL; LINE_WIDTH];
    }

    /// The evaluation of a line's sprites begins, with the sprite mask
    /// carried in from the line above or not.
    pub(super) fn begin_sprites(&mut self, mask_carried_in: bool) {
        self.sprites.clear();
        self.flags = if mask_carried_in {
            line_flags::MASK_CARRIED_IN
        } else {
            0
        };
        self.clipped = None;
    }

    /// The evaluation took `sprite`, of slot `slot`, hidden by the sprite
    /// mask or not.
    pub(super) fn met_sprite(&mut self, slot: u8, sprite: &Sprite, masked: bool) {
        self.sprites.push(MetSprite {
            slot,
            x: sprite.x,
            width: sprite.width_tiles * 8,
            masked,
            left_out: None,
        });
    }

    /// The line's dots ran out with the sprite the evaluation took last;
    /// `clip_end` is the screen x the renderer stops drawing it at, `None`
    /// when the mask hides it.
    pub(super) fn dots_ran_out(&mut self, clip_end: Option<i16>) {
        self.flags |= line_flags::OUT_OF_TIME;
        if let Some(end) = clip_end {
            self.clipped = Some((self.sprites.len().saturating_sub(1), end));
        }
    }

    /// The line met more sprites than it shows.
    pub(super) fn over_sprite_limit(&mut self) {
        self.flags |= line_flags::OVER_LIMIT;
    }

    /// The evaluation stopped before `sprite`, of slot `slot`, which falls
    /// on the line: it is listed with `fate`.
    pub(super) fn left_out(&mut self, slot: u8, sprite: &Sprite, fate: SpriteFate) {
        self.sprites.push(MetSprite {
            slot,
            x: sprite.x,
            width: sprite.width_tiles * 8,
            masked: false,
            left_out: Some(fate),
        });
    }

    /// The sprite of slot `slot` drew column `x` of the sprite layer, from
    /// character `pattern` at `row` and `column` after flips.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn sprite_pixel(
        &mut self,
        x: usize,
        slot: u8,
        pattern: u16,
        row: u8,
        column: u8,
        across: bool,
        down: bool,
    ) {
        if x < LINE_WIDTH {
            self.sprite_slot[x] = slot;
            self.sprite_tile[x] = character_tile(pattern, row, column);
            self.sprite_flips[x] = flip_bits(across, down);
        }
    }

    /// Where the sprite pixel in front at column `x` came from.
    pub(super) fn sprite_origin(&self, x: usize) -> PenOrigin {
        PenOrigin {
            layer: DRAWN_BY_SPRITE | self.sprite_flips[x],
            tile: self.sprite_tile[x],
            cell: u16::from(self.sprite_slot[x]),
        }
    }

    /// The composite left the pixel of `origin` at column `x` of the index
    /// buffers.
    pub(super) fn pen(&mut self, x: usize, origin: PenOrigin) {
        if x < LINE_WIDTH {
            self.pen_layer[x] = origin.layer;
            self.pen_tile[x] = origin.tile;
            self.pen_cell[x] = origin.cell;
        }
    }

    /// The renderer coloured the columns `from..to` from the index buffers
    /// (`index`, `operator`, `high`: the line's resolved CRAM index,
    /// shadow and highlight operator and priority), with shadow and
    /// highlight on or off.
    pub(super) fn remapped(
        &mut self,
        (index, operator, high): (&[u8], &[u8], &[bool]),
        from: usize,
        to: usize,
        shadow_highlight: bool,
    ) {
        let to = to.min(LINE_WIDTH).min(index.len());
        for x in from.min(to)..to {
            let operator_code = match operator[x] {
                14 => OPERATOR_HIGHLIGHT,
                15 => OPERATOR_SHADOW,
                _ => 0,
            };
            let shaded = shade(shadow_highlight, operator[x], high[x]) as u16;
            self.layer[x] = self.pen_layer[x]
                | if high[x] { HIGH_PRIORITY } else { 0 }
                | (shaded << SHADE_SHIFT)
                | (operator_code << OPERATOR_SHIFT);
            self.colour[x] = u16::from(index[x]);
            self.tile[x] = self.pen_tile[x];
            self.cell[x] = self.pen_cell[x];
        }
    }

    /// The renderer filled the first `width` columns with backdrop colour
    /// `backdrop` (a CRAM index), the display off, and left that pen in the
    /// index buffers.
    pub(super) fn blanked(&mut self, width: usize, backdrop: usize) {
        let width = width.min(LINE_WIDTH);
        self.pen_layer[..width].fill(layer::BACKDROP);
        self.pen_tile[..width].fill(0);
        self.pen_cell[..width].fill(NO_CELL);
        self.layer[..width].fill(layer::NOTHING);
        self.colour[..width].fill(backdrop.min(63) as u16);
        self.tile[..width].fill(0);
        self.cell[..width].fill(NO_CELL);
    }

    /// A CRAM transfer with the display off painted column `x` with the
    /// colour word `word`.
    pub(super) fn painted(&mut self, x: usize, word: u16) {
        if x < LINE_WIDTH {
            self.layer[x] = layer::UNLAYERED_COLOUR;
            self.colour[x] = word;
            self.tile[x] = 0;
            self.cell[x] = NO_CELL;
        }
    }

    /// The line's sprite entries, each with its fate and the columns of it
    /// drawn on a screen `width` pixels wide.
    fn entries(&self, width: i32) -> impl Iterator<Item = SpriteEntry> + '_ {
        self.sprites.iter().enumerate().map(move |(at, &sprite)| {
            let (fate, drawn) = match sprite.left_out {
                Some(fate) => (fate, 0),
                None if sprite.masked => (SpriteFate::Masked, 0),
                None => {
                    let left = i32::from(sprite.x);
                    let right = left + i32::from(sprite.width);
                    let start = left.max(0);
                    let visible = right.clamp(0, width) - start;
                    match self.clipped {
                        _ if visible <= 0 => (SpriteFate::OffScreen, 0),
                        Some((clipped, end)) if clipped == at && i32::from(end) < right => {
                            let drawn = right.min(i32::from(end)).clamp(0, width) - start;
                            (SpriteFate::DrawnInPart, drawn.max(0))
                        }
                        _ => (SpriteFate::DrawnWhole, visible),
                    }
                }
            };
            SpriteEntry {
                sprite,
                fate,
                drawn: drawn as u8,
            }
        })
    }
}

impl Vdp {
    /// Where each pixel of the held frame came from ([`PixelSources`],
    /// packed as the module's notes say), in the planes `planes` asks for
    /// ([`planes::LAYER`], [`planes::COLOUR`], [`planes::TILE`],
    /// [`planes::CELL`]; other bits are ignored), the mode's width (256 or
    /// 320) by [`Self::drawn_height`] lines, cut as the framebuffer is.
    /// `frame` is the frame being drawn ([`Self::set_frame_being_drawn`]).
    ///
    /// The frame is drawn again as [`Self::picture_from_line_states`]
    /// draws it, each line from the line table where it holds the line and
    /// from the state as it is now otherwise, with a trace that notes what
    /// the renderer read for each pixel: the planes describe the picture
    /// that redraw draws, and `matches_picture` says it is the framebuffer's
    /// byte for byte. The debugger's layer toggles apply, as they do to
    /// that picture. Nothing runs: the framebuffer, and everything a
    /// savestate or the state hash reads, stay as they were.
    pub fn pixel_sources(&mut self, planes: u32) -> PixelSources {
        let width = self.display_width() as usize;
        let height = self.drawn_height() as usize;
        let with_table = self.line_table().is_some();
        let mut layer = vec![layer::NOTHING; width * height];
        let mut colour = vec![0u16; width * height];
        let mut tile = vec![0u32; width * height];
        let mut cell = vec![NO_CELL; width * height];
        let kept = DrawState::keep(self);
        let framebuffer = self.framebuffer.clone();
        let mut trace = LineTrace::new();
        let from_line_states = self.redraw_rows(height as u16, |vdp, line, row| {
            trace.begin_line();
            vdp.draw_row(line, row, &mut *trace);
            if row.is_none() && with_table {
                // The table holds no record of the line: it was drawn from
                // the state as it is now, which says nothing of what drew
                // it in the frame.
                return;
            }
            let start = usize::from(line) * width;
            layer[start..start + width].copy_from_slice(&trace.layer[..width]);
            colour[start..start + width].copy_from_slice(&trace.colour[..width]);
            tile[start..start + width].copy_from_slice(&trace.tile[..width]);
            cell[start..start + width].copy_from_slice(&trace.cell[..width]);
        });
        let traced = crop_rgba(
            &self.framebuffer,
            FRAMEBUFFER_WIDTH,
            width as u32,
            height as u32,
        );
        let matches_picture = equals_cropped_rgba(
            &framebuffer,
            FRAMEBUFFER_WIDTH,
            width as u32,
            height as u32,
            &traced,
        );
        self.framebuffer = framebuffer;
        kept.restore(self);
        let asked = |plane: u32| planes & plane != 0;
        PixelSources {
            frame: self.frame_being_drawn,
            width: width as u32,
            height: height as u32,
            from_line_states,
            matches_picture,
            layer: asked(planes::LAYER).then_some(layer),
            colour: asked(planes::COLOUR).then_some(colour),
            tile: asked(planes::TILE).then_some(tile),
            cell: asked(planes::CELL).then_some(cell),
            sub_layer: None,
            sub_colour: None,
        }
    }

    /// The sprites each line of the held frame met and what became of each
    /// ([`LineSprites`], with the fates and flags the module's notes list;
    /// `frame` the frame being drawn, [`Self::set_frame_being_drawn`]):
    /// every line of the picture in order, the evaluation of each run again
    /// from the state the line table recorded for it (its registers, the
    /// sprite list it drew from and the sprite mask carried in) where the
    /// table holds the line, from the state as it is now otherwise. Where
    /// the line's limit or its dots stop the evaluation, the sprites after
    /// are listed too, with their fate, as a scan that changes nothing.
    /// Nothing runs: the framebuffer, and everything a savestate or the
    /// state hash reads, stay as they were.
    pub fn line_sprites(&mut self) -> LineSprites {
        let height = self.drawn_height() as usize;
        let kept = DrawState::keep(self);
        let mut trace = LineTrace::new();
        let mut lines = vec![LineEvaluation::default(); height];
        let from_line_states = self.redraw_rows(height as u16, |vdp, line, row| {
            let evaluation = &mut lines[usize::from(line)];
            if vdp.row_is_drawn(row) {
                vdp.render_sprite_line(line, &mut *trace);
                let width = vdp.display_width() as i32;
                evaluation.flags = trace.flags;
                evaluation.entries = trace.entries(width).collect();
            } else {
                // The display was off: the renderer evaluated no sprite.
                let carried = vdp.sprite_mask_overflow;
                evaluation.flags = if carried {
                    line_flags::MASK_CARRIED_IN
                } else {
                    0
                };
                evaluation.entries.clear();
            }
        });
        kept.restore(self);

        let entries = lines.iter().map(|line| line.entries.len()).sum();
        let mut sprites = LineSprites {
            frame: self.frame_being_drawn,
            from_line_states,
            lines: Vec::with_capacity(height * LINE_SPRITES_STRIDE),
            slots: Vec::with_capacity(entries),
            fates: Vec::with_capacity(entries),
            x: Vec::with_capacity(entries),
            width: Vec::with_capacity(entries),
            drawn_width: Vec::with_capacity(entries),
        };
        for (line, evaluation) in lines.iter().enumerate() {
            sprites.lines.extend([
                line as u16,
                sprites.slots.len() as u16,
                evaluation.entries.len() as u16,
                evaluation.flags,
            ]);
            for entry in &evaluation.entries {
                sprites.slots.push(entry.sprite.slot);
                sprites.fates.push(entry.fate as u8);
                sprites.x.push(entry.sprite.x);
                sprites.width.push(entry.sprite.width);
                sprites.drawn_width.push(entry.drawn);
            }
        }
        sprites
    }

    /// The sprites of the chosen slots drawn alone, as the held frame drew
    /// them: `selection` holds one flag a slot of the sprite table (not 0
    /// chooses it; a slot past its end is not chosen). Each line is drawn
    /// from its own state, the line table's where it holds the line, after
    /// the line's sprite limit and dots and after priority among sprites,
    /// so a chosen sprite behind another shows only where the one in front
    /// is transparent. The picture is the screen in RGBA, as
    /// [`Self::render_layer_image`] draws the sprite layer: a pixel a chosen
    /// sprite drew has its palette colour, before shadow and highlight, and
    /// alpha 255; every other pixel is 0 in all four bytes. With every slot
    /// chosen it is that layer. Nothing runs: the framebuffer, and
    /// everything a savestate or the state hash reads, stay as they were.
    pub fn render_sprites(&mut self, selection: &[u8]) -> RenderedLayer {
        let chosen: [bool; SPRITE_TABLE_SLOTS] =
            std::array::from_fn(|slot| selection.get(slot).is_some_and(|&flag| flag != 0));
        let width = self.display_width() as usize;
        let height = self.drawn_height() as usize;
        let mut rgba = vec![0u8; width * height * 4];
        let kept = DrawState::keep(self);
        let hidden = std::mem::replace(&mut self.debug_layer_disable, [false; LAYER_COUNT]);
        let debug_pixel = self.debug_pixel.take();
        let mut trace = LineTrace::new();
        let from_line_states = self.redraw_rows(height as u16, |vdp, line, row| {
            if !vdp.row_is_drawn(row) {
                return;
            }
            vdp.composite_scanline_with(line, &mut *trace);
            let slots = &trace.sprite_slot;
            let start = usize::from(line) * width * 4;
            vdp.colour_row(row, &mut rgba[start..start + width * 4], |vdp, x| {
                vdp.layer_pixel(LAYER_SPRITES, x)
                    .filter(|_| chosen[usize::from(slots[x])])
            });
        });
        self.debug_pixel = debug_pixel;
        self.debug_layer_disable = hidden;
        kept.restore(self);
        RenderedLayer {
            image: PixelImage {
                width: width as u32,
                height: height as u32,
                rgba,
            },
            from_line_states,
        }
    }

    /// Whether line `row` (the line table's record, or `None` for a line
    /// drawn from the state as it is now) was drawn with the display on.
    pub(super) fn row_is_drawn(&self, row: Option<&LineRecord>) -> bool {
        match row {
            Some(row) => row.kind == LineKind::Drawn,
            None => (self.registers[1] & 0x40) != 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_character_starts_at_sixteen_words_a_character() {
        let value = character_tile(0x7FF, 5, 6);
        assert_eq!(value & 0x7FFF, 0x7FF0);
        assert_eq!((value >> 15) & 0x7FF, 0x7FF);
        assert_eq!((value >> 26) & 7, 5);
        assert_eq!((value >> 29) & 7, 6);
        assert_eq!(character_tile(0, 0, 0), 0);
        assert_eq!(
            character_tile(0x0803, 1, 2),
            character_tile(0x0003, 1, 2),
            "the character's 11 bits"
        );
    }

    #[test]
    fn a_map_entry_s_flips_turn_the_row_and_column_read() {
        // Character 3, flipped across and down, at byte 0xC010.
        let origin = PenOrigin::character(DRAWN_BY_PLANE_B, 0x1803, 0xC010, 1, 2);
        assert_eq!(
            origin.layer,
            DRAWN_BY_PLANE_B | FLIPPED_ACROSS | FLIPPED_DOWN
        );
        assert_eq!(origin.tile, character_tile(3, 5, 6));
        assert_eq!(origin.cell, 0x6008);
    }

    #[test]
    fn shade_is_the_rule_the_renderer_colours_by() {
        for operator in [0u8, 14, 15] {
            for high in [false, true] {
                assert_eq!(shade(false, operator, high), Shade::AsIs);
            }
        }
        assert_eq!(shade(true, 14, false), Shade::Highlighted);
        assert_eq!(shade(true, 14, true), Shade::Highlighted);
        assert_eq!(shade(true, 15, false), Shade::AsIs);
        assert_eq!(shade(true, 0, true), Shade::AsIs);
        assert_eq!(shade(true, 0, false), Shade::Shadowed);
    }

    #[test]
    fn a_sprite_s_fate_follows_where_it_lies_and_where_the_dots_ran_out() {
        let sprite = |x: i16, width_tiles: u8| Sprite {
            x,
            y: 0,
            width_tiles,
            height_tiles: 1,
            pattern_base: 0,
            palette: 0,
            priority: false,
            flip_h: false,
            flip_v: false,
            link: 0,
        };
        let mut trace = LineTrace::new();
        trace.begin_sprites(false);
        trace.met_sprite(1, &sprite(-8, 2), false); // half on screen
        trace.met_sprite(2, &sprite(400, 1), false); // off the right edge
        trace.met_sprite(3, &sprite(10, 1), true); // masked
        trace.met_sprite(4, &sprite(300, 4), false); // the dots run out in it
        trace.dots_ran_out(Some(316));
        trace.left_out(5, &sprite(0, 1), SpriteFate::LeftOut);
        let entries: Vec<(u8, SpriteFate, u8)> = trace
            .entries(320)
            .map(|entry| (entry.sprite.slot, entry.fate, entry.drawn))
            .collect();
        assert_eq!(
            entries,
            [
                (1, SpriteFate::DrawnWhole, 8),
                (2, SpriteFate::OffScreen, 0),
                (3, SpriteFate::Masked, 0),
                (4, SpriteFate::DrawnInPart, 16),
                (5, SpriteFate::LeftOut, 0),
            ]
        );
        assert_eq!(trace.flags, line_flags::OUT_OF_TIME);
    }
}
