//! A pad that shifts its buttons out on a serial line.
//!
//! The pads of the SNES (16 bits) and the NES (8 bits) work the same way:
//! a latch line, held high, loads the buttons into a shift register, and
//! each read clocks one bit out of it, the first button first; past the
//! last bit the data line reads `1` (the pull-up of an open collector).
//! The console's automatic read takes the whole register at once.

use serde::{Deserialize, Serialize};

/// A pad `BITS` bits long (`BITS` at most 16). The buttons are in the
/// console's hardware order: the first bit out is bit `BITS - 1`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SerialPad<const BITS: u32> {
    /// The buttons held.
    buttons: u16,
    /// The serial shift register; bit `BITS - 1` is the next bit out.
    shift_register: u16,
    /// The latch line. While it is high the register is loaded again and
    /// again, so a read gives the first button.
    strobe: bool,
}

/// What a savestate keeps of a [`SerialPad`], in the order of its bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerialPadState {
    pub buttons: u16,
    pub shift_register: u16,
    pub strobe: bool,
}

impl<const BITS: u32> SerialPad<BITS> {
    /// The bits the register holds.
    const MASK: u16 = if BITS >= 16 {
        u16::MAX
    } else {
        (1u16 << BITS) - 1
    };

    /// A pad with no button held and the latch line low.
    pub fn new() -> Self {
        Self::default()
    }

    /// Hold `state`, in the console's hardware order.
    #[inline]
    pub fn set_buttons(&mut self, state: u16) {
        self.buttons = state;
    }

    /// The buttons held, as the console's automatic read takes them.
    #[inline]
    pub fn buttons(&self) -> u16 {
        self.buttons
    }

    /// Drive the latch line. Its high-to-low edge loads the buttons into
    /// the shift register.
    #[inline]
    pub fn strobe(&mut self, high: bool) {
        if self.strobe && !high {
            self.shift_register = self.buttons;
        }
        self.strobe = high;
    }

    /// One bit of the data line, the register clocked on. With the latch
    /// line high it is the first button; past the last bit it is `1`.
    #[inline]
    pub fn read_serial(&mut self) -> u8 {
        if self.strobe {
            return ((self.buttons >> (BITS - 1)) & 1) as u8;
        }
        let bit = (self.shift_register >> (BITS - 1)) & 1;
        self.shift_register = ((self.shift_register << 1) | 1) & Self::MASK;
        bit as u8
    }

    /// The register as the console's automatic read leaves it: every bit
    /// clocked out, so a read without a new latch gives `1`.
    pub fn exhaust_serial(&mut self) {
        self.shift_register = Self::MASK;
    }

    /// The pad as a savestate keeps it.
    pub fn state(&self) -> SerialPadState {
        SerialPadState {
            buttons: self.buttons,
            shift_register: self.shift_register,
            strobe: self.strobe,
        }
    }

    /// Make the pad the state's.
    pub fn restore(&mut self, state: &SerialPadState) {
        self.buttons = state.buttons;
        self.shift_register = state.shift_register;
        self.strobe = state.strobe;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all<const BITS: u32>(pad: &mut SerialPad<BITS>, n: u32) -> Vec<u8> {
        (0..n).map(|_| pad.read_serial()).collect()
    }

    #[test]
    fn sixteen_bits_shift_out_first_button_first_then_ones() {
        let mut pad = SerialPad::<16>::new();
        pad.set_buttons(0b1010_0000_0000_0001);
        pad.strobe(true);
        assert_eq!(
            pad.read_serial(),
            1,
            "the latch held high reads the first button"
        );
        pad.strobe(false);
        let bits = read_all(&mut pad, 18);
        assert_eq!(&bits[..4], &[1, 0, 1, 0]);
        assert_eq!(bits[15], 1);
        assert_eq!(&bits[16..], &[1, 1], "past the last bit the line reads 1");
    }

    #[test]
    fn eight_bits_keep_their_own_length() {
        let mut pad = SerialPad::<8>::new();
        pad.set_buttons(0b0100_0001);
        pad.strobe(true);
        assert_eq!(pad.read_serial(), 0);
        pad.strobe(false);
        assert_eq!(read_all(&mut pad, 10), vec![0, 1, 0, 0, 0, 0, 0, 1, 1, 1]);
    }

    #[test]
    fn the_automatic_read_takes_the_buttons_and_exhausts_the_line() {
        let mut pad = SerialPad::<16>::new();
        pad.set_buttons(0xC080);
        assert_eq!(pad.buttons(), 0xC080);
        pad.exhaust_serial();
        assert_eq!(pad.read_serial(), 1);
    }

    #[test]
    fn a_state_round_trips() {
        let mut pad = SerialPad::<16>::new();
        pad.set_buttons(0x8001);
        pad.strobe(true);
        pad.strobe(false);
        pad.read_serial();
        let state = pad.state();
        let mut other = SerialPad::<16>::new();
        other.restore(&state);
        assert_eq!(other.state(), state);
        assert_eq!(other.read_serial(), pad.read_serial());
    }
}
