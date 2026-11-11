//! The status register's bits and the flags each accumulator carries.

/// SR, the status register, kept whole in [`Upd7725::status`]; these are
/// the bits the chip acts on. The others, the two output ports, the
/// serial and interrupt controls, the DMA flag, the two user flags, are
/// only stored: the program reads them back with the rest of SR, and the
/// S-CPU sees the high byte.
///
/// [`Upd7725::status`]: crate::Upd7725::status
pub mod status {
    /// RQM, request for master: the chip has put a word in DR for the
    /// S-CPU, or taken one and wants the next. The chip sets it when it
    /// reads or writes DR itself; the S-CPU's transfer of the last byte
    /// clears it.
    pub const HOST_REQUEST: u16 = 0x8000;
    /// DRS: the low byte of a 16-bit transfer has gone, so the next byte
    /// the S-CPU reads or writes is the high one.
    pub const HIGH_BYTE_NEXT: u16 = 0x1000;
    /// DRC: transfers through DR are a byte each instead of a word.
    pub const EIGHT_BIT_TRANSFERS: u16 = 0x0400;
    /// What a load of SR leaves alone: RQM and DRS, which belong to the
    /// transfers, and bits 2-6, which the chip does not have.
    pub const KEPT_ON_LOAD: u16 = HOST_REQUEST | HIGH_BYTE_NEXT | 0x007C;
}

/// FLAGA or FLAGB: what the last ALU operation on an accumulator left.
///
/// The two latched flags are what a saturating sum is built on. An
/// overflow sets `latched_overflow`, and `latched_sign` keeps the sign of
/// the result that overflowed. A further overflow while the latch is set
/// clears it if the sign has gone back the other way, two overflows in
/// opposite directions cancel, and leaves it set if not. The SGN source
/// turns `latched_sign` into the value to saturate to.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct AluFlags {
    /// OV0: this operation overflowed.
    pub overflow: bool,
    /// OV1: an overflow is outstanding.
    pub latched_overflow: bool,
    /// Z: the result is zero.
    pub zero: bool,
    /// C: the carry or borrow out of the operation.
    pub carry: bool,
    /// S0: bit 15 of the result.
    pub sign: bool,
    /// S1: follows `sign` while no overflow is outstanding, and keeps the
    /// sign of the result that overflowed while one is.
    pub latched_sign: bool,
}

impl AluFlags {
    /// The flags as six bits, OV0 in bit 0 up to S1 in bit 5, how the
    /// savestate and the debugger carry them.
    pub fn to_bits(self) -> u16 {
        u16::from(self.overflow)
            | u16::from(self.latched_overflow) << 1
            | u16::from(self.zero) << 2
            | u16::from(self.carry) << 3
            | u16::from(self.sign) << 4
            | u16::from(self.latched_sign) << 5
    }

    /// The flags from the six bits [`AluFlags::to_bits`] makes; the bits
    /// above them are not read.
    pub fn from_bits(bits: u16) -> Self {
        let bit = |n: u32| bits >> n & 1 != 0;
        Self {
            overflow: bit(0),
            latched_overflow: bit(1),
            zero: bit(2),
            carry: bit(3),
            sign: bit(4),
            latched_sign: bit(5),
        }
    }
}
