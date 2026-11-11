//! Bit-manipulation instructions, `SET1`/`CLR1` on direct-page bits,
//! `NOT1` on any 13-bit-addressable byte, `OR1`/`AND1`/`EOR1`/`MOV1`
//! against the carry flag, and `TSET`/`TCLR` (test-and-set / test-and
//! -clear at `!abs`).
//!
//! 27 opcodes total:
//! - **DP bit set/clear** (16): `SET1 dp.b` (8 bits x 1 mode) + `CLR1
//!   dp.b` (8). The bit index `b` is recovered from `(opcode >> 5) & 7`.
//! - **MemBit RMW** (1): `NOT1 m.b` flips bit `b` at the 13-bit-encoded
//!   address.
//! - **Carry-flag bit ops** (7): `OR1 c, m.b` / `OR1 c, /m.b` (the
//!   `MemBitNot` mode inverts the bit before OR-ing); analogous AND1
//!   pair; `EOR1 c, m.b`; `MOV1 c, m.b` (load C from bit) and
//!   `MOV1 m.b, c` (store C into bit).
//! - **Test-and-set / test-and-clear at `!abs`** (2): `TSET`/`TCLR`
//!   compute `N`/`Z` from `(A - mem)` (no write to A, just flags),
//!   then write back `mem | A` (TSET) or `mem & ~A` (TCLR). `C`, `V`,
//!   and `H` are NEVER touched.
//!
//! The semantics are the ones fullsnes and Anomie's SPC700 notes
//! describe; every opcode passes the SingleStepTests sweep, and the
//! flags were checked against bsnes-plus.

use retro_core::cpu::Addressable;

use super::{FLAG_C, FLAG_N, FLAG_Z};
use crate::addressing::{dp_addr, fetch_mem_bit, fetch_pc_byte, fetch_pc_word};
use crate::decode::OpcodeInfo;
use crate::Spc700;

/// Recover the bit index encoded into the opcode byte for the
/// `SET1`/`CLR1`/`BBS`/`BBC` families: its top three bits.
fn bit_from_opcode(opcode: u8) -> u8 {
    (opcode >> 5) & 7
}

// -- Handlers: SET1 / CLR1 --------------------------------------------------

pub fn set1(cpu: &mut Spc700, bus: &mut dyn Addressable, opcode: u8, info: &OpcodeInfo) -> u32 {
    let bit = bit_from_opcode(opcode);
    let dp = fetch_pc_byte(cpu, bus);
    let addr = dp_addr(cpu, dp);
    let val = bus.read(addr as u32);
    bus.write(addr as u32, val | (1 << bit));
    info.base_cycles as u32
}

pub fn clr1(cpu: &mut Spc700, bus: &mut dyn Addressable, opcode: u8, info: &OpcodeInfo) -> u32 {
    let bit = bit_from_opcode(opcode);
    let dp = fetch_pc_byte(cpu, bus);
    let addr = dp_addr(cpu, dp);
    let val = bus.read(addr as u32);
    bus.write(addr as u32, val & !(1 << bit));
    info.base_cycles as u32
}

// -- Handler: NOT1 m.b ------------------------------------------------------

pub fn not1(cpu: &mut Spc700, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    let (addr, bit) = fetch_mem_bit(cpu, bus);
    let val = bus.read(addr as u32);
    bus.write(addr as u32, val ^ (1 << bit));
    info.base_cycles as u32
}

// -- Handlers: OR1 / AND1 / EOR1 (carry-flag bit ops) -----------------------

/// Read the addressed bit (low position) for the carry-flag bit ops.
/// Returns `1` if the bit is set, `0` otherwise.
fn read_addressed_bit(cpu: &mut Spc700, bus: &mut dyn Addressable) -> u8 {
    let (addr, bit) = fetch_mem_bit(cpu, bus);
    (bus.read(addr as u32) >> bit) & 1
}

/// `OR1 c, m.b` (`0x0A`) / `OR1 c, /m.b` (`0x2A`). The `MemBitNot`
/// variant inverts the addressed bit before OR-ing into C.
pub fn or1(cpu: &mut Spc700, bus: &mut dyn Addressable, opcode: u8, info: &OpcodeInfo) -> u32 {
    let mut bit_val = read_addressed_bit(cpu, bus);
    if opcode == 0x2A {
        bit_val ^= 1;
    }
    let new_c = (cpu.get_flag(FLAG_C) as u8) | bit_val;
    cpu.set_flag(FLAG_C, new_c != 0);
    info.base_cycles as u32
}

/// `AND1 c, m.b` (`0x4A`) / `AND1 c, /m.b` (`0x6A`).
pub fn and1(cpu: &mut Spc700, bus: &mut dyn Addressable, opcode: u8, info: &OpcodeInfo) -> u32 {
    let mut bit_val = read_addressed_bit(cpu, bus);
    if opcode == 0x6A {
        bit_val ^= 1;
    }
    let new_c = (cpu.get_flag(FLAG_C) as u8) & bit_val;
    cpu.set_flag(FLAG_C, new_c != 0);
    info.base_cycles as u32
}

/// `EOR1 c, m.b` (`0x8A`). Only one mode, there is no `/m.b` variant.
pub fn eor1(cpu: &mut Spc700, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    let bit_val = read_addressed_bit(cpu, bus);
    let new_c = (cpu.get_flag(FLAG_C) as u8) ^ bit_val;
    cpu.set_flag(FLAG_C, new_c != 0);
    info.base_cycles as u32
}

// -- Handlers: MOV1 c<->bit ------------------------------------------------

pub fn mov1(cpu: &mut Spc700, bus: &mut dyn Addressable, opcode: u8, info: &OpcodeInfo) -> u32 {
    match opcode {
        // MOV1 C, m.b, load C from the addressed bit.
        0xAA => {
            let bit_val = read_addressed_bit(cpu, bus);
            cpu.set_flag(FLAG_C, bit_val != 0);
        }
        // MOV1 m.b, C, store C into the addressed bit, leaving the
        // other 7 bits of the destination byte alone.
        0xCA => {
            let (addr, bit) = fetch_mem_bit(cpu, bus);
            let val = bus.read(addr as u32);
            let mask = 1u8 << bit;
            let new_val = if cpu.get_flag(FLAG_C) {
                val | mask
            } else {
                val & !mask
            };
            bus.write(addr as u32, new_val);
        }
        _ => unreachable!("mov1 dispatched with non-MOV1 opcode {:02X}", opcode),
    }
    info.base_cycles as u32
}

// -- Handlers: TSET / TCLR --------------------------------------------------

/// `TSET !abs, A` (`0x0E`): compute `N`/`Z` flags from `A - mem` (the
/// difference is discarded), then write `mem | A` back.
pub fn tset(cpu: &mut Spc700, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    let addr = fetch_pc_word(cpu, bus);
    let mem = bus.read(addr as u32);
    let diff = cpu.a.wrapping_sub(mem);
    cpu.set_flag(FLAG_N, diff & 0x80 != 0);
    cpu.set_flag(FLAG_Z, diff == 0);
    bus.write(addr as u32, mem | cpu.a);
    info.base_cycles as u32
}

/// `TCLR !abs, A` (`0x4E`): same flag derivation as TSET, but writes
/// `mem & ~A` back.
pub fn tclr(cpu: &mut Spc700, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    let addr = fetch_pc_word(cpu, bus);
    let mem = bus.read(addr as u32);
    let diff = cpu.a.wrapping_sub(mem);
    cpu.set_flag(FLAG_N, diff & 0x80 != 0);
    cpu.set_flag(FLAG_Z, diff == 0);
    bus.write(addr as u32, mem & !cpu.a);
    info.base_cycles as u32
}

#[cfg(test)]
mod tests {
    use super::super::FLAG_H;
    use super::super::FLAG_V;
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

    fn dummy_info(mnem: Mnemonic, mode: AddressingMode, cycles: u8) -> OpcodeInfo {
        OpcodeInfo {
            mnemonic: mnem,
            mode,
            base_cycles: cycles,
        }
    }

    // -- SET1 / CLR1 ------------------------------------------------------

    #[test]
    fn set1_sets_specific_bit_in_dp() {
        let (mut cpu, mut bus) = fresh(0x2000);
        // SET1 dp.0, opcode 0x02, bit = (0x02 >> 5) & 7 = 0.
        bus.write(0x2000, 0x40); // DP operand
        bus.write(0x0040, 0x00); // DP target byte (initially zero)
        let info = dummy_info(Mnemonic::SET1, AddressingMode::DirectPage, 4);
        let cycles = set1(&mut cpu, &mut bus, 0x02, &info);
        assert_eq!(cycles, 4);
        assert_eq!(bus.read(0x0040), 0x01, "bit 0 should be set");
        // SET1 dp.7, opcode 0xE2, bit = 7.
        cpu.pc = 0x2010;
        bus.write(0x2010, 0x40);
        let info = dummy_info(Mnemonic::SET1, AddressingMode::DirectPage, 4);
        let _ = set1(&mut cpu, &mut bus, 0xE2, &info);
        assert_eq!(bus.read(0x0040), 0x81, "bit 7 should now be set too");
    }

    #[test]
    fn clr1_clears_specific_bit_in_dp() {
        let (mut cpu, mut bus) = fresh(0x2000);
        // CLR1 dp.3, opcode 0x72, bit = (0x72 >> 5) & 7 = 3.
        bus.write(0x2000, 0x42);
        bus.write(0x0042, 0xFF); // all bits set
        let info = dummy_info(Mnemonic::CLR1, AddressingMode::DirectPage, 4);
        let _ = clr1(&mut cpu, &mut bus, 0x72, &info);
        assert_eq!(bus.read(0x0042), 0xF7, "bit 3 should be cleared");
    }

    #[test]
    fn set1_does_not_touch_flags() {
        let (mut cpu, mut bus) = fresh(0x2000);
        cpu.psw = 0xFF; // all flags set
        bus.write(0x2000, 0x10);
        bus.write(0x0010, 0x00);
        // PSW.P=1, so dp_base is $0100. Need to seed there:
        bus.write(0x0110, 0x00);
        let info = dummy_info(Mnemonic::SET1, AddressingMode::DirectPage, 4);
        let _ = set1(&mut cpu, &mut bus, 0x42, &info); // SET1 dp.2
        assert_eq!(cpu.psw, 0xFF, "all flags must survive SET1");
    }

    // -- NOT1 -------------------------------------------------------------

    #[test]
    fn not1_flips_bit_at_mem_bit_address() {
        let (mut cpu, mut bus) = fresh(0x3000);
        // Operand = 0x4123 -> bit = 0x4123 >> 13 = 2, addr = 0x0123.
        bus.write(0x3000, 0x23);
        bus.write(0x3001, 0x41);
        bus.write(0x0123, 0b1111_0000);
        let info = dummy_info(Mnemonic::NOT1, AddressingMode::MemBit, 5);
        let _ = not1(&mut cpu, &mut bus, &info);
        assert_eq!(bus.read(0x0123), 0b1111_0100, "bit 2 should be flipped");
        // Flip again, should restore.
        cpu.pc = 0x3000;
        let info = dummy_info(Mnemonic::NOT1, AddressingMode::MemBit, 5);
        let _ = not1(&mut cpu, &mut bus, &info);
        assert_eq!(bus.read(0x0123), 0b1111_0000);
    }

    // -- MOV1 -------------------------------------------------------------

    #[test]
    fn mov1_c_from_bit_reads_correct_bit() {
        let (mut cpu, mut bus) = fresh(0x3000);
        // Operand = 0x6000 -> bit = 3, addr = 0x0000.
        bus.write(0x3000, 0x00);
        bus.write(0x3001, 0x60);
        bus.write(0x0000, 0b0000_1000); // bit 3 set
        let info = dummy_info(Mnemonic::MOV1, AddressingMode::MemBit, 4);
        let _ = mov1(&mut cpu, &mut bus, 0xAA, &info);
        assert!(cpu.get_flag(FLAG_C), "C should be loaded with bit 3 = 1");

        // Now point at a byte where bit 3 is clear.
        cpu.pc = 0x3000;
        bus.write(0x0000, 0b1111_0111); // bit 3 cleared
        let info = dummy_info(Mnemonic::MOV1, AddressingMode::MemBit, 4);
        let _ = mov1(&mut cpu, &mut bus, 0xAA, &info);
        assert!(!cpu.get_flag(FLAG_C));
    }

    #[test]
    fn mov1_bit_from_c_writes_back_with_other_bits_preserved() {
        let (mut cpu, mut bus) = fresh(0x3000);
        cpu.set_flag(FLAG_C, true);
        // Operand = 0xA000 -> bit = 5, addr = 0x0000.
        bus.write(0x3000, 0x00);
        bus.write(0x3001, 0xA0);
        bus.write(0x0000, 0b1100_0011); // bits 0,1,6,7 set; bit 5 clear
        let info = dummy_info(Mnemonic::MOV1, AddressingMode::MemBit, 6);
        let _ = mov1(&mut cpu, &mut bus, 0xCA, &info);
        assert_eq!(
            bus.read(0x0000),
            0b1110_0011,
            "bit 5 set from C, others untouched"
        );

        // Clear C and rewrite.
        cpu.pc = 0x3000;
        cpu.set_flag(FLAG_C, false);
        let info = dummy_info(Mnemonic::MOV1, AddressingMode::MemBit, 6);
        let _ = mov1(&mut cpu, &mut bus, 0xCA, &info);
        assert_eq!(bus.read(0x0000), 0b1100_0011, "bit 5 cleared from C");
    }

    // -- OR1 / AND1 / EOR1 ------------------------------------------------

    #[test]
    fn or1_inverted_variant_uses_complement_of_bit() {
        let (mut cpu, mut bus) = fresh(0x3000);
        // bit cleared in memory -> non-inverted OR with C=0 stays 0.
        // Operand = 0x0000 -> bit = 0, addr = 0x0000. Byte = 0x00 (bit 0 = 0).
        bus.write(0x3000, 0x00);
        bus.write(0x3001, 0x00);
        bus.write(0x0000, 0x00);
        cpu.set_flag(FLAG_C, false);
        let info = dummy_info(Mnemonic::OR1, AddressingMode::MemBit, 5);
        let _ = or1(&mut cpu, &mut bus, 0x0A, &info);
        assert!(!cpu.get_flag(FLAG_C), "0 | 0 = 0");

        // Same data, but inverted-bit variant (`0x2A` c,/m.b) inverts
        // the read bit to 1, then ORs: 0 | 1 = 1.
        cpu.pc = 0x3000;
        cpu.set_flag(FLAG_C, false);
        let info = dummy_info(Mnemonic::OR1, AddressingMode::MemBitNot, 5);
        let _ = or1(&mut cpu, &mut bus, 0x2A, &info);
        assert!(cpu.get_flag(FLAG_C), "0 | !0 = 1");
    }

    #[test]
    fn and1_inverted_variant_uses_complement_of_bit() {
        let (mut cpu, mut bus) = fresh(0x3000);
        // bit SET in memory -> AND1 c,m.b with C=1 stays 1.
        bus.write(0x3000, 0x00);
        bus.write(0x3001, 0x00);
        bus.write(0x0000, 0x01); // bit 0 = 1
        cpu.set_flag(FLAG_C, true);
        let info = dummy_info(Mnemonic::AND1, AddressingMode::MemBit, 4);
        let _ = and1(&mut cpu, &mut bus, 0x4A, &info);
        assert!(cpu.get_flag(FLAG_C), "1 & 1 = 1");

        // Inverted-bit variant (`0x6A` c,/m.b): C=1 AND !1 = 0.
        cpu.pc = 0x3000;
        cpu.set_flag(FLAG_C, true);
        let info = dummy_info(Mnemonic::AND1, AddressingMode::MemBitNot, 4);
        let _ = and1(&mut cpu, &mut bus, 0x6A, &info);
        assert!(!cpu.get_flag(FLAG_C), "1 & !1 = 0");
    }

    #[test]
    fn eor1_xors_carry_with_addressed_bit() {
        let (mut cpu, mut bus) = fresh(0x3000);
        bus.write(0x3000, 0x00);
        bus.write(0x3001, 0x00);
        bus.write(0x0000, 0x01);
        cpu.set_flag(FLAG_C, true);
        let info = dummy_info(Mnemonic::EOR1, AddressingMode::MemBit, 5);
        let _ = eor1(&mut cpu, &mut bus, &info);
        assert!(!cpu.get_flag(FLAG_C), "1 ^ 1 = 0");
    }

    // -- TSET / TCLR ------------------------------------------------------

    #[test]
    fn tset_n_z_from_a_minus_mem_writes_or() {
        let (mut cpu, mut bus) = fresh(0x3000);
        cpu.a = 0xAA;
        // operand = $1234, mem = 0x55.
        bus.write(0x3000, 0x34);
        bus.write(0x3001, 0x12);
        bus.write(0x1234, 0x55);
        // Pre-set V/H to confirm they survive.
        cpu.set_flag(FLAG_V, true);
        cpu.set_flag(FLAG_H, true);
        let info = dummy_info(Mnemonic::TSET, AddressingMode::Absolute, 6);
        let _ = tset(&mut cpu, &mut bus, &info);
        // diff = 0xAA - 0x55 = 0x55 -> N=0, Z=0.
        assert!(!cpu.get_flag(FLAG_N));
        assert!(!cpu.get_flag(FLAG_Z));
        // V/H must survive.
        assert!(cpu.get_flag(FLAG_V));
        assert!(cpu.get_flag(FLAG_H));
        // mem now = 0xAA | 0x55 = 0xFF.
        assert_eq!(bus.read(0x1234), 0xFF);
    }

    #[test]
    fn tset_zero_diff_sets_z_when_a_equals_mem() {
        let (mut cpu, mut bus) = fresh(0x3000);
        cpu.a = 0x42;
        bus.write(0x3000, 0x00);
        bus.write(0x3001, 0x40);
        bus.write(0x4000, 0x42); // mem == A
        let info = dummy_info(Mnemonic::TSET, AddressingMode::Absolute, 6);
        let _ = tset(&mut cpu, &mut bus, &info);
        assert!(cpu.get_flag(FLAG_Z), "A==mem -> diff=0 -> Z=1");
        assert!(!cpu.get_flag(FLAG_N));
        // Write back is mem | A = 0x42 (idempotent here).
        assert_eq!(bus.read(0x4000), 0x42);
    }

    #[test]
    fn tclr_n_z_from_a_minus_mem_writes_and_not() {
        let (mut cpu, mut bus) = fresh(0x3000);
        cpu.a = 0x0F;
        bus.write(0x3000, 0x00);
        bus.write(0x3001, 0x50);
        bus.write(0x5000, 0xFF);
        let info = dummy_info(Mnemonic::TCLR, AddressingMode::Absolute, 6);
        let _ = tclr(&mut cpu, &mut bus, &info);
        // diff = 0x0F - 0xFF = 0x10 (wrapping) -> N=0, Z=0.
        // mem & ~A = 0xFF & 0xF0 = 0xF0.
        assert_eq!(bus.read(0x5000), 0xF0);
        assert!(!cpu.get_flag(FLAG_Z));
        assert!(!cpu.get_flag(FLAG_N));
    }

    #[test]
    fn tclr_negative_diff_sets_n() {
        let (mut cpu, mut bus) = fresh(0x3000);
        cpu.a = 0x10;
        bus.write(0x3000, 0x00);
        bus.write(0x3001, 0x60);
        bus.write(0x6000, 0x20); // mem > A -> diff negative
        let info = dummy_info(Mnemonic::TCLR, AddressingMode::Absolute, 6);
        let _ = tclr(&mut cpu, &mut bus, &info);
        // diff = 0x10 - 0x20 = 0xF0 -> N=1.
        assert!(cpu.get_flag(FLAG_N));
        assert!(!cpu.get_flag(FLAG_Z));
        // mem & ~A = 0x20 & 0xEF = 0x20.
        assert_eq!(bus.read(0x6000), 0x20);
    }
}
