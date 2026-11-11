//! Integration tests for VDP debugger functionality.

use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode};
use sg1_core::GenesisEmulator;

/// Helper to create ROM and setup VDP with test data.
fn setup_vdp_test() -> GenesisEmulator {
    let mut rom = vec![0u8; 0x10000];
    rom[0x100..0x110].copy_from_slice(b"SEGA GENESIS    ");

    let mut emu = GenesisEmulator::new();
    emu.load_rom(&rom).expect("Failed to load ROM");

    emu
}

#[test]
fn test_get_tile_rgba() {
    let mut emu = setup_vdp_test();

    // Access VDP to write a tile pattern
    // Tile 0 at VRAM 0x0000
    let vdp = emu.vdp_mut();

    // Write a simple pattern (diagonal line)
    for y in 0..8 {
        for x in 0..8 {
            let pixel_index = y * 8 + x;
            let byte_index = pixel_index / 2;
            let is_high_nibble = (pixel_index % 2) == 0;

            let color = if x == y { 15 } else { 0 }; // Diagonal line in color 15

            if is_high_nibble {
                vdp.get_vram_mut()[byte_index] =
                    (vdp.get_vram_mut()[byte_index] & 0x0F) | (color << 4);
            } else {
                vdp.get_vram_mut()[byte_index] = (vdp.get_vram_mut()[byte_index] & 0xF0) | color;
            }
        }
    }

    // Set palette color 15 to white
    vdp.get_cram_mut()[15] = 0x0EEE; // Bright white

    // Get tile as RGBA
    let rgba = vdp.get_tile_rgba(0, 0); // Tile 0, palette 0

    // Should be 8x8x4 = 256 bytes
    assert_eq!(rgba.len(), 256);

    // Check that we got data (exact pixel values depend on tile format)
    // Verify at least some pixels are not all zero
    let has_data = rgba.iter().any(|&b| b != 0);
    assert!(has_data, "Tile should have some color data");
}

#[test]
fn test_get_all_palettes_rgba() {
    let mut emu = setup_vdp_test();

    let vdp = emu.vdp_mut();

    // Set some colors in different palettes
    vdp.get_cram_mut()[0] = 0x0000; // Palette 0, color 0: black
    vdp.get_cram_mut()[1] = 0x000E; // Palette 0, color 1: red
    vdp.get_cram_mut()[16] = 0x00E0; // Palette 1, color 0: green
    vdp.get_cram_mut()[32] = 0x0E00; // Palette 2, color 0: blue
    vdp.get_cram_mut()[63] = 0x0EEE; // Palette 3, color 15: white

    // Get all palettes as RGBA
    let rgba = vdp.get_all_palettes_rgba();

    // Should be 64 colors x 4 bytes = 256 bytes
    assert_eq!(rgba.len(), 256);

    // Check black (color 0)
    assert_eq!(rgba[0], 0); // R
    assert_eq!(rgba[1], 0); // G
    assert_eq!(rgba[2], 0); // B

    // Check red (color 1)
    assert_eq!(rgba[4], 255); // R
    assert_eq!(rgba[5], 0); // G
    assert_eq!(rgba[6], 0); // B

    // Check green (color 16)
    assert_eq!(rgba[64], 0); // R
    assert_eq!(rgba[65], 255); // G
    assert_eq!(rgba[66], 0); // B

    // Check blue (color 32)
    assert_eq!(rgba[128], 0); // R
    assert_eq!(rgba[129], 0); // G
    assert_eq!(rgba[130], 255); // B

    // Check white (color 63)
    assert_eq!(rgba[252], 255); // R
    assert_eq!(rgba[253], 255); // G
    assert_eq!(rgba[254], 255); // B
}

#[test]
fn test_vram_access_debugger() {
    let mut emu = setup_vdp_test();

    let vdp = emu.vdp_mut();

    // Write pattern to VRAM
    for i in 0..256 {
        vdp.get_vram_mut()[i] = i as u8;
    }

    // Read back via debugger
    let vram = vdp.get_vram();

    // Should be 64KB
    assert_eq!(vram.len(), 0x10000);

    // Check pattern
    for (i, &byte) in vram.iter().take(256).enumerate() {
        assert_eq!(byte, i as u8);
    }
}

#[test]
fn test_cram_access_debugger() {
    let mut emu = setup_vdp_test();

    let vdp = emu.vdp_mut();

    // Write colors to CRAM
    for i in 0..64 {
        vdp.get_cram_mut()[i] = (i as u16) * 0x111;
    }

    // Read back via debugger
    let cram = vdp.get_cram();

    // Should be 64 colors
    assert_eq!(cram.len(), 64);

    // Check colors
    for (i, &color) in cram.iter().enumerate() {
        assert_eq!(color, (i as u16) * 0x111);
    }
}

#[test]
fn test_vsram_access_debugger() {
    let mut emu = setup_vdp_test();

    let vdp = emu.vdp_mut();

    // Write scroll values to VSRAM
    for i in 0..40 {
        vdp.get_vsram_mut()[i] = i as u16;
    }

    // Read back via debugger
    let vsram = vdp.get_vsram();

    // 64 addressable words (40 physical scroll entries + the data-port-only
    // upper slots the VDP address counter can still reach).
    assert_eq!(vsram.len(), 64);

    // Check scroll values
    for (i, &value) in vsram.iter().take(40).enumerate() {
        assert_eq!(value, i as u16);
    }
}

#[test]
fn test_plane_nametable_addresses() {
    let mut emu = setup_vdp_test();

    let vdp = emu.vdp_mut();

    // Set register 2 (Plane A nametable)
    vdp.set_test_register(2, 0b00111000); // Address 0xE000
    let plane_a = vdp.get_plane_a_nametable();
    assert_eq!(plane_a, 0xE000);

    // Set register 4 (Plane B nametable)
    vdp.set_test_register(4, 0b00000111); // Address 0xE000
    let plane_b = vdp.get_plane_b_nametable();
    assert_eq!(plane_b, 0xE000);

    // Set register 3 (Window nametable): address 0xF800, but H40 (the
    // default) ignores bit 11
    vdp.set_test_register(3, 0b00111110);
    assert_eq!(vdp.get_window_nametable(), 0xF000);

    // H32
    vdp.set_test_register(12, 0x00);
    assert_eq!(vdp.get_window_nametable(), 0xF800);
}

#[test]
fn test_sprite_count_debug_counts_the_link_chain() {
    let mut emu = setup_vdp_test();
    let vdp = emu.vdp_mut();
    vdp.set_test_register(5, 0b01111100); // SAT at 0xF800 (H40)
    let sat = 0xF800;

    // An empty table still holds sprite 0, whose link of 0 ends the chain.
    assert_eq!(vdp.get_sprite_count_debug(), 1);

    // Sprite 0 links to 1 and sprite 1 to 2; sprite 2's link of 0 ends it.
    vdp.get_vram_mut()[sat + 3] = 1;
    vdp.get_vram_mut()[sat + 8 + 3] = 2;
    assert_eq!(vdp.get_sprite_count_debug(), 3);

    // A link past the table's 80 sprites ends the chain too.
    vdp.get_vram_mut()[sat + 16 + 3] = 90;
    assert_eq!(vdp.get_sprite_count_debug(), 3);
}

#[test]
fn test_tile_rgba_all_palettes() {
    let mut emu = setup_vdp_test();

    let vdp = emu.vdp_mut();

    // Set different colors in each palette
    for pal in 0..4 {
        vdp.get_cram_mut()[pal * 16 + 1] = match pal {
            0 => 0x000E, // Red
            1 => 0x00E0, // Green
            2 => 0x0E00, // Blue
            _ => 0x0EEE, // White
        };
    }

    // Write a simple tile (all pixels = color 1)
    for i in 0..32 {
        vdp.get_vram_mut()[i] = 0x11; // All nibbles = 1
    }

    // Get tile with different palettes
    let rgba_pal0 = vdp.get_tile_rgba(0, 0);
    let rgba_pal1 = vdp.get_tile_rgba(0, 1);
    let rgba_pal2 = vdp.get_tile_rgba(0, 2);

    // Verify we got data for each palette
    assert_eq!(rgba_pal0.len(), 256);
    assert_eq!(rgba_pal1.len(), 256);
    assert_eq!(rgba_pal2.len(), 256);

    // Success: All palettes render without crashing
}

#[test]
fn test_vdp_debugger_during_execution() {
    let mut emu = setup_vdp_test();

    // Run a few frames
    emu.debugger().set_execution_mode(ExecutionMode::Running);
    for _ in 0..5 {
        emu.run_frame();
    }

    // VDP debugger should still work
    let vdp = emu.vdp();

    let vram = vdp.get_vram();
    assert_eq!(vram.len(), 0x10000);

    let cram = vdp.get_cram();
    assert_eq!(cram.len(), 64);

    let palettes = vdp.get_all_palettes_rgba();
    assert_eq!(palettes.len(), 256);
}
