//! What the machine does on a fixed program, recorded as literal values so
//! that a change to how it is reset, saved, loaded, switched between regions
//! or told to the host shows here unless the change meant it: every form a
//! savestate can take and what a load answers, the state hash and its parts,
//! what each reset leaves, the sound, the system information, the region.
//!
//! A change that means to move a value says which, beside it. To print the
//! table the code gives now, in the form of `RECORDED`:
//!
//! ```bash
//! MACHINE_BASELINE_PRINT=1 cargo test -p sg1-core --test machine_baseline -- --nocapture
//! ```

use std::time::Instant;

use retro_core::savestate::{
    deserialize_compressed, serialize_compressed, RomCheck, SaveStateEnvelope,
};
use retro_core::{
    Breakpoint, BreakpointType, DebuggerCore, EmulatorCore, ExecutionMode, InputState,
};
use sg1_core::savestate::{serialize_and_compress, SystemSaveState};
use sg1_core::GenesisEmulator;

fn put_words(rom: &mut [u8], at: usize, words: &[u16]) {
    for (i, w) in words.iter().enumerate() {
        rom[at + 2 * i] = (w >> 8) as u8;
        rom[at + 2 * i + 1] = (*w & 0xFF) as u8;
    }
}

/// The program: it turns the display on, writes a word of VRAM, sets the
/// user stack pointer, counts its boots in work RAM ($FF0010), plays a PSG
/// tone, then reads pad 1 forever and adds each read to $FF0000. The header
/// declares battery RAM, so a reset that keeps the save shows.
fn program() -> Vec<u8> {
    let mut rom = vec![0u8; 0x20000];
    put_words(&mut rom, 0, &[0x00FF, 0xFE00, 0x0000, 0x0200]);
    rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
    rom[0x120..0x138].copy_from_slice(b"MACHINE BASELINE        ");
    rom[0x150..0x168].copy_from_slice(b"MACHINE BASELINE        ");
    rom[0x1A0..0x1A8].copy_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0xFF, 0xFF]);
    // Battery RAM at $200001-$203FFF, odd bytes.
    rom[0x1B0..0x1BC].copy_from_slice(&[
        b'R', b'A', 0xF8, 0x20, 0x00, 0x20, 0x00, 0x01, 0x00, 0x20, 0x3F, 0xFF,
    ]);
    put_words(
        &mut rom,
        0x200,
        &[
            0x46FC, 0x2700, // MOVE.W #$2700,SR
            0x41F9, 0x00C0, 0x0004, // LEA $C00004,A0
            0x30BC, 0x8164, // register 1: display on, mode 5
            0x30BC, 0x8C81, // register 12: 40 cells
            0x30BC, 0x8F02, // register 15: auto-increment 2
            0x30BC, 0x8238, // register 2: plane A at $E000
            0x20BC, 0x4000, 0x0000, // MOVE.L #$40000000,(A0): VRAM write at 0
            0x33FC, 0xABCD, 0x00C0, 0x0000, // MOVE.W #$ABCD,$C00000
            0x43F9, 0x00FF, 0xE000, // LEA $FFE000,A1
            0x4E61, // MOVE A1,USP
            0x5239, 0x00FF, 0x0010, // ADDQ.B #1,$FF0010
            0x13FC, 0x008E, 0x00C0, 0x0011, // PSG: channel 0 tone, low bits
            0x13FC, 0x000F, 0x00C0, 0x0011, // PSG: high bits
            0x13FC, 0x0090, 0x00C0, 0x0011, // PSG: channel 0 at full volume
            0x1239, 0x00A1, 0x0003, // loop: MOVE.B $A10003,D1
            0xD339, 0x00FF, 0x0000, // ADD.B D1,$FF0000
            0x6000, 0xFFF2, // BRA.W loop
        ],
    );
    rom
}

/// C, then B, then nothing: buttons the pad shows with TH high.
fn schedule(frame: u64) -> u32 {
    match frame {
        120..=239 => 1 << 5,
        240..=359 => 1 << 4,
        _ => 0,
    }
}

/// A machine on the program, three-button pads in both ports, a save the
/// host put back, running.
fn booted(rom: &[u8]) -> GenesisEmulator {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(rom).expect("the program loads");
    emu.connect_controllers(false);
    let mut save = emu.get_save_ram().expect("battery RAM").to_vec();
    for (i, byte) in save.iter_mut().enumerate() {
        *byte = (i as u8).wrapping_mul(7) ^ 0x5A;
    }
    emu.set_save_ram(&save);
    emu.set_execution_mode(ExecutionMode::Running);
    emu
}

/// Frames `from..to` of the schedule, the sound drained as a host does.
fn run(emu: &mut GenesisEmulator, from: u64, to: u64) {
    for frame in from..to {
        emu.set_input(
            0,
            InputState {
                buttons: schedule(frame),
            },
        );
        emu.run_frame();
        let _ = emu.get_audio_samples();
    }
}

/// FNV-1a, 64 bits.
fn fnv(bytes: impl IntoIterator<Item = u8>) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn samples_hash(samples: &[f32]) -> u64 {
    fnv(samples.iter().flat_map(|s| s.to_bits().to_le_bytes()))
}

fn describe(emu: &mut GenesisEmulator) -> String {
    let work_ram = emu.read_memory_debug(0xFF_0000, 0x1_0000);
    format!(
        "hash {:016x} | work RAM {:016x} boots {} | save RAM {} | mode {:?} | breakpoints {} | region {} | frames {}",
        emu.state_hash(),
        fnv(work_ram.iter().copied()),
        work_ram[0x10],
        emu.get_save_ram()
            .map_or("none".to_string(), |s| format!("{:016x}", fnv(s.iter().copied()))),
        emu.debugger_ref().get_execution_mode(),
        emu.debugger_ref().list_breakpoints().len(),
        emu.region_str(),
        emu.frame_count(),
    )
}

/// Every value the baseline holds, in a fixed order.
fn record() -> Vec<(String, String)> {
    let rom = program();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut put = |key: &str, value: String| out.push((key.to_string(), value));

    // What a new machine and a load leave.
    let mut fresh = GenesisEmulator::new();
    put(
        "new machine",
        format!(
            "mode {:?} | region {} | audio {}",
            fresh.debugger_ref().get_execution_mode(),
            fresh.region_str(),
            fresh.is_audio_enabled()
        ),
    );
    fresh.load_rom(&rom).expect("the program loads");
    put(
        "after load_rom",
        format!(
            "mode {:?} | region {} | audio {} | frames {}",
            fresh.debugger_ref().get_execution_mode(),
            fresh.region_str(),
            fresh.is_audio_enabled(),
            fresh.frame_count()
        ),
    );

    // The state hash and its parts.
    let mut emu = booted(&rom);
    let mut at = 0;
    for frame in [1u64, 60, 300] {
        run(&mut emu, at, frame);
        at = frame;
        put(
            &format!("frame {frame}: state hash"),
            format!("{:016x}", emu.state_hash()),
        );
        let parts = emu
            .state_hash_parts()
            .iter()
            .map(|(name, hash)| format!("{name}={hash:016x}"))
            .collect::<Vec<_>>()
            .join(" ");
        put(&format!("frame {frame}: parts"), parts);
    }

    // Every form of savestate, loaded into a machine on the program.
    let mut saved_at = booted(&rom);
    run(&mut saved_at, 0, 60);
    let state = saved_at.save_state();
    let envelope =
        deserialize_compressed::<SystemSaveState>(&state).expect("the save opens as an envelope");
    put(
        "the envelope",
        format!(
            "envelope {} | core {} | version {} | rom hash {} | payload version {} | title {:?}",
            envelope.envelope_version,
            envelope.core_id,
            envelope.core_version,
            envelope.rom_hash,
            envelope.payload.version,
            envelope.payload.metadata.game_title
        ),
    );
    let reseal = |change: &dyn Fn(&mut SaveStateEnvelope<SystemSaveState>)| {
        let mut e = envelope.clone();
        change(&mut e);
        serialize_compressed(&e, 3).expect("seal")
    };
    let legacy = serialize_and_compress(&envelope.payload, 3).expect("legacy form");
    let mut other_game = envelope.payload.clone();
    other_game.metadata.rom_hash = "f".repeat(64);
    other_game.metadata.game_title = "ANOTHER GAME".to_string();
    let legacy_other = serialize_and_compress(&other_game, 3).expect("legacy form");
    let mut newer = envelope.payload.clone();
    newer.version += 1;
    let legacy_newer = serialize_and_compress(&newer, 3).expect("legacy form");
    let cases: Vec<(&str, Vec<u8>, RomCheck, bool)> = vec![
        ("envelope", state.clone(), RomCheck::SameRom, true),
        ("legacy", legacy, RomCheck::SameRom, true),
        (
            "legacy, another game",
            legacy_other.clone(),
            RomCheck::SameRom,
            true,
        ),
        (
            "legacy, another game, any ROM",
            legacy_other,
            RomCheck::AnyRom,
            true,
        ),
        (
            "legacy, a newer version",
            legacy_newer,
            RomCheck::SameRom,
            true,
        ),
        (
            "core version + 1",
            reseal(&|e| e.core_version += 1),
            RomCheck::SameRom,
            true,
        ),
        (
            "another core's id",
            reseal(&|e| e.core_id = "snes1".to_string()),
            RomCheck::SameRom,
            true,
        ),
        (
            "envelope version 2",
            reseal(&|e| e.envelope_version = 2),
            RomCheck::SameRom,
            true,
        ),
        (
            "another ROM",
            reseal(&|e| e.rom_hash = "0".repeat(64)),
            RomCheck::SameRom,
            true,
        ),
        (
            "another ROM, any ROM",
            reseal(&|e| e.rom_hash = "0".repeat(64)),
            RomCheck::AnyRom,
            true,
        ),
        ("no ROM loaded", state.clone(), RomCheck::SameRom, false),
        (
            "garbage",
            b"this is not a savestate".to_vec(),
            RomCheck::SameRom,
            true,
        ),
        ("empty", Vec::new(), RomCheck::SameRom, true),
    ];
    for (name, bytes, check, with_rom) in cases {
        let mut target = if with_rom {
            booted(&rom)
        } else {
            GenesisEmulator::new()
        };
        let answer = match target.load_state(&bytes, check) {
            Ok(()) => format!(
                "Ok | the saved machine's state: {}",
                target.state_hash() == saved_at.state_hash()
            ),
            Err(e) => format!("{e:?} | {e}"),
        };
        put(&format!("load: {name}"), answer);
    }

    // What each reset leaves, at frame 300 with a breakpoint armed.
    for name in ["reset", "hard_reset", "power_on_reset"] {
        let mut emu = booted(&rom);
        run(&mut emu, 0, 300);
        emu.debugger().add_breakpoint(Breakpoint {
            address: 0x00FF_FF00,
            bp_type: BreakpointType::execute(),
            cpu_id: Some(0),
            condition: None,
        });
        match name {
            "reset" => emu.reset(),
            "hard_reset" => emu.hard_reset(),
            _ => emu.power_on_reset(),
        }
        put(&format!("{name}: right after"), describe(&mut emu));
        emu.set_execution_mode(ExecutionMode::Running);
        run(&mut emu, 0, 2);
        put(&format!("{name}: two frames later"), describe(&mut emu));
    }

    // The sound, frame by frame.
    let mut emu = booted(&rom);
    emu.set_audio_enabled(true);
    let mut all = 0xcbf2_9ce4_8422_2325u64;
    let mut total = 0usize;
    for frame in 0..300u64 {
        emu.set_input(
            0,
            InputState {
                buttons: schedule(frame),
            },
        );
        emu.run_frame();
        let samples = emu.get_audio_samples().to_vec();
        let hash = samples_hash(&samples);
        all = (all ^ hash).wrapping_mul(0x0000_0100_0000_01b3);
        total += samples.len();
        if [0u64, 59, 299].contains(&frame) {
            put(
                &format!("sound, frame {}", frame + 1),
                format!("{} samples {hash:016x}", samples.len()),
            );
        }
    }
    put("sound, 300 frames", format!("{total} samples {all:016x}"));

    // What the host is told.
    let emu = booted(&rom);
    put(
        "system info",
        serde_json::to_string(emu.system_info()).expect("system info as JSON"),
    );

    // The region: chosen, and carried by a savestate.
    let mut pal = booted(&rom);
    pal.set_region_str("pal").expect("pal");
    put(
        "set_region_str(pal)",
        format!(
            "region {} | refresh {:.4} | hash {:016x}",
            pal.region_str(),
            pal.get_display_info().refresh_rate,
            pal.state_hash()
        ),
    );
    // A PAL state loaded into an NTSC machine is the machine conformance
    // suite's (`the_region_survives_a_savestate`).
    run(&mut pal, 0, 60);
    put(
        "a PAL machine, 60 frames",
        format!(
            "frames {} | hash {:016x} | refresh {:.4}",
            pal.frame_count(),
            pal.state_hash(),
            pal.get_display_info().refresh_rate
        ),
    );
    put(
        "set_region_str(secam)",
        format!("{:?}", booted(&rom).set_region_str("secam")),
    );
    out
}

/// The values the code gives. Where one moved when both consoles' machine
/// interface became retro-core's, the comment above it says what for.
const RECORDED: &[(&str, &str)] = &[
    // No construction, reset or load changes the debugger's mode: a new
    // machine runs.
    ("new machine", "mode Running | region ntsc | audio true"),
    // A load leaves the mode as it was.
    ("after load_rom", "mode Running | region ntsc | audio true | frames 0"),
    ("frame 1: state hash", "a1a1eb1c57fb6620"),
    ("frame 1: parts", "cpu68k=6b077e1553b1b732 z80=d833438a3ed05bdb vdp=83f41ad2b93b85bb psg=4eed1aa85cfb3e35 ym2612=efc29b4251347646 io=3c6fedfeb6914524 bus=93dce42ef61c1e7c timing=f49acf6a880a28eb ram=9c59a697fe657b9c z80_ram=2b549268bfc71525 cartridge=a8c7f832281a39c5 frame_count=0000000000000001"),
    ("frame 60: state hash", "ccd7b2fd1e4ebd75"),
    ("frame 60: parts", "cpu68k=0cc0c7e4121110fe z80=d833438a3ed05bdb vdp=a0e86f1e16e58e83 psg=c30663dccdf0fa93 ym2612=828155374cb1c3fc io=5ccb29e732035f28 bus=93dce42ef61c1e7c timing=c8e9547137799345 ram=48632110ad257b4c z80_ram=2b549268bfc71525 cartridge=a8c7f832281a39c5 frame_count=000000000000003c"),
    ("frame 300: state hash", "cbd72df6ea1a5e2d"),
    ("frame 300: parts", "cpu68k=62884b7c10d28f84 z80=d833438a3ed05bdb vdp=7ef7614cd4cafe58 psg=a88151fc562fed75 ym2612=f5c0e15a2a0bb7e4 io=042a3110292eaa1d bus=55e7561ce03d8a3a timing=97f881ccbb702d8e ram=d9ad915fdcd57c38 z80_ram=2b549268bfc71525 cartridge=a8c7f832281a39c5 frame_count=000000000000012c"),
    ("the envelope", "envelope 1 | core sg1 | version 3 | rom hash 5bdbda6d2a59f3d8650fdf88c75783d2d48f0468d10570953ffcbeb2cc24105b | payload version 3 | title \"MACHINE BASELINE\""),
    ("load: envelope", "Ok | the saved machine's state: true"),
    ("load: legacy", "Ok | the saved machine's state: true"),
    ("load: legacy, another game", "OtherRom { state_rom: \"ANOTHER GAME\", loaded_rom: \"MACHINE BASELINE\" } | ROM hash mismatch: savestate is for 'ANOTHER GAME' but loaded ROM is 'MACHINE BASELINE'"),
    ("load: legacy, another game, any ROM", "Ok | the saved machine's state: true"),
    ("load: legacy, a newer version", "Unreadable(\"Save state version 4 is newer than supported version 3\") | Save state version 4 is newer than supported version 3"),
    // A state of a newer build is refused (it loaded as this build's layout).
    ("load: core version + 1", "Unreadable(\"sg1 save state core_version 4 is newer than supported version 3\") | sg1 save state core_version 4 is newer than supported version 3"),
    // Another core's state is refused.
    ("load: another core's id", "Unreadable(\"savestate is for core 'snes1', not 'sg1'\") | savestate is for core 'snes1', not 'sg1'"),
    // A newer envelope is refused.
    ("load: envelope version 2", "Unreadable(\"savestate envelope version 2 is newer than supported version 1\") | savestate envelope version 2 is newer than supported version 1"),
    ("load: another ROM", "OtherRom { state_rom: \"0000000000000000000000000000000000000000000000000000000000000000\", loaded_rom: \"5bdbda6d2a59f3d8650fdf88c75783d2d48f0468d10570953ffcbeb2cc24105b\" } | ROM hash mismatch: savestate is for '0000000000000000000000000000000000000000000000000000000000000000' but loaded ROM is '5bdbda6d2a59f3d8650fdf88c75783d2d48f0468d10570953ffcbeb2cc24105b'"),
    ("load: another ROM, any ROM", "Ok | the saved machine's state: true"),
    ("load: no ROM loaded", "Ok | the saved machine's state: false"),
    ("load: garbage", "Unreadable(\"Decompression failed: Unknown frame descriptor\") | Decompression failed: Unknown frame descriptor"),
    ("load: empty", "Unreadable(\"Decompression failed: incomplete frame\") | Decompression failed: incomplete frame"),
    // No reset changes the debugger's mode. Right after a reset or a power
    // cycle the state hash covers what the savestate's extension holds
    // then: the first frame after the reset, and the beam before the
    // 68000's first step.
    ("reset: right after", "hash 8f2c3896786e4f91 | work RAM d96dbf20b7991b11 boots 1 | save RAM 3eeeb248c6ea2325 | mode Running | breakpoints 1 | region ntsc | frames 0"),
    ("reset: two frames later", "hash f0c9f6440172de78 | work RAM 071a9068ba8c33b4 boots 2 | save RAM 3eeeb248c6ea2325 | mode Running | breakpoints 1 | region ntsc | frames 2"),
    // The app's Reset is a power cycle that keeps the save: a new 68000,
    // whose user stack pointer is clear (the reset button keeps it); the
    // mode is kept.
    ("hard_reset: right after", "hash 9370832a4b3e1532 | work RAM eb05052ea5b62325 boots 0 | save RAM 3eeeb248c6ea2325 | mode Running | breakpoints 1 | region ntsc | frames 0"),
    ("hard_reset: two frames later", "hash c8de137910717fca | work RAM d3f8a09307d72bfa boots 1 | save RAM 3eeeb248c6ea2325 | mode Running | breakpoints 1 | region ntsc | frames 2"),
    // Netplay's power cycle keeps the mode.
    ("power_on_reset: right after", "hash a719edc7e0f0d532 | work RAM eb05052ea5b62325 boots 0 | save RAM 0d65b7532d396325 | mode Running | breakpoints 0 | region ntsc | frames 0"),
    ("power_on_reset: two frames later", "hash 7e3b75fdbecdffca | work RAM d3f8a09307d72bfa boots 1 | save RAM 0d65b7532d396325 | mode Running | breakpoints 0 | region ntsc | frames 2"),
    ("sound, frame 1", "1470 samples 07ee79f5ea4a1265"),
    ("sound, frame 60", "1472 samples c9261c5eebf2bb25"),
    ("sound, frame 300", "1472 samples b36d9a838a611b25"),
    ("sound, 300 frames", "441568 samples a4de362dcebd88c9"),
    // Each button's id is the bit the six-button pad reads it from.
    ("system info", "{\"name\":\"Sega Genesis\",\"short_name\":\"genesis\",\"manufacturer\":\"Sega\",\"year\":1988,\"cpus\":[{\"id\":0,\"name\":\"Motorola 68000\",\"architecture\":\"M68000\",\"clock_speed\":7670454,\"registers\":[{\"name\":\"D0\",\"size_bits\":32,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"D1\",\"size_bits\":32,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"D2\",\"size_bits\":32,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"D3\",\"size_bits\":32,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"D4\",\"size_bits\":32,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"D5\",\"size_bits\":32,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"D6\",\"size_bits\":32,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"D7\",\"size_bits\":32,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"A0\",\"size_bits\":32,\"display_format\":\"Address\",\"category\":\"Address\"},{\"name\":\"A1\",\"size_bits\":32,\"display_format\":\"Address\",\"category\":\"Address\"},{\"name\":\"A2\",\"size_bits\":32,\"display_format\":\"Address\",\"category\":\"Address\"},{\"name\":\"A3\",\"size_bits\":32,\"display_format\":\"Address\",\"category\":\"Address\"},{\"name\":\"A4\",\"size_bits\":32,\"display_format\":\"Address\",\"category\":\"Address\"},{\"name\":\"A5\",\"size_bits\":32,\"display_format\":\"Address\",\"category\":\"Address\"},{\"name\":\"A6\",\"size_bits\":32,\"display_format\":\"Address\",\"category\":\"Address\"},{\"name\":\"A7\",\"size_bits\":32,\"display_format\":\"Address\",\"category\":\"Address\"},{\"name\":\"PC\",\"size_bits\":32,\"display_format\":\"Address\",\"category\":\"Special\"},{\"name\":\"SR\",\"size_bits\":16,\"display_format\":\"Binary\",\"category\":\"Flags\"}],\"location\":\"Motherboard\",\"address_width\":24},{\"id\":1,\"name\":\"Zilog Z80\",\"architecture\":\"Z80\",\"clock_speed\":3579545,\"registers\":[{\"name\":\"A\",\"size_bits\":8,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"F\",\"size_bits\":8,\"display_format\":\"Binary\",\"category\":\"Flags\"},{\"name\":\"B\",\"size_bits\":8,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"C\",\"size_bits\":8,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"D\",\"size_bits\":8,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"E\",\"size_bits\":8,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"H\",\"size_bits\":8,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"L\",\"size_bits\":8,\"display_format\":\"Hexadecimal\",\"category\":\"General\"},{\"name\":\"PC\",\"size_bits\":16,\"display_format\":\"Address\",\"category\":\"Special\"},{\"name\":\"SP\",\"size_bits\":16,\"display_format\":\"Address\",\"category\":\"Special\"},{\"name\":\"IX\",\"size_bits\":16,\"display_format\":\"Address\",\"category\":\"Address\"},{\"name\":\"IY\",\"size_bits\":16,\"display_format\":\"Address\",\"category\":\"Address\"}],\"location\":\"Motherboard\",\"address_width\":16}],\"display\":{\"width\":320,\"height\":224,\"aspect_ratio\":1.3333334,\"refresh_rate\":59.922745,\"pixel_format\":\"RGBA8888\"},\"audio\":{\"sample_rate\":44100,\"channels\":2,\"bits_per_sample\":16},\"input\":{\"max_players\":2,\"button_layout\":[{\"name\":\"Up\",\"id\":0},{\"name\":\"Down\",\"id\":1},{\"name\":\"Left\",\"id\":2},{\"name\":\"Right\",\"id\":3},{\"name\":\"A\",\"id\":6},{\"name\":\"B\",\"id\":4},{\"name\":\"C\",\"id\":5},{\"name\":\"Start\",\"id\":7},{\"name\":\"X\",\"id\":10},{\"name\":\"Y\",\"id\":9},{\"name\":\"Z\",\"id\":8},{\"name\":\"Mode\",\"id\":11}]}}"),
    // Another region is a power cycle in it, which keeps the save (the
    // hash, right after the power cycle, as the resets' above).
    ("set_region_str(pal)", "region pal | refresh 49.7015 | hash aa6d2532ea142717"),
    // The machine power-cycled in the region, as the line above.
    ("a PAL machine, 60 frames", "frames 60 | hash de277ebf626300b0 | refresh 49.7015"),
    ("set_region_str(secam)", "Err(\"unknown region 'secam' (expected 'ntsc' or 'pal')\")"),
];

#[test]
fn the_machine_gives_the_recorded_values() {
    let got = record();
    if std::env::var_os("MACHINE_BASELINE_PRINT").is_some() {
        println!("const RECORDED: &[(&str, &str)] = &[");
        for (key, value) in &got {
            println!("    ({key:?}, {value:?}),");
        }
        println!("];");
    }
    let mut differ = Vec::new();
    for (key, value) in &got {
        match RECORDED.iter().find(|(k, _)| k == key) {
            Some((_, recorded)) if recorded == value => {}
            Some((_, recorded)) => {
                differ.push(format!("{key}:\n  now      {value}\n  recorded {recorded}"))
            }
            None => differ.push(format!("{key}: not recorded (now {value})")),
        }
    }
    for (key, _) in RECORDED {
        if !got.iter().any(|(k, _)| k == key) {
            differ.push(format!("{key}: recorded, no longer produced"));
        }
    }
    assert!(differ.is_empty(), "{}", differ.join("\n"));
}

/// Save and load on the program at frame 300, as netplay's rollback does
/// on every frame: the times, not checked. Run with `--ignored --nocapture`,
/// in release.
#[test]
#[ignore = "timing: run with --ignored --nocapture"]
fn save_and_load_times() {
    let rom = program();
    let mut emu = booted(&rom);
    run(&mut emu, 0, 300);
    let percentiles = |mut ms: Vec<f64>| {
        ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let at = |q: f64| ms[((ms.len() as f64 - 1.0) * q).round() as usize];
        (at(0.5), at(0.95))
    };
    let mut save = Vec::new();
    let mut load = Vec::new();
    let mut bytes = Vec::new();
    for _ in 0..300 {
        let start = Instant::now();
        bytes = emu.save_state();
        save.push(start.elapsed().as_secs_f64() * 1000.0);
        let start = Instant::now();
        emu.load_state(&bytes, RomCheck::SameRom).expect("load");
        load.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let (s50, s95) = percentiles(save);
    let (l50, l95) = percentiles(load);
    println!(
        "sg1 save p50 {s50:.3} ms p95 {s95:.3} ms | load p50 {l50:.3} ms p95 {l95:.3} ms | {} bytes",
        bytes.len()
    );
}
