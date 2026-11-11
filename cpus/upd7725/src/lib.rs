//! The NEC uPD7725, the signal processor inside the DSP-1 cartridges (and
//! the DSP-2, DSP-3 and DSP-4).
//!
//! A 16-bit fixed-point DSP that NEC sold with its program in mask ROM:
//! 2048 instructions of 24 bits, 1024 words of data ROM, 256 words of
//! data RAM, a multiplier that forms a 16-by-16 product on every
//! instruction, two accumulators with a set of flags each, and a return
//! stack of 16 entries. Nintendo's chips are this part with Nintendo's
//! program, the firmware the DSP-1 games call on for their 3-D maths,
//! and this crate runs a dump of it: [`Upd7725::step`] executes one
//! instruction, which is one clock of the chip's 8 MHz.
//!
//! The chip has no bus. Its memories are all inside it, and the S-CPU
//! reaches it only through the data register DR and the status register
//! SR ([`Upd7725::read_data`], [`Upd7725::write_data`],
//! [`Upd7725::read_status`]). The host, `snes1-core`'s `Dsp1Chip`, runs
//! the chip up to the moment of each access before making it.
//!
//! How each instruction runs follows ares (`component/processor/upd96050`,
//! a model of this chip and of its larger sibling, the uPD96050),
//! published under the ISC license, its notice is at the end of
//! `LICENSE`: in this tree's names and structure. In three
//! places it does what bsnes-plus does where ares differs, and each is
//! said where it lives: the carry out of ADC and SBB (`instructions.rs`),
//! DRS as SR shows it while transfers are a byte each
//! ([`Upd7725::read_status`]), and the four jumps on the serial
//! acknowledges, which never jump (`instructions.rs`).

mod host;
mod instructions;
pub mod registers;

pub use registers::AluFlags;
use retro_core::cpu::Resettable;

/// Instructions in the program ROM, 24 bits each.
pub const PROGRAM_WORDS: usize = 2048;
/// Words in the data ROM, the program's constants.
pub const DATA_ROM_WORDS: usize = 1024;
/// Words in the data RAM.
pub const DATA_RAM_WORDS: usize = 256;

/// PC is 11 bits: it addresses the 2048 instructions.
const PC_MASK: u16 = 0x07FF;
/// RP is 10 bits: it addresses the data ROM.
const RP_MASK: u16 = 0x03FF;
/// DP is 8 bits: it addresses the data RAM.
const DP_MASK: u16 = 0x00FF;
/// The return stack's pointer is 4 bits.
const SP_MASK: u8 = 0x0F;

/// The chip: its registers, its memories, and the clocks it has run.
///
/// The registers are kept as the bits the chip holds; only the multiplier
/// reads two of them as signed.
#[derive(Clone)]
pub struct Upd7725 {
    /// PC: the address of the next instruction (11 bits).
    pub program_counter: u16,
    /// The return stack, 16 entries; a call pushes the address after it.
    pub call_stack: [u16; 16],
    /// SP: the next free entry of the return stack (4 bits).
    pub stack_pointer: u8,
    /// RP: the data-ROM word the RO source and the KLR move read
    /// (10 bits).
    pub rom_pointer: u16,
    /// DP: the data-RAM word an instruction reads and writes (8 bits).
    pub ram_pointer: u16,
    /// A and B, the accumulators: index 0 is A, 1 is B.
    pub accumulators: [u16; 2],
    /// FLAGA and FLAGB, each accumulator's flags, indexed as
    /// [`Upd7725::accumulators`] is.
    pub flags: [AluFlags; 2],
    /// K, the multiplier's first input.
    pub multiplicand: u16,
    /// L, the multiplier's second input.
    pub multiplier: u16,
    /// M: the sign and the top 15 bits of the product of K and L, as a
    /// 16-bit fraction.
    pub product_high: u16,
    /// N: the low 15 bits of the product, shifted up one.
    pub product_low: u16,
    /// TR, the temporary register.
    pub temporary: u16,
    /// TRB, the second temporary register.
    pub temporary_b: u16,
    /// SR, the status register, see [`registers::status`].
    pub status: u16,
    /// DR, the data register: the word passing between the chip and the
    /// S-CPU.
    pub data_port: u16,
    /// SI, the serial input register (nothing drives it on the SNES).
    pub serial_in: u16,
    /// SO, the serial output register; JMPSO jumps to it.
    pub serial_out: u16,
    /// The program, [`PROGRAM_WORDS`] instructions of 24 bits.
    pub program: Vec<u32>,
    /// The data ROM, [`DATA_ROM_WORDS`] words.
    pub data_rom: Vec<u16>,
    /// The data RAM.
    pub data_ram: [u16; DATA_RAM_WORDS],
    /// Instructions executed since reset, clocks, one each.
    pub clocks: u64,
}

impl Default for Upd7725 {
    fn default() -> Self {
        Self::new()
    }
}

impl Upd7725 {
    /// A chip at power-on with both ROMs and the data RAM zero; give it a
    /// program with [`Upd7725::load_firmware`].
    pub fn new() -> Self {
        Self {
            program_counter: 0,
            call_stack: [0; 16],
            stack_pointer: 0,
            rom_pointer: 0,
            ram_pointer: 0,
            accumulators: [0; 2],
            flags: [AluFlags::default(); 2],
            multiplicand: 0,
            multiplier: 0,
            product_high: 0,
            product_low: 0,
            temporary: 0,
            temporary_b: 0,
            status: 0,
            data_port: 0,
            serial_in: 0,
            serial_out: 0,
            program: vec![0; PROGRAM_WORDS],
            data_rom: vec![0; DATA_ROM_WORDS],
            data_ram: [0; DATA_RAM_WORDS],
            clocks: 0,
        }
    }

    /// A chip at power-on with the given firmware loaded.
    pub fn with_firmware(program: &[u32], data_rom: &[u16]) -> Self {
        let mut chip = Self::new();
        chip.load_firmware(program, data_rom);
        chip
    }

    /// Copy a firmware into the two ROMs. Each instruction keeps its low
    /// 24 bits; words beyond either ROM's size are not read, and a short
    /// firmware leaves the rest of the ROM as it was.
    pub fn load_firmware(&mut self, program: &[u32], data_rom: &[u16]) {
        for (slot, word) in self.program.iter_mut().zip(program) {
            *slot = word & 0x00FF_FFFF;
        }
        for (slot, word) in self.data_rom.iter_mut().zip(data_rom) {
            *slot = *word;
        }
    }

    /// Run one instruction, one clock. PC moves on before the instruction
    /// runs, so a call pushes the address after it; the two top bits of the
    /// word say which of the four kinds it is. The multiplier then forms
    /// the product of K and L, whatever the instruction did.
    pub fn step(&mut self) {
        let word = self.program[usize::from(self.program_counter & PC_MASK)];
        self.program_counter = self.program_counter.wrapping_add(1) & PC_MASK;
        match word >> 22 {
            0 => self.operate(word),
            1 => {
                self.operate(word);
                self.return_from_call();
            }
            2 => self.jump(word),
            _ => self.load(word),
        }
        self.multiply();
        self.clocks = self.clocks.wrapping_add(1);
    }

    /// The signed product of K and L, as M (the sign and the top 15 bits,
    /// the product of two 1.15 fractions) and N (the bits below them).
    fn multiply(&mut self) {
        let product = i32::from(self.multiplicand as i16) * i32::from(self.multiplier as i16);
        self.product_high = (product >> 15) as u16;
        self.product_low = (product << 1) as u16;
    }
}

impl Resettable for Upd7725 {
    /// The chip's reset: every register clear and the program run again
    /// from the top. The two ROMs and the data RAM keep what they hold.
    fn reset(&mut self) {
        *self = Self {
            program: std::mem::take(&mut self.program),
            data_rom: std::mem::take(&mut self.data_rom),
            data_ram: self.data_ram,
            ..Self::new()
        };
    }
}

#[cfg(test)]
mod tests;
