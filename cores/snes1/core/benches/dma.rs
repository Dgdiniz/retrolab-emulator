//! HDMA-per-frame benchmark.
//!
//! Measures the scheduler-side HDMA cost of a frame with three
//! channels active, the shape a game uses for a Mode 7 roto-zoom plus
//! a colour gradient plus a windowed effect:
//!
//! - **Channel 0** (direct, mode 0): a per-line byte gradient into
//!   COLDATA (`$2132`), the cheapest "1 byte/line" table walk.
//! - **Channel 1** (direct, mode 2): a per-line 16-bit value into the
//!   Mode 7 matrix register M7A (`$211B`), two table bytes per line.
//! - **Channel 2** (indirect, mode 0): a per-line byte into CGDATA
//!   (`$2122`) sourced through the indirect pointer, exercising the
//!   pointer-fetch path in `hdma_read_table`.
//!
//! The loop drives [`SnesBus::hdma_init_frame`] once and
//! [`SnesBus::hdma_run_line`] for every visible scanline, the exact
//! pair `system.rs` invokes from the `HdmaInit` / `HdmaTrigger`
//! scheduler events.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use retro_core::cpu::Addressable;
use snes1_core::SnesBus;

const VISIBLE_LINES: u16 = 224;

fn ch_reg(ch: u8, offset: u8) -> u32 {
    0x4300 | (u32::from(ch) << 4) | u32::from(offset)
}

fn program_channel(bus: &mut SnesBus, ch: u8, params: u8, dest: u8, table: u16) {
    bus.write(ch_reg(ch, 0x0), params);
    bus.write(ch_reg(ch, 0x1), dest);
    bus.write(ch_reg(ch, 0x2), (table & 0xFF) as u8);
    bus.write(ch_reg(ch, 0x3), (table >> 8) as u8);
    bus.write(ch_reg(ch, 0x4), 0x7E); // table bank = WRAM $7E
}

fn poke_wram(bus: &mut SnesBus, offset: u16, bytes: &[u8]) {
    for (i, &b) in bytes.iter().enumerate() {
        bus.write(0x7E_0000 + u32::from(offset) + i as u32, b);
    }
}

/// Direct HDMA table covering `lines` scanlines with `bytes_per_line`
/// data bytes each, every entry flagged repeat (bit 7) so a fresh
/// value transfers on every line. Terminated with `0x00`.
fn direct_table(lines: u16, bytes_per_line: u8, step: u8) -> Vec<u8> {
    let mut table = Vec::new();
    let mut remaining = lines;
    let mut value = 0u8;
    while remaining > 0 {
        let n = remaining.min(127) as u8;
        table.push(0x80 | n);
        for _ in 0..n {
            for _ in 0..bytes_per_line {
                table.push(value);
                value = value.wrapping_add(step);
            }
        }
        remaining -= u16::from(n);
    }
    table.push(0x00);
    table
}

/// Indirect HDMA table covering `lines` scanlines (mode 0 -> one byte
/// per line) plus the contiguous data block its pointers reference.
/// Returns `(table, data)`; the data lives at `data_base` in the same
/// bank.
fn indirect_table(lines: u16, data_base: u16) -> (Vec<u8>, Vec<u8>) {
    let mut table = Vec::new();
    let mut data = Vec::new();
    let mut remaining = lines;
    let mut ptr = data_base;
    let mut value = 0u8;
    while remaining > 0 {
        let n = remaining.min(127) as u8;
        table.push(0x80 | n);
        table.push((ptr & 0xFF) as u8);
        table.push((ptr >> 8) as u8);
        for _ in 0..n {
            data.push(value);
            value = value.wrapping_add(3);
        }
        ptr = ptr.wrapping_add(u16::from(n));
        remaining -= u16::from(n);
    }
    table.push(0x00);
    (table, data)
}

fn setup_hdma_scene() -> SnesBus {
    let mut bus = SnesBus::new();
    // Fresh bus leaves display forced-blank, so the CGRAM access gate
    // is open for the indirect channel's $2122 writes.

    poke_wram(&mut bus, 0x1000, &direct_table(VISIBLE_LINES, 1, 1));
    poke_wram(&mut bus, 0x2000, &direct_table(VISIBLE_LINES, 2, 3));
    let (table2, data2) = indirect_table(VISIBLE_LINES, 0x3800);
    poke_wram(&mut bus, 0x3000, &table2);
    poke_wram(&mut bus, 0x3800, &data2);

    program_channel(&mut bus, 0, 0x00, 0x32, 0x1000); // COLDATA gradient
    program_channel(&mut bus, 1, 0x02, 0x1B, 0x2000); // M7A matrix
    program_channel(&mut bus, 2, 0x40, 0x22, 0x3000); // CGDATA (indirect)
    bus.write(ch_reg(2, 0x7), 0x7E); // indirect data bank

    bus.write(0x0000_420C, 0x07); // HDMAEN: channels 0, 1, 2
    bus
}

fn bench_hdma_frame(c: &mut Criterion) {
    let mut bus = setup_hdma_scene();
    c.bench_function("dma_hdma_frame", |b| {
        b.iter(|| {
            black_box(bus.hdma_init_frame());
            for _ in 0..VISIBLE_LINES {
                black_box(bus.hdma_run_line());
            }
            black_box(bus.ppu().cgram()[0]);
        });
    });
}

fn bench_general_dma_vram_upload(c: &mut Criterion) {
    // A canonical 32 KiB VRAM upload (mode 1, $7E WRAM -> $2118/$2119),
    // re-armed each iteration since run_general_dma drains the count.
    let mut bus = SnesBus::new();
    let src: Vec<u8> = (0..0x8000u32).map(|i| (i & 0xFF) as u8).collect();
    poke_wram(&mut bus, 0x0000, &src[..0x8000.min(src.len())]);
    bus.write(0x0000_2115, 0x80); // VMAIN high-side increment
    c.bench_function("dma_general_vram_32k", |b| {
        b.iter(|| {
            bus.write(0x0000_2116, 0x00);
            bus.write(0x0000_2117, 0x00);
            program_channel(&mut bus, 0, 0x01, 0x18, 0x0000);
            bus.write(ch_reg(0, 0x5), 0x00);
            bus.write(ch_reg(0, 0x6), 0x80); // DAS = 0x8000 (32 KiB)
            black_box(run_general_dma(&mut bus, 0x01));
        });
    });
}

fn run_general_dma(bus: &mut SnesBus, mask: u8) -> u64 {
    bus.write(0x0000_420B, mask);
    let pending = bus.take_pending_general_dma();
    bus.run_general_dma(pending)
}

criterion_group!(benches, bench_hdma_frame, bench_general_dma_vram_upload);
criterion_main!(benches);
