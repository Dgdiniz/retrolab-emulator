//! SingleStepTests runner for the WDC 65C816, shared by the crate's
//! integration tests and `snes1-cli singlestep-65816`.
//!
//! Test suite source: <https://github.com/SingleStepTests/65816>
//!
//! Each test file (`<opcode>.<e|n>.json`) holds an array of 10 000 cases;
//! a case provides the full CPU/RAM state before and after one instruction
//! plus the per-cycle bus trace. The `.e` variant pins the CPU in emulation
//! mode; `.n` covers native mode with random M/X combinations.
//!
//! This module is compiled only when the `singlestep` feature is enabled so
//! the default (and WASM) builds never pull in `serde`/`serde_json`. Keeping
//! the runner here, rather than duplicated in the CLI, means the CLI and
//! the `cargo test` harness validate the CPU against byte-identical logic.
//!
//! `run_test_file` honours two developer env vars: `SINGLESTEP_INDEX=N`
//! diagnoses a single case by index, and `SINGLESTEP_DIAGNOSE=1` dumps the
//! initial/final/diff for the first failure.

use crate::instructions::{FLAG_M, FLAG_X};
use crate::Wdc65816;
use retro_core::cpu::Addressable;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};

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

// ============================================================================
// Dataset loading
// ============================================================================

/// Parse one test file. Uses `serde_json::from_reader` over a buffered reader
/// so that 30+ MB native-mode files do not require holding both the source
/// text and the DOM in RAM simultaneously.
pub fn load_test_file(path: &Path) -> Result<Vec<Test65816>, Box<dyn std::error::Error>> {
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let tests: Vec<Test65816> = serde_json::from_reader(reader)?;
    Ok(tests)
}

/// Resolve a SingleStepTests file for `<opcode>.<mode>.json` under `base/v1`.
/// Returns `None` if the file is missing.
pub fn find_test_file(base: &Path, opcode: u8, mode: TestMode) -> Option<PathBuf> {
    let name = format!("{:02x}.{}.json", opcode, mode.suffix());
    let candidate = base.join("v1").join(name);
    if candidate.is_file() {
        Some(candidate)
    } else {
        None
    }
}

/// List every `*.json` test file under `base/v1`.
pub fn find_test_files(base: &Path) -> Vec<PathBuf> {
    let dir = base.join("v1");
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

// ============================================================================
// Test bus
// ============================================================================

const RAM_SIZE: usize = 0x100_0000; // full 24-bit address space

/// Flat 16 MB RAM. `read`/`write` mask to 24-bit. I/O ports are unused by the
/// 65C816: defaults from the trait are sufficient.
struct FlatMemoryBus {
    mem: Vec<u8>,
}

impl FlatMemoryBus {
    fn new() -> Self {
        Self {
            mem: vec![0; RAM_SIZE],
        }
    }

    fn read_byte(&self, addr: u32) -> u8 {
        self.mem[(addr as usize) & (RAM_SIZE - 1)]
    }

    fn write_byte(&mut self, addr: u32, value: u8) {
        self.mem[(addr as usize) & (RAM_SIZE - 1)] = value;
    }
}

impl Addressable for FlatMemoryBus {
    fn read(&mut self, addr: u32) -> u8 {
        self.read_byte(addr)
    }

    fn write(&mut self, addr: u32, value: u8) {
        self.write_byte(addr, value);
    }
}

// ============================================================================
// State management
// ============================================================================

/// Populate the CPU and bus with the initial test state.
///
/// Emulation-mode normalisations match real silicon:
/// - `P` keeps M and X forced to 1.
/// - X/Y high bytes clear (index registers are 8-bit).
/// - SP high byte clamped to `$01`.
///
/// `A` is loaded into `C` raw (16-bit). In M=1 modes only the low byte is
/// architecturally visible, but the high byte (`B`) is preserved.
fn apply_initial_state(cpu: &mut Wdc65816, bus: &mut FlatMemoryBus, test: &Test65816) {
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

    for byte in bus.mem.iter_mut() {
        *byte = 0;
    }
    for entry in &state.ram {
        bus.write_byte(entry.0, entry.1 as u8);
    }
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

/// Compare the CPU + RAM + total cycle count against the expected final
/// state. Returns the human-readable list of mismatches.
fn check_final_state(
    cpu: &Wdc65816,
    bus: &FlatMemoryBus,
    test: &Test65816,
    cycles_consumed: u32,
) -> Result<(), Vec<String>> {
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
        let actual = bus.read_byte(entry.0);
        if actual != entry.1 as u8 {
            errors.push(format!(
                "RAM[{:06X}]: expected {:02X}, got {:02X}",
                entry.0, entry.1 as u8, actual
            ));
        }
    }

    let expected_cycles = test.cycles.len() as u32;
    if cycles_consumed != expected_cycles {
        errors.push(format!(
            "Cycles: expected {}, got {}",
            expected_cycles, cycles_consumed
        ));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
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
fn execute_step(cpu: &mut Wdc65816, bus: &mut FlatMemoryBus, test: &Test65816) -> u32 {
    let opcode = bus.read_byte(((cpu.pbr as u32) << 16) | cpu.pc as u32);
    if opcode != 0x44 && opcode != 0x54 {
        return cpu.step(bus);
    }

    let budget = test.cycles.len() as u32;
    let mut accumulated: u32 = 0;

    while accumulated + CYCLES_PER_BLOCK_MOVE_ITER <= budget {
        let step_cycles = cpu.step(bus);
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

/// Outcome of a single SingleStepTests case.
pub enum TestResult {
    Pass,
    Fail(Vec<String>),
    Panic(String),
}

/// Run a single case in isolation, catching a panic so the case surfaces as
/// `Panic` rather than aborting the sweep.
pub fn run_test(test: &Test65816) -> TestResult {
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        let mut cpu = Wdc65816::new();
        let mut bus = FlatMemoryBus::new();
        apply_initial_state(&mut cpu, &mut bus, test);
        let cycles = execute_step(&mut cpu, &mut bus, test);
        check_final_state(&cpu, &bus, test, cycles)
    }));

    match result {
        Ok(Ok(())) => TestResult::Pass,
        Ok(Err(errors)) => TestResult::Fail(errors),
        Err(panic_info) => {
            let msg = if let Some(s) = panic_info.downcast_ref::<String>() {
                s.clone()
            } else if let Some(s) = panic_info.downcast_ref::<&str>() {
                s.to_string()
            } else {
                "Unknown panic".to_string()
            };
            TestResult::Panic(msg)
        }
    }
}

/// Print an initial/final/diff dump for one case (developer diagnostics).
pub fn diagnose(test: &Test65816) {
    println!("\n{}", "=".repeat(70));
    println!("DIAGNOSE: {}", test.name);
    println!("{}", "=".repeat(70));

    println!("\n--- INITIAL ---");
    print_state("initial", &test.initial);
    println!("\nInitial RAM:");
    for entry in &test.initial.ram {
        println!("  [{:06X}] = {:02X}", entry.0, entry.1 as u8);
    }

    let exec = panic::catch_unwind(AssertUnwindSafe(|| {
        let mut cpu = Wdc65816::new();
        let mut bus = FlatMemoryBus::new();
        apply_initial_state(&mut cpu, &mut bus, test);
        let cycles = execute_step(&mut cpu, &mut bus, test);
        (cpu, bus, cycles)
    }));

    match exec {
        Ok((cpu, bus, cycles)) => {
            println!("\n--- ACTUAL (after step) ---");
            print_cpu("actual", &cpu);
            println!("  cycles = {}", cycles);

            println!("\n--- EXPECTED ---");
            print_state("expected", &test.final_state);
            println!("  cycles = {}", test.cycles.len());

            println!("\n--- DIFF ---");
            match check_final_state(&cpu, &bus, test, cycles) {
                Ok(()) => println!("  (no differences - case PASSES)"),
                Err(errors) => {
                    for e in &errors {
                        println!("  {}", e);
                    }
                }
            }
        }
        Err(panic_info) => {
            let msg = if let Some(s) = panic_info.downcast_ref::<String>() {
                s.clone()
            } else if let Some(s) = panic_info.downcast_ref::<&str>() {
                s.to_string()
            } else {
                "Unknown panic".to_string()
            };
            println!("\n--- PANIC ---\n  {}", msg);
        }
    }
}

fn print_state(label: &str, state: &State65816) {
    println!("  {} A={:04X} X={:04X} Y={:04X} S={:04X} D={:04X} PC={:04X} PBR={:02X} DBR={:02X} P={:02X} E={}",
        label, state.a, state.x, state.y, state.s, state.d, state.pc, state.pbr, state.dbr, state.p, state.e);
}

fn print_cpu(label: &str, cpu: &Wdc65816) {
    println!(
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
    );
}

// ============================================================================
// File-level runner
// ============================================================================

/// Aggregate result of running one opcode-mode test file.
pub struct FileResult {
    pub label: String,
    pub passed: usize,
    pub failed: usize,
    pub panicked: usize,
    pub total: usize,
    pub first_failure_idx: Option<usize>,
    pub first_errors: Vec<String>,
}

/// Run up to `max_cases` of one test file. `0` means "all". Honours the
/// `SINGLESTEP_INDEX` / `SINGLESTEP_DIAGNOSE` env vars for single-case
/// diagnosis.
pub fn run_test_file(path: &Path, label: &str, max_cases: usize) -> FileResult {
    let tests = match load_test_file(path) {
        Ok(t) => t,
        Err(e) => {
            return FileResult {
                label: label.to_string(),
                passed: 0,
                failed: 0,
                panicked: 1,
                total: 1,
                first_failure_idx: Some(0),
                first_errors: vec![format!("load_test_file failed: {}", e)],
            };
        }
    };

    let diagnose_mode = std::env::var("SINGLESTEP_DIAGNOSE").is_ok();
    let specific_index: Option<usize> = std::env::var("SINGLESTEP_INDEX")
        .ok()
        .and_then(|s| s.parse().ok());

    if let Some(idx) = specific_index {
        if idx >= tests.len() {
            return FileResult {
                label: label.to_string(),
                passed: 0,
                failed: 1,
                panicked: 0,
                total: 1,
                first_failure_idx: Some(0),
                first_errors: vec![format!("index {} >= {}", idx, tests.len())],
            };
        }
        diagnose(&tests[idx]);
        let (passed, failed, panicked, errors) = match run_test(&tests[idx]) {
            TestResult::Pass => (1, 0, 0, Vec::new()),
            TestResult::Fail(e) => (0, 1, 0, e),
            TestResult::Panic(e) => (0, 0, 1, vec![e]),
        };
        return FileResult {
            label: label.to_string(),
            passed,
            failed,
            panicked,
            total: 1,
            first_failure_idx: if passed == 1 { None } else { Some(idx) },
            first_errors: errors,
        };
    }

    let limit = if max_cases == 0 {
        tests.len()
    } else {
        tests.len().min(max_cases)
    };

    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut panicked = 0usize;
    let mut first_failure_idx: Option<usize> = None;
    let mut first_errors: Vec<String> = Vec::new();

    for (i, test) in tests.iter().enumerate().take(limit) {
        match run_test(test) {
            TestResult::Pass => passed += 1,
            TestResult::Fail(errors) => {
                failed += 1;
                if first_failure_idx.is_none() {
                    first_failure_idx = Some(i);
                    first_errors = errors;
                }
            }
            TestResult::Panic(msg) => {
                panicked += 1;
                if first_failure_idx.is_none() {
                    first_failure_idx = Some(i);
                    first_errors = vec![format!("PANIC: {}", msg)];
                }
            }
        }
    }

    if diagnose_mode {
        if let Some(idx) = first_failure_idx {
            println!("\n>>> first failure at index {} of {}", idx, label);
            diagnose(&tests[idx]);
        }
    }

    FileResult {
        label: label.to_string(),
        passed,
        failed,
        panicked,
        total: limit,
        first_failure_idx,
        first_errors,
    }
}

fn label_for(opcode: u8, mode: TestMode) -> String {
    format!("{:02X}.{}", opcode, mode.suffix())
}

/// Run all cases (or `max_cases`) of one opcode-mode. Returns `None` when the
/// `base/v1` dataset directory or the specific opcode file is absent so
/// callers can print a "fixture not found" hint and exit cleanly.
pub fn run_opcode(base: &Path, opcode: u8, mode: TestMode, max_cases: usize) -> Option<FileResult> {
    if !base.join("v1").is_dir() {
        return None;
    }
    let path = find_test_file(base, opcode, mode)?;
    let label = label_for(opcode, mode);
    Some(run_test_file(&path, &label, max_cases))
}

/// Print a one-file result line plus the first failure's errors (up to 8).
pub fn print_file_result(res: &FileResult) {
    let pct = (res.passed * 100).checked_div(res.total).unwrap_or(0);
    println!(
        "{}: {}/{} pass ({}%) - failed={} panicked={}",
        res.label, res.passed, res.total, pct, res.failed, res.panicked
    );
    if !res.first_errors.is_empty() {
        if let Some(idx) = res.first_failure_idx {
            println!("  first failure at index {}:", idx);
            for e in res.first_errors.iter().take(8) {
                println!("    {}", e);
            }
        }
    }
}

// ============================================================================
// Sweep
// ============================================================================

/// Roll-up of an all-opcode sweep.
pub struct SweepSummary {
    pub passed: usize,
    pub failed: usize,
    pub panicked: usize,
    pub total: usize,
}

/// Run `max_cases` of every opcode-mode file under `base/v1`, printing a
/// histogram and the worst offenders. Returns `None` when the dataset is
/// missing/empty.
pub fn run_sweep(base: &Path, max_cases: usize, label: &str) -> Option<SweepSummary> {
    if !base.join("v1").is_dir() {
        return None;
    }
    let files = find_test_files(base);
    if files.is_empty() {
        return None;
    }

    println!(
        "Sweep '{}': {} files, max_cases={}",
        label,
        files.len(),
        max_cases
    );
    println!("{}", "=".repeat(70));

    let mut total_passed = 0usize;
    let mut total_failed = 0usize;
    let mut total_panicked = 0usize;
    let mut histogram: HashMap<&'static str, usize> = HashMap::new();
    let buckets = ["0", "1-10", "11-50", "51-500", "501-999", "1000+"];
    for b in &buckets {
        histogram.insert(*b, 0);
    }
    let mut per_opcode: Vec<(String, usize, usize, usize, usize)> = Vec::new();

    for path in &files {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
        let res = run_test_file(path, stem, max_cases);
        total_passed += res.passed;
        total_failed += res.failed;
        total_panicked += res.panicked;

        let bad = res.failed + res.panicked;
        let bucket = bucket_of(bad);
        *histogram.entry(bucket).or_insert(0) += 1;
        per_opcode.push((
            res.label.clone(),
            res.passed,
            res.failed,
            res.panicked,
            res.total,
        ));

        if res.failed > 0 || res.panicked > 0 {
            print_file_result(&res);
        }
    }

    let total = total_passed + total_failed + total_panicked;
    let pct = (total_passed * 100).checked_div(total).unwrap_or(0);
    println!("\n{}", "=".repeat(70));
    println!(
        "OVERALL: {}/{} pass ({}%) - failed={} panicked={}",
        total_passed, total, pct, total_failed, total_panicked
    );

    println!(
        "\nHistogram (files by failure count, out of {} per file):",
        max_cases
    );
    for b in &buckets {
        println!("  {:>8}: {}", b, histogram.get(b).copied().unwrap_or(0));
    }

    let mut worst: Vec<_> = per_opcode
        .iter()
        .filter(|(_, _, f, p, _)| *f + *p > 0)
        .collect();
    worst.sort_by_key(|e| std::cmp::Reverse(e.2 + e.3));
    println!("\nTop failing opcode-modes (up to 30):");
    for (label, passed, failed, panicked, total) in worst.iter().take(30) {
        println!(
            "  {}: failed={} panicked={} (passed={} of {})",
            label, failed, panicked, passed, total
        );
    }

    Some(SweepSummary {
        passed: total_passed,
        failed: total_failed,
        panicked: total_panicked,
        total,
    })
}

fn bucket_of(bad: usize) -> &'static str {
    match bad {
        0 => "0",
        1..=10 => "1-10",
        11..=50 => "11-50",
        51..=500 => "51-500",
        501..=999 => "501-999",
        _ => "1000+",
    }
}
