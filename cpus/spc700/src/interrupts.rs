//! SPC700 reset and interrupt vectors.
//!
//! The SPC700 only honours `BRK` and external reset; there is no
//! maskable IRQ exposed to the SNES side. Both `BRK` and the reset
//! line read their 16-bit target from the top of the address space.

/// Reset vector (`$FFFE` low, `$FFFF` high). Latched by the IPL ROM
/// on power-up; the SNES boot vector at `$FFC0` is reached because
/// the canonical IPL writes `$FFC0` here before the first read.
pub const RESET_VECTOR: u16 = 0xFFFE;

/// `BRK` software-interrupt vector. Shares the layout with the
/// reset vector and is fetched at the same physical address.
pub const BRK_VECTOR: u16 = 0xFFDE;
