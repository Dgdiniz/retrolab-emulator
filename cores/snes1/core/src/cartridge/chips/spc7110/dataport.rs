//! SPC7110 direct data-ROM access unit (`$4810-$481A`).
//!
//! A byte-faithful port of the data port unit of the SPC7110 emulator by
//! byuu and neviksti (its notice is at the end of `LICENSE`), read in the
//! copy bsnes-plus carries. The unit reads the DATA ROM through a
//! 24-bit **pointer** (`$4811-$4813`) with an optional 16-bit **adjust**
//! (`$4814/$4815`) and 16-bit **increment** (`$4816/$4817`); the `$4818`
//! control byte is a bit matrix that decides, per read, which of those advance
//! and how (see [`Spc7110DataPort::read_4810`] /
//! [`Spc7110DataPort::read_481a`]).
//!
//! Two gates guard the unit, exactly like the hardware:
//!
//! - **Ready latch** `r481x`: bits 0-2 set as `$4811/$4812/$4813` are written.
//!   The read ports and the `$4818` control write are inert until all three
//!   pointer bytes have been loaded (`r481x == 0x07`).
//! - **Adjust write latches**: writing `$4814` *and* `$4815` (in either order)
//!   can pre-advance the pointer by the adjust value, but only in adjust mode
//!   (`$4818 bit1`) with the "apply to adjust" bit (`bit4`) clear.
//!
//! The unit does not own the ROM: the two read ports take a `read_rom` closure
//! that maps a *logical* data-ROM address through `datarom_addr` and returns
//! the byte, keeping this module pure and unit-testable.

use crate::savestate::Spc7110DataPortState;

/// The SPC7110 data-port register file (`$4811-$4818`) plus the ready / adjust
/// latches. `$4810` and `$481A` are read-only FIFO-style ports with side
/// effects, handled by [`Self::read_4810`] / [`Self::read_481a`].
#[derive(Clone, Default)]
pub(super) struct Spc7110DataPort {
    /// `$4811-$4813`: 24-bit data pointer, little-endian.
    pointer: [u8; 3],
    /// `$4814-$4815`: 16-bit data adjust, little-endian.
    adjust: [u8; 2],
    /// `$4816-$4817`: 16-bit data increment, little-endian.
    increment: [u8; 2],
    /// `$4818`: control-bit matrix.
    control: u8,
    /// `r481x` ready latch: bits 0-2 track the three pointer-byte writes; the
    /// ports and the `$4818` write stay inert until it reaches `0x07`.
    ready: u8,
    /// `$4814` written since the last `$4818` write (for the adjust-write
    /// pre-advance side effect).
    latch_4814: bool,
    /// `$4815` written since the last `$4818` write.
    latch_4815: bool,
}

impl Spc7110DataPort {
    /// 24-bit data pointer (`$4811 + $4812<<8 + $4813<<16`).
    fn data_pointer(&self) -> u32 {
        u32::from(self.pointer[0])
            | (u32::from(self.pointer[1]) << 8)
            | (u32::from(self.pointer[2]) << 16)
    }

    /// 16-bit data adjust (`$4814 + $4815<<8`), not yet sign-extended.
    fn data_adjust(&self) -> u32 {
        u32::from(self.adjust[0]) | (u32::from(self.adjust[1]) << 8)
    }

    /// 16-bit data increment (`$4816 + $4817<<8`).
    fn data_increment(&self) -> u32 {
        u32::from(self.increment[0]) | (u32::from(self.increment[1]) << 8)
    }

    /// Store the low 24 bits of `addr` back into the pointer registers.
    fn set_data_pointer(&mut self, addr: u32) {
        self.pointer[0] = addr as u8;
        self.pointer[1] = (addr >> 8) as u8;
        self.pointer[2] = (addr >> 16) as u8;
    }

    /// Store the low 16 bits of `addr` back into the adjust registers.
    fn set_data_adjust(&mut self, addr: u32) {
        self.adjust[0] = addr as u8;
        self.adjust[1] = (addr >> 8) as u8;
    }

    /// Read a plain latch register (`$4811-$4818`) with no side effects, also
    /// the debugger path for every data-port address. `$4810`/`$481A` return
    /// `0` here (their real reads have side effects and go through the port
    /// methods).
    pub(super) fn read(&self, reg: u16) -> u8 {
        match reg {
            0x4811..=0x4813 => self.pointer[(reg - 0x4811) as usize],
            0x4814..=0x4815 => self.adjust[(reg - 0x4814) as usize],
            0x4816..=0x4817 => self.increment[(reg - 0x4816) as usize],
            0x4818 => self.control,
            _ => 0,
        }
    }

    /// Write a data-port register (`$4811-$4818`). Loading a pointer byte sets
    /// its ready bit; `$4814`/`$4815` may pre-advance the pointer; a `$4818`
    /// write is ignored until the pointer is fully loaded and clears the adjust
    /// latches.
    pub(super) fn write(&mut self, reg: u16, data: u8) {
        match reg {
            0x4811 => {
                self.pointer[0] = data;
                self.ready |= 0x01;
            }
            0x4812 => {
                self.pointer[1] = data;
                self.ready |= 0x02;
            }
            0x4813 => {
                self.pointer[2] = data;
                self.ready |= 0x04;
            }
            0x4814 => {
                self.adjust[0] = data;
                self.latch_4814 = true;
                let other = self.latch_4815;
                self.adjust_write_advance(other);
            }
            0x4815 => {
                self.adjust[1] = data;
                self.latch_4815 = true;
                let other = self.latch_4814;
                self.adjust_write_advance(other);
            }
            0x4816 => self.increment[0] = data,
            0x4817 => self.increment[1] = data,
            0x4818 => {
                if self.ready != 0x07 {
                    return;
                }
                self.control = data;
                self.latch_4814 = false;
                self.latch_4815 = false;
            }
            _ => {}
        }
    }

    /// Read port `$4810`. Returns `0` until the pointer is fully loaded.
    /// Otherwise reads the byte at the (optionally adjust-offset) pointer and
    /// advances the pointer or adjust per the `$4818` matrix.
    pub(super) fn read_4810(&mut self, read_rom: impl Fn(u32) -> u8) -> u8 {
        if self.ready != 0x07 {
            return 0;
        }
        let addr = self.data_pointer();
        let adjust = self.sign_extended_adjust();

        let mut adjustaddr = addr;
        if self.control & 0x02 != 0 {
            adjustaddr = adjustaddr.wrapping_add(adjust);
            self.set_data_adjust(adjust.wrapping_add(1));
        }

        let data = read_rom(adjustaddr);

        if self.control & 0x02 == 0 {
            let mut increment = if self.control & 0x01 != 0 {
                self.data_increment()
            } else {
                1
            };
            if self.control & 0x04 != 0 {
                increment = sign_extend_16(increment);
            }
            if self.control & 0x10 == 0 {
                self.set_data_pointer(addr.wrapping_add(increment));
            } else {
                self.set_data_adjust(adjust.wrapping_add(increment));
            }
        }

        data
    }

    /// Read port `$481A`. Returns `0` until the pointer is fully loaded.
    /// Otherwise reads the byte at `pointer + adjust`; only the `$4818 bit5|bit6
    /// == both set` mode advances the pointer/adjust.
    pub(super) fn read_481a(&mut self, read_rom: impl Fn(u32) -> u8) -> u8 {
        if self.ready != 0x07 {
            return 0;
        }
        let addr = self.data_pointer();
        let adjust = self.sign_extended_adjust();

        let data = read_rom(addr.wrapping_add(adjust));

        if self.control & 0x60 == 0x60 {
            if self.control & 0x10 == 0 {
                self.set_data_pointer(addr.wrapping_add(adjust));
            } else {
                self.set_data_adjust(adjust.wrapping_add(adjust));
            }
        }

        data
    }

    /// The `$4814`/`$4815` write side effect: pre-advance the pointer by the
    /// adjust value, but only when the *other* adjust byte has also been
    /// written since the last `$4818`, adjust mode is on (`bit1`), and the
    /// "apply to adjust" bit (`bit4`) is clear. `bit5/bit6` pick an 8- or
    /// 16-bit adjust step; `bit3` sign-extends it.
    fn adjust_write_advance(&mut self, other_latched: bool) {
        if !other_latched {
            return;
        }
        if self.control & 0x02 == 0 {
            return;
        }
        if self.control & 0x10 != 0 {
            return;
        }

        let mode = self.control & 0x60;
        if mode == 0x20 {
            let mut increment = self.data_adjust() & 0xff;
            if self.control & 0x08 != 0 {
                increment = sign_extend_8(increment);
            }
            self.set_data_pointer(self.data_pointer().wrapping_add(increment));
        } else if mode == 0x40 {
            let mut increment = self.data_adjust();
            if self.control & 0x08 != 0 {
                increment = sign_extend_16(increment);
            }
            self.set_data_pointer(self.data_pointer().wrapping_add(increment));
        }
    }

    /// The adjust value, sign-extended to 32 bits when `$4818 bit3` is set.
    fn sign_extended_adjust(&self) -> u32 {
        let adjust = self.data_adjust();
        if self.control & 0x08 != 0 {
            sign_extend_16(adjust)
        } else {
            adjust
        }
    }

    /// 24-bit data pointer, for the debug view.
    pub(super) fn pointer(&self) -> u32 {
        self.data_pointer()
    }

    /// 16-bit data adjust (raw, not sign-extended), for the debug view.
    pub(super) fn adjust(&self) -> u16 {
        self.data_adjust() as u16
    }

    /// `$4818` control byte, for the debug view.
    pub(super) fn control(&self) -> u8 {
        self.control
    }

    /// `r481x` ready latch, for the debug view.
    pub(super) fn ready(&self) -> u8 {
        self.ready
    }

    /// Capture the volatile register file for the savestate.
    pub(super) fn snapshot(&self) -> Spc7110DataPortState {
        Spc7110DataPortState {
            pointer: self.pointer,
            adjust: self.adjust,
            increment: self.increment,
            control: self.control,
            ready: self.ready,
            latch_4814: self.latch_4814,
            latch_4815: self.latch_4815,
        }
    }

    /// Restore the register file from a savestate.
    pub(super) fn restore_state(&mut self, s: &Spc7110DataPortState) {
        self.pointer = s.pointer;
        self.adjust = s.adjust;
        self.increment = s.increment;
        self.control = s.control;
        self.ready = s.ready;
        self.latch_4814 = s.latch_4814;
        self.latch_4815 = s.latch_4815;
    }
}

/// Sign-extend the low 16 bits of `value` to a full 32-bit two's-complement
/// value (bsnes `increment = (int16)increment`).
fn sign_extend_16(value: u32) -> u32 {
    (value as u16 as i16) as u32
}

/// Sign-extend the low 8 bits of `value` (bsnes `increment = (int8)increment`).
fn sign_extend_8(value: u32) -> u32 {
    (value as u8 as i8) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A port with the pointer fully loaded to `addr` and control `ctrl`.
    fn port(addr: u32, ctrl: u8) -> Spc7110DataPort {
        let mut p = Spc7110DataPort::default();
        p.write(0x4811, addr as u8);
        p.write(0x4812, (addr >> 8) as u8);
        p.write(0x4813, (addr >> 16) as u8);
        p.write(0x4818, ctrl);
        p
    }

    /// A closure that returns the low byte of the *logical* address requested,
    /// so tests can assert which address the port computed.
    fn echo_low(addr: u32) -> u8 {
        addr as u8
    }

    #[test]
    fn ports_are_inert_until_pointer_fully_loaded() {
        let mut p = Spc7110DataPort::default();
        // No pointer bytes written yet -> not ready.
        assert_eq!(p.read_4810(echo_low), 0);
        assert_eq!(p.read_481a(echo_low), 0);
        p.write(0x4811, 0x00);
        p.write(0x4812, 0x00);
        // Only two of three bytes -> still inert, and $4818 writes are ignored.
        p.write(0x4818, 0xFF);
        assert_eq!(p.control(), 0x00, "$4818 ignored until ready");
        assert_eq!(p.read_4810(echo_low), 0);
        p.write(0x4813, 0x00);
        assert_eq!(p.ready(), 0x07);
        // Now the control write takes.
        p.write(0x4818, 0x00);
        assert_eq!(p.read_4810(echo_low), 0x00);
    }

    #[test]
    fn read_4810_default_increments_pointer_by_one() {
        // control = 0: no adjust, increment by 1 each read, apply to pointer.
        let mut p = port(0x001000, 0x00);
        assert_eq!(p.read_4810(echo_low), 0x00); // reads $001000
        assert_eq!(p.read_4810(echo_low), 0x01); // pointer advanced to $001001
        assert_eq!(p.read_4810(echo_low), 0x02);
        assert_eq!(p.pointer(), 0x001003);
    }

    #[test]
    fn read_4810_uses_increment_register_when_bit0_set() {
        // control = 0x01: use $4816/$4817 increment.
        let mut p = port(0x000000, 0x01);
        p.write(0x4816, 0x10);
        p.write(0x4817, 0x00);
        assert_eq!(p.read_4810(echo_low), 0x00); // $000000
        assert_eq!(p.pointer(), 0x000010);
        assert_eq!(p.read_4810(echo_low), 0x10); // $000010
        assert_eq!(p.pointer(), 0x000020);
    }

    #[test]
    fn read_4810_adjust_mode_offsets_then_bumps_adjust() {
        // control = 0x02: adjustaddr = pointer + adjust; adjust++ after.
        let mut p = port(0x001000, 0x02);
        p.write(0x4814, 0x05);
        p.write(0x4815, 0x00);
        assert_eq!(p.read_4810(echo_low), 0x05); // reads $001005
        assert_eq!(p.adjust(), 0x0006, "adjust incremented");
        assert_eq!(p.pointer(), 0x001000, "pointer unchanged in adjust mode");
        assert_eq!(p.read_4810(echo_low), 0x06); // $001006
        assert_eq!(p.adjust(), 0x0007);
    }

    #[test]
    fn read_4810_increment_applies_to_adjust_when_bit4_set() {
        // control = 0x11: increment register + apply to adjust (not pointer).
        let mut p = port(0x002000, 0x11);
        p.write(0x4816, 0x04);
        p.write(0x4817, 0x00);
        assert_eq!(p.read_4810(echo_low), 0x00); // reads pointer $002000
        assert_eq!(p.pointer(), 0x002000, "pointer unchanged");
        assert_eq!(p.adjust(), 0x0004, "adjust advanced by increment");
    }

    #[test]
    fn read_4810_negative_increment_sign_extends() {
        // control = 0x05: use increment register, 16-bit sign-extend it.
        let mut p = port(0x001000, 0x05);
        p.write(0x4816, 0xFF);
        p.write(0x4817, 0xFF); // increment = -1
        assert_eq!(p.read_4810(echo_low), 0x00); // reads $001000
        assert_eq!(p.pointer(), 0x000FFF, "pointer decremented by 1");
    }

    #[test]
    fn read_481a_reads_pointer_plus_adjust_without_advancing_by_default() {
        // control = 0x08: sign-extend adjust, no auto-advance (bits5/6 clear).
        let mut p = port(0x001000, 0x08);
        p.write(0x4814, 0x10);
        p.write(0x4815, 0x00);
        assert_eq!(p.read_481a(echo_low), 0x10); // $001010
        assert_eq!(p.pointer(), 0x001000, "pointer unchanged");
        assert_eq!(p.adjust(), 0x0010, "adjust unchanged");
    }

    #[test]
    fn read_481a_advances_pointer_when_bits_5_and_6_set() {
        // control = 0x60: pointer += adjust after the read.
        let mut p = port(0x001000, 0x60);
        p.write(0x4814, 0x08);
        p.write(0x4815, 0x00);
        assert_eq!(p.read_481a(echo_low), 0x08); // $001008
        assert_eq!(p.pointer(), 0x001008, "pointer += adjust");
    }

    #[test]
    fn read_481a_doubles_adjust_when_bit4_also_set() {
        // control = 0x70: bits5/6 set + apply-to-adjust -> adjust += adjust.
        let mut p = port(0x000000, 0x70);
        p.write(0x4814, 0x09);
        p.write(0x4815, 0x00);
        let _ = p.read_481a(echo_low);
        assert_eq!(p.adjust(), 0x0012, "adjust doubled");
        assert_eq!(p.pointer(), 0x000000);
    }

    #[test]
    fn adjust_write_advances_pointer_in_mode_0x40() {
        // control = 0x42: adjust mode + 16-bit adjust-write pre-advance.
        let mut p = port(0x001000, 0x42);
        // Writing both $4814 and $4815 (second write triggers) advances the
        // pointer by the 16-bit adjust.
        p.write(0x4814, 0x00);
        p.write(0x4815, 0x01); // adjust = 0x0100 -> pointer += 0x100
        assert_eq!(p.pointer(), 0x001100);
    }

    #[test]
    fn adjust_write_advances_pointer_in_mode_0x20_low_byte() {
        // control = 0x22: adjust mode + 8-bit adjust-write pre-advance.
        let mut p = port(0x001000, 0x22);
        p.write(0x4814, 0x07);
        p.write(0x4815, 0xFF); // only low byte (0x07) used in 8-bit mode
        assert_eq!(p.pointer(), 0x001007);
    }

    #[test]
    fn adjust_write_needs_both_bytes_and_adjust_mode() {
        // control = 0x40 (bit1 clear): no pre-advance regardless of writes.
        let mut p = port(0x001000, 0x40);
        p.write(0x4814, 0x10);
        p.write(0x4815, 0x00);
        assert_eq!(p.pointer(), 0x001000, "no advance when bit1 clear");
    }

    #[test]
    fn latch_registers_round_trip() {
        let mut p = port(0x123456, 0x00);
        assert_eq!(p.read(0x4811), 0x56);
        assert_eq!(p.read(0x4812), 0x34);
        assert_eq!(p.read(0x4813), 0x12);
        p.write(0x4816, 0xAB);
        assert_eq!(p.read(0x4816), 0xAB);
        assert_eq!(p.read(0x4818), 0x00);
        // $4810/$481a return 0 on the plain (debugger) read path.
        assert_eq!(p.read(0x4810), 0x00);
        assert_eq!(p.read(0x481a), 0x00);
    }
}
