//! SPC700 instruction implementations, grouped by category to mirror
//! the file layout of `cpus/wdc65816/src/instructions/`.
//!
//! [`dispatch`] is the single entry point called by [`crate::Spc700::step`]
//! after fetching the opcode byte. Each category file implements its
//! handlers and `dispatch` routes the opcode there based on
//! [`OpcodeInfo::mnemonic`](crate::decode::OpcodeInfo::mnemonic).
//! The match is exhaustive over [`Mnemonic`]: every one of the 256
//! opcodes resolves to a real handler. The lone `Mnemonic::ILL`
//! variant is a sentinel reserved for future table growth and panics
//! loudly so a missing handler can't silently miscount cycles.

pub mod arithmetic;
pub mod binary_ops;
pub mod bit_ops;
pub mod branches;
pub mod compare;
pub mod flags;
pub mod jumps;
pub mod load_store;
pub mod logical;
pub mod shifts;
pub mod stack;
pub mod system;

use retro_core::cpu::Addressable;

use crate::decode::{Mnemonic, OpcodeInfo};
use crate::Spc700;

// PSW bit layout: N V P B H I Z C  (bit 7 -> bit 0).
pub const FLAG_C: u8 = 0x01;
pub const FLAG_Z: u8 = 0x02;
pub const FLAG_I: u8 = 0x04;
pub const FLAG_H: u8 = 0x08;
pub const FLAG_B: u8 = 0x10;
/// Direct-page select: `P=0` selects `$0000..=$00FF`, `P=1` selects
/// `$0100..=$01FF`. Used by every direct-page addressing mode.
pub const FLAG_P: u8 = 0x20;
pub const FLAG_V: u8 = 0x40;
pub const FLAG_N: u8 = 0x80;

/// Execute the body of one instruction (the opcode byte has already
/// been consumed from `PC`). Returns the **total** cycle count for
/// the instruction (baseline from `info.base_cycles` plus any
/// branch/conditional penalty added by the handler).
///
/// An opcode no handler expects (`Mnemonic::ILL`, or a byte routed to the
/// wrong handler) `panic!`s with a stable message, so the SingleStepTests
/// runner classifies it as `panicked` and the sweep report shows it.
pub fn dispatch(cpu: &mut Spc700, bus: &mut dyn Addressable, opcode: u8, info: &OpcodeInfo) -> u32 {
    match info.mnemonic {
        // --- System ---------------------------------------------------------
        Mnemonic::NOP => system::nop(cpu, bus, info),
        Mnemonic::SLEEP => system::sleep(cpu, bus, info),
        Mnemonic::STOP => system::stop(cpu, bus, info),
        Mnemonic::BRK => system::brk(cpu, bus, info),

        // --- Flag setters ---------------------------------------------------
        Mnemonic::CLRP => flags::clrp(cpu, bus, info),
        Mnemonic::SETP => flags::setp(cpu, bus, info),
        Mnemonic::CLRC => flags::clrc(cpu, bus, info),
        Mnemonic::SETC => flags::setc(cpu, bus, info),
        Mnemonic::EI => flags::ei(cpu, bus, info),
        Mnemonic::DI => flags::di(cpu, bus, info),
        Mnemonic::CLRV => flags::clrv(cpu, bus, info),
        Mnemonic::NOTC => flags::notc(cpu, bus, info),

        // --- Stack ----------------------------------------------------------
        Mnemonic::PUSH => stack::push(cpu, bus, opcode, info),
        Mnemonic::POP => stack::pop(cpu, bus, opcode, info),

        // --- Branches -------------------------------------------------------
        Mnemonic::BPL
        | Mnemonic::BMI
        | Mnemonic::BVC
        | Mnemonic::BVS
        | Mnemonic::BCC
        | Mnemonic::BCS
        | Mnemonic::BNE
        | Mnemonic::BEQ
        | Mnemonic::BRA => branches::dispatch_conditional(cpu, bus, opcode, info),
        Mnemonic::BBS | Mnemonic::BBC => branches::dispatch_bit_branch(cpu, bus, opcode, info),
        Mnemonic::CBNE => branches::cbne(cpu, bus, opcode, info),
        Mnemonic::DBNZ => branches::dbnz(cpu, bus, opcode, info),

        // --- Jumps / Calls / Returns ---------------------------------------
        Mnemonic::JMP => jumps::jmp(cpu, bus, opcode, info),
        Mnemonic::CALL => jumps::call(cpu, bus, info),
        Mnemonic::PCALL => jumps::pcall(cpu, bus, info),
        Mnemonic::TCALL => jumps::tcall(cpu, bus, opcode, info),
        Mnemonic::RET => jumps::ret(cpu, bus, info),
        Mnemonic::RETI => jumps::reti(cpu, bus, info),

        // --- Load / Store ---------------------------------------------------
        Mnemonic::MOV => load_store::mov(cpu, bus, opcode, info),
        Mnemonic::MOVW => load_store::movw(cpu, bus, opcode, info),

        // --- Arithmetic -----------------------------------------------------
        Mnemonic::ADC => arithmetic::adc(cpu, bus, opcode, info),
        Mnemonic::SBC => arithmetic::sbc(cpu, bus, opcode, info),
        Mnemonic::ADDW => arithmetic::addw(cpu, bus, info),
        Mnemonic::SUBW => arithmetic::subw(cpu, bus, info),
        Mnemonic::INC => arithmetic::inc(cpu, bus, opcode, info),
        Mnemonic::DEC => arithmetic::dec(cpu, bus, opcode, info),
        Mnemonic::INCW => arithmetic::incw(cpu, bus, info),
        Mnemonic::DECW => arithmetic::decw(cpu, bus, info),
        Mnemonic::MUL => arithmetic::mul(cpu, bus, info),
        Mnemonic::DIV => arithmetic::div(cpu, bus, info),
        Mnemonic::DAA => arithmetic::daa(cpu, bus, info),
        Mnemonic::DAS => arithmetic::das(cpu, bus, info),

        // --- Logical --------------------------------------------------------
        Mnemonic::AND => logical::and(cpu, bus, opcode, info),
        Mnemonic::OR => logical::or(cpu, bus, opcode, info),
        Mnemonic::EOR => logical::eor(cpu, bus, opcode, info),

        // --- Shifts / rotates ----------------------------------------------
        Mnemonic::ASL => shifts::asl(cpu, bus, opcode, info),
        Mnemonic::LSR => shifts::lsr(cpu, bus, opcode, info),
        Mnemonic::ROL => shifts::rol(cpu, bus, opcode, info),
        Mnemonic::ROR => shifts::ror(cpu, bus, opcode, info),
        Mnemonic::XCN => shifts::xcn(cpu, bus, info),

        // --- Compare --------------------------------------------------------
        Mnemonic::CMP => compare::cmp(cpu, bus, opcode, info),
        Mnemonic::CMPW => compare::cmpw(cpu, bus, info),

        // --- Bit ops --------------------------------------------------------
        Mnemonic::SET1 => bit_ops::set1(cpu, bus, opcode, info),
        Mnemonic::CLR1 => bit_ops::clr1(cpu, bus, opcode, info),
        Mnemonic::NOT1 => bit_ops::not1(cpu, bus, info),
        Mnemonic::OR1 => bit_ops::or1(cpu, bus, opcode, info),
        Mnemonic::AND1 => bit_ops::and1(cpu, bus, opcode, info),
        Mnemonic::EOR1 => bit_ops::eor1(cpu, bus, info),
        Mnemonic::MOV1 => bit_ops::mov1(cpu, bus, opcode, info),
        Mnemonic::TSET => bit_ops::tset(cpu, bus, info),
        Mnemonic::TCLR => bit_ops::tclr(cpu, bus, info),

        // --- Reserved illegal sentinel --------------------------------------
        // The opcode table never produces `ILL` today (every SPC700
        // byte decodes to a real instruction), but the variant is kept
        // so the disassembler can render unknown bytes gracefully. If
        // it *does* show up here, that means the table grew without an
        // accompanying handler, surface it loudly rather than silently
        // miscounting cycles.
        Mnemonic::ILL => panic!(
            "spc700: ILL opcode {:02X} reached dispatch (table corruption)",
            opcode
        ),
    }
}
