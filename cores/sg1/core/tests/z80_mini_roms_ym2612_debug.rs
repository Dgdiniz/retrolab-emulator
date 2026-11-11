//! Debug Z80 Mini ROM YM2612 Test

mod helpers;

use helpers::{Z80GenesisTestRunner, Z80RomBuilder};

#[test]
fn test_minirom_lfo_enable_debug() {
    let rom = Z80RomBuilder::new(0x0000)
        .ym_write_part0(0x22, 0x08)
        .halt()
        .build();

    println!("\n=== ROM Generated ===");
    for (i, byte) in rom.iter().enumerate() {
        print!("{:02X} ", byte);
        if (i + 1) % 16 == 0 {
            println!();
        }
    }
    println!("\n=====================\n");

    let mut runner = Z80GenesisTestRunner::new();
    runner.load_rom(&rom, 0x0000);

    println!("Before execution:");
    println!("  Z80 PC: 0x{:04X}", runner.z80().get_pc());
    println!("  Z80 A: 0x{:02X}", runner.z80().get_a());
    println!(
        "  YM2612 LFO enabled: {}",
        runner.ym2612().lfo().is_enabled()
    );

    // Execute instruction by instruction
    let mut instr_count = 0;
    while !runner.z80().is_halted() && instr_count < 20 {
        let pc_before = runner.z80().get_pc();
        runner.run_instructions(1).expect("Should run");
        let pc_after = runner.z80().get_pc();

        println!(
            "  Instr #{}: PC 0x{:04X} -> 0x{:04X}",
            instr_count, pc_before, pc_after
        );

        instr_count += 1;
    }

    println!("\nAfter execution:");
    println!("  Z80 PC: 0x{:04X}", runner.z80().get_pc());
    println!("  Z80 halted: {}", runner.z80().is_halted());
    println!("  Instructions executed: {}", instr_count);
    println!(
        "  YM2612 LFO enabled: {}",
        runner.ym2612().lfo().is_enabled()
    );

    assert!(
        runner.ym2612().lfo().is_enabled(),
        "LFO should be enabled after Z80 writes 0x08 to YM2612 register 0x22"
    );
}
