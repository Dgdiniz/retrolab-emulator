//! The debugger's rules on the SNES, each shown on a small program: what a
//! watchpoint compares and how often it counts, what a condition reads, that
//! the debugger's focus is no part of the machine, that no host steps the
//! SPC700, where a state loaded at a stop is checked, what a reset or a load
//! empties, what the cartridge's CPU traces, and that nothing the debugger
//! reads for itself changes the machine, through any coprocessor's window.
//! The rules of the frame loops and the host step are the frame driver's
//! conformance suite (`frame_conformance_test.rs`).

use retro_core::{
    Breakpoint, BreakpointAction, BreakpointType, EmulatorCore, ExecutionMode, ExecutionTier,
    MatchSpec, BREAKPOINT_PARAM_ANY,
};
use snes1_core::{MapTarget, SnesEmulator, CARTRIDGE_CPU, MAIN_CPU, SOUND_CPU};

/// A cartridge image of `size` bytes. Its header, at `header` (`0x7FB0` for
/// a LoROM layout, `0xFFB0` for a HiROM one), names `map_mode`,
/// `cartridge_type`, the ROM's size code and the RAM's; `code` sits where
/// `$00:8000` reads, the reset vector points there, and the NMI and IRQ
/// vectors, emulation and native, point at an `RTI` at `$8030`.
fn cart(
    size: usize,
    header: usize,
    map_mode: u8,
    cartridge_type: u8,
    rom_size_code: u8,
    ram_size_code: u8,
    code: &[u8],
) -> Vec<u8> {
    let mut rom = vec![0u8; size];
    let base = header - 0x7FB0;
    rom[base..base + code.len()].copy_from_slice(code);
    rom[base + 0x30] = 0x40; // RTI
    rom[header + 0x10..header + 0x25].copy_from_slice(b"DEBUGGER RULES       ");
    rom[header + 0x25] = map_mode;
    rom[header + 0x26] = cartridge_type;
    rom[header + 0x27] = rom_size_code;
    rom[header + 0x28] = ram_size_code;
    rom[header + 0x29] = 0x01; // USA, NTSC
    rom[header + 0x2A] = 0x01;
    rom[header + 0x2C..header + 0x2E].copy_from_slice(&(!0x1234u16).to_le_bytes());
    rom[header + 0x2E..header + 0x30].copy_from_slice(&0x1234u16.to_le_bytes());
    for vector in [0x3A, 0x3E, 0x4A, 0x4E] {
        rom[header + vector..header + vector + 2].copy_from_slice(&0x8030u16.to_le_bytes());
    }
    rom[header + 0x4C..header + 0x4E].copy_from_slice(&0x8000u16.to_le_bytes());
    rom
}

/// A 128 KiB LoROM with `blocks`, each `(address, bytes)` in bank `$00`,
/// and the program starting at `$00:8000`.
fn lorom(blocks: &[(u16, &[u8])]) -> Vec<u8> {
    let mut rom = cart(0x2_0000, 0x7FB0, 0x20, 0x00, 0x07, 0x00, &[]);
    for (address, bytes) in blocks {
        let at = usize::from(address - 0x8000);
        rom[at..at + bytes.len()].copy_from_slice(bytes);
    }
    rom
}

/// The emulator with `rom` loaded and nothing run.
fn loaded(rom: &[u8]) -> SnesEmulator {
    let mut emu = SnesEmulator::new();
    emu.load_rom(rom).expect("the program loads");
    emu
}

/// The 65C816's address as `PBR:PC`.
fn pc24(emu: &SnesEmulator) -> u32 {
    (u32::from(emu.cpu().pbr) << 16) | u32::from(emu.cpu().pc)
}

/// The hit count of breakpoint `id`.
fn hits(emu: &SnesEmulator, id: u32) -> u32 {
    emu.debugger_ref()
        .list_breakpoints()
        .into_iter()
        .find(|info| info.id == id)
        .map(|info| info.hit_count)
        .expect("the breakpoint exists")
}

/// An address breakpoint of `bp_type` on the main CPU at `address`, with
/// `spec`, added to `emu`; its id.
fn add(emu: &mut SnesEmulator, address: u32, bp_type: BreakpointType, spec: MatchSpec) -> u32 {
    let id = emu.debugger().add_breakpoint(Breakpoint {
        address,
        bp_type,
        cpu_id: Some(MAIN_CPU),
        condition: None,
    });
    emu.debugger().set_breakpoint_match_spec(id, spec);
    id
}

/// The main CPU's call stack, outermost first, as `call->return` pairs.
fn call_stack(emu: &SnesEmulator) -> Vec<(u32, u32)> {
    emu.debugger_ref()
        .get_call_stack(MAIN_CPU)
        .iter()
        .map(|frame| (frame.call_address, frame.return_address))
        .collect()
}

/// The main CPU's history, newest first, by address.
fn history(emu: &SnesEmulator) -> Vec<u32> {
    emu.debugger_ref()
        .get_instruction_history(MAIN_CPU, 64)
        .iter()
        .map(|entry| entry.address)
        .collect()
}

/// Native mode, a sixteen-bit accumulator, `$1234` stored to `$0010` (two
/// bytes, one instruction), then an eight-bit accumulator and `$56` stored
/// to `$0012`, then a loop.
const STORES: &[(u16, &[u8])] = &[(
    0x8000,
    &[
        0x18, // $8000 CLC
        0xFB, // $8001 XCE
        0xC2, 0x20, // $8002 REP #$20
        0xA9, 0x34, 0x12, // $8004 LDA #$1234
        0x8D, 0x10, 0x00, // $8007 STA $0010
        0xE2, 0x20, // $800A SEP #$20
        0xA9, 0x56, // $800C LDA #$56
        0x8D, 0x12, 0x00, // $800E STA $0012
        0x80, 0xFE, // $8011 BRA $8011
    ],
)];

/// A write watch of `width` bytes at `$0010` waiting for `value`.
fn value_watch(width: u8, value: u32) -> MatchSpec {
    MatchSpec {
        value: Some(value),
        value_size: width,
        ..MatchSpec::default()
    }
}

#[test]
fn a_sixteen_bit_watch_compares_the_whole_value_after_the_instruction() {
    let mut emu = loaded(&lorom(STORES));
    let id = add(
        &mut emu,
        0x10,
        BreakpointType::write(),
        value_watch(2, 0x1234),
    );
    emu.run_frame();
    assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::Paused);
    assert_eq!(pc24(&emu), 0x00_800A, "it stops after the store");
    assert_eq!(hits(&emu, id), 1, "one hit for the two bytes");
}

#[test]
fn a_twenty_four_bit_watch_compares_the_whole_value() {
    let mut emu = loaded(&lorom(STORES));
    let id = add(
        &mut emu,
        0x10,
        BreakpointType::write(),
        value_watch(3, 0x56_1234),
    );
    emu.run_frame();
    assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::Paused);
    assert_eq!(
        pc24(&emu),
        0x00_8011,
        "the value is whole only after the third byte is stored"
    );
    assert_eq!(hits(&emu, id), 1);
}

#[test]
fn a_watch_counts_one_hit_per_instruction() {
    let mut emu = loaded(&lorom(STORES));
    let id = add(
        &mut emu,
        0x10,
        BreakpointType::write(),
        MatchSpec {
            address_end: Some(0x11),
            action: BreakpointAction::Log,
            ..MatchSpec::default()
        },
    );
    emu.run_frame();
    assert_eq!(
        emu.debugger().get_execution_mode(),
        ExecutionMode::Running,
        "a watch that logs does not stop"
    );
    assert_eq!(hits(&emu, id), 1, "the sixteen-bit store is one hit");
}

#[test]
fn a_watch_condition_reads_the_registers_after_the_instruction() {
    let mut emu = loaded(&lorom(STORES));
    let matching = add(
        &mut emu,
        0x10,
        BreakpointType::write(),
        MatchSpec {
            expr: Some("a == $1234 && [$0010].w == $1234".to_string()),
            ..MatchSpec::default()
        },
    );
    let other = add(
        &mut emu,
        0x10,
        BreakpointType::write(),
        MatchSpec {
            expr: Some("a == $9999".to_string()),
            ..MatchSpec::default()
        },
    );
    emu.run_frame();
    assert_eq!(pc24(&emu), 0x00_800A);
    assert_eq!(hits(&emu, matching), 1);
    assert_eq!(hits(&emu, other), 0, "a condition that fails holds it back");
}

#[test]
fn an_execute_breakpoint_stops_only_where_its_condition_holds() {
    let mut emu = loaded(&lorom(STORES));
    let never = add(
        &mut emu,
        0x00_8007,
        BreakpointType::execute(),
        MatchSpec {
            expr: Some("a != $1234".to_string()),
            ..MatchSpec::default()
        },
    );
    let id = add(
        &mut emu,
        0x00_800E,
        BreakpointType::execute(),
        MatchSpec {
            expr: Some("a == $1256".to_string()),
            ..MatchSpec::default()
        },
    );
    emu.run_frame();
    assert_eq!(emu.debugger().get_execution_mode(), ExecutionMode::Paused);
    assert_eq!(
        pc24(&emu),
        0x00_800E,
        "it stops before the store, the registers as they stand"
    );
    assert_eq!(hits(&emu, id), 1);
    assert_eq!(hits(&emu, never), 0);
}

/// A call and its return, nested: `JSR` to a routine that makes a `JSL`
/// to another, which returns with `RTL`, then `RTS`, then a loop.
const CALLS: &[(u16, &[u8])] = &[
    (0x8000, &[0x20, 0x10, 0x80]),       // JSR $8010
    (0x8003, &[0x80, 0xFE]),             // BRA $8003
    (0x8010, &[0x22, 0x20, 0x80, 0x00]), // JSL $00:8020
    (0x8014, &[0x60]),                   // RTS
    (0x8020, &[0xEA]),                   // NOP
    (0x8021, &[0x6B]),                   // RTL
];

/// A loop of two instructions: `NOP` at `$8000`, then `BRA $8000`.
const LOOP: &[(u16, &[u8])] = &[(0x8000, &[0xEA, 0x80, 0xFD])];

#[test]
fn a_step_focused_on_the_spc700_runs_nothing() {
    // No host steps the SPC700: focused on it, a step runs nothing.
    let mut emu = loaded(&lorom(CALLS));
    emu.set_focused_cpu(SOUND_CPU);
    let before = emu.state_hash();
    assert!(!EmulatorCore::step(&mut emu).instruction_executed);
    assert_eq!(emu.state_hash(), before);
}

#[test]
fn a_state_loaded_at_a_stop_is_checked_at_its_own_address() {
    let mut emu = loaded(&lorom(LOOP));
    let id = add(
        &mut emu,
        0x00_8000,
        BreakpointType::execute(),
        MatchSpec::default(),
    );
    emu.run_frame();
    let state = emu.save_state();
    emu.set_execution_mode(ExecutionMode::Running);
    emu.run_frame();
    assert_eq!(hits(&emu, id), 2);

    emu.load_state(&state).expect("the state loads");
    let cycles = emu.cpu().cycles_executed;
    emu.set_execution_mode(ExecutionMode::Running);
    emu.run_frame();
    assert_eq!(pc24(&emu), 0x00_8000);
    assert_eq!(hits(&emu, id), 3);
    assert_eq!(
        emu.cpu().cycles_executed,
        cycles,
        "the loaded machine stops before running anything"
    );
}

#[test]
fn a_reset_or_a_load_empties_the_call_stack_and_keeps_the_history() {
    let rom = lorom(CALLS);
    let mut emu = loaded(&rom);
    let state = emu.save_state();
    let into_call = |emu: &mut SnesEmulator| {
        emu.step_cpu(MAIN_CPU);
        emu.step_cpu(MAIN_CPU);
        assert_eq!(call_stack(emu).len(), 2);
        history(emu).len()
    };

    let kept = into_call(&mut emu);
    emu.reset();
    assert!(call_stack(&emu).is_empty(), "after a reset");
    assert_eq!(history(&emu).len(), kept);

    let kept = into_call(&mut emu);
    emu.load_rom(&rom).expect("the program loads again");
    assert!(call_stack(&emu).is_empty(), "after a ROM load");
    assert_eq!(history(&emu).len(), kept);

    let kept = into_call(&mut emu);
    emu.load_state(&state).expect("the state loads");
    assert!(call_stack(&emu).is_empty(), "after a state load");
    assert_eq!(history(&emu).len(), kept);

    let kept = into_call(&mut emu);
    emu.power_on_reset();
    assert!(call_stack(&emu).is_empty(), "after a power-on reset");
    assert_eq!(history(&emu).len(), kept);
}

/// A loop of 124 `NOP`s, then `BRA $8000`: about 25,000 instructions a
/// frame.
fn nop_loop() -> Vec<u8> {
    let mut code = vec![0xEA; 124];
    code.extend_from_slice(&[0x80, 0x82]);
    code
}

#[test]
fn the_main_cpu_trace_flushes_alone_and_any_cpu_names_it() {
    // A loop of NOPs with the main CPU traced: each instruction is traced,
    // and ten frames pass the ring's first eight MiB.
    let mut emu = loaded(&lorom(&[(0x8000, &nop_loop())]));
    emu.debugger().set_tracing_enabled(MAIN_CPU, true);
    emu.debugger().set_tracing_enabled(SOUND_CPU, true);
    assert!(!emu.debugger_ref().any_trace_needs_flush());
    for _ in 0..10 {
        emu.run_frame();
        emu.debugger().clear_trace_flushed(SOUND_CPU);
    }
    let debugger = emu.debugger_ref();
    assert!(debugger.needs_trace_flush(MAIN_CPU));
    assert!(!debugger.needs_trace_flush(SOUND_CPU));
    assert!(debugger.any_trace_needs_flush());
}

/// How many addresses the main CPU ran instructions from.
fn covered(emu: &SnesEmulator) -> u32 {
    emu.debugger_ref().coverage_stats(MAIN_CPU).1
}

#[test]
fn a_cartridge_without_a_cpu_that_traces_keeps_its_trace_off() {
    let mut emu = loaded(&lorom(LOOP));
    emu.debugger().set_tracing_enabled(CARTRIDGE_CPU, true);
    assert!(!emu.debugger_ref().is_tracing_enabled(CARTRIDGE_CPU));
    assert_eq!(emu.debugger_ref().trace_entry_size(CARTRIDGE_CPU), None);
    emu.run_frame();
    assert_eq!(
        emu.debugger_ref().execution_tier(),
        ExecutionTier::Fast,
        "the frame ran on the fast loop"
    );
}

#[test]
fn coverage_is_cleared_when_another_program_loads() {
    let rom = lorom(LOOP);
    let mut emu = loaded(&rom);
    for _ in 0..3 {
        emu.step_cpu(MAIN_CPU);
    }
    assert_eq!(covered(&emu), 2);
    emu.load_rom(&rom).expect("the same program loads again");
    assert_eq!(covered(&emu), 2, "the same program keeps it");
    emu.load_rom(&lorom(CALLS)).expect("another program loads");
    assert_eq!(covered(&emu), 0, "another program clears it");
}

/// A coprocessor cartridge, built by `rom`, with a program that loops
/// reading one of the chip's windows; `chip` is the chip's name.
#[derive(Clone, Copy)]
struct ChipCart {
    chip: &'static str,
    rom: fn() -> Vec<u8>,
}

/// `loop: LDA window ; BRA loop`, the window a 24-bit address.
fn reading(window: u32) -> Vec<u8> {
    let [low, high, bank, _] = window.to_le_bytes();
    vec![0xAF, low, high, bank, 0x80, 0xFA]
}

fn sa1_rom() -> Vec<u8> {
    cart(
        0x10_0000,
        0x7FB0,
        0x23,
        0x35,
        0x0A,
        0x03,
        &reading(0x00_3000),
    )
}

fn superfx_rom() -> Vec<u8> {
    cart(
        0x10_0000,
        0x7FB0,
        0x20,
        0x13,
        0x0A,
        0x00,
        &reading(0x70_0000),
    )
}

fn cx4_rom() -> Vec<u8> {
    cart(
        0x20_0000,
        0x7FB0,
        0x20,
        0xF3,
        0x0B,
        0x00,
        &reading(0x00_6000),
    )
}

fn spc7110_rom() -> Vec<u8> {
    cart(
        0x40_0000,
        0xFFB0,
        0x3A,
        0xF5,
        0x0C,
        0x03,
        &reading(0xD0_0000),
    )
}

fn sdd1_rom() -> Vec<u8> {
    cart(
        0x10_0000,
        0x7FB0,
        0x32,
        0x45,
        0x0A,
        0x03,
        &reading(0xC0_0000),
    )
}

fn dsp1_rom() -> Vec<u8> {
    cart(
        0x10_0000,
        0x7FB0,
        0x20,
        0x03,
        0x0A,
        0x00,
        &reading(0x30_C000),
    )
}

/// The coprocessor cartridges this build runs.
fn chip_carts() -> Vec<ChipCart> {
    let mut carts = vec![
        ChipCart {
            chip: "SPC7110",
            rom: spc7110_rom,
        },
        ChipCart {
            chip: "S-DD1",
            rom: sdd1_rom,
        },
        ChipCart {
            chip: "DSP-1",
            rom: dsp1_rom,
        },
    ];
    if cfg!(feature = "chip-sa1") {
        carts.push(ChipCart {
            chip: "SA-1",
            rom: sa1_rom,
        });
    }
    if cfg!(feature = "chip-superfx") {
        carts.push(ChipCart {
            chip: "Super FX",
            rom: superfx_rom,
        });
    }
    if cfg!(feature = "chip-cx4") {
        carts.push(ChipCart {
            chip: "Cx4",
            rom: cx4_rom,
        });
    }
    carts
}

/// The cartridge loaded, its chip installed.
fn chip_cart(cart: ChipCart) -> SnesEmulator {
    let emu = loaded(&(cart.rom)());
    let label = emu.coprocessor_label().unwrap_or_default();
    assert!(
        label.contains(cart.chip),
        "{}: the chip is installed ({label})",
        cart.chip
    );
    emu
}

/// The cartridge loaded and two frames run.
fn chip_loaded(cart: ChipCart) -> SnesEmulator {
    let mut emu = chip_cart(cart);
    emu.run_frame();
    emu.run_frame();
    emu
}

/// The first address of every slot of the cartridge's map whose target is
/// the chip, and of the two register windows a chip can claim.
fn chip_windows(emu: &SnesEmulator) -> Vec<u32> {
    let plan = emu
        .bus()
        .cartridge_plan()
        .expect("a cartridge is installed");
    let mut windows: Vec<u32> = plan
        .slots
        .iter()
        .filter(|slot| matches!(slot.target, MapTarget::ChipMmio { .. }))
        .map(|slot| {
            (u32::from(*slot.bank_range.start()) << 16) | u32::from(*slot.offset_range.start())
        })
        .collect();
    windows.extend([0x00_2200, 0x00_4800]);
    windows
}

#[test]
fn a_debugger_read_of_any_chip_window_leaves_the_machine_as_it_was() {
    for cart in chip_carts() {
        let mut emu = chip_loaded(cart);
        for window in chip_windows(&emu) {
            let before = emu.state_hash();
            let _ = emu.read_memory_debug(window, 0x100);
            let _ = emu.disassemble_cpu(MAIN_CPU, window, 16);
            let _ = emu.disassemble_window(MAIN_CPU, window, 4, 4);
            assert_eq!(
                emu.state_hash(),
                before,
                "{}: a read of the window at {window:06X}",
                cart.chip
            );
        }
    }
}

/// `emu` set up as `config` names: `"light"` one disabled execute
/// breakpoint, so frames run on the instrumented loop; `"armed"` an
/// execute breakpoint, a watch over every address and every event, all
/// logging; `"traced"` every CPU traced.
fn instrument(emu: &mut SnesEmulator, config: &str) {
    match config {
        "light" => {
            let id = add(
                emu,
                0x7F_FFFF,
                BreakpointType::execute(),
                MatchSpec::default(),
            );
            emu.debugger().enable_breakpoint(id, false);
        }
        "armed" => {
            let everywhere = MatchSpec {
                address_end: Some(0x00FF_FFFF),
                action: BreakpointAction::Log,
                ..MatchSpec::default()
            };
            add(emu, 0, BreakpointType::execute(), everywhere.clone());
            add(emu, 0, BreakpointType::read_write(), everywhere);
            for code in 1..=12 {
                let id = emu.debugger().add_breakpoint(Breakpoint {
                    address: 0,
                    bp_type: BreakpointType::Event {
                        code,
                        param: BREAKPOINT_PARAM_ANY,
                    },
                    cpu_id: None,
                    condition: None,
                });
                emu.debugger().set_breakpoint_match_spec(
                    id,
                    MatchSpec {
                        action: BreakpointAction::Log,
                        ..MatchSpec::default()
                    },
                );
            }
        }
        "traced" => {
            for cpu in [MAIN_CPU, SOUND_CPU, CARTRIDGE_CPU] {
                emu.debugger().set_tracing_enabled(cpu, true);
            }
        }
        other => panic!("no configuration {other}"),
    }
}

#[test]
fn every_chip_cartridge_runs_on_the_instrumented_loop_as_on_the_fast_loop() {
    for cart in chip_carts() {
        for config in ["light", "armed", "traced"] {
            let mut fast = chip_cart(cart);
            let mut instrumented = chip_cart(cart);
            instrument(&mut instrumented, config);
            for frame in 1..=4 {
                fast.run_frame();
                instrumented.run_frame();
                assert_eq!(
                    instrumented.debugger().get_execution_mode(),
                    ExecutionMode::Running
                );
                assert_eq!(
                    instrumented.state_hash(),
                    fast.state_hash(),
                    "{} {config}: frame {frame}",
                    cart.chip
                );
                for cpu in [MAIN_CPU, SOUND_CPU, CARTRIDGE_CPU] {
                    instrumented.debugger().clear_trace_flushed(cpu);
                }
            }
            assert_eq!(
                instrumented.debugger_ref().execution_tier(),
                ExecutionTier::LightDebug,
                "{} {config}: the frames ran on the instrumented loop",
                cart.chip
            );
        }
    }
}

#[cfg(feature = "chip-sa1")]
#[test]
fn the_focus_is_no_part_of_the_machine() {
    let mut emu = chip_loaded(ChipCart {
        chip: "SA-1",
        rom: sa1_rom,
    });
    let hash = emu.state_hash();
    for cpu in [SOUND_CPU, CARTRIDGE_CPU, MAIN_CPU] {
        emu.set_focused_cpu(cpu);
        assert_eq!(emu.focused_cpu(), cpu);
        assert_eq!(emu.state_hash(), hash, "focused on CPU {cpu}");
    }
    emu.set_focused_cpu(SOUND_CPU);
    let state = emu.save_state();
    emu.set_focused_cpu(CARTRIDGE_CPU);
    emu.load_state(&state).expect("the state loads");
    assert_eq!(
        emu.focused_cpu(),
        CARTRIDGE_CPU,
        "a loaded state leaves the focus alone"
    );
}

#[cfg(all(feature = "chip-superfx", feature = "chip-cx4"))]
#[test]
fn the_cartridge_cpu_slot_follows_the_cartridge_loaded() {
    use retro_core::debug::TraceEntry;
    use snes1_core::{TraceEntryBinaryCx4, TraceEntryBinaryGsu};
    let gsu = <TraceEntryBinaryGsu as TraceEntry>::SIZE;
    let cx4 = <TraceEntryBinaryCx4 as TraceEntry>::SIZE;
    let mut emu = loaded(&superfx_rom());
    assert_eq!(
        emu.debugger_ref().trace_entry_size(CARTRIDGE_CPU),
        Some(gsu)
    );
    emu.debugger().set_tracing_enabled(CARTRIDGE_CPU, true);
    assert!(emu.debugger_ref().is_tracing_enabled(CARTRIDGE_CPU));

    emu.load_rom(&superfx_rom())
        .expect("the same cartridge loads again");
    assert!(
        emu.debugger_ref().is_tracing_enabled(CARTRIDGE_CPU),
        "the same CPU keeps its slot"
    );

    emu.load_rom(&cx4_rom()).expect("a Cx4 cartridge loads");
    assert_eq!(
        emu.debugger_ref().trace_entry_size(CARTRIDGE_CPU),
        Some(cx4)
    );
    assert!(
        !emu.debugger_ref().is_tracing_enabled(CARTRIDGE_CPU),
        "another CPU gets its slot anew"
    );

    emu.load_rom(&lorom(LOOP)).expect("a plain cartridge loads");
    assert_eq!(emu.debugger_ref().trace_entry_size(CARTRIDGE_CPU), None);
}
