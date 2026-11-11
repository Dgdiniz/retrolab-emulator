//! Comparison: HALT from a plain Ram and from the AddressBus

use retro_core::cpu::Addressable;
use retro_core::memory::Ram;
use sg1_core::bus::AddressBus;
use sg1_core::z80::Z80;

#[test]
fn test_halt_with_ram_direct() {
    let mut cpu = Z80::new();
    let mut ram = Ram::new(0x10000);

    // Write HALT
    ram.write(0x0000, 0x76);
    cpu.pc = 0x0000;

    println!("[RAM] Before: PC=0x{:04X}, halted={}", cpu.pc, cpu.halted);

    // Step
    let cycles = cpu.step(&mut ram);

    println!(
        "[RAM] After: PC=0x{:04X}, halted={}, cycles={}",
        cpu.pc, cpu.halted, cycles
    );

    assert!(cpu.halted, "HALT with Ram should work!");
    assert_eq!(cpu.pc, 0x0001, "PC should advance");
}

#[test]
#[ignore] // AddressBus treats 0x0000 as read-only ROM by design. Use Z80GenesisTestRunner for proper Z80+Genesis tests.
fn test_halt_with_address_bus() {
    let mut cpu = Z80::new();
    let mut bus = AddressBus::new();

    // Write HALT
    bus.write(0x0000, 0x76);
    cpu.pc = 0x0000;

    println!("[BUS] Before: PC=0x{:04X}, halted={}", cpu.pc, cpu.halted);

    // Step
    let cycles = cpu.step(&mut bus);

    println!(
        "[BUS] After: PC=0x{:04X}, halted={}, cycles={}",
        cpu.pc, cpu.halted, cycles
    );

    assert!(cpu.halted, "HALT with AddressBus should work!");
    assert_eq!(cpu.pc, 0x0001, "PC should advance");
}

#[test]
#[ignore] // AddressBus treats 0x0000 as read-only ROM by design. Use Z80GenesisTestRunner for proper Z80+Genesis tests.
fn test_multiple_steps_ram_vs_bus() {
    // Test with Ram
    let mut cpu1 = Z80::new();
    let mut ram = Ram::new(0x10000);
    ram.write(0x0000, 0x76); // HALT
    cpu1.pc = 0x0000;

    let mut total_cycles1 = 0;
    let mut steps1 = 0;
    while !cpu1.halted && steps1 < 10 {
        total_cycles1 += cpu1.step(&mut ram);
        steps1 += 1;
    }

    println!(
        "[RAM] Steps: {}, Cycles: {}, Halted: {}",
        steps1, total_cycles1, cpu1.halted
    );

    // Test with AddressBus
    let mut cpu2 = Z80::new();
    let mut bus = AddressBus::new();
    bus.write(0x0000, 0x76); // HALT
    cpu2.pc = 0x0000;

    let mut total_cycles2 = 0;
    let mut steps2 = 0;
    while !cpu2.halted && steps2 < 10 {
        total_cycles2 += cpu2.step(&mut bus);
        steps2 += 1;
    }

    println!(
        "[BUS] Steps: {}, Cycles: {}, Halted: {}",
        steps2, total_cycles2, cpu2.halted
    );

    assert_eq!(steps1, 1, "Ram should execute 1 step");
    assert_eq!(steps2, 1, "Bus should execute 1 step");
}
