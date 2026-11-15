//! The debugger's video-memory events: a DMA channel done (by its B-bus
//! port), a palette colour written, a tile changed. Each pauses the frame
//! after the instruction or the HDMA batch that raised it and counts one hit
//! per event; with no breakpoint watching colours or tiles, the PPU's ports
//! keep their plain path.

use retro_core::{
    Breakpoint, BreakpointId, BreakpointType, EmulatorCore, ExecutionMode, BREAKPOINT_PARAM_ANY,
};
use snes1_core::debugger::snes_events;
use snes1_core::SnesEmulator;

// -------------------------------------------------------------
// Programs
// -------------------------------------------------------------

/// 65C816 code at `$00:8000`, emulation mode.
#[derive(Default)]
struct Code(Vec<u8>);

impl Code {
    /// The address of the next instruction.
    fn here(&self) -> u32 {
        0x00_8000 + self.0.len() as u32
    }

    /// `LDA #value` / `STA address`; returns the address of the store.
    fn store(&mut self, value: u8, address: u16) -> u32 {
        self.0.extend([0xA9, value]);
        let at = self.here();
        self.0.push(0x8D);
        self.0.extend(address.to_le_bytes());
        at
    }

    /// Program DMA channel `channel`: `length` bytes from `source` to the
    /// port `$21xx` in transfer unit `unit`.
    fn channel(&mut self, channel: u16, unit: u8, port: u8, source: u32, length: u16) {
        let base = 0x4300 + (channel << 4);
        self.store(unit, base);
        self.store(port, base + 1);
        self.store(source as u8, base + 2);
        self.store((source >> 8) as u8, base + 3);
        self.store((source >> 16) as u8, base + 4);
        self.store(length as u8, base + 5);
        self.store((length >> 8) as u8, base + 6);
    }

    /// A general DMA on `channel`; returns the address of the store that
    /// starts it.
    fn dma(&mut self, channel: u16, unit: u8, port: u8, source: u32, length: u16) -> u32 {
        self.channel(channel, unit, port, source, length);
        self.store(1 << channel, 0x420B)
    }

    /// The VRAM word address, with the step on the high byte.
    fn vram_address(&mut self, word: u16) {
        self.store(0x80, 0x2115);
        self.store(word as u8, 0x2116);
        self.store((word >> 8) as u8, 0x2117);
    }

    /// `BRA` back to `target`.
    fn branch_to(&mut self, target: u32) {
        let back = target as i64 - (self.here() as i64 + 2);
        self.0.extend([0x80, back as i8 as u8]);
    }

    /// `BRA *`; returns its address.
    fn spin(&mut self) -> u32 {
        let at = self.here();
        self.branch_to(at);
        at
    }
}

/// Where a LoROM file offset is seen on the A bus.
fn lorom(offset: usize) -> u32 {
    (((offset >> 15) as u32) << 16) | 0x8000 | (offset as u32 & 0x7FFF)
}

/// A 128 KiB LoROM image: `reset` at `$00:8000`, an empty NMI handler, and
/// the `data` blocks at their file offsets.
fn build_rom(reset: &[u8], data: &[(usize, Vec<u8>)]) -> Vec<u8> {
    let mut rom = vec![0u8; 0x2_0000];
    rom[..reset.len()].copy_from_slice(reset);
    rom[0x7000] = 0x40; // RTI
    for (offset, bytes) in data {
        rom[*offset..*offset + bytes.len()].copy_from_slice(bytes);
    }
    let header = 0x7FB0;
    rom[header + 0x10..header + 0x25].copy_from_slice(b"CAPTURE EVENTS       ");
    rom[header + 0x25] = 0x20; // LoROM
    rom[header + 0x27] = 0x07; // 128 KiB
    rom[header + 0x29] = 0x01; // NTSC
    rom[header + 0x2C..header + 0x2E].copy_from_slice(&(!0x1234u16).to_le_bytes());
    rom[header + 0x2E..header + 0x30].copy_from_slice(&0x1234u16.to_le_bytes());
    rom[0x7FFA..0x7FFC].copy_from_slice(&0xF000u16.to_le_bytes()); // NMI
    rom[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes()); // reset
    rom
}

/// Where the data blocks sit in the image.
const DATA: usize = 0x8000;

fn loaded(rom: &[u8]) -> SnesEmulator {
    let mut emu = SnesEmulator::new();
    emu.load_rom(rom).expect("the program loads");
    emu
}

/// Add an event breakpoint.
fn add_event(emu: &mut SnesEmulator, code: u32, param: u32) -> BreakpointId {
    emu.debugger().add_breakpoint(Breakpoint {
        address: 0,
        bp_type: BreakpointType::Event { code, param },
        cpu_id: None,
        condition: None,
    })
}

/// The hits of breakpoint `id`.
fn hits(emu: &mut SnesEmulator, id: BreakpointId) -> u32 {
    emu.debugger()
        .list_breakpoints()
        .into_iter()
        .find(|info| info.id == id)
        .expect("the breakpoint")
        .hit_count
}

/// The processor's `PBR:PC`.
fn pc(emu: &SnesEmulator) -> u32 {
    let cpu = emu.cpu();
    (u32::from(cpu.pbr) << 16) | u32::from(cpu.pc)
}

// -------------------------------------------------------------
// DMA done
// -------------------------------------------------------------

/// In forced blank: a DMA into VRAM, then one into the sprite table, then
/// `BRA *`. Returns the ROM, the address of the store that starts the
/// sprite-table DMA, and the spin's.
fn two_dmas_rom() -> (Vec<u8>, u32, u32) {
    let mut code = Code::default();
    code.0.push(0x78); // SEI
    code.store(0x80, 0x2100);
    code.vram_address(0x0000);
    code.dma(0, 0x01, 0x18, lorom(DATA), 32);
    code.store(0x00, 0x2102);
    code.store(0x00, 0x2103);
    let sprite_dma = code.dma(1, 0x00, 0x04, lorom(DATA), 0x220);
    let spin = code.spin();
    (
        build_rom(&code.0, &[(DATA, vec![0x5A; 0x220])]),
        sprite_dma,
        spin,
    )
}

#[test]
fn dma_done_on_the_sprite_table_pauses_after_the_instruction_that_started_it() {
    let (rom, sprite_dma, spin) = two_dmas_rom();
    let mut emu = loaded(&rom);
    let id = add_event(&mut emu, snes_events::DMA_DONE, 0x04);
    emu.run_frame();
    assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::Paused);
    assert_eq!(
        pc(&emu),
        sprite_dma + 3,
        "paused after the store that started the sprite-table DMA, not after the VRAM one"
    );
    assert_eq!(pc(&emu), spin);
    assert_eq!(hits(&mut emu, id), 1);
    assert_eq!(emu.ppu().oam()[0], 0x5A, "the block ran before the pause");
}

#[test]
fn dma_start_keeps_its_meaning_and_dma_done_counts_each_channel() {
    // One `$420B` write starts two channels.
    let mut code = Code::default();
    code.0.push(0x78);
    code.store(0x80, 0x2100);
    code.vram_address(0x0000);
    code.channel(0, 0x01, 0x18, lorom(DATA), 32);
    code.channel(1, 0x00, 0x22, lorom(DATA), 32);
    let start = code.store(0x03, 0x420B);
    code.spin();
    let mut emu = loaded(&build_rom(&code.0, &[(DATA, vec![0x11; 32])]));
    let started = add_event(&mut emu, snes_events::DMA_START, BREAKPOINT_PARAM_ANY);
    let done = add_event(&mut emu, snes_events::DMA_DONE, BREAKPOINT_PARAM_ANY);
    let palette = add_event(&mut emu, snes_events::DMA_DONE, 0x22);
    emu.run_frame();
    assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::Paused);
    assert_eq!(pc(&emu), start + 3);
    assert_eq!(hits(&mut emu, started), 1, "one `$420B` write");
    assert_eq!(hits(&mut emu, done), 2, "two channels");
    assert_eq!(hits(&mut emu, palette), 1, "one channel into CGRAM");
}

// -------------------------------------------------------------
// Palette writes
// -------------------------------------------------------------

/// The display on and HDMA channel 1 writing colour 0 on every line (unit 3
/// to `$2121`: the colour index twice, then the colour), then `BRA *`.
fn gradient_rom() -> Vec<u8> {
    let mut code = Code::default();
    code.0.push(0x78);
    code.store(0x0F, 0x2100);
    let table = lorom(DATA);
    code.store(0x03, 0x4310);
    code.store(0x21, 0x4311);
    code.store(table as u8, 0x4312);
    code.store((table >> 8) as u8, 0x4313);
    code.store((table >> 16) as u8, 0x4314);
    code.store(0x02, 0x420C);
    code.spin();
    let mut gradient = Vec::new();
    for line in 0..240u16 {
        gradient.extend([0x01, 0x00, 0x00]);
        gradient.extend((0x0400 + line).to_le_bytes());
    }
    gradient.push(0x00);
    build_rom(&code.0, &[(DATA, gradient)])
}

#[test]
fn a_palette_write_pauses_after_an_hdma_gradients_batch() {
    let mut emu = loaded(&gradient_rom());
    // The program sets HDMA up during frame 1; frame 2 runs it whole.
    emu.run_frame();
    let colour_zero = add_event(&mut emu, snes_events::PALETTE_WRITE, 0);
    let colour_five = add_event(&mut emu, snes_events::PALETTE_WRITE, 5);
    emu.run_frame();
    assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::Paused);
    assert_eq!(emu.frame_count(), 1, "frame 2 stopped on its way");
    assert_eq!(
        emu.bus().cached_v_scanline(),
        0,
        "after the first line's HDMA batch"
    );
    assert_eq!(emu.ppu().cgram()[0], 0x0400, "the first entry's colour");
    assert_eq!(hits(&mut emu, colour_zero), 1);
    assert_eq!(hits(&mut emu, colour_five), 0);

    // On to the next line's batch.
    emu.set_execution_mode(ExecutionMode::Running);
    emu.run_frame();
    assert_eq!(emu.bus().cached_v_scanline(), 1);
    assert_eq!(emu.ppu().cgram()[0], 0x0401);
    assert_eq!(hits(&mut emu, colour_zero), 2);
}

#[test]
fn a_single_step_counts_the_palette_writes_of_its_dma() {
    let mut code = Code::default();
    code.0.push(0x78);
    code.store(0x00, 0x2121);
    code.dma(0, 0x00, 0x22, lorom(DATA), 8);
    let spin = code.spin();
    let mut emu = loaded(&build_rom(
        &code.0,
        &[(DATA, vec![0x1F, 0x00, 0xE0, 0x03, 0x00, 0x7C, 0xFF, 0x7F])],
    ));
    let any = add_event(&mut emu, snes_events::PALETTE_WRITE, BREAKPOINT_PARAM_ANY);
    while pc(&emu) != spin {
        emu.step_cpu(0);
    }
    assert_eq!(hits(&mut emu, any), 4, "four colours, one event each");
    assert_eq!(emu.ppu().cgram()[3], 0x7FFF);
}

#[test]
fn the_debuggers_own_writes_raise_no_event() {
    let mut code = Code::default();
    code.spin();
    let mut emu = loaded(&build_rom(&code.0, &[]));
    let palette = add_event(&mut emu, snes_events::PALETTE_WRITE, BREAKPOINT_PARAM_ANY);
    let tile = add_event(&mut emu, snes_events::TILE_CHANGE, BREAKPOINT_PARAM_ANY);
    // A frame call arms the watch.
    emu.run_frame();
    assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::Running);
    emu.write_memory_debug(0x2121, &[0x00]);
    emu.write_memory_debug(0x2122, &[0x1F]);
    emu.write_memory_debug(0x2122, &[0x00]);
    emu.write_memory_debug(0x2115, &[0x80, 0x00, 0x00]);
    emu.write_memory_debug(0x2118, &[0x77, 0x66]);
    emu.write_vram_debug(0x0100, &[1, 2, 3]);
    emu.run_frame();
    assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::Running);
    assert_eq!(hits(&mut emu, palette), 0);
    assert_eq!(hits(&mut emu, tile), 0);
    assert_eq!(emu.ppu().cgram()[0], 0x001F);
}

// -------------------------------------------------------------
// Tiles
// -------------------------------------------------------------

#[test]
fn a_tile_change_fires_when_a_dma_changes_the_tile_and_not_when_it_writes_the_same_bytes() {
    // In forced blank: tile 0x20 (VRAM word 0x200) filled from ROM, then
    // the same upload over and over.
    let mut code = Code::default();
    code.0.push(0x78);
    code.store(0x80, 0x2100);
    code.vram_address(0x0200);
    let first = code.dma(0, 0x01, 0x18, lorom(DATA), 32);
    let again = code.here();
    code.vram_address(0x0200);
    code.dma(0, 0x01, 0x18, lorom(DATA), 32);
    code.branch_to(again);
    let mut emu = loaded(&build_rom(&code.0, &[(DATA, vec![0x55; 32])]));
    let watched = add_event(&mut emu, snes_events::TILE_CHANGE, 0x20);
    let next_tile = add_event(&mut emu, snes_events::TILE_CHANGE, 0x21);

    emu.run_frame();
    assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::Paused);
    assert_eq!(pc(&emu), first + 3, "after the store that started the DMA");
    assert_eq!(hits(&mut emu, watched), 1, "one event for the whole tile");
    assert_eq!(emu.ppu().vram()[0x200], 0x5555);

    // The same bytes again and again: the frame runs to its end.
    emu.set_execution_mode(ExecutionMode::Running);
    emu.run_frame();
    assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::Running);
    assert_eq!(emu.frame_count(), 1);
    assert_eq!(hits(&mut emu, watched), 1);
    assert_eq!(hits(&mut emu, next_tile), 0);
}

// -------------------------------------------------------------
// The catalogue and the tiers
// -------------------------------------------------------------

#[test]
fn the_catalogue_lists_the_three_classes() {
    let mut emu = SnesEmulator::new();
    let events = emu.debugger().available_events();
    let class = |code: u32| {
        events
            .iter()
            .find(|class| class.code == code)
            .unwrap_or_else(|| panic!("event {code} listed"))
    };
    let dma_done = class(snes_events::DMA_DONE);
    assert_eq!(dma_done.label, "DMA done");
    let ports: Vec<u32> = dma_done.param_options.iter().map(|o| o.value).collect();
    assert_eq!(ports, vec![0x18, 0x22, 0x04, 0x80]);
    for code in [snes_events::PALETTE_WRITE, snes_events::TILE_CHANGE] {
        let class = class(code);
        assert!(class.param_label.is_some(), "event {code} takes a number");
        assert!(class.param_options.is_empty());
    }
}

#[test]
fn the_ports_take_their_plain_path_without_a_watching_breakpoint() {
    let (rom, _, _) = two_dmas_rom();
    let mut emu = loaded(&rom);
    // Another event breakpoint arms the debugger, not the ports.
    let vblank = add_event(&mut emu, snes_events::VBLANK, BREAKPOINT_PARAM_ANY);
    emu.run_frame();
    assert!(!emu.ppu().notes_port_writes());
    emu.debugger().remove_breakpoint(vblank);
    emu.set_execution_mode(ExecutionMode::Running);

    let tile = add_event(&mut emu, snes_events::TILE_CHANGE, 3);
    emu.run_frame();
    assert!(emu.ppu().notes_port_writes(), "a tile watched");
    emu.debugger().remove_breakpoint(tile);
    emu.run_frame();
    assert!(!emu.ppu().notes_port_writes(), "the watch dropped with it");
    assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::Running);
}
