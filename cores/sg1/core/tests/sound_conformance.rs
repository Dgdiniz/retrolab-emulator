//! The sound conformance suite (`retro_core::audio::conformance`) on the
//! Mega Drive: a program that loops on one instruction and writes no sound
//! register, a sound moment that keys an FM note on the YM2612's first
//! channel and a tone on the PSG's second, and a program that writes the
//! mailbox, the Z80's RAM, holding the Z80's bus.

use retro_core::audio::conformance::SoundCoreUnderTest;
use retro_core::audio::{
    SoundCore, SoundEvent, SoundLogContents, SoundLogEntry, SoundSamples, StemLayout, StemTake,
};
use retro_core::{EmulatorCore, ExecutionMode};
use sg1_core::ym2612::FmDacModel;
use sg1_core::GenesisEmulator;

mod helpers;
use helpers::MiniCartridgeBuilder;

/// Where the program loops: `BRA.w` to itself.
const LOOP: u32 = 0x400;

/// The YM2612's registers for a note on channel 1, in part 0: algorithm
/// 7 (every operator a carrier), one operator at full level and the
/// other three silent, the fastest attack, no decay, both speakers,
/// F-number 0x269 in block 4, then every operator keyed on.
#[rustfmt::skip]
const FM_NOTE: [(u8, u8); 30] = [
    (0x22, 0x00), (0x27, 0x00), (0x2B, 0x00), (0x28, 0x00),
    (0x30, 0x01), (0x34, 0x01), (0x38, 0x01), (0x3C, 0x01),
    (0x40, 0x00), (0x44, 0x7F), (0x48, 0x7F), (0x4C, 0x7F),
    (0x50, 0x1F), (0x54, 0x1F), (0x58, 0x1F), (0x5C, 0x1F),
    (0x60, 0x00), (0x64, 0x00), (0x68, 0x00), (0x6C, 0x00),
    (0x70, 0x00), (0x74, 0x00), (0x78, 0x00), (0x7C, 0x00),
    (0xB0, 0x07), (0xB4, 0xC0), (0xA4, 0x22), (0xA0, 0x69),
    (0x80, 0x0F), (0x28, 0xF0),
];

/// The PSG's bytes for a tone on its second channel: period 0x0FE, no
/// attenuation.
const PSG_TONE: [u8; 3] = [0xAE, 0x0F, 0xB0];

fn program(title: &str) -> Vec<u8> {
    program_of(title, &[0x6000, 0xFFFE]) // BRA.w LOOP
}

/// A program that runs `code` from `LOOP`.
fn program_of(title: &str, code: &[u16]) -> Vec<u8> {
    MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title(title)
        .with_instructions(LOOP as usize, code)
        .build()
}

fn rom() -> Vec<u8> {
    program("SOUND CONFORMANCE")
}

fn other_rom() -> Vec<u8> {
    program("SOUND CONFORMANCE B")
}

/// Where the mailbox program writes in the Z80's RAM.
const MAILBOX_PORT: u16 = 0x1F00;

/// A program that takes the Z80's bus (the Z80 held in reset) and writes 0,
/// 1, 2 and on into the Z80's RAM at `MAILBOX_PORT`, about fifty times a
/// frame.
fn mailbox_rom() -> Vec<u8> {
    #[rustfmt::skip]
    let code = [
        0x33FC, 0x0100, 0x00A1, 0x1100, // move.w #$0100,($A11100).l  the bus
        0x7000,                         // moveq #0,d0
        0x13C0, 0x00A0, 0x1F00,         // loop: move.b d0,($A01F00).l
        0x5200,                         // addq.b #1,d0
        0x323C, 0x00FF,                 // move.w #$FF,d1
        0x51C9, 0xFFFE,                 // dbra d1,*
        0x60EE,                         // bra.s loop
    ];
    program_of("SOUND CONFORMANCE MAILBOX", &code)
}

fn build() -> GenesisEmulator {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(&rom()).expect("the program loads");
    emu.set_audio_enabled(true);
    emu.set_execution_mode(ExecutionMode::Running);
    emu
}

fn sound_moment(emu: &mut GenesisEmulator) {
    let ym = emu.ym2612_mut();
    for (register, value) in FM_NOTE {
        ym.write_port(0, register);
        ym.write_port(1, value);
    }
    let psg = emu.psg_mut();
    for byte in PSG_TONE {
        psg.write(byte);
    }
}

fn mega_drive() -> SoundCoreUnderTest<GenesisEmulator> {
    SoundCoreUnderTest {
        build,
        sound_moment,
        every_frame: LOOP,
        rom: rom(),
        other_rom: other_rom(),
        mailbox_rom: mailbox_rom(),
        mailbox_port: MAILBOX_PORT,
    }
}

retro_core::sound_conformance_tests!(mega_drive());

// -- The Mega Drive's stems ----------------------------------------------

/// A machine past the sound moment with every stem armed.
fn armed() -> (GenesisEmulator, StemLayout) {
    armed_through(FmDacModel::default())
}

/// [`armed`], the YM2612's channels through the DAC of `model` from the
/// start.
fn armed_through(model: FmDacModel) -> (GenesisEmulator, StemLayout) {
    let mut emu = build();
    emu.set_fm_dac_model(model);
    for _ in 0..4 {
        emu.run_frame();
    }
    sound_moment(&mut emu);
    let layout = emu.stem_layout().expect("the Mega Drive takes stems");
    emu.arm_stems(layout.all());
    (emu, layout)
}

/// Stem `name` of a take of every stem.
fn stem<'a>(take: &'a StemTake, layout: &StemLayout, name: &str) -> &'a [f32] {
    match &take.stems[layout.index_of(name).expect(name)] {
        SoundSamples::F32(samples) => samples,
        SoundSamples::I16(_) => panic!("the Mega Drive's stems are floats"),
    }
}

/// The master is the mix the host hears before the limiter: below the
/// limiter's threshold, the host's very samples, and none departs past
/// the stems' first span: they start from silence, where the master holds
/// the YM2612's silence, which the discrete chip's DAC sits 4 steps above.
#[test]
fn the_master_is_the_host_s_sound_below_the_limiter() {
    let (mut emu, _) = armed();
    let _ = emu.get_audio_samples();
    emu.render_frame();
    let take = emu.take_stems().expect("armed");
    assert_eq!(
        take.master,
        SoundSamples::F32(emu.get_audio_samples().to_vec())
    );
    assert!(
        take.departures
            .iter()
            .all(|span| span.start + span.frames <= STAGE_SPAN as u32),
        "{:?}",
        take.departures
    );
    assert!(
        take.frames > 700,
        "a frame's sample frames: {}",
        take.frames
    );
}

/// A muted channel's stem is its sound still, and its samples depart from
/// the master, which leaves it out; the scope shows the channel too.
/// Through the integrated chip's DAC, whose silence is 0, the master is
/// then the PSG's tone alone.
#[test]
fn a_muted_channel_keeps_its_stem_and_its_samples_depart() {
    let (mut emu, layout) = armed_through(FmDacModel::Integrated);
    emu.audio_debugger(0)
        .expect("the YM2612")
        .mute_channel(0, true);
    emu.render_frame();
    let take = emu.take_stems().expect("armed");
    assert!(stem(&take, &layout, "fm1").iter().any(|&s| s != 0.0));
    let departed: u32 = take.departures.iter().map(|span| span.frames).sum();
    assert!(departed > take.frames / 2, "{departed} of {}", take.frames);
    let psg2 = stem(&take, &layout, "psg2");
    let SoundSamples::F32(master) = &take.master else {
        panic!("floats")
    };
    for (frame, &tone) in psg2.iter().enumerate() {
        assert_eq!(
            master[frame * 2],
            tone,
            "the master is the PSG's tone alone"
        );
    }
    let wave = emu
        .audio_debugger(0)
        .expect("the YM2612")
        .get_waveform(0, 64);
    assert!(
        wave.iter().any(|&s| s != 0.0),
        "the scope shows the channel"
    );
}

/// The samples the host's band-limited stage takes to reach a level it
/// steps to: the kernel's 32 samples each side of the step.
const STAGE_SPAN: usize = 64;

/// While the DAC plays in channel 6's place the `dac` stem holds it, as
/// it enters the master, from the stage's span on, which its step from
/// silence takes; and channel 6's FM stem is silent.
#[test]
fn the_dac_stem_holds_channel_6_while_the_dac_plays() {
    let (mut emu, layout) = armed();
    let ym = emu.ym2612_mut();
    for (port, register, value) in [(2, 0xB6, 0xC0), (0, 0x2B, 0x80), (0, 0x2A, 0xF0)] {
        ym.write_port(port, register);
        ym.write_port(port + 1, value);
    }
    emu.render_frame();
    let take = emu.take_stems().expect("armed");
    let level = FmDacModel::Discrete.output((0xF0 - 128) << 6, true) as f32 / 8192.0 * 0.75 / 6.0;
    // A stereo stem: two values a sample frame.
    let dac = stem(&take, &layout, "dac");
    assert!(dac[2 * STAGE_SPAN..].iter().all(|&s| s == level));
    assert!(dac[..2 * STAGE_SPAN].iter().any(|&s| s != level));
    assert!(stem(&take, &layout, "fm6").iter().all(|&s| s == 0.0));
}

/// A channel panned to one side keeps in its stem what the DAC gives the
/// other: the level of its sign through the discrete chip's DAC, nothing
/// through the integrated chip's; and through either the stems add up to
/// the master past their first span.
#[test]
fn a_channel_panned_to_one_side_keeps_its_other_side_in_its_stem() {
    for model in [FmDacModel::Discrete, FmDacModel::Integrated] {
        let (mut emu, layout) = armed_through(model);
        let ym = emu.ym2612_mut();
        ym.write_port(0, 0xB4);
        ym.write_port(1, 0x80); // channel 1: left alone
        let _ = emu.get_audio_samples();
        emu.render_frame();
        let take = emu.take_stems().expect("armed");
        assert!(
            take.departures
                .iter()
                .all(|span| span.start + span.frames <= STAGE_SPAN as u32),
            "{model:?}: {:?}",
            take.departures
        );
        // A stereo stem: the left side's value, then the right's.
        let fm1 = stem(&take, &layout, "fm1");
        let mut other_side = fm1.iter().skip(1).step_by(2);
        match model {
            FmDacModel::Discrete => assert!(other_side.any(|&s| s != 0.0), "the sign's level"),
            FmDacModel::Integrated => assert!(other_side.all(|&s| s == 0.0), "nothing"),
        }
        assert!(
            fm1.iter().step_by(2).any(|&s| s != 0.0),
            "{model:?}: the tone"
        );
    }
}

// -- The Mega Drive's sound log -------------------------------------------

const WRITES: SoundLogContents = SoundLogContents {
    writes: true,
    keys: false,
    mailbox: false,
};

/// A machine past its warm-up, its log started, the sound moment made,
/// `frames` frames run: the log, taken whole.
fn logged(sound: bool, frames: u32) -> Vec<retro_core::audio::SoundLogEntry> {
    let mut emu = build();
    emu.set_audio_enabled(sound);
    for _ in 0..4 {
        emu.run_frame();
    }
    emu.start_sound_log(WRITES);
    sound_moment(&mut emu);
    let mut entries = Vec::new();
    for _ in 0..frames {
        emu.render_frame();
        entries.extend(emu.take_sound_log().expect("a log runs").entries);
    }
    entries
}

/// The log is on the chips' input clocks, which run whether the chips
/// make samples or not: with the sound off it logs what it logs with the
/// sound on, at the same times.
#[test]
fn the_log_s_times_are_the_same_with_the_sound_off() {
    assert_eq!(logged(false, 8), logged(true, 8));
}

/// Writes the Z80 makes in its bursts are logged in the order it made
/// them, each stamped with the YM2612's input clock when it came (the
/// clock the chip renders its sound on, which runs on the Z80's
/// instructions inside a burst), the later one later, both before the end
/// of the frame that made them.
#[test]
fn a_z80_s_writes_are_logged_in_their_order_on_the_chip_s_clock() {
    #[rustfmt::skip]
    let program: [u8; 34] = [
        0x3E, 0x30, 0x32, 0x00, 0x40, // ld a,$30 ; ld ($4000),a
        0x3E, 0x01, 0x32, 0x01, 0x40, // ld a,$01 ; ld ($4001),a   the first write
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // ten nop
        0x3E, 0x34, 0x32, 0x00, 0x40, // ld a,$34 ; ld ($4000),a
        0x3E, 0x02, 0x32, 0x01, 0x40, // ld a,$02 ; ld ($4001),a   the second
        0x18, 0xFE, // jr $
        0x00, 0x00,
    ];
    let mut emu = build();
    emu.run_frame();
    emu.write_memory_debug(0xA1_1100, &[0x01]);
    emu.write_memory_debug(0xA0_0000, &program);
    emu.start_sound_log(WRITES);
    emu.write_memory_debug(0xA1_1200, &[0x01]);
    emu.write_memory_debug(0xA1_1100, &[0x00]);
    emu.render_frame();
    let log = emu.take_sound_log().expect("a log runs");
    // The last write of each: the chip's registers at the log's start
    // (and after the reset the Z80's release makes) come first.
    let at = |register, value| {
        log.entries
            .iter()
            .rposition(|entry| entry.event == SoundEvent::Write { register, value })
            .unwrap_or_else(|| panic!("the program's write of {register:#X}"))
    };
    let (first, second) = (at(0x30, 0x01), at(0x34, 0x02));
    let end = log
        .entries
        .iter()
        .position(|entry| entry.event == SoundEvent::FrameEnd)
        .expect("the frame's end");
    assert!(
        first < second && second < end,
        "in their order, in their frame"
    );
    let (first, second) = (log.entries[first].time, log.entries[second].time);
    assert!(0 < first && first < second, "stamped {first} then {second}");
    assert!(second <= log.entries[end].time);
}

/// A Z80 program that turns the DAC on and then, forever, writes a value
/// to the DAC and right after it the PSG's first channel's attenuation,
/// each pair 318 Z80 cycles after the one before (about 187 pairs a
/// frame).
#[rustfmt::skip]
const DAC_AND_PSG: [u8; 36] = [
    0x3E, 0x2B, 0x32, 0x00, 0x40, // ld a,$2B ; ld ($4000),a
    0x3E, 0x80, 0x32, 0x01, 0x40, // ld a,$80 ; ld ($4001),a   the DAC on
    0x3E, 0x2A, 0x32, 0x00, 0x40, // ld a,$2A ; ld ($4000),a   its data
    0x06, 0x00,                   // ld b,0
    0x78,                         // loop: ld a,b
    0x32, 0x01, 0x40,             // ld ($4001),a              the DAC
    0xE6, 0x0F, 0xF6, 0x90,       // and $0F ; or $90
    0x32, 0x11, 0x7F,             // ld ($7F11),a              the PSG
    0x04,                         // inc b
    0x0E, 0x10,                   // ld c,16
    0x0D, 0x20, 0xFD,             // delay: dec c ; jr nz,delay
    0x18, 0xED,                   // jr loop
];

/// A Z80 writing the DAC and the PSG through every frame, under a 68000
/// that runs long instructions (each line's last runs far past the line's
/// end, more than the PSG's share of the line pays for), logged for
/// `frames` frames after a first: each take with the master clocks the
/// machine had run at its frame's end.
fn dac_and_psg_logged(frames: u32) -> Vec<(Vec<SoundLogEntry>, u64)> {
    // loop: move.w #$FFFF,d1 ; mulu d1,d1 ; bra.s loop
    let rom = program_of("SOUND LOG CLOCK", &[0x323C, 0xFFFF, 0xC2C1, 0x60F8]);
    let mut emu = GenesisEmulator::new();
    emu.load_rom(&rom).expect("the program loads");
    emu.set_audio_enabled(true);
    emu.set_execution_mode(ExecutionMode::Running);
    emu.run_frame();
    emu.write_memory_debug(0xA1_1100, &[0x01]);
    emu.write_memory_debug(0xA0_0000, &DAC_AND_PSG);
    emu.write_memory_debug(0xA1_1200, &[0x01]);
    emu.write_memory_debug(0xA1_1100, &[0x00]);
    emu.render_frame();
    emu.start_sound_log(WRITES);
    let mut takes = Vec::new();
    for _ in 0..frames {
        emu.render_frame();
        let log = emu.take_sound_log().expect("a log runs");
        takes.push((log.entries[log.image_len..].to_vec(), emu.master_cycles()));
    }
    takes
}

/// One line of the Mega Drive, in master clocks.
const LINE: u64 = 3420;

/// Each chip's writes sit on the master clock, in their frame: every
/// frame's end comes the master clocks the frame ran after the one before;
/// the DAC's writes of a frame are spread across it in their order, none
/// raised to the frame's start (one may stand there: a write in the frame's
/// first Z80 instruction); and each PSG write lies close to the DAC write
/// the Z80 made just before it, though the 68000's long instructions give
/// the PSG a few percent more clocks than its share. Close is two lines:
/// inside a line the YM2612 may run up to a line ahead, and the PSG's
/// extra clocks do not fall evenly over the frame.
#[test]
fn each_chip_s_writes_sit_on_the_master_clock_in_their_frame() {
    let takes = dac_and_psg_logged(30);
    let (mut dac, mut psg) = (Vec::new(), Vec::new());
    let mut last_end: Option<(u64, u64)> = None;
    for (frame, (entries, master)) in takes.iter().enumerate() {
        let end = entries.last().expect("the frame's end");
        assert_eq!(end.event, SoundEvent::FrameEnd, "frame {frame} ends");
        let writes: Vec<u64> = entries
            .iter()
            .filter(|entry| matches!(entry.event, SoundEvent::Write { register: 0x2A, .. }))
            .map(|entry| entry.time)
            .collect();
        if let Some((start, last_master)) = last_end {
            assert_eq!(
                end.time - start,
                master - last_master,
                "frame {frame}: its end is the master clocks it ran after the one before"
            );
            assert!(
                writes.len() > 150,
                "frame {frame}: {} DAC writes",
                writes.len()
            );
            let spacing = (end.time - start) / writes.len() as u64;
            let gaps: Vec<u64> = std::iter::once(start)
                .chain(writes.iter().copied())
                .chain(std::iter::once(end.time))
                .collect::<Vec<_>>()
                .windows(2)
                .map(|pair| {
                    assert!(pair[0] <= pair[1], "frame {frame}: a write out of order");
                    pair[1] - pair[0]
                })
                .collect();
            let at_start = writes.iter().filter(|&&time| time == start).count();
            assert!(
                at_start <= 1,
                "frame {frame}: {at_start} DAC writes at its start"
            );
            let widest = gaps.iter().max().expect("gaps");
            assert!(
                *widest <= 2 * spacing,
                "frame {frame}: a gap of {widest} master clocks between the DAC's writes, \
                 which come {spacing} apart"
            );
        }
        last_end = Some((end.time, *master));
        for entry in entries {
            match entry.event {
                SoundEvent::Write { register: 0x2A, .. } => dac.push(entry.time),
                SoundEvent::Write { .. } if entry.chip == 1 => psg.push(entry.time),
                _ => {}
            }
        }
    }
    assert!(dac.len() > 4000 && psg.len() >= dac.len());
    for (pair, (&dac, &psg)) in dac.iter().zip(&psg).enumerate() {
        assert!(
            dac.abs_diff(psg) <= 2 * LINE,
            "pair {pair}: the DAC write at {dac}, the PSG write after it at {psg}"
        );
    }
}

// -- The YM2612's DAC, a host setting ------------------------------------

/// A Z80 program: the DAC on, then a loop that reads a byte through the
/// bank window and writes it as the DAC's data, so the YM2612 follows the
/// Z80 and makes samples past a frame's end as the Z80 runs past it.
#[rustfmt::skip]
const Z80_DAC_LOOP: [u8; 23] = [
    0x3E, 0x2B, 0x32, 0x00, 0x40, // LD A,$2B; LD ($4000),A: the DAC's register
    0x3E, 0x80, 0x32, 0x01, 0x40, // LD A,$80; LD ($4001),A: the DAC on
    0x3E, 0x2A, 0x32, 0x00, 0x40, // LD A,$2A; LD ($4000),A: the DAC's data
    0x3A, 0x00, 0x80,             // loop: LD A,($8000), through the bank window
    0x32, 0x01, 0x40,             // LD ($4001),A
    0x18, 0xF8,                   // JR loop
];

/// The program running with the Z80 running [`Z80_DAC_LOOP`], put in its
/// RAM by the host with the Z80's bus held; channel 6, which the DAC plays
/// in, panned to the left alone, so the discrete chip's DAC and the
/// integrated chip's part on the side it is not panned to as well.
fn z80_dac_playing() -> GenesisEmulator {
    let mut emu = build();
    emu.run_frame();
    let ym = emu.ym2612_mut();
    ym.write_port(2, 0xB6);
    ym.write_port(3, 0x80);
    emu.write_memory_debug(0xA1_1100, &[0x01]);
    emu.write_memory_debug(0xA0_0000, &Z80_DAC_LOOP);
    emu.write_memory_debug(0xA1_1200, &[0x01]);
    emu.write_memory_debug(0xA1_1100, &[0x00]);
    emu
}

/// The DAC's model and the channels' mutes are the host's: with the Z80
/// driving the YM2612's DAC, a machine through the integrated chip's DAC
/// and one with every FM channel muted hold the state of a machine with
/// neither, after every frame, the frames whose YM2612 made samples past
/// their end among them; and the integrated chip's DAC plays something
/// else. Set on a new machine, the model lasts a ROM's load, the reset
/// button, a power cycle and a savestate's load.
#[test]
fn the_dac_s_model_and_the_mutes_change_no_state() {
    let mut plain = z80_dac_playing();
    let mut integrated = z80_dac_playing();
    integrated.set_fm_dac_model(FmDacModel::Integrated);
    let mut muted = z80_dac_playing();
    for channel in 0..6 {
        muted.mute_ym2612_channel(channel, true);
    }
    let (mut past_the_end, mut heard_apart) = (0, false);
    for frame in 1..=60 {
        for emu in [&mut plain, &mut integrated, &mut muted] {
            emu.run_frame();
        }
        past_the_end += usize::from(plain.savestate_extension().ym2612_output.len() > 1);
        assert_eq!(
            integrated.state_hash(),
            plain.state_hash(),
            "frame {frame}: the DAC's model"
        );
        assert_eq!(
            muted.state_hash(),
            plain.state_hash(),
            "frame {frame}: the mutes"
        );
        heard_apart |= integrated.get_audio_samples() != plain.get_audio_samples();
        let _ = muted.get_audio_samples();
    }
    assert!(
        past_the_end > 0,
        "no frame's end found a YM2612 sample past it"
    );
    assert!(heard_apart, "the two DACs play alike");

    let mut emu = GenesisEmulator::new();
    assert_eq!(emu.fm_dac_model(), FmDacModel::Discrete, "the default");
    emu.set_fm_dac_model(FmDacModel::Integrated);
    emu.load_rom(&rom()).expect("the program loads");
    emu.run_frame();
    let state = emu.save_state();
    emu.reset();
    emu.power_cycle();
    emu.load_rom(&other_rom()).expect("the other program loads");
    emu.load_state(&state, retro_core::RomCheck::AnyRom)
        .expect("the state loads");
    assert_eq!(
        emu.fm_dac_model(),
        FmDacModel::Integrated,
        "kept throughout"
    );
}
