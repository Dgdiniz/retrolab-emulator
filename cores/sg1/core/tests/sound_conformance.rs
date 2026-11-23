//! The sound conformance suite (`retro_core::audio::conformance`) on the
//! Mega Drive: a program that loops on one instruction and writes no sound
//! register, and a sound moment that keys an FM note on the YM2612's first
//! channel and a tone on the PSG's second.

use retro_core::audio::conformance::SoundCoreUnderTest;
use retro_core::{EmulatorCore, ExecutionMode};
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
    MiniCartridgeBuilder::new()
        .with_size(0x80000)
        .with_title(title)
        .with_instructions(LOOP as usize, &[0x6000, 0xFFFE]) // BRA.w LOOP
        .build()
}

fn rom() -> Vec<u8> {
    program("SOUND CONFORMANCE")
}

fn other_rom() -> Vec<u8> {
    program("SOUND CONFORMANCE B")
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
    }
}

retro_core::sound_conformance_tests!(mega_drive());
