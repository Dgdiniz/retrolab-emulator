//! Integration tests for `disassemble_window`, the bitmap-aware
//! disassembly entry point that replaces `disassemble_around_pc`.
//!
//! Each test handcrafts a 68K image directly into ROM and primes the
//! coverage bitmap via `record_coverage_68k` so the algorithm's
//! behaviour can be observed without having to drive real execution
//! through the system step paths.

use retro_core::{DisasmRowKind, EmulatorCore};
use sg1_core::GenesisEmulator;

/// Build a Genesis-shaped 1 MiB ROM with a minimal header so
/// `build_genesis_cartridge` accepts it. Vector table sits at the
/// start (SSP / reset vector), code goes at `0x200` by convention.
fn build_min_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 1024 * 1024];
    // Reset SSP @ 0x000000 = 0x00FF0000
    rom[0..4].copy_from_slice(&[0x00, 0xFF, 0x00, 0x00]);
    // Reset PC  @ 0x000004 = 0x00000200
    rom[4..8].copy_from_slice(&[0x00, 0x00, 0x02, 0x00]);
    // Genesis console marker at 0x100 (matches Genesis ROM detection).
    let console_marker = b"SEGA GENESIS    ";
    rom[0x100..0x100 + console_marker.len()].copy_from_slice(console_marker);
    rom
}

fn write_word(rom: &mut [u8], addr: usize, value: u16) {
    rom[addr..addr + 2].copy_from_slice(&value.to_be_bytes());
}

fn load_min_emu_with_rom(rom: Vec<u8>) -> GenesisEmulator {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(&rom).expect("load_rom");
    emu
}

#[test]
fn empty_bitmap_yields_unknown_before_and_code_from_pc_forward() {
    // Lay out: 0x200..0x208 = 4 NOPs (0x4E71 each), so PC=0x200 has
    // valid code immediately even with no coverage recorded yet.
    let mut rom = build_min_rom();
    write_word(&mut rom, 0x200, 0x4E71);
    write_word(&mut rom, 0x202, 0x4E71);
    write_word(&mut rom, 0x204, 0x4E71);
    write_word(&mut rom, 0x206, 0x4E71);
    let mut emu = load_min_emu_with_rom(rom);

    // Empty coverage -> no covered predecessors below PC.
    let rows = emu.disassemble_window(0, 0x200, 5, 5);

    assert_eq!(rows.len(), 10);
    // First 5 rows are Unknown (no covered predecessors at all).
    for row in &rows[..5] {
        assert_eq!(row.kind, DisasmRowKind::Unknown);
        assert!(row.mnemonic.is_empty());
    }
    // The PC row is always decoded even if the bitmap hasn't recorded
    // it yet: the CPU is about to execute it.
    assert_eq!(rows[5].kind, DisasmRowKind::Code);
    assert_eq!(rows[5].address, 0x200);
    assert_eq!(rows[5].mnemonic.to_uppercase(), "NOP");
    // Forward-from-PC rows: first one was decoded above; rows[6..10]
    // are Unknown because their addresses aren't covered yet (only
    // the PC is special-cased).
    for row in &rows[6..] {
        assert_eq!(row.kind, DisasmRowKind::Unknown);
    }
}

#[test]
fn fully_covered_bitmap_yields_all_code_rows() {
    // 4 NOPs at 0x200..0x208, then PC at 0x208 (NOP). Coverage
    // primed for every instruction start.
    let mut rom = build_min_rom();
    for off in (0x200..0x210).step_by(2) {
        write_word(&mut rom, off, 0x4E71);
    }
    let mut emu = load_min_emu_with_rom(rom);
    for addr in (0x200..0x210u32).step_by(2) {
        emu.debugger().record_coverage_68k(addr);
    }

    let rows = emu.disassemble_window(0, 0x208, 3, 2);
    assert_eq!(rows.len(), 5);
    for row in &rows {
        assert_eq!(
            row.kind,
            DisasmRowKind::Code,
            "expected Code at 0x{:X}",
            row.address
        );
        assert_eq!(row.mnemonic.to_uppercase(), "NOP");
    }
    // Backward addresses should be the three predecessors of PC=0x208.
    assert_eq!(rows[0].address, 0x202);
    assert_eq!(rows[1].address, 0x204);
    assert_eq!(rows[2].address, 0x206);
    assert_eq!(rows[3].address, 0x208);
    assert_eq!(rows[4].address, 0x20A);
}

#[test]
fn gap_inside_covered_region_emits_byte_by_byte_unknown_rows() {
    // Code at 0x200, 0x202, 0x204, then a 16-byte data hole, then
    // code resumes at 0x216. Coverage primed only for the code
    // addresses. Forward from PC=0x200 should walk the three NOPs,
    // then emit one Unknown row *per byte* of the hole (mirrors the
    // backward walk's byte-by-byte behaviour, no jumping), and pick
    // code back up at 0x216 once the covered address is reached.
    let mut rom = build_min_rom();
    write_word(&mut rom, 0x200, 0x4E71);
    write_word(&mut rom, 0x202, 0x4E71);
    write_word(&mut rom, 0x204, 0x4E71);
    // Garbage bytes 0x206..0x216, coverage NOT recorded for these.
    for off in (0x206..0x216).step_by(2) {
        write_word(&mut rom, off, 0xFFFF); // illegal opcode -> DC.W
    }
    write_word(&mut rom, 0x216, 0x4E71);
    write_word(&mut rom, 0x218, 0x4E71);
    let mut emu = load_min_emu_with_rom(rom);
    for addr in [0x200u32, 0x202, 0x204, 0x216, 0x218] {
        emu.debugger().record_coverage_68k(addr);
    }

    // 3 NOPs + 16 byte-by-byte Unknown gap rows (0x206..0x215) + 2 NOPs
    // at 0x216 / 0x218 = 21 rows total.
    let rows = emu.disassemble_window(0, 0x200, 0, 21);
    assert_eq!(rows.len(), 21);
    assert_eq!(rows[0].kind, DisasmRowKind::Code);
    assert_eq!(rows[0].address, 0x200);
    assert_eq!(rows[1].kind, DisasmRowKind::Code);
    assert_eq!(rows[1].address, 0x202);
    assert_eq!(rows[2].kind, DisasmRowKind::Code);
    assert_eq!(rows[2].address, 0x204);
    // 16 Unknown rows at 0x206..=0x215, one per byte, no jump.
    for (i, expected_addr) in (0x206u32..=0x215).enumerate() {
        let row = &rows[3 + i];
        assert_eq!(
            row.kind,
            DisasmRowKind::Unknown,
            "row {} should be Unknown at 0x{:X}, was {:?}",
            3 + i,
            expected_addr,
            row.kind
        );
        assert_eq!(row.address, expected_addr);
    }
    // Code resumes at the next covered address.
    assert_eq!(rows[19].kind, DisasmRowKind::Code);
    assert_eq!(rows[19].address, 0x216);
    assert_eq!(rows[20].kind, DisasmRowKind::Code);
    assert_eq!(rows[20].address, 0x218);
}

#[test]
fn branch_jump_scenario_does_not_show_stale_predecessors() {
    // The 68K executed 0x200, 0x202, 0x204, then branched far away to
    // 0x5000. Those addresses are covered and lie below PC, but they are
    // not its predecessors, so they must not show as the rows "above" it.
    //
    // The neighborhood walk only probes addresses immediately below
    // the current target. Since no covered address lies in
    // [0x4FF6..0x4FFF], every before-row must be a byte-by-byte
    // Unknown at PC - (3 - i), not an address in the 0x200 range.
    let mut rom = build_min_rom();
    for off in [0x200, 0x202, 0x204, 0x5000, 0x5002] {
        write_word(&mut rom, off, 0x4E71);
    }
    let mut emu = load_min_emu_with_rom(rom);
    for addr in [0x200u32, 0x202, 0x204] {
        emu.debugger().record_coverage_68k(addr);
    }
    // The branch target itself isn't covered yet (the CPU hasn't
    // resumed stepping): this mirrors the "we just hit a breakpoint at
    // the branch target" scenario.

    let rows = emu.disassemble_window(0, 0x5000, 3, 2);
    assert_eq!(rows.len(), 5);
    // Before-rows must be the three bytes immediately below PC, in
    // ascending order after the reverse: 0x4FFD, 0x4FFE, 0x4FFF.
    assert_eq!(rows[0].kind, DisasmRowKind::Unknown);
    assert_eq!(rows[0].address, 0x4FFD);
    assert_eq!(rows[1].kind, DisasmRowKind::Unknown);
    assert_eq!(rows[1].address, 0x4FFE);
    assert_eq!(rows[2].kind, DisasmRowKind::Unknown);
    assert_eq!(rows[2].address, 0x4FFF);
    // The PC row is always decoded (special-cased so the user can see
    // what's about to run).
    assert_eq!(rows[3].kind, DisasmRowKind::Code);
    assert_eq!(rows[3].address, 0x5000);
}

#[test]
fn branch_jump_scenario_fills_in_after_more_steps_cover_the_new_path() {
    // Same scenario as above, but now the CPU has executed a few
    // instructions in the new path -> predecessors are covered ->
    // backward walk returns real Code rows.
    let mut rom = build_min_rom();
    for off in [0x200, 0x202, 0x204, 0x5000, 0x5002, 0x5004, 0x5006, 0x5008] {
        write_word(&mut rom, off, 0x4E71);
    }
    let mut emu = load_min_emu_with_rom(rom);
    for addr in [0x200u32, 0x202, 0x204, 0x5000, 0x5002, 0x5004, 0x5006] {
        emu.debugger().record_coverage_68k(addr);
    }

    let rows = emu.disassemble_window(0, 0x5008, 3, 2);
    assert_eq!(rows.len(), 5);
    // All three before-rows should be real code from the 0x5000 path.
    for row in &rows[..3] {
        assert_eq!(row.kind, DisasmRowKind::Code);
        assert!(row.address >= 0x5000);
        assert_eq!(row.mnemonic.to_uppercase(), "NOP");
    }
    assert_eq!(rows[0].address, 0x5002);
    assert_eq!(rows[1].address, 0x5004);
    assert_eq!(rows[2].address, 0x5006);
}

#[test]
fn before_count_of_zero_skips_backward_walk_entirely() {
    let mut rom = build_min_rom();
    write_word(&mut rom, 0x200, 0x4E71);
    write_word(&mut rom, 0x202, 0x4E71);
    let mut emu = load_min_emu_with_rom(rom);

    let rows = emu.disassemble_window(0, 0x200, 0, 2);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.address >= 0x200));
}

#[test]
fn after_count_of_zero_returns_only_backward_rows() {
    let mut rom = build_min_rom();
    write_word(&mut rom, 0x200, 0x4E71);
    let mut emu = load_min_emu_with_rom(rom);
    emu.debugger().record_coverage_68k(0x200);

    let rows = emu.disassemble_window(0, 0x202, 2, 0);
    assert_eq!(rows.len(), 2);
    // First row is Unknown (the gap below 0x200), second is the
    // covered 0x200 NOP.
    assert_eq!(rows[1].kind, DisasmRowKind::Code);
    assert_eq!(rows[1].address, 0x200);
}

#[test]
fn pc_at_address_zero_does_not_walk_backward_past_origin() {
    let mut rom = build_min_rom();
    write_word(&mut rom, 0x200, 0x4E71);
    let mut emu = load_min_emu_with_rom(rom);

    // PC=0 -> no addresses below it -> backward walk should bail out
    // gracefully without panicking.
    let rows = emu.disassemble_window(0, 0, 5, 1);
    // Forward decode at address 0 always produces one row; backward
    // rows may be empty when PC is at the start of the address space.
    assert!(rows.iter().any(|r| r.address == 0));
}

#[test]
fn branch_jump_then_partial_step_back_into_old_region() {
    // The CPU executed the TST.L at 0x200 and the BNE at 0x206, and PC
    // is now 0x208: about to execute, not yet covered. The rows above PC
    // must be those two instructions, not Unknown rows walked byte by
    // byte down from the end of the BNE.
    //
    // The backward probes at target=0x208 try {0x206, 0x204, 0x202,
    // 0x200, 0x1FE}. 0x206 decodes to a 2-byte BNE ending at 0x208 ->
    // match, emit Code. The next iteration's probes from target=0x206
    // land on 0x200 (a 6-byte TST.L ending at 0x206) -> match, emit
    // Code. From target=0x200 the probes are all uncovered -> emit
    // Unknown rows immediately below 0x200, byte by byte.
    let mut rom = build_min_rom();
    // TST.L $00A10008 at 0x200 (6 bytes).
    write_word(&mut rom, 0x200, 0x4AB9);
    write_word(&mut rom, 0x202, 0x00A1);
    write_word(&mut rom, 0x204, 0x0008);
    // BNE +6 (to 0x20E) at 0x206, but for this test we only need
    // the bytes to be a valid 2-byte 68K instruction so coverage
    // probes work.
    write_word(&mut rom, 0x206, 0x6606);
    // The BNE's fall-through (the branch not taken), about to execute,
    // not yet covered.
    write_word(&mut rom, 0x208, 0x4E71);
    let mut emu = load_min_emu_with_rom(rom);
    // Cover the two instructions actually executed before the branch.
    for addr in [0x200u32, 0x206] {
        emu.debugger().record_coverage_68k(addr);
    }

    let rows = emu.disassemble_window(0, 0x208, 6, 1);
    assert_eq!(rows.len(), 7);

    // Backward neighborhood, in display order (top -> bottom):
    //   0x1FC Unknown   <- byte-by-byte below 0x200
    //   0x1FD Unknown
    //   0x1FE Unknown
    //   0x1FF Unknown
    //   0x200 Code   (TST.L, 6 bytes ending at 0x206)
    //   0x206 Code   (BNE,   2 bytes ending at 0x208)
    //   0x208 Code   (PC row, always decoded)
    //
    // The TST.L and BNE are *adjacent*: the TST.L decodes to a single
    // 6-byte row, so addresses 0x201..0x205 don't appear as separate
    // rows: they're interior bytes of the TST.L. Likewise 0x207 is
    // an interior byte of the BNE. Once `target` reaches 0x200, the
    // backward probes (0x1FE..0x1F6) are all uncovered, so the
    // remaining 4 backward iterations emit byte-by-byte Unknown rows
    // immediately below 0x200.
    assert_eq!(rows[0].kind, DisasmRowKind::Unknown);
    assert_eq!(rows[0].address, 0x1FC);
    assert_eq!(rows[1].kind, DisasmRowKind::Unknown);
    assert_eq!(rows[1].address, 0x1FD);
    assert_eq!(rows[2].kind, DisasmRowKind::Unknown);
    assert_eq!(rows[2].address, 0x1FE);
    assert_eq!(rows[3].kind, DisasmRowKind::Unknown);
    assert_eq!(rows[3].address, 0x1FF);
    assert_eq!(rows[4].kind, DisasmRowKind::Code);
    assert_eq!(rows[4].address, 0x200);
    assert_eq!(rows[4].bytes.len(), 6);
    assert_eq!(rows[5].kind, DisasmRowKind::Code);
    assert_eq!(rows[5].address, 0x206);
    assert_eq!(rows[5].bytes.len(), 2);

    // PC row (always decoded even when uncovered).
    assert_eq!(rows[6].kind, DisasmRowKind::Code);
    assert_eq!(rows[6].address, 0x208);
}

#[test]
fn forward_walk_emits_byte_by_byte_unknown_in_gap() {
    // The forward walk emits one Unknown row per byte of a gap,
    // matching the backward walk's behaviour, rather than one row
    // for the whole gap.
    let mut rom = build_min_rom();
    write_word(&mut rom, 0x200, 0x4E71); // NOP, covered (the PC row)
    let mut emu = load_min_emu_with_rom(rom);
    emu.debugger().record_coverage_68k(0x200);

    let rows = emu.disassemble_window(0, 0x200, 0, 6);
    assert_eq!(rows.len(), 6);
    // PC row is always decoded.
    assert_eq!(rows[0].kind, DisasmRowKind::Code);
    assert_eq!(rows[0].address, 0x200);
    // Remaining rows are byte-by-byte Unknown, addressed
    // sequentially starting one byte past the end of the NOP.
    for (i, expected_addr) in (0x202u32..=0x206).enumerate() {
        let row = &rows[1 + i];
        assert_eq!(
            row.kind,
            DisasmRowKind::Unknown,
            "row {} should be Unknown at 0x{:X}, was {:?}",
            1 + i,
            expected_addr,
            row.kind
        );
        assert_eq!(row.address, expected_addr);
    }
}

#[test]
fn z80_disassemble_window_uses_z80_coverage_bitmap() {
    // Empty ROM is enough: Z80 disasm reads from Z80 RAM via the
    // Z80 bus, but for this test we just want to verify the routing.
    let rom = build_min_rom();
    let mut emu = load_min_emu_with_rom(rom);

    // Record a Z80 coverage entry at 0x100 so the backward walk has
    // something to find.
    emu.debugger().record_coverage_z80(0x100);

    let rows = emu.disassemble_window(1, 0x200, 2, 2);
    assert_eq!(rows.len(), 4);
    // The PC row at 0x200 always decodes; rows beyond it are Unknown
    // (no Z80 coverage further forward).
    assert_eq!(rows[2].kind, DisasmRowKind::Code);
    assert_eq!(rows[2].address, 0x200);
}
