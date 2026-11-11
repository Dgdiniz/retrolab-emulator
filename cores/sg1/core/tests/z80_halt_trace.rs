//! Detailed trace to see what is happening

use retro_core::cpu::Addressable;
use sg1_core::bus::AddressBus;
use sg1_core::z80::Z80;

#[test]
#[ignore] // AddressBus treats 0x0000 as read-only ROM by design. Use Z80GenesisTestRunner for proper Z80+Genesis tests.
fn trace_what_is_being_executed() {
    let mut z80 = Z80::new();
    let mut bus = AddressBus::new();

    // Write HALT (0x76) at 0x0000
    bus.write(0x0000, 0x76);

    // Write a few more bytes for context
    for i in 1..10 {
        bus.write(i, 0x00); // NOP
    }

    z80.set_pc(0x0000);

    println!("Memory:");
    for i in 0..10 {
        println!("  0x{:04X}: 0x{:02X}", i, bus.read(i));
    }

    println!("\nBefore executing:");
    println!("  PC = 0x{:04X}", z80.get_pc());
    println!("  A = 0x{:02X}", z80.get_a());

    println!("\nExecuting step...");
    let cycles = z80.step(&mut bus);

    println!("\nAfter executing:");
    println!("  PC = 0x{:04X}", z80.get_pc());
    println!("  A = 0x{:02X}", z80.get_a());
    println!("  cycles = {}", cycles);
    println!("  is_halted = {}", z80.is_halted());

    // Check that PC advanced correctly
    assert_eq!(
        z80.get_pc(),
        0x0001,
        "PC should advance to 0x0001 after HALT"
    );
    assert!(z80.is_halted(), "Should be halted!");
}
