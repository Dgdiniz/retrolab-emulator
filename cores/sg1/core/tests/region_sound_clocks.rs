//! The sound chips run on the clocks of the machine's region.
//!
//! The PSG runs on the Z80's clock and the YM2612 on the 68000's, each the
//! region's master clock divided, so a PAL console's chips run 0.9 % slower
//! than an NTSC console's and make each frame's sound for a frame of their
//! clock. Chips left on the other region's clocks make 0.9 % too few samples
//! for a PAL frame, which then ends in silence (six to nine stereo pairs,
//! a buzz at the frame rate) with its notes 0.9 % high, or 0.9 % too many
//! for an NTSC frame, whose sound then loses its end with its notes 0.9 %
//! low. A bus builds its chips on the clocks of the region it is built in;
//! the machine's region reaches them on every path a region reaches the
//! machine: a ROM's load (which builds the bus in the region the machine
//! had, then takes the header's), a region change's power cycle, a state's
//! load.

mod helpers;

use helpers::M68kRomBuilder;
use retro_core::audio::{AudioChip, AudioDebugger};
use retro_core::cpu::Clockable;
use retro_core::{EmulatorCore, RomCheck};
use sg1_core::psg::Psg;
use sg1_core::timing::{GenesisTiming, VideoRegion};
use sg1_core::ym2612::Ym2612;
use sg1_core::GenesisEmulator;

/// The PSG's bytes for a chord that holds: tone 0 at period $0FE and full
/// volume, tone 1 at period $1AB and attenuation 4, white noise at
/// attenuation 8.
const PSG_NOTE: &[u8] = &[0x8E, 0x0F, 0x90, 0xAB, 0x1A, 0xB4, 0xE4, 0xF8];

/// The YM2612's registers, part 0, for a note on channel 1: algorithm 7,
/// operators 1 and 4 sounding, each detuned and multiplied apart, operator
/// 4's envelope decaying to a sustain level, the LFO's vibrato and tremolo
/// on, block 4, F-number 1083 (440 Hz on an NTSC clock, 436 Hz on a PAL
/// one), then the key on.
const FM_NOTE: &[(u8, u8)] = &[
    (0x22, 0x0D), // LFO on, rate 5
    (0x27, 0x00), // channel 3 normal
    (0x28, 0x00), // key off
    (0xB0, 0x07), // algorithm 7, no feedback
    (0xB4, 0xD3), // left and right, AMS 1, PMS 3
    (0x30, 0x31), // operator 1: DT 3, MUL 1
    (0x34, 0x02), // operator 3: MUL 2
    (0x38, 0x02), // operator 2: MUL 2
    (0x3C, 0x52), // operator 4: DT 5, MUL 2
    (0x40, 0x10), // operator 1: TL 16
    (0x44, 0x7F), // operators 3 and 2 silent
    (0x48, 0x7F),
    (0x4C, 0x08), // operator 4: TL 8
    (0x50, 0x1F), // AR 31 for all four
    (0x54, 0x1F),
    (0x58, 0x1F),
    (0x5C, 0x1F),
    (0x60, 0x00), // D1R: operator 1 none
    (0x64, 0x00),
    (0x68, 0x00),
    (0x6C, 0x86), // operator 4: AM on, D1R 6
    (0x70, 0x00), // D2R
    (0x74, 0x00),
    (0x78, 0x00),
    (0x7C, 0x02), // operator 4: D2R 2
    (0x80, 0x0F), // SL 0, RR 15
    (0x84, 0x0F),
    (0x88, 0x0F),
    (0x8C, 0x4F), // operator 4: SL 4, RR 15
    (0xA4, 0x24), // block 4, F-number high bits (1083 >> 8)
    (0xA0, 0x3B), // F-number low bits
    (0x28, 0xF0), // key on: channel 1, all operators
];

/// A program that plays [`PSG_NOTE`] and [`FM_NOTE`] and loops, its header
/// naming `regions`. The YM2612's ports are reached through the Z80's bus,
/// granted at once while the Z80 is held in reset.
fn tone_rom(regions: &[u8]) -> Vec<u8> {
    let mut b = M68kRomBuilder::new(0x400);
    for &byte in PSG_NOTE {
        b.move_b_imm_to_abs(byte, 0xC00011);
    }
    b.move_b_imm_to_abs(0x01, 0xA11100);
    for &(register, value) in FM_NOTE {
        b.move_b_imm_to_abs(register, 0xA04000);
        b.move_b_imm_to_abs(value, 0xA04001);
    }
    b.bra_s(-2);
    let mut rom = b.build();
    rom[0x1F0..0x200].fill(b' ');
    rom[0x1F0..0x1F0 + regions.len()].copy_from_slice(regions);
    rom
}

fn loaded(rom: &[u8]) -> GenesisEmulator {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(rom).expect("the program loads");
    emu
}

/// Each frame's samples, interleaved left and right.
fn listen(emu: &mut GenesisEmulator, frames: usize) -> Vec<Vec<f32>> {
    (0..frames)
        .map(|_| {
            emu.run_frame();
            emu.get_audio_samples().to_vec()
        })
        .collect()
}

/// Whether a frame's sound ends in four silent stereo pairs, which a held
/// chord of square waves and an FM note never makes.
fn ends_in_silence(frame: &[f32]) -> bool {
    frame.len() >= 8 && frame[frame.len() - 8..].iter().all(|&s| s == 0.0)
}

/// The first frame whose sound differs between two runs.
fn first_difference(a: &[Vec<f32>], b: &[Vec<f32>]) -> Option<usize> {
    (0..a.len().max(b.len())).find(|&i| a.get(i) != b.get(i))
}

fn assert_clocks(emu: &GenesisEmulator, region: VideoRegion, path: &str) {
    assert_eq!(emu.region(), region, "{path}: the region");
    assert_eq!(
        u64::from(emu.psg().clock_freq()),
        region.master_clock() / 15,
        "{path}: the PSG on the Z80's clock"
    );
    assert_eq!(
        u64::from(emu.ym2612().clock_freq()),
        region.cpu_clock(),
        "{path}: the YM2612 on the 68000's clock"
    );
}

#[test]
fn a_pal_only_cartridge_sounds_on_its_first_load_as_after_a_power_cycle() {
    let rom = tone_rom(b"E");
    let mut first = loaded(&rom);
    let mut cycled = loaded(&rom);
    cycled.hard_reset();
    assert_eq!(first.region(), VideoRegion::Pal);
    assert_eq!(cycled.region(), VideoRegion::Pal);

    let heard = listen(&mut first, 60);
    let reference = listen(&mut cycled, 60);
    let silent = heard.iter().filter(|f| ends_in_silence(f)).count();
    assert_eq!(silent, 0, "{silent} of 60 frames end in silence");
    assert!(
        first
            .ym2612()
            .get_channel_state(0)
            .is_some_and(|c| c.enabled),
        "the FM note is keyed on"
    );
    assert_eq!(
        first_difference(&heard, &reference),
        None,
        "the first frame whose sound is not the power cycle's"
    );
    assert_eq!(first.state_hash(), cycled.state_hash());
}

#[test]
fn the_sound_chips_take_the_regions_clocks_on_every_path_a_region_takes() {
    let pal_rom = tone_rom(b"E");
    let ntsc_rom = tone_rom(b"U");

    let mut emu = GenesisEmulator::new();
    assert_clocks(&emu, VideoRegion::Ntsc, "a new machine");
    emu.load_rom(&pal_rom).unwrap();
    assert_clocks(&emu, VideoRegion::Pal, "a PAL-only cartridge's first load");
    listen(&mut emu, 2);
    emu.load_rom(&ntsc_rom).unwrap();
    assert_clocks(&emu, VideoRegion::Ntsc, "an NTSC cartridge after a PAL one");
    listen(&mut emu, 2);
    emu.load_rom(&pal_rom).unwrap();
    assert_clocks(&emu, VideoRegion::Pal, "a PAL cartridge after an NTSC one");
    emu.set_region(VideoRegion::Ntsc).unwrap();
    assert_clocks(&emu, VideoRegion::Ntsc, "set_region(Ntsc)");
    emu.load_rom(&pal_rom).unwrap();
    assert_clocks(&emu, VideoRegion::Pal, "the same cartridge loaded again");
    emu.set_region(VideoRegion::Ntsc).unwrap();
    emu.set_region(VideoRegion::Pal).unwrap();
    assert_clocks(&emu, VideoRegion::Pal, "set_region(Pal)");
    emu.hard_reset();
    assert_clocks(&emu, VideoRegion::Pal, "hard_reset");
    emu.power_on_reset();
    assert_clocks(&emu, VideoRegion::Pal, "power_on_reset");

    // A state takes its own region, chips included, whatever the loading
    // machine's.
    listen(&mut emu, 2);
    let pal_state = emu.save_state();
    emu.set_region(VideoRegion::Ntsc).unwrap();
    emu.load_state(&pal_state, RomCheck::SameRom).unwrap();
    assert_clocks(&emu, VideoRegion::Pal, "a PAL state in an NTSC machine");
    let mut emu = loaded(&ntsc_rom);
    listen(&mut emu, 2);
    let ntsc_state = emu.save_state();
    emu.set_region(VideoRegion::Pal).unwrap();
    emu.load_state(&ntsc_state, RomCheck::SameRom).unwrap();
    assert_clocks(&emu, VideoRegion::Ntsc, "an NTSC state in a PAL machine");
}

#[test]
fn a_state_plays_on_as_the_machine_that_saved_it_whatever_clocks_the_loader_had() {
    for (regions, other) in [
        (&b"E"[..], VideoRegion::Ntsc),
        (&b"U"[..], VideoRegion::Pal),
    ] {
        let rom = tone_rom(regions);
        let mut saver = loaded(&rom);
        listen(&mut saver, 10);
        let state = saver.save_state();
        let expected = listen(&mut saver, 60);

        let mut loader = loaded(&rom);
        loader.set_region(other).unwrap();
        loader.load_state(&state, RomCheck::SameRom).unwrap();
        let heard = listen(&mut loader, 60);
        let header = String::from_utf8_lossy(regions);
        assert_eq!(
            first_difference(&heard, &expected),
            None,
            "a \"{header}\" state: the first frame whose sound differs"
        );
        assert_eq!(
            loader.state_hash(),
            saver.state_hash(),
            "a \"{header}\" state"
        );
    }
}

/// The YM2612's clocks: NTSC's and PAL's 68000.
const YM_NTSC: u32 = 7_670_453;
const YM_PAL: u32 = 7_600_489;
/// The PSG's: NTSC's and PAL's Z80.
const PSG_NTSC: u32 = 3_579_545;
const PSG_PAL: u32 = 3_546_894;

#[test]
fn the_clocks_are_the_regions() {
    assert_eq!(u64::from(YM_NTSC), VideoRegion::Ntsc.cpu_clock());
    assert_eq!(u64::from(YM_PAL), VideoRegion::Pal.cpu_clock());
    assert_eq!(u64::from(PSG_NTSC), VideoRegion::Ntsc.master_clock() / 15);
    assert_eq!(u64::from(PSG_PAL), VideoRegion::Pal.master_clock() / 15);
}

/// The chip's sound over fifty frames of `cycles` input clocks each, spread
/// over 313 lines, `pairs` samples (stereo pairs when `stereo`) taken a
/// frame.
fn frames_of<C: Clockable + AudioChip>(
    chip: &mut C,
    cycles: u32,
    pairs: usize,
    stereo: bool,
) -> Vec<Vec<f32>> {
    let width = if stereo { 2 } else { 1 };
    (0..50)
        .map(|_| {
            // A frame's clocks in lines, as the machine clocks the chips.
            for line in 0..313 {
                chip.clock(cycles / 313 + u32::from(line < cycles % 313));
            }
            let mut buf = vec![0.0f32; pairs * width];
            let n = chip.generate_samples(&mut buf, 44_100);
            buf.truncate(n);
            buf
        })
        .collect()
}

#[test]
fn a_chip_moved_to_another_clock_plays_as_one_built_on_it() {
    let mut moved = Ym2612::new(YM_NTSC, 44_100);
    let mut built = Ym2612::new(YM_PAL, 44_100);
    for chip in [&mut moved, &mut built] {
        for &(register, value) in FM_NOTE {
            chip.write_register(0, register, value);
        }
    }
    moved.set_clock_freq(YM_PAL);
    assert_eq!(moved.clock_freq(), YM_PAL);
    let state = |chip: &Ym2612| format!("{:?}", chip.to_savestate());
    assert_eq!(
        state(&moved),
        state(&built),
        "the YM2612's phase increments"
    );
    // A PAL frame: 1,070,460 master clocks, 152,922 of the 68000's.
    let sound = frames_of(&mut moved, 152_922, 887, true);
    let reference = frames_of(&mut built, 152_922, 887, true);
    assert_eq!(first_difference(&sound, &reference), None, "the YM2612");
    assert!(
        sound.iter().flatten().any(|&s| s != 0.0),
        "the FM note sounds"
    );
    assert_eq!(state(&moved), state(&built));

    let mut moved = Psg::new(PSG_NTSC, 44_100);
    let mut built = Psg::new(PSG_PAL, 44_100);
    for chip in [&mut moved, &mut built] {
        for &byte in PSG_NOTE {
            chip.write(byte);
        }
    }
    moved.set_clock_freq(PSG_PAL);
    assert_eq!(moved.clock_freq(), PSG_PAL);
    let tone = |chip: &Psg| chip.get_channel_state(0).map(|c| c.frequency);
    assert_eq!(tone(&moved), tone(&built), "the debugger's tone in Hz");
    // The Z80's 71,364 a PAL frame.
    let sound = frames_of(&mut moved, 71_364, 887, false);
    let reference = frames_of(&mut built, 71_364, 887, false);
    assert_eq!(first_difference(&sound, &reference), None, "the PSG");
    assert!(
        sound
            .iter()
            .all(|f| f.len() == 887 && f.iter().all(|&s| s != 0.0)),
        "every PSG sample sounds"
    );
}

#[test]
fn a_chip_moved_to_its_own_clock_keeps_what_it_holds() {
    let mut chip = Ym2612::new(YM_PAL, 44_100);
    for &(register, value) in FM_NOTE {
        chip.write_register(0, register, value);
    }
    frames_of(&mut chip, 152_922, 887, true);
    // A block written alone: the phase increments it left were worked out
    // before the key code it sets, which working them out again would use.
    chip.write_register(0, 0xA4, 0x34);
    let before = format!("{:?}", chip.to_savestate());
    chip.set_clock_freq(YM_PAL);
    assert_eq!(format!("{:?}", chip.to_savestate()), before);

    let mut psg = Psg::new(PSG_PAL, 44_100);
    for &byte in PSG_NOTE {
        psg.write(byte);
    }
    frames_of(&mut psg, 71_364, 887, false);
    let before = format!("{:?}", psg.to_savestate());
    psg.set_clock_freq(PSG_PAL);
    assert_eq!(format!("{:?}", psg.to_savestate()), before);
}
