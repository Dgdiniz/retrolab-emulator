//! Z80 ROM Builder - a helper that builds small test ROMs
//!
//! Shared by several independent test binaries, each of which compiles this
//! module separately and uses a different subset of it, so per-binary
//! dead-code lints fire on items a sibling binary depends on.
#![allow(dead_code)]
//!
//! Builds executable Z80 programs that exercise real emulator features,
//! especially the communication with the YM2612.

/// Builder for Z80 ROMs
pub struct Z80RomBuilder {
    code: Vec<u8>,
    org: u16,
    labels: std::collections::HashMap<String, u16>,
    fixups: Vec<(usize, String)>, // (position, label)
}

impl Z80RomBuilder {
    pub fn new(org: u16) -> Self {
        Self {
            code: Vec::new(),
            org,
            labels: std::collections::HashMap::new(),
            fixups: Vec::new(),
        }
    }

    /// Defines a label at the current position
    pub fn label(&mut self, name: &str) -> &mut Self {
        let addr = self.org + self.code.len() as u16;
        self.labels.insert(name.to_string(), addr);
        self
    }

    // ===== Basic Instructions =====

    /// NOP
    pub fn nop(&mut self) -> &mut Self {
        self.code.push(0x00);
        self
    }

    /// HALT
    pub fn halt(&mut self) -> &mut Self {
        self.code.push(0x76);
        self
    }

    /// LD A, n
    pub fn ld_a(&mut self, value: u8) -> &mut Self {
        self.code.push(0x3E);
        self.code.push(value);
        self
    }

    /// LD B, n
    pub fn ld_b(&mut self, value: u8) -> &mut Self {
        self.code.push(0x06);
        self.code.push(value);
        self
    }

    /// LD C, n
    pub fn ld_c(&mut self, value: u8) -> &mut Self {
        self.code.push(0x0E);
        self.code.push(value);
        self
    }

    /// LD D, n
    pub fn ld_d(&mut self, value: u8) -> &mut Self {
        self.code.push(0x16);
        self.code.push(value);
        self
    }

    /// LD E, n
    pub fn ld_e(&mut self, value: u8) -> &mut Self {
        self.code.push(0x1E);
        self.code.push(value);
        self
    }

    /// LD H, n
    pub fn ld_h(&mut self, value: u8) -> &mut Self {
        self.code.push(0x26);
        self.code.push(value);
        self
    }

    /// LD L, n
    pub fn ld_l(&mut self, value: u8) -> &mut Self {
        self.code.push(0x2E);
        self.code.push(value);
        self
    }

    /// LD BC, nn
    pub fn ld_bc(&mut self, value: u16) -> &mut Self {
        self.code.push(0x01);
        self.code.push((value & 0xFF) as u8);
        self.code.push((value >> 8) as u8);
        self
    }

    /// LD DE, nn
    pub fn ld_de(&mut self, value: u16) -> &mut Self {
        self.code.push(0x11);
        self.code.push((value & 0xFF) as u8);
        self.code.push((value >> 8) as u8);
        self
    }

    /// LD HL, nn
    pub fn ld_hl(&mut self, value: u16) -> &mut Self {
        self.code.push(0x21);
        self.code.push((value & 0xFF) as u8);
        self.code.push((value >> 8) as u8);
        self
    }

    /// LD SP, nn
    pub fn ld_sp(&mut self, value: u16) -> &mut Self {
        self.code.push(0x31);
        self.code.push((value & 0xFF) as u8);
        self.code.push((value >> 8) as u8);
        self
    }

    /// LD (nn), A - Store A to memory address nn
    pub fn ld_nn_a(&mut self, addr: u16) -> &mut Self {
        self.code.push(0x32); // LD (nn), A
        self.code.push((addr & 0xFF) as u8); // Low byte
        self.code.push((addr >> 8) as u8); // High byte
        self
    }

    /// LD A, (nn) - Load A from memory address nn
    pub fn ld_a_nn(&mut self, addr: u16) -> &mut Self {
        self.code.push(0x3A); // LD A, (nn)
        self.code.push((addr & 0xFF) as u8); // Low byte
        self.code.push((addr >> 8) as u8); // High byte
        self
    }

    /// LD A, B
    pub fn ld_a_b(&mut self) -> &mut Self {
        self.code.push(0x78);
        self
    }

    /// LD A, C
    pub fn ld_a_c(&mut self) -> &mut Self {
        self.code.push(0x79);
        self
    }

    /// OUT (n), A
    pub fn out_n_a(&mut self, port: u8) -> &mut Self {
        self.code.push(0xD3);
        self.code.push(port);
        self
    }

    /// OUT (C), A
    pub fn out_c_a(&mut self) -> &mut Self {
        self.code.push(0xED);
        self.code.push(0x79);
        self
    }

    /// IN A, (n)
    pub fn in_a_n(&mut self, port: u8) -> &mut Self {
        self.code.push(0xDB);
        self.code.push(port);
        self
    }

    /// IN A, (C)
    pub fn in_a_c(&mut self) -> &mut Self {
        self.code.push(0xED);
        self.code.push(0x78);
        self
    }

    /// INC A
    pub fn inc_a(&mut self) -> &mut Self {
        self.code.push(0x3C);
        self
    }

    /// DEC A
    pub fn dec_a(&mut self) -> &mut Self {
        self.code.push(0x3D);
        self
    }

    /// INC B
    pub fn inc_b(&mut self) -> &mut Self {
        self.code.push(0x04);
        self
    }

    /// DEC B
    pub fn dec_b(&mut self) -> &mut Self {
        self.code.push(0x05);
        self
    }

    /// INC C
    pub fn inc_c(&mut self) -> &mut Self {
        self.code.push(0x0C);
        self
    }

    /// DJNZ offset (Decrement B and jump if not zero)
    pub fn djnz(&mut self, offset: i8) -> &mut Self {
        self.code.push(0x10);
        self.code.push(offset as u8);
        self
    }

    /// JR offset (Relative jump)
    pub fn jr(&mut self, offset: i8) -> &mut Self {
        self.code.push(0x18);
        self.code.push(offset as u8);
        self
    }

    /// JP nn (Absolute jump)
    pub fn jp(&mut self, addr: u16) -> &mut Self {
        self.code.push(0xC3);
        self.code.push((addr & 0xFF) as u8);
        self.code.push((addr >> 8) as u8);
        self
    }

    /// CALL nn
    pub fn call(&mut self, addr: u16) -> &mut Self {
        self.code.push(0xCD);
        self.code.push((addr & 0xFF) as u8);
        self.code.push((addr >> 8) as u8);
        self
    }

    /// RET
    pub fn ret(&mut self) -> &mut Self {
        self.code.push(0xC9);
        self
    }

    /// PUSH AF
    pub fn push_af(&mut self) -> &mut Self {
        self.code.push(0xF5);
        self
    }

    /// POP AF
    pub fn pop_af(&mut self) -> &mut Self {
        self.code.push(0xF1);
        self
    }

    /// PUSH BC
    pub fn push_bc(&mut self) -> &mut Self {
        self.code.push(0xC5);
        self
    }

    /// POP BC
    pub fn pop_bc(&mut self) -> &mut Self {
        self.code.push(0xC1);
        self
    }

    /// CP n (Compare A with n)
    pub fn cp(&mut self, value: u8) -> &mut Self {
        self.code.push(0xFE);
        self.code.push(value);
        self
    }

    /// XOR A (Zero A register)
    pub fn xor_a(&mut self) -> &mut Self {
        self.code.push(0xAF);
        self
    }

    /// AND n
    pub fn and(&mut self, value: u8) -> &mut Self {
        self.code.push(0xE6);
        self.code.push(value);
        self
    }

    /// OR n
    pub fn or(&mut self, value: u8) -> &mut Self {
        self.code.push(0xF6);
        self.code.push(value);
        self
    }

    /// ADD A, n
    pub fn add_a(&mut self, value: u8) -> &mut Self {
        self.code.push(0xC6);
        self.code.push(value);
        self
    }

    /// LD (HL), n
    pub fn ld_hl_ind(&mut self, value: u8) -> &mut Self {
        self.code.push(0x36);
        self.code.push(value);
        self
    }

    /// LD A, (HL)
    pub fn ld_a_hl_ind(&mut self) -> &mut Self {
        self.code.push(0x7E);
        self
    }

    /// LD (HL), A
    pub fn ld_hl_ind_a(&mut self) -> &mut Self {
        self.code.push(0x77);
        self
    }

    // ===== YM2612 Helpers =====

    /// Write to YM2612 Part 0 (Address + Data)
    pub fn ym_write_part0(&mut self, register: u8, value: u8) -> &mut Self {
        // YM2612 is memory-mapped at 0x4000-0x4003 in Z80 address space
        // 0x4000: Address latch Part 0
        // 0x4001: Data Part 0
        self.ld_a(register);
        self.ld_nn_a(0x4000); // LD (0x4000), A
        self.ld_a(value);
        self.ld_nn_a(0x4001); // LD (0x4001), A
        self
    }

    /// Write to YM2612 Part 1 (Address + Data)
    pub fn ym_write_part1(&mut self, register: u8, value: u8) -> &mut Self {
        // YM2612 is memory-mapped at 0x4000-0x4003 in Z80 address space
        // 0x4002: Address latch Part 1
        // 0x4003: Data Part 1
        self.ld_a(register);
        self.ld_nn_a(0x4002); // LD (0x4002), A
        self.ld_a(value);
        self.ld_nn_a(0x4003); // LD (0x4003), A
        self
    }

    /// Read the YM2612 status into A: LD A, (0x4000), where the chip sits in
    /// the Z80's memory (its I/O ports reach nothing on the Mega Drive)
    pub fn ym_read_status(&mut self) -> &mut Self {
        self.ld_a_nn(0x4000)
    }

    /// Simple delay (a run of NOPs)
    pub fn delay(&mut self, count: u8) -> &mut Self {
        for _ in 0..count {
            self.nop();
        }
        self
    }

    /// Infinite loop (JR -2)
    pub fn infinite_loop(&mut self) -> &mut Self {
        self.jr(-2)
    }

    // ===== Build final =====

    /// Finish and return the ROM
    pub fn build(&self) -> Vec<u8> {
        // Labels and fixups are not resolved: return the raw code
        self.code.clone()
    }

    /// Get current address (org + code length)
    pub fn current_addr(&self) -> u16 {
        self.org + self.code.len() as u16
    }

    /// Add raw bytes
    pub fn raw(&mut self, bytes: &[u8]) -> &mut Self {
        self.code.extend_from_slice(bytes);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_basic() {
        let rom = Z80RomBuilder::new(0x0000).ld_a(0x42).halt().build();

        assert_eq!(rom, vec![0x3E, 0x42, 0x76]);
    }

    #[test]
    fn test_builder_ym_write() {
        let rom = Z80RomBuilder::new(0x0000)
            .ym_write_part0(0x22, 0x08) // Enable LFO
            .halt()
            .build();

        // Expected: LD A, 0x22; LD (0x4000), A; LD A, 0x08; LD (0x4001), A; HALT
        // YM2612 is memory-mapped at 0x4000-0x4003
        assert_eq!(
            rom,
            vec![
                0x3E, 0x22, // LD A, 0x22
                0x32, 0x00, 0x40, // LD (0x4000), A
                0x3E, 0x08, // LD A, 0x08
                0x32, 0x01, 0x40, // LD (0x4001), A
                0x76, // HALT
            ]
        );
    }
}
