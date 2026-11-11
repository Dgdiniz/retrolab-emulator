//! Integration tests for the per-CPU coverage bitmap.
//!
//! Verifies the bitmap's public lifecycle:
//!   - `step_*` paths record coverage for the address they executed.
//!   - `reset()` preserves coverage (only CPU/VDP/audio state resets).
//!   - `load_rom(different)` clears coverage (new address-space layout).
//!   - `load_rom(same hash)` preserves coverage (true hot-reload).
//!   - `clear_coverage(cpu_id)` is the explicit user-driven wipe.
//!
//! The 68K runs a small program each test builds: a run of NOPs and a
//! branch back to its start.

mod helpers;

use helpers::M68kRomBuilder;
use retro_core::{DebuggerCore, EmulatorCore};
use sg1_core::GenesisEmulator;

/// Where the program starts.
const ORG: u32 = 0x400;

/// A program of 64 NOPs that branches back to its start, so every step
/// lands on an address of its own until the loop comes round. `tag` is the
/// immediate of its first instruction: two tags are two different
/// cartridges.
fn program(tag: u16) -> Vec<u8> {
    let mut b = M68kRomBuilder::new(ORG);
    b.move_w_imm_to_d0(tag);
    b.delay(64);
    // The displacement counts from the word after the BRA's opcode.
    let back = ORG as i32 - (b.current_pc() as i32 + 2);
    b.bra_w(back as i16);
    b.build()
}

fn load_emu(rom: &[u8]) -> GenesisEmulator {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(rom).expect("load_rom");
    emu
}

#[test]
fn coverage_starts_empty_on_fresh_emulator() {
    let emu = GenesisEmulator::new();
    let (size_68k, covered_68k) = emu.debugger_ref().coverage_stats(0);
    let (size_z80, covered_z80) = emu.debugger_ref().coverage_stats(1);
    assert!(size_68k > 0, "68K coverage bitmap should be allocated");
    assert!(size_z80 > 0, "Z80 coverage bitmap should be allocated");
    assert_eq!(covered_68k, 0);
    assert_eq!(covered_z80, 0);
}

#[test]
fn step_records_coverage_for_each_executed_address() {
    use retro_core::ExecutionMode;

    let mut emu = load_emu(&program(1));
    emu.debugger().set_execution_mode(ExecutionMode::Paused);

    // Read PC directly off the M68k struct: `get_cpu_states()` only
    // returns a snapshot updated by the full debug-step path, so it's
    // empty before any step has run.
    let pc_before_first = emu.cpu().pc;
    assert!(
        !emu.debugger_ref().is_covered(0, pc_before_first),
        "Reset PC should not be covered until we step from it"
    );

    let mut executed_pcs: Vec<u32> = Vec::new();
    for _ in 0..20 {
        let pc = emu.cpu().pc;
        executed_pcs.push(pc);
        emu.step();
    }

    for pc in &executed_pcs {
        assert!(
            emu.debugger_ref().is_covered(0, *pc),
            "PC 0x{:06X} should be covered after step()",
            pc
        );
    }

    // Coverage count is the number of *unique* executed addresses, so
    // it equals `executed_pcs` deduped. Reading both ways guards
    // against double-counting bugs in the bitmap.
    use std::collections::HashSet;
    let unique_pcs: HashSet<u32> = executed_pcs.iter().copied().collect();
    let (_, covered) = emu.debugger_ref().coverage_stats(0);
    assert_eq!(
        covered as usize,
        unique_pcs.len(),
        "Coverage count should match the unique executed PC set"
    );
}

#[test]
fn coverage_survives_reset() {
    let mut emu = load_emu(&program(1));

    for _ in 0..10 {
        emu.step();
    }
    let (_, covered_before) = emu.debugger_ref().coverage_stats(0);
    assert!(covered_before > 0);

    emu.reset();

    let (_, covered_after) = emu.debugger_ref().coverage_stats(0);
    assert_eq!(
        covered_before, covered_after,
        "reset() must NOT wipe the coverage bitmap"
    );
}

#[test]
fn coverage_survives_hot_reload_of_same_rom() {
    let rom = program(1);
    let mut emu = GenesisEmulator::new();
    emu.load_rom(&rom).expect("first load");

    for _ in 0..10 {
        emu.step();
    }
    let (_, covered_before) = emu.debugger_ref().coverage_stats(0);
    assert!(covered_before > 0);

    // Re-load the same ROM bytes: the hash matches, so coverage is kept.
    emu.load_rom(&rom).expect("hot reload");

    let (_, covered_after) = emu.debugger_ref().coverage_stats(0);
    assert_eq!(
        covered_before, covered_after,
        "load_rom() with identical bytes must preserve coverage (hot-reload path)"
    );
}

#[test]
fn coverage_clears_on_load_of_different_rom() {
    let mut emu = load_emu(&program(1));

    for _ in 0..10 {
        emu.step();
    }
    let (_, covered_before) = emu.debugger_ref().coverage_stats(0);
    assert!(covered_before > 0);

    emu.load_rom(&program(2)).expect("load the second program");

    let (_, covered_after) = emu.debugger_ref().coverage_stats(0);
    assert_eq!(
        covered_after, 0,
        "load_rom() with a different cartridge must clear coverage"
    );
}

#[test]
fn explicit_clear_coverage_wipes_only_the_targeted_cpu() {
    use retro_core::cpu::Addressable;

    let mut emu = load_emu(&program(1));

    // Ten 68K steps over the program's straight run mark ten 68K addresses,
    // with the Z80 still held in reset.
    for _ in 0..10 {
        emu.step();
    }
    // Then the Z80, released from reset, steps through ten NOPs (0x00) at
    // the start of its RAM, marking Z80 addresses 0x0000-0x0009.
    for addr in 0..10 {
        emu.bus_mut().z80_ram_mut().write(addr, 0x00);
    }
    emu.set_z80_reset(false);
    for _ in 0..10 {
        emu.step_cpu(1);
    }
    let (_, covered_68k) = emu.debugger_ref().coverage_stats(0);
    let (_, covered_z80) = emu.debugger_ref().coverage_stats(1);
    assert_eq!(covered_68k, 10, "ten 68K steps should mark ten addresses");
    assert_eq!(covered_z80, 10, "ten Z80 steps should mark ten addresses");

    // Clearing the 68K's bitmap leaves every Z80 mark in place.
    emu.debugger().clear_coverage(0);
    assert_eq!(emu.debugger_ref().coverage_stats(0).1, 0);
    assert_eq!(
        emu.debugger_ref().coverage_stats(1).1,
        covered_z80,
        "clear_coverage(0) must not touch the Z80 bitmap"
    );
    assert!(emu.debugger_ref().is_covered(1, 0x0000));

    // And the other way round: the 68K marks a run again (with the Z80
    // released, each 68K step also runs it), then clearing the Z80's bitmap
    // leaves every 68K mark in place.
    for _ in 0..10 {
        emu.step();
    }
    let (_, covered_68k) = emu.debugger_ref().coverage_stats(0);
    assert!(covered_68k > 0);
    emu.debugger().clear_coverage(1);
    assert_eq!(emu.debugger_ref().coverage_stats(1).1, 0);
    assert_eq!(
        emu.debugger_ref().coverage_stats(0).1,
        covered_68k,
        "clear_coverage(1) must not touch the 68K bitmap"
    );
}

#[test]
fn run_frame_populates_coverage_in_running_mode() {
    use retro_core::ExecutionMode;

    let mut emu = load_emu(&program(1));
    emu.debugger().set_execution_mode(ExecutionMode::Running);

    let (_, covered_before) = emu.debugger_ref().coverage_stats(0);
    emu.run_frame();
    let (_, covered_after) = emu.debugger_ref().coverage_stats(0);

    assert!(
        covered_after > covered_before,
        "run_frame() (fast path) must record coverage for each executed instruction \
         (was {covered_before}, now {covered_after})"
    );
}

#[test]
fn nearest_set_at_or_before_returns_the_most_recent_executed_address() {
    let mut emu = load_emu(&program(1));
    for _ in 0..30 {
        emu.step();
    }

    let pc = emu.cpu().pc;
    let nearest = emu
        .debugger_ref()
        .coverage_nearest_set_at_or_before(0, pc.saturating_sub(1));
    assert!(
        nearest.is_some(),
        "After 30 steps there must be at least one covered address strictly before the live PC"
    );
}

#[test]
fn out_of_range_cpu_id_is_silently_ignored() {
    let mut emu = load_emu(&program(1));
    emu.step();
    assert_eq!(emu.debugger_ref().coverage_stats(2), (0, 0));
    assert_eq!(emu.debugger_ref().coverage_stats(99), (0, 0));
    assert!(!emu.debugger_ref().is_covered(2, 0x100));
    assert_eq!(
        emu.debugger_ref()
            .coverage_nearest_set_at_or_before(2, 0x100),
        None
    );
    assert_eq!(
        emu.debugger_ref()
            .coverage_nearest_set_at_or_after(2, 0x100),
        None
    );

    // Make sure clear_coverage on an invalid id doesn't panic and
    // doesn't accidentally wipe a real CPU's bitmap.
    let (_, covered_68k_before) = emu.debugger_ref().coverage_stats(0);
    emu.debugger().clear_coverage(42);
    let (_, covered_68k_after) = emu.debugger_ref().coverage_stats(0);
    assert_eq!(covered_68k_before, covered_68k_after);
}
