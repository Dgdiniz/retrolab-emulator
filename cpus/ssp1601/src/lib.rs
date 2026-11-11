//! Samsung SSP1601 DSP interpreter, the core of the Sega Virtua Processor
//! (SVP) cartridge (Virtua Racing).
//!
//! # Origin and license
//!
//! The interpreter is a translation into Rust of the SSP1601 interpreter in
//! PicoDrive (`pico/carthw/svp/ssp16.c` and `ssp16.h`), Copyright (c)
//! Gražvydas "notaz" Ignotas, 2008, which its author publishes under the
//! BSD 3-Clause license. The full notice is at the end of `LICENSE`. "The
//! reference" in this crate's comments is that file.
//!
//! The translation was made from the copy of the file that Genesis Plus GX
//! carries, the author's text of early 2008 with changes for big-endian
//! machines, and then checked function by function against the author's
//! own file. Where the two differ this crate follows the author's: the run
//! loop executes nothing while a wait flag is pending, and `mld` sets `Z`.
//! One difference from it is deliberate, the cost charged per instruction;
//! see [`Ssp1601::run`].
//!
//! # Shape
//!
//! It is a **pure** interpreter: it owns the DSP register file, the two
//! internal RAM banks, the six-level stack, the PMAC address/mode file, and
//! the IRAM+program-ROM word array, but it reaches the *shared* DRAM and
//! the cartridge ROM through an injected [`SspHost`], so the crate has no
//! dependency on the Genesis core (mirroring `cpus/mos6502` /
//! `cpus/wdc65816`).
//!
//! # Model notes (from the reference)
//!
//! - Every general register is 16-bit **except** the accumulator `A`, the
//!   product `P`, and `PMC`, which are 32-bit. The 16-bit registers live in
//!   the **high** half of a 32-bit slot (the union trick from `ssp16.c`):
//!   `reg16(i)` reads the high word, `A` low half is `AL`.
//! - `PC` is a **word** offset into `iram_rom`. IRAM is words `0x000..0x400`
//!   (writable), program ROM is words `0x400..0x10000` (copied from the
//!   cartridge at reset).
//! - The main loop dispatches on `op >> 9` (the top 7 bits), ~60 forms.
//! - Only the `Z` and `N` status flags are emulated; SVP code never checks
//!   `L`/`OV` (the reference makes the same simplification).

mod execute;
mod pointers;
mod registers;

#[cfg(test)]
mod tests;

/// Access to the memory the SSP1601 does **not** own: the DRAM it shares
/// with the 68000 (word-addressed, `0..0x10000`) and the cartridge ROM
/// (word-addressed, up to 20 bits). The SVP coprocessor implements this by
/// lending its DRAM buffer and ROM slice.
pub trait SspHost {
    /// Read a 16-bit word from shared DRAM. `word_addr` is a word index
    /// (`0..0x10000`), i.e. the byte address `>> 1`.
    fn read_dram(&self, word_addr: u16) -> u16;

    /// Write a 16-bit word to shared DRAM. This is the **DSP-side** write:
    /// it is a plain store with none of the 68000 `0x30FE0x` sync-word
    /// wake logic (that belongs to the 68000 bus path).
    fn write_dram(&mut self, word_addr: u16, value: u16);

    /// Read a 16-bit word from the cartridge ROM. `word_addr` can span the
    /// full 2 MiB (20-bit word index).
    fn read_cart_rom(&self, word_addr: u32) -> u16;
}

// -- Register indices (ssp16.h `enum`) -------------------------------------
pub const SSP_GR0: usize = 0;
pub const SSP_X: usize = 1;
pub const SSP_Y: usize = 2;
pub const SSP_A: usize = 3;
pub const SSP_ST: usize = 4;
pub const SSP_STACK: usize = 5;
pub const SSP_PC: usize = 6;
pub const SSP_P: usize = 7;
pub const SSP_PM0: usize = 8;
pub const SSP_PM1: usize = 9;
pub const SSP_PM2: usize = 10;
pub const SSP_XST: usize = 11;
pub const SSP_PM4: usize = 12;
pub const SSP_PMC: usize = 14;
pub const SSP_AL: usize = 15;

// -- Status flags (only Z and N are actually consumed by SVP) --------------
const FLAG_L: u16 = 1 << 0xc;
const FLAG_Z: u16 = 1 << 0xd;
const FLAG_V: u16 = 1 << 0xe;
const FLAG_N: u16 = 1 << 0xf;

// -- `emu_status` bits (ssp16.h) -------------------------------------------
/// PMAC address word written, waiting for the mode word.
pub const SSP_PMC_HAVE_ADDR: u32 = 0x0001;
/// PMAC fully set (address + mode); next blind PM access programs it.
pub const SSP_PMC_SET: u32 = 0x0002;
/// The 68000 hangs the SVP (during DMA). Not modelled; reserved.
pub const SSP_HANG: u32 = 0x1000;
/// SSP is tight-looping on `PM0` bit 1 (waiting for a 68000 command).
pub const SSP_WAIT_PM0: u32 = 0x2000;
/// SSP is waiting on `dram[0x30FE06]` becoming non-zero.
pub const SSP_WAIT_30FE06: u32 = 0x4000;
/// SSP is waiting on `dram[0x30FE08]` becoming non-zero.
pub const SSP_WAIT_30FE08: u32 = 0x8000;
/// Any wait flag set -> `run` yields early.
pub const SSP_WAIT_MASK: u32 = 0xf000;

/// Words in the combined IRAM (`0x000..0x400`) + program ROM
/// (`0x400..0x10000`) array. `0x10000` words = the reference `iram_rom`'s
/// `0x20000` bytes.
pub const IRAM_ROM_WORDS: usize = 0x1_0000;
/// IRAM data region size in words (`0x000..0x400`); the rest is program ROM.
pub const IRAM_WORDS: usize = 0x400;
/// Internal RAM: two banks of 256 words.
pub const RAM_WORDS: usize = 256 * 2;

/// The SSP1601 DSP core.
pub struct Ssp1601 {
    /// General registers. 16-bit values live in the high half; `A`/`P`/`PMC`
    /// use the full 32 bits (`gr[SSP_A]` = `AL` in low half, `A` in high).
    pub gr: [u32; 16],
    /// Two internal RAM banks (`RAM0` = `[0..256]`, `RAM1` = `[256..512]`).
    pub ram: [u16; RAM_WORDS],
    /// Eight 8-bit pointer registers (`r0..r3` -> RAM0, `r4..r7` -> RAM1).
    pub ptr: [u8; 8],
    /// Six-level hardware stack.
    pub stack: [u16; 6],
    /// PMAC address/mode file: `pmac[0]` = read, `pmac[1]` = write, indexed
    /// by PM register (`PM0..PM5`).
    pub pmac: [[u32; 6]; 2],
    /// PMC programming state machine + wait-loop flags (`SSP_*`).
    pub emu_status: u32,
    /// IRAM (`0x000..0x400`) + program ROM (`0x400..`), as words.
    pub iram_rom: Vec<u16>,
    /// Live program counter (word index), valid only during `run`; synced
    /// to `gr[SSP_PC]` at the run boundaries.
    pc: u16,
    /// Remaining cycle budget during `run`.
    cycles: i32,
    /// Total instructions retired (diagnostic / debugger).
    pub instructions: u64,
}

impl Default for Ssp1601 {
    fn default() -> Self {
        Self::new()
    }
}

impl Ssp1601 {
    /// A fresh, un-reset DSP: everything zeroed, program ROM empty. Call
    /// [`load_program`](Self::load_program) then [`reset`](Self::reset)
    /// before running.
    pub fn new() -> Self {
        Self {
            gr: [0; 16],
            ram: [0; RAM_WORDS],
            ptr: [0; 8],
            stack: [0; 6],
            pmac: [[0; 6]; 2],
            emu_status: 0,
            iram_rom: vec![0u16; IRAM_ROM_WORDS],
            pc: 0,
            cycles: 0,
            instructions: 0,
        }
    }

    /// Copy the program ROM out of the cartridge (the reference's
    /// `PicoSVPReset`, in `svp.c`, copies bytes `0x800..0x20000`): ROM
    /// bytes `0x800..0x20000` become program words `0x400..0x10000`
    /// (big-endian). IRAM (words `0x000..0x400`) is left untouched.
    pub fn load_program(&mut self, rom: &[u8]) {
        for word in IRAM_WORDS..IRAM_ROM_WORDS {
            let byte = word * 2;
            let hi = rom.get(byte).copied().unwrap_or(0);
            let lo = rom.get(byte + 1).copied().unwrap_or(0);
            self.iram_rom[word] = (u16::from(hi) << 8) | u16::from(lo);
        }
    }

    /// Hardware reset (`ssp1601_reset`): `GR0 = 0xFFFF`, `PC = 0x400`,
    /// `STACK = 0`, `ST = 0`, `emu_status = 0`. Does **not** touch IRAM,
    /// internal RAM, or the program ROM (call `load_program` for that).
    pub fn reset(&mut self) {
        self.emu_status = 0;
        self.gr[SSP_GR0] = 0xFFFF_0000;
        self.set_reg16(SSP_PC, 0x400);
        self.set_reg16(SSP_STACK, 0);
        self.set_reg16(SSP_ST, 0);
        self.pc = 0x400;
    }

    /// Full cold reset: zero the internal RAM / pointers / stack / PMAC /
    /// registers, reload the program ROM from `rom`, then apply the
    /// `ssp1601_reset` register state.
    pub fn cold_reset(&mut self, rom: &[u8]) {
        self.gr = [0; 16];
        self.ram = [0; RAM_WORDS];
        self.ptr = [0; 8];
        self.stack = [0; 6];
        self.pmac = [[0; 6]; 2];
        self.instructions = 0;
        self.load_program(rom);
        self.reset();
    }

    // -- Register accessors (the ssp16.c union) ---------------------------

    /// The 16-bit value of register `i` (its high half).
    #[inline]
    pub fn reg16(&self, i: usize) -> u16 {
        (self.gr[i] >> 16) as u16
    }

    /// Set the 16-bit (high-half) value of register `i`.
    #[inline]
    pub fn set_reg16(&mut self, i: usize, v: u16) {
        self.gr[i] = (self.gr[i] & 0x0000_FFFF) | ((v as u32) << 16);
    }

    /// The low 16 bits of register `i` (only meaningful for `A`/`PMC`).
    #[inline]
    fn reg_lo(&self, i: usize) -> u16 {
        self.gr[i] as u16
    }

    /// Set the low 16 bits of register `i`.
    #[inline]
    fn set_reg_lo(&mut self, i: usize, v: u16) {
        self.gr[i] = (self.gr[i] & 0xFFFF_0000) | (v as u32);
    }

    /// The program counter (word offset), valid between runs.
    #[inline]
    pub fn program_counter(&self) -> u16 {
        self.reg16(SSP_PC)
    }

    // -- Flag helpers -----------------------------------------------------

    /// `UPD_ACC_ZN`: recompute Z/N from the 32-bit accumulator (used by the
    /// logic ops and the `mod` shifts).
    #[inline]
    fn upd_acc_zn(&mut self) {
        let mut st = self.reg16(SSP_ST) & !(FLAG_Z | FLAG_N);
        if self.gr[SSP_A] == 0 {
            st |= FLAG_Z;
        } else {
            st |= ((self.gr[SSP_A] >> 16) as u16) & FLAG_N;
        }
        self.set_reg16(SSP_ST, st);
    }

    /// `UPD_LZVN`: like `upd_acc_zn` but also clears L/V (the arithmetic
    /// ops). L/V are never read by SVP, so they are only cleared.
    #[inline]
    fn upd_lzvn(&mut self) {
        let mut st = self.reg16(SSP_ST) & !(FLAG_L | FLAG_Z | FLAG_V | FLAG_N);
        if self.gr[SSP_A] == 0 {
            st |= FLAG_Z;
        } else {
            st |= ((self.gr[SSP_A] >> 16) as u16) & FLAG_N;
        }
        self.set_reg16(SSP_ST, st);
    }

    /// Set Z/N (clearing L/V) from an arbitrary 32-bit result `t` without
    /// modifying the accumulator, the `cmp` path.
    #[inline]
    fn set_flags_from(&mut self, t: u32) {
        let mut st = self.reg16(SSP_ST) & !(FLAG_L | FLAG_Z | FLAG_V | FLAG_N);
        if t == 0 {
            st |= FLAG_Z;
        } else {
            st |= ((t >> 16) as u16) & FLAG_N;
        }
        self.set_reg16(SSP_ST, st);
    }

    // -- Program fetch / PC -----------------------------------------------

    /// Fetch the next program word and advance PC (`op = *PC++`).
    #[inline]
    fn fetch(&mut self) -> u16 {
        let op = self.iram_rom[self.pc as usize];
        self.pc = self.pc.wrapping_add(1);
        op
    }

    /// The last word fetched (`*(PC-1)` in the reference), used by the PMC
    /// "blind access" checks.
    #[inline]
    fn prev_word(&self) -> u16 {
        self.iram_rom[self.pc.wrapping_sub(1) as usize]
    }

    /// `GET_PPC_OFFS()`: byte offset of the instruction currently executing
    /// (`(PC-1) * 2`), used by the wait-loop detectors.
    #[inline]
    fn ppc_offs(&self) -> u32 {
        (self.pc.wrapping_sub(1) as u32) << 1
    }

    /// Multiply update (`read_P`): `P = sign(X) * sign(Y) * 2`; returns the
    /// high word of P.
    #[inline]
    fn read_p(&mut self) -> u16 {
        let m1 = (self.reg16(SSP_X) as i16) as i32;
        let m2 = (self.reg16(SSP_Y) as i16) as i32;
        self.gr[SSP_P] = m1.wrapping_mul(m2).wrapping_mul(2) as u32;
        (self.gr[SSP_P] >> 16) as u16
    }

    /// Run up to `cycles` DSP cycles, yielding as soon as a wait flag is set.
    /// Nothing runs while one is pending: the 68000 side clears it. Returns
    /// the number of instructions retired this call.
    ///
    /// Every instruction costs one cycle, and a taken `call`/`bra` or a load
    /// into `PC` one more, so the budget is close to a count of
    /// instructions. The reference charges further cycles, one for an
    /// immediate word, two for a read through `((ri))` or `(a)`, and the
    /// second cycle of a `call`/`bra` whether or not it is taken, against a
    /// larger budget per scanline. Both are estimates of the chip's speed;
    /// the budget that goes with this accounting is set by the caller.
    pub fn run(&mut self, cycles: i32, host: &mut impl SspHost) -> u64 {
        // SET_PC(rPC): load the live PC from the register file.
        self.pc = self.reg16(SSP_PC);
        self.cycles = cycles;
        let start = self.instructions;

        while self.cycles > 0 && (self.emu_status & SSP_WAIT_MASK) == 0 {
            let op = self.fetch();
            self.execute(op, host);
            self.instructions += 1;
            self.cycles -= 1;
        }

        // rPC = GET_PC() writes the live PC back; read_P() updates P.
        self.set_reg16(SSP_PC, self.pc);
        self.read_p();
        self.instructions - start
    }
}
