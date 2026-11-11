//! The plot engine: PLOT, RPIX, COLOR and CMODE.
//!
//! The GSU draws on a surface of SNES characters in Game Pak RAM: 8 x 8
//! pixels each, two, four or eight bit planes deep, starting at SCBR. PLOT
//! does not write a pixel at once: it collects it in a line of the pixel
//! cache, which holds the eight pixels of one character row, and writes
//! the whole row to RAM, plane by plane, when PLOT moves on to another row
//! or fills this one. Of a row not filled, only the pixels drawn change.
//! Every RAM access costs a bus access.
//!
//! The pixel cache follows ares (`sfc/coprocessor/superfx/core.cpp`), ISC,
//! the notice is at the end of `LICENSE`.

use retro_core::cpu::Addressable;

use crate::registers::{plot_options, screen_mode};
use crate::{Gsu, PixelCache};

/// Where each bit plane's byte of a row is within a character: the planes
/// go in pairs, a pair's two bytes side by side and 16 bytes from one pair
/// to the next.
const PLANE_OFFSETS: [u32; 8] = [0, 1, 16, 17, 32, 33, 48, 49];

impl Gsu {
    /// `$4C` PLOT: draw COLR at (R1, R2) and step R1 on to the next pixel.
    /// Under ALT1, RPIX: the destination takes the colour of the pixel
    /// there.
    pub(crate) fn plot_or_read_pixel(&mut self, bus: &mut dyn Addressable) {
        let (x, y) = (self.registers[1] as u8, self.registers[2] as u8);
        if self.alt1() {
            let pixel = u16::from(self.read_pixel(bus, x, y));
            self.write_destination(pixel);
            self.set_sign_and_zero(pixel);
        } else {
            self.plot(bus, x, y);
            self.registers[1] = self.registers[1].wrapping_add(1);
        }
        self.clear_prefixes();
    }

    /// `$4E` COLOR: COLR takes the source's low byte, shaped by the plot
    /// options. Under ALT1, CMODE: the plot options take it.
    pub(crate) fn color_or_options(&mut self) {
        let source = self.source_value() as u8;
        if self.alt1() {
            self.plot_options = source;
        } else {
            self.color = self.filter_color(source);
        }
        self.clear_prefixes();
    }

    /// Draw COLR at (`x`, `y`).
    pub(crate) fn plot(&mut self, bus: &mut dyn Addressable, x: u8, y: u8) {
        let depth = self.screen_mode & screen_mode::DEPTH;
        if self.plot_options & plot_options::PLOT_TRANSPARENT == 0 {
            // A pixel of colour 0 is left as it is. In the 8-bit mode the
            // whole byte must be 0, unless FREEZE HIGH narrows the test to
            // the low nibble.
            let whole_byte = depth == 3 && self.plot_options & plot_options::FREEZE_HIGH == 0;
            let tested = if whole_byte {
                self.color
            } else {
                self.color & 0x0f
            };
            if tested == 0 {
                return;
            }
        }

        let mut color = self.color;
        if self.plot_options & plot_options::DITHER != 0 && depth != 3 {
            // A checkerboard of COLR's two nibbles: odd squares take the
            // high one.
            if (x ^ y) & 1 != 0 {
                color >>= 4;
            }
            color &= 0x0f;
        }

        let group = (u16::from(y) << 5) + u16::from(x >> 3);
        if group != self.pixel_cache[0].group {
            self.retire_pixel_line(bus);
            self.pixel_cache[0].group = group;
        }
        let bit = (x & 7) ^ 7;
        let line = &mut self.pixel_cache[0];
        line.colors[usize::from(bit)] = color;
        line.pending |= 1 << bit;
        if line.pending == 0xff {
            self.retire_pixel_line(bus);
        }
    }

    /// Write out the older line of the pixel cache and move the current
    /// one into its place, leaving the current line empty.
    fn retire_pixel_line(&mut self, bus: &mut dyn Addressable) {
        self.flush_pixels(bus, 1);
        self.pixel_cache[1] = self.pixel_cache[0];
        self.pixel_cache[0].pending = 0;
    }

    /// The colour of the pixel at (`x`, `y`), after writing out both lines
    /// of the pixel cache so RAM holds every pixel drawn.
    pub(crate) fn read_pixel(&mut self, bus: &mut dyn Addressable, x: u8, y: u8) -> u8 {
        self.flush_pixels(bus, 1);
        self.flush_pixels(bus, 0);
        let row = self.pixel_row_address(x, y);
        let bit = (x & 7) ^ 7;
        let mut pixel = 0;
        for (plane, offset) in PLANE_OFFSETS.iter().take(self.planes()).enumerate() {
            let clocks = self.memory_clocks();
            self.spend(bus, clocks);
            pixel |= ((bus.read(row + offset) >> bit) & 1) << plane;
        }
        pixel
    }

    /// Write line `line` of the pixel cache to RAM, if it holds any pixel.
    pub(crate) fn flush_pixels(&mut self, bus: &mut dyn Addressable, line: usize) {
        let PixelCache {
            group,
            pending,
            colors,
        } = self.pixel_cache[line];
        if pending == 0 {
            return;
        }
        let x = (group << 3) as u8;
        let y = (group >> 5) as u8;
        let row = self.pixel_row_address(x, y);
        for (plane, offset) in PLANE_OFFSETS.iter().take(self.planes()).enumerate() {
            let mut bits = colors.iter().enumerate().fold(0u8, |byte, (bit, color)| {
                byte | (((color >> plane) & 1) << bit)
            });
            if pending != 0xff {
                // The pixels not drawn keep what RAM holds.
                let clocks = self.memory_clocks();
                self.spend(bus, clocks);
                bits = (bits & pending) | (bus.read(row + offset) & !pending);
            }
            let clocks = self.memory_clocks();
            self.spend(bus, clocks);
            bus.write(row + offset, bits);
        }
        self.pixel_cache[line].pending = 0;
    }

    /// The RAM address of the first plane byte of the row that holds pixel
    /// (`x`, `y`).
    pub(crate) fn pixel_row_address(&self, x: u8, y: u8) -> u32 {
        let character_bytes = self.planes() as u32 * 8;
        0x70_0000
            + u32::from(self.screen_base) * 1024
            + self.character_index(x, y) * character_bytes
            + u32::from(y & 7) * 2
    }

    /// Which character of the surface holds pixel (`x`, `y`). The 128-,
    /// 160- and 192-pixel surfaces run their characters down each column
    /// of 16, 20 or 24; the 256 x 256 layout of sprites is four quadrants
    /// of 16 x 16 characters, row by row.
    pub(crate) fn character_index(&self, x: u8, y: u8) -> u32 {
        let column = u32::from(x >> 3);
        let row = u32::from(y >> 3);
        let layout = if self.plot_options & plot_options::SPRITE_LAYOUT != 0 {
            3
        } else {
            screen_mode::height(self.screen_mode)
        };
        match layout {
            0 => column * 16 + row,
            1 => column * 20 + row,
            2 => column * 24 + row,
            _ => (row & 0x10) * 32 + (column & 0x10) * 16 + (row & 0x0f) * 16 + (column & 0x0f),
        }
    }

    /// The surface's bits a pixel: 2, 4 or 8 (a depth of 2 also means 4).
    pub(crate) fn planes(&self) -> usize {
        match self.screen_mode & screen_mode::DEPTH {
            0 => 2,
            3 => 8,
            _ => 4,
        }
    }
}
