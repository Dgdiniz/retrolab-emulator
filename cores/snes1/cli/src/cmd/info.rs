//! `info`: load a ROM and print its header, detected mapper, and the
//! resulting CPU-address memory map.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use retro_core::cartridge::{AccessType, CartridgeDebugger, MemoryType};
use retro_core::EmulatorCore;
use snes1_core::{PresetKind, SnesEmulator};

#[derive(Args, Debug)]
pub struct InfoArgs {
    /// Path to the ROM file (`.sfc` / `.smc`).
    pub rom: PathBuf,
    /// Run this many frames after loading before dumping coprocessor +
    /// memory state (0 = static snapshot at reset). Use to confirm a
    /// coprocessor actually fired, e.g. S-DD1 `DECOMPS > 0`.
    #[arg(long, default_value_t = 0)]
    pub frames: u64,
    /// DSP-1 firmware + coprocessor-mode flags. Supply
    /// `--dsp1-firmware` to see whether the dump is recognised (the
    /// `Coprocessor:` line then reports `... (LLE)` instead of the
    /// firmware-free `... (HLE)` fallback).
    #[command(flatten)]
    pub coprocessor: super::CoprocessorArgs,
}

pub fn run(args: InfoArgs) -> Result<()> {
    let rom = super::read_rom(&args.rom)?;
    let mut emu = SnesEmulator::new();
    args.coprocessor.apply(&mut emu)?;
    emu.load_rom(&rom)
        .map_err(|e| anyhow::anyhow!("failed to load ROM: {e}"))?;

    // Optionally drive the game so the coprocessor + memory-map dumps
    // reflect live activity (e.g. S-DD1 decompressions) rather than the
    // reset snapshot.
    for _ in 0..args.frames {
        emu.run_frame();
    }

    println!("File:        {}", args.rom.display());
    println!(
        "File size:   {} bytes ({})",
        rom.len(),
        human_bytes(rom.len())
    );

    let rom_info = emu.get_rom_info();
    let header = emu.header().cloned();

    if let Some(info) = &rom_info {
        println!(
            "Title:       {:?} (confidence: {})",
            info.title.trim(),
            info.title_confidence
        );
        println!("Region:      {}", info.region);
    }

    let mapper = match emu.preset_kind() {
        Some(k) => preset_name(k),
        None => "unknown",
    };

    if let Some(h) = &header {
        println!(
            "Mapper:      {mapper} (map_mode ${:02x}, low ${:x}){}",
            h.map_mode,
            h.map_mode_low(),
            if h.is_fastrom() {
                ", FastROM"
            } else {
                ", SlowROM"
            }
        );
        println!(
            "ROM size:    header {} ({}), file {}",
            h.rom_size_bytes(),
            human_bytes(h.rom_size_bytes() as usize),
            human_bytes(rom.len())
        );
        println!(
            "SRAM size:   {} ({})",
            h.sram_size_bytes(),
            human_bytes(h.sram_size_bytes() as usize)
        );
        println!(
            "Reset vec:   ${:04x}   NMI: native ${:04x} / emu ${:04x}",
            h.vectors.emu_reset, h.vectors.native_nmi, h.vectors.emu_nmi
        );
        println!(
            "IRQ vec:     native ${:04x} / emu ${:04x}",
            h.vectors.native_irq, h.vectors.emu_irq
        );
        println!(
            "Checksum:    ${:04x}  complement ${:04x}  [{}]",
            h.checksum,
            h.checksum_complement,
            if h.checksum_is_consistent() {
                "consistent"
            } else {
                "INCONSISTENT"
            }
        );
    } else {
        println!("Mapper:      {mapper} (no parsed header)");
    }

    super::print_coprocessors(&emu);
    print_memory_map(&mut emu);
    Ok(())
}

fn print_memory_map(emu: &mut SnesEmulator) {
    let map = emu.cartridge_debugger().get_memory_map();
    if map.regions.is_empty() {
        return;
    }
    println!();
    println!("CPU memory map ({} regions):", map.regions.len());
    for r in &map.regions {
        println!(
            "  {:06x}-{:06x}  {:>10}  {:<12} {:<10} {}",
            r.cpu_address_start,
            r.cpu_address_end,
            human_bytes(r.size),
            memory_type_name(r.memory_type),
            access_name(r.access),
            r.description,
        );
    }
}

fn preset_name(k: PresetKind) -> &'static str {
    match k {
        PresetKind::LoRom => "LoROM",
        PresetKind::HiRom => "HiROM",
        PresetKind::ExHiRom => "ExHiROM",
        PresetKind::Sdd1 => "S-DD1",
        PresetKind::Spc7110 => "SPC7110",
        PresetKind::Sa1 => "SA-1",
        PresetKind::SuperFx => "Super FX",
        PresetKind::Cx4 => "Cx4",
        PresetKind::Custom => "Custom",
    }
}

fn memory_type_name(t: MemoryType) -> &'static str {
    match t {
        MemoryType::ROM => "ROM",
        MemoryType::SRAM => "SRAM",
        MemoryType::CartridgeRAM => "CartRAM",
        MemoryType::ExpansionChipRAM => "ChipRAM",
        MemoryType::MappedIO => "MappedIO",
    }
}

fn access_name(a: AccessType) -> &'static str {
    match a {
        AccessType::ReadOnly => "ro",
        AccessType::WriteOnly => "wo",
        AccessType::ReadWrite => "rw",
        AccessType::MappedIO => "io",
    }
}

/// Compact human-readable byte size (KiB/MiB), exact powers of two.
fn human_bytes(n: usize) -> String {
    const KIB: usize = 1024;
    const MIB: usize = 1024 * 1024;
    if n == 0 {
        "0 B".to_string()
    } else if n.is_multiple_of(MIB) {
        format!("{} MiB", n / MIB)
    } else if n.is_multiple_of(KIB) {
        format!("{} KiB", n / KIB)
    } else {
        format!("{n} B")
    }
}
