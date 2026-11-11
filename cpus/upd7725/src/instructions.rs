//! The four kinds of instruction.
//!
//! An OP does up to three things at once: an ALU operation on one
//! accumulator, a move of one register's value to another over the
//! internal bus, and a step of the two data pointers. An RT is an OP that
//! then returns. A JP jumps, calls, or jumps when a condition holds. An LD
//! puts a 16-bit value from the instruction into a register, the same
//! path an OP's move takes, which is why one function serves both.

use crate::registers::status::{HOST_REQUEST, KEPT_ON_LOAD};
use crate::{Upd7725, DP_MASK, PC_MASK, RP_MASK, SP_MASK};

/// DST 4, a move into DP: it takes the place of DP's own step.
const TO_RAM_POINTER: u32 = 4;
/// DST 5, a move into RP: it takes the place of RP's own step.
const TO_ROM_POINTER: u32 = 5;

/// The fields of an OP word; an RT has the same ones.
struct Operation {
    /// PSEL: the ALU's second input, data RAM at DP, the bus value, M or N.
    alu_input: u32,
    /// ALU: the function, 0 for none.
    alu_function: u32,
    /// ASL: the accumulator the ALU works on, 0 for A and 1 for B.
    accumulator: usize,
    /// DPL: how the low nibble of DP moves, 1 up, 2 down, 3 to zero.
    ram_pointer_step: u32,
    /// DPHM: bits flipped in the high nibble of DP.
    ram_pointer_flip: u16,
    /// RPDCR: RP steps down one.
    rom_pointer_down: bool,
    /// SRC: where the bus value comes from.
    source: u32,
    /// DST: where it goes.
    destination: u32,
}

impl Operation {
    fn decode(word: u32) -> Self {
        Self {
            alu_input: word >> 20 & 3,
            alu_function: word >> 16 & 0xF,
            accumulator: (word >> 15 & 1) as usize,
            ram_pointer_step: word >> 13 & 3,
            ram_pointer_flip: (word >> 9 & 0xF) as u16,
            rom_pointer_down: word & 1 << 8 != 0,
            source: word >> 4 & 0xF,
            destination: word & 0xF,
        }
    }
}

impl Upd7725 {
    /// OP. The bus value is read first, so the ALU and the move both see
    /// the registers as the instruction found them; the move lands after
    /// the ALU's result, and the pointers step last.
    pub(crate) fn operate(&mut self, word: u32) {
        let op = Operation::decode(word);
        let bus = self.read_source(op.source);
        if op.alu_function != 0 {
            self.run_alu(&op, bus);
        }
        self.store(op.destination, bus);

        if op.destination != TO_RAM_POINTER {
            let mut column = self.ram_pointer & 0x0F;
            match op.ram_pointer_step {
                1 => column = column.wrapping_add(1) & 0x0F,
                2 => column = column.wrapping_sub(1) & 0x0F,
                3 => column = 0,
                _ => {}
            }
            self.ram_pointer = (self.ram_pointer & 0xF0 | column) ^ op.ram_pointer_flip << 4;
        }
        if op.destination != TO_ROM_POINTER && op.rom_pointer_down {
            self.rom_pointer = self.rom_pointer.wrapping_sub(1) & RP_MASK;
        }
    }

    /// RT, after its OP: pop the return stack into PC.
    pub(crate) fn return_from_call(&mut self) {
        self.stack_pointer = self.stack_pointer.wrapping_sub(1) & SP_MASK;
        self.program_counter = self.call_stack[usize::from(self.stack_pointer)] & PC_MASK;
    }

    /// LD: bits 6-21 of the word into the register the low four bits name.
    pub(crate) fn load(&mut self, word: u32) {
        self.store(word & 0xF, (word >> 6) as u16);
    }

    /// The value an OP puts on the bus, by SRC.
    fn read_source(&mut self, source: u32) -> u16 {
        match source {
            0 => self.temporary_b,
            1 => self.accumulators[0],
            2 => self.accumulators[1],
            3 => self.temporary,
            4 => self.ram_pointer,
            5 => self.rom_pointer,
            // RO: the data ROM at RP.
            6 => self.data_rom[usize::from(self.rom_pointer & RP_MASK)],
            // SGN: the value a saturating sum on A clamps to, $7FFF when
            // the overflow that is outstanding left A negative, $8000 when
            // it left it positive.
            7 => 0x8000 - u16::from(self.flags[0].latched_sign),
            // DR, read by the program: the word is taken, so the S-CPU may
            // send the next.
            8 => {
                self.status |= HOST_REQUEST;
                self.data_port
            }
            // DRNF: DR without the request.
            9 => self.data_port,
            10 => self.status,
            // SIM and SIL: the serial input, high or low byte first; the
            // same word either way here.
            11 | 12 => self.serial_in,
            13 => self.multiplicand,
            14 => self.multiplier,
            _ => self.data_ram[usize::from(self.ram_pointer & DP_MASK)],
        }
    }

    /// The register a move or an LD writes, by DST.
    fn store(&mut self, destination: u32, value: u16) {
        match destination {
            1 => self.accumulators[0] = value,
            2 => self.accumulators[1] = value,
            3 => self.temporary = value,
            4 => self.ram_pointer = value & DP_MASK,
            5 => self.rom_pointer = value & RP_MASK,
            // DR, written by the program: a word is ready for the S-CPU.
            6 => {
                self.data_port = value;
                self.status |= HOST_REQUEST;
            }
            7 => self.status = self.status & KEPT_ON_LOAD | value & !KEPT_ON_LOAD,
            // SOL and SOM: the serial output, low or high byte first.
            8 | 9 => self.serial_out = value,
            10 => self.multiplicand = value,
            // KLR: K takes the value and L the data ROM at RP.
            11 => {
                self.multiplicand = value;
                self.multiplier = self.data_rom[usize::from(self.rom_pointer & RP_MASK)];
            }
            // KLM: L takes the value and K the data RAM at DP, with bit 6
            // of the address set.
            12 => {
                self.multiplier = value;
                self.multiplicand = self.data_ram[usize::from((self.ram_pointer | 0x40) & DP_MASK)];
            }
            13 => self.multiplier = value,
            14 => self.temporary_b = value,
            15 => self.data_ram[usize::from(self.ram_pointer & DP_MASK)] = value,
            // 0: no move.
            _ => {}
        }
    }

    /// An OP's ALU operation: the accumulator ASL names, with the second
    /// input PSEL names, into that accumulator and its flags. ADC and SBB
    /// add in, and SHL1 shifts in, the carry of the other accumulator.
    fn run_alu(&mut self, op: &Operation, bus: u16) {
        let target = op.accumulator;
        let carry_in = self.flags[target ^ 1].carry;
        let value = self.accumulators[target];
        let mut input = match op.alu_input {
            0 => self.data_ram[usize::from(self.ram_pointer & DP_MASK)],
            1 => bus,
            2 => self.product_high,
            _ => self.product_low,
        };

        let result = match op.alu_function {
            1 => value | input,
            2 => value & input,
            3 => value ^ input,
            4 => value.wrapping_sub(input),
            5 => value.wrapping_add(input),
            6 => value.wrapping_sub(input).wrapping_sub(u16::from(carry_in)),
            7 => value.wrapping_add(input).wrapping_add(u16::from(carry_in)),
            // DEC and INC: one is the input, for the flags too.
            8 => {
                input = 1;
                value.wrapping_sub(1)
            }
            9 => {
                input = 1;
                value.wrapping_add(1)
            }
            10 => !value,
            // SHR1: arithmetic, bit 15 stays.
            11 => value >> 1 | value & 0x8000,
            // SHL1: the carry in comes in at bit 0.
            12 => value << 1 | u16::from(carry_in),
            // SHL2 and SHL4 fill with ones.
            13 => value << 2 | 0b11,
            14 => value << 4 | 0b1111,
            // XCHG: the two bytes swap.
            _ => value.rotate_left(8),
        };

        let flags = &mut self.flags[target];
        flags.zero = result == 0;
        flags.sign = result & 0x8000 != 0;
        if !flags.latched_overflow {
            flags.latched_sign = flags.sign;
        }
        match op.alu_function {
            4..=9 => {
                let adds = op.alu_function & 1 != 0;
                let overflow =
                    (value ^ result) & (input ^ if adds { result } else { value }) & 0x8000 != 0;
                flags.latched_overflow = if overflow && flags.latched_overflow {
                    flags.latched_sign == flags.sign
                } else {
                    overflow || flags.latched_overflow
                };
                flags.overflow = overflow;
                // The carry out compares the result with the accumulator it
                // came from: a sum that wrapped comes out below it, a
                // difference that wrapped above it. That is the true carry,
                // except for ADC and SBB of $FFFF with a carry in, whose
                // result equals the accumulator and so leaves the carry
                // clear. bsnes-plus computes it so and this interpreter
                // keeps it; ares sets the carry there.
                flags.carry = if adds { result < value } else { value < result };
            }
            11 => {
                flags.carry = value & 1 != 0;
                flags.overflow = false;
                flags.latched_overflow = false;
            }
            12 => {
                flags.carry = value & 0x8000 != 0;
                flags.overflow = false;
                flags.latched_overflow = false;
            }
            _ => {
                flags.carry = false;
                flags.overflow = false;
                flags.latched_overflow = false;
            }
        }
        self.accumulators[target] = result;
    }

    /// JP. Every target is NA, the word's 11-bit address: the bank bits
    /// and the high and low forms of the jump and the call only matter
    /// with the uPD96050's 14-bit PC.
    pub(crate) fn jump(&mut self, word: u32) {
        let branch = word >> 13 & 0x1FF;
        let target = (word >> 2 & 0x7FF) as u16;
        match branch {
            // JMPSO: to the address in SO.
            0x000 => self.program_counter = self.serial_out & PC_MASK,
            // LJMP, HJMP.
            0x100 | 0x101 => self.program_counter = target,
            // LCALL, HCALL.
            0x140 | 0x141 => {
                self.call_stack[usize::from(self.stack_pointer & SP_MASK)] = self.program_counter;
                self.stack_pointer = self.stack_pointer.wrapping_add(1) & SP_MASK;
                self.program_counter = target;
            }
            _ => {
                if self.condition_holds(branch) {
                    self.program_counter = target;
                }
            }
        }
    }

    /// Whether a conditional jump is taken. Its code says what it tests:
    ///
    /// - `$080-$0AE`, even: a flag of an accumulator. Bits 5-3 name the
    ///   flag, C, Z, OV0, OV1, S0, S1, bit 2 the accumulator, and bit 1
    ///   whether the jump wants it set.
    /// - `$0B0-$0B3`: the low nibble of DP, against 0 (bit 1 clear) or $F
    ///   (bit 1 set), jumping when equal (bit 0 clear) or not (bit 0 set).
    /// - `$0BC`, `$0BE`: RQM, wanted clear or set.
    ///
    /// Every other code is taken as never true. That includes the four on
    /// the serial acknowledges, JNSIAK, JSIAK, JNSOAK and JSOAK
    /// (`$0B4-$0BA`): bsnes-plus does not decode them and this interpreter
    /// keeps that, where ares tests acknowledges that are never set, so
    /// that its JNSIAK and JNSOAK always jump. Nothing on the SNES drives
    /// the serial port.
    fn condition_holds(&self, branch: u32) -> bool {
        let wants_set = branch & 0b10 != 0;
        match branch {
            0x080..=0x0AF if branch & 1 == 0 => {
                let flags = self.flags[(branch >> 2 & 1) as usize];
                let flag = match branch >> 3 & 7 {
                    0 => flags.carry,
                    1 => flags.zero,
                    2 => flags.overflow,
                    3 => flags.latched_overflow,
                    4 => flags.sign,
                    _ => flags.latched_sign,
                };
                flag == wants_set
            }
            0x0B0..=0x0B3 => {
                let edge = if branch & 0b10 == 0 { 0x00 } else { 0x0F };
                (self.ram_pointer & 0x0F == edge) == (branch & 1 == 0)
            }
            0x0BC | 0x0BE => (self.status & HOST_REQUEST != 0) == wants_set,
            _ => false,
        }
    }
}
