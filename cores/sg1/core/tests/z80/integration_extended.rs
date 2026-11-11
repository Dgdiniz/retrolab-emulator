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
// Block Load Tests (LDI/LDD/LDIR/LDDR)
// ============================================================================

#[test]
fn test_ldi_sequence() {
    // Mini ROM: Copy 3 bytes using LDI
    let rom = [
        // Setup
        0x21, 0x00, 0x10, // LD HL, 0x1000 (source)
        0x11, 0x00, 0x20, // LD DE, 0x2000 (dest)
        0x01, 0x03, 0x00, // LD BC, 3 (count)
        // Execute LDI 3 times
        0xED, 0xA0, // LDI
        0xED, 0xA0, // LDI
        0xED, 0xA0, // LDI
        0x76, // HALT
    ];

    let (_cpu, _ram) = execute_rom(&rom);

    // Setup source data before execution
    let mut cpu2 = Z80::new();
    let mut ram2 = Ram::new(0x10000);
    for (i, &byte) in rom.iter().enumerate() {
        ram2.write(i as u32, byte);
    }
    ram2.write(0x1000, 0xAA);
    ram2.write(0x1001, 0xBB);
    ram2.write(0x1002, 0xCC);

    cpu2.reset();
    cpu2.pc = 0;

    // Execute until HALT
    loop {
        let opcode = ram2.read(cpu2.pc as u32);
        if opcode == 0x76 {
            break;
        }
        cpu2.step(&mut ram2);
    }

    // Verify data was copied
    assert_eq!(ram2.read(0x2000), 0xAA);
    assert_eq!(ram2.read(0x2001), 0xBB);
    assert_eq!(ram2.read(0x2002), 0xCC);

    // Verify pointers updated
    assert_eq!(cpu2.hl(), 0x1003);
    assert_eq!(cpu2.de(), 0x2003);
    assert_eq!(cpu2.bc(), 0x0000);
    assert!(!cpu2.flag_pv()); // BC = 0
}

#[test]
fn test_ldir_block_copy() {
    // Mini ROM: Copy 5 bytes using LDIR (single instruction, repeats)
    let rom = [
        // Setup source data
        0x21, 0x00, 0x10, // LD HL, 0x1000
        0x11, 0x00, 0x20, // LD DE, 0x2000
        0x01, 0x05, 0x00, // LD BC, 5
        0xED, 0xB0, // LDIR (repeats until BC=0)
        0x76, // HALT
    ];

    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Load ROM and source data
    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }
    ram.write(0x1000, 0x11);
    ram.write(0x1001, 0x22);
    ram.write(0x1002, 0x33);
    ram.write(0x1003, 0x44);
    ram.write(0x1004, 0x55);

    cpu.reset();
    cpu.pc = 0;

    // Execute until HALT
    loop {
        let opcode = ram.read(cpu.pc as u32);
        if opcode == 0x76 {
            break;
        }
        cpu.step(&mut ram);
    }

    // Verify all data copied
    assert_eq!(ram.read(0x2000), 0x11);
    assert_eq!(ram.read(0x2001), 0x22);
    assert_eq!(ram.read(0x2002), 0x33);
    assert_eq!(ram.read(0x2003), 0x44);
    assert_eq!(ram.read(0x2004), 0x55);

    assert_eq!(cpu.bc(), 0x0000);
    assert!(!cpu.flag_pv());
}

#[test]
fn test_lddr_reverse_copy() {
    // Mini ROM: Copy backwards using LDDR
    let rom = [
        0x21, 0x04, 0x10, // LD HL, 0x1004 (end of source)
        0x11, 0x04, 0x20, // LD DE, 0x2004 (end of dest)
        0x01, 0x05, 0x00, // LD BC, 5
        0xED, 0xB8, // LDDR
        0x76, // HALT
    ];

    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }
    ram.write(0x1000, 0xAA);
    ram.write(0x1001, 0xBB);
    ram.write(0x1002, 0xCC);
    ram.write(0x1003, 0xDD);
    ram.write(0x1004, 0xEE);

    cpu.reset();
    cpu.pc = 0;

    loop {
        let opcode = ram.read(cpu.pc as u32);
        if opcode == 0x76 {
            break;
        }
        cpu.step(&mut ram);
    }

    // Verify reverse copy
    assert_eq!(ram.read(0x2000), 0xAA);
    assert_eq!(ram.read(0x2001), 0xBB);
    assert_eq!(ram.read(0x2002), 0xCC);
    assert_eq!(ram.read(0x2003), 0xDD);
    assert_eq!(ram.read(0x2004), 0xEE);
}

// ============================================================================
// Block Compare Tests (CPI/CPD/CPIR/CPDR)
// ============================================================================

#[test]
fn test_cpir_string_search() {
    // Mini ROM: Search for byte 0x42 in array
    let rom = [
        0x3E, 0x42, // LD A, 0x42 (looking for this)
        0x21, 0x00, 0x10, // LD HL, 0x1000 (array start)
        0x01, 0x10, 0x00, // LD BC, 16 (max search length)
        0xED, 0xB1, // CPIR (search)
        0x76, // HALT
    ];

    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    // Array with 0x42 at index 5
    ram.write(0x1000, 0x10);
    ram.write(0x1001, 0x20);
    ram.write(0x1002, 0x30);
    ram.write(0x1003, 0x40);
    ram.write(0x1004, 0x41);
    ram.write(0x1005, 0x42); // Match!
    ram.write(0x1006, 0x43);

    cpu.reset();
    cpu.pc = 0;

    loop {
        let opcode = ram.read(cpu.pc as u32);
        if opcode == 0x76 {
            break;
        }
        cpu.step(&mut ram);
    }

    // Verify match found
    assert!(cpu.flag_z()); // Z=1 means match
    assert_eq!(cpu.hl(), 0x1006); // HL points past match
    assert_eq!(cpu.bc(), 0x000A); // 16 - 6 = 10 remaining
    assert!(cpu.flag_pv()); // BC != 0
}

#[test]
fn test_cpir_not_found() {
    // Mini ROM: Search for byte that doesn't exist
    let rom = [
        0x3E, 0xFF, // LD A, 0xFF (not in array)
        0x21, 0x00, 0x10, // LD HL, 0x1000
        0x01, 0x05, 0x00, // LD BC, 5
        0xED, 0xB1, // CPIR
        0x76, // HALT
    ];

    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }
    ram.write(0x1000, 0x01);
    ram.write(0x1001, 0x02);
    ram.write(0x1002, 0x03);
    ram.write(0x1003, 0x04);
    ram.write(0x1004, 0x05);

    cpu.reset();
    cpu.pc = 0;

    loop {
        let opcode = ram.read(cpu.pc as u32);
        if opcode == 0x76 {
            break;
        }
        cpu.step(&mut ram);
    }

    // Verify not found
    assert!(!cpu.flag_z()); // Z=0 means no match
    assert_eq!(cpu.bc(), 0x0000); // BC exhausted
    assert!(!cpu.flag_pv()); // BC = 0
}

// ============================================================================
// 16-bit Load Extended Tests
// ============================================================================

#[test]
fn test_ld_extended_bc_de() {
    // Mini ROM: Test LD (nn), BC and LD DE, (nn)
    let rom = [
        // Store BC to memory
        0x01, 0x34, 0x12, // LD BC, 0x1234
        0xED, 0x43, 0x00, 0x30, // LD (0x3000), BC
        // Load DE from same location
        0xED, 0x5B, 0x00, 0x30, // LD DE, (0x3000)
        0x76, // HALT
    ];

    let (cpu, mut ram) = execute_rom(&rom);

    // Verify DE loaded correctly
    assert_eq!(cpu.de(), 0x1234);

    // Verify memory contents
    assert_eq!(ram.read(0x3000), 0x34); // Low byte
    assert_eq!(ram.read(0x3001), 0x12); // High byte
}

#[test]
fn test_ld_extended_sp() {
    // Mini ROM: Test LD (nn), SP and LD SP, (nn)
    let rom = [
        // Setup SP
        0x31, 0xFE, 0xFF, // LD SP, 0xFFFE
        // Store to memory
        0xED, 0x73, 0x00, 0x40, // LD (0x4000), SP
        // Change SP
        0x31, 0x00, 0x00, // LD SP, 0x0000
        // Load back
        0xED, 0x7B, 0x00, 0x40, // LD SP, (0x4000)
        0x76, // HALT
    ];

    let (cpu, _ram) = execute_rom(&rom);

    // Verify SP restored
    assert_eq!(cpu.sp, 0xFFFE);
}

// ============================================================================
// Interrupt Mode Tests
// ============================================================================

#[test]
fn test_interrupt_modes() {
    // Mini ROM: Cycle through IM 0/1/2
    let rom = [
        0xED, 0x46, // IM 0
        0xED, 0x56, // IM 1
        0xED, 0x5E, // IM 2
        0x76, // HALT
    ];

    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    cpu.reset();
    cpu.pc = 0;

    // Execute IM 0
    cpu.step(&mut ram);
    assert_eq!(cpu.im, 0);

    // Execute IM 1
    cpu.step(&mut ram);
    assert_eq!(cpu.im, 1);

    // Execute IM 2
    cpu.step(&mut ram);
    assert_eq!(cpu.im, 2);
}

#[test]
fn test_reti_retn() {
    // Mini ROM: Test RETI (RETN has a test of its own, below)
    let rom = [
        // Setup stack
        0x31, 0x00, 0x20, // LD SP, 0x2000
        // Push return address
        0x21, 0x34, 0x12, // LD HL, 0x1234
        0xE5, // PUSH HL
        // Test RETI
        0xED, 0x4D, // RETI
        // Unreachable code (after RETI)
        0x00, 0x00,
    ];

    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    cpu.reset();
    cpu.pc = 0;

    // Execute setup
    cpu.step(&mut ram); // LD SP
    cpu.step(&mut ram); // LD HL
    cpu.step(&mut ram); // PUSH HL

    assert_eq!(cpu.sp, 0x1FFE); // Stack decreased

    // Execute RETI
    cpu.step(&mut ram);

    assert_eq!(cpu.pc, 0x1234); // Jumped to return address
    assert_eq!(cpu.sp, 0x2000); // Stack restored
}

#[test]
fn test_retn_restores_iff1() {
    // Mini ROM: Test RETN IFF restore
    let rom = [
        0x31, 0x00, 0x20, // LD SP, 0x2000
        0x21, 0x00, 0x01, // LD HL, 0x0100
        0xE5, // PUSH HL
        0xED, 0x45, // RETN
        0x76, // HALT (unreachable)
    ];

    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    cpu.reset();
    cpu.pc = 0;

    // Simulate NMI scenario: IFF1 cleared, IFF2 preserved
    cpu.iff1 = false;
    cpu.iff2 = true;

    // Execute setup
    cpu.step(&mut ram);
    cpu.step(&mut ram);
    cpu.step(&mut ram);

    // Execute RETN
    cpu.step(&mut ram);

    // Verify IFF1 restored from IFF2
    assert!(cpu.iff1);
    assert_eq!(cpu.pc, 0x0100);
}

// ============================================================================
// Combined Tests
// ============================================================================

#[test]
fn test_memory_copy_with_compare() {
    // Mini ROM: Copy data and check the first byte with CPI
    let rom = [
        // Copy 4 bytes using LDIR
        0x21, 0x00, 0x10, // LD HL, 0x1000 (source)
        0x11, 0x00, 0x20, // LD DE, 0x2000 (dest)
        0x01, 0x04, 0x00, // LD BC, 4
        0xED, 0xB0, // LDIR
        // Verify first byte copied correctly
        0x3E, 0xAA, // LD A, 0xAA (expected first byte)
        0x21, 0x00, 0x20, // LD HL, 0x2000
        0x01, 0x01, 0x00, // LD BC, 1
        0xED, 0xA1, // CPI (compare)
        0x76, // HALT
    ];

    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    for (i, &byte) in rom.iter().enumerate() {
        ram.write(i as u32, byte);
    }

    // Source data
    ram.write(0x1000, 0xAA);
    ram.write(0x1001, 0xBB);
    ram.write(0x1002, 0xCC);
    ram.write(0x1003, 0xDD);

    cpu.reset();
    cpu.pc = 0;

    loop {
        let opcode = ram.read(cpu.pc as u32);
        if opcode == 0x76 {
            break;
        }
        cpu.step(&mut ram);
    }

    // Verify copy successful
    assert_eq!(ram.read(0x2000), 0xAA);
    assert_eq!(ram.read(0x2001), 0xBB);
    assert_eq!(ram.read(0x2002), 0xCC);
    assert_eq!(ram.read(0x2003), 0xDD);

    // Verify CPI found match
    assert!(cpu.flag_z()); // Match
}
