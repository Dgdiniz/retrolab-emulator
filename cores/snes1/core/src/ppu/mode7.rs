//! Mode 7 matrix + multiplier: `$211A` M7SEL, `$211B-$2120` M7A-M7Y,
//! `$2134-$2136` MPYL/MPYM/MPYH; and the affine plane's rows.
//!
//! Mode 7 turns BG1 into an affine-transformed plane. The six
//! coefficients (A, B, C, D, X, Y) are 16-bit each; the chip
//! exposes them through six byte-side write ports that share a
//! single 8-bit latch ([`Ppu::mode7_latchdata`]), every write
//! delivers `(prev_latch, data)` as the new 16-bit value and
//! refreshes the latch to `data` for the next register
//! ([`Ppu::mode7_latched_word`], which BG1's scroll registers use
//! too). Written from fullsnes's Mode 7 register description; the
//! multiplier's reads checked against bsnes-plus.
//!
//! ## Multiplier
//!
//! The PPU continuously evaluates `M7A x (M7B >> 8)` as a 16x8
//! signed multiply and exposes the 24-bit result through three
//! 8-bit read ports (MPYL = low, MPYM = mid, MPYH = high). The
//! shift on M7B is silicon's way of treating it as the matrix
//! coefficient's *integer* part, Mode 7 software writes a
//! signed 8-bit value into M7B's high byte to get a unitless
//! scalar suitable for projection math.
//!
//! Each MPYL/M/H read updates [`Ppu::ppu1_mdr`] with the byte
//! returned, so partial reads see the right "open bus" tail.
//!
//! ## The affine rows
//!
//! `Ppu::render_mode7_rows` fills BG1's row buffer from the plane, and
//! BG2's with EXTBG, for the renderer's composite. The rows go through
//! the same mosaic / snapshot / composite pipeline as the tile modes;
//! only the BG fill differs.

use retro_core::capture::PixelTrace;

use super::render::{direct_color_word, BgRowBuffer, BgRowPixel, NORMAL_SCANLINE_WIDTH};
use super::tile::{mode7_clip, mode7_sample, mode7_sample_at, sign_extend_13};
use super::trace::{mode7_origin, FrameTrace};
use super::Ppu;

impl Ppu {
    // -- Helpers -----------------------------------------------

    /// 24-bit signed result of `M7A x (M7B >> 8)`. M7A is a full
    /// `i16`; `M7B >> 8` is the signed high byte (`i8`).
    fn mode7_multiply(&self) -> i32 {
        let a = self.m7a as i16 as i32;
        let b_hi = (self.m7b >> 8) as i8 as i32;
        a * b_hi
    }

    // -- Write handlers ---------------------------------------

    /// The 16-bit value a Mode 7 write delivers: the byte written over
    /// the one the previous write left in the shared latch, which this
    /// write then refreshes.
    pub(super) fn mode7_latched_word(&mut self, value: u8) -> u16 {
        let word = u16::from_le_bytes([self.mode7_latchdata, value]);
        self.mode7_latchdata = value;
        word
    }

    /// `$211A` M7SEL, screen-over (bits 6-7), V flip (bit 1),
    /// H flip (bit 0).
    pub(super) fn write_211a(&mut self, value: u8) {
        self.mode7_repeat = (value >> 6) & 0x3;
        self.mode7_vflip = value & 0x02 != 0;
        self.mode7_hflip = value & 0x01 != 0;
    }

    /// `$211B` M7A.
    pub(super) fn write_211b(&mut self, value: u8) {
        self.m7a = self.mode7_latched_word(value);
    }

    /// `$211C` M7B.
    pub(super) fn write_211c(&mut self, value: u8) {
        self.m7b = self.mode7_latched_word(value);
    }

    /// `$211D` M7C.
    pub(super) fn write_211d(&mut self, value: u8) {
        self.m7c = self.mode7_latched_word(value);
    }

    /// `$211E` M7D.
    pub(super) fn write_211e(&mut self, value: u8) {
        self.m7d = self.mode7_latched_word(value);
    }

    /// `$211F` M7X.
    pub(super) fn write_211f(&mut self, value: u8) {
        self.m7x = self.mode7_latched_word(value);
    }

    /// `$2120` M7Y.
    pub(super) fn write_2120(&mut self, value: u8) {
        self.m7y = self.mode7_latched_word(value);
    }

    // -- Read handlers ----------------------------------------

    /// `$2134` MPYL, low 8 bits of `M7A x (M7B >> 8)`.
    pub(super) fn read_2134(&mut self) -> u8 {
        let result = self.mode7_multiply();
        let byte = (result & 0xFF) as u8;
        self.ppu1_mdr = byte;
        byte
    }

    /// `$2135` MPYM, middle 8 bits.
    pub(super) fn read_2135(&mut self) -> u8 {
        let result = self.mode7_multiply();
        let byte = ((result >> 8) & 0xFF) as u8;
        self.ppu1_mdr = byte;
        byte
    }

    /// `$2136` MPYH, high 8 bits (sign-extended from bit 23).
    pub(super) fn read_2136(&mut self) -> u8 {
        let result = self.mode7_multiply();
        let byte = ((result >> 16) & 0xFF) as u8;
        self.ppu1_mdr = byte;
        byte
    }

    // -- Rasteriser -------------------------------------------

    /// Fill the Mode 7 BG row buffers from the affine plane.
    ///
    /// Mode 7 replaces the four tile BGs with one affine layer
    /// (BG1). With EXTBG (`$2133` bit 6) the same sampled byte also
    /// feeds BG2: its low 7 bits are the colour index and bit 7 is
    /// the priority bit. Only `main_rows` is filled, Mode 7 is
    /// non-hires so the sub plane never reaches the composite.
    /// `bg1` and `bg2` say which of the two rows to fill: the compositor
    /// fills the ones on the main screen (BG2 only with EXTBG), a
    /// single-layer render the one it draws.
    ///
    /// The matrix math is the hardware's as Anomie's Mode 7 notes give
    /// it (checked against bsnes-plus). Coefficients A-D are 8.8 fixed
    /// point; the `& !63` truncation on the three base terms of each
    /// axis is the chip's (the `(cx << 8)` centre term and the
    /// per-pixel `A*xf` / `C*xf` terms are not truncated).
    /// [`sign_extend_13`] reads the centre / scroll registers;
    /// [`mode7_clip`] is the separate clip applied to the
    /// scroll-minus-centre differences.
    pub(super) fn render_mode7_rows<T: PixelTrace<Notes = FrameTrace>>(
        &self,
        scanline: u16,
        main_rows: &mut [BgRowBuffer; 4],
        bg1: bool,
        bg2: bool,
        trace: &mut T,
    ) {
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

        let direct = self.direct_color;
        let bg1_main = bg1;
        let bg2_main = bg2;

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

            // A traced redraw notes where the byte came from too.
            let (byte, sample) = if T::ON {
                let Some(sample) = mode7_sample_at(self.vram(), px, py, self.mode7_repeat) else {
                    continue;
                };
                (sample.byte, Some(sample))
            } else {
                let Some(byte) = mode7_sample(self.vram(), px, py, self.mode7_repeat) else {
                    continue;
                };
                (byte, None)
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
                if T::ON {
                    if let Some(sample) = sample {
                        trace.notes().fetch[0][x] = mode7_origin(sample, byte, direct, false);
                    }
                }
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
                    if T::ON {
                        if let Some(sample) = sample {
                            trace.notes().fetch[1][x] =
                                mode7_origin(sample, idx, false, byte & 0x80 != 0);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::capture::NoTrace;

    use crate::ppu::render::MAX_SCANLINE_WIDTH;

    #[test]
    fn m7sel_decodes_flip_and_repeat_bits() {
        let mut ppu = Ppu::new();
        ppu.write_211a(0xC3); // bits 7+6 set (repeat = 3), bits 1+0 set (both flips)
        assert_eq!(ppu.mode7_repeat, 3);
        assert!(ppu.mode7_vflip);
        assert!(ppu.mode7_hflip);

        ppu.write_211a(0x80); // repeat = 2, no flips
        assert_eq!(ppu.mode7_repeat, 2);
        assert!(!ppu.mode7_vflip);
        assert!(!ppu.mode7_hflip);
    }

    #[test]
    fn m7_writes_share_one_latch_byte() {
        let mut ppu = Ppu::new();
        // Sequential writes to A, B, C, D each rotate the latch.
        ppu.write_211b(0x12); // m7a = (0x12 << 8) | 0 = 0x1200; latch = 0x12
        assert_eq!(ppu.m7a, 0x1200);
        assert_eq!(ppu.mode7_latchdata, 0x12);

        ppu.write_211b(0x34); // m7a = (0x34 << 8) | 0x12 = 0x3412; latch = 0x34
        assert_eq!(ppu.m7a, 0x3412);

        ppu.write_211c(0x56); // m7b = (0x56 << 8) | 0x34 = 0x5634; latch = 0x56
        assert_eq!(ppu.m7b, 0x5634);

        // The same latch is used by X/Y too.
        ppu.write_211f(0x78); // m7x = (0x78 << 8) | 0x56 = 0x7856; latch = 0x78
        assert_eq!(ppu.m7x, 0x7856);
    }

    #[test]
    fn mpy_returns_24bit_signed_multiply() {
        let mut ppu = Ppu::new();
        // Build M7A = 0x0100 (256).
        ppu.write_211b(0x00); // latch = 0x00, m7a = 0
        ppu.write_211b(0x01); // m7a = 0x0100
        assert_eq!(ppu.m7a, 0x0100);

        // Build M7B = 0x0300 -> (m7b >> 8) = 0x03 = 3.
        ppu.write_211c(0x00); // latch (now 0x01) becomes low byte... wait,
                              // after write_211b(0x01) latch is 0x01.
                              // So m7b = (0x00 << 8) | 0x01 = 0x0001. NOT 0.
                              // The shared latch behaviour means consecutive
                              // writes don't isolate cleanly. Reset by writing
                              // the latch low first then the target reg high.
        ppu.write_211c(0x03); // m7b = (0x03 << 8) | 0x00 = 0x0300
        assert_eq!(ppu.m7b, 0x0300);

        // M7A (=256) x (M7B >> 8) (=3) = 768 = 0x0300.
        // bytes: LL=0x00, MM=0x03, HH=0x00.
        assert_eq!(ppu.read_2134(), 0x00); // MPYL
        assert_eq!(ppu.read_2135(), 0x03); // MPYM
        assert_eq!(ppu.read_2136(), 0x00); // MPYH
        assert_eq!(ppu.ppu1_mdr, 0x00);
    }

    #[test]
    fn mpy_handles_negative_m7a_with_sign_extension() {
        let mut ppu = Ppu::new();
        // M7A = -1 (0xFFFF).
        ppu.write_211b(0xFF);
        ppu.write_211b(0xFF);
        assert_eq!(ppu.m7a, 0xFFFF);

        // M7B = 0x0500 (low byte irrelevant; high byte = 5).
        ppu.write_211c(0x00); // latch = 0xFF -> m7b = (0x00 << 8) | 0xFF = 0x00FF
        ppu.write_211c(0x05); // m7b = (0x05 << 8) | 0x00 = 0x0500
        assert_eq!(ppu.m7b, 0x0500);

        // -1 x 5 = -5 -> 24-bit two's complement = 0xFFFFFB.
        assert_eq!(ppu.read_2134(), 0xFB); // LL
        assert_eq!(ppu.read_2135(), 0xFF); // MM
        assert_eq!(ppu.read_2136(), 0xFF); // HH (sign extends)
    }

    #[test]
    fn mpy_handles_negative_m7b_high_byte() {
        let mut ppu = Ppu::new();
        // M7A = 100.
        ppu.write_211b(0x00);
        ppu.write_211b(0x00);
        // Build M7A via single full pair.
        // Easier: write low then high.
        ppu.write_211b(0x64); // latch_after = 0x64, m7a = (0x64 << 8) | prev_latch
        ppu.write_211b(0x00); // m7a = (0x00 << 8) | 0x64 = 0x0064
        assert_eq!(ppu.m7a, 100);

        // M7B high byte = 0x80 (i8 = -128).
        ppu.write_211c(0x00); // m7b = (0x00 << 8) | 0x00 = 0
        ppu.write_211c(0x80); // m7b = 0x8000
        assert_eq!(ppu.m7b, 0x8000);

        // 100 x -128 = -12800 -> 24-bit = 0xFFCE00.
        assert_eq!(ppu.read_2134(), 0x00);
        assert_eq!(ppu.read_2135(), 0xCE);
        assert_eq!(ppu.read_2136(), 0xFF);
    }

    // -- Rasteriser -------------------------------------------

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
        ppu.render_mode7_rows(1, &mut rows, true, false, &mut NoTrace::<FrameTrace>::new());

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
        ppu.render_mode7_rows(1, &mut rows, true, true, &mut NoTrace::<FrameTrace>::new());

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
        ppu.render_mode7_rows(1, &mut rows, true, false, &mut NoTrace::<FrameTrace>::new());

        assert!(rows[0][0].opaque);
        assert_eq!(rows[0][0].color, direct_color_word(0x07, 0));
    }
}
