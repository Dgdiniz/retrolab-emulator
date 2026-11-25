//! The SNES core's golden images (`retro_core::capture::image_goldens`):
//! two programs, as their image sets hold them (`program.bin`), each run
//! from power-on, each listed frame held at its capture boundary, and the
//! images a host reads of it held to the set in
//! `tests/golden/images/<set>/`: the picture, each layer alone, each
//! layer's whole map, each sprite the frame drew and those sprites
//! together, the tiles in each palette line in use, and the palette.
//!
//! `conformance` is the program the capture conformance suite runs, which
//! `capture_conformance_test.rs` holds its set's program to: BG1 in Mode 1,
//! four sprites that move, a scroll changed part of the way down the
//! picture. In frame 5 the program takes the sprites off the main screen.
//!
//! `mode7` is this file's own program: a Mode 7 plane of 128 by 128
//! characters, turned and scaled by the matrix, BG1 and BG2 both on the
//! main screen under EXTBG, the plane's horizontal scroll moved one pixel a
//! frame by the NMI handler.
//!
//! `RETRO_UPDATE_GOLDEN=1` writes the sets again.

use retro_core::capture::image_goldens::hold_program;

retro_core::image_golden_tests!(snes1_core::SnesEmulator;
    conformance => [4, 5, 10],
    mode7 => [3, 30],
);

/// The set the Mode 7 program's bytes are kept in.
fn mode7_set() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/images/mode7")
}

#[test]
fn the_mode_7_program_is_its_image_set_s_program() {
    hold_program(&mode7_set().join("program.bin"), &mode7_program());
}

/// Where the NMI handler starts, `$00:F000`, and where it sits in the file.
const NMI_HANDLER: u16 = 0xF000;
const NMI_HANDLER_IN_FILE: usize = 0x7000;

/// Where an `RTI` the IRQ vector names sits, `$00:F100`, and in the file;
/// the program raises no IRQ.
const RETURN: u16 = 0xF100;
const RETURN_IN_FILE: usize = 0x7100;

/// Where the data sits in the file: the plane's map, its characters, the
/// palette.
const MAP: usize = 0x8000;
const CHARACTERS: usize = 0xC000;
const PALETTE: usize = 0xD000;

/// The characters the map names, 1 to 4 (character 0 is empty).
const CHARACTER_COUNT: usize = 5;

/// The matrix: a turn of 30 degrees and a scale of 1.25, in the PPU's 8.8
/// fixed point (`A = D = 1.25 cos 30`, `B = -C = 1.25 sin 30`).
const MATRIX: [u16; 4] = [0x0115, 0x00A0, 0xFF60, 0x0115];

/// The centre of the turn, in the plane: the middle of the picture.
const CENTRE: (u16, u16) = (128, 112);

/// `LDA #value` / `STA address`.
fn store(code: &mut Vec<u8>, value: u8, address: u16) {
    code.extend_from_slice(&[0xA9, value, 0x8D]);
    code.extend_from_slice(&address.to_le_bytes());
}

/// Write a register the PPU takes in two writes, low byte then high.
fn store_twice(code: &mut Vec<u8>, value: u16, address: u16) {
    store(code, value as u8, address);
    store(code, (value >> 8) as u8, address);
}

/// A general DMA on channel 0 of `length` bytes from the A-bus address
/// `source` to the B-bus port `$21xx`, one register written.
fn dma(code: &mut Vec<u8>, port: u8, source: u32, length: u16) {
    store(code, 0x00, 0x4300);
    store(code, port, 0x4301);
    store(code, source as u8, 0x4302);
    store(code, (source >> 8) as u8, 0x4303);
    store(code, (source >> 16) as u8, 0x4304);
    store(code, length as u8, 0x4305);
    store(code, (length >> 8) as u8, 0x4306);
    store(code, 0x01, 0x420B);
}

/// Where a LoROM file offset is seen on the A-bus.
fn lorom(offset: usize) -> u32 {
    (((offset >> 15) as u32) << 16) | 0x8000 | (offset as u32 & 0x7FFF)
}

/// Pixel `(x, y)` of character `character`, a colour of 8 bits. Under
/// EXTBG, BG2 shows the low 7 bits and takes bit 7 as its priority, so the
/// characters set bit 7 on some pixels: one with bit 7 alone (character 4's
/// left half) is a colour to BG1 and transparent to BG2.
fn character_pixel(character: usize, x: usize, y: usize) -> u8 {
    match character {
        1 if x == 0 || y == 0 || x == 7 || y == 7 => 0x81,
        1 => 0x01,
        2 if x == y => 0x82,
        2 => 0x02,
        3 if (x + y).is_multiple_of(2) => 0x03,
        4 if x < 4 => 0x80,
        4 => 0x84,
        _ => 0x00,
    }
}

/// Colour `index` of the program's palette, BGR555: colours 1 to 4 and
/// `0x80` to `0x84` each a colour of its own, every other one black, so
/// that two palette lines are in use.
fn colour(index: u16) -> u16 {
    match index {
        1..=4 | 0x80..=0x84 => {
            let n = index & 0x0F;
            (n * 6 + 1) | (((index >> 7) * 20 + 5) << 5) | ((31 - n * 5) << 10)
        }
        _ => 0,
    }
}

/// The Mode 7 program of the module's notes.
fn mode7_program() -> Vec<u8> {
    let mut reset = vec![0x78]; // SEI
    store(&mut reset, 0x80, 0x2100); // forced blank
    store(&mut reset, 0x07, 0x2105); // Mode 7
    store(&mut reset, 0x00, 0x211A); // M7SEL: the plane repeats, no flip
    for (register, value) in (0x211Bu16..).zip(MATRIX) {
        store_twice(&mut reset, value, register);
    }
    store_twice(&mut reset, CENTRE.0, 0x211F);
    store_twice(&mut reset, CENTRE.1, 0x2120);
    store_twice(&mut reset, 0, 0x210D); // the plane's scroll
    store_twice(&mut reset, 0, 0x210E);
    store(&mut reset, 0x40, 0x2133); // SETINI: EXTBG
    store(&mut reset, 0x03, 0x212C); // BG1 and BG2 on the main screen

    // The map: the low byte of VRAM words 0 to $3FFF, the address stepping
    // after each low byte.
    store(&mut reset, 0x00, 0x2115);
    store(&mut reset, 0x00, 0x2116);
    store(&mut reset, 0x00, 0x2117);
    dma(&mut reset, 0x18, lorom(MAP), 128 * 128);

    // The characters: the high byte of VRAM words 0 to 319, the address
    // stepping after each high byte.
    store(&mut reset, 0x80, 0x2115);
    store(&mut reset, 0x00, 0x2116);
    store(&mut reset, 0x00, 0x2117);
    dma(
        &mut reset,
        0x19,
        lorom(CHARACTERS),
        (CHARACTER_COUNT * 64) as u16,
    );
    store(&mut reset, 0x00, 0x2121);
    dma(&mut reset, 0x22, lorom(PALETTE), 512);
    store(&mut reset, 0x80, 0x4200); // the NMI on
    store(&mut reset, 0x0F, 0x2100); // display on
    reset.extend_from_slice(&[0x80, 0xFE]); // BRA *

    // The NMI: PHA / INC $10 (the frames) / LDA $10 / STA $210D /
    // STZ $210D: the plane's horizontal scroll is the frame count.
    let nmi = [
        0x48, 0xE6, 0x10, 0xA5, 0x10, 0x8D, 0x0D, 0x21, 0x9C, 0x0D, 0x21, 0x68, 0x40,
    ];

    let mut rom = vec![0u8; 0x2_0000];
    assert!(reset.len() < NMI_HANDLER_IN_FILE, "the reset code fits");
    rom[..reset.len()].copy_from_slice(&reset);
    rom[NMI_HANDLER_IN_FILE..NMI_HANDLER_IN_FILE + nmi.len()].copy_from_slice(&nmi);
    rom[RETURN_IN_FILE] = 0x40; // RTI

    // The map's cell of row r and column c names character 1 + (r / 2 +
    // c / 2) % 4.
    let map: Vec<u8> = (0..128usize)
        .flat_map(|row| (0..128usize).map(move |column| (1 + (row / 2 + column / 2) % 4) as u8))
        .collect();
    let characters: Vec<u8> = (0..CHARACTER_COUNT)
        .flat_map(|character| {
            (0..64).map(move |pixel| character_pixel(character, pixel % 8, pixel / 8))
        })
        .collect();
    let palette: Vec<u8> = (0..256u16)
        .flat_map(|index| colour(index).to_le_bytes())
        .collect();
    for (offset, bytes) in [(MAP, map), (CHARACTERS, characters), (PALETTE, palette)] {
        rom[offset..offset + bytes.len()].copy_from_slice(&bytes);
    }

    let header = 0x7FB0;
    rom[header + 0x10..header + 0x25].copy_from_slice(b"MODE 7 IMAGES        ");
    rom[header + 0x25] = 0x20; // LoROM
    rom[header + 0x27] = 0x07; // 128 KiB
    rom[header + 0x29] = 0x01; // NTSC
    rom[header + 0x2C..header + 0x2E].copy_from_slice(&(!0x1234u16).to_le_bytes());
    rom[header + 0x2E..header + 0x30].copy_from_slice(&0x1234u16.to_le_bytes());
    rom[0x7FFA..0x7FFC].copy_from_slice(&NMI_HANDLER.to_le_bytes());
    rom[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes()); // reset
    rom[0x7FFE..0x8000].copy_from_slice(&RETURN.to_le_bytes());
    rom
}
