//! Unit tests of the machine as a whole.

use super::*;
use crate::scheduler::EventKind;
use retro_core::cpu::Addressable;
use retro_core::savestate::RomCheck;
use retro_core::{CpuArchitecture, ExecutionMode, InputState};

#[test]
fn system_info_lists_both_cpus() {
    let emu = SnesEmulator::new();
    let info = emu.system_info();
    assert_eq!(info.cpus.len(), 2);
    assert_eq!(info.cpus[0].architecture, CpuArchitecture::Mos65816);
    assert_eq!(info.cpus[1].architecture, CpuArchitecture::Spc700);
    assert_eq!(info.short_name, "SNES");
}

#[test]
fn display_info_matches_snes_geometry() {
    let emu = SnesEmulator::new();
    let display = emu.get_display_info();
    assert_eq!(display.width, 256);
    assert_eq!(display.height, 224);
    assert!((display.refresh_rate - 60.0985).abs() < 1e-4);
}

#[test]
fn display_info_refresh_rate_follows_pal_region() {
    let mut emu = SnesEmulator::new();
    emu.set_region_str("pal").unwrap();
    assert!((emu.get_display_info().refresh_rate - 50.0070).abs() < 1e-4);
    emu.set_region_str("ntsc").unwrap();
    assert!((emu.get_display_info().refresh_rate - 60.0985).abs() < 1e-4);
}

#[test]
fn load_rom_pal_destination_selects_pal_timing_and_refresh_rate() {
    let mut rom = synth_hirom_2kib_sram_rom();
    rom[0xFFB0 + 0x29] = 0x02; // Europe
    let mut emu = SnesEmulator::new();
    emu.load_rom(&rom).expect("loads");
    assert_eq!(emu.timing(), TimingProfile::PAL);
    assert!((emu.get_display_info().refresh_rate - 50.0070).abs() < 1e-4);
}

#[test]
fn repaint_held_frame_runs_over_live_bus() {
    let mut emu = SnesEmulator::new();
    // No ROM/content loaded: this proves the paused-repaint wrapper
    // (render every visible scanline + finalize) runs over the real
    // bus without panicking and leaves a self-consistent framebuffer.
    emu.repaint_held_frame();
    let fb = emu.get_framebuffer();
    assert!(!fb.data.is_empty());
    assert_eq!(fb.data.len(), (fb.width * fb.height * 4) as usize);
}

#[test]
fn bg_layout_wrapper_reads_reset_state() {
    let emu = SnesEmulator::new();
    let layout = emu.bg_layout(0);
    assert_eq!(layout.bg, 0);
    // Reset state: tilemap + character bases default to 0.
    assert_eq!(layout.tilemap_base_byte, 0);
    assert_eq!(layout.char_base_byte, 0);
}

#[test]
fn new_defaults_to_ntsc_timing() {
    let emu = SnesEmulator::new();
    assert_eq!(emu.timing(), TimingProfile::NTSC);
    assert_eq!(emu.master_cycles(), 0);
    assert_eq!(emu.frame_count(), 0);
    assert!(emu.header().is_none());
    assert!(emu.preset_kind().is_none());
}

#[test]
fn audio_disabled_by_default() {
    let emu = SnesEmulator::new();
    assert!(!emu.is_audio_enabled());
    assert_eq!(emu.get_audio_info().sample_rate, 44_100);
}

#[test]
fn focused_cpu_clamps_to_known_range() {
    let mut emu = SnesEmulator::new();
    assert_eq!(emu.focused_cpu(), 0);
    emu.set_focused_cpu(1);
    assert_eq!(emu.focused_cpu(), 1);
    emu.set_focused_cpu(99);
    assert_eq!(emu.focused_cpu(), 1);
}

#[test]
fn load_rom_invalid_returns_err() {
    let mut emu = SnesEmulator::new();
    let bad = vec![0u8; 16];
    let result = emu.load_rom(&bad);
    assert!(
        result.is_err(),
        "expected detection failure on tiny zero rom"
    );
    assert!(emu.get_rom_info().is_none());
}

/// 4 MiB HiROM image with a well-formed header at `$FFB0` declaring
/// 2 KiB battery SRAM, enough for the detector to score it as
/// HiROM and resolve a 2 KiB SRAM chip.
fn synth_hirom_2kib_sram_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x40_0000];
    let offset = 0xFFB0;
    rom[offset + 0x10..offset + 0x10 + 21].copy_from_slice(b"OVERRIDE TEST ROM    ");
    rom[offset + 0x25] = 0x21; // HiROM, slow
    rom[offset + 0x26] = 0x02; // ROM + RAM + battery
    rom[offset + 0x27] = 0x0C; // 4 MiB
    rom[offset + 0x28] = 0x01; // 2 KiB SRAM
    rom[offset + 0x29] = 0x01; // USA
    rom[offset + 0x2A] = 0x01;
    let checksum: u16 = 0x1234;
    rom[offset + 0x2C..offset + 0x2E].copy_from_slice(&(!checksum).to_le_bytes());
    rom[offset + 0x2E..offset + 0x30].copy_from_slice(&checksum.to_le_bytes());
    rom[offset + 0x4C..offset + 0x4E].copy_from_slice(&0x8000u16.to_le_bytes());
    rom
}

#[test]
fn a_pause_reports_where_the_fast_loop_stopped() {
    let mut emu = SnesEmulator::new();
    emu.load_rom(&synth_hirom_2kib_sram_rom()).expect("loads");
    let at_load = emu.debugger.get_cpu_states()[0].pc;
    emu.set_execution_mode(ExecutionMode::Running);
    for _ in 0..3 {
        retro_core::frame::run_frame(&mut emu, &mut retro_core::frame::Play);
    }
    // The fast loop refreshes no snapshot: the debugger still holds the
    // one the reset took.
    assert_eq!(emu.debugger.get_cpu_states()[0].pc, at_load);

    emu.set_execution_mode(ExecutionMode::Paused);

    let live = cpu_state_65816(emu.cpu()).pc;
    assert_ne!(live, at_load);
    assert_eq!(emu.debugger.get_cpu_states()[0].pc, live);
}

#[test]
fn manifest_override_changes_installed_plan() {
    let rom = synth_hirom_2kib_sram_rom();
    let mut emu = SnesEmulator::new();

    // Pure detection: HiROM plan, 2 KiB SRAM.
    emu.load_rom(&rom).expect("loads via detection");
    assert_eq!(emu.get_save_ram().map(<[u8]>::len), Some(2048));
    assert_eq!(emu.bus.cartridge().unwrap().map_plan().name, "hirom");

    // Override: force LoROM + 32 KiB SRAM, then reload.
    let mut over = emu.detected_manifest(&rom);
    over.mapping.preset = "lorom".to_string();
    over.mapping.sram_size = 32 * 1024;
    emu.set_cartridge_manifest_override(Some(over));
    emu.load_rom(&rom).expect("loads with override");
    assert_eq!(emu.get_save_ram().map(<[u8]>::len), Some(32 * 1024));
    assert_eq!(emu.bus.cartridge().unwrap().map_plan().name, "lorom");

    // Clearing the override reverts to pure detection.
    emu.set_cartridge_manifest_override(None);
    emu.load_rom(&rom).expect("loads after clear");
    assert_eq!(emu.get_save_ram().map(<[u8]>::len), Some(2048));
    assert_eq!(emu.bus.cartridge().unwrap().map_plan().name, "hirom");
}

#[test]
fn reset_without_rom_does_not_panic() {
    let mut emu = SnesEmulator::new();
    emu.reset();
    // No cart -> bus returns 0 from open-bus -> reset vector
    // resolves to 0x0000.
    assert_eq!(emu.cpu().pc, 0x0000);
    assert_eq!(emu.cpu().pbr, 0x00);
}

#[test]
fn power_on_reset_zeroes_wram() {
    let mut emu = SnesEmulator::new();
    emu.load_rom(&synth_hirom_2kib_sram_rom()).expect("loads");

    // Dirty WRAM through the real bus write path ($7E_0000.. is WRAM).
    for i in 0..256u32 {
        emu.bus.write_fast(0x7E_0000 + i, 0xAB);
    }
    assert!(
        emu.bus.wram().iter().any(|&b| b != 0),
        "WRAM should be dirty before the cold reset"
    );

    emu.power_on_reset();

    assert!(
        emu.bus.wram().iter().all(|&b| b == 0),
        "power-on reset must zero WRAM"
    );
    assert_eq!(emu.frame_count(), 0);
}

/// What the SNES alone keeps across a power cycle, beside what the machine
/// conformance suite checks on both consoles: the overscan preference,
/// which the bus the load builds again must take from the emulator.
#[test]
fn a_power_cycle_keeps_the_overscan_preference() {
    let mut emu = SnesEmulator::new();
    emu.load_rom(&synth_hirom_2kib_sram_rom()).expect("loads");
    emu.set_show_overscan(true);
    emu.power_on_reset();
    assert!(emu.show_overscan_enabled());
    assert!(emu.ppu().show_overscan_enabled());
    emu.hard_reset();
    assert!(emu.ppu().show_overscan_enabled());
}

/// A 1 MiB LoROM image (FastROM) with 8 KiB of battery SRAM. Its boot code
/// counts its boots in work RAM ($12), waits about five frames with the
/// NMI not enabled, stores in $11 how many NMIs ran during that wait, then
/// enables the NMI and sets BG mode 1, BG1 on the main screen and the
/// display on at full brightness. The NMI handler counts NMIs in $10.
fn boot_counting_rom() -> Vec<u8> {
    #[rustfmt::skip]
    const BOOT: [u8; 44] = [
        0x78,             // SEI
        0xA9, 0x80,       // LDA #$80
        0x8D, 0x00, 0x21, // STA $2100: forced blank
        0x64, 0x10,       // STZ $10: the NMI count
        0xE6, 0x12,       // INC $12: boots
        0xA0, 0x00,       // LDY #$00
        0xA2, 0x00,       // outer: LDX #$00
        0xCA,             // inner: DEX
        0xD0, 0xFD,       // BNE inner
        0x88,             // DEY
        0xD0, 0xF8,       // BNE outer
        0xA5, 0x10,       // LDA $10
        0x85, 0x11,       // STA $11: the NMIs that ran during the wait
        0xA9, 0x80,       // LDA #$80
        0x8D, 0x00, 0x42, // STA $4200: NMI on
        0xA9, 0x01,       // LDA #$01
        0x8D, 0x05, 0x21, // STA $2105: BG mode 1
        0x8D, 0x2C, 0x21, // STA $212C: BG1 on the main screen
        0xA9, 0x0F,       // LDA #$0F
        0x8D, 0x00, 0x21, // STA $2100: display on, brightness 15
        0x80, 0xFE,       // BRA *
    ];
    let mut chip = vec![0u8; 0x10_0000];
    chip[..BOOT.len()].copy_from_slice(&BOOT);
    // NMI: INC $10 / LDA $4210 / RTI. IRQ: RTI.
    chip[0x40..0x46].copy_from_slice(&[0xE6, 0x10, 0xAD, 0x10, 0x42, 0x40]);
    chip[0x60] = 0x40;
    let h = 0x7FB0;
    chip[h + 0x10..h + 0x10 + 21].copy_from_slice(b"RESET LINE TEST      ");
    chip[h + 0x25] = 0x30; // LoROM, FastROM
    chip[h + 0x26] = 0x02; // ROM, RAM and battery
    chip[h + 0x27] = 0x0A; // 1 MiB
    chip[h + 0x28] = 0x03; // 8 KiB of RAM
    chip[h + 0x29] = 0x01; // North America: NTSC
    chip[h + 0x2A] = 0x01;
    let checksum: u16 = 0x1234;
    chip[h + 0x2C..h + 0x2E].copy_from_slice(&(!checksum).to_le_bytes());
    chip[h + 0x2E..h + 0x30].copy_from_slice(&checksum.to_le_bytes());
    chip[0x7FFA..0x7FFC].copy_from_slice(&0x8040u16.to_le_bytes());
    chip[0x7FFC..0x7FFE].copy_from_slice(&0x8000u16.to_le_bytes());
    chip[0x7FFE..0x8000].copy_from_slice(&0x8060u16.to_le_bytes());
    chip
}

/// The reset button does what the console's reset line does: the 5A22's
/// registers, its DMA channels and the PPU's registers go back to the
/// values a power-on gives and the sprite table is cleared, and work RAM,
/// VRAM, CGRAM, the sound processor's memory and the battery save keep the
/// game's. So the boot's wait, which runs with no NMI after a power-on,
/// runs with none after a reset: the NMI the game had enabled is off again.
#[test]
fn the_reset_button_resets_the_registers_and_clears_the_sprite_table() {
    use crate::dma::DmaController;
    use crate::ppu::Ppu;
    use crate::savestate::PpuState;
    use crate::system_regs::SystemRegs;

    // The NMIs during the boot's wait ($11) and the boots ($12).
    let counts = |emu: &mut SnesEmulator| {
        let ram = emu.read_memory_debug(0x7E_0011, 2);
        (ram[0], ram[1])
    };
    // The PPU's state but VRAM and CGRAM, which a reset keeps.
    let registers = |state: PpuState| {
        let mut state = state;
        state.vram.clear();
        state.cgram.clear();
        format!("{state:?}")
    };

    let mut emu = SnesEmulator::new();
    emu.load_rom(&boot_counting_rom()).expect("loads");
    emu.set_execution_mode(ExecutionMode::Running);
    emu.write_vram_debug(0, &[0xAB, 0xCD]);
    emu.write_cgram_debug(1, &[0x001F]);
    emu.write_oam_debug(0, &[0x10, 0x20, 0x30, 0x40]);
    emu.write_oam_debug(0x21F, &[0x55]);
    emu.bus.apu_mut().spc_write(0x8000, 0x77);
    emu.set_save_ram(&[0x5A; 8192]);
    for _ in 0..10 {
        emu.run_frame();
    }
    assert_eq!(counts(&mut emu), (0, 1));
    assert_eq!(emu.bus.system_regs().nmitimen(), 0x80);
    assert_eq!(emu.ppu().bgmode(), 1);
    assert_eq!(emu.ppu().display_brightness(), 15);

    // What else a game leaves in the 5A22 and its channels, and what a
    // register write leaves for the next instruction boundary.
    emu.bus.write_fast(0x00_420D, 0x01); // FastROM on
    emu.bus.write_fast(0x00_4202, 0x12); // WRMPYA
    emu.bus.write_fast(0x00_4302, 0x34); // channel 0's A-bus address
    emu.bus.write_fast(0x00_2181, 0x56); // the WRAM port's address
    emu.bus.write_fast(0x00_420B, 0x01); // a general DMA asked for
    emu.bus.system_regs_mut().set_nmi_flag(true);
    emu.bus.write_fast(0x00_4200, 0x00);
    emu.bus.write_fast(0x00_4200, 0x80); // an NMI enabled with its flag up
    assert_eq!(emu.bus.cycles_for(0x80_8000), 6);

    emu.reset();

    assert_eq!(
        format!("{:?}", emu.bus.system_regs().to_savestate()),
        format!("{:?}", SystemRegs::new().to_savestate())
    );
    assert_eq!(
        format!("{:?}", emu.bus.dma().to_savestate()),
        format!("{:?}", DmaController::new().to_savestate())
    );
    assert_eq!(
        registers(emu.ppu().to_savestate()),
        registers(Ppu::new().to_savestate())
    );
    assert!(!emu.bus.memsel_fast());
    assert_eq!(emu.bus.cycles_for(0x80_8000), 8);
    let bus = emu.bus.to_savestate();
    assert_eq!(bus.wmdata.wm_addr, 0);
    assert_eq!(bus.pending_general_dma, 0);
    assert!(!bus.pending_irq_reschedule);
    assert!(!bus.pending_nmi);
    assert_eq!((bus.cached_h_dot, bus.cached_v_scanline), (0, 0));
    // The sprite table is cleared, both tables (the comparison above holds
    // OAM to a new PPU's); work RAM, VRAM, CGRAM, the sound processor's
    // memory and the save keep what the game left there.
    let ppu = emu.ppu().to_savestate();
    assert!(ppu.oam.iter().all(|&b| b == 0));
    assert_eq!(counts(&mut emu), (0, 1));
    assert_eq!(ppu.vram[0], 0xCDAB);
    assert_eq!(ppu.cgram[1], 0x001F);
    assert_eq!(emu.bus.apu().aram()[0x8000], 0x77);
    assert_eq!(emu.get_save_ram().map(|s| s[0]), Some(0x5A));

    for _ in 0..10 {
        emu.run_frame();
    }
    assert_eq!(counts(&mut emu), (0, 2));
    assert_eq!(emu.bus.system_regs().nmitimen(), 0x80);
    assert_eq!(emu.ppu().bgmode(), 1);
}

#[test]
fn step_cpu_one_returns_empty_step_result() {
    let mut emu = SnesEmulator::new();
    let result = emu.step_cpu(1);
    assert_eq!(result.cycles, 0);
    assert!(!result.instruction_executed);
    assert!(!result.breakpoint_hit);
}

#[test]
fn step_cpu_out_of_range_returns_empty_step_result() {
    let mut emu = SnesEmulator::new();
    let result = emu.step_cpu(7);
    assert_eq!(result.cycles, 0);
    assert!(!result.instruction_executed);
}

#[test]
fn get_audio_samples_empty_when_no_producer() {
    let mut emu = SnesEmulator::new();
    let samples = emu.get_audio_samples();
    assert!(samples.is_empty());
}

#[test]
fn set_input_writes_through_to_port_state() {
    let mut emu = SnesEmulator::new();
    emu.set_input(
        0,
        InputState {
            buttons: 0xABCD_0000,
        },
    );
    assert_eq!(emu.input_manager.get_port_state(0), 0xABCD_0000);
}

#[test]
fn set_input_out_of_range_player_is_silent_noop() {
    let mut emu = SnesEmulator::new();
    emu.set_input(
        9,
        InputState {
            buttons: 0xFFFFFFFF,
        },
    );
    assert_eq!(emu.input_manager.get_port_state(0), 0);
    assert_eq!(emu.input_manager.get_port_state(1), 0);
}

#[test]
fn set_input_refreshes_controller_pads() {
    use crate::controller::BTN_B;
    let mut emu = SnesEmulator::new();
    emu.set_input(
        0,
        InputState {
            buttons: 1 << BTN_B,
        },
    );
    // The pad now holds B (0x8000); an auto-read surfaces it in
    // JOY1H ($4219 bit 7).
    emu.bus_mut().auto_joypad_read();
    assert_eq!(emu.bus_mut().read(0x00_4219) & 0x80, 0x80);
}

#[test]
fn boots_with_keyboard_provider_bound_to_port_zero() {
    // The keyboard path (`keyDown`/`keyUp` -> `update_keyboard`) only
    // reaches a port if a keyboard provider is registered AND
    // assigned. A manager built with no providers registers none,
    // which is exactly the bug this guards against.
    let emu = SnesEmulator::new();
    assert_eq!(
        emu.input_manager().get_port_assignment(0),
        Some("keyboard".to_string()),
        "snes1 must boot with the default keyboard bound to port 0"
    );
}

#[test]
fn keyboard_key_drives_port_zero_state() {
    let mut emu = SnesEmulator::new();
    emu.input_manager_mut()
        .update_keyboard("ArrowUp".to_string(), true);
    assert_ne!(
        emu.input_manager().get_port_state(0),
        0,
        "a default-mapped key press must reach port 0"
    );
}

#[test]
fn keyboard_keys_map_to_their_snes_hardware_bits() {
    let mut emu = SnesEmulator::new();
    let bit = |emu: &SnesEmulator| input_state_to_snes_pad(emu.input_manager().get_port_state(0));

    // Press one default key at a time and confirm it lands on the
    // SNES button the layout promises (not the Genesis position the
    // shared provider would otherwise pick).
    let cases = [
        ("KeyZ", 0x8000u16),    // B
        ("KeyX", 0x0080),       // A
        ("KeyA", 0x4000),       // Y
        ("KeyS", 0x0040),       // X
        ("KeyQ", 0x0020),       // L
        ("KeyW", 0x0010),       // R
        ("Enter", 0x1000),      // Start
        ("ShiftRight", 0x2000), // Select
        ("ArrowUp", 0x0800),    // Up
    ];
    for (code, expected) in cases {
        emu.input_manager_mut()
            .update_keyboard(code.to_string(), true);
        assert_eq!(bit(&emu), expected, "{code} must map to {expected:#06x}");
        emu.input_manager_mut()
            .update_keyboard(code.to_string(), false);
    }
}

#[test]
fn keyboard_key_reaches_the_hardware_pad_end_to_end() {
    // Full path: key press -> port state -> controller pad -> auto-read
    // register. `KeyZ` is B, which surfaces in JOY1H ($4219 bit 7).
    let mut emu = SnesEmulator::new();
    emu.input_manager_mut()
        .update_keyboard("KeyZ".to_string(), true);
    emu.refresh_controllers_from_input();
    emu.bus_mut().auto_joypad_read();
    assert_eq!(emu.bus_mut().read(0x00_4219) & 0x80, 0x80);
}

#[test]
fn auto_registered_gamepad_uses_snes_face_layout() {
    use crate::controller::{BTN_B, BTN_X};
    let mut emu = SnesEmulator::new();
    // First input from an unseen pad auto-registers it (onto port 0,
    // beside the keyboard) with snes1's default gamepad map: Web
    // button 0 -> B.
    emu.input_manager_mut()
        .update_gamepad_button("pad-0".to_string(), 0, true);
    assert_eq!(emu.input_manager().provider_port("pad-0"), Some(0));
    let mask = emu.input_manager().get_port_state(0);
    assert_eq!(
        mask & (1 << BTN_B),
        1 << BTN_B,
        "gamepad button 0 must be SNES B"
    );
    // Guard against the Genesis default leaking through: Genesis
    // maps button 0 -> bit 6, which on the SNES layout is X.
    assert_eq!(
        mask & (1 << BTN_X),
        0,
        "must not fall back to the Genesis button-0 -> bit-6 mapping"
    );
}

#[test]
fn button_layout_lists_twelve_snes_buttons() {
    let emu = SnesEmulator::new();
    let layout = &emu.get_input_info().button_layout;
    assert_eq!(layout.len(), 12);
    // Ids are unique and cover 0..=11.
    let mut ids: Vec<u32> = layout.iter().map(|b| b.id).collect();
    ids.sort_unstable();
    assert_eq!(ids, (0..12).collect::<Vec<_>>());
    // Spot-check a couple of names line up with the convention.
    let by_name = |n: &str| layout.iter().find(|b| b.name == n).map(|b| b.id);
    assert_eq!(by_name("Up"), Some(crate::controller::BTN_UP));
    assert_eq!(by_name("Select"), Some(crate::controller::BTN_SELECT));
}

#[test]
fn destination_region_str_known_codes() {
    assert_eq!(destination_region_str(0x00), "Japan");
    assert_eq!(destination_region_str(0x01), "USA");
    assert_eq!(destination_region_str(0x02), "Europe");
    assert_eq!(destination_region_str(0x09), "Germany");
    assert_eq!(destination_region_str(0xFF), "Unknown");
}

#[test]
fn preset_kind_name_lorom_hirom_exhirom_custom() {
    assert_eq!(preset_kind_name(PresetKind::LoRom), "LoROM");
    assert_eq!(preset_kind_name(PresetKind::HiRom), "HiROM");
    assert_eq!(preset_kind_name(PresetKind::ExHiRom), "ExHiROM");
    assert_eq!(preset_kind_name(PresetKind::Sdd1), "S-DD1");
    assert_eq!(preset_kind_name(PresetKind::Spc7110), "SPC7110");
    assert_eq!(preset_kind_name(PresetKind::Sa1), "SA-1");
    assert_eq!(preset_kind_name(PresetKind::Custom), "Custom");
}

#[test]
fn save_state_emits_envelope_and_round_trips_without_rom() {
    // Even with no ROM loaded the emulator can snapshot/restore its
    // volatile state. The bytes carry the retro-core magic prefix.
    let mut emu = SnesEmulator::new();
    emu.reset();
    let bytes = emu.save_state();
    assert!(!bytes.is_empty());
    assert!(retro_core::savestate::has_magic(&bytes));
    assert!(emu.load_state(&bytes, RomCheck::SameRom).is_ok());
}

#[test]
fn load_state_rejects_empty_and_garbage() {
    let mut emu = SnesEmulator::new();
    // No magic prefix -> clear error, not a panic.
    assert!(emu.load_state(&[], RomCheck::SameRom).is_err());
    assert!(emu
        .load_state(&[0xDE, 0xAD, 0xBE, 0xEF], RomCheck::SameRom)
        .is_err());
}

#[test]
fn save_ram_is_none_without_a_cartridge() {
    let mut emu = SnesEmulator::new();
    assert!(emu.get_save_ram().is_none());
    emu.set_save_ram(&[1, 2, 3]); // no cartridge installed -> no-op, must not panic
    assert!(emu.get_save_ram().is_none());
}

#[test]
fn disassemble_decodes_65816_and_skips_other_cpus() {
    let mut emu = SnesEmulator::new();
    // Forward-decode `count` 65C816 instructions; addresses advance by
    // each instruction's byte length.
    let rows = emu.disassemble_at(0, 10);
    assert_eq!(rows.len(), 10);
    assert_eq!(rows[0].address, 0);
    assert!(rows[1].address > rows[0].address);
    assert!(!rows[0].mnemonic.is_empty());
    assert_eq!(emu.disassemble_cpu(0, 0, 10).len(), 10);
    // The SPC700 decoder is wired: cpu 1 forward-decodes its memory
    // (ARAM here) against a side-effect-free view.
    let spc = emu.disassemble_cpu(1, 0, 10);
    assert_eq!(spc.len(), 10);
    assert_eq!(spc[0].address, 0);
    assert!(!spc[0].mnemonic.is_empty());
    // Slot 2 stays empty: no cartridge is loaded to host a CPU there.
    assert!(emu.disassemble_cpu(2, 0, 10).is_empty());
}

#[test]
fn read_memory_debug_returns_open_bus_when_no_rom() {
    let mut emu = SnesEmulator::new();
    let bytes = emu.read_memory_debug(0x00_8000, 4);
    assert_eq!(bytes.len(), 4);
    // No cart installed -> cartridge pages stay as OpenBus ->
    // reads return 0 from the MDR latch.
    assert_eq!(bytes, vec![0, 0, 0, 0]);
}

#[test]
fn ppu_accessor_returns_post_reset_state() {
    let emu = SnesEmulator::new();
    // PPU is reset on construction -> display forced blank,
    // brightness 0, VRAM auto-increment step 1, all memories
    // zeroed.
    assert!(emu.ppu().display_disable());
    assert_eq!(emu.ppu().display_brightness(), 0);
    assert_eq!(emu.ppu().vram_incsize(), 1);
    assert!(emu.ppu().vram().iter().all(|&w| w == 0));
}

#[test]
fn reset_zeros_framebuffer_bytes() {
    let mut emu = SnesEmulator::new();
    // Scribble through the framebuffer to prove `reset` clears.
    for byte in emu.framebuffer.data.iter_mut() {
        *byte = 0xAB;
    }
    emu.reset();
    assert!(emu.framebuffer.data.iter().all(|&b| b == 0));
}

#[test]
fn run_frame_renders_visible_scanlines_via_render_pos() {
    // Forced blank + brightness 0 means the renderer writes
    // black RGB with alpha 0xFF on every visible line that
    // RenderPos fires. The renderer maps SNES scanline N -> host
    // fb row N-1, so a full frame should flip alpha to 0xFF on
    // rows 0..=223 (NTSC visible lines 1..=224 -> rows 0..=223).
    // Skipping happens at the renderer for scanline 0 (pre-
    // render), so there's no untouched row at the top after
    // the first frame.
    let mut emu = SnesEmulator::new();
    emu.reset();
    emu.run_frame();
    let stride = (emu.framebuffer.width * 4) as usize;
    // First host row (= SNES scanline 1) must be alpha=0xFF.
    assert_eq!(
        emu.framebuffer.data[3], 0xFF,
        "row 0 alpha set by render_scanline(scanline=1)"
    );
    // Last host row (= SNES scanline 224) must also be touched.
    let last_visible_offset = stride * 223 + 3;
    assert_eq!(
        emu.framebuffer.data[last_visible_offset], 0xFF,
        "row 223 alpha set by render_scanline(scanline=224)"
    );
}

#[test]
fn framebuffer_allocates_max_hires_size() {
    // The emulator owns a 512x480 framebuffer regardless of the
    // live SNES mode so a hot-swap into Mode 5/6 (and interlace,
    // which doubles the height to 448/480) doesn't need a
    // reallocation. The host crops via the dynamic `DisplayInfo`
    // (see `get_display_info`).
    let emu = SnesEmulator::new();
    assert_eq!(emu.framebuffer.width, 512);
    assert_eq!(emu.framebuffer.height, 480);
    assert_eq!(emu.framebuffer.data.len(), 512 * 480 * 4);
}

#[test]
fn display_info_sync_tracks_hires_and_overscan() {
    // Default reset: bgmode 0 + overscan off -> 256x224.
    let mut emu = SnesEmulator::new();
    emu.reset();
    assert_eq!(emu.get_display_info().width, 256);
    assert_eq!(emu.get_display_info().height, 224);

    // Poke Mode 5 + overscan via PPU registers. The reported size is
    // FRAME-GLOBAL (built each frame at scanline 1), so render one
    // scanline to set the flags before syncing, mirrors what
    // VBlankStart does (finalize + sync after the frame's scanlines
    // render) without needing a full CPU frame / ROM.
    // `$2105 = 0x05` -> bgmode 5; `$2133 = 0x04` -> overscan.
    // Any hi-res frame reports doubled height (bsnes doubles
    // non-interlace hi-res for display). By default overscan is
    // cropped to the TV window, so overscan Mode 5 -> 512x448.
    emu.bus.write(0x00_2105, 0x05);
    emu.bus.write(0x00_2133, 0x04);
    emu.bus.ppu_mut().render_scanline(1, &mut emu.framebuffer);
    emu.sync_display_info();
    assert_eq!(emu.get_display_info().width, 512);
    assert_eq!(emu.get_display_info().height, 448);

    // The overscan toggle exposes the whole frame, 512x478, and the
    // setter re-syncs display info immediately.
    emu.set_show_overscan(true);
    assert_eq!(emu.get_display_info().height, 478);
    emu.set_show_overscan(false);
    assert_eq!(emu.get_display_info().height, 448);

    // Mode 5 + interlace ($2133 bit 0, overscan off) -> 512x448
    // through the same path.
    emu.bus.write(0x00_2133, 0x01);
    emu.bus.ppu_mut().render_scanline(1, &mut emu.framebuffer);
    emu.sync_display_info();
    assert_eq!(emu.get_display_info().width, 512);
    assert_eq!(emu.get_display_info().height, 448);
}

#[test]
fn mixed_mode_frame_doubles_lores_band_to_full_width() {
    // Simulate an HDMA mid-frame BGMODE sweep (as in the PPU bus
    // activity test): a lo-res band (Mode 0, fills cols 0..255 only)
    // followed by a hires band (Mode 5, fills all 512). The hires band
    // forces the frame-global width to 512; `finalize_frame` (run at
    // VBlankStart, before the size sync) must expand the lo-res band to
    // fill all 512 columns so the host doesn't see a half-width right
    // edge.
    let mut emu = SnesEmulator::new();
    emu.reset();
    emu.bus.write(0x00_2100, 0x0F); // display on, full brightness

    // Scanline 1: Mode 0 (lo-res) -> row 0 fills cols 0..255.
    emu.bus.write(0x00_2105, 0x00);
    emu.bus.ppu_mut().render_scanline(1, &mut emu.framebuffer);
    // Scanline 2: Mode 5 (hires) -> row 1 fills all 512 cols.
    emu.bus.write(0x00_2105, 0x05);
    emu.bus.ppu_mut().render_scanline(2, &mut emu.framebuffer);

    // The hires band forces a frame-global 512 width and (because
    // this frame is non-interlace hi-res) a doubled 448 height.
    assert_eq!(emu.bus.ppu().active_display_size(), (512, 448));

    // Run the finalize post-pass the way VBlankStart does.
    emu.bus.ppu().finalize_frame(&mut emu.framebuffer);

    // With no BG enabled the composite is the (black) backdrop, so
    // the doubled lo-res band's right half is now opaque black,
    // proof finalize filled cols 256..511 (untouched by the Mode 0
    // render, which only wrote cols 0..255).
    let stride = 512 * 4usize;
    let right = 300 * 4; // a right-half display column
    assert_eq!(
        &emu.framebuffer.data[right..right + 4],
        &[0, 0, 0, 0xFF][..],
        "lo-res band right half filled by finalize doubling"
    );
    // Non-interlace vertical doubling copies source row 0 to output
    // rows 0 and 1, so output row 1 mirrors the lo-res band.
    let dup = stride + right;
    assert_eq!(
        &emu.framebuffer.data[dup..dup + 4],
        &[0, 0, 0, 0xFF][..],
        "output row 1 is the vertical-doubled copy of lo-res row 0"
    );
    // The hires band (source row 1) moved to output rows 2..3 and
    // already spanned 512 columns before doubling.
    let p2 = 2 * stride + right;
    assert_eq!(
        &emu.framebuffer.data[p2..p2 + 4],
        &[0, 0, 0, 0xFF][..],
        "hires band lands at output row 2 after vertical doubling"
    );
}

#[test]
fn handle_event_vblank_start_toggles_ppu_field() {
    let mut emu = SnesEmulator::new();
    emu.reset();
    // Fresh reset -> field = false.
    assert!(!emu.bus.ppu().field());
    emu.handle_event(EventKind::VBlankStart);
    assert!(emu.bus.ppu().field(), "field flips on first VBlankStart");
    emu.handle_event(EventKind::VBlankStart);
    assert!(
        !emu.bus.ppu().field(),
        "second VBlankStart returns to false"
    );
}

#[test]
fn hblank_start_sets_hvbjoy_bit6_and_render_pos_clears_it() {
    // Without an HBlankStart handler $4212 bit 6 would stay clear, and
    // a game polling `lda $4212 / bit #$40` (Chrono Trigger's NMI
    // handler) would spin forever. HBlankStart (dot 274) must raise it;
    // RenderPos (dot 22, active display resumes) must lower it.
    let mut emu = SnesEmulator::new();
    emu.reset();
    assert_eq!(
        emu.bus.system_regs_mut().read(0x4212) & 0x40,
        0x00,
        "HBlank flag clear after reset"
    );
    emu.handle_event(EventKind::HBlankStart);
    assert_eq!(
        emu.bus.system_regs_mut().read(0x4212) & 0x40,
        0x40,
        "HBlankStart raises $4212 bit 6"
    );
    emu.handle_event(EventKind::RenderPos);
    assert_eq!(
        emu.bus.system_regs_mut().read(0x4212) & 0x40,
        0x00,
        "RenderPos lowers the HBlank flag"
    );
}

#[test]
fn deferred_hdma_event_re_anchors_raster_snapshot_to_scheduled_hblank_dot() {
    // A long atomic general DMA folds many scanlines into one step, so
    // the scheduler drains a *batch* of per-line HDMA triggers at once
    // while the live `master_cycles` has already walked deep into a later
    // active-display line. Handling each trigger against `master_cycles`
    // would snapshot the raster inside the active-scan window (dots
    // 22..274), so the CGRAM active-scan gate would redirect the per-line
    // HDMA water-palette writes to the internal address (colour 0) and
    // the sea of Yoshi's Island's intro would flash white.
    // `handle_event_at` must instead re-anchor the snapshot to each
    // event's own scheduled H-blank dot (276).
    let mut emu = SnesEmulator::new();
    emu.reset();
    // Display on so the active-scan gate is armed (blanked => never gates).
    emu.bus.write(0x00_2100, 0x0F);

    // Poison the snapshot to a mid-active-display dot on a *later* visible
    // line, exactly where the live cursor sits after a long general DMA.
    // Dot 150 in [22, 274) and line 120 is visible, i.e. inside the gate
    // (a CGRAM write here would be redirected to colour 0).
    emu.bus.set_raster_snapshot(150, 120);

    // A per-line HDMA trigger for line 100 is armed at dot 276 (H-blank),
    // the exact cycle the scheduler assigns it (`line_base + 276 * 4`).
    let cps = emu.timing.cycles_per_scanline as u64;
    let trigger_cycle = emu.frame_base_cycle + 100 * cps + 276 * 4;
    assert_eq!(
        emu.raster_position(trigger_cycle),
        (100, 276),
        "the trigger's scheduled cycle decodes to line 100, dot 276"
    );

    emu.handle_event_at(trigger_cycle, EventKind::HdmaTrigger, &mut ());

    // The snapshot now reflects the trigger's own H-blank dot, not the
    // poisoned mid-active cursor: dot 276 is >= the active-scan gate's
    // H-end (274), so the per-line HDMA CGRAM/OAM writes land at their
    // real target address instead of being redirected to colour 0.
    assert_eq!(
        (emu.bus.cached_h_dot(), emu.bus.cached_v_scanline()),
        (276, 100),
        "HdmaTrigger re-anchors the raster snapshot to its scheduled dot"
    );

    // HdmaInit (top-of-frame, dot 6) re-anchors the same way.
    emu.bus.set_raster_snapshot(150, 120);
    let init_cycle = emu.frame_base_cycle + 6 * 4;
    emu.handle_event_at(init_cycle, EventKind::HdmaInit, &mut ());
    assert_eq!(
        (emu.bus.cached_h_dot(), emu.bus.cached_v_scanline()),
        (6, 0),
        "HdmaInit re-anchors the raster snapshot to its scheduled dot"
    );
}

#[test]
fn vblank_and_hblank_flags_are_independent() {
    // The VBlank handlers must drive only bit 7, leaving the HBlank
    // bit (6) the scheduler manages untouched, otherwise the
    // VBlank edges would stomp an in-progress HBlank flag.
    let mut emu = SnesEmulator::new();
    emu.reset();
    emu.handle_event(EventKind::VBlankStart);
    emu.handle_event(EventKind::HBlankStart);
    assert_eq!(
        emu.bus.system_regs_mut().read(0x4212) & 0xC0,
        0xC0,
        "VBlank (bit 7) and HBlank (bit 6) coexist"
    );
    emu.handle_event(EventKind::VBlankEnd);
    assert_eq!(
        emu.bus.system_regs_mut().read(0x4212) & 0xC0,
        0x40,
        "VBlankEnd clears bit 7 but preserves the HBlank bit 6"
    );
}

#[test]
fn handle_event_vblank_start_resets_oam_address_when_display_enabled() {
    let mut emu = SnesEmulator::new();
    emu.reset();
    emu.bus.write(0x00_2100, 0x0F); // display on, full brightness
    emu.bus.write(0x00_2102, 0x10); // OAMADDL -> re-anchors oam_addr to base
    let base = emu.bus.ppu().oam_addr();
    // Two OAMDATA writes walk the pointer off the base.
    emu.bus.write(0x00_2104, 0xAA);
    emu.bus.write(0x00_2104, 0xBB);
    assert_ne!(
        emu.bus.ppu().oam_addr(),
        base,
        "writes advanced the pointer"
    );

    emu.handle_event(EventKind::VBlankStart);
    assert_eq!(
        emu.bus.ppu().oam_addr(),
        base,
        "VBlank entry re-anchors OAM address to base when display is on"
    );
}

#[test]
fn handle_event_vblank_start_keeps_oam_address_in_forced_blank() {
    let mut emu = SnesEmulator::new();
    emu.reset();
    emu.bus.write(0x00_2100, 0x80); // forced blank -> display disabled
    emu.bus.write(0x00_2102, 0x10);
    emu.bus.write(0x00_2104, 0xAA);
    emu.bus.write(0x00_2104, 0xBB);
    let advanced = emu.bus.ppu().oam_addr();

    emu.handle_event(EventKind::VBlankStart);
    assert_eq!(
        emu.bus.ppu().oam_addr(),
        advanced,
        "forced blank suppresses the VBlank OAM reset"
    );
}

#[test]
fn step_cpu_refreshes_bus_raster_snapshot() {
    let mut emu = SnesEmulator::new();
    // The frame base starts at 0. Stepping the CPU once should
    // advance the master clock and refresh the snapshot to the
    // frame-relative raster position at that boundary.
    emu.step_without_events();
    let (v, h) = emu.raster_position(emu.master_cycles);
    assert_eq!(emu.bus.cached_h_dot(), h);
    assert_eq!(emu.bus.cached_v_scanline(), v);
}

#[test]
fn vblank_nmi_reads_vblank_vcounter_after_frame_overshoot() {
    // Regression: the CPU-visible vcounter must be decoded against
    // `frame_base_cycle`, not an absolute `master_cycles %
    // cycles_per_frame` grid. With the frame base pushed off the
    // absolute grid (the overshoot a real run accumulates), the
    // VBlankStart event and the `$213D` latch must still agree that
    // the raster is at the vblank start line.
    let mut emu = SnesEmulator::new();
    let cps = emu.timing.cycles_per_scanline as u64;
    let vbl = emu.timing.vblank_start_line as u64;
    // Anchor the frame base ~100 scanlines off the absolute grid.
    emu.frame_base_cycle = 100 * cps + 37;
    // The VBlankStart event is armed at frame_base + vbl * cps.
    emu.master_cycles = emu.frame_base_cycle + vbl * cps;
    let (v, _h) = emu.raster_position(emu.master_cycles);
    assert_eq!(
        v, emu.timing.vblank_start_line,
        "vcounter at the VBlankStart cycle must read the vblank start line \
         regardless of how far frame_base_cycle sits off the absolute grid"
    );
}

/// Write a `lda $4210 / bpl` vblank-wait at `base` in low RAM
/// (the `$00:0000-$1FFF` WRAM mirror the CPU can fetch from).
fn write_vblank_poll_loop(emu: &mut SnesEmulator, base: u32) {
    emu.bus.write(base, 0xAD); // LDA $4210 (absolute)
    emu.bus.write(base + 1, 0x10);
    emu.bus.write(base + 2, 0x42);
    emu.bus.write(base + 3, 0x10); // BPL -5 -> back to the LDA
    emu.bus.write(base + 4, 0xFB);
}

/// Phase A: the in-flight instruction at VBlank is the poll's
/// `LDA $4210`. It reads the freshly-latched flag the very first
/// step, so delivery releases the NMI immediately, but only *after*
/// the poll observed bit 7.
#[test]
fn nmi_delivery_releases_after_poll_observes_flag_lda_in_flight() {
    let mut emu = SnesEmulator::new();
    emu.reset();
    emu.bus.write(0x00_4200, 0x80); // NMITIMEN: NMI on, no auto-joypad
    write_vblank_poll_loop(&mut emu, 0x0200);
    emu.cpu.pbr = 0;
    emu.cpu.dbr = 0;
    emu.cpu.pc = 0x0200; // in-flight = LDA $4210

    emu.handle_event(EventKind::VBlankStart);
    assert!(
        emu.nmi_deliver_armed,
        "VBlankStart arms delivery with NMI enabled"
    );
    assert!(
        !emu.cpu.nmi_pending,
        "NMI must not be vectored at the boundary"
    );

    emu.step_without_events();
    assert_eq!(emu.cpu.c & 0xFF, 0x80, "the poll read the latched NMI flag");
    assert!(
        emu.cpu.nmi_pending,
        "NMI delivered once the poll observed the flag"
    );
    assert!(!emu.nmi_deliver_armed, "delivery disarmed after firing");
}

/// Phase B: the in-flight instruction is the poll's `BPL`. A fixed
/// 1-instruction window loses here (the NMI vectors before the next
/// `LDA` can read the flag, so the handler clears it and the loop
/// spins forever: the Chrono Trigger black screen). Poll-aware
/// delivery holds off until the following `LDA` observes bit 7.
#[test]
fn nmi_delivery_holds_off_until_poll_reads_flag_bpl_in_flight() {
    let mut emu = SnesEmulator::new();
    emu.reset();
    emu.bus.write(0x00_4200, 0x80);
    write_vblank_poll_loop(&mut emu, 0x0200);
    emu.cpu.pbr = 0;
    emu.cpu.dbr = 0;
    emu.cpu.pc = 0x0203; // in-flight = BPL
    emu.cpu.p &= !0x80; // N clear -> BPL taken (poll was reading $00)

    emu.handle_event(EventKind::VBlankStart);

    // The BPL reads no register: the NMI must stay armed, not fire.
    emu.step_without_events();
    assert_eq!(emu.cpu.pc, 0x0200, "BPL branched back to the LDA");
    assert!(
        !emu.cpu.nmi_pending,
        "NMI held off until the poll reads the flag"
    );
    assert!(
        emu.nmi_deliver_armed,
        "still armed after the non-reading instruction"
    );

    // The LDA now observes the flag -> NMI released.
    emu.step_without_events();
    assert_eq!(emu.cpu.c & 0xFF, 0x80, "the poll read the latched NMI flag");
    assert!(
        emu.cpu.nmi_pending,
        "NMI delivered after the poll observed the flag"
    );
    assert!(!emu.nmi_deliver_armed);
}

/// A main thread that drives NMI without polling `$4210` never sets
/// the observed flag; delivery falls back to the bounded window so the
/// NMI still fires promptly.
#[test]
fn nmi_delivery_falls_back_to_window_when_main_thread_does_not_poll() {
    let mut emu = SnesEmulator::new();
    emu.reset();
    emu.bus.write(0x00_4200, 0x80);
    for i in 0..8 {
        emu.bus.write(0x00_0200 + i, 0xEA); // NOP sled, never reads $4210
    }
    emu.cpu.pbr = 0;
    emu.cpu.pc = 0x0200;

    emu.handle_event(EventKind::VBlankStart);
    for n in 1..NMI_DELIVERY_MAX_INSNS {
        emu.step_without_events();
        assert!(
            !emu.cpu.nmi_pending,
            "not delivered before the window elapses (step {n})"
        );
        assert!(emu.nmi_deliver_armed);
    }
    emu.step_without_events();
    assert!(
        emu.cpu.nmi_pending,
        "NMI delivered when the fallback window expires"
    );
    assert!(!emu.nmi_deliver_armed);
}

#[test]
fn the_third_cpu_reports_its_own_clock() {
    assert_eq!(secondary_cpu_info(None).clock_speed, 10_740_000);
    assert_eq!(
        secondary_cpu_info(Some(PresetKind::SuperFx)).clock_speed,
        10_740_000
    );
    assert_eq!(
        secondary_cpu_info(Some(PresetKind::Cx4)).clock_speed,
        20_000_000
    );
}
