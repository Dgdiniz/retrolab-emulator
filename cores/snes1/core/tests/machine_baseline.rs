//! What the machine does on a fixed program, recorded as literal values so
//! that a change to how it is reset, saved, loaded, switched between regions
//! or told to the host shows here unless the change meant it: every form a
//! savestate can take and what a load answers, the state hash and its parts,
//! the savestate's bytes, what each reset leaves, the sound, the system
//! information, the region.
//!
//! A change that means to move a value says which, beside it. To print the
//! table the code gives now, in the form of `RECORDED`:
//!
//! ```bash
//! MACHINE_BASELINE_PRINT=1 cargo test -p snes1-core --test machine_baseline -- --nocapture
//! ```

use std::time::Instant;

use retro_core::savestate::{
    deserialize_compressed, serialize_compressed, RomCheck, SaveStateEnvelope,
};
use retro_core::{Breakpoint, BreakpointType, EmulatorCore, ExecutionMode, InputState};
use snes1_core::{SnesEmulator, SnesSaveState};

/// The program (65C816, emulation mode): forced blank, a boot counted in
/// work RAM ($12), BG mode 1 with BG1 on the main screen, the display on at
/// full brightness, the NMI and the automatic pad read on; then it adds the
/// pad's two bytes to $00 and $01 forever. The NMI handler counts NMIs in
/// $10 and reads $4210. The header declares battery RAM.
#[rustfmt::skip]
const RESET: [u8; 44] = [
    0x78,                         // SEI
    0xA9, 0x80, 0x8D, 0x00, 0x21, // LDA #$80 / STA $2100: forced blank
    0xE6, 0x12,                   // INC $12: boots
    0xA9, 0x01, 0x8D, 0x05, 0x21, // LDA #$01 / STA $2105: BG mode 1
    0x8D, 0x2C, 0x21,             // STA $212C: BG1 on the main screen
    0xA9, 0x0F, 0x8D, 0x00, 0x21, // LDA #$0F / STA $2100: display on
    0xA9, 0x81, 0x8D, 0x00, 0x42, // LDA #$81 / STA $4200: NMI, pad read
    // loop:
    0xAD, 0x18, 0x42,             // LDA $4218
    0x18, 0x65, 0x00, 0x85, 0x00, // CLC / ADC $00 / STA $00
    0xAD, 0x19, 0x42,             // LDA $4219
    0x18, 0x65, 0x01, 0x85, 0x01, // CLC / ADC $01 / STA $01
    0x80, 0xEE,                   // BRA loop
];

fn program() -> Vec<u8> {
    let mut chip = vec![0u8; 0x10_0000];
    chip[..RESET.len()].copy_from_slice(&RESET);
    // NMI: INC $10 / LDA $4210 / RTI. IRQ: RTI.
    chip[0x40..0x46].copy_from_slice(&[0xE6, 0x10, 0xAD, 0x10, 0x42, 0x40]);
    chip[0x60] = 0x40;
    let h = 0x7FB0;
    chip[h + 0x10..h + 0x10 + 21].copy_from_slice(b"MACHINE BASELINE     ");
    chip[h + 0x25] = 0x20; // LoROM
    chip[h + 0x26] = 0x02; // ROM, RAM and battery
    chip[h + 0x27] = 0x0A; // 1 MiB
    chip[h + 0x28] = 0x03; // 8 KiB of RAM
    chip[h + 0x29] = 0x01; // North America: NTSC
    chip[h + 0x2A] = 0x01;
    let checksum: u16 = 0x1234;
    chip[h + 0x2C..h + 0x2E].copy_from_slice(&(!checksum).to_le_bytes());
    chip[h + 0x2E..h + 0x30].copy_from_slice(&checksum.to_le_bytes());
    chip[h + 0x4C..h + 0x4E].copy_from_slice(&0x8000u16.to_le_bytes());
    chip[0x7FFA..0x7FFC].copy_from_slice(&0x8040u16.to_le_bytes());
    chip[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    chip[0x7FFE..0x8000].copy_from_slice(&0x8060u16.to_le_bytes());
    chip
}

/// A square wave on voice 0, written straight into the sound processor's
/// memory and DSP registers: a directory at $0200 naming one looping BRR
/// block at $0300.
fn start_tone(emu: &mut SnesEmulator) {
    let apu = emu.bus_mut().apu_mut();
    for (i, byte) in [0x00, 0x03, 0x00, 0x03].iter().enumerate() {
        apu.spc_write(0x0200 + i as u16, *byte);
    }
    let block = [0xB3, 0x77, 0x77, 0x77, 0x77, 0x99, 0x99, 0x99, 0x99];
    for (i, byte) in block.iter().enumerate() {
        apu.spc_write(0x0300 + i as u16, *byte);
    }
    for (register, value) in [
        (0x6C, 0x20), // FLG: out of reset, not muted, no echo writes
        (0x5D, 0x02), // DIR: $0200
        (0x00, 0x7F), // voice 0 volume, left
        (0x01, 0x7F), // and right
        (0x02, 0x00), // pitch, low
        (0x03, 0x08), // pitch, high: 16 kHz
        (0x04, 0x00), // source 0
        (0x05, 0x00), // envelope off: the gain applies
        (0x07, 0x7F), // gain
        (0x0C, 0x7F), // main volume, left
        (0x1C, 0x7F), // and right
        (0x2C, 0x00), // echo volume, left
        (0x3C, 0x00), // and right
        (0x4C, 0x01), // key on voice 0
    ] {
        apu.spc_write(0x00F2, register);
        apu.spc_write(0x00F3, value);
    }
}

/// A and B, then Start, then nothing.
fn schedule(frame: u64) -> u32 {
    match frame {
        120..=239 => (1 << 4) | (1 << 5),
        240..=359 => 1 << 10,
        _ => 0,
    }
}

/// A machine on the program, a save the host put back, the tone started,
/// running.
fn booted(rom: &[u8]) -> SnesEmulator {
    let mut emu = SnesEmulator::new();
    emu.load_rom(rom).expect("the program loads");
    let save: Vec<u8> = (0..8192u32)
        .map(|i| (i as u8).wrapping_mul(7) ^ 0x5A)
        .collect();
    emu.set_save_ram(&save);
    start_tone(&mut emu);
    emu.set_execution_mode(ExecutionMode::Running);
    emu
}

/// Frames `from..to` of the schedule, the sound drained as a host does.
fn run(emu: &mut SnesEmulator, from: u64, to: u64) {
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

fn describe(emu: &mut SnesEmulator) -> String {
    let work_ram = emu.read_memory_debug(0x7E_0000, 0x2_0000);
    format!(
        "hash {:016x} | work RAM {:016x} boots {} NMIs {} | save RAM {} | mode {:?} | breakpoints {} | region {} | frames {}",
        emu.state_hash(),
        fnv(work_ram.iter().copied()),
        work_ram[0x12],
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
    let mut fresh = SnesEmulator::new();
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
    put(
        "the savestate's bytes at frame 60",
        format!("{} bytes {:016x}", state.len(), fnv(state.iter().copied())),
    );
    let envelope =
        deserialize_compressed::<SnesSaveState>(&state).expect("the save opens as an envelope");
    put(
        "the envelope",
        format!(
            "envelope {} | core {} | version {} | rom hash {} | created at {} | payload version {}",
            envelope.envelope_version,
            envelope.core_id,
            envelope.core_version,
            envelope.rom_hash,
            envelope.created_at,
            envelope.payload.version
        ),
    );
    let reseal = |change: &dyn Fn(&mut SaveStateEnvelope<SnesSaveState>)| {
        let mut e = envelope.clone();
        change(&mut e);
        serialize_compressed(&e, 3).expect("seal")
    };
    let cases: Vec<(&str, Vec<u8>, RomCheck, bool)> = vec![
        ("envelope", state.clone(), RomCheck::SameRom, true),
        (
            "core version + 1",
            reseal(&|e| e.core_version += 1),
            RomCheck::SameRom,
            true,
        ),
        (
            "payload version + 1",
            reseal(&|e| e.payload.version += 1),
            RomCheck::SameRom,
            true,
        ),
        (
            "another core's id",
            reseal(&|e| e.core_id = "sg1".to_string()),
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
        (
            "another ROM and a newer version",
            reseal(&|e| {
                e.rom_hash = "0".repeat(64);
                e.core_version += 1;
            }),
            RomCheck::SameRom,
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
            SnesEmulator::new()
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
            address: 0x00_FFF0,
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
    let mut loudest = 0.0f32;
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
        loudest = samples.iter().fold(loudest, |m, s| m.max(s.abs()));
        if [0u64, 59, 299].contains(&frame) {
            put(
                &format!("sound, frame {}", frame + 1),
                format!("{} samples {hash:016x}", samples.len()),
            );
        }
    }
    put(
        "sound, 300 frames",
        format!("{total} samples {all:016x} loudest {loudest:.4}"),
    );

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
    run(&mut pal, 0, 60);
    let pal_state = pal.save_state();
    let mut ntsc = booted(&rom);
    ntsc.load_state(&pal_state, RomCheck::SameRom)
        .expect("a PAL state loads");
    put(
        "a PAL state loaded into an NTSC machine",
        format!(
            "region {} | refresh {:.4} | the saved machine's state: {}",
            ntsc.region_str(),
            ntsc.get_display_info().refresh_rate,
            ntsc.state_hash() == pal.state_hash()
        ),
    );
    run(&mut pal, 60, 120);
    run(&mut ntsc, 60, 120);
    put(
        "the two, 60 frames later",
        format!(
            "frames {} and {} | the same state: {}",
            pal.frame_count(),
            ntsc.frame_count(),
            pal.state_hash() == ntsc.state_hash()
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
    ("new machine", "mode Running | region ntsc | audio false"),
    ("after load_rom", "mode Running | region ntsc | audio false | frames 0"),
    ("frame 1: state hash", "2d29b65b177388f8"),
    ("frame 1: parts", "cpu=d637074d4fde171d wram=16ffea38ea683963 ppu=600c899dfbeca761 apu=09989ecd24188b56 dma=a4ca53d582377be5 system_regs=12401bc3e69c058c controllers=a90f0cbfa2aac455 sram=85ee0b68fcf89525 coprocessors=a8c7f832281a39c5 bus=b63bc2cb7c80c3d7 scheduler=1b840e9f9854a9c0 timing=0e55f178e5221789 frame_count=0000000000000001"),
    ("frame 60: state hash", "887c99404a0cea50"),
    ("frame 60: parts", "cpu=89d12f98d82869ca wram=33e19ad27fa1e5ca ppu=6cbb6ac8793c0e5e apu=560096c336498921 dma=a4ca53d582377be5 system_regs=12401bc3e69c058c controllers=a90f0cbfa2aac455 sram=85ee0b68fcf89525 coprocessors=a8c7f832281a39c5 bus=0286d0aab17e6152 scheduler=f9b335226afb6c9e timing=0e55f178e5221789 frame_count=000000000000003c"),
    ("frame 300: state hash", "e7bc7fe1503ff060"),
    ("frame 300: parts", "cpu=300d3bd88b8e756e wram=2d2417e708fbb1ec ppu=6cbb6ac8793c0e5e apu=b8cbed98b579e399 dma=a4ca53d582377be5 system_regs=3093be96e203c93c controllers=2674934dc2ded325 sram=85ee0b68fcf89525 coprocessors=a8c7f832281a39c5 bus=0cb8039125ad2dab scheduler=48eb9dc9713092af timing=0e55f178e5221789 frame_count=000000000000012c"),
    ("the savestate's bytes at frame 60", "4201 bytes 416ffbf84a468f24"),
    ("the envelope", "envelope 1 | core snes1 | version 4 | rom hash 400f45ca07e2eda8b7dd0511268c34a11a9d8908de688ac4a59d8280c452c5ce | created at 0 | payload version 4"),
    ("load: envelope", "Ok | the saved machine's state: true"),
    // The refusal names the core by its id.
    ("load: core version + 1", "Unreadable(\"snes1 save state core_version 5 is newer than supported version 4\") | snes1 save state core_version 5 is newer than supported version 4"),
    ("load: payload version + 1", "Unreadable(\"SNES save state version 5 is newer than supported version 4\") | SNES save state version 5 is newer than supported version 4"),
    // Another core's state is refused.
    ("load: another core's id", "Unreadable(\"savestate is for core 'sg1', not 'snes1'\") | savestate is for core 'sg1', not 'snes1'"),
    // A newer envelope is refused.
    ("load: envelope version 2", "Unreadable(\"savestate envelope version 2 is newer than supported version 1\") | savestate envelope version 2 is newer than supported version 1"),
    ("load: another ROM", "OtherRom { state_rom: \"0000000000000000000000000000000000000000000000000000000000000000\", loaded_rom: \"400f45ca07e2eda8b7dd0511268c34a11a9d8908de688ac4a59d8280c452c5ce\" } | ROM hash mismatch: savestate is for '0000000000000000000000000000000000000000000000000000000000000000' but loaded ROM is '400f45ca07e2eda8b7dd0511268c34a11a9d8908de688ac4a59d8280c452c5ce'"),
    ("load: another ROM, any ROM", "Ok | the saved machine's state: true"),
    // A state of a newer build is refused as such, before its ROM is looked at.
    ("load: another ROM and a newer version", "Unreadable(\"snes1 save state core_version 5 is newer than supported version 4\") | snes1 save state core_version 5 is newer than supported version 4"),
    // With no ROM loaded there is no ROM to refuse a state for.
    ("load: no ROM loaded", "Ok | the saved machine's state: false"),
    ("load: garbage", "Unreadable(\"not a retro-core savestate (missing magic)\") | not a retro-core savestate (missing magic)"),
    ("load: empty", "Unreadable(\"not a retro-core savestate (missing magic)\") | not a retro-core savestate (missing magic)"),
    // The reset button resets the registers of the 5A22, its DMA channels
    // and the PPU, as the console's reset line does. The state moves, and
    // two frames on work RAM too: the loop adds the pad registers, which
    // read 0 after the reset until the next automatic read, where they kept
    // the Start the pad held at frame 300.
    ("reset: right after", "hash f989ff0e5b73ca4e | work RAM 406ec6cb96e116a6 boots 1 NMIs 44 | save RAM 146a7cfda5862325 | mode Running | breakpoints 1 | region ntsc | frames 0"),
    // As the line above.
    ("reset: two frames later", "hash c7ac4597f8dcf27f | work RAM 53ecd1d7fe8b189f boots 2 NMIs 46 | save RAM 146a7cfda5862325 | mode Running | breakpoints 1 | region ntsc | frames 2"),
    // The app's Reset, a power cycle that keeps the save; its breakpoints
    // and its mode are kept.
    ("hard_reset: right after", "hash cda23afb8772ab20 | work RAM c74b47c8c74a2325 boots 0 NMIs 0 | save RAM 146a7cfda5862325 | mode Running | breakpoints 1 | region ntsc | frames 0"),
    // As the line above.
    ("hard_reset: two frames later", "hash fced5df7f50a1d15 | work RAM 633ef910c0af79fe boots 1 NMIs 2 | save RAM 146a7cfda5862325 | mode Running | breakpoints 1 | region ntsc | frames 2"),
    // Netplay's power cycle removes every breakpoint, watchpoint and event.
    ("power_on_reset: right after", "hash 79ae05a1c8502b20 | work RAM c74b47c8c74a2325 boots 0 NMIs 0 | save RAM b9d103fd6854a325 | mode Running | breakpoints 0 | region ntsc | frames 0"),
    // As the line above.
    ("power_on_reset: two frames later", "hash c99dae85623b1d15 | work RAM 633ef910c0af79fe boots 1 NMIs 2 | save RAM b9d103fd6854a325 | mode Running | breakpoints 0 | region ntsc | frames 2"),
    ("sound, frame 1", "1468 samples befcaa57c36220d1"),
    ("sound, frame 60", "1466 samples b9a5302e4f8cd725"),
    ("sound, frame 300", "1466 samples d491114ab1f83ffd"),
    ("sound, 300 frames", "440276 samples 0c76c78426a3645d loudest 0.4276"),
    ("system info", "{\"name\":\"Super Nintendo Entertainment System\",\"short_name\":\"SNES\",\"manufacturer\":\"Nintendo\",\"year\":1990,\"cpus\":[{\"id\":0,\"name\":\"Ricoh 5A22 (WDC 65C816)\",\"architecture\":\"Mos65816\",\"clock_speed\":3580000,\"registers\":[],\"location\":\"Motherboard\",\"address_width\":24},{\"id\":1,\"name\":\"Sony SPC700\",\"architecture\":\"Spc700\",\"clock_speed\":1024000,\"registers\":[],\"location\":\"Motherboard\",\"address_width\":16}],\"display\":{\"width\":256,\"height\":224,\"aspect_ratio\":1.1428572,\"refresh_rate\":60.09847,\"pixel_format\":\"RGBA8888\",\"grid\":{\"columns_per_dot\":1,\"rows_per_line\":1,\"woven\":false}},\"audio\":{\"sample_rate\":44100,\"channels\":2,\"bits_per_sample\":32},\"input\":{\"max_players\":2,\"button_layout\":[{\"name\":\"Up\",\"id\":0},{\"name\":\"Down\",\"id\":1},{\"name\":\"Left\",\"id\":2},{\"name\":\"Right\",\"id\":3},{\"name\":\"A\",\"id\":4},{\"name\":\"B\",\"id\":5},{\"name\":\"X\",\"id\":6},{\"name\":\"Y\",\"id\":7},{\"name\":\"L\",\"id\":8},{\"name\":\"R\",\"id\":9},{\"name\":\"Start\",\"id\":10},{\"name\":\"Select\",\"id\":11}]}}"),
    // Another region is a power cycle in it, which keeps the save (it
    // pressed the reset button).
    ("set_region_str(pal)", "region pal | refresh 50.0070 | hash 15e10514edb9e898"),
    ("a PAL state loaded into an NTSC machine", "region pal | refresh 50.0070 | the saved machine's state: true"),
    ("the two, 60 frames later", "frames 120 and 120 | the same state: true"),
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
        "snes1 save p50 {s50:.3} ms p95 {s95:.3} ms | load p50 {l50:.3} ms p95 {l95:.3} ms | {} bytes",
        bytes.len()
    );
}
