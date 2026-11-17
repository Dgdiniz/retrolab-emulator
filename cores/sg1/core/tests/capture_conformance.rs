//! The capture contract's conformance suite (`retro_core::capture::conformance`)
//! run on the Mega Drive core, with a program of its own.
//!
//! The program draws plane A, a pattern of tiles with holes where plane B
//! shows through, and four sprites of two cells by two that overlap. Every
//! frame its vertical interrupt moves the sprites and scrolls plane A, sends
//! the sprite table and one of two faces of tile 1 by transfer, stores a
//! colour, plane A's vertical scroll and its horizontal scroll through the
//! ports, moves a PSG tone, and points the sprite table at an empty one in
//! one frame of four. Its horizontal interrupt, every hundred lines, moves
//! plane A's horizontal scroll through the port again, part of the way down
//! the picture.

mod helpers;

use helpers::M68kProgram;
use retro_core::capture::conformance::{CoreUnderTest, VideoMemoryEdit};
use retro_core::debug::DebugEvent;
use retro_core::types::{Breakpoint, BreakpointType};
use retro_core::video::VideoDebugger;
use retro_core::{EmulatorCore, ExecutionMode};
use sg1_core::debugger::GenesisEvent;
use sg1_core::vdp::LAYER_SPRITES;
use sg1_core::GenesisEmulator;

/// Where the vertical interrupt's handler starts.
const VERTICAL_BLANK_HANDLER: u32 = 0x500;

/// The program of the module's notes.
fn program() -> Vec<u8> {
    let mut program = M68kProgram::new(0x8000, 0x200);
    program.vector(28, 0x400).vector(30, VERTICAL_BLANK_HANDLER);

    #[rustfmt::skip]
    program.org(0x200).words(&[
        0x46FC, 0x2700,                 // MOVE.W #$2700,SR
        0x41F9, 0x00C0, 0x0004,         // LEA $C00004,A0: control port
        0x43F9, 0x00C0, 0x0000,         // LEA $C00000,A1: data port
        0x30BC, 0x8014,                 // register 0: horizontal interrupt on
        0x30BC, 0x8134,                 // register 1: display off, vertical interrupt, DMA, mode 5
        0x30BC, 0x8230,                 // register 2: plane A at $C000
        0x30BC, 0x8334,                 // register 3: window at $D000
        0x30BC, 0x8407,                 // register 4: plane B at $E000
        0x30BC, 0x8578,                 // register 5: sprites at $F000
        0x30BC, 0x8700,                 // register 7: backdrop colour 0
        0x30BC, 0x8A63,                 // register 10: a horizontal interrupt every 100 lines
        0x30BC, 0x8B00,                 // register 11: whole-screen scrolls
        0x30BC, 0x8C81,                 // register 12: 40 cells
        0x30BC, 0x8D3F,                 // register 13: horizontal scroll at $FC00
        0x30BC, 0x8F02,                 // register 15: auto-increment 2
        0x30BC, 0x9001,                 // register 16: planes of 64 by 32 cells
        // Tiles 1 to 8: 128 words from $1000 to VRAM $0020 by transfer.
        0x30BC, 0x9380, 0x30BC, 0x9400,
        0x30BC, 0x9500, 0x30BC, 0x9608, 0x30BC, 0x9700,
        0x20BC, 0x4020, 0x0080,         // MOVE.L #$40200080,(A0)
        // The palette: 64 words from $1400 to CRAM.
        0x30BC, 0x9340, 0x30BC, 0x9400,
        0x30BC, 0x9500, 0x30BC, 0x960A, 0x30BC, 0x9700,
        0x20BC, 0xC000, 0x0080,         // CRAM colour 0 by transfer
        // Plane A: 2048 words from $2000 to VRAM $C000.
        0x30BC, 0x9300, 0x30BC, 0x9408,
        0x30BC, 0x9500, 0x30BC, 0x9610, 0x30BC, 0x9700,
        0x20BC, 0x4000, 0x0083,         // VRAM $C000 by transfer
        // Plane B: 2048 words from $3000 to VRAM $E000.
        0x30BC, 0x9300, 0x30BC, 0x9408,
        0x30BC, 0x9500, 0x30BC, 0x9618, 0x30BC, 0x9700,
        0x20BC, 0x6000, 0x0083,         // VRAM $E000 by transfer
        // The sprite table, kept in work RAM at $FF0100.
        0x45F9, 0x0000, 0x1600,         // LEA $1600,A2
        0x47F9, 0x00FF, 0x0100,         // LEA $FF0100,A3
        0x7007,                         // MOVEQ #7,D0
    ]).label("copy").words(&[
        0x26DA,                         // MOVE.L (A2)+,(A3)+
    ]).dbra(0, "copy").words(&[
        0x13FC, 0x008F, 0x00C0, 0x0011, // MOVE.B #$8F,($C00011).L: PSG tone 0
        0x13FC, 0x0008, 0x00C0, 0x0011, // MOVE.B #$08,($C00011).L
        0x13FC, 0x0090, 0x00C0, 0x0011, // MOVE.B #$90,($C00011).L: its volume
        0x7A00,                         // MOVEQ #0,D5
        0x30BC, 0x8174,                 // register 1: display on
        0x46FC, 0x2000,                 // MOVE.W #$2000,SR: interrupts on
    ]).label("idle").branch(0x6000, "idle");

    // The horizontal interrupt: plane A sixteen pixels further right.
    #[rustfmt::skip]
    program.org(0x400).words(&[
        0x0645, 0x0010,                 // ADDI.W #16,D5
        0x20BC, 0x7C00, 0x0003,         // MOVE.L #$7C000003,(A0): VRAM $FC00
        0x3285,                         // MOVE.W D5,(A1)
        0x4E73,                         // RTE
    ]);

    // The vertical interrupt.
    #[rustfmt::skip]
    program.org(VERTICAL_BLANK_HANDLER as usize).words(&[
        0x5279, 0x00FF, 0x0000,         // ADDQ.W #1,($FF0000).L: the frames
        0x3239, 0x00FF, 0x0000,         // MOVE.W ($FF0000).L,D1
        0x5279, 0x00FF, 0x0106,         // ADDQ.W #1,($FF0106).L: sprite 0 one pixel right
        0x5279, 0x00FF, 0x010E,         // sprite 1
        0x5279, 0x00FF, 0x0116,         // sprite 2
        0x5279, 0x00FF, 0x011E,         // sprite 3
        // The sprite table: 16 words from $FF0100 to VRAM $F000 by transfer.
        0x30BC, 0x9310, 0x30BC, 0x9400,
        0x30BC, 0x9580, 0x30BC, 0x9680, 0x30BC, 0x977F,
        0x20BC, 0x7000, 0x0083,         // VRAM $F000 by transfer
        // Tile 1: 16 words from $1000, or $1200 in odd frames.
        0x30BC, 0x9310, 0x30BC, 0x9400,
        0x30BC, 0x9500,
        0x3401,                         // MOVE.W D1,D2
        0x0242, 0x0001,                 // ANDI.W #1,D2
        0x0042, 0x9608,                 // ORI.W #$9608,D2
        0x3082,                         // MOVE.W D2,(A0)
        0x30BC, 0x9700,
        0x20BC, 0x4020, 0x0080,         // VRAM $0020 by transfer
        // Colour 2 through the port.
        0x3401,                         // MOVE.W D1,D2
        0x0242, 0x0EEE,                 // ANDI.W #$0EEE,D2
        0x20BC, 0xC004, 0x0000,         // MOVE.L #$C0040000,(A0): CRAM colour 2
        0x3282,                         // MOVE.W D2,(A1)
        // Plane A's vertical scroll through the port.
        0x20BC, 0x4000, 0x0010,         // VSRAM word 0
        0x3281,                         // MOVE.W D1,(A1)
        // Plane A's horizontal scroll for the top of the picture.
        0x3A01,                         // MOVE.W D1,D5
        0x20BC, 0x7C00, 0x0003,         // VRAM $FC00
        0x3285,                         // MOVE.W D5,(A1)
        // One frame in four, the sprite table at $F400, which is empty.
        0x3401,                         // MOVE.W D1,D2
        0x0242, 0x0003,                 // ANDI.W #3,D2
    ]).branch(0x6700, "no_sprites").words(&[
        0x30BC, 0x8578,                 // register 5: sprites at $F000
    ]).branch(0x6000, "sprites_chosen").label("no_sprites").words(&[
        0x30BC, 0x857A,                 // register 5: sprites at $F400
    ]).label("sprites_chosen").words(&[
        0x13FC, 0x008F, 0x00C0, 0x0011, // MOVE.B #$8F,($C00011).L
        0x1601,                         // MOVE.B D1,D3
        0x0203, 0x003F,                 // ANDI.B #$3F,D3
        0x13C3, 0x00C0, 0x0011,         // MOVE.B D3,($C00011).L: PSG tone 0 moves
        0x4E73,                         // RTE
    ]);

    // Tiles 1 to 3 of colours 1 to 3, tile 4 empty, tiles 5 to 8 (the
    // sprites' four cells) of colours 5 to 8.
    let mut tiles = Vec::new();
    for colour in [1u8, 2, 3, 0, 5, 6, 7, 8] {
        tiles.extend_from_slice(&[(colour << 4) | colour; 32]);
    }
    program.data(0x1000, &tiles);
    // Tile 1's other face: colours 1 and 9 in a checker.
    let checker: Vec<u8> = (0..8)
        .flat_map(|row| [if row % 2 == 0 { 0x19 } else { 0x91 }; 4])
        .collect();
    program.data(0x1200, &checker);
    let palette: Vec<u8> = (0..64u16)
        .flat_map(|index| colour(index).to_be_bytes())
        .collect();
    program.data(0x1400, &palette);
    // Four sprites of two cells by two (tiles 5 to 8), ten pixels apart, so
    // that they overlap, linked 0 to 3; the odd ones of high priority.
    let mut sprites = Vec::new();
    for sprite in 0..4u16 {
        let link = if sprite < 3 { sprite + 1 } else { 0 };
        let attributes = ((sprite % 2) << 15) | ((sprite % 4) << 13) | 5;
        for word in [
            128 + 64 + 6 * sprite,
            0x0500 | link,
            attributes,
            228 + 10 * sprite,
        ] {
            sprites.extend_from_slice(&word.to_be_bytes());
        }
    }
    program.data(0x1600, &sprites);
    // Plane A: the cell of row r and column c is tile 1 + (r + c) % 4.
    let plane_a: Vec<u8> = (0..32u16)
        .flat_map(|row| (0..64u16).map(move |column| 1 + (row + column) % 4))
        .flat_map(|cell| cell.to_be_bytes())
        .collect();
    program.data(0x2000, &plane_a);
    // Plane B: tile 2 of palette line 1 in every cell.
    let plane_b: Vec<u8> = (0..32 * 64)
        .flat_map(|_| ((1u16 << 13) | 2).to_be_bytes())
        .collect();
    program.data(0x3000, &plane_b);
    program.build()
}

/// Colour `index` of the program's palette, a CRAM word: a different one
/// for each index but 0, the backdrop's black.
fn colour(index: u16) -> u16 {
    if index == 0 {
        0
    } else {
        ((index % 8) << 1) | (((index / 8) % 8) << 5) | (((index * 3) % 8) << 9)
    }
}

/// A machine running the program, its audio on.
fn build() -> GenesisEmulator {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(&program()).expect("the program loads");
    emu.set_execution_mode(ExecutionMode::Running);
    emu.set_audio_enabled(true);
    emu
}

/// An event breakpoint on the start of line `line`.
fn on_line(line: u16) -> Breakpoint {
    let (code, param) = GenesisEvent::Scanline(line).code_param();
    Breakpoint {
        address: 0,
        bp_type: BreakpointType::Event { code, param },
        cpu_id: None,
        condition: None,
    }
}

/// The Mega Drive core, as the suite takes it.
fn genesis() -> CoreUnderTest<GenesisEmulator> {
    CoreUnderTest {
        build,
        rom: program(),
        run_frame: |emu| EmulatorCore::run_frame(emu),
        state_hash: GenesisEmulator::state_hash,
        frame_count: GenesisEmulator::frame_count,
        sprite_order: |emu| emu.vdp_mut().resolved_sprite_render_order(),
        sprite_layer: LAYER_SPRITES as u32,
        rest_of_frame: VERTICAL_BLANK_HANDLER,
        stop_in_picture: on_line(100),
        stop_in_vertical_blank: on_line(230),
        video_memory_edits: vec![
            VideoMemoryEdit {
                memory: "VRAM, tile 3 of plane A",
                write: |emu| emu.vdp_mut().write_vram_debug(3 * 32, &[0x44; 32]),
            },
            VideoMemoryEdit {
                memory: "CRAM, colour 3 of plane A",
                write: |emu| emu.vdp_mut().write_cram_debug(3, &[0x0EEE]),
            },
            VideoMemoryEdit {
                memory: "VSRAM, plane A's vertical scroll",
                write: |emu| {
                    let scroll = emu.vdp().get_vsram()[0];
                    emu.vdp_mut()
                        .write_vsram_debug(0, &[scroll.wrapping_add(8) & 0x07FF]);
                },
            },
            VideoMemoryEdit {
                memory: "VRAM, the sprite table",
                write: |emu| {
                    let vram = emu.vdp().get_vram();
                    let x = u16::from_be_bytes([vram[0xF006], vram[0xF007]]);
                    emu.vdp_mut()
                        .write_vram_debug(0xF006, &(x + 40).to_be_bytes());
                },
            },
        ],
    }
}

retro_core::capture_conformance_tests!(genesis());
