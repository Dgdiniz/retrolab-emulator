//! Zilog Z80 CPU emulation.
//!
//! The full instruction set, undocumented opcodes included.
//!
//! Organized like the 68000 core:
//! - instructions in modules by category (`instructions/`)
//! - unit tests beside the instructions
//! - a disassembler (`disasm`)
//! - a decoder that covers every opcode
//! - run by `GenesisEmulator`

pub mod addressing;
pub mod decode;
pub mod disasm;
pub mod exceptions;
pub mod instructions;

use retro_core::cpu::{Addressable, Clockable, Resettable};

/// Z80 CPU state
pub struct Z80 {
    // Main registers (8-bit)
    pub a: u8, // Accumulator
    pub f: u8, // Flags: S Z Y H X P/V N C
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,

    // Alternate registers (shadow set)
    pub a_: u8,
    pub f_: u8,
    pub b_: u8,
    pub c_: u8,
    pub d_: u8,
    pub e_: u8,
    pub h_: u8,
    pub l_: u8,

    // Index registers (16-bit)
    pub ix: u16,
    pub iy: u16,

    // Special registers
    pub sp: u16, // Stack pointer
    pub pc: u16, // Program counter
    pub i: u8,   // Interrupt vector base
    pub r: u8,   // Memory refresh

    // Internal registers
    pub wz: u16, // Internal WZ (MEMPTR) register - affects undocumented flag bits 3/5

    // Interrupt state
    pub iff1: bool, // Interrupt flip-flop 1
    pub iff2: bool, // Interrupt flip-flop 2
    pub im: u8,     // Interrupt mode (0, 1, 2)
    pub ei: u8,     // EI just-executed flag (delays interrupt enable by one instruction)

    // Internal flags for test validation
    pub p: u8,  // P flag: set after LD A,I or LD A,R
    pub q: u8,  // Q flag: set when flags register is modified
    prev_q: u8, // Q from previous instruction (used by SCF/CCF undocumented behavior)

    // CPU state
    pub halted: bool,
    pub cycles_executed: u64,
    flags_modified: bool, // Internal: tracks if any set_flag_* was called during instruction
}

impl Z80 {
    /// Create a new Z80 CPU
    pub fn new() -> Self {
        Self {
            a: 0,
            f: 0,
            b: 0,
            c: 0,
            d: 0,
            e: 0,
            h: 0,
            l: 0,
            a_: 0,
            f_: 0,
            b_: 0,
            c_: 0,
            d_: 0,
            e_: 0,
            h_: 0,
            l_: 0,
            ix: 0,
            iy: 0,
            sp: 0xFFFF, // Start at top of memory
            pc: 0x0000,
            i: 0,
            r: 0,
            wz: 0,
            iff1: false,
            iff2: false,
            im: 0,
            ei: 0,
            p: 0,
            q: 0,
            prev_q: 0,
            halted: false,
            cycles_executed: 0,
            flags_modified: false,
        }
    }

    /// Execute one instruction
    /// Returns the number of T-states (cycles) consumed
    pub fn step(&mut self, bus: &mut dyn Addressable) -> u32 {
        if self.halted {
            // HALT still clears internal flags
            self.ei = 0;
            self.p = 0;
            self.q = 0;
            return 4; // HALT consumes 4 T-states per iteration
        }

        // Save Q from previous instruction (needed by SCF/CCF undocumented behavior)
        self.prev_q = self.q;

        // Clear internal flags at the start of each instruction.
        // Specific instructions will set them:
        //   ei = 1 by EI instruction
        //   p  = 1 by LD A,I / LD A,R
        //   q  = new F value by any instruction that modifies flags
        self.ei = 0;
        self.p = 0;
        self.q = 0;
        self.flags_modified = false;

        // Fetch instruction byte
        let opcode = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);

        // Increment refresh register (lower 7 bits)
        self.r = (self.r & 0x80) | ((self.r + 1) & 0x7F);

        // Decode and execute
        let result = self.decode_and_execute(bus, opcode);

        // If any set_flag_* was called during the instruction, set Q to the new F value.
        if self.flags_modified {
            self.q = self.f;
        }

        // Update cycle counter
        self.cycles_executed += result.cycles as u64;

        result.cycles
    }

    /// Decode and execute an instruction (main dispatcher)
    fn decode_and_execute(&mut self, bus: &mut dyn Addressable, opcode: u8) -> InstructionResult {
        use instructions::*;

        match opcode {
            // Prefixes (multi-byte instructions)
            0xCB => self.decode_cb_prefix(bus),
            0xDD => self.decode_dd_prefix(bus), // IX operations
            0xED => self.decode_ed_prefix(bus), // Extended instructions
            0xFD => self.decode_fd_prefix(bus), // IY operations

            // NOP
            0x00 => self.nop(),

            0x01 => self.ld_dd_nn(bus, Reg16::BC),
            0x02 => self.ld_bc_indirect_a(bus),
            0x03 => self.inc_rr(Reg16::BC), // INC BC
            0x04 => self.inc_r(Reg8::B),
            0x05 => self.dec_r(Reg8::B),
            0x06 => self.ld_r_n(bus, Reg8::B),
            0x07 => self.rlca(),               // RLCA
            0x08 => self.ex_af_af_prime(),     // EX AF, AF'
            0x09 => self.add_hl_rr(Reg16::BC), // ADD HL, BC
            0x0A => self.ld_a_bc_indirect(bus),
            0x0B => self.dec_rr(Reg16::BC), // DEC BC
            0x0C => self.inc_r(Reg8::C),
            0x0D => self.dec_r(Reg8::C),
            0x0E => self.ld_r_n(bus, Reg8::C),
            0x0F => self.rrca(), // RRCA

            0x10 => self.djnz(bus), // DJNZ e
            0x11 => self.ld_dd_nn(bus, Reg16::DE),
            0x12 => self.ld_de_indirect_a(bus),
            0x13 => self.inc_rr(Reg16::DE), // INC DE
            0x14 => self.inc_r(Reg8::D),
            0x15 => self.dec_r(Reg8::D),
            0x16 => self.ld_r_n(bus, Reg8::D),
            0x17 => self.rla(),                // RLA
            0x18 => self.jr_e(bus),            // JR e
            0x19 => self.add_hl_rr(Reg16::DE), // ADD HL, DE
            0x1A => self.ld_a_de_indirect(bus),
            0x1B => self.dec_rr(Reg16::DE), // DEC DE
            0x1C => self.inc_r(Reg8::E),
            0x1D => self.dec_r(Reg8::E),
            0x1E => self.ld_r_n(bus, Reg8::E),
            0x1F => self.rra(), // RRA

            0x20 => self.jr_nz_e(bus), // JR NZ, e
            0x21 => self.ld_dd_nn(bus, Reg16::HL),
            0x22 => self.ld_nn_indirect_hl(bus),
            0x23 => self.inc_rr(Reg16::HL), // INC HL
            0x24 => self.inc_r(Reg8::H),
            0x25 => self.dec_r(Reg8::H),
            0x26 => self.ld_r_n(bus, Reg8::H),
            0x27 => self.daa(),                // DAA
            0x28 => self.jr_z_e(bus),          // JR Z, e
            0x29 => self.add_hl_rr(Reg16::HL), // ADD HL, HL
            0x2A => self.ld_hl_nn_indirect(bus),
            0x2B => self.dec_rr(Reg16::HL), // DEC HL
            0x2C => self.inc_r(Reg8::L),
            0x2D => self.dec_r(Reg8::L),
            0x2E => self.ld_r_n(bus, Reg8::L),
            0x2F => self.cpl(), // CPL

            0x30 => self.jr_nc_e(bus), // JR NC, e
            0x31 => self.ld_dd_nn(bus, Reg16::SP),
            0x32 => self.ld_nn_indirect_a(bus),
            0x33 => self.inc_rr(Reg16::SP),     // INC SP
            0x34 => self.inc_hl_indirect(bus),  // INC (HL)
            0x35 => self.dec_hl_indirect(bus),  // DEC (HL)
            0x36 => self.ld_hl_indirect_n(bus), // LD (HL), n
            0x37 => self.scf(),                 // SCF
            0x38 => self.jr_c_e(bus),           // JR C, e
            0x39 => self.add_hl_rr(Reg16::SP),  // ADD HL, SP
            0x3A => self.ld_a_nn_indirect(bus),
            0x3B => self.dec_rr(Reg16::SP), // DEC SP
            0x3C => self.inc_r(Reg8::A),
            0x3D => self.dec_r(Reg8::A),
            0x3E => self.ld_r_n(bus, Reg8::A),
            0x3F => self.ccf(), // CCF

            0x40 => self.ld_r_r(Reg8::B, Reg8::B),
            0x41 => self.ld_r_r(Reg8::B, Reg8::C),
            0x42 => self.ld_r_r(Reg8::B, Reg8::D),
            0x43 => self.ld_r_r(Reg8::B, Reg8::E),
            0x44 => self.ld_r_r(Reg8::B, Reg8::H),
            0x45 => self.ld_r_r(Reg8::B, Reg8::L),
            0x46 => self.ld_r_hl_indirect(bus, Reg8::B), // LD B, (HL)
            0x47 => self.ld_r_r(Reg8::B, Reg8::A),
            0x48 => self.ld_r_r(Reg8::C, Reg8::B),
            0x49 => self.ld_r_r(Reg8::C, Reg8::C),
            0x4A => self.ld_r_r(Reg8::C, Reg8::D),
            0x4B => self.ld_r_r(Reg8::C, Reg8::E),
            0x4C => self.ld_r_r(Reg8::C, Reg8::H),
            0x4D => self.ld_r_r(Reg8::C, Reg8::L),
            0x4E => self.ld_r_hl_indirect(bus, Reg8::C), // LD C, (HL)
            0x4F => self.ld_r_r(Reg8::C, Reg8::A),

            0x50 => self.ld_r_r(Reg8::D, Reg8::B),
            0x51 => self.ld_r_r(Reg8::D, Reg8::C),
            0x52 => self.ld_r_r(Reg8::D, Reg8::D),
            0x53 => self.ld_r_r(Reg8::D, Reg8::E),
            0x54 => self.ld_r_r(Reg8::D, Reg8::H),
            0x55 => self.ld_r_r(Reg8::D, Reg8::L),
            0x56 => self.ld_r_hl_indirect(bus, Reg8::D), // LD D, (HL)
            0x57 => self.ld_r_r(Reg8::D, Reg8::A),
            0x58 => self.ld_r_r(Reg8::E, Reg8::B),
            0x59 => self.ld_r_r(Reg8::E, Reg8::C),
            0x5A => self.ld_r_r(Reg8::E, Reg8::D),
            0x5B => self.ld_r_r(Reg8::E, Reg8::E),
            0x5C => self.ld_r_r(Reg8::E, Reg8::H),
            0x5D => self.ld_r_r(Reg8::E, Reg8::L),
            0x5E => self.ld_r_hl_indirect(bus, Reg8::E), // LD E, (HL)
            0x5F => self.ld_r_r(Reg8::E, Reg8::A),

            0x60 => self.ld_r_r(Reg8::H, Reg8::B),
            0x61 => self.ld_r_r(Reg8::H, Reg8::C),
            0x62 => self.ld_r_r(Reg8::H, Reg8::D),
            0x63 => self.ld_r_r(Reg8::H, Reg8::E),
            0x64 => self.ld_r_r(Reg8::H, Reg8::H),
            0x65 => self.ld_r_r(Reg8::H, Reg8::L),
            0x66 => self.ld_r_hl_indirect(bus, Reg8::H), // LD H, (HL)
            0x67 => self.ld_r_r(Reg8::H, Reg8::A),
            0x68 => self.ld_r_r(Reg8::L, Reg8::B),
            0x69 => self.ld_r_r(Reg8::L, Reg8::C),
            0x6A => self.ld_r_r(Reg8::L, Reg8::D),
            0x6B => self.ld_r_r(Reg8::L, Reg8::E),
            0x6C => self.ld_r_r(Reg8::L, Reg8::H),
            0x6D => self.ld_r_r(Reg8::L, Reg8::L),
            0x6E => self.ld_r_hl_indirect(bus, Reg8::L), // LD L, (HL)
            0x6F => self.ld_r_r(Reg8::L, Reg8::A),

            0x70 => self.ld_hl_indirect_r(bus, Reg8::B), // LD (HL), B
            0x71 => self.ld_hl_indirect_r(bus, Reg8::C), // LD (HL), C
            0x72 => self.ld_hl_indirect_r(bus, Reg8::D), // LD (HL), D
            0x73 => self.ld_hl_indirect_r(bus, Reg8::E), // LD (HL), E
            0x74 => self.ld_hl_indirect_r(bus, Reg8::H), // LD (HL), H
            0x75 => self.ld_hl_indirect_r(bus, Reg8::L), // LD (HL), L
            0x76 => self.halt(),                         // HALT
            0x77 => self.ld_hl_indirect_r(bus, Reg8::A), // LD (HL), A
            0x78 => self.ld_r_r(Reg8::A, Reg8::B),
            0x79 => self.ld_r_r(Reg8::A, Reg8::C),
            0x7A => self.ld_r_r(Reg8::A, Reg8::D),
            0x7B => self.ld_r_r(Reg8::A, Reg8::E),
            0x7C => self.ld_r_r(Reg8::A, Reg8::H),
            0x7D => self.ld_r_r(Reg8::A, Reg8::L),
            0x7E => self.ld_r_hl_indirect(bus, Reg8::A), // LD A, (HL)
            0x7F => self.ld_r_r(Reg8::A, Reg8::A),

            0x80 => self.add_a_r(Reg8::B),
            0x81 => self.add_a_r(Reg8::C),
            0x82 => self.add_a_r(Reg8::D),
            0x83 => self.add_a_r(Reg8::E),
            0x84 => self.add_a_r(Reg8::H),
            0x85 => self.add_a_r(Reg8::L),
            0x86 => self.add_a_hl(bus), // ADD A, (HL)
            0x87 => self.add_a_r(Reg8::A),
            0x88 => self.adc_a_r(Reg8::B), // ADC A, B
            0x89 => self.adc_a_r(Reg8::C), // ADC A, C
            0x8A => self.adc_a_r(Reg8::D), // ADC A, D
            0x8B => self.adc_a_r(Reg8::E), // ADC A, E
            0x8C => self.adc_a_r(Reg8::H), // ADC A, H
            0x8D => self.adc_a_r(Reg8::L), // ADC A, L
            0x8E => self.adc_a_hl(bus),    // ADC A, (HL)
            0x8F => self.adc_a_r(Reg8::A), // ADC A, A

            0x90 => self.sub_r(Reg8::B),
            0x91 => self.sub_r(Reg8::C),
            0x92 => self.sub_r(Reg8::D),
            0x93 => self.sub_r(Reg8::E),
            0x94 => self.sub_r(Reg8::H),
            0x95 => self.sub_r(Reg8::L),
            0x96 => self.sub_hl(bus), // SUB (HL)
            0x97 => self.sub_r(Reg8::A),
            0x98 => self.sbc_a_r(Reg8::B), // SBC A, B
            0x99 => self.sbc_a_r(Reg8::C), // SBC A, C
            0x9A => self.sbc_a_r(Reg8::D), // SBC A, D
            0x9B => self.sbc_a_r(Reg8::E), // SBC A, E
            0x9C => self.sbc_a_r(Reg8::H), // SBC A, H
            0x9D => self.sbc_a_r(Reg8::L), // SBC A, L
            0x9E => self.sbc_a_hl(bus),    // SBC A, (HL)
            0x9F => self.sbc_a_r(Reg8::A), // SBC A, A

            0xA0 => self.and_r(Reg8::B),
            0xA1 => self.and_r(Reg8::C),
            0xA2 => self.and_r(Reg8::D),
            0xA3 => self.and_r(Reg8::E),
            0xA4 => self.and_r(Reg8::H),
            0xA5 => self.and_r(Reg8::L),
            0xA6 => self.and_hl(bus), // AND (HL)
            0xA7 => self.and_r(Reg8::A),
            0xA8 => self.xor_r(Reg8::B),
            0xA9 => self.xor_r(Reg8::C),
            0xAA => self.xor_r(Reg8::D),
            0xAB => self.xor_r(Reg8::E),
            0xAC => self.xor_r(Reg8::H),
            0xAD => self.xor_r(Reg8::L),
            0xAE => self.xor_hl(bus), // XOR (HL)
            0xAF => self.xor_r(Reg8::A),

            0xB0 => self.or_r(Reg8::B),
            0xB1 => self.or_r(Reg8::C),
            0xB2 => self.or_r(Reg8::D),
            0xB3 => self.or_r(Reg8::E),
            0xB4 => self.or_r(Reg8::H),
            0xB5 => self.or_r(Reg8::L),
            0xB6 => self.or_hl(bus), // OR (HL)
            0xB7 => self.or_r(Reg8::A),
            0xB8 => self.cp_r(Reg8::B),
            0xB9 => self.cp_r(Reg8::C),
            0xBA => self.cp_r(Reg8::D),
            0xBB => self.cp_r(Reg8::E),
            0xBC => self.cp_r(Reg8::H),
            0xBD => self.cp_r(Reg8::L),
            0xBE => self.cp_hl(bus), // CP (HL)
            0xBF => self.cp_r(Reg8::A),

            0xC0 => self.ret_cc(bus, !self.flag_z()), // RET NZ
            0xC1 => self.pop_qq(bus, Reg16::BC),      // POP BC
            0xC2 => self.jp_cc_nn(bus, !self.flag_z()), // JP NZ, nn
            0xC3 => self.jp_nn(bus),                  // JP nn
            0xC4 => self.call_cc_nn(bus, !self.flag_z()), // CALL NZ, nn
            0xC5 => self.push_qq(bus, Reg16::BC),     // PUSH BC
            0xC6 => self.add_a_n(bus),
            0xC7 => self.rst(bus, 0x00),                 // RST 00h
            0xC8 => self.ret_cc(bus, self.flag_z()),     // RET Z
            0xC9 => self.ret(bus),                       // RET
            0xCA => self.jp_cc_nn(bus, self.flag_z()),   // JP Z, nn
            0xCC => self.call_cc_nn(bus, self.flag_z()), // CALL Z, nn
            0xCD => self.call_nn(bus),                   // CALL nn
            0xCE => self.adc_a_n(bus),                   // ADC A, n
            0xCF => self.rst(bus, 0x08),                 // RST 08h

            0xD0 => self.ret_cc(bus, !self.flag_c()), // RET NC
            0xD1 => self.pop_qq(bus, Reg16::DE),      // POP DE
            0xD2 => self.jp_cc_nn(bus, !self.flag_c()), // JP NC, nn
            0xD3 => self.out_n_a(bus),                // OUT (n), A
            0xD4 => self.call_cc_nn(bus, !self.flag_c()), // CALL NC, nn
            0xD5 => self.push_qq(bus, Reg16::DE),     // PUSH DE
            0xD6 => self.sub_n(bus),
            0xD7 => self.rst(bus, 0x10),                 // RST 10h
            0xD8 => self.ret_cc(bus, self.flag_c()),     // RET C
            0xD9 => self.exx(),                          // EXX
            0xDA => self.jp_cc_nn(bus, self.flag_c()),   // JP C, nn
            0xDB => self.in_a_n(bus),                    // IN A, (n)
            0xDC => self.call_cc_nn(bus, self.flag_c()), // CALL C, nn
            0xDE => self.sbc_a_n(bus),                   // SBC A, n
            0xDF => self.rst(bus, 0x18),                 // RST 18h

            0xE0 => self.ret_cc(bus, !self.flag_pv()), // RET PO
            0xE1 => self.pop_qq(bus, Reg16::HL),       // POP HL
            0xE2 => self.jp_cc_nn(bus, !self.flag_pv()), // JP PO, nn
            0xE3 => self.ex_sp_hl(bus),                // EX (SP), HL
            0xE4 => self.call_cc_nn(bus, !self.flag_pv()), // CALL PO, nn
            0xE5 => self.push_qq(bus, Reg16::HL),      // PUSH HL
            0xE6 => self.and_n(bus),
            0xE7 => self.rst(bus, 0x20),                  // RST 20h
            0xE8 => self.ret_cc(bus, self.flag_pv()),     // RET PE
            0xE9 => self.jp_hl(),                         // JP (HL)
            0xEA => self.jp_cc_nn(bus, self.flag_pv()),   // JP PE, nn
            0xEB => self.ex_de_hl(),                      // EX DE, HL
            0xEC => self.call_cc_nn(bus, self.flag_pv()), // CALL PE, nn
            0xEE => self.xor_n(bus),
            0xEF => self.rst(bus, 0x28), // RST 28h

            0xF0 => self.ret_cc(bus, !self.flag_s()), // RET P
            0xF1 => self.pop_qq(bus, Reg16::AF),      // POP AF
            0xF2 => self.jp_cc_nn(bus, !self.flag_s()), // JP P, nn
            0xF3 => self.di(),                        // DI
            0xF4 => self.call_cc_nn(bus, !self.flag_s()), // CALL P, nn
            0xF5 => self.push_qq(bus, Reg16::AF),     // PUSH AF
            0xF6 => self.or_n(bus),
            0xF7 => self.rst(bus, 0x30),                 // RST 30h
            0xF8 => self.ret_cc(bus, self.flag_s()),     // RET M
            0xF9 => self.ld_sp_hl(),                     // LD SP, HL
            0xFA => self.jp_cc_nn(bus, self.flag_s()),   // JP M, nn
            0xFB => self.ei(),                           // EI
            0xFC => self.call_cc_nn(bus, self.flag_s()), // CALL M, nn
            0xFE => self.cp_n(bus),
            0xFF => self.rst(bus, 0x38), // RST 38h
        }
    }

    /// Decode CB prefix (bit operations)
    fn decode_cb_prefix(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let opcode = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);

        // CB prefix causes a second R increment
        self.r = (self.r & 0x80) | ((self.r + 1) & 0x7F);

        // CB prefix: Rotate/Shift and Bit Operations
        // Bits 0-2: register index (B/C/D/E/H/L/(HL)/A)
        // Bits 3-5: operation
        // Bits 6-7: category

        let reg_index = opcode & 0x07;

        // Special case: index 6 means (HL), not a register
        if reg_index == 6 {
            match opcode {
                // Rotate/Shift (HL)
                0x06 => self.rlc_hl(bus), // RLC (HL)
                0x0E => self.rrc_hl(bus), // RRC (HL)
                0x16 => self.rl_hl(bus),  // RL (HL)
                0x1E => self.rr_hl(bus),  // RR (HL)
                0x26 => self.sla_hl(bus), // SLA (HL)
                0x2E => self.sra_hl(bus), // SRA (HL)
                0x36 => self.sll_hl(bus), // SLL (HL)
                0x3E => self.srl_hl(bus), // SRL (HL)

                // BIT b, (HL) (0x46, 0x4E, 0x56, 0x5E, 0x66, 0x6E, 0x76, 0x7E)
                0x46 | 0x4E | 0x56 | 0x5E | 0x66 | 0x6E | 0x76 | 0x7E => {
                    let bit = (opcode >> 3) & 0x07;
                    self.bit_b_hl(bit, bus)
                }

                // RES b, (HL) (0x86, 0x8E, 0x96, 0x9E, 0xA6, 0xAE, 0xB6, 0xBE)
                0x86 | 0x8E | 0x96 | 0x9E | 0xA6 | 0xAE | 0xB6 | 0xBE => {
                    let bit = (opcode >> 3) & 0x07;
                    self.res_b_hl(bit, bus)
                }

                // SET b, (HL) (0xC6, 0xCE, 0xD6, 0xDE, 0xE6, 0xEE, 0xF6, 0xFE)
                0xC6 | 0xCE | 0xD6 | 0xDE | 0xE6 | 0xEE | 0xF6 | 0xFE => {
                    let bit = (opcode >> 3) & 0x07;
                    self.set_b_hl(bit, bus)
                }

                _ => self.unimplemented_prefix(0xCB, opcode),
            }
        } else {
            let reg = self.decode_reg8_index(reg_index);

            match opcode {
                // RLC r (0x00-0x07) - Rotate Left Circular
                0x00..=0x07 => self.rlc_r(reg),

                // RRC r (0x08-0x0F) - Rotate Right Circular
                0x08..=0x0F => self.rrc_r(reg),

                // RL r (0x10-0x17) - Rotate Left through Carry
                0x10..=0x17 => self.rl_r(reg),

                // RR r (0x18-0x1F) - Rotate Right through Carry
                0x18..=0x1F => self.rr_r(reg),

                // SLA r (0x20-0x27) - Shift Left Arithmetic
                0x20..=0x27 => self.sla_r(reg),

                // SRA r (0x28-0x2F) - Shift Right Arithmetic
                0x28..=0x2F => self.sra_r(reg),

                // SLL r (0x30-0x37) - Shift Left Logical (undocumented)
                0x30..=0x37 => self.sll_r(reg),

                // SRL r (0x38-0x3F) - Shift Right Logical
                0x38..=0x3F => self.srl_r(reg),

                // BIT b, r (0x40-0x7F) - Test bit
                0x40..=0x7F => {
                    let bit = (opcode >> 3) & 0x07; // Bits 3-5 = bit number
                    self.bit_b_r(bit, reg)
                }

                // RES b, r (0x80-0xBF) - Reset bit
                0x80..=0xBF => {
                    let bit = (opcode >> 3) & 0x07;
                    self.res_b_r(bit, reg)
                }

                // SET b, r (0xC0-0xFF) - Set bit
                0xC0..=0xFF => {
                    let bit = (opcode >> 3) & 0x07;
                    self.set_b_r(bit, reg)
                }
            }
        }
    }

    /// Decode DD prefix (IX operations)
    fn decode_dd_prefix(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let opcode = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);

        // DD prefix causes a second R increment
        self.r = (self.r & 0x80) | ((self.r + 1) & 0x7F);

        match opcode {
            // Index operations (IX) - Basic
            0x09 => self.add_idx_rr(false, 0),      // ADD IX, BC
            0x19 => self.add_idx_rr(false, 1),      // ADD IX, DE
            0x21 => self.ld_idx_nn(bus, false),     // LD IX, nn
            0x22 => self.ld_nn_idx(bus, false),     // LD (nn), IX
            0x23 => self.inc_idx(false),            // INC IX
            0x29 => self.add_idx_rr(false, 2),      // ADD IX, IX
            0x2A => self.ld_idx_nn_ind(bus, false), // LD IX, (nn)
            0x2B => self.dec_idx(false),            // DEC IX
            0x39 => self.add_idx_rr(false, 3),      // ADD IX, SP
            0xE1 => self.pop_idx(bus, false),       // POP IX
            0xE3 => self.ex_sp_idx(bus, false),     // EX (SP), IX
            0xE5 => self.push_idx(bus, false),      // PUSH IX
            0xE9 => self.jp_ix(),                   // JP (IX)
            0xF9 => self.ld_sp_idx(false),          // LD SP, IX

            // Undocumented IXH/IXL operations
            0x24 => self.inc_idx_high(false),            // INC IXH
            0x25 => self.dec_idx_high(false),            // DEC IXH
            0x26 => self.ld_idx_high_n(bus, false),      // LD IXH, n
            0x2C => self.inc_idx_low(false),             // INC IXL
            0x2D => self.dec_idx_low(false),             // DEC IXL
            0x2E => self.ld_idx_low_n(bus, false),       // LD IXL, n
            0x44 => self.ld_r_idx_half(false, true, 0),  // LD B, IXH
            0x45 => self.ld_r_idx_half(false, false, 0), // LD B, IXL
            0x4C => self.ld_r_idx_half(false, true, 1),  // LD C, IXH
            0x4D => self.ld_r_idx_half(false, false, 1), // LD C, IXL
            0x54 => self.ld_r_idx_half(false, true, 2),  // LD D, IXH
            0x55 => self.ld_r_idx_half(false, false, 2), // LD D, IXL
            0x5C => self.ld_r_idx_half(false, true, 3),  // LD E, IXH
            0x5D => self.ld_r_idx_half(false, false, 3), // LD E, IXL
            0x60 => self.ld_idx_high_r(false, 0),        // LD IXH, B
            0x61 => self.ld_idx_high_r(false, 1),        // LD IXH, C
            0x62 => self.ld_idx_high_r(false, 2),        // LD IXH, D
            0x63 => self.ld_idx_high_r(false, 3),        // LD IXH, E
            0x64 => self.ld_idx_high_r(false, 4),        // LD IXH, IXH
            0x65 => self.ld_idx_high_r(false, 5),        // LD IXH, IXL
            0x67 => self.ld_idx_high_r(false, 7),        // LD IXH, A
            0x68 => self.ld_idx_low_r(false, 0),         // LD IXL, B
            0x69 => self.ld_idx_low_r(false, 1),         // LD IXL, C
            0x6A => self.ld_idx_low_r(false, 2),         // LD IXL, D
            0x6B => self.ld_idx_low_r(false, 3),         // LD IXL, E
            0x6C => self.ld_idx_low_r(false, 4),         // LD IXL, IXH
            0x6D => self.ld_idx_low_r(false, 5),         // LD IXL, IXL
            0x6F => self.ld_idx_low_r(false, 7),         // LD IXL, A
            0x7C => self.ld_r_idx_half(false, true, 7),  // LD A, IXH
            0x7D => self.ld_r_idx_half(false, false, 7), // LD A, IXL
            0x84 => self.alu_idx_half(false, true, 0),   // ADD A, IXH
            0x85 => self.alu_idx_half(false, false, 0),  // ADD A, IXL
            0x8C => self.alu_idx_half(false, true, 1),   // ADC A, IXH
            0x8D => self.alu_idx_half(false, false, 1),  // ADC A, IXL
            0x94 => self.alu_idx_half(false, true, 2),   // SUB IXH
            0x95 => self.alu_idx_half(false, false, 2),  // SUB IXL
            0x9C => self.alu_idx_half(false, true, 3),   // SBC A, IXH
            0x9D => self.alu_idx_half(false, false, 3),  // SBC A, IXL
            0xA4 => self.alu_idx_half(false, true, 4),   // AND IXH
            0xA5 => self.alu_idx_half(false, false, 4),  // AND IXL
            0xAC => self.alu_idx_half(false, true, 5),   // XOR IXH
            0xAD => self.alu_idx_half(false, false, 5),  // XOR IXL
            0xB4 => self.alu_idx_half(false, true, 6),   // OR IXH
            0xB5 => self.alu_idx_half(false, false, 6),  // OR IXL
            0xBC => self.alu_idx_half(false, true, 7),   // CP IXH
            0xBD => self.alu_idx_half(false, false, 7),  // CP IXL

            // Index operations with displacement (IX+d)
            0x34 => self.inc_idx_d(bus, false),     // INC (IX+d)
            0x35 => self.dec_idx_d(bus, false),     // DEC (IX+d)
            0x36 => self.ld_idx_d_n(bus, false),    // LD (IX+d), n
            0x46 => self.ld_r_idx_d(bus, false, 0), // LD B, (IX+d)
            0x4E => self.ld_r_idx_d(bus, false, 1), // LD C, (IX+d)
            0x56 => self.ld_r_idx_d(bus, false, 2), // LD D, (IX+d)
            0x5E => self.ld_r_idx_d(bus, false, 3), // LD E, (IX+d)
            0x66 => self.ld_r_idx_d(bus, false, 4), // LD H, (IX+d)
            0x6E => self.ld_r_idx_d(bus, false, 5), // LD L, (IX+d)
            0x70 => self.ld_idx_d_r(bus, false, 0), // LD (IX+d), B
            0x71 => self.ld_idx_d_r(bus, false, 1), // LD (IX+d), C
            0x72 => self.ld_idx_d_r(bus, false, 2), // LD (IX+d), D
            0x73 => self.ld_idx_d_r(bus, false, 3), // LD (IX+d), E
            0x74 => self.ld_idx_d_r(bus, false, 4), // LD (IX+d), H
            0x75 => self.ld_idx_d_r(bus, false, 5), // LD (IX+d), L
            0x77 => self.ld_idx_d_r(bus, false, 7), // LD (IX+d), A
            0x7E => self.ld_r_idx_d(bus, false, 7), // LD A, (IX+d)
            0x86 => self.add_a_idx_d(bus, false),   // ADD A, (IX+d)
            0x8E => self.adc_a_idx_d(bus, false),   // ADC A, (IX+d)
            0x96 => self.sub_idx_d(bus, false),     // SUB (IX+d)
            0x9E => self.sbc_a_idx_d(bus, false),   // SBC A, (IX+d)
            0xA6 => self.and_idx_d(bus, false),     // AND (IX+d)
            0xAE => self.xor_idx_d(bus, false),     // XOR (IX+d)
            0xB6 => self.or_idx_d(bus, false),      // OR (IX+d)
            0xBE => self.cp_idx_d(bus, false),      // CP (IX+d)

            // CB prefix - bit operations with displacement
            0xCB => self.decode_cb_idx_prefix(bus, false),

            // Default: pass-through to base instruction with 4-cycle DD prefix overhead
            _ => {
                let result = self.decode_and_execute(bus, opcode);
                InstructionResult::new(result.cycles + 4)
            }
        }
    }

    /// Unified DD CB / FD CB prefix decoder - handles all 256 opcodes
    /// Format: DD/FD CB d oo (d=displacement, oo=opcode)
    /// Undocumented: opcodes with reg != 6 also store result to that register
    fn decode_cb_idx_prefix(
        &mut self,
        bus: &mut dyn Addressable,
        is_iy: bool,
    ) -> InstructionResult {
        let displacement = bus.read(self.pc as u32) as i8;
        self.pc = self.pc.wrapping_add(1);
        let opcode = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);

        let addr = self.calc_indexed_addr(is_iy, displacement);
        self.wz = addr;
        let value = bus.read(addr as u32);

        let op = (opcode >> 6) & 3; // 0=rot/shift, 1=BIT, 2=RES, 3=SET
        let bit = (opcode >> 3) & 7; // bit number (for BIT/RES/SET) or shift type
        let reg = opcode & 7; // target register (6 = no register store)

        match op {
            0 => {
                // Rotate/Shift operations
                let carry_in_bit = if self.flag_c() { 1u8 } else { 0u8 };
                let (result, carry) = match bit {
                    0 => (value.rotate_left(1), value & 0x80 != 0), // RLC
                    1 => (value.rotate_right(1), value & 0x01 != 0), // RRC
                    2 => ((value << 1) | carry_in_bit, value & 0x80 != 0), // RL
                    3 => ((value >> 1) | (carry_in_bit << 7), value & 0x01 != 0), // RR
                    4 => (value << 1, value & 0x80 != 0),           // SLA
                    5 => ((value >> 1) | (value & 0x80), value & 0x01 != 0), // SRA
                    6 => ((value << 1) | 0x01, value & 0x80 != 0),  // SLL (undoc)
                    7 => (value >> 1, value & 0x01 != 0),           // SRL
                    _ => unreachable!(),
                };
                bus.write(addr as u32, result);
                self.set_flag_s(result & 0x80 != 0);
                self.set_flag_z(result == 0);
                self.set_flag_y(result & 0x20 != 0);
                self.set_flag_h(false);
                self.set_flag_x(result & 0x08 != 0);
                self.set_flag_pv(result.count_ones() % 2 == 0);
                self.set_flag_n(false);
                self.set_flag_c(carry);
                if reg != 6 {
                    self.write_reg8_by_code(reg, result);
                }
                InstructionResult::new(23)
            }
            1 => {
                // BIT b, (IX/IY+d) - all variants behave the same (no register store)
                let bit_value = value & (1 << bit);
                let addr_high = (addr >> 8) as u8;
                self.set_flag_z(bit_value == 0);
                self.set_flag_h(true);
                self.set_flag_n(false);
                self.set_flag_s(bit == 7 && bit_value != 0);
                self.set_flag_pv(bit_value == 0);
                self.set_flag_y(addr_high & 0x20 != 0);
                self.set_flag_x(addr_high & 0x08 != 0);
                InstructionResult::new(20)
            }
            2 => {
                // RES b, (IX/IY+d) - optionally store to register
                let result = value & !(1 << bit);
                bus.write(addr as u32, result);
                if reg != 6 {
                    self.write_reg8_by_code(reg, result);
                }
                InstructionResult::new(23)
            }
            3 => {
                // SET b, (IX/IY+d) - optionally store to register
                let result = value | (1 << bit);
                bus.write(addr as u32, result);
                if reg != 6 {
                    self.write_reg8_by_code(reg, result);
                }
                InstructionResult::new(23)
            }
            _ => unreachable!(),
        }
    }

    /// Decode ED prefix (extended instructions)
    fn decode_ed_prefix(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let opcode = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);

        // ED prefix causes a second R increment
        self.r = (self.r & 0x80) | ((self.r + 1) & 0x7F);

        match opcode {
            // I/O operations
            0x40 => self.in_r_c(bus, Reg8::B),  // IN B, (C)
            0x41 => self.out_c_r(bus, Reg8::B), // OUT (C), B
            0x48 => self.in_r_c(bus, Reg8::C),  // IN C, (C)
            0x49 => self.out_c_r(bus, Reg8::C), // OUT (C), C
            0x50 => self.in_r_c(bus, Reg8::D),  // IN D, (C)
            0x51 => self.out_c_r(bus, Reg8::D), // OUT (C), D
            0x58 => self.in_r_c(bus, Reg8::E),  // IN E, (C)
            0x59 => self.out_c_r(bus, Reg8::E), // OUT (C), E
            0x60 => self.in_r_c(bus, Reg8::H),  // IN H, (C)
            0x61 => self.out_c_r(bus, Reg8::H), // OUT (C), H
            0x68 => self.in_r_c(bus, Reg8::L),  // IN L, (C)
            0x69 => self.out_c_r(bus, Reg8::L), // OUT (C), L
            0x70 => self.in_c_flags_only(bus),  // IN (C) - flags only, undocumented
            0x71 => self.out_c_zero(bus),       // OUT (C), 0 - undocumented
            0x78 => self.in_r_c(bus, Reg8::A),  // IN A, (C)
            0x79 => self.out_c_r(bus, Reg8::A), // OUT (C), A

            // 16-bit arithmetic and loads, NEG, RETN/RETI, IM, LD I/R, RRD/RLD
            0x42 => self.sbc_hl_rr(Reg16::BC), // SBC HL, BC
            0x43 => instructions::extended::ld_nn_bc(self, bus), // LD (nn), BC
            0x44 => self.neg(),                // NEG
            0x45 => instructions::extended::retn(self, bus), // RETN
            0x46 => instructions::extended::im_0(self), // IM 0
            0x47 => instructions::extended::ld_i_a(self), // LD I, A
            0x4A => self.adc_hl_rr(Reg16::BC), // ADC HL, BC
            0x4B => instructions::extended::ld_bc_nn(self, bus), // LD BC, (nn)
            0x4D => instructions::extended::reti(self, bus), // RETI
            0x4F => instructions::extended::ld_r_a(self), // LD R, A
            0x4C | 0x54 | 0x5C | 0x64 | 0x6C | 0x74 | 0x7C => self.neg(), // NEG duplicates
            0x4E | 0x66 | 0x6E => instructions::extended::im_0(self), // IM 0 duplicates

            0x52 => self.sbc_hl_rr(Reg16::DE), // SBC HL, DE
            0x53 => instructions::extended::ld_nn_de(self, bus), // LD (nn), DE
            0x55 | 0x5D | 0x65 | 0x6D | 0x75 | 0x7D => instructions::extended::retn(self, bus), // RETN duplicates
            0x56 => instructions::extended::im_1(self), // IM 1
            0x57 => instructions::extended::ld_a_i(self), // LD A, I
            0x5A => self.adc_hl_rr(Reg16::DE),          // ADC HL, DE
            0x5B => instructions::extended::ld_de_nn(self, bus), // LD DE, (nn)
            0x5E => instructions::extended::im_2(self), // IM 2
            0x5F => instructions::extended::ld_a_r(self), // LD A, R

            0x62 => self.sbc_hl_rr(Reg16::HL), // SBC HL, HL
            0x63 => instructions::extended::ld_nn_hl_ed(self, bus), // LD (nn), HL (ED version)
            0x67 => self.rrd(bus),             // RRD
            0x6A => self.adc_hl_rr(Reg16::HL), // ADC HL, HL
            0x6B => instructions::extended::ld_hl_nn_ed(self, bus), // LD HL, (nn) (ED version)
            0x6F => self.rld(bus),             // RLD

            0x72 => self.sbc_hl_rr(Reg16::SP), // SBC HL, SP
            0x73 => instructions::extended::ld_nn_sp(self, bus), // LD (nn), SP
            0x76 => instructions::extended::im_1(self), // IM 1 duplicate
            0x7A => self.adc_hl_rr(Reg16::SP), // ADC HL, SP
            0x7B => instructions::extended::ld_sp_nn(self, bus), // LD SP, (nn)
            0x7E => instructions::extended::im_2(self), // IM 2 duplicate

            // Block transfers, searches and I/O (LDI, CPI, INI, OUTI and their repeats)
            0xA0 => instructions::extended::ldi(self, bus), // LDI
            0xA1 => instructions::extended::cpi(self, bus), // CPI
            0xA2 => instructions::extended::ini(self, bus), // INI
            0xA3 => instructions::extended::outi(self, bus), // OUTI
            0xA8 => instructions::extended::ldd(self, bus), // LDD
            0xA9 => instructions::extended::cpd(self, bus), // CPD
            0xAA => instructions::extended::ind(self, bus), // IND
            0xAB => instructions::extended::outd(self, bus), // OUTD

            0xB0 => instructions::extended::ldir(self, bus), // LDIR
            0xB1 => instructions::extended::cpir(self, bus), // CPIR
            0xB2 => instructions::extended::inir(self, bus), // INIR
            0xB3 => instructions::extended::otir(self, bus), // OTIR
            0xB8 => instructions::extended::lddr(self, bus), // LDDR
            0xB9 => instructions::extended::cpdr(self, bus), // CPDR
            0xBA => instructions::extended::indr(self, bus), // INDR
            0xBB => instructions::extended::otdr(self, bus), // OTDR

            // All undefined ED opcodes act as 8-cycle NOPs
            _ => instructions::extended::ed_nop(self),
        }
    }

    /// Decode FD prefix (IY operations)
    fn decode_fd_prefix(&mut self, bus: &mut dyn Addressable) -> InstructionResult {
        let opcode = bus.read(self.pc as u32);
        self.pc = self.pc.wrapping_add(1);

        // FD prefix causes a second R increment
        self.r = (self.r & 0x80) | ((self.r + 1) & 0x7F);

        match opcode {
            // Index operations (IY) - Basic
            0x09 => self.add_idx_rr(true, 0),      // ADD IY, BC
            0x19 => self.add_idx_rr(true, 1),      // ADD IY, DE
            0x21 => self.ld_idx_nn(bus, true),     // LD IY, nn
            0x22 => self.ld_nn_idx(bus, true),     // LD (nn), IY
            0x23 => self.inc_idx(true),            // INC IY
            0x29 => self.add_idx_rr(true, 2),      // ADD IY, IY
            0x2A => self.ld_idx_nn_ind(bus, true), // LD IY, (nn)
            0x2B => self.dec_idx(true),            // DEC IY
            0x39 => self.add_idx_rr(true, 3),      // ADD IY, SP
            0xE1 => self.pop_idx(bus, true),       // POP IY
            0xE3 => self.ex_sp_idx(bus, true),     // EX (SP), IY
            0xE5 => self.push_idx(bus, true),      // PUSH IY
            0xE9 => self.jp_iy(),                  // JP (IY)
            0xF9 => self.ld_sp_idx(true),          // LD SP, IY

            // Undocumented IYH/IYL operations
            0x24 => self.inc_idx_high(true),            // INC IYH
            0x25 => self.dec_idx_high(true),            // DEC IYH
            0x26 => self.ld_idx_high_n(bus, true),      // LD IYH, n
            0x2C => self.inc_idx_low(true),             // INC IYL
            0x2D => self.dec_idx_low(true),             // DEC IYL
            0x2E => self.ld_idx_low_n(bus, true),       // LD IYL, n
            0x44 => self.ld_r_idx_half(true, true, 0),  // LD B, IYH
            0x45 => self.ld_r_idx_half(true, false, 0), // LD B, IYL
            0x4C => self.ld_r_idx_half(true, true, 1),  // LD C, IYH
            0x4D => self.ld_r_idx_half(true, false, 1), // LD C, IYL
            0x54 => self.ld_r_idx_half(true, true, 2),  // LD D, IYH
            0x55 => self.ld_r_idx_half(true, false, 2), // LD D, IYL
            0x5C => self.ld_r_idx_half(true, true, 3),  // LD E, IYH
            0x5D => self.ld_r_idx_half(true, false, 3), // LD E, IYL
            0x60 => self.ld_idx_high_r(true, 0),        // LD IYH, B
            0x61 => self.ld_idx_high_r(true, 1),        // LD IYH, C
            0x62 => self.ld_idx_high_r(true, 2),        // LD IYH, D
            0x63 => self.ld_idx_high_r(true, 3),        // LD IYH, E
            0x64 => self.ld_idx_high_r(true, 4),        // LD IYH, IYH
            0x65 => self.ld_idx_high_r(true, 5),        // LD IYH, IYL
            0x67 => self.ld_idx_high_r(true, 7),        // LD IYH, A
            0x68 => self.ld_idx_low_r(true, 0),         // LD IYL, B
            0x69 => self.ld_idx_low_r(true, 1),         // LD IYL, C
            0x6A => self.ld_idx_low_r(true, 2),         // LD IYL, D
            0x6B => self.ld_idx_low_r(true, 3),         // LD IYL, E
            0x6C => self.ld_idx_low_r(true, 4),         // LD IYL, IYH
            0x6D => self.ld_idx_low_r(true, 5),         // LD IYL, IYL
            0x6F => self.ld_idx_low_r(true, 7),         // LD IYL, A
            0x7C => self.ld_r_idx_half(true, true, 7),  // LD A, IYH
            0x7D => self.ld_r_idx_half(true, false, 7), // LD A, IYL
            0x84 => self.alu_idx_half(true, true, 0),   // ADD A, IYH
            0x85 => self.alu_idx_half(true, false, 0),  // ADD A, IYL
            0x8C => self.alu_idx_half(true, true, 1),   // ADC A, IYH
            0x8D => self.alu_idx_half(true, false, 1),  // ADC A, IYL
            0x94 => self.alu_idx_half(true, true, 2),   // SUB IYH
            0x95 => self.alu_idx_half(true, false, 2),  // SUB IYL
            0x9C => self.alu_idx_half(true, true, 3),   // SBC A, IYH
            0x9D => self.alu_idx_half(true, false, 3),  // SBC A, IYL
            0xA4 => self.alu_idx_half(true, true, 4),   // AND IYH
            0xA5 => self.alu_idx_half(true, false, 4),  // AND IYL
            0xAC => self.alu_idx_half(true, true, 5),   // XOR IYH
            0xAD => self.alu_idx_half(true, false, 5),  // XOR IYL
            0xB4 => self.alu_idx_half(true, true, 6),   // OR IYH
            0xB5 => self.alu_idx_half(true, false, 6),  // OR IYL
            0xBC => self.alu_idx_half(true, true, 7),   // CP IYH
            0xBD => self.alu_idx_half(true, false, 7),  // CP IYL

            // Index operations with displacement (IY+d)
            0x34 => self.inc_idx_d(bus, true),     // INC (IY+d)
            0x35 => self.dec_idx_d(bus, true),     // DEC (IY+d)
            0x36 => self.ld_idx_d_n(bus, true),    // LD (IY+d), n
            0x46 => self.ld_r_idx_d(bus, true, 0), // LD B, (IY+d)
            0x4E => self.ld_r_idx_d(bus, true, 1), // LD C, (IY+d)
            0x56 => self.ld_r_idx_d(bus, true, 2), // LD D, (IY+d)
            0x5E => self.ld_r_idx_d(bus, true, 3), // LD E, (IY+d)
            0x66 => self.ld_r_idx_d(bus, true, 4), // LD H, (IY+d)
            0x6E => self.ld_r_idx_d(bus, true, 5), // LD L, (IY+d)
            0x70 => self.ld_idx_d_r(bus, true, 0), // LD (IY+d), B
            0x71 => self.ld_idx_d_r(bus, true, 1), // LD (IY+d), C
            0x72 => self.ld_idx_d_r(bus, true, 2), // LD (IY+d), D
            0x73 => self.ld_idx_d_r(bus, true, 3), // LD (IY+d), E
            0x74 => self.ld_idx_d_r(bus, true, 4), // LD (IY+d), H
            0x75 => self.ld_idx_d_r(bus, true, 5), // LD (IY+d), L
            0x77 => self.ld_idx_d_r(bus, true, 7), // LD (IY+d), A
            0x7E => self.ld_r_idx_d(bus, true, 7), // LD A, (IY+d)
            0x86 => self.add_a_idx_d(bus, true),   // ADD A, (IY+d)
            0x8E => self.adc_a_idx_d(bus, true),   // ADC A, (IY+d)
            0x96 => self.sub_idx_d(bus, true),     // SUB (IY+d)
            0x9E => self.sbc_a_idx_d(bus, true),   // SBC A, (IY+d)
            0xA6 => self.and_idx_d(bus, true),     // AND (IY+d)
            0xAE => self.xor_idx_d(bus, true),     // XOR (IY+d)
            0xB6 => self.or_idx_d(bus, true),      // OR (IY+d)
            0xBE => self.cp_idx_d(bus, true),      // CP (IY+d)

            // CB prefix - bit operations with displacement
            0xCB => self.decode_cb_idx_prefix(bus, true),

            // Default: pass-through to base instruction with 4-cycle FD prefix overhead
            _ => {
                let result = self.decode_and_execute(bus, opcode);
                InstructionResult::new(result.cycles + 4)
            }
        }
    }

    fn _unimplemented(&self, opcode: u8) -> InstructionResult {
        panic!(
            "Unimplemented Z80 instruction: ${:02X} at PC=${:04X}",
            opcode,
            self.pc.wrapping_sub(1)
        );
    }

    fn unimplemented_prefix(&self, prefix: u8, opcode: u8) -> InstructionResult {
        panic!(
            "Unimplemented Z80 prefixed instruction: ${:02X} ${:02X} at PC=${:04X}",
            prefix,
            opcode,
            self.pc.wrapping_sub(2)
        );
    }

    /// Disassemble instruction at current PC (helper for debugging)
    pub fn disassemble_current(&self, bus: &mut dyn Addressable) -> String {
        let (mnemonic, _) = disasm::disassemble_at(bus, self.pc);
        mnemonic
    }

    // ========================
    // Register helpers (public for instruction modules)
    // ========================

    /// Helper: Read 8-bit register
    #[inline]
    pub(crate) fn read_reg8(&self, reg: Reg8) -> u8 {
        match reg {
            Reg8::A => self.a,
            Reg8::B => self.b,
            Reg8::C => self.c,
            Reg8::D => self.d,
            Reg8::E => self.e,
            Reg8::H => self.h,
            Reg8::L => self.l,
            Reg8::IXH => (self.ix >> 8) as u8,
            Reg8::IXL => (self.ix & 0xFF) as u8,
            Reg8::IYH => (self.iy >> 8) as u8,
            Reg8::IYL => (self.iy & 0xFF) as u8,
        }
    }

    /// Helper: Write 8-bit register
    #[inline]
    pub(crate) fn write_reg8(&mut self, reg: Reg8, value: u8) {
        match reg {
            Reg8::A => self.a = value,
            Reg8::B => self.b = value,
            Reg8::C => self.c = value,
            Reg8::D => self.d = value,
            Reg8::E => self.e = value,
            Reg8::H => self.h = value,
            Reg8::L => self.l = value,
            Reg8::IXH => self.ix = (self.ix & 0x00FF) | ((value as u16) << 8),
            Reg8::IXL => self.ix = (self.ix & 0xFF00) | (value as u16),
            Reg8::IYH => self.iy = (self.iy & 0x00FF) | ((value as u16) << 8),
            Reg8::IYL => self.iy = (self.iy & 0xFF00) | (value as u16),
        }
    }

    /// Helper: Write 8-bit register by numeric code (0=B,1=C,2=D,3=E,4=H,5=L,7=A)
    #[inline]
    fn write_reg8_by_code(&mut self, code: u8, value: u8) {
        match code {
            0 => self.b = value,
            1 => self.c = value,
            2 => self.d = value,
            3 => self.e = value,
            4 => self.h = value,
            5 => self.l = value,
            7 => self.a = value,
            _ => {} // 6 = (HL), not used here
        }
    }

    /// Helper: Read 16-bit register
    #[inline]
    pub(crate) fn read_reg16(&self, reg: Reg16) -> u16 {
        match reg {
            Reg16::BC => ((self.b as u16) << 8) | (self.c as u16),
            Reg16::DE => ((self.d as u16) << 8) | (self.e as u16),
            Reg16::HL => ((self.h as u16) << 8) | (self.l as u16),
            Reg16::SP => self.sp,
            Reg16::AF => ((self.a as u16) << 8) | (self.f as u16),
            Reg16::IX => self.ix,
            Reg16::IY => self.iy,
        }
    }

    /// Helper: Write 16-bit register
    #[inline]
    pub(crate) fn write_reg16(&mut self, reg: Reg16, value: u16) {
        match reg {
            Reg16::BC => {
                self.b = (value >> 8) as u8;
                self.c = value as u8;
            }
            Reg16::DE => {
                self.d = (value >> 8) as u8;
                self.e = value as u8;
            }
            Reg16::HL => {
                self.h = (value >> 8) as u8;
                self.l = value as u8;
            }
            Reg16::SP => self.sp = value,
            Reg16::AF => {
                self.a = (value >> 8) as u8;
                self.f = value as u8;
            }
            Reg16::IX => self.ix = value,
            Reg16::IY => self.iy = value,
        }
    }

    /// Helper: Decode register index (0-7) to Reg8
    /// 0=B, 1=C, 2=D, 3=E, 4=H, 5=L, 6=(HL), 7=A
    /// Note: index 6 is special - it represents (HL), not a register
    #[inline]
    fn decode_reg8_index(&self, index: u8) -> Reg8 {
        match index {
            0 => Reg8::B,
            1 => Reg8::C,
            2 => Reg8::D,
            3 => Reg8::E,
            4 => Reg8::H,
            5 => Reg8::L,
            6 => panic!("decode_reg8_index: index 6 represents (HL), not a register"),
            7 => Reg8::A,
            _ => unreachable!("Invalid register index: {}", index),
        }
    }
}

impl Default for Z80 {
    fn default() -> Self {
        Self::new()
    }
}

impl Clockable for Z80 {
    fn clock(&mut self, _cycles: u32) -> u32 {
        // Stub: `GenesisEmulator` runs the Z80 through `step()`, one instruction
        // at a time, so this runs nothing and returns 0.
        0
    }
}

impl Resettable for Z80 {
    fn reset(&mut self) {
        // Reset to initial state (as per Z80 spec)
        self.pc = 0x0000;
        self.sp = 0xFFFF;
        self.a = 0xFF;
        self.f = 0xFF;
        self.b = 0xFF;
        self.c = 0xFF;
        self.d = 0xFF;
        self.e = 0xFF;
        self.h = 0xFF;
        self.l = 0xFF;

        // Alternate registers
        self.a_ = 0xFF;
        self.f_ = 0xFF;
        self.b_ = 0xFF;
        self.c_ = 0xFF;
        self.d_ = 0xFF;
        self.e_ = 0xFF;
        self.h_ = 0xFF;
        self.l_ = 0xFF;

        // Index registers
        self.ix = 0xFFFF;
        self.iy = 0xFFFF;

        // Special registers
        self.i = 0;
        self.r = 0;

        // Internal registers
        self.wz = 0;

        // Interrupt state
        self.iff1 = false;
        self.iff2 = false;
        self.im = 0;
        self.ei = 0;

        // Internal flags
        self.p = 0;
        self.q = 0;

        // CPU state
        self.halted = false;
        self.cycles_executed = 0;
    }
}

/// Instruction execution result
#[derive(Debug, Clone, Copy)]
pub struct InstructionResult {
    pub cycles: u32,
}

impl InstructionResult {
    pub fn new(cycles: u32) -> Self {
        Self { cycles }
    }
}

// Flag bit positions
pub const FLAG_S: u8 = 0x80; // Sign (bit 7)
pub const FLAG_Z: u8 = 0x40; // Zero (bit 6)
pub const FLAG_Y: u8 = 0x20; // Undocumented (bit 5)
pub const FLAG_H: u8 = 0x10; // Half carry (bit 4)
pub const FLAG_X: u8 = 0x08; // Undocumented (bit 3)
pub const FLAG_PV: u8 = 0x04; // Parity/Overflow (bit 2)
pub const FLAG_N: u8 = 0x02; // Add/Subtract (bit 1)
pub const FLAG_C: u8 = 0x01; // Carry (bit 0)

impl Z80 {
    // Register getters
    pub fn get_a(&self) -> u8 {
        self.a
    }

    pub fn get_f(&self) -> u8 {
        self.f
    }

    pub fn get_b(&self) -> u8 {
        self.b
    }

    pub fn get_c(&self) -> u8 {
        self.c
    }

    pub fn get_d(&self) -> u8 {
        self.d
    }

    pub fn get_e(&self) -> u8 {
        self.e
    }

    pub fn get_h(&self) -> u8 {
        self.h
    }

    pub fn get_l(&self) -> u8 {
        self.l
    }

    pub fn get_pc(&self) -> u16 {
        self.pc
    }

    pub fn get_sp(&self) -> u16 {
        self.sp
    }

    pub fn get_ix(&self) -> u16 {
        self.ix
    }

    pub fn get_iy(&self) -> u16 {
        self.iy
    }

    pub fn get_bc(&self) -> u16 {
        ((self.b as u16) << 8) | (self.c as u16)
    }

    pub fn get_de(&self) -> u16 {
        ((self.d as u16) << 8) | (self.e as u16)
    }

    pub fn get_hl(&self) -> u16 {
        ((self.h as u16) << 8) | (self.l as u16)
    }

    pub fn is_halted(&self) -> bool {
        self.halted
    }

    // Register setters
    pub fn set_pc(&mut self, value: u16) {
        self.pc = value;
    }

    // Flag getters
    #[inline]
    pub fn flag_s(&self) -> bool {
        (self.f & FLAG_S) != 0
    }

    #[inline]
    pub fn flag_z(&self) -> bool {
        (self.f & FLAG_Z) != 0
    }

    #[inline]
    pub fn flag_h(&self) -> bool {
        (self.f & FLAG_H) != 0
    }

    #[inline]
    pub fn flag_pv(&self) -> bool {
        (self.f & FLAG_PV) != 0
    }

    #[inline]
    pub fn flag_n(&self) -> bool {
        (self.f & FLAG_N) != 0
    }

    #[inline]
    pub fn flag_c(&self) -> bool {
        (self.f & FLAG_C) != 0
    }

    // Flag setters - each marks flags_modified for Q tracking
    #[inline]
    pub fn set_flag_s(&mut self, val: bool) {
        if val {
            self.f |= FLAG_S;
        } else {
            self.f &= !FLAG_S;
        }
        self.flags_modified = true;
    }

    #[inline]
    pub fn set_flag_z(&mut self, val: bool) {
        if val {
            self.f |= FLAG_Z;
        } else {
            self.f &= !FLAG_Z;
        }
        self.flags_modified = true;
    }

    #[inline]
    pub fn set_flag_h(&mut self, val: bool) {
        if val {
            self.f |= FLAG_H;
        } else {
            self.f &= !FLAG_H;
        }
        self.flags_modified = true;
    }

    #[inline]
    pub fn set_flag_pv(&mut self, val: bool) {
        if val {
            self.f |= FLAG_PV;
        } else {
            self.f &= !FLAG_PV;
        }
        self.flags_modified = true;
    }

    #[inline]
    pub fn set_flag_n(&mut self, val: bool) {
        if val {
            self.f |= FLAG_N;
        } else {
            self.f &= !FLAG_N;
        }
        self.flags_modified = true;
    }

    #[inline]
    pub fn set_flag_c(&mut self, val: bool) {
        if val {
            self.f |= FLAG_C;
        } else {
            self.f &= !FLAG_C;
        }
        self.flags_modified = true;
    }

    #[inline]
    pub fn set_flag_y(&mut self, val: bool) {
        if val {
            self.f |= FLAG_Y;
        } else {
            self.f &= !FLAG_Y;
        }
        self.flags_modified = true;
    }

    #[inline]
    pub fn set_flag_x(&mut self, val: bool) {
        if val {
            self.f |= FLAG_X;
        } else {
            self.f &= !FLAG_X;
        }
        self.flags_modified = true;
    }

    // Helper: Set S, Z, P/V flags based on result
    #[inline]
    pub fn set_flags_szpv(&mut self, val: u8) {
        self.set_flag_s((val & 0x80) != 0);
        self.set_flag_z(val == 0);
        self.set_flag_pv(val.count_ones().is_multiple_of(2)); // Parity
    }

    // Helper: Get 16-bit register pairs
    #[inline]
    pub fn bc(&self) -> u16 {
        ((self.b as u16) << 8) | (self.c as u16)
    }

    #[inline]
    pub fn de(&self) -> u16 {
        ((self.d as u16) << 8) | (self.e as u16)
    }

    #[inline]
    pub fn hl(&self) -> u16 {
        ((self.h as u16) << 8) | (self.l as u16)
    }

    #[inline]
    pub fn af(&self) -> u16 {
        ((self.a as u16) << 8) | (self.f as u16)
    }

    // Helper: Set 16-bit register pairs
    #[inline]
    pub fn set_bc(&mut self, val: u16) {
        self.b = (val >> 8) as u8;
        self.c = (val & 0xFF) as u8;
    }

    #[inline]
    pub fn set_de(&mut self, val: u16) {
        self.d = (val >> 8) as u8;
        self.e = (val & 0xFF) as u8;
    }

    #[inline]
    pub fn set_hl(&mut self, val: u16) {
        self.h = (val >> 8) as u8;
        self.l = (val & 0xFF) as u8;
    }

    #[inline]
    pub fn set_af(&mut self, val: u16) {
        self.a = (val >> 8) as u8;
        self.f = (val & 0xFF) as u8;
    }
}

// Re-export register enums
pub use addressing::{Reg16, Reg8};

// Savestate support
impl Z80 {
    pub fn to_savestate(&self) -> crate::system::savestate::Z80State {
        crate::system::savestate::Z80State {
            a: self.a,
            f: self.f,
            b: self.b,
            c: self.c,
            d: self.d,
            e: self.e,
            h: self.h,
            l: self.l,
            a_prime: self.a_,
            f_prime: self.f_,
            b_prime: self.b_,
            c_prime: self.c_,
            d_prime: self.d_,
            e_prime: self.e_,
            h_prime: self.h_,
            l_prime: self.l_,
            ix: self.ix,
            iy: self.iy,
            sp: self.sp,
            pc: self.pc,
            i: self.i,
            r: self.r,
            wz: self.wz,
            iff1: self.iff1,
            iff2: self.iff2,
            im: self.im,
            ei: self.ei,
            p: self.p,
            q: self.q,
            prev_q: self.prev_q,
            halted: self.halted,
            cycles: self.cycles_executed,
        }
    }

    pub fn from_savestate(&mut self, state: &crate::system::savestate::Z80State) {
        self.a = state.a;
        self.f = state.f;
        self.b = state.b;
        self.c = state.c;
        self.d = state.d;
        self.e = state.e;
        self.h = state.h;
        self.l = state.l;
        self.a_ = state.a_prime;
        self.f_ = state.f_prime;
        self.b_ = state.b_prime;
        self.c_ = state.c_prime;
        self.d_ = state.d_prime;
        self.e_ = state.e_prime;
        self.h_ = state.h_prime;
        self.l_ = state.l_prime;
        self.ix = state.ix;
        self.iy = state.iy;
        self.sp = state.sp;
        self.pc = state.pc;
        self.i = state.i;
        self.r = state.r;
        self.wz = state.wz;
        self.iff1 = state.iff1;
        self.iff2 = state.iff2;
        self.im = state.im;
        self.ei = state.ei;
        self.p = state.p;
        self.q = state.q;
        self.prev_q = state.prev_q;
        self.halted = state.halted;
        self.cycles_executed = state.cycles;
    }
}
