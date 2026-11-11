//! Comparison instructions, `CMP A`, `CMP X`, `CMP Y`, `CMPW`.
//!
//! 19 opcodes split into:
//! - `CMP A` (12 modes): same 12-mode binary-ALU matrix as ADC/SBC/AND
//!   /OR/EOR. For the A-target modes the result of `A - operand` is
//!   discarded; only `N`, `Z`, `C` are written. For the memory-target
//!   modes (`DirectPageDirectPage`, `DirectPageImm`, `IxIy`) the
//!   result of `dst_val - src_val` is similarly discarded; no bus
//!   write happens (the SingleStepTests record none).
//! - `CMP X` (3 modes: `Immediate` `$C8`, `DirectPage` `$3E`,
//!   `Absolute` `$1E`): subtracts operand from `X`.
//! - `CMP Y` (3 modes: `Immediate` `$AD`, `DirectPage` `$7E`,
//!   `Absolute` `$5E`): subtracts operand from `Y`.
//! - `CMPW YA, $dp` (`$5A`): 16-bit compare of `YA` against a DP word.
//!
//! Flag semantics (checked against bsnes-plus):
//! `N` = bit 7/15 of the difference, `Z` = truncated diff is zero,
//! `C` = unsigned `x >= y`. `V` and `H` are NEVER touched.

use retro_core::cpu::Addressable;

use crate::addressing::{dp_addr, fetch_pc_byte, fetch_pc_word, read_dp_word};
use crate::decode::{Mnemonic, OpcodeInfo};
use crate::instructions::binary_ops::{
    is_mem_target, read_a_target_operand, read_mem_target_operands,
};
use crate::instructions::FLAG_C;
use crate::Spc700;

// -- ALU helpers -------------------------------------------------------------

/// 8-bit unsigned compare: the difference is thrown away. `N` and `Z`
/// are its own and `C` is set when nothing was borrowed; `V` and `H`
/// stay as they were.
pub(super) fn compare(cpu: &mut Spc700, register: u8, operand: u8) {
    let (difference, borrowed) = register.overflowing_sub(operand);
    cpu.update_nz_u8(difference);
    cpu.set_flag(FLAG_C, !borrowed);
}

/// 16-bit unsigned compare for `CMPW`: [`compare`] at word width, `N`
/// reading bit 15 and `Z` the whole difference.
pub(super) fn compare_words(cpu: &mut Spc700, register: u16, operand: u16) {
    let (difference, borrowed) = register.overflowing_sub(operand);
    cpu.update_nz_u16(difference);
    cpu.set_flag(FLAG_C, !borrowed);
}

// -- Handlers ----------------------------------------------------------------

/// Single entry point for the SPC700's three `CMP` reg-vs-operand
/// families (A, X, Y). The Mnemonic table records all three with
/// `Mnemonic::CMP`; we recover which register is the LHS from the
/// opcode byte. See header comment for the 18 opcodes covered.
pub fn cmp(cpu: &mut Spc700, bus: &mut dyn Addressable, opcode: u8, info: &OpcodeInfo) -> u32 {
    // CMP X: 3 dedicated opcodes (Immediate / DirectPage / Absolute).
    match opcode {
        0xC8 => {
            // CMP X, #imm
            let val = fetch_pc_byte(cpu, bus);
            compare(cpu, cpu.x, val);
            return info.base_cycles as u32;
        }
        0x3E => {
            // CMP X, $dp
            let dp = fetch_pc_byte(cpu, bus);
            let val = bus.read(dp_addr(cpu, dp) as u32);
            compare(cpu, cpu.x, val);
            return info.base_cycles as u32;
        }
        0x1E => {
            // CMP X, !abs
            let addr = fetch_pc_word(cpu, bus);
            let val = bus.read(addr as u32);
            compare(cpu, cpu.x, val);
            return info.base_cycles as u32;
        }
        0xAD => {
            // CMP Y, #imm
            let val = fetch_pc_byte(cpu, bus);
            compare(cpu, cpu.y, val);
            return info.base_cycles as u32;
        }
        0x7E => {
            // CMP Y, $dp
            let dp = fetch_pc_byte(cpu, bus);
            let val = bus.read(dp_addr(cpu, dp) as u32);
            compare(cpu, cpu.y, val);
            return info.base_cycles as u32;
        }
        0x5E => {
            // CMP Y, !abs
            let addr = fetch_pc_word(cpu, bus);
            let val = bus.read(addr as u32);
            compare(cpu, cpu.y, val);
            return info.base_cycles as u32;
        }
        _ => {}
    }

    // CMP A: 12 modes routed through the shared binary-ALU helpers.
    debug_assert_eq!(info.mnemonic, Mnemonic::CMP);
    if is_mem_target(info.mode) {
        let (_dst_addr, dst_val, src_val) = read_mem_target_operands(cpu, bus, info.mode);
        compare(cpu, dst_val, src_val);
        // No bus.write, CMP discards the would-be RMW result.
    } else {
        let src = read_a_target_operand(cpu, bus, info.mode);
        compare(cpu, cpu.a, src);
    }
    info.base_cycles as u32
}

/// `CMPW YA, $dp` (`$5A`): 16-bit compare between `YA` and the
/// DP-resident word.
pub fn cmpw(cpu: &mut Spc700, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    let dp = fetch_pc_byte(cpu, bus);
    let operand = read_dp_word(cpu, bus, dp);
    compare_words(cpu, cpu.ya(), operand);
    info.base_cycles as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instructions::{FLAG_C, FLAG_H, FLAG_N, FLAG_V, FLAG_Z};
    use crate::Spc700;

    fn cpu_with_v_h() -> Spc700 {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        cpu.set_flag(FLAG_V, true);
        cpu.set_flag(FLAG_H, true);
        cpu
    }

    #[test]
    fn compare_sets_n_z_c_only() {
        let mut cpu = cpu_with_v_h();
        compare(&mut cpu, 0x10, 0x10);
        assert!(cpu.get_flag(FLAG_Z));
        assert!(cpu.get_flag(FLAG_C), "x >= y -> C=1");
        assert!(!cpu.get_flag(FLAG_N));
        assert!(cpu.get_flag(FLAG_V), "V must survive CMP");
        assert!(cpu.get_flag(FLAG_H), "H must survive CMP");
    }

    #[test]
    fn compare_carry_when_reg_geq_val() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        compare(&mut cpu, 0x20, 0x10);
        assert!(cpu.get_flag(FLAG_C));
        assert!(!cpu.get_flag(FLAG_Z));
        assert!(!cpu.get_flag(FLAG_N));

        compare(&mut cpu, 0x10, 0x20);
        assert!(!cpu.get_flag(FLAG_C), "x < y -> C=0");
        assert!(!cpu.get_flag(FLAG_Z));
        assert!(cpu.get_flag(FLAG_N), "negative diff -> N");
    }

    #[test]
    fn compare_words_full_16bit_zero() {
        let mut cpu = cpu_with_v_h();
        compare_words(&mut cpu, 0x1234, 0x1234);
        assert!(cpu.get_flag(FLAG_Z));
        assert!(cpu.get_flag(FLAG_C));
        assert!(cpu.get_flag(FLAG_V), "V must survive CMPW");
        assert!(cpu.get_flag(FLAG_H), "H must survive CMPW");
    }

    #[test]
    fn compare_words_low_byte_only_difference_is_not_zero() {
        let mut cpu = Spc700::new();
        cpu.psw = 0;
        compare_words(&mut cpu, 0x1200, 0x1234);
        assert!(!cpu.get_flag(FLAG_Z), "low-byte mismatch must defeat Z");
        assert!(cpu.get_flag(FLAG_N));
    }
}
