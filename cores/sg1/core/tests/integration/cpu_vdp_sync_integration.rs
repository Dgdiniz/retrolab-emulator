//! Integration tests for CPU-VDP synchronization.

use retro_core::{Breakpoint, BreakpointType, DebuggerCore, EmulatorCore, ExecutionMode};
use sg1_core::GenesisEmulator;

/// Where the VINT handler counts the interrupts it takes (a word in work RAM).
const VINT_COUNT_ADDR: u32 = 0x00FF_0000;

/// Where the VINT handler stores the HV counter it reads on entry (a word in
/// work RAM): the high byte is the V counter, the line the interrupt came on.
const VINT_HV_ADDR: u32 = 0x00FF_0002;

/// Write big-endian 16-bit words into `rom`, starting at `addr`.
fn put_words(rom: &mut [u8], addr: usize, words: &[u16]) {
    for (i, &word) in words.iter().enumerate() {
        rom[addr + 2 * i] = (word >> 8) as u8;
        rom[addr + 2 * i + 1] = (word & 0xFF) as u8;
    }
}

/// Read a big-endian 16-bit word from work RAM.
fn read_ram_word(emu: &mut GenesisEmulator, addr: u32) -> u16 {
    let bytes = emu.read_memory_debug(addr, 2);
    u16::from_be_bytes([bytes[0], bytes[1]])
}

/// Helper to create a mini ROM that turns VINT on and loops on a NOP, with a
/// VINT handler at 0x300 that records each interrupt in work RAM.
fn create_vblank_wait_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x10000];

    // ROM Header (minimal, at 0x100)
    let header_offset = 0x100;
    rom[header_offset..header_offset + 16].copy_from_slice(b"SEGA GENESIS    ");

    // Entry point (at 0x200)
    let entry = 0x200;
    let vint_handler = 0x300;

    // Initial SSP at 0x000: 0x00FFFE00, in work RAM, where the interrupt
    // pushes its return address and SR
    put_words(&mut rom, 0x00, &[0x00FF, 0xFE00]);

    // Initial PC at 0x004
    put_words(&mut rom, 0x04, &[0x0000, entry as u16]);

    // VINT vector at 0x78 (vector 30, the level-6 autovector)
    put_words(&mut rom, 0x78, &[0x0000, vint_handler as u16]);

    // Main program: VDP register 1 = 0x64 (display on, VINT on, Mode 5, 224
    // lines), the interrupt mask lowered to 0, then a loop that only waits
    put_words(
        &mut rom,
        entry,
        &[
            0x33FC, 0x8164, 0x00C0, 0x0004, // MOVE.W #$8164, ($C00004).L
            0x46FC, 0x2000, // MOVE #$2000, SR
            0x4E71, // @loop: NOP
            0x60FC, // BRA.S @loop (-4)
        ],
    );

    // VINT handler: store the HV counter, count the interrupt, return
    put_words(
        &mut rom,
        vint_handler,
        &[
            0x33F9, 0x00C0, 0x0008, 0x00FF, 0x0002, // MOVE.W ($C00008).L, ($FF0002).L
            0x5279, 0x00FF, 0x0000, // ADDQ.W #1, ($FF0000).L
            0x4E73, // RTE
        ],
    );

    rom
}

#[test]
fn test_vint_triggers_at_scanline_224() {
    let mut emu = GenesisEmulator::new();
    let rom = create_vblank_wait_rom();

    emu.load_rom(&rom).expect("Failed to load ROM");
    emu.debugger().set_execution_mode(ExecutionMode::Running);

    // The VDP comes out of reset past its VBLANK, so the first frame raises
    // no VINT; from the second frame on, each frame's VINT runs the handler
    // once
    emu.run_frame();
    let count_after_first = read_ram_word(&mut emu, VINT_COUNT_ADDR);
    for _ in 0..4 {
        emu.run_frame();
    }
    let count = read_ram_word(&mut emu, VINT_COUNT_ADDR);
    assert_eq!(
        count.wrapping_sub(count_after_first),
        4,
        "the VINT handler should run once per frame (count {} after the first frame, {} after five)",
        count_after_first,
        count
    );

    // The handler read the HV counter as it began: the V counter in its high
    // byte is the line the interrupt was taken on, 224 (0xE0)
    let hv = read_ram_word(&mut emu, VINT_HV_ADDR);
    assert_eq!(
        hv >> 8,
        0xE0,
        "VINT should be taken on line 224 (HV counter 0x{:04X})",
        hv
    );

    // Every RTE took its return address from the stack in work RAM, back to
    // the wait loop (the NOP at 0x20C and the BRA at 0x20E)
    let pc = emu.cpu().pc;
    assert!(
        matches!(pc, 0x20C | 0x20E),
        "the program should still be in its wait loop, PC 0x{:06X}",
        pc
    );
}

#[test]
fn test_breakpoint_pauses_during_run_frame() {
    let mut emu = GenesisEmulator::new();
    let rom = create_vblank_wait_rom();

    emu.load_rom(&rom).expect("Failed to load ROM");

    // Add breakpoint at the entry point (0x200)
    let bp = Breakpoint {
        address: 0x200,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    };
    emu.debugger().add_breakpoint(bp);

    // Set to running mode
    emu.debugger().set_execution_mode(ExecutionMode::Running);

    // Run one frame - should hit breakpoint and pause
    emu.run_frame();

    // Check that debugger is paused
    let exec_mode = emu.debugger().get_execution_mode();
    assert_eq!(format!("{:?}", exec_mode), "Paused");
}

#[test]
fn test_step_mode_works() {
    let mut emu = GenesisEmulator::new();
    let rom = create_vblank_wait_rom();

    emu.load_rom(&rom).expect("Failed to load ROM");

    // Set to step mode
    emu.debugger().set_execution_mode(ExecutionMode::StepInto);

    // Run frame (should execute one instruction and pause)
    emu.run_frame();

    // Should be paused after stepping
    let exec_mode = emu.debugger().get_execution_mode();
    assert_eq!(format!("{:?}", exec_mode), "Paused");
}

#[test]
fn test_multiple_frames_execute() {
    let mut emu = GenesisEmulator::new();
    let rom = create_vblank_wait_rom();

    emu.load_rom(&rom).expect("Failed to load ROM");

    // Set to running mode
    emu.debugger().set_execution_mode(ExecutionMode::Running);

    // Run multiple frames
    for _ in 0..10 {
        emu.run_frame();
    }

    // System should still be functional
    assert!(emu.system_info().name == "Sega Genesis");
}

#[test]
fn test_execution_mode_persists() {
    let mut emu = GenesisEmulator::new();
    let rom = create_vblank_wait_rom();

    emu.load_rom(&rom).expect("Failed to load ROM");

    // Set to paused
    emu.debugger().set_execution_mode(ExecutionMode::Paused);

    // Verify paused mode is set
    let mode_before = emu.debugger().get_execution_mode();
    assert_eq!(format!("{:?}", mode_before), "Paused");

    // Run frame (should do nothing because paused)
    emu.run_frame();

    // Should still be paused
    let mode_after = emu.debugger().get_execution_mode();
    assert_eq!(format!("{:?}", mode_after), "Paused");
}

#[test]
fn test_scanline_based_timing() {
    let mut emu = GenesisEmulator::new();
    let rom = create_vblank_wait_rom();

    emu.load_rom(&rom).expect("Failed to load ROM");

    // Set to running mode
    emu.debugger().set_execution_mode(ExecutionMode::Running);

    // Run one frame
    emu.run_frame();

    // Frame should have completed
    // (Specific timing validation would require more instrumentation)
    assert!(emu.system_info().name == "Sega Genesis");
}

#[test]
fn test_breakpoint_removes_correctly() {
    let mut emu = GenesisEmulator::new();
    let rom = create_vblank_wait_rom();

    emu.load_rom(&rom).expect("Failed to load ROM");

    // Add breakpoint
    let bp = Breakpoint {
        address: 0x200,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    };
    let bp_id = emu.debugger().add_breakpoint(bp);

    // Verify breakpoint exists
    let breakpoints = emu.debugger().list_breakpoints();
    assert_eq!(breakpoints.len(), 1);

    // Remove breakpoint
    emu.debugger().remove_breakpoint(bp_id);

    // Verify removed
    let breakpoints_after = emu.debugger().list_breakpoints();
    assert_eq!(breakpoints_after.len(), 0);
}
