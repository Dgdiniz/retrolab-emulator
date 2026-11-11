//! Register-file I/O: the `REG_READ`/`REG_WRITE` dispatch (indices 5-15 go
//! through handlers) and the PMAC "programmable memory" engine (`pm_io`)
//! that routes PM0-PM4 reads/writes to cartridge ROM, shared DRAM, or IRAM.
//!
//! Faithful port of `ssp16.c`'s handler table + `pm_io`.

use crate::{
    Ssp1601, SspHost, SSP_A, SSP_PM0, SSP_PMC, SSP_PMC_HAVE_ADDR, SSP_PMC_SET, SSP_ST, SSP_STACK,
    SSP_WAIT_30FE06, SSP_WAIT_30FE08, SSP_WAIT_PM0, SSP_XST,
};

/// `get_inc(mode)`: the PMAC auto-increment step (`0 1 2 4 8 16 32 128`,
/// negated for decrement mode). Returned as a signed word delta.
fn get_inc(mode: u32) -> i32 {
    let mut inc = ((mode >> 11) & 7) as i32;
    if inc != 0 {
        if inc != 7 {
            inc -= 1;
        }
        inc = 1 << inc;
        if mode & 0x8000 != 0 {
            inc = -inc;
        }
    }
    inc
}

/// `overwite_write`: per-nibble masked store, only nibbles that are
/// non-zero in `d` overwrite the corresponding nibble of `dst`.
fn overwrite_nibbles(mut dst: u16, d: u16) -> u16 {
    for shift in [12, 8, 4, 0] {
        let nib = d & (0xf << shift);
        if nib != 0 {
            dst = (dst & !(0xf << shift)) | nib;
        }
    }
    dst
}

impl Ssp1601 {
    /// `REG_READ`: registers 0-4 read their high word directly, 5-15 go
    /// through handlers.
    pub(crate) fn read_reg(&mut self, r: u16, host: &mut impl SspHost) -> u16 {
        match r & 0x0f {
            0..=4 => self.reg16((r & 0x0f) as usize),
            5 => self.read_stack(),
            6 => self.pc, // read_PC -> GET_PC()
            7 => self.read_p(),
            8 => self.read_pm0(host),
            9 => self.read_pm1(host),
            10 => self.read_pm2(host),
            11 => self.read_xst(host),
            12 => self.read_pm4(host),
            13 => 0, // read_unknown (gr13)
            14 => self.read_pmc(),
            _ => self.read_al(), // 15
        }
    }

    /// `REG_WRITE`: registers >= 4 go through handlers; 1-3 write their high
    /// word; register 0 (the `0xFFFF` constant) ignores writes.
    pub(crate) fn write_reg(&mut self, r: u16, d: u16, host: &mut impl SspHost) {
        let r = (r & 0x0f) as usize;
        if r >= 4 {
            match r {
                4 => self.set_reg16(SSP_ST, d), // write_ST
                5 => self.write_stack(d),
                6 => self.write_pc(d),
                7 => {} // write_unknown (P is not directly writable)
                8 => self.write_pm0(d, host),
                9 => self.write_pm1(d, host),
                10 => self.write_pm2(d, host),
                11 => self.write_xst(d, host),
                12 => self.write_pm4(d, host),
                13 => {} // write_unknown (gr13)
                14 => self.write_pmc(d),
                _ => self.write_al(d), // 15
            }
        } else if r > 0 {
            self.set_reg16(r, d);
        }
    }

    // -- STACK / PC -------------------------------------------------------

    fn read_stack(&mut self) -> u16 {
        // `--rSTACK`, underflow-wrap to 5, then return `stack[rSTACK]`. `s` is
        // always in `0..=5` here, so it indexes the six-level stack directly.
        let mut s = self.reg16(SSP_STACK).wrapping_sub(1);
        if (s as i16) < 0 {
            s = 5;
        }
        self.set_reg16(SSP_STACK, s);
        self.stack[(s as usize).min(5)]
    }

    pub(crate) fn write_stack(&mut self, d: u16) {
        let mut s = self.reg16(SSP_STACK);
        if s >= 6 {
            s = 0;
        }
        self.stack[s as usize] = d;
        self.set_reg16(SSP_STACK, s + 1);
    }

    pub(crate) fn write_pc(&mut self, d: u16) {
        self.pc = d;
        self.cycles -= 1;
    }

    // -- PMAC engine ------------------------------------------------------

    /// `pm_io`: the programmable-memory access. Returns `None` when this is
    /// *not* a PM access (the reference's `(u32)-1` sentinel) so the caller
    /// falls back to the raw PM register; `Some(v)` when it handled the
    /// access (`v` is the read value, or `0` for a write / PMC-programming).
    fn pm_io(&mut self, reg: usize, write: bool, d: u16, host: &mut impl SspHost) -> Option<u16> {
        if self.emu_status & SSP_PMC_SET != 0 {
            // Must be a blind r/w to program the PMAC; a non-blind access
            // (both dest and src nibbles set) aborts the programming.
            let prev = self.prev_word();
            if (prev & 0xff0f) != 0 && (prev & 0xfff0) != 0 {
                self.emu_status &= !SSP_PMC_SET;
                return Some(0);
            }
            self.pmac[write as usize][reg] = self.gr[SSP_PMC];
            self.emu_status &= !SSP_PMC_SET;
            return Some(0);
        }

        if self.emu_status & SSP_PMC_HAVE_ADDR != 0 {
            self.emu_status &= !SSP_PMC_HAVE_ADDR;
        }

        if reg == 4 || (self.reg16(SSP_ST) & 0x60) != 0 {
            let w = write as usize;
            let entry = self.pmac[w][reg];
            let addr = entry & 0xffff;
            let mode = entry >> 16;
            let mut out = d;

            if write {
                if (mode & 0x43ff) == 0x0018 {
                    // DRAM
                    let inc = get_inc(mode);
                    self.dram_pm_write(host, addr as u16, d, mode);
                    self.pmac[w][reg] = entry.wrapping_add(inc as u32);
                } else if (mode & 0xfbff) == 0x4018 {
                    // DRAM, cell increment
                    self.dram_pm_write(host, addr as u16, d, mode);
                    let step = if addr & 1 != 0 { 31 } else { 1 };
                    self.pmac[w][reg] = entry.wrapping_add(step);
                } else if (mode & 0x47ff) == 0x001c {
                    // IRAM
                    let inc = get_inc(mode);
                    self.iram_rom[(addr & 0x3ff) as usize] = d;
                    self.pmac[w][reg] = entry.wrapping_add(inc as u32);
                }
            } else if (mode & 0xfff0) == 0x0800 {
                // ROM, increment 1
                self.pmac[w][reg] = entry.wrapping_add(1);
                out = host.read_cart_rom(addr | ((mode & 0xf) << 16));
            } else if (mode & 0x47ff) == 0x0018 {
                // DRAM
                let inc = get_inc(mode);
                out = host.read_dram(addr as u16);
                self.pmac[w][reg] = entry.wrapping_add(inc as u32);
            } else {
                out = 0;
            }

            // PMC reflects the last PMAC entry accessed.
            self.gr[SSP_PMC] = self.pmac[w][reg];
            return Some(out);
        }

        None
    }

    /// PM DRAM write with the optional per-nibble overwrite mode (`mode` bit
    /// 0x0400).
    fn dram_pm_write(&mut self, host: &mut impl SspHost, addr: u16, d: u16, mode: u32) {
        if mode & 0x0400 != 0 {
            let cur = host.read_dram(addr);
            host.write_dram(addr, overwrite_nibbles(cur, d));
        } else {
            host.write_dram(addr, d);
        }
    }

    // -- PM0-PM4 / XST register handlers ----------------------------------

    fn read_pm0(&mut self, host: &mut impl SspHost) -> u16 {
        if let Some(d) = self.pm_io(0, false, 0, host) {
            return d;
        }
        let d = self.reg16(SSP_PM0);
        if (d & 2) == 0 && (self.ppc_offs() == 0x800 || self.ppc_offs() == 0x1851E) {
            self.emu_status |= SSP_WAIT_PM0;
        }
        let cleared = self.reg16(SSP_PM0) & !2;
        self.set_reg16(SSP_PM0, cleared);
        d
    }

    fn write_pm0(&mut self, d: u16, host: &mut impl SspHost) {
        if self.pm_io(0, true, d, host).is_some() {
            return;
        }
        self.set_reg16(SSP_PM0, d);
    }

    fn read_pm1(&mut self, host: &mut impl SspHost) -> u16 {
        self.pm_io(1, false, 0, host)
            .unwrap_or_else(|| self.reg16(crate::SSP_PM1))
    }

    fn write_pm1(&mut self, d: u16, host: &mut impl SspHost) {
        if self.pm_io(1, true, d, host).is_none() {
            self.set_reg16(crate::SSP_PM1, d);
        }
    }

    fn read_pm2(&mut self, host: &mut impl SspHost) -> u16 {
        self.pm_io(2, false, 0, host)
            .unwrap_or_else(|| self.reg16(crate::SSP_PM2))
    }

    fn write_pm2(&mut self, d: u16, host: &mut impl SspHost) {
        if self.pm_io(2, true, d, host).is_none() {
            self.set_reg16(crate::SSP_PM2, d);
        }
    }

    fn read_xst(&mut self, host: &mut impl SspHost) -> u16 {
        self.pm_io(3, false, 0, host)
            .unwrap_or_else(|| self.reg16(SSP_XST))
    }

    fn write_xst(&mut self, d: u16, host: &mut impl SspHost) {
        if self.pm_io(3, true, d, host).is_some() {
            return;
        }
        // The SSP writing XST raises PM0 bit 0 ("SSP wrote"), which the
        // 68000 clears when it reads 0xA15004.
        let pm0 = self.reg16(SSP_PM0) | 1;
        self.set_reg16(SSP_PM0, pm0);
        self.set_reg16(SSP_XST, d);
    }

    fn read_pm4(&mut self, host: &mut impl SspHost) -> u16 {
        match self.pm_io(4, false, 0, host) {
            Some(d) => {
                // Tight-loop detection on the sync words `[30fe08]`/`[30fe06]`.
                if d == 0 {
                    match self.ppc_offs() {
                        0x0854 => self.emu_status |= SSP_WAIT_30FE08,
                        0x4f12 => self.emu_status |= SSP_WAIT_30FE06,
                        _ => {}
                    }
                }
                d
            }
            None => self.reg16(crate::SSP_PM4),
        }
    }

    fn write_pm4(&mut self, d: u16, host: &mut impl SspHost) {
        if self.pm_io(4, true, d, host).is_none() {
            self.set_reg16(crate::SSP_PM4, d);
        }
    }

    // -- PMC / AL ---------------------------------------------------------

    fn read_pmc(&mut self) -> u16 {
        if self.emu_status & SSP_PMC_HAVE_ADDR != 0 {
            self.emu_status |= SSP_PMC_SET;
            self.emu_status &= !SSP_PMC_HAVE_ADDR;
            let l = self.reg_lo(SSP_PMC);
            ((l << 4) & 0xfff0) | ((l >> 4) & 0xf)
        } else {
            self.emu_status |= SSP_PMC_HAVE_ADDR;
            self.reg_lo(SSP_PMC)
        }
    }

    fn write_pmc(&mut self, d: u16) {
        if self.emu_status & SSP_PMC_HAVE_ADDR != 0 {
            self.emu_status |= SSP_PMC_SET;
            self.emu_status &= !SSP_PMC_HAVE_ADDR;
            self.set_reg16(SSP_PMC, d); // byte.h <- mode word
        } else {
            self.emu_status |= SSP_PMC_HAVE_ADDR;
            self.set_reg_lo(SSP_PMC, d); // byte.l <- address word
        }
    }

    fn read_al(&mut self) -> u16 {
        // `ld -, AL` (opcode 0x000f) is a dummy PM assign that resets the
        // PMC programming state.
        if self.prev_word() == 0x000f {
            self.emu_status &= !(SSP_PMC_SET | SSP_PMC_HAVE_ADDR);
        }
        self.reg_lo(SSP_A)
    }

    fn write_al(&mut self, d: u16) {
        self.set_reg_lo(SSP_A, d);
    }
}
