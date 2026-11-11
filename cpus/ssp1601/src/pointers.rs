//! Pointer-register indirection: `(rX)` (first level, into internal RAM,
//! with optional post-inc/dec and RPL modulo) and `((rX))` (second level,
//! the RAM word indexes program memory and is post-incremented).
//!
//! Faithful port of `ssp16.c`'s `ptr1_read_` / `ptr1_write` / `ptr2_read`.
//! `r0..r3` index RAM bank 0 (`ram[0..256]`), `r4..r7` index bank 1
//! (`ram[256..512]`). `r3`/`r7` are fixed and short-form-addressed
//! (`(r3|NN)`).

use crate::{Ssp1601, SSP_ST};

impl Ssp1601 {
    /// `(rX)` read, `ptr1_read(op)` = `ptr1_read_(op&3, (op>>6)&4, (op<<1)&0x18)`.
    pub(crate) fn ptr1_read(&mut self, op: u16) -> u16 {
        self.ptr1_read_(op & 3, (op >> 6) & 4, (op << 1) & 0x18)
    }

    /// The decomposed `(rX)` read used directly by the MAC ops (they build
    /// the `ri`/`rj` selectors themselves).
    pub(crate) fn ptr1_read_(&mut self, ri: u16, isj2: u16, modi3: u16) -> u16 {
        let t = (ri | isj2 | modi3) as usize;
        let rpl = (self.reg16(SSP_ST) & 7) as u32;
        match t {
            // mod=0 (00): plain
            0x00..=0x02 => self.ram[self.ptr[t & 3] as usize],
            0x03 => self.ram[0],
            0x04..=0x06 => self.ram[256 + self.ptr[4 + (t & 3)] as usize],
            0x07 => self.ram[256],
            // mod=1 (01): "+!" post-increment, unaffected by RPL
            0x08..=0x0a => {
                let pi = t & 3;
                let v = self.ram[self.ptr[pi] as usize];
                self.ptr[pi] = self.ptr[pi].wrapping_add(1);
                v
            }
            0x0b => self.ram[1],
            0x0c..=0x0e => {
                let pi = 4 + (t & 3);
                let v = self.ram[256 + self.ptr[pi] as usize];
                self.ptr[pi] = self.ptr[pi].wrapping_add(1);
                v
            }
            0x0f => self.ram[256 + 1],
            // mod=2 (10): "-" post-decrement (RPL modulo)
            0x10..=0x12 => self.ptr_mod_read(t & 3, false, -1, rpl),
            0x13 => self.ram[2],
            0x14..=0x16 => self.ptr_mod_read(t & 3, true, -1, rpl),
            0x17 => self.ram[256 + 2],
            // mod=3 (11): "+" post-increment (RPL modulo)
            0x18..=0x1a => self.ptr_mod_read(t & 3, false, 1, rpl),
            0x1b => self.ram[3],
            0x1c..=0x1e => self.ptr_mod_read(t & 3, true, 1, rpl),
            0x1f => self.ram[256 + 3],
            _ => 0,
        }
    }

    /// Read `RAM[ptr]` then apply the post-inc/-dec: a plain step when RPL is
    /// zero, else a modulo step over the low `RPL` bits (`ssp16.c` `modulo:`).
    fn ptr_mod_read(&mut self, bank_off: usize, bank1: bool, add: i32, rpl: u32) -> u16 {
        let pi = if bank1 { 4 + bank_off } else { bank_off };
        let base = if bank1 { 256 } else { 0 };
        let ptr_val = self.ptr[pi];
        let v = self.ram[base + ptr_val as usize];
        if rpl == 0 {
            self.ptr[pi] = if add < 0 {
                ptr_val.wrapping_sub(1)
            } else {
                ptr_val.wrapping_add(1)
            };
        } else {
            let mask = (1u32 << rpl) - 1;
            let p = ptr_val as u32;
            self.ptr[pi] = ((p & !mask) | (p.wrapping_add(add as u32) & mask)) as u8;
        }
        v
    }

    /// `(rX)` write, post-increment (mod 1/3) or post-decrement (mod 2);
    /// writes ignore RPL modulo (the reference never applies it here).
    pub(crate) fn ptr1_write(&mut self, op: u16, d: u16) {
        let t = ((op & 3) | ((op >> 6) & 4) | ((op << 1) & 0x18)) as usize;
        match t {
            0x00..=0x02 => {
                let i = self.ptr[t & 3] as usize;
                self.ram[i] = d;
            }
            0x03 => self.ram[0] = d,
            0x04..=0x06 => {
                let i = 256 + self.ptr[4 + (t & 3)] as usize;
                self.ram[i] = d;
            }
            0x07 => self.ram[256] = d,
            // mod 1 ("+!") and mod 3 ("+"): post-increment
            0x08..=0x0a | 0x18..=0x1a => {
                let pi = t & 3;
                let i = self.ptr[pi] as usize;
                self.ram[i] = d;
                self.ptr[pi] = self.ptr[pi].wrapping_add(1);
            }
            0x0b => self.ram[1] = d,
            0x0c..=0x0e | 0x1c..=0x1e => {
                let pi = 4 + (t & 3);
                let i = 256 + self.ptr[pi] as usize;
                self.ram[i] = d;
                self.ptr[pi] = self.ptr[pi].wrapping_add(1);
            }
            0x0f => self.ram[256 + 1] = d,
            // mod 2 ("-"): post-decrement
            0x10..=0x12 => {
                let pi = t & 3;
                let i = self.ptr[pi] as usize;
                self.ram[i] = d;
                self.ptr[pi] = self.ptr[pi].wrapping_sub(1);
            }
            0x13 => self.ram[2] = d,
            0x14..=0x16 => {
                let pi = 4 + (t & 3);
                let i = 256 + self.ptr[pi] as usize;
                self.ram[i] = d;
                self.ptr[pi] = self.ptr[pi].wrapping_sub(1);
            }
            0x17 => self.ram[256 + 2] = d,
            0x1b => self.ram[3] = d,
            0x1f => self.ram[256 + 3] = d,
            _ => {}
        }
    }

    /// `((rX))` read: the RAM word at `(rX)` is used as a program-memory
    /// address; the RAM word is post-incremented and `iram_rom[old]` is
    /// returned.
    pub(crate) fn ptr2_read(&mut self, op: u16) -> u16 {
        let t = ((op & 3) | ((op >> 6) & 4) | ((op << 1) & 0x18)) as usize;
        let (base, idx) = match t {
            0x00..=0x02 => (0usize, self.ptr[t & 3] as usize),
            0x03 => (0, 0),
            0x04..=0x06 => (256usize, self.ptr[4 + (t & 3)] as usize),
            0x07 => (256, 0),
            0x0b => (0, 1),
            0x0f => (256, 1),
            0x13 => (0, 2),
            0x17 => (256, 2),
            0x1b => (0, 3),
            0x1f => (256, 3),
            _ => return 0,
        };
        let cell = base + idx;
        let mv = self.ram[cell];
        self.ram[cell] = mv.wrapping_add(1);
        self.iram_rom[mv as usize]
    }
}
