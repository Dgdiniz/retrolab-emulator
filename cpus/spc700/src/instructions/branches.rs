//! Conditional and unconditional short branches plus the bit-branch
//! and decrement-and-branch families:
//!
//! - **Flag-conditioned** (`BPL/BMI/BVC/BVS/BCC/BCS/BNE/BEQ`) and the
//!   unconditional `BRA`, dispatched by [`dispatch_conditional`].
//! - **Bit-conditioned** (`BBS` x 8 and `BBC` x 8, dispatched by
//!   [`dispatch_bit_branch`]). The bit index is encoded in the
//!   opcode itself (`(opcode >> 5) & 7`).
//! - **Compare-and-branch** (`CBNE` x 2): branch when `A != mem`. NO
//!   flag updates, the comparison is private.
//! - **Decrement-and-branch** (`DBNZ` x 2): decrement target, branch
//!   when the result is non-zero. NO flag updates, the decrement
//!   never touches `N`/`Z`/`H`/`C`/`V`.
//!
//! Each branch returns `base_cycles + 2` when the branch is taken,
//! `base_cycles` otherwise, matching the SingleStepTests cycle deltas.

use retro_core::cpu::Addressable;

use super::{FLAG_C, FLAG_N, FLAG_V, FLAG_Z};
use crate::addressing::{dp_addr, dp_addr_x, fetch_pc_byte};
use crate::decode::OpcodeInfo;
use crate::Spc700;

/// Take or skip an 8-bit signed relative branch. Returns the cycle
/// count (`base + 2` when taken).
fn take_branch(cpu: &mut Spc700, bus: &mut dyn Addressable, info: &OpcodeInfo, taken: bool) -> u32 {
    let offset = fetch_pc_byte(cpu, bus) as i8 as i16;
    let mut cycles = info.base_cycles as u32;
    if taken {
        cpu.pc = cpu.pc.wrapping_add(offset as u16);
        cycles += 2;
    }
    cycles
}

/// Dispatcher for `BPL/BMI/BVC/BVS/BCC/BCS/BNE/BEQ/BRA`. Decodes the
/// condition from the opcode. `BRA` (`$2F`) is unconditional and the
/// SingleStepTests dataset bakes the "taken" penalty straight into the
/// baseline (4 cycles), so it skips [`take_branch`]'s `+2` adjustment.
pub fn dispatch_conditional(
    cpu: &mut Spc700,
    bus: &mut dyn Addressable,
    opcode: u8,
    info: &OpcodeInfo,
) -> u32 {
    if opcode == 0x2F {
        let offset = fetch_pc_byte(cpu, bus) as i8 as i16;
        cpu.pc = cpu.pc.wrapping_add(offset as u16);
        return info.base_cycles as u32;
    }

    let taken = match opcode {
        0x10 => (cpu.psw & FLAG_N) == 0, // BPL: N=0
        0x30 => (cpu.psw & FLAG_N) != 0, // BMI: N=1
        0x50 => (cpu.psw & FLAG_V) == 0, // BVC: V=0
        0x70 => (cpu.psw & FLAG_V) != 0, // BVS: V=1
        0x90 => (cpu.psw & FLAG_C) == 0, // BCC: C=0
        0xB0 => (cpu.psw & FLAG_C) != 0, // BCS: C=1
        0xD0 => (cpu.psw & FLAG_Z) == 0, // BNE: Z=0
        0xF0 => (cpu.psw & FLAG_Z) != 0, // BEQ: Z=1
        _ => panic!(
            "spc700: branch dispatcher hit unexpected opcode {:02X}",
            opcode
        ),
    };
    take_branch(cpu, bus, info, taken)
}

/// Dispatcher for the bit-branch family: `BBS bit, $dp, rel` (opcodes
/// `0x03`, `0x23`, ..., `0xE3`) and `BBC bit, $dp, rel` (`0x13`,
/// `0x33`, ..., `0xF3`). The bit index `b` lives in `(opcode >> 5) & 7`
/// same encoding scheme as `SET1`/`CLR1`. The low nibble selects
/// the *direction*: `0x03` style -> branch when bit IS set, `0x13`
/// style -> branch when bit is CLEAR.
///
/// Reads the DP byte first, then the signed relative offset, then
/// applies the branch with the usual `+2` penalty when taken. No flags
/// are touched.
pub fn dispatch_bit_branch(
    cpu: &mut Spc700,
    bus: &mut dyn Addressable,
    opcode: u8,
    info: &OpcodeInfo,
) -> u32 {
    let bit = (opcode >> 5) & 7;
    let dp = fetch_pc_byte(cpu, bus);
    let val = bus.read(dp_addr(cpu, dp) as u32);
    let bit_set = (val & (1 << bit)) != 0;
    // BBS variants have low nibble 0x3 (0x03/0x23/...); BBC variants
    // have low nibble 0x3 with the next opcode column shifted by 0x10
    // (0x13/0x33/...). Bit 4 of the opcode flips set->clear.
    let want_set = (opcode & 0x10) == 0;
    let taken = bit_set == want_set;
    take_branch(cpu, bus, info, taken)
}

/// `CBNE $dp, rel` (`0x2E`) and `CBNE $dp+X, rel` (`0xDE`). Compares
/// the accumulator against the addressed byte and branches when they
/// differ. The comparison NEVER updates `N`/`Z`/`C`/`V`/`H`, these
/// are completely private branch deciders.
pub fn cbne(cpu: &mut Spc700, bus: &mut dyn Addressable, opcode: u8, info: &OpcodeInfo) -> u32 {
    let dp = fetch_pc_byte(cpu, bus);
    let addr = match opcode {
        0x2E => dp_addr(cpu, dp),
        0xDE => dp_addr_x(cpu, dp),
        _ => panic!(
            "spc700: cbne dispatched with non-CBNE opcode {:02X}",
            opcode
        ),
    };
    let val = bus.read(addr as u32);
    let taken = cpu.a != val;
    take_branch(cpu, bus, info, taken)
}

/// `DBNZ $dp, rel` (`0x6E`) decrements the DP byte; `DBNZ Y, rel`
/// (`0xFE`) decrements `Y`. In both cases the resulting value is
/// tested against zero, branch when non-zero. The decrement here is
/// silent: `N`/`Z`/`H`/`C`/`V` are NEVER touched (this is what makes
/// `DBNZ` different from a regular `DEC` + `BNE` pair).
pub fn dbnz(cpu: &mut Spc700, bus: &mut dyn Addressable, opcode: u8, info: &OpcodeInfo) -> u32 {
    let new_val = match opcode {
        0x6E => {
            let dp = fetch_pc_byte(cpu, bus);
            let addr = dp_addr(cpu, dp);
            let nv = bus.read(addr as u32).wrapping_sub(1);
            bus.write(addr as u32, nv);
            nv
        }
        0xFE => {
            cpu.y = cpu.y.wrapping_sub(1);
            cpu.y
        }
        _ => panic!(
            "spc700: dbnz dispatched with non-DBNZ opcode {:02X}",
            opcode
        ),
    };
    take_branch(cpu, bus, info, new_val != 0)
}

#[cfg(test)]
mod bit_branch_tests {
    use super::*;
    use crate::addressing::AddressingMode;
    use crate::decode::Mnemonic;
    use crate::Spc700;

    struct TinyBus {
        ram: [u8; 0x10000],
    }
    impl TinyBus {
        fn new() -> Self {
            Self { ram: [0; 0x10000] }
        }
    }
    impl Addressable for TinyBus {
        fn read(&mut self, addr: u32) -> u8 {
            self.ram[(addr & 0xFFFF) as usize]
        }
        fn write(&mut self, addr: u32, value: u8) {
            self.ram[(addr & 0xFFFF) as usize] = value;
        }
    }

    fn fresh(pc: u16) -> (Spc700, TinyBus) {
        let mut cpu = Spc700::new();
        cpu.pc = pc;
        cpu.psw = 0;
        (cpu, TinyBus::new())
    }

    fn info(mnem: Mnemonic, mode: AddressingMode, cycles: u8) -> OpcodeInfo {
        OpcodeInfo {
            mnemonic: mnem,
            mode,
            base_cycles: cycles,
        }
    }

    // -- BBS / BBC --------------------------------------------------------

    #[test]
    fn bbs_taken_when_bit_set_costs_plus_two() {
        let (mut cpu, mut bus) = fresh(0x2000);
        // Opcode 0x43 = BBS bit=2 (from (0x43>>5)&7), DP operand at $2000, rel at $2001.
        bus.write(0x2000, 0x40); // DP = 0x40 -> abs $0040
        bus.write(0x2001, 0x10); // rel = +0x10
        bus.write(0x0040, 0b0000_0100); // bit 2 SET
        let i = info(Mnemonic::BBS, AddressingMode::DirectPageRelative, 5);
        let cycles = dispatch_bit_branch(&mut cpu, &mut bus, 0x43, &i);
        assert_eq!(cycles, 7, "base 5 + 2 taken");
        assert_eq!(cpu.pc, 0x2002u16.wrapping_add(0x10));
    }

    #[test]
    fn bbs_skipped_when_bit_clear_no_penalty() {
        let (mut cpu, mut bus) = fresh(0x2000);
        bus.write(0x2000, 0x40);
        bus.write(0x2001, 0x10);
        bus.write(0x0040, 0b0000_0000);
        let i = info(Mnemonic::BBS, AddressingMode::DirectPageRelative, 5);
        let cycles = dispatch_bit_branch(&mut cpu, &mut bus, 0x43, &i);
        assert_eq!(cycles, 5, "base only when skipped");
        assert_eq!(cpu.pc, 0x2002, "PC advances past operand bytes only");
    }

    #[test]
    fn bbc_taken_when_bit_clear() {
        let (mut cpu, mut bus) = fresh(0x2000);
        // Opcode 0xF3 = BBC bit=7, DP at $2000, rel at $2001.
        bus.write(0x2000, 0x55);
        bus.write(0x2001, 0xF0); // rel = -16
        bus.write(0x0055, 0b0000_0001); // bit 7 CLEAR
        let i = info(Mnemonic::BBC, AddressingMode::DirectPageRelative, 5);
        let cycles = dispatch_bit_branch(&mut cpu, &mut bus, 0xF3, &i);
        assert_eq!(cycles, 7);
        // PC = 0x2002 + (-16 as i16) = 0x1FF2
        assert_eq!(cpu.pc, 0x1FF2);
    }

    #[test]
    fn bbs_does_not_touch_flags() {
        let (mut cpu, mut bus) = fresh(0x2000);
        cpu.psw = 0xFF;
        bus.write(0x2000, 0x10);
        bus.write(0x2001, 0x00);
        bus.write(0x0010, 0x80);
        let i = info(Mnemonic::BBS, AddressingMode::DirectPageRelative, 5);
        let _ = dispatch_bit_branch(&mut cpu, &mut bus, 0xE3, &i); // BBS bit=7
        assert_eq!(cpu.psw, 0xFF, "BBS/BBC must not touch any flag");
    }

    // -- CBNE -------------------------------------------------------------

    #[test]
    fn cbne_dp_branches_when_a_differs_from_mem() {
        let (mut cpu, mut bus) = fresh(0x3000);
        cpu.a = 0x11;
        bus.write(0x3000, 0x20); // DP=0x20
        bus.write(0x3001, 0x08); // rel=+8
        bus.write(0x0020, 0x22); // mem
        let i = info(Mnemonic::CBNE, AddressingMode::DirectPageRelative, 5);
        let cycles = cbne(&mut cpu, &mut bus, 0x2E, &i);
        assert_eq!(cycles, 7);
        assert_eq!(cpu.pc, 0x3002u16.wrapping_add(8));
    }

    #[test]
    fn cbne_dp_skips_when_a_equals_mem_and_preserves_flags() {
        let (mut cpu, mut bus) = fresh(0x3000);
        cpu.a = 0x42;
        cpu.psw = 0xFF; // P=1 -> dp_base = $0100, so the byte we want
                        // CBNE to read lives at $0120, not $0020.
        bus.write(0x3000, 0x20);
        bus.write(0x3001, 0x08);
        bus.write(0x0120, 0x42);
        let i = info(Mnemonic::CBNE, AddressingMode::DirectPageRelative, 5);
        let cycles = cbne(&mut cpu, &mut bus, 0x2E, &i);
        assert_eq!(cycles, 5, "no penalty when equal -> skipped");
        assert_eq!(cpu.pc, 0x3002);
        assert_eq!(cpu.psw, 0xFF, "CBNE must not touch any flag");
    }

    #[test]
    fn cbne_dpx_uses_x_offset_into_dp() {
        let (mut cpu, mut bus) = fresh(0x3000);
        cpu.a = 0x00;
        cpu.x = 0x04;
        bus.write(0x3000, 0x20); // DP base
        bus.write(0x3001, 0x10); // rel
        bus.write(0x0024, 0x55); // mem at DP+X
        let i = info(Mnemonic::CBNE, AddressingMode::DirectPageXRelative, 6);
        let cycles = cbne(&mut cpu, &mut bus, 0xDE, &i);
        assert_eq!(cycles, 8);
        assert_eq!(cpu.pc, 0x3002u16.wrapping_add(0x10));
    }

    // -- DBNZ -------------------------------------------------------------

    #[test]
    fn dbnz_dp_decrements_mem_then_branches_when_nonzero() {
        let (mut cpu, mut bus) = fresh(0x4000);
        cpu.psw = 0xFF; // P=1 -> DP $20 maps to $0120, not $0020.
        bus.write(0x4000, 0x20);
        bus.write(0x4001, 0x05);
        bus.write(0x0120, 0x03);
        let i = info(Mnemonic::DBNZ, AddressingMode::DirectPageRelative, 5);
        let cycles = dbnz(&mut cpu, &mut bus, 0x6E, &i);
        assert_eq!(cycles, 7);
        assert_eq!(bus.read(0x0120), 0x02, "mem decremented");
        assert_eq!(cpu.pc, 0x4002u16.wrapping_add(5));
        assert_eq!(cpu.psw, 0xFF, "DBNZ must not touch flags");
    }

    #[test]
    fn dbnz_dp_skips_when_result_is_zero() {
        let (mut cpu, mut bus) = fresh(0x4000);
        bus.write(0x4000, 0x20);
        bus.write(0x4001, 0x05);
        bus.write(0x0020, 0x01); // -> becomes 0
        let i = info(Mnemonic::DBNZ, AddressingMode::DirectPageRelative, 5);
        let cycles = dbnz(&mut cpu, &mut bus, 0x6E, &i);
        assert_eq!(cycles, 5, "no penalty when result == 0");
        assert_eq!(bus.read(0x0020), 0x00);
        assert_eq!(cpu.pc, 0x4002);
    }

    #[test]
    fn dbnz_y_decrements_y_register_only_then_branches() {
        let (mut cpu, mut bus) = fresh(0x4000);
        cpu.y = 0x05;
        cpu.psw = 0;
        bus.write(0x4000, 0x10);
        let i = info(Mnemonic::DBNZ, AddressingMode::YRelative, 4);
        let cycles = dbnz(&mut cpu, &mut bus, 0xFE, &i);
        assert_eq!(cycles, 6);
        assert_eq!(cpu.y, 0x04);
        assert_eq!(cpu.pc, 0x4001u16.wrapping_add(0x10));
        assert_eq!(cpu.psw, 0, "Y decrement must not set N/Z");
    }

    #[test]
    fn dbnz_y_zero_result_skips_branch() {
        let (mut cpu, mut bus) = fresh(0x4000);
        cpu.y = 0x01; // becomes 0 after dec
        bus.write(0x4000, 0x10);
        let i = info(Mnemonic::DBNZ, AddressingMode::YRelative, 4);
        let cycles = dbnz(&mut cpu, &mut bus, 0xFE, &i);
        assert_eq!(cycles, 4);
        assert_eq!(cpu.y, 0x00);
        assert_eq!(cpu.pc, 0x4001);
    }
}
