//! The picture's size and shape: the framebuffer's columns and rows, the
//! width and height the mode registers set, the lines drawn, the first
//! line of vertical blanking, the interlace mode a frame draws with, how
//! the picture lies over the dots and lines, and the rows a line is drawn
//! into.

use std::ops::Range;

use retro_core::PictureGrid;

use super::Vdp;

/// Pixels in a row of the framebuffer: the 40-cell mode's width, which
/// the 32-cell mode's 256 fit inside.
pub(crate) const FRAMEBUFFER_WIDTH: u32 = 320;
/// Rows of the framebuffer: the 30-line mode's 240 lines, two rows each in
/// interlace mode 2. A frame of any other mode draws its lines into the
/// first 224 or 240 rows, one row a line.
pub(crate) const FRAMEBUFFER_ROWS: u32 = 480;

/// Rows of a cell outside interlace mode 2: one pattern, 8 pixels by 8.
pub(super) const NORMAL_CELL_ROWS: u16 = 8;
/// Rows of a cell in interlace mode 2 (double resolution): two patterns,
/// 8 pixels by 16, a cell's number the index's low 10 bits.
pub(super) const DOUBLE_CELL_ROWS: u16 = 16;

/// The interlace mode of register 12 (bits 1 and 2) a frame draws with
/// ([`Vdp::interlace`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interlace {
    /// No interlace: bit 1 clear (bits `00`, and `10`, which names no
    /// mode).
    Off,
    /// Interlace mode 1 (bits `01`): each field draws the same picture.
    Normal,
    /// Interlace mode 2, double resolution (bits `11`): each field draws
    /// rows of its own, and a cell is 8 pixels by 16.
    Double,
}

impl Interlace {
    /// The mode register 12's value `register_12` selects.
    pub fn of_register_12(register_12: u8) -> Self {
        match register_12 & 0x06 {
            0x02 => Self::Normal,
            0x06 => Self::Double,
            _ => Self::Off,
        }
    }
}

impl Vdp {
    /// Get display width based on register 12 (H40/H32 mode).
    pub fn display_width(&self) -> u32 {
        if (self.registers[12] & 0x01) != 0 {
            320 // H40 mode
        } else {
            256 // H32 mode
        }
    }

    /// The first line of vertical blanking: the line whose start raises
    /// the vertical interrupt's flag. The lines before it draw the picture
    /// ([`Self::drawn_height`]).
    pub fn first_vblank_line(&self) -> u16 {
        self.timing.visible_scanlines()
    }

    /// Lines of the picture the VDP draws into its framebuffer this frame:
    /// 224 or 240, register 1 bit 3 as the frame's first line found it, on
    /// either region (a change shows from the next frame).
    pub fn drawn_height(&self) -> u32 {
        u32::from(self.timing.visible_scanlines())
    }

    /// The picture's height in rows: the lines drawn
    /// ([`Self::drawn_height`]), two rows each in interlace mode 2 (448 or
    /// 480), one each otherwise.
    pub fn picture_height(&self) -> u32 {
        self.drawn_height() * u32::from(self.picture_grid().rows_per_line)
    }

    /// The height the mode register asks for now, register 1 bit 3 (V30 or
    /// V28). The frame being drawn has [`Self::drawn_height`] lines.
    pub fn display_height(&self) -> u32 {
        if (self.registers[1] & 0x08) != 0 {
            240 // V30 mode
        } else {
            224 // V28 mode
        }
    }

    /// The interlace mode the frame draws with: register 12 bits 1 and 2
    /// as the frame's first line found it, so a change shows from the next
    /// frame, as Genesis Plus GX and MAME apply it; a reset and a
    /// savestate's load take it from the register too.
    pub fn interlace(&self) -> Interlace {
        self.interlace
    }

    /// Take the interlace mode the frame draws with from register 12
    /// ([`Self::interlace`]).
    pub(super) fn latch_interlace(&mut self) {
        self.interlace = Interlace::of_register_12(self.registers[12]);
    }

    /// How the picture lies over the dots and lines: one column a dot in
    /// both widths; one row a line, or in interlace mode 2 two rows a line,
    /// the line's two fields woven, the even field on its first row and the
    /// odd field on its second.
    pub fn picture_grid(&self) -> PictureGrid {
        match self.interlace {
            Interlace::Double => PictureGrid {
                columns_per_dot: 1,
                rows_per_line: 2,
                woven: true,
            },
            Interlace::Off | Interlace::Normal => PictureGrid::ONE,
        }
    }

    /// The framebuffer row line `line` is drawn into for the field being
    /// drawn ([`Self::draw_line_rows`]): the grid's row of that field
    /// ([`PictureGrid::row`]), the line itself outside interlace mode 2.
    pub(super) fn picture_row(&self, line: u16) -> usize {
        self.picture_grid().row(u32::from(line), self.field_drawn) as usize
    }

    /// Every framebuffer row line `line` is drawn into: its one row, or
    /// its two fields' rows in interlace mode 2.
    pub(super) fn line_rows(&self, line: u16) -> Range<usize> {
        let grid = self.picture_grid();
        let first = grid.row(u32::from(line), 0) as usize;
        first..first + usize::from(grid.rows_per_line)
    }

    /// Draw a line's rows with `draw`, which draws the row of the field
    /// being drawn ([`Self::picture_row`]): the line's one row, or in
    /// interlace mode 2 both fields' rows, the other field's first and the
    /// line's own field (`field`, the frame's: 0 even, 1 odd) last.
    ///
    /// The other field's row is drawn as a repaint draws a line, changing
    /// nothing a game reads: the sprite flags drawing it sets (the
    /// overflow and the collision of the status word, and the mask carried
    /// to the next line) are put back, so the line's own row starts from
    /// them as the line above left them and leaves them as the game reads
    /// them. The rest of what drawing leaves ([`super::DrawState`]) needs no
    /// putting back: the sprite list is the frame's either way, and the
    /// line's own row writes every line buffer over the other's. After the
    /// line, the field drawn is the line's own, whose row a palette write
    /// made while the line is on screen colours again.
    #[inline]
    pub(super) fn draw_line_rows(&mut self, field: u8, mut draw: impl FnMut(&mut Vdp)) {
        if self.interlace != Interlace::Double {
            draw(self);
            return;
        }
        let flags = (
            self.sprite_overflow,
            self.sprite_collision,
            self.sprite_mask_overflow,
        );
        self.field_drawn = field ^ 1;
        draw(self);
        (
            self.sprite_overflow,
            self.sprite_collision,
            self.sprite_mask_overflow,
        ) = flags;
        self.field_drawn = field;
        draw(self);
    }
}
