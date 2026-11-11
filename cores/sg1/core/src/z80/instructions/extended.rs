use crate::z80::{InstructionResult, Z80};
use retro_core::cpu::Addressable;

/// Block Load: LDI (ED A0)
/// Load (DE) <- (HL), increment both, decrement BC
/// Flags: H=0, P/V=(BC!=0 after dec), N=0, X/Y from A+value
pub(crate) fn ldi(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let value = bus.read(cpu.hl() as u32);
    bus.write(cpu.de() as u32, value);

    let hl = cpu.hl().wrapping_add(1);
    let de = cpu.de().wrapping_add(1);
    let bc = cpu.bc().wrapping_sub(1);

    cpu.set_hl(hl);
    cpu.set_de(de);
    cpu.set_bc(bc);

    // Undocumented X/Y flags: n = A + transferred_byte
    let n = cpu.a.wrapping_add(value);
    cpu.set_flag_h(false);
    cpu.set_flag_n(false);
    cpu.set_flag_pv(bc != 0);
    cpu.set_flag_y(n & 0x02 != 0); // bit 1 of n
    cpu.set_flag_x(n & 0x08 != 0); // bit 3 of n

    InstructionResult::new(16)
}

/// Block Load: LDD (ED A8)
/// Load (DE) <- (HL), decrement both, decrement BC
/// Flags: H=0, P/V=(BC!=0 after dec), N=0, X/Y from A+value
pub(crate) fn ldd(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let value = bus.read(cpu.hl() as u32);
    bus.write(cpu.de() as u32, value);

    let hl = cpu.hl().wrapping_sub(1);
    let de = cpu.de().wrapping_sub(1);
    let bc = cpu.bc().wrapping_sub(1);

    cpu.set_hl(hl);
    cpu.set_de(de);
    cpu.set_bc(bc);

    // Undocumented X/Y flags: n = A + transferred_byte
    let n = cpu.a.wrapping_add(value);
    cpu.set_flag_h(false);
    cpu.set_flag_n(false);
    cpu.set_flag_pv(bc != 0);
    cpu.set_flag_y(n & 0x02 != 0); // bit 1 of n
    cpu.set_flag_x(n & 0x08 != 0); // bit 3 of n

    InstructionResult::new(16)
}

/// Block Load Repeat: LDIR (ED B0)
/// Repeat LDI until BC=0
/// Flags: H=0, N=0; P/V=1 on each repeated step, 0 once BC=0
pub(crate) fn ldir(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let value = bus.read(cpu.hl() as u32);
    bus.write(cpu.de() as u32, value);

    let hl = cpu.hl().wrapping_add(1);
    let de = cpu.de().wrapping_add(1);
    let bc = cpu.bc().wrapping_sub(1);

    cpu.set_hl(hl);
    cpu.set_de(de);
    cpu.set_bc(bc);

    // Undocumented X/Y flags: n = A + transferred_byte
    let n = cpu.a.wrapping_add(value);
    cpu.set_flag_h(false);
    cpu.set_flag_n(false);
    cpu.set_flag_y(n & 0x02 != 0);
    cpu.set_flag_x(n & 0x08 != 0);

    if bc != 0 {
        cpu.pc = cpu.pc.wrapping_sub(2);
        cpu.set_flag_pv(true);
        // WZ = PC + 1 (after the decrement, so PC is at instruction start)
        cpu.wz = cpu.pc.wrapping_add(1);
        // Undocumented: extra 5 T-states overwrite X/Y with PC bits
        cpu.set_flag_y(cpu.pc & 0x2000 != 0); // PC.13
        cpu.set_flag_x(cpu.pc & 0x0800 != 0); // PC.11
        InstructionResult::new(21)
    } else {
        cpu.set_flag_pv(false);
        InstructionResult::new(16)
    }
}

/// Block Load Repeat: LDDR (ED B8)
/// Repeat LDD until BC=0
/// Flags: H=0, N=0; P/V=1 on each repeated step, 0 once BC=0
pub(crate) fn lddr(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let value = bus.read(cpu.hl() as u32);
    bus.write(cpu.de() as u32, value);

    let hl = cpu.hl().wrapping_sub(1);
    let de = cpu.de().wrapping_sub(1);
    let bc = cpu.bc().wrapping_sub(1);

    cpu.set_hl(hl);
    cpu.set_de(de);
    cpu.set_bc(bc);

    // Undocumented X/Y flags: n = A + transferred_byte
    let n = cpu.a.wrapping_add(value);
    cpu.set_flag_h(false);
    cpu.set_flag_n(false);
    cpu.set_flag_y(n & 0x02 != 0);
    cpu.set_flag_x(n & 0x08 != 0);

    if bc != 0 {
        cpu.pc = cpu.pc.wrapping_sub(2);
        cpu.set_flag_pv(true);
        // WZ = PC + 1 (after the decrement, so PC is at instruction start)
        cpu.wz = cpu.pc.wrapping_add(1);
        // Undocumented: extra 5 T-states overwrite X/Y with PC bits
        cpu.set_flag_y(cpu.pc & 0x2000 != 0); // PC.13
        cpu.set_flag_x(cpu.pc & 0x0800 != 0); // PC.11
        InstructionResult::new(21)
    } else {
        cpu.set_flag_pv(false);
        InstructionResult::new(16)
    }
}

/// Block Compare: CPI (ED A1)
/// Compare A with (HL), increment HL, decrement BC
/// Flags: S, Z, H (from subtraction), P/V=(BC!=0 after), N=1
pub(crate) fn cpi(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let value = bus.read(cpu.hl() as u32);
    let result = cpu.a.wrapping_sub(value);
    let half_carry = (cpu.a & 0x0F) < (value & 0x0F);

    let hl = cpu.hl().wrapping_add(1);
    let bc = cpu.bc().wrapping_sub(1);

    cpu.set_hl(hl);
    cpu.set_bc(bc);

    // Undocumented X/Y: n = A - value - H (where H is half-carry)
    let n = result.wrapping_sub(if half_carry { 1 } else { 0 });

    cpu.set_flag_s(result & 0x80 != 0);
    cpu.set_flag_z(result == 0);
    cpu.set_flag_h(half_carry);
    cpu.set_flag_pv(bc != 0);
    cpu.set_flag_n(true);
    cpu.set_flag_y(n & 0x02 != 0); // bit 1 of n
    cpu.set_flag_x(n & 0x08 != 0); // bit 3 of n

    // WZ = WZ + 1
    cpu.wz = cpu.wz.wrapping_add(1);

    InstructionResult::new(16)
}

/// Block Compare: CPD (ED A9)
/// Compare A with (HL), decrement HL, decrement BC
/// Flags: S, Z, H (from subtraction), P/V=(BC!=0 after), N=1
pub(crate) fn cpd(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let value = bus.read(cpu.hl() as u32);
    let result = cpu.a.wrapping_sub(value);
    let half_carry = (cpu.a & 0x0F) < (value & 0x0F);

    let hl = cpu.hl().wrapping_sub(1);
    let bc = cpu.bc().wrapping_sub(1);

    cpu.set_hl(hl);
    cpu.set_bc(bc);

    // Undocumented X/Y: n = A - value - H
    let n = result.wrapping_sub(if half_carry { 1 } else { 0 });

    cpu.set_flag_s(result & 0x80 != 0);
    cpu.set_flag_z(result == 0);
    cpu.set_flag_h(half_carry);
    cpu.set_flag_pv(bc != 0);
    cpu.set_flag_n(true);
    cpu.set_flag_y(n & 0x02 != 0);
    cpu.set_flag_x(n & 0x08 != 0);

    // WZ = WZ - 1
    cpu.wz = cpu.wz.wrapping_sub(1);

    InstructionResult::new(16)
}

/// Block Compare Repeat: CPIR (ED B1)
/// Repeat CPI until BC=0 or Z flag is set (match found)
/// Flags: S, Z, H, P/V=(BC!=0 after), N=1
pub(crate) fn cpir(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let value = bus.read(cpu.hl() as u32);
    let result = cpu.a.wrapping_sub(value);
    let half_carry = (cpu.a & 0x0F) < (value & 0x0F);

    let hl = cpu.hl().wrapping_add(1);
    let bc = cpu.bc().wrapping_sub(1);

    cpu.set_hl(hl);
    cpu.set_bc(bc);

    // Undocumented X/Y: n = A - value - H
    let n = result.wrapping_sub(if half_carry { 1 } else { 0 });

    cpu.set_flag_s(result & 0x80 != 0);
    cpu.set_flag_z(result == 0);
    cpu.set_flag_h(half_carry);
    cpu.set_flag_n(true);
    cpu.set_flag_y(n & 0x02 != 0);
    cpu.set_flag_x(n & 0x08 != 0);

    let match_found = result == 0;

    if bc != 0 && !match_found {
        cpu.pc = cpu.pc.wrapping_sub(2);
        cpu.set_flag_pv(true);
        // WZ = PC + 1 (after decrement)
        cpu.wz = cpu.pc.wrapping_add(1);
        // Undocumented: extra 5 T-states overwrite X/Y with PC bits
        cpu.set_flag_y(cpu.pc & 0x2000 != 0); // PC.13
        cpu.set_flag_x(cpu.pc & 0x0800 != 0); // PC.11
        InstructionResult::new(21)
    } else {
        cpu.set_flag_pv(bc != 0);
        // WZ = WZ + 1 (same as CPI when not repeating)
        cpu.wz = cpu.wz.wrapping_add(1);
        InstructionResult::new(16)
    }
}

/// Block Compare Repeat: CPDR (ED B9)
/// Repeat CPD until BC=0 or Z flag is set (match found)
/// Flags: S, Z, H, P/V=(BC!=0 after), N=1
pub(crate) fn cpdr(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let value = bus.read(cpu.hl() as u32);
    let result = cpu.a.wrapping_sub(value);
    let half_carry = (cpu.a & 0x0F) < (value & 0x0F);

    let hl = cpu.hl().wrapping_sub(1);
    let bc = cpu.bc().wrapping_sub(1);

    cpu.set_hl(hl);
    cpu.set_bc(bc);

    // Undocumented X/Y: n = A - value - H
    let n = result.wrapping_sub(if half_carry { 1 } else { 0 });

    cpu.set_flag_s(result & 0x80 != 0);
    cpu.set_flag_z(result == 0);
    cpu.set_flag_h(half_carry);
    cpu.set_flag_n(true);
    cpu.set_flag_y(n & 0x02 != 0);
    cpu.set_flag_x(n & 0x08 != 0);

    let match_found = result == 0;

    if bc != 0 && !match_found {
        cpu.pc = cpu.pc.wrapping_sub(2);
        cpu.set_flag_pv(true);
        // WZ = PC + 1 (after decrement)
        cpu.wz = cpu.pc.wrapping_add(1);
        // Undocumented: extra 5 T-states overwrite X/Y with PC bits
        cpu.set_flag_y(cpu.pc & 0x2000 != 0); // PC.13
        cpu.set_flag_x(cpu.pc & 0x0800 != 0); // PC.11
        InstructionResult::new(21)
    } else {
        cpu.set_flag_pv(bc != 0);
        // WZ = WZ - 1 (same as CPD when not repeating)
        cpu.wz = cpu.wz.wrapping_sub(1);
        InstructionResult::new(16)
    }
}

/// 16-bit Load Extended: LD (nn), BC (ED 43 nn nn)
pub(crate) fn ld_nn_bc(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let addr_low = bus.read(cpu.pc as u32);
    cpu.pc = cpu.pc.wrapping_add(1);
    let addr_high = bus.read(cpu.pc as u32);
    cpu.pc = cpu.pc.wrapping_add(1);
    let addr = u16::from_le_bytes([addr_low, addr_high]);

    let bc = cpu.bc();
    bus.write(addr as u32, (bc & 0xFF) as u8);
    bus.write(addr.wrapping_add(1) as u32, (bc >> 8) as u8);

    cpu.wz = addr.wrapping_add(1);

    InstructionResult::new(20)
}

/// 16-bit Load Extended: LD (nn), DE (ED 53 nn nn)
pub(crate) fn ld_nn_de(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let addr_low = bus.read(cpu.pc as u32);
    cpu.pc = cpu.pc.wrapping_add(1);
    let addr_high = bus.read(cpu.pc as u32);
    cpu.pc = cpu.pc.wrapping_add(1);
    let addr = u16::from_le_bytes([addr_low, addr_high]);

    let de = cpu.de();
    bus.write(addr as u32, (de & 0xFF) as u8);
    bus.write(addr.wrapping_add(1) as u32, (de >> 8) as u8);

    cpu.wz = addr.wrapping_add(1);

    InstructionResult::new(20)
}

/// 16-bit Load Extended: LD (nn), HL (ED 63 nn nn)
/// Note: This is the ED prefix version; there's also 0x22 direct version
pub(crate) fn ld_nn_hl_ed(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let addr_low = bus.read(cpu.pc as u32);
    cpu.pc = cpu.pc.wrapping_add(1);
    let addr_high = bus.read(cpu.pc as u32);
    cpu.pc = cpu.pc.wrapping_add(1);
    let addr = u16::from_le_bytes([addr_low, addr_high]);

    let hl = cpu.hl();
    bus.write(addr as u32, (hl & 0xFF) as u8);
    bus.write(addr.wrapping_add(1) as u32, (hl >> 8) as u8);

    cpu.wz = addr.wrapping_add(1);

    InstructionResult::new(20)
}

/// 16-bit Load Extended: LD (nn), SP (ED 73 nn nn)
pub(crate) fn ld_nn_sp(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let addr_low = bus.read(cpu.pc as u32);
    cpu.pc = cpu.pc.wrapping_add(1);
    let addr_high = bus.read(cpu.pc as u32);
    cpu.pc = cpu.pc.wrapping_add(1);
    let addr = u16::from_le_bytes([addr_low, addr_high]);

    bus.write(addr as u32, (cpu.sp & 0xFF) as u8);
    bus.write(addr.wrapping_add(1) as u32, (cpu.sp >> 8) as u8);

    cpu.wz = addr.wrapping_add(1);

    InstructionResult::new(20)
}

/// 16-bit Load Extended: LD BC, (nn) (ED 4B nn nn)
pub(crate) fn ld_bc_nn(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let addr_low = bus.read(cpu.pc as u32);
    cpu.pc = cpu.pc.wrapping_add(1);
    let addr_high = bus.read(cpu.pc as u32);
    cpu.pc = cpu.pc.wrapping_add(1);
    let addr = u16::from_le_bytes([addr_low, addr_high]);

    let low = bus.read(addr as u32);
    let high = bus.read(addr.wrapping_add(1) as u32);
    let value = u16::from_le_bytes([low, high]);

    cpu.set_bc(value);
    cpu.wz = addr.wrapping_add(1);

    InstructionResult::new(20)
}

/// 16-bit Load Extended: LD DE, (nn) (ED 5B nn nn)
pub(crate) fn ld_de_nn(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let addr_low = bus.read(cpu.pc as u32);
    cpu.pc = cpu.pc.wrapping_add(1);
    let addr_high = bus.read(cpu.pc as u32);
    cpu.pc = cpu.pc.wrapping_add(1);
    let addr = u16::from_le_bytes([addr_low, addr_high]);

    let low = bus.read(addr as u32);
    let high = bus.read(addr.wrapping_add(1) as u32);
    let value = u16::from_le_bytes([low, high]);

    cpu.set_de(value);
    cpu.wz = addr.wrapping_add(1);

    InstructionResult::new(20)
}

/// 16-bit Load Extended: LD HL, (nn) (ED 6B nn nn)
/// Note: This is the ED prefix version; there's also 0x2A direct version
pub(crate) fn ld_hl_nn_ed(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let addr_low = bus.read(cpu.pc as u32);
    cpu.pc = cpu.pc.wrapping_add(1);
    let addr_high = bus.read(cpu.pc as u32);
    cpu.pc = cpu.pc.wrapping_add(1);
    let addr = u16::from_le_bytes([addr_low, addr_high]);

    let low = bus.read(addr as u32);
    let high = bus.read(addr.wrapping_add(1) as u32);
    let value = u16::from_le_bytes([low, high]);

    cpu.set_hl(value);
    cpu.wz = addr.wrapping_add(1);

    InstructionResult::new(20)
}

/// 16-bit Load Extended: LD SP, (nn) (ED 7B nn nn)
pub(crate) fn ld_sp_nn(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let addr_low = bus.read(cpu.pc as u32);
    cpu.pc = cpu.pc.wrapping_add(1);
    let addr_high = bus.read(cpu.pc as u32);
    cpu.pc = cpu.pc.wrapping_add(1);
    let addr = u16::from_le_bytes([addr_low, addr_high]);

    let low = bus.read(addr as u32);
    let high = bus.read(addr.wrapping_add(1) as u32);
    let value = u16::from_le_bytes([low, high]);

    cpu.sp = value;
    cpu.wz = addr.wrapping_add(1);

    InstructionResult::new(20)
}

/// Interrupt Mode: IM 0 (ED 46)
/// Set interrupt mode to 0 (execute instruction on data bus)
pub(crate) fn im_0(cpu: &mut Z80) -> InstructionResult {
    cpu.im = 0;
    InstructionResult::new(8)
}

/// Interrupt Mode: IM 1 (ED 56)
/// Set interrupt mode to 1 (RST 38h)
pub(crate) fn im_1(cpu: &mut Z80) -> InstructionResult {
    cpu.im = 1;
    InstructionResult::new(8)
}

/// Interrupt Mode: IM 2 (ED 5E)
/// Set interrupt mode to 2 (vectored interrupt via I register)
pub(crate) fn im_2(cpu: &mut Z80) -> InstructionResult {
    cpu.im = 2;
    InstructionResult::new(8)
}

/// Return from Interrupt: RETI (ED 4D)
/// Return from maskable interrupt (signals I/O devices)
pub(crate) fn reti(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    // Pop PC from stack
    let low = bus.read(cpu.sp as u32);
    cpu.sp = cpu.sp.wrapping_add(1);
    let high = bus.read(cpu.sp as u32);
    cpu.sp = cpu.sp.wrapping_add(1);

    cpu.pc = u16::from_le_bytes([low, high]);
    cpu.wz = cpu.pc;
    cpu.iff1 = cpu.iff2;

    // RETI signals to I/O devices that interrupt is finished
    // (in a real system, this would affect daisy-chain interrupt controller)

    InstructionResult::new(14)
}

/// Return from NMI: RETN (ED 45)
/// Return from non-maskable interrupt, restore IFF1 from IFF2
pub(crate) fn retn(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    // Pop PC from stack
    let low = bus.read(cpu.sp as u32);
    cpu.sp = cpu.sp.wrapping_add(1);
    let high = bus.read(cpu.sp as u32);
    cpu.sp = cpu.sp.wrapping_add(1);

    cpu.pc = u16::from_le_bytes([low, high]);
    cpu.wz = cpu.pc;

    // Restore IFF1 from IFF2
    cpu.iff1 = cpu.iff2;

    InstructionResult::new(14)
}

/// Load I Register: LD I, A (ED 47)
/// Copy A register to I register (interrupt vector base)
pub(crate) fn ld_i_a(cpu: &mut Z80) -> InstructionResult {
    cpu.i = cpu.a;
    InstructionResult::new(9)
}

/// Load R from A: LD R, A (ED 4F)
pub(crate) fn ld_r_a(cpu: &mut Z80) -> InstructionResult {
    cpu.r = cpu.a;
    InstructionResult::new(9)
}

/// Load A from I Register: LD A, I (ED 57)
/// Copy I register to A register
/// Flags: S=(I bit 7), Z=(I==0), H=0, P/V=IFF2, N=0, X/Y from A
pub(crate) fn ld_a_i(cpu: &mut Z80) -> InstructionResult {
    cpu.a = cpu.i;
    cpu.p = 1;

    // Set flags
    cpu.set_flag_s(cpu.a & 0x80 != 0);
    cpu.set_flag_z(cpu.a == 0);
    cpu.set_flag_h(false);
    cpu.set_flag_pv(cpu.iff2); // P/V flag = IFF2
    cpu.set_flag_n(false);
    cpu.set_flag_y(cpu.a & 0x20 != 0);
    cpu.set_flag_x(cpu.a & 0x08 != 0);
    // Carry flag not affected

    InstructionResult::new(9)
}

/// Load A from R Register: LD A, R (ED 5F)
/// Copy R register to A register
/// Flags: S=(R bit 7), Z=(R==0), H=0, P/V=IFF2, N=0, X/Y from A
pub(crate) fn ld_a_r(cpu: &mut Z80) -> InstructionResult {
    cpu.a = cpu.r;
    cpu.p = 1;

    // Set flags
    cpu.set_flag_s(cpu.a & 0x80 != 0);
    cpu.set_flag_z(cpu.a == 0);
    cpu.set_flag_h(false);
    cpu.set_flag_pv(cpu.iff2); // P/V flag = IFF2
    cpu.set_flag_n(false);
    cpu.set_flag_y(cpu.a & 0x20 != 0);
    cpu.set_flag_x(cpu.a & 0x08 != 0);
    // Carry flag not affected

    InstructionResult::new(9)
}

// ============================================================================
// I/O Block Instructions
// ============================================================================

/// Helper: apply undocumented flag modifications for INxR/OTxR when repeating
/// During the extra 5 T-states that decrement PC, X/Y flags get overwritten
/// and PF/HF may be additionally modified based on CF and data.
fn apply_ioxr_repeat_flags(cpu: &mut Z80) {
    // X/Y from PC bits (same as LDxR/CPxR)
    cpu.set_flag_y(cpu.pc & 0x2000 != 0); // PC.13
    cpu.set_flag_x(cpu.pc & 0x0800 != 0); // PC.11

    // Additional PF/HF modifications when CF is set
    let cf = cpu.flag_c();
    let nf = cpu.flag_n(); // N holds bit 7 of the byte transferred (set by INx/OUTx)
    if cf {
        if nf {
            // data & 0x80 was set (N=1 means data had bit 7 set)
            let pf_adj = ((cpu.b.wrapping_sub(1)) & 0x07)
                .count_ones()
                .is_multiple_of(2);
            cpu.set_flag_pv(cpu.flag_pv() ^ pf_adj ^ true);
            cpu.set_flag_h((cpu.b & 0x0F) == 0x00);
        } else {
            let pf_adj = ((cpu.b.wrapping_add(1)) & 0x07)
                .count_ones()
                .is_multiple_of(2);
            cpu.set_flag_pv(cpu.flag_pv() ^ pf_adj ^ true);
            cpu.set_flag_h((cpu.b & 0x0F) == 0x0F);
        }
    } else {
        let pf_adj = (cpu.b & 0x07).count_ones().is_multiple_of(2);
        cpu.set_flag_pv(cpu.flag_pv() ^ pf_adj ^ true);
    }
}

/// INI (ED A2) - Input and Increment
/// Read from port (C) into (HL), HL++, B--
/// Cycles: 16
pub(crate) fn ini(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let port = ((cpu.b as u16) << 8) | (cpu.c as u16);
    cpu.b = cpu.b.wrapping_sub(1);
    let value = bus.io_read(port);
    bus.write(cpu.hl() as u32, value);

    let hl = cpu.hl().wrapping_add(1);
    cpu.set_hl(hl);

    // WZ = port + 1
    cpu.wz = port.wrapping_add(1);

    // Flags
    cpu.set_flag_z(cpu.b == 0);
    cpu.set_flag_n(value & 0x80 != 0);
    let k = (value as u16).wrapping_add((cpu.c.wrapping_add(1)) as u16);
    cpu.set_flag_h(k > 255);
    cpu.set_flag_c(k > 255);
    cpu.set_flag_pv((((k & 7) as u8) ^ cpu.b).count_ones().is_multiple_of(2));
    cpu.set_flag_s(cpu.b & 0x80 != 0);
    cpu.set_flag_y(cpu.b & 0x20 != 0);
    cpu.set_flag_x(cpu.b & 0x08 != 0);

    InstructionResult::new(16)
}

/// IND (ED AA) - Input and Decrement
/// Read from port (C) into (HL), HL--, B--
/// Cycles: 16
pub(crate) fn ind(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let port = ((cpu.b as u16) << 8) | (cpu.c as u16);
    cpu.b = cpu.b.wrapping_sub(1);
    let value = bus.io_read(port);
    bus.write(cpu.hl() as u32, value);

    let hl = cpu.hl().wrapping_sub(1);
    cpu.set_hl(hl);

    // WZ = port - 1
    cpu.wz = port.wrapping_sub(1);

    // Flags
    cpu.set_flag_z(cpu.b == 0);
    cpu.set_flag_n(value & 0x80 != 0);
    let k = (value as u16).wrapping_add((cpu.c.wrapping_sub(1)) as u16);
    cpu.set_flag_h(k > 255);
    cpu.set_flag_c(k > 255);
    cpu.set_flag_pv((((k & 7) as u8) ^ cpu.b).count_ones().is_multiple_of(2));
    cpu.set_flag_s(cpu.b & 0x80 != 0);
    cpu.set_flag_y(cpu.b & 0x20 != 0);
    cpu.set_flag_x(cpu.b & 0x08 != 0);

    InstructionResult::new(16)
}

/// INIR (ED B2) - Input, Increment, Repeat
/// Repeat INI until B=0. 21 cycles per iteration, 16 for last.
pub(crate) fn inir(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let result = ini(cpu, bus);
    if cpu.b != 0 {
        cpu.pc = cpu.pc.wrapping_sub(2);
        cpu.wz = cpu.pc.wrapping_add(1);
        apply_ioxr_repeat_flags(cpu);
        InstructionResult::new(21)
    } else {
        result
    }
}

/// INDR (ED BA) - Input, Decrement, Repeat
/// Repeat IND until B=0. 21 cycles per iteration, 16 for last.
pub(crate) fn indr(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let result = ind(cpu, bus);
    if cpu.b != 0 {
        cpu.pc = cpu.pc.wrapping_sub(2);
        cpu.wz = cpu.pc.wrapping_add(1);
        apply_ioxr_repeat_flags(cpu);
        InstructionResult::new(21)
    } else {
        result
    }
}

/// OUTI (ED A3) - Output and Increment
/// Read from (HL), output to port (C), HL++, B--
/// Cycles: 16
pub(crate) fn outi(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let value = bus.read(cpu.hl() as u32);
    cpu.b = cpu.b.wrapping_sub(1);
    let port = ((cpu.b as u16) << 8) | (cpu.c as u16);
    bus.io_write(port, value);

    let hl = cpu.hl().wrapping_add(1);
    cpu.set_hl(hl);

    // WZ = port + 1
    cpu.wz = port.wrapping_add(1);

    // Flags
    cpu.set_flag_z(cpu.b == 0);
    cpu.set_flag_n(value & 0x80 != 0);
    let k = (value as u16).wrapping_add(cpu.l as u16);
    cpu.set_flag_h(k > 255);
    cpu.set_flag_c(k > 255);
    cpu.set_flag_pv((((k & 7) as u8) ^ cpu.b).count_ones().is_multiple_of(2));
    cpu.set_flag_s(cpu.b & 0x80 != 0);
    cpu.set_flag_y(cpu.b & 0x20 != 0);
    cpu.set_flag_x(cpu.b & 0x08 != 0);

    InstructionResult::new(16)
}

/// OUTD (ED AB) - Output and Decrement
/// Read from (HL), output to port (C), HL--, B--
/// Cycles: 16
pub(crate) fn outd(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let value = bus.read(cpu.hl() as u32);
    cpu.b = cpu.b.wrapping_sub(1);
    let port = ((cpu.b as u16) << 8) | (cpu.c as u16);
    bus.io_write(port, value);

    let hl = cpu.hl().wrapping_sub(1);
    cpu.set_hl(hl);

    // WZ = port - 1
    cpu.wz = port.wrapping_sub(1);

    // Flags
    cpu.set_flag_z(cpu.b == 0);
    cpu.set_flag_n(value & 0x80 != 0);
    let k = (value as u16).wrapping_add(cpu.l as u16);
    cpu.set_flag_h(k > 255);
    cpu.set_flag_c(k > 255);
    cpu.set_flag_pv((((k & 7) as u8) ^ cpu.b).count_ones().is_multiple_of(2));
    cpu.set_flag_s(cpu.b & 0x80 != 0);
    cpu.set_flag_y(cpu.b & 0x20 != 0);
    cpu.set_flag_x(cpu.b & 0x08 != 0);

    InstructionResult::new(16)
}

/// OTIR (ED B3) - Output, Increment, Repeat
/// Repeat OUTI until B=0. 21 cycles per iteration, 16 for last.
pub(crate) fn otir(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let result = outi(cpu, bus);
    if cpu.b != 0 {
        cpu.pc = cpu.pc.wrapping_sub(2);
        cpu.wz = cpu.pc.wrapping_add(1);
        apply_ioxr_repeat_flags(cpu);
        InstructionResult::new(21)
    } else {
        result
    }
}

/// OTDR (ED BB) - Output, Decrement, Repeat
/// Repeat OUTD until B=0. 21 cycles per iteration, 16 for last.
pub(crate) fn otdr(cpu: &mut Z80, bus: &mut dyn Addressable) -> InstructionResult {
    let result = outd(cpu, bus);
    if cpu.b != 0 {
        cpu.pc = cpu.pc.wrapping_sub(2);
        cpu.wz = cpu.pc.wrapping_add(1);
        apply_ioxr_repeat_flags(cpu);
        InstructionResult::new(21)
    } else {
        result // 16 cycles for last iteration
    }
}

/// ED NOP - 8-cycle NOP for undefined ED opcodes
pub(crate) fn ed_nop(_cpu: &mut Z80) -> InstructionResult {
    InstructionResult::new(8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::memory::Ram;

    #[test]
    fn test_ldi() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_hl(0x1000);
        cpu.set_de(0x2000);
        cpu.set_bc(0x0003);
        ram.write(0x1000, 0x42);

        ldi(&mut cpu, &mut ram);

        assert_eq!(ram.read(0x2000), 0x42);
        assert_eq!(cpu.hl(), 0x1001);
        assert_eq!(cpu.de(), 0x2001);
        assert_eq!(cpu.bc(), 0x0002);
        assert!(cpu.flag_pv()); // BC != 0
        assert!(!cpu.flag_h());
        assert!(!cpu.flag_n());
    }

    #[test]
    fn test_ldi_bc_becomes_zero() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_hl(0x1000);
        cpu.set_de(0x2000);
        cpu.set_bc(0x0001);
        ram.write(0x1000, 0x42);

        ldi(&mut cpu, &mut ram);

        assert_eq!(cpu.bc(), 0x0000);
        assert!(!cpu.flag_pv()); // BC == 0
    }

    #[test]
    fn test_ldd() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_hl(0x1000);
        cpu.set_de(0x2000);
        cpu.set_bc(0x0003);
        ram.write(0x1000, 0x42);

        ldd(&mut cpu, &mut ram);

        assert_eq!(ram.read(0x2000), 0x42);
        assert_eq!(cpu.hl(), 0x0FFF);
        assert_eq!(cpu.de(), 0x1FFF);
        assert_eq!(cpu.bc(), 0x0002);
        assert!(cpu.flag_pv());
    }

    #[test]
    fn test_ldir() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        // Setup source data
        ram.write(0x1000, 0x11);
        ram.write(0x1001, 0x22);
        ram.write(0x1002, 0x33);

        cpu.set_hl(0x1000);
        cpu.set_de(0x2000);
        cpu.set_bc(0x0003);
        cpu.pc = 0x0100;

        // Execute LDIR manually 3 times
        let result1 = ldir(&mut cpu, &mut ram);
        assert_eq!(result1.cycles, 21); // Repeating
        assert_eq!(cpu.pc, 0x0100 - 2); // PC decremented
        cpu.pc = 0x0100; // Restore PC for next iteration

        let result2 = ldir(&mut cpu, &mut ram);
        assert_eq!(result2.cycles, 21);
        cpu.pc = 0x0100;

        let result3 = ldir(&mut cpu, &mut ram);
        assert_eq!(result3.cycles, 16); // Last iteration

        // Verify copied data
        assert_eq!(ram.read(0x2000), 0x11);
        assert_eq!(ram.read(0x2001), 0x22);
        assert_eq!(ram.read(0x2002), 0x33);
        assert_eq!(cpu.bc(), 0x0000);
        assert!(!cpu.flag_pv());
    }

    #[test]
    fn test_lddr() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        // Setup source data
        ram.write(0x1002, 0x11);
        ram.write(0x1001, 0x22);
        ram.write(0x1000, 0x33);

        cpu.set_hl(0x1002);
        cpu.set_de(0x2002);
        cpu.set_bc(0x0003);
        cpu.pc = 0x0100;

        // Execute LDDR manually 3 times
        lddr(&mut cpu, &mut ram);
        cpu.pc = 0x0100;
        lddr(&mut cpu, &mut ram);
        cpu.pc = 0x0100;
        let result = lddr(&mut cpu, &mut ram);

        assert_eq!(result.cycles, 16);
        assert_eq!(ram.read(0x2002), 0x11);
        assert_eq!(ram.read(0x2001), 0x22);
        assert_eq!(ram.read(0x2000), 0x33);
        assert_eq!(cpu.bc(), 0x0000);
    }

    #[test]
    fn test_cpi_match() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x42;
        cpu.set_hl(0x1000);
        cpu.set_bc(0x0005);
        ram.write(0x1000, 0x42);

        cpi(&mut cpu, &mut ram);

        assert!(cpu.flag_z()); // Match found
        assert_eq!(cpu.hl(), 0x1001);
        assert_eq!(cpu.bc(), 0x0004);
        assert!(cpu.flag_pv()); // BC != 0
        assert!(cpu.flag_n());
    }

    #[test]
    fn test_cpi_no_match() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x42;
        cpu.set_hl(0x1000);
        cpu.set_bc(0x0005);
        ram.write(0x1000, 0x11);

        cpi(&mut cpu, &mut ram);

        assert!(!cpu.flag_z()); // No match
        assert_eq!(cpu.hl(), 0x1001);
        assert_eq!(cpu.bc(), 0x0004);
    }

    #[test]
    fn test_cpd() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.a = 0x42;
        cpu.set_hl(0x1000);
        cpu.set_bc(0x0005);
        ram.write(0x1000, 0x42);

        cpd(&mut cpu, &mut ram);

        assert!(cpu.flag_z());
        assert_eq!(cpu.hl(), 0x0FFF);
        assert_eq!(cpu.bc(), 0x0004);
    }

    #[test]
    fn test_cpir_finds_match() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        // Setup test data - looking for 0x42
        ram.write(0x1000, 0x11);
        ram.write(0x1001, 0x22);
        ram.write(0x1002, 0x42); // Match here

        cpu.a = 0x42;
        cpu.set_hl(0x1000);
        cpu.set_bc(0x0005);
        cpu.pc = 0x0100;

        // First iteration - no match
        let result1 = cpir(&mut cpu, &mut ram);
        assert_eq!(result1.cycles, 21);
        assert!(!cpu.flag_z());
        cpu.pc = 0x0100;

        // Second iteration - no match
        let result2 = cpir(&mut cpu, &mut ram);
        assert_eq!(result2.cycles, 21);
        cpu.pc = 0x0100;

        // Third iteration - match found!
        let result3 = cpir(&mut cpu, &mut ram);
        assert_eq!(result3.cycles, 16); // Stops
        assert!(cpu.flag_z()); // Match
        assert_eq!(cpu.hl(), 0x1003);
        assert_eq!(cpu.bc(), 0x0002); // Still > 0
        assert!(cpu.flag_pv()); // BC != 0 when stopped
    }

    #[test]
    fn test_cpir_bc_exhausted() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        ram.write(0x1000, 0x11);

        cpu.a = 0x42; // Looking for 0x42, but not there
        cpu.set_hl(0x1000);
        cpu.set_bc(0x0001);
        cpu.pc = 0x0100;

        let result = cpir(&mut cpu, &mut ram);

        assert_eq!(result.cycles, 16); // Stops because BC=0
        assert!(!cpu.flag_z()); // No match
        assert_eq!(cpu.bc(), 0x0000);
        assert!(!cpu.flag_pv()); // BC == 0
    }

    #[test]
    fn test_cpdr() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        ram.write(0x1002, 0x11);
        ram.write(0x1001, 0x22);
        ram.write(0x1000, 0x42);

        cpu.a = 0x42;
        cpu.set_hl(0x1002);
        cpu.set_bc(0x0005);
        cpu.pc = 0x0100;

        // Execute until match
        cpdr(&mut cpu, &mut ram);
        cpu.pc = 0x0100;
        cpdr(&mut cpu, &mut ram);
        cpu.pc = 0x0100;
        let result = cpdr(&mut cpu, &mut ram);

        assert_eq!(result.cycles, 16);
        assert!(cpu.flag_z());
        assert_eq!(cpu.hl(), 0x0FFF);
    }

    #[test]
    fn test_ld_nn_bc() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_bc(0x1234);
        cpu.pc = 0x0100;
        ram.write(0x0100, 0x00); // Address low
        ram.write(0x0101, 0x20); // Address high = 0x2000

        ld_nn_bc(&mut cpu, &mut ram);

        assert_eq!(ram.read(0x2000), 0x34); // Low byte
        assert_eq!(ram.read(0x2001), 0x12); // High byte
        assert_eq!(cpu.pc, 0x0102);
    }

    #[test]
    fn test_ld_bc_nn() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x0100;
        ram.write(0x0100, 0x00); // Address low
        ram.write(0x0101, 0x20); // Address high = 0x2000
        ram.write(0x2000, 0x34); // Low byte
        ram.write(0x2001, 0x12); // High byte

        ld_bc_nn(&mut cpu, &mut ram);

        assert_eq!(cpu.bc(), 0x1234);
        assert_eq!(cpu.pc, 0x0102);
    }

    #[test]
    fn test_ld_nn_de() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.set_de(0xABCD);
        cpu.pc = 0x0100;
        ram.write(0x0100, 0x00);
        ram.write(0x0101, 0x30);

        ld_nn_de(&mut cpu, &mut ram);

        assert_eq!(ram.read(0x3000), 0xCD);
        assert_eq!(ram.read(0x3001), 0xAB);
    }

    #[test]
    fn test_ld_de_nn() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x0100;
        ram.write(0x0100, 0x00);
        ram.write(0x0101, 0x30);
        ram.write(0x3000, 0xCD);
        ram.write(0x3001, 0xAB);

        ld_de_nn(&mut cpu, &mut ram);

        assert_eq!(cpu.de(), 0xABCD);
    }

    #[test]
    fn test_ld_nn_sp() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0xFFFE;
        cpu.pc = 0x0100;
        ram.write(0x0100, 0x00);
        ram.write(0x0101, 0x40);

        ld_nn_sp(&mut cpu, &mut ram);

        assert_eq!(ram.read(0x4000), 0xFE);
        assert_eq!(ram.read(0x4001), 0xFF);
    }

    #[test]
    fn test_ld_sp_nn() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.pc = 0x0100;
        ram.write(0x0100, 0x00);
        ram.write(0x0101, 0x40);
        ram.write(0x4000, 0xFE);
        ram.write(0x4001, 0xFF);

        ld_sp_nn(&mut cpu, &mut ram);

        assert_eq!(cpu.sp, 0xFFFE);
    }

    #[test]
    fn test_im_0() {
        let mut cpu = Z80::new();
        cpu.im = 2;

        im_0(&mut cpu);

        assert_eq!(cpu.im, 0);
    }

    #[test]
    fn test_im_1() {
        let mut cpu = Z80::new();
        cpu.im = 0;

        im_1(&mut cpu);

        assert_eq!(cpu.im, 1);
    }

    #[test]
    fn test_im_2() {
        let mut cpu = Z80::new();
        cpu.im = 0;

        im_2(&mut cpu);

        assert_eq!(cpu.im, 2);
    }

    #[test]
    fn test_reti() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0x2000;
        ram.write(0x2000, 0x34); // Return address low
        ram.write(0x2001, 0x12); // Return address high

        reti(&mut cpu, &mut ram);

        assert_eq!(cpu.pc, 0x1234);
        assert_eq!(cpu.sp, 0x2002);
    }

    #[test]
    fn test_retn() {
        let mut cpu = Z80::new();
        let mut ram = Ram::new(0x10000);

        cpu.sp = 0x2000;
        cpu.iff1 = false;
        cpu.iff2 = true;
        ram.write(0x2000, 0x34);
        ram.write(0x2001, 0x12);

        retn(&mut cpu, &mut ram);

        assert_eq!(cpu.pc, 0x1234);
        assert_eq!(cpu.sp, 0x2002);
        assert!(cpu.iff1); // Restored from IFF2
    }
}
