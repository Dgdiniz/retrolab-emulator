//! SingleStepTests Z80 integration
//!
//! Test suite from: https://github.com/SingleStepTests/z80
//!
//! Each test provides full CPU and memory state before and after executing a single instruction.
//!
//! What is the Z80's is here: the case's shape, the test bus (64 KiB of RAM
//! and the I/O ports a case replays), the state a case sets, what is
//! compared and how the state is shown ([`UnderTest`]); retro-core's runner
//! (`retro_core::testing::singlestep`) runs the cases.
//!
//! Shared by several independent test binaries, each of which compiles this
//! module separately and uses a different subset of it, so per-binary
//! dead-code lints fire on items a sibling binary depends on.
#![allow(dead_code)]

use retro_core::cpu::Addressable;
use retro_core::testing::singlestep::{CpuUnderTest, CycleCheck, FlatMemory};
use serde::{Deserialize, Serialize};
use sg1_core::z80::Z80;
use std::collections::HashMap;

/// A single test case from SingleStepTests
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Z80Test {
    pub name: String,
    pub initial: Z80State,
    #[serde(rename = "final")]
    pub final_state: Z80State,
    #[serde(default)]
    pub cycles: Vec<CycleState>,
    #[serde(default)]
    pub ports: Vec<PortAccess>,
}

/// Z80 CPU state (registers and memory)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Z80State {
    pub pc: u16,
    pub sp: u16,
    pub a: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub f: u8,
    pub h: u8,
    pub l: u8,
    pub i: u8,
    pub r: u8,
    pub ei: u8,  // EI executed flag
    pub wz: u16, // Internal WZ register
    pub ix: u16,
    pub iy: u16,
    pub af_: u16, // Shadow AF (stored as 16-bit)
    pub bc_: u16, // Shadow BC (stored as 16-bit)
    pub de_: u16, // Shadow DE (stored as 16-bit)
    pub hl_: u16, // Shadow HL (stored as 16-bit)
    pub im: u8,
    pub p: u8,              // P flag (tracks LD A,I / LD A,R)
    pub q: u8,              // Q flag (tracks flag modifications)
    pub iff1: u8,           // IFF1 (interrupt enable flip-flop 1)
    pub iff2: u8,           // IFF2 (interrupt enable flip-flop 2)
    pub ram: Vec<[u16; 2]>, // [[address, value], ...]
}

/// I/O port access: [port_address, value, "r"|"w"]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortAccess(pub u16, pub u8, pub String);

/// Bus cycle state
/// Format: [address, data, flags]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CycleState {
    Full(CycleAddress, CycleData, String),
}

/// Address can be u16 or null
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CycleAddress {
    Value(u16),
    Null,
}

/// Data can be u8 or null
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CycleData {
    Value(u8),
    Null,
}

impl Z80State {
    /// Create memory map from RAM entries
    pub fn memory_map(&self) -> HashMap<u16, u8> {
        self.ram
            .iter()
            .map(|entry| (entry[0], entry[1] as u8))
            .collect()
    }

    /// Get AF register (A and F combined)
    pub fn af(&self) -> u16 {
        ((self.a as u16) << 8) | (self.f as u16)
    }

    /// Get BC register (B and C combined)
    pub fn bc(&self) -> u16 {
        ((self.b as u16) << 8) | (self.c as u16)
    }

    /// Get DE register (D and E combined)
    pub fn de(&self) -> u16 {
        ((self.d as u16) << 8) | (self.e as u16)
    }

    /// Get HL register (H and L combined)
    pub fn hl(&self) -> u16 {
        ((self.h as u16) << 8) | (self.l as u16)
    }
}

/// Opcode classification from filename
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::upper_case_acronyms)]
pub enum OpcodeGroup {
    /// Unprefixed: "xx.json"
    Unprefixed(u8),
    /// CB prefix: "cb xx.json"
    CB(u8),
    /// DD prefix (no CB): "dd xx.json"
    DD(u8),
    /// ED prefix: "ed xx.json"
    ED(u8),
    /// FD prefix (no CB): "fd xx.json"
    FD(u8),
    /// DD CB prefix: "dd cb __ xx.json"
    DDCB(u8),
    /// FD CB prefix: "fd cb __ xx.json"
    FDCB(u8),
}

impl OpcodeGroup {
    /// Human-readable label for this opcode group
    pub fn label(&self) -> String {
        match self {
            OpcodeGroup::Unprefixed(op) => format!("{:02X}", op),
            OpcodeGroup::CB(op) => format!("CB {:02X}", op),
            OpcodeGroup::DD(op) => format!("DD {:02X}", op),
            OpcodeGroup::ED(op) => format!("ED {:02X}", op),
            OpcodeGroup::FD(op) => format!("FD {:02X}", op),
            OpcodeGroup::DDCB(op) => format!("DD CB __ {:02X}", op),
            OpcodeGroup::FDCB(op) => format!("FD CB __ {:02X}", op),
        }
    }

    /// Group name for the prefix (used in summary)
    pub fn prefix_name(&self) -> &'static str {
        match self {
            OpcodeGroup::Unprefixed(_) => "Unprefixed",
            OpcodeGroup::CB(_) => "CB",
            OpcodeGroup::DD(_) => "DD",
            OpcodeGroup::ED(_) => "ED",
            OpcodeGroup::FD(_) => "FD",
            OpcodeGroup::DDCB(_) => "DD CB",
            OpcodeGroup::FDCB(_) => "FD CB",
        }
    }
}

/// Parse opcode group from test file name
///
/// Handles all naming patterns:
/// - "00.json" -> Unprefixed(0x00)
/// - "cb 00.json" -> CB(0x00)
/// - "dd 21.json" -> DD(0x21)
/// - "ed 78.json" -> ED(0x78)
/// - "fd db.json" -> FD(0xDB)
/// - "dd cb __ 04.json" -> DDCB(0x04)
/// - "fd cb __ 93.json" -> FDCB(0x93)
pub fn opcode_from_filename(path: &std::path::Path) -> Option<OpcodeGroup> {
    let filename = path.file_stem()?.to_str()?;
    let parts: Vec<&str> = filename.split_whitespace().collect();

    match parts.as_slice() {
        // "xx" -> Unprefixed
        [hex] => {
            let op = u8::from_str_radix(hex, 16).ok()?;
            Some(OpcodeGroup::Unprefixed(op))
        }
        // "cb xx", "dd xx", "ed xx", "fd xx"
        [prefix, hex] => {
            let op = u8::from_str_radix(hex, 16).ok()?;
            match *prefix {
                "cb" => Some(OpcodeGroup::CB(op)),
                "dd" => Some(OpcodeGroup::DD(op)),
                "ed" => Some(OpcodeGroup::ED(op)),
                "fd" => Some(OpcodeGroup::FD(op)),
                _ => None,
            }
        }
        // "dd cb __ xx" or "fd cb __ xx"
        [prefix, "cb", "__", hex] => {
            let op = u8::from_str_radix(hex, 16).ok()?;
            match *prefix {
                "dd" => Some(OpcodeGroup::DDCB(op)),
                "fd" => Some(OpcodeGroup::FDCB(op)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The prefix group of a file a report's label names (`DD CB __ 04` is in
/// `DD CB`, `CB 00` in `CB`, `00` in `Unprefixed`), for a sweep's lines per
/// group.
pub fn group_of_label(label: &str) -> String {
    match label.split_whitespace().collect::<Vec<_>>().as_slice() {
        [_] => "Unprefixed".to_string(),
        [prefix, _] => prefix.to_string(),
        [prefix, cb, "__", _] => format!("{prefix} {cb}"),
        _ => "Unknown".to_string(),
    }
}

// ============================================================================
// Test Bus with I/O Port Support
// ============================================================================

/// Test bus providing 64KB RAM and I/O port read/write support.
/// Port reads return values from `port_reads` map; port writes are captured.
struct TestBus {
    memory: FlatMemory,
    /// Expected port read values: port_address -> value
    port_reads: Vec<(u16, u8)>,
    port_read_index: usize,
    /// Captured port writes: (port_address, value)
    port_writes: Vec<(u16, u8)>,
}

impl TestBus {
    fn new() -> Self {
        Self {
            memory: FlatMemory::new(0x10000),
            port_reads: Vec::new(),
            port_read_index: 0,
            port_writes: Vec::new(),
        }
    }
}

impl Addressable for TestBus {
    fn read(&mut self, addr: u32) -> u8 {
        self.memory.byte(addr)
    }

    fn write(&mut self, addr: u32, value: u8) {
        self.memory.set(addr, value);
    }

    fn io_read(&mut self, _port: u16) -> u8 {
        // Return next expected port read value in order
        if self.port_read_index < self.port_reads.len() {
            let (_, value) = self.port_reads[self.port_read_index];
            self.port_read_index += 1;
            value
        } else {
            0xFF // Floating bus default
        }
    }

    fn io_write(&mut self, port: u16, value: u8) {
        self.port_writes.push((port, value));
    }
}

// ============================================================================
// The CPU under test
// ============================================================================

/// The Z80 and the test bus a case runs it on.
pub struct UnderTest {
    cpu: Z80,
    bus: TestBus,
}

impl CpuUnderTest for UnderTest {
    type Case = Z80Test;
    /// The vectors' cycle list has one entry a cycle, its length the
    /// count; it is checked only on request (`SINGLESTEP_CHECK_CYCLES`).
    const CYCLES: CycleCheck = CycleCheck::OnRequest;
    const FILE_KIND: &'static str = "opcodes";

    fn case_name(case: &Z80Test) -> &str {
        &case.name
    }

    fn prepare(case: &Z80Test) -> Self {
        let mut cpu = Z80::new();
        let mut bus = TestBus::new();
        apply_initial_state(&mut cpu, &mut bus, case);
        Self { cpu, bus }
    }

    fn execute(&mut self, _case: &Z80Test) -> u32 {
        self.cpu.step(&mut self.bus)
    }

    fn differences(&mut self, case: &Z80Test) -> Vec<String> {
        state_differences(&self.cpu, &self.bus, case)
    }

    fn expected_cycles(case: &Z80Test) -> u32 {
        case.cycles.len() as u32
    }

    fn describe(&mut self, case: &Z80Test) -> String {
        let mut lines = vec![String::new(), "--- INITIAL STATE ---".to_string()];
        lines.extend(state_lines("Initial", &case.initial));
        if !case.initial.ram.is_empty() {
            lines.push(String::new());
            lines.push("Initial RAM:".to_string());
            for entry in &case.initial.ram {
                lines.push(format!("  [{:04X}] = {:02X}", entry[0], entry[1]));
            }
        }
        if !case.ports.is_empty() {
            lines.push(String::new());
            lines.push("Ports:".to_string());
            for p in &case.ports {
                lines.push(format!("  port {:04X} = {:02X} ({})", p.0, p.1, p.2));
            }
        }
        lines.push(String::new());
        lines.push("--- ACTUAL STATE (after execution) ---".to_string());
        lines.extend(cpu_lines("Actual", &self.cpu));
        lines.push(String::new());
        lines.push("--- EXPECTED STATE ---".to_string());
        lines.extend(state_lines("Expected", &case.final_state));
        if self.cpu.f != case.final_state.f {
            lines.extend(flag_diff(case.final_state.f, self.cpu.f));
        }
        lines.join("\n") + "\n"
    }
}

// ============================================================================
// State Management
// ============================================================================

/// Apply initial state to Z80 CPU and TestBus
fn apply_initial_state(cpu: &mut Z80, bus: &mut TestBus, test: &Z80Test) {
    let state = &test.initial;

    // Set 8-bit registers
    cpu.a = state.a;
    cpu.b = state.b;
    cpu.c = state.c;
    cpu.d = state.d;
    cpu.e = state.e;
    cpu.f = state.f;
    cpu.h = state.h;
    cpu.l = state.l;
    cpu.i = state.i;
    cpu.r = state.r;

    // Set 16-bit registers
    cpu.pc = state.pc;
    cpu.sp = state.sp;
    cpu.ix = state.ix;
    cpu.iy = state.iy;

    // Set internal registers
    cpu.wz = state.wz;
    cpu.ei = state.ei;
    cpu.p = state.p;
    cpu.q = state.q;

    // Set shadow registers (stored as 16-bit in test format)
    cpu.a_ = (state.af_ >> 8) as u8;
    cpu.f_ = (state.af_ & 0xFF) as u8;
    cpu.b_ = (state.bc_ >> 8) as u8;
    cpu.c_ = (state.bc_ & 0xFF) as u8;
    cpu.d_ = (state.de_ >> 8) as u8;
    cpu.e_ = (state.de_ & 0xFF) as u8;
    cpu.h_ = (state.hl_ >> 8) as u8;
    cpu.l_ = (state.hl_ & 0xFF) as u8;

    // Set interrupt state
    cpu.im = state.im;
    cpu.iff1 = state.iff1 != 0;
    cpu.iff2 = state.iff2 != 0;

    // Ensure CPU is not halted
    cpu.halted = false;

    // Write RAM
    bus.memory.load(
        state
            .ram
            .iter()
            .map(|entry| (u32::from(entry[0]), entry[1] as u8)),
    );

    // Setup I/O port expectations from test's ports field
    bus.port_reads.clear();
    bus.port_read_index = 0;
    bus.port_writes.clear();

    for port_access in &test.ports {
        if port_access.2 == "r" {
            bus.port_reads.push((port_access.0, port_access.1));
        }
    }
}

/// What differs from the expected final state, one line each (the runner
/// adds the cycle count's).
fn state_differences(cpu: &Z80, bus: &TestBus, test: &Z80Test) -> Vec<String> {
    let expected = &test.final_state;
    let mut errors = Vec::new();

    // Check 8-bit registers
    macro_rules! check_reg8 {
        ($name:expr, $actual:expr, $expected:expr) => {
            if $actual != $expected {
                errors.push(format!(
                    "{}: expected {:02X}, got {:02X}",
                    $name, $expected, $actual
                ));
            }
        };
    }
    check_reg8!("A", cpu.a, expected.a);
    check_reg8!("B", cpu.b, expected.b);
    check_reg8!("C", cpu.c, expected.c);
    check_reg8!("D", cpu.d, expected.d);
    check_reg8!("E", cpu.e, expected.e);
    check_reg8!("F", cpu.f, expected.f);
    check_reg8!("H", cpu.h, expected.h);
    check_reg8!("L", cpu.l, expected.l);
    check_reg8!("I", cpu.i, expected.i);
    check_reg8!("R", cpu.r, expected.r);

    // Check 16-bit registers
    macro_rules! check_reg16 {
        ($name:expr, $actual:expr, $expected:expr) => {
            if $actual != $expected {
                errors.push(format!(
                    "{}: expected {:04X}, got {:04X}",
                    $name, $expected, $actual
                ));
            }
        };
    }
    check_reg16!("PC", cpu.pc, expected.pc);
    check_reg16!("SP", cpu.sp, expected.sp);
    check_reg16!("IX", cpu.ix, expected.ix);
    check_reg16!("IY", cpu.iy, expected.iy);
    check_reg16!("WZ", cpu.wz, expected.wz);

    // Check shadow registers
    let actual_af_ = ((cpu.a_ as u16) << 8) | (cpu.f_ as u16);
    check_reg16!("AF'", actual_af_, expected.af_);

    let actual_bc_ = ((cpu.b_ as u16) << 8) | (cpu.c_ as u16);
    check_reg16!("BC'", actual_bc_, expected.bc_);

    let actual_de_ = ((cpu.d_ as u16) << 8) | (cpu.e_ as u16);
    check_reg16!("DE'", actual_de_, expected.de_);

    let actual_hl_ = ((cpu.h_ as u16) << 8) | (cpu.l_ as u16);
    check_reg16!("HL'", actual_hl_, expected.hl_);

    // Check interrupt state
    check_reg8!("IM", cpu.im, expected.im);

    let actual_iff1: u8 = if cpu.iff1 { 1 } else { 0 };
    if actual_iff1 != expected.iff1 {
        errors.push(format!(
            "IFF1: expected {}, got {}",
            expected.iff1, actual_iff1
        ));
    }

    let actual_iff2: u8 = if cpu.iff2 { 1 } else { 0 };
    if actual_iff2 != expected.iff2 {
        errors.push(format!(
            "IFF2: expected {}, got {}",
            expected.iff2, actual_iff2
        ));
    }

    // Check internal state
    check_reg8!("EI", cpu.ei, expected.ei);
    check_reg8!("P", cpu.p, expected.p);
    check_reg8!("Q", cpu.q, expected.q);

    // Check RAM
    for entry in &expected.ram {
        let addr = entry[0];
        let expected_val = entry[1] as u8;
        let actual_val = bus.memory.byte(u32::from(addr));
        if actual_val != expected_val {
            errors.push(format!(
                "RAM[{:04X}]: expected {:02X}, got {:02X}",
                addr, expected_val, actual_val
            ));
        }
    }

    // Check I/O port writes
    let expected_writes: Vec<(u16, u8)> = test
        .ports
        .iter()
        .filter(|p| p.2 == "w")
        .map(|p| (p.0, p.1))
        .collect();

    if expected_writes.len() != bus.port_writes.len() {
        errors.push(format!(
            "Port writes count: expected {}, got {}",
            expected_writes.len(),
            bus.port_writes.len()
        ));
    } else {
        for (i, (exp, act)) in expected_writes
            .iter()
            .zip(bus.port_writes.iter())
            .enumerate()
        {
            if exp.1 != act.1 {
                errors.push(format!(
                    "Port write[{}] value: expected {:02X} (port {:04X}), got {:02X} (port {:04X})",
                    i, exp.1, exp.0, act.1, act.0
                ));
            }
        }
    }

    errors
}

// ============================================================================
// Display Helpers
// ============================================================================

fn state_lines(label: &str, state: &Z80State) -> Vec<String> {
    vec![
        format!("{}:", label),
        format!(
            "  A={:02X} F={:02X} B={:02X} C={:02X} D={:02X} E={:02X} H={:02X} L={:02X}",
            state.a, state.f, state.b, state.c, state.d, state.e, state.h, state.l
        ),
        format!(
            "  I={:02X} R={:02X} IX={:04X} IY={:04X} SP={:04X} PC={:04X} WZ={:04X}",
            state.i, state.r, state.ix, state.iy, state.sp, state.pc, state.wz
        ),
        format!(
            "  AF'={:04X} BC'={:04X} DE'={:04X} HL'={:04X}",
            state.af_, state.bc_, state.de_, state.hl_
        ),
        format!(
            "  IM={} IFF1={} IFF2={} EI={} P={} Q={}",
            state.im, state.iff1, state.iff2, state.ei, state.p, state.q
        ),
        flags_line(state.f),
    ]
}

fn cpu_lines(label: &str, cpu: &Z80) -> Vec<String> {
    let af_ = ((cpu.a_ as u16) << 8) | (cpu.f_ as u16);
    let bc_ = ((cpu.b_ as u16) << 8) | (cpu.c_ as u16);
    let de_ = ((cpu.d_ as u16) << 8) | (cpu.e_ as u16);
    let hl_ = ((cpu.h_ as u16) << 8) | (cpu.l_ as u16);
    vec![
        format!("{}:", label),
        format!(
            "  A={:02X} F={:02X} B={:02X} C={:02X} D={:02X} E={:02X} H={:02X} L={:02X}",
            cpu.a, cpu.f, cpu.b, cpu.c, cpu.d, cpu.e, cpu.h, cpu.l
        ),
        format!(
            "  I={:02X} R={:02X} IX={:04X} IY={:04X} SP={:04X} PC={:04X} WZ={:04X}",
            cpu.i, cpu.r, cpu.ix, cpu.iy, cpu.sp, cpu.pc, cpu.wz
        ),
        format!(
            "  AF'={:04X} BC'={:04X} DE'={:04X} HL'={:04X}",
            af_, bc_, de_, hl_
        ),
        format!(
            "  IM={} IFF1={} IFF2={} EI={} P={} Q={}",
            cpu.im,
            if cpu.iff1 { 1 } else { 0 },
            if cpu.iff2 { 1 } else { 0 },
            cpu.ei,
            cpu.p,
            cpu.q
        ),
        flags_line(cpu.f),
    ]
}

fn flags_line(f: u8) -> String {
    format!(
        "  Flags: S={} Z={} Y={} H={} X={} P/V={} N={} C={}",
        (f >> 7) & 1,
        (f >> 6) & 1,
        (f >> 5) & 1,
        (f >> 4) & 1,
        (f >> 3) & 1,
        (f >> 2) & 1,
        (f >> 1) & 1,
        f & 1,
    )
}

fn flag_diff(expected: u8, actual: u8) -> Option<String> {
    let flag_names = ["C", "N", "P/V", "X", "H", "Y", "Z", "S"];
    let mut diffs = Vec::new();
    for (bit, name) in flag_names.iter().enumerate() {
        let exp = (expected >> bit) & 1;
        let act = (actual >> bit) & 1;
        if exp != act {
            diffs.push(format!("{}:{}->{}", name, exp, act));
        }
    }
    (!diffs.is_empty()).then(|| format!("  Flag diffs: {}", diffs.join(", ")))
}
