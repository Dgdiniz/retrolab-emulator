//! Debug HALT - the simplest possible test

use retro_core::cpu::Addressable;
use sg1_core::bus::AddressBus;
use sg1_core::z80::Z80;

#[test]
#[ignore] // AddressBus treats 0x0000 as read-only ROM by design. Use Z80GenesisTestRunner for proper Z80+Genesis tests.
fn debug_halt_manual() {
    let mut z80 = Z80::new();
    let mut bus = AddressBus::new();

    // Write the HALT instruction (0x76) at address 0x0000
    bus.write(0x0000, 0x76);

    // Set PC to 0x0000
    z80.set_pc(0x0000);

    println!("Before step:");
    println!("  PC = 0x{:04X}", z80.get_pc());
    println!("  is_halted = {}", z80.is_halted());

    // Execute 1 step (should execute HALT)
    let cycles = z80.step(&mut bus);

    println!("\nAfter step:");
    println!("  PC = 0x{:04X}", z80.get_pc());
    println!("  is_halted = {}", z80.is_halted());
    println!("  cycles = {}", cycles);

    // HALT should have been detected
    assert!(
        z80.is_halted(),
        "Z80 should be halted after executing 0x76!"
    );
}

#[test]
#[ignore] // AddressBus treats 0x0000 as read-only ROM by design. Use Z80GenesisTestRunner for proper Z80+Genesis tests.
fn debug_halt_loop() {
    let mut z80 = Z80::new();
    let mut bus = AddressBus::new();

    // Write HALT
    bus.write(0x0000, 0x76);
    z80.set_pc(0x0000);

    let mut iterations = 0;
    let max_iterations = 10;

    while iterations < max_iterations {
        println!("Iteration {}: is_halted = {}", iterations, z80.is_halted());

        if z80.is_halted() {
            println!("HALT detected at iteration {}!", iterations);
            return; // Success!
        }

        let cycles = z80.step(&mut bus);
        println!("  Executed step, cycles = {}", cycles);

        iterations += 1;
    }

    panic!("HALT was not detected after {} iterations!", max_iterations);
}
