//! Event breakpoints on the 68000's interrupts: an event on the vertical or
//! the horizontal interrupt stops where the interrupt is taken, on the
//! debugger's tier, and never for a request the 68000's mask holds back;
//! with no breakpoint the frame runs on the play tier. An execute
//! breakpoint at a handler the 68000 enters in a step stops in front of
//! the handler's first instruction, and the frame then ends as one run
//! whole.

mod helpers;

use helpers::M68kProgram;
use retro_core::debug::DebugEvent;
use retro_core::frame::run_frame_on;
use retro_core::types::{Breakpoint, BreakpointType};
use retro_core::{DebuggerCore, EmulatorCore, ExecutionMode, ExecutionTier};
use sg1_core::debugger::GenesisEvent;
use sg1_core::GenesisEmulator;

/// Where the horizontal and vertical interrupt handlers start.
const HORIZONTAL_HANDLER: u32 = 0x400;
const VERTICAL_HANDLER: u32 = 0x500;

/// A program with the horizontal interrupt every 50 lines and the vertical
/// one on, each handler counting itself, the 68000 idling with its
/// interrupt mask at `mask`.
fn program(mask: u16) -> Vec<u8> {
    let mut program = M68kProgram::new(0x1000, 0x200);
    program
        .vector(28, HORIZONTAL_HANDLER)
        .vector(30, VERTICAL_HANDLER);
    #[rustfmt::skip]
    program.org(0x200).words(&[
        0x46FC, 0x2700,                 // MOVE.W #$2700,SR
        0x41F9, 0x00C0, 0x0004,         // LEA $C00004,A0
        0x30BC, 0x8014,                 // register 0: horizontal interrupt on
        0x30BC, 0x8164,                 // register 1: display, vertical interrupt, mode 5
        0x30BC, 0x8A31,                 // register 10: every 50 lines
        0x46FC, 0x2000 | (mask << 8),   // MOVE.W #mask,SR
    ]).label("idle").branch(0x6000, "idle");
    #[rustfmt::skip]
    program.org(HORIZONTAL_HANDLER as usize).words(&[
        0x5279, 0x00FF, 0x0002,         // ADDQ.W #1,($FF0002).L
        0x4E73,                         // RTE
    ]);
    #[rustfmt::skip]
    program.org(VERTICAL_HANDLER as usize).words(&[
        0x5279, 0x00FF, 0x0000,         // ADDQ.W #1,($FF0000).L
        0x4E73,                         // RTE
    ]);
    program.build()
}

fn running(rom: &[u8]) -> GenesisEmulator {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(rom).expect("the program loads");
    emu.set_execution_mode(ExecutionMode::Running);
    for _ in 0..3 {
        run_frame_on(&mut emu, ExecutionTier::Play);
    }
    emu
}

fn break_on(emu: &mut GenesisEmulator, event: GenesisEvent) -> u32 {
    let (code, param) = event.code_param();
    emu.debugger().add_breakpoint(Breakpoint {
        address: 0,
        bp_type: BreakpointType::Event { code, param },
        cpu_id: None,
        condition: None,
    })
}

fn paused(emu: &mut GenesisEmulator) -> bool {
    emu.debugger().get_execution_mode() == ExecutionMode::Paused
}

fn count(emu: &GenesisEmulator, address: usize) -> u16 {
    let ram = emu.bus().memory().work_ram.as_slice();
    u16::from_be_bytes([ram[address], ram[address + 1]])
}

#[test]
fn a_vertical_interrupt_event_stops_in_its_handler() {
    let mut emu = running(&program(0));
    let taken = count(&emu, 0);
    break_on(&mut emu, GenesisEvent::VInt);
    EmulatorCore::run_frame(&mut emu);
    assert!(paused(&mut emu), "the event stopped the frame");
    // Taken part of the way into the first line of vertical blanking:
    // the stop comes after the handler's first instruction.
    assert_eq!(emu.vdp().timing().scanline, emu.vdp().first_vblank_line(),);
    assert_eq!(emu.cpu().pc, VERTICAL_HANDLER + 6);
    assert_eq!(emu.cpu().sr & 0x0700, 0x0600, "in the handler");
    assert_eq!(count(&emu, 0), taken + 1);
}

#[test]
fn a_horizontal_interrupt_event_stops_where_the_interrupt_is_taken() {
    let mut emu = running(&program(0));
    break_on(&mut emu, GenesisEvent::HInt);
    EmulatorCore::run_frame(&mut emu);
    assert!(paused(&mut emu));
    // Taken at the start of its line: the stop holds the frame with the
    // 68000 at the handler's first instruction.
    assert_eq!(emu.cpu().pc, HORIZONTAL_HANDLER);
    assert_eq!(emu.cpu().sr & 0x0700, 0x0400);
    assert!(emu.vdp().timing().scanline < emu.vdp().first_vblank_line());
}

#[test]
fn no_event_stops_for_an_interrupt_the_mask_holds_back() {
    // The mask at 4: the horizontal interrupt is requested every 50
    // lines and never taken; the vertical one is taken.
    let mut emu = running(&program(4));
    assert_eq!(count(&emu, 2), 0, "the horizontal handler never ran");
    break_on(&mut emu, GenesisEvent::HInt);
    for _ in 0..3 {
        EmulatorCore::run_frame(&mut emu);
        assert!(!paused(&mut emu), "nothing was taken");
    }
    assert_eq!(count(&emu, 2), 0);
    assert!(count(&emu, 0) > 0, "the vertical interrupt was");
}

#[test]
fn with_no_breakpoint_the_frame_runs_on_the_play_tier() {
    let mut emu = running(&program(0));
    assert_eq!(emu.debugger().execution_tier(), ExecutionTier::Play);
    let id = break_on(&mut emu, GenesisEvent::VInt);
    assert_eq!(emu.debugger().execution_tier(), ExecutionTier::LightDebug);
    emu.debugger().remove_breakpoint(id);
    assert_eq!(emu.debugger().execution_tier(), ExecutionTier::Play);
}

#[test]
fn an_execute_breakpoint_at_the_vertical_handler_stops_before_it_and_the_frame_ends_whole() {
    let mut emu = running(&program(0));
    let mut whole = running(&program(0));
    let taken = count(&emu, 0);
    emu.debugger().add_breakpoint(Breakpoint {
        address: VERTICAL_HANDLER,
        bp_type: BreakpointType::execute(),
        cpu_id: None,
        condition: None,
    });
    EmulatorCore::run_frame(&mut emu);
    assert!(paused(&mut emu), "the breakpoint stopped the frame");
    // The 68000 takes this interrupt in a step, part of the way into the
    // first line of vertical blanking: the stop comes after the step took
    // it, in front of the handler's first instruction.
    assert_eq!(emu.cpu().pc, VERTICAL_HANDLER);
    assert_eq!(emu.cpu().sr & 0x0700, 0x0600, "the interrupt was taken");
    assert_eq!(count(&emu, 0), taken, "the handler has not run yet");

    // Going on runs the handler's first instruction in that same step,
    // which counts the interrupt's cycles once: the frame ends as the frame
    // run whole.
    emu.set_execution_mode(ExecutionMode::Running);
    EmulatorCore::run_frame(&mut emu);
    run_frame_on(&mut whole, ExecutionTier::Play);
    assert!(!paused(&mut emu), "the rest of the frame ran");
    assert_eq!(count(&emu, 0), taken + 1);
    assert_eq!(emu.frame_count(), whole.frame_count());
    assert_eq!(emu.state_hash(), whole.state_hash(), "the same machine");
}
