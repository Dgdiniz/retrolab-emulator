//! Unit tests of the VDP's copy of the sprite table's first words: every
//! way of writing VRAM reaches it, moving the table leaves it, the sprite
//! list follows a write into the table from the line after next, and a
//! load and a reset give it back.

use super::*;

/// Where [`vdp_with_a_sprite`] keeps its sprite table, and another table
/// the tests move it to.
const TABLE: u16 = 0xF000;
const OTHER_TABLE: u16 = 0xE000;

/// The four words of a sprite table entry, as bytes.
fn entry(words: [u16; 4]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_be_bytes()).collect()
}

/// A VDP with the display on, 40 cells wide, its planes hidden, its sprite
/// table at [`TABLE`], cells 1 to 4 every pixel colour 1 and cells 8 to 11
/// colour 2, and sprite 0 four cells tall (lines 0 to 31) at x 0, drawn
/// with cells 1 to 4, ending the chain.
fn vdp_with_a_sprite() -> Vdp {
    let mut vdp = Vdp::new();
    vdp.set_test_register(1, 0x44); // display on, mode 5
    vdp.set_test_register(12, 0x81); // 40 cells
    vdp.set_test_register(5, (TABLE >> 9) as u8);
    vdp.set_test_register(15, 2);
    vdp.set_layer_debug_disabled(LAYER_PLANE_A, true);
    vdp.set_layer_debug_disabled(LAYER_PLANE_B, true);
    vdp.set_layer_debug_disabled(LAYER_WINDOW, true);
    vdp.write_vram_debug(32, &[0x11; 128]);
    vdp.write_vram_debug(256, &[0x22; 128]);
    vdp.write_vram_debug(u32::from(TABLE), &entry([128, 0x0300, 0x0001, 128]));
    vdp
}

/// A VRAM write at `address` set up through the control port.
fn write_vram_from(vdp: &mut Vdp, address: u16) {
    vdp.write_word_port(0x04, 0x4000 | (address & 0x3FFF));
    vdp.write_word_port(0x04, address >> 14);
}

/// The palette index drawn at `x` on the last line drawn.
fn drawn(vdp: &Vdp, x: usize) -> u8 {
    vdp.line_index[x]
}

#[test]
fn a_port_write_into_the_table_shows_from_the_line_after_next() {
    let mut vdp = vdp_with_a_sprite();
    for line in 0..=10 {
        vdp.scanline(line);
    }
    assert_eq!((drawn(&vdp, 0), drawn(&vdp, 16)), (1, 0), "line 10");

    // While line 10 runs, sprite 0's X written through the ports: 16 dots
    // to the right.
    write_vram_from(&mut vdp, TABLE + 6);
    vdp.write_word_port(0x00, 128 + 16);
    assert!(vdp.sprite_list_stale);

    vdp.scanline(11);
    assert_eq!(
        (drawn(&vdp, 0), drawn(&vdp, 16)),
        (1, 0),
        "line 11 draws from the list taken as line 10 began"
    );
    vdp.scanline(12);
    assert_eq!(
        (drawn(&vdp, 0), drawn(&vdp, 16)),
        (0, 1),
        "line 12 from the list taken as line 11 began"
    );
    assert!(!vdp.sprite_list_stale);
}

#[test]
fn a_write_of_the_y_takes_the_sprite_off_the_lines_after_next() {
    let mut vdp = vdp_with_a_sprite();
    for line in 0..=4 {
        vdp.scanline(line);
    }
    // Sprite 0 moved to line 100 while line 4 runs.
    write_vram_from(&mut vdp, TABLE);
    vdp.write_word_port(0x00, 128 + 100);
    assert_eq!(
        vdp.sprite_heads[..2],
        (128u16 + 100).to_be_bytes(),
        "the copy takes the word"
    );
    vdp.scanline(5);
    assert_eq!(drawn(&vdp, 0), 1, "line 5");
    vdp.scanline(6);
    assert_eq!(drawn(&vdp, 0), 0, "line 6");
}

#[test]
fn every_way_of_writing_the_table_reaches_the_copy() {
    let table = usize::from(TABLE);
    let heads_are_the_table = |vdp: &Vdp| vdp.sprite_heads[..4] == vdp.vram[table..table + 4];

    // A fill: the high byte of the word into each byte from the second.
    let mut vdp = vdp_with_a_sprite();
    vdp.sprite_list_stale = false;
    vdp.code = 0x01;
    vdp.set_test_register(15, 1);
    vdp.execute_dma_vram_fill(TABLE, 0xAB00, 4);
    assert_eq!(vdp.vram[table + 1], 0xAB);
    assert!(heads_are_the_table(&vdp) && vdp.sprite_list_stale, "a fill");

    // A copy from VRAM.
    let mut vdp = vdp_with_a_sprite();
    vdp.sprite_list_stale = false;
    vdp.vram[0x2000..0x2004].copy_from_slice(&[0x01, 0x23, 0x0C, 0x05]);
    vdp.set_test_register(15, 1);
    vdp.execute_dma_vram_copy(0x2000, TABLE, 4);
    assert_eq!(vdp.vram[table..table + 4], [0x01, 0x23, 0x0C, 0x05]);
    assert!(heads_are_the_table(&vdp) && vdp.sprite_list_stale, "a copy");

    // A transfer from 68000 memory.
    let mut vdp = vdp_with_a_sprite();
    vdp.sprite_list_stale = false;
    vdp.set_test_register(15, 2);
    vdp.set_test_register(19, 2);
    vdp.set_test_register(20, 0);
    vdp.address = TABLE;
    vdp.code = 0x01;
    vdp.dma_pending = true;
    vdp.execute_pending_dma(&[0x00, 0xC8, 0x05, 0x01]);
    assert_eq!(vdp.vram[table..table + 4], [0x00, 0xC8, 0x05, 0x01]);
    assert!(
        heads_are_the_table(&vdp) && vdp.sprite_list_stale,
        "a transfer"
    );

    // The pattern and X words make the list stale, and stay out of the
    // copy; a write outside the table does neither.
    let mut vdp = vdp_with_a_sprite();
    vdp.sprite_list_stale = false;
    let before = vdp.sprite_heads.clone();
    write_vram_from(&mut vdp, TABLE + 4);
    vdp.write_word_port(0x00, 0x0002);
    assert!(vdp.sprite_heads == before && vdp.sprite_list_stale);
    vdp.sprite_list_stale = false;
    write_vram_from(&mut vdp, TABLE - 2);
    vdp.write_word_port(0x00, 0xFFFF);
    assert!(!vdp.sprite_list_stale, "the word before the table");
}

#[test]
fn moving_the_table_keeps_the_copy_of_its_first_words() {
    let mut vdp = vdp_with_a_sprite();
    // The other table written while it is not the table: sprite 0 on lines
    // 100 to 131, at x 160, drawn with cells 8 to 11.
    vdp.write_vram_debug(
        u32::from(OTHER_TABLE),
        &entry([128 + 100, 0x0300, 0x0008, 128 + 160]),
    );
    assert_eq!(
        vdp.sprite_heads[..4],
        entry([128, 0x0300, 0, 0])[..4],
        "a write outside the table leaves the copy"
    );

    for line in 0..=5 {
        vdp.scanline(line);
    }
    // The table moved while line 5 runs.
    vdp.write_word_port(0x04, 0x8500 | (OTHER_TABLE >> 9));
    assert!(vdp.sprite_list_stale);
    vdp.scanline(6);
    assert_eq!((drawn(&vdp, 0), drawn(&vdp, 160)), (1, 0), "line 6");
    vdp.scanline(7);
    // The Y, size and link from the copy (lines 0 to 31), the pattern and
    // X from the table now there: cell 8 at x 160.
    assert_eq!((drawn(&vdp, 0), drawn(&vdp, 160)), (0, 2), "line 7");

    // The debugger reads the sprite as the renderer does.
    let (slot, sprite) = vdp.sprite_table_chain()[0];
    assert_eq!(
        (slot, sprite.y, sprite.x, sprite.pattern_base),
        (0, 0, 160, 8)
    );
}

#[test]
fn a_load_takes_the_copy_from_the_table_and_a_reset_clears_it() {
    let vdp = vdp_with_a_sprite();
    let mut loaded = Vdp::new();
    loaded.from_savestate(&vdp.to_savestate());
    assert_eq!(loaded.sprite_heads, vdp.sprite_heads);
    assert!(!loaded.sprite_list_stale);

    let mut vdp = vdp;
    vdp.reset();
    assert!(vdp.sprite_heads.iter().all(|&byte| byte == 0));
    assert!(!vdp.sprite_list_stale);
}
