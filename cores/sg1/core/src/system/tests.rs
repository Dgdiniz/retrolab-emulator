//! Unit tests of the machine as a whole.

use super::*;
use crate::vdp::dma::MemoryDma;

#[test]
fn test_genesis_creation() {
    let genesis = GenesisEmulator::new();
    let info = genesis.system_info();

    assert_eq!(info.name, "Sega Genesis");
    assert_eq!(info.cpus.len(), 2);
    assert_eq!(info.cpus[0].architecture, CpuArchitecture::M68000);
    assert_eq!(info.cpus[1].architecture, CpuArchitecture::Z80);
    assert_eq!(genesis.frame_count(), 0);
}

#[test]
fn test_display_info() {
    let genesis = GenesisEmulator::new();
    let display = genesis.get_display_info();

    assert_eq!(display.width, 320);
    assert_eq!(display.height, 224);
}

#[test]
fn test_framebuffer() {
    let genesis = GenesisEmulator::new();
    let fb = genesis.get_framebuffer();

    assert_eq!(fb.width, 320);
    assert_eq!(fb.height, 224);
    assert_eq!(fb.data.len(), 320 * 224 * 4);
}

#[test]
fn repaint_held_frame_pushes_vdp_state_into_cached_framebuffer() {
    // A paused debugger edit (layer toggle, VRAM/CRAM write)
    // must reach the host. The browser reads get_framebuffer(), the
    // cached copy, so repaint must re-render the VDP *and* re-sync that
    // cache. Drive a distinct backdrop through the public VDP ports, then
    // confirm the cached framebuffer paints it and tracks a later change.
    // (Display ON + empty VRAM => both planes are transparent and the whole
    // screen falls through to the backdrop colour.)
    use crate::vdp::Vdp;

    fn set_reg(vdp: &mut Vdp, reg: u16, val: u16) {
        vdp.write_word_port(0x04, 0x8000 | (reg << 8) | (val & 0xFF));
    }
    fn write_cram(vdp: &mut Vdp, entry: u16, color: u16) {
        // CRAM-write code (CD=0b0011): first ctrl word top bits = 0b11.
        vdp.write_word_port(0x04, 0xC000 | ((entry * 2) & 0x3FFF));
        vdp.write_word_port(0x04, 0x0000);
        vdp.write_word_port(0x00, color);
    }
    fn top_left(genesis: &GenesisEmulator) -> (u8, u8, u8) {
        let d = &genesis.get_framebuffer().data;
        (d[0], d[1], d[2])
    }

    let mut genesis = GenesisEmulator::new();
    {
        let vdp = genesis.vdp_mut();
        set_reg(vdp, 1, 0x44); // display on, mode 5
        write_cram(vdp, 1, 0x0E00); // CRAM[1] = bright blue
        set_reg(vdp, 7, 0x01); // backdrop -> CRAM entry 1
    }

    genesis.repaint_held_frame();
    let green = top_left(&genesis);
    assert_ne!(
        green,
        (0, 0, 0),
        "repaint must composite the backdrop into the cached framebuffer"
    );

    // Change the backdrop colour while paused and repaint again: the
    // cached frame the host reads must track the new VDP state.
    write_cram(genesis.vdp_mut(), 1, 0x000E); // CRAM[1] = bright red
    genesis.repaint_held_frame();
    assert_ne!(
        top_left(&genesis),
        green,
        "a paused VDP change must reach the cached framebuffer after repaint"
    );
}

/// Helper to create a valid Genesis ROM for testing
fn create_test_rom() -> Vec<u8> {
    // Create 128KB ROM (enough for multiple frames)
    let mut rom = Vec::with_capacity(131072);
    for _ in 0..65536 {
        rom.push(0x4E);
        rom.push(0x71);
    }

    // Reset vectors
    rom[0] = 0x00; // SSP high
    rom[1] = 0xFF;
    rom[2] = 0x00;
    rom[3] = 0x00;
    rom[4] = 0x00; // PC
    rom[5] = 0x00;
    rom[6] = 0x04;
    rom[7] = 0x00;

    // Genesis header
    rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
    rom[0x120..0x138].copy_from_slice(b"TEST ROM                ");
    // ROM $000000-$01FFFF and work RAM $FF0000-$FFFFFF, as a real
    // header gives them. Left as NOP bytes, the RAM field reads as an
    // SRAM range of $4E000001 bytes, which the header parser cuts to its
    // 4 MB limit: every emulator a test builds would allocate 4 MB for it.
    rom[0x1A0..0x1B0].copy_from_slice(&[
        0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0xFF, 0xFF, 0x00, 0xFF, 0x00, 0x00, 0x00, 0xFF, 0xFF,
        0xFF,
    ]);

    // At 0x400, put NOPs followed by infinite loop at the end
    // This ensures execution doesn't go off the rails
    let loop_addr = 0x1FFF0; // Near end of ROM
    rom[loop_addr] = 0x60; // BRA.S
    rom[loop_addr + 1] = 0xFE; // -2 (infinite loop)

    rom
}

#[test]
fn test_reset() {
    let mut genesis = GenesisEmulator::new();
    let rom = create_test_rom();
    genesis.load_rom(&rom).unwrap();

    assert_eq!(genesis.cpu().pc, 0x00000400);
    assert_eq!(genesis.frame_count(), 0);
}

#[test]
fn a_pause_reports_where_the_play_tier_stopped() {
    use retro_core::frame::run_frame_on;
    use retro_core::DebuggerCore;

    let mut genesis = GenesisEmulator::new();
    genesis.load_rom(&create_test_rom()).unwrap();
    genesis.set_execution_mode(ExecutionMode::Running);
    for _ in 0..3 {
        run_frame_on(&mut genesis, ExecutionTier::Play);
    }
    // The play tier writes no CPU state: the debugger still holds the one
    // the reset wrote.
    assert_eq!(genesis.debugger().get_cpu_states()[0].pc, 0x400);

    genesis.set_execution_mode(ExecutionMode::Paused);

    let pc = genesis.cpu().pc;
    assert_ne!(pc, 0x400);
    assert_eq!(genesis.debugger().get_cpu_states()[0].pc, pc);
}

#[test]
fn power_on_reset_zeroes_volatile_ram() {
    let mut genesis = GenesisEmulator::new();
    genesis.load_rom(&create_test_rom()).unwrap();

    // Dirty the volatile RAM the way a running game leaves it.
    genesis.bus.memory_mut().work_ram.as_mut_slice().fill(0xAB);
    genesis.bus.memory_mut().z80_ram.as_mut_slice().fill(0xCD);

    genesis.power_on_reset();

    assert!(
        genesis
            .bus
            .memory()
            .work_ram
            .as_slice()
            .iter()
            .all(|&b| b == 0),
        "power-on reset must zero the 68K work RAM"
    );
    assert!(
        genesis
            .bus
            .memory()
            .z80_ram
            .as_slice()
            .iter()
            .all(|&b| b == 0),
        "power-on reset must zero the Z80 RAM"
    );
    assert_eq!(genesis.frame_count(), 0);
    assert_eq!(
        genesis.cpu().pc,
        0x00000400,
        "cold reset still boots the ROM vector"
    );
}

#[test]
fn soft_reset_preserves_usp_but_power_on_reset_clears_it() {
    // A soft reset re-boots the 68000 (PC/SP/SR from the vector, GP registers
    // cleared) but, faithful to hardware, does NOT clear the user stack
    // pointer, so a host that has been playing and a fresh guest diverge on
    // `cpu68k` and their netplay start commitments differ. A power-on reset
    // rebuilds the machine, clearing it.
    let mut genesis = GenesisEmulator::new();
    genesis.load_rom(&create_test_rom()).unwrap();
    genesis.cpu_mut().usp = 0x00CA_FE00;

    // Soft reset: USP survives (one way two netplay peers diverge).
    EmulatorCore::reset(&mut genesis);
    assert_eq!(
        genesis.cpu().usp,
        0x00CA_FE00,
        "a soft reset must NOT clear the 68K user stack pointer (hardware-faithful)"
    );

    // Power-on reset: the machine is rebuilt, so USP is cleared.
    genesis.cpu_mut().usp = 0x00CA_FE00;
    genesis.power_on_reset();
    assert_eq!(
        genesis.cpu().usp,
        0,
        "power-on reset must clear the 68K user stack pointer"
    );
}

#[test]
fn power_on_reset_converges_cpu_and_ram_across_divergent_history() {
    // The netplay committed-start premise: a host that has been running (its
    // 68K state + RAM dirtied) and a freshly-joined guest must reach a byte-identical
    // frame-0 once both cold-reset. Compares the (tiny) 68K savestate + RAM
    // directly rather than the full `state_hash`; the SNES convergence test
    // exercises the full `state_hash` path.
    let rom = create_test_rom();
    let cpu_state = |g: &GenesisEmulator| bincode::serialize(&g.cpu().to_savestate()).unwrap();

    let mut host = GenesisEmulator::new();
    host.load_rom(&rom).unwrap();
    // Dirty the 68K field a soft reset preserves (USP) plus the volatile RAM.
    host.cpu_mut().usp = 0x00CA_FE00;
    host.bus.memory_mut().work_ram.as_mut_slice().fill(0x5A);
    host.bus.memory_mut().z80_ram.as_mut_slice().fill(0xA5);

    let mut guest = GenesisEmulator::new();
    guest.load_rom(&rom).unwrap();

    // Sanity: the two genuinely diverge before the cold reset.
    assert_ne!(
        cpu_state(&host),
        cpu_state(&guest),
        "divergent 68K state should differ before the cold reset"
    );

    host.power_on_reset();
    guest.power_on_reset();

    assert_eq!(
        cpu_state(&host),
        cpu_state(&guest),
        "cold reset converges the 68K state"
    );
    assert_eq!(
        host.bus.memory().work_ram.as_slice(),
        guest.bus.memory().work_ram.as_slice(),
        "cold reset converges the work RAM"
    );
    assert_eq!(
        host.bus.memory().z80_ram.as_slice(),
        guest.bus.memory().z80_ram.as_slice(),
        "cold reset converges the Z80 RAM"
    );
}

#[test]
fn manifest_override_changes_installed_plan() {
    let mut genesis = GenesisEmulator::new();
    let rom = create_test_rom();

    // Pure detection: a small plain cartridge.
    genesis.load_rom(&rom).unwrap();
    assert_eq!(genesis.bus.cartridge().unwrap().map_plan().name, "plain");
    assert!(genesis
        .bus
        .cartridge()
        .unwrap()
        .coprocessor_label()
        .is_none());
    // The live ROM is remembered so the editor can re-detect it.
    assert_eq!(
        genesis.detected_manifest(genesis.last_rom()).mapping.preset,
        "plain"
    );

    // Override: force the SEGA mapper, then reload.
    let mut over = genesis.detected_manifest(&rom);
    over.mapping.preset = "sega".to_string();
    genesis.set_cartridge_manifest_override(Some(over));
    genesis.load_rom(&rom).unwrap();
    assert_eq!(genesis.bus.cartridge().unwrap().map_plan().name, "sega");
    assert_eq!(
        genesis.bus.cartridge().unwrap().coprocessor_label(),
        Some("SEGA mapper")
    );

    // Clearing the override reverts to pure detection.
    genesis.set_cartridge_manifest_override(None);
    genesis.load_rom(&rom).unwrap();
    assert_eq!(genesis.bus.cartridge().unwrap().map_plan().name, "plain");
}

/// End-to-end SVP memory-controller test through the real 68000 bus: the
/// "SV" tag at ROM 0x1C8 builds a composable SVP cartridge, and the DRAM
/// window, the two cell-arrange views, and the 0xA15000 mailbox all
/// dispatch to the SVP coprocessor. A system savestate round-trips the
/// SVP's shared DRAM and mailbox.
#[test]
fn svp_cartridge_maps_windows_and_savestate_round_trips() {
    let mut genesis = GenesisEmulator::new();
    let mut rom = create_test_rom();
    // The SVP marker in the header.
    rom[0x1C8] = b'S';
    rom[0x1C9] = b'V';
    genesis.load_rom(&rom).unwrap();

    // Detected + installed as a composable SVP cartridge.
    assert_eq!(genesis.bus.cartridge().unwrap().map_plan().name, "svp");
    assert_eq!(
        genesis.bus.cartridge().unwrap().coprocessor_label(),
        Some("SVP")
    );

    // DRAM window (banks 0x30/0x31) round-trips a word.
    genesis.bus.write_word(0x300000, 0x1234);
    assert_eq!(genesis.bus.read_word(0x300000), 0x1234);

    // Cell-arrange view 1 (0x390000) reads the same low DRAM word (the
    // permutation of address 0 is 0).
    assert_eq!(genesis.bus.read_word(0x390000), 0x1234);
    // Cell-arrange view 2 (0x3A0000) likewise.
    assert_eq!(genesis.bus.read_word(0x3A0000), 0x1234);

    // Mailbox: a 68000 byte write to 0xA15000/0xA15001 sets XST; reading
    // it back returns the high then low byte (the handshake round-trips).
    genesis.bus.write(0xA15000, 0xBE);
    genesis.bus.write(0xA15001, 0xEF);
    assert_eq!(genesis.bus.read(0xA15000), 0xBE);
    assert_eq!(genesis.bus.read(0xA15001), 0xEF);
    // Reading the PM0 status register (0xA15004) is a defined access.
    let _ = genesis.bus.read(0xA15004);

    // The SVP coprocessor was exercised by all of the above.
    assert!(genesis.bus.cartridge().unwrap().coprocessor_accesses() > 0);

    // Savestate round-trips the SVP DRAM + mailbox through the typed
    // multi-chip bridge.
    let snapshot = genesis.save_state();
    assert!(!snapshot.is_empty());
    // Clobber DRAM + mailbox after the snapshot.
    genesis.bus.write_word(0x300000, 0x5A5A);
    genesis.bus.write(0xA15000, 0x00);
    genesis.bus.write(0xA15001, 0x00);
    assert_eq!(genesis.bus.read_word(0x300000), 0x5A5A);

    genesis.load_state(&snapshot).unwrap();
    assert_eq!(genesis.bus.read_word(0x300000), 0x1234, "DRAM restored");
    assert_eq!(genesis.bus.read(0xA15000), 0xBE, "XST high restored");
    assert_eq!(genesis.bus.read(0xA15001), 0xEF, "XST low restored");
}

#[test]
fn test_step() {
    let mut genesis = GenesisEmulator::new();
    let rom = create_test_rom();
    genesis.load_rom(&rom).unwrap();

    let initial_pc = genesis.cpu().pc;
    let result = genesis.step();

    assert!(result.cycles > 0);
    assert!(!result.breakpoint_hit);
    assert_ne!(genesis.cpu().pc, initial_pc); // PC should have advanced
}

/// Build a test ROM whose first instruction writes a word to work RAM,
/// then spins. Reset PC is 0x400 (see `create_test_rom`).
///   MOVE.W #$1234, ($00FF0000).L   ; 33 FC 12 34 00 FF 00 00
///   BRA.S  *                       ; 60 FE
fn create_ram_write_rom() -> Vec<u8> {
    let mut rom = create_test_rom();
    let prog = [0x33u8, 0xFC, 0x12, 0x34, 0x00, 0xFF, 0x00, 0x00, 0x60, 0xFE];
    rom[0x400..0x400 + prog.len()].copy_from_slice(&prog);
    rom
}

/// `MOVE.W ($00000500).L, D0` then `BRA.S *`: one read of a ROM table.
const ROM_READ_PROGRAM: [u8; 8] = [0x30, 0x39, 0x00, 0x00, 0x05, 0x00, 0x60, 0xFE];

/// A test ROM running [`ROM_READ_PROGRAM`] from the reset PC (0x400),
/// with the word `$ABCD` at 0x500.
fn create_rom_read_rom() -> Vec<u8> {
    let mut rom = create_test_rom();
    rom[0x400..0x408].copy_from_slice(&ROM_READ_PROGRAM);
    rom[0x500] = 0xAB;
    rom[0x501] = 0xCD;
    rom
}

/// What the `MOVE.W` of [`ROM_READ_PROGRAM`] at `pc` does on the bus:
/// its opcode and two extension words, then the table read.
fn rom_read_accesses(pc: u32) -> Vec<(u32, u8, bool, u16)> {
    vec![
        (pc, 2, false, 0x3039),
        (pc + 2, 2, false, 0x0000),
        (pc + 4, 2, false, 0x0500),
        (0x500, 2, false, 0xABCD),
    ]
}

#[test]
fn the_instruction_access_log_keeps_one_instruction_and_its_pc() {
    let mut genesis = GenesisEmulator::new();
    genesis.load_rom(&create_rom_read_rom()).unwrap();

    // Only the instruction's own accesses: not the step's look at the
    // opcode nor the disassembly it makes for the history.
    genesis.set_instruction_access_logging(true);
    genesis.step();
    let log = genesis.take_instruction_access_log();
    assert_eq!(log.pc, 0x400);
    assert_eq!(log.accesses, rom_read_accesses(0x400));
    assert_eq!(
        genesis.take_instruction_access_log(),
        InstructionAccessLog::default(),
        "taking the log empties it"
    );

    genesis.set_instruction_access_logging(false);
    genesis.step();
    assert_eq!(
        genesis.take_instruction_access_log(),
        InstructionAccessLog::default(),
        "nothing is kept while the log is off"
    );
}

#[test]
fn the_instruction_access_log_leaves_out_the_interrupt_entered_before_it() {
    // Level-4 autovector -> a handler at 0x600 that makes the same read.
    let mut rom = create_rom_read_rom();
    rom[0x70..0x74].copy_from_slice(&[0x00, 0x00, 0x06, 0x00]);
    rom[0x600..0x608].copy_from_slice(&ROM_READ_PROGRAM);
    let mut genesis = GenesisEmulator::new();
    genesis.load_rom(&rom).unwrap();
    genesis.cpu_mut().sr = 0x2000; // supervisor, interrupt mask 0
    genesis.cpu_mut().request_interrupt(4);

    genesis.set_instruction_access_logging(true);
    genesis.step();
    let log = genesis.take_instruction_access_log();
    // The step entered the interrupt (stacking PC and SR, reading the
    // vector) and ran the handler's first instruction; only that
    // instruction's accesses, under its own address, are kept.
    assert_eq!(log.pc, 0x600);
    assert_eq!(log.accesses, rom_read_accesses(0x600));
}

/// Starts a DMA of 16 words from ROM 0x500 to VRAM 0x4000: eight
/// `MOVE.W #imm, ($C00004).L` set registers 1, 15 and 19-23 and the
/// command's first word, and the ninth writes its second word.
fn create_dma_from_rom_rom() -> Vec<u8> {
    let mut rom = create_test_rom();
    let words: [u16; 9] = [
        0x8114, 0x8F02, 0x9310, 0x9400, 0x9580, 0x9602, 0x9700, 0x4000, 0x0081,
    ];
    let mut at = 0x400;
    for w in words {
        let [hi, lo] = w.to_be_bytes();
        rom[at..at + 8].copy_from_slice(&[0x33, 0xFC, hi, lo, 0x00, 0xC0, 0x00, 0x04]);
        at += 8;
    }
    rom[at..at + 2].copy_from_slice(&[0x60, 0xFE]);
    rom
}

#[test]
fn the_instruction_access_log_keeps_the_dma_an_instruction_starts() {
    use crate::vdp::dma::DmaTarget;

    let mut genesis = GenesisEmulator::new();
    genesis.load_rom(&create_dma_from_rom_rom()).unwrap();
    genesis.set_instruction_access_logging(true);
    for _ in 0..8 {
        genesis.step();
        assert!(genesis.take_instruction_access_log().dmas.is_empty());
    }

    genesis.step();
    let log = genesis.take_instruction_access_log();
    assert_eq!(log.pc, 0x440);
    assert_eq!(
        log.dmas,
        vec![MemoryDma {
            source: 0x500,
            words: 16,
            target: DmaTarget::Vram,
            destination: 0x4000,
        }]
    );
    // The VDP read the source itself: it is not a 68000 read.
    assert!(log.accesses.iter().all(|&(addr, ..)| addr != 0x500));
}

fn watch_hits(genesis: &GenesisEmulator, id: BreakpointId) -> u32 {
    genesis
        .debugger_ref()
        .list_breakpoints()
        .iter()
        .find(|b| b.id == id)
        .map(|b| b.hit_count)
        .expect("breakpoint should still exist")
}

#[test]
fn test_write_watchpoint_fires_on_ram_write() {
    use retro_core::DebuggerCore;

    let mut genesis = GenesisEmulator::new();
    genesis.load_rom(&create_ram_write_rom()).unwrap();

    // The 68K is CPU id 0; a write watch on the destination word must
    // trip when the MOVE writes there.
    let id = genesis.debugger().add_breakpoint(Breakpoint {
        address: 0x00FF0000,
        bp_type: BreakpointType::write(),
        cpu_id: Some(0),
        condition: None,
    });

    // First instruction at the reset PC is the MOVE.W.
    // A host step counts what its checks find and stops on none of them.
    genesis.step();
    assert_eq!(
        watch_hits(&genesis, id),
        1,
        "write watchpoint should fire on the RAM write"
    );
}

#[test]
fn test_write_watchpoint_ignores_unrelated_address() {
    use retro_core::DebuggerCore;

    let mut genesis = GenesisEmulator::new();
    genesis.load_rom(&create_ram_write_rom()).unwrap();

    // Watch a word the program never touches.
    let id = genesis.debugger().add_breakpoint(Breakpoint {
        address: 0x00FF1000,
        bp_type: BreakpointType::write(),
        cpu_id: Some(0),
        condition: None,
    });

    // A host step counts what its checks find and stops on none of them.
    genesis.step();
    assert_eq!(
        watch_hits(&genesis, id),
        0,
        "watchpoint on an unwritten address must not fire"
    );
}

#[test]
fn test_read_watchpoint_does_not_fire_on_write() {
    use retro_core::DebuggerCore;

    let mut genesis = GenesisEmulator::new();
    genesis.load_rom(&create_ram_write_rom()).unwrap();

    // A read-only watch on the destination must stay silent when the
    // access is a write (direction is honored independently of address).
    let id = genesis.debugger().add_breakpoint(Breakpoint {
        address: 0x00FF0000,
        bp_type: BreakpointType::read(),
        cpu_id: Some(0),
        condition: None,
    });

    // A host step counts what its checks find and stops on none of them.
    genesis.step();
    assert_eq!(
        watch_hits(&genesis, id),
        0,
        "read watchpoint must not fire on a write access"
    );
}

#[test]
fn test_value_matched_write_watchpoint_fires_on_matching_byte() {
    use retro_core::types::MatchSpec;
    use retro_core::DebuggerCore;

    let mut genesis = GenesisEmulator::new();
    genesis.load_rom(&create_ram_write_rom()).unwrap();

    // MOVE.W #$1234 -> ($00FF0000): high byte 0x12 lands at 0x00FF0000.
    let id = genesis.debugger().add_breakpoint(Breakpoint {
        address: 0x00FF0000,
        bp_type: BreakpointType::write(),
        cpu_id: Some(0),
        condition: None,
    });
    genesis.debugger().set_breakpoint_match_spec(
        id,
        MatchSpec {
            value: Some(0x12),
            ..Default::default()
        },
    );

    // A host step counts what its checks find and stops on none of them.
    genesis.step();
    assert_eq!(
        watch_hits(&genesis, id),
        1,
        "value watchpoint should fire when the written byte matches"
    );
}

#[test]
fn test_value_matched_write_watchpoint_ignores_other_values() {
    use retro_core::types::MatchSpec;
    use retro_core::DebuggerCore;

    let mut genesis = GenesisEmulator::new();
    genesis.load_rom(&create_ram_write_rom()).unwrap();

    // The byte written at 0x00FF0000 is 0x12, not 0x99.
    let id = genesis.debugger().add_breakpoint(Breakpoint {
        address: 0x00FF0000,
        bp_type: BreakpointType::write(),
        cpu_id: Some(0),
        condition: None,
    });
    genesis.debugger().set_breakpoint_match_spec(
        id,
        MatchSpec {
            value: Some(0x99),
            ..Default::default()
        },
    );

    // A host step counts what its checks find and stops on none of them.
    genesis.step();
    assert_eq!(
        watch_hits(&genesis, id),
        0,
        "value watchpoint must stay silent when the written byte differs"
    );
}

#[test]
fn test_value_matched_watchpoint_picks_low_byte_of_word() {
    use retro_core::types::MatchSpec;
    use retro_core::DebuggerCore;

    let mut genesis = GenesisEmulator::new();
    genesis.load_rom(&create_ram_write_rom()).unwrap();

    // Low byte 0x34 lands at 0x00FF0001 (big-endian word write).
    let id = genesis.debugger().add_breakpoint(Breakpoint {
        address: 0x00FF0001,
        bp_type: BreakpointType::write(),
        cpu_id: Some(0),
        condition: None,
    });
    genesis.debugger().set_breakpoint_match_spec(
        id,
        MatchSpec {
            value: Some(0x34),
            ..Default::default()
        },
    );

    // A host step counts what its checks find and stops on none of them.
    genesis.step();
    assert_eq!(
        watch_hits(&genesis, id),
        1,
        "value watchpoint should match the low byte of a word write"
    );
}

/// MOVE.L #$12345678, ($00FF0000).L ; BRA.S *: writes a full 32-bit
/// value to work RAM so multi-byte watchpoints have something to
/// assemble.
fn create_ram_write_long_rom() -> Vec<u8> {
    let mut rom = create_test_rom();
    let prog = [
        0x23u8, 0xFC, 0x12, 0x34, 0x56, 0x78, 0x00, 0xFF, 0x00, 0x00, 0x60, 0xFE,
    ];
    rom[0x400..0x400 + prog.len()].copy_from_slice(&prog);
    rom
}

#[test]
fn test_sized_32bit_watchpoint_fires_on_assembled_long() {
    use retro_core::types::MatchSpec;
    use retro_core::DebuggerCore;

    let mut genesis = GenesisEmulator::new();
    genesis.load_rom(&create_ram_write_long_rom()).unwrap();

    // A 32-bit watch at 0xFF0000 fires when the assembled big-endian
    // long at 0xFF0000..0xFF0003 equals the target, even though the
    // MOVE.L writes it as two separate word stores.
    let id = genesis.debugger().add_breakpoint(Breakpoint {
        address: 0x00FF0000,
        bp_type: BreakpointType::write(),
        cpu_id: Some(0),
        condition: None,
    });
    genesis.debugger().set_breakpoint_match_spec(
        id,
        MatchSpec {
            value: Some(0x12345678),
            value_size: 4,
            ..Default::default()
        },
    );

    // A host step counts what its checks find and stops on none of them.
    genesis.step();
    // The four covered bytes are all written this instruction, but the
    // watch must count a single hit, not one per byte.
    assert_eq!(
        watch_hits(&genesis, id),
        1,
        "32-bit watch should fire when the assembled long matches"
    );
}

#[test]
fn test_sized_32bit_watchpoint_ignores_other_values() {
    use retro_core::types::MatchSpec;
    use retro_core::DebuggerCore;

    let mut genesis = GenesisEmulator::new();
    genesis.load_rom(&create_ram_write_long_rom()).unwrap();

    let id = genesis.debugger().add_breakpoint(Breakpoint {
        address: 0x00FF0000,
        bp_type: BreakpointType::write(),
        cpu_id: Some(0),
        condition: None,
    });
    genesis.debugger().set_breakpoint_match_spec(
        id,
        MatchSpec {
            value: Some(0xDEADBEEF),
            value_size: 4,
            ..Default::default()
        },
    );

    // A host step counts what its checks find and stops on none of them.
    genesis.step();
    assert_eq!(
        watch_hits(&genesis, id),
        0,
        "32-bit watch must stay silent when the assembled long differs"
    );
}

#[test]
fn test_sized_16bit_watchpoint_fires_via_consecutive_byte() {
    use retro_core::types::MatchSpec;
    use retro_core::DebuggerCore;

    let mut genesis = GenesisEmulator::new();
    // MOVE.W #$1234 -> ($00FF0000): bytes 0x12 0x34 at 0xFF0000-1.
    genesis.load_rom(&create_ram_write_rom()).unwrap();

    // A 16-bit watch keyed at 0xFF0000 with value 0x1234 fires even
    // when only the low byte 0xFF0001 is named as the base? No, the
    // base is 0xFF0000 and width 2 covers 0xFF0000..0xFF0001.
    let id = genesis.debugger().add_breakpoint(Breakpoint {
        address: 0x00FF0000,
        bp_type: BreakpointType::write(),
        cpu_id: Some(0),
        condition: None,
    });
    genesis.debugger().set_breakpoint_match_spec(
        id,
        MatchSpec {
            value: Some(0x1234),
            value_size: 2,
            ..Default::default()
        },
    );

    // A host step counts what its checks find and stops on none of them.
    genesis.step();
    assert_eq!(
        watch_hits(&genesis, id),
        1,
        "16-bit watch should fire when the assembled word matches"
    );
}

#[test]
fn test_conditional_execute_breakpoint_fires_when_expression_true() {
    use retro_core::types::MatchSpec;
    use retro_core::DebuggerCore;

    let mut genesis = GenesisEmulator::new();
    genesis.load_rom(&create_ram_write_rom()).unwrap();

    // Execute breakpoint at the reset PC, gated on a condition that is
    // true at the fetch (pc == 0x400), proves the expression context
    // sees live registers through the real step path.
    let id = genesis.debugger().add_breakpoint(Breakpoint {
        address: 0x00000400,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    });
    genesis.debugger().set_breakpoint_match_spec(
        id,
        MatchSpec {
            expr: Some("pc == 0x400".to_string()),
            ..Default::default()
        },
    );

    // A host step runs whatever breakpoint sits at its instruction: the
    // check is the debugger's tier's, which stops before the instruction.
    genesis.set_execution_mode(ExecutionMode::Running);
    genesis.run_frame();
    assert_eq!(
        watch_hits(&genesis, id),
        1,
        "conditional execute breakpoint should fire when its expression holds"
    );
    assert_eq!(genesis.cpu().pc, 0x400, "it stops before the instruction");
}

#[test]
fn test_conditional_execute_breakpoint_silent_when_expression_false() {
    use retro_core::types::MatchSpec;
    use retro_core::DebuggerCore;

    let mut genesis = GenesisEmulator::new();
    genesis.load_rom(&create_ram_write_rom()).unwrap();

    // Same breakpoint, but the condition can never hold at this pc.
    let id = genesis.debugger().add_breakpoint(Breakpoint {
        address: 0x00000400,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    });
    genesis.debugger().set_breakpoint_match_spec(
        id,
        MatchSpec {
            expr: Some("pc == 0x500".to_string()),
            ..Default::default()
        },
    );

    genesis.set_execution_mode(ExecutionMode::Running);
    genesis.run_frame();
    assert_eq!(
        watch_hits(&genesis, id),
        0,
        "conditional execute breakpoint must stay silent when its expression is false"
    );
    assert_eq!(genesis.frame_count(), 1, "nothing stopped the frame");
}

#[test]
fn test_conditional_watchpoint_reads_memory_in_expression() {
    use retro_core::types::MatchSpec;
    use retro_core::DebuggerCore;

    let mut genesis = GenesisEmulator::new();
    // MOVE.W #$1234 -> ($00FF0000): byte 0x12 lands at 0xFF0000.
    genesis.load_rom(&create_ram_write_rom()).unwrap();

    // Write watch whose expression reads the just-written byte back from
    // memory. The peek happens after the write, so [0xFF0000].b == 0x12.
    let id = genesis.debugger().add_breakpoint(Breakpoint {
        address: 0x00FF0000,
        bp_type: BreakpointType::write(),
        cpu_id: Some(0),
        condition: None,
    });
    genesis.debugger().set_breakpoint_match_spec(
        id,
        MatchSpec {
            expr: Some("[0xFF0000].b == 0x12".to_string()),
            ..Default::default()
        },
    );

    // A host step counts what its checks find and stops on none of them.
    genesis.step();
    assert_eq!(
        watch_hits(&genesis, id),
        1,
        "conditional watchpoint should fire when its memory expression holds"
    );
}

#[test]
fn test_conditional_watchpoint_silent_when_expression_false() {
    use retro_core::types::MatchSpec;
    use retro_core::DebuggerCore;

    let mut genesis = GenesisEmulator::new();
    genesis.load_rom(&create_ram_write_rom()).unwrap();

    let id = genesis.debugger().add_breakpoint(Breakpoint {
        address: 0x00FF0000,
        bp_type: BreakpointType::write(),
        cpu_id: Some(0),
        condition: None,
    });
    genesis.debugger().set_breakpoint_match_spec(
        id,
        MatchSpec {
            // The byte at 0xFF0000 is 0x12, never 0x99.
            expr: Some("[0xFF0000].b == 0x99".to_string()),
            ..Default::default()
        },
    );

    // A host step counts what its checks find and stops on none of them.
    genesis.step();
    assert_eq!(
        watch_hits(&genesis, id),
        0,
        "conditional watchpoint must stay silent when its memory expression is false"
    );
}

#[test]
fn test_run_frame() {
    use retro_core::DebuggerCore;

    let mut genesis = GenesisEmulator::new();
    let rom = create_test_rom();
    genesis.load_rom(&rom).unwrap();

    // Set to running mode
    genesis
        .debugger()
        .set_execution_mode(ExecutionMode::Running);

    assert_eq!(genesis.frame_count(), 0);

    genesis.run_frame();

    assert_eq!(genesis.frame_count(), 1);
    // CPU should have executed many cycles
    assert!(genesis.cpu().total_cycles() > 0);
}

#[test]
fn test_multiple_frames() {
    use retro_core::DebuggerCore;

    let mut genesis = GenesisEmulator::new();
    let rom = create_test_rom();
    genesis.load_rom(&rom).unwrap();

    // Set to running mode
    genesis
        .debugger()
        .set_execution_mode(ExecutionMode::Running);

    for i in 1..=10 {
        genesis.run_frame();
        assert_eq!(genesis.frame_count(), i);
    }
}

#[test]
fn test_region_change() {
    let mut genesis = GenesisEmulator::new();
    assert_eq!(genesis.region(), VideoRegion::Ntsc);

    genesis.set_region(VideoRegion::Pal);
    assert_eq!(genesis.region(), VideoRegion::Pal);
}

#[test]
fn display_info_refresh_rate_tracks_set_region() {
    let mut genesis = GenesisEmulator::new();
    assert!((genesis.get_display_info().refresh_rate - 59.9227).abs() < 1e-4);
    genesis.set_region(VideoRegion::Pal);
    assert!((genesis.get_display_info().refresh_rate - 49.7015).abs() < 1e-4);
    genesis.set_region(VideoRegion::Ntsc);
    assert!((genesis.get_display_info().refresh_rate - 59.9227).abs() < 1e-4);
}

fn test_rom_with_regions(regions: &[u8]) -> Vec<u8> {
    let mut rom = create_test_rom();
    // Header region field: three ASCII slots at $1F0..$1F3, space-padded.
    rom[0x1F0..0x1F3].copy_from_slice(b"   ");
    rom[0x1F0..0x1F0 + regions.len()].copy_from_slice(regions);
    rom
}

#[test]
fn load_rom_europe_only_header_selects_pal() {
    for header in [&b"E"[..], &b"8"[..]] {
        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&test_rom_with_regions(header)).unwrap();
        assert_eq!(genesis.region(), VideoRegion::Pal, "header {header:?}");
        assert!((genesis.get_display_info().refresh_rate - 49.7015).abs() < 1e-4);
    }
}

#[test]
fn load_rom_multi_region_header_stays_ntsc() {
    for header in [&b"JUE"[..], &b"UE"[..], &b"U"[..], &b"   "[..]] {
        let mut genesis = GenesisEmulator::new();
        genesis.load_rom(&test_rom_with_regions(header)).unwrap();
        assert_eq!(genesis.region(), VideoRegion::Ntsc, "header {header:?}");
        assert!((genesis.get_display_info().refresh_rate - 59.9227).abs() < 1e-4);
    }
}

#[test]
fn load_rom_header_region_replaces_a_stale_explicit_region() {
    // A PAL choice left over from a previous session must not leak
    // onto the next cartridge: the header decides on every load.
    let mut genesis = GenesisEmulator::new();
    genesis.set_region(VideoRegion::Pal);
    genesis.load_rom(&test_rom_with_regions(b"JUE")).unwrap();
    assert_eq!(genesis.region(), VideoRegion::Ntsc);
}

#[test]
fn power_on_reset_keeps_an_explicit_region_over_the_header() {
    // The netplay handshake sets the region after load; a cold boot
    // re-applies that override on top of the header's choice.
    let mut genesis = GenesisEmulator::new();
    genesis.load_rom(&test_rom_with_regions(b"E")).unwrap();
    assert_eq!(genesis.region(), VideoRegion::Pal);
    genesis.set_region(VideoRegion::Ntsc);
    genesis.power_on_reset();
    assert_eq!(genesis.region(), VideoRegion::Ntsc);
    assert!((genesis.get_display_info().refresh_rate - 59.9227).abs() < 1e-4);
}

#[test]
fn test_cartridge_audio_mixing() {
    // No Genesis cartridge provides audio (`audio()` is `None`), so this
    // checks the console mix alone: non-empty and in range.
    use retro_core::DebuggerCore;

    let mut genesis = GenesisEmulator::new();
    let rom = create_test_rom();
    genesis.load_rom(&rom).unwrap();

    genesis
        .debugger()
        .set_execution_mode(ExecutionMode::Running);

    // Run a frame to generate audio
    genesis.run_frame();

    // Audio buffer should have samples (PSG + YM2612)
    let audio = genesis.get_audio_buffer();
    assert!(
        !audio.is_empty(),
        "Audio buffer should not be empty after a frame"
    );

    // All samples should be in valid range (-1.0 to 1.0)
    for sample in &audio {
        assert!(
            (-1.0..=1.0).contains(sample),
            "Audio sample out of range: {}",
            sample
        );
    }

    // TODO: check the mix with a cartridge that has audio of its own.
}

#[test]
fn test_z80_integration() {
    let mut genesis = GenesisEmulator::new();

    // Z80 should start in reset
    assert!(genesis.z80_is_reset());

    // Z80 PC should be 0x0000
    assert_eq!(genesis.z80().pc, 0x0000);

    // Take Z80 out of reset
    genesis.set_z80_reset(false);
    assert!(!genesis.z80_is_reset());

    // Z80 should have default state
    assert_eq!(genesis.z80().sp, 0xFFFF);
    assert_eq!(genesis.z80().a, 0);
}

#[test]
fn test_z80_bus_request() {
    let mut genesis = GenesisEmulator::new();

    // Initially no bus request
    assert!(!genesis.z80_bus_requested());

    // Request Z80 bus
    genesis.set_z80_bus_request(true);
    assert!(genesis.z80_bus_requested());

    // Release bus
    genesis.set_z80_bus_request(false);
    assert!(!genesis.z80_bus_requested());
}

#[test]
fn test_z80_reset() {
    let mut genesis = GenesisEmulator::new();

    // Take Z80 out of reset and modify state
    genesis.set_z80_reset(false);
    genesis.z80_mut().a = 0x42;
    genesis.z80_mut().pc = 0x1234;

    // Reset should clear state
    genesis.reset();
    assert!(genesis.z80_is_reset());
    assert_eq!(genesis.z80().pc, 0x0000);
    assert_eq!(genesis.z80().a, 0);
}

#[test]
fn load_rom_leaves_the_reset_state_in_the_debugger() {
    use retro_core::DebuggerCore;

    // No step and no frame: the platform's debugger reads the state the
    // moment a project's Build & Run has loaded the ROM.
    let mut genesis = GenesisEmulator::new();
    genesis.load_rom(&create_test_rom()).unwrap();

    let cpu_states = genesis.debugger().get_cpu_states();
    assert_eq!(cpu_states.len(), 2);
    assert_eq!(cpu_states[0].cpu_id, 0);
    assert_eq!(cpu_states[0].pc, 0x0000_0400);
    assert_eq!(cpu_states[1].cpu_id, 1);
}

#[test]
fn test_debugger_shows_both_cpus() {
    use retro_core::DebuggerCore;

    let mut genesis = GenesisEmulator::new();
    let rom = create_test_rom();
    genesis.load_rom(&rom).unwrap();

    // Update debugger state to populate CPU states
    genesis.refresh_debugger_state();

    // Get CPU states from debugger
    let cpu_states = genesis.debugger().get_cpu_states();

    // Should have 2 CPUs
    assert_eq!(cpu_states.len(), 2);

    // First CPU should be 68K (ID 0)
    assert_eq!(cpu_states[0].cpu_id, 0);
    assert!(!cpu_states[0].registers.is_empty());

    // Second CPU should be Z80 (ID 1)
    assert_eq!(cpu_states[1].cpu_id, 1);
    assert!(!cpu_states[1].registers.is_empty());

    // Check Z80 has expected registers
    let z80_regs: Vec<String> = cpu_states[1]
        .registers
        .iter()
        .map(|r| r.name.clone())
        .collect();

    assert!(z80_regs.contains(&"A".to_string()));
    assert!(z80_regs.contains(&"PC".to_string()));
    assert!(z80_regs.contains(&"SP".to_string()));
}

// --------------------------------------------------------------------
// EmulatorCore::set_input: touch overlay / virtual-input bridge
//
// The JS-side touch overlay calls `setInput(player, mask)`. `set_input`
// routes `InputState.buttons` through `InputManager` as a direct
// override that OR-merges with the assigned provider's state, so touch +
// keyboard + gamepad all stack onto the same port and a press reaches
// the I/O ports.
// --------------------------------------------------------------------

#[test]
fn test_set_input_drives_port_state() {
    use retro_core::{EmulatorCore, InputState};

    let mut genesis = GenesisEmulator::new();
    // Genesis A is bit 6 (the layout `set_port_button_state` documents).
    genesis.set_input(0, InputState { buttons: 1 << 6 });

    // The port state reports A as pressed.
    assert_eq!(
        genesis.input_manager().get_port_state(0),
        1 << 6,
        "set_input must drive get_port_state: the touch overlay relies on this path",
    );
}

#[test]
fn test_set_input_combines_with_keyboard_provider() {
    use retro_core::{EmulatorCore, InputState};

    let mut genesis = GenesisEmulator::new();

    // Keyboard arrow-up presses Genesis UP (bit 0) via the default
    // keyboard mapping in `InputManager::new()`.
    genesis
        .input_manager_mut()
        .update_keyboard("ArrowUp".to_string(), true);
    let keyboard_only = genesis.input_manager().get_port_state(0);
    assert_ne!(keyboard_only, 0, "keyboard alone must produce input");

    // Touch overlay simulates A (bit 6). Both must be visible to the
    // I/O ports: there's no reason a player on mobile shouldn't be
    // allowed to use both at once if they happen to plug in a keyboard.
    genesis.set_input(0, InputState { buttons: 1 << 6 });
    assert_eq!(
        genesis.input_manager().get_port_state(0),
        keyboard_only | (1 << 6),
        "set_input must OR with the keyboard provider, not replace it",
    );

    // Releasing the touch (mask = 0) leaves the keyboard alone.
    genesis.set_input(0, InputState { buttons: 0 });
    assert_eq!(genesis.input_manager().get_port_state(0), keyboard_only);
}

#[test]
fn test_set_input_player_2_targets_port_1() {
    use retro_core::{EmulatorCore, InputState};

    let mut genesis = GenesisEmulator::new();
    genesis.set_input(1, InputState { buttons: 1 << 7 }); // START

    assert_eq!(genesis.input_manager().get_port_state(0), 0);
    assert_eq!(genesis.input_manager().get_port_state(1), 1 << 7);
}

#[test]
fn read_memory_debug_leaves_the_vdp_ports_alone() {
    let mut genesis = GenesisEmulator::new();
    let bus = genesis.bus_mut();
    bus.write_word(0xC00004, 0x8F02); // auto-increment 2
                                      // The first half of a VRAM write command at address 0: the VDP now
                                      // waits for the second word.
    bus.write_word(0xC00004, 0x4000);

    // A memory view over the ports must not cancel that wait, as a CPU
    // read of the control port would.
    let view = genesis.read_memory_debug(0xC00000, 0x20);
    assert!(view.iter().all(|&b| b == 0));

    let bus = genesis.bus_mut();
    bus.write_word(0xC00004, 0x0000);
    bus.write_word(0xC00000, 0x1234);
    assert_eq!(&genesis.vdp().get_vram()[0..2], &[0x12, 0x34]);
}

#[test]
fn disassembly_views_leave_the_vdp_ports_alone() {
    let mut genesis = GenesisEmulator::new();
    let bus = genesis.bus_mut();
    bus.write_word(0xC00004, 0x8F02); // auto-increment 2
    bus.write_word(0xC00004, 0x4000); // first half of a VRAM write at 0

    // The debugger's disassembly, pointed at the ports, must not read
    // them as the 68000 would.
    let rows = genesis.disassemble_at(0xC00000, 8);
    assert_eq!(rows.len(), 8);
    let rows = genesis.disassemble_around_pc(0xC00004, 0, 8);
    assert_eq!(rows.len(), 8);

    let bus = genesis.bus_mut();
    bus.write_word(0xC00004, 0x0000);
    bus.write_word(0xC00000, 0x1234);
    assert_eq!(&genesis.vdp().get_vram()[0..2], &[0x12, 0x34]);
}
