use retro_core::cpu::{Addressable, Resettable};
use retro_core::memory::Ram;
use sg1_core::z80::Z80;

/// Helper: Execute a mini ROM and return final CPU state
fn execute_rom(rom: &[u8]) -> (Z80, Ram) {
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Load ROM at address 0
    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    cpu.reset();
    cpu.pc = 0;

    // Execute until we hit a HALT (0x76) or exceed cycle limit
    let max_cycles = 100000;
    let mut total_cycles = 0;

    loop {
        let opcode = ram.read(cpu.pc as u32);
        if opcode == 0x76 {
            // HALT
            break;
        }

        let cycles = cpu.step(&mut ram);
        total_cycles += cycles;

        if total_cycles > max_cycles {
            panic!("Exceeded max cycles (infinite loop?)");
        }
    }

    (cpu, ram)
}

// ============================================================================
// Basic IX/IY Operations
// ============================================================================

#[test]
fn test_ld_ix_immediate() {
    // Mini ROM: Load IX with immediate value
    let rom = vec![
        0xDD, 0x21, 0x34, 0x12, // LD IX, 0x1234
        0x76, // HALT
    ];

    let (cpu, _ram) = execute_rom(&rom);

    assert_eq!(cpu.ix, 0x1234);
}

#[test]
fn test_ld_iy_immediate() {
    // Mini ROM: Load IY with immediate value
    let rom = vec![
        0xFD, 0x21, 0xCD, 0xAB, // LD IY, 0xABCD
        0x76, // HALT
    ];

    let (cpu, _ram) = execute_rom(&rom);

    assert_eq!(cpu.iy, 0xABCD);
}

#[test]
fn test_ix_memory_store_load() {
    // Mini ROM: Store IX to memory and load back
    let rom = vec![
        0xDD, 0x21, 0x78, 0x56, // LD IX, 0x5678
        0xDD, 0x22, 0x00, 0x30, // LD (0x3000), IX
        0xDD, 0x21, 0x00, 0x00, // LD IX, 0x0000 (clear)
        0xDD, 0x2A, 0x00, 0x30, // LD IX, (0x3000)
        0x76, // HALT
    ];

    let (cpu, mut ram) = execute_rom(&rom);

    assert_eq!(cpu.ix, 0x5678);
    assert_eq!(ram.read(0x3000), 0x78); // Low byte
    assert_eq!(ram.read(0x3001), 0x56); // High byte
}

#[test]
fn test_add_ix_bc() {
    // Mini ROM: Test ADD IX, BC
    let rom = vec![
        0xDD, 0x21, 0x00, 0x10, // LD IX, 0x1000
        0x01, 0x34, 0x02, // LD BC, 0x0234
        0xDD, 0x09, // ADD IX, BC
        0x76, // HALT
    ];

    let (cpu, _ram) = execute_rom(&rom);

    assert_eq!(cpu.ix, 0x1234);
    assert!(!cpu.flag_n()); // N flag cleared
}

#[test]
fn test_add_ix_with_carry() {
    // Mini ROM: Test ADD IX with carry
    let rom = vec![
        0xDD, 0x21, 0xFF, 0xFF, // LD IX, 0xFFFF
        0x01, 0x02, 0x00, // LD BC, 0x0002
        0xDD, 0x09, // ADD IX, BC
        0x76, // HALT
    ];

    let (cpu, _ram) = execute_rom(&rom);

    assert_eq!(cpu.ix, 0x0001);
    assert!(cpu.flag_c()); // Carry flag set
}

#[test]
fn test_inc_dec_ix() {
    // Mini ROM: Test INC and DEC IX
    let rom = vec![
        0xDD, 0x21, 0x34, 0x12, // LD IX, 0x1234
        0xDD, 0x23, // INC IX
        0xDD, 0x23, // INC IX
        0xDD, 0x2B, // DEC IX
        0x76, // HALT
    ];

    let (cpu, _ram) = execute_rom(&rom);

    assert_eq!(cpu.ix, 0x1235); // 0x1234 + 2 - 1
}

#[test]
fn test_push_pop_ix() {
    // Mini ROM: Test PUSH and POP IX
    let rom = vec![
        0x31, 0x00, 0x20, // LD SP, 0x2000
        0xDD, 0x21, 0xEF, 0xBE, // LD IX, 0xBEEF
        0xDD, 0xE5, // PUSH IX
        0xDD, 0x21, 0x00, 0x00, // LD IX, 0x0000 (clear)
        0xDD, 0xE1, // POP IX
        0x76, // HALT
    ];

    let (cpu, _ram) = execute_rom(&rom);

    assert_eq!(cpu.ix, 0xBEEF);
    assert_eq!(cpu.sp, 0x2000); // SP restored
}

#[test]
fn test_ex_sp_ix() {
    // Mini ROM: Test EX (SP), IX
    let rom = vec![
        0x31, 0x00, 0x20, // LD SP, 0x2000
        // Setup stack with value
        0x21, 0xCD, 0xAB, // LD HL, 0xABCD
        0xE5, // PUSH HL (stack now has 0xABCD)
        // Setup IX
        0xDD, 0x21, 0x34, 0x12, // LD IX, 0x1234
        // Exchange
        0xDD, 0xE3, // EX (SP), IX
        0x76, // HALT
    ];

    let (cpu, mut ram) = execute_rom(&rom);

    assert_eq!(cpu.ix, 0xABCD); // IX got value from stack
    assert_eq!(ram.read(0x1FFE), 0x34); // Stack got IX low
    assert_eq!(ram.read(0x1FFF), 0x12); // Stack got IX high
}

#[test]
fn test_ld_sp_ix() {
    // Mini ROM: Test LD SP, IX
    let rom = vec![
        0xDD, 0x21, 0xFE, 0xFF, // LD IX, 0xFFFE
        0xDD, 0xF9, // LD SP, IX
        0x76, // HALT
    ];

    let (cpu, _ram) = execute_rom(&rom);

    assert_eq!(cpu.sp, 0xFFFE);
}

#[test]
fn test_iy_operations() {
    // Mini ROM: Test IY equivalents
    let rom = vec![
        0xFD, 0x21, 0x00, 0x80, // LD IY, 0x8000
        0xFD, 0x23, // INC IY
        0x11, 0x00, 0x01, // LD DE, 0x0100
        0xFD, 0x19, // ADD IY, DE
        0x76, // HALT
    ];

    let (cpu, _ram) = execute_rom(&rom);

    assert_eq!(cpu.iy, 0x8101); // 0x8000 + 1 + 0x0100
}

#[test]
fn test_add_ix_ix() {
    // Mini ROM: Test ADD IX, IX (doubles IX)
    let rom = vec![
        0xDD, 0x21, 0x34, 0x12, // LD IX, 0x1234
        0xDD, 0x29, // ADD IX, IX
        0x76, // HALT
    ];

    let (cpu, _ram) = execute_rom(&rom);

    assert_eq!(cpu.ix, 0x2468); // 0x1234 * 2
}

#[test]
fn test_ix_as_pointer() {
    // Mini ROM: Use IX as base pointer
    let rom = vec![
        0xDD, 0x21, 0x00, 0x40, // LD IX, 0x4000 (base address)
        0xDD, 0x22, 0x00, 0x50, // LD (0x5000), IX (save base)
        // Increment base
        0xDD, 0x23, // INC IX
        0xDD, 0x23, // INC IX
        0x76, // HALT
    ];

    let (cpu, mut ram) = execute_rom(&rom);

    assert_eq!(cpu.ix, 0x4002); // Base + 2
    assert_eq!(ram.read(0x5000), 0x00); // Original base low
    assert_eq!(ram.read(0x5001), 0x40); // Original base high
}

#[test]
fn test_mixed_ix_iy() {
    // Mini ROM: Use IX and IY together
    let rom = vec![
        0xDD, 0x21, 0x00, 0x10, // LD IX, 0x1000
        0xFD, 0x21, 0x00, 0x20, // LD IY, 0x2000
        // Add 0x100 to both
        0x01, 0x00, 0x01, // LD BC, 0x0100
        0xDD, 0x09, // ADD IX, BC
        0xFD, 0x09, // ADD IY, BC
        0x76, // HALT
    ];

    let (cpu, _ram) = execute_rom(&rom);

    assert_eq!(cpu.ix, 0x1100);
    assert_eq!(cpu.iy, 0x2100);
}

// ============================================================================
// Integration Tests: Index with Displacement (IX+d)/(IY+d)
// ============================================================================

#[test]
fn test_ld_r_ix_d_integration() {
    // Mini ROM: LD registers from (IX+d)
    let rom = vec![
        0xDD, 0x21, 0x00, 0x30, // LD IX, 0x3000
        0xDD, 0x46, 0x05, // LD B, (IX+5)   -> loads from 0x3005
        0xDD, 0x4E, 0x08, // LD C, (IX+8)   -> loads from 0x3008
        0xDD, 0x7E, 0xFE, // LD A, (IX-2)   -> loads from 0x2FFE
        0x76, // HALT
    ];

    let (cpu, mut ram) = execute_rom(&rom);

    // Verify loads
    assert_eq!(cpu.b, ram.read(0x3005));
    assert_eq!(cpu.c, ram.read(0x3008));
    assert_eq!(cpu.a, ram.read(0x2FFE));
}

#[test]
fn test_ld_ix_d_r_integration() {
    // Mini ROM: Store registers to (IX+d)
    let rom = vec![
        0x06, 0x42, // LD B, 0x42
        0x0E, 0x99, // LD C, 0x99
        0x3E, 0xAA, // LD A, 0xAA
        0xDD, 0x21, 0x00, 0x40, // LD IX, 0x4000
        0xDD, 0x70, 0x00, // LD (IX+0), B   -> stores to 0x4000
        0xDD, 0x71, 0x10, // LD (IX+16), C  -> stores to 0x4010
        0xDD, 0x77, 0xFC, // LD (IX-4), A   -> stores to 0x3FFC
        0x76, // HALT
    ];

    let (_cpu, mut ram) = execute_rom(&rom);

    assert_eq!(ram.read(0x4000), 0x42);
    assert_eq!(ram.read(0x4010), 0x99);
    assert_eq!(ram.read(0x3FFC), 0xAA);
}

#[test]
fn test_ld_ix_d_n_integration() {
    // Mini ROM: LD (IX+d), n
    let rom = vec![
        0xDD, 0x21, 0x00, 0x50, // LD IX, 0x5000
        0xDD, 0x36, 0x00, 0x11, // LD (IX+0), 0x11
        0xDD, 0x36, 0x05, 0x22, // LD (IX+5), 0x22
        0xDD, 0x36, 0xFF, 0x33, // LD (IX-1), 0x33
        0x76, // HALT
    ];

    let (_cpu, mut ram) = execute_rom(&rom);

    assert_eq!(ram.read(0x5000), 0x11);
    assert_eq!(ram.read(0x5005), 0x22);
    assert_eq!(ram.read(0x4FFF), 0x33);
}

#[test]
fn test_inc_dec_ix_d_integration() {
    // Mini ROM: INC/DEC (IX+d)
    let rom = vec![
        0xDD, 0x21, 0x00, 0x60, // LD IX, 0x6000
        0xDD, 0x36, 0x00, 0x7F, // LD (IX+0), 0x7F
        0xDD, 0x34, 0x00, // INC (IX+0)      -> 0x80 (overflow)
        0xDD, 0x36, 0x01, 0x01, // LD (IX+1), 0x01
        0xDD, 0x35, 0x01, // DEC (IX+1)      -> 0x00 (zero)
        0x76, // HALT
    ];

    let (cpu, mut ram) = execute_rom(&rom);

    // Verify memory changes
    assert_eq!(ram.read(0x6000), 0x80); // INC result
    assert_eq!(ram.read(0x6001), 0x00); // DEC result

    // Flags are from the last operation (DEC)
    assert!(cpu.flag_z()); // Zero flag from DEC
    assert!(cpu.flag_n()); // Subtract flag from DEC
}

#[test]
fn test_arithmetic_ix_d_integration() {
    // Mini ROM: Arithmetic operations with (IX+d)
    let rom = vec![
        0xDD, 0x21, 0x00, 0x70, // LD IX, 0x7000
        0xDD, 0x36, 0x00, 0x20, // LD (IX+0), 0x20
        0x3E, 0x10, // LD A, 0x10
        0xDD, 0x86, 0x00, // ADD A, (IX+0)   -> A = 0x30
        0x3E, 0x50, // LD A, 0x50
        0xDD, 0x96, 0x00, // SUB (IX+0)      -> A = 0x30
        0x76, // HALT
    ];

    let (cpu, _ram) = execute_rom(&rom);

    assert_eq!(cpu.a, 0x30);
    assert!(!cpu.flag_z());
    assert!(cpu.flag_n()); // N flag from SUB
}

#[test]
fn test_logical_ix_d_integration() {
    // Mini ROM: Logical operations with (IX+d)
    let rom = vec![
        0xDD, 0x21, 0x00, 0x80, // LD IX, 0x8000
        0xDD, 0x36, 0x00, 0x0F, // LD (IX+0), 0x0F
        0x3E, 0xFF, // LD A, 0xFF
        0xDD, 0xA6, 0x00, // AND (IX+0)      -> A = 0x0F
        0x3E, 0xAA, // LD A, 0xAA
        0xDD, 0xAE, 0x00, // XOR (IX+0)      -> A = 0xA5
        0x76, // HALT
    ];

    let (cpu, _ram) = execute_rom(&rom);

    assert_eq!(cpu.a, 0xA5);
}

#[test]
fn test_cp_ix_d_integration() {
    // Mini ROM: CP (IX+d)
    let rom = vec![
        0xDD, 0x21, 0x00, 0x90, // LD IX, 0x9000
        0xDD, 0x36, 0x00, 0x42, // LD (IX+0), 0x42
        0x3E, 0x42, // LD A, 0x42
        0xDD, 0xBE, 0x00, // CP (IX+0)       -> Equal
        0x76, // HALT
    ];

    let (cpu, _ram) = execute_rom(&rom);

    assert_eq!(cpu.a, 0x42); // A unchanged
    assert!(cpu.flag_z()); // Equal
    assert!(cpu.flag_n()); // Subtract flag
}

#[test]
fn test_iy_d_with_negative_displacement() {
    // Mini ROM: IY with large negative displacement
    let rom = vec![
        0xFD, 0x21, 0x00, 0xA0, // LD IY, 0xA000
        0x3E, 0xDD, // LD A, 0xDD
        0xFD, 0x77, 0x80, // LD (IY-128), A  -> stores to 0x9F80
        0xFD, 0x7E, 0x80, // LD A, (IY-128)  -> loads from 0x9F80
        0x76, // HALT
    ];

    let (cpu, mut ram) = execute_rom(&rom);

    assert_eq!(ram.read(0x9F80), 0xDD);
    assert_eq!(cpu.a, 0xDD);
}

#[test]
fn test_mixed_ix_iy_displacement() {
    // Mini ROM: Mix IX and IY with displacement
    let rom = vec![
        0xDD, 0x21, 0x00, 0xB0, // LD IX, 0xB000
        0xFD, 0x21, 0x00, 0xC0, // LD IY, 0xC000
        0x06, 0x11, // LD B, 0x11
        0x0E, 0x22, // LD C, 0x22
        0xDD, 0x70, 0x05, // LD (IX+5), B    -> 0xB005 = 0x11
        0xFD, 0x71, 0x0A, // LD (IY+10), C   -> 0xC00A = 0x22
        0xDD, 0x46, 0x05, // LD B, (IX+5)    -> B = 0x11
        0xFD, 0x4E, 0x0A, // LD C, (IY+10)   -> C = 0x22
        0x76, // HALT
    ];

    let (cpu, mut ram) = execute_rom(&rom);

    assert_eq!(ram.read(0xB005), 0x11);
    assert_eq!(ram.read(0xC00A), 0x22);
    assert_eq!(cpu.b, 0x11);
    assert_eq!(cpu.c, 0x22);
}

// ============================================================================
// Integration Tests: CB DD/FD - Bit Operations with Displacement
// ============================================================================

#[test]
fn test_rotate_ix_d_integration() {
    // Mini ROM: Rotate operations on (IX+d)
    let rom = vec![
        0xDD, 0x21, 0x00, 0x50, // LD IX, 0x5000
        0xDD, 0x36, 0x00, 0x81, // LD (IX+0), 0x81
        0xDD, 0xCB, 0x00, 0x06, // RLC (IX+0)  -> 0x03, C=1
        0xDD, 0x36, 0x01, 0x83, // LD (IX+1), 0x83
        0xDD, 0xCB, 0x01, 0x0E, // RRC (IX+1)  -> 0xC1, C=1
        0x76, // HALT
    ];

    let (cpu, mut ram) = execute_rom(&rom);

    assert_eq!(ram.read(0x5000), 0x03); // RLC result
    assert_eq!(ram.read(0x5001), 0xC1); // RRC result
    assert!(cpu.flag_c());
}

#[test]
fn test_shift_iy_d_integration() {
    // Mini ROM: Shift operations on (IY+d)
    let rom = vec![
        0xFD, 0x21, 0x00, 0x60, // LD IY, 0x6000
        0xFD, 0x36, 0x00, 0xC5, // LD (IY+0), 0xC5
        0xFD, 0xCB, 0x00, 0x26, // SLA (IY+0)  -> 0x8A, C=1
        0xFD, 0x36, 0x01, 0x83, // LD (IY+1), 0x83
        0xFD, 0xCB, 0x01, 0x2E, // SRA (IY+1)  -> 0xC1, C=1
        0x76, // HALT
    ];

    let (cpu, mut ram) = execute_rom(&rom);

    assert_eq!(ram.read(0x6000), 0x8A); // SLA result
    assert_eq!(ram.read(0x6001), 0xC1); // SRA result (sign-extended)
    assert!(cpu.flag_c());
}

#[test]
fn test_bit_ix_d_integration() {
    // Mini ROM: BIT operations on (IX+d)
    let rom = vec![
        0xDD, 0x21, 0x00, 0x70, // LD IX, 0x7000
        0xDD, 0x36, 0x00, 0xFE, // LD (IX+0), 0xFE (bit 0 clear)
        0xDD, 0xCB, 0x00, 0x46, // BIT 0, (IX+0) -> Z=1
        0xDD, 0x36, 0x01, 0x80, // LD (IX+1), 0x80 (bit 7 set)
        0xDD, 0xCB, 0x01, 0x7E, // BIT 7, (IX+1) -> Z=0, S=1
        0x76, // HALT
    ];

    let (cpu, mut ram) = execute_rom(&rom);

    assert_eq!(ram.read(0x7000), 0xFE);
    assert_eq!(ram.read(0x7001), 0x80);
    // Last BIT was on bit 7 (set)
    assert!(!cpu.flag_z()); // Bit 7 is set
    assert!(cpu.flag_s());
}

#[test]
fn test_set_res_iy_d_integration() {
    // Mini ROM: SET and RES operations on (IY+d)
    let rom = vec![
        0xFD, 0x21, 0x00, 0x80, // LD IY, 0x8000
        0xFD, 0x36, 0x00, 0x00, // LD (IY+0), 0x00
        0xFD, 0xCB, 0x00, 0xCE, // SET 1, (IY+0) -> 0x02
        0xFD, 0xCB, 0x00, 0xF6, // SET 6, (IY+0) -> 0x42
        0xFD, 0x36, 0x01, 0xFF, // LD (IY+1), 0xFF
        0xFD, 0xCB, 0x01, 0x86, // RES 0, (IY+1) -> 0xFE
        0xFD, 0xCB, 0x01, 0xBE, // RES 7, (IY+1) -> 0x7E
        0x76, // HALT
    ];

    let (_cpu, mut ram) = execute_rom(&rom);

    assert_eq!(ram.read(0x8000), 0x42); // SET 1 and 6
    assert_eq!(ram.read(0x8001), 0x7E); // RES 0 and 7
}

#[test]
fn test_mixed_cb_ix_operations() {
    // Mini ROM: Mix of CB operations on (IX+d)
    let rom = vec![
        0xDD, 0x21, 0x00, 0x90, // LD IX, 0x9000
        0xDD, 0x36, 0x05, 0x55, // LD (IX+5), 0x55 (01010101)
        0xDD, 0xCB, 0x05, 0x26, // SLA (IX+5)  -> 0xAA (10101010)
        0xDD, 0xCB, 0x05, 0x76, // BIT 6, (IX+5) -> not set, Z=1
        0xDD, 0xCB, 0x05, 0xF6, // SET 6, (IX+5) -> 0xEA (11101010)
        0xDD, 0xCB, 0x05, 0x76, // BIT 6, (IX+5) -> set, Z=0
        0x76, // HALT
    ];

    let (cpu, mut ram) = execute_rom(&rom);

    assert_eq!(ram.read(0x9005), 0xEA);
    assert!(!cpu.flag_z()); // Last BIT found bit set
}

#[test]
fn test_cb_negative_displacement() {
    // Mini ROM: CB operations with negative displacement
    let rom = vec![
        0xFD, 0x21, 0x00, 0xA0, // LD IY, 0xA000
        0xFD, 0x36, 0x80, 0x11, // LD (IY-128), 0x11 at 0x9F80
        0xFD, 0xCB, 0x80, 0x06, // RLC (IY-128) -> 0x22
        0xFD, 0xCB, 0x80, 0xC6, // SET 0, (IY-128) -> 0x23
        0x76, // HALT
    ];

    let (_cpu, mut ram) = execute_rom(&rom);

    assert_eq!(ram.read(0x9F80), 0x23);
}

#[test]
fn test_cb_all_rotate_ops() {
    // Mini ROM: Test all rotate operations
    let rom = vec![
        0xDD, 0x21, 0x00, 0xB0, // LD IX, 0xB000
        // RLC
        0xDD, 0x36, 0x00, 0x80, // LD (IX+0), 0x80
        0xDD, 0xCB, 0x00, 0x06, // RLC (IX+0) -> 0x01, C=1
        // RRC
        0xDD, 0x36, 0x01, 0x01, // LD (IX+1), 0x01
        0xDD, 0xCB, 0x01, 0x0E, // RRC (IX+1) -> 0x80, C=1
        // RL (with carry)
        0x37, // SCF (set carry)
        0xDD, 0x36, 0x02, 0x40, // LD (IX+2), 0x40
        0xDD, 0xCB, 0x02, 0x16, // RL (IX+2)  -> 0x81
        // RR (with carry)
        0x37, // SCF
        0xDD, 0x36, 0x03, 0x01, // LD (IX+3), 0x01
        0xDD, 0xCB, 0x03, 0x1E, // RR (IX+3)  -> 0x80, C=1
        0x76, // HALT
    ];

    let (cpu, mut ram) = execute_rom(&rom);

    assert_eq!(ram.read(0xB000), 0x01); // RLC result
    assert_eq!(ram.read(0xB001), 0x80); // RRC result
    assert_eq!(ram.read(0xB002), 0x81); // RL result
    assert_eq!(ram.read(0xB003), 0x80); // RR result
    assert!(cpu.flag_c()); // Last RR set carry
}
