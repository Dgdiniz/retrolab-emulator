//! Unit tests of the DMA: fills, copies and transfers from 68000 memory,
//! their state, and their timing.

use super::*;
use crate::vdp::dma;

#[test]
fn test_dma_vram_fill() {
    let mut vdp = Vdp::new();

    // Set auto-increment to 1 and code to VRAM write
    vdp.set_test_register(15, 1);
    vdp.code = 0x01; // VRAM write

    // Execute VRAM fill (length 16 means 17 iterations per MAME behavior)
    vdp.execute_dma_vram_fill(0x1000, 0xAB00, 16);

    // Check that VRAM was filled (high byte of fill value)
    // VRAM fill writes the high byte to alternating byte positions:
    // Even addr -> writes to odd byte (low byte of word)
    // Odd addr -> writes to even byte (high byte of word)
    for i in 0..16 {
        assert_eq!(
            vdp.vram[0x1000 + i],
            0xAB,
            "VRAM at {:04X} should be 0xAB",
            0x1000 + i
        );
    }
}

#[test]
fn test_dma_vram_copy() {
    let mut vdp = Vdp::new();

    // Set auto-increment to 1
    vdp.set_test_register(15, 1);

    // Set source data
    for i in 0..8 {
        vdp.vram[0x2000 + i] = (0x10 + i) as u8;
    }

    // Execute VRAM copy
    vdp.execute_dma_vram_copy(0x2000, 0x3000, 8);

    // Check that data was copied
    for i in 0..8 {
        assert_eq!(
            vdp.vram[0x3000 + i],
            (0x10 + i) as u8,
            "VRAM at {:04X} should be {:02X}",
            0x3000 + i,
            0x10 + i
        );
    }
}

#[test]
fn test_dma_memory_to_vram() {
    let mut vdp = Vdp::new();

    // Set auto-increment to 2 (word access)
    vdp.set_test_register(15, 2);

    // Create fake CPU memory
    let mut cpu_memory = vec![0u8; 0x10000];
    cpu_memory[0x1000] = 0x12;
    cpu_memory[0x1001] = 0x34;
    cpu_memory[0x1002] = 0x56;
    cpu_memory[0x1003] = 0x78;

    // Execute DMA
    vdp.execute_dma_memory_to_vram(0x1000, 0x0000, 2, &cpu_memory);

    // Check VRAM (big-endian)
    assert_eq!(vdp.vram[0x0000], 0x12);
    assert_eq!(vdp.vram[0x0001], 0x34);
    assert_eq!(vdp.vram[0x0002], 0x56);
    assert_eq!(vdp.vram[0x0003], 0x78);
}

#[test]
fn test_dma_vram_fill_with_auto_increment() {
    let mut vdp = Vdp::new();

    // Set auto-increment to 2 (skip bytes) and code to VRAM write
    vdp.set_test_register(15, 2);
    vdp.code = 0x01; // VRAM write

    // Execute VRAM fill (length 4 -> 5 iterations per MAME)
    vdp.execute_dma_vram_fill(0x0000, 0xCC00, 4);

    // With auto-increment 2, addresses are: 0, 2, 4, 6, 8
    // Even addresses: fill_byte written to odd byte position (addr+1)
    // So: byte[1]=CC, byte[3]=CC, byte[5]=CC, byte[7]=CC, byte[9]=CC
    assert_eq!(vdp.vram[0x0001], 0xCC);
    assert_eq!(vdp.vram[0x0000], 0x00); // High byte of word not filled
    assert_eq!(vdp.vram[0x0003], 0xCC);
    assert_eq!(vdp.vram[0x0002], 0x00);
    assert_eq!(vdp.vram[0x0005], 0xCC);
    assert_eq!(vdp.vram[0x0004], 0x00);
    assert_eq!(vdp.vram[0x0007], 0xCC);
}

#[test]
fn test_vram_fill_byte_trigger_uses_even_data_lane() {
    // Contra's HSCROLL upload does `MOVE.B Dn, $C00000` to start a VRAM
    // fill. The even-address byte must land in the high lane so trailing
    // cells replicate that byte (`value >> 8`), not zero.
    let mut vdp = Vdp::new();
    vdp.set_test_register(1, 0x10); // DMA enable
    vdp.set_test_register(15, 4); // auto-inc 4 (plane-B HSCROLL stride)
    vdp.set_test_register(19, 3); // length = 3 trailing cells after the trigger word
    vdp.set_test_register(20, 0);
    vdp.scanline(224); // VBLANK -> fill_initial_chunk commits the whole transfer
    vdp.address = 0xBA02;
    vdp.code = 0x01; // VRAM write
    vdp.dma_fill_pending = true;
    vdp.dma_busy = true;
    vdp.dma_remaining_clocks = u64::MAX;

    // MOVE.B of 0x0F onto the even data port must become word 0x0F00.
    vdp.write(0xC00000, 0x0F);

    assert_eq!(
        u16::from_be_bytes([vdp.vram[0xBA02], vdp.vram[0xBA03]]),
        0x0F00,
        "trigger word"
    );
    // Even-address fill cells write `value>>8` into the low byte of each
    // subsequent word (as in Genesis Plus GX and MAME). A zero-extended
    // byte trigger would make that byte 0, and every trailing cell 0000.
    for line in 1..=3u16 {
        let off = 0xBA02 + (line as usize) * 4;
        let word = u16::from_be_bytes([vdp.vram[off], vdp.vram[off + 1]]);
        assert_eq!(word, 0x000F, "fill cell at line+{line}");
    }
}

#[test]
fn test_dma_state_tracking() {
    let mut vdp = Vdp::new();

    // DMA should not be active initially
    assert!(!vdp.is_dma_active());

    // Start DMA manually
    vdp.dma.start(
        dma::DmaOperation::VramFill {
            dest: 0,
            value: 0,
            length: 100,
        },
        false,
    );

    // DMA should be active
    assert!(vdp.is_dma_active());

    // Status register should have DMA busy bit (bit 1)
    let status = vdp.get_status_register();
    assert_eq!(status & 0x0002, 0x0002);

    // Complete DMA
    vdp.dma.complete();
    assert!(!vdp.is_dma_active());
}

#[test]
fn test_dma_master_clocks_rate_model() {
    let mut vdp = Vdp::new();
    // Active display, H40 (R1 bit6 = display on, R12 bit0 = H40).
    vdp.set_test_register(1, 0x40);
    vdp.set_test_register(12, 0x01);
    vdp.scanline(100); // active region -> not in vblank
    assert!(!vdp.timing.in_vblank);

    // 18 access-slots/line in H40 active. A transfer of `rate` words always
    // takes exactly one scanline (3420 mclks). 68k->VRAM and Copy cost
    // two slots/word -> rate halves to 9.
    use DmaTransfer::{BusToCramOrVsram, BusToVram, Copy, Fill};
    assert_eq!(vdp.dma_master_clocks(BusToCramOrVsram, 18), 3420); // 1 slot/word
    assert_eq!(vdp.dma_master_clocks(Fill, 18), 3420); // 1 slot/byte
    assert_eq!(vdp.dma_master_clocks(BusToVram, 9), 3420); // 2 slots/word
    assert_eq!(vdp.dma_master_clocks(Copy, 9), 3420); // 2 slots/byte
    assert_eq!(vdp.dma_master_clocks(BusToVram, 18), 6840); // two lines

    // Length 0 means a full 0x10000-word transfer (pre-decrementing counter).
    assert_eq!(vdp.dma_master_clocks(BusToVram, 0), 0x10000u64 * 3420 / 9);

    // H32 active: 16 access-slots/line.
    vdp.set_test_register(12, 0x00);
    assert_eq!(vdp.dma_master_clocks(BusToCramOrVsram, 16), 3420);
    assert_eq!(vdp.dma_master_clocks(BusToVram, 8), 3420);
}

#[test]
fn test_dma_master_clocks_blanking() {
    let mut vdp = Vdp::new();
    // Display off forces the blanking rate even outside vblank.
    vdp.set_test_register(1, 0x00);
    vdp.scanline(100);
    assert!(!vdp.timing.in_vblank);

    // H40 blanking: 204 slots/line; a word to VRAM takes two -> 102.
    use DmaTransfer::{BusToCramOrVsram, BusToVram};
    vdp.set_test_register(12, 0x01);
    assert_eq!(vdp.dma_master_clocks(BusToVram, 102), 3420);
    // 68k->CRAM/VSRAM loses refresh slots: 204 -> 198.
    assert_eq!(vdp.dma_master_clocks(BusToCramOrVsram, 198), 3420);

    // H32 blanking: 166 slots; CRAM/VSRAM loses refresh slots: 166 -> 161.
    vdp.set_test_register(12, 0x00);
    assert_eq!(vdp.dma_master_clocks(BusToCramOrVsram, 161), 3420);
    assert_eq!(vdp.dma_master_clocks(BusToVram, 83), 83 * 3420 / 83); // 166 / 2 = 83
}

#[test]
fn test_dma_busy_master_clocks_beam_aware() {
    let mut vdp = Vdp::new();
    vdp.set_test_register(12, 0x01); // H40
    let active = vdp.timing.visible_scanlines() as u64; // 224 (NTSC)

    // Display OFF blanks every line, so the beam-aware integral has no
    // active-rate lines to cross and collapses to the single blanking-rate
    // duration for every transfer type.
    vdp.set_test_register(1, 0x00);
    vdp.scanline(100);
    for &(transfer, len) in &[
        (DmaTransfer::Fill, 204 * 3),
        (DmaTransfer::Copy, 102 * 5),
        (DmaTransfer::BusToVram, 102 * 4),
        (DmaTransfer::BusToCramOrVsram, 198 * 2),
    ] {
        assert_eq!(
            vdp.dma_busy_master_clocks(transfer, len),
            vdp.dma_master_clocks(transfer, len),
            "display-off DMA should equal the single blanking rate ({transfer:?})"
        );
    }

    // Display ON, large fill kicked off mid-active-display (line 100): the
    // chip drains the remaining active lines at the slow active rate (18) then
    // empties the rest during the ~10x faster VBLANK. The busy window must
    // be far shorter than locking the active rate for the whole transfer,
    // and never shorter than draining it entirely at the blanking rate.
    vdp.set_test_register(1, 0x40);
    vdp.scanline(100);
    assert!(!vdp.timing.in_vblank);
    let len: u32 = 0x1fff;
    let busy = vdp.dma_busy_master_clocks(DmaTransfer::Fill, len);
    let locked_active = vdp.dma_master_clocks(DmaTransfer::Fill, len);
    let pure_blanking = (len as u64) * 3420 / 204;
    assert!(
        busy < locked_active,
        "beam-aware ({busy}) must beat locked active rate ({locked_active})"
    );
    assert!(
        busy >= pure_blanking,
        "beam-aware ({busy}) cannot beat a pure blanking drain ({pure_blanking})"
    );

    // Exact integral: active lines [100, 224) at 18 units/line, then the
    // remainder during VBLANK at 204 units/line (fits before line wrap).
    let active_units = (active - 100) * 18;
    let rem = len as u64 - active_units;
    let expected = (active - 100) * 3420 + (rem / 204) * 3420 + (rem % 204) * 3420 / 204;
    assert_eq!(busy, expected, "beam-aware integral mismatch");
}

/// The sixteen rates of the DMA model: what one scanline carries of each
/// kind of transfer, by width and by whether the line is blanked.
#[test]
fn dma_line_rates_by_transfer_width_and_blanking() {
    use DmaTransfer::{BusToCramOrVsram, BusToVram, Copy, Fill};
    let mut vdp = Vdp::new();
    // Each row: H32 active, H40 active, H32 blanked, H40 blanked.
    let expected = [
        (BusToCramOrVsram, [16, 18, 161, 198]),
        (BusToVram, [8, 9, 83, 102]),
        (Fill, [16, 18, 166, 204]),
        (Copy, [8, 9, 83, 102]),
    ];
    for (transfer, rates) in expected {
        for (case, rate) in rates.into_iter().enumerate() {
            vdp.registers[12] = (case & 1) as u8;
            assert_eq!(
                vdp.dma_line_rate(transfer, case >= 2),
                rate,
                "{transfer:?}, case {case}"
            );
        }
    }
}
