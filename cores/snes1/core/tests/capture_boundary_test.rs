//! The capture boundary: a frame call that stops once the frame's active
//! display is drawn, in front of its `VBlankStart`, and the frames, steps,
//! savestates and redraws that go on from there.
//!
//! The synthetic program below draws a screen of nothing but the backdrop
//! (CGRAM colour 0) and, from its NMI handler, writes a new backdrop colour
//! every vertical blank. The picture a frame shows is therefore drawn with
//! the colour the previous vertical blank wrote: a snapshot of CGRAM taken
//! at the boundary must match the picture, and one taken after the whole
//! frame must not. The main loop reads the APU's first port without end,
//! so the APU is caught up all through the frame.

use retro_core::cpu::Addressable;
use retro_core::{Breakpoint, BreakpointType, EmulatorCore, ExecutionMode};
use snes1_core::apu::Apu;
use snes1_core::debugger::snes_events;
use snes1_core::{FrameSnapshot, SnesEmulator};

// -------------------------------------------------------------
// The program
// -------------------------------------------------------------

/// `SEI` / `LDA #$0F` / `STA $2100` (display on, full brightness) /
/// `LDA #$80` / `STA $4200` (NMI on) / `LDA $2140` / `BRA -5` (read the
/// APU port forever), at `$00:8000`.
const RESET_CODE: [u8; 16] = [
    0x78, 0xA9, 0x0F, 0x8D, 0x00, 0x21, 0xA9, 0x80, 0x8D, 0x00, 0x42, 0xAD, 0x40, 0x21, 0x80, 0xFB,
];

/// At `$00:F000`: `PHA` / `INC $10` / `STZ $2121` / `LDA $10` /
/// `STA $2122` / `STZ $2122` / `PLA` / `RTI`. Colour 0 becomes the frame
/// count kept at `$10`, a new backdrop each vertical blank.
const NMI_HANDLER: [u8; 16] = [
    0x48, 0xE6, 0x10, 0x9C, 0x21, 0x21, 0xA5, 0x10, 0x8D, 0x22, 0x21, 0x9C, 0x22, 0x21, 0x68, 0x40,
];

/// A 128 KiB LoROM holding the program, with the emulation-mode NMI and
/// reset vectors.
fn build_rom() -> Vec<u8> {
    build_rom_with(&RESET_CODE)
}

/// The same LoROM with other reset code (the NMI handler stays).
fn build_rom_with(reset_code: &[u8]) -> Vec<u8> {
    let mut rom = vec![0u8; 0x2_0000];
    rom[..reset_code.len()].copy_from_slice(reset_code);
    rom[0x7000..0x7000 + NMI_HANDLER.len()].copy_from_slice(&NMI_HANDLER);
    let header = 0x7FB0;
    rom[header + 0x10..header + 0x25].copy_from_slice(b"CAPTURE BOUNDARY     ");
    rom[header + 0x25] = 0x20; // LoROM
    rom[header + 0x27] = 0x07; // 128 KiB
    rom[header + 0x29] = 0x01; // NTSC
    rom[header + 0x2C..header + 0x2E].copy_from_slice(&(!0x1234u16).to_le_bytes());
    rom[header + 0x2E..header + 0x30].copy_from_slice(&0x1234u16.to_le_bytes());
    rom[0x7FFA..0x7FFC].copy_from_slice(&0xF000u16.to_le_bytes()); // NMI
    rom[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes()); // reset
    rom
}

fn loaded() -> SnesEmulator {
    let mut emu = SnesEmulator::new();
    emu.load_rom(&build_rom()).expect("the program loads");
    emu
}

/// Reset code that, in forced blank, copies 64 KiB of ROM into VRAM with
/// one general DMA (8 master clocks a byte, about a frame and a half),
/// then turns the display and the NMI on and reads the APU port forever,
/// as [`RESET_CODE`] does. The DMA runs inside one instruction: the clock
/// jumps past the first frame's `VBlankStart` and its end at once.
const LONG_DMA_RESET_CODE: [u8; 64] = [
    0x78, // SEI
    0xA9, 0x80, 0x8D, 0x00, 0x21, // forced blank
    0xA9, 0x80, 0x8D, 0x15, 0x21, // VRAM address steps on the high byte
    0x9C, 0x16, 0x21, 0x9C, 0x17, 0x21, // VRAM word address 0
    0xA9, 0x01, 0x8D, 0x00, 0x43, // channel 0: two registers, A to B
    0xA9, 0x18, 0x8D, 0x01, 0x43, // to $2118/$2119
    0x9C, 0x02, 0x43, 0xA9, 0x80, 0x8D, 0x03, 0x43, 0x9C, 0x04, 0x43, // from $00:8000
    0x9C, 0x05, 0x43, 0x9C, 0x06, 0x43, // 65536 bytes
    0xA9, 0x01, 0x8D, 0x0B, 0x42, // start it
    0xA9, 0x0F, 0x8D, 0x00, 0x21, // display on
    0xA9, 0x80, 0x8D, 0x00, 0x42, // NMI on
    0xAD, 0x40, 0x21, 0x80, 0xFB, // read the APU port forever
];

fn loaded_long_dma() -> SnesEmulator {
    let mut emu = SnesEmulator::new();
    emu.load_rom(&build_rom_with(&LONG_DMA_RESET_CODE))
        .expect("the program loads");
    emu
}

/// The RGBA the renderer draws for a BGR555 colour at full brightness.
fn rgba(colour: u16) -> [u8; 4] {
    let expand = |c: u16| {
        let c = (c & 0x1F) as u8;
        (c << 3) | (c >> 2)
    };
    [
        expand(colour),
        expand(colour >> 5),
        expand(colour >> 10),
        0xFF,
    ]
}

/// The top-left pixel of a snapshot's picture.
fn first_pixel(snapshot: &FrameSnapshot) -> [u8; 4] {
    snapshot.framebuffer[..4].try_into().unwrap()
}

/// The frame count the program keeps at `$10`.
fn program_frame_count(emu: &mut SnesEmulator) -> u8 {
    emu.bus_mut().read(0x0010)
}

// -------------------------------------------------------------
// The boundary
// -------------------------------------------------------------

#[test]
fn a_snapshot_at_the_boundary_has_the_memory_its_picture_was_drawn_from() {
    let mut held = loaded();
    let mut whole = loaded();
    for _ in 0..3 {
        held.run_frame();
    }
    held.run_to_capture_boundary();
    for _ in 0..4 {
        whole.run_frame();
    }
    assert!(held.at_capture_boundary());
    assert!(!whole.at_capture_boundary());

    let at_boundary = held.capture_frame_snapshot();
    let after_frame = whole.capture_frame_snapshot();
    assert!(at_boundary.at_boundary);
    assert!(!after_frame.at_boundary);
    // Both hold frame 4's picture...
    assert_eq!(at_boundary.frame, 4);
    assert_eq!(after_frame.frame, 4);
    assert_eq!(at_boundary.framebuffer, after_frame.framebuffer);
    assert_eq!((at_boundary.width, at_boundary.height), (256, 224));
    // ...drawn with the colour frame 3's vertical blank wrote. At the
    // boundary CGRAM still holds it; after the whole frame it holds the
    // colour frame 4's vertical blank wrote for frame 5.
    assert_eq!(at_boundary.palette[0], 3);
    assert_eq!(first_pixel(&at_boundary), rgba(at_boundary.palette[0]));
    assert_eq!(after_frame.palette[0], 4);
    assert_ne!(first_pixel(&after_frame), rgba(after_frame.palette[0]));

    // Nothing of the vertical blank has run at the boundary: no NMI.
    assert_eq!(program_frame_count(&mut held), 3);
    assert_eq!(program_frame_count(&mut whole), 4);
}

#[test]
fn each_call_advances_exactly_one_frame() {
    let mut emu = loaded();
    for frame in 1..=6u64 {
        emu.run_to_capture_boundary();
        assert!(emu.at_capture_boundary());
        assert_eq!(
            emu.frame_count(),
            frame - 1,
            "frame {frame} is not counted yet"
        );
        let snapshot = emu.capture_frame_snapshot();
        assert_eq!(snapshot.frame, frame);
        assert_eq!(first_pixel(&snapshot), rgba(snapshot.palette[0]));
    }
}

#[test]
fn a_frame_call_at_the_boundary_runs_the_rest_of_the_frame() {
    type FrameCall = fn(&mut SnesEmulator);
    let calls: [(&str, FrameCall); 2] = [
        ("run_frame", |emu| emu.run_frame()),
        ("run_frame_headless", |emu| emu.run_frame_headless()),
    ];
    for (name, call) in calls {
        let mut held = loaded();
        let mut whole = loaded();
        held.run_frame();
        held.run_to_capture_boundary();
        call(&mut held);
        whole.run_frame();
        whole.run_frame();
        assert!(!held.at_capture_boundary(), "{name}");
        assert_eq!(held.frame_count(), 2, "{name}");
        assert_eq!(held.state_hash(), whole.state_hash(), "{name}");
    }
}

#[test]
fn run_until_vblank_stops_at_the_boundary() {
    let mut emu = loaded();
    let mut whole = loaded();
    emu.run_frame();
    whole.run_frame();
    emu.run_until_vblank();
    assert!(emu.at_capture_boundary());
    assert_eq!(emu.frame_count(), 1, "the held frame is not counted yet");
    emu.run_until_vblank();
    assert!(emu.at_capture_boundary());
    assert_eq!(emu.frame_count(), 2);
    emu.run_frame();
    for _ in 0..2 {
        whole.run_frame();
    }
    assert_eq!(emu.state_hash(), whole.state_hash());
}

#[test]
fn the_boundary_and_its_finish_leave_the_state_of_frames_run_whole() {
    let mut held = loaded();
    let mut whole = loaded();
    for _ in 0..5 {
        held.run_to_capture_boundary();
    }
    held.run_frame();
    for _ in 0..5 {
        whole.run_frame();
    }
    assert_eq!(held.state_hash(), whole.state_hash());
    assert_eq!(held.get_framebuffer().data, whole.get_framebuffer().data);
}

#[test]
fn a_frame_a_long_dma_carries_past_its_end_still_stops_at_its_boundary() {
    let mut held = loaded_long_dma();
    let mut whole = loaded_long_dma();
    held.run_to_capture_boundary();
    assert!(held.at_capture_boundary(), "the first frame holds");
    assert!(
        held.master_cycles() >= 262 * 1364,
        "the clock is past the first frame's end"
    );
    assert!(held.frame_in_progress());
    for frame in 2..=4 {
        held.run_to_capture_boundary();
        assert!(held.at_capture_boundary(), "frame {frame} holds");
        assert_eq!(held.capture_frame_snapshot().frame, frame);
    }
    held.run_frame();
    for _ in 0..4 {
        whole.run_frame();
    }
    assert_eq!(held.state_hash(), whole.state_hash());
    assert_eq!(held.get_framebuffer().data, whole.get_framebuffer().data);
}

/// `LDA #value` / `STA addr`.
fn store(code: &mut Vec<u8>, value: u8, addr: u16) {
    code.extend_from_slice(&[0xA9, value, 0x8D]);
    code.extend_from_slice(&addr.to_le_bytes());
}

/// `STZ addr`.
fn store_zero(code: &mut Vec<u8>, addr: u16) {
    code.push(0x9C);
    code.extend_from_slice(&addr.to_le_bytes());
}

/// A program that keeps HDMA busy and the bus full: channels 1-7 each
/// write four bytes a line (to the window edges, which nothing uses),
/// stealing some 300 master clocks from every line, while the main loop
/// copies 128 to 255 bytes of ROM into WRAM with a general DMA on channel
/// 0, over and over, a different length each time. Each DMA runs inside
/// one instruction, so now and then the instruction that reaches
/// `VBlankStart` also reached line 224's HDMA, whose stolen cycles then
/// bring later events due inside the same batch. The auto-joypad read is
/// on, so `VBlankStart`'s handler queues an event (the one that ends the
/// read), the one event a handler adds to the queue.
fn busy_bus_rom() -> Vec<u8> {
    let mut code = vec![0x78]; // SEI
    store(&mut code, 0x0F, 0x2100); // display on
    for channel in 1..8u16 {
        let base = 0x4300 + (channel << 4);
        store(&mut code, 0x03, base); // four bytes, two registers, A to B
        store(&mut code, 0x26, base + 1); // to $2126/$2127
        store_zero(&mut code, base + 2); // table at $00:8200
        store(&mut code, 0x82, base + 3);
        store_zero(&mut code, base + 4);
    }
    store(&mut code, 0xFE, 0x420C); // HDMA on channels 1-7
    store(&mut code, 0x01, 0x4200); // the auto-joypad read on
    let main_loop = code.len();
    store_zero(&mut code, 0x2181); // WRAM address $7F:0000
    store_zero(&mut code, 0x2182);
    store(&mut code, 0x01, 0x2183);
    store_zero(&mut code, 0x4300); // one register, A to B
    store(&mut code, 0x80, 0x4301); // to WMDATA
    store_zero(&mut code, 0x4302); // from $00:8000
    store(&mut code, 0x80, 0x4303);
    store_zero(&mut code, 0x4304);
    // 128 to 255 bytes, a different length each pass, so the copy that
    // runs over `VBlankStart` ends at a different point each frame:
    // `INC $20` / `LDA $20` / `AND #$7F` / `ORA #$80` / `STA $4305`.
    code.extend_from_slice(&[
        0xE6, 0x20, 0xA5, 0x20, 0x29, 0x7F, 0x09, 0x80, 0x8D, 0x05, 0x43,
    ]);
    store_zero(&mut code, 0x4306);
    store(&mut code, 0x01, 0x420B); // start it
    let back = main_loop as isize - (code.len() as isize + 2);
    code.extend_from_slice(&[0x80, back as i8 as u8]); // BRA main_loop
    assert!(code.len() <= 0x200);

    let mut rom = build_rom_with(&code);
    // The HDMA table: one line at a time, four bytes a line, then the end.
    let mut table = Vec::new();
    for line in 0..240u32 {
        table.extend_from_slice(&[0x01, line as u8, 0xFF, line as u8, 0xFF]);
    }
    table.push(0x00);
    rom[0x200..0x200 + table.len()].copy_from_slice(&table);
    rom
}

#[test]
fn the_rest_of_a_split_batch_runs_before_what_came_due_since() {
    // The boundary splits a drain's batch in front of `VBlankStart`. When
    // the frame goes on, that batch's rest is handled before the events
    // the batch's HDMA brought due meanwhile, as in a frame run whole. A
    // copy long enough runs the clock from before line 224's HDMA to late
    // in line 225, and the HDMA's cycles then bring that line's
    // `HBlankStart` due: `VBlankStart` must queue its auto-joypad event
    // before that `HBlankStart` leaves the queue. The queue then stores
    // its events in the same order and the savestates match byte for
    // byte, also in V-blank, where the queue still holds the rest of the
    // frame (by the frame's end it has emptied, and an order that differed
    // no longer shows).
    let rom = busy_bus_rom();
    let mut held = SnesEmulator::new();
    let mut whole = SnesEmulator::new();
    held.load_rom(&rom).expect("the program loads");
    whole.load_rom(&rom).expect("the program loads");
    scanline_breakpoint(&mut held, 230);
    scanline_breakpoint(&mut whole, 230);
    for frame in 1..=60 {
        held.run_to_capture_boundary();
        assert!(held.at_capture_boundary(), "frame {frame}");
        held.run_frame();
        whole.run_frame();
        for emu in [&mut held, &mut whole] {
            let mode = emu.debugger().get_execution_mode();
            assert_eq!(mode, ExecutionMode::Paused, "frame {frame}");
        }
        assert_eq!(
            held.save_state(),
            whole.save_state(),
            "line 230 of frame {frame}"
        );
        for emu in [&mut held, &mut whole] {
            emu.set_execution_mode(ExecutionMode::Running);
            emu.run_frame();
        }
        assert_eq!(held.save_state(), whole.save_state(), "frame {frame}");
    }
}

#[test]
fn a_step_from_a_boundary_past_the_frames_end_closes_the_frame() {
    let mut stepped = loaded_long_dma();
    let mut whole = loaded_long_dma();
    stepped.run_to_capture_boundary();
    stepped.step_cpu(0);
    assert_eq!(
        stepped.frame_count(),
        1,
        "leaving the boundary closed frame 1"
    );
    while stepped.frame_count() < 2 {
        stepped.step_cpu(0);
    }
    whole.run_frame();
    whole.run_frame();
    stepped.run_frame();
    whole.run_frame();
    assert_eq!(stepped.state_hash(), whole.state_hash());
}

// -------------------------------------------------------------
// Audio
// -------------------------------------------------------------

fn dsp_write(apu: &mut Apu, register: u8, value: u8) {
    apu.spc_write(0x00F2, register);
    apu.spc_write(0x00F3, value);
}

/// Key voice 0 on, playing a looped BRR square wave, so the S-DSP output
/// is not silence.
fn play_a_square_wave(emu: &mut SnesEmulator) {
    let apu = emu.bus_mut().apu_mut();
    for (address, value) in [
        (0x0200, 0x00),
        (0x0201, 0x03),
        (0x0202, 0x00),
        (0x0203, 0x03),
    ] {
        apu.spc_write(address, value);
    }
    apu.spc_write(0x0300, 0x83);
    for address in 0x0301..=0x0304 {
        apu.spc_write(address, 0x77);
    }
    for address in 0x0305..=0x0308 {
        apu.spc_write(address, 0x99);
    }
    for (register, value) in [
        (0x00, 0x7F),
        (0x01, 0x40),
        (0x02, 0x00),
        (0x03, 0x08),
        (0x04, 0x00),
        (0x05, 0x00),
        (0x07, 0x7F),
        (0x5D, 0x02),
        (0x0C, 0x7F),
        (0x1C, 0x7F),
        (0x6C, 0x00),
        (0x4C, 0x01),
    ] {
        dsp_write(apu, register, value);
    }
}

/// A machine one frame in, with the IPL booted and the voice keyed on.
fn playing(audio_enabled: bool) -> SnesEmulator {
    let mut emu = loaded();
    emu.set_audio_enabled(audio_enabled);
    emu.run_frame();
    play_a_square_wave(&mut emu);
    emu
}

fn take_host_audio(emu: &mut SnesEmulator, into: &mut Vec<f32>) {
    into.extend_from_slice(emu.get_audio_samples());
}

fn take_native_audio(emu: &mut SnesEmulator, into: &mut Vec<i16>) {
    let apu = emu.bus_mut().apu_mut();
    let mut samples = vec![0i16; apu.audio_samples_available()];
    let drained = apu.drain_audio(&mut samples);
    into.extend_from_slice(&samples[..drained]);
}

#[test]
fn audio_over_a_boundary_and_its_finish_is_the_audio_of_the_frame_run_whole() {
    const FRAMES: usize = 12;

    // The 44.1 kHz host stream.
    let (mut held, mut whole) = (playing(true), playing(true));
    let (mut held_audio, mut whole_audio) = (Vec::new(), Vec::new());
    for _ in 0..FRAMES {
        held.run_to_capture_boundary();
        take_host_audio(&mut held, &mut held_audio);
    }
    held.run_frame();
    take_host_audio(&mut held, &mut held_audio);
    for _ in 0..FRAMES {
        whole.run_frame();
        take_host_audio(&mut whole, &mut whole_audio);
    }
    assert!(
        whole_audio.iter().any(|&sample| sample.abs() > 0.01),
        "the voice is heard"
    );
    assert_eq!(held_audio, whole_audio);

    // The 32 kHz stream of the S-DSP, which the host stream resamples.
    let (mut held, mut whole) = (playing(false), playing(false));
    let (mut held_audio, mut whole_audio) = (Vec::new(), Vec::new());
    for _ in 0..FRAMES {
        held.run_to_capture_boundary();
        take_native_audio(&mut held, &mut held_audio);
    }
    held.run_frame();
    take_native_audio(&mut held, &mut held_audio);
    for _ in 0..FRAMES {
        whole.run_frame();
        take_native_audio(&mut whole, &mut whole_audio);
    }
    assert!(whole_audio
        .iter()
        .any(|&sample| sample.unsigned_abs() > 100));
    assert_eq!(held_audio, whole_audio);
    assert_eq!(held.state_hash(), whole.state_hash());
}

// -------------------------------------------------------------
// Steps, savestates, resets
// -------------------------------------------------------------

#[test]
fn a_step_from_the_boundary_takes_the_step_the_frame_loop_takes() {
    let mut stepped = loaded();
    let mut whole = loaded();
    stepped.run_frame();
    stepped.run_to_capture_boundary();
    // The first step starts the vertical blank first: the NMI is armed
    // then and delivered a few instructions on, as in the frame loop.
    while stepped.frame_count() < 2 {
        stepped.step_cpu(0);
        assert!(!stepped.at_capture_boundary());
    }
    whole.run_frame();
    whole.run_frame();
    assert_eq!(program_frame_count(&mut stepped), 2);
    // One more frame on each opens the next frame at the same cycle, so
    // the whole machine compares, the frame base included.
    stepped.run_frame();
    whole.run_frame();
    assert_eq!(stepped.state_hash(), whole.state_hash());
}

#[test]
fn a_step_after_a_whole_frame_opens_the_next_frame() {
    // The frame base of a frame run to its end is that frame's; a step
    // that took it as the frame in progress would close a frame at once
    // and arm a second set of events.
    let mut stepped = loaded();
    let mut whole = loaded();
    stepped.run_frame();
    stepped.step_cpu(0);
    assert_eq!(stepped.frame_count(), 1, "one step closes no frame");
    while stepped.frame_count() < 2 {
        stepped.step_cpu(0);
    }
    whole.run_frame();
    whole.run_frame();
    stepped.run_frame();
    whole.run_frame();
    assert_eq!(stepped.state_hash(), whole.state_hash());
}

#[test]
fn bare_steps_between_frames_count_inside_the_next_frame() {
    // The bare step handles no events; the debugger step that follows
    // must still close the frame at the end its events were armed with,
    // and keep closing frames after it.
    let mut emu = loaded();
    emu.run_frame();
    let armed_end = emu.master_cycles() + 262 * 1364;
    for _ in 0..50 {
        emu.step();
    }
    emu.step_cpu(0);
    assert_eq!(emu.frame_count(), 1, "one step closes no frame");
    while emu.frame_count() < 2 {
        assert!(
            emu.master_cycles() < armed_end,
            "the frame closes at its end"
        );
        emu.step_cpu(0);
    }
    assert!(emu.master_cycles() >= armed_end);
    while emu.frame_count() < 3 {
        assert!(emu.master_cycles() < armed_end + 2 * 262 * 1364);
        emu.step_cpu(0);
    }
    assert_eq!(program_frame_count(&mut emu), 3, "an NMI every frame");
}

#[test]
fn a_savestate_at_the_boundary_is_taken_once_the_frame_is_finished() {
    let mut held = loaded();
    let mut whole = loaded();
    held.run_frame();
    held.run_to_capture_boundary();
    // A paused debugger does not stop the finish: a savestate never holds
    // a frame stopped at the boundary.
    held.set_execution_mode(ExecutionMode::Paused);
    let saved = held.save_state_finishing_held_frame();
    assert!(!held.at_capture_boundary());
    whole.run_frame();
    whole.run_frame();
    assert_eq!(saved, whole.save_state());

    // Anywhere else the save path saves the machine as it stands.
    assert_eq!(whole.save_state_finishing_held_frame(), whole.save_state());
}

#[test]
fn a_reset_and_a_state_load_drop_the_boundary() {
    let mut emu = loaded();
    emu.run_frame();
    let state = emu.save_state();
    emu.run_to_capture_boundary();
    emu.load_state(&state).expect("its own state loads");
    assert!(!emu.at_capture_boundary());
    emu.run_to_capture_boundary();
    emu.reset();
    assert!(!emu.at_capture_boundary());
}

#[test]
fn a_paused_debugger_keeps_the_boundary_call_from_running() {
    let mut emu = loaded();
    emu.run_frame();
    emu.set_execution_mode(ExecutionMode::Paused);
    let before = emu.state_hash();
    emu.run_to_capture_boundary();
    assert!(!emu.at_capture_boundary());
    assert_eq!(emu.state_hash(), before);
}

// -------------------------------------------------------------
// A frame stopped at a breakpoint
// -------------------------------------------------------------

fn scanline_breakpoint(emu: &mut SnesEmulator, line: u32) -> u32 {
    emu.debugger().add_breakpoint(Breakpoint {
        address: 0,
        bp_type: BreakpointType::Event {
            code: snes_events::SCANLINE,
            param: line,
        },
        cpu_id: None,
        condition: None,
    })
}

#[test]
fn a_frame_stopped_at_a_breakpoint_goes_on_where_it_stopped() {
    // Both machines run the instrumented loop with the same breakpoints
    // armed: one on line 100, and one on a line no frame draws. Only the
    // first machine's line-100 breakpoint is on.
    let mut broken = loaded();
    let mut whole = loaded();
    let line_100 = scanline_breakpoint(&mut broken, 100);
    scanline_breakpoint(&mut broken, 300);
    let disabled = scanline_breakpoint(&mut whole, 100);
    whole.debugger().enable_breakpoint(disabled, false);
    scanline_breakpoint(&mut whole, 300);

    broken.run_frame();
    assert_eq!(
        broken.debugger().get_execution_mode(),
        ExecutionMode::Paused
    );
    assert!(broken.frame_in_progress());
    assert_eq!(broken.frame_count(), 0, "stopped inside the first frame");
    broken.debugger().enable_breakpoint(line_100, false);
    broken.set_execution_mode(ExecutionMode::Running);
    broken.run_frame();
    assert_eq!(broken.frame_count(), 1);

    whole.run_frame();
    assert_eq!(broken.state_hash(), whole.state_hash());
}

#[test]
fn a_boundary_run_stopped_by_a_breakpoint_reports_no_boundary() {
    let mut emu = loaded();
    emu.run_frame();
    let line_100 = scanline_breakpoint(&mut emu, 100);
    emu.run_to_capture_boundary();
    assert!(!emu.at_capture_boundary());
    assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::Paused);
    // Resumed, the call runs on to the boundary of the same frame.
    emu.debugger().enable_breakpoint(line_100, false);
    emu.set_execution_mode(ExecutionMode::Running);
    emu.run_to_capture_boundary();
    assert!(emu.at_capture_boundary());
    assert_eq!(emu.capture_frame_snapshot().frame, 2);
}

// -------------------------------------------------------------
// Redraws
// -------------------------------------------------------------

#[test]
fn a_repaint_and_a_layer_render_leave_the_state_hash_where_it_was() {
    let mut emu = loaded();
    emu.run_frame();
    // Sprites on: 40 of them on line 50, past the 32 a line holds. The
    // frame sets the sprite overflow flag, and the evaluator moves the OAM
    // read address, both saved in a savestate.
    {
        let ppu = emu.bus_mut().ppu_mut();
        let mut oam = vec![0u8; 0x220];
        for sprite in 0..128 {
            oam[sprite * 4 + 1] = if sprite < 40 { 50 } else { 0xF0 };
        }
        ppu.write_oam_debug(0, &oam);
    }
    emu.write_memory_debug(0x212C, &[0x10]); // sprites on the main screen
    emu.run_frame();
    let stat77 = emu
        .bus_mut()
        .ppu_mut()
        .read_reg(0x3E, &snes1_core::PpuAccessCtx::ZERO);
    assert!(stat77 & 0x40 != 0, "the frame overflowed line 50");

    // The game moves its sprites in the vertical blank: a repaint would
    // now clear the flag and move the OAM address.
    let mut oam = vec![0u8; 0x220];
    for sprite in 0..128 {
        oam[sprite * 4 + 1] = 0xF0;
    }
    oam[0] = 20;
    oam[1] = 20;
    emu.write_oam_debug(0, &oam);
    let before = emu.state_hash();
    emu.repaint_held_frame();
    assert_eq!(emu.state_hash(), before, "after a repaint");
    let framebuffer = emu.get_framebuffer().data.clone();
    emu.render_layer(4).expect("the sprites");
    assert_eq!(emu.state_hash(), before, "after a layer render");
    assert_eq!(
        emu.get_framebuffer().data,
        framebuffer,
        "the layer render leaves the framebuffer"
    );

    // The same at the boundary.
    emu.run_to_capture_boundary();
    let before = emu.state_hash();
    emu.repaint_held_frame();
    emu.render_layer(0).expect("BG1");
    assert_eq!(emu.state_hash(), before, "at the boundary");
}
