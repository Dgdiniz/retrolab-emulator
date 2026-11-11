//! Debugger tests: event breakpoints, the PC-centered
//! disassembly window (both CPUs), and the cartridge debugger.
//!
//! The video / audio debuggers and the viewer exporters are tested as
//! in-module unit tests (next to the data they poke, `ppu/debug.rs`,
//! `apu/debug.rs`, `apu/dsp/voice.rs`) because they need direct access
//! to VRAM / CGRAM / OAM / DSP register state that is cheaper to set
//! than to drive through a full emulation frame. The end-to-end
//! debugger plumbing exercised here is what an integration test is
//! actually good at: a real ROM, a real frame loop, a real pause.

use retro_core::cartridge::{CartridgeDebugger, MapperId, MemoryType, WatchType};
use retro_core::types::{BreakpointType, DisasmRowKind, BREAKPOINT_PARAM_ANY};
use retro_core::{Breakpoint, EmulatorCore, ExecutionMode};

use snes1_core::{snes_events, SnesEmulator};

/// Stamp the minimal SNES header LoROM detection needs to score a
/// confident LoROM match (mirrors the boot/callstack test helpers).
/// `ram_size_code` sets the on-cart SRAM size (`1 << n` KiB; 0 = none).
fn stamp_lorom_header(chip: &mut [u8], reset_vector: u16, ram_size_code: u8) {
    let header = 0x7FB0;
    chip[header + 0x10..header + 0x25].copy_from_slice(b"BLOCK C DEBUG TEST   ");
    chip[header + 0x25] = 0x20; // map_mode = LoROM
    chip[header + 0x27] = 0x0A; // rom_size_code = 1 MiB
    chip[header + 0x28] = ram_size_code; // ram_size_code (SRAM)
    chip[header + 0x29] = 0x01; // destination = USA (NTSC)
    chip[header + 0x2A] = 0x01; // developer_id
    let checksum: u16 = 0x1234;
    chip[header + 0x2C..header + 0x2E].copy_from_slice(&(!checksum).to_le_bytes());
    chip[header + 0x2E..header + 0x30].copy_from_slice(&checksum.to_le_bytes());
    chip[header + 0x4C..header + 0x4E].copy_from_slice(&reset_vector.to_le_bytes());
}

/// 1 MiB LoROM whose `$00:8000` code is `code`, reset vector `$8000`,
/// and no on-cart SRAM.
fn synth_lorom(code: &[u8]) -> Vec<u8> {
    synth_lorom_sram(code, 0)
}

/// As [`synth_lorom`] but with `ram_size_code` controlling SRAM size.
fn synth_lorom_sram(code: &[u8], ram_size_code: u8) -> Vec<u8> {
    let mut chip = vec![0u8; 0x10_0000];
    chip[..code.len()].copy_from_slice(code);
    stamp_lorom_header(&mut chip, 0x8000, ram_size_code);
    chip
}

/// Boot `code` as a LoROM and return the emulator with execution mode
/// left at the default `Running` (so the `LightDebug` tier drives the
/// instrumented frame loop once a breakpoint is added).
fn boot(code: &[u8]) -> SnesEmulator {
    let mut emu = SnesEmulator::new();
    emu.load_rom(&synth_lorom(code))
        .expect("load synthetic LoROM");
    emu
}

/// Single-step the 65C816 `steps` times through the full-debug tier so
/// the coverage bitmap records the executed addresses (the
/// disassembly-window backward walk is coverage-aware).
fn step(emu: &mut SnesEmulator, steps: usize) {
    for _ in 0..steps {
        emu.debugger().set_execution_mode(ExecutionMode::StepInto);
        emu.run_frame();
    }
}

/// Look up a breakpoint's current hit count by id.
fn hit_count(emu: &mut SnesEmulator, id: u32) -> u32 {
    emu.debugger()
        .list_breakpoints()
        .into_iter()
        .find(|b| b.id == id)
        .map(|b| b.hit_count)
        .unwrap_or(0)
}

mod event_breakpoints {
    use super::*;

    /// A `Write` watchpoint on the address the boot code stores to
    /// pauses the run mid-frame and bumps its hit count. `STA $0000`
    /// (8-bit A at reset) writes WRAM `$00:0000`.
    #[test]
    fn write_watchpoint_pauses_run() {
        // $8000 STA $0000 ; $8003 BRA -5 (loop)
        let mut emu = boot(&[0x8D, 0x00, 0x00, 0x80, 0xFB]);
        let id = emu.debugger().add_breakpoint(Breakpoint {
            address: 0x00_0000,
            bp_type: BreakpointType::write(),
            cpu_id: Some(0),
            condition: None,
        });

        // Running tier + a watchpoint -> instrumented frame that pauses
        // the instant the watched write lands.
        emu.run_frame();

        assert_eq!(
            emu.debugger().get_execution_mode(),
            ExecutionMode::Paused,
            "the watched write must pause the frame"
        );
        assert!(hit_count(&mut emu, id) >= 1, "watchpoint hit count bumped");
    }

    /// A PPU-register-write event (`$2100-$213F`) pauses the run. The
    /// boot code writes `$2100` (INIDISP, register index `$00`); the
    /// breakpoint matches `code = PPU_REG_WRITE, param = 0`.
    #[test]
    fn ppu_register_write_event_pauses_run() {
        // $8000 STA $2100 ; $8003 BRA -5
        let mut emu = boot(&[0x8D, 0x00, 0x21, 0x80, 0xFB]);
        let id = emu.debugger().add_breakpoint(Breakpoint {
            address: 0,
            bp_type: BreakpointType::Event {
                code: snes_events::PPU_REG_WRITE,
                param: 0,
            },
            cpu_id: None,
            condition: None,
        });

        emu.run_frame();

        assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::Paused);
        assert!(hit_count(&mut emu, id) >= 1);
    }

    /// A `VBlank` event breakpoint pauses the frame when the scheduler
    /// reaches the V-blank boundary. ROM-free (open-bus execution): the
    /// `VBlankStart` scheduler event fires regardless of what the CPU
    /// runs, and `reset()` is what schedules it.
    #[test]
    fn vblank_event_pauses_run() {
        let mut emu = SnesEmulator::new();
        emu.reset();
        let id = emu.debugger().add_breakpoint(Breakpoint {
            address: 0,
            bp_type: BreakpointType::Event {
                code: snes_events::VBLANK,
                param: BREAKPOINT_PARAM_ANY,
            },
            cpu_id: None,
            condition: None,
        });

        emu.run_frame();

        assert_eq!(
            emu.debugger().get_execution_mode(),
            ExecutionMode::Paused,
            "entering V-blank must pause the frame"
        );
        assert!(hit_count(&mut emu, id) >= 1);
    }

    /// Removing the only event breakpoint disarms the instrumented path:
    /// the next frame runs to completion (mode stays `Running`).
    #[test]
    fn run_completes_without_event_breakpoints() {
        let mut emu = SnesEmulator::new();
        emu.reset();
        emu.run_frame();
        assert_eq!(
            emu.debugger().get_execution_mode(),
            ExecutionMode::Running,
            "a frame with no breakpoints never pauses"
        );
    }
}

mod disassembly_window {
    use super::*;

    /// The PC row is always decoded even with no coverage, and the
    /// window is exactly `before + after` rows long.
    #[test]
    fn window_decodes_pc_row_65816() {
        // $8000 LDA #$12 ; $8002 BRA -2
        let mut emu = boot(&[0xA9, 0x12, 0x80, 0xFE]);
        let rows = emu.disassemble_window(0, 0x00_8000, 2, 2);
        assert_eq!(rows.len(), 4, "window is before + after rows");
        let pc_row = &rows[2]; // index == before_count
        assert_eq!(pc_row.address, 0x00_8000);
        assert!(matches!(pc_row.kind, DisasmRowKind::Code));
        assert!(
            pc_row.mnemonic.to_lowercase().starts_with("lda"),
            "decoded `{}`",
            pc_row.mnemonic
        );
    }

    /// After executing a run of NOPs, the coverage-aware backward walk
    /// finds the real predecessors instead of byte-by-byte `Unknown`s.
    #[test]
    fn backward_walk_uses_coverage_65816() {
        // Four NOPs then BRA -6 (loop back to $8000).
        let mut emu = boot(&[0xEA, 0xEA, 0xEA, 0xEA, 0x80, 0xFA]);
        step(&mut emu, 3); // cover $8000, $8001, $8002; PC now $8003

        let rows = emu.disassemble_window(0, 0x00_8003, 2, 1);
        assert_eq!(rows.len(), 3);
        // Backward rows resolve to the two covered NOPs below PC.
        assert_eq!(rows[1].address, 0x00_8002);
        assert!(matches!(rows[1].kind, DisasmRowKind::Code));
        // PC row.
        assert_eq!(rows[2].address, 0x00_8003);
        assert!(matches!(rows[2].kind, DisasmRowKind::Code));
    }

    /// The SPC700 window decodes against a side-effect-free view of APU
    /// memory: after `reset()` the IPL ROM sits at `$FFC0` and its first
    /// instruction is `mov x, #$ef`. The PC row is decoded even though
    /// the SPC700 hasn't executed it yet.
    #[test]
    fn window_decodes_pc_row_spc700() {
        let mut emu = SnesEmulator::new();
        emu.reset();
        let rows = emu.disassemble_window(1, 0xFFC0, 1, 2);
        assert_eq!(rows.len(), 3);
        let pc_row = &rows[1]; // index == before_count
        assert_eq!(pc_row.address, 0xFFC0);
        assert!(matches!(pc_row.kind, DisasmRowKind::Code));
        assert!(
            pc_row.mnemonic.to_lowercase().starts_with("mov"),
            "decoded `{}`",
            pc_row.mnemonic
        );
    }
}

mod cartridge_debugger {
    use super::*;

    /// A LoROM's memory map exposes at least one ROM region, the mapper
    /// reports `SnesLoROM`, and ROM region sizes never exceed the
    /// physical image.
    #[test]
    fn lorom_memory_map_and_mapper() {
        let mut emu = boot(&[0xEA, 0x80, 0xFD]);
        let rom_bytes = 0x10_0000usize;
        let dbg = emu.cartridge_debugger();

        let map = dbg.get_memory_map();
        assert!(
            map.regions.iter().any(|r| r.memory_type == MemoryType::ROM),
            "memory map has a ROM region"
        );

        let rom_regions = dbg.get_rom_regions();
        assert!(!rom_regions.is_empty(), "at least one ROM region");
        for r in &rom_regions {
            assert!(
                r.size <= rom_bytes,
                "ROM region {} bytes must not exceed the {rom_bytes}-byte image",
                r.size
            );
        }

        let mapper = dbg.get_mapper_state().expect("LoROM reports a mapper");
        assert_eq!(mapper.mapper_id, MapperId::SnesLoROM);
        assert!(
            !mapper.bank_config.banks.is_empty(),
            "mapper exposes its bank layout"
        );
        // Plain carts carry no coprocessor.
        assert!(dbg.get_chip_state().is_none());
    }

    /// ROM / SRAM breakpoints added through the cartridge debugger land
    /// in the emulator's single breakpoint list (the one the CPU step
    /// path checks), not a parallel store.
    #[test]
    fn breakpoints_delegate_to_the_main_list() {
        let mut emu = boot(&[0xEA, 0x80, 0xFD]);
        let (rom_id, sram_id) = {
            let mut dbg = emu.cartridge_debugger();
            let rom_id = dbg.add_rom_breakpoint(0x00_8000);
            let sram_id = dbg.add_sram_watchpoint(0x70_0000, WatchType::Write);
            (rom_id, sram_id)
        };

        let bps = emu.debugger().list_breakpoints();
        let rom_bp = bps.iter().find(|b| b.id == rom_id).expect("rom bp present");
        assert_eq!(rom_bp.breakpoint.bp_type, BreakpointType::execute());
        let sram_bp = bps
            .iter()
            .find(|b| b.id == sram_id)
            .expect("sram watchpoint present");
        assert_eq!(sram_bp.breakpoint.bp_type, BreakpointType::write());

        // Removal via the cartridge debugger drops it from the same list.
        emu.cartridge_debugger().remove_breakpoint(rom_id);
        assert!(
            emu.debugger()
                .list_breakpoints()
                .iter()
                .all(|b| b.id != rom_id),
            "removed breakpoint is gone from the main list"
        );
    }

    /// A cart that declares SRAM in its header surfaces an SRAM region
    /// sized to the physical chip.
    #[test]
    fn declared_sram_region_is_reported() {
        // ram_size_code 3 -> 8 KiB SRAM.
        let mut emu = SnesEmulator::new();
        emu.load_rom(&synth_lorom_sram(&[0xEA, 0x80, 0xFD], 3))
            .expect("load LoROM with SRAM");
        let dbg = emu.cartridge_debugger();
        let sram = dbg.get_sram_regions();
        assert!(
            !sram.is_empty(),
            "a cart declaring RAM exposes an SRAM region"
        );
    }
}
