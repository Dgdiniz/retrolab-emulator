//! The sound conformance suite (`retro_core::audio::conformance`) on the
//! SNES: a LoROM that loops on one instruction and never wakes the sound
//! processor, which waits in its boot ROM, and a sound moment that writes
//! a looping sample into ARAM and keys it on voice 0, routed to the echo
//! with the echo's writes on.

use retro_core::audio::conformance::SoundCoreUnderTest;
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

fn rom() -> Vec<u8> {
    let mut chip = vec![0u8; 0x10_0000];
    chip[..2].copy_from_slice(&[0x80, 0xFE]); // BRA LOOP
    let h = 0x7FB0;
    chip[h + 0x10..h + 0x10 + 21].copy_from_slice(b"SOUND CONFORMANCE    ");
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
    }
}

retro_core::sound_conformance_tests!(snes());
