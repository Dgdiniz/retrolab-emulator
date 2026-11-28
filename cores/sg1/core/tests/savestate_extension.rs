//! What a savestate must carry beyond its layout's fields: a VRAM fill still
//! running, the YM2612's busy window, and a frame stopped part of the way
//! through, with what the machine holds within a frame: the samples the
//! sound chips made so far, where the beam stands, the sprite list read for
//! the frame, a Z80 bus request waiting for its grant, the vertical
//! interrupt held off, the first frame after a reset. Each test saves where
//! the machine holds one of them, loads the state into another machine, and
//! holds the two to the machine that never stopped.

use retro_core::savestate::RomCheck;
use retro_core::{Breakpoint, BreakpointType, DebuggerCore, EmulatorCore, ExecutionMode};
use sg1_core::debugger::genesis_events;
use sg1_core::GenesisEmulator;

fn put_words(rom: &mut [u8], at: usize, words: &[u16]) {
    for (i, w) in words.iter().enumerate() {
        rom[at + 2 * i] = (w >> 8) as u8;
        rom[at + 2 * i + 1] = (*w & 0xFF) as u8;
    }
}

fn cartridge(title: &[u8; 24], code: &[u16]) -> Vec<u8> {
    let mut rom = vec![0u8; 0x20000];
    put_words(&mut rom, 0, &[0x00FF, 0xFE00, 0x0000, 0x0200]);
    rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
    rom[0x120..0x138].copy_from_slice(title);
    rom[0x1A0..0x1A8].copy_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0xFF, 0xFF]);
    put_words(&mut rom, 0x200, code);
    rom
}

/// [`cartridge`] with a vertical interrupt handler at $300.
fn cartridge_with_vint(title: &[u8; 24], code: &[u16], handler: &[u16]) -> Vec<u8> {
    let mut rom = cartridge(title, code);
    put_words(&mut rom, 0x78, &[0x0000, 0x0300]);
    put_words(&mut rom, 0x300, handler);
    rom
}

fn booted(rom: &[u8]) -> GenesisEmulator {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(rom).expect("the program loads");
    emu.set_execution_mode(ExecutionMode::Running);
    emu
}

/// A 64 KiB VRAM fill started with the display on: it streams for about
/// four frames.
fn fill_program() -> Vec<u8> {
    cartridge(
        b"FILL IN FLIGHT          ",
        &[
            0x46FC, 0x2700, // MOVE.W #$2700,SR
            0x41F9, 0x00C0, 0x0004, // LEA $C00004,A0
            0x30BC, 0x8154, // register 1: display on, DMA on, mode 5
            0x30BC, 0x8F01, // register 15: auto-increment 1
            0x30BC, 0x93FF, // register 19
            0x30BC, 0x94FF, // register 20: length $FFFF
            0x30BC, 0x9780, // register 23: fill
            0x20BC, 0x4000, 0x0080, // MOVE.L #$40000080,(A0): VRAM at 0, DMA
            0x33FC, 0x1234, 0x00C0, 0x0000, // MOVE.W #$1234,$C00000: the fill starts
            0x60FE, // BRA *
        ],
    )
}

fn filled(emu: &GenesisEmulator) -> usize {
    emu.vdp().get_vram().iter().filter(|&&b| b == 0x12).count()
}

#[test]
fn a_fill_in_flight_goes_on_after_a_load_into_a_new_machine() {
    let rom = fill_program();
    let mut live = booted(&rom);
    live.run_frame();
    live.run_frame();
    assert!(
        live.vdp().is_dma_active(),
        "the fill runs at the frame's end"
    );
    let state = live.save_state();
    let mut loaded = booted(&rom);
    loaded.load_state(&state, RomCheck::SameRom).expect("load");
    for frame in 1..=6 {
        live.run_frame();
        loaded.run_frame();
        assert_eq!(
            (filled(&loaded), loaded.state_hash()),
            (filled(&live), live.state_hash()),
            "{frame} frames after the load"
        );
    }
    assert_eq!(
        filled(&live),
        0x10000 - 1,
        "the fill ends where it was asked to"
    );
}

#[test]
fn a_fill_in_flight_goes_on_after_a_rollback() {
    let rom = fill_program();
    let mut live = booted(&rom);
    live.run_frame();
    live.run_frame();
    let state = live.save_state();
    // The machine that loads it ran one frame further, as a netplay
    // rollback's does.
    let mut rolled = booted(&rom);
    for _ in 0..3 {
        rolled.run_frame();
    }
    rolled.load_state(&state, RomCheck::SameRom).expect("load");
    for frame in 1..=6 {
        live.run_frame();
        rolled.run_frame();
        assert_eq!(
            (filled(&rolled), rolled.state_hash()),
            (filled(&live), live.state_hash()),
            "{frame} frames after the rollback"
        );
    }
}

/// The 68000 takes the Z80's bus, then writes a YM2612 register again and
/// again, polling the busy flag after each write and counting the polls
/// that found the chip busy ($FF0000, a long word).
fn busy_program() -> Vec<u8> {
    cartridge(
        b"YM2612 BUSY             ",
        &[
            0x46FC, 0x2700, // MOVE.W #$2700,SR
            0x33FC, 0x0100, 0x00A1, 0x1100, // MOVE.W #$0100,$A11100: the Z80's bus
            0x33FC, 0x0100, 0x00A1, 0x1200, // MOVE.W #$0100,$A11200: Z80 out of reset
            0x0839, 0x0000, 0x00A1, 0x1100, // wait: BTST #0,$A11100
            0x66F6, // BNE.S wait
            0x4BF9, 0x00A0, 0x4000, // LEA $A04000,A5
            0x1ABC, 0x0030, // write: MOVE.B #$30,(A5)
            0x1B40, 0x0001, // MOVE.B D0,1(A5)
            0x5200, // ADDQ.B #1,D0
            0x0815, 0x0007, // poll: BTST #7,(A5)
            0x6708, // BEQ.S next
            0x52B9, 0x00FF, 0x0000, // ADDQ.L #1,$FF0000
            0x60F2, // BRA.S poll
            0x60E6, // next: BRA.S write
        ],
    )
}

#[test]
fn a_busy_ym2612_is_still_busy_after_a_load() {
    let rom = busy_program();
    let mut live = booted(&rom);
    let mut parted = Vec::new();
    let mut busy_at_save = 0;
    let mut pending: Option<(u32, GenesisEmulator)> = None;
    for at in 1..=31u32 {
        live.run_frame();
        // The state saved a frame ago, loaded into a new machine, has run
        // the frame `live` just ran.
        if let Some((saved_at, mut loaded)) = pending.take() {
            loaded.run_frame();
            if loaded.state_hash() != live.state_hash() {
                parted.push(saved_at);
            }
        }
        if at <= 30 {
            if live.bus().ym2612().get_status() & 0x80 != 0 {
                busy_at_save += 1;
            }
            let state = live.save_state();
            let mut loaded = booted(&rom);
            loaded.load_state(&state, RomCheck::SameRom).expect("load");
            pending = Some((at, loaded));
        }
    }
    assert!(busy_at_save > 0, "no frame ended inside a busy window");
    assert!(
        parted.is_empty(),
        "saved at the end of these frames ({busy_at_save} of 30 ended busy), a load went on otherwise: {parted:?}"
    );
}

/// The program stopped at a breakpoint part of the way through a frame:
/// a loop of $400 steps, then the breakpoint at $210 and back.
fn stop_program() -> Vec<u8> {
    cartridge(
        b"STOPPED PART OF THE WAY ",
        &[
            0x46FC, 0x2700, // $200 MOVE.W #$2700,SR
            0x5280, // $204 ADDQ.L #1,D0
            0x0C80, 0x0000, 0x0400, // $206 CMPI.L #$400,D0
            0x66F6, // $20C BNE $204
            0x7000, // $20E MOVEQ #0,D0
            0x5281, // $210 ADDQ.L #1,D1: the breakpoint
            0x60F0, // $212 BRA $204
        ],
    )
}

#[test]
fn a_save_at_a_breakpoint_holds_the_stop() {
    let rom = stop_program();
    let mut live = booted(&rom);
    live.run_frame();
    live.run_frame();
    let id = live.debugger().add_breakpoint(Breakpoint {
        address: 0x210,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    });
    live.run_frame();
    let stopped = (
        live.cpu().pc,
        live.cpu().d[1],
        live.frame_count(),
        live.vdp().current_scanline(),
    );
    assert_eq!(
        live.debugger_ref().get_execution_mode(),
        ExecutionMode::Paused
    );
    assert_eq!(stopped.0, 0x210, "stopped at the breakpoint");
    // The host's save.
    let state = live.save_state();
    assert_eq!(
        (
            live.cpu().pc,
            live.cpu().d[1],
            live.frame_count(),
            live.vdp().current_scanline()
        ),
        stopped,
        "the save moved the machine the debugger shows"
    );
    let mut loaded = booted(&rom);
    loaded.load_state(&state, RomCheck::SameRom).expect("load");
    assert_eq!(
        (
            loaded.cpu().pc,
            loaded.cpu().d[1],
            loaded.frame_count(),
            loaded.vdp().current_scanline()
        ),
        stopped,
        "the loaded machine is not where the saved one stopped"
    );
    live.debugger().remove_breakpoint(id);
    live.set_execution_mode(ExecutionMode::Running);
    loaded.set_execution_mode(ExecutionMode::Running);
    for frame in 1..=3 {
        live.run_frame();
        loaded.run_frame();
        assert_eq!(
            loaded.state_hash(),
            live.state_hash(),
            "{frame} frames after the load"
        );
    }
}

/// A long word of work RAM.
fn long_at(emu: &mut GenesisEmulator, address: u32) -> u32 {
    let bytes = emu.read_memory_debug(address, 4);
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Run `machine` until it has stopped at its breakpoints `stops` times.
fn stop_times(machine: &mut GenesisEmulator, stops: u32) {
    let mut stopped = 0;
    while stopped < stops {
        machine.set_execution_mode(ExecutionMode::Running);
        machine.run_frame();
        if machine.execution_mode() == ExecutionMode::Paused {
            stopped += 1;
        }
    }
}

fn execute_at(address: u32) -> Breakpoint {
    Breakpoint {
        address,
        bp_type: BreakpointType::execute(),
        cpu_id: Some(0),
        condition: None,
    }
}

fn scanline(line: u32) -> Breakpoint {
    Breakpoint {
        address: 0,
        bp_type: BreakpointType::Event {
            code: genesis_events::SCANLINE,
            param: line,
        },
        cpu_id: None,
        condition: None,
    }
}

/// The machine `live`, stopped, saved; a new machine loads the state; both
/// run `frames` frames with their breakpoints removed: the state equal
/// after every frame.
fn saved_and_loaded_go_on_alike(rom: &[u8], live: &mut GenesisEmulator, frames: u32) {
    let state = live.save_state();
    let mut loaded = booted(rom);
    loaded
        .load_state(&state, RomCheck::SameRom)
        .expect("the state loads");
    assert_eq!(loaded.state_hash(), live.state_hash(), "the loaded state");
    for info in live.debugger_ref().list_breakpoints() {
        live.debugger().remove_breakpoint(info.id);
    }
    live.set_execution_mode(ExecutionMode::Running);
    for frame in 1..=frames {
        live.run_frame();
        loaded.run_frame();
        assert_eq!(
            loaded.state_hash(),
            live.state_hash(),
            "{frame} frames after the load"
        );
    }
}

/// The 68000 writes the YM2612's DAC and the PSG in a loop, so each chip
/// holds the samples its writes made so far in the frame.
fn sound_program() -> Vec<u8> {
    cartridge(
        b"SOUND IN THE FRAME      ",
        &[
            0x46FC, 0x2700, // $200 MOVE.W #$2700,SR
            0x33FC, 0x0100, 0x00A1, 0x1100, // $204 MOVE.W #$0100,$A11100: the Z80's bus
            0x33FC, 0x0100, 0x00A1, 0x1200, // $20C MOVE.W #$0100,$A11200: Z80 out of reset
            0x0839, 0x0000, 0x00A1, 0x1100, // $214 wait: BTST #0,$A11100
            0x66F6, // $21C BNE.S wait
            0x4BF9, 0x00A0, 0x4000, // $21E LEA $A04000,A5
            0x1ABC, 0x002B, // $224 MOVE.B #$2B,(A5): the DAC's register
            0x1B7C, 0x0080, 0x0001, // $228 MOVE.B #$80,1(A5): the DAC on
            0x1B7C, 0x00B6, 0x0002, // $22E MOVE.B #$B6,2(A5): channel 6's panning
            0x1B7C, 0x00C0, 0x0003, // $234 MOVE.B #$C0,3(A5): left and right
            0x1ABC, 0x002A, // $23A loop: MOVE.B #$2A,(A5): the DAC's data
            0x1B40, 0x0001, // $23E MOVE.B D0,1(A5)
            0x13C0, 0x00C0, 0x0011, // $242 MOVE.B D0,$C00011: the PSG
            0x5600, // $248 ADDQ.B #3,D0
            0x60EE, // $24A BRA.S loop
        ],
    )
}

/// A machine that loads the state it saved part of the way through a frame
/// goes on as its twin that never saved, the frame's sound included: the
/// chips' records of the frame so far are in the state, and the frame's
/// end, which takes them, finds them.
#[test]
fn a_save_mid_frame_keeps_the_samples_the_sound_chips_made() {
    let rom = sound_program();
    let mut twin = booted(&rom);
    let mut loading = booted(&rom);
    for emu in [&mut twin, &mut loading] {
        emu.set_audio_enabled(true);
        for _ in 0..2 {
            emu.run_frame();
            emu.get_audio_samples();
        }
        emu.debugger().add_breakpoint(execute_at(0x23A));
        stop_times(emu, 400);
    }
    let extension = loading.savestate_extension();
    assert!(
        extension.ym2612_output.len() > 1,
        "the YM2612's record of the frame"
    );
    assert!(
        extension.psg_output.len() > 1,
        "the PSG's record of the frame"
    );
    assert!(
        extension.ym2612.samples.is_empty() && extension.psg.samples.is_empty(),
        "none at the host's rate"
    );
    let state = loading.save_state();
    loading
        .load_state(&state, RomCheck::SameRom)
        .expect("the state loads");
    let mut fresh = booted(&rom);
    fresh
        .load_state(&state, RomCheck::SameRom)
        .expect("the state loads");
    fresh.set_audio_enabled(true);
    for emu in [&mut twin, &mut loading] {
        for info in emu.debugger_ref().list_breakpoints() {
            emu.debugger().remove_breakpoint(info.id);
        }
        emu.set_execution_mode(ExecutionMode::Running);
    }
    for frame in 1..=3 {
        twin.run_frame();
        loading.run_frame();
        fresh.run_frame();
        assert_eq!(loading.state_hash(), twin.state_hash(), "{frame} frames on");
        assert_eq!(fresh.state_hash(), twin.state_hash(), "{frame} frames on");
        let heard = twin.get_audio_samples().to_vec();
        assert!(heard.iter().any(|&s| s != 0.0), "the program sounds");
        assert_eq!(
            loading.get_audio_samples(),
            heard.as_slice(),
            "the samples {frame} frames on"
        );
    }
}

/// The 68000 reads the HV counter in a loop and adds what it reads into
/// work RAM.
fn beam_program() -> Vec<u8> {
    cartridge(
        b"WHERE THE BEAM STANDS   ",
        &[
            0x46FC, 0x2700, // $200 MOVE.W #$2700,SR
            0x3039, 0x00C0, 0x0008, // $204 loop: MOVE.W $C00008,D0: the HV counter
            0xD179, 0x00FF, 0x0000, // $20A ADD.W D0,$FF0000
            0x60F2, // $210 BRA.S loop
        ],
    )
}

/// Stopped before the instruction that reads the HV counter, part of the
/// way through a line: the read after the load finds the beam where it
/// stood.
#[test]
fn a_save_mid_line_keeps_where_the_beam_stands() {
    let rom = beam_program();
    let mut live = booted(&rom);
    live.run_frame();
    live.debugger().add_breakpoint(execute_at(0x204));
    stop_times(&mut live, 300);
    assert!(
        live.savestate_extension().vdp.beam.is_some(),
        "stopped part of the way through a line"
    );
    saved_and_loaded_go_on_alike(&rom, &mut live, 3);
}

/// The display on in 40-cell mode, the sprite table at $F000; the 68000
/// reads the VDP's status in a loop and adds it into work RAM, so the
/// sprite overflow flag a line raises is seen.
fn sprite_program() -> Vec<u8> {
    cartridge(
        b"THE FRAME'S SPRITE LIST ",
        &[
            0x46FC, 0x2700, // $200 MOVE.W #$2700,SR
            0x41F9, 0x00C0, 0x0004, // $204 LEA $C00004,A0
            0x30BC, 0x8144, // $20A register 1: display on, mode 5
            0x30BC, 0x8C81, // $20E register 12: 40 cells
            0x30BC, 0x8578, // $212 register 5: the sprite table at $F000
            0x3210, // $216 loop: MOVE.W (A0),D1: the status
            0xD379, 0x00FF, 0x0000, // $218 ADD.W D1,$FF0000
            0x60F6, // $21E BRA.S loop
        ],
    )
}

/// The sprite table's 21 sprites, linked in order, all at line `y`: one
/// more than a 40-cell line shows, which raises the overflow flag there.
/// Written as the debugger writes, which reaches the VDP's copy of the
/// table's first words, as a port write does.
fn put_sprites(emu: &mut GenesisEmulator, y: u16) {
    let mut table = Vec::new();
    for i in 0..21u16 {
        let link = if i == 20 { 0 } else { i + 1 };
        for word in [y + 128, link, 0x0001, 128 + 12 * i] {
            table.extend_from_slice(&word.to_be_bytes());
        }
    }
    emu.vdp_mut().write_vram_debug(0xF000, &table);
}

/// The 21 sprites moved to line `y` through the VDP's ports, as the 68000
/// writes them: register 15 set to step 8 bytes, a VRAM write from $F000,
/// and each sprite's Y word.
fn move_sprites_through_the_ports(emu: &mut GenesisEmulator, y: u16) {
    let vdp = emu.vdp_mut();
    vdp.write_word_port(0x04, 0x8F08);
    vdp.write_word_port(0x04, 0x7000);
    vdp.write_word_port(0x04, 0x0003);
    for _ in 0..21 {
        vdp.write_word_port(0x00, y + 128);
    }
}

/// The VDP draws a line's sprites from a list it took as the line before
/// began: a write into the sprite table while a line runs shows from the
/// line after next. Stopped at line 50 with the sprites on line 51, the
/// table written then takes them off the screen from line 52; line 51 still
/// draws them from the list the extension carries, so the overflow they
/// raise there is raised after a load too, and the list is built again at
/// the same line.
#[test]
fn a_save_mid_frame_keeps_the_sprite_list_the_next_line_draws() {
    let rom = sprite_program();
    let mut live = booted(&rom);
    // The program places the sprite table at $F000 as it starts; the
    // sprites are written after, so the VDP's copy takes them.
    live.run_frame();
    put_sprites(&mut live, 51);
    for _ in 0..2 {
        live.run_frame();
    }
    live.debugger().add_breakpoint(scanline(50));
    live.run_frame();
    assert_eq!(
        live.execution_mode(),
        ExecutionMode::Paused,
        "stopped at line 50"
    );
    move_sprites_through_the_ports(&mut live, 400);
    let read = live.savestate_extension().vdp.sprites;
    assert_eq!(
        read.map(|list| list.iter().filter(|sprite| sprite.y == 51).count()),
        Some(21),
        "line 51 draws from the list taken before the write"
    );
    saved_and_loaded_go_on_alike(&rom, &mut live, 3);
}

/// The 68000 asks for the Z80's bus with a short write and reads the grant
/// in the next instruction, before it comes; it adds what it reads into
/// work RAM.
fn bus_grant_program() -> Vec<u8> {
    cartridge(
        b"THE Z80 BUS GRANT       ",
        &[
            0x46FC, 0x2700, // $200 MOVE.W #$2700,SR
            0x43F9, 0x00A1, 0x1100, // $204 LEA $A11100,A1
            0x33FC, 0x0100, 0x00A1, 0x1200, // $20A MOVE.W #$0100,$A11200: Z80 out of reset
            0x323C, 0x0100, // $212 MOVE.W #$0100,D1
            0x3281, // $216 loop: MOVE.W D1,(A1): the request
            0x1411, // $218 MOVE.B (A1),D2: the grant, not come yet
            0xD539, 0x00FF, 0x0000, // $21A ADD.B D2,$FF0000
            0x4251, // $220 CLR.W (A1): the bus given back
            0x60F2, // $222 BRA.S loop
        ],
    )
}

/// Stopped before the read of the grant, the request waiting for it: the
/// read after the load finds it not granted yet, as the machine that went
/// on does.
#[test]
fn a_save_keeps_a_z80_bus_request_waiting_for_its_grant() {
    let rom = bus_grant_program();
    let mut live = booted(&rom);
    live.run_frame();
    live.debugger().add_breakpoint(execute_at(0x218));
    stop_times(&mut live, 200);
    assert_ne!(
        live.savestate_extension().z80_bus_grant,
        0,
        "the request waits for its grant"
    );
    saved_and_loaded_go_on_alike(&rom, &mut live, 3);
}

/// The vertical interrupt on, counted in $FF0004 by its handler; the main
/// loop counts in D0.
fn vint_program() -> Vec<u8> {
    cartridge_with_vint(
        b"THE VERTICAL INTERRUPT  ",
        &[
            0x46FC, 0x2000, // $200 MOVE.W #$2000,SR
            0x41F9, 0x00C0, 0x0004, // $204 LEA $C00004,A0
            0x30BC, 0x8164, // $20A register 1: display on, vertical interrupt on, mode 5
            0x5280, // $20E loop: ADDQ.L #1,D0
            0x60FC, // $210 BRA.S loop
        ],
        &[
            0x52B9, 0x00FF, 0x0004, // ADDQ.L #1,$FF0004
            0x4E73, // RTE
        ],
    )
}

/// Stopped where vertical blanking starts, the interrupt's request held
/// off into the line: the load holds it off as long.
#[test]
fn a_save_where_vertical_blanking_starts_keeps_the_interrupt_held_off() {
    let rom = vint_program();
    let mut live = booted(&rom);
    for _ in 0..3 {
        live.run_frame();
    }
    live.debugger().add_breakpoint(scanline(224));
    live.run_frame();
    assert_eq!(
        live.execution_mode(),
        ExecutionMode::Paused,
        "stopped at line 224"
    );
    assert!(
        live.savestate_extension().vdp.vint_delay.is_some(),
        "the interrupt held off"
    );
    let counted = long_at(&mut live, 0x00FF_0004);
    saved_and_loaded_go_on_alike(&rom, &mut live, 3);
    assert_eq!(
        long_at(&mut live, 0x00FF_0004),
        counted + 3,
        "one interrupt a frame"
    );
}

/// Stopped in the first frame after the reset, whose vertical blank raises
/// no interrupt: the machine that loads the state skips it too.
#[test]
fn a_save_in_the_first_frame_keeps_its_blank_without_an_interrupt() {
    let rom = vint_program();
    let mut live = booted(&rom);
    live.debugger().add_breakpoint(scanline(100));
    live.run_frame();
    assert_eq!(live.frame_count(), 0, "stopped in the first frame");
    assert!(
        live.savestate_extension().vdp.first_frame_after_reset,
        "the first frame after the reset"
    );
    saved_and_loaded_go_on_alike(&rom, &mut live, 3);
    assert_eq!(
        long_at(&mut live, 0x00FF_0004),
        2,
        "no interrupt in the first frame"
    );
}
