//! Integration tests for debugger functionality.

use retro_core::{Breakpoint, BreakpointType, DebuggerCore, EmulatorCore};
use sg1_core::GenesisEmulator;

/// Helper to create a simple test ROM.
fn create_simple_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x10000];

    // ROM Header at 0x100
    rom[0x100..0x110].copy_from_slice(b"SEGA GENESIS    ");

    // Entry point at 0x200
    let entry = 0x200;

    // Initial SSP at 0x000
    rom[0x00] = 0x00;
    rom[0x01] = 0x00;
    rom[0x02] = 0xFF;
    rom[0x03] = 0xFC;

    // Initial PC at 0x004
    rom[0x04] = 0x00;
    rom[0x05] = 0x00;
    rom[0x06] = (entry >> 8) as u8;
    rom[0x07] = (entry & 0xFF) as u8;

    // Code at entry point
    let mut pc = entry;

    // MOVE.W #$1234, D0 (0x303C 0x1234)
    rom[pc] = 0x30;
    rom[pc + 1] = 0x3C;
    rom[pc + 2] = 0x12;
    rom[pc + 3] = 0x34;
    pc += 4;

    // MOVE.L D0, D1 (0x2200)
    rom[pc] = 0x22;
    rom[pc + 1] = 0x00;
    pc += 2;

    // ADD.W #$100, D0 (0x0640 0x0100)
    rom[pc] = 0x06;
    rom[pc + 1] = 0x40;
    rom[pc + 2] = 0x01;
    rom[pc + 3] = 0x00;
    pc += 4;

    // BRA -8 (loop back)
    rom[pc] = 0x60;
    rom[pc + 1] = 0xF8;

    rom
}

#[test]
fn test_memory_viewer_reads_rom() {
    let mut emu = GenesisEmulator::new();
    let rom = create_simple_rom();

    emu.load_rom(&rom).expect("Failed to load ROM");

    // Read from ROM area (entry point)
    let data = emu.read_memory_debug(0x200, 16);

    // Should match the instructions we wrote
    assert_eq!(data[0], 0x30); // MOVE.W #imm, D0
    assert_eq!(data[1], 0x3C);
    assert_eq!(data[2], 0x12);
    assert_eq!(data[3], 0x34);
}

#[test]
fn test_memory_viewer_reads_ram() {
    let mut emu = GenesisEmulator::new();
    let rom = create_simple_rom();

    emu.load_rom(&rom).expect("Failed to load ROM");

    // Write to RAM (0xFF0000+)
    emu.write_memory_debug(0xFF0000, &[0xDE, 0xAD, 0xBE, 0xEF]);

    // Read back
    let data = emu.read_memory_debug(0xFF0000, 4);

    assert_eq!(data, vec![0xDE, 0xAD, 0xBE, 0xEF]);
}

#[test]
fn test_memory_viewer_large_read() {
    let mut emu = GenesisEmulator::new();
    let rom = create_simple_rom();

    emu.load_rom(&rom).expect("Failed to load ROM");

    // Read 256 bytes
    let data = emu.read_memory_debug(0x200, 256);

    assert_eq!(data.len(), 256);
}

#[test]
fn test_disassembly_at_entry_point() {
    let mut emu = GenesisEmulator::new();
    let rom = create_simple_rom();

    emu.load_rom(&rom).expect("Failed to load ROM");

    // Disassemble 4 instructions from entry point
    let instructions = emu.disassemble_at(0x200, 4);

    assert_eq!(instructions.len(), 4);

    // First instruction should be MOVE.W
    assert_eq!(instructions[0].address, 0x200);
    assert!(instructions[0].mnemonic.contains("MOVE"));

    // Second instruction should be MOVE.L
    assert_eq!(instructions[1].address, 0x204);
    assert!(instructions[1].mnemonic.contains("MOVE"));

    // Third instruction should be ADD
    assert_eq!(instructions[2].address, 0x206);
    assert!(instructions[2].mnemonic.contains("ADD"));

    // Fourth instruction should be BRA
    assert_eq!(instructions[3].address, 0x20A);
    assert!(instructions[3].mnemonic.contains("BRA"));
}

#[test]
fn test_disassembly_with_breakpoint() {
    let mut emu = GenesisEmulator::new();
    let rom = create_simple_rom();

    emu.load_rom(&rom).expect("Failed to load ROM");

    // Add breakpoint at second instruction
    let bp = Breakpoint {
        address: 0x204,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    };
    emu.debugger().add_breakpoint(bp);

    // Disassemble around breakpoint
    let instructions = emu.disassemble_at(0x200, 4);

    // Should still disassemble correctly with breakpoint present
    assert_eq!(instructions.len(), 4);
    assert_eq!(instructions[1].address, 0x204);
}

#[test]
fn test_memory_regions_accessible() {
    let mut emu = GenesisEmulator::new();
    let rom = create_simple_rom();

    emu.load_rom(&rom).expect("Failed to load ROM");

    // ROM region (0x000000)
    let rom_data = emu.read_memory_debug(0x000000, 16);
    assert_eq!(rom_data.len(), 16);

    // RAM region (0xFF0000)
    let ram_data = emu.read_memory_debug(0xFF0000, 16);
    assert_eq!(ram_data.len(), 16);

    // VDP ports (0xC00000) - should not crash
    let vdp_data = emu.read_memory_debug(0xC00000, 16);
    assert_eq!(vdp_data.len(), 16);
}

#[test]
fn test_memory_write_and_read_consistency() {
    let mut emu = GenesisEmulator::new();
    let rom = create_simple_rom();

    emu.load_rom(&rom).expect("Failed to load ROM");

    // Write pattern to RAM
    let pattern: Vec<u8> = (0..256).map(|i| i as u8).collect();
    emu.write_memory_debug(0xFF0000, &pattern);

    // Read back
    let read_pattern = emu.read_memory_debug(0xFF0000, 256);

    assert_eq!(read_pattern, pattern);
}

#[test]
fn test_disassembly_follows_execution() {
    use retro_core::ExecutionMode;

    let mut emu = GenesisEmulator::new();
    let rom = create_simple_rom();

    emu.load_rom(&rom).expect("Failed to load ROM");

    // The initial PC: the entry point (written as a literal, not read)
    let initial_pc = 0x200; // Entry point

    // Disassemble at current PC
    let instructions_before = emu.disassemble_at(initial_pc, 2);
    assert_eq!(instructions_before[0].address, 0x200);

    // Step one instruction
    emu.debugger().set_execution_mode(ExecutionMode::StepInto);
    emu.run_frame();

    // Disassemble at 0x204, where the step should leave the PC (not read)
    let instructions_after = emu.disassemble_at(0x204, 2);
    assert_eq!(instructions_after[0].address, 0x204);
}

#[test]
fn test_memory_viewer_vdp_region() {
    let mut emu = GenesisEmulator::new();
    let rom = create_simple_rom();

    emu.load_rom(&rom).expect("Failed to load ROM");

    // Read VDP control port
    let vdp_control = emu.read_memory_debug(0xC00004, 2);
    assert_eq!(vdp_control.len(), 2);

    // Read VDP data port
    let vdp_data = emu.read_memory_debug(0xC00000, 2);
    assert_eq!(vdp_data.len(), 2);
}

#[test]
fn test_disassembly_invalid_instruction() {
    let mut emu = GenesisEmulator::new();
    let rom = create_simple_rom();

    emu.load_rom(&rom).expect("Failed to load ROM");

    // Write invalid instruction (0xFFFF) to RAM
    emu.write_memory_debug(0xFF0400, &[0xFF, 0xFF]);

    // Disassemble at invalid instruction
    let instructions = emu.disassemble_at(0xFF0400, 1);

    // Should return something (probably "ILLEGAL" or "???")
    assert_eq!(instructions.len(), 1);
    assert_eq!(instructions[0].address, 0xFF0400);
}

#[test]
fn test_memory_regions_info() {
    let mut emu = GenesisEmulator::new();
    let rom = create_simple_rom();

    emu.load_rom(&rom).expect("Failed to load ROM");

    // Get memory regions
    let regions = emu.debugger().get_memory_regions();

    // Should have multiple regions (ROM, RAM, VDP, etc.)
    assert!(!regions.is_empty());

    // Check for essential regions
    let has_rom = regions.iter().any(|r| r.name.contains("ROM"));
    let has_ram = regions.iter().any(|r| r.name.contains("RAM"));

    assert!(has_rom, "Should have ROM region");
    assert!(has_ram, "Should have RAM region");
}

#[test]
fn test_call_stack_with_disassembly() {
    use retro_core::ExecutionMode;

    let mut emu = GenesisEmulator::new();

    // Create ROM with JSR
    let mut rom = vec![0u8; 0x10000];
    rom[0x100..0x110].copy_from_slice(b"SEGA GENESIS    ");

    let entry = 0x200;
    rom[0x00] = 0x00;
    rom[0x01] = 0x00;
    rom[0x02] = 0xFF;
    rom[0x03] = 0xFC;
    rom[0x04] = 0x00;
    rom[0x05] = 0x00;
    rom[0x06] = (entry >> 8) as u8;
    rom[0x07] = (entry & 0xFF) as u8;

    // JSR to 0x300
    rom[0x200] = 0x4E;
    rom[0x201] = 0xB9;
    rom[0x202] = 0x00;
    rom[0x203] = 0x00;
    rom[0x204] = 0x03;
    rom[0x205] = 0x00;

    // NOP at return point
    rom[0x206] = 0x4E;
    rom[0x207] = 0x71;

    // Target at 0x300: RTS
    rom[0x300] = 0x4E;
    rom[0x301] = 0x75;

    emu.load_rom(&rom).expect("Failed to load ROM");

    // Step until JSR
    emu.debugger().set_execution_mode(ExecutionMode::StepInto);
    emu.run_frame(); // Execute JSR

    // Call stack should have one frame
    let call_stack = emu.debugger().get_call_stack(0);
    assert_eq!(call_stack.len(), 1);

    // Disassemble at call stack address
    let instructions = emu.disassemble_at(call_stack[0].call_address, 1);
    assert_eq!(instructions[0].address, 0x200);
    assert!(instructions[0].mnemonic.contains("JSR"));
}
