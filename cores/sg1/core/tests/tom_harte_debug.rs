//! Debug individual Tom Harte tests to understand failures

mod tom_harte;

use retro_core::cpu::Addressable;
use sg1_core::bus::AddressBus;
use sg1_core::cpu68k::M68k;
use std::path::PathBuf;
use tom_harte::{find_test_files, load_test_file};

#[test]
fn debug_first_moveq_test() {
    let repo_path = PathBuf::from("../../../tests/tom_harte_repo");
    let test_files = find_test_files(&repo_path);

    // Developer debug helper over Tom Harte's 68000 test vectors, which the
    // README says how to download into `tests/tom_harte_repo`. Skip (rather
    // than panic) when they are absent so a plain `cargo test --workspace`
    // stays green.
    let Some(test_file) = test_files.iter().find(|p| {
        p.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.starts_with("MOVE.q"))
            .unwrap_or(false)
    }) else {
        eprintln!(
            "skipping debug_first_moveq_test: Tom Harte vectors not downloaded \
             (tests/tom_harte_repo is empty)"
        );
        return;
    };

    let tests = load_test_file(test_file).expect("Failed to load tests");
    let test = &tests[0]; // First test

    println!("\n=== TEST: {} ===", test.name);
    println!("\n--- INITIAL STATE ---");
    println!("PC: 0x{:08X}", test.initial.pc);
    println!("SR: 0x{:04X}", test.initial.sr);
    println!(
        "Prefetch: [0x{:04X}, 0x{:04X}]",
        test.initial.prefetch[0], test.initial.prefetch[1]
    );
    println!("D0-D7:");
    for i in 0..8 {
        let val = test.initial.data_regs()[i];
        println!("  D{}: 0x{:08X}", i, val);
    }

    // Setup
    let mut cpu = M68k::new();
    let mut bus = AddressBus::new();

    // Apply initial state
    cpu.sr = test.initial.sr;
    cpu.pc = test.initial.pc;
    for i in 0..8 {
        cpu.d[i] = test.initial.data_regs()[i];
        cpu.a[i] = test.initial.addr_regs()[i];
    }
    cpu.usp = test.initial.usp;
    cpu.ssp = test.initial.ssp;

    // Write opcode and extension
    println!("\n--- WRITING TO MEMORY ---");
    println!(
        "Writing opcode 0x{:04X} at 0x{:08X}",
        test.initial.prefetch[0], test.initial.pc
    );
    bus.write_word(test.initial.pc, test.initial.prefetch[0]);
    bus.write_word(test.initial.pc + 2, test.initial.prefetch[1]);

    // Write RAM
    for (addr, value) in &test.initial.ram {
        bus.write(*addr, *value);
    }

    // Verify what was written
    let read_back = bus.read_word(test.initial.pc);
    println!("Read back from PC: 0x{:04X}", read_back);

    println!("\n--- EXECUTING ---");
    let cycles = cpu.step(&mut bus);
    println!("Cycles: {}", cycles);

    println!("\n--- FINAL STATE (ACTUAL) ---");
    println!("PC: 0x{:08X}", cpu.pc);
    println!("SR: 0x{:04X}", cpu.sr);
    println!("D0-D7:");
    for i in 0..8 {
        println!("  D{}: 0x{:08X}", i, cpu.d[i]);
    }

    println!("\n--- FINAL STATE (EXPECTED) ---");
    println!("PC: 0x{:08X}", test.final_state.pc);
    println!("SR: 0x{:04X}", test.final_state.sr);
    println!("D0-D7:");
    for i in 0..8 {
        let val = test.final_state.data_regs()[i];
        println!("  D{}: 0x{:08X}", i, val);
    }

    println!("\n--- COMPARISON ---");
    if cpu.pc != test.final_state.pc {
        println!(
            "PC mismatch: expected 0x{:08X}, got 0x{:08X}",
            test.final_state.pc, cpu.pc
        );
    } else {
        println!("PC matches");
    }

    if cpu.sr != test.final_state.sr {
        println!(
            "SR mismatch: expected 0x{:04X}, got 0x{:04X}",
            test.final_state.sr, cpu.sr
        );
    } else {
        println!("SR matches");
    }

    for i in 0..8 {
        let expected = test.final_state.data_regs()[i];
        if cpu.d[i] != expected {
            println!(
                "D{} mismatch: expected 0x{:08X}, got 0x{:08X}",
                i, expected, cpu.d[i]
            );
        } else {
            println!("D{} matches", i);
        }
    }

    // Don't fail the test, just show info
    println!("\n=== END DEBUG ===\n");
}

#[test]
#[ignore = "Needs proper ROM setup for reset()"]
fn debug_simple_moveq_manual() {
    println!("\n=== MANUAL MOVEQ TEST ===");

    let mut cpu = M68k::new();
    let mut bus = AddressBus::new();

    // Reset vectors
    bus.write_word(0x0, 0x0000);
    bus.write_word(0x2, 0xFF00); // Initial SSP
    bus.write_word(0x4, 0x0000);
    bus.write_word(0x6, 0x0400); // Initial PC

    cpu.reset(&mut bus);

    println!("After reset:");
    println!("  PC: 0x{:08X}", cpu.pc);
    println!("  SSP: 0x{:08X}", cpu.ssp);
    println!("  SR: 0x{:04X}", cpu.sr);

    // MOVEQ #5, D0 = 0x7005
    bus.write_word(0x400, 0x7005);

    println!("\nBefore MOVEQ #5, D0:");
    println!("  D0: 0x{:08X}", cpu.d[0]);

    let cycles = cpu.step(&mut bus);

    println!("\nAfter MOVEQ #5, D0:");
    println!("  D0: 0x{:08X}", cpu.d[0]);
    println!("  PC: 0x{:08X}", cpu.pc);
    println!("  Cycles: {}", cycles);

    assert_eq!(cpu.d[0], 5, "MOVEQ #5, D0 should set D0 to 5");
    assert_eq!(cpu.pc, 0x402, "PC should advance by 2");

    println!("Manual MOVEQ test passed!");
}
