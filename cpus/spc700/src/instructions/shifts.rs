//! Shift and rotate instructions, `ASL`, `LSR`, `ROL`, `ROR`, `XCN`.
//!
//! 17 opcodes:
//! - `ASL` / `LSR` / `ROL` / `ROR`: 4 modes each (A, `$dp`, `!abs`,
//!   `$dp+X`). A-variants and memory variants share the underlying
//!   ALU helper but differ in cycle count and bus access pattern.
//! - `XCN A` (`$9F`): swaps the nibbles of `A`. 5 cycles.
//!
//! Flag semantics (checked against bsnes-plus): all shifts/rotates
//! update `C` (the bit shifted out), `N` (bit 7 of result), and `Z`
//! (result == 0). `V` and `H` are NEVER touched. `LSR`'s `N` is always
//! 0 because the unsigned right-shift never produces bit 7.

use retro_core::cpu::Addressable;

use crate::addressing::{dp_addr, dp_addr_x, fetch_pc_byte, fetch_pc_word, AddressingMode};
use crate::decode::OpcodeInfo;
use crate::instructions::FLAG_C;
use crate::Spc700;

// -- ALU helpers -------------------------------------------------------------

/// Which end of the byte the bits move towards.
#[derive(Clone, Copy)]
enum Towards {
    Top,
    Bottom,
}

/// Moves a byte one place: the bit that falls off goes to `C`, `bit_in`
/// takes the place left empty, and `N` and `Z` are the result's.
fn moved(cpu: &mut Spc700, value: u8, towards: Towards, bit_in: bool) -> u8 {
    let (result, bit_out) = match towards {
        Towards::Top => ((value << 1) | u8::from(bit_in), value >> 7 != 0),
        Towards::Bottom => ((value >> 1) | (u8::from(bit_in) << 7), value & 1 != 0),
    };
    cpu.set_flag(FLAG_C, bit_out);
    cpu.update_nz_u8(result);
    result
}

/// `ASL`: a zero comes in at the bottom.
pub(super) fn shift_left(cpu: &mut Spc700, value: u8) -> u8 {
    moved(cpu, value, Towards::Top, false)
}

/// `LSR`: a zero comes in at the top, so `N` always ends clear.
pub(super) fn shift_right(cpu: &mut Spc700, value: u8) -> u8 {
    moved(cpu, value, Towards::Bottom, false)
}

/// `ROL`: the old `C` comes in at the bottom.
pub(super) fn rotate_left(cpu: &mut Spc700, value: u8) -> u8 {
    let carry = cpu.get_flag(FLAG_C);
    moved(cpu, value, Towards::Top, carry)
}

/// `ROR`: the old `C` comes in at the top.
pub(super) fn rotate_right(cpu: &mut Spc700, value: u8) -> u8 {
    let carry = cpu.get_flag(FLAG_C);
    moved(cpu, value, Towards::Bottom, carry)
}

// -- Shared RMW driver -------------------------------------------------------

/// Drive an 8-bit RMW shift/rotate against the addressing mode. The
/// `op` closure performs the ALU work and the flag updates; we just
/// route operand reads/writes here so each per-mnemonic handler stays
/// a 5-line dispatch.
fn rmw_8<F>(cpu: &mut Spc700, bus: &mut dyn Addressable, mode: AddressingMode, op: F)
where
    F: FnOnce(&mut Spc700, u8) -> u8,
{
    match mode {
        AddressingMode::Accumulator => {
            cpu.a = op(cpu, cpu.a);
        }
        AddressingMode::DirectPage => {
            let dp = fetch_pc_byte(cpu, bus);
            let addr = dp_addr(cpu, dp);
            let val = bus.read(addr as u32);
            let r = op(cpu, val);
            bus.write(addr as u32, r);
        }
        AddressingMode::DirectPageX => {
            let dp = fetch_pc_byte(cpu, bus);
            let addr = dp_addr_x(cpu, dp);
            let val = bus.read(addr as u32);
            let r = op(cpu, val);
            bus.write(addr as u32, r);
        }
        AddressingMode::Absolute => {
            let addr = fetch_pc_word(cpu, bus);
            let val = bus.read(addr as u32);
            let r = op(cpu, val);
            bus.write(addr as u32, r);
        }
        _ => unreachable!("shifts::rmw_8 called with non-RMW mode: {:?}", mode),
    }
}

// -- Handlers ----------------------------------------------------------------

pub fn asl(cpu: &mut Spc700, bus: &mut dyn Addressable, _opcode: u8, info: &OpcodeInfo) -> u32 {
    rmw_8(cpu, bus, info.mode, shift_left);
    info.base_cycles as u32
}

pub fn lsr(cpu: &mut Spc700, bus: &mut dyn Addressable, _opcode: u8, info: &OpcodeInfo) -> u32 {
    rmw_8(cpu, bus, info.mode, shift_right);
    info.base_cycles as u32
}

pub fn rol(cpu: &mut Spc700, bus: &mut dyn Addressable, _opcode: u8, info: &OpcodeInfo) -> u32 {
    rmw_8(cpu, bus, info.mode, rotate_left);
    info.base_cycles as u32
}

pub fn ror(cpu: &mut Spc700, bus: &mut dyn Addressable, _opcode: u8, info: &OpcodeInfo) -> u32 {
    rmw_8(cpu, bus, info.mode, rotate_right);
    info.base_cycles as u32
}

/// `XCN A` (`$9F`): swap the high and low nibbles of `A`. Sets N/Z
/// from the result; leaves C/V/H alone.
pub fn xcn(cpu: &mut Spc700, _bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    cpu.a = cpu.a.rotate_left(4);
    cpu.update_nz_u8(cpu.a);
    info.base_cycles as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::{FLAG_C, FLAG_H, FLAG_N, FLAG_V, FLAG_Z};
    use crate::Spc700;

    fn cpu_with_c(c: bool) -> Spc700 {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        cpu.set_flag(FLAG_C, c);
        // pre-set V/H so we can check they survive
        cpu.set_flag(FLAG_V, true);
        cpu.set_flag(FLAG_H, true);
        cpu
    }

    #[test]
    fn asl_carry_from_bit_7() {
        let mut cpu = cpu_with_c(false);
        let r = shift_left(&mut cpu, 0x81);
        assert_eq!(r, 0x02);
        assert!(cpu.get_flag(FLAG_C), "bit 7 of 0x81 -> C");
        assert!(!cpu.get_flag(FLAG_N));
        assert!(!cpu.get_flag(FLAG_Z));
        assert!(cpu.get_flag(FLAG_V), "V must be preserved");
        assert!(cpu.get_flag(FLAG_H), "H must be preserved");
    }

    #[test]
    fn lsr_n_flag_always_zero() {
        let mut cpu = cpu_with_c(false);
        let r = shift_right(&mut cpu, 0xFF);
        assert_eq!(r, 0x7F);
        assert!(cpu.get_flag(FLAG_C), "bit 0 of 0xFF -> C");
        assert!(
            !cpu.get_flag(FLAG_N),
            "unsigned right-shift can never produce N"
        );
    }

    #[test]
    fn rol_carry_round_trip() {
        // 0x80 with C=1 -> ROL -> 0x01 with C=1.
        let mut cpu = cpu_with_c(true);
        let r = rotate_left(&mut cpu, 0x80);
        assert_eq!(r, 0x01);
        assert!(cpu.get_flag(FLAG_C));
        assert!(!cpu.get_flag(FLAG_N));
    }

    #[test]
    fn ror_carry_round_trip() {
        // 0x01 with C=1 -> ROR -> 0x80 with C=1.
        let mut cpu = cpu_with_c(true);
        let r = rotate_right(&mut cpu, 0x01);
        assert_eq!(r, 0x80);
        assert!(cpu.get_flag(FLAG_C));
        assert!(cpu.get_flag(FLAG_N), "rotated-in carry sets bit 7");
    }

    #[test]
    fn xcn_swaps_nibbles_and_sets_nz() {
        let mut cpu = Spc700::new();
        cpu.psw = 0xFF; // all flags set
        cpu.a = 0x4A;
        let _ = xcn(&mut cpu, &mut DummyBus, &dummy_info(5));
        assert_eq!(cpu.a, 0xA4);
        assert!(cpu.get_flag(FLAG_N), "0xA4 has bit 7 set");
        assert!(!cpu.get_flag(FLAG_Z));
        // C/V/H must survive XCN.
        assert!(cpu.get_flag(FLAG_C));
        assert!(cpu.get_flag(FLAG_V));
        assert!(cpu.get_flag(FLAG_H));
    }

    struct DummyBus;
    impl Addressable for DummyBus {
        fn read(&mut self, _: u32) -> u8 {
            0
        }
        fn write(&mut self, _: u32, _: u8) {}
    }
    fn dummy_info(cycles: u8) -> OpcodeInfo {
        OpcodeInfo {
            mnemonic: crate::decode::Mnemonic::XCN,
            mode: AddressingMode::Accumulator,
            base_cycles: cycles,
        }
    }
}
