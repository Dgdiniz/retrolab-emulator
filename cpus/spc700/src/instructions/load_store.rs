//! `MOV` / `MOVW` family, the workhorse of the SPC700.
//!
//! 41 distinct `MOV` opcodes cover every combination of source /
//! destination register (`A`/`X`/`Y`/`SP`) and operand shape that
//! the instruction set allows; 2 `MOVW` opcodes load/store the 16-bit
//! `YA` pair from/to a direct-page word. Together they exercise every
//! addressing mode implemented in [`crate::addressing`], so getting
//! them right validates the whole infrastructure.
//!
//! Flag semantics:
//!
//! - Loads (`MOV reg, <src>`): update `N`/`Z` from the loaded byte.
//! - Stores (`MOV <dst>, reg`, including `MOV $dp,#imm` and
//!   `MOV $dp,$dp`): do **not** touch flags.
//! - Register-to-register `MOV <dst>, <src>`: update `N`/`Z` from the
//!   destination, with the one exception `MOV SP, X` (`$BD`), which
//!   keeps PSW intact (the SingleStepTests show it; checked against
//!   snes9x).
//! - `MOVW YA, $dp`: load the 16-bit word, update `N`/`Z` from the
//!   full 16-bit value.
//! - `MOVW $dp, YA`: store the word, leaving flags alone.

use retro_core::cpu::Addressable;

use crate::addressing::{
    fetch_pc_byte, fetch_pc_word, indirect_indirect_x, indirect_indirect_y, read_dp_word,
    write_dp_word, x_indirect_addr,
};
use crate::decode::OpcodeInfo;
use crate::Spc700;

pub fn mov(cpu: &mut Spc700, bus: &mut dyn Addressable, opcode: u8, info: &OpcodeInfo) -> u32 {
    let cycles = info.base_cycles as u32;
    let dp_base = cpu.dp_base();

    match opcode {
        // -- Register-to-register (Implied) ---------------------------------

        // MOV A, X
        0x7D => {
            cpu.a = cpu.x;
            cpu.update_nz_u8(cpu.a);
        }
        // MOV A, Y
        0xDD => {
            cpu.a = cpu.y;
            cpu.update_nz_u8(cpu.a);
        }
        // MOV X, A
        0x5D => {
            cpu.x = cpu.a;
            cpu.update_nz_u8(cpu.x);
        }
        // MOV Y, A
        0xFD => {
            cpu.y = cpu.a;
            cpu.update_nz_u8(cpu.y);
        }
        // MOV X, SP
        0x9D => {
            cpu.x = cpu.sp;
            cpu.update_nz_u8(cpu.x);
        }
        // MOV SP, X, quirk: this is the only register-to-register MOV
        // that does NOT update N/Z (checked against snes9x and bsnes-plus).
        0xBD => {
            cpu.sp = cpu.x;
        }

        // -- Loads from immediate -------------------------------------------

        // MOV A, #imm
        0xE8 => {
            cpu.a = fetch_pc_byte(cpu, bus);
            cpu.update_nz_u8(cpu.a);
        }
        // MOV X, #imm
        0xCD => {
            cpu.x = fetch_pc_byte(cpu, bus);
            cpu.update_nz_u8(cpu.x);
        }
        // MOV Y, #imm
        0x8D => {
            cpu.y = fetch_pc_byte(cpu, bus);
            cpu.update_nz_u8(cpu.y);
        }

        // -- Loads from direct page -----------------------------------------

        // MOV A, $dp
        0xE4 => {
            let dp = fetch_pc_byte(cpu, bus);
            cpu.a = bus.read((dp_base | dp as u16) as u32);
            cpu.update_nz_u8(cpu.a);
        }
        // MOV X, $dp
        0xF8 => {
            let dp = fetch_pc_byte(cpu, bus);
            cpu.x = bus.read((dp_base | dp as u16) as u32);
            cpu.update_nz_u8(cpu.x);
        }
        // MOV Y, $dp
        0xEB => {
            let dp = fetch_pc_byte(cpu, bus);
            cpu.y = bus.read((dp_base | dp as u16) as u32);
            cpu.update_nz_u8(cpu.y);
        }

        // -- Stores to direct page ------------------------------------------

        // MOV $dp, A
        0xC4 => {
            let dp = fetch_pc_byte(cpu, bus);
            bus.write((dp_base | dp as u16) as u32, cpu.a);
        }
        // MOV $dp, X
        0xD8 => {
            let dp = fetch_pc_byte(cpu, bus);
            bus.write((dp_base | dp as u16) as u32, cpu.x);
        }
        // MOV $dp, Y
        0xCB => {
            let dp = fetch_pc_byte(cpu, bus);
            bus.write((dp_base | dp as u16) as u32, cpu.y);
        }

        // -- Loads from direct-page indexed ---------------------------------

        // MOV A, $dp+X
        0xF4 => {
            let dp = fetch_pc_byte(cpu, bus);
            let addr = dp_base | (dp.wrapping_add(cpu.x) as u16);
            cpu.a = bus.read(addr as u32);
            cpu.update_nz_u8(cpu.a);
        }
        // MOV X, $dp+Y
        0xF9 => {
            let dp = fetch_pc_byte(cpu, bus);
            let addr = dp_base | (dp.wrapping_add(cpu.y) as u16);
            cpu.x = bus.read(addr as u32);
            cpu.update_nz_u8(cpu.x);
        }
        // MOV Y, $dp+X
        0xFB => {
            let dp = fetch_pc_byte(cpu, bus);
            let addr = dp_base | (dp.wrapping_add(cpu.x) as u16);
            cpu.y = bus.read(addr as u32);
            cpu.update_nz_u8(cpu.y);
        }

        // -- Stores to direct-page indexed ----------------------------------

        // MOV $dp+X, A
        0xD4 => {
            let dp = fetch_pc_byte(cpu, bus);
            let addr = dp_base | (dp.wrapping_add(cpu.x) as u16);
            bus.write(addr as u32, cpu.a);
        }
        // MOV $dp+Y, X
        0xD9 => {
            let dp = fetch_pc_byte(cpu, bus);
            let addr = dp_base | (dp.wrapping_add(cpu.y) as u16);
            bus.write(addr as u32, cpu.x);
        }
        // MOV $dp+X, Y
        0xDB => {
            let dp = fetch_pc_byte(cpu, bus);
            let addr = dp_base | (dp.wrapping_add(cpu.x) as u16);
            bus.write(addr as u32, cpu.y);
        }

        // -- Loads from absolute --------------------------------------------

        // MOV A, !abs
        0xE5 => {
            let addr = fetch_pc_word(cpu, bus);
            cpu.a = bus.read(addr as u32);
            cpu.update_nz_u8(cpu.a);
        }
        // MOV X, !abs
        0xE9 => {
            let addr = fetch_pc_word(cpu, bus);
            cpu.x = bus.read(addr as u32);
            cpu.update_nz_u8(cpu.x);
        }
        // MOV Y, !abs
        0xEC => {
            let addr = fetch_pc_word(cpu, bus);
            cpu.y = bus.read(addr as u32);
            cpu.update_nz_u8(cpu.y);
        }

        // -- Stores to absolute ---------------------------------------------

        // MOV !abs, A
        0xC5 => {
            let addr = fetch_pc_word(cpu, bus);
            bus.write(addr as u32, cpu.a);
        }
        // MOV !abs, X
        0xC9 => {
            let addr = fetch_pc_word(cpu, bus);
            bus.write(addr as u32, cpu.x);
        }
        // MOV !abs, Y
        0xCC => {
            let addr = fetch_pc_word(cpu, bus);
            bus.write(addr as u32, cpu.y);
        }

        // -- Loads from absolute indexed ------------------------------------

        // MOV A, !abs+X
        0xF5 => {
            let base = fetch_pc_word(cpu, bus);
            cpu.a = bus.read(base.wrapping_add(cpu.x as u16) as u32);
            cpu.update_nz_u8(cpu.a);
        }
        // MOV A, !abs+Y
        0xF6 => {
            let base = fetch_pc_word(cpu, bus);
            cpu.a = bus.read(base.wrapping_add(cpu.y as u16) as u32);
            cpu.update_nz_u8(cpu.a);
        }

        // -- Stores to absolute indexed -------------------------------------

        // MOV !abs+X, A
        0xD5 => {
            let base = fetch_pc_word(cpu, bus);
            bus.write(base.wrapping_add(cpu.x as u16) as u32, cpu.a);
        }
        // MOV !abs+Y, A
        0xD6 => {
            let base = fetch_pc_word(cpu, bus);
            bus.write(base.wrapping_add(cpu.y as u16) as u32, cpu.a);
        }

        // -- Indirect through X ---------------------------------------------

        // MOV A, (X)
        0xE6 => {
            cpu.a = bus.read(x_indirect_addr(cpu) as u32);
            cpu.update_nz_u8(cpu.a);
        }
        // MOV (X), A
        0xC6 => {
            bus.write(x_indirect_addr(cpu) as u32, cpu.a);
        }

        // -- Indirect through X with auto-increment -------------------------

        // MOV A, (X)+   (post-increment X with wrap)
        0xBF => {
            cpu.a = bus.read(x_indirect_addr(cpu) as u32);
            cpu.x = cpu.x.wrapping_add(1);
            cpu.update_nz_u8(cpu.a);
        }
        // MOV (X)+, A   (post-increment X with wrap)
        0xAF => {
            bus.write(x_indirect_addr(cpu) as u32, cpu.a);
            cpu.x = cpu.x.wrapping_add(1);
        }

        // -- Indirect-indirect X ($dp+X) ------------------------------------

        // MOV A, ($dp+X)
        0xE7 => {
            let dp = fetch_pc_byte(cpu, bus);
            let addr = indirect_indirect_x(cpu, bus, dp);
            cpu.a = bus.read(addr as u32);
            cpu.update_nz_u8(cpu.a);
        }
        // MOV ($dp+X), A
        0xC7 => {
            let dp = fetch_pc_byte(cpu, bus);
            let addr = indirect_indirect_x(cpu, bus, dp);
            bus.write(addr as u32, cpu.a);
        }

        // -- Indirect-indirect Y ($dp)+Y ------------------------------------

        // MOV A, ($dp)+Y
        0xF7 => {
            let dp = fetch_pc_byte(cpu, bus);
            let addr = indirect_indirect_y(cpu, bus, dp);
            cpu.a = bus.read(addr as u32);
            cpu.update_nz_u8(cpu.a);
        }
        // MOV ($dp)+Y, A
        0xD7 => {
            let dp = fetch_pc_byte(cpu, bus);
            let addr = indirect_indirect_y(cpu, bus, dp);
            bus.write(addr as u32, cpu.a);
        }

        // -- DirectPageImm: MOV $dp, #imm -----------------------------------

        // MOV $dp, #imm   (encoded immediate first, then $dp)
        0x8F => {
            let imm = fetch_pc_byte(cpu, bus);
            let dp = fetch_pc_byte(cpu, bus);
            bus.write((dp_base | dp as u16) as u32, imm);
        }

        // -- DirectPage-to-DirectPage: MOV $dp(dst), $dp(src) ---------------

        // MOV $dp, $dp   (encoded SOURCE byte first, DESTINATION second).
        0xFA => {
            let src = fetch_pc_byte(cpu, bus);
            let dst = fetch_pc_byte(cpu, bus);
            let val = bus.read((dp_base | src as u16) as u32);
            bus.write((dp_base | dst as u16) as u32, val);
        }

        _ => panic!(
            "spc700: mov dispatched with non-MOV opcode {:02X} ({:?})",
            opcode, info.mode
        ),
    }

    cycles
}

pub fn movw(cpu: &mut Spc700, bus: &mut dyn Addressable, opcode: u8, info: &OpcodeInfo) -> u32 {
    let cycles = info.base_cycles as u32;

    match opcode {
        // MOVW YA, $dp, load 16-bit word from DP into Y:A, set N/Z
        // based on the full 16-bit result.
        0xBA => {
            let dp = fetch_pc_byte(cpu, bus);
            let v = read_dp_word(cpu, bus, dp);
            cpu.set_ya(v);
            cpu.update_nz_u16(v);
        }
        // MOVW $dp, YA, store Y:A (16-bit) into DP, no flag update.
        0xDA => {
            let dp = fetch_pc_byte(cpu, bus);
            write_dp_word(cpu, bus, dp, cpu.ya());
        }
        _ => panic!(
            "spc700: movw dispatched with non-MOVW opcode {:02X} ({:?})",
            opcode, info.mode
        ),
    }

    cycles
}
