//! The Z80's reads and writes through the bank window, measured on a
//! program: each waits for the 68000's bus, which costs the Z80
//! `BANK_WAIT_CYCLES` of its cycles, and holds the 68000 off its bus for
//! `BANK_HOLD_CYCLES` of the 68000's.

use retro_core::{EmulatorCore, ExecutionMode};
use sg1_core::z80_bus::{BANK_HOLD_CYCLES, BANK_WAIT_CYCLES};
use sg1_core::GenesisEmulator;

mod helpers;
use helpers::MiniCartridgeBuilder;

/// The 68000 counts in D0, at the reset vector's $400: ADDQ.L #1,D0 and
/// BRA.S back, 8 and 10 cycles a turn.
fn counting_rom() -> Vec<u8> {
    MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title("BANK WINDOW")
        .with_instructions(0x400, &[0x5280, 0x60FC])
        .build()
}

/// The 68000's cycles of one turn of its count.
const COUNT_TURN: f64 = 18.0;

/// The Z80: the YM2612's DAC on, then a loop that reads a byte through the
/// bank window and writes it as the DAC's data: LD A,(nn) 13 cycles, LD
/// (nn),A 13 and JR 12, one read through the window a turn.
#[rustfmt::skip]
const Z80_LOOP: [u8; 23] = [
    0x3E, 0x2B, 0x32, 0x00, 0x40, // LD A,$2B; LD ($4000),A: the DAC's register
    0x3E, 0x80, 0x32, 0x01, 0x40, // LD A,$80; LD ($4001),A: the DAC on
    0x3E, 0x2A, 0x32, 0x00, 0x40, // LD A,$2A; LD ($4000),A: the DAC's data
    0x3A, 0x00, 0x80,             // loop: LD A,($8000), through the bank window
    0x32, 0x01, 0x40,             // LD ($4001),A
    0x18, 0xF8,                   // JR loop
];

/// The Z80's cycles of one turn of its loop, the read's wait left out.
const Z80_TURN: f64 = 38.0;

/// The Z80's cycles in a frame of the NTSC console: 262 lines of 3,420
/// master clocks, the Z80 at a fifteenth of the master clock.
const Z80_FRAME: f64 = 262.0 * 3420.0 / 15.0;

/// The counting program, the Z80 running its loop (put in its RAM by the
/// host, the Z80's bus held) or held in reset, run two frames.
fn machine(z80_runs: bool) -> GenesisEmulator {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(&counting_rom()).expect("the program loads");
    emu.set_execution_mode(ExecutionMode::Running);
    emu.run_frame();
    if z80_runs {
        emu.write_memory_debug(0xA1_1100, &[0x01]);
        emu.write_memory_debug(0xA0_0000, &Z80_LOOP);
        emu.write_memory_debug(0xA1_1200, &[0x01]);
        emu.write_memory_debug(0xA1_1100, &[0x00]);
    }
    emu.run_frame();
    emu.ym2612_mut().enable_write_log();
    emu
}

/// The turns of the Z80's loop and of the 68000's count in `frames` frames.
fn turns(emu: &mut GenesisEmulator, frames: u32) -> (usize, u32) {
    let _ = emu.ym2612_mut().take_write_log();
    let before = emu.cpu().d[0];
    for _ in 0..frames {
        emu.run_frame();
    }
    let z80 = emu.ym2612_mut().take_write_log().len();
    (z80, emu.cpu().d[0].wrapping_sub(before))
}

/// The Z80's loop turns as its cycles and the read's wait give: a frame's
/// Z80 cycles over the turn's 38 and the wait; and the 68000 counts as many
/// turns fewer as the Z80's reads held it off its bus: 11 of its cycles a
/// read, over its turn's 18 (a DRAM refresh the held time crossed adds a
/// little: within 3 %).
#[test]
fn a_read_through_the_bank_window_waits_and_holds_the_68000() {
    const FRAMES: u32 = 10;
    let mut streaming = machine(true);
    let mut alone = machine(false);
    let (reads, counted) = turns(&mut streaming, FRAMES);
    let (none, counted_alone) = turns(&mut alone, FRAMES);
    assert_eq!(none, 0, "the Z80 held in reset reads nothing");
    let expected = f64::from(FRAMES) * Z80_FRAME / (Z80_TURN + f64::from(BANK_WAIT_CYCLES));
    assert!(
        (reads as f64 - expected).abs() <= 2.0,
        "{reads} turns of the Z80's loop in {FRAMES} frames, {expected:.1} expected"
    );
    let held = reads as f64 * f64::from(BANK_HOLD_CYCLES);
    let fewer = f64::from(counted_alone - counted);
    let expected_fewer = held / COUNT_TURN;
    assert!(
        (fewer - expected_fewer).abs() <= 0.03 * expected_fewer,
        "the 68000 counted {fewer} turns fewer, {expected_fewer:.1} expected"
    );
}

/// States saved where frames end, and at stops part of the way through
/// frames, while the Z80 streams through the bank window: each loaded into
/// a new machine goes on as the machine that saved it. The time the Z80's
/// reads hold the 68000 is the sound clocks' at once, so a save finds them
/// where a load derives them.
#[test]
fn saves_while_the_z80_reads_through_the_window_go_on_alike() {
    let load = |state: &[u8]| {
        let mut emu = GenesisEmulator::new();
        emu.load_rom(&counting_rom()).expect("the program loads");
        emu.set_execution_mode(ExecutionMode::Running);
        emu.load_state(state, retro_core::RomCheck::SameRom)
            .expect("the state loads");
        emu
    };
    let mut saver = machine(true);
    let mut loaded: Vec<(GenesisEmulator, String, u32)> = Vec::new();
    for frame in 0..30u32 {
        if frame % 3 == 1 {
            for _ in 0..(200 + 37 * frame) {
                saver.step_cpu(0);
            }
            let emu = load(&saver.save_state());
            assert_eq!(
                emu.state_hash(),
                saver.state_hash(),
                "a stop in frame {frame}"
            );
            loaded.push((emu, format!("the stop in frame {frame}"), 0));
        }
        saver.run_frame();
        for (emu, what, frames) in &mut loaded {
            emu.run_frame();
            *frames += 1;
            assert_eq!(
                emu.state_hash(),
                saver.state_hash(),
                "{what}, {frames} frames on"
            );
        }
        loaded.retain(|entry| entry.2 < 2);
        let emu = load(&saver.save_state());
        loaded.push((emu, format!("the end of frame {frame}"), 0));
    }
}
