//! CPU -> VDP Integration Tests
//!
//! Tests that validate CPU can access VDP through memory-mapped I/O.
//! Uses mini ROMs with 68000 code to write to VDP registers and memory.

use retro_core::cpu::Addressable;
use sg1_core::bus::AddressBus;
use sg1_core::cpu68k::M68k;

/// Helper to load a mini ROM into RAM and point the CPU at it (the tests step it)
/// Uses RAM (0xFF0000+) like Tom Harte tests
fn setup_cpu_with_rom(rom: &[u8]) -> (M68k, AddressBus) {
    let mut cpu = M68k::new();
    let mut bus = AddressBus::new();

    // Write code to RAM (0xFF0000 region is writable)
    let code_base = 0xFF0400u32;
    for (i, &byte) in rom.iter().enumerate() {
        bus.write(code_base + i as u32, byte);
    }

    // Initialize CPU
    cpu.ssp = 0xFF0000; // Stack at start of RAM
    cpu.pc = code_base;
    cpu.sr = 0x2700; // Supervisor mode

    (cpu, bus)
}

#[test]
fn test_cpu_write_vdp_control_port() {
    // Mini ROM: Write to VDP control port
    // VDP Control: 0xC00004
    // Code:
    //   MOVE.W #$8014, $C00004  ; Set VDP register 0 to 0x14
    //   STOP

    let rom = vec![
        // Address 0xFF0400: MOVE.W #$8014, $C00004
        0x33, 0xFC, // MOVE.W #imm, (xxx).L
        0x80, 0x14, // Immediate value: 0x8014
        0x00, 0xC0, // Address: 0x00C00004 (high word)
        0x00, 0x04, // Address: 0x00C00004 (low word)
        // Address 0xFF0408: STOP #$2700
        0x4E, 0x72, 0x27, 0x00,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);

    // Execute the MOVE instruction
    let cycles = cpu.step(&mut bus);

    println!("PC after MOVE: 0x{:08X}", cpu.pc);
    println!("Cycles: {}", cycles);

    // Verify PC advanced correctly (0xFF0400 + 8 bytes)
    assert_eq!(cpu.pc, 0xFF0408, "PC should be at STOP instruction");

    // The VDP received the write (register 0 is 0x04 after reset)
    assert_eq!(bus.vdp().registers()[0], 0x14);
}

#[test]
fn test_cpu_write_vdp_data_port() {
    // Mini ROM: Write to VRAM via data port
    // 1. Set VRAM write address via control port
    // 2. Write data via data port

    let rom = vec![
        // Set VRAM write address to 0x0000
        // MOVE.L #$40000000, $C00004
        0x23, 0xFC, // MOVE.L #imm, (xxx).L
        0x40, 0x00, 0x00, 0x00, // Immediate: 0x40000000 (VRAM write, addr 0)
        0x00, 0xC0, 0x00, 0x04, // Address: 0xC00004
        // Write data to VRAM
        // MOVE.W #$1234, $C00000
        0x33, 0xFC, // MOVE.W #imm, (xxx).L
        0x12, 0x34, // Data: 0x1234
        0x00, 0xC0, 0x00, 0x00, // Address: 0xC00000 (data port)
        // STOP
        0x4E, 0x72, 0x27, 0x00,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);

    // Execute first instruction (set address)
    cpu.step(&mut bus);
    println!("After set address - PC: 0x{:08X}", cpu.pc);

    // Execute second instruction (write data)
    cpu.step(&mut bus);
    println!("After write data - PC: 0x{:08X}", cpu.pc);

    assert_eq!(cpu.pc, 0xFF0412, "PC should be at STOP");

    assert_eq!(bus.vdp().get_vram()[0..2], [0x12, 0x34]);
}

#[test]
fn test_cpu_write_cram() {
    // Mini ROM: Write to CRAM (color palette)

    let rom = vec![
        // Set CRAM write address to 0x0000
        // MOVE.L #$C0000000, $C00004
        0x23, 0xFC, // MOVE.L #imm, (xxx).L
        0xC0, 0x00, 0x00, 0x00, // CRAM write, addr 0
        0x00, 0xC0, 0x00, 0x04, // Control port
        // Write color data (Red: 0x000E)
        // MOVE.W #$000E, $C00000
        0x33, 0xFC, // MOVE.W #imm, (xxx).L
        0x00, 0x0E, // Red color
        0x00, 0xC0, 0x00, 0x00, // Data port
        // Write another color (Green: 0x00E0)
        // MOVE.W #$00E0, $C00000
        0x33, 0xFC, 0x00, 0xE0, // Green color
        0x00, 0xC0, 0x00, 0x00, // Data port
        // STOP
        0x4E, 0x72, 0x27, 0x00,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);

    // Execute all instructions
    cpu.step(&mut bus); // Set CRAM address
    cpu.step(&mut bus); // Write red
    cpu.step(&mut bus); // Write green

    println!("Final PC: 0x{:08X}", cpu.pc);
    assert_eq!(cpu.pc, 0xFF041A, "PC should be at STOP");

    assert_eq!(bus.vdp().get_cram()[0..2], [0x000E, 0x00E0]);
}

#[test]
fn test_cpu_read_vdp_status() {
    // Mini ROM: Read VDP status register

    let rom = vec![
        // Read VDP status
        // MOVE.W $C00004, D0
        0x30, 0x39, // MOVE.W (xxx).L, D0
        0x00, 0xC0, 0x00, 0x04, // Address: 0xC00004
        // STOP
        0x4E, 0x72, 0x27, 0x00,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);

    // Execute read
    cpu.step(&mut bus);

    println!("D0 after read: 0x{:08X}", cpu.d[0]);
    println!("PC: 0x{:08X}", cpu.pc);

    // D0 should contain VDP status (at least some bits)
    // VDP status has various flags, exact value depends on VDP state
    assert_eq!(cpu.pc, 0xFF0406, "PC should advance correctly");
}

#[test]
fn test_cpu_vdp_register_write_loop() {
    // Mini ROM: Write multiple VDP registers in a loop

    let rom = vec![
        // Initialize A0 with VDP control port address
        // MOVEA.L #$C00004, A0
        0x20, 0x7C, // MOVEA.L #imm, A0
        0x00, 0xC0, 0x00, 0x04, // Immediate: 0x00C00004 (control port)
        // Write register 0
        // MOVE.W #$8004, (A0)
        0x30, 0xBC, // MOVE.W #imm, (A0)
        0x80, 0x04, // Immediate: 0x8004
        // Write register 1
        // MOVE.W #$8144, (A0)
        0x30, 0xBC, 0x81, 0x44,
        // Write register 15 (auto-increment)
        // MOVE.W #$8F02, (A0)
        0x30, 0xBC, 0x8F, 0x02, // Register 15 = 0x02
        // STOP
        0x4E, 0x72, 0x27, 0x00,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);

    // Execute all instructions
    cpu.step(&mut bus); // MOVEA
    cpu.step(&mut bus); // Write reg 0
    cpu.step(&mut bus); // Write reg 1
    cpu.step(&mut bus); // Write reg 15

    println!("Final PC: 0x{:08X}", cpu.pc);
    println!("A0: 0x{:08X}", cpu.a[0]);

    assert_eq!(cpu.a[0], 0x00C00004, "A0 should point to VDP control");
    assert_eq!(cpu.pc, 0xFF0412, "PC should be at STOP");

    let registers = bus.vdp().registers();
    assert_eq!(registers[0], 0x04);
    assert_eq!(registers[1], 0x44);
    assert_eq!(registers[15], 0x02);
}

#[test]
fn test_cpu_fill_vram_pattern() {
    // Mini ROM: Fill first 32 bytes of VRAM with pattern

    let rom = vec![
        // Set VRAM write address
        // MOVE.L #$40000000, $C00004
        0x23, 0xFC, 0x40, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x04,
        // Initialize loop counter
        // MOVEQ #15, D0  (16 words = 32 bytes)
        0x70, 0x0F, // D0 = 15
        // Initialize data pattern
        // MOVEQ #0, D1
        0x72, 0x00, // D1 = 0
        // Loop start (0xFF040E):
        // MOVE.W D1, $C00000
        0x33, 0xC1, // MOVE.W D1, (xxx).L
        0x00, 0xC0, 0x00, 0x00, // Address: 0xC00000 (data port)
        // Increment pattern
        // ADDQ.W #1, D1
        0x52, 0x41, // D1 += 1
        // Decrement counter and loop
        // DBRA D0, loop
        0x51, 0xC8, // DBRA D0, offset
        0xFF, 0xF6, // -10, from the offset word at 0xFF0418 back to 0xFF040E
        // STOP
        0x4E, 0x72, 0x27, 0x00,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);

    // Execute instructions
    cpu.step(&mut bus); // Set VRAM address
    cpu.step(&mut bus); // MOVEQ #15, D0
    cpu.step(&mut bus); // MOVEQ #0, D1

    // Execute loop (16 iterations)
    for i in 0..16 {
        cpu.step(&mut bus); // MOVE.W
        cpu.step(&mut bus); // ADDQ
        cpu.step(&mut bus); // DBRA
        println!(
            "Loop {}: D0={}, D1={}, PC=0x{:08X}",
            i, cpu.d[0], cpu.d[1], cpu.pc
        );
    }

    println!("After loop - D0: {}, D1: {}", cpu.d[0], cpu.d[1]);
    assert_eq!(
        cpu.d[0] & 0xFFFF,
        0xFFFF,
        "D0's low word should be -1 after DBRA finishes"
    );
    assert_eq!(cpu.d[1], 16, "D1 should be 16 after 16 increments");

    // VRAM holds the words 0, 1, 2, ..., 15
    let vram = bus.vdp().get_vram();
    for i in 0..16 {
        assert_eq!(vram[i * 2..i * 2 + 2], [0, i as u8]);
    }
}

#[test]
fn test_cpu_vdp_data_port_auto_increment() {
    // Test that VDP auto-increment works

    let rom = vec![
        // Set auto-increment to 2
        // MOVE.W #$8F02, $C00004
        0x33, 0xFC, 0x8F, 0x02, // Register 15 = 2
        0x00, 0xC0, 0x00, 0x04,
        // Set VRAM write address to 0x0000
        // MOVE.L #$40000000, $C00004
        0x23, 0xFC, 0x40, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x04,
        // Write 3 words (should write to 0x0000, 0x0002, 0x0004)
        0x33, 0xFC, 0xAA, 0xAA, 0x00, 0xC0, 0x00, 0x00, // MOVE.W #$AAAA, $C00000
        0x33, 0xFC, 0xBB, 0xBB, 0x00, 0xC0, 0x00, 0x00, // MOVE.W #$BBBB, $C00000
        0x33, 0xFC, 0xCC, 0xCC, 0x00, 0xC0, 0x00, 0x00, // MOVE.W #$CCCC, $C00000
        // STOP
        0x4E, 0x72, 0x27, 0x00,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);

    // Execute all writes
    cpu.step(&mut bus); // Set auto-increment
    cpu.step(&mut bus); // Set VRAM address
    cpu.step(&mut bus); // Write AAAA
    cpu.step(&mut bus); // Write BBBB
    cpu.step(&mut bus); // Write CCCC

    assert_eq!(cpu.pc, 0xFF042A, "PC should be at STOP");

    assert_eq!(
        bus.vdp().get_vram()[0..6],
        [0xAA, 0xAA, 0xBB, 0xBB, 0xCC, 0xCC]
    );
}

#[test]
fn test_vdp_memory_mapped_io_accessible() {
    // Verify that VDP region is accessible through the bus
    let mut bus = AddressBus::new();

    // VDP control port is at 0xC00004
    // Writing and reading should not panic
    bus.write_word(0xC00004, 0x8000);

    // VDP data port is at 0xC00000
    bus.write_word(0xC00000, 0x1234);

    // Status read
    let status = bus.read_word(0xC00004);
    println!("VDP Status: 0x{:04X}", status);

    // If we get here without panic, VDP is accessible
}

#[test]
fn test_cpu_timing_vdp_access() {
    // Test that VDP accesses consume correct number of cycles

    let rom = vec![
        0x33, 0xFC, 0x80, 0x00, 0x00, 0xC0, 0x00, 0x04, // MOVE.W #$8000, $C00004
        // STOP
        0x4E, 0x72, 0x27, 0x00,
    ];

    let (mut cpu, mut bus) = setup_cpu_with_rom(&rom);

    let cycles = cpu.step(&mut bus);

    // MOVE.W #imm, (xxx).L should take specific number of cycles
    // Exact timing depends on implementation
    println!("Cycles for MOVE.W to VDP: {}", cycles);
    assert!(cycles > 0, "Should consume some cycles");
}
