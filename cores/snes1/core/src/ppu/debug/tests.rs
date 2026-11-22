//! Unit tests of the PPU's debugger surface: the `VideoDebugger` impl,
//! the viewer exporters and the debugger's writes into video memory.

use super::*;

/// 15-bit BGR red (R = 31) expands to opaque RGBA red.
#[test]
fn bgr15_red_expands_to_full_red() {
    assert_eq!(bgr15_to_rgba8(0x001F), [0xFF, 0x00, 0x00, 0xFF]);
    assert_eq!(rgba8_to_u32([0xFF, 0x00, 0x00, 0xFF]), 0xFF00_00FF);
}

#[test]
fn tile_data_count_and_bounds() {
    let ppu = Ppu::new();
    assert_eq!(ppu.get_tile_count(), VRAM_4BPP_TILE_COUNT);
    let tile = ppu.get_tile_data(0).expect("tile 0 in range");
    assert_eq!(tile.width, 8);
    assert_eq!(tile.height, 8);
    assert_eq!(tile.pixels.len(), 64);
    // One past the last addressable tile is out of range.
    assert!(ppu.get_tile_data(VRAM_4BPP_TILE_COUNT).is_none());
}

#[test]
fn palette_data_decodes_cgram() {
    let mut ppu = Ppu::new();
    // CGRAM colour 1 = 15-bit BGR red.
    ppu.cgram[1] = 0x001F;
    let palette = ppu.get_palette_data();
    assert_eq!(palette.len(), 16, "16 sixteen-colour palettes");
    assert_eq!(palette[0].colors.len(), 16);
    assert_eq!(palette[0].colors[1], 0xFF00_00FF, "decoded red swatch");
}

#[test]
fn sprite_data_lists_all_128() {
    let ppu = Ppu::new();
    assert_eq!(ppu.get_sprite_data().len(), 128);
}

#[test]
fn vram_view_spans_the_full_bank() {
    let ppu = Ppu::new();
    assert_eq!(ppu.get_vram().len(), 0x1_0000, "64 KiB of VRAM as bytes");
}

#[test]
fn registers_snapshot_includes_bgmode_and_mode7() {
    let ppu = Ppu::new();
    let regs = ppu.get_registers();
    assert!(regs.iter().any(|r| r.name == "BGMODE"));
    assert!(regs.iter().any(|r| r.name == "M7A"));
}

#[test]
fn layer_count_is_five() {
    let ppu = Ppu::new();
    assert_eq!(ppu.get_layer_count(), LAYER_COUNT as u32);
}

/// `toggle_layer` drives the debug-disable mask: a BG reads back
/// disabled after a hide and enabled again after a show. The reported
/// `enabled` flag tracks the debug mask ONLY, never the game's TM
/// main-screen enable, so the checkbox starts checked even for a BG the
/// game isn't currently displaying, and hiding it takes a single uncheck.
#[test]
fn toggle_layer_blanks_a_bg() {
    let mut ppu = Ppu::new();
    // bg_main_enable left false on purpose: the checkbox must still start
    // checked because nothing is debug-hidden.
    assert!(
        !ppu.bg_main_enable[0],
        "game has BG1 off on the main screen"
    );
    assert!(
        ppu.get_layer_data(0).expect("BG1 layer").enabled,
        "a BG starts visible regardless of the game's TM enable bit"
    );
    ppu.toggle_layer(0, false);
    assert!(
        !ppu.get_layer_data(0).unwrap().enabled,
        "hiding the layer clears its enabled flag"
    );
    ppu.toggle_layer(0, true);
    assert!(ppu.get_layer_data(0).unwrap().enabled, "showing it again");
}

/// The Mode 7 matrix export reinterprets the stored unsigned
/// registers as signed fixed-point.
#[test]
fn mode7_matrix_export() {
    let mut ppu = Ppu::new();
    ppu.m7a = 0x0100; // 256
    ppu.m7b = 0x0001; // 1
    ppu.m7c = 0xFFFF; // -1
    ppu.m7d = 0x0100; // 256
    ppu.m7x = 0x0010; // 16
    ppu.m7y = 0xFFF0; // -16
    assert_eq!(ppu.get_mode7_matrix(), [256, 1, -1, 256, 16, -16]);
}

#[test]
fn palette_image_dims() {
    let ppu = Ppu::new();
    // 16x16 grid of 16x16-pixel swatches = 256x256 RGBA.
    assert_eq!(ppu.export_palette_image().len(), 256 * 256 * 4);
}

#[test]
fn tile_image_dims() {
    let ppu = Ppu::new();
    let img = ppu.export_tile_image(TileSetSource::Bg, 0);
    assert_eq!((img.width, img.height), (128, 128));
    assert_eq!(img.rgba.len(), 128 * 128 * 4);
}

#[test]
fn tilemap_image_dims_default_32x32() {
    let ppu = Ppu::new();
    // Default screen size selector (0) -> 32x32 tiles -> 256x256 px.
    let img = ppu.export_tilemap_image(0);
    assert_eq!((img.width, img.height), (256, 256));
    assert_eq!(img.rgba.len(), 256 * 256 * 4);
}

// -- The tilemap still ------------------------------------------

/// VRAM and CGRAM filled with values that look random and are the same on
/// every run, so every map entry, character and colour is some value.
fn scramble(ppu: &mut Ppu, seed: u32) {
    let mut state = seed;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    for word in ppu.vram.iter_mut() {
        *word = next() as u16;
    }
    for colour in ppu.cgram.iter_mut() {
        *colour = next() as u16 & 0x7FFF;
    }
}

/// Every pixel the renderer draws of background `bg` on 224 lines, with the
/// scroll at each of `scrolls`, is the tilemap still's pixel at the same
/// place of the map, and a pixel it leaves transparent is transparent there.
fn assert_the_still_is_what_the_renderer_draws(ppu: &mut Ppu, bg: usize, scrolls: &[(u16, u16)]) {
    use crate::ppu::render::{BgRowPixel, MAX_SCANLINE_WIDTH};
    use crate::ppu::trace::FrameTrace;
    use retro_core::capture::NoTrace;
    let still = ppu.export_tilemap_image(bg);
    let hires = is_hires_mode(ppu.bgmode);
    let width: u16 = if hires { 512 } else { 256 };
    for &(hoffset, voffset) in scrolls {
        ppu.bg_hoffset[bg] = hoffset;
        ppu.bg_voffset[bg] = voffset;
        ppu.invalidate_tilemap_caches();
        let h_scroll = if hires { hoffset << 1 } else { hoffset };
        for scanline in 1..=224u16 {
            let mut row = [BgRowPixel::TRANSPARENT; MAX_SCANLINE_WIDTH];
            ppu.render_bg_row(
                bg,
                scanline,
                &mut row,
                hires,
                &mut NoTrace::<FrameTrace>::new(),
            );
            let map_y = u32::from(scanline.wrapping_add(voffset)) % still.height;
            for x in 0..width {
                let map_x = u32::from(x.wrapping_add(h_scroll)) % still.width;
                let at = ((map_y * still.width + map_x) * 4) as usize;
                let pixel = row[usize::from(x)];
                let expected = if pixel.opaque {
                    bgr15_to_rgba8(pixel.color)
                } else {
                    [0; 4]
                };
                assert_eq!(
                    still.rgba[at..at + 4],
                    expected,
                    "mode {} BG{} scroll ({hoffset}, {voffset}) line {scanline} x {x}",
                    ppu.bgmode,
                    bg + 1
                );
            }
        }
    }
}

#[test]
fn the_tilemap_still_draws_each_background_as_the_renderer_does() {
    // Mode 0: four backgrounds of 2 bits, each with 32 colours of its own;
    // every screen size.
    for bg in 0..4 {
        let mut ppu = Ppu::new();
        scramble(&mut ppu, 11 + bg as u32);
        ppu.bgmode = 0;
        ppu.bg_screen_addr[bg] = 0x2000 * bg as u32;
        ppu.bg_tiledata_addr[bg] = 0x4000;
        ppu.bg_screen_size[bg] = bg as u8;
        assert_the_still_is_what_the_renderer_draws(&mut ppu, bg, &[(0, 0), (300, 290)]);
    }
    // Mode 1: 4 bits on BG1 and BG2, 2 on BG3; 16x16 tiles on BG2.
    let mut ppu = Ppu::new();
    scramble(&mut ppu, 21);
    ppu.bgmode = 1;
    ppu.bg_screen_size[0] = 3;
    ppu.bg_tiledata_addr[1] = 0x8000;
    ppu.bg_tile_size[1] = true;
    ppu.bg_screen_size[1] = 1;
    for bg in 0..3 {
        assert_the_still_is_what_the_renderer_draws(&mut ppu, bg, &[(0, 0), (700, 500)]);
    }
    // Mode 3: BG1's 8 bits through CGRAM, then in direct colour.
    let mut ppu = Ppu::new();
    scramble(&mut ppu, 31);
    ppu.bgmode = 3;
    ppu.bg_tile_size[0] = true;
    assert_the_still_is_what_the_renderer_draws(&mut ppu, 0, &[(0, 0), (100, 200)]);
    ppu.direct_color = true;
    assert!(ppu.bg_direct_colour(0));
    assert_the_still_is_what_the_renderer_draws(&mut ppu, 0, &[(0, 0)]);
    // Mode 5: hires, its entries 16 dots wide; 16x16 tiles on BG2.
    let mut ppu = Ppu::new();
    scramble(&mut ppu, 41);
    ppu.bgmode = 5;
    ppu.bg_screen_size[0] = 1;
    ppu.bg_tile_size[1] = true;
    for bg in 0..2 {
        assert_the_still_is_what_the_renderer_draws(&mut ppu, bg, &[(0, 0), (130, 70)]);
    }
}

#[test]
fn the_tilemap_still_s_size_follows_the_tiles_and_the_hires_modes() {
    let mut ppu = Ppu::new();
    let size = |ppu: &Ppu, bg: usize| {
        let still = ppu.export_tilemap_image(bg);
        assert_eq!(still.rgba.len(), (still.width * still.height * 4) as usize);
        (still.width, still.height)
    };
    ppu.bgmode = 1;
    ppu.bg_screen_size[0] = 1;
    assert_eq!(size(&ppu, 0), (512, 256), "64 by 32 entries of 8 by 8");
    ppu.bg_tile_size[0] = true;
    assert_eq!(size(&ppu, 0), (1024, 512), "of 16 by 16");
    ppu.bgmode = 5;
    ppu.bg_tile_size[0] = false;
    assert_eq!(size(&ppu, 0), (1024, 256), "hires: 16 dots by 8");
    // A background the mode does not draw: the map's size, transparent.
    scramble(&mut ppu, 51);
    let off = ppu.export_tilemap_image(2);
    assert_eq!((off.width, off.height), (512, 256));
    assert!(off.rgba.iter().all(|&b| b == 0));
}

#[test]
fn the_tilemap_still_of_mode_7_is_its_plane() {
    let mut ppu = Ppu::new();
    ppu.bgmode = 7;
    // Map cell (1, 0) names character 2; its pixel (3, 4) is colour 0x85,
    // its pixel (4, 4) colour 0x80, and the rest of the map character 0,
    // all colour 0 but its pixel (0, 0), colour 0x05.
    ppu.vram[1] = 0x0002;
    ppu.vram[2 * 64 + 4 * 8 + 3] |= 0x8500;
    ppu.vram[2 * 64 + 4 * 8 + 4] |= 0x8000;
    ppu.vram[0] |= 0x0500;
    ppu.cgram[0x05] = 0x001F;
    ppu.cgram[0x80] = 0x03E0;
    ppu.cgram[0x85] = 0x7C00;
    let pixel = |still: &ViewerImage, x: u32, y: u32| {
        let at = ((y * still.width + x) * 4) as usize;
        [
            still.rgba[at],
            still.rgba[at + 1],
            still.rgba[at + 2],
            still.rgba[at + 3],
        ]
    };
    let plane = ppu.export_tilemap_image(0);
    assert_eq!((plane.width, plane.height), (1024, 1024));
    assert_eq!(pixel(&plane, 11, 4), bgr15_to_rgba8(0x7C00), "8 bits");
    assert_eq!(pixel(&plane, 12, 4), bgr15_to_rgba8(0x03E0));
    assert_eq!(pixel(&plane, 16, 0), bgr15_to_rgba8(0x001F), "character 0");
    assert_eq!(pixel(&plane, 17, 0), [0; 4], "colour 0 is transparent");
    ppu.direct_color = true;
    let direct = ppu.export_tilemap_image(0);
    assert_eq!(
        pixel(&direct, 11, 4),
        bgr15_to_rgba8(direct_color_word(0x85, 0))
    );
    // BG2 under EXTBG: the low 7 bits, colour 0 transparent.
    ppu.mode7_extbg = true;
    let extbg = ppu.export_tilemap_image(1);
    assert_eq!(pixel(&extbg, 11, 4), bgr15_to_rgba8(0x001F), "0x85 as 0x05");
    assert_eq!(pixel(&extbg, 12, 4), [0; 4], "0x80 as 0");
}

#[test]
fn oam_sprites_export_all_128() {
    let ppu = Ppu::new();
    assert_eq!(ppu.export_oam_sprites().len(), 128);
}

// -- decode_tiles (2 / 4 / 8 bpp) --------------------------------

fn vram_decode_req(bpp: u8, count: u32) -> TileDecodeRequest {
    TileDecodeRequest {
        source: TileSource::Vram,
        base_addr: 0,
        first_tile: 0,
        count,
        bpp,
        tile_size: TileSize::Px8,
    }
}

#[test]
fn decode_tiles_reads_no_more_characters_than_vram_holds() {
    let ppu = Ppu::new();
    let request = |source, count, bpp, tile_size| TileDecodeRequest {
        source,
        base_addr: 0,
        first_tile: 0,
        count,
        bpp,
        tile_size,
    };
    for (source, count, bpp, size) in [
        (TileSource::Vram, 4096, 2, TileSize::Px8),
        (TileSource::Vram, 2048, 4, TileSize::Px8),
        (TileSource::Vram, 1024, 8, TileSize::Px8),
        (TileSource::Vram, 1024, 4, TileSize::Px16),
        (TileSource::Mode7, 512, 8, TileSize::Px8),
    ] {
        let at_most = request(source, count, bpp, size);
        assert!(ppu.decode_tiles(&at_most).is_ok(), "{at_most:?}");
        let one_more = request(source, count + 1, bpp, size);
        assert_eq!(
            ppu.decode_tiles(&one_more),
            Err(ExportError::OutOfRange),
            "{one_more:?}"
        );
    }
    let huge = request(TileSource::Vram, u32::MAX, 4, TileSize::Px8);
    assert_eq!(ppu.decode_tiles(&huge), Err(ExportError::OutOfRange));
}

#[test]
fn decode_tiles_decodes_each_depth() {
    let mut ppu = Ppu::new();
    // Row 0 plane 0 = 0xFF (low byte of VRAM word 0) -> every column
    // resolves to palette index 1 at 2/4/8 bpp alike.
    ppu.vram[0] = 0x00FF;
    for bpp in [2u8, 4, 8] {
        let img = ppu.decode_tiles(&vram_decode_req(bpp, 1)).expect("decode");
        assert_eq!((img.width, img.height), (8, 8), "bpp {bpp} dims");
        assert_eq!(img.bpp, bpp);
        assert_eq!(img.transparent_index, Some(0));
        assert_eq!(&img.pixels[0..8], &[1u8; 8], "bpp {bpp} row 0 indices");
        assert_eq!(&img.pixels[8..16], &[0u8; 8], "bpp {bpp} row 1 untouched");
    }
}

#[test]
fn decode_tiles_stacks_count_vertically() {
    let mut ppu = Ppu::new();
    // Two 4bpp tiles (16 words each): tile 0 + tile 1 both lit on row 0.
    ppu.vram[0] = 0x00FF;
    ppu.vram[16] = 0x00FF;
    let img = ppu.decode_tiles(&vram_decode_req(4, 2)).expect("two tiles");
    assert_eq!((img.width, img.height), (8, 16));
    assert_eq!(&img.pixels[0..8], &[1u8; 8], "tile 0 row 0");
    assert_eq!(
        &img.pixels[64..72],
        &[1u8; 8],
        "tile 1 row 0 (output row 8)"
    );
}

#[test]
fn decode_tiles_rejects_bad_requests() {
    let ppu = Ppu::new();
    // count 0 -> Empty.
    assert_eq!(
        ppu.decode_tiles(&vram_decode_req(4, 0)),
        Err(ExportError::Empty)
    );
    // Unsupported bpp (only 2/4/8).
    assert_eq!(
        ppu.decode_tiles(&vram_decode_req(3, 1)),
        Err(ExportError::Unsupported)
    );
    // A start word already past the 32 KiB-word bank -> OutOfRange.
    let oob = TileDecodeRequest {
        base_addr: 0x1_0000,
        ..vram_decode_req(4, 1)
    };
    assert_eq!(ppu.decode_tiles(&oob), Err(ExportError::OutOfRange));
    // A 16x16 block is named by a ten-bit character number.
    let px16_past_numbers = TileDecodeRequest {
        tile_size: TileSize::Px16,
        first_tile: 1024,
        ..vram_decode_req(4, 1)
    };
    assert_eq!(
        ppu.decode_tiles(&px16_past_numbers),
        Err(ExportError::OutOfRange)
    );
    // Mode 7 characters are 8bpp and 8x8, and start inside VRAM.
    let mode7 = |bpp: u8, tile_size: TileSize, first_tile: u32| TileDecodeRequest {
        source: TileSource::Mode7,
        tile_size,
        first_tile,
        ..vram_decode_req(bpp, 1)
    };
    assert_eq!(
        ppu.decode_tiles(&mode7(4, TileSize::Px8, 0)),
        Err(ExportError::Unsupported)
    );
    assert_eq!(
        ppu.decode_tiles(&mode7(8, TileSize::Px16, 0)),
        Err(ExportError::Unsupported)
    );
    assert_eq!(
        ppu.decode_tiles(&mode7(8, TileSize::Px8, 512)),
        Err(ExportError::OutOfRange)
    );
}

/// Write 4bpp character `character` of the table at byte `base_byte`,
/// its pixel `(x, y)` holding `index(x, y)`.
fn write_4bpp_character(
    ppu: &mut Ppu,
    base_byte: u32,
    character: u32,
    index: impl Fn(usize, usize) -> u8,
) {
    let word_base = (base_byte >> 1) as usize + character as usize * 16;
    for y in 0..8 {
        let mut planes = [0u8; 4];
        for x in 0..8 {
            let value = index(x, y);
            for (bit, plane) in planes.iter_mut().enumerate() {
                if (value >> bit) & 1 != 0 {
                    *plane |= 0x80 >> x;
                }
            }
        }
        ppu.vram[(word_base + y) & 0x7FFF] = u16::from(planes[0]) | (u16::from(planes[1]) << 8);
        ppu.vram[(word_base + 8 + y) & 0x7FFF] = u16::from(planes[2]) | (u16::from(planes[3]) << 8);
    }
}

#[test]
fn decode_tiles_16x16_is_the_block_the_renderer_draws() {
    use retro_core::Framebuffer;
    let mut ppu = Ppu::new();
    ppu.display_disable = false;
    ppu.display_brightness = 15;
    ppu.bgmode = 1;
    ppu.bg_main_enable[0] = true;
    ppu.bg_tile_size[0] = true;
    ppu.bg_screen_addr[0] = 0;
    ppu.bg_tiledata_addr[0] = 0x4000;
    // Scanline 1 draws the tilemap's first row.
    ppu.bg_voffset[0] = 0xFFFF;
    // The first tilemap entry names block 0x0F. Its right half is
    // character 0x10, on the next row of the 16-wide table, and its
    // bottom half the characters 0x1F and 0x20.
    ppu.vram[0] = 0x000F;
    for (quadrant, character) in [0x0Fu32, 0x10, 0x1F, 0x20].into_iter().enumerate() {
        write_4bpp_character(&mut ppu, 0x4000, character, |x, y| {
            ((quadrant * 5 + x + y) % 15 + 1) as u8
        });
    }
    for index in 0..16u16 {
        ppu.cgram[usize::from(index)] = index * 0x0421 + 0x0010;
    }

    let mut fb = Framebuffer::new(256, 224);
    for scanline in 1..=16 {
        ppu.render_scanline(scanline, &mut fb);
    }
    let block = ppu
        .decode_tiles(&TileDecodeRequest {
            source: TileSource::Vram,
            base_addr: 0x4000,
            first_tile: 0x0F,
            count: 1,
            bpp: 4,
            tile_size: TileSize::Px16,
        })
        .expect("one 16x16 block");
    assert_eq!((block.width, block.height), (16, 16));
    for y in 0..16 {
        for x in 0..16 {
            let index = block.pixels[y * 16 + x];
            assert_ne!(index, 0, "every pixel of the block is drawn");
            assert_eq!(
                pixel(&fb.data, x, y),
                bgr15_to_rgba8(ppu.cgram[usize::from(index)]),
                "pixel ({x}, {y})"
            );
        }
    }
}

#[test]
fn decode_tiles_16x16_stacks_blocks_and_wraps_the_character_number() {
    let mut ppu = Ppu::new();
    // Block 0x3FF reads characters 0x3FF, 0x000, 0x00F and 0x010:
    // the character number wraps at ten bits, as the renderer's does.
    for (character, value) in [(0x3FFu32, 1u8), (0x000, 2), (0x00F, 3), (0x010, 4)] {
        write_4bpp_character(&mut ppu, 0, character, |_, _| value);
    }
    let blocks = ppu
        .decode_tiles(&TileDecodeRequest {
            source: TileSource::Vram,
            base_addr: 0,
            first_tile: 0x3FF,
            count: 2,
            bpp: 4,
            tile_size: TileSize::Px16,
        })
        .expect("two 16x16 blocks");
    assert_eq!((blocks.width, blocks.height), (16, 32));
    let at = |x: usize, y: usize| blocks.pixels[y * 16 + x];
    assert_eq!([at(0, 0), at(8, 0), at(0, 8), at(8, 8)], [1, 2, 3, 4]);
    // The second block is block 0x000 (0x3FF + 1, wrapped): its top
    // left is the character 0x000 the first block's top right is.
    assert_eq!(at(0, 16), 2);
}

#[test]
fn decode_tiles_mode7_is_the_character_the_plane_samples() {
    use crate::ppu::tile::mode7_sample;
    use retro_core::Framebuffer;
    let mut ppu = Ppu::new();
    ppu.display_disable = false;
    ppu.display_brightness = 15;
    ppu.bgmode = 7;
    ppu.bg_main_enable[0] = true;
    ppu.m7a = 0x0100;
    ppu.m7d = 0x0100;
    // Tilemap cell (0, 0), the low byte of word 0, names character 5;
    // the character's pixel (x, y) is the high byte of word
    // 5 * 64 + 8y + x. The low bytes of those words are other tilemap
    // cells, left at 0.
    ppu.vram[0] = 0x0005;
    for y in 0..8 {
        for x in 0..8 {
            let word = 5 * 64 + y * 8 + x;
            ppu.vram[word] = ((1 + x + 8 * y) as u16) << 8;
        }
    }
    for index in 0..256u16 {
        ppu.cgram[usize::from(index)] = (index * 37) & 0x7FFF;
    }

    let mut fb = Framebuffer::new(256, 224);
    for scanline in 1..8 {
        ppu.render_scanline(scanline, &mut fb);
    }
    let character = ppu
        .decode_tiles(&TileDecodeRequest {
            source: TileSource::Mode7,
            base_addr: 0,
            first_tile: 5,
            count: 1,
            bpp: 8,
            tile_size: TileSize::Px8,
        })
        .expect("one Mode 7 character");
    assert_eq!((character.width, character.height), (8, 8));
    assert_eq!(character.bpp, 8);
    // With the identity matrix scanline `s` samples world row `s` (the
    // line count starts at 1) and draws output row `s - 1`.
    for world_y in 1..8 {
        for x in 0..8 {
            let index = character.pixels[world_y * 8 + x];
            assert_eq!(index, (1 + x + 8 * world_y) as u8);
            assert_eq!(
                pixel(&fb.data, x, world_y - 1),
                bgr15_to_rgba8(ppu.cgram[usize::from(index)]),
                "pixel ({x}, {world_y})"
            );
        }
    }
    // Row 0 no line draws here; the plane's own sampler reads it.
    for x in 0..8 {
        assert_eq!(
            Some(character.pixels[x]),
            mode7_sample(ppu.vram(), x as i32, 0, 0)
        );
    }
}

#[test]
fn settings_read_the_mode_flags_and_mode7_select_from_their_own_fields() {
    use crate::ppu::PpuAccessCtx;
    let mut ppu = Ppu::new();
    let ctx = PpuAccessCtx::ZERO;
    // BGMODE: mode 1, BG3 lifted, 16x16 tiles on BG2 and BG4.
    ppu.write_reg(0x05, 0b1010_1001, &ctx);
    // M7SEL: screen over 2, both flips.
    ppu.write_reg(0x1A, 0b1000_0011, &ctx);
    let settings = ppu.settings();
    assert_eq!(settings.bgmode, 1);
    assert!(settings.bg3_priority);
    assert_eq!(settings.bg_tile_size, [false, true, false, true]);
    assert_eq!(
        (
            settings.mode7_repeat,
            settings.mode7_vflip,
            settings.mode7_hflip
        ),
        (2, true, true)
    );
    let m7sel = ppu
        .get_registers()
        .into_iter()
        .find(|register| register.name == "M7SEL")
        .expect("M7SEL is listed");
    assert_eq!(m7sel.value, 0b1000_0011);
}

#[test]
fn settings_sign_extend_the_mode7_offsets_and_centre() {
    let mut ppu = Ppu::new();
    ppu.mode7_hoffset = 0x1FFF;
    ppu.mode7_voffset = 0x0FFF;
    ppu.m7x = 0x1000;
    ppu.m7y = 0x0010;
    ppu.m7a = 0xFF00;
    let settings = ppu.settings();
    assert_eq!(settings.mode7_hoffset, -1);
    assert_eq!(settings.mode7_voffset, 0x0FFF);
    assert_eq!(settings.mode7_centre, [-0x1000, 0x10]);
    assert_eq!(settings.mode7_matrix[0], -0x100);
}

// -- sprite list + render order ----------------------------------

#[test]
fn get_sprite_data_normalizes_obj_x_to_signed_screen_space() {
    let mut ppu = Ppu::new();
    // Sprite 0: X = 500 (x_lo = 244, high-table X bit 8 set) -> the
    // 9-bit value wraps to screen -12 (partially off the left).
    ppu.oam[0] = 244;
    ppu.oam[1] = 30; // Y on a line drawn, kept
    ppu.oam[0x200] |= 0x01; // sprite 0 high bits: X bit 8 = 1
                            // Sprite 1: X = 100, no high bit -> unchanged.
    ppu.oam[4] = 100;
    ppu.oam[5] = 40;

    let sprites = ppu.get_sprite_data();
    assert_eq!(sprites[0].x, -12, "9-bit X 500 normalizes to -12");
    assert_eq!(sprites[0].y, 30, "a Y on a line drawn stays the Y byte");
    assert_eq!(sprites[1].x, 100, "in-range X is unchanged");
}

/// A PPU showing sprite 0 alone at `y` (x 40), a large sprite `size`
/// pixels square (16, 32 or 64, by OBSEL's size selector), every pixel of
/// every character colour 5. The other 127 sprites sit past the right
/// edge, where no line draws them.
fn sprite_0_at(y: u8, size: u8) -> Ppu {
    let mut ppu = Ppu::new();
    ppu.display_disable = false;
    ppu.display_brightness = 15;
    ppu.obj_main_enable = true;
    ppu.obj_base_size = match size {
        16 => 0,
        32 => 1,
        64 => 2,
        _ => unreachable!("a large sprite is 16, 32 or 64 pixels square"),
    };
    // 4bpp: planes 0 and 2 set, 1 and 3 clear.
    ppu.vram.fill(0x00FF);
    for sprite in 1..128 {
        ppu.oam[sprite * 4] = 44; // x 300 with its bit 8, set below
    }
    ppu.oam[0x200..].fill(0x55);
    ppu.oam[0] = 40;
    ppu.oam[1] = y;
    ppu.oam[0x200] = 0x56; // sprite 0: X bit 8 clear, the size bit set
    ppu
}

/// The rows of the picture the renderer paints sprite 0 on, drawn alone,
/// and the picture's height.
fn rows_sprite_0_paints(ppu: &mut Ppu) -> (Vec<i32>, u32) {
    let mut chosen = [false; 128];
    chosen[0] = true;
    let image = ppu.render_sprites(&chosen);
    let painted = (0..image.height)
        .filter(|&row| {
            (0..image.width).any(|x| image.rgba[((row * image.width + x) * 4 + 3) as usize] != 0)
        })
        .map(|row| row as i32)
        .collect();
    (painted, image.height)
}

/// A placement of sprite 0: its Y byte, its size, the y the sprite list
/// and the sprite table must give, and the runs of picture rows the
/// renderer paints, each its first and last row.
type Placement<'a> = (u8, u8, i32, &'a [(i32, i32)]);

/// Sprite 0 at each placement, on a frame of 224 lines, or 239 with
/// `overscan` (shown whole, so a picture row is a line drawn), with or
/// without OBJ interlace.
fn check_placements(overscan: bool, obj_interlace: bool, placements: &[Placement<'_>]) {
    for &(y_byte, size, y, paints) in placements {
        let what = format!(
            "{size}x{size} at {y_byte}, overscan {overscan}, OBJ interlace {obj_interlace}"
        );
        let mut ppu = sprite_0_at(y_byte, size);
        ppu.overscan = overscan;
        ppu.show_overscan = overscan;
        ppu.obj_interlace = obj_interlace;

        let sprites = ppu.get_sprite_data();
        let sprite = &sprites[0];
        assert_eq!((sprite.x, sprite.y), (40, y), "{what}: the sprite list");
        let entry = &ppu.sprite_table()[0];
        assert_eq!((entry.index, entry.y), (0, y), "{what}: the sprite table");

        let (painted, height) = rows_sprite_0_paints(&mut ppu);
        assert_eq!(height, if overscan { 240 } else { 224 }, "{what}");
        let expected: Vec<i32> = paints
            .iter()
            .flat_map(|&(first, last)| first..=last)
            .collect();
        assert_eq!(painted, expected, "{what}: the rows painted");

        // Every row of the sprite's box on a line drawn is painted, and
        // every other row painted is a row of the box wrapped past line
        // 255 onto the top.
        let lines_drawn = if overscan { 239 } else { 224 };
        let lines_covered = if obj_interlace {
            sprite.height as i32 / 2
        } else {
            sprite.height as i32
        };
        let in_box = |row: i32| (y..y + lines_covered).contains(&row);
        for row in (0..lines_drawn).filter(|&row| in_box(row)) {
            assert!(painted.contains(&row), "{what}: row {row} of the box");
        }
        for &row in &painted {
            assert!(in_box(row) || in_box(row + 256), "{what}: row {row}");
        }

        // On this picture of one row a line, from the frame's first line,
        // the box on the picture is the sprite's dots and lines.
        let on_picture = PictureRect {
            x: 40,
            y,
            width: u32::from(size),
            height: lines_covered as u32,
        };
        assert_eq!(sprite.picture, on_picture, "{what}: the box on the picture");
        assert_eq!(entry.picture, on_picture, "{what}: the table's box");
    }
}

/// The box of picture pixels the renderer paints sprite 0 on, drawn alone:
/// from its first column and row to its last; `None` when it paints none.
fn box_sprite_0_paints(ppu: &mut Ppu) -> Option<PictureRect> {
    let mut chosen = [false; 128];
    chosen[0] = true;
    let image = ppu.render_sprites(&chosen);
    let mut corners: Option<(u32, u32, u32, u32)> = None;
    for row in 0..image.height {
        for column in 0..image.width {
            if image.rgba[((row * image.width + column) * 4 + 3) as usize] == 0 {
                continue;
            }
            let (left, top, right, bottom) = corners.get_or_insert((column, row, column, row));
            *left = (*left).min(column);
            *top = (*top).min(row);
            *right = (*right).max(column);
            *bottom = (*bottom).max(row);
        }
    }
    corners.map(|(left, top, right, bottom)| PictureRect {
        x: left as i32,
        y: top as i32,
        width: right - left + 1,
        height: bottom - top + 1,
    })
}

/// Sprite 0 at line 100, 16x16, on the picture `shape` makes, held to the
/// box the renderer paints it on and the box the sprite list and the
/// sprite table give.
fn check_box(what: &str, shape: impl Fn(&mut Ppu), expected: PictureRect) {
    let mut ppu = sprite_0_at(100, 16);
    shape(&mut ppu);
    assert_eq!(
        box_sprite_0_paints(&mut ppu),
        Some(expected),
        "{what}: the box painted"
    );
    assert_eq!(
        ppu.get_sprite_data()[0].picture,
        expected,
        "{what}: the list"
    );
    assert_eq!(ppu.sprite_table()[0].picture, expected, "{what}: the table");
}

#[test]
fn a_sprite_s_box_on_the_picture_is_where_the_picture_shows_it() {
    check_box(
        "lo-res, 224 lines",
        |_| {},
        PictureRect {
            x: 40,
            y: 100,
            width: 16,
            height: 16,
        },
    );
    // The picture starts at the line below the 7 overscan lines it crops.
    check_box(
        "overscan cropped",
        |ppu| ppu.overscan = true,
        PictureRect {
            x: 40,
            y: 93,
            width: 16,
            height: 16,
        },
    );
    // Hires, its lines doubled or two fields woven: two columns a dot and
    // two rows a line.
    for interlace in [false, true] {
        check_box(
            &format!("hires, interlace {interlace}"),
            |ppu| {
                ppu.bgmode = 5;
                ppu.interlace = interlace;
                ppu.frame_hires = true;
                ppu.render_interlace = interlace;
            },
            PictureRect {
                x: 80,
                y: 200,
                width: 32,
                height: 32,
            },
        );
    }
    // Hires with OBJ interlace: the sprite's 16 rows cover 8 lines, 16 rows
    // of the woven picture.
    check_box(
        "hires, woven, OBJ interlace",
        |ppu| {
            ppu.bgmode = 5;
            ppu.interlace = true;
            ppu.obj_interlace = true;
            ppu.frame_hires = true;
            ppu.render_interlace = true;
        },
        PictureRect {
            x: 80,
            y: 200,
            width: 32,
            height: 16,
        },
    );
}

#[test]
fn a_sprite_s_y_is_the_line_the_renderer_draws_its_first_row_on() {
    // 224 lines: a sprite whose first row is past them and whose rows wrap
    // onto the top reads its Y byte less 256; every other keeps its byte.
    check_placements(
        false,
        false,
        &[
            (248, 16, -8, &[(0, 7)]),
            (100, 16, 100, &[(100, 115)]),
            (220, 16, 220, &[(220, 223)]), // cut by the bottom edge
            (250, 16, -6, &[(0, 9)]),
            (223, 16, 223, &[(223, 223)]), // on the last line drawn
            (200, 32, 200, &[(200, 223)]), // cut by the bottom edge
            (224, 16, 224, &[]),           // drawn on no line
            (240, 16, 240, &[]),           // its last row on line 255
            (241, 16, -15, &[(0, 0)]),     // its last row on line 0
            (232, 32, -24, &[(0, 7)]),
            // Taller than the 32 lines below the picture: at 220 its first
            // rows are drawn at the bottom and its last ones on the top, at
            // 224 only its last ones.
            (220, 64, 220, &[(0, 27), (220, 223)]),
            (224, 64, -32, &[(0, 31)]),
        ],
    );
    // 239 lines: the first row of the sprite at 232 is drawn.
    check_placements(
        true,
        false,
        &[
            (232, 32, 232, &[(0, 7), (232, 238)]),
            (239, 32, -17, &[(0, 14)]), // on the first line past the picture
            (230, 16, 230, &[(230, 238)]),
            (250, 16, -6, &[(0, 9)]),
        ],
    );
    // OBJ interlace: a field shows every other row, so 16 rows cover 8
    // lines, and a sprite at 248 ends on line 255.
    check_placements(
        false,
        true,
        &[
            (248, 16, 248, &[]),
            (250, 16, -6, &[(0, 1)]),
            (100, 16, 100, &[(100, 107)]),
        ],
    );
}

/// A PPU showing sprites on the main screen, every sprite parked below
/// the visible lines but slots 0-9, which sit on line 10 side by side,
/// slot `n` with priority `n % 4`.
fn ten_sprites_shown() -> Ppu {
    let mut ppu = Ppu::new();
    ppu.display_disable = false;
    ppu.display_brightness = 15;
    ppu.obj_main_enable = true;
    for slot in 0..128 {
        ppu.oam[slot * 4 + 1] = 0xF0;
    }
    for slot in 0..10 {
        ppu.oam[slot * 4] = (slot * 10) as u8;
        ppu.oam[slot * 4 + 1] = 10;
        ppu.oam[slot * 4 + 3] = ((slot % 4) as u8) << 4;
    }
    ppu
}

#[test]
fn resolved_render_order_lists_the_drawn_sprites_by_the_rotation() {
    let mut ppu = ten_sprites_shown();
    ppu.oam_first_sprite = 5;

    // The walk meets 5..9 then 0..4; back to front is the reverse: the
    // sprite before the first sprite at the back, the first in front.
    let order = ppu.resolved_sprite_render_order();
    let got: Vec<u32> = order.iter().map(|e| e.index).collect();
    assert_eq!(got, vec![4, 3, 2, 1, 0, 9, 8, 7, 6, 5]);
    assert_eq!(order.last().unwrap().index, 5, "first-sprite paints on top");
    assert!(order.iter().all(|e| e.priority == e.index % 4));

    // Without the rotation, slot 0 is in front.
    ppu.oam_first_sprite = 0;
    let got: Vec<u32> = ppu
        .resolved_sprite_render_order()
        .iter()
        .map(|e| e.index)
        .collect();
    assert_eq!(got, (0..10u32).rev().collect::<Vec<_>>());
}

#[test]
fn resolved_render_order_leaves_out_what_drew_nothing() {
    let mut ppu = ten_sprites_shown();
    // Slot 3 at x = 256: met and fetched, but no pixel of it shows.
    ppu.oam[3 * 4] = 0;
    ppu.oam[0x200] |= 0x01 << (3 * 2);
    let got: Vec<u32> = ppu
        .resolved_sprite_render_order()
        .iter()
        .map(|e| e.index)
        .collect();
    assert!(!got.contains(&3));
    assert_eq!(got.len(), 9);

    // A frame that shows no sprite draws none.
    ppu.obj_main_enable = false;
    assert!(ppu.resolved_sprite_render_order().is_empty());
    ppu.obj_main_enable = true;
    ppu.display_disable = true;
    assert!(ppu.resolved_sprite_render_order().is_empty());
}

// -- export_sprite_image -----------------------------------------

#[test]
fn export_sprite_index_out_of_range() {
    let ppu = Ppu::new();
    assert_eq!(ppu.export_sprite_image(128), Err(ExportError::OutOfRange));
}

#[test]
fn export_sprite_applies_palette_and_index0_transparency() {
    let mut ppu = Ppu::new();
    // Sprite 0: small 8x8 (OBSEL selector 0), palette 1.
    ppu.oam[3] = 0x02; // attr: palette 1 (bits 1-3), no flip/priority
                       // Tile 0 row 0: plane0 bit 7 set -> only column 0 is index 1.
    ppu.vram[0] = 0x0080;
    // OBJ palette 1 colour 1 lives at CGRAM 128 + 1*16 + 1 = 145.
    ppu.cgram[145] = 0x001F; // red
    let img = ppu.export_sprite_image(0).expect("sprite 0");
    assert_eq!((img.width, img.height), (8, 8));
    assert_eq!(&img.rgba[0..4], &[0xFF, 0, 0, 0xFF], "col 0 = opaque red");
    assert_eq!(
        &img.rgba[4..8],
        &[0, 0, 0, 0],
        "col 1 index 0 = transparent"
    );
}

#[test]
fn export_sprite_honours_hflip() {
    let mut ppu = Ppu::new();
    ppu.oam[3] = 0x02 | 0x40; // palette 1 + hflip
    ppu.vram[0] = 0x0080; // col 0 index 1 before flip
    ppu.cgram[145] = 0x001F;
    let img = ppu.export_sprite_image(0).expect("sprite 0");
    assert_eq!(&img.rgba[0..4], &[0, 0, 0, 0], "flipped: col 0 transparent");
    let last = 7 * 4;
    assert_eq!(
        &img.rgba[last..last + 4],
        &[0xFF, 0, 0, 0xFF],
        "lit pixel moved to col 7"
    );
}

// -- bg_bpp / bg_layout ------------------------------------------

#[test]
fn bg_bpp_tracks_mode() {
    let mut ppu = Ppu::new();
    ppu.bgmode = 1;
    assert_eq!(ppu.bg_bpp(0), 4, "BG1 mode 1 = 4bpp");
    assert_eq!(ppu.bg_bpp(2), 2, "BG3 mode 1 = 2bpp");
    ppu.bgmode = 3;
    assert_eq!(ppu.bg_bpp(0), 8, "BG1 mode 3 = 8bpp");
}

#[test]
fn bg_layout_reads_live_registers() {
    let mut ppu = Ppu::new();
    ppu.bgmode = 0;
    ppu.bg_screen_addr[1] = 0x2000;
    ppu.bg_tiledata_addr[1] = 0x4000;
    ppu.bg_screen_size[1] = 0x03; // 64x64
    ppu.bg_tile_size[1] = true;
    ppu.bg_hoffset[1] = 0x1FF;
    ppu.bg_voffset[1] = 0x0AA;
    let layout = ppu.bg_layout(1);
    assert_eq!(layout.bg, 1);
    assert_eq!(layout.bg_mode, 0);
    assert_eq!(layout.tilemap_base_byte, 0x2000);
    assert_eq!(layout.char_base_byte, 0x4000);
    assert_eq!((layout.width_cells, layout.height_cells), (64, 64));
    assert_eq!(layout.screen_size, 0x03);
    assert!(layout.tile_size16);
    assert_eq!(layout.bpp, 2, "BG2 mode 0 = 2bpp");
    assert_eq!(layout.palette_base, 32, "mode 0 -> bg*32");
    assert_eq!((layout.scroll_x, layout.scroll_y), (0x1FF, 0x0AA));
}

// -- sprite_tile_settings ----------------------------------------

#[test]
fn sprite_tile_settings_read_obsel_and_obj_interlace() {
    let mut ppu = Ppu::new();
    // Power-on OBSEL is 0: 8x8 and 16x16, the first table at word 0,
    // the second 256 tiles past it.
    assert_eq!(
        ppu.sprite_tile_settings(),
        SpriteTileSettings {
            small_width: 8,
            small_height: 8,
            large_width: 16,
            large_height: 16,
            first_table_base_word: 0,
            second_table_base_word: 0x1000,
        }
    );
    // Base 2, gap 1: the tables at words $4000 and $4000 + $1000 +
    // $1000.
    ppu.write_2101(0x0A);
    let settings = ppu.sprite_tile_settings();
    assert_eq!(settings.first_table_base_word, 0x4000);
    assert_eq!(settings.second_table_base_word, 0x6000);
    // Base 4 is past VRAM's 32 K words and wraps to word 0, as the
    // renderer reads it.
    ppu.write_2101(0x04);
    assert_eq!(ppu.sprite_tile_settings().first_table_base_word, 0);
    // Selector 6 under OBJ interlace: the small height is 16, as in
    // the sprite list; the large size is unchanged.
    ppu.write_2101(0xC0);
    ppu.obj_interlace = true;
    let settings = ppu.sprite_tile_settings();
    assert_eq!((settings.small_width, settings.small_height), (16, 16));
    assert_eq!((settings.large_width, settings.large_height), (32, 64));
}

/// A host that adds a cell's place in the 16 x 16 table to the
/// table's address, both axes wrapping inside the table, reads the
/// tile the renderer reads: for every OBSEL value, both tables, and
/// every cell of a 64x64 sprite at characters on the table's edges
/// and inside it.
#[test]
fn sprite_tile_settings_locate_every_tile_the_renderer_reads() {
    let mut ppu = Ppu::new();
    for obsel in 0..=0xFFu8 {
        ppu.write_2101(obsel);
        let settings = ppu.sprite_tile_settings();
        for second_table in [false, true] {
            let table = if second_table {
                settings.second_table_base_word
            } else {
                settings.first_table_base_word
            } >> 4;
            for character in [0x00u8, 0x0F, 0x5A, 0xF0, 0xFE, 0xFF] {
                for column in 0..8u8 {
                    for tile_row in 0..8u16 {
                        let renderer = sprite_tile_number(
                            ppu.obj_tiledata_addr,
                            ppu.obj_name_select,
                            character,
                            second_table,
                            false,
                            64,
                            column,
                            tile_row * 8,
                        );
                        let grid_column = (u16::from(character & 15) + u16::from(column)) & 15;
                        let grid_row = (u16::from(character >> 4) + tile_row) & 15;
                        assert_eq!(
                            renderer,
                            (table + grid_row * 16 + grid_column) & 0x07FF,
                            "OBSEL {obsel:#04x}, second table {second_table}, \
                             character {character:#04x}, column {column}, row {tile_row}"
                        );
                    }
                }
            }
        }
    }
}

// -- repaint sequence (the core of SnesEmulator::repaint_held_frame) --

#[test]
fn repaint_sequence_reflects_layer_toggle() {
    use retro_core::Framebuffer;
    let mut ppu = Ppu::new();
    ppu.display_disable = false;
    ppu.display_brightness = 15;
    ppu.bgmode = 0; // BG1 = 2bpp
    ppu.bg_main_enable[0] = true;
    ppu.bg_screen_addr[0] = 0; // tilemap word base 0
    ppu.bg_tiledata_addr[0] = 0x1000; // char word base 0x800
                                      // Fill the whole 32x32 tilemap with "tile 1, palette 0" so the
                                      // BG covers the screen regardless of scroll.
    for cell in &mut ppu.vram[0..1024] {
        *cell = 0x0001;
    }
    // Char tile 1 (2bpp, 8 words at word base 0x800 + 1*8 = 0x808):
    // every row plane0 = 0xFF -> a solid index-1 tile.
    for r in 0..8 {
        ppu.vram[0x808 + r] = 0x00FF;
    }
    ppu.cgram[0] = 0x0000; // backdrop black
    ppu.cgram[1] = 0x7FFF; // BG colour white

    let repaint = |ppu: &mut Ppu| -> Vec<u8> {
        let mut fb = Framebuffer::new(256, 224);
        for scanline in 1..240u16 {
            ppu.render_scanline(scanline, &mut fb);
        }
        ppu.finalize_frame(&mut fb);
        fb.data
    };

    let with_bg = repaint(&mut ppu);
    ppu.set_layer_debug_disabled(0, true);
    let without_bg = repaint(&mut ppu);
    assert_ne!(
        with_bg, without_bg,
        "hiding BG1 must change the repainted frame"
    );
    // Re-enabling restores the original frame exactly.
    ppu.set_layer_debug_disabled(0, false);
    assert_eq!(repaint(&mut ppu), with_bg, "showing BG1 again is identical");
}

// -- debugger writes into video memory -------------------------------

/// Repaint a whole frame as `SnesEmulator::repaint_held_frame` does,
/// into a 256x224 framebuffer, and return its RGBA bytes.
fn repaint(ppu: &mut Ppu) -> Vec<u8> {
    let mut fb = retro_core::Framebuffer::new(256, 224);
    for scanline in 1..240u16 {
        ppu.render_scanline(scanline, &mut fb);
    }
    ppu.finalize_frame(&mut fb);
    fb.data
}

/// The RGBA of pixel `(x, y)` of a 256-wide frame.
fn pixel(frame: &[u8], x: usize, y: usize) -> [u8; 4] {
    let at = (y * 256 + x) * 4;
    [frame[at], frame[at + 1], frame[at + 2], frame[at + 3]]
}

const WHITE: u16 = 0x7FFF;
const RED: u16 = 0x001F;

/// Mode 0 with BG1 on the main screen: its 32x32 tilemap at word 0 all
/// tile 1 (2bpp, all colour 1, white), and tile 2 (all colour 2, red)
/// beside it, at char word base 0x800.
fn ppu_with_bg1_of_tile_1() -> Ppu {
    let mut ppu = Ppu::new();
    ppu.display_disable = false;
    ppu.display_brightness = 15;
    ppu.bgmode = 0;
    ppu.bg_main_enable[0] = true;
    ppu.bg_screen_addr[0] = 0;
    ppu.bg_tiledata_addr[0] = 0x1000;
    for cell in &mut ppu.vram[0..1024] {
        *cell = 0x0001;
    }
    for row in 0..8 {
        ppu.vram[0x808 + row] = 0x00FF; // tile 1: plane 0, colour 1
        ppu.vram[0x810 + row] = 0xFF00; // tile 2: plane 1, colour 2
    }
    ppu.cgram[1] = WHITE;
    ppu.cgram[2] = RED;
    ppu
}

/// Sprites on the main screen and nothing else: sprite 0 small (8x8)
/// at (0, 0) with tile 0 of palette 0, all colour 1 (white), and the
/// other 127 below the picture.
fn ppu_with_sprite_0() -> Ppu {
    let mut ppu = Ppu::new();
    ppu.display_disable = false;
    ppu.display_brightness = 15;
    ppu.obj_main_enable = true;
    for sprite in 1..128 {
        ppu.oam[sprite * 4 + 1] = 0xF0;
    }
    // Tile 0, 4bpp at word 0: every row with plane 0 set, colour 1.
    for row in 0..8 {
        ppu.vram[row] = 0x00FF;
    }
    ppu.cgram[128 + 1] = WHITE;
    ppu.cgram[128 + 2] = RED;
    ppu
}

#[test]
fn debugger_writes_read_back_and_leave_the_ports_alone() {
    let mut ppu = Ppu::new();
    // The display is on, which closes the ports while lines are drawn,
    // and each port holds an address and a latched byte.
    ppu.display_disable = false;
    ppu.vram_addr = 0x1234;
    ppu.vram_readbuffer = 0xBEEF;
    ppu.oam_addr = 0x0123;
    ppu.oam_latchdata = 0x45;
    ppu.cgram_addr = 0x0042;
    ppu.cgram_latchdata = 0x67;

    ppu.write_vram_debug(0x2000, &[0x34, 0x12, 0x78]);
    ppu.write_cgram_debug(5, &[0x1234, 0x0567]);
    ppu.write_oam_debug(0x200, &[0x89, 0xAB]);

    assert_eq!(&ppu.get_vram()[0x4000..0x4003], &[0x34, 0x12, 0x78]);
    assert_eq!(ppu.vram()[0x2000], 0x1234, "the low half of a word first");
    assert_eq!(ppu.vram()[0x2001], 0x0078, "an odd count keeps a high half");
    assert_eq!(&ppu.cgram()[5..7], &[0x1234, 0x0567]);
    assert_eq!(&ppu.oam()[0x200..0x202], &[0x89, 0xAB]);

    assert_eq!(ppu.vram_addr(), 0x1234);
    assert_eq!(ppu.vram_readbuffer, 0xBEEF);
    assert_eq!(ppu.oam_addr(), 0x0123);
    assert_eq!(ppu.oam_latchdata, 0x45);
    assert_eq!(ppu.cgram_addr(), 0x0042);
    assert_eq!(ppu.cgram_latchdata, 0x67);
}

#[test]
fn a_vram_write_wraps_past_the_end_and_is_cut_to_the_memory() {
    let mut ppu = Ppu::new();
    // Word 0x7FFF is the last; the run goes on at word 0.
    ppu.write_vram_debug(0x7FFF, &[1, 2, 3, 4]);
    assert_eq!(&ppu.get_vram()[0xFFFE..], &[1, 2]);
    assert_eq!(&ppu.get_vram()[..2], &[3, 4]);

    // A word address past the end wraps: 0x8003 is word 3.
    ppu.write_vram_debug(0x8003, &[9]);
    assert_eq!(ppu.get_vram()[6], 9);

    // A buffer two bytes longer than VRAM from word 1: its first 64 KiB
    // land once each, from byte 2 round to byte 1, and the two left
    // over are dropped instead of written over the first two.
    let pattern = |i: usize| (i % 251) as u8;
    let long: Vec<u8> = (0..VRAM_BYTES + 2).map(pattern).collect();
    ppu.write_vram_debug(1, &long);
    let vram = ppu.get_vram();
    assert!((0..VRAM_BYTES).all(|i| vram[(2 + i) % VRAM_BYTES] == pattern(i)));
}

#[test]
fn an_empty_vram_write_changes_nothing() {
    let mut ppu = Ppu::new();
    ppu.vram[5] = 0xBEEF;
    let generation = ppu.vram_generation;

    ppu.write_vram_debug(5, &[]);

    let written = ppu.vram.iter().filter(|&&word| word != 0).count();
    assert_eq!((ppu.vram[5], written), (0xBEEF, 1));
    assert_eq!(
        ppu.vram_generation, generation,
        "the caches keep what they hold"
    );
}

#[test]
fn a_vram_write_reaches_the_tilemap_row_the_renderer_keeps() {
    let mut ppu = ppu_with_bg1_of_tile_1();
    let mut fb = retro_core::Framebuffer::new(256, 224);
    // Scanline 1 draws output row 0 from cell row 0, a row the renderer
    // keeps for the scanlines after it in the same band.
    ppu.render_scanline(1, &mut fb);
    assert_eq!(pixel(&fb.data, 0, 0), bgr15_to_rgba8(WHITE));

    // Poked behind the renderer's back, cell (0, 0) keeps the tile the
    // kept row names.
    ppu.vram[0] = 0x0002;
    ppu.render_scanline(2, &mut fb);
    assert_eq!(
        pixel(&fb.data, 0, 1),
        bgr15_to_rgba8(WHITE),
        "the kept row still names tile 1"
    );

    // The same word through the debugger write reaches the renderer.
    ppu.write_vram_debug(0, &[0x02, 0x00]);
    ppu.render_scanline(3, &mut fb);
    assert_eq!(
        pixel(&fb.data, 0, 2),
        bgr15_to_rgba8(RED),
        "cell (0, 0) is tile 2 now"
    );
    assert_eq!(
        pixel(&fb.data, 8, 2),
        bgr15_to_rgba8(WHITE),
        "cell (1, 0) is still tile 1"
    );
}

#[test]
fn a_vram_write_reaches_the_sprite_tiles_the_renderer_keeps() {
    let mut ppu = ppu_with_sprite_0();
    assert_eq!(pixel(&repaint(&mut ppu), 0, 0), bgr15_to_rgba8(WHITE));

    // Poked behind the renderer's back, tile 0 keeps the decode the
    // renderer kept.
    for row in 0..8 {
        ppu.vram[row] = 0xFF00; // plane 1: colour 2
    }
    assert_eq!(
        pixel(&repaint(&mut ppu), 0, 0),
        bgr15_to_rgba8(WHITE),
        "the kept decode is still colour 1"
    );

    // The same rows through the debugger write reach the renderer.
    ppu.write_vram_debug(0, &[0x00, 0xFF].repeat(8));
    assert_eq!(
        pixel(&repaint(&mut ppu), 0, 0),
        bgr15_to_rgba8(RED),
        "tile 0 is colour 2 now"
    );
}

#[test]
fn a_cgram_write_recolours_the_next_repaint() {
    let mut ppu = ppu_with_bg1_of_tile_1();
    assert_eq!(pixel(&repaint(&mut ppu), 0, 0), bgr15_to_rgba8(WHITE));

    let green = 0x03E0;
    ppu.write_cgram_debug(1, &[green]);

    assert_eq!(ppu.cgram()[1], green);
    assert_eq!(pixel(&repaint(&mut ppu), 0, 0), bgr15_to_rgba8(green));
}

#[test]
fn a_cgram_write_keeps_15_bits_wraps_and_is_cut_to_the_memory() {
    let mut ppu = Ppu::new();
    // Colour 255, then on round to colour 0; bit 15 is dropped, as the
    // port drops it.
    ppu.write_cgram_debug(255, &[0xFFFF, 0x1234]);
    assert_eq!(ppu.cgram()[255], 0x7FFF);
    assert_eq!(ppu.cgram()[0], 0x1234);

    // An index past the 256 colours wraps.
    ppu.write_cgram_debug(256 + 7, &[0x0042]);
    assert_eq!(ppu.cgram()[7], 0x0042);

    // 300 colours from colour 10: the first 256 land once each, the 44
    // left over are dropped instead of written over 10..54.
    let long: Vec<u16> = (0..300)
        .map(|i| if i < 256 { 0x0001 } else { 0x0002 })
        .collect();
    ppu.write_cgram_debug(10, &long);
    assert!(ppu.cgram().iter().all(|&colour| colour == 0x0001));

    let before = *ppu.cgram();
    ppu.write_cgram_debug(3, &[]);
    assert_eq!(*ppu.cgram(), before, "an empty write changes nothing");
}

#[test]
fn an_oam_write_moves_the_sprite_on_the_next_repaint() {
    let mut ppu = ppu_with_sprite_0();
    let frame = repaint(&mut ppu);
    assert_eq!(pixel(&frame, 0, 0), bgr15_to_rgba8(WHITE));
    assert_eq!(pixel(&frame, 16, 0), bgr15_to_rgba8(0), "the backdrop");

    // Sprite 0's X is its first byte.
    ppu.write_oam_debug(0, &[16]);

    assert_eq!(ppu.get_sprite_data()[0].x, 16);
    let frame = repaint(&mut ppu);
    assert_eq!(
        pixel(&frame, 0, 0),
        bgr15_to_rgba8(0),
        "x 0 is backdrop now"
    );
    assert_eq!(pixel(&frame, 16, 0), bgr15_to_rgba8(WHITE));
}

#[test]
fn an_oam_write_wraps_past_the_end_and_is_cut_to_the_memory() {
    let mut ppu = Ppu::new();
    // The last byte of the high table, then on round to sprite 0's X.
    ppu.write_oam_debug(0x21F, &[0xAA, 0xBB]);
    assert_eq!(ppu.oam()[0x21F], 0xAA);
    assert_eq!(ppu.oam()[0], 0xBB);

    // An address past the 544 bytes wraps: 544 + 9 is byte 9.
    ppu.write_oam_debug(0x220 + 9, &[0x42]);
    assert_eq!(ppu.oam()[9], 0x42);

    // 600 bytes from byte 10: the first 544 land once each, the 56
    // left over are dropped instead of written over 10..66.
    let long: Vec<u8> = (0..600).map(|i| if i < 0x220 { 1 } else { 2 }).collect();
    ppu.write_oam_debug(10, &long);
    assert!(ppu.oam().iter().all(|&byte| byte == 1));

    let before = *ppu.oam();
    ppu.write_oam_debug(3, &[]);
    assert_eq!(*ppu.oam(), before, "an empty write changes nothing");
}

#[test]
fn the_sprite_table_starts_at_the_first_sprite_and_links_none() {
    let mut ppu = Ppu::new();
    ppu.oam[4 * 3] = 40; // sprite 3's x
    ppu.oam[4 * 3 + 2] = 0x12; // its character
    ppu.oam[4 * 3 + 3] = 0x01; // in the second table
    ppu.oam_first_sprite = 3;
    let table = ppu.sprite_table();
    assert_eq!(table.len(), 128);
    assert_eq!(
        (table[0].index, table[125].index, table[127].index),
        (3, 0, 2)
    );
    assert!(table.iter().all(|entry| entry.next.is_none()));
    let sprites = ppu.get_sprite_data();
    for entry in &table {
        let sprite = &sprites[entry.index as usize];
        assert_eq!(
            (
                entry.x,
                entry.y,
                entry.width,
                entry.height,
                entry.tile_index
            ),
            (
                sprite.x,
                sprite.y,
                sprite.width,
                sprite.height,
                sprite.tile_index
            )
        );
    }
    let settings = ppu.sprite_tile_settings();
    assert_eq!(
        table[0].tile_address,
        (u32::from(settings.second_table_base_word) * 2 + 0x12 * 32) & 0xFFFF
    );
}

#[test]
fn a_background_s_layout_is_its_registers_and_mode_7_its_own() {
    let mut ppu = Ppu::new();
    ppu.bgmode = 1;
    for bg in 0..4 {
        let layout = ppu.layer_layout(bg as u32).unwrap();
        let registers = ppu.bg_layout(bg);
        assert_eq!(
            (
                layout.map_base,
                layout.map_width,
                layout.map_height,
                layout.character_base,
                layout.bits_per_pixel
            ),
            (
                registers.tilemap_base_byte,
                registers.width_cells,
                registers.height_cells,
                registers.char_base_byte,
                registers.bpp
            )
        );
        let kind = if registers.bpp == 0 {
            LayerKind::Off
        } else {
            LayerKind::TileMap
        };
        assert_eq!(layout.kind, kind, "BG{}", bg + 1);
    }
    ppu.bgmode = 7;
    let affine = ppu.layer_layout(0).unwrap();
    assert_eq!(
        (
            affine.kind,
            affine.map_width,
            affine.map_height,
            affine.bits_per_pixel
        ),
        (LayerKind::AffineMap, 128, 128, 8)
    );
    assert_eq!(ppu.layer_layout(1).unwrap().kind, LayerKind::Off);
    ppu.mode7_extbg = true;
    let extbg = ppu.layer_layout(1).unwrap();
    assert_eq!(
        (extbg.kind, extbg.bits_per_pixel),
        (LayerKind::AffineMap, 7)
    );
    assert_eq!(ppu.layer_layout(4).unwrap().kind, LayerKind::Sprites);
    assert!(ppu.layer_layout(5).is_none());
}

#[test]
fn a_layer_s_scroll_is_its_scroll_registers_or_mode_7_s_offsets() {
    let mut ppu = Ppu::new();
    ppu.bgmode = 1;
    ppu.bg_hoffset[0] = 0x1FF;
    ppu.bg_voffset[0] = 0x0AA;
    ppu.mode7_hoffset = 0x1F00; // -256 in 13 bits
    ppu.mode7_voffset = 0x0040;
    let scroll = |ppu: &Ppu, id: u32| {
        let data = ppu.get_layer_data(id).unwrap();
        let layout = ppu.layer_layout(id).unwrap();
        assert_eq!(
            (data.scroll_x, data.scroll_y),
            (layout.scroll_x, layout.scroll_y),
            "the layout reports what the list does"
        );
        assert!(data.line_scroll.is_empty() && layout.line_scroll.is_empty());
        (data.scroll_x, data.scroll_y)
    };
    assert_eq!(scroll(&ppu, 0), (0x1FF, 0x0AA), "BGnHOFS and BGnVOFS");
    ppu.bgmode = 7;
    assert_eq!(scroll(&ppu, 0), (-256, 0x40), "Mode 7's own, signed");
    ppu.bg_hoffset[1] = 3;
    assert_eq!(scroll(&ppu, 1), (3, 0), "BG2 is no plane of Mode 7's");
    ppu.mode7_extbg = true;
    assert_eq!(
        scroll(&ppu, 1),
        (-256, 0x40),
        "EXTBG draws BG2 as the plane"
    );
    assert_eq!(scroll(&ppu, 4), (0, 0), "the sprites do not scroll");
}

#[test]
fn a_video_memory_write_counts_bytes_from_any_byte() {
    let mut ppu = Ppu::new();
    assert!(ppu.write_video_memory(VideoMemoryId::Vram, 0x0201, &[0xAB, 0xCD]));
    assert_eq!(&ppu.get_vram()[0x200..0x204], &[0x00, 0xAB, 0xCD, 0x00]);
    assert!(ppu.write_video_memory(VideoMemoryId::Vram, 0xFFFF, &[0x11, 0x22]));
    assert_eq!((ppu.get_vram()[0xFFFF], ppu.get_vram()[0]), (0x11, 0x22));
    assert!(ppu.write_video_memory(VideoMemoryId::Palette, 5, &[0x34, 0x92]));
    assert_eq!(ppu.cgram()[2], 0x1234, "low byte first, bit 15 dropped");
    assert_eq!(ppu.read_video_memory(VideoMemoryId::VerticalScroll), None);
    assert!(!ppu.write_video_memory(VideoMemoryId::VerticalScroll, 0, &[1, 2]));
}
