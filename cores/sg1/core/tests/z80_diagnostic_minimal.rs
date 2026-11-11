//! Minimal diagnostic tests: isolating a Z80 vs YM2612 problem
//!
//! The tests grow one step at a time, from a lone HALT to writes to the
//! YM2612's registers, then check the ROM builder's bytes and trace the run,
//! so the first one that fails shows where the problem is.

mod helpers;

use helpers::{Z80GenesisTestRunner, Z80RomBuilder};

// ============================================================================
// DIAGNOSTIC 1: HALT alone (the simplest case)
// ============================================================================

#[test]
fn diagnostic_001_halt_only() {
    let rom = Z80RomBuilder::new(0x0000).halt().build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.set_max_cycles(1000); // Short timeout
    runner.load_rom(&rom, 0x0000);

    let result = runner.run_until_halt();
    assert!(result.is_ok(), "HALT alone should work: {:?}", result);
    assert!(runner.is_halted());
}

// ============================================================================
// DIAGNOSTIC 2: LD + HALT (no I/O)
// ============================================================================

#[test]
fn diagnostic_002_ld_halt() {
    let rom = Z80RomBuilder::new(0x0000)
        .ld_a(0x42)
        .ld_b(0x12)
        .ld_c(0x34)
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.set_max_cycles(1000);
    runner.load_rom(&rom, 0x0000);

    let result = runner.run_until_halt();
    assert!(result.is_ok(), "LD + HALT should work: {:?}", result);
    assert_eq!(runner.get_a(), 0x42);
    assert_eq!(runner.get_b(), 0x12);
    assert_eq!(runner.get_c(), 0x34);
}

// ============================================================================
// DIAGNOSTIC 3: OUT without the YM2612 (generic I/O)
// ============================================================================

#[test]
fn diagnostic_003_out_generic() {
    let rom = Z80RomBuilder::new(0x0000)
        .ld_a(0xFF)
        .out_n_a(0x7F) // Generic port (no device on the Z80 bus receives it)
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.set_max_cycles(1000);
    runner.load_rom(&rom, 0x0000);

    let result = runner.run_until_halt();
    assert!(
        result.is_ok(),
        "Generic OUT + HALT should work: {:?}",
        result
    );
}

// ============================================================================
// DIAGNOSTIC 4: OUT to port 0x40 (one write, no data byte)
// ============================================================================

#[test]
fn diagnostic_004_out_ym2612_single() {
    let rom = Z80RomBuilder::new(0x0000)
        .ld_a(0x22) // LFO register
        .out_n_a(0x40) // OUT (0x40), A: no device on the Z80 bus receives it
        .halt() // HALT without writing data
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.set_max_cycles(1000);
    runner.load_rom(&rom, 0x0000);

    let result = runner.run_until_halt();
    assert!(
        result.is_ok(),
        "OUT YM2612 address + HALT should work: {:?}",
        result
    );
}

// ============================================================================
// DIAGNOSTIC 5: complete YM2612 write (address + data)
// ============================================================================

#[test]
fn diagnostic_005_out_ym2612_complete() {
    // The YM2612 is memory-mapped on the Z80 (0x4000-0x5FFF), not I/O-mapped
    let rom = Z80RomBuilder::new(0x0000)
        .ym_write_part0(0x22, 0x08) // LFO register 0x22 = 0x08 (enable)
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.set_max_cycles(1000);
    runner.load_rom(&rom, 0x0000);

    let result = runner.run_until_halt();
    assert!(
        result.is_ok(),
        "Complete YM2612 OUT + HALT should work: {:?}",
        result
    );

    // Check that the LFO was enabled
    assert!(runner.ym2612().lfo().is_enabled());
}

// ============================================================================
// DIAGNOSTIC 6: the ym_write_part0 helper (which the YM2612 mini-ROM tests use)
// ============================================================================

#[test]
fn diagnostic_006_ym_write_helper() {
    let rom = Z80RomBuilder::new(0x0000)
        .ym_write_part0(0x22, 0x08) // Using the helper
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.set_max_cycles(1000);
    runner.load_rom(&rom, 0x0000);

    let result = runner.run_until_halt();
    assert!(
        result.is_ok(),
        "ym_write_part0 helper + HALT should work: {:?}",
        result
    );

    // Check that the LFO was enabled
    assert!(runner.ym2612().lfo().is_enabled());
}

// ============================================================================
// DIAGNOSTIC 7: Check the bytes the ROM builder generates
// ============================================================================

#[test]
fn diagnostic_007_verify_rom_bytes() {
    // Build the ROM with the helper
    let rom = Z80RomBuilder::new(0x0000)
        .ym_write_part0(0x22, 0x08)
        .halt()
        .build();

    // Expected bytes:
    // LD A, 0x22 = 0x3E, 0x22
    // LD (0x4000), A = 0x32, 0x00, 0x40  (memory-mapped YM2612)
    // LD A, 0x08 = 0x3E, 0x08
    // LD (0x4001), A = 0x32, 0x01, 0x40
    // HALT = 0x76

    let expected = vec![
        0x3E, 0x22, // LD A, 0x22
        0x32, 0x00, 0x40, // LD (0x4000), A
        0x3E, 0x08, // LD A, 0x08
        0x32, 0x01, 0x40, // LD (0x4001), A
        0x76, // HALT
    ];

    assert_eq!(rom, expected, "ROM bytes do not match the expected ones!");
}

// ============================================================================
// DIAGNOSTIC 8: Trace execution (for when the helper does not work)
// ============================================================================

#[test]
fn diagnostic_008_trace_execution() {
    let rom = Z80RomBuilder::new(0x0000)
        .ym_write_part0(0x22, 0x08)
        .halt()
        .build();

    let mut runner = Z80GenesisTestRunner::new();
    runner.set_max_cycles(100); // Very short, to force a timeout if something is wrong
    runner.load_rom(&rom, 0x0000);

    // Execute step by step
    let mut steps = 0;
    while steps < 20 && !runner.is_halted() {
        let pc_before = runner.get_pc();
        let result = runner.run_instructions(1);
        let pc_after = runner.get_pc();

        println!(
            "Step {}: PC 0x{:04X} -> 0x{:04X}",
            steps, pc_before, pc_after
        );

        if result.is_err() {
            panic!("Error at step {}: {:?}", steps, result);
        }

        steps += 1;
    }

    assert!(
        runner.is_halted(),
        "Should have reached HALT in {} steps",
        steps
    );
    println!("Reached HALT in {} steps", steps);
}
