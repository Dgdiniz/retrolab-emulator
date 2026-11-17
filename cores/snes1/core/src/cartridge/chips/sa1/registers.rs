//! How the SA-1's registers take a write: bit fields inside a byte, and
//! registers wider than a byte written one byte at a time, lowest address
//! first.

/// Bit `bit` of a register byte, as a flag.
#[inline]
pub(super) fn flag(value: u8, bit: u8) -> bool {
    value & (1 << bit) != 0
}

/// The field of `width` bits (1 to 8) that starts at bit `low`.
#[inline]
pub(super) fn field(value: u8, low: u8, width: u8) -> u8 {
    debug_assert!((1..=8).contains(&width));
    (value >> low) & (0xff >> (8 - width))
}

/// A byte of flags: each flag at its bit, over the `low` bits given.
pub(super) fn pack<const N: usize>(bits: [(bool, u8); N], low: u8) -> u8 {
    bits.iter()
        .fold(low, |byte, &(on, bit)| byte | (u8::from(on) << bit))
}

/// A register two or three bytes wide, which the CPU writes and reads one
/// byte at a time: byte `index` is the one at the register's address plus
/// `index`.
pub(super) trait Bytes: Sized {
    /// The register with byte `index` replaced by `value`.
    fn with_byte(self, index: usize, value: u8) -> Self;
    /// Byte `index` of the register.
    fn byte(self, index: usize) -> u8;
}

impl Bytes for u16 {
    fn with_byte(self, index: usize, value: u8) -> Self {
        let mut bytes = self.to_le_bytes();
        bytes[index] = value;
        Self::from_le_bytes(bytes)
    }

    fn byte(self, index: usize) -> u8 {
        self.to_le_bytes()[index]
    }
}

impl Bytes for u32 {
    fn with_byte(self, index: usize, value: u8) -> Self {
        let mut bytes = self.to_le_bytes();
        bytes[index] = value;
        Self::from_le_bytes(bytes)
    }

    fn byte(self, index: usize) -> u8 {
        self.to_le_bytes()[index]
    }
}

impl Bytes for u64 {
    fn with_byte(self, index: usize, value: u8) -> Self {
        let mut bytes = self.to_le_bytes();
        bytes[index] = value;
        Self::from_le_bytes(bytes)
    }

    fn byte(self, index: usize) -> u8 {
        self.to_le_bytes()[index]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_and_field_read_the_named_bits() {
        assert!(flag(0x80, 7));
        assert!(!flag(0x7f, 7));
        assert_eq!(field(0b1011_0110, 2, 3), 0b101);
        assert_eq!(field(0xff, 0, 8), 0xff);
        assert_eq!(pack([(true, 7), (false, 6), (true, 4)], 0x03), 0x93);
    }

    #[test]
    fn a_byte_write_keeps_the_other_bytes() {
        assert_eq!(0x1234u16.with_byte(0, 0xab), 0x12ab);
        assert_eq!(0x1234u16.with_byte(1, 0xab), 0xab34);
        assert_eq!(0x00_1234u32.with_byte(2, 0x56), 0x56_1234);
        assert_eq!(0x56_1234u32.byte(2), 0x56);
        assert_eq!(0xff_0000_0000u64.byte(4), 0xff);
    }
}
