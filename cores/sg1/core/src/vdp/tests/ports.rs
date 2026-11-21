//! Unit tests of the ports: the status word, the control and data ports, the
//! registers, the HV counter, and the access slots the FIFO writes in.

use super::*;

#[test]
fn test_vdp_status_read() {
    let mut vdp = Vdp::new();
    // Read high byte of status (0xC00004)
    let status_high = vdp.read(0xC00004);
    // Read low byte of status (0xC00005)
    let status_low = vdp.read(0xC00005);
    let status = ((status_high as u16) << 8) | (status_low as u16);
    assert_eq!(status & 0x0200, 0x0200); // FIFO empty bit set

    // Set VBLANK
    vdp.timing.in_vblank = true;
    let status_high = vdp.read(0xC00004);
    let status_low = vdp.read(0xC00005);
    let status = ((status_high as u16) << 8) | (status_low as u16);
    assert_eq!(status & 0x0008, 0x0008); // VBLANK bit set
}

#[test]
fn test_control_port_write() {
    let mut vdp = Vdp::new();

    // Write control word (two writes)
    vdp.write(0xC00004, 0x00);
    assert!(vdp.pending_write);

    vdp.write(0xC00004, 0x40);
    assert!(!vdp.pending_write);
}

#[test]
fn test_vram_write() {
    let mut vdp = Vdp::new();

    // Set VRAM write mode directly
    vdp.code = 0x01; // VRAM write
    vdp.address = 0x1000;
    // Default auto-increment is 2 (register 15 = 0x02)

    // Write 16-bit word: high byte goes to even addr, low byte to odd addr
    vdp.write_data_port(0x1234, true);
    assert_eq!(vdp.vram[0x1000], 0x12); // high byte
    assert_eq!(vdp.vram[0x1001], 0x34); // low byte
                                        // Auto-increment by 2
    assert_eq!(vdp.address, 0x1002);

    // Write data at new address
    vdp.write_data_port(0x5678, true);
    assert_eq!(vdp.vram[0x1002], 0x56); // high byte
    assert_eq!(vdp.vram[0x1003], 0x78); // low byte
    assert_eq!(vdp.address, 0x1004);
}

#[test]
fn test_cram_write() {
    let mut vdp = Vdp::new();

    // Set CRAM write mode
    vdp.code = 0x03; // CRAM write
    vdp.address = 0x00;
    vdp.registers[15] = 2; // Auto-increment by 2

    // Write a color (9-bit RGB: 0000 BBB0 GGG0 RRR0)
    vdp.write_data_port(0x0EEE, true); // Bright white

    // Check CRAM
    assert_eq!(vdp.cram[0], 0x0EEE);
    assert_eq!(vdp.address, 2); // Auto-incremented
}

#[test]
fn test_register_write() {
    let mut vdp = Vdp::new();

    // Write to register 7 (backdrop color)
    vdp.write_register(7, 0x3F);
    assert_eq!(vdp.registers[7], 0x3F);

    // Write to invalid register (should be ignored or handled safely)
    vdp.write_register(25, 0xFF);
    // No panic = good
}

#[test]
fn test_display_enable_flag() {
    let mut vdp = Vdp::new();

    // Enable the display (bit 6 of reg 1; reset leaves it clear)
    vdp.set_test_register(1, 0x44); // Display enabled
    assert_eq!(vdp.registers[1] & 0x40, 0x40);

    // Disable display
    vdp.set_test_register(1, 0x04);
    assert_eq!(vdp.registers[1] & 0x40, 0x00);
}

#[test]
fn test_shadow_highlight_mode_register() {
    let mut vdp = Vdp::new();

    // Shadow/Highlight disabled by default
    assert_eq!(vdp.registers[12] & 0x08, 0x00);

    // Enable Shadow/Highlight mode (bit 3 of reg 12)
    vdp.set_test_register(12, 0x89); // H40 + S/H
    assert_eq!(vdp.registers[12] & 0x08, 0x08);
}

#[test]
fn test_interlace_mode_register() {
    let mut vdp = Vdp::new();

    // Interlace disabled by default
    let interlace_mode = (vdp.registers[12] >> 1) & 0x03;
    assert_eq!(interlace_mode, 0);

    // Enable interlace mode 1 (bits 1-2 of reg 12)
    vdp.set_test_register(12, 0x83); // H40 + interlace mode 1
    let interlace_mode = (vdp.registers[12] >> 1) & 0x03;
    assert_eq!(interlace_mode, 1);
}

#[test]
fn the_status_reads_the_odd_field_only_while_interlace_is_on() {
    let mut vdp = Vdp::new();
    let odd_field = |vdp: &Vdp| vdp.get_status_register() & 0x0010 != 0;
    vdp.odd_frame = true;

    // Shadow and highlight (bit 3), the unused bit 4 and bit 2 alone are
    // not interlace: an odd field reads clear.
    for register_12 in [0x81, 0x89, 0x91, 0x99, 0x85] {
        vdp.set_test_register(12, register_12);
        assert!(!odd_field(&vdp), "register 12 = {register_12:#04X}");
    }
    // Interlace (bit 1): in normal resolution, in double resolution, and
    // with shadow and highlight on.
    for register_12 in [0x83, 0x87, 0x8B] {
        vdp.set_test_register(12, register_12);
        assert!(odd_field(&vdp), "register 12 = {register_12:#04X}");
    }
    // The even field reads clear with interlace on.
    vdp.odd_frame = false;
    assert!(!odd_field(&vdp));
}

#[test]
fn test_sprite_overflow_flag() {
    let mut vdp = Vdp::new();

    // Initially no overflow
    assert!(!vdp.sprite_overflow);

    // Set overflow flag manually for testing
    vdp.sprite_overflow = true;

    // Check status register includes overflow bit (bit 6)
    let status = vdp.get_status_register();
    assert_eq!(status & 0x0040, 0x0040);
}

#[test]
fn test_sprite_collision_flag() {
    let mut vdp = Vdp::new();

    // Initially no collision
    assert!(!vdp.sprite_collision);

    // Set collision flag manually for testing
    vdp.sprite_collision = true;

    // Check status register includes collision bit (bit 5)
    let status = vdp.get_status_register();
    assert_eq!(status & 0x0020, 0x0020);
}

#[test]
fn test_status_flags_cleared_on_read() {
    let mut vdp = Vdp::new();

    // Set both sprite flags
    vdp.sprite_overflow = true;
    vdp.sprite_collision = true;

    // Read status register (should clear flags)
    let _ = vdp.read(0xC00004);

    // Flags should be cleared
    assert!(!vdp.sprite_overflow);
    assert!(!vdp.sprite_collision);
}

#[test]
fn test_status_word_is_side_effect_free() {
    let mut vdp = Vdp::new();
    vdp.sprite_overflow = true;
    vdp.sprite_collision = true;

    // `status_word()` returns the same value the port read would
    // compute, but must NOT clear the sprite flags (it is the peek
    // the platform's debugger and lessons rely on).
    let peeked = vdp.status_word();
    assert_eq!(peeked, vdp.get_status_register());
    assert!(vdp.sprite_overflow);
    assert!(vdp.sprite_collision);
}

#[test]
fn test_vram_auto_increment() {
    let mut vdp = Vdp::new();

    // Set auto-increment to 2
    vdp.set_test_register(15, 2);

    // Set VRAM write mode
    vdp.set_test_access(0x1000, 0x01);

    // Write a byte
    vdp.write_data_port(0x42, true);

    // Address should have incremented by 2
    let (addr, _) = vdp.current_access();
    assert_eq!(addr, 0x1002);
}

#[test]
fn test_cram_auto_increment() {
    let mut vdp = Vdp::new();

    // Set auto-increment to 2
    vdp.set_test_register(15, 2);

    // Set CRAM write mode
    vdp.set_test_access(0x00, 0x03);

    // Write a color
    vdp.write_data_port(0x0EEE, true);

    // Address should have incremented by 2
    let (addr, _) = vdp.current_access();
    assert_eq!(addr, 0x02);

    // Check color was written
    assert_eq!(vdp.cram[0], 0x0EEE);
}

#[test]
fn test_hv_v_counter_ntsc_jump() {
    let mut vdp = Vdp::new();
    // NTSC 224-line: V counter counts 0x00..=0xEA then jumps to 0xE5..=0xFF.
    vdp.timing.scanline = 0x00;
    assert_eq!(vdp.hv_v_counter(), 0x00);
    vdp.timing.scanline = 0xEA;
    assert_eq!(vdp.hv_v_counter(), 0xEA);
    vdp.timing.scanline = 0xEB; // 235 - 262 = -27 -> 0xE5
    assert_eq!(vdp.hv_v_counter(), 0xE5);
    vdp.timing.scanline = 261; // last NTSC line -> 0xFF
    assert_eq!(vdp.hv_v_counter(), 0xFF);
}

#[test]
fn test_hv_h_counter_from_beam() {
    let mut vdp = Vdp::new();
    vdp.set_test_register(12, 0x01); // H40
                                     // Beam at line start -> H counter 0.
    vdp.set_hblank_approx(0, 3420);
    assert_eq!(vdp.hv_h_counter(), 0x00);
    // Mid-line stays in the low (pre-gap) segment: 50% -> p=105.
    vdp.set_hblank_approx(1710, 3420);
    assert_eq!(vdp.hv_h_counter(), 105);
    // Beam at line end -> top of the H40 ramp (0xFF, not a wrapped 0x00).
    vdp.set_hblank_approx(3420, 3420);
    assert_eq!(vdp.hv_h_counter(), 0xFF);
}

// ===== Interrupt Tests =====

/// Verify that writing a word through the VDP data port and reading it
/// back through the renderer produces consistent tile attributes.
#[test]
fn test_vram_write_read_roundtrip_priority() {
    let mut vdp = Vdp::new();

    // Set up VRAM write mode: CD0=1 (VRAM write), address=0x1000
    vdp.code = 0x01;
    vdp.address = 0x1000;
    vdp.registers[15] = 2; // auto-increment

    let test_words: &[u16] = &[0x8001, 0x825C, 0xE001, 0x0001, 0x025C, 0xFFFF];

    for &word in test_words {
        vdp.address = 0x1000;
        vdp.write_data_port(word, true);

        // Read back from VRAM using renderer's decode path
        let hi = vdp.vram[0x1000] as u16;
        let lo = vdp.vram[0x1001] as u16;
        let readback = (hi << 8) | lo;

        assert_eq!(
            readback, word,
            "Write/read roundtrip failed for 0x{:04X}: got 0x{:04X}",
            word, readback
        );

        let pri = (readback & 0x8000) != 0;
        let expected_pri = (word & 0x8000) != 0;
        assert_eq!(
            pri, expected_pri,
            "Priority roundtrip failed for 0x{:04X}: pri={} expected={}",
            word, pri, expected_pri
        );
    }
}

/// FNV-1a over a table's entries, so the whole table is pinned by one
/// number.
fn table_hash(table: &[u64]) -> u64 {
    let bytes: Vec<u8> = table.iter().flat_map(|v| v.to_le_bytes()).collect();
    retro_core::fnv1a64(&bytes)
}

#[test]
fn the_fifo_slot_tables_are_the_recorded_ones() {
    // Pinned to the slot times the recorded runs were made with: any
    // change to a slot's time changes these.
    assert_eq!(table_hash(&FIFO_TIMING_H32), 0xd7f0_55cb_1722_762c);
    assert_eq!(table_hash(&FIFO_TIMING_H40), 0x653e_3cb7_7c81_d5a1);
}

#[test]
fn a_line_has_sixteen_slots_in_h32_and_eighteen_in_h40() {
    for (table, slots) in [(&FIFO_TIMING_H32[..], 16), (&FIFO_TIMING_H40[..], 18)] {
        assert_eq!(table.len(), slots + FIFO_SLOTS_OF_NEXT_LINE);
        assert!(table.windows(2).all(|w| w[0] < w[1]), "ascending");
        assert!(table[..slots].iter().all(|&t| t < MASTER_CLOCKS_PER_LINE));
        for k in 0..FIFO_SLOTS_OF_NEXT_LINE {
            assert_eq!(
                table[slots + k],
                MASTER_CLOCKS_PER_LINE + table[k],
                "slot {k} of the next line"
            );
        }
    }
}

#[test]
fn within_the_picture_slots_come_three_in_a_row_every_sixteen_pixels() {
    // Sixteen pixels are 160 master clocks in H32 and 128 in H40; the
    // fourth position of each group is the chip's own refresh.
    let gaps = |table: &[u64], from: usize, count: usize| -> Vec<u64> {
        table[from..from + count]
            .windows(2)
            .map(|w| w[1] - w[0])
            .collect()
    };
    assert_eq!(
        gaps(&FIFO_TIMING_H32, 2, 12),
        [160, 160, 320, 160, 160, 320, 160, 160, 320, 160, 160]
    );
    assert_eq!(
        gaps(&FIFO_TIMING_H40, 1, 15),
        [128, 128, 256, 128, 128, 256, 128, 128, 256, 128, 128, 256, 128, 128]
    );
}

#[test]
fn the_h_counter_takes_one_line_to_come_round() {
    for h40 in [false, true] {
        let origin = if h40 {
            LINE_ORIGIN_H40
        } else {
            LINE_ORIGIN_H32
        };
        let (mut at, mut clocks, mut values) = (origin, 0u64, 0u32);
        loop {
            clocks += dot_master_clocks(h40, at);
            at = next_hcounter(h40, at);
            values += 1;
            if at == origin {
                break;
            }
        }
        assert_eq!(clocks, MASTER_CLOCKS_PER_LINE, "h40={h40}");
        assert_eq!(
            values,
            if h40 { 420 } else { 342 },
            "pixels a line, h40={h40}"
        );
    }
    // Horizontal sync in H40: 32 pixels in 314 master clocks.
    let sync: u64 = (H40_SYNC.0..=H40_SYNC.1)
        .map(|h| dot_master_clocks(true, h))
        .sum();
    assert_eq!(sync, 314);
}

#[test]
fn every_slot_is_at_its_documented_position_but_the_one_in_h40_sync() {
    for &h in &ACCESS_SLOTS_H32 {
        assert!(
            FIFO_TIMING_H32[..16].contains(&master_clocks_to(false, h)),
            "{h:#05x}"
        );
    }
    for &h in &ACCESS_SLOTS_H40 {
        let documented = master_clocks_to(true, h);
        let kept = if h == H40_SYNC_SLOT {
            documented + H40_SYNC_SLOT_LATENESS
        } else {
            documented
        };
        assert!(FIFO_TIMING_H40[..18].contains(&kept), "{h:#05x}");
    }
    assert_eq!(master_clocks_to(true, H40_SYNC_SLOT), 346);
    assert_eq!(FIFO_TIMING_H40[0], 352);
}
