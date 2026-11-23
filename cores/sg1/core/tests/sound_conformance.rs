//! The sound conformance suite (`retro_core::audio::conformance`) on the
//! Mega Drive: a program that loops on one instruction and writes no sound
//! register, and a sound moment that keys an FM note on the YM2612's first
//! channel and a tone on the PSG's second.

use retro_core::audio::conformance::SoundCoreUnderTest;
use retro_core::audio::{SoundCore, SoundSamples, StemLayout, StemTake};
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

// -- The Mega Drive's stems ----------------------------------------------

/// A machine past the sound moment with every stem armed.
fn armed() -> (GenesisEmulator, StemLayout) {
    let mut emu = build();
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
/// limiter's threshold, the host's very samples, and none departs.
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
    assert!(take.departures.is_empty(), "{:?}", take.departures);
    assert!(
        take.frames > 700,
        "a frame's sample frames: {}",
        take.frames
    );
}

/// A muted channel's stem is its sound still, and its samples depart from
/// the master, which leaves it out; the scope shows the channel too.
#[test]
fn a_muted_channel_keeps_its_stem_and_its_samples_depart() {
    let (mut emu, layout) = armed();
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

/// While the DAC plays in channel 6's place the `dac` stem holds it, as
/// it enters the master, and channel 6's FM stem is silent.
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
    let level = ((0xF0 - 128) << 6) as f32 / 8192.0 * 0.75 / 6.0;
    assert!(stem(&take, &layout, "dac").iter().all(|&s| s == level));
    assert!(stem(&take, &layout, "fm6").iter().all(|&s| s == 0.0));
}
