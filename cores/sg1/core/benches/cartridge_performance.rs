//! Cartridge Performance Benchmarks
//!
//! Benchmarks for cartridge operations to ensure acceptable performance.
//! Exercises the cartridge (`GenesisCartridge`) surface: bus reads/writes
//! and the SRAM + coprocessor savestate path, and the 68000's own path to
//! the cartridge through `AddressBus` (`Addressable::read`, `read_word` and
//! `write`), which every instruction fetch takes.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use retro_core::cartridge::Cartridge;
use retro_core::cpu::Addressable;
use sg1_core::bus::AddressBus;
use sg1_core::cartridge::build_genesis_cartridge;

/// Genesis NTSC master clock, for rebinding coprocessor clocks on restore.
const MASTER_CLOCK_HZ: u64 = 53_693_175;

/// Helper to create a test ROM
fn create_test_rom_simple(size: usize, has_sram: bool) -> Vec<u8> {
    let mut rom = vec![0xFF; size.max(0x200)];

    // Genesis header (minimal): the domestic and overseas names (48 bytes
    // each), the 14-byte serial and the region
    rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
    for name in [0x120..0x150, 0x150..0x180] {
        rom[name.clone()].fill(b' ');
        rom[name.start..name.start + 9].copy_from_slice(b"BENCH ROM");
    }
    rom[0x180..0x18E].copy_from_slice(b"GM 00000000-00");
    rom[0x1F0..0x1F3].copy_from_slice(b"JUE");

    // ROM range
    let rom_end = (size - 1) as u32;
    rom[0x1A4..0x1A8].copy_from_slice(&rom_end.to_be_bytes());

    // SRAM if requested
    if has_sram {
        rom[0x1A8] = 0xE0; // RAM indicator
        rom[0x1A9] = 0x20; // Start 0x200001
        rom[0x1AA] = 0x00;
        rom[0x1AB] = 0x01;
        rom[0x1AC] = 0x00; // End 0x203FFF (8KB * 2 for odd-byte)
        rom[0x1AD] = 0x20;
        rom[0x1AE] = 0x3F;
        rom[0x1AF] = 0xFF;
    }

    rom
}

fn bench_rom_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("cartridge_rom_read");

    for size in [0x80000, 0x200000, 0x400000].iter() {
        let rom = create_test_rom_simple(*size, false);
        let mut cart = build_genesis_cartridge(rom).unwrap();

        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{}KB", size / 1024)),
            size,
            |b, _| {
                b.iter(|| {
                    // Read from various addresses
                    for addr in (0..10000).step_by(100) {
                        black_box(cart.read_bus(addr, 0, 0xFF));
                    }
                })
            },
        );
    }

    group.finish();
}

fn bench_sram_write(c: &mut Criterion) {
    let rom = create_test_rom_simple(0x80000, true);
    let mut cart = build_genesis_cartridge(rom).unwrap();

    c.bench_function("cartridge_sram_write", |b| {
        b.iter(|| {
            // Write 512 bytes to SRAM, one at each odd address of 1 KB
            for i in 0..512 {
                let addr = 0x200001 + (i * 2); // Odd-byte addressing
                cart.write_bus(addr, black_box((i & 0xFF) as u8), 0);
            }
        })
    });
}

fn bench_sram_read(c: &mut Criterion) {
    let rom = create_test_rom_simple(0x80000, true);
    let mut cart = build_genesis_cartridge(rom).unwrap();

    // Pre-fill SRAM
    for i in 0..512 {
        let addr = 0x200001 + (i * 2);
        cart.write_bus(addr, (i & 0xFF) as u8, 0);
    }

    c.bench_function("cartridge_sram_read", |b| {
        b.iter(|| {
            // Read 512 bytes from SRAM, one at each odd address of 1 KB
            for i in 0..512 {
                let addr = 0x200001 + (i * 2);
                black_box(cart.read_bus(addr, 0, 0xFF));
            }
        })
    });
}

fn bench_save_state(c: &mut Criterion) {
    let rom = create_test_rom_simple(0x80000, true);
    let mut cart = build_genesis_cartridge(rom).unwrap();

    // Write some data to SRAM
    for i in 0..100 {
        let addr = 0x200001 + (i * 2);
        cart.write_bus(addr, (i & 0xFF) as u8, 0);
    }

    c.bench_function("cartridge_save_state", |b| {
        b.iter(|| {
            // The composable savestate is (SRAM image, typed coprocessor states).
            black_box((cart.sram().to_vec(), cart.coprocessors_savestate()));
        })
    });
}

fn bench_load_state(c: &mut Criterion) {
    let rom = create_test_rom_simple(0x80000, true);
    let mut cart = build_genesis_cartridge(rom.clone()).unwrap();

    // Populate then snapshot the state we will restore each iteration.
    for i in 0..100 {
        let addr = 0x200001 + (i * 2);
        cart.write_bus(addr, (i & 0xFF) as u8, 0);
    }
    let sram = cart.sram().to_vec();
    let coprocessors = cart.coprocessors_savestate();

    c.bench_function("cartridge_load_state", |b| {
        b.iter(|| {
            let mut cart2 = build_genesis_cartridge(rom.clone()).unwrap();
            cart2.restore_sram(&sram);
            cart2.restore_coprocessors_savestate(&coprocessors, MASTER_CLOCK_HZ);
            black_box(cart2.sram().len());
        })
    });
}

fn bench_get_save_ram(c: &mut Criterion) {
    let rom = create_test_rom_simple(0x80000, true);
    let cart = build_genesis_cartridge(rom).unwrap();

    c.bench_function("cartridge_get_save_ram", |b| {
        b.iter(|| {
            black_box(cart.sram());
        })
    });
}

fn bench_set_save_ram(c: &mut Criterion) {
    let rom = create_test_rom_simple(0x80000, true);
    let mut cart = build_genesis_cartridge(rom).unwrap();
    let sram_data = vec![0x42; 0x2000];

    c.bench_function("cartridge_set_save_ram", |b| {
        b.iter(|| {
            cart.restore_sram(black_box(&sram_data));
        })
    });
}

/// A bus with the cartridge `rom` builds plugged in.
fn bus_with(rom: Vec<u8>) -> AddressBus {
    let mut bus = AddressBus::new();
    bus.set_cartridge(build_genesis_cartridge(rom).unwrap());
    bus
}

/// The 68000's reads of the cartridge window: a word and a byte at each of
/// 256 addresses spread over `span` bytes from `base`.
fn read_through_the_bus(bus: &mut AddressBus, base: u32, span: u32) -> u32 {
    let step = (span / 256) & !1;
    let mut sum = 0u32;
    for i in 0..256 {
        let addr = base + i * step;
        sum = sum.wrapping_add(u32::from(bus.read_word(addr)));
        sum = sum.wrapping_add(u32::from(bus.read(addr + 1)));
    }
    sum
}

fn bench_bus_cartridge(c: &mut Criterion) {
    let mut group = c.benchmark_group("bus_cartridge");

    // A plain 2 MiB ROM: one ROM slot, the plan resolved on every access.
    let mut plain = bus_with(create_test_rom_simple(0x200000, false));
    group.bench_function("plain_2mib_rom", |b| {
        b.iter(|| black_box(read_through_the_bus(&mut plain, 0, 0x200000)))
    });

    // A 5 MiB ROM: the SEGA mapper answers the whole window through its banks.
    let mut sega = bus_with(create_test_rom_simple(0x500000, false));
    sega.write(0xA130F7, 9);
    group.bench_function("sega_mapper_5mib_rom", |b| {
        b.iter(|| black_box(read_through_the_bus(&mut sega, 0, 0x400000)))
    });

    // An SVP cartridge: its DRAM window, a chip window ahead of the ROM slot.
    let mut svp_rom = create_test_rom_simple(0x200000, false);
    svp_rom[0x1C8] = b'S';
    svp_rom[0x1C9] = b'V';
    let mut svp = bus_with(svp_rom);
    group.bench_function("svp_dram", |b| {
        b.iter(|| black_box(read_through_the_bus(&mut svp, 0x300000, 0x20000)))
    });

    // A 4 MiB ROM with SRAM inside its window, switched on through $A130F1:
    // reads and writes of the SRAM slot ahead of the ROM slot.
    let mut sram = bus_with(create_test_rom_simple(0x400000, true));
    sram.write(0xA130F1, 1);
    group.bench_function("sram_switched_on", |b| {
        b.iter(|| {
            for i in 0..256u32 {
                sram.write(0x200001 + i * 2, black_box(i as u8));
            }
            black_box(read_through_the_bus(&mut sram, 0x200000, 0x400))
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_bus_cartridge,
    bench_rom_read,
    bench_sram_write,
    bench_sram_read,
    bench_save_state,
    bench_load_state,
    bench_get_save_ram,
    bench_set_save_ram,
);

criterion_main!(benches);
