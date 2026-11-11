//! The instruction dispatch, `switch (op >> 9)` from `ssp16.c`'s
//! `ssp1601_run`. The top 7 bits pick the instruction class (~60 forms);
//! the low bits pick registers / addressing / condition.

use crate::{Ssp1601, SspHost, FLAG_N, FLAG_Z, SSP_A, SSP_P, SSP_ST, SSP_X, SSP_Y};

/// The six accumulator ALU operations shared across the addressing modes.
#[derive(Clone, Copy)]
enum Alu {
    Sub,
    Cmp,
    Add,
    And,
    Or,
    Eor,
}

impl Ssp1601 {
    // -- Accumulator operations (the `OP_*` macros) -----------------------

    /// `ld A, x`: 16-bit load into the accumulator's high word, no flags.
    #[inline]
    fn op_lda(&mut self, x: u16) {
        self.set_reg16(SSP_A, x);
    }

    #[inline]
    fn op_suba(&mut self, x: u16) {
        self.gr[SSP_A] = self.gr[SSP_A].wrapping_sub((x as u32) << 16);
        self.upd_lzvn();
    }

    #[inline]
    fn op_suba32(&mut self, x: u32) {
        self.gr[SSP_A] = self.gr[SSP_A].wrapping_sub(x);
        self.upd_lzvn();
    }

    #[inline]
    fn op_cmpa(&mut self, x: u16) {
        let t = self.gr[SSP_A].wrapping_sub((x as u32) << 16);
        self.set_flags_from(t);
    }

    #[inline]
    fn op_cmpa32(&mut self, x: u32) {
        let t = self.gr[SSP_A].wrapping_sub(x);
        self.set_flags_from(t);
    }

    #[inline]
    fn op_adda(&mut self, x: u16) {
        self.gr[SSP_A] = self.gr[SSP_A].wrapping_add((x as u32) << 16);
        self.upd_lzvn();
    }

    #[inline]
    fn op_adda32(&mut self, x: u32) {
        self.gr[SSP_A] = self.gr[SSP_A].wrapping_add(x);
        self.upd_lzvn();
    }

    #[inline]
    fn op_anda(&mut self, x: u16) {
        self.gr[SSP_A] &= (x as u32) << 16;
        self.upd_acc_zn();
    }

    #[inline]
    fn op_anda32(&mut self, x: u32) {
        self.gr[SSP_A] &= x;
        self.upd_acc_zn();
    }

    #[inline]
    fn op_ora(&mut self, x: u16) {
        self.gr[SSP_A] |= (x as u32) << 16;
        self.upd_acc_zn();
    }

    #[inline]
    fn op_ora32(&mut self, x: u32) {
        self.gr[SSP_A] |= x;
        self.upd_acc_zn();
    }

    #[inline]
    fn op_eora(&mut self, x: u16) {
        self.gr[SSP_A] ^= (x as u32) << 16;
        self.upd_acc_zn();
    }

    #[inline]
    fn op_eora32(&mut self, x: u32) {
        self.gr[SSP_A] ^= x;
        self.upd_acc_zn();
    }

    #[inline]
    fn alu16(&mut self, k: Alu, x: u16) {
        match k {
            Alu::Sub => self.op_suba(x),
            Alu::Cmp => self.op_cmpa(x),
            Alu::Add => self.op_adda(x),
            Alu::And => self.op_anda(x),
            Alu::Or => self.op_ora(x),
            Alu::Eor => self.op_eora(x),
        }
    }

    #[inline]
    fn alu32(&mut self, k: Alu, x: u32) {
        match k {
            Alu::Sub => self.op_suba32(x),
            Alu::Cmp => self.op_cmpa32(x),
            Alu::Add => self.op_adda32(x),
            Alu::And => self.op_anda32(x),
            Alu::Or => self.op_ora32(x),
            Alu::Eor => self.op_eora32(x),
        }
    }

    /// `OP a, s`: the register-sourced ALU form, with the `OP_CHECK32`
    /// escape: `A op P` and `A op A` operate on the full 32-bit values (and
    /// `A op P` first recomputes P).
    #[inline]
    fn alu_reg(&mut self, k: Alu, op: u16, host: &mut impl SspHost) {
        match op & 0x0f {
            v if v as usize == SSP_P => {
                self.read_p();
                let p = self.gr[SSP_P];
                self.alu32(k, p);
            }
            v if v as usize == SSP_A => {
                let a = self.gr[SSP_A];
                self.alu32(k, a);
            }
            other => {
                let x = self.read_reg(other, host);
                self.alu16(k, x);
            }
        }
    }

    /// `COND_CHECK`: only "always", `Z`-matches, and `N`-matches are used by
    /// SVP; every other condition is treated as false (as in the reference).
    #[inline]
    fn cond_check(&self, op: u16) -> bool {
        match op & 0xf0 {
            0x00 => true,
            0x50 => ((self.reg16(SSP_ST) as u32 ^ ((op as u32) << 5)) & FLAG_Z as u32) == 0,
            0x70 => ((self.reg16(SSP_ST) as u32 ^ ((op as u32) << 7)) & FLAG_N as u32) == 0,
            _ => false,
        }
    }

    /// The pointer-register index `IJind = ((op>>6)&4) | (op&3)`.
    #[inline]
    fn ij_index(op: u16) -> usize {
        (((op >> 6) & 4) | (op & 3)) as usize
    }

    /// Execute one already-fetched instruction word.
    pub(crate) fn execute(&mut self, op: u16, host: &mut impl SspHost) {
        match op >> 9 {
            // ld d, s
            0x00 => {
                if op == 0 {
                    return; // nop
                }
                if op == (((SSP_A as u16) << 4) | SSP_P as u16) {
                    // A <- P (32-bit)
                    self.read_p();
                    self.gr[SSP_A] = self.gr[SSP_P];
                } else {
                    let v = self.read_reg(op & 0x0f, host);
                    self.write_reg((op & 0xf0) >> 4, v, host);
                }
            }

            // ld d, (ri)
            0x01 => {
                let v = self.ptr1_read(op);
                self.write_reg((op & 0xf0) >> 4, v, host);
            }

            // ld (ri), s
            0x02 => {
                let v = self.read_reg((op & 0xf0) >> 4, host);
                self.ptr1_write(op, v);
            }

            // ldi d, imm
            0x04 => {
                let v = self.fetch();
                self.write_reg((op & 0xf0) >> 4, v, host);
            }

            // ld d, ((ri))
            0x05 => {
                let v = self.ptr2_read(op);
                self.write_reg((op & 0xf0) >> 4, v, host);
            }

            // ldi (ri), imm
            0x06 => {
                let v = self.fetch();
                self.ptr1_write(op, v);
            }

            // ld adr, a
            0x07 => {
                let a = self.reg16(SSP_A);
                self.ram[(op & 0x1ff) as usize] = a;
            }

            // ld d, ri
            0x09 => {
                let v = self.ptr[Self::ij_index(op)] as u16;
                self.write_reg((op & 0xf0) >> 4, v, host);
            }

            // ld ri, s
            0x0a => {
                let v = self.read_reg((op & 0xf0) >> 4, host);
                self.ptr[Self::ij_index(op)] = v as u8;
            }

            // ldi ri, simm
            0x0c..=0x0f => {
                self.ptr[((op >> 8) & 7) as usize] = op as u8;
            }

            // call cond, addr
            0x24 => {
                if self.cond_check(op) {
                    let new_pc = self.fetch();
                    let ret = self.pc;
                    self.write_stack(ret);
                    self.write_pc(new_pc);
                } else {
                    self.pc = self.pc.wrapping_add(1);
                }
            }

            // ld d, (a)
            0x25 => {
                let a = self.reg16(SSP_A);
                let v = self.iram_rom[a as usize];
                self.write_reg((op & 0xf0) >> 4, v, host);
            }

            // bra cond, addr
            0x26 => {
                if self.cond_check(op) {
                    let new_pc = self.fetch();
                    self.write_pc(new_pc);
                } else {
                    self.pc = self.pc.wrapping_add(1);
                }
            }

            // mod cond, op
            0x48 => {
                if self.cond_check(op) {
                    match op & 7 {
                        2 => self.gr[SSP_A] = ((self.gr[SSP_A] as i32) >> 1) as u32, // shr
                        3 => self.gr[SSP_A] = self.gr[SSP_A].wrapping_shl(1),        // shl
                        6 => self.gr[SSP_A] = (self.gr[SSP_A] as i32).wrapping_neg() as u32, // neg
                        7 if (self.gr[SSP_A] as i32) < 0 => {
                            self.gr[SSP_A] = (self.gr[SSP_A] as i32).wrapping_neg() as u32;
                        }
                        _ => {}
                    }
                    self.upd_acc_zn();
                }
            }

            // mpys (rj), (ri), b
            0x1b => {
                self.read_p();
                self.gr[SSP_A] = self.gr[SSP_A].wrapping_sub(self.gr[SSP_P]);
                self.upd_acc_zn();
                self.mac_load_operands(op);
            }

            // mpya (rj), (ri), b
            0x4b => {
                self.read_p();
                self.gr[SSP_A] = self.gr[SSP_A].wrapping_add(self.gr[SSP_P]);
                self.upd_acc_zn();
                self.mac_load_operands(op);
            }

            // mld (rj), (ri), b
            0x5b => {
                // The accumulator is cleared, so the flags describe a zero
                // result: L, V and N clear, Z set.
                self.gr[SSP_A] = 0;
                let st = (self.reg16(SSP_ST) & 0x0fff) | FLAG_Z;
                self.set_reg16(SSP_ST, st);
                self.mac_load_operands(op);
            }

            // OP a, s
            0x10 => self.alu_reg(Alu::Sub, op, host),
            0x30 => self.alu_reg(Alu::Cmp, op, host),
            0x40 => self.alu_reg(Alu::Add, op, host),
            0x50 => self.alu_reg(Alu::And, op, host),
            0x60 => self.alu_reg(Alu::Or, op, host),
            0x70 => self.alu_reg(Alu::Eor, op, host),

            // OP a, (ri)
            0x11 => {
                let v = self.ptr1_read(op);
                self.op_suba(v);
            }
            0x31 => {
                let v = self.ptr1_read(op);
                self.op_cmpa(v);
            }
            0x41 => {
                let v = self.ptr1_read(op);
                self.op_adda(v);
            }
            0x51 => {
                let v = self.ptr1_read(op);
                self.op_anda(v);
            }
            0x61 => {
                let v = self.ptr1_read(op);
                self.op_ora(v);
            }
            0x71 => {
                let v = self.ptr1_read(op);
                self.op_eora(v);
            }

            // OP a, adr  (0x03 = ld)
            0x03 => {
                let v = self.ram[(op & 0x1ff) as usize];
                self.op_lda(v);
            }
            0x13 => {
                let v = self.ram[(op & 0x1ff) as usize];
                self.op_suba(v);
            }
            0x33 => {
                let v = self.ram[(op & 0x1ff) as usize];
                self.op_cmpa(v);
            }
            0x43 => {
                let v = self.ram[(op & 0x1ff) as usize];
                self.op_adda(v);
            }
            0x53 => {
                let v = self.ram[(op & 0x1ff) as usize];
                self.op_anda(v);
            }
            0x63 => {
                let v = self.ram[(op & 0x1ff) as usize];
                self.op_ora(v);
            }
            0x73 => {
                let v = self.ram[(op & 0x1ff) as usize];
                self.op_eora(v);
            }

            // OP a, imm
            0x14 => {
                let v = self.fetch();
                self.op_suba(v);
            }
            0x34 => {
                let v = self.fetch();
                self.op_cmpa(v);
            }
            0x44 => {
                let v = self.fetch();
                self.op_adda(v);
            }
            0x54 => {
                let v = self.fetch();
                self.op_anda(v);
            }
            0x64 => {
                let v = self.fetch();
                self.op_ora(v);
            }
            0x74 => {
                let v = self.fetch();
                self.op_eora(v);
            }

            // OP a, ((ri))
            0x15 => {
                let v = self.ptr2_read(op);
                self.op_suba(v);
            }
            0x35 => {
                let v = self.ptr2_read(op);
                self.op_cmpa(v);
            }
            0x45 => {
                let v = self.ptr2_read(op);
                self.op_adda(v);
            }
            0x55 => {
                let v = self.ptr2_read(op);
                self.op_anda(v);
            }
            0x65 => {
                let v = self.ptr2_read(op);
                self.op_ora(v);
            }
            0x75 => {
                let v = self.ptr2_read(op);
                self.op_eora(v);
            }

            // OP a, ri
            0x19 => {
                let v = self.ptr[Self::ij_index(op)] as u16;
                self.op_suba(v);
            }
            0x39 => {
                let v = self.ptr[Self::ij_index(op)] as u16;
                self.op_cmpa(v);
            }
            0x49 => {
                let v = self.ptr[Self::ij_index(op)] as u16;
                self.op_adda(v);
            }
            0x59 => {
                let v = self.ptr[Self::ij_index(op)] as u16;
                self.op_anda(v);
            }
            0x69 => {
                let v = self.ptr[Self::ij_index(op)] as u16;
                self.op_ora(v);
            }
            0x79 => {
                let v = self.ptr[Self::ij_index(op)] as u16;
                self.op_eora(v);
            }

            // OP simm
            0x1c => self.op_suba(op & 0xff),
            0x3c => self.op_cmpa(op & 0xff),
            0x4c => self.op_adda(op & 0xff),
            0x5c => self.op_anda(op & 0xff),
            0x6c => self.op_ora(op & 0xff),
            0x7c => self.op_eora(op & 0xff),

            // Unhandled forms are no-ops (the reference logs and continues).
            _ => {}
        }
    }

    /// Load the MAC operands into X and Y (`mpys`/`mpya`/`mld` tail):
    /// `X = (ri)`, `Y = (rj)`.
    #[inline]
    fn mac_load_operands(&mut self, op: u16) {
        let x = self.ptr1_read_(op & 3, 0, (op << 1) & 0x18);
        let y = self.ptr1_read_((op >> 4) & 3, 4, (op >> 3) & 0x18);
        self.set_reg16(SSP_X, x);
        self.set_reg16(SSP_Y, y);
    }
}
