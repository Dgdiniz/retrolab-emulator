//! Who wrote each byte of VRAM, and what each frame wrote: the two
//! recorders that run while frames run, and the ROM offsets behind the
//! 68000 addresses they name.
//!
//! The first program below writes video memory every way the 68000 can,
//! once, as it boots with the display off: a transfer from ROM, one from
//! work RAM, one whose source crosses the end of its 128 KB window, stores
//! through the data port, a fill, a copy, a store into the sprite table, a
//! palette store, and transfers into CRAM and VSRAM. The test then reads
//! back, byte by byte, what wrote each byte and from where.

mod helpers;

use helpers::M68kProgram;
use retro_core::capture::{
    CaptureCore, CaptureRecording, FrameWrites, WriteKind, WriteOrigin, Writer, WORDS_PER_RECORD,
};
use retro_core::cartridge::{SOURCE_COPROCESSOR, SOURCE_UNKNOWN, SOURCE_WORK_RAM};
use retro_core::cpu::Addressable;
use retro_core::frame::run_frame_on;
use retro_core::savestate::RomCheck;
use retro_core::{EmulatorCore, ExecutionMode, ExecutionTier};
use sg1_core::vdp::WriteTarget;
use sg1_core::GenesisEmulator;

/// Both live recorders.
fn writes_recorded() -> CaptureRecording {
    CaptureRecording {
        video_memory_sources: true,
        frame_writes: true,
        ..CaptureRecording::default()
    }
}

/// A machine running `rom` with both recorders armed from its first frame.
fn recording(rom: &[u8]) -> GenesisEmulator {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(rom).expect("the program loads");
    emu.set_execution_mode(ExecutionMode::Running);
    emu.set_audio_enabled(true);
    emu.set_capture_recording(writes_recorded());
    emu
}

/// The program of the module's notes, its labels kept for the checks.
fn boot_writes_program() -> M68kProgram {
    let mut program = M68kProgram::new(0x2_0000, 0x200);
    #[rustfmt::skip]
    program.org(0x200).words(&[
        0x46FC, 0x2700,                 // MOVE.W #$2700,SR
        0x41F9, 0x00C0, 0x0004,         // LEA $C00004,A0: control port
        0x43F9, 0x00C0, 0x0000,         // LEA $C00000,A1: data port
        0x30BC, 0x8114,                 // register 1: display off, transfers on, mode 5
        0x30BC, 0x8C81,                 // register 12: 40 cells
        0x30BC, 0x8F02,                 // register 15: auto-increment 2
        0x30BC, 0x8578,                 // register 5: sprites at $F000
        // Eight words $A000 to $A007 into work RAM at $FF0100.
        0x45F9, 0x00FF, 0x0100,         // LEA $FF0100,A2
        0x303C, 0xA000,                 // MOVE.W #$A000,D0
        0x7207,                         // MOVEQ #7,D1
    ]).label("ram_words").words(&[
        0x34C0,                         // MOVE.W D0,(A2)+
        0x5240,                         // ADDQ.W #1,D0
    ]).dbra(1, "ram_words").words(&[
        // 16 words from ROM $2000 to VRAM $0000.
        0x30BC, 0x9310, 0x30BC, 0x9400, // length 16
        0x30BC, 0x9500, 0x30BC, 0x9610, 0x30BC, 0x9700, // source $2000
        0x20BC, 0x4000, 0x0080,         // MOVE.L #$40000080,(A0): VRAM $0000 by DMA
        // 8 words from work RAM $FF0100 to VRAM $0100.
        0x30BC, 0x9308, 0x30BC, 0x9400,
        0x30BC, 0x9580, 0x30BC, 0x9680, 0x30BC, 0x977F, // source $FF0100
        0x20BC, 0x4100, 0x0080,         // VRAM $0100 by DMA
        // 4 words from ROM $01FFFC on, across the end of the 128 KB window.
        0x30BC, 0x9304, 0x30BC, 0x9400,
        0x30BC, 0x95FE, 0x30BC, 0x96FF, 0x30BC, 0x9700, // source $01FFFC
        0x20BC, 0x4200, 0x0080,         // VRAM $0200 by DMA
        // Stores through the port at VRAM $0300.
        0x45F9, 0x0000, 0x3000,         // LEA $3000,A2
        0x20BC, 0x4300, 0x0000,         // VRAM $0300
        0x329A,                         // MOVE.W (A2)+,(A1): $0300
        0x329A,                         // MOVE.W (A2)+,(A1): $0302
        0x229A,                         // MOVE.L (A2)+,(A1): $0304 and $0306
    ]).label("immediate_store").words(&[
        0x32BC, 0xABCD,                 // MOVE.W #$ABCD,(A1): $0308
        // A fill of 32 cells of $AB at VRAM $0400, increment 1.
        0x30BC, 0x8F01,
        0x30BC, 0x9320, 0x30BC, 0x9400,
        0x30BC, 0x9780,                 // fill
        0x20BC, 0x4400, 0x0080,         // VRAM $0400 by DMA
    ]).label("fill_store").words(&[
        0x32BC, 0xAB00,                 // MOVE.W #$AB00,(A1): starts the fill
    ]).label("wait_fill").words(&[
        0x3010,                         // MOVE.W (A0),D0: the status
        0x0800, 0x0001,                 // BTST #1,D0: a transfer running
    ]).branch(0x6600, "wait_fill").words(&[
        // A copy of 16 bytes from VRAM $0000 to $0500.
        0x30BC, 0x9310, 0x30BC, 0x9400,
        0x30BC, 0x9500, 0x30BC, 0x9600,
        0x30BC, 0x97C0,                 // copy
        0x20BC, 0x0500, 0x00C0,         // VRAM $0500 by copy
    ]).label("wait_copy").words(&[
        0x3010,
        0x0800, 0x0001,
    ]).branch(0x6600, "wait_copy").words(&[
        0x30BC, 0x8F02,
        // A store into the sprite table.
        0x20BC, 0x7000, 0x0003,         // VRAM $F000
    ]).label("sprite_store").words(&[
        0x32BC, 0x00B2,                 // MOVE.W #$00B2,(A1)
        // A palette colour through the port.
        0x20BC, 0xC006, 0x0000,         // CRAM colour 3
    ]).label("colour_store").words(&[
        0x32BC, 0x0E0E,                 // MOVE.W #$0E0E,(A1)
        // 4 words from ROM $2100 into CRAM colour 8.
        0x30BC, 0x9304, 0x30BC, 0x9400,
        0x30BC, 0x9580, 0x30BC, 0x9610, 0x30BC, 0x9700, // source $2100
        0x20BC, 0xC010, 0x0080,         // CRAM colour 8 by DMA
        // 2 words from ROM $2200 into VSRAM word 0.
        0x30BC, 0x9302, 0x30BC, 0x9400,
        0x30BC, 0x9500, 0x30BC, 0x9611, 0x30BC, 0x9700, // source $2200
        0x20BC, 0x4000, 0x0090,         // VSRAM 0 by DMA
    ]).label("idle").branch(0x6000, "idle");

    let tiles: Vec<u8> = (0..32u8).map(|i| 0x10 + i).collect();
    program.data(0x2000, &tiles);
    program.data(0x2100, &[0x00, 0x02, 0x00, 0x04, 0x00, 0x06, 0x00, 0x08]);
    program.data(0x2200, &[0x00, 0x10, 0x00, 0x20]);
    program.data(0x3000, &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
    program.data(0x1_FFFC, &[0xC1, 0xC2, 0xC3, 0xC4]);
    program
}

/// The writer an origin names.
fn writer(origin: u32) -> u32 {
    WriteOrigin(origin).writer()
}

/// An origin of `writer` from `source`, as the recorder packs it.
fn origin(writer: Writer, source: u32) -> u32 {
    WriteOrigin::new(writer, 0, source).0
}

/// The records of a take.
fn records(writes: &FrameWrites) -> &[[u32; WORDS_PER_RECORD]] {
    let (records, rest) = writes.records.as_chunks::<WORDS_PER_RECORD>();
    assert!(rest.is_empty(), "whole records");
    records
}

/// The VRAM byte indices a fill's cells write: each cell writes the byte
/// at its address with bit 0 flipped.
fn fill_cells(first_cell: usize, cells: usize) -> Vec<usize> {
    (first_cell..first_cell + cells)
        .map(|raw| raw ^ 1)
        .collect()
}

#[test]
fn every_byte_names_its_writer_and_where_it_came_from() {
    let mut program = boot_writes_program();
    let rom = program.build();
    let mut emu = recording(&rom);
    run_frame_on(&mut emu, ExecutionTier::Play);
    let sources = emu.video_memory_sources().expect("armed");
    assert_eq!(sources.origins().len(), 0x1_0000);
    assert_eq!(sources.frames().len(), 0x1_0000);
    let vram = emu.vdp().get_vram().to_vec();
    let at = |index: usize| sources.origins()[index];

    // The transfer from ROM: each byte its own source address.
    for i in 0..32 {
        assert_eq!(at(i), origin(Writer::Transfer, 0x2000 + i as u32));
        assert_eq!(vram[i], rom[0x2000 + i]);
    }
    // From work RAM.
    for i in 0..16 {
        assert_eq!(
            at(0x100 + i),
            origin(Writer::Transfer, 0xFF_0100 + i as u32)
        );
    }
    assert_eq!(vram[0x100..0x104], [0xA0, 0x00, 0xA0, 0x01]);
    // Across the end of the window: the last two words, then the window's
    // first two, which hold the stack pointer the vectors start with.
    let wrapped = [0x1_FFFC, 0x1_FFFD, 0x1_FFFE, 0x1_FFFF, 0, 1, 2, 3];
    for (i, &source) in wrapped.iter().enumerate() {
        assert_eq!(at(0x200 + i), origin(Writer::Transfer, source), "byte {i}");
        assert_eq!(vram[0x200 + i], rom[source as usize], "byte {i}");
    }

    // Stores through the port: the word each was copied from, the second
    // word of a long for both halves, an immediate's own word.
    let immediate = program.address("immediate_store") + 2;
    for (bytes, source) in [
        (0x300..0x302, 0x3000),
        (0x302..0x304, 0x3002),
        (0x304..0x308, 0x3006),
        (0x308..0x30A, immediate),
    ] {
        for index in bytes {
            assert_eq!(at(index), origin(Writer::Processor, source), "{index:#06X}");
        }
    }
    assert_eq!(
        vram[0x300..0x30A],
        [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0xAB, 0xCD]
    );

    // The fill: the store that started it keeps the byte no cell wrote
    // over; every cell's byte names the fill's value.
    let fill_store = program.address("fill_store") + 2;
    let cells = fill_cells(0x401, 32);
    for index in 0x400..0x422 {
        let expected = if cells.contains(&index) {
            origin(Writer::Fill, 0xAB)
        } else if index < 0x402 {
            origin(Writer::Processor, fill_store)
        } else {
            0
        };
        assert_eq!(at(index), expected, "{index:#06X}");
    }
    assert_eq!(vram[0x401], 0x00, "the starting store's low byte");
    assert!(cells.iter().all(|&index| vram[index] == 0xAB));

    // The copy: each byte names the byte it was read from.
    for k in 0..16usize {
        let index = (0x500 + k) ^ 1;
        assert_eq!(
            at(index),
            origin(Writer::Copy, (k ^ 1) as u32),
            "{index:#06X}"
        );
    }
    assert_eq!(vram[0x500..0x510], vram[0x000..0x010]);

    // The store into the sprite table.
    let sprite_store = program.address("sprite_store") + 2;
    assert_eq!(at(0xF000), origin(Writer::Processor, sprite_store));
    assert_eq!(at(0xF001), origin(Writer::Processor, sprite_store));

    // Every byte written was written in the first frame; the rest, never.
    for (index, (&origin, &frame)) in sources.origins().iter().zip(sources.frames()).enumerate() {
        if writer(origin) == Writer::None as u32 {
            assert_eq!((origin, frame), (0, 0), "{index:#06X}");
        } else {
            assert_eq!(frame, 1, "{index:#06X}");
        }
    }

    // A debugger write names the debugger, in the frame drawn last.
    emu.vdp_mut().write_vram_debug(0x6000, &[1, 2, 3]);
    let sources = emu.video_memory_sources().expect("armed");
    for index in 0x6000..0x6003 {
        assert_eq!(sources.origins()[index], origin(Writer::Debugger, 0));
        assert_eq!(sources.frames()[index], 1);
    }
    assert_eq!(sources.origins()[0x6003], 0);
}

#[test]
fn a_frame_s_writes_are_its_transfers_fills_copies_and_port_stores_in_order() {
    let mut program = boot_writes_program();
    let rom = program.build();
    let mut emu = recording(&rom);
    run_frame_on(&mut emu, ExecutionTier::Play);
    let writes = emu.take_frame_writes().expect("armed");
    assert_eq!(writes.dropped, 0);
    let records = records(&writes);
    let kinds: Vec<u32> = records.iter().map(|record| record[0] & 0xFF).collect();
    use WriteKind::*;
    assert_eq!(
        kinds,
        [
            Transfer,
            Transfer,
            Transfer,
            Fill,
            Copy,
            SpriteTableStore,
            PaletteStore,
            Transfer,
            Transfer
        ]
        .map(|kind| kind as u32)
    );
    // (source, target, bytes, start, end) of each.
    let sprite_store = program.address("sprite_store") + 2;
    let colour_store = program.address("colour_store") + 2;
    let expected = [
        (0x2000, WriteTarget::Vram, 32, 0x0000, 0x001E),
        (0xFF_0100, WriteTarget::Vram, 16, 0x0100, 0x010E),
        (0x1_FFFC, WriteTarget::Vram, 8, 0x0200, 0x0206),
        (0xAB, WriteTarget::Vram, 34, 0x0400, 0x0420),
        (0x0000, WriteTarget::Vram, 16, 0x0500, 0x050F),
        (sprite_store, WriteTarget::Vram, 2, 0, 0x00B2),
        (colour_store, WriteTarget::Cram, 2, 3, 0x0E0E),
        (0x2100, WriteTarget::Cram, 8, 8, 11),
        (0x2200, WriteTarget::Vsram, 4, 0, 1),
    ];
    let mut last_line = 0;
    for (record, &(source, target, bytes, start, end)) in records.iter().zip(&expected) {
        assert_eq!(record[1], 1, "the first frame: {record:X?}");
        let line = record[2] & 0xFFFF;
        assert!(line >= last_line && line < 262, "{record:X?}");
        last_line = line;
        assert!(record[2] >> 16 < 489, "cycles into the line: {record:X?}");
        assert_eq!(record[3], source, "{record:X?}");
        assert_eq!(record[4], target as u32 | (bytes << 8), "{record:X?}");
        assert_eq!(
            (record[5], record[6], record[7]),
            (start, end, 0),
            "{record:X?}"
        );
    }
    // The stores name their writer, the 68000.
    for record in &records[5..7] {
        assert_eq!((record[0] >> 16) & 7, Writer::Processor as u32);
    }
    for record in records
        .iter()
        .filter(|record| record[0] & 0xFF != 4 && record[0] & 0xFF != 5)
    {
        assert_eq!(record[0] >> 8, 0, "no writer for a block: {record:X?}");
    }

    // A take empties the record; an idle frame writes nothing.
    assert_eq!(emu.take_frame_writes(), Some(FrameWrites::default()));
    run_frame_on(&mut emu, ExecutionTier::Play);
    assert_eq!(emu.take_frame_writes(), Some(FrameWrites::default()));
}

/// A program that stores colour 1 on line 100 of every frame and, from its
/// vertical interrupt, sends 16 words from ROM to VRAM.
fn paced_program() -> Vec<u8> {
    let mut program = M68kProgram::new(0x4000, 0x200);
    program.vector(30, 0x500);
    #[rustfmt::skip]
    program.org(0x200).words(&[
        0x46FC, 0x2700,
        0x41F9, 0x00C0, 0x0004,         // LEA $C00004,A0
        0x43F9, 0x00C0, 0x0000,         // LEA $C00000,A1
        0x30BC, 0x8004,                 // register 0
        0x30BC, 0x8174,                 // register 1: display, vertical interrupt, transfers, mode 5
        0x30BC, 0x8C81,
        0x30BC, 0x8F02,
        0x46FC, 0x2000,                 // MOVE.W #$2000,SR: interrupts on
    ]).label("frame").words(&[
        0x1039, 0x00C0, 0x0008,         // MOVE.B ($C00008).L,D0: the line
        0x0C00, 0x0064,                 // CMPI.B #100,D0
    ]).branch(0x6600, "frame").words(&[
        0x20BC, 0xC002, 0x0000,         // CRAM colour 1
        0x32BC, 0x0EEE,                 // MOVE.W #$0EEE,(A1)
    ]).label("leave_line").words(&[
        0x1039, 0x00C0, 0x0008,
        0x0C00, 0x0064,
    ]).branch(0x6700, "leave_line").branch(0x6000, "frame");
    #[rustfmt::skip]
    program.org(0x500).words(&[
        0x30BC, 0x9310, 0x30BC, 0x9400,
        0x30BC, 0x9500, 0x30BC, 0x9610, 0x30BC, 0x9700, // ROM $2000
        0x20BC, 0x4000, 0x0080,         // VRAM $0000 by DMA
        0x4E73,                         // RTE
    ]);
    program.build()
}

#[test]
fn taken_at_each_boundary_the_writes_are_what_built_the_frame_held_there() {
    let mut emu = recording(&paced_program());
    for _ in 0..3 {
        run_frame_on(&mut emu, ExecutionTier::Play);
    }
    emu.run_to_capture_boundary();
    let _ = emu.take_frame_writes();
    for _ in 0..3 {
        emu.run_to_capture_boundary();
        let frame = emu.capture_frame_snapshot().frame as u32;
        let writes = emu.take_frame_writes().expect("armed");
        let records = records(&writes);
        assert_eq!(records.len(), 2, "{:X?}", writes.records);
        // The upload of the vertical blank before the frame, then the
        // store made while its picture was drawn.
        let (upload, store) = (records[0], records[1]);
        assert_eq!(upload[0], WriteKind::Transfer as u32);
        assert_eq!(upload[1], frame - 1);
        assert_eq!(upload[2] & 0xFFFF, u32::from(emu.vdp().first_vblank_line()));
        assert_eq!(upload[4], WriteTarget::Vram as u32 | (32 << 8));
        assert_eq!(store[0] & 0xFF, WriteKind::PaletteStore as u32);
        assert_eq!(store[1], frame);
        assert_eq!(store[2] & 0xFFFF, 100);
        assert_eq!((store[5], store[6]), (1, 0x0EEE));
        assert_eq!(emu.take_frame_writes(), Some(FrameWrites::default()));
    }
    // The rest of the held frame: its own upload, in the vertical blank.
    let frame = emu.capture_frame_snapshot().frame as u32;
    run_frame_on(&mut emu, ExecutionTier::Play);
    let rest = emu.take_frame_writes().expect("armed");
    let rest = records(&rest);
    assert_eq!(rest.len(), 1);
    assert_eq!(
        (rest[0][0], rest[0][1]),
        (WriteKind::Transfer as u32, frame)
    );
}

/// A program that stores palette colours as fast as it can, counting them
/// in D2 after each store (at the label `count`).
fn palette_flood_program() -> M68kProgram {
    let mut program = M68kProgram::new(0x4000, 0x200);
    #[rustfmt::skip]
    program.org(0x200).words(&[
        0x46FC, 0x2700,
        0x41F9, 0x00C0, 0x0004,
        0x43F9, 0x00C0, 0x0000,
        0x30BC, 0x8154,                 // register 1: display, transfers, mode 5
        0x30BC, 0x8F02,
        0x20BC, 0xC000, 0x0000,         // CRAM colour 0
        0x7400,                         // MOVEQ #0,D2
    ]).label("store").words(&[
        0x3282,                         // MOVE.W D2,(A1)
    ]).label("count").words(&[
        0x5282,                         // ADDQ.L #1,D2
    ]).branch(0x6000, "store");
    program
}

#[test]
fn past_its_cap_a_take_counts_what_it_dropped() {
    let mut program = palette_flood_program();
    let mut emu = recording(&program.build());
    // The stores made: D2, and one more when a frame ended between a
    // store and its count.
    let count = program.address("count");
    let stores_made = |emu: &GenesisEmulator| emu.cpu().d[2] + u32::from(emu.cpu().pc == count);
    let mut frames = 0;
    while stores_made(&emu) <= 40_000 {
        run_frame_on(&mut emu, ExecutionTier::Play);
        frames += 1;
        assert!(frames < 60);
    }
    let stores = stores_made(&emu);
    let writes = emu.take_frame_writes().expect("armed");
    let kept = writes.records.len() / WORDS_PER_RECORD;
    assert_eq!(kept, sg1_core::vdp::writes::MOST_RECORDS);
    assert_eq!(
        kept as u32 + writes.dropped,
        stores,
        "every store kept or counted"
    );
    // The first stores are the ones kept, in order.
    for (i, record) in records(&writes).iter().take(100).enumerate() {
        assert_eq!(record[5], (i % 64) as u32);
        assert_eq!(record[6], (i as u32) & 0x0EEE);
    }
    // The count starts again with the next take.
    run_frame_on(&mut emu, ExecutionTier::Play);
    let next = emu.take_frame_writes().expect("armed");
    assert_eq!(next.dropped, 0);
    assert_eq!(
        (next.records.len() / WORDS_PER_RECORD) as u32,
        stores_made(&emu) - stores
    );
}

#[test]
fn turned_on_each_starts_empty_and_a_reset_a_load_and_a_new_rom_empty_it() {
    let mut program = boot_writes_program();
    let rom = program.build();
    let mut emu = recording(&rom);
    let empty = |emu: &mut GenesisEmulator| {
        let sources = emu.video_memory_sources().cloned().expect("armed");
        let writes = emu.take_frame_writes().expect("armed");
        sources.origins().iter().all(|&o| o == 0)
            && sources.frames().iter().all(|&f| f == 0)
            && writes == FrameWrites::default()
    };
    assert!(empty(&mut emu));
    run_frame_on(&mut emu, ExecutionTier::Play);
    let state = emu.save_state();
    assert!(!empty(&mut emu), "the first frame wrote");

    // Turned off, nothing is kept; on again, it starts empty.
    emu.set_capture_recording(CaptureRecording::default());
    assert!(emu.video_memory_sources().is_none());
    assert!(emu.take_frame_writes().is_none());
    emu.set_capture_recording(writes_recorded());
    assert_eq!(emu.capture_recording(), writes_recorded());
    assert!(empty(&mut emu));
    // Asked again, a recorder already on keeps what it holds.
    EmulatorCore::reset(&mut emu);
    emu.set_execution_mode(ExecutionMode::Running);
    run_frame_on(&mut emu, ExecutionTier::Play);
    emu.set_capture_recording(writes_recorded());
    assert!(!empty(&mut emu));

    EmulatorCore::reset(&mut emu);
    assert!(empty(&mut emu), "a reset empties them");
    emu.set_execution_mode(ExecutionMode::Running);
    run_frame_on(&mut emu, ExecutionTier::Play);
    emu.load_state(&state, RomCheck::SameRom)
        .expect("the savestate loads");
    assert!(empty(&mut emu), "a savestate load empties them");
    emu.set_execution_mode(ExecutionMode::Running);
    run_frame_on(&mut emu, ExecutionTier::Play);
    emu.load_rom(&paced_program()).expect("another program");
    assert_eq!(emu.capture_recording(), writes_recorded());
    assert!(empty(&mut emu), "a new ROM empties them");
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
fn recording_the_writes_changes_nothing_on_any_frame_loop() {
    for frame_loop in [ExecutionTier::Play, ExecutionTier::LightDebug] {
        for rom in [boot_writes_program().build(), paced_program()] {
            let mut plain = GenesisEmulator::new();
            plain.load_rom(&rom).expect("the program loads");
            plain.set_execution_mode(ExecutionMode::Running);
            plain.set_audio_enabled(true);
            let mut recorded = recording(&rom);
            recorded.set_capture_recording(CaptureRecording {
                line_states: true,
                pixel_sources: true,
                line_sprites: true,
                video_memory_sources: true,
                frame_writes: true,
            });
            let mut audio = (Vec::new(), Vec::new());
            for frame in 0..10 {
                for emu in [&mut plain, &mut recorded] {
                    run_frame_on(emu, frame_loop);
                }
                if frame % 3 == 0 {
                    let _ = recorded.take_frame_writes();
                }
                assert_eq!(
                    recorded.state_hash(),
                    plain.state_hash(),
                    "{frame_loop:?}, frame {frame}"
                );
                assert_eq!(
                    recorded.capture_frame_snapshot().framebuffer,
                    plain.capture_frame_snapshot().framebuffer,
                    "{frame_loop:?}, frame {frame}"
                );
                audio.0.extend(take_audio(&mut plain));
                audio.1.extend(take_audio(&mut recorded));
            }
            assert!(!audio.0.is_empty());
            assert_eq!(audio.1, audio.0, "{frame_loop:?}: the samples");
        }
    }
}

#[test]
fn the_sources_resolve_to_rom_offsets_work_ram_and_nothing() {
    let mut program = boot_writes_program();
    let rom = program.build();
    let mut emu = recording(&rom);
    run_frame_on(&mut emu, ExecutionTier::Play);
    let sources = emu.video_memory_sources().expect("armed");
    let hash = emu.state_hash();
    let addresses: Vec<u32> = sources.origins()[..0x10]
        .iter()
        .chain(&sources.origins()[0x100..0x104])
        .chain(&sources.origins()[0x200..0x208])
        .copied()
        .collect();
    let resolved = emu.resolve_sources(&addresses);
    let mut expected: Vec<i32> = (0x2000..0x2010).collect();
    expected.extend([SOURCE_WORK_RAM; 4]);
    expected.extend([0x1_FFFC, 0x1_FFFD, 0x1_FFFE, 0x1_FFFF, 0, 1, 2, 3]);
    assert_eq!(resolved, expected, "bits 24-31 of an origin are ignored");
    assert_eq!(
        emu.resolve_sources(&[
            0xA0_0000,
            0xC0_0000,
            0x80_0000,
            0x2_0000,
            WriteOrigin::SOURCE_MASK
        ]),
        [
            SOURCE_UNKNOWN,
            SOURCE_UNKNOWN,
            SOURCE_UNKNOWN,
            0,
            SOURCE_WORK_RAM
        ],
        "Z80 space, the video ports and past the cartridge window name no file; \
         the 128 KB ROM shows again past its end"
    );
    assert_eq!(emu.state_hash(), hash, "resolving changes nothing");
}

/// A program on a 5 MB cartridge (the SEGA mapper's 512 KB banks) that
/// switches bank 9 into slot 6 ($300000) and sends 8 words from there to
/// VRAM.
fn banked_program() -> Vec<u8> {
    let mut program = M68kProgram::new(0x50_0000, 0x200);
    #[rustfmt::skip]
    program.org(0x200).words(&[
        0x46FC, 0x2700,
        0x41F9, 0x00C0, 0x0004,
        0x30BC, 0x8114, 0x30BC, 0x8C81, 0x30BC, 0x8F02,
        0x13FC, 0x0009, 0x00A1, 0x30FD, // MOVE.B #9,($A130FD).L: slot 6 is bank 9
        0x30BC, 0x9308, 0x30BC, 0x9400,
        0x30BC, 0x9500, 0x30BC, 0x9600, 0x30BC, 0x9718, // source $300000
        0x20BC, 0x4000, 0x0080,         // VRAM $0000 by DMA
    ]).label("idle").branch(0x6000, "idle");
    program.data(0x30_0000, &[0x66; 16]);
    let bank_nine: Vec<u8> = (0..16).map(|i| 0x90 + i).collect();
    program.data(0x48_0000, &bank_nine);
    program.build()
}

#[test]
fn a_banked_source_resolves_through_the_banks_as_they_stand() {
    let rom = banked_program();
    let mut emu = recording(&rom);
    run_frame_on(&mut emu, ExecutionTier::Play);
    let sources = emu.video_memory_sources().expect("armed");
    let vram = emu.vdp().get_vram();
    for i in 0..16 {
        assert_eq!(
            sources.origins()[i],
            origin(Writer::Transfer, 0x30_0000 + i as u32)
        );
        assert_eq!(vram[i], rom[0x48_0000 + i], "bank 9 was read");
    }
    let addresses: Vec<u32> = sources.origins()[..16].to_vec();
    let expected: Vec<i32> = (0x48_0000..0x48_0010).collect();
    assert_eq!(emu.resolve_sources(&addresses), expected);
    // Slot 0 is bank 0, as the mapper starts.
    assert_eq!(emu.resolve_sources(&[0x00_0100]), [0x100]);

    // The banks as they stand: slot 6 back to bank 6.
    emu.bus_mut().write(0xA1_30FD, 6);
    assert_eq!(emu.resolve_sources(&[0x30_0004]), [0x30_0004]);
}

#[test]
fn a_coprocessor_s_memory_is_named_as_such() {
    // The SVP cartridge's DRAM window.
    let mut rom = boot_writes_program().build();
    rom[0x1C8..0x1CA].copy_from_slice(b"SV");
    let mut emu = GenesisEmulator::new();
    emu.load_rom(&rom).expect("an SVP cartridge");
    assert_eq!(
        emu.resolve_sources(&[0x30_0000, 0x00_0100]),
        [SOURCE_COPROCESSOR, 0x100]
    );
    let empty = GenesisEmulator::new();
    assert_eq!(
        empty.resolve_sources(&[0x100]),
        [SOURCE_UNKNOWN],
        "no cartridge"
    );
}
