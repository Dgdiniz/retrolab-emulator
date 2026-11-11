//! The 64-byte SPC700 IPL boot ROM.
//!
//! Mapped read-only at `$FFC0-$FFFF` whenever CONTROL (`$F1`) bit 7
//! is set (the power-on default). On reset the SPC700 fetches its
//! vector from `$FFFE`/`$FFFF`, which fall inside this ROM and point
//! at `$FFC0`, so the chip always boots into the IPL. The IPL writes
//! the ready handshake bytes (`$AA` to port 0, `$BB` to port 1) and
//! then spins on the mailbox waiting for the main CPU to drive the
//! upload protocol.
//!
//! These 64 bytes are fixed silicon, identical across every SNES /
//! Super Famicom revision.

/// SPC700 boot ROM contents (`$FFC0-$FFFF`). The last two bytes are
/// the reset vector (`$FFC0`, little-endian).
pub const IPL_ROM: [u8; 64] = [
    0xCD, 0xEF, 0xBD, 0xE8, 0x00, 0xC6, 0x1D, 0xD0, //
    0xFC, 0x8F, 0xAA, 0xF4, 0x8F, 0xBB, 0xF5, 0x78, //
    0xCC, 0xF4, 0xD0, 0xFB, 0x2F, 0x19, 0xEB, 0xF4, //
    0xD0, 0xFC, 0x7E, 0xF4, 0xD0, 0x0B, 0xE4, 0xF5, //
    0xCB, 0xF4, 0xD7, 0x00, 0xFC, 0xD0, 0xF3, 0xAB, //
    0x01, 0x10, 0xEF, 0x7E, 0xF4, 0x10, 0xEB, 0xBA, //
    0xF6, 0xDA, 0x00, 0xBA, 0xF4, 0xC4, 0xF4, 0xDD, //
    0x5D, 0xD0, 0xDB, 0x1F, 0x00, 0x00, 0xC0, 0xFF, //
];

/// First ROM address backed by [`IPL_ROM`] (`$FFC0`).
pub const IPL_BASE: u16 = 0xFFC0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipl_rom_is_64_bytes() {
        assert_eq!(IPL_ROM.len(), 64);
    }

    #[test]
    fn ipl_reset_vector_points_at_ipl_base() {
        // $FFFE/$FFFF (the last two bytes) are the reset vector and
        // must point back at $FFC0 so the chip boots into the IPL.
        let lo = IPL_ROM[0x3E] as u16;
        let hi = IPL_ROM[0x3F] as u16;
        assert_eq!((hi << 8) | lo, IPL_BASE);
    }

    #[test]
    fn ipl_first_opcode_is_known() {
        // `MOV X,#$EF` (CD EF), the documented IPL entry point that
        // sets up the stack pointer.
        assert_eq!(IPL_ROM[0], 0xCD);
        assert_eq!(IPL_ROM[1], 0xEF);
    }
}
