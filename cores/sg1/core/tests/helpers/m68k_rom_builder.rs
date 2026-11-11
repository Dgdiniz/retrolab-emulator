//! M68000 ROM Builder - a helper that builds small test ROMs
//!
//! Shared by several independent test binaries, each of which compiles this
//! module separately and uses a different subset of it, so per-binary
//! dead-code lints fire on items a sibling binary depends on.
#![allow(dead_code)]
//!
//! Builds executable 68000 programs that exercise real emulator features,
//! especially I/O and the controllers.

/// Builder for 68000 ROMs
pub struct M68kRomBuilder {
    code: Vec<u16>, // 68000 instructions are 16-bit words
    org: u32,
}

impl M68kRomBuilder {
    /// Create a new ROM builder
    ///
    /// # Arguments
    /// * `org` - Origin address where code will be placed (typically 0x400)
    pub fn new(org: u32) -> Self {
        Self {
            code: Vec::new(),
            org,
        }
    }

    /// Build the final ROM with proper Genesis header
    ///
    /// Creates a ROM with:
    /// - Initial SP at 0x00FFFFFE
    /// - Initial PC at `org`
    /// - Code at `org` offset
    pub fn build(&self) -> Vec<u8> {
        // Create ROM large enough for header + code
        let code_end = self.org as usize + self.code.len() * 2;
        let rom_size = code_end.max(0x200); // At least 512 bytes for header

        // Fill ROM with NOPs (0x4E71) to avoid executing invalid instructions
        // if PC goes beyond valid code
        let mut rom = vec![0u8; rom_size];
        for i in (0..rom_size).step_by(2) {
            if i + 1 < rom_size {
                rom[i] = 0x4E; // NOP high byte
                rom[i + 1] = 0x71; // NOP low byte
            }
        }

        // Initial SP (offset 0x000)
        rom[0..4].copy_from_slice(&0x00FF_FFFEu32.to_be_bytes());

        // Initial PC (offset 0x004) - where code starts
        rom[4..8].copy_from_slice(&self.org.to_be_bytes());

        // Create minimal Genesis ROM header (0x100-0x1FF)
        // Console type (0x100-0x10F)
        let console = b"SEGA MEGA DRIVE ";
        rom[0x100..0x110].copy_from_slice(console);

        // Copyright (0x110-0x11F)
        let copyright = b"(C)TEST 2024.NOV";
        rom[0x110..0x120].copy_from_slice(copyright);

        // Domestic name (0x120-0x14F)
        let domestic_name = b"TEST ROM                                        ";
        rom[0x120..0x150].copy_from_slice(domestic_name);

        // Overseas name (0x150-0x17F)
        let overseas_name = b"TEST ROM                                        ";
        rom[0x150..0x180].copy_from_slice(overseas_name);

        // Serial (0x180-0x18D)
        let serial = b"GM 00000000-00";
        rom[0x180..0x18E].copy_from_slice(serial);

        // Checksum (0x18E-0x18F) - will be wrong, but that's ok for tests
        rom[0x18E] = 0x00;
        rom[0x18F] = 0x00;

        // Device support (0x190-0x19F) - Joypad
        let devices = b"J               ";
        rom[0x190..0x1A0].copy_from_slice(devices);

        // ROM range (0x1A0-0x1A7)
        rom[0x1A0..0x1A4].copy_from_slice(&0x00000000u32.to_be_bytes()); // ROM start
        rom[0x1A4..0x1A8].copy_from_slice(&0x000FFFFFu32.to_be_bytes()); // ROM end (1MB)

        // RAM range (0x1A8-0x1AF)
        rom[0x1A8..0x1AC].copy_from_slice(&0x00FF0000u32.to_be_bytes()); // RAM start
        rom[0x1AC..0x1B0].copy_from_slice(&0x00FFFFFFu32.to_be_bytes()); // RAM end

        // No SRAM (0x1B0-0x1B7)
        rom[0x1B0..0x1B8].copy_from_slice(b"        ");

        // Regions (0x1F0-0x1FF) - USA, Europe, Japan
        let regions = b"JUE             ";
        rom[0x1F0..0x200].copy_from_slice(regions);

        // Copy code as big-endian 16-bit words
        let code_offset = self.org as usize;
        for (i, &word) in self.code.iter().enumerate() {
            let offset = code_offset + i * 2;
            rom[offset..offset + 2].copy_from_slice(&word.to_be_bytes());
        }

        rom
    }

    // ===== Data Movement Instructions =====

    /// MOVE.B #imm, addr.L
    ///
    /// Writes an immediate byte value to an absolute long address
    /// Note: emitted as MOVEQ #value, D1 + MOVE.B D1, abs.L, so it overwrites D1
    pub fn move_b_imm_to_abs(&mut self, value: u8, addr: u32) -> &mut Self {
        // Load value into D1 using MOVEQ (sign-extended)
        let signed_value = value as i8;
        self.code.push(0x7200 | ((signed_value as u8) as u16)); // MOVEQ #value, D1

        // Write D1 to address
        self.code.push(0x13C1); // MOVE.B D1, abs.L
        self.code.push((addr >> 16) as u16);
        self.code.push((addr & 0xFFFF) as u16);
        self
    }

    /// MOVE.B addr.L, D0
    ///
    /// Reads a byte from absolute long address into D0
    /// Opcode: 0x1039
    pub fn move_b_abs_to_d0(&mut self, addr: u32) -> &mut Self {
        self.code.push(0x1039); // MOVE.B abs.L, D0
        self.code.push((addr >> 16) as u16);
        self.code.push((addr & 0xFFFF) as u16);
        self
    }

    /// MOVE.B D0, addr.L
    ///
    /// Writes D0 (low byte) to absolute long address
    /// Opcode: 0x13C0
    pub fn move_b_d0_to_abs(&mut self, addr: u32) -> &mut Self {
        self.code.push(0x13C0); // MOVE.B D0, abs.L
        self.code.push((addr >> 16) as u16);
        self.code.push((addr & 0xFFFF) as u16);
        self
    }

    /// MOVE.L D0, addr.L
    ///
    /// Writes D0 (long) to absolute long address
    /// Opcode: 0x23C0
    pub fn move_l_d0_to_abs(&mut self, addr: u32) -> &mut Self {
        self.code.push(0x23C0); // MOVE.L D0, abs.L
        self.code.push((addr >> 16) as u16);
        self.code.push((addr & 0xFFFF) as u16);
        self
    }

    /// MOVE.W #imm, D0
    ///
    /// Loads an immediate word value into D0
    /// Opcode: 0x303C
    pub fn move_w_imm_to_d0(&mut self, value: u16) -> &mut Self {
        self.code.push(0x303C); // MOVE.W #imm, D0
        self.code.push(value);
        self
    }

    /// MOVE.L #imm, D0
    ///
    /// Loads an immediate long value into D0
    /// Opcode: 0x203C
    pub fn move_l_imm_to_d0(&mut self, value: u32) -> &mut Self {
        self.code.push(0x203C); // MOVE.L #imm, D0
        self.code.push((value >> 16) as u16);
        self.code.push((value & 0xFFFF) as u16);
        self
    }

    /// MOVEQ #imm, D0
    ///
    /// Loads a sign-extended 8-bit immediate into D0 (fast)
    /// Opcode: 0x7000 | (imm & 0xFF)
    pub fn moveq_imm_to_d0(&mut self, value: i8) -> &mut Self {
        self.code.push(0x7000 | ((value as u8) as u16));
        self
    }

    // ===== Control Flow =====

    /// NOP
    ///
    /// No operation (4 cycles)
    /// Opcode: 0x4E71
    pub fn nop(&mut self) -> &mut Self {
        self.code.push(0x4E71);
        self
    }

    /// STOP - Infinite loop (replaces actual STOP for tests)
    ///
    /// Creates an infinite loop (BRA $) instead of using the STOP instruction:
    /// the CPU keeps executing the branch, and no new SR is loaded.
    /// Opcode: BRA.S -2 (0x60FE)
    pub fn stop(&mut self) -> &mut Self {
        self.code.push(0x60FE); // BRA.S -2 (infinite loop)
        self
    }

    /// BRA.S offset
    ///
    /// Branch always (short, 8-bit offset)
    /// Offset is relative to PC after instruction (PC+2)
    /// Opcode: 0x6000 | (offset & 0xFF)
    pub fn bra_s(&mut self, offset: i8) -> &mut Self {
        self.code.push(0x6000 | ((offset as u8) as u16));
        self
    }

    /// BRA.W offset
    ///
    /// Branch always (word, 16-bit offset)
    /// Offset is relative to PC after instruction (PC+2)
    /// Opcode: 0x6000
    pub fn bra_w(&mut self, offset: i16) -> &mut Self {
        self.code.push(0x6000);
        self.code.push(offset as u16);
        self
    }

    // ===== Arithmetic =====

    /// ADDQ.L #imm, D0
    ///
    /// Add quick (1-8) to D0
    /// Opcode: 0x5080 | ((imm & 7) << 9), so 8 is encoded as 0
    pub fn addq_l_imm_to_d0(&mut self, value: u8) -> &mut Self {
        assert!((1..=8).contains(&value), "ADDQ immediate must be 1-8");
        let imm = if value == 8 { 0 } else { value };
        self.code.push(0x5080 | ((imm as u16) << 9));
        self
    }

    /// SUBQ.L #imm, D0
    ///
    /// Subtract quick (1-8) from D0
    /// Opcode: 0x5180 | ((imm & 7) << 9), so 8 is encoded as 0
    pub fn subq_l_imm_from_d0(&mut self, value: u8) -> &mut Self {
        assert!((1..=8).contains(&value), "SUBQ immediate must be 1-8");
        let imm = if value == 8 { 0 } else { value };
        self.code.push(0x5180 | ((imm as u16) << 9));
        self
    }

    // ===== Comparison and Branching =====

    /// CMP.L #imm, D0
    ///
    /// Compare D0 with immediate long
    /// Opcode: 0x0C80
    pub fn cmp_l_imm_d0(&mut self, value: u32) -> &mut Self {
        self.code.push(0x0C80); // CMP.L #imm, D0
        self.code.push((value >> 16) as u16);
        self.code.push((value & 0xFFFF) as u16);
        self
    }

    /// BEQ.S offset
    ///
    /// Branch if equal (Z flag set)
    /// Opcode: 0x6700 | (offset & 0xFF)
    pub fn beq_s(&mut self, offset: i8) -> &mut Self {
        self.code.push(0x6700 | ((offset as u8) as u16));
        self
    }

    /// BNE.S offset
    ///
    /// Branch if not equal (Z flag clear)
    /// Opcode: 0x6600 | (offset & 0xFF)
    pub fn bne_s(&mut self, offset: i8) -> &mut Self {
        self.code.push(0x6600 | ((offset as u8) as u16));
        self
    }

    // ===== Logical =====

    /// AND.L #imm, D0
    ///
    /// Logical AND with D0
    /// Opcode: 0x0280
    pub fn and_l_imm_d0(&mut self, value: u32) -> &mut Self {
        self.code.push(0x0280); // AND.L #imm, D0
        self.code.push((value >> 16) as u16);
        self.code.push((value & 0xFFFF) as u16);
        self
    }

    /// OR.L #imm, D0
    ///
    /// Logical OR with D0
    /// Opcode: 0x0080
    pub fn or_l_imm_d0(&mut self, value: u32) -> &mut Self {
        self.code.push(0x0080); // OR.L #imm, D0
        self.code.push((value >> 16) as u16);
        self.code.push((value & 0xFFFF) as u16);
        self
    }

    // ===== Helper Methods =====

    /// Add multiple NOPs for delay
    pub fn delay(&mut self, nop_count: u32) -> &mut Self {
        for _ in 0..nop_count {
            self.nop();
        }
        self
    }

    /// Get current code size in words
    pub fn code_size_words(&self) -> usize {
        self.code.len()
    }

    /// Get current code size in bytes
    pub fn code_size_bytes(&self) -> usize {
        self.code.len() * 2
    }

    /// Get current PC (org + code size)
    pub fn current_pc(&self) -> u32 {
        self.org + self.code_size_bytes() as u32
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder_creation() {
        let builder = M68kRomBuilder::new(0x400);
        assert_eq!(builder.org, 0x400);
        assert_eq!(builder.code_size_words(), 0);
    }

    #[test]
    fn test_build_header() {
        let rom = M68kRomBuilder::new(0x400).build();

        // Check Initial SP
        let sp = u32::from_be_bytes([rom[0], rom[1], rom[2], rom[3]]);
        assert_eq!(sp, 0x00FF_FFFE);

        // Check Initial PC
        let pc = u32::from_be_bytes([rom[4], rom[5], rom[6], rom[7]]);
        assert_eq!(pc, 0x400);
    }

    #[test]
    fn test_nop_instruction() {
        let rom = M68kRomBuilder::new(0x400).nop().build();

        // NOP at offset 0x400
        assert_eq!(rom[0x400], 0x4E);
        assert_eq!(rom[0x401], 0x71);
    }

    #[test]
    fn test_stop_instruction() {
        let rom = M68kRomBuilder::new(0x400).stop().build();

        // BRA.S -2 (infinite loop) at offset 0x400
        assert_eq!(rom[0x400], 0x60); // BRA.S opcode high byte
        assert_eq!(rom[0x401], 0xFE); // BRA.S opcode low byte (offset -2)
    }

    #[test]
    fn test_move_b_imm_to_abs() {
        let rom = M68kRomBuilder::new(0x400)
            .move_b_imm_to_abs(0x55, 0xA10003)
            .build();

        // MOVEQ #$55, D1 + MOVE.B D1, $A10003 at offset 0x400
        // MOVEQ #$55, D1 -> 0x7255
        assert_eq!(rom[0x400], 0x72);
        assert_eq!(rom[0x401], 0x55);
        // MOVE.B D1, abs.L -> 0x13C1
        assert_eq!(rom[0x402], 0x13);
        assert_eq!(rom[0x403], 0xC1);
        // Address
        assert_eq!(rom[0x404], 0x00);
        assert_eq!(rom[0x405], 0xA1);
        assert_eq!(rom[0x406], 0x00);
        assert_eq!(rom[0x407], 0x03);
    }

    #[test]
    fn test_move_b_abs_to_d0() {
        let rom = M68kRomBuilder::new(0x400)
            .move_b_abs_to_d0(0xA10001)
            .build();

        // MOVE.B $A10001, D0 at offset 0x400
        // Opcode: 0x1039
        assert_eq!(rom[0x400], 0x10);
        assert_eq!(rom[0x401], 0x39);
        // Address
        assert_eq!(rom[0x402], 0x00);
        assert_eq!(rom[0x403], 0xA1);
        assert_eq!(rom[0x404], 0x00);
        assert_eq!(rom[0x405], 0x01);
    }

    #[test]
    fn test_move_l_d0_to_abs() {
        let rom = M68kRomBuilder::new(0x400)
            .move_l_d0_to_abs(0xFF0000)
            .build();

        // MOVE.L D0, $FF0000 at offset 0x400
        // Opcode: 0x23C0
        assert_eq!(rom[0x400], 0x23);
        assert_eq!(rom[0x401], 0xC0);
        // Address
        assert_eq!(rom[0x402], 0x00);
        assert_eq!(rom[0x403], 0xFF);
        assert_eq!(rom[0x404], 0x00);
        assert_eq!(rom[0x405], 0x00);
    }

    #[test]
    fn test_moveq_imm_to_d0() {
        let rom = M68kRomBuilder::new(0x400).moveq_imm_to_d0(42).build();

        // MOVEQ #42, D0 at offset 0x400
        // Opcode: 0x702A
        assert_eq!(rom[0x400], 0x70);
        assert_eq!(rom[0x401], 0x2A);
    }

    #[test]
    fn test_delay() {
        let mut builder = M68kRomBuilder::new(0x400);
        builder.delay(3);

        assert_eq!(builder.code_size_words(), 3);

        let rom = builder.build();
        // Check all 3 NOPs
        for i in 0..3 {
            let offset = 0x400 + i * 2;
            assert_eq!(rom[offset], 0x4E);
            assert_eq!(rom[offset + 1], 0x71);
        }
    }

    #[test]
    fn test_complex_program() {
        let rom = M68kRomBuilder::new(0x400)
            .move_b_abs_to_d0(0xA10001) // Read version register
            .move_l_d0_to_abs(0xFF0000) // Store in RAM
            .nop()
            .nop()
            .stop()
            .build();

        // Verify ROM is valid
        assert!(rom.len() >= 0x400 + 6 * 2); // At least 6 words of code

        // Verify header
        let sp = u32::from_be_bytes([rom[0], rom[1], rom[2], rom[3]]);
        assert_eq!(sp, 0x00FF_FFFE);

        let pc = u32::from_be_bytes([rom[4], rom[5], rom[6], rom[7]]);
        assert_eq!(pc, 0x400);
    }
}
