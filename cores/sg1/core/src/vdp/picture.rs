//! The picture's size and shape: the framebuffer's columns and rows, the
//! width and height the mode registers set, the lines drawn, the first
//! line of vertical blanking, and how the picture lies over the dots and
//! lines.

use retro_core::PictureGrid;

use super::Vdp;

/// Pixels in a row of the framebuffer: the 40-cell mode's width, which
/// the 32-cell mode's 256 fit inside.
pub(crate) const FRAMEBUFFER_WIDTH: u32 = 320;
/// Rows of the framebuffer: the 30-line mode's 240, which the 28-line
/// mode's 224 fit inside.
pub(crate) const FRAMEBUFFER_LINES: u32 = 240;

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

    /// The height the mode register asks for now, register 1 bit 3 (V30 or
    /// V28). The frame being drawn has [`Self::drawn_height`] lines.
    pub fn display_height(&self) -> u32 {
        if (self.registers[1] & 0x08) != 0 {
            240 // V30 mode
        } else {
            224 // V28 mode
        }
    }

    /// How the picture lies over the dots and lines: one column a dot and
    /// one row a line, in both widths and both heights.
    pub fn picture_grid(&self) -> PictureGrid {
        PictureGrid::ONE
    }
}
