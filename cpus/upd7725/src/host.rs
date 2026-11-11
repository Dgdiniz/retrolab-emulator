//! The chip as the S-CPU sees it: DR and SR.
//!
//! A word crosses DR a byte at a time, low byte first, unless the program
//! has set DRC for single bytes. DRS says which half comes next, and the
//! S-CPU's transfer of the last byte clears RQM, the chip's sign that it
//! is waiting for the S-CPU.

use crate::registers::status::{EIGHT_BIT_TRANSFERS, HIGH_BYTE_NEXT, HOST_REQUEST};
use crate::Upd7725;

impl Upd7725 {
    /// SR's high byte, which is all the S-CPU reads of it: RQM in bit 7.
    ///
    /// DRS is read as it is stored, also while DRC has transfers a byte
    /// each and it no longer means anything, as bsnes-plus has it, and
    /// this interpreter keeps it. ares reads it as clear then, through
    /// here, through the SR source and through a load of SR, which then
    /// clears it.
    pub fn read_status(&self) -> u8 {
        (self.status >> 8) as u8
    }

    /// The S-CPU cannot write SR; the write is dropped.
    pub fn write_status(&mut self, _value: u8) {}

    /// The S-CPU reads a byte of DR.
    pub fn read_data(&mut self) -> u8 {
        if self.status & EIGHT_BIT_TRANSFERS != 0 {
            self.status &= !HOST_REQUEST;
            return self.data_port as u8;
        }
        if self.status & HIGH_BYTE_NEXT == 0 {
            self.status |= HIGH_BYTE_NEXT;
            self.data_port as u8
        } else {
            self.status &= !(HIGH_BYTE_NEXT | HOST_REQUEST);
            (self.data_port >> 8) as u8
        }
    }

    /// The S-CPU writes a byte of DR.
    pub fn write_data(&mut self, value: u8) {
        if self.status & EIGHT_BIT_TRANSFERS != 0 {
            self.status &= !HOST_REQUEST;
            self.data_port = self.data_port & 0xFF00 | u16::from(value);
        } else if self.status & HIGH_BYTE_NEXT == 0 {
            self.status |= HIGH_BYTE_NEXT;
            self.data_port = self.data_port & 0xFF00 | u16::from(value);
        } else {
            self.status &= !(HIGH_BYTE_NEXT | HOST_REQUEST);
            self.data_port = u16::from(value) << 8 | self.data_port & 0x00FF;
        }
    }

    /// DR's low byte, read without moving the transfer on, for the
    /// debugger.
    pub fn peek_data(&self) -> u8 {
        self.data_port as u8
    }
}
