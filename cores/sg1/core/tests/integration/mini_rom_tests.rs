//! Mini ROM tests - Testing multiple instructions in sequence
//!
//! These tests create small ROM programs with multiple instructions and verify
//! that the CPU executes them correctly in sequence.

use retro_core::cpu::Addressable;
use sg1_core::bus::AddressBus;
use sg1_core::cartridge::composable::build_genesis_cartridge;
use sg1_core::cpu68k::M68k;

/// Helper to create a ROM with instructions and initialize CPU
fn create_rom_and_cpu(instructions: &[u16]) -> (M68k, AddressBus) {
    let mut cpu = M68k::new();
    let mut bus = AddressBus::new();

    // Create ROM with reset vectors and instructions (minimum 512 bytes for valid header)
    let mut rom = vec![0u8; 0x1000];

    // Reset vectors
    rom[0x00] = 0x00;
    rom[0x01] = 0x00;
    rom[0x02] = 0x10;
    rom[0x03] = 0x00; // Initial SSP = 0x00001000
    rom[0x04] = 0x00;
    rom[0x05] = 0x00;
    rom[0x06] = 0x04;
    rom[0x07] = 0x00; // Initial PC = 0x00000400

    // Genesis header (required for factory function)
    rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
    rom[0x120..0x138].copy_from_slice(b"TEST ROM                ");

    // Write instructions starting at 0x400
    for (i, &instr) in instructions.iter().enumerate() {
        let addr = 0x400 + (i * 2);
        rom[addr] = (instr >> 8) as u8;
        rom[addr + 1] = (instr & 0xFF) as u8;
    }

    // Create cartridge using factory function
    let cartridge = build_genesis_cartridge(rom).expect("Failed to create test cartridge");
    bus.set_cartridge(cartridge);
    cpu.reset(&mut bus);

    (cpu, bus)
}

/// Helper to load a pre-built ROM (adds Genesis header if needed)
fn load_rom_to_bus(bus: &mut AddressBus, mut rom: Vec<u8>) {
    // Ensure minimum size for header
    if rom.len() < 512 {
        rom.resize(512, 0);
    }

    // Add Genesis header if not present
    if &rom[0x100..0x110] != b"SEGA MEGA DRIVE " {
        rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
        rom[0x120..0x138].copy_from_slice(b"TEST ROM                ");
    }

    let cartridge = build_genesis_cartridge(rom).expect("Failed to create test cartridge");
    bus.set_cartridge(cartridge);
}

#[test]
fn test_sequence_moveq_add_move() {
    // Program:
    // MOVEQ #5, D0      ; D0 = 5
    // MOVEQ #3, D1      ; D1 = 3
    // ADD.W D1, D0      ; D0 = D0 + D1 = 8
    // MOVE.L D0, D2     ; D2 = D0 = 8

    let instructions = [
        0x7005, // MOVEQ #5, D0
        0x7203, // MOVEQ #3, D1
        0xD041, // ADD.W D1, D0
        0x2400, // MOVE.L D0, D2
    ];

    let (mut cpu, mut bus) = create_rom_and_cpu(&instructions);

    // Execute MOVEQ #5, D0
    cpu.step(&mut bus);
    assert_eq!(cpu.d[0], 5, "After MOVEQ #5, D0 should be 5");
    assert_eq!(cpu.pc, 0x00000402, "PC should advance to next instruction");

    // Execute MOVEQ #3, D1
    cpu.step(&mut bus);
    assert_eq!(cpu.d[1], 3, "After MOVEQ #3, D1 should be 3");
    assert_eq!(cpu.pc, 0x00000404, "PC should advance");

    // Execute ADD.W D1, D0
    cpu.step(&mut bus);
    assert_eq!(cpu.d[0], 8, "After ADD, D0 should be 8");
    assert_eq!(cpu.pc, 0x00000406, "PC should advance");

    // Execute MOVE.L D0, D2
    cpu.step(&mut bus);
    assert_eq!(cpu.d[2], 8, "After MOVE, D2 should be 8");
    assert_eq!(cpu.pc, 0x00000408, "PC should advance");
}

#[test]
fn test_sequence_loop_with_dbra() {
    // Program: Simple loop that counts down
    // MOVEQ #5, D0      ; Counter = 5
    // loop:
    // SUBQ.W #1, D1     ; D1--
    // DBRA D0, loop     ; Loop while D0 >= 0
    // MOVEQ #-1, D2     ; Mark completion

    let instructions = [
        0x7005, // MOVEQ #5, D0
        0x5341, // SUBQ.W #1, D1 (loop start at 0x402)
        0x51C8, 0xFFFC, // DBRA D0, -4 (back to 0x402)
        0x74FF, // MOVEQ #-1, D2
    ];

    let (mut cpu, mut bus) = create_rom_and_cpu(&instructions);

    cpu.d[1] = 100; // Initial value

    // Execute MOVEQ #5, D0
    cpu.step(&mut bus);
    assert_eq!(cpu.d[0], 5);

    // Loop should execute 6 times (D0 = 5, 4, 3, 2, 1, 0)
    for i in 0..6 {
        // SUBQ.W #1, D1
        cpu.step(&mut bus);
        assert_eq!(
            cpu.d[1],
            100 - (i + 1),
            "D1 should decrement on iteration {}",
            i
        );

        // DBRA D0, loop
        cpu.step(&mut bus);

        if i < 5 {
            // Should loop back
            assert_eq!(cpu.pc, 0x00000402, "Should loop back on iteration {}", i);
        } else {
            // Should fall through
            assert_eq!(
                cpu.pc, 0x00000408,
                "Should fall through after last iteration"
            );
        }
    }

    // Execute MOVEQ #-1, D2
    cpu.step(&mut bus);
    assert_eq!(cpu.d[2], 0xFFFFFFFF, "D2 should be -1");
    assert_eq!(cpu.d[1], 94, "D1 should have decremented 6 times");
}

#[test]
fn test_sequence_conditional_branch() {
    // Program: Test conditional branching
    // MOVEQ #0, D0
    // MOVEQ #10, D1
    // CMP.W D0, D1      ; Compare D1 with D0
    // BEQ skip          ; If equal, skip (won't happen)
    // MOVEQ #1, D2      ; This should execute
    // skip:
    // MOVEQ #2, D3      ; This always executes

    let instructions = [
        0x7000, // MOVEQ #0, D0 at 0x400
        0x720A, // MOVEQ #10, D1 at 0x402
        0xB240, // CMP.W D0, D1 at 0x404
        0x6700, 0x0002, // BEQ +2 (word displacement): to 0x40A, the MOVEQ #1 itself
        0x7401, // MOVEQ #1, D2 at 0x40A
        0x7602, // MOVEQ #2, D3 at 0x40C
    ];

    let (mut cpu, mut bus) = create_rom_and_cpu(&instructions);

    // MOVEQ #0, D0
    cpu.step(&mut bus);
    assert_eq!(cpu.d[0], 0);

    // MOVEQ #10, D1
    cpu.step(&mut bus);
    assert_eq!(cpu.d[1], 10);

    // CMP.W D0, D1
    cpu.step(&mut bus);
    // Z flag should be clear (not equal)

    // BEQ skip (should not branch)
    cpu.step(&mut bus);
    assert_eq!(
        cpu.pc, 0x0000040A,
        "Should not branch, continue to next instruction"
    );

    // MOVEQ #1, D2 (should execute)
    cpu.step(&mut bus);
    assert_eq!(cpu.d[2], 1, "D2 should be set");

    // MOVEQ #2, D3
    cpu.step(&mut bus);
    assert_eq!(cpu.d[3], 2, "D3 should be set");
}

#[test]
fn test_sequence_subroutine_call() {
    // Program: JSR and RTS
    // Main:
    //   MOVEQ #5, D0
    //   JSR subroutine
    //   MOVEQ #10, D2     ; After return
    //   NOP
    // subroutine:
    //   MOVEQ #7, D1
    //   RTS

    let mut rom = vec![0u8; 0x1000];

    // Reset vectors
    rom[0x00] = 0x00;
    rom[0x01] = 0xFF;
    rom[0x02] = 0xFF;
    rom[0x03] = 0xFC; // Initial SSP = 0x00FFFFFC (top of RAM)
    rom[0x04] = 0x00;
    rom[0x05] = 0x00;
    rom[0x06] = 0x04;
    rom[0x07] = 0x00; // Initial PC = 0x00000400

    // Main program at 0x400
    rom[0x400] = 0x70;
    rom[0x401] = 0x05; // MOVEQ #5, D0
    rom[0x402] = 0x4E;
    rom[0x403] = 0xB9; // JSR
    rom[0x404] = 0x00;
    rom[0x405] = 0x00; // (absolute long address)
    rom[0x406] = 0x04;
    rom[0x407] = 0x10; // 0x00000410 (subroutine)
    rom[0x408] = 0x74;
    rom[0x409] = 0x0A; // MOVEQ #10, D2
    rom[0x40A] = 0x4E;
    rom[0x40B] = 0x71; // NOP

    // Subroutine at 0x410
    rom[0x410] = 0x72;
    rom[0x411] = 0x07; // MOVEQ #7, D1
    rom[0x412] = 0x4E;
    rom[0x413] = 0x75; // RTS

    let mut cpu = M68k::new();
    let mut bus = AddressBus::new();
    load_rom_to_bus(&mut bus, rom);
    cpu.reset(&mut bus);

    // MOVEQ #5, D0
    cpu.step(&mut bus);
    assert_eq!(cpu.d[0], 5);
    assert_eq!(cpu.pc, 0x00000402);

    // JSR subroutine
    let sp_before = cpu.a[7];
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x00000410, "PC should jump to subroutine");
    assert_eq!(cpu.a[7], sp_before - 4, "SP should be decremented by 4");

    // MOVEQ #7, D1 (inside subroutine)
    cpu.step(&mut bus);
    assert_eq!(cpu.d[1], 7);
    assert_eq!(cpu.pc, 0x00000412);

    // RTS
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x00000408, "PC should return to after JSR");
    assert_eq!(cpu.a[7], sp_before, "SP should be restored");

    // MOVEQ #10, D2
    cpu.step(&mut bus);
    assert_eq!(cpu.d[2], 10);
}

#[test]
fn test_sequence_memory_operations() {
    // Program: Test memory read/write sequence
    // MOVEQ #0, D0
    // MOVE.L #$FF1000, A0   ; Point to RAM
    // MOVE.W #$1234, (A0)   ; Write to memory
    // MOVE.W (A0), D1       ; Read from memory
    // ADDQ.W #2, A0         ; Advance pointer
    // MOVE.W #$5678, (A0)   ; Write another value

    let mut rom = vec![0u8; 0x1000];

    // Reset vectors
    rom[0x00] = 0x00;
    rom[0x01] = 0x00;
    rom[0x02] = 0xFF;
    rom[0x03] = 0x00; // SSP
    rom[0x04] = 0x00;
    rom[0x05] = 0x00;
    rom[0x06] = 0x04;
    rom[0x07] = 0x00; // PC

    // Program at 0x400
    rom[0x400] = 0x70;
    rom[0x401] = 0x00; // MOVEQ #0, D0
    rom[0x402] = 0x20;
    rom[0x403] = 0x7C; // MOVE.L #imm, A0
    rom[0x404] = 0x00;
    rom[0x405] = 0xFF; // (high word)
    rom[0x406] = 0x10;
    rom[0x407] = 0x00; // (low word) = 0x00FF1000
    rom[0x408] = 0x30;
    rom[0x409] = 0xBC; // MOVE.W #imm, (A0); 0xFC would make it (A0)+
    rom[0x40A] = 0x12;
    rom[0x40B] = 0x34; // = 0x1234
    rom[0x40C] = 0x32;
    rom[0x40D] = 0x10; // MOVE.W (A0), D1
    rom[0x40E] = 0x54;
    rom[0x40F] = 0x48; // ADDQ.W #2, A0
    rom[0x410] = 0x30;
    rom[0x411] = 0xBC; // MOVE.W #imm, (A0)
    rom[0x412] = 0x56;
    rom[0x413] = 0x78; // = 0x5678

    let mut cpu = M68k::new();
    let mut bus = AddressBus::new();
    load_rom_to_bus(&mut bus, rom);
    cpu.reset(&mut bus);

    // MOVEQ #0, D0
    cpu.step(&mut bus);
    assert_eq!(cpu.d[0], 0);

    // MOVE.L #$FF1000, A0
    cpu.step(&mut bus);
    assert_eq!(cpu.a[0], 0x00FF1000, "A0 should point to RAM");

    // MOVE.W #$1234, (A0)
    cpu.step(&mut bus);
    let value1 = bus.read_word(0x00FF1000);
    assert_eq!(value1, 0x1234, "Memory should contain 0x1234");
    assert_eq!(
        cpu.a[0], 0x00FF1000,
        "A0 should not change after MOVE.W #imm, (A0)"
    );

    // MOVE.W (A0), D1
    cpu.step(&mut bus);
    assert_eq!(
        cpu.a[0], 0x00FF1000,
        "A0 should not change after MOVE.W (A0), D1"
    );
    assert_eq!(
        cpu.d[1] & 0xFFFF,
        0x1234,
        "D1 should contain value from memory"
    );

    // ADDQ.W #2, A0
    cpu.step(&mut bus);
    assert_eq!(cpu.a[0], 0x00FF1002, "A0 should advance by 2");

    // MOVE.W #$5678, (A0)
    cpu.step(&mut bus);
    let value2 = bus.read_word(0x00FF1002);
    assert_eq!(value2, 0x5678, "Memory at A0 should contain 0x5678");
}

#[test]
fn test_sequence_arithmetic_with_flags() {
    // Program: Test arithmetic sequence with flag checks
    // MOVEQ #-1, D0     ; D0 = 0xFFFFFFFF
    // ADDQ.L #1, D0     ; D0 = 0, Z=1, C=1
    // MOVEQ #0, D1
    // BEQ zero_branch   ; Should branch (Z=1)
    // MOVEQ #99, D1     ; Should not execute
    // zero_branch:
    // MOVEQ #1, D2      ; Should execute

    let instructions = [
        0x70FF, // MOVEQ #-1, D0 at 0x400
        0x5280, // ADDQ.L #1, D0 at 0x402
        0x7200, // MOVEQ #0, D1 at 0x404
        0x6700,
        0x0004, // BEQ +4 (word displacement) at 0x406, PC after=0x408, target=0x408+4=0x40C
        0x7263, // MOVEQ #99, D1 (should skip) at 0x40A
        0x7401, // MOVEQ #1, D2 at 0x40C
    ];

    let (mut cpu, mut bus) = create_rom_and_cpu(&instructions);

    // MOVEQ #-1, D0
    cpu.step(&mut bus);
    assert_eq!(cpu.d[0], 0xFFFFFFFF);

    // ADDQ.L #1, D0
    cpu.step(&mut bus);
    assert_eq!(cpu.d[0], 0, "D0 should wrap to 0");

    // MOVEQ #0, D1
    cpu.step(&mut bus);

    // BEQ zero_branch (should branch since Z=1)
    let pc_before_branch = cpu.pc;
    cpu.step(&mut bus);
    // BEQ is 4 bytes (word displacement), skip MOVEQ #99 (2 bytes) -> +6 total
    assert_eq!(cpu.pc, pc_before_branch + 6, "Should branch forward");

    // MOVEQ #1, D2 (after branch)
    cpu.step(&mut bus);
    assert_eq!(cpu.d[2], 1);
    assert_eq!(
        cpu.d[1], 0,
        "D1 should still be 0 (didn't execute MOVEQ #99)"
    );
}

#[test]
fn test_sequence_exception_trap() {
    // Program: Test TRAP exception
    // MOVEQ #5, D0
    // TRAP #0
    // MOVEQ #99, D1     ; Should not execute (PC changed by TRAP)

    let mut rom = vec![0u8; 0x1000];

    // Reset vectors
    rom[0x00] = 0x00;
    rom[0x01] = 0x00;
    rom[0x02] = 0xFF;
    rom[0x03] = 0x00; // SSP
    rom[0x04] = 0x00;
    rom[0x05] = 0x00;
    rom[0x06] = 0x04;
    rom[0x07] = 0x00; // PC

    // TRAP #0 vector (32*4 = 0x80) points to 0x00000500
    rom[0x80] = 0x00;
    rom[0x81] = 0x00;
    rom[0x82] = 0x05;
    rom[0x83] = 0x00;

    // Main program at 0x400
    rom[0x400] = 0x70;
    rom[0x401] = 0x05; // MOVEQ #5, D0
    rom[0x402] = 0x4E;
    rom[0x403] = 0x40; // TRAP #0
    rom[0x404] = 0x72;
    rom[0x405] = 0x63; // MOVEQ #99, D1 (shouldn't execute)

    // TRAP handler at 0x500
    rom[0x500] = 0x74;
    rom[0x501] = 0x07; // MOVEQ #7, D2

    let mut cpu = M68k::new();
    let mut bus = AddressBus::new();
    load_rom_to_bus(&mut bus, rom);
    cpu.reset(&mut bus);

    // MOVEQ #5, D0
    cpu.step(&mut bus);
    assert_eq!(cpu.d[0], 5);

    // TRAP #0
    let sp_before = cpu.a[7];
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x00000500, "PC should jump to TRAP handler");
    assert_eq!(cpu.a[7], sp_before - 6, "SP should have SR and PC pushed");
    assert_eq!(cpu.sr & 0x2000, 0x2000, "Should be in supervisor mode");

    // Verify D1 was not modified (MOVEQ #99 didn't execute)
    assert_eq!(cpu.d[1], 0, "D1 should still be 0");

    // Execute instruction in TRAP handler
    cpu.step(&mut bus);
    assert_eq!(cpu.d[2], 7, "TRAP handler should execute");
}

#[test]
fn test_sequence_multiple_data_sizes() {
    // Program: Mix byte, word, and long operations
    // MOVE.B #$AB, D0     ; D0 = 0x000000AB
    // MOVE.W #$1234, D1   ; D1 = 0x00001234
    // MOVE.L #$FEDCBA98, D2  ; D2 = 0xFEDCBA98
    // ADD.B D0, D1        ; D1 = 0x000012DF (0x34 + 0xAB = 0xDF)
    // ADD.W D1, D2        ; D2 = 0xFEDCCD77 (0xBA98 + 0x12DF = 0xCD77, upper word kept)

    let mut rom = vec![0u8; 0x1000];

    // Reset vectors
    rom[0x00] = 0x00;
    rom[0x01] = 0x00;
    rom[0x02] = 0xFF;
    rom[0x03] = 0x00;
    rom[0x04] = 0x00;
    rom[0x05] = 0x00;
    rom[0x06] = 0x04;
    rom[0x07] = 0x00;

    // Program
    rom[0x400] = 0x10;
    rom[0x401] = 0x3C; // MOVE.B #imm, D0
    rom[0x402] = 0x00;
    rom[0x403] = 0xAB;
    rom[0x404] = 0x32;
    rom[0x405] = 0x3C; // MOVE.W #imm, D1
    rom[0x406] = 0x12;
    rom[0x407] = 0x34;
    rom[0x408] = 0x24;
    rom[0x409] = 0x3C; // MOVE.L #imm, D2
    rom[0x40A] = 0xFE;
    rom[0x40B] = 0xDC;
    rom[0x40C] = 0xBA;
    rom[0x40D] = 0x98;
    rom[0x40E] = 0xD2;
    rom[0x40F] = 0x00; // ADD.B D0, D1
    rom[0x410] = 0xD4;
    rom[0x411] = 0x41; // ADD.W D1, D2

    let mut cpu = M68k::new();
    let mut bus = AddressBus::new();
    load_rom_to_bus(&mut bus, rom);
    cpu.reset(&mut bus);

    // MOVE.B #$AB, D0
    cpu.step(&mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0xAB);

    // MOVE.W #$1234, D1
    cpu.step(&mut bus);
    assert_eq!(cpu.d[1] & 0xFFFF, 0x1234);

    // MOVE.L #$FEDCBA98, D2
    cpu.step(&mut bus);
    assert_eq!(cpu.d[2], 0xFEDCBA98);

    // ADD.B D0, D1
    cpu.step(&mut bus);
    let expected_byte = (0x34u8).wrapping_add(0xAB);
    assert_eq!(cpu.d[1] & 0xFF, expected_byte as u32);

    // ADD.W D1, D2
    let d1_word_before = cpu.d[1] & 0xFFFF;
    let d2_word_before = cpu.d[2] & 0xFFFF;
    cpu.step(&mut bus);
    let expected_word = d2_word_before.wrapping_add(d1_word_before) & 0xFFFF;
    assert_eq!(
        cpu.d[2] & 0xFFFF,
        expected_word,
        "ADD.W D1, D2: {:04X} + {:04X} should = {:04X}",
        d2_word_before,
        d1_word_before,
        expected_word
    );
}
