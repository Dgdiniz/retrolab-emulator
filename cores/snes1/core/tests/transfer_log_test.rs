//! Who wrote video memory: the origin of every byte of VRAM and the log of
//! each frame's transfers and palette and sprite-table writes, recorded live
//! while a host asks.
//!
//! The DMA and HDMA cases drive the bus directly, the way `dma_test.rs`
//! does: program the channel registers, start the transfer, read what was
//! recorded. The cases that need the processor's own stores, the frames and
//! the capture boundary run small programs on a whole emulator.

use retro_core::capture::{
    CaptureCore, CaptureRecording, FrameWrite, WriteKind, WriteOrigin, Writer, WORDS_PER_RECORD,
};
use retro_core::cpu::Addressable;
use retro_core::EmulatorCore;
use snes1_core::ppu::writes::{port, FRAME_WRITES_CAP, VRAM_BYTES};
use snes1_core::{parse_manifest, SnesBus, SnesEmulator};

// -------------------------------------------------------------
// Helpers
// -------------------------------------------------------------

/// The two live recorders armed.
fn both_recorders() -> CaptureRecording {
    CaptureRecording {
        video_memory_sources: true,
        frame_writes: true,
        ..Default::default()
    }
}

/// A bare bus with the two live recorders armed. The PPU starts in forced
/// blank, so every port is open.
fn recording_bus() -> SnesBus {
    let mut bus = SnesBus::new();
    bus.set_capture_recording(both_recorders());
    bus
}

/// Write a `$43xN` channel register through the bus.
fn set_channel(bus: &mut SnesBus, channel: u8, offset: u8, value: u8) {
    bus.write(
        0x4300 | (u32::from(channel) << 4) | u32::from(offset),
        value,
    );
}

/// Program a channel: its control byte, B-bus port, A-bus address and byte
/// count (or, for HDMA, its table's address).
fn program_channel(bus: &mut SnesBus, channel: u8, control: u8, port: u8, source: u32, count: u16) {
    set_channel(bus, channel, 0x0, control);
    set_channel(bus, channel, 0x1, port);
    set_channel(bus, channel, 0x2, source as u8);
    set_channel(bus, channel, 0x3, (source >> 8) as u8);
    set_channel(bus, channel, 0x4, (source >> 16) as u8);
    set_channel(bus, channel, 0x5, count as u8);
    set_channel(bus, channel, 0x6, (count >> 8) as u8);
}

/// Bytes into memory at an A-bus address, through the bus.
fn poke(bus: &mut SnesBus, address: u32, bytes: &[u8]) {
    for (i, &byte) in bytes.iter().enumerate() {
        bus.write(address + i as u32, byte);
    }
}

/// Start the general DMA of the channels in `mask` the way the emulator
/// does at the end of the instruction that wrote `$420B`.
fn run_general_dma(bus: &mut SnesBus, mask: u8) {
    bus.write(0x420B, mask);
    let pending = bus.take_pending_general_dma();
    bus.run_general_dma(pending);
}

/// The VRAM word address the port writes next, with the step on the high
/// byte.
fn vram_address(bus: &mut SnesBus, word: u16) {
    bus.write(0x2115, 0x80);
    bus.write(0x2116, word as u8);
    bus.write(0x2117, (word >> 8) as u8);
}

/// The records taken out of the log.
fn take(bus: &mut SnesBus) -> Vec<FrameWrite> {
    bus.ppu_mut()
        .take_frame_writes()
        .expect("the log is recorded")
        .iter()
        .collect()
}

/// The origin of VRAM byte `byte`.
fn origin(bus: &SnesBus, byte: usize) -> WriteOrigin {
    bus.ppu()
        .video_memory_sources()
        .expect("the sources are recorded")
        .origin(byte)
}

/// The frame VRAM byte `byte` was last written in.
fn frame_of(bus: &SnesBus, byte: usize) -> u32 {
    bus.ppu().video_memory_sources().unwrap().frames()[byte]
}

// -------------------------------------------------------------
// Programs
// -------------------------------------------------------------

/// 65C816 code at `$00:8000`, emulation mode, assembled a few instructions
/// at a time; the stores return their own address.
#[derive(Default)]
struct Code(Vec<u8>);

impl Code {
    /// The address of the next instruction.
    fn here(&self) -> u32 {
        0x00_8000 + self.0.len() as u32
    }

    /// `LDA #value`.
    fn load(&mut self, value: u8) {
        self.0.extend([0xA9, value]);
    }

    /// `STA address`; returns the instruction's address.
    fn store_a(&mut self, address: u16) -> u32 {
        let at = self.here();
        self.0.push(0x8D);
        self.0.extend(address.to_le_bytes());
        at
    }

    /// `LDA #value` / `STA address`; returns the store's address.
    fn store(&mut self, value: u8, address: u16) -> u32 {
        self.load(value);
        self.store_a(address)
    }

    /// `STZ address`; returns the instruction's address.
    fn store_zero(&mut self, address: u16) -> u32 {
        let at = self.here();
        self.0.push(0x9C);
        self.0.extend(address.to_le_bytes());
        at
    }

    /// A general DMA on `channel` of `length` bytes from `source` to the
    /// port `$21xx`; returns the address of the store that starts it.
    fn dma(&mut self, channel: u16, unit: u8, port: u8, source: u32, length: u16) -> u32 {
        let base = 0x4300 + (channel << 4);
        self.store(unit, base);
        self.store(port, base + 1);
        self.store(source as u8, base + 2);
        self.store((source >> 8) as u8, base + 3);
        self.store((source >> 16) as u8, base + 4);
        self.store(length as u8, base + 5);
        self.store((length >> 8) as u8, base + 6);
        self.store(1 << channel, 0x420B)
    }

    /// `BRA *`; returns its address.
    fn spin(&mut self) -> u32 {
        let at = self.here();
        self.0.extend([0x80, 0xFE]);
        at
    }
}

/// Where a LoROM file offset is seen on the A bus.
fn lorom(offset: usize) -> u32 {
    (((offset >> 15) as u32) << 16) | 0x8000 | (offset as u32 & 0x7FFF)
}

/// A LoROM image of `size` bytes: `reset` at `$00:8000`, `nmi` at
/// `$00:F000`, the `data` blocks at their file offsets, and a header asking
/// for `sram_code` (the header's SRAM size byte).
fn build_rom(
    size: usize,
    reset: &[u8],
    nmi: &[u8],
    data: &[(usize, Vec<u8>)],
    sram_code: u8,
) -> Vec<u8> {
    let mut rom = vec![0u8; size];
    rom[..reset.len()].copy_from_slice(reset);
    rom[0x7000..0x7000 + nmi.len()].copy_from_slice(nmi);
    for (offset, bytes) in data {
        rom[*offset..*offset + bytes.len()].copy_from_slice(bytes);
    }
    let header = 0x7FB0;
    rom[header + 0x10..header + 0x25].copy_from_slice(b"TRANSFER LOG         ");
    rom[header + 0x25] = 0x20; // LoROM
    rom[header + 0x27] = (size.next_power_of_two().trailing_zeros() - 10) as u8;
    rom[header + 0x28] = sram_code;
    rom[header + 0x29] = 0x01; // NTSC
    rom[header + 0x2C..header + 0x2E].copy_from_slice(&(!0x1234u16).to_le_bytes());
    rom[header + 0x2E..header + 0x30].copy_from_slice(&0x1234u16.to_le_bytes());
    rom[0x7FFA..0x7FFC].copy_from_slice(&0xF000u16.to_le_bytes()); // NMI
    rom[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes()); // reset
    rom
}

/// A program of stores by the processor through the three ports, in forced
/// blank (the reset state), then `BRA *`. Returns the ROM and the address
/// of each store that lands: the VRAM byte, the colour's high half and the
/// second byte of the sprite-table pair.
fn stores_rom() -> (Vec<u8>, [u32; 3], u32) {
    let mut code = Code::default();
    code.store(0x80, 0x2115);
    code.store(0x00, 0x2116);
    code.store(0x20, 0x2117);
    let vram = code.store(0xAB, 0x2118);
    code.store(0x05, 0x2121);
    code.store(0x1F, 0x2122);
    let colour = code.store(0x00, 0x2122);
    code.store_zero(0x2102);
    code.store_zero(0x2103);
    code.store(0x11, 0x2104);
    let sprite = code.store(0x22, 0x2104);
    let spin = code.spin();
    (
        build_rom(0x2_0000, &code.0, &[0x40], &[], 0),
        [vram, colour, sprite],
        spin,
    )
}

/// Step `emu` until the processor reaches `address`.
fn step_to(emu: &mut SnesEmulator, address: u32) {
    for _ in 0..1000 {
        let cpu = emu.cpu();
        if (u32::from(cpu.pbr) << 16) | u32::from(cpu.pc) == address {
            return;
        }
        emu.step_without_events();
    }
    panic!("the processor never reached {address:#08x}");
}

// -------------------------------------------------------------
// DMA
// -------------------------------------------------------------

#[test]
fn a_dma_block_into_vram_is_one_record_and_names_the_source_of_each_byte() {
    let mut bus = recording_bus();
    vram_address(&mut bus, 0x1000);
    let data: Vec<u8> = (0..64u8).map(|i| i ^ 0x5A).collect();
    poke(&mut bus, 0x7E_2000, &data);
    program_channel(&mut bus, 2, 0x01, port::VRAM_LOW, 0x7E_2000, 64);
    run_general_dma(&mut bus, 1 << 2);

    assert_eq!(
        take(&mut bus),
        vec![FrameWrite {
            kind: WriteKind::Transfer as u8,
            channel: 2,
            unit: 1,
            frame: 1,
            source: 0x7E_2000,
            target: port::VRAM_LOW,
            count: 64,
            start: 0x1000,
            end_or_value: 0x101F,
            ..FrameWrite::default()
        }]
    );
    for i in 0..64 {
        assert_eq!(
            origin(&bus, 0x2000 + i),
            WriteOrigin::transfer(2, 0x7E_2000 + i as u32),
            "byte {i}"
        );
        assert_eq!(frame_of(&bus, 0x2000 + i), 1);
    }
    assert_eq!(origin(&bus, 0x2040), WriteOrigin::NONE);
    assert_eq!(frame_of(&bus, 0x2040), 0, "never written");
    assert!(take(&mut bus).is_empty(), "a take empties the log");
}

#[test]
fn a_dma_into_cgram_oam_or_work_ram_records_where_it_started_and_ended() {
    let mut bus = recording_bus();
    // Four colours from colour 16 on.
    poke(
        &mut bus,
        0x7E_0100,
        &[0x1F, 0x00, 0xE0, 0x03, 0x00, 0x7C, 0xFF, 0x7F],
    );
    bus.write(0x2121, 0x10);
    program_channel(&mut bus, 0, 0x00, port::PALETTE, 0x7E_0100, 8);
    run_general_dma(&mut bus, 1);
    // The whole sprite table.
    bus.write(0x2102, 0x00);
    bus.write(0x2103, 0x00);
    program_channel(&mut bus, 1, 0x00, port::SPRITE_TABLE, 0x7E_0200, 0x220);
    run_general_dma(&mut bus, 1 << 1);
    // Sixteen bytes of work RAM through its port, from open bus.
    poke(&mut bus, 0x2181, &[0x00, 0x00, 0x01]);
    program_channel(&mut bus, 3, 0x00, port::WORK_RAM, 0x40_0000, 16);
    run_general_dma(&mut bus, 1 << 3);
    // And from work RAM itself, which the WRAM port cannot copy.
    program_channel(&mut bus, 4, 0x00, port::WORK_RAM, 0x7E_0000, 16);
    run_general_dma(&mut bus, 1 << 4);

    let records = take(&mut bus);
    assert_eq!(
        records.len(),
        4,
        "blocks only: a DMA's writes are its block"
    );
    let places: Vec<(u8, u32, u32, bool)> = records
        .iter()
        .map(|r| (r.target, r.start, r.end_or_value, r.nothing_landed))
        .collect();
    assert_eq!(
        places,
        vec![
            (port::PALETTE, 0x10, 0x13, false),
            (port::SPRITE_TABLE, 0, 0x21F, false),
            (port::WORK_RAM, 0x1_0000, 0x1_000F, false),
            (port::WORK_RAM, 0x1_0010, 0x1_0010, true),
        ]
    );
    assert_eq!(bus.ppu().cgram()[0x11], 0x03E0);
    assert!(records.iter().all(|r| r.kind == WriteKind::Transfer as u8));
}

#[test]
fn a_vram_dma_while_the_picture_is_drawn_lands_nothing_and_says_where_the_port_pointed() {
    let mut bus = recording_bus();
    vram_address(&mut bus, 0x0800);
    bus.write(0x2100, 0x0F); // the display on
    bus.set_raster_snapshot(100, 50); // dot 100 of line 50: the port is shut
    program_channel(&mut bus, 0, 0x01, port::VRAM_LOW, 0x7E_0000, 32);
    run_general_dma(&mut bus, 1);

    let records = take(&mut bus);
    assert_eq!(records.len(), 1);
    let block = records[0];
    assert!(block.nothing_landed);
    assert_eq!((block.start, block.end_or_value), (0x0800, 0x0800));
    assert_eq!((block.line, block.position), (50, 100));
    assert!(
        (0x1000..0x1040).all(|byte| origin(&bus, byte) == WriteOrigin::NONE),
        "nothing reached VRAM"
    );
}

#[test]
fn a_mode_7_split_upload_names_two_sources_for_one_word() {
    // The map into the low bytes, then the characters into the high bytes,
    // as two DMAs from two places.
    let mut bus = recording_bus();
    bus.write(0x2115, 0x00); // step on the low byte
    bus.write(0x2116, 0x00);
    bus.write(0x2117, 0x00);
    program_channel(&mut bus, 0, 0x00, port::VRAM_LOW, 0x7E_1000, 256);
    run_general_dma(&mut bus, 1);
    vram_address(&mut bus, 0x0000); // step on the high byte
    program_channel(&mut bus, 1, 0x00, port::VRAM_HIGH, 0x7E_3000, 256);
    run_general_dma(&mut bus, 1 << 1);

    for word in 0..256usize {
        assert_eq!(
            origin(&bus, 2 * word),
            WriteOrigin::transfer(0, 0x7E_1000 + word as u32)
        );
        assert_eq!(
            origin(&bus, 2 * word + 1),
            WriteOrigin::transfer(1, 0x7E_3000 + word as u32)
        );
    }
    let spans: Vec<(u8, u32, u32)> = take(&mut bus)
        .iter()
        .map(|r| (r.target, r.start, r.end_or_value))
        .collect();
    assert_eq!(
        spans,
        vec![(port::VRAM_LOW, 0, 0xFF), (port::VRAM_HIGH, 0, 0xFF)]
    );
}

#[test]
fn a_dma_from_the_b_bus_records_the_a_bus_places_it_wrote() {
    let mut bus = recording_bus();
    vram_address(&mut bus, 0x0000);
    // VRAM read back through `$2139/$213A` into work RAM.
    program_channel(&mut bus, 5, 0x81, 0x39, 0x7E_4000, 8);
    run_general_dma(&mut bus, 1 << 5);
    let records = take(&mut bus);
    assert_eq!(records.len(), 1);
    assert!(records[0].port_to_memory);
    assert_eq!(
        (records[0].start, records[0].end_or_value),
        (0x7E_4000, 0x7E_4007)
    );
}

// -------------------------------------------------------------
// HDMA
// -------------------------------------------------------------

#[test]
fn hdma_logs_its_set_up_per_channel_and_each_unit_with_the_bytes_it_moved() {
    let mut bus = recording_bus();
    // Channel 1, unit 3 to `$2121`: the colour index twice, then the colour,
    // two lines in a row.
    poke(
        &mut bus,
        0x7E_8000,
        &[0x82, 0x07, 0x07, 0x1F, 0x00, 0x09, 0x09, 0xE0, 0x03, 0x00],
    );
    program_channel(&mut bus, 1, 0x03, 0x21, 0x7E_8000, 0);
    // Channel 3, indirect, unit 0 to COLDATA: one byte from `$7E:A000`.
    poke(&mut bus, 0x7E_9000, &[0x81, 0x00, 0xA0, 0x00]);
    poke(&mut bus, 0x7E_A000, &[0x3F]);
    program_channel(&mut bus, 3, 0x40, 0x32, 0x7E_9000, 0);
    set_channel(&mut bus, 3, 0x7, 0x7E);
    bus.write(0x420C, 0b1010);

    bus.set_raster_snapshot(6, 0);
    bus.hdma_init_frame();
    let setup = take(&mut bus);
    assert_eq!(
        setup,
        vec![
            FrameWrite {
                kind: WriteKind::LineTransferSetUp as u8,
                channel: 1,
                unit: 3,
                frame: 1,
                position: 6,
                source: 0x7E_8000,
                target: 0x21,
                ..FrameWrite::default()
            },
            FrameWrite {
                kind: WriteKind::LineTransferSetUp as u8,
                channel: 3,
                indirect: true,
                frame: 1,
                position: 6,
                source: 0x7E_9000,
                target: 0x32,
                ..FrameWrite::default()
            },
        ]
    );

    bus.set_raster_snapshot(276, 0);
    bus.hdma_run_line();
    let line = take(&mut bus);
    assert_eq!(
        line,
        vec![
            FrameWrite {
                kind: WriteKind::LineTransferUnit as u8,
                channel: 1,
                unit: 3,
                frame: 1,
                position: 276,
                source: 0x7E_8001,
                target: 0x21,
                count: 4,
                start: 7,
                end_or_value: u32::from_le_bytes([0x07, 0x07, 0x1F, 0x00]),
                ..FrameWrite::default()
            },
            FrameWrite {
                kind: WriteKind::PaletteStore as u8,
                channel: 1,
                writer: Writer::LineTransfer as u8,
                frame: 1,
                position: 276,
                source: 0x7E_8004,
                target: port::PALETTE,
                count: 2,
                start: 7,
                end_or_value: 0x001F,
                ..FrameWrite::default()
            },
            FrameWrite {
                kind: WriteKind::LineTransferUnit as u8,
                channel: 3,
                indirect: true,
                frame: 1,
                position: 276,
                source: 0x7E_A000,
                target: 0x32,
                count: 1,
                end_or_value: 0x3F,
                extra: 0x7E_A000,
                ..FrameWrite::default()
            },
        ]
    );

    // The next line: channel 1's second colour; channel 3's table ended.
    bus.set_raster_snapshot(276, 1);
    bus.hdma_run_line();
    let next: Vec<(u8, u32, u32)> = take(&mut bus)
        .iter()
        .map(|r| (r.kind, r.start, r.end_or_value))
        .collect();
    assert_eq!(
        next,
        vec![
            (
                WriteKind::LineTransferUnit as u8,
                9,
                u32::from_le_bytes([0x09, 0x09, 0xE0, 0x03])
            ),
            (WriteKind::PaletteStore as u8, 9, 0x03E0),
        ]
    );
}

#[test]
fn hdma_into_vram_data_is_put_down_to_hdma() {
    let mut bus = recording_bus();
    bus.write(0x2115, 0x00); // step on the low byte
    bus.write(0x2116, 0x00);
    bus.write(0x2117, 0x04);
    poke(&mut bus, 0x7E_8000, &[0x82, 0x11, 0x22, 0x00]);
    program_channel(&mut bus, 5, 0x00, port::VRAM_LOW, 0x7E_8000, 0);
    bus.write(0x420C, 1 << 5);
    bus.hdma_init_frame();
    bus.hdma_run_line();
    bus.hdma_run_line();
    assert_eq!(
        origin(&bus, 0x0800),
        WriteOrigin::line_transfer(5, 0x7E_8001)
    );
    assert_eq!(
        origin(&bus, 0x0802),
        WriteOrigin::line_transfer(5, 0x7E_8002)
    );
    assert_eq!(bus.ppu().vram()[0x400], 0x0011);
    assert_eq!(bus.ppu().vram()[0x401], 0x0022);
    let units: Vec<u32> = take(&mut bus)
        .iter()
        .filter(|r| r.kind == WriteKind::LineTransferUnit as u8)
        .map(|r| r.start)
        .collect();
    assert_eq!(units, vec![0x400, 0x401], "the VRAM word each unit wrote");
}

// -------------------------------------------------------------
// The processor, the debugger, the frames
// -------------------------------------------------------------

#[test]
fn a_store_by_the_processor_is_put_down_to_its_instruction() {
    let (rom, [vram, colour, sprite], spin) = stores_rom();
    let mut emu = SnesEmulator::new();
    emu.set_capture_recording(both_recorders());
    emu.load_rom(&rom).expect("the program loads");
    step_to(&mut emu, spin);

    let sources = emu.video_memory_sources().expect("recorded");
    assert_eq!(sources.origin(0x4000), WriteOrigin::processor(vram));
    assert_eq!(sources.frames()[0x4000], 1);
    assert_eq!(sources.origin(0x4001), WriteOrigin::NONE);

    let records: Vec<FrameWrite> = emu.take_frame_writes().unwrap().iter().collect();
    let without_raster: Vec<FrameWrite> = records
        .iter()
        .map(|r| FrameWrite {
            line: 0,
            position: 0,
            ..*r
        })
        .collect();
    assert_eq!(
        without_raster,
        vec![
            FrameWrite {
                kind: WriteKind::PaletteStore as u8,
                writer: Writer::Processor as u8,
                frame: 1,
                source: colour,
                target: port::PALETTE,
                count: 2,
                start: 5,
                end_or_value: 0x001F,
                ..FrameWrite::default()
            },
            FrameWrite {
                kind: WriteKind::SpriteTableStore as u8,
                writer: Writer::Processor as u8,
                frame: 1,
                source: sprite,
                target: port::SPRITE_TABLE,
                count: 2,
                start: 0,
                end_or_value: 0x2211,
                ..FrameWrite::default()
            },
        ]
    );
}

#[test]
fn a_debugger_write_is_put_down_to_the_debugger_and_logs_nothing() {
    let (rom, _, _) = stores_rom();
    let mut emu = SnesEmulator::new();
    emu.set_capture_recording(both_recorders());
    emu.load_rom(&rom).expect("the program loads");
    // Straight into VRAM, and through the ports.
    emu.write_vram_debug(0x0100, &[1, 2, 3]);
    emu.write_memory_debug(0x2115, &[0x80, 0x00, 0x30]);
    emu.write_memory_debug(0x2118, &[0x44]);
    emu.write_memory_debug(0x2121, &[0x02]);
    emu.write_memory_debug(0x2122, &[0x1F]);
    emu.write_memory_debug(0x2122, &[0x00]);
    let sources = emu.video_memory_sources().unwrap();
    for byte in [0x200, 0x201, 0x202, 0x6000] {
        assert_eq!(
            sources.origin(byte),
            WriteOrigin::DEBUGGER,
            "byte {byte:#x}"
        );
        assert_eq!(sources.frames()[byte], 1);
    }
    assert!(
        emu.take_frame_writes().unwrap().records.is_empty(),
        "the debugger's writes are not the frame's"
    );
    assert_eq!(emu.ppu().cgram()[2], 0x001F);

    // The processor writes again after the debugger.
    let (_, [vram, _, _], spin) = stores_rom();
    step_to(&mut emu, spin);
    assert_eq!(
        emu.video_memory_sources().unwrap().origin(0x4000),
        WriteOrigin::processor(vram)
    );
}

/// `SEI` / display on / NMI on / `BRA *`, with an NMI handler that writes
/// the count of vertical blanks kept at `$10` into colour 0.
fn palette_per_frame_rom() -> Vec<u8> {
    let mut code = Code::default();
    code.0.push(0x78);
    code.store(0x0F, 0x2100);
    code.store(0x80, 0x4200);
    code.spin();
    // PHA / INC $10 / STZ $2121 / LDA $10 / STA $2122 / STZ $2122 / PLA / RTI
    let nmi = [
        0x48, 0xE6, 0x10, 0x9C, 0x21, 0x21, 0xA5, 0x10, 0x8D, 0x22, 0x21, 0x9C, 0x22, 0x21, 0x68,
        0x40,
    ];
    build_rom(0x2_0000, &code.0, &nmi, &[], 0)
}

#[test]
fn the_writes_taken_at_a_boundary_carry_the_frame_they_were_made_in() {
    let mut emu = SnesEmulator::new();
    emu.set_capture_recording(both_recorders());
    emu.load_rom(&palette_per_frame_rom())
        .expect("the program loads");
    emu.run_to_capture_boundary();
    assert!(
        emu.take_frame_writes().unwrap().records.is_empty(),
        "no vertical blank before frame 1's picture"
    );
    for frame in 2..=5u64 {
        emu.run_to_capture_boundary();
        assert_eq!(emu.capture_frame_snapshot().frame, frame);
        let records: Vec<FrameWrite> = emu.take_frame_writes().unwrap().iter().collect();
        assert_eq!(records.len(), 1, "frame {frame}");
        let colour = records[0];
        assert_eq!(colour.kind, WriteKind::PaletteStore as u8);
        assert_eq!(colour.writer, Writer::Processor as u8);
        assert_eq!(
            colour.frame,
            frame - 1,
            "made in the vertical blank after frame {}",
            frame - 1
        );
        assert_eq!(colour.end_or_value, (frame - 1) as u32);
        assert!(
            colour.line >= 225,
            "in the vertical blank: line {}",
            colour.line
        );
    }
}

#[test]
fn the_recorders_start_empty_and_a_reset_a_state_load_and_a_rom_load_empty_them() {
    let (rom, _, spin) = stores_rom();
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).expect("the program loads");
    step_to(&mut emu, spin);
    // Turned on after the stores: empty.
    emu.set_capture_recording(both_recorders());
    let is_empty = |emu: &mut SnesEmulator| {
        let sources = emu.video_memory_sources().expect("armed");
        sources.origins().iter().all(|&o| o == 0)
            && sources.frames().iter().all(|&f| f == 0)
            && emu.take_frame_writes().expect("armed") == Default::default()
    };
    assert!(is_empty(&mut emu));

    emu.reset();
    step_to(&mut emu, spin);
    assert!(!is_empty(&mut emu));
    emu.write_vram_debug(0, &[9]);
    emu.reset();
    assert!(is_empty(&mut emu), "a reset empties them");
    assert_eq!(emu.capture_recording(), both_recorders(), "still armed");

    let state = emu.save_state();
    step_to(&mut emu, spin);
    assert!(!is_empty(&mut emu));
    emu.write_vram_debug(0, &[9]);
    emu.load_state(&state).expect("its own state loads");
    assert!(is_empty(&mut emu), "a state load empties them");

    step_to(&mut emu, spin);
    emu.write_vram_debug(0, &[9]);
    emu.load_rom(&rom).expect("the program loads again");
    assert!(is_empty(&mut emu), "a ROM load empties them");

    emu.set_capture_recording(CaptureRecording::default());
    assert!(emu.video_memory_sources().is_none());
    assert!(emu.take_frame_writes().is_none());
}

#[test]
fn the_log_keeps_its_cap_and_counts_what_it_dropped() {
    let mut bus = recording_bus();
    bus.write(0x2121, 0x00);
    for _ in 0..FRAME_WRITES_CAP + 10 {
        bus.write(0x2122, 0x1F);
        bus.write(0x2122, 0x00);
    }
    let taken = bus.ppu_mut().take_frame_writes().unwrap();
    assert_eq!(taken.records.len(), FRAME_WRITES_CAP * WORDS_PER_RECORD);
    assert_eq!(taken.dropped, 10);
    let again = bus.ppu_mut().take_frame_writes().unwrap();
    assert!(again.records.is_empty());
    assert_eq!(again.dropped, 0, "counted since the last take");
}

// -------------------------------------------------------------
// Resolving a source
// -------------------------------------------------------------

#[test]
fn bytes_uploaded_from_rom_name_their_rom_bytes() {
    // In forced blank, the program's first 64 bytes of ROM, at `$00:8000`,
    // into VRAM word 0x0800 with transfer unit 1 on channel 3.
    let mut code = Code::default();
    code.store(0x80, 0x2115);
    code.store(0x00, 0x2116);
    code.store(0x08, 0x2117);
    code.dma(3, 0x01, port::VRAM_LOW, 0x00_8000, 64);
    let spin = code.spin();
    let rom = build_rom(0x2_0000, &code.0, &[0x40], &[], 0);
    let mut emu = SnesEmulator::new();
    emu.set_capture_recording(both_recorders());
    emu.load_rom(&rom).expect("the program loads");
    step_to(&mut emu, spin);

    let sources = emu.video_memory_sources().unwrap();
    let addresses: Vec<u32> = (0..64)
        .map(|i| {
            let origin = sources.origin(0x1000 + i);
            assert_eq!(
                origin,
                WriteOrigin::transfer(3, 0x00_8000 + i as u32),
                "byte {i}"
            );
            origin.source()
        })
        .collect();
    let offsets = emu.resolve_sources(&addresses);
    assert_eq!(
        offsets,
        (0..64).collect::<Vec<i32>>(),
        "the program's own bytes"
    );
    assert_eq!(
        emu.ppu().vram()[0x800],
        u16::from_le_bytes([rom[0], rom[1]])
    );
}

#[test]
fn a_source_resolves_through_the_cartridge_mapping() {
    // 256 KiB of LoROM with 2 KiB of SRAM.
    let rom = build_rom(0x4_0000, &[0x80, 0xFE], &[0x40], &[], 0x01);
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).expect("the program loads");
    let addresses = [
        0x00_8000, 0x80_8000, 0x01_8000, 0x07_FFFF, 0x7E_1234, 0x00_0100, 0x70_0000, 0x00_2100,
        0x00_4200, 0x40_0000,
    ];
    assert_eq!(
        emu.resolve_sources(&addresses),
        vec![0, 0, 0x8000, 0x3_FFFF, -2, -2, -3, -1, -1, -1]
    );

    // The same image behind a copier header: offsets count from the file.
    let mut headered = vec![0u8; 512];
    headered.extend_from_slice(&rom);
    emu.load_rom(&headered).expect("the headered image loads");
    assert_eq!(
        emu.resolve_sources(&[0x00_8000, 0x01_8000]),
        vec![512, 0x8200]
    );

    // A 192 KiB image is padded to 256 KiB: the padding is in no file.
    let short = build_rom(0x3_0000, &[0x80, 0xFE], &[0x40], &[], 0);
    emu.load_rom(&short).expect("the short image loads");
    assert_eq!(
        emu.resolve_sources(&[0x05_FFFF, 0x06_8000]),
        vec![0x2_FFFF, -1]
    );
}

#[test]
fn a_coprocessor_window_resolves_to_the_coprocessor() {
    let rom = build_rom(0x4_0000, &[0x80, 0xFE], &[0x40], &[], 0);
    let manifest = parse_manifest(
        r#"{
            "core": "snes1",
            "version": "1.0",
            "rom": { "sha256": "", "size": 262144 },
            "mapping": { "preset": "lorom" },
            "chips": [{
                "id": 1,
                "kind": "dsp1",
                "mapping": {
                    "bank_range": ["0x60", "0x6F"],
                    "offset_range": ["0x0000", "0x7FFF"],
                    "target": { "kind": "Rom", "base_offset": 0 },
                    "speed": "Slow"
                }
            }]
        }"#,
    )
    .expect("the manifest parses");
    let mut emu = SnesEmulator::new();
    emu.set_cartridge_manifest_override(Some(manifest));
    emu.load_rom(&rom).expect("the program loads");
    assert_eq!(emu.resolve_sources(&[0x60_0000, 0x00_8000]), vec![-4, 0]);
}

// -------------------------------------------------------------
// Recording changes nothing
// -------------------------------------------------------------

/// A busy program: HDMA writes colour 0 on every line, the main loop
/// uploads 512 bytes of ROM into VRAM over and over (most of them while
/// the picture is drawn, where they land nowhere), and the NMI handler
/// writes a colour and the sprite table.
fn busy_rom() -> Vec<u8> {
    const GRADIENT: usize = 0x8000;
    const TILES: usize = 0x9000;
    let mut code = Code::default();
    code.0.push(0x78);
    code.store(0x0F, 0x2100);
    code.store(0x03, 0x4310);
    code.store(0x21, 0x4311);
    let table = lorom(GRADIENT);
    code.store(table as u8, 0x4312);
    code.store((table >> 8) as u8, 0x4313);
    code.store((table >> 16) as u8, 0x4314);
    code.store(0x02, 0x420C);
    code.store(0x80, 0x4200);
    let main_loop = code.here();
    code.store(0x80, 0x2115);
    code.store_zero(0x2116);
    code.store_zero(0x2117);
    code.dma(0, 0x01, port::VRAM_LOW, lorom(TILES), 512);
    let back = main_loop as i64 - (code.here() as i64 + 2);
    code.0.extend([0x80, back as i8 as u8]);
    let mut gradient = Vec::new();
    for line in 0..240u16 {
        gradient.extend([0x01, 0x00, 0x00]);
        gradient.extend((line * 33).to_le_bytes());
    }
    gradient.push(0x00);
    let tiles: Vec<u8> = (0..512u32).map(|i| (i * 7) as u8).collect();
    // PHA / STZ $2121 / LDA #$1F / STA $2122 / STZ $2122 / STZ $2102 /
    // STZ $2103 / LDA #$40 / STA $2104 / STA $2104 / PLA / RTI
    let nmi = [
        0x48, 0x9C, 0x21, 0x21, 0xA9, 0x1F, 0x8D, 0x22, 0x21, 0x9C, 0x22, 0x21, 0x9C, 0x02, 0x21,
        0x9C, 0x03, 0x21, 0xA9, 0x40, 0x8D, 0x04, 0x21, 0x8D, 0x04, 0x21, 0x68, 0x40,
    ];
    build_rom(
        0x2_0000,
        &code.0,
        &nmi,
        &[(GRADIENT, gradient), (TILES, tiles)],
        0,
    )
}

#[test]
fn recording_and_taking_the_writes_changes_nothing_a_savestate_holds() {
    let rom = busy_rom();
    let mut plain = SnesEmulator::new();
    let mut recording = SnesEmulator::new();
    recording.set_capture_recording(both_recorders());
    plain.load_rom(&rom).expect("the program loads");
    recording.load_rom(&rom).expect("the program loads");
    let mut kinds = [0usize; 6];
    for frame in 1..=60 {
        for emu in [&mut plain, &mut recording] {
            if frame % 3 == 0 {
                emu.run_to_capture_boundary();
            } else {
                emu.run_frame();
            }
        }
        for record in recording.take_frame_writes().unwrap().iter() {
            kinds[usize::from(record.kind)] += 1;
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
    // Every kind of record was made.
    for kind in 1..=5 {
        assert!(kinds[kind] > 0, "kind {kind}: {kinds:?}");
    }
    let sources = recording.video_memory_sources().unwrap();
    let written = (0..VRAM_BYTES)
        .filter(|&byte| sources.origin(byte).writer() == Writer::Transfer as u32)
        .count();
    assert_eq!(written, 512, "the upload landed in some vertical blank");
}
