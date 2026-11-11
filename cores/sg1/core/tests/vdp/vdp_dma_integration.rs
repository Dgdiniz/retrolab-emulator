//! Integration tests for VDP DMA operations.
//!
//! Tests all three types of DMA:
//! 1. Memory to VRAM
//! 2. VRAM Fill
//! 3. VRAM Copy

use sg1_core::vdp::Vdp;

// Helper to create test data in "CPU memory"
fn create_test_memory(size: usize) -> Vec<u8> {
    let mut mem = vec![0u8; size];
    // Fill with test pattern
    for (i, byte) in mem.iter_mut().enumerate() {
        *byte = (i & 0xFF) as u8;
    }
    mem
}

/// R1 with the display off and Mode 5, DMA enabled or not (bit 4).
const R1_DMA_ON: u8 = 0x14;
const R1_DMA_OFF: u8 = 0x04;

/// What a DMA does, and where it reads.
enum Dma {
    /// Copy from 68000 memory at this byte address.
    Memory(u32),
    /// Fill with the value written to the data port.
    Fill,
    /// Copy from this VRAM address.
    Copy(u16),
}

/// Sets the registers of a DMA (using test API): the length in words (R19,
/// R20), the source and the kind of DMA (R21 to R23: a word address for 68000
/// memory, a byte address for a VRAM copy, and the kind in R23's top two
/// bits), the auto-increment (R15) and R1.
fn setup_dma_registers(vdp: &mut Vdp, dma: Dma, length: u16, increment: u8, r1: u8) {
    let (r21, r22, r23) = match dma {
        Dma::Memory(source) => {
            let word = source >> 1;
            (word as u8, (word >> 8) as u8, ((word >> 16) & 0x3F) as u8)
        }
        Dma::Fill => (0, 0, 0x80),
        Dma::Copy(source) => (source as u8, (source >> 8) as u8, 0xC0),
    };
    vdp.set_test_register(19, length as u8);
    vdp.set_test_register(20, (length >> 8) as u8);
    vdp.set_test_register(21, r21);
    vdp.set_test_register(22, r22);
    vdp.set_test_register(23, r23);
    vdp.set_test_register(15, increment);
    vdp.set_test_register(1, r1);
}

// Helper to set VRAM write address (using test API)
fn set_vram_write(vdp: &mut Vdp, address: u16) {
    vdp.set_test_access(address, 0x01); // code 0x01 = VRAM write
}

#[test]
fn test_dma_memory_to_vram_simple() {
    let mut vdp = Vdp::new();
    let cpu_memory = create_test_memory(0x10000);

    // Source: 0x1000, Length: 16 words
    setup_dma_registers(&mut vdp, Dma::Memory(0x1000), 16, 2, R1_DMA_ON);
    set_vram_write(&mut vdp, 0x0000);
    vdp.start_dma(&cpu_memory);

    assert!(vdp.is_dma_active());
}

#[test]
fn test_dma_memory_to_vram_large() {
    let mut vdp = Vdp::new();
    let mut cpu_memory = vec![0u8; 0x100000];

    for i in 0..1024 {
        cpu_memory[0x2000 + i] = (i & 0xFF) as u8;
    }

    // Source: 0x2000, Length: 512 words
    setup_dma_registers(&mut vdp, Dma::Memory(0x2000), 512, 2, R1_DMA_ON);
    set_vram_write(&mut vdp, 0x1000);
    vdp.start_dma(&cpu_memory);

    assert!(vdp.is_dma_active());
}

#[test]
fn test_dma_vram_fill_constant() {
    let mut vdp = Vdp::new();
    let cpu_memory = vec![0u8; 0x10000];

    // VRAM Fill: Length: 128 words
    setup_dma_registers(&mut vdp, Dma::Fill, 128, 1, R1_DMA_ON);
    set_vram_write(&mut vdp, 0x0000);

    // Write fill value to data port
    vdp.write(0xC00000, 0xAA);

    vdp.start_dma(&cpu_memory);
    assert!(vdp.is_dma_active());
}

#[test]
fn test_dma_vram_copy_internal() {
    let mut vdp = Vdp::new();
    let cpu_memory = vec![0u8; 0x10000];

    // First, write some data to VRAM source location
    set_vram_write(&mut vdp, 0x0000);
    for i in 0..64 {
        vdp.write(0xC00000, i);
    }

    // VRAM Copy: Source: 0x0, Length: 64 words
    setup_dma_registers(&mut vdp, Dma::Copy(0x0000), 64, 1, R1_DMA_ON);
    set_vram_write(&mut vdp, 0x1000);

    vdp.start_dma(&cpu_memory);
    assert!(vdp.is_dma_active());
}

#[test]
fn test_dma_cpu_blocked_during_dma() {
    let mut vdp = Vdp::new();
    let cpu_memory = create_test_memory(0x10000);

    setup_dma_registers(&mut vdp, Dma::Memory(0x1000), 16, 2, R1_DMA_ON);
    set_vram_write(&mut vdp, 0x0000);
    vdp.start_dma(&cpu_memory);

    assert!(vdp.dma_state().cpu_blocked);
}

#[test]
fn test_dma_timing_cycles_vblank() {
    let mut vdp = Vdp::new();
    let cpu_memory = create_test_memory(0x10000);

    vdp.scanline(224); // Enter VBLANK

    setup_dma_registers(&mut vdp, Dma::Memory(0x1000), 100, 2, R1_DMA_ON);
    set_vram_write(&mut vdp, 0x0000);
    vdp.start_dma(&cpu_memory);

    // During VBLANK: 2 cycles/word, 100 words * 2 = 200 cycles
    assert_eq!(vdp.dma_state().cycles_remaining, 200);
}

#[test]
fn test_dma_timing_cycles_active_display() {
    let mut vdp = Vdp::new();
    let cpu_memory = create_test_memory(0x10000);

    vdp.scanline(100); // Active display

    setup_dma_registers(&mut vdp, Dma::Memory(0x1000), 100, 2, R1_DMA_ON);
    set_vram_write(&mut vdp, 0x0000);
    vdp.start_dma(&cpu_memory);

    // During active display: 16 cycles/word, 100 words * 16 = 1600 cycles
    assert_eq!(vdp.dma_state().cycles_remaining, 1600);
}

#[test]
fn test_dma_execute_cycles_completes() {
    let mut vdp = Vdp::new();
    let cpu_memory = create_test_memory(0x10000);

    setup_dma_registers(&mut vdp, Dma::Memory(0x1000), 10, 2, R1_DMA_ON);
    set_vram_write(&mut vdp, 0x0000);
    vdp.start_dma(&cpu_memory);

    assert!(vdp.is_dma_active());

    let completed = vdp.execute_dma_cycles(1000);

    assert!(completed);
    assert!(!vdp.is_dma_active());
}

#[test]
fn test_dma_execute_cycles_partial() {
    let mut vdp = Vdp::new();
    let cpu_memory = create_test_memory(0x10000);

    vdp.scanline(224); // VBLANK
    setup_dma_registers(&mut vdp, Dma::Memory(0x1000), 256, 2, R1_DMA_ON);
    set_vram_write(&mut vdp, 0x0000);
    vdp.start_dma(&cpu_memory);

    let initial_cycles = vdp.dma_state().cycles_remaining;

    let completed = vdp.execute_dma_cycles(100);

    assert!(!completed);
    assert!(vdp.is_dma_active());
    assert_eq!(vdp.dma_state().cycles_remaining, initial_cycles - 100);
}

#[test]
fn test_dma_load_tiles_integration() {
    let mut vdp = Vdp::new();
    let mut cpu_memory = vec![0u8; 0x100000];

    // Create tile data (32 bytes per tile)
    let tile_data = vec![
        0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22, 0x22,
        0x22, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x33, 0x44, 0x44, 0x44, 0x44, 0x44, 0x44,
        0x44, 0x44,
    ];
    cpu_memory[0x1000..0x1000 + tile_data.len()].copy_from_slice(&tile_data);

    // Source: 0x1000, Length: 16 words (32 bytes)
    setup_dma_registers(&mut vdp, Dma::Memory(0x1000), 16, 2, R1_DMA_ON);
    set_vram_write(&mut vdp, 0x0000);
    vdp.start_dma(&cpu_memory);

    assert!(vdp.is_dma_active());
    assert_eq!(vdp.get_vram()[0..32], tile_data[..]);
}

#[test]
fn test_dma_load_nametable_integration() {
    let mut vdp = Vdp::new();
    let mut cpu_memory = vec![0u8; 0x100000];

    // Create nametable entries
    let nametable_size = 64 * 32 * 2; // 64x32 plane
    for i in 0..nametable_size / 2 {
        let entry = (i % 256) as u16;
        cpu_memory[0x2000 + i * 2] = (entry >> 8) as u8;
        cpu_memory[0x2000 + i * 2 + 1] = (entry & 0xFF) as u8;
    }

    // Source: 0x2000, Length: 4096 words (a 64x32 plane is 2048)
    setup_dma_registers(&mut vdp, Dma::Memory(0x2000), 4096, 2, R1_DMA_ON);
    set_vram_write(&mut vdp, 0x2000);
    vdp.start_dma(&cpu_memory);

    assert!(vdp.is_dma_active());
}

#[test]
fn test_dma_vblank_faster_than_active() {
    let mut vdp = Vdp::new();
    let cpu_memory = create_test_memory(0x10000);

    // Test during active display
    vdp.scanline(100);
    setup_dma_registers(&mut vdp, Dma::Memory(0x1000), 100, 2, R1_DMA_ON);
    set_vram_write(&mut vdp, 0x0000);
    vdp.start_dma(&cpu_memory);
    let active_cycles = vdp.dma_state().cycles_remaining;

    // Test during VBLANK
    let mut vdp2 = Vdp::new();
    vdp2.scanline(224);
    setup_dma_registers(&mut vdp2, Dma::Memory(0x1000), 100, 2, R1_DMA_ON);
    set_vram_write(&mut vdp2, 0x0000);
    vdp2.start_dma(&cpu_memory);
    let vblank_cycles = vdp2.dma_state().cycles_remaining;

    // VBLANK should be 8x faster
    assert_eq!(active_cycles, vblank_cycles * 8);
}

#[test]
fn test_dma_disabled_no_operation() {
    let mut vdp = Vdp::new();
    let cpu_memory = create_test_memory(0x10000);

    // DMA DISABLED (R1 bit 4 = 0)
    setup_dma_registers(&mut vdp, Dma::Memory(0x1000), 16, 2, R1_DMA_OFF);
    set_vram_write(&mut vdp, 0x0000);
    vdp.start_dma(&cpu_memory);

    // DMA should not activate
    assert!(!vdp.is_dma_active());
}

#[test]
fn test_dma_during_render_integration() {
    let mut vdp = Vdp::new();
    let cpu_memory = create_test_memory(0x10000);

    // Simulate DMA during active display (slower)
    vdp.scanline(50);

    setup_dma_registers(&mut vdp, Dma::Memory(0x1000), 32, 2, R1_DMA_ON);
    set_vram_write(&mut vdp, 0x0000);
    vdp.start_dma(&cpu_memory);

    assert!(vdp.is_dma_active());
    // During active display, should take longer
    assert!(vdp.dma_state().cycles_remaining > 100);
}

#[test]
fn test_dma_fill_pattern() {
    let mut vdp = Vdp::new();
    let cpu_memory = vec![0u8; 0x10000];

    // Fill with 0xFF
    setup_dma_registers(&mut vdp, Dma::Fill, 64, 1, R1_DMA_ON);
    set_vram_write(&mut vdp, 0x0000);
    vdp.write(0xC00000, 0xFF);
    vdp.start_dma(&cpu_memory);

    assert!(vdp.is_dma_active());
}
