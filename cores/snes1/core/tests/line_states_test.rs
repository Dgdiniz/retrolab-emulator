//! The line table: the settings each line of a frame was drawn with and the
//! writes to video memory made while it was drawn, and the frame drawn again
//! from them.
//!
//! Each scene below changes what the PPU draws from one line to the next,
//! the way games do it, with HDMA, an IRQ in the middle of the frame or the
//! CPU between two lines; a frame drawn again from its line table must be
//! the picture the frame left, while one drawn from the registers as they
//! stand is not. Recording must change nothing the machine does.

use retro_core::capture::{layer, planes, CaptureCore, CaptureRecording};
use retro_core::cpu::Addressable;
use retro_core::savestate::RomCheck;
use retro_core::video::{LineStateTable, SpriteOrderEntry, VideoDebugger};
use retro_core::{Breakpoint, BreakpointType, EmulatorCore, ExecutionMode, Framebuffer};
use snes1_core::debugger::snes_events;
use snes1_core::{SnesBus, SnesEmulator};

// -------------------------------------------------------------
// Helpers
// -------------------------------------------------------------

/// Only the line table armed.
fn line_states_on() -> CaptureRecording {
    CaptureRecording {
        line_states: true,
        ..Default::default()
    }
}

/// Every recorder armed.
fn everything_on() -> CaptureRecording {
    CaptureRecording {
        line_states: true,
        pixel_sources: true,
        line_sprites: true,
        video_memory_sources: true,
        frame_writes: true,
    }
}

/// The picture the host shows: the framebuffer cropped to the display.
fn picture(emu: &SnesEmulator) -> (u32, u32, Vec<u8>) {
    let display = emu.get_display_info();
    let fb = emu.get_framebuffer();
    (
        display.width,
        display.height,
        crop(fb, display.width, display.height),
    )
}

/// The top-left `width` x `height` of a framebuffer, row by row.
fn crop(fb: &Framebuffer, width: u32, height: u32) -> Vec<u8> {
    let stride = fb.width as usize * 4;
    let row = width as usize * 4;
    let mut out = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        out.extend_from_slice(&fb.data[y * stride..y * stride + row]);
    }
    out
}

/// The picture drawn again from the line table, as `(width, height, rgba)`.
fn redrawn(emu: &mut SnesEmulator) -> (u32, u32, Vec<u8>) {
    let image = emu
        .picture_from_line_states()
        .expect("the line table holds the frame");
    (image.width, image.height, image.rgba)
}

/// Video memory as it stands: VRAM, CGRAM, OAM.
fn video_memory(emu: &SnesEmulator) -> (Vec<u16>, Vec<u16>, Vec<u8>) {
    let ppu = emu.ppu();
    (
        ppu.vram().to_vec(),
        ppu.cgram().to_vec(),
        ppu.oam().to_vec(),
    )
}

/// The value of column `name` in every row of `table`.
fn column(table: &LineStateTable, name: &str) -> Vec<u32> {
    let width = table.columns.len();
    let at = table
        .columns
        .iter()
        .position(|c| c == name)
        .unwrap_or_else(|| panic!("no column {name}"));
    table.values.chunks(width).map(|row| row[at]).collect()
}

/// The RGBA the renderer draws for a BGR555 colour at full brightness.
fn rgba(colour: u16) -> [u8; 4] {
    let expand = |c: u16| {
        let c = (c & 0x1F) as u8;
        (c << 3) | (c >> 2)
    };
    [
        expand(colour),
        expand(colour >> 5),
        expand(colour >> 10),
        0xFF,
    ]
}

fn pixel(image: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let at = ((y * width + x) * 4) as usize;
    image[at..at + 4].try_into().unwrap()
}

// -------------------------------------------------------------
// Programs
// -------------------------------------------------------------

/// `LDA #value` / `STA addr`.
fn store(code: &mut Vec<u8>, value: u8, addr: u16) {
    code.extend_from_slice(&[0xA9, value, 0x8D]);
    code.extend_from_slice(&addr.to_le_bytes());
}

/// `STZ addr`.
fn store_zero(code: &mut Vec<u8>, addr: u16) {
    code.push(0x9C);
    code.extend_from_slice(&addr.to_le_bytes());
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

/// HDMA channel `channel`: unit `unit`, direct, to `$21xx`, its table at the
/// A-bus address `table`.
fn hdma(code: &mut Vec<u8>, channel: u16, unit: u8, port: u8, table: u32) {
    let base = 0x4300 + (channel << 4);
    store(code, unit, base);
    store(code, port, base + 1);
    store(code, table as u8, base + 2);
    store(code, (table >> 8) as u8, base + 3);
    store(code, (table >> 16) as u8, base + 4);
}

/// `LDA #$80` / `STA $2115` / the VRAM word address `word`.
fn vram_address(code: &mut Vec<u8>, word: u16) {
    store(code, 0x80, 0x2115);
    store(code, word as u8, 0x2116);
    store(code, (word >> 8) as u8, 0x2117);
}

/// Where a LoROM file offset is seen on the A-bus.
fn lorom(offset: usize) -> u32 {
    (((offset >> 15) as u32) << 16) | 0x8000 | (offset as u32 & 0x7FFF)
}

/// A LoROM image of `size` bytes: `reset` at `$00:8000`, `irq` at
/// `$00:E000`, `nmi` at `$00:F000` (the vectors of emulation mode), and the
/// `data` blocks at their file offsets.
fn build_rom(
    size: usize,
    reset: &[u8],
    irq: &[u8],
    nmi: &[u8],
    data: &[(usize, Vec<u8>)],
) -> Vec<u8> {
    assert!(
        reset.len() <= 0x6000,
        "the reset code runs into the IRQ handler"
    );
    let mut rom = vec![0u8; size];
    rom[..reset.len()].copy_from_slice(reset);
    rom[0x6000..0x6000 + irq.len()].copy_from_slice(irq);
    rom[0x7000..0x7000 + nmi.len()].copy_from_slice(nmi);
    for (offset, bytes) in data {
        assert!(*offset >= 0x8000, "data stays out of bank 0");
        rom[*offset..*offset + bytes.len()].copy_from_slice(bytes);
    }
    let header = 0x7FB0;
    rom[header + 0x10..header + 0x25].copy_from_slice(b"LINE STATES          ");
    rom[header + 0x25] = 0x20; // LoROM
    rom[header + 0x27] = (size.trailing_zeros() - 10) as u8;
    rom[header + 0x29] = 0x01; // NTSC
    rom[header + 0x2C..header + 0x2E].copy_from_slice(&(!0x1234u16).to_le_bytes());
    rom[header + 0x2E..header + 0x30].copy_from_slice(&0x1234u16.to_le_bytes());
    rom[0x7FFA..0x7FFC].copy_from_slice(&0xF000u16.to_le_bytes()); // NMI
    rom[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes()); // reset
    rom[0x7FFE..0x8000].copy_from_slice(&0xE000u16.to_le_bytes()); // IRQ
    rom
}

/// A 4bpp tile filled with colour `colour`.
fn solid_tile(colour: u8) -> [u8; 32] {
    let mut tile = [0u8; 32];
    for row in 0..8 {
        let plane = |bit: u8| if colour & (1 << bit) != 0 { 0xFF } else { 0x00 };
        tile[row * 2] = plane(0);
        tile[row * 2 + 1] = plane(1);
        tile[16 + row * 2] = plane(2);
        tile[16 + row * 2 + 1] = plane(3);
    }
    tile
}

/// Sixteen 4bpp tiles, tile `t` filled with colour `t` (tile 0 empty).
fn sixteen_tiles() -> Vec<u8> {
    (0..16u8).flat_map(solid_tile).collect()
}

/// A 32x32 tilemap whose column `c` holds tile `c % 15 + 1` on every row:
/// a background that repeats every 120 pixels across and never down.
fn column_tilemap() -> Vec<u8> {
    (0..32 * 32u16)
        .flat_map(|cell| ((cell % 32) % 15 + 1).to_le_bytes())
        .collect()
}

/// BG palette 0: colour `t` of sixteen distinct colours.
fn bg_colour(t: u16) -> u16 {
    if t == 0 {
        0
    } else {
        (t * 2) | ((31 - t * 2) << 5) | ((t * 7 % 32) << 10)
    }
}

fn bg_palette() -> Vec<u8> {
    (0..16).flat_map(|t| bg_colour(t).to_le_bytes()).collect()
}

/// A small deterministic generator, so a failure repeats.
struct Numbers(u64);

impl Numbers {
    fn next(&mut self) -> u8 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 32) as u8
    }

    fn bytes(&mut self, count: usize) -> Vec<u8> {
        (0..count).map(|_| self.next()).collect()
    }
}

// -------------------------------------------------------------
// The HDMA scene: a scroll, a colour and a window down the frame
// -------------------------------------------------------------

/// BG1's horizontal scroll on line `line`.
fn hdma_scroll(line: u16) -> u16 {
    (line * 5 + line * line / 7) & 0x3FF
}

/// The backdrop colour on line `line`, a different one on every line.
fn hdma_backdrop(line: u16) -> u16 {
    (line % 32) | ((line / 32) << 5) | ((line % 7) << 10)
}

/// Window 1's edges on line `line`.
fn hdma_window(line: u16) -> (u8, u8) {
    let left = (line * 3 % 200) as u8;
    (left, left + 30)
}

/// Mode 1, BG1 of [`column_tilemap`] on the main screen, and three HDMA
/// channels: unit 2 to BG1HOFS (a scroll per line), unit 3 to CGADD/CGDATA
/// (colour 0 per line) and unit 1 to WH0/WH1 (window 1 per line, which
/// masks BG1 on the main screen). The table entry written at line `n`
/// draws line `n + 1`.
fn hdma_scene_rom() -> Vec<u8> {
    const TILES: usize = 0x1_0000;
    const MAP: usize = 0x1_4000;
    const PALETTE: usize = 0x1_6000;
    const SCROLLS: usize = 0x8000;
    const COLOURS: usize = 0x9000;
    const WINDOWS: usize = 0xA000;

    let mut code = vec![0x78]; // SEI
    store(&mut code, 0x80, 0x2100); // forced blank
    store(&mut code, 0x01, 0x2105); // Mode 1
    store(&mut code, 0x04, 0x2107); // BG1 tilemap at word $0400
    store_zero(&mut code, 0x210B); // BG1 characters at word 0
    store(&mut code, 0x01, 0x212C); // BG1 on the main screen
    store(&mut code, 0x02, 0x2123); // window 1 on BG1
    store(&mut code, 0x01, 0x212E); // and masking it on the main screen
    vram_address(&mut code, 0x0000);
    dma(&mut code, 0x01, 0x18, lorom(TILES), 16 * 32);
    vram_address(&mut code, 0x0400);
    dma(&mut code, 0x01, 0x18, lorom(MAP), 32 * 32 * 2);
    store_zero(&mut code, 0x2121);
    dma(&mut code, 0x00, 0x22, lorom(PALETTE), 32);
    hdma(&mut code, 1, 0x02, 0x0D, lorom(SCROLLS));
    hdma(&mut code, 2, 0x03, 0x21, lorom(COLOURS));
    hdma(&mut code, 3, 0x01, 0x26, lorom(WINDOWS));
    store(&mut code, 0x0E, 0x420C);
    store(&mut code, 0x0F, 0x2100); // display on
    code.extend_from_slice(&[0x80, 0xFE]); // BRA *

    let mut scrolls = Vec::new();
    let mut colours = Vec::new();
    let mut windows = Vec::new();
    for entry in 0..240u16 {
        let line = entry + 1;
        scrolls.push(0x01);
        scrolls.extend_from_slice(&hdma_scroll(line).to_le_bytes());
        colours.extend_from_slice(&[0x01, 0x00, 0x00]);
        colours.extend_from_slice(&hdma_backdrop(line).to_le_bytes());
        let (left, right) = hdma_window(line);
        windows.extend_from_slice(&[0x01, left, right]);
    }
    for table in [&mut scrolls, &mut colours, &mut windows] {
        table.push(0x00);
    }
    build_rom(
        0x2_0000,
        &code,
        &[0x40], // RTI
        &[0x40],
        &[
            (TILES, sixteen_tiles()),
            (MAP, column_tilemap()),
            (PALETTE, bg_palette()),
            (SCROLLS, scrolls),
            (COLOURS, colours),
            (WINDOWS, windows),
        ],
    )
}

/// The HDMA scene armed, held at the boundary of its third frame.
fn hdma_scene_at_boundary() -> SnesEmulator {
    let mut emu = SnesEmulator::new();
    emu.set_capture_recording(line_states_on());
    emu.load_rom(&hdma_scene_rom()).expect("the program loads");
    emu.run_frame();
    emu.run_frame();
    emu.run_to_capture_boundary();
    assert!(emu.at_capture_boundary());
    emu
}

/// BG1 of the HDMA scene on line `line`, pixel `x`, as drawn alone.
fn hdma_scene_bg1(line: u16, x: u32) -> [u8; 4] {
    let screen_x = (x + u32::from(hdma_scroll(line))) & 0xFF;
    let tile = (screen_x / 8) % 15 + 1;
    rgba(bg_colour(tile as u16))
}

#[test]
fn the_hdma_path_is_recorded() {
    let mut emu = hdma_scene_at_boundary();
    let table = emu.line_states().expect("a line table");
    assert!(table.from_first_line);
    assert!(table.complete);
    assert!(table.memory_exact);
    assert_eq!(table.frame, 3);
    assert_eq!((table.rows_per_line, table.first_row_line), (1, 1));
    assert_eq!(table.columns[0], "line");

    let lines = column(&table, "line");
    assert_eq!(lines, (1..=224).collect::<Vec<u32>>());
    let scrolls = column(&table, "bg_hoffset[0]");
    let lefts = column(&table, "window_one_left");
    let rights = column(&table, "window_one_right");
    for (row, &line) in lines.iter().enumerate() {
        let line = line as u16;
        assert_eq!(scrolls[row], u32::from(hdma_scroll(line)), "line {line}");
        let (left, right) = hdma_window(line);
        assert_eq!((lefts[row], rights[row]), (left.into(), right.into()));
    }

    // The colour HDMA wrote to CGRAM, line by line.
    let palettes = emu.line_palettes().expect("the palettes");
    assert_eq!(palettes.colours_per_palette, 256);
    assert_eq!(palettes.palette_of_line.len(), lines.len());
    assert_eq!(palettes.palettes.len() / 256, 224, "a palette per line");
    for (row, &line) in lines.iter().enumerate() {
        let palette = usize::from(palettes.palette_of_line[row]);
        let backdrop = palettes.palettes[palette * 256];
        assert_eq!(backdrop, hdma_backdrop(line as u16), "line {line}");
    }

    let shown = picture(&emu);
    assert_eq!(redrawn(&mut emu), shown);
    // Inside the window line 100 shows its backdrop, outside it BG1.
    let (left, right) = hdma_window(100);
    let (width, _, image) = &shown;
    assert_eq!(
        pixel(image, *width, u32::from(left) + 1, 99),
        rgba(hdma_backdrop(100))
    );
    assert_eq!(
        pixel(image, *width, u32::from(right) + 2, 99),
        hdma_scene_bg1(100, u32::from(right) + 2)
    );
}

#[test]
fn the_pixel_sources_of_a_raster_effect_describe_its_picture() {
    let mut emu = hdma_scene_at_boundary();
    let state = emu.state_hash();
    let shown = picture(&emu);
    let found = emu
        .pixel_sources(planes::MAIN | planes::SUB_SCREEN)
        .expect("a game is loaded");
    assert!(found.matches_picture);
    assert_eq!(found.frame, 3, "the number a snapshot gives the picture");
    let sources = &found;
    assert!(sources.from_line_states);
    assert_eq!((sources.width, sources.height), (256, 224));
    let layer = sources.layer.as_ref().unwrap();
    let cell = sources.cell.as_ref().unwrap();
    let at = |x: u32, y: u32| (y * 256 + x) as usize;
    // Line 100 (row 99): window 1 clips BG1 inside it, where the backdrop
    // shows; outside it, BG1 from the map column the line's scroll reads.
    let (left, right) = hdma_window(100);
    assert_eq!(layer[at(u32::from(left) + 1, 99)] & layer::WHAT_DREW, 0);
    let x = u32::from(right) + 2;
    assert_eq!(layer[at(x, 99)] & layer::WHAT_DREW, 1);
    let screen_x = (x + u32::from(hdma_scroll(100))) & 0xFF;
    assert_eq!(
        u32::from(cell[at(x, 99)]),
        0x0400 + (100 / 8) * 32 + screen_x / 8,
        "the map entry of line 100's own scroll"
    );
    assert_eq!(emu.state_hash(), state);
    assert_eq!(picture(&emu), shown);

    // The window masks of each line are the window HDMA wrote for it.
    let masks = emu.window_masks().expect("a game is loaded");
    assert_eq!(masks.frame, 3);
    assert!(masks.from_line_states);
    for (row, &line) in masks.lines.iter().enumerate() {
        let (left, right) = hdma_window(line);
        let bg1 = &masks.masks[row * 256..row * 256 + 32];
        for x in 0..256usize {
            let inside = x >= usize::from(left) && x <= usize::from(right);
            assert_eq!(
                bg1[x >> 3] & (1 << (x & 7)) != 0,
                inside,
                "line {line} dot {x}"
            );
        }
    }

    // With no table kept, the frame is read from the registers as they
    // stand: the raster effect is lost, and the pass says so.
    emu.set_capture_recording(CaptureRecording::default());
    let flat = emu.pixel_sources(planes::LAYER).unwrap();
    assert!(!flat.from_line_states);
    assert!(!flat.matches_picture);
    assert!(!emu.window_masks().unwrap().from_line_states);
    assert_eq!(emu.state_hash(), state);
}

#[test]
fn a_layer_render_keeps_the_raster_effect() {
    let mut emu = hdma_scene_at_boundary();
    assert!(emu.draws_from_line_states());
    let layer = emu.render_layer(0).expect("BG1").image;
    assert_eq!((layer.width, layer.height), (256, 224));
    for y in 0..224 {
        for x in [0u32, 7, 100, 255] {
            assert_eq!(
                pixel(&layer.rgba, 256, x, y),
                hdma_scene_bg1(y as u16 + 1, x),
                "row {y} column {x}"
            );
        }
    }
    // The same render from the registers as they stand draws every row
    // with the scroll the last HDMA line wrote, the one for line 225.
    assert_eq!(emu.ppu().bg_hoffset(0), hdma_scroll(225));
    emu.set_capture_recording(CaptureRecording::default());
    let flat = emu.render_layer(0).expect("BG1").image;
    assert_ne!(flat.rgba, layer.rgba);
    for y in [0u32, 100, 223] {
        assert_eq!(
            pixel(&flat.rgba, 256, 30, y),
            hdma_scene_bg1(225, 30),
            "row {y}"
        );
    }
}

#[test]
fn a_register_written_by_the_debugger_draws_from_the_registers() {
    let mut emu = hdma_scene_at_boundary();
    let shown = picture(&emu);
    emu.write_memory_debug(0x0010, &[0x55]); // WRAM: not the PPU
    assert!(emu.draws_from_line_states());
    // BG1's scroll set to 0 by hand: a repaint shows it on every line.
    emu.write_memory_debug(0x210D, &[0x00, 0x00]);
    assert!(!emu.draws_from_line_states());
    emu.repaint_held_frame();
    let repainted = picture(&emu);
    assert_ne!(repainted, shown);
    let (width, _, image) = &repainted;
    for y in [10u32, 150] {
        let (left, right) = hdma_window(224);
        let x = if left > 40 { 5 } else { u32::from(right) + 5 };
        assert_eq!(
            pixel(image, *width, x, y),
            rgba(bg_colour(((x / 8) % 15 + 1) as u16)),
            "row {y}"
        );
    }
    // The table still describes the frame as it was drawn.
    assert_eq!(redrawn(&mut emu), shown);
    // The next frame's line 1 starts a table of its own.
    emu.run_to_capture_boundary();
    assert!(emu.draws_from_line_states());
}

#[test]
fn a_layer_hidden_with_the_toggle_stays_hidden_in_a_redraw_from_the_table() {
    let mut emu = hdma_scene_at_boundary();
    emu.video_debugger().toggle_layer(0, false);
    assert!(emu.draws_from_line_states());
    emu.repaint_held_frame();
    let (width, height, image) = picture(&emu);
    for y in 0..height {
        for x in [0u32, 128, 255] {
            assert_eq!(
                pixel(&image, width, x, y),
                rgba(hdma_backdrop(y as u16 + 1)),
                "row {y} column {x}: only the backdrop, the line's own"
            );
        }
    }
}

// -------------------------------------------------------------
// Video memory changed in the middle of the frame
// -------------------------------------------------------------

/// Tile 1's first row, plane 0 and plane 1, as the program loads it.
const TILE_ROW: [u8; 2] = [0xFF, 0x00];
/// Colour 1 as the program loads it.
const COLOUR_ONE: u16 = 0x001F;
/// Sprite 1's X as the program loads it, and its Y: its eight lines span
/// line 100, so it is drawn at both places.
const SPRITE_X: u8 = 40;
const SPRITE_Y: u8 = 96;

/// Mode 1 with BG1 all tile 1 (colour 1) and sprites on, sprite 0 near the
/// top and sprite 1 across line 100. An IRQ at line 100 turns forced blank on,
/// writes tile 1's first row, colour 1 and sprite 1's X, and turns it off
/// again; the NMI handler writes them back. Every frame is drawn with the
/// first values above line 100 and the second below it.
fn mid_frame_memory_rom() -> Vec<u8> {
    const TILES: usize = 0x1_0000;
    const MAP: usize = 0x1_4000;
    const PALETTE: usize = 0x1_6000;
    const OAM: usize = 0x1_7000;

    let mut code = vec![0x78]; // SEI
    store(&mut code, 0x80, 0x2100);
    store(&mut code, 0x01, 0x2105); // Mode 1
    store(&mut code, 0x04, 0x2107); // BG1 tilemap at word $0400
    store_zero(&mut code, 0x210B);
    store(&mut code, 0x02, 0x2101); // sprite tiles at word $4000, 8x8
    store(&mut code, 0x11, 0x212C); // BG1 and sprites on the main screen
    vram_address(&mut code, 0x0000);
    dma(&mut code, 0x01, 0x18, lorom(TILES), 16 * 32);
    vram_address(&mut code, 0x4000);
    dma(&mut code, 0x01, 0x18, lorom(TILES) + 2 * 32, 32); // sprite tile 0: colour 2
    vram_address(&mut code, 0x0400);
    dma(&mut code, 0x01, 0x18, lorom(MAP), 32 * 32 * 2);
    store_zero(&mut code, 0x2121);
    dma(&mut code, 0x00, 0x22, lorom(PALETTE), 512);
    store_zero(&mut code, 0x2102);
    store_zero(&mut code, 0x2103);
    dma(&mut code, 0x00, 0x04, lorom(OAM), 544);
    store(&mut code, 100, 0x4209); // V-IRQ on line 100
    store_zero(&mut code, 0x420A);
    store(&mut code, 0xA0, 0x4200); // NMI and the V-IRQ on
    code.push(0x58); // CLI
    store(&mut code, 0x0F, 0x2100);
    code.extend_from_slice(&[0x80, 0xFE]); // BRA *

    let mut irq = vec![0x48]; // PHA
    store(&mut irq, 0x80, 0x2100);
    vram_address(&mut irq, 0x0010); // tile 1, first row
    store(&mut irq, 0x55, 0x2118);
    store(&mut irq, 0xAA, 0x2119);
    store(&mut irq, 0x01, 0x2121);
    store(&mut irq, 0xFF, 0x2122);
    store(&mut irq, 0x7F, 0x2122); // colour 1 white
    store(&mut irq, 0x02, 0x2102); // sprite 1
    store_zero(&mut irq, 0x2103);
    store(&mut irq, 0xC0, 0x2104);
    store(&mut irq, SPRITE_Y, 0x2104);
    store(&mut irq, 0x0F, 0x2100);
    irq.extend_from_slice(&[0xAD, 0x11, 0x42]); // LDA $4211
    irq.extend_from_slice(&[0x68, 0x40]); // PLA / RTI

    let mut nmi = vec![0x48];
    vram_address(&mut nmi, 0x0010);
    store(&mut nmi, TILE_ROW[0], 0x2118);
    store(&mut nmi, TILE_ROW[1], 0x2119);
    store(&mut nmi, 0x01, 0x2121);
    store(&mut nmi, COLOUR_ONE as u8, 0x2122);
    store(&mut nmi, (COLOUR_ONE >> 8) as u8, 0x2122);
    store(&mut nmi, 0x02, 0x2102);
    store_zero(&mut nmi, 0x2103);
    store(&mut nmi, SPRITE_X, 0x2104);
    store(&mut nmi, SPRITE_Y, 0x2104);
    nmi.extend_from_slice(&[0x68, 0x40]);

    let map: Vec<u8> = (0..32 * 32).flat_map(|_| 1u16.to_le_bytes()).collect();
    let mut palette: Vec<u8> = (0..256u16)
        .flat_map(|c| (c.wrapping_mul(0x0421).wrapping_add(0x0842) & 0x7FFF).to_le_bytes())
        .collect();
    palette[2..4].copy_from_slice(&COLOUR_ONE.to_le_bytes());
    let mut oam = vec![0u8; 544];
    for sprite in 0..128 {
        oam[sprite * 4 + 1] = 0xF0; // off the screen
    }
    oam[0..4].copy_from_slice(&[60, 20, 0, 0x30]); // sprite 0: X, Y, tile, priority
    oam[4..8].copy_from_slice(&[SPRITE_X, SPRITE_Y, 0, 0x32]);
    build_rom(
        0x2_0000,
        &code,
        &irq,
        &nmi,
        &[
            (TILES, sixteen_tiles()),
            (MAP, map),
            (PALETTE, palette),
            (OAM, oam),
        ],
    )
}

fn mid_frame_memory(recording: CaptureRecording) -> SnesEmulator {
    let mut emu = SnesEmulator::new();
    emu.set_capture_recording(recording);
    emu.load_rom(&mid_frame_memory_rom())
        .expect("the program loads");
    emu
}

#[test]
fn memory_a_line_saw_is_put_back() {
    let mut emu = mid_frame_memory(line_states_on());
    emu.run_frame();
    emu.run_frame();
    emu.run_to_capture_boundary();
    let shown = picture(&emu);
    let (width, _, image) = &shown;
    assert_ne!(
        pixel(image, *width, 0, 40),
        pixel(image, *width, 0, 140),
        "colour 1 changed between the two halves"
    );
    let table = emu.line_states().unwrap();
    assert!(table.memory_exact);
    let memory = video_memory(&emu);
    let state = emu.state_hash();

    assert_eq!(redrawn(&mut emu), shown);
    assert_eq!(video_memory(&emu), memory, "the memory as it was");
    assert_eq!(emu.state_hash(), state);
    assert_eq!(picture(&emu), shown, "the framebuffer is left alone");

    // A repaint draws from the table too, onto the picture as it was.
    emu.repaint_held_frame();
    assert_eq!(picture(&emu), shown);
    assert_eq!(video_memory(&emu), memory);
    assert_eq!(emu.state_hash(), state);
}

#[test]
fn each_line_lists_its_sprites_as_the_sprite_table_stood_for_it() {
    let mut emu = mid_frame_memory(line_states_on());
    emu.run_frame();
    emu.run_frame();
    emu.run_to_capture_boundary();
    let sprites = emu.line_sprites().expect("a game is loaded");
    assert!(sprites.from_line_states);
    let sources = emu.pixel_sources(planes::MAIN).unwrap();
    assert!(sources.matches_picture);
    let cell = sources.cell.as_ref().unwrap();
    let layer = sources.layer.as_ref().unwrap();
    // Sprite 1 spans lines 97-104; the IRQ at line 100 moves it to x 192.
    let mut places = Vec::new();
    for line in 97..=104u16 {
        let row = sprites
            .lines
            .chunks(4)
            .find(|row| row[0] == line)
            .expect("the line is listed");
        let (first, count) = (usize::from(row[1]), usize::from(row[2]));
        let entry = (first..first + count)
            .find(|&i| sprites.slots[i] == 1)
            .expect("sprite 1 is on the line");
        let x = sprites.x[entry];
        places.push(x);
        // Where the line drew, the sprite's pixels name it.
        let at = (u32::from(line - 1) * 256 + x as u32) as usize;
        if layer[at] & layer::WHAT_DREW != layer::NOTHING {
            assert_eq!(cell[at], 1, "line {line}: sprite 1 at x {x}");
        }
    }
    assert_eq!(places.first(), Some(&i16::from(SPRITE_X)));
    assert_eq!(places.last(), Some(&0xC0));
}

#[test]
fn vertical_blank_uploads_after_a_whole_frame_are_undone() {
    let mut emu = mid_frame_memory(line_states_on());
    for _ in 0..3 {
        emu.run_frame();
    }
    // The frame ran whole: its vertical blank wrote the first values back,
    // while the picture shows the second ones below line 100.
    let shown = picture(&emu);
    let memory = video_memory(&emu);
    assert_eq!(memory.1[1], COLOUR_ONE);
    assert_eq!(redrawn(&mut emu), shown);
    emu.repaint_held_frame();
    assert_eq!(picture(&emu), shown);
    assert_eq!(video_memory(&emu), memory);

    // Without the table a repaint draws every line with the memory as it
    // stands, the first values throughout.
    emu.set_capture_recording(CaptureRecording::default());
    emu.repaint_held_frame();
    assert_ne!(picture(&emu), shown);
}

#[test]
fn a_repaint_and_a_layer_render_leave_the_state_hash_where_it_was() {
    let mut emu = mid_frame_memory(everything_on());
    emu.run_frame();
    emu.run_frame();
    for at_boundary in [false, true] {
        if at_boundary {
            emu.run_to_capture_boundary();
        }
        assert!(emu.draws_from_line_states());
        let state = emu.state_hash();
        let shown = picture(&emu);
        emu.repaint_held_frame();
        for layer in 0..5 {
            emu.render_layer(layer).expect("a layer");
        }
        emu.picture_from_line_states().expect("the picture");
        assert_eq!(emu.state_hash(), state, "at the boundary: {at_boundary}");
        assert_eq!(picture(&emu), shown);
    }
}

#[test]
fn recording_changes_nothing_a_savestate_holds() {
    for rom in [busy_bus_rom(), mid_frame_memory_rom(), hdma_scene_rom()] {
        let mut plain = SnesEmulator::new();
        let mut recording = SnesEmulator::new();
        recording.set_capture_recording(everything_on());
        plain.load_rom(&rom).expect("the program loads");
        recording.load_rom(&rom).expect("the program loads");
        for frame in 1..=60 {
            for emu in [&mut plain, &mut recording] {
                if frame % 3 == 0 {
                    emu.run_to_capture_boundary();
                } else {
                    emu.run_frame();
                }
            }
            assert_eq!(plain.state_hash(), recording.state_hash(), "frame {frame}");
            assert_eq!(
                plain.get_framebuffer().data,
                recording.get_framebuffer().data,
                "frame {frame}"
            );
            if frame % 10 == 0 {
                assert_eq!(plain.save_state(), recording.save_state(), "frame {frame}");
            }
        }
        assert!(recording.line_states().is_some());
    }
}

// -------------------------------------------------------------
// When the table is not used
// -------------------------------------------------------------

#[test]
fn a_table_armed_mid_frame_is_not_used() {
    let mut emu = mid_frame_memory(CaptureRecording::default());
    emu.run_frame();
    let line_100 = emu.debugger().add_breakpoint(Breakpoint {
        address: 0,
        bp_type: BreakpointType::Event {
            code: snes_events::SCANLINE,
            param: 100,
        },
        cpu_id: None,
        condition: None,
    });
    emu.run_frame();
    assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::Paused);
    emu.set_capture_recording(line_states_on());
    assert!(emu.line_states().is_none(), "no line drawn since");
    emu.debugger().enable_breakpoint(line_100, false);
    emu.set_execution_mode(ExecutionMode::Running);
    emu.run_to_capture_boundary();
    assert!(emu.at_capture_boundary(), "the frame's picture is finished");
    let table = emu.line_states().unwrap();
    assert!(!table.from_first_line);
    assert!(!table.complete, "its first hundred lines are not in it");
    assert_eq!(column(&table, "line")[0], 101);
    assert!(!emu.draws_from_line_states());
    assert!(emu.picture_from_line_states().is_none());

    emu.run_to_capture_boundary();
    let table = emu.line_states().unwrap();
    assert!(
        table.from_first_line && table.complete,
        "the next frame is whole"
    );
    assert!(emu.draws_from_line_states());
}

#[test]
fn a_load_or_a_reset_drops_the_table() {
    let mut emu = mid_frame_memory(line_states_on());
    emu.run_frame();
    let state = emu.save_state();
    emu.run_to_capture_boundary();
    assert!(emu.line_states().is_some());

    emu.load_state(&state, RomCheck::SameRom)
        .expect("its own state loads");
    assert!(emu.line_states().is_none());
    assert_eq!(emu.capture_recording(), line_states_on());
    emu.run_to_capture_boundary();
    let table = emu.line_states().unwrap();
    assert!(table.from_first_line);
    assert_eq!(table.frame, 2);

    emu.reset();
    assert!(emu.line_states().is_none());
    assert_eq!(emu.capture_recording(), line_states_on());
    emu.run_to_capture_boundary();
    assert_eq!(emu.line_states().unwrap().frame, 1);
}

#[test]
fn load_rom_keeps_the_recording_switch() {
    let mut emu = SnesEmulator::new();
    let recording = CaptureRecording {
        line_states: true,
        frame_writes: true,
        ..Default::default()
    };
    emu.set_capture_recording(recording);
    for _ in 0..2 {
        emu.load_rom(&mid_frame_memory_rom())
            .expect("the program loads");
        assert_eq!(emu.capture_recording(), recording);
        assert_eq!(emu.ppu().capture_recording(), recording);
        emu.run_to_capture_boundary();
        assert!(emu.line_states().unwrap().from_first_line);
    }
    emu.set_capture_recording(CaptureRecording::default());
    assert!(emu.line_states().is_none());
    assert!(!emu.draws_from_line_states());
}

#[test]
fn a_debugger_edit_of_video_memory_shows_though_no_line_read_it() {
    type Edit = fn(&mut SnesEmulator);
    let edits: [(&str, Edit); 3] = [
        // Planes 0 and 1 of row 4 of tile 1, the tile BG1 shows everywhere.
        ("VRAM", |emu| emu.write_vram_debug(0x0014, &[0x00, 0x00])),
        // The colour sprite 0 is drawn with.
        ("the palette", |emu| emu.write_cgram_debug(130, &[0x7C00])),
        // Sprite 0's X.
        ("the sprite table", |emu| emu.write_oam_debug(0, &[160])),
    ];
    for (memory, edit) in edits {
        let mut emu = mid_frame_memory(line_states_on());
        emu.run_frame();
        emu.run_to_capture_boundary();
        assert!(emu.line_states().unwrap().memory_exact, "{memory}");
        let shown = picture(&emu);

        edit(&mut emu);
        let table = emu.line_states().unwrap();
        assert!(table.from_first_line && table.complete, "{memory}");
        assert!(!table.memory_exact, "{memory}: no line read the edit");
        assert!(emu.draws_from_line_states(), "{memory}");
        emu.repaint_held_frame();
        assert_ne!(picture(&emu), shown, "{memory}: a redraw shows the edit");

        // The next frame's lines read it: exact again from its first line.
        emu.run_to_capture_boundary();
        let table = emu.line_states().unwrap();
        assert!(table.from_first_line && table.memory_exact, "{memory}");
    }
}

/// Whether the layer render, the pixel sources and the line sprites of the
/// held frame say they read it from the line table.
fn readers_from_line_states(emu: &mut SnesEmulator) -> [bool; 3] {
    [
        emu.render_layer(0).expect("BG1").from_line_states,
        emu.pixel_sources(planes::MAIN)
            .expect("a game is loaded")
            .from_line_states,
        emu.line_sprites()
            .expect("a game is loaded")
            .from_line_states,
    ]
}

#[test]
fn the_picture_is_drawn_from_the_line_states_only_from_a_whole_frame() {
    let mut emu = mid_frame_memory(line_states_on());
    emu.run_frame();
    emu.run_to_capture_boundary();
    assert!(emu.picture_from_line_states().is_some(), "a whole frame");
    assert_eq!(readers_from_line_states(&mut emu), [true; 3]);

    // Stopped at line 120: the table holds the frame's first lines only.
    emu.run_frame();
    let line_120 = emu.debugger().add_breakpoint(Breakpoint {
        address: 0,
        bp_type: BreakpointType::Event {
            code: snes_events::SCANLINE,
            param: 120,
        },
        cpu_id: None,
        condition: None,
    });
    emu.run_frame();
    assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::Paused);
    let table = emu.line_states().unwrap();
    assert!(table.from_first_line && !table.complete);
    assert!(emu.picture_from_line_states().is_none(), "not finished");
    assert_eq!(readers_from_line_states(&mut emu), [false; 3]);

    // Armed there, the table holds the frame from line 121 to its picture.
    emu.set_capture_recording(CaptureRecording::default());
    emu.set_capture_recording(line_states_on());
    emu.debugger().enable_breakpoint(line_120, false);
    emu.set_execution_mode(ExecutionMode::Running);
    emu.run_to_capture_boundary();
    let table = emu.line_states().unwrap();
    assert!(!table.from_first_line && !table.complete);
    assert!(emu.picture_from_line_states().is_none(), "lines missing");
    assert_eq!(readers_from_line_states(&mut emu), [false; 3]);
}

#[test]
fn the_lines_keep_their_numbers_and_the_first_row_says_where_the_picture_starts() {
    let mut emu = hdma_scene_at_boundary();
    let table = emu.line_states().unwrap();
    assert_eq!(column(&table, "line")[0], 1, "the first line drawn is 1");
    assert_eq!(table.first_row_line, 1);

    // With overscan the frame draws 239 lines, and the picture shows the
    // 224 from line 8 on.
    emu.write_memory_debug(0x2133, &[0x04]);
    emu.run_to_capture_boundary();
    let table = emu.line_states().unwrap();
    assert!(table.from_first_line && table.complete);
    assert_eq!(column(&table, "line"), (1..=239).collect::<Vec<u32>>());
    assert_eq!(table.first_row_line, 8);
    let shown = picture(&emu);
    let (width, height, image) = &shown;
    assert_eq!(*height, 224);
    // Inside window 1 BG1 is masked: the row shows its line's backdrop.
    let (left, _) = hdma_window(8);
    assert_eq!(
        pixel(image, *width, u32::from(left) + 1, 0),
        rgba(hdma_backdrop(8)),
        "the picture's first row is line 8"
    );
    assert_eq!(redrawn(&mut emu), shown);
}

// -------------------------------------------------------------
// A scene driven line by line from outside the emulator
// -------------------------------------------------------------

fn ppu_addr(reg: u16) -> u32 {
    u32::from(reg)
}

fn write_word(bus: &mut SnesBus, reg: u16, value: u16) {
    bus.write(ppu_addr(reg), value as u8);
    bus.write(ppu_addr(reg), (value >> 8) as u8);
}

fn write_vram_word(bus: &mut SnesBus, word: u16, value: u16) {
    bus.write(ppu_addr(0x2115), 0x80);
    bus.write(ppu_addr(0x2116), word as u8);
    bus.write(ppu_addr(0x2117), (word >> 8) as u8);
    bus.write(ppu_addr(0x2118), value as u8);
    bus.write(ppu_addr(0x2119), (value >> 8) as u8);
}

fn write_colour(bus: &mut SnesBus, index: u8, colour: u16) {
    bus.write(ppu_addr(0x2121), index);
    bus.write(ppu_addr(0x2122), colour as u8);
    bus.write(ppu_addr(0x2122), (colour >> 8) as u8 & 0x7F);
}

#[test]
fn a_frame_drawn_again_from_its_line_states_is_the_frame() {
    let mut bus = SnesBus::new();
    bus.ppu_mut().set_capture_recording(line_states_on());
    // The vertical blank: the memory ports are open.
    bus.set_raster_snapshot(0, 225);
    let tiles = sixteen_tiles();
    for (word, pair) in tiles.chunks(2).enumerate() {
        write_vram_word(
            &mut bus,
            word as u16,
            u16::from_le_bytes([pair[0], pair[1]]),
        );
    }
    let map = column_tilemap();
    for (cell, pair) in map.chunks(2).enumerate() {
        write_vram_word(
            &mut bus,
            0x0400 + cell as u16,
            u16::from_le_bytes([pair[0], pair[1]]),
        );
    }
    // BG2 shows the same characters through a tilemap of tile 3 at $0800.
    for cell in 0..1024u16 {
        write_vram_word(&mut bus, 0x0800 + cell, 3);
    }
    // Sprite tile 0 (colour 5) at word $4000, sprite 0 on lines 20-27.
    for (word, pair) in solid_tile(5).chunks(2).enumerate() {
        write_vram_word(
            &mut bus,
            0x4000 + word as u16,
            u16::from_le_bytes([pair[0], pair[1]]),
        );
    }
    for t in 0..16u8 {
        write_colour(&mut bus, t, bg_colour(u16::from(t)));
        write_colour(&mut bus, 128 + t, bg_colour(15 - u16::from(t)));
    }
    bus.write(ppu_addr(0x2102), 0x00);
    bus.write(ppu_addr(0x2103), 0x00);
    for sprite in 0..128u8 {
        let (x, y) = if sprite == 0 { (90, 20) } else { (0, 0xF0) };
        for byte in [x, y, 0, 0x30] {
            bus.write(ppu_addr(0x2104), byte);
        }
    }
    for _ in 0..32 {
        bus.write(ppu_addr(0x2104), 0);
    }
    bus.write(ppu_addr(0x2101), 0x02);
    bus.write(ppu_addr(0x2105), 0x01);
    bus.write(ppu_addr(0x2107), 0x04);
    bus.write(ppu_addr(0x2108), 0x08);
    bus.write(ppu_addr(0x210B), 0x00);
    bus.write(ppu_addr(0x212C), 0x13);
    bus.write(ppu_addr(0x2123), 0x02); // window 1 on BG1
    bus.write(ppu_addr(0x212E), 0x01);
    bus.write(ppu_addr(0x2100), 0x0F);

    let mut fb = Framebuffer::new(512, 480);
    for line in 1..=224u16 {
        // What changes before this line is drawn, at the end of the line
        // before (dot 300, past the colour fetcher's dots).
        bus.set_raster_snapshot(300, line - 1);
        write_word(&mut bus, 0x210D, line * 3);
        write_colour(&mut bus, 0, (line * 37) & 0x7FFF);
        if line % 16 == 1 {
            bus.write(ppu_addr(0x2126), (line / 2) as u8);
            bus.write(ppu_addr(0x2127), (line / 2 + 60) as u8);
        }
        if line == 60 {
            // A tile rewritten in forced blank between two lines.
            bus.write(ppu_addr(0x2100), 0x80);
            write_vram_word(&mut bus, 0x0010, 0x0F0F);
            bus.write(ppu_addr(0x2100), 0x0F);
        }
        if line == 90 {
            // Sprite 0 moved while the frame is drawn, through the
            // evaluator's address.
            bus.write(ppu_addr(0x2102), 0x00);
            bus.write(ppu_addr(0x2104), 0x30);
            bus.write(ppu_addr(0x2104), 100);
        }
        if line >= 120 {
            bus.write(ppu_addr(0x2105), 0x07); // Mode 7, a matrix per line
            write_word(&mut bus, 0x211B, 0x0100 + line);
            write_word(&mut bus, 0x211C, line.wrapping_mul(3));
            write_word(&mut bus, 0x211D, 0xFF00u16.wrapping_add(line));
            write_word(&mut bus, 0x211E, 0x0100);
        }
        if line == 160 {
            bus.write(ppu_addr(0x212C), 0x11);
            bus.write(ppu_addr(0x212D), 0x02);
            bus.write(ppu_addr(0x2130), 0x02);
            bus.write(ppu_addr(0x2131), 0x61);
        }
        if line == 180 {
            bus.write(ppu_addr(0x2106), 0x31); // mosaic of 4 on BG1
        }
        if line == 200 {
            bus.write(ppu_addr(0x2100), 0x08); // half brightness
        }
        bus.ppu_mut().render_scanline(line, &mut fb);
    }
    bus.ppu_mut().note_picture_finished();
    let shown = crop(&fb, 256, 224);

    let state = bincode::serialize(&bus.ppu().to_savestate()).unwrap();
    let image = bus
        .ppu_mut()
        .picture_from_line_states()
        .expect("the table holds the frame");
    assert_eq!((image.width, image.height), (256, 224));
    assert_eq!(image.rgba, shown);
    assert_eq!(
        bincode::serialize(&bus.ppu().to_savestate()).unwrap(),
        state,
        "the PPU is left as it was"
    );

    // From the registers as they stand the frame is another picture.
    bus.ppu_mut()
        .set_capture_recording(CaptureRecording::default());
    let mut again = Framebuffer::new(512, 480);
    for line in 1..=224u16 {
        bus.ppu_mut().render_scanline(line, &mut again);
    }
    assert_ne!(crop(&again, 256, 224), shown);
}

#[test]
fn the_render_order_names_the_sprites_that_drew_with_the_priority_their_first_line_read() {
    let mut bus = SnesBus::new();
    bus.ppu_mut().set_capture_recording(line_states_on());
    // The vertical blank: the memory ports are open.
    bus.set_raster_snapshot(0, 225);
    // Sprite tile 0 (colour 5) at word $4000; tile 1 is left empty, every
    // pixel transparent.
    for (word, pair) in solid_tile(5).chunks(2).enumerate() {
        write_vram_word(
            &mut bus,
            0x4000 + word as u16,
            u16::from_le_bytes([pair[0], pair[1]]),
        );
    }
    // Sprite 0 on lines 21-28 with priority 1; sprite 1, of the empty tile,
    // on the same lines with priority 2; sprite 2 on lines 101-108, where
    // no line shows sprites; sprite 3 on lines 61-68 with priority 2; the
    // others below the picture.
    bus.write(ppu_addr(0x2102), 0x00);
    bus.write(ppu_addr(0x2103), 0x00);
    for sprite in 0..128u8 {
        let (x, y, tile, attributes) = match sprite {
            0 => (10, 20, 0, 0x10),
            1 => (30, 20, 1, 0x20),
            2 => (50, 100, 0, 0x30),
            3 => (70, 60, 0, 0x20),
            _ => (0, 0xF0, 0, 0),
        };
        for byte in [x, y, tile, attributes] {
            bus.write(ppu_addr(0x2104), byte);
        }
    }
    for _ in 0..32 {
        bus.write(ppu_addr(0x2104), 0);
    }
    bus.write(ppu_addr(0x2101), 0x02);
    bus.write(ppu_addr(0x212C), 0x10); // sprites on the main screen
    bus.write(ppu_addr(0x2100), 0x0F);

    let mut fb = Framebuffer::new(512, 480);
    for line in 1..=224u16 {
        bus.set_raster_snapshot(300, line - 1);
        if line == 25 {
            // Sprite 0's priority rewritten in forced blank between two of
            // its lines: 3 from line 25 on.
            bus.write(ppu_addr(0x2100), 0x80);
            bus.write(ppu_addr(0x2102), 0x01);
            bus.write(ppu_addr(0x2103), 0x00);
            bus.write(ppu_addr(0x2104), 0x00);
            bus.write(ppu_addr(0x2104), 0x30);
            bus.write(ppu_addr(0x2100), 0x0F);
        }
        if line == 90 {
            bus.write(ppu_addr(0x212C), 0x00); // sprites on neither screen
        }
        if line == 121 {
            bus.write(ppu_addr(0x212C), 0x10);
        }
        bus.ppu_mut().render_scanline(line, &mut fb);
    }
    bus.ppu_mut().note_picture_finished();

    let entry = |index: u32, priority: u32| SpriteOrderEntry { index, priority };
    // Back to front, slot 0 in front: what drew, the empty sprite included
    // (it drew its columns), each with the priority its first line read.
    assert_eq!(
        bus.ppu_mut().resolved_sprite_render_order(),
        vec![entry(3, 2), entry(1, 2), entry(0, 1)]
    );

    // From the registers as they stand every line shows sprites and
    // sprite 0 has priority 3.
    bus.ppu_mut()
        .set_capture_recording(CaptureRecording::default());
    assert_eq!(
        bus.ppu_mut().resolved_sprite_render_order(),
        vec![entry(3, 2), entry(2, 3), entry(1, 2), entry(0, 3)]
    );
}

// -------------------------------------------------------------
// Every register, written down the frame
// -------------------------------------------------------------

/// A program that fills VRAM, CGRAM and OAM with noise in forced blank and
/// then has eight HDMA channels, unit 4, write noise to four registers each
/// on every line: the registers starting at `bases`. Overscan is left off
/// (a frame that turns it on after its last line draws past its picture).
fn noise_rom(bases: [u8; 8], seed: u64) -> Vec<u8> {
    const VRAM_LOW: usize = 0x1_0000;
    const VRAM_HIGH: usize = 0x1_8000;
    const PALETTE: usize = 0x2_0000;
    const OAM: usize = 0x2_1000;
    const TABLES: usize = 0x3_0000;

    let mut numbers = Numbers(seed);
    let mut code = vec![0x78];
    store(&mut code, 0x80, 0x2100);
    vram_address(&mut code, 0x0000);
    dma(&mut code, 0x01, 0x18, lorom(VRAM_LOW), 0x8000);
    vram_address(&mut code, 0x4000);
    dma(&mut code, 0x01, 0x18, lorom(VRAM_HIGH), 0x8000);
    store_zero(&mut code, 0x2121);
    dma(&mut code, 0x00, 0x22, lorom(PALETTE), 512);
    store_zero(&mut code, 0x2102);
    store_zero(&mut code, 0x2103);
    dma(&mut code, 0x00, 0x04, lorom(OAM), 544);
    store(&mut code, 0x1F, 0x212C); // everything on the main screen
    store(&mut code, 0x0F, 0x2100);
    let mut tables = Vec::new();
    for (channel, &base) in bases.iter().enumerate() {
        let start = TABLES + tables.len();
        hdma(&mut code, channel as u16, 0x04, base, lorom(start));
        for _ in 0..240 {
            let mut unit = [0u8; 4];
            for (k, byte) in unit.iter_mut().enumerate() {
                let register = 0x2100 | (u16::from(base) + k as u16);
                *byte = match register {
                    // Display on, bright, most of the time.
                    0x2100 => {
                        let value = numbers.next();
                        if value & 0x70 == 0 {
                            value | 0x80
                        } else {
                            (value & 0x07) | 0x08
                        }
                    }
                    0x2133 => numbers.next() & !0x04,
                    _ => numbers.next(),
                };
            }
            tables.push(0x01);
            tables.extend_from_slice(&unit);
        }
        tables.push(0x00);
        while tables.len() % 0x800 != 0 {
            tables.push(0);
        }
    }
    store(&mut code, 0xFF, 0x420C);
    code.extend_from_slice(&[0x80, 0xFE]);

    build_rom(
        0x4_0000,
        &code,
        &[0x40],
        &[0x40],
        &[
            (VRAM_LOW, numbers.bytes(0x8000)),
            (VRAM_HIGH, numbers.bytes(0x8000)),
            (PALETTE, numbers.bytes(512)),
            (OAM, numbers.bytes(544)),
            (TABLES, tables),
        ],
    )
}

#[test]
fn every_register_written_down_the_frame_is_drawn_again() {
    let roms = [
        // $2100-$211F: the display, sprites, the OAM, VRAM and Mode 7
        // ports, the mode, mosaic, the maps and the scrolls.
        (
            [0x00, 0x04, 0x08, 0x0C, 0x10, 0x14, 0x18, 0x1C],
            0x5EED_0001u64,
        ),
        // $2120-$2133: Mode 7's centre, CGRAM, the windows, the screens,
        // colour math, SETINI; with the display, the mode and two scrolls
        // so the frame has something to show.
        (
            [0x20, 0x24, 0x28, 0x2C, 0x30, 0x00, 0x05, 0x0D],
            0x5EED_0002,
        ),
    ];
    for (bases, seed) in roms {
        let mut emu = SnesEmulator::new();
        emu.set_capture_recording(line_states_on());
        emu.load_rom(&noise_rom(bases, seed))
            .expect("the program loads");
        emu.run_frame();
        emu.run_frame();
        let mut raster_effect = false;
        for frame in 0..4 {
            emu.run_to_capture_boundary();
            let table = emu.line_states().unwrap();
            assert!(table.from_first_line && table.complete);
            assert!(table.memory_exact, "{bases:02x?} frame {frame}");
            let shown = picture(&emu);
            let memory = video_memory(&emu);
            let state = emu.state_hash();
            assert_eq!(redrawn(&mut emu), shown, "{bases:02x?} frame {frame}");
            // The redraw that notes where each pixel came from draws it too.
            let found = emu
                .pixel_sources(planes::MAIN | planes::SUB_SCREEN)
                .unwrap();
            assert!(found.matches_picture, "{bases:02x?} frame {frame}");
            let _ = emu.line_sprites();
            let _ = emu.window_masks();
            let _ = emu.offset_per_tile();
            let _ = emu.render_sprites(&[1; 128]);
            assert_eq!(video_memory(&emu), memory);
            assert_eq!(emu.state_hash(), state);
            let layer = emu.render_layer(0).expect("BG1").image;
            assert_eq!((layer.width, layer.height), (shown.0, shown.1));
            if frame == 3 {
                emu.set_capture_recording(CaptureRecording::default());
                emu.repaint_held_frame();
                raster_effect = picture(&emu) != shown;
            }
            emu.run_frame();
        }
        assert!(
            raster_effect,
            "the registers as they stand draw another picture"
        );
    }
}

/// A program that keeps HDMA busy and the bus full, the one of the capture
/// boundary's tests: channels 1-7 write four bytes a line to the window
/// edges, while the main loop copies 128 to 255 bytes of ROM into WRAM with
/// a general DMA on channel 0, over and over.
fn busy_bus_rom() -> Vec<u8> {
    let mut code = vec![0x78];
    store(&mut code, 0x0F, 0x2100);
    for channel in 1..8u16 {
        hdma(&mut code, channel, 0x03, 0x26, lorom(0x8000));
    }
    store(&mut code, 0xFE, 0x420C);
    store(&mut code, 0x01, 0x4200);
    let main_loop = code.len();
    store_zero(&mut code, 0x2181);
    store_zero(&mut code, 0x2182);
    store(&mut code, 0x01, 0x2183);
    store_zero(&mut code, 0x4300);
    store(&mut code, 0x80, 0x4301);
    store_zero(&mut code, 0x4302);
    store(&mut code, 0x80, 0x4303);
    store_zero(&mut code, 0x4304);
    code.extend_from_slice(&[
        0xE6, 0x20, 0xA5, 0x20, 0x29, 0x7F, 0x09, 0x80, 0x8D, 0x05, 0x43,
    ]);
    store_zero(&mut code, 0x4306);
    store(&mut code, 0x01, 0x420B);
    let back = main_loop as isize - (code.len() as isize + 2);
    code.extend_from_slice(&[0x80, back as i8 as u8]);
    let mut table = Vec::new();
    for line in 0..240u32 {
        table.extend_from_slice(&[0x01, line as u8, 0xFF, line as u8, 0xFF]);
    }
    table.push(0x00);
    build_rom(0x2_0000, &code, &[0x40], &[0x40], &[(0x8000, table)])
}
