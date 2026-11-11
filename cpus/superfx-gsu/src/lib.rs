//! The Super FX's **GSU** (Graphics Support Unit): the 16-bit RISC
//! processor inside the Super FX and MARIO Chip cartridges (Star Fox, Doom,
//! Yoshi's Island).
//!
//! Sixteen general registers, R15 the program counter; an instruction
//! cache of 512 bytes; the ALT1/ALT2 prefixes, which give half the opcodes
//! a second meaning; a one-byte pipeline, so the instruction after a jump
//! runs before the jump lands; a ROM buffer and a RAM buffer that finish
//! their transfers some clocks after they start; and a plot engine that
//! writes pixels into Game Pak RAM in the SNES's character format.
//! [`Gsu::step`] runs one instruction against an [`Addressable`] the host
//! supplies, Game Pak ROM in banks `$00-$5F`, Game Pak RAM at
//! `$70-$71`. The host (`snes1-core`'s `SuperFxChip`) owns the rest: the
//! S-CPU's windows onto ROM and RAM, the arbitration between the two
//! processors, and the timing against the console.
//!
//! The execution follows ares (`component/processor/gsu`,
//! `sfc/coprocessor/superfx`), whose GSU is the same code as bsnes-plus's
//! and is published under the ISC license, its notice is at the end of
//! `LICENSE`: the instruction bodies, the cache, the two
//! buffers and the plot engine keep its method, in this tree's names and
//! structure. The decoder, the disassembler and the register ports are
//! this tree's own.

pub mod decode;
pub mod disasm;
mod instructions;
mod memory;
mod ports;
pub mod registers;

use registers::{screen_mode, status};
use retro_core::cpu::{Addressable, Clockable, Resettable};

/// The GSU: its registers, its caches and the state of an instruction in
/// progress.
///
/// The packed registers (SFR, SCMR, POR, CFGR) are kept whole; the
/// modules of [`registers`] name their bits.
#[derive(Clone)]
pub struct Gsu {
    /// R0-R15. R15 is the program counter and R14 the address the ROM
    /// buffer reads; LOOP counts R12 down and jumps to R13; LINK leaves
    /// the return address in R11; PLOT draws at (R1, R2); LMULT takes R6
    /// and leaves the product's low half in R4.
    pub registers: [u16; 16],
    /// SFR, the status register, see [`registers::status`].
    pub status: u16,
    /// PBR: the bank code is fetched from (7 bits).
    pub program_bank: u8,
    /// ROMBR: the bank the ROM buffer reads (7 bits).
    pub rom_bank: u8,
    /// RAMBR: which of the two 64 KiB banks of Game Pak RAM the loads and
    /// stores reach.
    pub ram_bank: bool,
    /// CBR: the code address the instruction cache starts at, a multiple
    /// of 16.
    pub cache_base: u16,
    /// SCBR: where the plot surface starts in Game Pak RAM, in units of
    /// 1 KiB.
    pub screen_base: u8,
    /// SCMR: the bus ownership, the colour depth and the surface height,
    /// see [`registers::screen_mode`].
    pub screen_mode: u8,
    /// COLR: the colour PLOT draws.
    pub color: u8,
    /// POR: how PLOT, COLOR and GETC treat a colour, see
    /// [`registers::plot_options`].
    pub plot_options: u8,
    /// BRAMR: the backup RAM may be written.
    pub backup_ram_enabled: bool,
    /// VCR: the chip's version, which the S-CPU reads to tell the chips
    /// apart.
    pub version: u8,
    /// CFGR: the interrupt mask and the multiplier's speed, see
    /// [`registers::config`].
    pub config: u8,
    /// CLSR, kept as the clocks one cache read takes: 2 at 10.7 MHz, 1 at
    /// 21.4 MHz.
    pub clock_divider: u8,
    /// The pipeline: the byte already fetched for the next instruction.
    pub next_opcode: u8,
    /// The address of the last load or store, which SBK writes back to.
    pub last_ram_address: u16,
    /// The register the next instruction reads (FROM, WITH).
    pub source: u8,
    /// The register the next instruction writes (TO, WITH).
    pub destination: u8,
    /// Clocks until the ROM buffer's read lands.
    pub rom_read_delay: u8,
    /// The ROM buffer: the byte at ROMBR:R14, once the read has landed.
    pub rom_data: u8,
    /// The address of the RAM buffer's pending write.
    pub ram_write_address: u16,
    /// Clocks until the RAM buffer's write lands.
    pub ram_write_delay: u8,
    /// The byte the RAM buffer is about to write.
    pub ram_write_data: u8,
    /// The instruction cache.
    pub cache: InstructionCache,
    /// The plot engine's two lines of pixels: the one being drawn and the
    /// one waiting to be written.
    pub pixel_cache: [PixelCache; 2],
    /// Clocks run since reset, for the debugger.
    pub cycles: u64,
    /// The instruction wrote R15, a jump, so the program counter does
    /// not move on after it.
    pub jumped: bool,
    /// The clocks the instruction in progress has taken; [`Gsu::step`]
    /// returns them.
    clocks: u32,
}

/// The instruction cache: 512 bytes of code from CBR on, in 32 lines of
/// 16, each loaded the first time it is needed.
#[derive(Clone)]
pub struct InstructionCache {
    /// The cached code; byte `i` is the code at CBR + `i`.
    pub bytes: [u8; 512],
    /// Whether each line holds its code yet.
    pub valid: [bool; 32],
}

impl Default for InstructionCache {
    fn default() -> Self {
        Self {
            bytes: [0; 512],
            valid: [false; 32],
        }
    }
}

/// One line of the plot engine's pixel cache: eight horizontally adjacent
/// pixels of one character row, written to Game Pak RAM together.
#[derive(Clone, Copy, Default)]
pub struct PixelCache {
    /// Which eight pixels these are: `(y << 5) + (x >> 3)`.
    pub group: u16,
    /// The pixels drawn so far, one bit each; bit 7 is the leftmost.
    pub pending: u8,
    /// Each pixel's colour, indexed by its bit in `pending`.
    pub colors: [u8; 8],
}

impl Default for Gsu {
    fn default() -> Self {
        Self::new()
    }
}

impl Gsu {
    /// The GSU at power-on: every register clear but VCR, which reads
    /// `$04`, and CLSR, at 10.7 MHz; the pipeline holds a NOP and the
    /// cache is empty.
    pub fn new() -> Self {
        Self {
            registers: [0; 16],
            status: 0,
            program_bank: 0,
            rom_bank: 0,
            ram_bank: false,
            cache_base: 0,
            screen_base: 0,
            screen_mode: 0,
            color: 0,
            plot_options: 0,
            backup_ram_enabled: false,
            version: 0x04,
            config: 0,
            clock_divider: 2,
            next_opcode: 0x01,
            last_ram_address: 0,
            source: 0,
            destination: 0,
            rom_read_delay: 0,
            rom_data: 0,
            ram_write_address: 0,
            ram_write_delay: 0,
            ram_write_data: 0,
            cache: InstructionCache::default(),
            pixel_cache: [PixelCache::default(); 2],
            cycles: 0,
            jumped: false,
            clocks: 0,
        }
    }

    /// The program counter, R15.
    #[inline]
    pub fn pc(&self) -> u16 {
        self.registers[15]
    }

    /// Set the program counter, R15.
    #[inline]
    pub fn set_pc(&mut self, value: u16) {
        self.registers[15] = value;
    }

    /// SFR.G: the GSU is running.
    #[inline]
    pub fn is_running(&self) -> bool {
        self.flag(status::GO)
    }

    /// SFR.ALT1 is set for the next opcode.
    #[inline]
    pub fn alt1(&self) -> bool {
        self.flag(status::ALT1)
    }

    /// SFR.ALT2 is set for the next opcode.
    #[inline]
    pub fn alt2(&self) -> bool {
        self.flag(status::ALT2)
    }

    /// SFR.IRQ: STOP raised the interrupt and the S-CPU has not read it
    /// away.
    #[inline]
    pub fn irq_pending(&self) -> bool {
        self.flag(status::IRQ)
    }

    /// SCMR.RON: the GSU has the Game Pak ROM bus.
    #[inline]
    pub fn owns_rom_bus(&self) -> bool {
        self.screen_mode & screen_mode::ROM_BUS != 0
    }

    /// SCMR.RAN: the GSU has the Game Pak RAM bus.
    #[inline]
    pub fn owns_ram_bus(&self) -> bool {
        self.screen_mode & screen_mode::RAM_BUS != 0
    }

    /// A read through the ROM buffer or a write through the RAM buffer has
    /// not landed yet.
    #[inline]
    pub fn has_transfer_in_flight(&self) -> bool {
        self.rom_read_delay != 0 || self.ram_write_delay != 0
    }

    /// Disassemble the instruction at `addr` as the GSU would read it now,
    /// with its current ALT prefixes.
    pub fn disassemble(&self, bus: &mut dyn Addressable, addr: u32) -> disasm::DisasmResult {
        disasm::disassemble_one(bus, addr, self.alt1(), self.alt2())
    }

    /// Run one instruction and return the clocks it took. A stopped GSU
    /// spends six clocks, so the buffers still drain.
    pub fn step(&mut self, bus: &mut dyn Addressable) -> u32 {
        self.clocks = 0;
        if !self.is_running() {
            self.spend(bus, 6);
            return self.clocks;
        }
        let opcode = self.take_opcode(bus);
        self.execute(bus, opcode);
        if !self.jumped {
            self.registers[15] = self.registers[15].wrapping_add(1);
        }
        self.clocks
    }

    /// One bit of SFR.
    #[inline]
    pub(crate) fn flag(&self, bit: u16) -> bool {
        self.status & bit != 0
    }

    /// Set or clear one bit of SFR.
    #[inline]
    pub(crate) fn set_flag(&mut self, bit: u16, on: bool) {
        if on {
            self.status |= bit;
        } else {
            self.status &= !bit;
        }
    }

    /// Write a register as an instruction does: writing R14 starts a read
    /// through the ROM buffer, and writing R15 is a jump.
    #[inline]
    pub(crate) fn write_register(&mut self, index: usize, value: u16) {
        self.registers[index] = value;
        match index {
            14 => self.start_rom_read(),
            15 => self.jumped = true,
            _ => {}
        }
    }

    /// The register FROM or WITH chose (R0 by default).
    #[inline]
    pub(crate) fn source_value(&self) -> u16 {
        self.registers[usize::from(self.source)]
    }

    /// Write the register TO or WITH chose (R0 by default).
    #[inline]
    pub(crate) fn write_destination(&mut self, value: u16) {
        self.write_register(usize::from(self.destination), value);
    }

    /// The second operand of the arithmetic families: the register the
    /// opcode's low nibble names, or under ALT2 the nibble itself.
    #[inline]
    pub(crate) fn register_or_immediate(&self, n: usize) -> u16 {
        if self.alt2() {
            n as u16
        } else {
            self.registers[n]
        }
    }

    /// End an instruction: the ALT prefixes and WITH apply to one opcode
    /// only, and the source and destination go back to R0.
    #[inline]
    pub(crate) fn clear_prefixes(&mut self) {
        self.status &= !(status::WITH | status::ALT1 | status::ALT2);
        self.source = 0;
        self.destination = 0;
    }

    /// S from bit 15 and Z, for a 16-bit result.
    #[inline]
    pub(crate) fn set_sign_and_zero(&mut self, value: u16) {
        self.set_flag(status::SIGN, value & 0x8000 != 0);
        self.set_flag(status::ZERO, value == 0);
    }

    /// S from bit 7 and Z, for a result that is one byte.
    #[inline]
    pub(crate) fn set_byte_sign_and_zero(&mut self, value: u16) {
        self.set_flag(status::SIGN, value & 0x80 != 0);
        self.set_flag(status::ZERO, value == 0);
    }
}

impl Clockable for Gsu {
    /// The GSU runs through [`Gsu::step`], one instruction at a time, which
    /// is how the host drives it; this only gives it the trait's shape.
    fn clock(&mut self, _cycles: u32) -> u32 {
        0
    }
}

impl Resettable for Gsu {
    /// Back to the power-on state of [`Gsu::new`].
    fn reset(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests;
