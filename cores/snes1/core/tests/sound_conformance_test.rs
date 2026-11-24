//! The sound conformance suite (`retro_core::audio::conformance`) on the
//! SNES: a LoROM that loops on one instruction and never wakes the sound
//! processor, which waits in its boot ROM, a sound moment that writes a
//! looping sample into ARAM and keys it on voice 0, routed to the echo
//! with the echo's writes on, and a LoROM that writes the mailbox's
//! fourth port.

use retro_core::audio::conformance::SoundCoreUnderTest;
use retro_core::audio::{
    SoundCore, SoundEvent, SoundLogContents, SoundSamples, StemLayout, StemTake,
};
use retro_core::{EmulatorCore, ExecutionMode};
use snes1_core::SnesEmulator;

/// Where the program loops: `BRA` to itself, at the reset vector.
const LOOP: u32 = 0x00_8000;

/// The sample's directory, page 2: entry 0 starts and loops at `SAMPLE`.
const DIRECTORY: u16 = 0x0200;
const SAMPLE: u16 = 0x0210;

/// One BRR block that ends and loops: shift 12, filter 0, then eight
/// nibbles of 7 and eight of -7, a square wave.
const BLOCK: [u8; 9] = [0xC3, 0x77, 0x77, 0x77, 0x77, 0x99, 0x99, 0x99, 0x99];

/// The S-DSP's registers, written in this order: the directory, the echo
/// (at $8000, 2 KiB, its feedback, its volume and a filter that passes the
/// frame read), the main volume, voice 0 (full volume, pitch 0x1000,
/// source 0, the fastest attack and a full sustain) routed to the echo,
/// FLG cleared (no soft reset, no mute, the echo's writes on), and voice 0
/// keyed on.
#[rustfmt::skip]
const DSP: [(u8, u8); 30] = [
    (0x5D, 0x02), (0x6D, 0x80), (0x7D, 0x01), (0x0D, 0x40),
    (0x2C, 0x40), (0x3C, 0x40), (0x0F, 0x7F), (0x1F, 0x00),
    (0x2F, 0x00), (0x3F, 0x00), (0x4F, 0x00), (0x5F, 0x00),
    (0x6F, 0x00), (0x7F, 0x00), (0x0C, 0x7F), (0x1C, 0x7F),
    (0x00, 0x7F), (0x01, 0x7F), (0x02, 0x00), (0x03, 0x10),
    (0x04, 0x00), (0x05, 0x8F), (0x06, 0xE0), (0x07, 0x00),
    (0x2D, 0x00), (0x3D, 0x00), (0x4D, 0x01), (0x5C, 0x00),
    (0x6C, 0x00), (0x4C, 0x01),
];

fn program(title: &[u8; 21]) -> Vec<u8> {
    program_of(title, &[0x80, 0xFE]) // BRA LOOP
}

/// A LoROM that runs `code` from `LOOP`, the reset vector, in the 65C816's
/// emulation mode.
fn program_of(title: &[u8; 21], code: &[u8]) -> Vec<u8> {
    let mut chip = vec![0u8; 0x10_0000];
    chip[..code.len()].copy_from_slice(code);
    let h = 0x7FB0;
    chip[h + 0x10..h + 0x10 + 21].copy_from_slice(title);
    chip[h + 0x25] = 0x20; // LoROM
    chip[h + 0x26] = 0x00; // ROM only
    chip[h + 0x27] = 0x0A; // 1 MiB
    chip[h + 0x29] = 0x01; // North America: NTSC
    chip[h + 0x2A] = 0x01;
    let checksum: u16 = 0x1234;
    chip[h + 0x2C..h + 0x2E].copy_from_slice(&(!checksum).to_le_bytes());
    chip[h + 0x2E..h + 0x30].copy_from_slice(&checksum.to_le_bytes());
    chip[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    chip
}

fn rom() -> Vec<u8> {
    program(b"SOUND CONFORMANCE    ")
}

fn other_rom() -> Vec<u8> {
    program(b"SOUND CONFORMANCE B  ")
}

/// The port the mailbox program writes: port 3, `$2143`.
const MAILBOX_PORT: u16 = 3;

/// A LoROM that writes 0, 1, 2 and on into the mailbox's port 3, about a
/// hundred and fifty times a frame. The sound processor waits in its boot
/// ROM, which reads port 0 alone.
fn mailbox_rom() -> Vec<u8> {
    #[rustfmt::skip]
    let code = [
        0xA9, 0x00,       // lda #$00
        0x8D, 0x43, 0x21, // loop: sta $2143
        0x1A,             // inc a
        0xA2, 0x40,       // ldx #$40
        0xCA,             // delay: dex
        0xD0, 0xFD,       // bne delay
        0x80, 0xF5,       // bra loop
    ];
    program_of(b"SOUND CONFORMANCE M  ", &code)
}

fn build() -> SnesEmulator {
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom()).expect("the program loads");
    emu.set_audio_enabled(true);
    emu.set_execution_mode(ExecutionMode::Running);
    emu
}

fn sound_moment(emu: &mut SnesEmulator) {
    let apu = emu.bus_mut().apu_mut();
    for (i, byte) in [SAMPLE, SAMPLE]
        .iter()
        .flat_map(|a| a.to_le_bytes())
        .enumerate()
    {
        apu.spc_write(DIRECTORY + i as u16, byte);
    }
    for (i, &byte) in BLOCK.iter().enumerate() {
        apu.spc_write(SAMPLE + i as u16, byte);
    }
    for (register, value) in DSP {
        apu.spc_write(0x00F2, register);
        apu.spc_write(0x00F3, value);
    }
}

fn snes() -> SoundCoreUnderTest<SnesEmulator> {
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

retro_core::sound_conformance_tests!(snes());

/// The mailbox is the S-CPU's four ports, mirrored every four bytes through
/// `$217F`: a write to a mirror is its port's, in the order they came; what
/// the sound processor writes back is not the mailbox's.
#[test]
fn the_mailbox_is_the_four_ports_and_their_mirrors() {
    let mut emu = build();
    for _ in 0..4 {
        emu.run_frame();
    }
    emu.start_sound_log(SoundLogContents {
        mailbox: true,
        ..SoundLogContents::default()
    });
    emu.write_memory_debug(0x2140, &[0x11]);
    emu.write_memory_debug(0x2147, &[0x22]);
    emu.write_memory_debug(0x217D, &[0x33]);
    emu.bus_mut().apu_mut().spc_write(0x00F5, 0x44);
    emu.render_frame();
    let log = emu.take_sound_log().expect("a log runs");
    let writes: Vec<(u16, u8)> = log
        .entries
        .iter()
        .filter_map(|entry| match entry.event {
            SoundEvent::Mailbox { port, value } => Some((port, value)),
            _ => None,
        })
        .collect();
    assert_eq!(writes, [(0, 0x11), (3, 0x22), (1, 0x33)]);
}

// -- The S-DSP's stems ---------------------------------------------------

/// The sound moment with a second voice, not routed to the echo, and the
/// levels lowered so that no sum clamps: voice 0 at half volume, voice 1
/// at 0x30 left and 0x50 right an octave lower, the main volume halved.
#[rustfmt::skip]
const SECOND_VOICE: [(u8, u8); 11] = [
    (0x00, 0x40), (0x01, 0x40), (0x0C, 0x40), (0x1C, 0x40),
    (0x10, 0x30), (0x11, 0x50), (0x12, 0x00), (0x13, 0x08),
    (0x15, 0x8F), (0x16, 0xE0), (0x4C, 0x02),
];

/// A machine with its sound off (the APU's ring then waits for the test),
/// past the sound moment and a second voice, every stem armed, and the
/// samples made before taken out of the ring.
fn two_voices() -> (SnesEmulator, StemLayout) {
    let mut emu = build();
    emu.set_audio_enabled(false);
    for _ in 0..4 {
        emu.run_frame();
    }
    sound_moment(&mut emu);
    let apu = emu.bus_mut().apu_mut();
    for (register, value) in SECOND_VOICE {
        apu.spc_write(0x00F2, register);
        apu.spc_write(0x00F3, value);
    }
    let layout = emu.stem_layout().expect("the SNES takes stems");
    emu.arm_stems(layout.all());
    let mut ring = vec![0i16; 8192];
    emu.bus_mut().apu_mut().drain_audio(&mut ring);
    (emu, layout)
}

/// The integers of a take's buffer.
fn ints(samples: &SoundSamples) -> &[i16] {
    match samples {
        SoundSamples::I16(samples) => samples,
        SoundSamples::F32(_) => panic!("the S-DSP's stems are 16-bit"),
    }
}

/// A frame run and taken: the take, and what the APU's ring received.
fn frame(emu: &mut SnesEmulator) -> (StemTake, Vec<i16>) {
    emu.render_frame();
    let take = emu.take_stems().expect("armed");
    let mut ring = vec![0i16; 8192];
    let received = emu.bus_mut().apu_mut().drain_audio(&mut ring);
    ring.truncate(received);
    (take, ring)
}

/// The master is the sample the S-DSP pushed to its ring, sample for
/// sample; with two voices and the echo at levels nothing clamps, no
/// sample departs and the voices through their volumes and the echo sum
/// to it within one step a voice.
#[test]
fn the_master_is_the_dsp_s_ring_and_two_voices_and_the_echo_sum_to_it() {
    let (mut emu, layout) = two_voices();
    let mut heard = false;
    for frame_number in 0..12 {
        let (take, ring) = frame(&mut emu);
        assert_eq!(ints(&take.master), &ring[..], "frame {frame_number}");
        assert!(
            take.departures.is_empty(),
            "frame {frame_number}: {:?}",
            take.departures
        );
        for sample in 0..take.frames as usize {
            for side in 0..2 {
                let mut sum = 0i32;
                for (stem, index) in take.stems.iter().zip(take.armed.iter()) {
                    let channel = &layout.channels[index];
                    if channel.in_master {
                        sum += i32::from(ints(stem)[sample * channel.width() + side]);
                    }
                }
                let master = i32::from(ring[sample * 2 + side]);
                assert!(
                    (master - sum).abs() <= 8,
                    "sample {sample}: {master} against {sum}"
                );
                heard |= master != 0;
            }
        }
    }
    assert!(heard, "the voices sounded");
    let left = ints(&emu.take_stems().expect("armed").master).len();
    assert_eq!(left, 0, "taken frame by frame");
}

/// A muted voice keeps its stems, dry and through its volumes, and its
/// samples depart from the master, which the speakers' mix leaves it out
/// of; the master is still the ring's sample, and the scope shows the
/// voice.
#[test]
fn a_muted_voice_keeps_its_stems_and_its_samples_depart() {
    let (mut emu, layout) = two_voices();
    emu.audio_debugger(0)
        .expect("the S-DSP")
        .mute_channel(1, true);
    let (take, ring) = frame(&mut emu);
    assert_eq!(ints(&take.master), &ring[..]);
    for name in ["voice1", "voice1-dry"] {
        let stem = ints(&take.stems[layout.index_of(name).expect(name)]);
        assert!(stem.iter().any(|&s| s != 0), "{name} plays");
    }
    let departed: u32 = take.departures.iter().map(|span| span.frames).sum();
    assert!(departed > take.frames / 2, "{departed} of {}", take.frames);
    let wave = emu
        .audio_debugger(0)
        .expect("the S-DSP")
        .get_waveform(1, 64);
    assert!(wave.iter().any(|&s| s != 0.0), "the scope shows the voice");
}
