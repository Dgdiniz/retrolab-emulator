//! The runner of CPU test vectors: files of cases, each the state of a
//! processor and its memory before and after one instruction, run one by
//! one against a CPU of this tree. The sets it runs are Tom Harte's 68000
//! vectors (ProcessorTests) and the SingleStepTests of the Z80, the 65C816
//! and the SPC700 (<https://github.com/SingleStepTests>): JSON arrays of
//! cases, one file an opcode or an instruction, plain (`.json`) or gzipped
//! (`.json.gz`).
//!
//! What is here is the same for every CPU: reading a file, running its
//! cases (a panic caught, a defective vector skipped, the cycle count
//! checked as the CPU asks), the counts and the report of a file, a sweep
//! over many files, the diagnosis of a case, and the developer's switches.
//! A CPU says what its hardware makes different, in its [`CpuUnderTest`]:
//! the shape of a case, how the CPU and its memory are set up from it, how
//! the instruction runs, what is compared, and how its state is shown.
//!
//! # The switches
//!
//! [`RunOptions::from_env`] reads them from the environment; a variable
//! counts as set whatever its value.
//!
//! - `SINGLESTEP_INDEX=N`: run case `N` of each file alone, after its
//!   diagnosis.
//! - `SINGLESTEP_DIAGNOSE`: diagnose each file's first failing case.
//! - `SINGLESTEP_FULL_DIAG`: diagnose each file's first
//!   [`FULL_DIAGNOSIS_CASES`] failing cases (it wins over
//!   `SINGLESTEP_DIAGNOSE`).
//! - `SINGLESTEP_CHECK_CYCLES`: check the cycle count of a CPU that checks
//!   it only on request ([`CycleCheck::OnRequest`]).
//!
//! A diagnosis prints the case's name, the CPU's state before the
//! instruction, after it and as the case expects it, the cycles it took
//! and what differs.

use std::any::Any;
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::io::Read;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use crate::cpu::Addressable;

/// The number of failing cases of a file `SINGLESTEP_FULL_DIAG` diagnoses.
pub const FULL_DIAGNOSIS_CASES: usize = 20;

/// The lines of a file's first failure its report prints.
const FIRST_FAILURE_LINES: usize = 8;

/// The files a sweep's report lists among the worst.
const WORST_FILES: usize = 30;

/// How a CPU's cycle count is checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CycleCheck {
    /// A count that differs from the case's fails it.
    Always,
    /// Only when the run asks for it ([`RunOptions::check_cycles`],
    /// `SINGLESTEP_CHECK_CYCLES`).
    OnRequest,
}

/// A CPU and the memory a test-vector case runs it on.
///
/// The runner makes one for each case ([`prepare`](Self::prepare)), runs
/// the instruction ([`execute`](Self::execute)) and asks what differs from
/// the case's final state ([`differences`](Self::differences)); it adds
/// the cycle count's difference itself, as [`CYCLES`](Self::CYCLES) says.
pub trait CpuUnderTest: Sized {
    /// One case, as the vector files hold it.
    type Case: DeserializeOwned;

    /// How the CPU's cycle count is checked.
    const CYCLES: CycleCheck;

    /// What one vector file holds the cases of, as the report of a sweep
    /// names its files: `opcodes`, `opcode-modes`, `instructions`.
    const FILE_KIND: &'static str;

    /// Cases whose vector is wrong, by name, each with what is wrong in
    /// it (said after "the vector"). They are skipped, counted, and left
    /// out of the pass rate.
    const DEFECTIVE: &'static [(&'static str, &'static str)] = &[];

    /// The case's name, as the vector file gives it.
    fn case_name(case: &Self::Case) -> &str;

    /// The CPU and its memory in the case's initial state.
    fn prepare(case: &Self::Case) -> Self;

    /// Run the case's instruction; the cycles it took.
    fn execute(&mut self, case: &Self::Case) -> u32;

    /// What differs from the case's final state, one line each, in the
    /// CPU's order (registers, flags, memory, ports); empty when nothing
    /// does.
    fn differences(&mut self, case: &Self::Case) -> Vec<String>;

    /// The cycle count the case gives.
    fn expected_cycles(case: &Self::Case) -> u32;

    /// The CPU's state before the instruction, after it and as the case
    /// expects it, for a diagnosis: called after [`execute`](Self::execute).
    fn describe(&mut self, case: &Self::Case) -> String;
}

/// What became of one case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaseResult {
    /// The CPU ended where the case says.
    Passed,
    /// What differs, one line each.
    Failed(Vec<String>),
    /// The instruction panicked: the panic's message.
    Panicked(String),
    /// The case is named in [`CpuUnderTest::DEFECTIVE`]: what is wrong in
    /// its vector.
    Skipped(&'static str),
}

/// Which failing cases a run diagnoses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Diagnose {
    /// None.
    #[default]
    No,
    /// Each file's first.
    First,
    /// Each file's first so many.
    Failures(usize),
}

/// How the cases of a file are run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunOptions {
    /// The cases run of each file, from its first; `0` runs every case.
    pub max_cases: usize,
    /// Run only this case of each file, after its diagnosis.
    pub only_case: Option<usize>,
    /// Which failing cases are diagnosed.
    pub diagnose: Diagnose,
    /// Check the cycle count of a CPU that checks it only on request.
    pub check_cycles: bool,
}

impl RunOptions {
    /// `max_cases` of each file, the rest from the environment
    /// (`SINGLESTEP_INDEX`, `SINGLESTEP_DIAGNOSE`, `SINGLESTEP_FULL_DIAG`,
    /// `SINGLESTEP_CHECK_CYCLES`; the module's doc says what each does).
    pub fn from_env(max_cases: usize) -> Self {
        Self::from_vars(max_cases, None, |name| std::env::var(name).ok())
    }

    /// As [`from_env`](Self::from_env), each `SINGLESTEP_` variable that
    /// is not set read under `fallback_prefix` instead: with `"HARTE"`,
    /// `HARTE_INDEX`, `HARTE_DIAGNOSE`, `HARTE_FULL_DIAG` and
    /// `HARTE_CHECK_CYCLES`, the names a runner used before it ran here.
    pub fn from_env_with_fallback(max_cases: usize, fallback_prefix: &str) -> Self {
        Self::from_vars(max_cases, Some(fallback_prefix), |name| {
            std::env::var(name).ok()
        })
    }

    /// The options from the variables `var` reads.
    fn from_vars(
        max_cases: usize,
        fallback_prefix: Option<&str>,
        var: impl Fn(&str) -> Option<String>,
    ) -> Self {
        let read = |switch: &str| {
            var(&format!("SINGLESTEP_{switch}"))
                .or_else(|| fallback_prefix.and_then(|prefix| var(&format!("{prefix}_{switch}"))))
        };
        let diagnose = if read("FULL_DIAG").is_some() {
            Diagnose::Failures(FULL_DIAGNOSIS_CASES)
        } else if read("DIAGNOSE").is_some() {
            Diagnose::First
        } else {
            Diagnose::No
        };
        Self {
            max_cases,
            only_case: read("INDEX").and_then(|value| value.parse().ok()),
            diagnose,
            check_cycles: read("CHECK_CYCLES").is_some(),
        }
    }

    /// Whether the cycle count of `T` is checked on this run.
    fn checks_cycles<T: CpuUnderTest>(&self) -> bool {
        T::CYCLES == CycleCheck::Always || self.check_cycles
    }
}

/// How many cases passed, failed, panicked and were skipped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub passed: usize,
    pub failed: usize,
    pub panicked: usize,
    pub skipped: usize,
}

impl Counts {
    /// Every case looked at.
    pub fn total(&self) -> usize {
        self.passed + self.failed + self.panicked + self.skipped
    }

    /// The cases that failed or panicked.
    pub fn failures(&self) -> usize {
        self.failed + self.panicked
    }

    /// The share that passed, in whole percent, of the cases not skipped;
    /// 0 when there are none.
    pub fn pass_percent(&self) -> usize {
        (self.passed * 100)
            .checked_div(self.passed + self.failed + self.panicked)
            .unwrap_or(0)
    }

    /// `P/T pass (N%) - failed=F panicked=X`, and ` skipped=S` when a case
    /// was skipped.
    pub fn line(&self) -> String {
        let mut line = format!(
            "{}/{} pass ({}%) - failed={} panicked={}",
            self.passed,
            self.total(),
            self.pass_percent(),
            self.failed,
            self.panicked
        );
        if self.skipped > 0 {
            line.push_str(&format!(" skipped={}", self.skipped));
        }
        line
    }

    fn add(&mut self, other: &Counts) {
        self.passed += other.passed;
        self.failed += other.failed;
        self.panicked += other.panicked;
        self.skipped += other.skipped;
    }
}

/// What became of the cases of one file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileResult {
    /// The file's name in the report.
    pub label: String,
    pub counts: Counts,
    /// The first case that failed or panicked: its index and what differs
    /// (`PANIC: ...` for a panic).
    pub first_failure: Option<(usize, Vec<String>)>,
    /// Every case that failed or panicked, by index.
    pub failing_cases: Vec<usize>,
    /// Every case skipped: its index, its name and what is wrong in its
    /// vector.
    pub skipped_cases: Vec<(usize, String, &'static str)>,
}

impl FileResult {
    fn new(label: &str) -> Self {
        Self {
            label: label.to_string(),
            ..Self::default()
        }
    }

    fn record(&mut self, index: usize, name: &str, outcome: CaseResult) {
        match outcome {
            CaseResult::Passed => self.counts.passed += 1,
            CaseResult::Failed(lines) => {
                self.counts.failed += 1;
                self.note_failure(index, lines);
            }
            CaseResult::Panicked(message) => {
                self.counts.panicked += 1;
                self.note_failure(index, vec![format!("PANIC: {message}")]);
            }
            CaseResult::Skipped(why) => {
                self.counts.skipped += 1;
                self.skipped_cases.push((index, name.to_string(), why));
            }
        }
    }

    fn note_failure(&mut self, index: usize, lines: Vec<String>) {
        self.failing_cases.push(index);
        if self.first_failure.is_none() {
            self.first_failure = Some((index, lines));
        }
    }
}

/// What became of the files of a sweep.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SweepResult {
    /// The cases run of each file (`0`: every case).
    pub max_cases: usize,
    /// What one file holds the cases of ([`CpuUnderTest::FILE_KIND`]).
    pub file_kind: &'static str,
    pub files: Vec<FileResult>,
}

/// The buckets of a sweep's histogram: files by their failing cases.
const BUCKETS: [&str; 6] = ["0", "1-10", "11-50", "51-500", "501-999", "1000+"];

impl SweepResult {
    /// The counts of every file together.
    pub fn totals(&self) -> Counts {
        let mut totals = Counts::default();
        for file in &self.files {
            totals.add(&file.counts);
        }
        totals
    }

    /// How many files fall in each bucket of failing cases (`0`, `1-10`,
    /// `11-50`, `51-500`, `501-999`, `1000+`).
    pub fn histogram(&self) -> [(&'static str, usize); 6] {
        let mut histogram = BUCKETS.map(|bucket| (bucket, 0));
        for file in &self.files {
            histogram[bucket_of(file.counts.failures())].1 += 1;
        }
        histogram
    }

    /// The counts of each group of files, `group` naming a file's group
    /// from its label, in the groups' order by name.
    pub fn groups(&self, group: fn(&str) -> String) -> Vec<(String, Counts)> {
        let mut groups: BTreeMap<String, Counts> = BTreeMap::new();
        for file in &self.files {
            groups
                .entry(group(&file.label))
                .or_default()
                .add(&file.counts);
        }
        groups.into_iter().collect()
    }
}

/// The bucket of [`SweepResult::histogram`] a file with `failures` failing
/// cases falls in.
fn bucket_of(failures: usize) -> usize {
    match failures {
        0 => 0,
        1..=10 => 1,
        11..=50 => 2,
        51..=500 => 3,
        501..=999 => 4,
        _ => 5,
    }
}

/// The vector files in `dir` (`*.json` and `*.json.gz`), each with its
/// name (the file's name less `.json` or `.json.gz`), in the order of
/// their paths; none when `dir` cannot be read.
pub fn vector_files(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(String, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter_map(|path| Some((vector_name(&path)?, path)))
        .collect();
    files.sort_by(|a, b| a.1.cmp(&b.1));
    files
}

/// The name of a vector file, or `None` for a file that is not one.
fn vector_name(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name
        .strip_suffix(".json.gz")
        .or_else(|| name.strip_suffix(".json"))?;
    Some(stem.to_string())
}

/// The cases of one vector file, gunzipped first when its name ends in
/// `.gz`.
pub fn load_cases<C: DeserializeOwned>(path: &Path) -> Result<Vec<C>, String> {
    let mut bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    if path.extension().is_some_and(|extension| extension == "gz") {
        let mut json = Vec::new();
        flate2::read::GzDecoder::new(bytes.as_slice())
            .read_to_end(&mut json)
            .map_err(|error| error.to_string())?;
        bytes = json;
    }
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

/// Run one case: skipped when its vector is named defective, its panic
/// caught.
pub fn run_case<T: CpuUnderTest>(case: &T::Case, options: &RunOptions) -> CaseResult {
    let name = T::case_name(case);
    if let Some((_, why)) = T::DEFECTIVE
        .iter()
        .find(|(defective, _)| *defective == name)
    {
        return CaseResult::Skipped(why);
    }
    match panic::catch_unwind(AssertUnwindSafe(|| {
        let mut cpu = T::prepare(case);
        let cycles = cpu.execute(case);
        differences(&mut cpu, case, cycles, options)
    })) {
        Ok(lines) if lines.is_empty() => CaseResult::Passed,
        Ok(lines) => CaseResult::Failed(lines),
        Err(payload) => CaseResult::Panicked(panic_message(payload)),
    }
}

/// What differs after a case's instruction ran: the CPU's lines, then the
/// cycle count's when it is checked.
fn differences<T: CpuUnderTest>(
    cpu: &mut T,
    case: &T::Case,
    cycles: u32,
    options: &RunOptions,
) -> Vec<String> {
    let mut lines = cpu.differences(case);
    let expected = T::expected_cycles(case);
    if options.checks_cycles::<T>() && cycles != expected {
        lines.push(format!("Cycles: expected {expected}, got {cycles}"));
    }
    lines
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&str>() {
        message.to_string()
    } else {
        "Unknown panic".to_string()
    }
}

/// Run the cases of one file, as `options` say, under `label`; diagnose
/// the failing cases they ask for.
pub fn run_cases<T: CpuUnderTest>(
    cases: &[T::Case],
    label: &str,
    options: &RunOptions,
) -> FileResult {
    let mut result = FileResult::new(label);
    if let Some(index) = options.only_case {
        let Some(case) = cases.get(index) else {
            result.counts.failed = 1;
            result.note_failure(index, vec![format!("index {index} >= {}", cases.len())]);
            return result;
        };
        diagnose::<T>(case, options);
        result.record(index, T::case_name(case), run_case::<T>(case, options));
        return result;
    }

    let limit = match options.max_cases {
        0 => cases.len(),
        max => cases.len().min(max),
    };
    for (index, case) in cases.iter().enumerate().take(limit) {
        result.record(index, T::case_name(case), run_case::<T>(case, options));
    }

    match options.diagnose {
        Diagnose::No => {}
        Diagnose::First => {
            if let Some(&index) = result.failing_cases.first() {
                println!("\n>>> first failure at index {index} of {label}");
                diagnose::<T>(&cases[index], options);
            }
        }
        Diagnose::Failures(most) => {
            let failing = &result.failing_cases;
            for (count, &index) in failing.iter().take(most).enumerate() {
                println!(
                    "\n>>> failure {} of {} at index {index} of {label}",
                    count + 1,
                    failing.len()
                );
                diagnose::<T>(&cases[index], options);
            }
            if failing.len() > most {
                println!("\n... {} more failures not diagnosed", failing.len() - most);
            }
        }
    }
    result
}

/// Read one vector file and run its cases ([`run_cases`]). A file that
/// cannot be read is one panicked case of one.
pub fn run_file<T: CpuUnderTest>(path: &Path, label: &str, options: &RunOptions) -> FileResult {
    match load_cases::<T::Case>(path) {
        Ok(cases) => run_cases::<T>(&cases, label, options),
        Err(error) => {
            let mut result = FileResult::new(label);
            result.counts.panicked = 1;
            result.first_failure =
                Some((0, vec![format!("cannot read {}: {error}", path.display())]));
            result
        }
    }
}

/// Run every file of `files` (each with its label), reporting each file
/// that has a failing case as it ends, then the sweep's report
/// ([`print_sweep`]); `group`, when given, names a file's group from its
/// label for the report's lines per group.
pub fn run_sweep<T: CpuUnderTest>(
    title: &str,
    files: &[(String, PathBuf)],
    options: &RunOptions,
    group: Option<fn(&str) -> String>,
) -> SweepResult {
    println!(
        "Sweep '{title}': {} files, max_cases={}",
        files.len(),
        options.max_cases
    );
    println!("{}", "=".repeat(70));
    let mut sweep = SweepResult {
        max_cases: options.max_cases,
        file_kind: T::FILE_KIND,
        files: Vec::with_capacity(files.len()),
    };
    for (label, path) in files {
        let result = run_file::<T>(path, label, options);
        if result.counts.failures() > 0 {
            print_file_result(&result);
        }
        sweep.files.push(result);
    }
    print_sweep(&sweep, group, WORST_FILES);
    sweep
}

/// Print a case's diagnosis: its name, the CPU's states
/// ([`CpuUnderTest::describe`]), the cycles and what differs.
fn diagnose<T: CpuUnderTest>(case: &T::Case, options: &RunOptions) {
    println!("\n{}", "=".repeat(70));
    println!("DIAGNOSE: {}", T::case_name(case));
    println!("{}", "=".repeat(70));
    let run = panic::catch_unwind(AssertUnwindSafe(|| {
        let mut cpu = T::prepare(case);
        let cycles = cpu.execute(case);
        let states = cpu.describe(case);
        let lines = differences(&mut cpu, case, cycles, options);
        (states, cycles, lines)
    }));
    match run {
        Ok((states, cycles, lines)) => {
            print!("{states}");
            println!("\nCycles: {cycles} (expected {})", T::expected_cycles(case));
            println!("\n--- DIFF ---");
            if lines.is_empty() {
                println!("  (no differences - case PASSES)");
            }
            for line in &lines {
                println!("  {line}");
            }
        }
        Err(payload) => println!("\n--- PANIC ---\n  {}", panic_message(payload)),
    }
}

/// Print one file's line (`LABEL: P/T pass (N%) - failed=F panicked=X`,
/// ` skipped=S` after it when a case was skipped), each case skipped, and
/// the first lines of its first failure.
pub fn print_file_result(result: &FileResult) {
    println!("{}: {}", result.label, result.counts.line());
    for (index, name, why) in &result.skipped_cases {
        println!("  skipped case {index} ({name}): the vector {why}");
    }
    if let Some((index, lines)) = &result.first_failure {
        if !lines.is_empty() {
            println!("  first failure at index {index}:");
            for line in lines.iter().take(FIRST_FAILURE_LINES) {
                println!("    {line}");
            }
        }
    }
}

/// Print a sweep's report: every case together, the lines of each group
/// when `group` names them, the histogram of the files by their failing
/// cases, and the `top` files with the most.
pub fn print_sweep(sweep: &SweepResult, group: Option<fn(&str) -> String>, top: usize) {
    println!("\n{}", "=".repeat(70));
    println!("OVERALL: {}", sweep.totals().line());
    if let Some(group) = group {
        println!("\nPer group:");
        for (name, counts) in sweep.groups(group) {
            println!("  {name}: {}", counts.line());
        }
    }

    if sweep.max_cases == 0 {
        println!("\nHistogram (files by failure count, every case of each file):");
    } else {
        println!(
            "\nHistogram (files by failure count, out of {} per file):",
            sweep.max_cases
        );
    }
    for (bucket, files) in sweep.histogram() {
        println!("  {bucket:>8}: {files}");
    }

    let mut worst: Vec<&FileResult> = sweep
        .files
        .iter()
        .filter(|file| file.counts.failures() > 0)
        .collect();
    worst.sort_by_key(|file| Reverse(file.counts.failures()));
    println!("\nTop failing {} (up to {top}):", sweep.file_kind);
    for file in worst.iter().take(top) {
        println!(
            "  {}: failed={} panicked={} (passed={} of {})",
            file.label,
            file.counts.failed,
            file.counts.panicked,
            file.counts.passed,
            file.counts.total()
        );
    }
}

/// RAM of a power-of-two size with every address masked to it: the memory
/// of a CPU whose address space is flat, as a case gives it.
pub struct FlatMemory {
    bytes: Vec<u8>,
}

impl FlatMemory {
    /// `size` bytes of zeros; `size` must be a power of two.
    pub fn new(size: usize) -> Self {
        assert!(size.is_power_of_two(), "a flat memory of {size} bytes");
        Self {
            bytes: vec![0; size],
        }
    }

    /// Write each `(address, value)` of `entries`.
    pub fn load(&mut self, entries: impl IntoIterator<Item = (u32, u8)>) {
        for (address, value) in entries {
            self.set(address, value);
        }
    }

    /// The byte at `address`.
    pub fn byte(&self, address: u32) -> u8 {
        self.bytes[address as usize & (self.bytes.len() - 1)]
    }

    /// Write `value` at `address`.
    pub fn set(&mut self, address: u32, value: u8) {
        let mask = self.bytes.len() - 1;
        self.bytes[address as usize & mask] = value;
    }
}

impl Addressable for FlatMemory {
    fn read(&mut self, address: u32) -> u8 {
        self.byte(address)
    }

    fn write(&mut self, address: u32, value: u8) {
        self.set(address, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::collections::HashMap;

    /// A CPU of one register that adds the case's operand to it, panicking
    /// on an operand of 255.
    struct Adder {
        a: u8,
        memory: FlatMemory,
    }

    #[derive(Deserialize)]
    struct AdderCase {
        name: String,
        a: u8,
        operand: u8,
        #[serde(rename = "final")]
        final_a: u8,
        cycles: u32,
    }

    /// The adder with its cycles checked on request; `AlwaysAdder` with
    /// them checked always.
    impl CpuUnderTest for Adder {
        type Case = AdderCase;
        const CYCLES: CycleCheck = CycleCheck::OnRequest;
        const FILE_KIND: &'static str = "opcodes";
        const DEFECTIVE: &'static [(&'static str, &'static str)] = &[("bad", "is wrong")];

        fn case_name(case: &AdderCase) -> &str {
            &case.name
        }
        fn prepare(case: &AdderCase) -> Self {
            let mut memory = FlatMemory::new(16);
            memory.load([(0, case.operand)]);
            Self { a: case.a, memory }
        }
        fn execute(&mut self, _: &AdderCase) -> u32 {
            let operand = self.memory.read(0);
            assert!(operand != 255, "operand 255");
            self.a = self.a.wrapping_add(operand);
            2
        }
        fn differences(&mut self, case: &AdderCase) -> Vec<String> {
            if self.a == case.final_a {
                Vec::new()
            } else {
                vec![format!(
                    "A: expected {:02X}, got {:02X}",
                    case.final_a, self.a
                )]
            }
        }
        fn expected_cycles(case: &AdderCase) -> u32 {
            case.cycles
        }
        fn describe(&mut self, _: &AdderCase) -> String {
            format!("A={:02X}\n", self.a)
        }
    }

    struct AlwaysAdder(Adder);

    impl CpuUnderTest for AlwaysAdder {
        type Case = AdderCase;
        const CYCLES: CycleCheck = CycleCheck::Always;
        const FILE_KIND: &'static str = "opcodes";

        fn case_name(case: &AdderCase) -> &str {
            &case.name
        }
        fn prepare(case: &AdderCase) -> Self {
            Self(Adder::prepare(case))
        }
        fn execute(&mut self, case: &AdderCase) -> u32 {
            self.0.execute(case)
        }
        fn differences(&mut self, case: &AdderCase) -> Vec<String> {
            self.0.differences(case)
        }
        fn expected_cycles(case: &AdderCase) -> u32 {
            case.cycles
        }
        fn describe(&mut self, case: &AdderCase) -> String {
            self.0.describe(case)
        }
    }

    fn case(name: &str, a: u8, operand: u8, final_a: u8, cycles: u32) -> AdderCase {
        AdderCase {
            name: name.to_string(),
            a,
            operand,
            final_a,
            cycles,
        }
    }

    #[test]
    fn a_case_passes_fails_panics_or_is_skipped() {
        let options = RunOptions::default();
        assert_eq!(
            run_case::<Adder>(&case("ok", 1, 2, 3, 2), &options),
            CaseResult::Passed
        );
        assert_eq!(
            run_case::<Adder>(&case("off", 1, 2, 4, 2), &options),
            CaseResult::Failed(vec!["A: expected 04, got 03".to_string()])
        );
        assert_eq!(
            run_case::<Adder>(&case("boom", 1, 255, 0, 2), &options),
            CaseResult::Panicked("operand 255".to_string())
        );
        assert_eq!(
            run_case::<Adder>(&case("bad", 1, 2, 9, 2), &options),
            CaseResult::Skipped("is wrong")
        );
    }

    #[test]
    fn the_cycle_count_is_checked_as_the_cpu_asks() {
        let slow = case("slow", 1, 2, 3, 5);
        let checked = RunOptions {
            check_cycles: true,
            ..RunOptions::default()
        };
        assert_eq!(
            run_case::<Adder>(&slow, &RunOptions::default()),
            CaseResult::Passed
        );
        let cycles = vec!["Cycles: expected 5, got 2".to_string()];
        assert_eq!(
            run_case::<Adder>(&slow, &checked),
            CaseResult::Failed(cycles.clone())
        );
        assert_eq!(
            run_case::<AlwaysAdder>(&slow, &RunOptions::default()),
            CaseResult::Failed(cycles)
        );
        assert_eq!(
            run_case::<AlwaysAdder>(&case("off", 1, 2, 4, 5), &RunOptions::default()),
            CaseResult::Failed(vec![
                "A: expected 04, got 03".to_string(),
                "Cycles: expected 5, got 2".to_string(),
            ])
        );
    }

    #[test]
    fn a_file_counts_its_cases_and_keeps_its_first_failure() {
        let cases = [
            case("ok", 1, 2, 3, 2),
            case("bad", 1, 2, 9, 2),
            case("off", 1, 2, 4, 2),
            case("boom", 1, 255, 0, 2),
            case("ok2", 0, 0, 0, 2),
        ];
        let result = run_cases::<Adder>(&cases, "01", &RunOptions::default());
        assert_eq!(
            result.counts,
            Counts {
                passed: 2,
                failed: 1,
                panicked: 1,
                skipped: 1
            }
        );
        assert_eq!(
            result.first_failure,
            Some((2, vec!["A: expected 04, got 03".to_string()]))
        );
        assert_eq!(result.failing_cases, vec![2, 3]);
        assert_eq!(
            result.skipped_cases,
            vec![(1, "bad".to_string(), "is wrong")]
        );
        assert_eq!(
            result.counts.line(),
            "2/5 pass (50%) - failed=1 panicked=1 skipped=1"
        );

        let first_two = RunOptions {
            max_cases: 2,
            ..RunOptions::default()
        };
        let result = run_cases::<Adder>(&cases, "01", &first_two);
        assert_eq!(
            result.counts.line(),
            "1/2 pass (100%) - failed=0 panicked=0 skipped=1"
        );

        let fourth = RunOptions {
            only_case: Some(3),
            ..RunOptions::default()
        };
        let result = run_cases::<Adder>(&cases, "01", &fourth);
        assert_eq!(result.counts.line(), "0/1 pass (0%) - failed=0 panicked=1");
        assert_eq!(
            result.first_failure,
            Some((3, vec!["PANIC: operand 255".to_string()]))
        );

        let past = RunOptions {
            only_case: Some(5),
            ..RunOptions::default()
        };
        let result = run_cases::<Adder>(&cases, "01", &past);
        assert_eq!(result.counts.line(), "0/1 pass (0%) - failed=1 panicked=0");
        assert_eq!(
            result.first_failure,
            Some((5, vec!["index 5 >= 5".to_string()]))
        );
    }

    #[test]
    fn the_line_leaves_the_skipped_cases_out_of_the_rate() {
        let counts = Counts {
            passed: 6190,
            failed: 1874,
            panicked: 0,
            skipped: 1,
        };
        assert_eq!(
            counts.line(),
            "6190/8065 pass (76%) - failed=1874 panicked=0 skipped=1"
        );
        assert_eq!(
            Counts::default().line(),
            "0/0 pass (0%) - failed=0 panicked=0"
        );
    }

    #[test]
    fn the_switches_are_read_with_their_fallback() {
        let vars: HashMap<&str, &str> = [
            ("SINGLESTEP_INDEX", "7"),
            ("HARTE_INDEX", "9"),
            ("HARTE_DIAGNOSE", "1"),
            ("HARTE_CHECK_CYCLES", ""),
        ]
        .into_iter()
        .collect();
        let var = |name: &str| vars.get(name).map(|value| value.to_string());
        assert_eq!(
            RunOptions::from_vars(10, None, var),
            RunOptions {
                max_cases: 10,
                only_case: Some(7),
                diagnose: Diagnose::No,
                check_cycles: false,
            }
        );
        assert_eq!(
            RunOptions::from_vars(0, Some("HARTE"), var),
            RunOptions {
                max_cases: 0,
                only_case: Some(7),
                diagnose: Diagnose::First,
                check_cycles: true,
            }
        );
        let full = |name: &str| {
            (name == "SINGLESTEP_FULL_DIAG" || name == "SINGLESTEP_DIAGNOSE").then(String::new)
        };
        assert_eq!(
            RunOptions::from_vars(0, None, full).diagnose,
            Diagnose::Failures(FULL_DIAGNOSIS_CASES)
        );
    }

    #[test]
    fn plain_and_gzipped_files_are_listed_and_read() {
        use flate2::write::GzEncoder;
        use std::io::Write;

        let dir =
            std::env::temp_dir().join(format!("retro-core-singlestep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let json = br#"[{"name": "ok", "a": 1, "operand": 2, "final": 3, "cycles": 2}]"#;
        std::fs::write(dir.join("b0.json"), json).unwrap();
        let mut gz = GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(json).unwrap();
        std::fs::write(dir.join("a1.json.gz"), gz.finish().unwrap()).unwrap();
        std::fs::write(dir.join("notes.txt"), "not a vector file").unwrap();

        let files = vector_files(&dir);
        let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["a1", "b0"]);
        for (name, path) in &files {
            let cases = load_cases::<AdderCase>(path).unwrap();
            assert_eq!(cases.len(), 1, "{name}");
            assert_eq!(cases[0].final_a, 3);
            let result = run_file::<Adder>(path, name, &RunOptions::default());
            assert_eq!(
                result.counts.line(),
                "1/1 pass (100%) - failed=0 panicked=0"
            );
        }

        let broken = dir.join("broken.json");
        std::fs::write(&broken, "[{").unwrap();
        let result = run_file::<Adder>(&broken, "broken", &RunOptions::default());
        assert_eq!(result.counts.line(), "0/1 pass (0%) - failed=0 panicked=1");
        assert!(result.first_failure.unwrap().1[0].starts_with("cannot read "));

        std::fs::remove_dir_all(&dir).unwrap();
        assert!(vector_files(&dir).is_empty());
    }

    #[test]
    fn a_sweep_adds_its_files_and_buckets_them() {
        let file = |label: &str, failed: usize| FileResult {
            label: label.to_string(),
            counts: Counts {
                passed: 1000 - failed,
                failed,
                ..Counts::default()
            },
            ..FileResult::default()
        };
        let sweep = SweepResult {
            max_cases: 1000,
            file_kind: "opcodes",
            files: vec![
                file("00", 0),
                file("CB 01", 10),
                file("CB 02", 11),
                file("DD 03", 500),
                file("DD 04", 999),
                file("DD 05", 1000),
            ],
        };
        assert_eq!(
            sweep.histogram(),
            [
                ("0", 1),
                ("1-10", 1),
                ("11-50", 1),
                ("51-500", 1),
                ("501-999", 1),
                ("1000+", 1)
            ]
        );
        assert_eq!(
            sweep.totals().line(),
            "3480/6000 pass (58%) - failed=2520 panicked=0"
        );
        let prefix = |label: &str| match label.split_once(' ') {
            Some((prefix, _)) => prefix.to_string(),
            None => "Unprefixed".to_string(),
        };
        let groups: Vec<(String, usize)> = sweep
            .groups(prefix)
            .into_iter()
            .map(|(name, counts)| (name, counts.failed))
            .collect();
        assert_eq!(
            groups,
            [
                ("CB".to_string(), 21),
                ("DD".to_string(), 2499),
                ("Unprefixed".to_string(), 0)
            ]
        );
    }

    #[test]
    fn a_flat_memory_masks_its_addresses() {
        let mut memory = FlatMemory::new(0x100);
        memory.load([(0x1_0010, 0xAB), (0x20, 0xCD)]);
        assert_eq!(memory.byte(0x10), 0xAB);
        assert_eq!(memory.read(0x120), 0xCD);
        memory.write(0x2FF, 0xEF);
        assert_eq!(memory.byte(0xFF), 0xEF);
    }
}
