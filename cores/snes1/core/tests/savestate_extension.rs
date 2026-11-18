//! What a savestate must carry beyond its layout's fields: an NMI on its
//! way (armed where vertical blanking starts, delivered a few instructions
//! later) and an IRQ a `$4211` read acknowledged inside the instruction it
//! rose in. Each test saves where the machine holds one of them, loads the
//! state into a new machine, and holds the two to the machine that never
//! stopped. The last test holds the host's sound after a load to the sound
//! of the machine that never loaded.

use retro_core::savestate::RomCheck;
use retro_core::{AccessFlags, Breakpoint, BreakpointType, EmulatorCore, ExecutionMode};
use snes1_core::{snes_events, SnesEmulator};

fn cartridge(title: &[u8; 21], reset: &[u8]) -> Vec<u8> {
    let mut chip = vec![0u8; 0x10_0000];
    chip[..reset.len()].copy_from_slice(reset);
    // NMI: INC $10 / LDA $4210 / RTI. IRQ: RTI.
    chip[0x40..0x46].copy_from_slice(&[0xE6, 0x10, 0xAD, 0x10, 0x42, 0x40]);
    chip[0x60] = 0x40;
    let h = 0x7FB0;
    chip[h + 0x10..h + 0x10 + 21].copy_from_slice(title);
    chip[h + 0x25] = 0x20; // LoROM
    chip[h + 0x27] = 0x0A; // 1 MiB
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

fn booted(rom: &[u8]) -> SnesEmulator {
    let mut emu = SnesEmulator::new();
    emu.load_rom(rom).expect("the program loads");
    emu.set_execution_mode(ExecutionMode::Running);
    emu
}

fn loaded_from(rom: &[u8], state: &[u8]) -> SnesEmulator {
    let mut emu = booted(rom);
    emu.load_state(state, RomCheck::SameRom)
        .expect("the state loads");
    emu
}

fn byte(emu: &mut SnesEmulator, address: u32) -> u8 {
    emu.read_memory_debug(address, 1)[0]
}

/// The NMI on and NOPs forever, never reading `$4210`: the NMI is delivered
/// three instructions after vertical blanking starts. The handler counts
/// NMIs in $10.
#[rustfmt::skip]
const NMI_RESET: [u8; 17] = [
    0x78,             // SEI
    0xA9, 0x80,       // LDA #$80
    0x8D, 0x00, 0x21, // STA $2100: forced blank
    0x64, 0x10,       // STZ $10
    0xA9, 0x80,       // LDA #$80
    0x8D, 0x00, 0x42, // STA $4200: NMI on
    0xEA,             // loop: NOP
    0xEA,             // NOP
    0x80, 0xFC,       // BRA loop
];

fn run_both(live: &mut SnesEmulator, loaded: &mut SnesEmulator, frames: u32) {
    live.set_execution_mode(ExecutionMode::Running);
    loaded.set_execution_mode(ExecutionMode::Running);
    for frame in 1..=frames {
        live.run_frame();
        loaded.run_frame();
        assert_eq!(
            (byte(loaded, 0x7E_0010), loaded.state_hash()),
            (byte(live, 0x7E_0010), live.state_hash()),
            "NMIs counted and the state, {frame} frames after the load"
        );
    }
}

#[test]
fn an_nmi_on_its_way_at_a_stop_on_its_event_is_delivered_after_a_load() {
    let rom = cartridge(b"NMI ON ITS WAY       ", &NMI_RESET);
    let mut live = booted(&rom);
    for _ in 0..3 {
        live.run_frame();
    }
    let id = live.debugger().add_breakpoint(Breakpoint {
        address: 0,
        bp_type: BreakpointType::Event {
            code: snes_events::NMI,
            param: 0,
        },
        cpu_id: None,
        condition: None,
    });
    live.run_frame();
    assert_eq!(
        live.debugger_ref().get_execution_mode(),
        ExecutionMode::Paused,
        "stopped on the NMI event"
    );
    let before = live.state_hash();
    let state = live.save_state();
    assert_eq!(live.state_hash(), before, "the save moved the machine");
    live.debugger().remove_breakpoint(id);
    let mut loaded = loaded_from(&rom, &state);
    assert_eq!(loaded.state_hash(), live.state_hash());
    run_both(&mut live, &mut loaded, 4);
}

#[test]
fn an_nmi_on_its_way_after_a_host_step_is_delivered_after_a_load() {
    let rom = cartridge(b"NMI ON ITS WAY       ", &NMI_RESET);
    let mut live = booted(&rom);
    for _ in 0..3 {
        live.run_frame();
    }
    live.set_execution_mode(ExecutionMode::Paused);
    let before = byte(&mut live, 0x7E_0010);
    let frames = live.frame_count();
    // Step until the NMI flag of `$4210` is latched (where the delivery is
    // armed) and the NMI not yet taken.
    loop {
        live.step_cpu(0);
        let latched = live.bus().system_regs().to_savestate().rdnmi & 0x80 != 0;
        if latched && byte(&mut live, 0x7E_0010) == before {
            break;
        }
        assert_eq!(live.frame_count(), frames, "the window was passed");
    }
    let state = live.save_state();
    let mut loaded = loaded_from(&rom, &state);
    run_both(&mut live, &mut loaded, 3);
}

/// The vertical IRQ at line 100, the I flag set so it is never taken; the
/// program reads `$4211` in a loop and counts in $20 the reads that find
/// the flag set.
#[rustfmt::skip]
const IRQ_RESET: [u8; 28] = [
    0x78,                         // SEI
    0xA9, 0x80, 0x8D, 0x00, 0x21, // LDA #$80 / STA $2100: forced blank
    0xA9, 0x64, 0x8D, 0x09, 0x42, // LDA #100 / STA $4209: VTIME, low
    0x9C, 0x0A, 0x42,             // STZ $420A: VTIME, high
    0xA9, 0x20, 0x8D, 0x00, 0x42, // LDA #$20 / STA $4200: the vertical IRQ on
    0xAD, 0x11, 0x42,             // loop: LDA $4211
    0x10, 0xFB,                   // BPL loop
    0xE6, 0x20,                   // INC $20
    0x80, 0xF7,                   // BRA loop
];

/// Stopped by a watchpoint on every read of `$4211`, from the start of
/// frame 4: a frame call returns after each read.
fn watching_reads(rom: &[u8]) -> SnesEmulator {
    let mut emu = booted(rom);
    for _ in 0..3 {
        emu.run_frame();
    }
    emu.debugger().add_breakpoint(Breakpoint {
        address: 0x00_4211,
        bp_type: BreakpointType::Access(AccessFlags::READ),
        cpu_id: Some(0),
        condition: None,
    });
    emu
}

fn next_read(emu: &mut SnesEmulator) {
    emu.set_execution_mode(ExecutionMode::Running);
    emu.run_frame();
}

#[test]
fn an_irq_acknowledged_early_is_not_seen_again_after_a_load() {
    let rom = cartridge(b"IRQ READ EARLY       ", &IRQ_RESET);
    // The reads that found the flag set: the count moves before the read
    // after one of them.
    let mut scout = watching_reads(&rom);
    let mut found = Vec::new();
    let mut last = byte(&mut scout, 0x7E_0020);
    let mut reads = 0u64;
    while found.len() < 4 {
        next_read(&mut scout);
        reads += 1;
        let now = byte(&mut scout, 0x7E_0020);
        if now != last {
            found.push(reads - 1);
            last = now;
        }
    }
    // Stopped right after each of those reads, saved, loaded: the IRQ is
    // counted once, as on the machine that went on.
    let mut parted = Vec::new();
    for &at in &found {
        let mut live = watching_reads(&rom);
        for _ in 0..at {
            next_read(&mut live);
        }
        let state = live.save_state();
        let mut loaded = loaded_from(&rom, &state);
        let id = live.debugger_ref().list_breakpoints()[0].id;
        live.debugger().remove_breakpoint(id);
        live.set_execution_mode(ExecutionMode::Running);
        loaded.set_execution_mode(ExecutionMode::Running);
        for _ in 0..2 {
            live.run_frame();
            loaded.run_frame();
        }
        let (a, b) = (byte(&mut live, 0x7E_0020), byte(&mut loaded, 0x7E_0020));
        if a != b || live.state_hash() != loaded.state_hash() {
            parted.push(format!(
                "saved after read {at}: counted {a} on the machine that went on, {b} after the load"
            ));
        }
    }
    assert!(parted.is_empty(), "{}", parted.join("\n"));
}

/// A program that does nothing: the sound comes from a voice the test
/// keys on in the APU itself.
#[rustfmt::skip]
const IDLE_RESET: [u8; 2] = [
    0x80, 0xFE, // BRA *
];

fn dsp_write(emu: &mut SnesEmulator, register: u8, value: u8) {
    let apu = emu.bus_mut().apu_mut();
    apu.spc_write(0x00F2, register);
    apu.spc_write(0x00F3, value);
}

/// Voice 0 plays a square wave from ARAM at full volume.
fn key_on_a_square_wave(emu: &mut SnesEmulator) {
    {
        let apu = emu.bus_mut().apu_mut();
        // Sample directory entry 0: start and loop at $0300.
        for (address, value) in [
            (0x0200, 0x00),
            (0x0201, 0x03),
            (0x0202, 0x00),
            (0x0203, 0x03),
        ] {
            apu.spc_write(address, value);
        }
        // One BRR block, range 8, looping: +1792 for 8 samples, -1792 for 8.
        apu.spc_write(0x0300, 0x83);
        for address in 0x0301..=0x0304 {
            apu.spc_write(address, 0x77);
        }
        for address in 0x0305..=0x0308 {
            apu.spc_write(address, 0x99);
        }
    }
    for (register, value) in [
        (0x00, 0x7F), // VOLL
        (0x01, 0x7F), // VOLR
        (0x02, 0x00), // PITCHL
        (0x03, 0x0E), // PITCHH: a pitch the host's rate is not a multiple of
        (0x04, 0x00), // SRCN
        (0x05, 0x00), // ADSR1: the GAIN register sets the envelope
        (0x07, 0x7F), // GAIN: direct, near the top
        (0x5D, 0x02), // DIR: the directory at $0200
        (0x0C, 0x7F), // MVOLL
        (0x1C, 0x7F), // MVOLR
        (0x6C, 0x00), // FLG: out of soft reset, unmuted
        (0x4C, 0x01), // KON: voice 0
    ] {
        dsp_write(emu, register, value);
    }
}

/// A machine that loads the state it just saved goes on with the sound the
/// host was playing: its first samples after the load are those of the
/// twin that never loaded, not a restart from silence.
#[test]
fn a_load_goes_on_with_the_sound_the_host_was_playing() {
    let rom = cartridge(b"SOUND AFTER A LOAD   ", &IDLE_RESET);
    let mut twin = booted(&rom);
    let mut loading = booted(&rom);
    for emu in [&mut twin, &mut loading] {
        emu.set_audio_enabled(true);
        emu.run_frame();
        key_on_a_square_wave(emu);
        for _ in 0..30 {
            emu.run_frame();
            emu.get_audio_samples();
        }
    }
    let state = loading.save_state();
    loading
        .load_state(&state, RomCheck::SameRom)
        .expect("the state loads");
    for frame in 1..=10 {
        twin.run_frame();
        loading.run_frame();
        let heard = twin.get_audio_samples().to_vec();
        assert!(heard.iter().any(|&s| s != 0.0), "the voice sounds");
        assert_eq!(
            loading.get_audio_samples(),
            heard.as_slice(),
            "the host's samples {frame} frames after the load"
        );
    }
}
