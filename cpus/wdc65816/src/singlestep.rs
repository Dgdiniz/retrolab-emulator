//! The 65C816 under the SingleStepTests, which retro-core's runner
//! (`retro_core::testing::singlestep`) runs for the crate's integration
//! tests and for `snes1-cli singlestep-65816`.
//!
//! Test suite source: <https://github.com/SingleStepTests/65816>
//!
//! Each test file (`<opcode>.<e|n>.json`) holds an array of 10 000 cases;
//! a case provides the full CPU/RAM state before and after one instruction
//! plus the per-cycle bus trace. The `.e` variant pins the CPU in emulation
//! mode; `.n` covers native mode with random M/X combinations.
//!
//! What is the 65C816's is here: the case's shape, the state a case sets
//! (emulation mode forcing M and X), the block moves run on the case's
//! cycle budget, what is compared, and the files' names. The rest (the
//! runs, the counts, the reports, the diagnosis and the developer's
//! switches) is the runner's.
//!
//! This module is compiled only when the `singlestep` feature is enabled so
//! the default (and WASM) builds never pull in `serde` or the runner.

use crate::instructions::{FLAG_M, FLAG_X};
use crate::Wdc65816;
use retro_core::testing::singlestep::{CpuUnderTest, CycleCheck, FlatMemory};
use serde::{Deserialize, Serialize};

// ============================================================================
// Test-case DTOs
// ============================================================================

/// A single SingleStepTests case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Test65816 {
    pub name: String,
    pub initial: State65816,
    #[serde(rename = "final")]
    pub final_state: State65816,
    #[serde(default)]
    pub cycles: Vec<CycleEntry>,
}

/// CPU state snapshot. `a` and `s` are always serialised as 16-bit values
/// even in emulation mode (the active half is the low byte). `e` is `0` or
/// `1`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State65816 {
    pub pc: u16,
    pub s: u16,
    pub p: u8,
    pub a: u16,
    pub x: u16,
    pub y: u16,
    pub dbr: u8,
    pub d: u16,
    pub pbr: u8,
    pub e: u8,
    /// `[24-bit address, 8-bit value]` pairs, encoded as numbers.
    pub ram: Vec<RamEntry>,
}

/// Memory snapshot entry: `[address, value]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RamEntry(pub u32, pub u32);

/// Bus cycle entry. The `[null, null, "flags"]` shape (internal cycle, no bus
/// activity) shares this enum with the regular `[addr, value, "flags"]` shape
/// via `#[serde(untagged)]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CycleEntry {
    Bus(CycleAddress, CycleData, String),
}

/// Address slot of a bus cycle, `null` for internal cycles.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CycleAddress {
    Value(u32),
    Null,
}

/// Data slot of a bus cycle, `null` for internal cycles.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CycleData {
    Value(u8),
    Null,
}

/// CPU mode for the opcode test file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestMode {
    /// Emulation mode (`.e.json`). M and X are forced to 1.
    Emulation,
    /// Native mode (`.n.json`). M/X come from the initial P value.
    Native,
}

impl TestMode {
    pub fn suffix(self) -> &'static str {
        match self {
            TestMode::Emulation => "e",
            TestMode::Native => "n",
        }
    }
}

/// The name of the test file of `opcode` in `mode` (`a9.e.json`), in the
/// dataset's `v1/` folder.
pub fn file_name(opcode: u8, mode: TestMode) -> String {
    format!("{:02x}.{}.json", opcode, mode.suffix())
}

/// The name a report gives the test file of `opcode` in `mode` (`A9.e`).
pub fn label(opcode: u8, mode: TestMode) -> String {
    format!("{:02X}.{}", opcode, mode.suffix())
}

// ============================================================================
// The CPU under test
// ============================================================================

const RAM_SIZE: usize = 0x100_0000; // full 24-bit address space

/// The 65C816 and 16 MB of RAM (the whole 24-bit address space) a case runs
/// it on.
pub struct UnderTest {
    cpu: Wdc65816,
    memory: FlatMemory,
}

impl CpuUnderTest for UnderTest {
    type Case = Test65816;
    const CYCLES: CycleCheck = CycleCheck::Always;
    const FILE_KIND: &'static str = "opcode-modes";

    fn case_name(case: &Test65816) -> &str {
        &case.name
    }

    fn prepare(case: &Test65816) -> Self {
        let mut cpu = Wdc65816::new();
        let mut memory = FlatMemory::new(RAM_SIZE);
        apply_initial_state(&mut cpu, &mut memory, case);
        Self { cpu, memory }
    }

    fn execute(&mut self, case: &Test65816) -> u32 {
        execute_step(&mut self.cpu, &mut self.memory, case)
    }

    fn differences(&mut self, case: &Test65816) -> Vec<String> {
        state_differences(&self.cpu, &self.memory, case)
    }

    fn expected_cycles(case: &Test65816) -> u32 {
        case.cycles.len() as u32
    }

    fn describe(&mut self, case: &Test65816) -> String {
        let mut lines = vec![
            String::new(),
            "--- INITIAL ---".to_string(),
            state_line("initial", &case.initial),
            String::new(),
            "Initial RAM:".to_string(),
        ];
        for entry in &case.initial.ram {
            lines.push(format!("  [{:06X}] = {:02X}", entry.0, entry.1 as u8));
        }
        lines.extend([
            String::new(),
            "--- ACTUAL (after step) ---".to_string(),
            cpu_line("actual", &self.cpu),
            String::new(),
            "--- EXPECTED ---".to_string(),
            state_line("expected", &case.final_state),
        ]);
        lines.join("\n") + "\n"
    }
}

// ============================================================================
// State management
// ============================================================================

/// Populate the CPU and memory with the initial test state.
///
/// Emulation-mode normalisations match real silicon:
/// - `P` keeps M and X forced to 1.
/// - X/Y high bytes clear (index registers are 8-bit).
/// - SP high byte clamped to `$01`.
///
/// `A` is loaded into `C` raw (16-bit). In M=1 modes only the low byte is
/// architecturally visible, but the high byte (`B`) is preserved.
fn apply_initial_state(cpu: &mut Wdc65816, memory: &mut FlatMemory, test: &Test65816) {
    let state = &test.initial;

    cpu.c = state.a;
    cpu.x = state.x;
    cpu.y = state.y;
    cpu.pc = state.pc;
    cpu.pbr = state.pbr;
    cpu.dbr = state.dbr;
    cpu.d = state.d;
    cpu.p = state.p;
    cpu.emulation = state.e == 1;

    if cpu.emulation {
        cpu.p |= FLAG_M | FLAG_X;
        cpu.x &= 0x00FF;
        cpu.y &= 0x00FF;
        cpu.sp = 0x0100 | (state.s & 0x00FF);
    } else {
        cpu.sp = state.s;
        if (cpu.p & FLAG_X) != 0 {
            cpu.x &= 0x00FF;
            cpu.y &= 0x00FF;
        }
    }

    cpu.cycles_executed = 0;
    cpu.waiting = false;
    cpu.stopped = false;
    cpu.nmi_pending = false;
    cpu.irq_pending = false;
    cpu.nmi_edge_detected = false;

    memory.load(state.ram.iter().map(|entry| (entry.0, entry.1 as u8)));
}

/// Effective `S` value to compare against the JSON (in emulation, the
/// canonical form is `$01xx`).
fn effective_sp(cpu: &Wdc65816) -> u16 {
    if cpu.emulation {
        0x0100 | (cpu.sp & 0x00FF)
    } else {
        cpu.sp
    }
}

/// Effective index registers for comparison (high byte zero in X=1).
fn effective_x(cpu: &Wdc65816) -> u16 {
    if cpu.emulation || (cpu.p & FLAG_X) != 0 {
        cpu.x & 0x00FF
    } else {
        cpu.x
    }
}

fn effective_y(cpu: &Wdc65816) -> u16 {
    if cpu.emulation || (cpu.p & FLAG_X) != 0 {
        cpu.y & 0x00FF
    } else {
        cpu.y
    }
}

/// Compare the CPU and RAM against the expected final state: the
/// human-readable list of mismatches (the runner adds the cycle count's).
fn state_differences(cpu: &Wdc65816, memory: &FlatMemory, test: &Test65816) -> Vec<String> {
    let expected = &test.final_state;
    let mut errors = Vec::new();

    if cpu.pc != expected.pc {
        errors.push(format!(
            "PC: expected {:04X}, got {:04X}",
            expected.pc, cpu.pc
        ));
    }
    if cpu.c != expected.a {
        errors.push(format!(
            "A (C): expected {:04X}, got {:04X}",
            expected.a, cpu.c
        ));
    }
    if effective_x(cpu) != expected.x {
        errors.push(format!(
            "X: expected {:04X}, got {:04X}",
            expected.x,
            effective_x(cpu)
        ));
    }
    if effective_y(cpu) != expected.y {
        errors.push(format!(
            "Y: expected {:04X}, got {:04X}",
            expected.y,
            effective_y(cpu)
        ));
    }
    if effective_sp(cpu) != expected.s {
        errors.push(format!(
            "S: expected {:04X}, got {:04X}",
            expected.s,
            effective_sp(cpu)
        ));
    }
    if cpu.d != expected.d {
        errors.push(format!("D: expected {:04X}, got {:04X}", expected.d, cpu.d));
    }
    if cpu.pbr != expected.pbr {
        errors.push(format!(
            "PBR: expected {:02X}, got {:02X}",
            expected.pbr, cpu.pbr
        ));
    }
    if cpu.dbr != expected.dbr {
        errors.push(format!(
            "DBR: expected {:02X}, got {:02X}",
            expected.dbr, cpu.dbr
        ));
    }
    if cpu.p != expected.p {
        errors.push(format!(
            "P: expected {:02X}, got {:02X} (diff bits: {:02X})",
            expected.p,
            cpu.p,
            cpu.p ^ expected.p
        ));
    }
    let expected_e = expected.e == 1;
    if cpu.emulation != expected_e {
        errors.push(format!(
            "E: expected {}, got {}",
            expected.e, cpu.emulation as u8
        ));
    }

    for entry in &expected.ram {
        let actual = memory.byte(entry.0);
        if actual != entry.1 as u8 {
            errors.push(format!(
                "RAM[{:06X}]: expected {:02X}, got {:02X}",
                entry.0, entry.1 as u8, actual
            ));
        }
    }

    errors
}

// ============================================================================
// Per-case execution
// ============================================================================

const CYCLES_PER_BLOCK_MOVE_ITER: u32 = 7;

/// Execute a single test case: one `cpu.step()` for normal instructions, or a
/// cycle-budget loop for block-move opcodes (0x44 MVP / 0x54 MVN).
///
/// Block moves execute one byte per `cpu.step()` and re-fetch the opcode until
/// the counter (`C`) wraps to `$FFFF`. Tom Harte's tests give a fixed cycle
/// budget (typically N*7 + 2) and capture the state after that many cycles,
/// including partial iterations where only the opcode and first operand have
/// been fetched (PC advanced by 2).
fn execute_step(cpu: &mut Wdc65816, memory: &mut FlatMemory, test: &Test65816) -> u32 {
    let opcode = memory.byte(((cpu.pbr as u32) << 16) | cpu.pc as u32);
    if opcode != 0x44 && opcode != 0x54 {
        return cpu.step(memory);
    }

    let budget = test.cycles.len() as u32;
    let mut accumulated: u32 = 0;

    while accumulated + CYCLES_PER_BLOCK_MOVE_ITER <= budget {
        let step_cycles = cpu.step(memory);
        accumulated += step_cycles;
        if cpu.c == 0xFFFF {
            break;
        }
    }

    let remainder = budget - accumulated;
    if remainder > 0 && cpu.c != 0xFFFF {
        cpu.pc = cpu.pc.wrapping_add(remainder.min(3) as u16);
    }

    budget
}

fn state_line(label: &str, state: &State65816) -> String {
    format!(
        "  {} A={:04X} X={:04X} Y={:04X} S={:04X} D={:04X} PC={:04X} PBR={:02X} DBR={:02X} P={:02X} E={}",
        label, state.a, state.x, state.y, state.s, state.d, state.pc, state.pbr, state.dbr, state.p, state.e
    )
}

fn cpu_line(label: &str, cpu: &Wdc65816) -> String {
    format!(
        "  {} A={:04X} X={:04X} Y={:04X} S={:04X} D={:04X} PC={:04X} PBR={:02X} DBR={:02X} P={:02X} E={}",
        label,
        cpu.c,
        effective_x(cpu),
        effective_y(cpu),
        effective_sp(cpu),
        cpu.d,
        cpu.pc,
        cpu.pbr,
        cpu.dbr,
        cpu.p,
        cpu.emulation as u8
    )
}
