//! Tom Harte's ProcessorTests integration
//!
//! Shared by several independent test binaries, each of which compiles this
//! module separately and uses a different subset of it, so per-binary
//! dead-code lints fire on items a sibling binary depends on.
#![allow(dead_code)]
//!
//! Test suite from: https://github.com/SingleStepTests/ProcessorTests
//!
//! Each test provides full CPU and memory state before and after executing a single instruction.
//!
//! What is the 68000's is here: the case's shape, the test bus (16 MB of RAM,
//! big-endian words, four cycles an access), the state a case sets (A7 from
//! the SSP, the prefetch words), what is compared, how the state is shown,
//! and the vectors of the set known to be wrong ([`UnderTest`]); retro-core's
//! runner (`retro_core::testing::singlestep`) runs the cases.

use retro_core::cpu::{Addressable, CyclesTrackable};
use retro_core::testing::singlestep::{vector_files, CpuUnderTest, CycleCheck, FlatMemory};
use serde::{Deserialize, Serialize};
use sg1_core::cpu68k::M68k;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A single test case from Tom Harte's test suite
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarteTest {
    pub name: String,
    pub initial: CpuState,
    #[serde(rename = "final")]
    pub final_state: CpuState,
    #[serde(rename = "length")]
    pub cycles: u32,
    #[serde(default)]
    pub transactions: Vec<Transaction>,
}

/// CPU state (registers and memory)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuState {
    pub d0: u32,
    pub d1: u32,
    pub d2: u32,
    pub d3: u32,
    pub d4: u32,
    pub d5: u32,
    pub d6: u32,
    pub d7: u32,
    pub a0: u32,
    pub a1: u32,
    pub a2: u32,
    pub a3: u32,
    pub a4: u32,
    pub a5: u32,
    pub a6: u32,
    pub usp: u32,
    pub ssp: u32,
    pub sr: u16,
    pub pc: u32,
    pub prefetch: [u16; 2],
    pub ram: Vec<(u32, u8)>,
}

/// Bus transaction (read/write/no-op)
/// We don't parse the details yet - just store as raw JSON value
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Transaction {
    /// Raw transaction data - can be various formats
    Raw(serde_json::Value),
}

impl CpuState {
    /// Get all data registers as array
    pub fn data_regs(&self) -> [u32; 8] {
        [
            self.d0, self.d1, self.d2, self.d3, self.d4, self.d5, self.d6, self.d7,
        ]
    }

    /// Get all address registers as array
    pub fn addr_regs(&self) -> [u32; 8] {
        [
            self.a0, self.a1, self.a2, self.a3, self.a4, self.a5, self.a6, self.ssp,
        ]
    }

    /// Create memory map from RAM entries
    pub fn memory_map(&self) -> HashMap<u32, u8> {
        self.ram.iter().copied().collect()
    }
}

/// The folder of the set's files (`tests/tom_harte_repo/680x0/68000/v1`).
pub fn vector_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tests/tom_harte_repo/680x0/68000/v1")
}

/// The file of an instruction's cases: the first, in the order of their
/// names, whose name starts with `instruction` (`ADD.b` finds
/// `ADD.b.json.gz`).
pub fn vector_file(instruction: &str) -> Option<PathBuf> {
    vector_files(&vector_dir())
        .into_iter()
        .find(|(name, _)| name.starts_with(instruction))
        .map(|(_, path)| path)
}

/// Vectors of the set that contradict the processor, by exact name, each
/// with what is wrong in it. They are skipped and counted, so a group that
/// holds one still reads whole and a new failure in it is not hidden behind
/// an old one.
const DEFECTIVE_VECTORS: &[(&str, &str)] = &[
    (
        "e502 [ASL.b Q, D2] 1583",
        "asks a byte shift to change the upper 24 bits of the register",
    ),
    (
        "e502 [ASL.b Q, D2] 1761",
        "asks a byte shift to change the upper 24 bits of the register",
    ),
    (
        "80ef [DIVU (d16, A7), D0] 5745",
        "stacks the division's own address as the zero-divide trap's return \
         address, where the trap stacks the next instruction's, as this set's \
         CHK vectors do",
    ),
];

/// Simple test bus that allows read/write to any address.
/// Tom Harte tests expect full 24-bit address space access.
struct TestBus {
    /// 16MB address space (full 24-bit)
    memory: FlatMemory,
    cycles_executed: u64,
}

impl TestBus {
    fn new() -> Self {
        Self {
            memory: FlatMemory::new(0x1000000), // 16MB
            cycles_executed: 0,
        }
    }
}

impl Addressable for TestBus {
    fn read(&mut self, addr: u32) -> u8 {
        self.cycles_executed += 4;

        self.memory.byte(addr)
    }

    fn write(&mut self, addr: u32, value: u8) {
        self.cycles_executed += 4;

        self.memory.set(addr, value);
    }

    fn read_word(&mut self, addr: u32) -> u16 {
        self.cycles_executed += 4;
        let high = self.memory.byte(addr) as u16;
        let low = self.memory.byte(addr.wrapping_add(1)) as u16;
        (high << 8) | low // Big Endian
    }

    fn write_word(&mut self, addr: u32, value: u16) {
        self.cycles_executed += 4;
        self.memory.set(addr, (value >> 8) as u8);
        self.memory.set(addr.wrapping_add(1), (value & 0xFF) as u8);
    }

    fn cycles(&self) -> Option<u64> {
        Some(self.cycles_executed)
    }
}

impl CyclesTrackable for TestBus {
    fn cycles_executed(&self) -> u64 {
        self.cycles_executed
    }
}

/// Tom Harte tests use addresses as-is (24-bit space)
fn translate_address(tom_harte_addr: u32) -> u32 {
    tom_harte_addr & 0xFFFFFF
}

// ============================================================================
// The CPU under test
// ============================================================================

/// The 68000 and the test bus a case runs it on.
pub struct UnderTest {
    cpu: M68k,
    bus: TestBus,
}

impl CpuUnderTest for UnderTest {
    type Case = HarteTest;
    /// Checked only on request (`SINGLESTEP_CHECK_CYCLES`).
    const CYCLES: CycleCheck = CycleCheck::OnRequest;
    const FILE_KIND: &'static str = "instructions";
    const DEFECTIVE: &'static [(&'static str, &'static str)] = DEFECTIVE_VECTORS;

    fn case_name(case: &HarteTest) -> &str {
        &case.name
    }

    fn prepare(case: &HarteTest) -> Self {
        let mut cpu = M68k::new();
        let mut bus = TestBus::new();
        apply_initial_state(&mut cpu, &mut bus, &case.initial);
        bus.cycles_executed = 0;
        Self { cpu, bus }
    }

    fn execute(&mut self, _case: &HarteTest) -> u32 {
        self.cpu.step(&mut self.bus)
    }

    fn differences(&mut self, case: &HarteTest) -> Vec<String> {
        state_differences(&self.cpu, &self.bus, &case.final_state)
    }

    fn expected_cycles(case: &HarteTest) -> u32 {
        case.cycles
    }

    fn describe(&mut self, case: &HarteTest) -> String {
        let initial = &case.initial;
        let expected = &case.final_state;
        let mut lines = vec![String::new(), "--- OPCODE ---".to_string()];
        let opcode = initial.prefetch[0];
        lines.push(format!("Prefetch[0]: {:04X} (instruction)", opcode));
        lines.push(format!(
            "Prefetch[1]: {:04X} (extension)",
            initial.prefetch[1]
        ));
        lines.extend(move_operands(opcode));

        lines.push(String::new());
        lines.push("--- INITIAL STATE ---".to_string());
        lines.extend(state_lines(
            "Initial",
            &initial.data_regs(),
            &initial.addr_regs(),
            initial.sr,
            translate_address(initial.pc),
            initial.usp,
            initial.ssp,
        ));
        if !initial.ram.is_empty() {
            lines.push(String::new());
            lines.push("Initial RAM:".to_string());
            for (addr, val) in &initial.ram {
                lines.push(format!("  [{:08X}] = {:02X}", addr, val));
            }
        }

        let cpu = &self.cpu;
        lines.push(String::new());
        lines.push("--- ACTUAL STATE (after execution) ---".to_string());
        lines.extend(state_lines(
            "Actual", &cpu.d, &cpu.a, cpu.sr, cpu.pc, cpu.usp, cpu.ssp,
        ));

        lines.push(String::new());
        lines.push("--- EXPECTED STATE ---".to_string());
        lines.extend(state_lines(
            "Expected",
            &expected.data_regs(),
            &expected.addr_regs(),
            expected.sr,
            expected.pc,
            expected.usp,
            expected.ssp,
        ));
        if cpu.sr != expected.sr {
            lines.extend(sr_diff(expected.sr, cpu.sr));
        }
        lines.join("\n") + "\n"
    }
}

// ============================================================================
// State management
// ============================================================================

/// Apply initial state to CPU and bus
fn apply_initial_state(cpu: &mut M68k, bus: &mut TestBus, state: &CpuState) {
    // Set data registers
    let d_regs = state.data_regs();
    for (i, val) in d_regs.iter().enumerate() {
        cpu.d[i] = *val;
    }

    // Set address registers
    let a_regs = state.addr_regs();
    for (i, val) in a_regs.iter().enumerate() {
        cpu.a[i] = *val;
    }

    // Set USP and SSP
    cpu.usp = state.usp;
    cpu.ssp = state.ssp;

    // Set SR and PC (the PC masked to 24 bits)
    cpu.sr = state.sr;
    cpu.pc = translate_address(state.pc);

    // Write opcode at PC from prefetch[0]
    // Tom Harte tests put the instruction to execute in prefetch[0]
    bus.write_word(cpu.pc, state.prefetch[0]);

    // Write extension words if present (from prefetch[1])
    // Note: Some instructions have immediate data or addressing extensions
    bus.write_word(cpu.pc + 2, state.prefetch[1]);

    // Write RAM (translated addresses)
    for (addr, value) in &state.ram {
        let translated_addr = translate_address(*addr);
        bus.write(translated_addr, *value);
    }
}

/// What differs from the expected final state, one line each (the runner
/// adds the cycle count's).
fn state_differences(cpu: &M68k, bus: &TestBus, expected: &CpuState) -> Vec<String> {
    let mut errors = Vec::new();

    // Check data registers
    for (i, expected_val) in expected.data_regs().into_iter().enumerate() {
        if cpu.d[i] != expected_val {
            errors.push(format!(
                "D{}: expected {:08X}, got {:08X}",
                i, expected_val, cpu.d[i]
            ));
        }
    }

    // Check address registers (excluding A7/SSP which we check separately)
    for (i, expected_val) in expected.addr_regs().into_iter().enumerate().take(7) {
        if cpu.a[i] != expected_val {
            errors.push(format!(
                "A{}: expected {:08X}, got {:08X}",
                i, expected_val, cpu.a[i]
            ));
        }
    }

    // Check SSP (A7 in supervisor mode)
    if cpu.ssp != expected.ssp {
        errors.push(format!(
            "SSP: expected {:08X}, got {:08X}",
            expected.ssp, cpu.ssp
        ));
    }

    // Check USP
    if cpu.usp != expected.usp {
        errors.push(format!(
            "USP: expected {:08X}, got {:08X}",
            expected.usp, cpu.usp
        ));
    }

    // Check SR
    if cpu.sr != expected.sr {
        errors.push(format!(
            "SR: expected {:04X}, got {:04X}",
            expected.sr, cpu.sr
        ));
    }

    // Check PC, all 32 bits of it. The processor keeps the whole value
    // and the vectors record the whole value; only the 24 address lines
    // drop the top byte. Masking the expected side alone reads every jump
    // to an address with a non-zero top byte as a failure.
    let expected_pc = expected.pc;
    if cpu.pc != expected_pc {
        errors.push(format!(
            "PC: expected {:08X}, got {:08X}",
            expected_pc, cpu.pc
        ));
    }

    // Note: We don't check prefetch as it's an internal implementation detail
    // The tests will pass/fail based on final register and memory state

    // Check RAM (translated addresses)
    for (addr, expected_val) in &expected.ram {
        let translated_addr = translate_address(*addr);
        let actual_val = bus.memory.byte(translated_addr);
        if actual_val != *expected_val {
            errors.push(format!(
                "RAM[{:08X}]: expected {:02X}, got {:02X}",
                addr, expected_val, actual_val
            ));
        }
    }

    errors
}

// ============================================================================
// Display helpers
// ============================================================================

/// The size and operands of a MOVE (bits 15-14 = 00, bits 13-12 != 00);
/// nothing for another instruction.
fn move_operands(opcode: u16) -> Vec<String> {
    if (opcode & 0xC000) != 0x0000 || (opcode & 0x3000) == 0x0000 {
        return Vec::new();
    }
    // MOVE encoding: 00 ss ddd Ddd sss Sss
    // ss = size (bits 13-12)
    // ddd = dest reg (bits 11-9)
    // Ddd = dest mode (bits 8-6)
    // sss = source mode (bits 5-3) <- NOT bits 2-0!
    // Sss = source reg (bits 2-0) <- NOT bits 5-3!
    let size_bits = (opcode >> 12) & 0x3;
    let dst_mode = ((opcode >> 6) & 0x7) as u8;
    let dst_reg = ((opcode >> 9) & 0x7) as u8;
    // Source mode is bits 5-3, source reg is bits 2-0
    let src_mode = ((opcode >> 3) & 0x7) as u8;
    let src_reg = (opcode & 0x7) as u8;

    let size_name = match size_bits {
        1 => "byte",
        2 => "long",
        3 => "word",
        _ => "?",
    };

    vec![
        format!("  Size: {}", size_name),
        format!(
            "  Source EA: mode={} reg={} ({})",
            src_mode,
            src_reg,
            decode_ea_name(src_mode, src_reg)
        ),
        format!(
            "  Dest EA:   mode={} reg={} ({})",
            dst_mode,
            dst_reg,
            decode_ea_name(dst_mode, dst_reg)
        ),
    ]
}

/// CPU state in a formatted way
fn state_lines(
    label: &str,
    d: &[u32; 8],
    a: &[u32; 8],
    sr: u16,
    pc: u32,
    usp: u32,
    ssp: u32,
) -> Vec<String> {
    vec![
        format!("{}:", label),
        format!(
            "  D0={:08X} D1={:08X} D2={:08X} D3={:08X}",
            d[0], d[1], d[2], d[3]
        ),
        format!(
            "  D4={:08X} D5={:08X} D6={:08X} D7={:08X}",
            d[4], d[5], d[6], d[7]
        ),
        format!(
            "  A0={:08X} A1={:08X} A2={:08X} A3={:08X}",
            a[0], a[1], a[2], a[3]
        ),
        format!(
            "  A4={:08X} A5={:08X} A6={:08X} A7={:08X}",
            a[4], a[5], a[6], a[7]
        ),
        format!(
            "  PC={:08X} SR={:04X} USP={:08X} SSP={:08X}",
            pc, sr, usp, ssp
        ),
        sr_flags(sr),
    ]
}

/// SR flags in human readable format
fn sr_flags(sr: u16) -> String {
    let t = (sr >> 15) & 1;
    let s = (sr >> 13) & 1;
    let i = (sr >> 8) & 7;
    let x = (sr >> 4) & 1;
    let n = (sr >> 3) & 1;
    let z = (sr >> 2) & 1;
    let v = (sr >> 1) & 1;
    let c = sr & 1;
    format!(
        "  Flags: T={} S={} I={} X={} N={} Z={} V={} C={}",
        t, s, i, x, n, z, v, c
    )
}

/// Decode EA mode name for display
fn decode_ea_name(mode: u8, reg: u8) -> String {
    match mode {
        0 => format!("D{}", reg),
        1 => format!("A{}", reg),
        2 => format!("(A{})", reg),
        3 => format!("(A{})+", reg),
        4 => format!("-(A{})", reg),
        5 => format!("(d16,A{})", reg),
        6 => format!("(d8,A{},Xn)", reg),
        7 => match reg {
            0 => "(xxx).w".to_string(),
            1 => "(xxx).l".to_string(),
            2 => "(d16,PC)".to_string(),
            3 => "(d8,PC,Xn)".to_string(),
            4 => "#imm".to_string(),
            _ => format!("mode7.{}", reg),
        },
        _ => format!("mode{}.{}", mode, reg),
    }
}

/// SR flag differences
fn sr_diff(expected: u16, actual: u16) -> Option<String> {
    let flags = [
        ("T", 15),
        ("S", 13),
        ("I2", 10),
        ("I1", 9),
        ("I0", 8),
        ("X", 4),
        ("N", 3),
        ("Z", 2),
        ("V", 1),
        ("C", 0),
    ];

    let mut diffs = Vec::new();
    for (name, bit) in &flags {
        let exp_bit = (expected >> bit) & 1;
        let act_bit = (actual >> bit) & 1;
        if exp_bit != act_bit {
            diffs.push(format!("{}:{}->{}", name, exp_bit, act_bit));
        }
    }

    (!diffs.is_empty()).then(|| format!("  Flag diffs: {}", diffs.join(", ")))
}
