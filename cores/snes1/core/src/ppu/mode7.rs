//! Mode 7 matrix + multiplier: `$211A` M7SEL, `$211B-$2120` M7A-M7Y,
//! `$2134-$2136` MPYL/MPYM/MPYH.
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
