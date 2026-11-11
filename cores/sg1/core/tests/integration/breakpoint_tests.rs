//! Breakpoint tests - Testing debugger breakpoint functionality
//!
//! These tests verify that breakpoints work correctly during execution.

#![allow(dead_code, unused_imports)]

use retro_core::cpu::Addressable;
use retro_core::debugger::DebuggerCore;
use retro_core::emulator::EmulatorCore; // Import trait for methods
use retro_core::types::{Breakpoint, BreakpointType, ExecutionMode};
use sg1_core::system::GenesisEmulator;

/// Helper to create a test ROM
fn create_test_rom(instructions: &[u16]) -> Vec<u8> {
    let mut rom = vec![0u8; 0x1000];

    // Reset vectors
    rom[0x00] = 0x00;
    rom[0x01] = 0x00;
    rom[0x02] = 0xFF;
    rom[0x03] = 0x00; // SSP
    rom[0x04] = 0x00;
    rom[0x05] = 0x00;
    rom[0x06] = 0x04;
    rom[0x07] = 0x00; // PC = 0x400

    // The console name the cartridge loader checks for.
    rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");

    // Write instructions
    for (i, &instr) in instructions.iter().enumerate() {
        let addr = 0x400 + (i * 2);
        rom[addr] = (instr >> 8) as u8;
        rom[addr + 1] = (instr & 0xFF) as u8;
    }

    rom
}

#[test]
fn test_breakpoint_add_remove() {
    let mut emu = GenesisEmulator::new();
    let debugger = emu.debugger();

    // Initially no breakpoints
    assert_eq!(debugger.list_breakpoints().len(), 0);

    // Add a breakpoint
    let bp = Breakpoint {
        address: 0x400,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    };
    let bp_id = debugger.add_breakpoint(bp);

    // Should have one breakpoint
    let breakpoints = debugger.list_breakpoints();
    assert_eq!(breakpoints.len(), 1);
    assert_eq!(breakpoints[0].id, bp_id);
    assert_eq!(breakpoints[0].breakpoint.address, 0x400);
    assert!(breakpoints[0].enabled);

    // Remove breakpoint
    debugger.remove_breakpoint(bp_id);
    assert_eq!(debugger.list_breakpoints().len(), 0);
}

#[test]
fn test_breakpoint_enable_disable() {
    let mut emu = GenesisEmulator::new();
    let debugger = emu.debugger();

    let bp = Breakpoint {
        address: 0x400,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    };
    let bp_id = debugger.add_breakpoint(bp);

    // Should be enabled by default
    let breakpoints = debugger.list_breakpoints();
    assert!(breakpoints[0].enabled);

    // Disable
    debugger.enable_breakpoint(bp_id, false);
    let breakpoints = debugger.list_breakpoints();
    assert!(!breakpoints[0].enabled);

    // Re-enable
    debugger.enable_breakpoint(bp_id, true);
    let breakpoints = debugger.list_breakpoints();
    assert!(breakpoints[0].enabled);
}

#[test]
fn test_breakpoint_execution_stops() {
    let mut emu = GenesisEmulator::new();

    // Program: 3 MOVEQ instructions
    let rom = create_test_rom(&[
        0x7001, // MOVEQ #1, D0 at 0x400
        0x7202, // MOVEQ #2, D1 at 0x402
        0x7403, // MOVEQ #3, D2 at 0x404
    ]);

    emu.load_rom(&rom).unwrap();
    emu.reset();

    // Add breakpoint at second instruction (0x402)
    let bp = Breakpoint {
        address: 0x402,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    };
    emu.debugger().add_breakpoint(bp);

    // Step first instruction
    emu.step();

    // CPU state after first instruction
    let cpu_states = emu.debugger().get_cpu_states();
    let d0 = cpu_states[0]
        .registers
        .iter()
        .find(|r| r.name == "D0")
        .unwrap();
    assert_eq!(d0.value, 1, "D0 should be 1 after first instruction");

    // PC should be at breakpoint location
    let pc = cpu_states[0]
        .registers
        .iter()
        .find(|r| r.name == "PC")
        .unwrap();
    assert_eq!(pc.value, 0x402, "PC should be at breakpoint");

    // Check breakpoint hit count
    let breakpoints = emu.debugger().list_breakpoints();
    assert_eq!(
        breakpoints[0].hit_count, 1,
        "Breakpoint should have been hit once"
    );
}

#[test]
fn test_breakpoint_multiple_hits() {
    let mut emu = GenesisEmulator::new();

    // Program: Loop that repeats
    // loop:
    //   MOVEQ #0, D0   ; 0x400
    //   DBRA D1, loop  ; 0x402 (loops back to 0x400)

    let rom = create_test_rom(&[
        0x7000, // MOVEQ #0, D0
        0x51C9, 0xFFFC, // DBRA D1, -4 (back to 0x400)
    ]);

    emu.load_rom(&rom).unwrap();
    emu.reset();

    // DBRA decrements D1.W and branches back while it is not -1, so with
    // D1 = 2 the loop body runs three times (D1 = 2, 1, 0)
    emu.cpu_mut().d[1] = 2;

    // Add breakpoint at loop start
    let bp = Breakpoint {
        address: 0x400,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    };
    emu.debugger().add_breakpoint(bp);

    // Three passes of MOVEQ + DBRA: six steps, the last DBRA falling through
    for _ in 0..6 {
        emu.step();
    }

    assert_eq!(emu.cpu().pc, 0x406, "the loop should have run out");
    assert_eq!(emu.cpu().d[1] & 0xFFFF, 0xFFFF, "DBRA leaves D1.W at -1");

    // Each pass reaches 0x400 once: three hits
    let breakpoints = emu.debugger().list_breakpoints();
    assert_eq!(
        breakpoints[0].hit_count, 3,
        "Breakpoint should be hit once per pass of the loop"
    );
}

#[test]
fn test_breakpoint_disabled_not_triggered() {
    let mut emu = GenesisEmulator::new();

    let rom = create_test_rom(&[
        0x7001, // MOVEQ #1, D0
        0x7202, // MOVEQ #2, D1
    ]);

    emu.load_rom(&rom).unwrap();
    emu.reset();

    // Add but disable breakpoint
    let bp = Breakpoint {
        address: 0x402,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    };
    let bp_id = emu.debugger().add_breakpoint(bp);
    emu.debugger().enable_breakpoint(bp_id, false);

    // Step through both instructions
    emu.step();
    emu.step();

    // Disabled breakpoint should not have been hit
    let breakpoints = emu.debugger().list_breakpoints();
    assert_eq!(
        breakpoints[0].hit_count, 0,
        "Disabled breakpoint should not be hit"
    );
}

#[test]
fn test_execution_mode_changes() {
    let mut emu = GenesisEmulator::new();

    // Check initial mode
    assert_eq!(
        emu.debugger().get_execution_mode(),
        ExecutionMode::Paused,
        "Should start paused"
    );

    // Change to running
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::Running);

    // Change to step into
    emu.debugger().set_execution_mode(ExecutionMode::StepInto);
    assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::StepInto);
}

#[test]
fn test_memory_breakpoint_read() {
    let mut emu = GenesisEmulator::new();

    // Program that reads from memory
    let mut rom = vec![0u8; 0x1000];
    rom[0x00] = 0x00;
    rom[0x01] = 0x00;
    rom[0x02] = 0xFF;
    rom[0x03] = 0x00; // SSP
    rom[0x04] = 0x00;
    rom[0x05] = 0x00;
    rom[0x06] = 0x04;
    rom[0x07] = 0x00; // PC
    rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");

    rom[0x400] = 0x20;
    rom[0x401] = 0x7C; // MOVE.L #imm, A0
    rom[0x402] = 0x00;
    rom[0x403] = 0xFF;
    rom[0x404] = 0x10;
    rom[0x405] = 0x00; // = 0x00FF1000
    rom[0x406] = 0x32;
    rom[0x407] = 0x10; // MOVE.W (A0), D1 - reads from 0xFF1000
    rom[0x408] = 0x4E;
    rom[0x409] = 0x71; // NOP - no data access

    emu.load_rom(&rom).unwrap();
    emu.reset();

    // The word the program reads
    emu.write_memory_debug(0xFF1000, &[0x12, 0x34]);

    // Add read breakpoint at 0xFF1000
    let bp = Breakpoint {
        address: 0xFF1000,
        bp_type: BreakpointType::read(),
        cpu_id: Some(0),
        condition: None,
    };
    emu.debugger().add_breakpoint(bp);

    // Execute MOVE.L #imm, A0
    let result = emu.step();
    assert!(!result.breakpoint_hit, "MOVE.L #imm, A0 reads only ROM");

    // Breakpoint should not be hit yet
    let breakpoints = emu.debugger().list_breakpoints();
    assert_eq!(breakpoints[0].hit_count, 0, "Read breakpoint not hit yet");

    // Execute MOVE.W (A0), D1 - this reads from memory
    let result = emu.step();

    // The read of 0xFF1000 fires the watchpoint: the step reports the hit
    // and the breakpoint counts it
    assert!(
        result.breakpoint_hit,
        "the read of 0xFF1000 should fire the watchpoint"
    );
    let breakpoints = emu.debugger().list_breakpoints();
    assert_eq!(breakpoints[0].hit_count, 1, "Read breakpoint hit once");
    assert_eq!(emu.cpu().d[1] & 0xFFFF, 0x1234, "D1 holds the word read");

    // The NOP after it does not read 0xFF1000: no new hit
    let result = emu.step();
    assert!(
        !result.breakpoint_hit,
        "the NOP should not fire the watchpoint"
    );
    let breakpoints = emu.debugger().list_breakpoints();
    assert_eq!(
        breakpoints[0].hit_count, 1,
        "Read breakpoint still hit once"
    );
}

#[test]
fn test_disassembly_with_breakpoints() {
    let mut emu = GenesisEmulator::new();

    let rom = create_test_rom(&[
        0x7001, // MOVEQ #1, D0
        0x7202, // MOVEQ #2, D1
        0x7403, // MOVEQ #3, D2
    ]);

    emu.load_rom(&rom).unwrap();
    emu.reset();

    // Add breakpoint at middle instruction
    let bp = Breakpoint {
        address: 0x402,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    };
    emu.debugger().add_breakpoint(bp);

    // Get disassembly (use emu.disassemble_at() which has bus access)
    let instructions = emu.disassemble_at(0x400, 3);

    // Should get 3 instructions
    assert_eq!(instructions.len(), 3);

    // First instruction should be at 0x400
    assert_eq!(instructions[0].address, 0x400);
    assert!(instructions[0].mnemonic.contains("MOVEQ"));

    // Second instruction should be at 0x402 (where breakpoint is)
    assert_eq!(instructions[1].address, 0x402);

    // Third instruction should be at 0x404
    assert_eq!(instructions[2].address, 0x404);
}

#[test]
fn test_step_over_breakpoints() {
    let mut emu = GenesisEmulator::new();

    let rom = create_test_rom(&[
        0x7001, // MOVEQ #1, D0 at 0x400
        0x7202, // MOVEQ #2, D1 at 0x402
        0x7403, // MOVEQ #3, D2 at 0x404
        0x7604, // MOVEQ #4, D3 at 0x406
    ]);

    emu.load_rom(&rom).unwrap();
    emu.reset();

    // Add breakpoints at 0x402 and 0x406
    let bp1 = Breakpoint {
        address: 0x402,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    };
    let bp2 = Breakpoint {
        address: 0x406,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    };
    emu.debugger().add_breakpoint(bp1);
    emu.debugger().add_breakpoint(bp2);

    // Step through all instructions
    for i in 0..4 {
        emu.step();

        let cpu_states = emu.debugger().get_cpu_states();
        let pc = cpu_states[0]
            .registers
            .iter()
            .find(|r| r.name == "PC")
            .unwrap();

        let expected_pc = 0x400 + ((i + 1) * 2);
        assert_eq!(
            pc.value, expected_pc,
            "PC should advance on iteration {}",
            i
        );
    }

    // Both breakpoints should have been hit once
    let breakpoints = emu.debugger().list_breakpoints();
    assert_eq!(
        breakpoints[0].hit_count, 1,
        "First breakpoint should be hit once"
    );
    assert_eq!(
        breakpoints[1].hit_count, 1,
        "Second breakpoint should be hit once"
    );
}

#[test]
fn test_multiple_breakpoint_types() {
    let mut emu = GenesisEmulator::new();

    let rom = create_test_rom(&[
        0x7001, // MOVEQ #1, D0
        0x7202, // MOVEQ #2, D1
    ]);

    emu.load_rom(&rom).unwrap();
    emu.reset();

    // Add different types of breakpoints
    let bp_exec = Breakpoint {
        address: 0x400,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    };
    let bp_read = Breakpoint {
        address: 0xFF1000,
        bp_type: BreakpointType::read(),
        cpu_id: Some(0),
        condition: None,
    };
    let bp_write = Breakpoint {
        address: 0xFF2000,
        bp_type: BreakpointType::write(),
        cpu_id: Some(0),
        condition: None,
    };

    emu.debugger().add_breakpoint(bp_exec);
    emu.debugger().add_breakpoint(bp_read);
    emu.debugger().add_breakpoint(bp_write);

    // Should have 3 breakpoints
    let breakpoints = emu.debugger().list_breakpoints();
    assert_eq!(breakpoints.len(), 3);

    // Verify types
    assert_eq!(breakpoints[0].breakpoint.bp_type, BreakpointType::execute());
    assert_eq!(breakpoints[1].breakpoint.bp_type, BreakpointType::read());
    assert_eq!(breakpoints[2].breakpoint.bp_type, BreakpointType::write());
}

#[test]
fn test_call_stack_with_breakpoints() {
    let mut emu = GenesisEmulator::new();

    // Program with subroutine call
    let mut rom = vec![0u8; 0x1000];
    rom[0x00] = 0x00;
    rom[0x01] = 0x00;
    rom[0x02] = 0xFF;
    rom[0x03] = 0x00;
    rom[0x04] = 0x00;
    rom[0x05] = 0x00;
    rom[0x06] = 0x04;
    rom[0x07] = 0x00;
    rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");

    rom[0x400] = 0x70;
    rom[0x401] = 0x05; // MOVEQ #5, D0
    rom[0x402] = 0x4E;
    rom[0x403] = 0xB9; // JSR
    rom[0x404] = 0x00;
    rom[0x405] = 0x00;
    rom[0x406] = 0x04;
    rom[0x407] = 0x10; // to 0x410
    rom[0x408] = 0x74;
    rom[0x409] = 0x0A; // MOVEQ #10, D2

    rom[0x410] = 0x72;
    rom[0x411] = 0x07; // MOVEQ #7, D1 (subroutine)
    rom[0x412] = 0x4E;
    rom[0x413] = 0x75; // RTS

    emu.load_rom(&rom).unwrap();
    emu.reset();

    // Add breakpoint inside subroutine
    let bp = Breakpoint {
        address: 0x410,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    };
    emu.debugger().add_breakpoint(bp);

    // Execute until JSR
    emu.step(); // MOVEQ #5, D0
    emu.step(); // JSR

    // Should be at subroutine (breakpoint)
    let cpu_states = emu.debugger().get_cpu_states();
    let pc = cpu_states[0]
        .registers
        .iter()
        .find(|r| r.name == "PC")
        .unwrap();
    assert_eq!(pc.value, 0x410);

    // Get call stack
    let call_stack = emu.debugger().get_call_stack(0);

    // Should have one entry (JSR from 0x402)
    assert!(
        !call_stack.is_empty(),
        "Call stack should have at least one entry"
    );
    if !call_stack.is_empty() {
        assert_eq!(
            call_stack[0].return_address, 0x408,
            "Return address should be after JSR"
        );
    }
}
