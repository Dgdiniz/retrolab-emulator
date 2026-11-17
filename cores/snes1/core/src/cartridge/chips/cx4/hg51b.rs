//! The HG51B169: its registers and how it runs an instruction.
//!
//! A 24-bit processor: an accumulator A, a 48-bit product in two 24-bit
//! halves, sixteen general registers, three data latches (for the cart bus,
//! the data ROM and the data RAM), a 15-bit page register P, an eight-deep
//! call stack, and four flags. Its program runs out of two cache pages of 256
//! words; the program counter's high bits name the program page and its low
//! byte the word within it.
//!
//! [`Hg51b::execute`] runs one instruction. Every instruction takes one
//! clock; a jump taken, a call and a return take two more, a skip taken one
//! more. Touching register-file index `$2E` or `$2F` starts a transfer on the
//! chip's cart bus that lands some clocks later ([`BusTransfer`]); LOOP waits
//! for it. The bus itself, the cache loading and the step driver are in
//! [`super::memory`], and the registers the S-CPU sees in [`super::ports`].

use super::data::DATA_ROM;
use super::instructions::{decode, AluOp, Condition, Flag, Lane, Op, ShiftOp};
use super::memory::Cx4Bus;
use super::registers::{self, Operand, RegisterRef};

/// The data RAM: 3 KiB the game and the chip exchange vertices through.
pub const DATA_RAM_SIZE: usize = 3072;

/// The page number of a cache page that holds no program page; it never
/// equals a real one (the program counter has 23 bits).
pub const NO_PAGE: u16 = 0xFFFF;

/// The 24 bits of a register.
const MASK_24: u32 = 0xFF_FFFF;

/// The program counter's 23 bits.
const PC_MASK: u32 = 0x7F_FFFF;

/// The four flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    /// N: bit 23 of the last result.
    pub negative: bool,
    /// Z: the last result was 0.
    pub zero: bool,
    /// V: the last addition or subtraction overflowed.
    pub overflow: bool,
    /// C: the carry, or no borrow, of the last addition, subtraction or
    /// comparison.
    pub carry: bool,
}

/// A transfer on the chip's cart bus. It starts when an instruction reads
/// or writes register-file index `$2E` (with the ROM's wait states) or `$2F`
/// (the RAM's), and is repeated each clock until its wait runs out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BusTransfer {
    /// The cart-bus address, taken from the bus address when it started.
    pub address: u32,
    /// Clocks left, counting the one it started on; 0 when none is in
    /// flight.
    pub remaining: u8,
    /// A write (of [`data`](Self::data)) rather than a read into the bus data
    /// latch.
    pub write: bool,
    /// What a write puts on the bus.
    pub data: u32,
}

/// One of the two instruction-cache pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachePage {
    /// Locked by the S-CPU (`$7F4C`): a jump never reloads it.
    pub locked: bool,
    /// The program page it holds, [`NO_PAGE`] for none.
    pub page: u16,
    /// Its 256 instruction words.
    pub words: [u16; 256],
}

impl CachePage {
    /// An unlocked page holding nothing.
    pub const fn empty() -> Self {
        Self {
            locked: false,
            page: NO_PAGE,
            words: [0; 256],
        }
    }
}

/// What the S-CPU sets through the register window (`$7F40-$7FFF`), and the
/// work it has asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Io {
    /// A DMA is waiting to run.
    pub dma_pending: bool,
    /// A cache page is being loaded, or a preload is waiting to run.
    pub cache_loading: bool,
    /// The core is suspended (`$7F55-$7F5C`; `$7F5D` resumes it).
    pub suspended: bool,
    /// Clocks a timed suspend still holds; 0 for one only `$7F5D` ends.
    pub suspend_remaining: u32,
    /// DMA source address (`$7F40-$7F42`).
    pub dma_source: u32,
    /// DMA length in bytes (`$7F43-$7F44`).
    pub dma_length: u16,
    /// DMA target address (`$7F45-$7F47`).
    pub dma_target: u32,
    /// The cache page a preload fills (`$7F48`).
    pub preload_page: u8,
    /// Where the program pages start in ROM (`$7F49-$7F4B`).
    pub program_offset: u32,
    /// The program page to start in (`$7F4D-$7F4E`), and the one a preload
    /// loads.
    pub program_page: u16,
    /// The word in that page to start at (`$7F4F`).
    pub entry_pc: u8,
    /// Wait states of a cart ROM access (`$7F50` high nibble).
    pub rom_wait: u8,
    /// Wait states of a cart RAM access (`$7F50` low nibble).
    pub ram_wait: u8,
    /// The interrupt to the S-CPU is masked (`$7F51`).
    pub irq_masked: bool,
    /// `$7F52`, whose purpose is not known (it reads back what was written;
    /// 1 at reset).
    pub register_7f52: u8,
    /// `$7F60-$7F7F`, kept for the S-CPU to read back.
    pub vectors: [u8; 32],
}

/// The HG51B169.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hg51b {
    /// The program counter: program page in bits 8-22, word in bits 0-7.
    pub pc: u32,
    /// P (15 bits): a long jump's target page, and where the program goes
    /// when it runs off the end of the first cache page.
    pub page: u16,
    /// N, Z, V and C.
    pub flags: Flags,
    /// A, the accumulator.
    pub accumulator: u32,
    /// The high 24 bits of the last product.
    pub product_high: u32,
    /// The low 24 bits of the last product.
    pub product_low: u32,
    /// R0-R15.
    pub registers: [u32; 16],
    /// The cart-bus data latch.
    pub bus_data: u32,
    /// The data ROM latch.
    pub rom_data: u32,
    /// The data RAM latch.
    pub ram_data: u32,
    /// The cart-bus address a transfer uses.
    pub bus_address: u32,
    /// What RDRAM/WRRAM add to an immediate RAM address.
    pub ram_base: u32,
    /// The last value through the register file. The S-CPU reads it in
    /// place of a general register while the core runs.
    pub last_value: u32,
    /// The call stack, newest first.
    pub call_stack: [u32; 8],
    /// The core is stopped.
    pub halted: bool,
    /// The cache page the program counter runs in (0 or 1).
    pub active_page: u8,
    /// The core went idle and the S-CPU has not acknowledged it.
    pub irq_pending: bool,
    /// The cart-bus transfer in flight.
    pub transfer: BusTransfer,
    /// The instruction word being run.
    pub opcode: u16,
    /// The two instruction-cache pages.
    pub cache: [CachePage; 2],
    /// The S-CPU's settings and requests.
    pub io: Io,
    /// The data RAM.
    pub data_ram: Box<[u8; DATA_RAM_SIZE]>,
    /// Clocks run.
    pub cycles: u64,
}

impl Default for Hg51b {
    fn default() -> Self {
        Self::new()
    }
}

impl Hg51b {
    /// The core at power-on: every register clear, then [`reset`](Self::reset).
    pub fn new() -> Self {
        let mut core = Self {
            pc: 0,
            page: 0,
            flags: Flags::default(),
            accumulator: 0,
            product_high: 0,
            product_low: 0,
            registers: [0; 16],
            bus_data: 0,
            rom_data: 0,
            ram_data: 0,
            bus_address: 0,
            ram_base: 0,
            last_value: 0,
            call_stack: [0; 8],
            halted: true,
            active_page: 0,
            irq_pending: false,
            transfer: BusTransfer::default(),
            opcode: 0,
            cache: [CachePage::empty(); 2],
            io: Io::default(),
            data_ram: Box::new([0; DATA_RAM_SIZE]),
            cycles: 0,
        };
        core.reset();
        core
    }

    /// A reset: the core stops, the data RAM, the flags, the S-CPU's
    /// settings and the cache are cleared, the wait states go back to 3 and
    /// `$7F52` to 1. A, the products, P, the general registers, the latches
    /// and the call stack keep their values; a game sets them before use.
    pub fn reset(&mut self) {
        self.data_ram.fill(0);
        self.halted = true;
        self.active_page = 0;
        self.irq_pending = false;
        self.transfer.remaining = 0;
        self.last_value = 0;
        self.flags = Flags::default();
        self.opcode = 0;
        self.io = Io {
            rom_wait: 3,
            ram_wait: 3,
            register_7f52: 1,
            ..Io::default()
        };
        self.cache = [CachePage::empty(); 2];
        self.cycles = 0;
    }

    /// The chip is working: running code, a DMA, or a cache load. The
    /// status register's busy bit.
    pub fn busy(&self) -> bool {
        self.io.dma_pending || self.io.cache_loading || !self.halted
    }

    /// The chip is using the cart bus: a DMA, a cache load, or a transfer in
    /// flight. The status register's bus bit.
    pub fn bus_in_use(&self) -> bool {
        self.io.dma_pending || self.io.cache_loading || self.transfer.remaining > 0
    }

    /// Read register-file `index`. The value read becomes the last value;
    /// `$2E`/`$2F` start a cart-bus read and give 0.
    pub fn read_register(&mut self, index: u8) -> u32 {
        let value = match registers::decode_register(index) {
            RegisterRef::Accumulator => self.accumulator,
            RegisterRef::ProductHigh => self.product_high,
            RegisterRef::ProductLow => self.product_low,
            RegisterRef::BusData => self.bus_data,
            RegisterRef::RomData => self.rom_data,
            RegisterRef::RamData => self.ram_data,
            RegisterRef::BusAddress => self.bus_address,
            RegisterRef::RamBase => self.ram_base,
            RegisterRef::ProgramCounter | RegisterRef::Page => u32::from(self.page),
            RegisterRef::BusTransfer => {
                self.start_transfer(index, None);
                0
            }
            RegisterRef::Constant(value) => value,
            RegisterRef::General(n) => self.registers[usize::from(n)],
            RegisterRef::Unmapped => 0,
        };
        self.last_value = value;
        value
    }

    /// Write register-file `index` (24 bits of `value`), which becomes the
    /// last value; `$2E`/`$2F` start a cart-bus write of it. P, the program
    /// counter and the constants cannot be written this way.
    pub fn write_register(&mut self, index: u8, value: u32) {
        let value = value & MASK_24;
        self.last_value = value;
        match registers::decode_register(index) {
            RegisterRef::Accumulator => self.accumulator = value,
            RegisterRef::ProductHigh => self.product_high = value,
            RegisterRef::ProductLow => self.product_low = value,
            RegisterRef::BusData => self.bus_data = value,
            RegisterRef::RomData => self.rom_data = value,
            RegisterRef::RamData => self.ram_data = value,
            RegisterRef::BusAddress => self.bus_address = value,
            RegisterRef::RamBase => self.ram_base = value,
            RegisterRef::BusTransfer => self.start_transfer(index, Some(value)),
            RegisterRef::General(n) => self.registers[usize::from(n)] = value,
            RegisterRef::ProgramCounter
            | RegisterRef::Page
            | RegisterRef::Constant(_)
            | RegisterRef::Unmapped => {}
        }
    }

    /// Start a cart-bus transfer at the bus address: a write of `write`'s
    /// value, or a read. An odd index waits the RAM's wait states, an even
    /// one the ROM's; the count includes the clock it starts on.
    fn start_transfer(&mut self, index: u8, write: Option<u32>) {
        let wait = if index & 1 != 0 {
            self.io.ram_wait
        } else {
            self.io.rom_wait
        };
        self.transfer.address = self.bus_address;
        self.transfer.remaining = wait + 1;
        self.transfer.write = write.is_some();
        if let Some(value) = write {
            self.transfer.data = value;
        }
    }

    /// The operand of `opcode`: its low byte, or the register that byte
    /// names (a read, with its effects).
    fn operand_value(&mut self, opcode: u16) -> u32 {
        match registers::operand(opcode) {
            Operand::Immediate(value) => u32::from(value),
            Operand::Register(index) => self.read_register(index),
        }
    }

    /// The data RAM address of RDRAM/WRRAM: the immediate plus the RAM
    /// base, or the register the low byte names.
    fn ram_address(&mut self, opcode: u16) -> u32 {
        match registers::operand(opcode) {
            Operand::Immediate(offset) => u32::from(offset).wrapping_add(self.ram_base) & MASK_24,
            Operand::Register(index) => self.read_register(index) & MASK_24,
        }
    }

    /// Whether a jump on `condition` is taken.
    fn holds(&self, condition: Condition) -> bool {
        match condition {
            Condition::Always => true,
            Condition::Zero => self.flags.zero,
            Condition::Carry => self.flags.carry,
            Condition::Negative => self.flags.negative,
            Condition::Overflow => self.flags.overflow,
        }
    }

    /// The value of `flag`.
    fn flag(&self, flag: Flag) -> bool {
        match flag {
            Flag::Overflow => self.flags.overflow,
            Flag::Carry => self.flags.carry,
            Flag::Zero => self.flags.zero,
            Flag::Negative => self.flags.negative,
        }
    }

    /// N and Z from a 24-bit result.
    fn set_sign_and_zero(&mut self, result: u32) {
        let result = result & MASK_24;
        self.flags.negative = result & 0x80_0000 != 0;
        self.flags.zero = result == 0;
    }

    /// Run one instruction.
    pub fn execute(&mut self, opcode: u16, bus: &mut Cx4Bus) {
        match decode(opcode) {
            Op::Nop => {}
            Op::Jump(condition) => {
                if self.holds(condition) {
                    self.jump(opcode, bus);
                }
            }
            Op::Skip { flag, when_set } => {
                if self.flag(flag) == when_set {
                    self.advance_pc();
                    self.spend(1, bus);
                }
            }
            Op::Return => {
                self.pc = registers::pull(&mut self.call_stack);
                self.select_page_for_pc();
                self.spend(2, bus);
            }
            Op::WaitForBus => {
                // Run the clocks the transfer still needs, all but the one
                // this instruction takes anyway.
                let left = u32::from(self.transfer.remaining);
                if left > 1 {
                    self.spend(left - 1, bus);
                }
            }
            Op::IncrementBusAddress => {
                self.bus_address = self.bus_address.wrapping_add(1) & MASK_24;
            }
            Op::Alu(op) => self.alu(op, opcode),
            Op::Multiply => self.multiply(opcode),
            Op::SignExtendByte => {
                let value = self.operand_value(opcode) as u8 as i8;
                self.accumulator = i32::from(value) as u32 & MASK_24;
            }
            Op::SignExtendWord => {
                let value = self.operand_value(opcode) as u16 as i16;
                self.accumulator = i32::from(value) as u32 & MASK_24;
            }
            Op::Load => self.accumulator = self.operand_value(opcode) & MASK_24,
            Op::LoadPage => self.page = (self.operand_value(opcode) & 0x7FFF) as u16,
            Op::LoadPageLow => self.set_page_byte(false, opcode as u8),
            Op::LoadPageHigh => self.set_page_byte(true, opcode as u8),
            Op::ReadBus => {
                self.read_register(opcode as u8);
            }
            Op::ReadRam(lane) => self.read_ram(lane, opcode),
            Op::WriteRam(lane) => self.write_ram(lane, opcode),
            Op::ReadRom => self.rom_data = DATA_ROM[(self.accumulator & 0x3FF) as usize],
            Op::Shift(op) => self.shift(op, opcode),
            Op::Store => {
                let value = self.accumulator;
                self.write_register(opcode as u8, value);
            }
            Op::StoreBusData => {
                let value = self.bus_data;
                self.write_register(opcode as u8, value);
            }
            Op::Swap => {
                let index = opcode as u8;
                let other = self.read_register(index);
                let value = self.accumulator;
                self.accumulator = other & MASK_24;
                self.write_register(index, value);
            }
            Op::Clear => {
                self.accumulator = 0;
                self.page = 0;
                self.ram_data = 0;
                self.ram_base = 0;
            }
            Op::Halt | Op::Unknown => self.halted = true,
        }
        self.spend(1, bus);
    }

    /// Take a jump: push the return address for a call, go to the target,
    /// and pick the cache page that holds it.
    fn jump(&mut self, opcode: u16, bus: &mut Cx4Bus) {
        if opcode & 0x2000 != 0 {
            registers::push(&mut self.call_stack, self.pc);
        }
        self.pc = registers::jump_target(opcode, self.pc, self.page);
        self.select_page_for_pc();
        self.spend(2, bus);
    }

    /// The arithmetic and logic operations on A, shifted first, and the
    /// operand.
    fn alu(&mut self, op: AluOp, opcode: u16) {
        let shifted = registers::shifted(opcode, self.accumulator);
        let operand = self.operand_value(opcode);
        match op {
            AluOp::Add => {
                let result = shifted.wrapping_add(operand);
                self.store_arithmetic(shifted, operand, result);
                self.flags.carry = (result as i32) > MASK_24 as i32;
            }
            AluOp::Subtract => {
                let result = shifted.wrapping_sub(operand);
                self.store_arithmetic(shifted, operand, result);
                self.flags.carry = (result as i32) >= 0;
            }
            AluOp::SubtractFrom => {
                let result = operand.wrapping_sub(shifted);
                self.store_arithmetic(operand, shifted, result);
                self.flags.carry = (result as i32) >= 0;
            }
            AluOp::Compare => self.compare(shifted.wrapping_sub(operand)),
            AluOp::CompareFrom => self.compare(operand.wrapping_sub(shifted)),
            AluOp::And => self.store_logic(shifted & operand),
            AluOp::Or => self.store_logic(shifted | operand),
            AluOp::Xor => self.store_logic(shifted ^ operand),
        }
    }

    /// Store the result of an addition or subtraction of `left` and `right`
    /// in A and set N, Z and V. V is set when bit 23 of the result differs
    /// from `left`'s while the operands do not both have it, the same test
    /// for all three (bsnes-plus has it, and this core keeps it).
    fn store_arithmetic(&mut self, left: u32, right: u32, result: u32) {
        self.accumulator = result & MASK_24;
        self.set_sign_and_zero(result);
        self.flags.overflow = (left ^ result) & !(left & right) & 0x80_0000 != 0;
    }

    /// A comparison's flags: N, Z, and C as for the subtraction.
    fn compare(&mut self, result: u32) {
        self.set_sign_and_zero(result);
        self.flags.carry = (result as i32) >= 0;
    }

    /// Store a logic operation's result in A and set N and Z.
    fn store_logic(&mut self, result: u32) {
        self.accumulator = result & MASK_24;
        self.set_sign_and_zero(result);
    }

    /// MUL: A times the operand, both read as signed 24-bit numbers; the
    /// 48-bit product in its two halves, N from its sign and Z if it is 0.
    fn multiply(&mut self, opcode: u16) {
        let left = signed_24(self.accumulator);
        let right = signed_24(self.operand_value(opcode));
        let product = left.wrapping_mul(right);
        self.product_low = product as u32 & MASK_24;
        self.product_high = (product >> 24) as u32 & MASK_24;
        self.flags.negative = self.product_high & 0x80_0000 != 0;
        self.flags.zero = product == 0;
    }

    /// The shifts of A by the operand's count.
    fn shift(&mut self, op: ShiftOp, opcode: u16) {
        let count = self.operand_value(opcode);
        let a = self.accumulator;
        let result = match op {
            ShiftOp::Right => a.checked_shr(count).unwrap_or(0),
            ShiftOp::Left => a.checked_shl(count).unwrap_or(0),
            ShiftOp::ArithmeticRight => {
                // A as a signed 24-bit number; past 31 the sign fills it.
                let signed = ((a << 8) as i32) >> 8;
                (signed >> count.min(31)) as u32
            }
            ShiftOp::RotateRight => {
                let a = a & MASK_24;
                match count % 24 {
                    0 => a,
                    n => (a >> n) | (a << (24 - n)),
                }
            }
        };
        self.accumulator = result & MASK_24;
        self.set_sign_and_zero(result);
    }

    /// One byte of P from an immediate (LD PL / LD PH), P staying 15 bits.
    fn set_page_byte(&mut self, high: bool, value: u8) {
        let [low_byte, high_byte] = self.page.to_le_bytes();
        let page = if high {
            u16::from_le_bytes([low_byte, value])
        } else {
            u16::from_le_bytes([value, high_byte])
        };
        self.page = page & 0x7FFF;
    }

    /// RDRAM: one byte of data RAM into its lane of the RAM data latch. An
    /// address past the 3 KiB reads nothing.
    fn read_ram(&mut self, lane: Lane, opcode: u16) {
        let address = self.ram_address(opcode);
        if let Some(&byte) = self.data_ram.get(address as usize) {
            let shift = lane.shift();
            self.ram_data =
                (self.ram_data & MASK_24 & !(0xFF << shift)) | (u32::from(byte) << shift);
            self.last_value = self.ram_data;
        }
    }

    /// WRRAM: one lane of the RAM data latch into data RAM. An address past
    /// the 3 KiB writes nothing.
    fn write_ram(&mut self, lane: Lane, opcode: u16) {
        let address = self.ram_address(opcode);
        let byte = (self.ram_data >> lane.shift()) as u8;
        if let Some(cell) = self.data_ram.get_mut(address as usize) {
            *cell = byte;
            self.last_value = self.ram_data;
        }
    }

    /// Step the program counter. Off the end of the first cache page the
    /// program goes on at the start of P's page, in the second; off the end
    /// of the second the core stops.
    pub fn advance_pc(&mut self) {
        self.pc = self.pc.wrapping_add(1) & PC_MASK;
        if self.pc & 0xFF != 0 {
            return;
        }
        match self.active_page {
            0 => {
                self.pc = u32::from(self.page) << 8;
                self.active_page = 1;
            }
            _ => self.halted = true,
        }
    }

    /// After a jump or a return, run from the cache page that holds the
    /// program counter's page: this one if it does, else the other if it
    /// does or may be reloaded; with both locked and neither holding it, the
    /// core stops.
    pub(super) fn select_page_for_pc(&mut self) {
        let wanted = (self.pc >> 8) as u16;
        let current = usize::from(self.active_page);
        let other = current ^ 1;
        if self.cache[current].page == wanted {
            return;
        }
        if self.cache[other].page == wanted || !self.cache[other].locked {
            self.active_page ^= 1;
        } else if self.cache[current].locked {
            self.halted = true;
        }
    }

    /// Let `clocks` clocks pass: each one moves the cart-bus transfer on,
    /// and a timed suspend runs out at once.
    pub fn spend(&mut self, clocks: u32, bus: &mut Cx4Bus) {
        for _ in 0..clocks {
            self.cycles = self.cycles.wrapping_add(1);
            self.advance_transfer(bus);
            // A timed suspend holds the core for its count of clocks, which
            // pass here at once; one the S-CPU ends (`$7F5D`) has no count.
            if self.io.suspended && self.io.suspend_remaining > 0 {
                self.cycles = self
                    .cycles
                    .wrapping_add(u64::from(self.io.suspend_remaining));
                self.io.suspend_remaining = 0;
                self.io.suspended = false;
            }
        }
    }

    /// One clock of the cart-bus transfer: the access is repeated each clock
    /// until the last, on which it ends without one.
    fn advance_transfer(&mut self, bus: &mut Cx4Bus) {
        if self.transfer.remaining == 0 {
            return;
        }
        self.transfer.remaining -= 1;
        if self.transfer.remaining == 0 {
            return;
        }
        if self.transfer.write {
            self.last_value = self.transfer.data;
            bus.write(self.transfer.address, self.transfer.data as u8);
        } else {
            self.bus_data = u32::from(bus.read(self.transfer.address));
            self.last_value = self.bus_data;
        }
    }
}

/// A 24-bit value read as a signed number.
fn signed_24(value: u32) -> i64 {
    i64::from(((value << 8) as i32) >> 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run one opcode against an empty bus, enough for every instruction
    /// that touches only the registers and the data RAM (none here starts a
    /// cart-bus transfer, so the bus is never read).
    fn exec(core: &mut Hg51b, opcode: u16) {
        let mut ram: Vec<u8> = Vec::new();
        let mut dirty = false;
        let mut bus = Cx4Bus {
            rom: &[],
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        core.execute(opcode, &mut bus);
    }

    #[test]
    fn a_new_core_is_halted_and_idle() {
        let core = Hg51b::new();
        assert!(core.halted, "boots halted (idle) until the S-CPU starts it");
        assert_eq!(core.pc, 0);
        assert_eq!(core.active_page, 0);
        assert_eq!(core.io.rom_wait, 3);
        assert_eq!(core.io.ram_wait, 3);
        assert_eq!(core.io.register_7f52, 1);
        assert!(!core.io.dma_pending && !core.io.cache_loading);
        for page in &core.cache {
            assert_eq!(page.page, NO_PAGE, "the cache pages start empty");
            assert!(!page.locked);
        }
        assert!(!core.busy(), "a reset core is idle");
        assert!(!core.bus_in_use());
    }

    // ---- register file ------------------------------------------------------

    #[test]
    fn register_file_round_trip_sets_the_last_value() {
        let mut c = Hg51b::new();
        c.write_register(0x00, 0x12_3456);
        assert_eq!(c.accumulator, 0x12_3456);
        assert_eq!(c.last_value, 0x12_3456, "a write sets the last value");
        assert_eq!(c.read_register(0x00), 0x12_3456);
        assert_eq!(c.last_value, 0x12_3456, "a read sets the last value");

        // Indices $60-$6F name R0-R15, and $70-$7F name them again.
        c.write_register(0x63, 0xAB_CDEF);
        assert_eq!(c.registers[3], 0xAB_CDEF);
        assert_eq!(c.read_register(0x63), 0xAB_CDEF);
        assert_eq!(c.read_register(0x73), 0xAB_CDEF, "$73 names R3 too");
        c.write_register(0x73, 0x00_0001);
        assert_eq!(c.registers[3], 0x00_0001, "writing $73 writes R3");
    }

    #[test]
    fn register_file_constants_and_unmapped() {
        let mut c = Hg51b::new();
        assert_eq!(c.read_register(0x50), 0x00_0000);
        assert_eq!(c.read_register(0x51), 0xFF_FFFF);
        assert_eq!(c.read_register(0x56), 0x80_0000);
        assert_eq!(
            c.last_value, 0x80_0000,
            "reading a constant sets the last value too"
        );
        // An unmapped index reads 0, which becomes the last value.
        c.last_value = 0xDEAD;
        assert_eq!(c.read_register(0x04), 0);
        assert_eq!(c.last_value, 0, "an unmapped read sets the last value to 0");
    }

    #[test]
    fn register_0x20_reads_p_not_the_program_counter() {
        // Index $20 reads P, as $28 does, not the program counter's low byte
        // (bsnes-plus reads it so too, and this core keeps that).
        let mut c = Hg51b::new();
        c.page = 0x1234;
        c.pc = 0x00_5678;
        assert_eq!(
            c.read_register(0x20),
            0x1234,
            "$20 reads P, not the program counter"
        );
        assert_eq!(c.read_register(0x28), 0x1234, "$28 reads P");
        assert_eq!(c.last_value, 0x1234);
    }

    #[test]
    fn registers_2e_and_2f_start_a_bus_transfer() {
        let mut c = Hg51b::new();
        c.bus_address = 0x12_3456;
        // A read through $2E waits the ROM's wait states (3 at reset), plus
        // the clock it starts on.
        c.read_register(0x2E);
        assert_eq!(c.transfer.address, 0x12_3456);
        assert_eq!(c.transfer.remaining, c.io.rom_wait + 1);
        assert!(!c.transfer.write, "$2E starts a read");
        assert_eq!(c.last_value, 0, "starting it reads 0");

        // A write through $2F waits the RAM's wait states and keeps the value.
        c.write_register(0x2F, 0xAA_BBCC);
        assert_eq!(c.transfer.address, 0x12_3456);
        assert_eq!(c.transfer.remaining, c.io.ram_wait + 1);
        assert!(c.transfer.write, "$2F starts a write");
        assert_eq!(c.transfer.data, 0xAA_BBCC);
        assert_eq!(c.last_value, 0xAA_BBCC);
    }

    // ---- ALU ----------------------------------------------------------------

    #[test]
    fn add_carry_and_overflow() {
        // add a,#5  ->  0x10 + 5 = 0x15, no flags.
        let mut c = Hg51b::new();
        c.accumulator = 0x00_0010;
        exec(&mut c, 0x8405);
        assert_eq!(c.accumulator, 0x00_0015);
        assert!(!c.flags.negative && !c.flags.zero && !c.flags.carry && !c.flags.overflow);

        // 0xffffff + 1 -> 0 with carry (unsigned wrap) and zero set.
        c.accumulator = 0xFF_FFFF;
        exec(&mut c, 0x8401);
        assert_eq!(c.accumulator, 0);
        assert!(c.flags.zero && c.flags.carry);
        assert!(!c.flags.negative);

        // 0x7fffff + 1 -> 0x800000: signed overflow, negative, no carry.
        c.accumulator = 0x7F_FFFF;
        exec(&mut c, 0x8401);
        assert_eq!(c.accumulator, 0x80_0000);
        assert!(c.flags.negative && c.flags.overflow);
        assert!(!c.flags.carry && !c.flags.zero);
    }

    #[test]
    fn sub_and_subr_borrow() {
        // sub a,#5 -> 0x10 - 5 = 0xB, carry set (no borrow).
        let mut c = Hg51b::new();
        c.accumulator = 0x00_0010;
        exec(&mut c, 0x9405);
        assert_eq!(c.accumulator, 0x00_000B);
        assert!(c.flags.carry && !c.flags.negative && !c.flags.zero);

        // sub a,#0x10 -> 5 - 0x10 = -0xB, borrow (c clear), negative.
        c.accumulator = 0x00_0005;
        exec(&mut c, 0x9410);
        assert_eq!(c.accumulator, 0xFF_FFF5);
        assert!(!c.flags.carry && c.flags.negative);

        // subr a,#0x20 -> 0x20 - a. a=5 -> 0x1B, carry set.
        c.accumulator = 0x00_0005;
        exec(&mut c, 0x8C20);
        assert_eq!(c.accumulator, 0x00_001B);
        assert!(c.flags.carry);
    }

    #[test]
    fn cmp_and_cmpr_set_flags_without_touching_a() {
        // cmp a,#0x20 with a=0x20 -> equal: z and c set, a unchanged.
        let mut c = Hg51b::new();
        c.accumulator = 0x00_0020;
        exec(&mut c, 0x5420);
        assert_eq!(c.accumulator, 0x00_0020, "cmp leaves a alone");
        assert!(c.flags.zero && c.flags.carry && !c.flags.negative);

        // cmpr a,#0x20 -> 0x20 - a. a=0x10 -> 0x10 >= 0: c set, not zero.
        // (0x4C20: the 0x0400 immediate bit is required, 0x4820 would read
        // register $20, which reads P.)
        c.accumulator = 0x00_0010;
        exec(&mut c, 0x4C20);
        assert_eq!(c.accumulator, 0x00_0010);
        assert!(c.flags.carry && !c.flags.zero);
    }

    #[test]
    fn shift_a_prefix_applies_before_alu() {
        // add a<<8,#1: a=0x000012 -> (0x1200) + 1 = 0x1201.
        let mut c = Hg51b::new();
        c.accumulator = 0x00_0012;
        exec(&mut c, 0x8601); // 0x8000 | 0x0200 (shift 8) | 0x0400 (imm) | 1
        assert_eq!(c.accumulator, 0x00_1201);
    }

    #[test]
    fn mul_produces_48bit_signed_result() {
        // 16 * 16 = 256.
        let mut c = Hg51b::new();
        c.accumulator = 0x00_0010;
        exec(&mut c, 0x9C10); // mul a,#0x10
        assert_eq!(c.product_low, 0x00_0100);
        assert_eq!(c.product_high, 0);
        assert!(!c.flags.negative && !c.flags.zero);

        // -1 * 2 = -2 -> low half 0xfffffe, high half 0xffffff, negative.
        c.accumulator = 0xFF_FFFF;
        exec(&mut c, 0x9C02);
        assert_eq!(c.product_low, 0xFF_FFFE);
        assert_eq!(c.product_high, 0xFF_FFFF);
        assert!(c.flags.negative && !c.flags.zero);

        // 0 * anything = 0 -> zero set.
        c.accumulator = 0;
        exec(&mut c, 0x9C05);
        assert!(c.flags.zero);
    }

    #[test]
    fn logic_ops() {
        let mut c = Hg51b::new();
        c.accumulator = 0x00_FF00;
        exec(&mut c, 0xACFF); // xor a,#0xff
        assert_eq!(c.accumulator, 0x00_FFFF);

        c.accumulator = 0xFF_FFFF;
        exec(&mut c, 0xB40F); // and a,#0x0f
        assert_eq!(c.accumulator, 0x00_000F);

        c.accumulator = 0x00_00F0;
        exec(&mut c, 0xBC0F); // or a,#0x0f
        assert_eq!(c.accumulator, 0x00_00FF);
    }

    #[test]
    fn shifts() {
        // shr a,#8.
        let mut c = Hg51b::new();
        c.accumulator = 0x00_FF00;
        exec(&mut c, 0xC408);
        assert_eq!(c.accumulator, 0x00_00FF);

        // shl a,#8.
        c.accumulator = 0x00_00FF;
        exec(&mut c, 0xDC08);
        assert_eq!(c.accumulator, 0x00_FF00);

        // asr a,#4 on a negative value keeps the sign.
        c.accumulator = 0x80_0000;
        exec(&mut c, 0xCC04);
        assert_eq!(c.accumulator, 0xF8_0000);
        assert!(c.flags.negative);

        // ror a,#4 of 0x000001 -> 0x100000.
        c.accumulator = 0x00_0001;
        exec(&mut c, 0xD404);
        assert_eq!(c.accumulator, 0x10_0000);
    }

    #[test]
    fn sign_extend_byte_and_word() {
        // sxb a,#0x80 -> 0xffff80.
        let mut c = Hg51b::new();
        exec(&mut c, 0x5D80); // 0x5900 | 0x0400 | 0x80
        assert_eq!(c.accumulator, 0xFF_FF80);

        // sxw a,r0 with R0 = 0x00ffff -> (i16)0xffff = -1 -> 0xffffff.
        c.registers[0] = 0x00_FFFF;
        exec(&mut c, 0x5A60); // 0x5a00 | reg 0x60
        assert_eq!(c.accumulator, 0xFF_FFFF);
    }

    #[test]
    fn ld_variants() {
        let mut c = Hg51b::new();
        exec(&mut c, 0x6455); // ld a,#0x55
        assert_eq!(c.accumulator, 0x00_0055);

        exec(&mut c, 0x677F); // ld p,#0x7f
        assert_eq!(c.page, 0x007F);

        c.page = 0x0000;
        exec(&mut c, 0x7C40); // ld pl,#0x40
        assert_eq!(c.page, 0x0040);
        exec(&mut c, 0x7D7F); // ld ph,#0x7f
        assert_eq!(c.page, 0x7F40);
        // ld ph,#0xff -> high byte masked into the 15-bit p.
        exec(&mut c, 0x7DFF);
        assert_eq!(c.page, 0x7F40, "ph 0xff sets p high, masked to 15 bits");
    }

    #[test]
    fn rdrom_reads_the_internal_data_rom() {
        let mut c = Hg51b::new();
        c.accumulator = 0;
        exec(&mut c, 0x7000);
        assert_eq!(c.rom_data, 0xFF_FFFF); // DATA_ROM[0]
        c.accumulator = 4;
        exec(&mut c, 0x7000);
        assert_eq!(c.rom_data, 0x20_0000); // DATA_ROM[4]
                                           // A's low ten bits index it.
        c.accumulator = 0x400;
        exec(&mut c, 0x7000);
        assert_eq!(c.rom_data, 0xFF_FFFF);
    }

    // ---- internal data RAM --------------------------------------------------

    #[test]
    fn rdram_lanes_and_the_ram_base() {
        let mut c = Hg51b::new();
        c.data_ram[0x10] = 0xAB;
        // rdraml [#0x10] with the RAM base 0.
        exec(&mut c, 0x6C10);
        assert_eq!(c.ram_data & 0xFF, 0xAB);
        assert_eq!(c.last_value, c.ram_data);

        // The RAM base is added: 0x10 + 0x100.
        c.ram_base = 0x100;
        c.ram_data = 0;
        c.data_ram[0x110] = 0xCD;
        exec(&mut c, 0x6C10);
        assert_eq!(c.ram_data & 0xFF, 0xCD);

        // rdramh / rdramb fill the middle / high lanes.
        c.ram_base = 0;
        c.ram_data = 0;
        c.data_ram[0x20] = 0x34;
        exec(&mut c, 0x6D20); // rdramh [#0x20]
        assert_eq!(c.ram_data, 0x00_3400);
        c.data_ram[0x20] = 0x12;
        exec(&mut c, 0x6E20); // rdramb [#0x20]
        assert_eq!(c.ram_data, 0x12_3400);
    }

    #[test]
    fn rdram_past_the_data_ram_reads_nothing() {
        let mut c = Hg51b::new();
        c.ram_data = 0x12_3456;
        c.ram_base = 0xC00; // address 0xc00, past the 3 KiB
        exec(&mut c, 0x6C00);
        assert_eq!(
            c.ram_data, 0x12_3456,
            "an address past the data RAM reads nothing"
        );
    }

    #[test]
    fn wrram_lanes() {
        let mut c = Hg51b::new();
        c.ram_data = 0x12_3456;
        exec(&mut c, 0xEC20); // wrraml [#0x20] -> low byte
        assert_eq!(c.data_ram[0x20], 0x56);
        exec(&mut c, 0xED21); // wrramh [#0x21] -> mid byte
        assert_eq!(c.data_ram[0x21], 0x34);
        exec(&mut c, 0xEE22); // wrramb [#0x22] -> high byte
        assert_eq!(c.data_ram[0x22], 0x12);
        assert_eq!(c.last_value, 0x12_3456);
    }

    #[test]
    fn st_swap_and_register_write_ops() {
        let mut c = Hg51b::new();
        c.accumulator = 0x11_2233;
        exec(&mut c, 0xE063); // st r3,a
        assert_eq!(c.registers[3], 0x11_2233);

        c.accumulator = 0x00_00FF;
        c.registers[5] = 0xAA_BBCC;
        exec(&mut c, 0xF065); // swap a,r5
        assert_eq!(c.accumulator, 0xAA_BBCC);
        assert_eq!(c.registers[5], 0x00_00FF);
    }

    #[test]
    fn clear_inc_halt_nop() {
        let mut c = Hg51b::new();
        c.accumulator = 0xFF;
        c.page = 0x10;
        c.ram_data = 0x20;
        c.ram_base = 0x30;
        exec(&mut c, 0xF800); // clear
        assert_eq!(
            (c.accumulator, c.page, c.ram_data, c.ram_base),
            (0, 0, 0, 0)
        );

        c.bus_address = 0xFF_FFFF;
        exec(&mut c, 0x4000); // inc -> wraps to 24 bits
        assert_eq!(c.bus_address, 0);

        c.halted = false;
        exec(&mut c, 0xFC00); // halt
        assert!(c.halted);

        // nop and unknown: nop leaves halt clear, unknown halts.
        c.halted = false;
        exec(&mut c, 0x0000);
        assert!(!c.halted);
        exec(&mut c, 0x2100); // an unknown word
        assert!(c.halted);
    }

    // ---- control flow -------------------------------------------------------

    #[test]
    fn jump_and_call_push() {
        let mut c = Hg51b::new();
        c.pc = 0x00_0050;
        exec(&mut c, 0x0810); // jump #0x10 (near)
        assert_eq!(c.pc, 0x00_0010);

        // call (bit 0x2000) pushes the return address.
        c.pc = 0x00_0050;
        c.call_stack = [0; 8];
        exec(&mut c, 0x2810); // jsr #0x10
        assert_eq!(c.pc, 0x00_0010);
        assert_eq!(
            c.call_stack[0], 0x00_0050,
            "a call pushes the return address"
        );
    }

    #[test]
    fn conditional_jump_respects_flag() {
        let mut c = Hg51b::new();
        c.pc = 0x00_0050;
        c.flags.zero = false;
        exec(&mut c, 0x0C10); // jumpeq #0x10, z clear -> not taken
        assert_eq!(c.pc, 0x00_0050);
        c.flags.zero = true;
        exec(&mut c, 0x0C10); // z set -> taken
        assert_eq!(c.pc, 0x00_0010);
    }

    #[test]
    fn ret_pulls_from_the_stack() {
        let mut c = Hg51b::new();
        c.call_stack = [0x00_1234, 0x00_5678, 0, 0, 0, 0, 0, 0];
        exec(&mut c, 0x3C00); // ret
        assert_eq!(c.pc, 0x00_1234);
        assert_eq!(
            c.call_stack[0], 0x00_5678,
            "the next return address moves up"
        );
    }

    #[test]
    fn skip_advances_pc_when_taken() {
        let mut c = Hg51b::new();
        c.pc = 0x00_0050;
        c.flags.zero = false;
        exec(&mut c, 0x2601); // skipeq, z clear -> not taken
        assert_eq!(c.pc, 0x00_0050);
        c.flags.zero = true;
        exec(&mut c, 0x2601); // skipeq, z set -> skip
        assert_eq!(c.pc, 0x00_0051);
    }

    #[test]
    fn advance_pc_crosses_pages_then_halts() {
        let mut c = Hg51b::new();
        c.halted = false;
        c.active_page = 0;
        c.page = 0x0005;
        c.pc = 0x00_00FF;
        c.advance_pc();
        assert_eq!(c.pc, 0x00_0500, "off the first page it goes on at P's page");
        assert_eq!(c.active_page, 1);

        // Off the end of the second page the core stops.
        c.pc = 0x00_05FF;
        c.advance_pc();
        assert!(c.halted, "running off the second page halts");
    }

    #[test]
    fn select_page_for_pc_switches_or_halts() {
        let mut c = Hg51b::new();
        c.halted = false;
        // The current page holds the target -> stay.
        c.active_page = 0;
        c.cache[0].page = 0x0002;
        c.pc = 0x00_0210;
        c.select_page_for_pc();
        assert_eq!(c.active_page, 0);

        // The target elsewhere, the other page unlocked -> switch.
        c.pc = 0x00_0310;
        c.select_page_for_pc();
        assert_eq!(c.active_page, 1);

        // Both pages locked, neither holding it -> halt.
        c.active_page = 0;
        c.cache[0] = CachePage {
            locked: true,
            page: 0x00AA,
            words: [0; 256],
        };
        c.cache[1] = CachePage {
            locked: true,
            page: 0x00BB,
            words: [0; 256],
        };
        c.pc = 0x00_0C10;
        c.select_page_for_pc();
        assert!(c.halted);
    }

    // ---- the S-CPU's registers ----------------------------------------------

    #[test]
    fn writing_7f4f_starts_the_core() {
        // Writing the entry point at $7F4F starts the program at that word of
        // the program page, and the status reads busy.
        let mut c = Hg51b::new();
        c.write_port(0x7F4D, 0x01); // program page low byte = 1
        c.write_port(0x7F4F, 0x20); // entry point 0x20 -> start
        assert!(!c.halted, "$7F4F starts the core when halted");
        // pc = program page : entry point = (1 << 8) | 0x20.
        assert_eq!(c.pc, (1 << 8) | 0x20);
        assert!(c.busy(), "a running core reads busy");
        assert_eq!(
            c.read_port(0x7F53) & 0x40,
            0x40,
            "the status's busy bit is set"
        );
    }

    #[test]
    fn writing_7f47_asks_for_the_dma_and_7f48_for_a_preload() {
        let mut c = Hg51b::new();
        c.write_port(0x7F47, 0x00); // the DMA target's top byte asks for it
        assert!(c.io.dma_pending, "$7F47 asks for the DMA while halted");

        let mut c = Hg51b::new();
        c.write_port(0x7F48, 0x01); // a preload into cache page 1
        assert!(c.io.cache_loading, "$7F48 asks for a preload while halted");
        assert_eq!(c.io.preload_page, 0x01);
    }

    #[test]
    fn writing_7f51_and_7f5e_manage_the_interrupt() {
        let mut c = Hg51b::new();
        c.irq_pending = true;
        // $7F5E acknowledges the interrupt.
        c.write_port(0x7F5E, 0x00);
        assert!(!c.irq_pending, "$7F5E clears the pending interrupt");

        // $7F51 bit 0 masks the interrupt and drops a pending one.
        c.irq_pending = true;
        c.write_port(0x7F51, 0x01);
        assert!(c.io.irq_masked);
        assert!(!c.irq_pending, "masking drops the pending interrupt");

        // Writing 0 unmasks it.
        c.write_port(0x7F51, 0x00);
        assert!(!c.io.irq_masked);
    }

    #[test]
    fn writing_the_suspend_and_resume_registers() {
        let mut c = Hg51b::new();
        // $7F58 = $7F55 + 3 -> suspend for 32 * 3 = 96 clocks.
        c.write_port(0x7F58, 0x00);
        assert!(c.io.suspended);
        assert_eq!(c.io.suspend_remaining, 96);
        // $7F5D resumes immediately.
        c.write_port(0x7F5D, 0x00);
        assert!(!c.io.suspended);
    }

    #[test]
    fn spend_runs_out_a_timed_suspend() {
        let mut ram: Vec<u8> = Vec::new();
        let mut dirty = false;
        let mut bus = Cx4Bus {
            rom: &[],
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        let mut c = Hg51b::new();
        c.write_port(0x7F56, 0x00); // suspend for 32 * 1 = 32 clocks
        assert_eq!(c.io.suspend_remaining, 32);
        c.cycles = 0;
        c.spend(1, &mut bus);
        // The clock spent, and the suspend's 32 at once; then it ends.
        assert_eq!(c.cycles, 33);
        assert!(!c.io.suspended, "the suspend ran out");
    }

    #[test]
    fn spend_holds_an_untimed_suspend_without_hanging() {
        let mut ram: Vec<u8> = Vec::new();
        let mut dirty = false;
        let mut bus = Cx4Bus {
            rom: &[],
            ram: &mut ram,
            ram_dirty: &mut dirty,
        };
        let mut c = Hg51b::new();
        c.write_port(0x7F55, 0x00); // an untimed suspend: until $7F5D
        assert!(c.io.suspended);
        assert_eq!(c.io.suspend_remaining, 0);
        c.cycles = 0;
        c.spend(1, &mut bus); // must not hang
        assert_eq!(c.cycles, 1, "an untimed suspend does not run out");
        assert!(c.io.suspended, "it holds until $7F5D resumes the core");
        c.write_port(0x7F5D, 0x00);
        assert!(!c.io.suspended);
    }
}
