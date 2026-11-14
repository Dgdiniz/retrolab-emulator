//! The capture boundary: where a frame's picture is drawn and vertical
//! blanking has not begun, so video memory and the framebuffer belong to the
//! same frame.
//!
//! The program below draws one tile at the top left of plane A, plays a
//! tone on the PSG and on the YM2612, and in its vertical interrupt gives
//! the tile its other colour and moves both tones: the frame's picture shows
//! one colour while VRAM, once the frame has run whole, already holds the
//! next. Every check here runs it, each frame call against the same frame
//! run whole.

use retro_core::types::{Breakpoint, BreakpointType};
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode, ExecutionTier};
use sg1_core::debugger::GenesisEvent;
use sg1_core::vdp::{Vdp, LAYER_PLANE_A, LAYER_PLANE_B, LAYER_SPRITES, LAYER_WINDOW};
use sg1_core::GenesisEmulator;

/// Frames run before a check, so the program has set the VDP up and its
/// vertical interrupt has run.
const WARM_UP: u64 = 4;

/// Frames each comparison runs.
const FRAMES: u64 = 12;

/// Write `words` into `rom` from byte `address` on, big-endian.
fn place(rom: &mut [u8], address: usize, words: &[u16]) {
    for (i, word) in words.iter().enumerate() {
        rom[address + i * 2..address + i * 2 + 2].copy_from_slice(&word.to_be_bytes());
    }
}

/// The program every check runs.
fn program_rom() -> Vec<u8> {
    let mut rom = vec![0xFF_u8; 0x1000];
    rom[0x000..0x004].copy_from_slice(&0x00FF_FE00_u32.to_be_bytes()); // SSP
    rom[0x004..0x008].copy_from_slice(&0x0000_0200_u32.to_be_bytes()); // PC
    rom[0x078..0x07C].copy_from_slice(&0x0000_0300_u32.to_be_bytes()); // vertical interrupt

    let header = &mut rom[0x100..0x200];
    header[0x00..0x10].copy_from_slice(b"SEGA MEGA DRIVE ");
    header[0x10..0x20].copy_from_slice(b"(C)TEST 2026.NOV");
    let mut title = [b' '; 48];
    title[..16].copy_from_slice(b"CAPTURE BOUNDARY");
    header[0x20..0x50].copy_from_slice(&title);
    header[0x50..0x80].copy_from_slice(&title);
    header[0x80..0x8E].copy_from_slice(b"GM 00000000-00");
    header[0x90..0xA0].copy_from_slice(b"J               ");
    header[0xA0..0xA4].copy_from_slice(&0x0000_0000_u32.to_be_bytes()); // ROM start
    header[0xA4..0xA8].copy_from_slice(&0x0000_0FFF_u32.to_be_bytes()); // ROM end
    header[0xA8..0xAC].copy_from_slice(&0x00FF_0000_u32.to_be_bytes()); // RAM start
    header[0xAC..0xB0].copy_from_slice(&0x00FF_FFFF_u32.to_be_bytes()); // RAM end
    header[0xB0..0xF0].fill(b' ');
    header[0xF0..0x100].copy_from_slice(b"JUE             ");

    #[rustfmt::skip]
    place(&mut rom, 0x200, &[
        0x46FC, 0x2700,                         // MOVE.W #$2700,SR
        0x41F9, 0x00C0, 0x0004,                 // LEA $C00004,A0: control port
        0x43F9, 0x00C0, 0x0000,                 // LEA $C00000,A1: data port
        0x30BC, 0x8004,                         // MOVE.W #$8004,(A0): register 0
        0x30BC, 0x8164,                         // register 1: display, vertical interrupt, mode 5
        0x30BC, 0x8230,                         // register 2: plane A at $C000
        0x30BC, 0x8407,                         // register 4: plane B at $E000
        0x30BC, 0x8578,                         // register 5: sprites at $F000
        0x30BC, 0x8700,                         // register 7: backdrop colour 0
        0x30BC, 0x8C81,                         // register 12: 40 cells
        0x30BC, 0x8D3F,                         // register 13: horizontal scroll at $FC00
        0x30BC, 0x8F02,                         // register 15: auto-increment 2
        0x30BC, 0x9001,                         // register 16: planes of 64x32 cells
        0x20BC, 0xC002, 0x0000,                 // MOVE.L #$C0020000,(A0): CRAM at colour 1
        0x32BC, 0x000E,                         // MOVE.W #$000E,(A1): colour 1, red
        0x32BC, 0x0E00,                         // MOVE.W #$0E00,(A1): colour 2, blue
        0x23FC, 0x1111, 0x1111, 0x00FF, 0x0004, // MOVE.L #$11111111,($FF0004).L: the tile row
        0x4EB9, 0x0000, 0x0380,                 // JSR $000380: put the tile
        0x20BC, 0x4000, 0x0003,                 // MOVE.L #$40000003,(A0): VRAM at $C000
        0x32BC, 0x0001,                         // MOVE.W #$0001,(A1): plane A cell 0 is tile 1
        0x13FC, 0x008F, 0x00C0, 0x0011,         // MOVE.B #$8F,($C00011).L: PSG tone 0
        0x13FC, 0x0008, 0x00C0, 0x0011,         // MOVE.B #$08,($C00011).L
        0x13FC, 0x0090, 0x00C0, 0x0011,         // MOVE.B #$90,($C00011).L: its volume
        0x33FC, 0x0100, 0x00A1, 0x1100,         // MOVE.W #$0100,($A11100).L: the Z80 bus
        0x45F9, 0x0000, 0x0400,                 // LEA $000400,A2: the YM2612 table
        0x7024,                                 // MOVEQ #36,D0
        0x13DA, 0x00A0, 0x4000,                 // MOVE.B (A2)+,($A04000).L: register
        0x13DA, 0x00A0, 0x4001,                 // MOVE.B (A2)+,($A04001).L: value
        0x51C8, 0xFFF2,                         // DBRA D0,-14
        0x46FC, 0x2000,                         // MOVE.W #$2000,SR: interrupts on
        0x60FE,                                 // BRA *
    ]);

    // The vertical interrupt: count the frame, give the tile its other
    // colour, move the PSG and YM2612 tones.
    #[rustfmt::skip]
    place(&mut rom, 0x300, &[
        0x5279, 0x00FF, 0x0000,                 // ADDQ.W #1,($FF0000).L
        0x0AB9, 0x3333, 0x3333, 0x00FF, 0x0004, // EORI.L #$33333333,($FF0004).L
        0x4EB9, 0x0000, 0x0380,                 // JSR $000380: put the tile
        0x1239, 0x00FF, 0x0001,                 // MOVE.B ($FF0001).L,D1: the count
        0x0201, 0x003F,                         // ANDI.B #$3F,D1
        0x13FC, 0x008F, 0x00C0, 0x0011,         // MOVE.B #$8F,($C00011).L
        0x13C1, 0x00C0, 0x0011,                 // MOVE.B D1,($C00011).L: PSG tone 0
        0x13FC, 0x00A0, 0x00A0, 0x4000,         // MOVE.B #$A0,($A04000).L
        0x13C1, 0x00A0, 0x4001,                 // MOVE.B D1,($A04001).L: FM frequency
        0x4E73,                                 // RTE
    ]);

    // Write the tile row kept in RAM over all eight rows of tile 1.
    #[rustfmt::skip]
    place(&mut rom, 0x380, &[
        0x2239, 0x00FF, 0x0004,                 // MOVE.L ($FF0004).L,D1
        0x23FC, 0x4020, 0x0000, 0x00C0, 0x0004, // MOVE.L #$40200000,($C00004).L: VRAM at $0020
        0x7007,                                 // MOVEQ #7,D0
        0x23C1, 0x00C0, 0x0000,                 // MOVE.L D1,($C00000).L
        0x51C8, 0xFFF8,                         // DBRA D0,-8
        0x4E75,                                 // RTS
    ]);

    // YM2612 register and value pairs: channel 1 on algorithm 7 with its
    // fourth operator loud, keyed on.
    let mut ym: Vec<u8> = vec![0x22, 0x00, 0x27, 0x00, 0x2B, 0x00, 0x28, 0x00];
    for operator in [0x00u8, 0x04, 0x08, 0x0C] {
        let level = if operator == 0x0C { 0x18 } else { 0x7F };
        for (register, value) in [
            (0x30, 0x01),
            (0x40, level),
            (0x50, 0x1F),
            (0x60, 0x00),
            (0x70, 0x00),
            (0x80, 0x0F),
            (0x90, 0x00),
        ] {
            ym.extend_from_slice(&[register + operator, value]);
        }
    }
    ym.extend_from_slice(&[0xB0, 0x07, 0xB4, 0xC0, 0xA4, 0x22, 0xA0, 0x69, 0x28, 0xF0]);
    assert_eq!(
        ym.len(),
        37 * 2,
        "the table holds the 37 pairs the loop writes"
    );
    rom[0x400..0x400 + ym.len()].copy_from_slice(&ym);
    rom
}

/// A machine running the program, its audio on.
fn emulator() -> GenesisEmulator {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(&program_rom()).expect("the program loads");
    emu.set_execution_mode(ExecutionMode::Running);
    emu.set_audio_enabled(true);
    emu
}

/// The samples the frames so far left for the host.
fn take_audio(emu: &mut GenesisEmulator) -> Vec<u32> {
    emu.get_audio_buffer().iter().map(|s| s.to_bits()).collect()
}

/// One whole frame on `frame_loop`.
fn run_whole(emu: &mut GenesisEmulator, frame_loop: ExecutionTier) {
    match frame_loop {
        ExecutionTier::Fast => emu.run_frame_fast(),
        ExecutionTier::LightDebug => emu.run_frame_debug_light(),
        ExecutionTier::FullDebug => EmulatorCore::run_frame(emu),
    }
}

/// The colour of pixel (0, 0) of the picture, and the colour tile 1's first
/// pixel has in the snapshot's memory: the two agree when the memory is the
/// one the picture was drawn from.
fn shown_and_held_colour(emu: &GenesisEmulator) -> ((u8, u8, u8), (u8, u8, u8)) {
    let snapshot = emu.capture_frame_snapshot();
    let shown = (
        snapshot.framebuffer[0],
        snapshot.framebuffer[1],
        snapshot.framebuffer[2],
    );
    let colour_index = (snapshot.vram[0x20] >> 4) as usize;
    let held = Vdp::convert_genesis_color(snapshot.palette[colour_index]);
    (shown, held)
}

#[test]
fn the_boundary_holds_the_memory_the_picture_was_drawn_from() {
    let mut emu = emulator();
    for _ in 0..WARM_UP {
        emu.run_frame_fast();
    }
    for _ in 0..FRAMES {
        emu.run_to_capture_boundary();
        assert!(emu.at_capture_boundary());
        let snapshot = emu.capture_frame_snapshot();
        assert!(snapshot.at_boundary);
        assert_eq!(
            snapshot.frame,
            emu.frame_count() + 1,
            "counted once it ends"
        );
        assert_eq!((snapshot.width, snapshot.height), (320, 224));
        assert_eq!(snapshot.framebuffer.len(), 320 * 224 * 4);
        assert_eq!(snapshot.vram.len(), 0x10000);
        assert_eq!(snapshot.palette.len(), 64);
        assert_eq!(snapshot.vsram.len(), 64);
        assert_eq!(snapshot.registers.len(), 24);
        assert_eq!(snapshot.registers[12].value, 0x81);
        let (shown, held) = shown_and_held_colour(&emu);
        assert_eq!(shown, held, "at the boundary the tile is as it was drawn");

        // The frame run to its end: VRAM has the tile the vertical interrupt
        // put for the next frame, the picture the one just drawn.
        emu.run_frame_fast();
        assert!(!emu.capture_frame_snapshot().at_boundary);
        let (shown_after, held_after) = shown_and_held_colour(&emu);
        assert_eq!(shown_after, shown, "VBlank lines draw nothing");
        assert_ne!(
            held_after, shown_after,
            "the memory moved on to the next frame"
        );
    }
}

#[test]
fn each_call_moves_one_frame_and_lands_on_a_boundary() {
    let mut emu = emulator();
    let mut whole = emulator();
    for frame in 0..=FRAMES {
        emu.run_to_capture_boundary();
        assert!(emu.at_capture_boundary());
        assert_eq!(
            emu.frame_count(),
            frame,
            "a held frame is counted when it ends"
        );
        // Held, the frame shows the picture it shows once it ends.
        whole.run_frame_fast();
        assert_eq!(
            emu.capture_frame_snapshot().framebuffer,
            whole.capture_frame_snapshot().framebuffer,
            "frame {frame}"
        );
    }
    // The VDP stands at the end of the picture, before the VBlank line.
    let last_drawn = emu.vdp().first_vblank_line() - 1;
    assert_eq!(emu.vdp().timing().scanline, last_drawn);
}

#[test]
fn a_frame_split_at_its_boundary_sounds_and_ends_as_a_whole_frame() {
    for frame_loop in [
        ExecutionTier::Fast,
        ExecutionTier::LightDebug,
        ExecutionTier::FullDebug,
    ] {
        let mut whole = emulator();
        let mut split = emulator();
        for _ in 0..WARM_UP {
            run_whole(&mut whole, frame_loop);
            run_whole(&mut split, frame_loop);
        }
        take_audio(&mut whole);
        take_audio(&mut split);

        let (mut whole_audio, mut split_audio) = (Vec::new(), Vec::new());
        for frame in 0..FRAMES {
            run_whole(&mut whole, frame_loop);
            whole_audio.extend(take_audio(&mut whole));

            split.run_to_capture_boundary_with(frame_loop);
            assert!(split.at_capture_boundary());
            assert!(
                take_audio(&mut split).is_empty(),
                "the audio waits for the frame's end"
            );
            run_whole(&mut split, frame_loop);
            assert!(!split.at_capture_boundary());
            split_audio.extend(take_audio(&mut split));

            assert_eq!(
                split.state_hash(),
                whole.state_hash(),
                "{frame_loop:?}, frame {frame}: the machine"
            );
            assert_eq!(
                split.get_framebuffer().data,
                whole.get_framebuffer().data,
                "{frame_loop:?}, frame {frame}: the picture"
            );
        }
        assert!(!whole_audio.is_empty());
        assert!(
            whole_audio.iter().any(|&bits| f32::from_bits(bits) != 0.0),
            "{frame_loop:?}: the program makes a sound"
        );
        assert_eq!(
            split_audio, whole_audio,
            "{frame_loop:?}: the same samples, bit for bit"
        );
    }
}

#[test]
fn every_frame_call_finishes_the_held_frame() {
    type FrameCall = fn(&mut GenesisEmulator);
    // Each call, and the loop it runs. Headless is the fast loop with nothing
    // shown, so it leaves the machine as the fast loop does.
    let calls: [(&str, FrameCall, ExecutionTier); 4] = [
        ("fast", GenesisEmulator::run_frame_fast, ExecutionTier::Fast),
        (
            "headless",
            GenesisEmulator::run_frame_headless,
            ExecutionTier::Fast,
        ),
        (
            "light",
            GenesisEmulator::run_frame_debug_light,
            ExecutionTier::LightDebug,
        ),
        (
            "full",
            |emu: &mut GenesisEmulator| EmulatorCore::run_frame(emu),
            ExecutionTier::FullDebug,
        ),
    ];
    for (name, call, frame_loop) in calls {
        let mut whole = emulator();
        let mut held = emulator();
        for _ in 0..WARM_UP {
            run_whole(&mut whole, frame_loop);
            run_whole(&mut held, frame_loop);
        }
        run_whole(&mut whole, frame_loop);
        held.run_to_capture_boundary_with(frame_loop);
        let count = held.frame_count();
        call(&mut held);
        assert!(!held.at_capture_boundary(), "{name}");
        assert_eq!(
            held.frame_count(),
            count + 1,
            "{name}: the rest of one frame"
        );
        assert_eq!(held.state_hash(), whole.state_hash(), "{name}");
    }
}

#[test]
fn run_until_vblank_stops_at_the_boundary_on_the_full_loop() {
    let mut emu = emulator();
    let mut whole = emulator();
    for _ in 0..WARM_UP {
        EmulatorCore::run_frame(&mut emu);
        EmulatorCore::run_frame(&mut whole);
    }
    emu.run_until_vblank();
    assert!(emu.at_capture_boundary());
    emu.run_until_vblank();
    assert!(emu.at_capture_boundary());
    EmulatorCore::run_frame(&mut emu);
    for _ in 0..2 {
        EmulatorCore::run_frame(&mut whole);
    }
    assert_eq!(emu.state_hash(), whole.state_hash());
}

#[test]
fn a_savestate_at_the_boundary_finishes_the_frame_first() {
    let mut emu = emulator();
    let mut whole = emulator();
    for _ in 0..WARM_UP {
        emu.run_frame_fast();
        whole.run_frame_fast();
    }
    whole.run_frame_fast();
    emu.run_to_capture_boundary();

    assert!(
        EmulatorCore::save_state(&emu).is_empty(),
        "a held frame has no savestate: it needs finishing first"
    );
    let state = emu.finish_frame_and_save_state();
    assert!(!state.is_empty());
    assert!(!emu.at_capture_boundary(), "the frame was finished");
    assert_eq!(emu.frame_count(), whole.frame_count());
    assert_eq!(emu.state_hash(), whole.state_hash());

    // The savestate is that of the finished frame, and goes on as it does.
    let mut restored = emulator();
    restored.load_state(&state).expect("the savestate loads");
    assert_eq!(restored.state_hash(), whole.state_hash());
    restored.run_frame_fast();
    whole.run_frame_fast();
    assert_eq!(restored.state_hash(), whole.state_hash());

    // Taken where no frame is held, nothing runs first.
    let count = emu.frame_count();
    assert!(!emu.finish_frame_and_save_state().is_empty());
    assert_eq!(emu.frame_count(), count);
}

#[test]
fn a_reset_a_savestate_load_and_a_rom_load_drop_the_held_frame() {
    let mut emu = emulator();
    emu.run_frame_fast();
    let state = emu.finish_frame_and_save_state();

    emu.run_to_capture_boundary();
    EmulatorCore::reset(&mut emu);
    assert!(!emu.at_capture_boundary());

    emu.set_execution_mode(ExecutionMode::Running);
    emu.run_to_capture_boundary();
    emu.load_state(&state).expect("the savestate loads");
    assert!(!emu.at_capture_boundary());

    emu.run_to_capture_boundary();
    emu.load_rom(&program_rom()).expect("the program loads");
    assert!(!emu.at_capture_boundary());

    // A refused savestate changes nothing, the held frame included.
    emu.set_execution_mode(ExecutionMode::Running);
    emu.run_to_capture_boundary();
    assert!(emu.load_state(&[1, 2, 3]).is_err());
    assert!(emu.at_capture_boundary());
}

/// Arm an event breakpoint and return its id.
fn break_on(emu: &mut GenesisEmulator, event: GenesisEvent) -> u32 {
    let (code, param) = event.to_code_param();
    emu.debugger().add_breakpoint(Breakpoint {
        address: 0,
        bp_type: BreakpointType::Event { code, param },
        cpu_id: None,
        condition: None,
    })
}

#[test]
fn an_event_breakpoint_stop_goes_on_where_it_stopped() {
    for frame_loop in [ExecutionTier::LightDebug, ExecutionTier::FullDebug] {
        for event in [GenesisEvent::Scanline(100), GenesisEvent::VBlank] {
            let mut whole = emulator();
            let mut stopped = emulator();
            for _ in 0..WARM_UP {
                run_whole(&mut whole, frame_loop);
                run_whole(&mut stopped, frame_loop);
            }
            take_audio(&mut whole);
            take_audio(&mut stopped);

            let id = break_on(&mut stopped, event);
            run_whole(&mut stopped, frame_loop);
            assert_eq!(
                stopped.debugger().get_execution_mode(),
                ExecutionMode::Paused,
                "{frame_loop:?} {event:?}: the breakpoint stopped the frame"
            );
            let line = match event {
                GenesisEvent::Scanline(line) => line,
                _ => stopped.vdp().first_vblank_line(),
            };
            assert_eq!(stopped.vdp().timing().scanline, line);
            assert!(
                !stopped.at_capture_boundary(),
                "an event stop is no boundary"
            );
            assert_eq!(stopped.frame_count(), whole.frame_count());

            // Going on runs the rest of that frame, not a new one.
            stopped.debugger().remove_breakpoint(id);
            stopped.set_execution_mode(ExecutionMode::Running);
            run_whole(&mut stopped, frame_loop);
            run_whole(&mut whole, frame_loop);
            assert_eq!(
                stopped.state_hash(),
                whole.state_hash(),
                "{frame_loop:?} {event:?}"
            );
            assert_eq!(
                take_audio(&mut stopped),
                take_audio(&mut whole),
                "{frame_loop:?} {event:?}: the frame's samples"
            );
        }
    }
}

#[test]
fn a_boundary_call_goes_on_from_an_event_stop_to_the_frames_boundary() {
    let mut emu = emulator();
    let mut whole = emulator();
    for _ in 0..WARM_UP {
        emu.run_frame_fast();
        whole.run_frame_fast();
    }
    let id = break_on(&mut emu, GenesisEvent::Scanline(100));
    emu.run_frame_debug_light();
    emu.debugger().remove_breakpoint(id);
    emu.set_execution_mode(ExecutionMode::Running);
    let count = emu.frame_count();

    emu.run_to_capture_boundary();
    assert!(emu.at_capture_boundary());
    assert_eq!(
        emu.frame_count(),
        count,
        "the same frame, held at its boundary"
    );
    emu.run_frame_fast();
    whole.run_frame_fast();
    assert_eq!(emu.state_hash(), whole.state_hash());
}

#[test]
fn a_step_at_the_boundary_runs_where_the_frame_is_held() {
    let mut emu = emulator();
    let mut whole = emulator();
    for _ in 0..WARM_UP {
        emu.run_frame_fast();
        whole.run_frame_fast();
    }
    whole.run_frame_fast();
    emu.run_to_capture_boundary();
    let line = emu.vdp().timing().scanline;

    // A thousand instructions, more than a line's worth of 68000 time.
    for _ in 0..1000 {
        assert!(emu.step().instruction_executed);
    }
    assert!(emu.at_capture_boundary(), "the frame stays held");
    assert_eq!(
        emu.vdp().timing().scanline,
        line,
        "the beam stays where it stopped"
    );
    assert!(!emu.vdp().in_vblank(), "no VBlank before the frame goes on");

    // The frame then ends where a whole frame ends: the stepped time came
    // out of the rest of it.
    emu.run_frame_fast();
    assert_eq!(emu.frame_count(), whole.frame_count());
    assert_eq!(emu.vdp().timing().scanline, whole.vdp().timing().scanline);
    let apart = emu.master_cycles().abs_diff(whole.master_cycles());
    assert!(
        apart < 200,
        "68000 time differs by {apart} cycles: more than where the last instruction ends"
    );
    // And it took its VBlank as a whole frame does.
    assert_eq!(
        emu.bus().memory().work_ram.as_slice()[..2],
        whole.bus().memory().work_ram.as_slice()[..2],
        "the vertical interrupt counted the frame"
    );
}

#[test]
fn a_step_at_an_event_stop_runs_the_handler_where_the_frame_is_held() {
    let mut emu = emulator();
    let mut whole = emulator();
    for _ in 0..WARM_UP {
        EmulatorCore::run_frame(&mut emu);
        EmulatorCore::run_frame(&mut whole);
    }
    let id = break_on(&mut emu, GenesisEvent::VBlank);
    EmulatorCore::run_frame(&mut emu);
    emu.debugger().remove_breakpoint(id);
    let line = emu.vdp().first_vblank_line();
    assert_eq!(
        emu.vdp().timing().scanline,
        line,
        "stopped on the VBlank line"
    );

    // Stepped through the full loop's step mode, the 68000 runs on where
    // the frame is held: the vertical interrupt lands part of the way into
    // the line and its handler runs, while the beam stays on the line. A
    // step that takes the interrupt runs the handler's first instruction.
    let in_handler = |emu: &GenesisEmulator| emu.cpu().sr & 0x0700 == 0x0600;
    let mut steps = 0;
    while !in_handler(&emu) {
        emu.set_execution_mode(ExecutionMode::StepInto);
        EmulatorCore::run_frame(&mut emu);
        assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::Paused);
        steps += 1;
        assert!(steps < 100, "the vertical interrupt is taken");
    }
    assert!(steps > 1, "the interrupt waits for its point in the line");
    assert_eq!(
        emu.cpu().pc & 0x00FF_FFFF,
        0x306,
        "the handler's first instruction ran"
    );
    for _ in 0..2 {
        emu.set_execution_mode(ExecutionMode::StepInto);
        EmulatorCore::run_frame(&mut emu);
    }
    assert_eq!(
        emu.cpu().pc & 0x00FF_FFFF,
        0x380,
        "and on into its subroutine"
    );
    assert_eq!(emu.vdp().timing().scanline, line);

    // The debugger paused: a frame call runs nothing.
    emu.set_execution_mode(ExecutionMode::Paused);
    let hash = emu.state_hash();
    EmulatorCore::run_frame(&mut emu);
    assert_eq!(emu.state_hash(), hash);

    // Going on finishes that frame.
    emu.set_execution_mode(ExecutionMode::Running);
    EmulatorCore::run_frame(&mut emu);
    EmulatorCore::run_frame(&mut whole);
    assert_eq!(emu.frame_count(), whole.frame_count());
    assert_eq!(
        emu.bus().memory().work_ram.as_slice()[..2],
        whole.bus().memory().work_ram.as_slice()[..2],
        "the vertical interrupt counted the frame once"
    );
}

#[test]
fn a_layer_is_drawn_alone_and_changes_nothing() {
    let mut emu = emulator();
    for _ in 0..WARM_UP {
        emu.run_frame_fast();
    }
    emu.run_to_capture_boundary();
    let hash = emu.state_hash();
    let shown = emu.get_framebuffer().data.clone();
    let (tile_colour, _) = shown_and_held_colour(&emu);

    let plane_a = emu
        .render_layer(LAYER_PLANE_A as u32)
        .expect("plane A")
        .image;
    assert_eq!((plane_a.width, plane_a.height), (320, 224));
    assert_eq!(
        plane_a.rgba[..4],
        [tile_colour.0, tile_colour.1, tile_colour.2, 255],
        "tile 1 at the top left"
    );
    assert_eq!(
        plane_a.rgba[8 * 4..8 * 4 + 4],
        [0, 0, 0, 0],
        "cell 1 is tile 0"
    );

    for layer in [LAYER_PLANE_B, LAYER_WINDOW, LAYER_SPRITES] {
        let image = emu.render_layer(layer as u32).expect("a layer").image;
        assert!(
            image.rgba.iter().all(|&byte| byte == 0),
            "layer {layer} covers nothing: every pixel transparent"
        );
    }
    assert!(emu.render_layer(4).is_none());

    assert_eq!(emu.state_hash(), hash, "nothing a savestate holds moved");
    assert_eq!(
        emu.get_framebuffer().data,
        shown,
        "the host's picture stayed"
    );
    assert!(emu.at_capture_boundary());

    // And the frame still ends as it would have.
    let mut whole = emulator();
    for _ in 0..=WARM_UP {
        whole.run_frame_fast();
    }
    emu.run_frame_fast();
    assert_eq!(emu.state_hash(), whole.state_hash());
}

/// Put `count` 8x8 sprites of tile 1 side by side on the lines 16 to 23,
/// linked in a chain from slot 0, in the sprite table at `$F000`.
fn put_sprites_in_a_row(emu: &mut GenesisEmulator, count: u8) {
    let mut table = Vec::new();
    for slot in 0..count {
        let link = if slot + 1 < count { slot + 1 } else { 0 };
        let x = 0x80 + u16::from(slot) * 8;
        table.extend_from_slice(&[0x00, 0x90, 0x00, link, 0x00, 0x01]);
        table.extend_from_slice(&x.to_be_bytes());
    }
    emu.vdp_mut().write_vram_debug(0xF000, &table);
}

#[test]
fn a_repaint_and_a_layer_move_no_saved_state_on_a_frame_with_sprite_overflow() {
    let mut emu = emulator();
    for _ in 0..WARM_UP {
        emu.run_frame_fast();
    }
    // 21 sprites on one line: one past the 40-cell mode's 20.
    put_sprites_in_a_row(&mut emu, 21);
    emu.run_to_capture_boundary();
    assert_ne!(
        emu.vdp().status_word() & 0x0040,
        0,
        "the frame drew too many sprites on a line"
    );
    // A status read clears the flag, as a game's does; drawing the frame
    // again would raise it.
    emu.vdp_mut().read_word_port(0x04);
    assert_eq!(emu.vdp().status_word() & 0x0040, 0);
    let hash = emu.state_hash();

    emu.repaint_held_frame();
    assert_eq!(emu.state_hash(), hash, "a repaint");
    for layer in 0..4 {
        emu.render_layer(layer);
        assert_eq!(emu.state_hash(), hash, "layer {layer}");
    }
    assert_eq!(emu.vdp().status_word() & 0x0040, 0);

    let sprites = emu
        .render_layer(LAYER_SPRITES as u32)
        .expect("the sprites")
        .image;
    let pixel = |x: usize, y: usize| &sprites.rgba[(y * 320 + x) * 4..(y * 320 + x) * 4 + 4];
    assert_eq!(pixel(0, 16)[3], 255, "the first sprite");
    assert_eq!(pixel(159, 16)[3], 255, "the twentieth");
    assert_eq!(
        pixel(160, 16)[3],
        0,
        "the twenty-first is past the line's limit"
    );
    assert_eq!(pixel(0, 15)[3], 0, "above the row");
}

#[test]
fn the_display_info_gives_the_mode_s_width_and_the_lines_drawn() {
    let mut emu = emulator();
    for _ in 0..WARM_UP {
        emu.run_frame_fast();
    }
    let size = |emu: &GenesisEmulator| {
        let info = emu.get_display_info();
        (info.width, info.height)
    };
    assert_eq!(size(&emu), (320, 224));

    // 32 cells.
    emu.vdp_mut().write_word_port(0x04, 0x8C00);
    emu.run_frame_fast();
    assert_eq!(size(&emu), (256, 224));
    assert_eq!(emu.get_framebuffer().width, 256);

    // 30-cell lines: the framebuffer draws 224 of them, so the picture is 224
    // high whatever the mode asks.
    emu.vdp_mut().write_word_port(0x04, 0x816C);
    emu.run_frame_fast();
    assert_eq!(size(&emu), (256, 224));
    assert_eq!(
        emu.capture_frame_snapshot().framebuffer.len(),
        256 * 224 * 4
    );
}
