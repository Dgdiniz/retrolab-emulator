//! The SPC700 under the SingleStepTests, which retro-core's runner
//! (`retro_core::testing::singlestep`) runs for the crate's integration
//! tests and for `snes1-cli singlestep-spc700`.
//!
//! Test suite source: <https://github.com/SingleStepTests/spc700>
//!
//! Each test file (`<opcode>.json`) holds an array of 1 000 cases; a case
//! provides the full CPU/RAM state before and after one instruction plus the
//! per-cycle bus trace. Unlike the 65816 suite there's no `.e`/`.n` split,
//! the SPC700 has no emulation mode and a single 64 KiB address space.
//!
//! What is the SPC700's is here: the case's shape, the state a case sets,
//! what is compared, and the files' names. The rest (the runs, the counts,
//! the reports, the diagnosis and the developer's switches) is the
//! runner's.
//!
//! This module is compiled only when the `singlestep` feature is enabled so
//! the default (and WASM) builds never pull in `serde` or the runner.

use crate::Spc700;
use retro_core::testing::singlestep::{CpuUnderTest, CycleCheck, FlatMemory};
use serde::{Deserialize, Serialize};

// ============================================================================
// Test-case DTOs
// ============================================================================

/// A single SingleStepTests case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestSpc700 {
    pub name: String,
    pub initial: StateSpc700,
    #[serde(rename = "final")]
    pub final_state: StateSpc700,
    #[serde(default)]
    pub cycles: Vec<CycleEntry>,
}

/// CPU state snapshot. The SPC700 has no emulation mode, no second page
/// register, and no data-bank register, so the surface is just
/// `pc/a/x/y/sp/psw + ram`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSpc700 {
    pub pc: u16,
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub psw: u8,
    /// `[16-bit address, 8-bit value]` pairs, encoded as numbers.
    pub ram: Vec<RamEntry>,
}

/// Memory snapshot entry: `[address, value]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RamEntry(pub u32, pub u32);

/// Bus cycle entry: `[addr, value, "read"|"write"]`. Internal cycles still
/// show the last bus address with `"wait"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CycleEntry {
    Bus(CycleAddress, CycleData, String),
}

/// Address slot of a bus cycle, may be `null` for internal cycles in some
/// opcode variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CycleAddress {
    Value(u32),
    Null,
}

/// Data slot of a bus cycle, may be `null` for internal cycles.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CycleData {
    Value(u8),
    Null,
}

/// The name of the test file of `opcode` (`e8.json`), in the dataset's
/// `v1/` folder.
pub fn file_name(opcode: u8) -> String {
    format!("{:02x}.json", opcode)
}

/// The name a report gives the test file of `opcode` (`E8`).
pub fn label(opcode: u8) -> String {
    format!("{:02X}", opcode)
}

// ============================================================================
// The CPU under test
// ============================================================================

const RAM_SIZE: usize = 0x10000;

/// The SPC700 and the 64 KiB of RAM a case runs it on.
pub struct UnderTest {
    cpu: Spc700,
    memory: FlatMemory,
}

impl CpuUnderTest for UnderTest {
    type Case = TestSpc700;
    const CYCLES: CycleCheck = CycleCheck::Always;
    const FILE_KIND: &'static str = "opcodes";

    fn case_name(case: &TestSpc700) -> &str {
        &case.name
    }

    fn prepare(case: &TestSpc700) -> Self {
        let mut cpu = Spc700::new();
        let mut memory = FlatMemory::new(RAM_SIZE);
        apply_initial_state(&mut cpu, &mut memory, case);
        Self { cpu, memory }
    }

    fn execute(&mut self, _case: &TestSpc700) -> u32 {
        self.cpu.step(&mut self.memory)
    }

    fn differences(&mut self, case: &TestSpc700) -> Vec<String> {
        state_differences(&self.cpu, &self.memory, case)
    }

    fn expected_cycles(case: &TestSpc700) -> u32 {
        case.cycles.len() as u32
    }

    fn describe(&mut self, case: &TestSpc700) -> String {
        let mut lines = vec![
            String::new(),
            "--- INITIAL ---".to_string(),
            state_line("initial", &case.initial),
            String::new(),
            "Initial RAM:".to_string(),
        ];
        for entry in &case.initial.ram {
            lines.push(format!("  [{:04X}] = {:02X}", entry.0, entry.1 as u8));
        }
        let cpu = &self.cpu;
        lines.extend([
            String::new(),
            "--- ACTUAL (after step) ---".to_string(),
            format!(
                "  actual A={:02X} X={:02X} Y={:02X} SP={:02X} PSW={:02X} PC={:04X}",
                cpu.a, cpu.x, cpu.y, cpu.sp, cpu.psw, cpu.pc
            ),
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

fn apply_initial_state(cpu: &mut Spc700, memory: &mut FlatMemory, test: &TestSpc700) {
    let s = &test.initial;
    cpu.pc = s.pc;
    cpu.a = s.a;
    cpu.x = s.x;
    cpu.y = s.y;
    cpu.sp = s.sp;
    cpu.psw = s.psw;
    cpu.cycles_executed = 0;

    memory.load(s.ram.iter().map(|entry| (entry.0, entry.1 as u8)));
}

/// Compare the CPU and RAM against the expected final state: the
/// human-readable list of mismatches (the runner adds the cycle count's).
fn state_differences(cpu: &Spc700, memory: &FlatMemory, test: &TestSpc700) -> Vec<String> {
    let expected = &test.final_state;
    let mut errors = Vec::new();

    if cpu.pc != expected.pc {
        errors.push(format!(
            "PC: expected {:04X}, got {:04X}",
            expected.pc, cpu.pc
        ));
    }
    if cpu.a != expected.a {
        errors.push(format!("A: expected {:02X}, got {:02X}", expected.a, cpu.a));
    }
    if cpu.x != expected.x {
        errors.push(format!("X: expected {:02X}, got {:02X}", expected.x, cpu.x));
    }
    if cpu.y != expected.y {
        errors.push(format!("Y: expected {:02X}, got {:02X}", expected.y, cpu.y));
    }
    if cpu.sp != expected.sp {
        errors.push(format!(
            "SP: expected {:02X}, got {:02X}",
            expected.sp, cpu.sp
        ));
    }
    if cpu.psw != expected.psw {
        errors.push(format!(
            "PSW: expected {:02X}, got {:02X} (diff bits: {:02X})",
            expected.psw,
            cpu.psw,
            cpu.psw ^ expected.psw
        ));
    }

    for entry in &expected.ram {
        let actual = memory.byte(entry.0);
        if actual != entry.1 as u8 {
            errors.push(format!(
                "RAM[{:04X}]: expected {:02X}, got {:02X}",
                entry.0, entry.1 as u8, actual
            ));
        }
    }

    errors
}

fn state_line(label: &str, s: &StateSpc700) -> String {
    format!(
        "  {} A={:02X} X={:02X} Y={:02X} SP={:02X} PSW={:02X} PC={:04X}",
        label, s.a, s.x, s.y, s.sp, s.psw, s.pc
    )
}
