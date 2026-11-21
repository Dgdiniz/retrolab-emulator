//! The picture's size: the framebuffer's columns and rows, the width and
//! height the mode registers set, the lines drawn, and the first line of
//! vertical blanking.

use super::Vdp;

/// Pixels in a row of the framebuffer: the 40-cell mode's width, which
/// the 32-cell mode's 256 fit inside.
pub(super) const FRAMEBUFFER_WIDTH: u32 = 320;
/// Rows of the framebuffer: the 28-cell mode's 224 lines.
pub(super) const FRAMEBUFFER_LINES: u32 = 224;

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
    /// the vertical interrupt's flag. The lines before it draw the picture.
    pub fn first_vblank_line(&self) -> u16 {
        self.timing.visible_scanlines()
    }

    /// Lines of the picture the VDP draws into its framebuffer: the mode's
    /// 224 or 240 ([`Self::display_height`]), cut to the rows the
    /// framebuffer holds.
    pub fn drawn_height(&self) -> u32 {
        self.display_height().min(FRAMEBUFFER_LINES)
    }

    /// Get display height based on register 1 (V30/V28 mode).
    pub fn display_height(&self) -> u32 {
        if (self.registers[1] & 0x08) != 0 {
            240 // V30 mode
        } else {
            224 // V28 mode
        }
    }
}
