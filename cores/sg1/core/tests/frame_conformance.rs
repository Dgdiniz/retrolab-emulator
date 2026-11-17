//! The frame driver's conformance suite (`retro_core::frame::conformance`)
//! run on the Mega Drive core, with a program of its own.
//!
//! At boot the program masks interrupts, sets the video chip up (40 cells,
//! the display and transfers on, the horizontal interrupt's counter at
//! 200 lines), gives the Z80 a counting loop and lets it run, waits a few
//! lines and turns the horizontal interrupt on, so it comes once a frame,
//! at line 200. Then, every frame: a call to a routine of two instructions,
//! sixteen turns of a `SUBQ` / `BNE` loop, a tone on the PSG, and `STOP
//! #$2300` to wait for the horizontal interrupt, which the 68000 takes at
//! line 200's start, where the core handles what falls due. Woken, it waits
//! for vertical blanking and starts a transfer of 4,096 words from ROM to
//! video memory, whose stall reaches past the frame's last line into the
//! next frame's first lines: lines whose time the stall uses up as they
//! start come at the frame's end. The vertical interrupt stays off: the
//! video chip raises none in the first frame after a reset.

mod helpers;

use helpers::M68kProgram;
use retro_core::debug::DebugEvent;
use retro_core::frame::conformance::FrameCoreUnderTest;
use retro_core::types::{Breakpoint, BreakpointType};
use retro_core::{EmulatorCore, ExecutionMode};
use sg1_core::debugger::GenesisEvent;
use sg1_core::GenesisEmulator;

/// Where the horizontal interrupt's handler starts.
const HORIZONTAL_HANDLER: u32 = 0x400;
/// The routine the program calls every frame.
const ROUTINE: u32 = 0x600;

/// The program of the module's notes, and its labels.
fn assembled() -> M68kProgram {
    let mut program = M68kProgram::new(0x4000, 0x200);
    program.vector(28, HORIZONTAL_HANDLER);
    #[rustfmt::skip]
    program.org(0x200).words(&[
        0x46FC, 0x2700,                 // MOVE.W #$2700,SR
        0x41F9, 0x00C0, 0x0004,         // LEA $C00004,A0: control port
        0x30BC, 0x8004,                 // register 0: the horizontal interrupt off
        0x30BC, 0x8154,                 // register 1: display, transfers, mode 5
        0x30BC, 0x8AC8,                 // register 10: the horizontal interrupt every 200 lines
        0x30BC, 0x8C81,                 // register 12: 40 cells
        0x30BC, 0x8F02,                 // register 15: auto-increment 2
        // The Z80: its bus taken, a counting loop written into its RAM,
        // let out of reset, its bus given back.
        0x33FC, 0x0100, 0x00A1, 0x1100, // MOVE.W #$0100,($A11100).L
        0x13FC, 0x0021, 0x00A0, 0x0000, // MOVE.B #$21,($A00000).L: LD HL,$1000
        0x13FC, 0x0000, 0x00A0, 0x0001, // MOVE.B #$00,($A00001).L
        0x13FC, 0x0010, 0x00A0, 0x0002, // MOVE.B #$10,($A00002).L
        0x13FC, 0x0034, 0x00A0, 0x0003, // MOVE.B #$34,($A00003).L: INC (HL)
        0x13FC, 0x0018, 0x00A0, 0x0004, // MOVE.B #$18,($A00004).L: JR -3
        0x13FC, 0x00FD, 0x00A0, 0x0005, // MOVE.B #$FD,($A00005).L
        0x33FC, 0x0100, 0x00A1, 0x1200, // MOVE.W #$0100,($A11200).L: out of reset
        0x33FC, 0x0000, 0x00A1, 0x1100, // MOVE.W #$0000,($A11100).L: its bus back
        // A few lines, so the counter has started from 200 before the
        // interrupt is on.
        0x303C, 0x00C8,                 // MOVE.W #200,D0
    ]).label("delay").dbra(0, "delay").words(&[
        0x30BC, 0x8014,                 // register 0: the horizontal interrupt on
    ]).label("frame").label("call").words(&[
        0x4EB9, 0x0000, ROUTINE as u16, // JSR routine
    ]).label("back").words(&[
        0x7210,                         // MOVEQ #16,D1
    ]).label("turn").words(&[
        0x5341,                         // SUBQ.W #1,D1
    ]).branch(0x6600, "turn").words(&[  // BNE.W turn
        0x13FC, 0x0080, 0x00C0, 0x0011, // MOVE.B #$80,($C00011).L: PSG tone 0
        0x13FC, 0x0010, 0x00C0, 0x0011, // MOVE.B #$10,($C00011).L
        0x13FC, 0x0090, 0x00C0, 0x0011, // MOVE.B #$90,($C00011).L: its volume
        0x4E72, 0x2300,                 // STOP #$2300: wait for the horizontal interrupt
    ]).label("waiting").words(&[
        0x4E71,                         // NOP: where the 68000 waits
    ]).label("blanking").words(&[
        0x3010,                         // MOVE.W (A0),D0: the status
        0x0800, 0x0003,                 // BTST #3,D0: vertical blanking
    ]).branch(0x6700, "blanking").words(&[ // BEQ.W blanking
        0x30BC, 0x9300,                 // registers 19 and 20: 4,096 words
        0x30BC, 0x9410,
        0x30BC, 0x9500,                 // registers 21 to 23: from ROM at 0
        0x30BC, 0x9600,
        0x30BC, 0x9700,
        0x20BC, 0x4000, 0x0080,         // MOVE.L #$40000080,(A0): to VRAM at 0
    ]).branch(0x6000, "frame"); // BRA.W frame
    #[rustfmt::skip]
    program.org(ROUTINE as usize).words(&[
        0x4E71,                         // NOP
        0x4E75,                         // RTS
    ]);
    #[rustfmt::skip]
    program.org(HORIZONTAL_HANDLER as usize).words(&[
        0x5279, 0x00FF, 0x0002,         // ADDQ.W #1,($FF0002).L
        0x4E73,                         // RTE
    ]);
    program
}

fn build() -> GenesisEmulator {
    let mut emu = GenesisEmulator::new();
    emu.load_rom(&assembled().build())
        .expect("the program loads");
    // A reset pauses this core's debugger; the suite starts it running.
    emu.set_execution_mode(ExecutionMode::Running);
    emu
}

/// The Mega Drive core, as the suite takes it.
fn genesis() -> FrameCoreUnderTest<GenesisEmulator> {
    let mut program = assembled();
    let rom = program.build();
    let (code, param) = GenesisEvent::Scanline(100).code_param();
    FrameCoreUnderTest {
        build,
        rom,
        state_hash: GenesisEmulator::state_hash,
        state_parts: GenesisEmulator::state_hash_parts,
        frame_count: GenesisEmulator::frame_count,
        repeated_address: program.address("turn"),
        handler_address: HORIZONTAL_HANDLER,
        call: (program.address("call"), program.address("back")),
        stop_in_picture: Breakpoint {
            address: 0,
            bp_type: BreakpointType::Event { code, param },
            cpu_id: None,
            condition: None,
        },
        waiting_at: program.address("waiting"),
    }
}

retro_core::frame_conformance_tests!(genesis());

/// The program keeps the frame's end busy: in some of the suite's frames
/// the last line's time is used up by the transfer's stall as it starts,
/// which is the case a host step's handling after its instruction must
/// close the frame for. A program that never met it would let the suite's
/// host-step check pass without that rule.
#[test]
fn the_transfer_s_stall_uses_up_a_frame_s_last_line_as_it_starts() {
    let mut emu = build();
    let last_line = 261u16;
    let mut met = 0;
    for _ in 0..6 {
        let target = emu.frame_count() + 1;
        while emu.frame_count() < target {
            let line = emu.vdp().timing().scanline;
            emu.step_cpu(0);
            // A step on the line before the last that closed the frame
            // started the last line and found its time spent.
            if line == last_line - 1 && emu.frame_count() == target {
                met += 1;
            }
        }
    }
    assert!(
        met > 0,
        "a frame's last line was used up by the stall as it started"
    );
}
