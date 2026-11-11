//! SingleStepTests runner for the SPC700, shared by the crate's integration
//! tests and `snes1-cli singlestep-spc700`.
//!
//! Test suite source: <https://github.com/SingleStepTests/spc700>
//!
//! Each test file (`<opcode>.json`) holds an array of 10 000 cases; a case
//! provides the full CPU/RAM state before and after one instruction plus the
//! per-cycle bus trace. Unlike the 65816 suite there's no `.e`/`.n` split,
//! the SPC700 has no emulation mode and a single 64 KiB address space.
//!
//! This module is compiled only when the `singlestep` feature is enabled so
//! the default (and WASM) builds never pull in `serde`/`serde_json`. Keeping
//! the runner here, rather than duplicated in the CLI, means the CLI and the
//! `cargo test` harness validate the CPU against byte-identical logic.
//!
//! `run_test_file` honours two developer env vars: `SINGLESTEP_INDEX=N`
//! diagnoses a single case by index, and `SINGLESTEP_DIAGNOSE=1` dumps the
//! initial/final/diff for the first failure.

use crate::Spc700;
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

// ============================================================================
// Dataset loading
// ============================================================================

/// Parse one test file via a buffered reader so 50+ MB files don't require
/// holding both source text and the DOM in RAM.
pub fn load_test_file(path: &Path) -> Result<Vec<TestSpc700>, Box<dyn std::error::Error>> {
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let tests: Vec<TestSpc700> = serde_json::from_reader(reader)?;
    Ok(tests)
}

/// Resolve a SingleStepTests file for `<opcode>.json` under `base/v1`.
/// Returns `None` if the file is missing.
pub fn find_test_file(base: &Path, opcode: u8) -> Option<PathBuf> {
    let candidate = base.join("v1").join(format!("{:02x}.json", opcode));
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

const RAM_SIZE: usize = 0x10000;

/// Flat 64 KiB RAM. `read`/`write` mask to 16-bit.
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

fn apply_initial_state(cpu: &mut Spc700, bus: &mut FlatMemoryBus, test: &TestSpc700) {
    let s = &test.initial;
    cpu.pc = s.pc;
    cpu.a = s.a;
    cpu.x = s.x;
    cpu.y = s.y;
    cpu.sp = s.sp;
    cpu.psw = s.psw;
    cpu.cycles_executed = 0;

    for byte in bus.mem.iter_mut() {
        *byte = 0;
    }
    for entry in &s.ram {
        bus.write_byte(entry.0, entry.1 as u8);
    }
}

fn check_final_state(
    cpu: &Spc700,
    bus: &FlatMemoryBus,
    test: &TestSpc700,
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
        let actual = bus.read_byte(entry.0);
        if actual != entry.1 as u8 {
            errors.push(format!(
                "RAM[{:04X}]: expected {:02X}, got {:02X}",
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

/// Outcome of a single SingleStepTests case.
pub enum TestResult {
    Pass,
    Fail(Vec<String>),
    Panic(String),
}

/// Run a single case in isolation, catching a panic so the case surfaces as
/// `Panic` rather than aborting the sweep.
pub fn run_test(test: &TestSpc700) -> TestResult {
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        let mut cpu = Spc700::new();
        let mut bus = FlatMemoryBus::new();
        apply_initial_state(&mut cpu, &mut bus, test);
        let cycles = cpu.step(&mut bus);
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
pub fn diagnose(test: &TestSpc700) {
    println!("\n{}", "=".repeat(70));
    println!("DIAGNOSE: {}", test.name);
    println!("{}", "=".repeat(70));

    println!("\n--- INITIAL ---");
    print_state("initial", &test.initial);
    println!("\nInitial RAM:");
    for entry in &test.initial.ram {
        println!("  [{:04X}] = {:02X}", entry.0, entry.1 as u8);
    }

    let exec = panic::catch_unwind(AssertUnwindSafe(|| {
        let mut cpu = Spc700::new();
        let mut bus = FlatMemoryBus::new();
        apply_initial_state(&mut cpu, &mut bus, test);
        let cycles = cpu.step(&mut bus);
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

fn print_state(label: &str, s: &StateSpc700) {
    println!(
        "  {} A={:02X} X={:02X} Y={:02X} SP={:02X} PSW={:02X} PC={:04X}",
        label, s.a, s.x, s.y, s.sp, s.psw, s.pc
    );
}

fn print_cpu(label: &str, cpu: &Spc700) {
    println!(
        "  {} A={:02X} X={:02X} Y={:02X} SP={:02X} PSW={:02X} PC={:04X}",
        label, cpu.a, cpu.x, cpu.y, cpu.sp, cpu.psw, cpu.pc
    );
}

// ============================================================================
// File-level runner
// ============================================================================

/// Aggregate result of running one opcode test file.
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

/// Run all cases (or `max_cases`) of one opcode. Returns `None` when the
/// `base/v1` dataset directory or the opcode file is absent so callers can
/// print a "fixture not found" hint and exit cleanly.
pub fn run_opcode(base: &Path, opcode: u8, max_cases: usize) -> Option<FileResult> {
    if !base.join("v1").is_dir() {
        return None;
    }
    let path = find_test_file(base, opcode)?;
    let label = format!("{:02X}", opcode);
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

/// Run `max_cases` of every opcode file under `base/v1`, printing a histogram
/// plus the worst and best offenders. Returns `None` when the dataset is
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
            // Print a one-liner for failing opcodes only; the full panic
            // backtraces from each instruction get pretty noisy at scale.
            let pct = (res.passed * 100).checked_div(res.total).unwrap_or(0);
            println!(
                "  {}: {}/{} pass ({}%) failed={} panicked={}",
                res.label, res.passed, res.total, pct, res.failed, res.panicked
            );
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
    println!("\nTop failing opcodes (up to 30):");
    for (label, passed, failed, panicked, total) in worst.iter().take(30) {
        println!(
            "  {}: failed={} panicked={} (passed={} of {})",
            label, failed, panicked, passed, total
        );
    }

    // Also show top passing so successful coverage is visible at a glance.
    let mut best: Vec<_> = per_opcode.iter().filter(|(_, p, _, _, _)| *p > 0).collect();
    best.sort_by_key(|e| std::cmp::Reverse(e.1));
    println!("\nTop passing opcodes (up to 30):");
    for (label, passed, failed, panicked, total) in best.iter().take(30) {
        println!(
            "  {}: passed={} (failed={} panicked={} of {})",
            label, passed, failed, panicked, total
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
