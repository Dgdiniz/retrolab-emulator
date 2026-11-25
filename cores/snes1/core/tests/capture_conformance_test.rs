//! The capture contract's conformance suite (`retro_core::capture::conformance`)
//! run on the SNES core, with a program of its own.
//!
//! The program draws BG1 in Mode 1, a pattern of tiles with holes where the
//! backdrop shows, and four sprites of 8 by 8 that overlap. Every frame its
//! NMI handler moves the sprites, sends their four entries of the sprite
//! table from work RAM and one of two faces of tile 1 by transfer, stores a
//! colour, BG1's two scrolls and a cell of its map through the ports, and
//! takes the sprites off the main screen in one frame of four. An IRQ on
//! line 100 moves BG1's horizontal scroll again, part of the way down the
//! picture. The S-DSP plays a square wave the whole time.

use retro_core::capture::conformance::{CoreUnderTest, VideoMemoryEdit};
use retro_core::{Breakpoint, BreakpointType, EmulatorCore, ExecutionMode};
use snes1_core::apu::Apu;
use snes1_core::debugger::snes_events;
use snes1_core::SnesEmulator;

/// Where the NMI handler starts, `$00:F000`.
const NMI_HANDLER: u32 = 0x00_F000;
/// Where the NMI handler sits in the file.
const NMI_HANDLER_IN_FILE: usize = 0x7000;

/// Where the IRQ handler starts, `$00:E000`.
const IRQ_HANDLER: u16 = 0xE000;
/// Where the IRQ handler sits in the file.
const IRQ_HANDLER_IN_FILE: usize = 0x6000;

/// Where the data sits in the file: tile 1's two faces, one after the
/// other; tiles 0 to 4; the sprites' tile; BG1's map; the palette; the
/// sprite table.
const TILE_ONE_FACES: usize = 0x8000;
const TILES: usize = 0x9000;
const SPRITE_TILE: usize = 0x9100;
const MAP: usize = 0xA000;
const PALETTE: usize = 0xB000;
const SPRITE_TABLE: usize = 0xC000;

/// Where the four moving sprites' entries are kept in work RAM.
const SPRITES_IN_WORK_RAM: u16 = 0x0200;

/// `LDA #value` / `STA address`.
fn store(code: &mut Vec<u8>, value: u8, address: u16) {
    code.extend_from_slice(&[0xA9, value, 0x8D]);
    code.extend_from_slice(&address.to_le_bytes());
}

/// `STZ address`.
fn store_zero(code: &mut Vec<u8>, address: u16) {
    code.push(0x9C);
    code.extend_from_slice(&address.to_le_bytes());
}

/// `LDA $10`, the frame count, then `bytes`.
fn with_frame_count(code: &mut Vec<u8>, bytes: &[u8]) {
    code.extend_from_slice(&[0xA5, 0x10]);
    code.extend_from_slice(bytes);
}

/// A general DMA on channel 0 of `length` bytes from the A-bus address
/// `source` to the B-bus port `$21xx`, in transfer unit `unit`.
fn dma(code: &mut Vec<u8>, unit: u8, port: u8, source: u32, length: u16) {
    store(code, unit, 0x4300);
    store(code, port, 0x4301);
    store(code, source as u8, 0x4302);
    store(code, (source >> 8) as u8, 0x4303);
    store(code, (source >> 16) as u8, 0x4304);
    store(code, length as u8, 0x4305);
    store(code, (length >> 8) as u8, 0x4306);
    store(code, 0x01, 0x420B);
}

/// The VRAM port stepping on its high byte, at word `word`.
fn vram_address(code: &mut Vec<u8>, word: u16) {
    store(code, 0x80, 0x2115);
    store(code, word as u8, 0x2116);
    store(code, (word >> 8) as u8, 0x2117);
}

/// Where a LoROM file offset is seen on the A-bus.
fn lorom(offset: usize) -> u32 {
    (((offset >> 15) as u32) << 16) | 0x8000 | (offset as u32 & 0x7FFF)
}

/// A 4bpp tile whose pixel at row `y` and column `x` is `pixel(x, y)`.
fn tile(pixel: impl Fn(usize, usize) -> u8) -> [u8; 32] {
    let mut tile = [0u8; 32];
    for y in 0..8 {
        for x in 0..8 {
            let value = pixel(x, y);
            for plane in 0..4 {
                if value & (1 << plane) != 0 {
                    let at = (plane / 2) * 16 + y * 2 + plane % 2;
                    tile[at] |= 0x80 >> x;
                }
            }
        }
    }
    tile
}

/// A 4bpp tile of one colour.
fn solid_tile(colour: u8) -> [u8; 32] {
    tile(|_, _| colour)
}

/// Colour `index` of the program's palette, BGR555: a different one for
/// each index but 0, the backdrop's black.
fn colour(index: u16) -> u16 {
    if index == 0 {
        0
    } else {
        (index & 0x1F) | (((index >> 5) * 4 + 3) << 5) | (((index * 7) & 0x1F) << 10)
    }
}

/// The four moving sprites' entries of the sprite table's low part: eight
/// by eight, tile 0, six pixels apart so that they overlap, in front of
/// BG1, each of its own palette.
fn moving_sprites() -> Vec<u8> {
    (0..4u8)
        .flat_map(|sprite| [100 + 6 * sprite, 64 + 3 * sprite, 0, 0x30 | (sprite << 1)])
        .collect()
}

/// The program of the module's notes.
fn program() -> Vec<u8> {
    let mut reset = vec![0x78]; // SEI
    store(&mut reset, 0x80, 0x2100); // forced blank
    store(&mut reset, 0x01, 0x2105); // Mode 1
    store(&mut reset, 0x04, 0x2107); // BG1's map at word $0400, 32 by 32
    store_zero(&mut reset, 0x210B); // BG1's characters at word 0
    store(&mut reset, 0x02, 0x2101); // sprite characters at word $4000, 8x8 and 16x16
    store(&mut reset, 0x11, 0x212C); // BG1 and the sprites on the main screen
    vram_address(&mut reset, 0x0000);
    dma(&mut reset, 0x01, 0x18, lorom(TILES), 5 * 32);
    vram_address(&mut reset, 0x4000);
    dma(&mut reset, 0x01, 0x18, lorom(SPRITE_TILE), 32);
    vram_address(&mut reset, 0x0400);
    dma(&mut reset, 0x01, 0x18, lorom(MAP), 32 * 32 * 2);
    store_zero(&mut reset, 0x2121);
    dma(&mut reset, 0x00, 0x22, lorom(PALETTE), 512);
    store_zero(&mut reset, 0x2102);
    store_zero(&mut reset, 0x2103);
    dma(&mut reset, 0x00, 0x04, lorom(SPRITE_TABLE), 544);
    for (at, byte) in moving_sprites().into_iter().enumerate() {
        store(&mut reset, byte, SPRITES_IN_WORK_RAM + at as u16);
    }
    store(&mut reset, 100, 0x4209); // the IRQ on line 100
    store_zero(&mut reset, 0x420A);
    store(&mut reset, 0xA0, 0x4200); // the NMI and the line IRQ on
    reset.push(0x58); // CLI
    store(&mut reset, 0x0F, 0x2100); // display on
    reset.extend_from_slice(&[0x80, 0xFE]); // BRA *

    let mut nmi = vec![0x48]; // PHA
    nmi.extend_from_slice(&[0xE6, 0x10]); // INC $10: the frames
    for sprite in 0..4u16 {
        // INC $0200 + 4 * sprite: the sprite one pixel right.
        nmi.push(0xEE);
        nmi.extend_from_slice(&(SPRITES_IN_WORK_RAM + 4 * sprite).to_le_bytes());
    }
    store_zero(&mut nmi, 0x2102);
    store_zero(&mut nmi, 0x2103);
    dma(&mut nmi, 0x00, 0x04, u32::from(SPRITES_IN_WORK_RAM), 16);
    // Tile 1, one face or the other: from the first, or 32 bytes on in odd
    // frames.
    vram_address(&mut nmi, 0x0010);
    store(&mut nmi, 0x01, 0x4300);
    store(&mut nmi, 0x18, 0x4301);
    // AND #$01 / ASL A five times / STA $4302
    with_frame_count(
        &mut nmi,
        &[0x29, 0x01, 0x0A, 0x0A, 0x0A, 0x0A, 0x0A, 0x8D, 0x02, 0x43],
    );
    let faces = lorom(TILE_ONE_FACES);
    store(&mut nmi, (faces >> 8) as u8, 0x4303);
    store(&mut nmi, (faces >> 16) as u8, 0x4304);
    store(&mut nmi, 32, 0x4305);
    store_zero(&mut nmi, 0x4306);
    store(&mut nmi, 0x01, 0x420B);
    // Colour 2 through the port: the frame count.
    store(&mut nmi, 0x02, 0x2121);
    with_frame_count(&mut nmi, &[0x8D, 0x22, 0x21]); // STA $2122
    store_zero(&mut nmi, 0x2122);
    // BG1's scrolls: the frame count, across and down.
    with_frame_count(&mut nmi, &[0x8D, 0x0D, 0x21]); // STA $210D
    store_zero(&mut nmi, 0x210D);
    with_frame_count(&mut nmi, &[0x8D, 0x0E, 0x21]); // STA $210E
    store_zero(&mut nmi, 0x210E);
    // A cell of the map through the port: word $0421 is tile 1 + count % 4.
    vram_address(&mut nmi, 0x0421);
    // AND #$03 / INC A / STA $2118
    with_frame_count(&mut nmi, &[0x29, 0x03, 0x1A, 0x8D, 0x18, 0x21]);
    store_zero(&mut nmi, 0x2119);
    // One frame in four, BG1 alone on the main screen: AND #$03 /
    // BNE +4 / LDA #$01 / BRA +2 / LDA #$11 / STA $212C.
    with_frame_count(
        &mut nmi,
        &[
            0x29, 0x03, 0xD0, 0x04, 0xA9, 0x01, 0x80, 0x02, 0xA9, 0x11, 0x8D, 0x2C, 0x21,
        ],
    );
    nmi.extend_from_slice(&[0x68, 0x40]); // PLA / RTI

    // The IRQ: read $4211 to answer it, then CLC / ADC #$10 / STA $210D:
    // BG1 sixteen pixels further right.
    let mut irq = vec![0x48, 0xAD, 0x11, 0x42]; // PHA / LDA $4211
    with_frame_count(&mut irq, &[0x18, 0x69, 0x10, 0x8D, 0x0D, 0x21]);
    store_zero(&mut irq, 0x210D);
    irq.extend_from_slice(&[0x68, 0x40]); // PLA / RTI

    let mut rom = vec![0u8; 0x2_0000];
    assert!(reset.len() < IRQ_HANDLER_IN_FILE, "the reset code fits");
    rom[..reset.len()].copy_from_slice(&reset);
    rom[IRQ_HANDLER_IN_FILE..IRQ_HANDLER_IN_FILE + irq.len()].copy_from_slice(&irq);
    rom[NMI_HANDLER_IN_FILE..NMI_HANDLER_IN_FILE + nmi.len()].copy_from_slice(&nmi);

    let mut faces = solid_tile(1).to_vec();
    faces.extend_from_slice(&tile(|x, y| if (x + y) % 2 == 0 { 1 } else { 5 }));
    // Tile 0 empty, tiles 1 to 3 of colours 1 to 3, tile 4 empty.
    let tiles: Vec<u8> = [0u8, 1, 2, 3, 0].into_iter().flat_map(solid_tile).collect();
    // The cell of row r and column c is tile 1 + (r + c) % 4.
    let map: Vec<u8> = (0..32u16)
        .flat_map(|row| (0..32u16).map(move |column| 1 + (row + column) % 4))
        .flat_map(|cell| cell.to_le_bytes())
        .collect();
    let palette: Vec<u8> = (0..256u16)
        .flat_map(|index| colour(index).to_le_bytes())
        .collect();
    // The four moving sprites, the other 124 below the picture.
    let mut sprite_table = moving_sprites();
    for _ in 4..128 {
        sprite_table.extend_from_slice(&[0, 0xF0, 0, 0]);
    }
    sprite_table.extend_from_slice(&[0; 32]);
    for (offset, bytes) in [
        (TILE_ONE_FACES, faces),
        (TILES, tiles),
        (SPRITE_TILE, solid_tile(1).to_vec()),
        (MAP, map),
        (PALETTE, palette),
        (SPRITE_TABLE, sprite_table),
    ] {
        rom[offset..offset + bytes.len()].copy_from_slice(&bytes);
    }

    let header = 0x7FB0;
    rom[header + 0x10..header + 0x25].copy_from_slice(b"CAPTURE CONFORMANCE  ");
    rom[header + 0x25] = 0x20; // LoROM
    rom[header + 0x27] = 0x07; // 128 KiB
    rom[header + 0x29] = 0x01; // NTSC
    rom[header + 0x2C..header + 0x2E].copy_from_slice(&(!0x1234u16).to_le_bytes());
    rom[header + 0x2E..header + 0x30].copy_from_slice(&0x1234u16.to_le_bytes());
    rom[0x7FFA..0x7FFC].copy_from_slice(&(NMI_HANDLER as u16).to_le_bytes());
    rom[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes()); // reset
    rom[0x7FFE..0x8000].copy_from_slice(&IRQ_HANDLER.to_le_bytes());
    rom
}

/// Write S-DSP register `register`.
fn dsp_write(apu: &mut Apu, register: u8, value: u8) {
    apu.spc_write(0x00F2, register);
    apu.spc_write(0x00F3, value);
}

/// Key voice 0 on, playing a looped BRR square wave, so that the S-DSP
/// output is not silence.
fn play_a_square_wave(emu: &mut SnesEmulator) {
    let apu = emu.bus_mut().apu_mut();
    for (address, value) in [
        (0x0200, 0x00),
        (0x0201, 0x03),
        (0x0202, 0x00),
        (0x0203, 0x03),
    ] {
        apu.spc_write(address, value);
    }
    apu.spc_write(0x0300, 0x83);
    for address in 0x0301..=0x0304 {
        apu.spc_write(address, 0x77);
    }
    for address in 0x0305..=0x0308 {
        apu.spc_write(address, 0x99);
    }
    for (register, value) in [
        (0x00, 0x7F),
        (0x01, 0x40),
        (0x02, 0x00),
        (0x03, 0x08),
        (0x04, 0x00),
        (0x05, 0x00),
        (0x07, 0x7F),
        (0x5D, 0x02),
        (0x0C, 0x7F),
        (0x1C, 0x7F),
        (0x6C, 0x00),
        (0x4C, 0x01),
    ] {
        dsp_write(apu, register, value);
    }
}

/// A machine running the program one frame in, the APU's boot ROM run and
/// the square wave playing, its audio on.
fn build() -> SnesEmulator {
    let mut emu = SnesEmulator::new();
    emu.load_rom(&program()).expect("the program loads");
    emu.set_execution_mode(ExecutionMode::Running);
    emu.set_audio_enabled(true);
    emu.run_frame();
    play_a_square_wave(&mut emu);
    emu
}

/// An event breakpoint on line `line`.
fn on_line(line: u32) -> Breakpoint {
    Breakpoint {
        address: 0,
        bp_type: BreakpointType::Event {
            code: snes_events::SCANLINE,
            param: line,
        },
        cpu_id: None,
        condition: None,
    }
}

/// The SNES core, as the suite takes it.
fn snes() -> CoreUnderTest<SnesEmulator> {
    CoreUnderTest {
        build,
        rom: program(),
        // The frame call a player's host makes, the fast loop while nothing
        // of the debugger is armed.
        run_frame: |emu| EmulatorCore::run_frame(emu),
        state_hash: SnesEmulator::state_hash,
        frame_count: SnesEmulator::frame_count,
        sprite_order: |emu| emu.video_debugger().resolved_sprite_render_order(),
        // The layer list: BG1 to BG4, then the sprites.
        sprite_layer: 4,
        rest_of_frame: NMI_HANDLER,
        stop_in_picture: on_line(100),
        stop_in_vertical_blank: on_line(230),
        video_memory_edits: vec![
            VideoMemoryEdit {
                memory: "VRAM, tile 3 of BG1",
                write: |emu| emu.write_vram_debug(3 * 16, &solid_tile(4)),
            },
            VideoMemoryEdit {
                memory: "CGRAM, colour 3 of BG1",
                write: |emu| emu.write_cgram_debug(3, &[0x7FFF]),
            },
            VideoMemoryEdit {
                memory: "OAM, sprite 0",
                write: |emu| {
                    let x = emu.ppu().oam()[0];
                    emu.write_oam_debug(0, &[x.wrapping_add(40)]);
                },
            },
        ],
    }
}

retro_core::capture_conformance_tests!(snes());

/// The program's bytes are its image set's (`tests/golden/images/conformance/`),
/// which the golden images (`image_goldens.rs`) and the command-line tools'
/// tests of `images` load, as a host's own tests can: `RETRO_UPDATE_GOLDEN=1`
/// writes them again.
#[test]
fn the_program_is_the_image_set_s_program() {
    retro_core::capture::image_goldens::hold_program(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden/images/conformance/program.bin"),
        &program(),
    );
}
