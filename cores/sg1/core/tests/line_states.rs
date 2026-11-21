//! The line table: the settings each line of the picture was drawn with,
//! recorded while frames run, and the picture drawn again from them.
//!
//! The program below changes, from its horizontal interrupt, plane A's
//! horizontal scroll on line 100, colour 1 early in line 120 (so the
//! change shows on line 120 itself from the beam's column on), plane A's
//! vertical scroll on line 140, the backdrop register on line 160 and a
//! nametable cell on line 180; its vertical interrupt puts all five back
//! for the next frame. Video memory, the palette and the registers at the
//! end of the picture are those of its bottom lines, so a picture drawn
//! again from them alone flattens every split; drawn from the line table,
//! it is the picture the frame drew.

mod helpers;

use helpers::M68kProgram;
use retro_core::capture::{
    layer, planes, CaptureCore, CaptureRecording, SpriteFate, NO_COLUMN, NO_PALETTE,
    WORDS_PER_RECORD,
};
use retro_core::debug::DebugEvent;
use retro_core::frame::{run_frame_on, run_to_capture_boundary_on};
use retro_core::savestate::RomCheck;
use retro_core::types::{Breakpoint, BreakpointType};
use retro_core::video::{LineStateTable, SpriteOrderEntry, VideoDebugger};
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode, ExecutionTier};
use sg1_core::debugger::GenesisEvent;
use sg1_core::vdp::sources::DRAWN_BY_SPRITE;
use sg1_core::vdp::{LineKind, Vdp, LAYER_PLANE_A, LAYER_SPRITES};
use sg1_core::GenesisEmulator;

/// Frames run before a check, so the program has set the VDP up.
const WARM_UP: u64 = 4;

/// Colour 1 before line 120 and from it on.
const RED: u16 = 0x000E;
const YELLOW: u16 = 0x00EE;

/// The program of the module's notes.
fn raster_program() -> Vec<u8> {
    let mut program = M68kProgram::new(0x4000, 0x200);
    program.vector(28, 0x400).vector(30, 0x500);

    #[rustfmt::skip]
    program.org(0x200).words(&[
        0x46FC, 0x2700,                 // MOVE.W #$2700,SR
        0x41F9, 0x00C0, 0x0004,         // LEA $C00004,A0: control port
        0x43F9, 0x00C0, 0x0000,         // LEA $C00000,A1: data port
        0x30BC, 0x8014,                 // register 0: horizontal interrupt on
        0x30BC, 0x8164,                 // register 1: display, vertical interrupt, mode 5
        0x30BC, 0x8230,                 // register 2: plane A at $C000
        0x30BC, 0x8407,                 // register 4: plane B at $E000
        0x30BC, 0x8578,                 // register 5: sprites at $F000
        0x30BC, 0x8700,                 // register 7: backdrop colour 0
        0x30BC, 0x8A00,                 // register 10: an interrupt every line
        0x30BC, 0x8B00,                 // register 11: whole-screen scrolls
        0x30BC, 0x8C81,                 // register 12: 40 cells
        0x30BC, 0x8D3F,                 // register 13: horizontal scroll at $FC00
        0x30BC, 0x8F02,                 // register 15: auto-increment 2
        0x30BC, 0x9001,                 // register 16: planes of 64 by 32 cells
        0x20BC, 0x4020, 0x0000,         // MOVE.L #$40200000,(A0): VRAM at $0020
        0x45F9, 0x0000, 0x0800,         // LEA $800,A2: tiles 1 to 8
        0x303C, 0x007F,                 // MOVE.W #127,D0
    ]).label("tiles").words(&[
        0x329A,                         // MOVE.W (A2)+,(A1)
    ]).dbra(0, "tiles").words(&[
        0x20BC, 0xC000, 0x0000,         // MOVE.L #$C0000000,(A0): CRAM at colour 0
        0x45F9, 0x0000, 0x0A00,         // LEA $A00,A2: colours 0 to 5
        0x303C, 0x0005,                 // MOVE.W #5,D0
    ]).label("colours").words(&[
        0x329A,                         // MOVE.W (A2)+,(A1)
    ]).dbra(0, "colours").words(&[
        // Plane A: the cell of row r and column c is tile 1 + (r + c) % 4.
        0x20BC, 0x4000, 0x0003,         // MOVE.L #$40000003,(A0): VRAM at $C000
        0x7200,                         // MOVEQ #0,D1: the row
    ]).label("rows").words(&[
        0x7400,                         // MOVEQ #0,D2: the column
    ]).label("columns").words(&[
        0x3601,                         // MOVE.W D1,D3
        0xD642,                         // ADD.W D2,D3
        0x0243, 0x0003,                 // ANDI.W #3,D3
        0x5243,                         // ADDQ.W #1,D3
        0x3283,                         // MOVE.W D3,(A1)
        0x5242,                         // ADDQ.W #1,D2
        0x0C42, 0x0040,                 // CMPI.W #64,D2
    ]).branch(0x6600, "columns").words(&[
        0x5241,                         // ADDQ.W #1,D1
        0x0C41, 0x0020,                 // CMPI.W #32,D1
    ]).branch(0x6600, "rows").words(&[
        // Sprite 0: two cells by two at (100, 50), tiles 5 to 8.
        0x20BC, 0x7000, 0x0003,         // MOVE.L #$70000003,(A0): VRAM at $F000
        0x32BC, 0x00B2,                 // Y 50
        0x32BC, 0x0500,                 // two by two, the last sprite
        0x32BC, 0x0005,                 // tile 5
        0x32BC, 0x00E4,                 // X 100
        0x13FC, 0x008F, 0x00C0, 0x0011, // MOVE.B #$8F,($C00011).L: PSG tone 0
        0x13FC, 0x0008, 0x00C0, 0x0011, // MOVE.B #$08,($C00011).L
        0x13FC, 0x0090, 0x00C0, 0x0011, // MOVE.B #$90,($C00011).L: its volume
        0x7C00,                         // MOVEQ #0,D6: no palette write waiting
        0x46FC, 0x2000,                 // MOVE.W #$2000,SR: interrupts on
    ]).label("idle").branch(0x6000, "idle");

    // The horizontal interrupt, at the start of every line.
    #[rustfmt::skip]
    program.org(0x400).words(&[
        0x4A06,                         // TST.B D6: a palette write waiting?
    ]).branch(0x6700, "which_line").words(&[
        0x3287,                         // MOVE.W D7,(A1): made early in the line
        0x4206,                         // CLR.B D6
    ]).label("which_line").words(&[
        0x1039, 0x00C0, 0x0008,         // MOVE.B ($C00008).L,D0: the line
        0x0C00, 0x0064,                 // CMPI.B #100,D0
    ]).branch(0x6700, "line_100").words(&[
        0x0C00, 0x0077,                 // CMPI.B #119,D0
    ]).branch(0x6700, "line_119").words(&[
        0x0C00, 0x008C,                 // CMPI.B #140,D0
    ]).branch(0x6700, "line_140").words(&[
        0x0C00, 0x00A0,                 // CMPI.B #160,D0
    ]).branch(0x6700, "line_160").words(&[
        0x0C00, 0x00B4,                 // CMPI.B #180,D0
    ]).branch(0x6700, "line_180").words(&[
        0x4E73,                         // RTE
    ]).label("line_100").words(&[
        0x20BC, 0x7C00, 0x0003,         // MOVE.L #$7C000003,(A0): VRAM at $FC00
        0x32BC, 0x0008,                 // plane A eight pixels right
        0x4E73,
    ]).label("line_119").words(&[
        0x20BC, 0xC002, 0x0000,         // MOVE.L #$C0020000,(A0): CRAM at colour 1
        0x3E3C, YELLOW,                 // MOVE.W #YELLOW,D7: written early in line 120
        0x7C01,                         // MOVEQ #1,D6
        0x4E73,
    ]).label("line_140").words(&[
        0x20BC, 0x4000, 0x0010,         // MOVE.L #$40000010,(A0): VSRAM at 0
        0x32BC, 0x0010,                 // plane A sixteen lines up
        0x4E73,
    ]).label("line_160").words(&[
        0x30BC, 0x8704,                 // register 7: backdrop colour 4
        0x4E73,
    ]).label("line_180").words(&[
        0x20BC, 0x4C94, 0x0003,         // MOVE.L #$4C940003,(A0): plane A cell (25, 10)
        0x32BC, 0x0005,                 // tile 5
        0x4E73,
    ]);

    // The vertical interrupt: every change back as the frame began.
    #[rustfmt::skip]
    program.org(0x500).words(&[
        0x20BC, 0x7C00, 0x0003,         // the horizontal scroll
        0x32BC, 0x0000,
        0x20BC, 0x4000, 0x0010,         // the vertical scroll
        0x32BC, 0x0000,
        0x30BC, 0x8700,                 // the backdrop
        0x20BC, 0xC002, 0x0000,         // colour 1
        0x32BC, RED,
        0x20BC, 0x4C94, 0x0003,         // the cell, tile (25 + 10) % 4 + 1
        0x32BC, 0x0004,
        0x5279, 0x00FF, 0x0000,         // ADDQ.W #1,($FF0000).L: the frames
        0x4E73,
    ]);

    // Tiles 1 to 3 of colours 1 to 3, tile 4 empty, tiles 5 to 8 colour 5.
    let mut tiles = Vec::new();
    for colour in [1u8, 2, 3, 0, 5, 5, 5, 5] {
        tiles.extend_from_slice(&[(colour << 4) | colour; 32]);
    }
    program.data(0x800, &tiles);
    let colours: Vec<u8> = [0x0000, RED, 0x00E0, 0x0E00, 0x0EEE, 0x0EE0]
        .iter()
        .flat_map(|word: &u16| word.to_be_bytes())
        .collect();
    program.data(0xA00, &colours);
    program.build()
}

/// A program that, with the display off, starts a CRAM transfer from
/// 68000 memory on line 50 of every frame: twenty lines' worth of colours,
/// which the lines after it show two pixels a colour.
fn direct_colour_program() -> Vec<u8> {
    let mut program = M68kProgram::new(0x4000, 0x200);
    #[rustfmt::skip]
    program.org(0x200).words(&[
        0x46FC, 0x2700,                 // MOVE.W #$2700,SR
        0x41F9, 0x00C0, 0x0004,         // LEA $C00004,A0
        0x30BC, 0x8004,                 // register 0
        0x30BC, 0x8114,                 // register 1: display off, transfers on, mode 5
        0x30BC, 0x8C81,                 // register 12: 40 cells
        0x30BC, 0x8F00,                 // register 15: no auto-increment
        0x30BC, 0x8700,                 // register 7: backdrop colour 0
    ]).label("frame").words(&[
        0x1039, 0x00C0, 0x0008,         // MOVE.B ($C00008).L,D0: the line
        0x0C00, 0x0032,                 // CMPI.B #50,D0
    ]).branch(0x6600, "frame").words(&[
        0x30BC, 0x9378,                 // registers 19 and 20: 3960 words
        0x30BC, 0x940F,
        0x30BC, 0x9500,                 // registers 21 to 23: from $001000
        0x30BC, 0x9608,
        0x30BC, 0x9700,
        0x20BC, 0xC000, 0x0080,         // MOVE.L #$C0000080,(A0): CRAM at 0, by transfer
    ]).label("leave_line").words(&[
        0x1039, 0x00C0, 0x0008,
        0x0C00, 0x0032,
    ]).branch(0x6700, "leave_line").branch(0x6000, "frame");

    let colours: Vec<u8> = (0..3960u16)
        .flat_map(|i| {
            let word = ((i % 8) << 1) | (((i / 8) % 8) << 5) | (((i / 64) % 8) << 9);
            word.to_be_bytes()
        })
        .collect();
    program.data(0x1000, &colours);
    program.build()
}

/// A program whose horizontal interrupt comes on line 119 of every frame,
/// its handler's first instruction a palette store, which is the line's
/// first instruction: colour 0, the backdrop, turns from red to yellow.
/// The vertical interrupt makes it red again for the next frame.
fn first_instruction_program() -> Vec<u8> {
    let mut program = M68kProgram::new(0x1000, 0x200);
    program.vector(28, 0x400).vector(30, 0x500);

    #[rustfmt::skip]
    program.org(0x200).words(&[
        0x46FC, 0x2700,                 // MOVE.W #$2700,SR
        0x41F9, 0x00C0, 0x0004,         // LEA $C00004,A0: control port
        0x43F9, 0x00C0, 0x0000,         // LEA $C00000,A1: data port
        0x30BC, 0x8014,                 // register 0: horizontal interrupt on
        0x30BC, 0x8164,                 // register 1: display, vertical interrupt, mode 5
        0x30BC, 0x8700,                 // register 7: backdrop colour 0
        0x30BC, 0x8A77,                 // register 10: an interrupt on line 119
        0x30BC, 0x8C81,                 // register 12: 40 cells
        0x30BC, 0x8F00,                 // register 15: no auto-increment
        0x20BC, 0xC000, 0x0000,         // MOVE.L #$C0000000,(A0): CRAM at colour 0
        0x32BC, RED,                    // MOVE.W #RED,(A1)
        0x3E3C, YELLOW,                 // MOVE.W #YELLOW,D7
        0x46FC, 0x2000,                 // MOVE.W #$2000,SR: interrupts on
    ]).label("idle").branch(0x6000, "idle");

    // The horizontal interrupt: its first instruction is the line's first.
    #[rustfmt::skip]
    program.org(0x400).words(&[
        0x3287,                         // MOVE.W D7,(A1): colour 0 yellow
        0x4E73,                         // RTE
    ]);

    // The vertical interrupt: colour 0 red again.
    #[rustfmt::skip]
    program.org(0x500).words(&[
        0x32BC, RED,                    // MOVE.W #RED,(A1)
        0x4E73,                         // RTE
    ]);
    program.build()
}

/// A machine running `rom`, its audio on.
fn running(rom: &[u8]) -> GenesisEmulator {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(rom).expect("the program loads");
    emu.set_execution_mode(ExecutionMode::Running);
    emu.set_audio_enabled(true);
    emu
}

/// A machine running `rom` with the line states recorded, warmed up.
fn recording(rom: &[u8]) -> GenesisEmulator {
    let mut emu = running(rom);
    emu.set_capture_recording(line_states());
    for _ in 0..WARM_UP {
        run_frame_on(&mut emu, ExecutionTier::Play);
    }
    emu
}

/// The recording of line states alone.
fn line_states() -> CaptureRecording {
    CaptureRecording {
        line_states: true,
        ..CaptureRecording::default()
    }
}

/// The picture the host shows.
fn shown(emu: &GenesisEmulator) -> Vec<u8> {
    emu.capture_frame_snapshot().framebuffer
}

/// Column `name` of the table, one value per row.
fn column(table: &LineStateTable, name: &str) -> Vec<u32> {
    let at = table
        .columns
        .iter()
        .position(|c| c == name)
        .unwrap_or_else(|| panic!("no column {name}"));
    table
        .values
        .chunks_exact(table.columns.len())
        .map(|row| row[at])
        .collect()
}

/// The rows, 320 pixels of RGBA each, where `a` and `b` differ.
fn rows_apart(a: &[u8], b: &[u8]) -> Vec<usize> {
    a.as_chunks::<{ 320 * 4 }>()
        .0
        .iter()
        .zip(b.as_chunks::<{ 320 * 4 }>().0)
        .enumerate()
        .filter(|(_, (x, y))| x != y)
        .map(|(row, _)| row)
        .collect()
}

#[test]
fn the_picture_drawn_again_from_the_table_is_the_picture_byte_for_byte() {
    let mut emu = recording(&raster_program());
    for _ in 0..6 {
        emu.run_to_capture_boundary();
        let picture = emu.picture_from_line_states().expect("line states on");
        assert_eq!((picture.width, picture.height), (320, 224));
        assert_eq!(picture.rgba, shown(&emu), "at the capture boundary");

        // After the vertical interrupt put everything back, the journal
        // undoes it for every line.
        run_frame_on(&mut emu, ExecutionTier::Play);
        let picture = emu.picture_from_line_states().expect("line states on");
        assert_eq!(picture.rgba, shown(&emu), "after the whole frame");
    }

    // A repaint draws every line from its record: the same picture.
    emu.run_to_capture_boundary();
    let before = shown(&emu);
    assert!(emu.repaint_held_frame(), "every line from its record");
    assert_eq!(shown(&emu), before);
}

#[test]
fn the_effects_are_real_drawn_again_from_the_end_of_the_frame_they_flatten() {
    let mut emu = running(&raster_program());
    for _ in 0..WARM_UP {
        run_frame_on(&mut emu, ExecutionTier::Play);
    }
    emu.run_to_capture_boundary();
    let live = shown(&emu);
    assert!(
        !emu.repaint_held_frame(),
        "no line states: from the state now"
    );
    let apart = rows_apart(&live, &shown(&emu));
    assert!(apart.contains(&0), "the top line takes the bottom's scroll");
    assert!(apart.contains(&120), "the line split by the palette write");
    assert!(
        apart.iter().all(|&row| row <= 180),
        "below line 180 the end state is the state each line read: {apart:?}"
    );
}

#[test]
fn the_table_says_what_each_line_was_drawn_with() {
    let mut emu = recording(&raster_program());
    emu.run_to_capture_boundary();
    let frame = emu.capture_frame_snapshot().frame;
    let table = emu.line_states().expect("line states on");

    let mut names = vec!["line", "frame", "kind", "exact"]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();
    names.extend((0..24).map(|i| format!("register[{i}]")));
    names.extend(["horizontal_scroll[0]", "horizontal_scroll[1]"].map(String::from));
    names.extend((0..40).map(|i| format!("vertical_scroll[{i}]")));
    names.extend(["sprite_mask_carried_in", "palette"].map(String::from));
    assert_eq!(table.columns, names);
    assert_eq!(table.values.len(), 224 * names.len());
    assert_eq!(table.frame, frame);
    assert!(table.from_first_line && table.complete && table.memory_exact);
    assert_eq!((table.rows_per_line, table.first_row_line), (1, 0));

    assert_eq!(column(&table, "line"), (0..224).collect::<Vec<u32>>());
    assert!(column(&table, "frame")
        .iter()
        .all(|&f| u64::from(f) == frame));
    assert!(column(&table, "kind")
        .iter()
        .all(|&k| k == LineKind::Drawn as u32));
    assert!(column(&table, "exact").iter().all(|&e| e == 1));

    // Each change shows from the line after the one whose interrupt made it.
    let scroll_x = column(&table, "horizontal_scroll[0]");
    assert_eq!((scroll_x[100], scroll_x[101]), (0, 8));
    let scroll_y = column(&table, "vertical_scroll[0]");
    assert_eq!((scroll_y[140], scroll_y[141]), (0, 16));
    let backdrop = column(&table, "register[7]");
    assert_eq!((backdrop[160], backdrop[161]), (0, 4));
    assert!(column(&table, "horizontal_scroll[1]")
        .iter()
        .all(|&s| s == 0));

    // The palette: one before the write on line 120, one after.
    let palettes = emu.line_palettes().expect("line states on");
    assert_eq!(palettes.colours_per_palette, 64);
    assert_eq!(palettes.palette_of_line.len(), 224);
    assert_eq!(palettes.palettes.len(), 2 * 64);
    assert_eq!(
        column(&table, "palette"),
        palettes
            .palette_of_line
            .iter()
            .map(|&p| u32::from(p))
            .collect::<Vec<_>>()
    );
    assert!(palettes.palette_of_line[..121].iter().all(|&p| p == 0));
    assert!(palettes.palette_of_line[121..].iter().all(|&p| p == 1));
    assert_eq!(palettes.palettes[1], RED);
    assert_eq!(palettes.palettes[64 + 1], YELLOW);

    // The write itself, on line 120, from the beam's column on.
    let changes = palettes.changes.as_ref().expect("this core records them");
    let changes = changes.as_chunks::<5>().0;
    assert_eq!(changes.len(), 1, "{changes:?}");
    let [row, from, index, colour, whole_line] = changes[0];
    assert_eq!((row, index, colour, whole_line), (120, 1, YELLOW, 0));
    assert!(from != NO_COLUMN && from > 0 && from < 320, "column {from}");
    // And the picture shows it: red left of the column, yellow from it.
    let picture = shown(&emu);
    let red = Vdp::convert_genesis_color(RED);
    let yellow = Vdp::convert_genesis_color(YELLOW);
    let colours_on_line: Vec<(u8, u8, u8)> = picture[120 * 320 * 4..121 * 320 * 4]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| (p[0], p[1], p[2]))
        .collect();
    let from = usize::from(from);
    assert!(colours_on_line[..from].contains(&red));
    assert!(!colours_on_line[..from].contains(&yellow));
    assert!(colours_on_line[from..].contains(&yellow));
    assert!(!colours_on_line[from..].contains(&red));
}

#[test]
fn a_palette_store_by_a_line_s_first_instruction_colours_that_line_from_the_beam() {
    let mut emu = running(&first_instruction_program());
    emu.set_capture_recording(CaptureRecording {
        line_states: true,
        frame_writes: true,
        ..CaptureRecording::default()
    });
    for _ in 0..WARM_UP {
        run_frame_on(&mut emu, ExecutionTier::Play);
    }
    let _ = emu.take_frame_writes();
    emu.run_to_capture_boundary();

    // The store's record names line 119, and as its position the cycles
    // the line's start took: the interrupt's entry (44) after what the line
    // before ran past its end.
    let writes = emu.take_frame_writes().expect("frame writes on");
    let records = writes.records.as_chunks::<WORDS_PER_RECORD>().0;
    assert_eq!(records.len(), 1, "{records:X?}");
    let (line, position) = (records[0][2] & 0xFFFF, records[0][2] >> 16);
    assert_eq!(line, 119, "{records:X?}");
    assert!(
        (44..60).contains(&position),
        "cycles into the line: {position}"
    );

    // The palette's record: the store coloured line 119 again from the
    // beam's column, early in the line.
    let palettes = emu.line_palettes().expect("line states on");
    let changes = palettes.changes.as_ref().expect("this core records them");
    let changes = changes.as_chunks::<5>().0;
    assert_eq!(changes.len(), 1, "{changes:?}");
    let [row, from, index, colour, whole_line] = changes[0];
    assert_eq!((row, index, colour, whole_line), (119, 0, YELLOW, 0));
    assert!(from != NO_COLUMN && from > 0 && from < 80, "column {from}");

    // And the picture: line 118 red, line 119 red left of the column and
    // yellow from it, line 120 yellow.
    let picture = shown(&emu);
    let colours = |row: usize| -> Vec<(u8, u8, u8)> {
        picture[row * 320 * 4..(row + 1) * 320 * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| (p[0], p[1], p[2]))
            .collect()
    };
    let red = Vdp::convert_genesis_color(RED);
    let yellow = Vdp::convert_genesis_color(YELLOW);
    let from = usize::from(from);
    assert!(colours(118).iter().all(|&c| c == red), "line 118");
    let line_119 = colours(119);
    assert!(line_119[..from].iter().all(|&c| c == red), "before {from}");
    assert!(line_119[from..].iter().all(|&c| c == yellow), "from {from}");
    assert!(colours(120).iter().all(|&c| c == yellow), "line 120");
}

#[test]
fn a_layer_drawn_from_the_table_shows_the_splits() {
    let mut emu = recording(&raster_program());
    emu.run_to_capture_boundary();
    let picture = shown(&emu);
    let layer = emu.render_layer(LAYER_PLANE_A as u32).expect("plane A");
    assert!(layer.from_line_states);
    let image = layer.image;
    assert_eq!((image.width, image.height), (320, 224));

    // Plane B is empty and shadow and highlight are off: wherever plane A
    // covers and the sprite does not, the picture is plane A's colour.
    let under_sprite = |x: usize, y: usize| (100..116).contains(&x) && (50..66).contains(&y);
    let mut covered = 0;
    for y in 0..224 {
        for x in 0..320 {
            let at = (y * 320 + x) * 4;
            if image.rgba[at + 3] == 255 && !under_sprite(x, y) {
                assert_eq!(image.rgba[at..at + 3], picture[at..at + 3], "({x}, {y})");
                covered += 1;
            }
        }
    }
    assert!(covered > 320 * 224 / 2);

    // Without the table the layer is drawn from the end of the frame.
    emu.set_capture_recording(CaptureRecording::default());
    let flat = emu.render_layer(LAYER_PLANE_A as u32).expect("plane A");
    assert!(!flat.from_line_states);
    assert!(!rows_apart(&flat.image.rgba, &image.rgba).is_empty());
}

#[test]
fn drawing_again_moves_nothing_a_savestate_holds() {
    let mut emu = recording(&raster_program());
    emu.run_to_capture_boundary();
    let hash = emu.state_hash();
    let picture = shown(&emu);
    emu.repaint_held_frame();
    for layer in 0..4 {
        emu.render_layer(layer).expect("a layer");
    }
    emu.picture_from_line_states().expect("line states on");
    emu.vdp_mut()
        .render_mode_image(sg1_core::vdp::RenderLayerMode::PriorityOverlay);
    assert_eq!(emu.state_hash(), hash);
    assert_eq!(shown(&emu), picture);

    // And the frame ends as one nothing was drawn again in does.
    let mut whole = recording(&raster_program());
    run_frame_on(&mut whole, ExecutionTier::Play);
    run_frame_on(&mut emu, ExecutionTier::Play);
    assert_eq!(emu.state_hash(), whole.state_hash());
}

/// Arm an event breakpoint and return its id.
fn break_on(emu: &mut GenesisEmulator, event: GenesisEvent) -> u32 {
    let (code, param) = event.code_param();
    emu.debugger().add_breakpoint(Breakpoint {
        address: 0,
        bp_type: BreakpointType::Event { code, param },
        cpu_id: None,
        condition: None,
    })
}

#[test]
fn after_a_stop_part_of_the_way_down_the_lines_below_are_the_frame_before() {
    let mut emu = recording(&raster_program());
    let frame = emu.frame_count();
    let id = break_on(&mut emu, GenesisEvent::Scanline(100));
    run_frame_on(&mut emu, ExecutionTier::LightDebug);
    emu.debugger().remove_breakpoint(id);
    assert_eq!(emu.vdp().timing().scanline, 100);

    let table = emu.line_states().expect("line states on");
    assert_eq!(table.frame, frame + 1);
    assert!(table.from_first_line && !table.complete && !table.memory_exact);
    let frames = column(&table, "frame");
    let exact = column(&table, "exact");
    for line in 0..224 {
        let drawn_in = if line <= 100 { frame + 1 } else { frame };
        assert_eq!(u64::from(frames[line]), drawn_in, "line {line}");
        assert_eq!(exact[line], u32::from(line <= 100), "line {line}");
    }
    // The picture the host holds is no frame's finished picture: the
    // snapshot numbers it by the frames finished.
    assert_eq!(emu.capture_frame_snapshot().frame, frame);

    // The table does not hold the frame from its first line to its
    // finished picture: there is no picture drawn from it.
    assert!(emu.picture_from_line_states().is_none());

    // A repaint draws every line from its record: those of this frame come
    // back exactly; those of the frame before come back with their own
    // scroll, palette and registers, over video memory as this frame found
    // it, which has the cell of line 180 put back: they differ there alone.
    let live = shown(&emu);
    assert!(emu.repaint_held_frame(), "every line from its record");
    let repainted = shown(&emu);
    let apart = rows_apart(&repainted, &live);
    assert_eq!(apart, (184..192).collect::<Vec<_>>());
    for y in 184..192 {
        for x in 0..320 {
            let at = (y * 320 + x) * 4;
            let same = repainted[at..at + 4] == live[at..at + 4];
            assert_eq!(same, !(88..96).contains(&x), "({x}, {y})");
        }
    }
}

#[test]
fn armed_part_of_the_way_through_a_frame_the_table_holds_the_lines_after() {
    let mut emu = running(&raster_program());
    for _ in 0..WARM_UP {
        run_frame_on(&mut emu, ExecutionTier::LightDebug);
    }
    let id = break_on(&mut emu, GenesisEvent::Scanline(100));
    run_frame_on(&mut emu, ExecutionTier::LightDebug);
    emu.debugger().remove_breakpoint(id);
    emu.set_capture_recording(line_states());
    emu.set_execution_mode(ExecutionMode::Running);
    run_to_capture_boundary_on(&mut emu, ExecutionTier::LightDebug);
    assert!(emu.at_capture_boundary());

    let table = emu.line_states().expect("line states on");
    assert!(!table.from_first_line && !table.complete);
    let kinds = column(&table, "kind");
    assert!(kinds[..=100]
        .iter()
        .all(|&k| k == LineKind::NotRecorded as u32));
    assert!(kinds[101..].iter().all(|&k| k == LineKind::Drawn as u32));
    let palettes = emu.line_palettes().expect("line states on");
    assert!(palettes.palette_of_line[..=100]
        .iter()
        .all(|&p| p == NO_PALETTE));

    // The table does not hold the frame from its first line: there is no
    // picture drawn from it.
    assert!(emu.picture_from_line_states().is_none());

    // A repaint draws the lines it holds exactly, the others from the
    // state now.
    let live = shown(&emu);
    assert!(!emu.repaint_held_frame(), "not every line has a record");
    let apart = rows_apart(&shown(&emu), &live);
    assert!(
        !apart.is_empty() && apart.iter().all(|&row| row <= 100),
        "{apart:?}"
    );
}

#[test]
fn a_debugger_edit_of_a_colour_or_the_vertical_scroll_shows_on_every_line() {
    let mut emu = recording(&raster_program());
    emu.run_to_capture_boundary();
    let white = Vdp::convert_genesis_color(0x0EEE);
    let red = Vdp::convert_genesis_color(RED);
    let yellow = Vdp::convert_genesis_color(YELLOW);
    let count = |picture: &[u8], colour: (u8, u8, u8)| {
        picture
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| (p[0], p[1], p[2]) == colour)
            .count()
    };
    let before = shown(&emu);
    let colour_1 = count(&before, red) + count(&before, yellow);
    assert!(count(&before, red) > 0 && count(&before, yellow) > 0);

    emu.vdp_mut().write_cram_debug(1, &[0x0EEE]);
    emu.repaint_held_frame();
    let after = shown(&emu);
    assert_eq!(count(&after, red) + count(&after, yellow), 0);
    assert_eq!(count(&after, white), count(&before, white) + colour_1);

    // The vertical scroll the lower lines read was 16; every line takes 0.
    emu.vdp_mut().write_vsram_debug(0, &[0]);
    let table = emu.line_states().expect("line states on");
    assert!(column(&table, "vertical_scroll[0]").iter().all(|&v| v == 0));
    let palettes = emu.line_palettes().expect("line states on");
    assert!(palettes
        .changes
        .expect("this core records them")
        .as_chunks::<5>()
        .0
        .iter()
        .all(|change| change[3] == 0x0EEE));
}

#[test]
fn a_debugger_write_into_video_memory_leaves_the_memory_inexact_until_the_next_picture() {
    type Edit = fn(&mut Vdp);
    let edits: [(&str, Edit); 3] = [
        // Tile 1's first four pixels, colour 4.
        ("VRAM", |vdp| vdp.write_vram_debug(0x0020, &[0x44; 2])),
        ("CRAM", |vdp| vdp.write_cram_debug(1, &[0x0EEE])),
        ("VSRAM", |vdp| vdp.write_vsram_debug(0, &[3])),
    ];
    for (what, edit) in edits {
        let mut emu = recording(&raster_program());
        emu.run_to_capture_boundary();
        // An empty write writes nothing.
        emu.vdp_mut().write_vram_debug(0x0020, &[]);
        emu.vdp_mut().write_cram_debug(1, &[]);
        emu.vdp_mut().write_vsram_debug(0, &[]);
        let table = emu.line_states().expect("line states on");
        assert!(table.complete && table.memory_exact, "{what}: no edit yet");
        let before = shown(&emu);

        edit(emu.vdp_mut());
        let table = emu.line_states().expect("line states on");
        assert!(table.complete, "{what}: the table still holds the frame");
        assert!(!table.memory_exact, "{what}: no line read the edit");
        assert!(column(&table, "exact").iter().all(|&e| e == 0), "{what}");

        // The edit shows in the frame drawn again, the repaint and the
        // picture drawn from the table alike.
        assert!(emu.repaint_held_frame(), "{what}");
        let repainted = shown(&emu);
        assert_ne!(repainted, before, "{what}: the edit shows");
        let picture = emu.picture_from_line_states().expect("the whole frame");
        assert_eq!(picture.rgba, repainted, "{what}");

        // From the next picture's first line, the memory is the frame's.
        emu.run_to_capture_boundary();
        let table = emu.line_states().expect("line states on");
        assert!(
            table.complete && table.memory_exact,
            "{what}: the next frame"
        );
        assert!(column(&table, "exact").iter().all(|&e| e == 1), "{what}");
    }
}

#[test]
fn a_cram_transfer_with_the_display_off_paints_its_lines_and_they_come_back() {
    let mut emu = recording(&direct_colour_program());
    emu.run_to_capture_boundary();
    let table = emu.line_states().expect("line states on");
    let kinds = column(&table, "kind");
    for (line, &kind) in kinds.iter().enumerate() {
        let painted = (51..=70).contains(&line);
        let expected = if painted {
            LineKind::DirectColour
        } else {
            LineKind::Blank
        };
        assert_eq!(kind, expected as u32, "line {line}");
    }
    let picture = emu.picture_from_line_states().expect("line states on");
    assert_eq!(picture.rgba, shown(&emu));

    // The line shows the colours two pixels apiece: not one colour.
    let line_51 = &picture.rgba[51 * 320 * 4..52 * 320 * 4];
    assert_ne!(line_51[..4], line_51[2 * 4..2 * 4 + 4]);
    assert_eq!(line_51[..4], line_51[4..8]);

    // Drawn again from the end of the frame, the lines are one colour.
    emu.set_capture_recording(CaptureRecording::default());
    emu.repaint_held_frame();
    let flat = shown(&emu);
    assert_eq!(
        flat[51 * 320 * 4..51 * 320 * 4 + 4],
        flat[51 * 320 * 4 + 8..51 * 320 * 4 + 12]
    );
}

/// The samples the frames so far left for the host.
fn take_audio(emu: &mut GenesisEmulator) -> Vec<u32> {
    emu.host_audio_mut()
        .take_all()
        .iter()
        .map(|s| s.to_bits())
        .collect()
}

#[test]
fn recording_changes_nothing_on_any_frame_loop() {
    for frame_loop in [ExecutionTier::Play, ExecutionTier::LightDebug] {
        for rom in [raster_program(), direct_colour_program()] {
            let mut plain = running(&rom);
            let mut recorded = running(&rom);
            recorded.set_capture_recording(line_states());
            let mut audio = (Vec::new(), Vec::new());
            for frame in 0..12 {
                for emu in [&mut plain, &mut recorded] {
                    run_frame_on(emu, frame_loop);
                }
                assert_eq!(
                    recorded.state_hash(),
                    plain.state_hash(),
                    "{frame_loop:?}, frame {frame}"
                );
                assert_eq!(
                    shown(&recorded),
                    shown(&plain),
                    "{frame_loop:?}, frame {frame}"
                );
                audio.0.extend(take_audio(&mut plain));
                audio.1.extend(take_audio(&mut recorded));
            }
            assert!(!audio.0.is_empty());
            assert_eq!(audio.1, audio.0, "{frame_loop:?}: the samples");
            assert!(recorded.line_states().is_some());
        }
    }
}

#[test]
fn the_setting_outlives_a_reset_a_load_and_a_power_cycle_but_the_records_do_not() {
    let rom = raster_program();
    let mut emu = recording(&rom);
    let state = emu.save_state();
    let all_unrecorded = |emu: &GenesisEmulator| {
        let table = emu.line_states().expect("still armed");
        column(&table, "kind")
            .iter()
            .all(|&k| k == LineKind::NotRecorded as u32)
    };

    EmulatorCore::reset(&mut emu);
    assert_eq!(emu.capture_recording(), line_states());
    assert!(all_unrecorded(&emu));

    emu.set_execution_mode(ExecutionMode::Running);
    run_frame_on(&mut emu, ExecutionTier::Play);
    assert!(!all_unrecorded(&emu));
    emu.load_state(&state, RomCheck::SameRom)
        .expect("the savestate loads");
    assert!(all_unrecorded(&emu));

    // A ROM that is another: the whole machine but the cartridge is
    // replaced, the video chip with it.
    emu.load_rom(&direct_colour_program())
        .expect("the program loads");
    assert_eq!(emu.capture_recording(), line_states());
    emu.power_on_reset();
    assert_eq!(emu.capture_recording(), line_states());
    emu.set_execution_mode(ExecutionMode::Running);
    run_frame_on(&mut emu, ExecutionTier::Play);
    assert!(!all_unrecorded(&emu));

    // Disarmed, nothing is kept.
    emu.set_capture_recording(CaptureRecording::default());
    assert_eq!(emu.capture_recording(), CaptureRecording::default());
    assert!(emu.line_states().is_none());
    assert!(emu.line_palettes().is_none());
    assert!(emu.picture_from_line_states().is_none());

    // The pixel sources and the line sprites are worked out from the line
    // table: each reads back as asked and keeps the table on its own. Every
    // recorder reads back as asked.
    for wanted in [
        CaptureRecording {
            pixel_sources: true,
            ..CaptureRecording::default()
        },
        CaptureRecording {
            line_sprites: true,
            ..CaptureRecording::default()
        },
    ] {
        let wanted = CaptureRecording {
            frame_writes: true,
            video_memory_sources: true,
            ..wanted
        };
        emu.set_capture_recording(wanted);
        assert_eq!(emu.capture_recording(), wanted);
        run_frame_on(&mut emu, ExecutionTier::Play);
        assert!(emu.line_states().is_some(), "{wanted:?} keeps the table");
    }
    // The writes are recorded as they are made: they keep no line table.
    let writes_only = CaptureRecording {
        frame_writes: true,
        video_memory_sources: true,
        ..CaptureRecording::default()
    };
    emu.set_capture_recording(writes_only);
    assert_eq!(emu.capture_recording(), writes_only);
    assert!(emu.line_states().is_none());
    assert!(emu.video_memory_sources().is_some());
    assert!(emu.take_frame_writes().is_some());
}

#[test]
fn where_each_pixel_came_from_is_the_frame_s_and_working_it_out_moves_nothing() {
    let mut emu = recording(&raster_program());
    let mut twin = recording(&raster_program());
    for _ in 0..3 {
        emu.run_to_capture_boundary();
        twin.run_to_capture_boundary();
        let hash = emu.state_hash();
        let picture = shown(&emu);

        let sources = emu.pixel_sources(planes::MAIN).expect("a game is loaded");
        assert!(sources.from_line_states && sources.matches_picture);
        assert_eq!(sources.frame, emu.capture_frame_snapshot().frame);
        let (layer, cell) = (sources.layer.unwrap(), sources.cell.unwrap());
        // Sprite 0, two cells by two at (100, 50), in front of plane A.
        for y in 50..66 {
            for x in 100..116 {
                assert_eq!(
                    layer[y * 320 + x] & layer::WHAT_DREW,
                    DRAWN_BY_SPRITE,
                    "({x}, {y})"
                );
                assert_eq!(cell[y * 320 + x], 0);
            }
        }
        let sprites = emu.line_sprites().expect("a game is loaded");
        assert!(sprites.from_line_states);
        assert_eq!(sprites.lines.len(), 224 * 4);
        for line in 0..224 {
            let row = &sprites.lines[line * 4..line * 4 + 4];
            let on_line = (50..66).contains(&line);
            assert_eq!(row[2], u16::from(on_line), "line {line}");
            if on_line {
                let at = usize::from(row[1]);
                assert_eq!(sprites.slots[at], 0);
                assert_eq!(sprites.fates[at], SpriteFate::DrawnWhole as u8);
                assert_eq!((sprites.x[at], sprites.width[at]), (100, 16));
                assert_eq!(sprites.drawn_width[at], 16);
            }
        }
        let alone = emu.render_sprites(&[1]).expect("a game is loaded");
        let layer_image = emu.render_layer(LAYER_SPRITES as u32).expect("the sprites");
        assert_eq!(alone, layer_image);
        // The one sprite the frame drew, of low priority.
        assert_eq!(
            emu.vdp_mut().resolved_sprite_render_order(),
            [SpriteOrderEntry {
                index: 0,
                priority: 0
            }]
        );

        assert_eq!(emu.state_hash(), hash, "nothing a savestate holds moved");
        assert_eq!(shown(&emu), picture, "the picture shown did not move");
        // And the frame goes on as one nothing was worked out in.
        run_frame_on(&mut emu, ExecutionTier::Play);
        run_frame_on(&mut twin, ExecutionTier::Play);
        assert_eq!(emu.state_hash(), twin.state_hash());
        assert_eq!(shown(&emu), shown(&twin));
    }
}

#[test]
fn a_pixel_a_cram_transfer_painted_names_the_colour_it_moved() {
    let mut emu = recording(&direct_colour_program());
    emu.run_to_capture_boundary();
    let sources = emu.pixel_sources(planes::MAIN).expect("a game is loaded");
    assert!(sources.from_line_states && sources.matches_picture);
    let (layer, colour) = (sources.layer.unwrap(), sources.colour.unwrap());
    let picture = shown(&emu);
    for line in 0..224 {
        for x in 0..320 {
            let at = line * 320 + x;
            if (51..=70).contains(&line) {
                assert_eq!(layer[at], layer::UNLAYERED_COLOUR, "({x}, {line})");
                let (red, green, blue) = Vdp::convert_genesis_color(colour[at]);
                assert_eq!(picture[at * 4..at * 4 + 3], [red, green, blue]);
            } else {
                assert_eq!(layer[at], layer::NOTHING, "({x}, {line})");
                assert_eq!(colour[at], 0, "the backdrop's index");
            }
        }
    }
}

#[test]
fn with_no_game_loaded_there_is_nothing_to_work_out() {
    let mut emu = GenesisEmulator::new();
    assert!(emu.pixel_sources(planes::MAIN).is_none());
    assert!(emu.line_sprites().is_none());
    assert!(emu.render_sprites(&[1; 80]).is_none());
}
