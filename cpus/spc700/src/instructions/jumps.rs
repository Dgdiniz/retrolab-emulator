//! Unconditional jump and call/return instructions.
//!
//! 21 opcodes total:
//! - **JMP** x 2: `!abs` (`0x5F`, 3 cycles), copy the 16-bit operand
//!   into `PC`; `[!abs+X]` (`0x1F`, 6 cycles), read the 16-bit
//!   pointer at `(operand + X)` and jump there.
//! - **CALL !abs** (`0x3F`, 8 cycles): push the return address (PCH
//!   first, then PCL, i.e. the byte order such that a subsequent
//!   `RET` reads them back in PCL/PCH order via two pre-incremented
//!   pulls), then jump to `!abs`.
//! - **PCALL u** (`0x4F`, 6 cycles): same push pattern as `CALL`, but
//!   the target is `$FF00 | u`. Used for "page-zero callbacks" the
//!   IPL ROM exposes.
//! - **TCALL n** x 16 (`0x01`, `0x11`, ..., `0xF1`, all 8 cycles): same
//!   push pattern as `CALL`; target is the 16-bit pointer stored at
//!   `$FFDE - (n << 1)`. `n` is recovered from `(opcode >> 4) & 0xF`.
//! - **RET** (`0x6F`, 5 cycles): pull PCL first, then PCH.
//! - **RETI** (`0x7F`, 6 cycles): pull PSW first, then PCL, then PCH.
//!
//! `CALL`/`PCALL`/`TCALL` push the address of the *byte after* the
//! call instruction (the 16-bit operand has already been consumed by
//! the time the push runs), so the matching `RET` lands the caller at
//! the right place. No flags are touched by any of these opcodes
//! (`RETI` of course replaces all of PSW from the stack).

use retro_core::cpu::Addressable;

use crate::addressing::{fetch_pc_byte, fetch_pc_word};
use crate::decode::OpcodeInfo;
use crate::Spc700;

/// Push the 16-bit value as PCH first, then PCL, the canonical
/// `CALL`/`BRK` order. The matching `pull_pc` below pulls in reverse
/// (PCL first, then PCH) thanks to SP being pre-incremented.
fn push_pc(cpu: &mut Spc700, bus: &mut dyn Addressable, addr: u16) {
    cpu.push_byte(bus, (addr >> 8) as u8);
    cpu.push_byte(bus, (addr & 0xFF) as u8);
}

/// Pull PCL first, then PCH (matching the `push_pc` order above).
fn pull_pc(cpu: &mut Spc700, bus: &mut dyn Addressable) -> u16 {
    let lo = cpu.pull_byte(bus) as u16;
    let hi = cpu.pull_byte(bus) as u16;
    (hi << 8) | lo
}

// -- JMP -----------------------------------------------------------------

/// Dispatch for the two JMP encodings (`!abs` and `[!abs+X]`). The
/// indirect variant reads a 16-bit pointer at `operand + X` (no DP /
/// page wrap, the addition is straight 16-bit).
pub fn jmp(cpu: &mut Spc700, bus: &mut dyn Addressable, opcode: u8, info: &OpcodeInfo) -> u32 {
    match opcode {
        0x5F => {
            let target = fetch_pc_word(cpu, bus);
            cpu.pc = target;
        }
        0x1F => {
            let base = fetch_pc_word(cpu, bus);
            let ptr_addr = base.wrapping_add(cpu.x as u16);
            let lo = bus.read(ptr_addr as u32) as u16;
            let hi = bus.read(ptr_addr.wrapping_add(1) as u32) as u16;
            cpu.pc = (hi << 8) | lo;
        }
        _ => panic!("spc700: jmp dispatched with non-JMP opcode {:02X}", opcode),
    }
    info.base_cycles as u32
}

// -- CALL ----------------------------------------------------------------

pub fn call(cpu: &mut Spc700, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    let target = fetch_pc_word(cpu, bus);
    push_pc(cpu, bus, cpu.pc);
    cpu.pc = target;
    info.base_cycles as u32
}

// -- PCALL ---------------------------------------------------------------

pub fn pcall(cpu: &mut Spc700, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    let u = fetch_pc_byte(cpu, bus);
    let target = 0xFF00u16 | (u as u16);
    push_pc(cpu, bus, cpu.pc);
    cpu.pc = target;
    info.base_cycles as u32
}

// -- TCALL ---------------------------------------------------------------

/// `TCALL n` (n = 0..=15): vector lives at `$FFDE - (n << 1)`. We
/// push the return PC (which is already past the 1-byte opcode) then
/// read the 16-bit vector and jump to it. `n=0` -> `$FFDE`, `n=15` ->
/// `$FFC0`. Same low nibble pattern as `tcall`: opcode `(n << 4) | 1`.
pub fn tcall(cpu: &mut Spc700, bus: &mut dyn Addressable, opcode: u8, info: &OpcodeInfo) -> u32 {
    let n = (opcode >> 4) & 0x0F;
    push_pc(cpu, bus, cpu.pc);
    let vector = 0xFFDEu16.wrapping_sub((n as u16) << 1);
    let lo = bus.read(vector as u32) as u16;
    let hi = bus.read(vector.wrapping_add(1) as u32) as u16;
    cpu.pc = (hi << 8) | lo;
    info.base_cycles as u32
}

// -- RET / RETI ----------------------------------------------------------

pub fn ret(cpu: &mut Spc700, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    cpu.pc = pull_pc(cpu, bus);
    info.base_cycles as u32
}

/// `RETI` pulls PSW *first*, then PCL, then PCH, i.e. the reverse of
/// `BRK`'s push order (which is PCH, PCL, PSW). No flag massaging
/// here: PSW comes back exactly as `BRK` saved it.
pub fn reti(cpu: &mut Spc700, bus: &mut dyn Addressable, info: &OpcodeInfo) -> u32 {
    cpu.psw = cpu.pull_byte(bus);
    cpu.pc = pull_pc(cpu, bus);
    info.base_cycles as u32
}

#[cfg(test)]
mod tests {
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

    fn fresh(pc: u16, sp: u8) -> (Spc700, TinyBus) {
        let mut cpu = Spc700::new();
        cpu.pc = pc;
        cpu.sp = sp;
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

    // -- JMP --------------------------------------------------------------

    #[test]
    fn jmp_abs_loads_operand_into_pc() {
        let (mut cpu, mut bus) = fresh(0x2000, 0xEF);
        bus.write(0x2000, 0x34);
        bus.write(0x2001, 0x12);
        let i = info(Mnemonic::JMP, AddressingMode::Absolute, 3);
        let cycles = jmp(&mut cpu, &mut bus, 0x5F, &i);
        assert_eq!(cycles, 3);
        assert_eq!(cpu.pc, 0x1234);
    }

    #[test]
    fn jmp_abs_indirect_x_uses_pointer_at_base_plus_x() {
        let (mut cpu, mut bus) = fresh(0x2000, 0xEF);
        cpu.x = 0x02;
        bus.write(0x2000, 0x00); // base low
        bus.write(0x2001, 0x40); // base high -> 0x4000
                                 // Pointer at 0x4002 -> 0xDEAD
        bus.write(0x4002, 0xAD);
        bus.write(0x4003, 0xDE);
        let i = info(Mnemonic::JMP, AddressingMode::AbsoluteIndirectX, 6);
        let cycles = jmp(&mut cpu, &mut bus, 0x1F, &i);
        assert_eq!(cycles, 6);
        assert_eq!(cpu.pc, 0xDEAD);
    }

    // -- CALL -------------------------------------------------------------

    #[test]
    fn call_pushes_return_pc_then_jumps_to_target() {
        let (mut cpu, mut bus) = fresh(0x3000, 0xEF);
        bus.write(0x3000, 0xCD);
        bus.write(0x3001, 0xAB); // target = 0xABCD
        let i = info(Mnemonic::CALL, AddressingMode::Absolute, 8);
        let cycles = call(&mut cpu, &mut bus, &i);
        assert_eq!(cycles, 8);
        assert_eq!(cpu.pc, 0xABCD);
        // Return PC = 0x3002. Pushed PCH first ($EF), then PCL ($EE).
        // After both pushes SP = 0xED.
        assert_eq!(cpu.sp, 0xED);
        assert_eq!(bus.read(0x01EF), 0x30, "PCH at higher stack slot");
        assert_eq!(bus.read(0x01EE), 0x02, "PCL at lower stack slot");
    }

    // -- PCALL ------------------------------------------------------------

    #[test]
    fn pcall_targets_ff00_plus_immediate() {
        let (mut cpu, mut bus) = fresh(0x3000, 0xEF);
        bus.write(0x3000, 0x42);
        let i = info(Mnemonic::PCALL, AddressingMode::PCallVector, 6);
        let cycles = pcall(&mut cpu, &mut bus, &i);
        assert_eq!(cycles, 6);
        assert_eq!(cpu.pc, 0xFF42);
        assert_eq!(cpu.sp, 0xED);
        // Return PC = 0x3001.
        assert_eq!(bus.read(0x01EF), 0x30);
        assert_eq!(bus.read(0x01EE), 0x01);
    }

    // -- TCALL ------------------------------------------------------------

    #[test]
    fn tcall_n0_jumps_to_vector_at_ffde() {
        let (mut cpu, mut bus) = fresh(0x3000, 0xEF);
        bus.write(0xFFDE, 0x12);
        bus.write(0xFFDF, 0x34);
        let i = info(Mnemonic::TCALL, AddressingMode::TCallVector, 8);
        let cycles = tcall(&mut cpu, &mut bus, 0x01, &i); // n=0
        assert_eq!(cycles, 8);
        assert_eq!(cpu.pc, 0x3412);
        // Return PC = 0x3000 (TCALL is 1 byte, so PC was already
        // advanced past the opcode by step()).
        assert_eq!(bus.read(0x01EF), 0x30);
        assert_eq!(bus.read(0x01EE), 0x00);
    }

    #[test]
    fn tcall_n15_jumps_to_vector_at_ffc0() {
        let (mut cpu, mut bus) = fresh(0x3000, 0xEF);
        bus.write(0xFFC0, 0xAD);
        bus.write(0xFFC1, 0xDE);
        let i = info(Mnemonic::TCALL, AddressingMode::TCallVector, 8);
        let _ = tcall(&mut cpu, &mut bus, 0xF1, &i); // n=15
        assert_eq!(cpu.pc, 0xDEAD);
    }

    // -- RET --------------------------------------------------------------

    #[test]
    fn ret_pulls_pc_in_pcl_pch_order() {
        let (mut cpu, mut bus) = fresh(0x0000, 0xED);
        // Stack from a prior CALL: PCH at $1EF, PCL at $1EE.
        bus.write(0x01EE, 0x42);
        bus.write(0x01EF, 0x30);
        let i = info(Mnemonic::RET, AddressingMode::Implied, 5);
        let cycles = ret(&mut cpu, &mut bus, &i);
        assert_eq!(cycles, 5);
        assert_eq!(cpu.pc, 0x3042);
        assert_eq!(cpu.sp, 0xEF, "two pre-increments restore SP");
    }

    // -- RETI -------------------------------------------------------------

    #[test]
    fn reti_restores_psw_then_pc() {
        let (mut cpu, mut bus) = fresh(0x0000, 0xEC);
        // Stack from a prior BRK: PSW at $1ED, PCL at $1EE, PCH at $1EF.
        bus.write(0x01ED, 0xA5);
        bus.write(0x01EE, 0x78);
        bus.write(0x01EF, 0x56);
        let i = info(Mnemonic::RETI, AddressingMode::Implied, 6);
        let cycles = reti(&mut cpu, &mut bus, &i);
        assert_eq!(cycles, 6);
        assert_eq!(cpu.psw, 0xA5);
        assert_eq!(cpu.pc, 0x5678);
        assert_eq!(cpu.sp, 0xEF);
    }
}
