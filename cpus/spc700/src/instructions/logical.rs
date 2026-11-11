//! Bitwise logical instructions, `AND`, `OR`, `EOR`.
//!
//! 36 opcodes (12 modes x 3 mnemonics), all sharing the same 12-mode
//! binary-ALU matrix used by `ADC`/`SBC`/`CMP A`. Operand fetch is
//! delegated to [`super::binary_ops`]. Bit-level ops (`OR1`/`AND1`/
//! `EOR1`/`MOV1`/`NOT1`) are NOT in this file, they live in
//! [`super::bit_ops`].
//!
//! Flag semantics (checked against bsnes-plus): bitwise ops
//! update only `N` and `Z` from the result; `V`, `H`, and `C` are left
//! alone.

use retro_core::cpu::Addressable;

use crate::decode::OpcodeInfo;
use crate::instructions::binary_ops::{
    is_mem_target, read_a_target_operand, read_mem_target_operands,
};
use crate::Spc700;

/// 8-bit AND. Updates ONLY `N`/`Z`.
pub(super) fn and_internal(cpu: &mut Spc700, x: u8, y: u8) -> u8 {
    let r = x & y;
    cpu.update_nz_u8(r);
    r
}

/// 8-bit OR. Updates ONLY `N`/`Z`.
pub(super) fn or_internal(cpu: &mut Spc700, x: u8, y: u8) -> u8 {
    let r = x | y;
    cpu.update_nz_u8(r);
    r
}

/// 8-bit EOR. Updates ONLY `N`/`Z`.
pub(super) fn eor_internal(cpu: &mut Spc700, x: u8, y: u8) -> u8 {
    let r = x ^ y;
    cpu.update_nz_u8(r);
    r
}

pub fn and(cpu: &mut Spc700, bus: &mut dyn Addressable, _opcode: u8, info: &OpcodeInfo) -> u32 {
    if is_mem_target(info.mode) {
        let (dst_addr, dst_val, src_val) = read_mem_target_operands(cpu, bus, info.mode);
        let r = and_internal(cpu, dst_val, src_val);
        bus.write(dst_addr as u32, r);
    } else {
        let src = read_a_target_operand(cpu, bus, info.mode);
        cpu.a = and_internal(cpu, cpu.a, src);
    }
    info.base_cycles as u32
}

pub fn or(cpu: &mut Spc700, bus: &mut dyn Addressable, _opcode: u8, info: &OpcodeInfo) -> u32 {
    if is_mem_target(info.mode) {
        let (dst_addr, dst_val, src_val) = read_mem_target_operands(cpu, bus, info.mode);
        let r = or_internal(cpu, dst_val, src_val);
        bus.write(dst_addr as u32, r);
    } else {
        let src = read_a_target_operand(cpu, bus, info.mode);
        cpu.a = or_internal(cpu, cpu.a, src);
    }
    info.base_cycles as u32
}

pub fn eor(cpu: &mut Spc700, bus: &mut dyn Addressable, _opcode: u8, info: &OpcodeInfo) -> u32 {
    if is_mem_target(info.mode) {
        let (dst_addr, dst_val, src_val) = read_mem_target_operands(cpu, bus, info.mode);
        let r = eor_internal(cpu, dst_val, src_val);
        bus.write(dst_addr as u32, r);
    } else {
        let src = read_a_target_operand(cpu, bus, info.mode);
        cpu.a = eor_internal(cpu, cpu.a, src);
    }
    info.base_cycles as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::{FLAG_C, FLAG_H, FLAG_N, FLAG_V, FLAG_Z};
    use crate::Spc700;

    fn cpu_all_flags_set() -> Spc700 {
        let mut cpu = Spc700::new();
        cpu.psw = 0xFF;
        cpu
    }

    #[test]
    fn and_internal_updates_only_nz_preserves_v_h_c() {
        let mut cpu = cpu_all_flags_set();
        let r = and_internal(&mut cpu, 0xF0, 0x0F);
        assert_eq!(r, 0x00);
        assert!(cpu.get_flag(FLAG_Z));
        assert!(!cpu.get_flag(FLAG_N));
        assert!(cpu.get_flag(FLAG_C), "C must survive AND");
        assert!(cpu.get_flag(FLAG_V), "V must survive AND");
        assert!(cpu.get_flag(FLAG_H), "H must survive AND");
    }

    #[test]
    fn or_internal_negative_result_sets_n() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        let r = or_internal(&mut cpu, 0x80, 0x00);
        assert_eq!(r, 0x80);
        assert!(cpu.get_flag(FLAG_N));
        assert!(!cpu.get_flag(FLAG_Z));
    }

    #[test]
    fn eor_internal_preserves_v_h_c() {
        let mut cpu = cpu_all_flags_set();
        let r = eor_internal(&mut cpu, 0xAA, 0xFF);
        assert_eq!(r, 0x55);
        assert!(!cpu.get_flag(FLAG_N));
        assert!(!cpu.get_flag(FLAG_Z));
        assert!(cpu.get_flag(FLAG_C));
        assert!(cpu.get_flag(FLAG_V));
        assert!(cpu.get_flag(FLAG_H));
    }
}
