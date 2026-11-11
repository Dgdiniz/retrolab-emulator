//! The bits of the GSU's packed registers.
//!
//! The status register (SFR), the screen mode (SCMR), the plot options
//! (POR) and the configuration (CFGR) each pack several one-bit fields.
//! [`crate::Gsu`] keeps the whole byte or word; these constants name the
//! bits, numbered as the Super FX documentation numbers them.

/// The status register (SFR), which the S-CPU reads at `$3030`/`$3031`.
pub mod status {
    /// Z: the last result was zero.
    pub const ZERO: u16 = 1 << 1;
    /// CY: the last addition carried out of bit 15; the last subtraction
    /// did not borrow.
    pub const CARRY: u16 = 1 << 2;
    /// S: bit 15 of the last result (bit 7 for the byte operations).
    pub const SIGN: u16 = 1 << 3;
    /// OV: the last addition or subtraction overflowed as a signed number.
    pub const OVERFLOW: u16 = 1 << 4;
    /// G: the GSU is running. A write of R15's high byte sets it, STOP
    /// clears it.
    pub const GO: u16 = 1 << 5;
    /// R: a read through the ROM buffer is under way.
    pub const ROM_READ: u16 = 1 << 6;
    /// ALT1: the next opcode is read in its first alternate form.
    pub const ALT1: u16 = 1 << 8;
    /// ALT2: the next opcode is read in its second alternate form; with
    /// ALT1, in its third (ALT3).
    pub const ALT2: u16 = 1 << 9;
    /// IL: an immediate's low byte has been read. Part of the register's
    /// layout; nothing in this core sets it.
    pub const IMMEDIATE_LOW: u16 = 1 << 10;
    /// IH: an immediate's high byte has been read. Part of the register's
    /// layout; nothing in this core sets it.
    pub const IMMEDIATE_HIGH: u16 = 1 << 11;
    /// B: the last opcode was WITH, so a TO or FROM that follows moves a
    /// register instead of naming one.
    pub const WITH: u16 = 1 << 12;
    /// IRQ: STOP raised the interrupt; reading `$3031` clears it.
    pub const IRQ: u16 = 1 << 15;
}

/// The screen mode register (SCMR, `$303A`).
pub mod screen_mode {
    /// RON: the GSU has the Game Pak ROM bus.
    pub const ROM_BUS: u8 = 1 << 4;
    /// RAN: the GSU has the Game Pak RAM bus.
    pub const RAM_BUS: u8 = 1 << 3;
    /// MD: the plot surface's colour depth, 0 for 2 bits a pixel, 1 and 2
    /// for 4, 3 for 8.
    pub const DEPTH: u8 = 0b11;

    /// HT, the plot surface's height, whose two bits are apart in the
    /// register: bit 5 is the high one, bit 2 the low one. 0, 1 and 2 are
    /// surfaces 128, 160 and 192 pixels tall; 3 is the 256 x 256 layout of
    /// sprites.
    pub const fn height(mode: u8) -> u8 {
        ((mode >> 4) & 0b10) | ((mode >> 2) & 0b01)
    }
}

/// The plot option register (POR), which CMODE loads.
pub mod plot_options {
    /// Set, PLOT draws a pixel of colour 0 too; clear, a pixel of colour 0
    /// leaves the surface as it was.
    pub const PLOT_TRANSPARENT: u8 = 1 << 0;
    /// In the 2- and 4-bit modes, pixels take COLR's two nibbles in a
    /// checkerboard.
    pub const DITHER: u8 = 1 << 1;
    /// COLOR and GETC put the source's high nibble into COLR's low one.
    pub const HIGH_NIBBLE: u8 = 1 << 2;
    /// COLOR and GETC change only COLR's low nibble; in the 8-bit mode,
    /// only the low nibble decides whether a colour is 0.
    pub const FREEZE_HIGH: u8 = 1 << 3;
    /// PLOT and RPIX use the 256 x 256 layout of sprites, whatever SCMR's
    /// height says.
    pub const SPRITE_LAYOUT: u8 = 1 << 4;
}

/// The configuration register (CFGR, `$3037`).
pub mod config {
    /// MS0: the fast multiplier, MULT, UMULT, FMULT and LMULT take fewer
    /// clocks.
    pub const FAST_MULTIPLY: u8 = 1 << 5;
    /// STOP does not raise the interrupt.
    pub const IRQ_MASKED: u8 = 1 << 7;
}
