//! `info`: load a ROM and print its header (title, region, checksum,
//! mapper), the installed coprocessor(s), and the resulting CPU-address
//! memory map.

use std::path::PathBuf;

use anyhow::Result;
use clap::Args;
use retro_core::cartridge::{AccessType, CartridgeDebugger, MemoryType};
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode};
use sg1_core::GenesisEmulator;

#[derive(Args, Debug)]
pub struct InfoArgs {
    /// Path to the ROM file (`.bin` / `.md` / `.gen`).
    pub rom: PathBuf,
    /// Run this many frames after loading before dumping coprocessor +
    /// memory state (0 = static snapshot at reset). Use to confirm a
    /// coprocessor actually fired, e.g. the SVP's cycle count climbing on
    /// Virtua Racing.
    #[arg(long, default_value_t = 0)]
    pub frames: u64,
}

pub fn run(args: InfoArgs) -> Result<()> {
    let (rom, mut emu) = super::load_rom(&args.rom)?;

    // Optionally drive the game so the coprocessor + memory-map dumps
    // reflect live activity rather than the reset snapshot. `run_frame`
    // only advances when the core is out of its default paused mode, so
    // switch to Running first (a coprocessor like the SVP only retires
    // cycles once the 68000 is actually executing the boot code).
    if args.frames > 0 {
        emu.debugger().set_execution_mode(ExecutionMode::Running);
        for _ in 0..args.frames {
            emu.run_frame();
        }
    }

    println!("ROM: {}", args.rom.display());
    println!("Size: {} bytes ({} KB)", rom.len(), rom.len() / 1024);

    if let Some(info) = emu.get_rom_info() {
        println!();
        println!("ROM Information:");
        println!("  Title: {}", info.title.trim());
        println!("  Region: {}", info.region);
        println!("  Checksum: 0x{:04X}", info.checksum);
        if let Some(mapper) = &info.mapper {
            println!("  Mapper: {mapper}");
        }
    }

    println!();
    super::print_coprocessors(&emu);
    print_memory_map(&mut emu);
    Ok(())
}

fn print_memory_map(emu: &mut GenesisEmulator) {
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
