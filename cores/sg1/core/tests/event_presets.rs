//! Event breakpoints on writes to video memory: a transfer finished (by the
//! memory it wrote), a palette colour written, a transfer into the sprite
//! table, a tile rewritten. Each stops after the instruction that made the
//! write, on both debugger frame loops; with none set the frame runs on the
//! loop a player runs.
//!
//! The program below makes each kind of write once, in order, with the
//! display off: a transfer from ROM into VRAM, a palette store, a transfer
//! into the sprite table, a store of the bytes a tile already holds, a store
//! that changes them, transfers into CRAM and into VSRAM, a copy within
//! VRAM, and, once the beam is in the vertical blank (where a fill lands
//! all at once), a fill.

mod helpers;

use helpers::M68kProgram;
use retro_core::types::{Breakpoint, BreakpointType, BREAKPOINT_PARAM_ANY};
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode, ExecutionTier};
use sg1_core::debugger::genesis_events;
use sg1_core::GenesisEmulator;

/// The program of the module's notes.
fn writes_program() -> M68kProgram {
    let mut program = M68kProgram::new(0x4000, 0x200);
    #[rustfmt::skip]
    program.org(0x200).words(&[
        0x46FC, 0x2700,                 // MOVE.W #$2700,SR
        0x41F9, 0x00C0, 0x0004,         // LEA $C00004,A0: control port
        0x43F9, 0x00C0, 0x0000,         // LEA $C00000,A1: data port
        0x30BC, 0x8114,                 // register 1: display off, transfers on, mode 5
        0x30BC, 0x8C81,                 // register 12: 40 cells
        0x30BC, 0x8F02,                 // register 15: auto-increment 2
        0x30BC, 0x8578,                 // register 5: sprites at $F000
        // 16 words from ROM $2000 to VRAM $0000 (tile 0).
        0x30BC, 0x9310, 0x30BC, 0x9400,
        0x30BC, 0x9500, 0x30BC, 0x9610, 0x30BC, 0x9700,
        0x20BC, 0x4000, 0x0080,         // MOVE.L #$40000080,(A0)
    ]).label("after_vram_transfer").words(&[
        0x20BC, 0xC00A, 0x0000,         // CRAM colour 5
        0x32BC, 0x0E00,                 // MOVE.W #$0E00,(A1)
    ]).label("after_palette_store").words(&[
        // 8 words from ROM $2000 into the sprite table at $F000 (tile 1920).
        0x30BC, 0x9308, 0x30BC, 0x9400,
        0x30BC, 0x9500, 0x30BC, 0x9610, 0x30BC, 0x9700,
        0x20BC, 0x7000, 0x0083,         // VRAM $F000 by DMA
    ]).label("after_sprite_table_transfer").words(&[
        // Tile 40 ($0500): its own zeros again, then a new word.
        0x20BC, 0x4500, 0x0000,
        0x32BC, 0x0000,
    ]).label("after_same_store").words(&[
        0x20BC, 0x4500, 0x0000,
        0x32BC, 0x1234,
    ]).label("after_tile_store").words(&[
        // 4 words from ROM $2100 into CRAM colours 8 to 11.
        0x30BC, 0x9304, 0x30BC, 0x9400,
        0x30BC, 0x9580, 0x30BC, 0x9610, 0x30BC, 0x9700,
        0x20BC, 0xC010, 0x0080,
    ]).label("after_cram_transfer").words(&[
        // 2 words from ROM $2200 into VSRAM.
        0x30BC, 0x9302, 0x30BC, 0x9400,
        0x30BC, 0x9500, 0x30BC, 0x9611, 0x30BC, 0x9700,
        0x20BC, 0x4000, 0x0090,
    ]).label("after_vsram_transfer").words(&[
        // 16 bytes copied from VRAM $0000 to $0700 (tile 56), increment 1.
        0x30BC, 0x8F01,
        0x30BC, 0x9310, 0x30BC, 0x9400,
        0x30BC, 0x9500, 0x30BC, 0x9600,
        0x30BC, 0x97C0,
        0x20BC, 0x0700, 0x00C0,         // VRAM $0700 by copy
    ]).label("after_copy").words(&[
        0x30BC, 0x9310, 0x30BC, 0x9400,
        0x30BC, 0x9780,                 // the next transfer is a fill
    ]).label("wait_for_vertical_blank").words(&[
        0x1039, 0x00C0, 0x0008,         // MOVE.B ($C00008).L,D0: the line
        0x0C00, 0x00E0,                 // CMPI.B #$E0,D0
    ]).branch(0x6500, "wait_for_vertical_blank").words(&[  // BCS
        // 16 cells of $55 at VRAM $0600 (tile 48).
        0x20BC, 0x4600, 0x0080,         // VRAM $0600 by DMA
        0x32BC, 0x5500,                 // MOVE.W #$5500,(A1): starts the fill
    ]).label("after_fill_store").label("idle").branch(0x6000, "idle");
    let bytes: Vec<u8> = (0..32u8).map(|i| 0x10 + i).collect();
    program.data(0x2000, &bytes);
    program.data(0x2100, &[0x00, 0x02, 0x00, 0x04, 0x00, 0x06, 0x00, 0x08]);
    program.data(0x2200, &[0x00, 0x10, 0x00, 0x20]);
    program
}

/// Run `rom` with an event breakpoint on `code` and `param`, on
/// `frame_loop`, resuming after each stop, and return the program counter
/// at each stop.
fn stops(rom: &[u8], code: u32, param: u32, frame_loop: ExecutionTier) -> Vec<u32> {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(rom).expect("the program loads");
    emu.set_execution_mode(ExecutionMode::Running);
    emu.debugger().add_breakpoint(Breakpoint {
        address: 0,
        bp_type: BreakpointType::Event { code, param },
        cpu_id: None,
        condition: None,
    });
    assert_eq!(emu.debugger().execution_tier(), ExecutionTier::LightDebug);
    let mut pcs = Vec::new();
    for _ in 0..12 {
        emu.set_execution_mode(ExecutionMode::Running);
        match frame_loop {
            ExecutionTier::LightDebug => emu.run_frame_debug_light(),
            _ => EmulatorCore::run_frame(&mut emu),
        }
        if emu.debugger().get_execution_mode() == ExecutionMode::Paused {
            pcs.push(emu.cpu().pc);
        }
    }
    pcs
}

#[test]
fn each_event_stops_after_the_instruction_that_made_its_write() {
    let mut program = writes_program();
    let rom = program.build();
    let at = |name| program.address(name);
    use genesis_events::*;
    let any = BREAKPOINT_PARAM_ANY;
    let cases: Vec<(&str, u32, u32, Vec<u32>)> = vec![
        (
            "a transfer into VRAM finished",
            TRANSFER_COMPLETE,
            0,
            vec![
                at("after_vram_transfer"),
                at("after_sprite_table_transfer"),
                at("after_copy"),
                at("after_fill_store"),
            ],
        ),
        (
            "a transfer into CRAM finished",
            TRANSFER_COMPLETE,
            1,
            vec![at("after_cram_transfer")],
        ),
        (
            "a transfer into VSRAM finished",
            TRANSFER_COMPLETE,
            2,
            vec![at("after_vsram_transfer")],
        ),
        (
            "any transfer finished",
            TRANSFER_COMPLETE,
            any,
            vec![
                at("after_vram_transfer"),
                at("after_sprite_table_transfer"),
                at("after_cram_transfer"),
                at("after_vsram_transfer"),
                at("after_copy"),
                at("after_fill_store"),
            ],
        ),
        (
            "colour 5 written",
            PALETTE_WRITE,
            5,
            vec![at("after_palette_store")],
        ),
        (
            "colour 9 written by a transfer",
            PALETTE_WRITE,
            9,
            vec![at("after_cram_transfer")],
        ),
        (
            "any colour written",
            PALETTE_WRITE,
            any,
            vec![at("after_palette_store"), at("after_cram_transfer")],
        ),
        ("colour 20, never written", PALETTE_WRITE, 20, vec![]),
        (
            "a transfer into the sprite table",
            SPRITE_TABLE_TRANSFER,
            0,
            vec![at("after_sprite_table_transfer")],
        ),
        (
            "tile 0 rewritten by a transfer",
            TILE_REWRITTEN,
            0,
            vec![at("after_vram_transfer")],
        ),
        (
            "tile 40 rewritten, not by its own bytes again",
            TILE_REWRITTEN,
            40,
            vec![at("after_tile_store")],
        ),
        (
            "tile 48 rewritten by a fill",
            TILE_REWRITTEN,
            48,
            vec![at("after_fill_store")],
        ),
        (
            "tile 56 rewritten by a copy",
            TILE_REWRITTEN,
            56,
            vec![at("after_copy")],
        ),
        ("tile 41, never written", TILE_REWRITTEN, 41, vec![]),
        (
            "any tile rewritten",
            TILE_REWRITTEN,
            any,
            vec![
                at("after_vram_transfer"),
                at("after_sprite_table_transfer"),
                at("after_tile_store"),
                at("after_copy"),
                at("after_fill_store"),
            ],
        ),
    ];
    for frame_loop in [ExecutionTier::LightDebug, ExecutionTier::FullDebug] {
        for (what, code, param, expected) in &cases {
            assert_eq!(
                &stops(&rom, *code, *param, frame_loop),
                expected,
                "{what} on {frame_loop:?}"
            );
        }
    }
}

#[test]
fn a_stop_is_counted_on_its_breakpoint_and_a_disabled_one_does_not_stop() {
    let rom = writes_program().build();
    let mut emu = GenesisEmulator::new();
    emu.load_rom(&rom).expect("the program loads");
    let id = emu.debugger().add_breakpoint(Breakpoint {
        address: 0,
        bp_type: BreakpointType::Event {
            code: genesis_events::PALETTE_WRITE,
            param: BREAKPOINT_PARAM_ANY,
        },
        cpu_id: None,
        condition: None,
    });
    emu.set_execution_mode(ExecutionMode::Running);
    emu.run_frame_debug_light();
    assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::Paused);
    let hits = |emu: &mut GenesisEmulator| {
        emu.debugger()
            .list_breakpoints()
            .into_iter()
            .find(|bp| bp.id == id)
            .unwrap()
            .hit_count
    };
    assert_eq!(hits(&mut emu), 1);

    // Disabled, the rest of the program runs through.
    emu.debugger().enable_breakpoint(id, false);
    for _ in 0..4 {
        emu.set_execution_mode(ExecutionMode::Running);
        emu.run_frame_debug_light();
        assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::Running);
    }
    assert_eq!(hits(&mut emu), 1);
}

#[test]
fn with_no_breakpoint_the_frame_runs_on_the_fast_loop_and_raises_nothing() {
    let rom = writes_program().build();
    let mut emu = GenesisEmulator::new();
    emu.load_rom(&rom).expect("the program loads");
    emu.set_execution_mode(ExecutionMode::Running);
    assert_eq!(emu.debugger().execution_tier(), ExecutionTier::Fast);
    let id = emu.debugger().add_breakpoint(Breakpoint {
        address: 0,
        bp_type: BreakpointType::Event {
            code: genesis_events::TILE_REWRITTEN,
            param: BREAKPOINT_PARAM_ANY,
        },
        cpu_id: None,
        condition: None,
    });
    assert_eq!(emu.debugger().execution_tier(), ExecutionTier::LightDebug);

    // The loop a player runs checks no event, even with one set: nothing
    // stops it and the video chip raises nothing.
    for _ in 0..3 {
        emu.run_frame_fast();
        assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::Running);
        assert!(emu
            .vdp_mut()
            .raised_events_mut()
            .is_none_or(|raised| raised.is_empty()));
    }
    emu.debugger().remove_breakpoint(id);
    assert_eq!(emu.debugger().execution_tier(), ExecutionTier::Fast);
}

#[test]
fn a_step_the_host_asks_for_stops_on_the_write_it_makes() {
    let mut program = writes_program();
    let rom = program.build();
    let mut emu = GenesisEmulator::new();
    emu.load_rom(&rom).expect("the program loads");
    emu.debugger().add_breakpoint(Breakpoint {
        address: 0,
        bp_type: BreakpointType::Event {
            code: genesis_events::PALETTE_WRITE,
            param: 5,
        },
        cpu_id: None,
        condition: None,
    });
    let target = program.address("after_palette_store");
    let mut hit_at = None;
    for _ in 0..200 {
        let result = EmulatorCore::step(&mut emu);
        if result.breakpoint_hit {
            hit_at = Some(emu.cpu().pc);
            break;
        }
    }
    assert_eq!(hit_at, Some(target));
}
