//! The SA-1's variable-length bit reader (`$2258-$225B` written, `$230C-$230D`
//! read): a window that reads a field of 1 to 16 bits from a stream of bits
//! in ROM, I-RAM or BW-RAM, so a game can unpack data of varying widths
//! without shifting and masking on the 65816.
//!
//! The program sets the stream's start (VDA, a byte address; writing its high
//! byte also starts at the byte's first bit) and the field width (VBD). A
//! read of `$230C`/`$230D` returns the low/high byte of the 16 bits that
//! start at the cursor. The cursor then moves on by the width at one of two
//! moments, which VBD bit 7 chooses: on every write of VBD (bit 7 clear), or
//! on every read of `$230D` (bit 7 set).
//!
//! Here the cursor is one number, the position in bits from the start of the
//! address space (the byte address times eight plus the bit), so moving on
//! is one addition. The bytes themselves come from the chip's own read-only
//! decode of the stream ([`super::Sa1Chip`]'s `vbr_read`), which never
//! reaches a register.
//!
//! Behaviour checked against bsnes-plus and ares, which agree.

use super::registers::{field, flag, Bytes};
use crate::savestate::Sa1VarBitState;

/// The cursor runs over 24 address bits and 3 bit-in-byte bits, and wraps.
const POSITION_MASK: u32 = (1 << 27) - 1;

/// The variable-length bit reader.
#[derive(Default)]
pub(super) struct Sa1VarBit {
    /// The cursor, in bits: the stream's byte address (VDA) times eight, plus
    /// the bit within that byte.
    position: u32,
    /// The field width in bits as VBD (`$2258`) last set it, 1 to 16 (a
    /// written 0 means 16). Zero only before the first write.
    width: u8,
    /// VBD bit 7: the cursor moves on when `$230D` is read, instead of when
    /// VBD is written.
    advance_on_read: bool,
}

impl Sa1VarBit {
    /// VBD (`$2258`): the width and when the cursor moves; with bit 7 clear,
    /// the write itself moves the cursor on by the new width.
    pub(super) fn write_control(&mut self, value: u8) {
        self.advance_on_read = flag(value, 7);
        self.width = match field(value, 0, 4) {
            0 => 16,
            bits => bits,
        };
        if !self.advance_on_read {
            self.advance();
        }
    }

    /// One byte of VDA (`$2259` + `index`). The high byte also puts the
    /// cursor on the first bit of the byte.
    pub(super) fn write_address(&mut self, index: usize, value: u8) {
        let address = (self.position >> 3).with_byte(index, value);
        let bit = if index == 2 { 0 } else { self.position & 7 };
        self.position = (address << 3 | bit) & POSITION_MASK;
    }

    /// The byte address of the cursor (24 bits).
    pub(super) fn address(&self) -> u32 {
        self.position >> 3
    }

    /// The bit of that byte the cursor is on (0-7).
    pub(super) fn bit(&self) -> u8 {
        (self.position & 7) as u8
    }

    /// Whether a read of `$230D` moves the cursor on.
    pub(super) fn advances_on_read(&self) -> bool {
        self.advance_on_read
    }

    /// The field width (1-16; 0 before the first VBD write).
    pub(super) fn width(&self) -> u8 {
        self.width
    }

    /// The 16 bits that start at the cursor, from the three bytes at the
    /// cursor's address and the two after it. The width only decides how far
    /// the cursor moves; the read returns all 16 bits.
    pub(super) fn field(&self, bytes: [u8; 3]) -> u16 {
        let window = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0]);
        (window >> self.bit()) as u16
    }

    /// Move the cursor on by the width.
    pub(super) fn advance(&mut self) {
        self.position = (self.position + u32::from(self.width)) & POSITION_MASK;
    }

    /// The reader for a savestate.
    pub(super) fn snapshot(&self) -> Sa1VarBitState {
        Sa1VarBitState {
            va: self.address(),
            vbit: self.bit(),
            vb: self.width,
            hl: self.advance_on_read,
        }
    }

    /// The reader from a savestate, kept in range: an address of 24 bits, a
    /// bit of 0-7, a width of at most 16.
    pub(super) fn restore(&mut self, state: &Sa1VarBitState) {
        self.position = ((state.va & 0xff_ffff) << 3) | u32::from(state.vbit & 7);
        self.width = state.vb.min(16);
        self.advance_on_read = state.hl;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_whole_byte_width_moves_the_cursor_one_byte() {
        let mut v = Sa1VarBit::default();
        v.write_control(0x08);
        assert_eq!((v.address(), v.bit()), (1, 0));
        assert_eq!(v.field([0x12, 0xab, 0x00]) & 0xff, 0x12);
    }

    #[test]
    fn the_field_starts_at_the_cursor_bit() {
        let mut v = Sa1VarBit::default();
        v.write_control(0x04);
        assert_eq!(v.bit(), 4);
        assert_eq!(v.field([0xf0, 0x0f, 0x00]), 0x00ff);
    }

    #[test]
    fn a_field_can_cross_a_byte_boundary() {
        let v = Sa1VarBit::default();
        let field = v.field([0xbc, 0x0a, 0x00]);
        assert_eq!(field & 0x0fff, 0x0abc);
        assert_eq!((field & 0xff, field >> 8), (0xbc, 0x0a));
    }

    #[test]
    fn with_bit_7_clear_each_write_of_vbd_moves_the_cursor() {
        let mut v = Sa1VarBit::default();
        v.write_address(0, 0x10);
        v.write_address(2, 0x00);
        v.write_control(0x05);
        assert_eq!((v.address(), v.bit()), (0x10, 5));
        v.write_control(0x05);
        assert_eq!((v.address(), v.bit()), (0x11, 2));
    }

    #[test]
    fn a_width_of_zero_means_sixteen() {
        let mut v = Sa1VarBit::default();
        v.write_control(0x80);
        assert_eq!(v.width(), 16);
        assert_eq!(v.field([0x34, 0x12, 0x00]), 0x1234);
    }

    #[test]
    fn with_bit_7_set_only_an_advance_moves_the_cursor() {
        let mut v = Sa1VarBit::default();
        v.write_control(0x8c);
        assert!(v.advances_on_read());
        assert_eq!((v.address(), v.bit()), (0, 0));
        let _ = v.field([0xff, 0xff, 0xff]);
        assert_eq!(v.address(), 0);
        v.advance();
        assert_eq!((v.address(), v.bit()), (1, 4));
    }

    #[test]
    fn writing_the_high_address_byte_goes_back_to_bit_zero() {
        let mut v = Sa1VarBit::default();
        v.write_control(0x03);
        assert_eq!(v.bit(), 3);
        v.write_address(2, 0x12);
        assert_eq!((v.address(), v.bit()), (0x12_0000, 0));
    }

    #[test]
    fn the_cursor_wraps_at_24_address_bits() {
        let mut v = Sa1VarBit::default();
        for index in 0..3 {
            v.write_address(index, 0xff);
        }
        v.write_control(0x08);
        assert_eq!((v.address(), v.bit()), (0, 0));
    }

    #[test]
    fn a_savestate_round_trips_and_is_kept_in_range() {
        let mut v = Sa1VarBit::default();
        v.write_address(0, 0x11);
        v.write_address(1, 0x22);
        v.write_control(0x86);
        let snap = v.snapshot();
        let mut restored = Sa1VarBit::default();
        restored.restore(&snap);
        assert_eq!(restored.snapshot(), snap);

        let corrupt = Sa1VarBitState {
            va: 0x1ff_ffff,
            vbit: 0x55,
            vb: 200,
            hl: true,
        };
        let mut r = Sa1VarBit::default();
        r.restore(&corrupt);
        assert_eq!((r.address(), r.bit(), r.width()), (0xff_ffff, 0x55 & 7, 16));
    }

    #[test]
    fn a_reader_never_written_keeps_a_width_of_zero() {
        let v = Sa1VarBit::default();
        let mut restored = Sa1VarBit::default();
        restored.restore(&v.snapshot());
        assert_eq!(restored.snapshot(), v.snapshot());
        assert_eq!(restored.width(), 0);
    }
}
