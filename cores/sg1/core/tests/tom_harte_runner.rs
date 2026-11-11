//! Tom Harte 68000 Test Suite Runner
//!
//! Runs the comprehensive ProcessorTests test suite to validate CPU implementation.
//!
//! ## Usage
//!
//! Run all tests for an instruction:
//! ```bash
//! cargo test --package sg1-core --test tom_harte_runner test_harte_add_b -- --ignored --nocapture
//! ```
//!
//! Run with detailed diagnostics for first failure:
//! ```bash
//! HARTE_DIAGNOSE=1 cargo test --package sg1-core --test tom_harte_runner test_harte_add_b -- --ignored --nocapture
//! ```
//!
//! Run a specific test by index:
//! ```bash
//! HARTE_INDEX=42 cargo test --package sg1-core --test tom_harte_runner test_harte_add_b -- --ignored --nocapture
//! ```

mod tom_harte;

use retro_core::cpu::{Addressable, CyclesTrackable};
use sg1_core::cpu68k::M68k;
use std::collections::HashMap;
use std::path::PathBuf;
use tom_harte::{find_test_files, load_test_file, HarteTest};

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
    memory: Vec<u8>,
    cycles_executed: u64,
}

impl TestBus {
    fn new() -> Self {
        Self {
            memory: vec![0; 0x1000000], // 16MB
            cycles_executed: 0,
        }
    }

    fn read_byte(&mut self, addr: u32) -> u8 {
        let addr = (addr & 0xFFFFFF) as usize;
        self.memory.get(addr).copied().unwrap_or(0)
    }

    fn write_byte(&mut self, addr: u32, value: u8) {
        let addr = (addr & 0xFFFFFF) as usize;
        if addr < self.memory.len() {
            self.memory[addr] = value;
        }
    }
}

impl Addressable for TestBus {
    fn read(&mut self, addr: u32) -> u8 {
        self.cycles_executed += 4;

        self.read_byte(addr)
    }

    fn write(&mut self, addr: u32, value: u8) {
        self.cycles_executed += 4;

        self.write_byte(addr, value);
    }

    fn read_word(&mut self, addr: u32) -> u16 {
        self.cycles_executed += 4;
        let high = self.read_byte(addr) as u16;
        let low = self.read_byte(addr.wrapping_add(1)) as u16;
        (high << 8) | low // Big Endian
    }

    fn write_word(&mut self, addr: u32, value: u16) {
        self.cycles_executed += 4;
        self.write_byte(addr, (value >> 8) as u8);
        self.write_byte(addr.wrapping_add(1), (value & 0xFF) as u8);
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

/// Apply initial state to CPU and bus
fn apply_initial_state(cpu: &mut M68k, bus: &mut TestBus, state: &tom_harte::CpuState) {
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

/// Check if final state matches expected
fn check_final_state(
    cpu: &M68k,
    bus: &mut TestBus,
    expected: &tom_harte::CpuState,
) -> Result<(), String> {
    let mut errors = Vec::new();

    // Check data registers
    for i in 0..8 {
        let expected_val = match i {
            0 => expected.d0,
            1 => expected.d1,
            2 => expected.d2,
            3 => expected.d3,
            4 => expected.d4,
            5 => expected.d5,
            6 => expected.d6,
            7 => expected.d7,
            _ => unreachable!(),
        };
        if cpu.d[i] != expected_val {
            errors.push(format!(
                "D{}: expected {:08X}, got {:08X}",
                i, expected_val, cpu.d[i]
            ));
        }
    }

    // Check address registers (excluding A7/SSP which we check separately)
    for i in 0..7 {
        let expected_val = match i {
            0 => expected.a0,
            1 => expected.a1,
            2 => expected.a2,
            3 => expected.a3,
            4 => expected.a4,
            5 => expected.a5,
            6 => expected.a6,
            _ => unreachable!(),
        };
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
        let actual_val = bus.read(translated_addr);
        if actual_val != *expected_val {
            errors.push(format!(
                "RAM[{:08X}]: expected {:02X}, got {:02X}",
                addr, expected_val, actual_val
            ));
        }
    }

    //print all errors (temp code)
    for error in errors.clone() {
        println!("{}", error);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

/// Run a single test
fn run_test(test: &HarteTest) -> Result<(), String> {
    let mut cpu = M68k::new();
    let mut bus = TestBus::new();

    // Apply initial state
    apply_initial_state(&mut cpu, &mut bus, &test.initial);

    println!("@@@@@@@@@@@@@@@ Initing instruction");

    bus.cycles_executed = 0;

    // Execute one instruction
    let actual_cycles = cpu.step(&mut bus);

    println!("@@@@@@@@@@@@@@@ Executed instruction");

    // Check final state
    check_final_state(&cpu, &mut bus, &test.final_state)?;

    // Check cycle count (optional - enable with HARTE_CHECK_CYCLES=1)
    if std::env::var("HARTE_CHECK_CYCLES").is_ok() && actual_cycles != test.cycles {
        return Err(format!(
            "Cycle count mismatch: expected {}, got {}",
            test.cycles, actual_cycles
        ));
    }

    Ok(())
}

/// Run a single test with detailed diagnostics
fn run_test_with_diagnostics(test: &HarteTest) {
    let mut cpu = M68k::new();
    let mut bus = TestBus::new();

    // Apply initial state
    apply_initial_state(&mut cpu, &mut bus, &test.initial);

    // Capture initial state for comparison
    let initial_d = cpu.d;
    let initial_a = cpu.a;
    let initial_sr = cpu.sr;
    let initial_pc = cpu.pc;
    let initial_usp = cpu.usp;
    let initial_ssp = cpu.ssp;

    println!("\n{}", "=".repeat(70));
    println!("TEST: {}", test.name);
    println!("{}", "=".repeat(70));

    // Print opcode info
    println!("\n--- OPCODE ---");
    let opcode = test.initial.prefetch[0];
    println!("Prefetch[0]: {:04X} (instruction)", opcode);
    println!("Prefetch[1]: {:04X} (extension)", test.initial.prefetch[1]);

    // Decode addressing modes for MOVE instructions
    if (opcode & 0xC000) == 0x0000 && (opcode & 0x3000) != 0x0000 {
        // MOVE instruction (bits 15-14 = 00, bits 13-12 != 00)
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

        println!("  Size: {}", size_name);
        println!(
            "  Source EA: mode={} reg={} ({})",
            src_mode,
            src_reg,
            decode_ea_name(src_mode, src_reg)
        );
        println!(
            "  Dest EA:   mode={} reg={} ({})",
            dst_mode,
            dst_reg,
            decode_ea_name(dst_mode, dst_reg)
        );
    }

    // Print initial state
    println!("\n--- INITIAL STATE ---");
    print_cpu_state(
        "Initial",
        &initial_d,
        &initial_a,
        initial_sr,
        initial_pc,
        initial_usp,
        initial_ssp,
    );

    // Print initial RAM
    if !test.initial.ram.is_empty() {
        println!("\nInitial RAM:");
        for (addr, val) in &test.initial.ram {
            println!("  [{:08X}] = {:02X}", addr, val);
        }
    }

    println!("@@@@@@@@@@@@@@@ Initing instruction with diagnostics");

    bus.cycles_executed = 0;

    // Execute one instruction
    let cycles = cpu.step(&mut bus);
    println!("@@@@@@@@@@@@@@@ Executed instruction with diagnostics");

    // Print actual state after execution
    println!("\n--- ACTUAL STATE (after execution) ---");
    print_cpu_state("Actual", &cpu.d, &cpu.a, cpu.sr, cpu.pc, cpu.usp, cpu.ssp);
    println!("Cycles: {} (expected: {})", cycles, test.cycles);

    // Print expected state
    println!("\n--- EXPECTED STATE ---");
    let expected = &test.final_state;
    let exp_d = expected.data_regs();
    let exp_a = expected.addr_regs();
    print_cpu_state(
        "Expected",
        &exp_d,
        &exp_a,
        expected.sr,
        expected.pc,
        expected.usp,
        expected.ssp,
    );

    // Print differences
    println!("\n--- DIFFERENCES ---");
    let mut has_diff = false;

    for (i, exp) in exp_d.iter().enumerate() {
        if cpu.d[i] != *exp {
            println!(
                "D{}: expected {:08X}, got {:08X} (diff: {:08X})",
                i,
                exp,
                cpu.d[i],
                exp ^ cpu.d[i]
            );
            has_diff = true;
        }
    }

    for (i, exp) in exp_a.iter().enumerate() {
        if cpu.a[i] != *exp {
            println!(
                "A{}: expected {:08X}, got {:08X} (diff: {:08X})",
                i,
                exp,
                cpu.a[i],
                exp ^ cpu.a[i]
            );
            has_diff = true;
        }
    }

    if cpu.ssp != expected.ssp {
        println!("SSP: expected {:08X}, got {:08X}", expected.ssp, cpu.ssp);
        has_diff = true;
    }

    if cpu.usp != expected.usp {
        println!("USP: expected {:08X}, got {:08X}", expected.usp, cpu.usp);
        has_diff = true;
    }

    let expected_pc = expected.pc;
    if cpu.pc != expected_pc {
        println!("PC: expected {:08X}, got {:08X}", expected_pc, cpu.pc);
        has_diff = true;
    }

    if cpu.sr != expected.sr {
        println!("SR: expected {:04X}, got {:04X}", expected.sr, cpu.sr);
        print_sr_diff(expected.sr, cpu.sr);
        has_diff = true;
    }

    // Check RAM differences
    for (addr, expected_val) in &expected.ram {
        let translated_addr = translate_address(*addr);
        let actual_val = bus.read(translated_addr);
        if actual_val != *expected_val {
            println!(
                "RAM[{:08X}]: expected {:02X}, got {:02X}",
                addr, expected_val, actual_val
            );
            has_diff = true;
        }
    }

    if !has_diff {
        println!("(no differences - test PASSED)");
    }
    println!();
}

/// Print CPU state in a formatted way
fn print_cpu_state(label: &str, d: &[u32; 8], a: &[u32; 8], sr: u16, pc: u32, usp: u32, ssp: u32) {
    println!("{}:", label);
    println!(
        "  D0={:08X} D1={:08X} D2={:08X} D3={:08X}",
        d[0], d[1], d[2], d[3]
    );
    println!(
        "  D4={:08X} D5={:08X} D6={:08X} D7={:08X}",
        d[4], d[5], d[6], d[7]
    );
    println!(
        "  A0={:08X} A1={:08X} A2={:08X} A3={:08X}",
        a[0], a[1], a[2], a[3]
    );
    println!(
        "  A4={:08X} A5={:08X} A6={:08X} A7={:08X}",
        a[4], a[5], a[6], a[7]
    );
    println!(
        "  PC={:08X} SR={:04X} USP={:08X} SSP={:08X}",
        pc, sr, usp, ssp
    );
    print_sr_flags(sr);
}

/// Print SR flags in human readable format
fn print_sr_flags(sr: u16) {
    let t = (sr >> 15) & 1;
    let s = (sr >> 13) & 1;
    let i = (sr >> 8) & 7;
    let x = (sr >> 4) & 1;
    let n = (sr >> 3) & 1;
    let z = (sr >> 2) & 1;
    let v = (sr >> 1) & 1;
    let c = sr & 1;
    println!(
        "  Flags: T={} S={} I={} X={} N={} Z={} V={} C={}",
        t, s, i, x, n, z, v, c
    );
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

/// Print SR flag differences
fn print_sr_diff(expected: u16, actual: u16) {
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

    if !diffs.is_empty() {
        println!("  Flag diffs: {}", diffs.join(", "));
    }
}

/// Test runner for a specific instruction file
fn run_instruction_tests(instruction_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Check environment variables for special modes
    let diagnose_mode = std::env::var("HARTE_DIAGNOSE").is_ok();
    let full_diag_mode = std::env::var("HARTE_FULL_DIAG").is_ok();
    let specific_index: Option<usize> = std::env::var("HARTE_INDEX")
        .ok()
        .and_then(|s| s.parse().ok());

    // Find test file (the path is relative to this crate's folder, where cargo
    // runs a test, and leads to the workspace root's tests/)
    let repo_path = PathBuf::from("../../../tests/tom_harte_repo");
    let test_files = find_test_files(&repo_path);

    let test_file = test_files
        .iter()
        .find(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.starts_with(instruction_name))
                .unwrap_or(false)
        })
        .ok_or_else(|| format!("Test file for {} not found", instruction_name))?;

    println!("Loading tests from: {}", test_file.display());
    let tests = load_test_file(test_file)?;
    println!("Loaded {} tests for {}", tests.len(), instruction_name);

    // If specific index requested, run only that test with diagnostics
    if let Some(idx) = specific_index {
        if idx >= tests.len() {
            return Err(format!("Index {} out of range (max: {})", idx, tests.len() - 1).into());
        }
        println!("\n>>> Running single test at index {} <<<", idx);
        run_test_with_diagnostics(&tests[idx]);
        return match run_test(&tests[idx]) {
            Ok(_) => Ok(()),
            Err(e) => Err(e.into()),
        };
    }

    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut first_failure_idx: Option<usize> = None;
    let mut failure_opcodes: HashMap<u16, u32> = HashMap::new();
    let mut failure_indices: Vec<usize> = Vec::new();

    // For MOVE instruction analysis - track EA modes
    let mut failure_dest_modes: HashMap<u8, u32> = HashMap::new();
    let mut failure_src_modes: HashMap<u8, u32> = HashMap::new();

    for (i, test) in tests.iter().enumerate() {
        if let Some((_, defect)) = DEFECTIVE_VECTORS
            .iter()
            .find(|(name, _)| *name == test.name)
        {
            skipped += 1;
            println!(
                "  Test {}/{}: {} skipped: the vector {}",
                i + 1,
                tests.len(),
                test.name,
                defect
            );
            continue;
        }
        match run_test(test) {
            Ok(_) => {
                passed += 1;
                if i < 5 || i % 1000 == 0 {
                    println!("  PASS Test {}/{}: {}", i + 1, tests.len(), test.name);
                }
            }
            Err(_e) => {
                failed += 1;

                // Track first failure
                if first_failure_idx.is_none() {
                    first_failure_idx = Some(i);
                }

                // Track all failure indices
                failure_indices.push(i);

                // Track failing opcodes
                let opcode = test.initial.prefetch[0];
                *failure_opcodes.entry(opcode).or_insert(0) += 1;

                // For MOVE instructions, track EA modes
                if (opcode & 0xC000) == 0x0000 && (opcode & 0x3000) != 0x0000 {
                    let dst_mode = ((opcode >> 6) & 0x7) as u8;
                    // Source mode is bits 5-3, not bits 2-0
                    let src_mode = ((opcode >> 3) & 0x7) as u8;
                    *failure_dest_modes.entry(dst_mode).or_insert(0) += 1;
                    *failure_src_modes.entry(src_mode).or_insert(0) += 1;
                }

                // Print failures (limit output unless full diag mode)
                if failed <= 50 || full_diag_mode {
                    println!(
                        "  FAIL Test {}/{}: {} [opcode: {:04X}]",
                        i + 1,
                        tests.len(),
                        test.name,
                        opcode
                    );
                }
            }
        }
    }

    // Summary
    println!("\n{}", "=".repeat(60));
    println!(
        "{}: {} passed, {} failed ({}%)",
        instruction_name,
        passed,
        failed,
        (passed * 100) / (tests.len() - skipped).max(1)
    );
    if skipped > 0 {
        println!("  ({} defective vector(s) of the set skipped)", skipped);
    }

    // If there were failures, print analysis
    if failed > 0 {
        // Print most common failing opcodes
        println!("\n--- Most Common Failing Opcodes ---");
        let mut sorted_opcodes: Vec<_> = failure_opcodes.into_iter().collect();
        sorted_opcodes.sort_by_key(|e| std::cmp::Reverse(e.1));
        for (opcode, count) in sorted_opcodes.iter().take(50) {
            println!("  {:04X}: {} failures", opcode, count);
        }

        // For MOVE instructions, print EA mode analysis
        if !failure_dest_modes.is_empty() {
            println!(
                "\n--- MOVE Instruction EA Mode Analysis (ALL {} failures) ---",
                failed
            );
            println!("\nDestination EA Modes:");
            let mut sorted_dest: Vec<_> = failure_dest_modes.into_iter().collect();
            sorted_dest.sort_by_key(|e| std::cmp::Reverse(e.1));
            for (mode, count) in &sorted_dest {
                let mode_name = match mode {
                    0 => "Dn",
                    1 => "An",
                    2 => "(An)",
                    3 => "(An)+",
                    4 => "-(An)",
                    5 => "(d16,An)",
                    6 => "(d8,An,Xn)",
                    7 => "mode 7 (xxx/imm/PC)",
                    _ => "unknown",
                };
                let percentage = (*count as f64 / failed as f64) * 100.0;
                println!(
                    "  mode={} {}: {} failures ({:.1}%)",
                    mode, mode_name, count, percentage
                );
            }

            println!("\nSource EA Modes:");
            let mut sorted_src: Vec<_> = failure_src_modes.into_iter().collect();
            sorted_src.sort_by_key(|e| std::cmp::Reverse(e.1));
            for (mode, count) in &sorted_src {
                let mode_name = match mode {
                    0 => "Dn",
                    1 => "An",
                    2 => "(An)",
                    3 => "(An)+",
                    4 => "-(An)",
                    5 => "(d16,An)",
                    6 => "(d8,An,Xn)",
                    7 => "mode 7 (xxx/imm/PC)",
                    _ => "unknown",
                };
                let percentage = (*count as f64 / failed as f64) * 100.0;
                println!(
                    "  mode={} {}: {} failures ({:.1}%)",
                    mode, mode_name, count, percentage
                );
            }
        }

        // If full diagnostic mode, show the failures (the first 20)
        if full_diag_mode {
            println!(
                "\n>>> FULL DIAGNOSTICS MODE - Showing all {} failures <<<",
                failure_indices.len()
            );
            let limit = failure_indices.len().min(20); // Limit to first 20 for sanity
            for (count, &idx) in failure_indices.iter().take(limit).enumerate() {
                println!("\n{}", "=".repeat(70));
                println!("FAILURE {} of {} (test index {})", count + 1, limit, idx);
                println!("{}", "=".repeat(70));
                run_test_with_diagnostics(&tests[idx]);
            }
            if failure_indices.len() > 20 {
                println!(
                    "\n... {} more failures not shown (limited to 20)",
                    failure_indices.len() - 20
                );
            }
        }
        // If diagnose mode, show detailed diagnostics for first failure
        else if diagnose_mode {
            if let Some(idx) = first_failure_idx {
                println!(
                    "\n>>> DETAILED DIAGNOSTICS FOR FIRST FAILURE (index {}) <<<",
                    idx
                );
                run_test_with_diagnostics(&tests[idx]);
            }
        } else if let Some(idx) = first_failure_idx {
            println!("\nTIP: Run with HARTE_DIAGNOSE=1 for detailed first failure analysis");
            println!("TIP: Run with HARTE_FULL_DIAG=1 to see all failures (limited to 20)");
            println!("TIP: Run with HARTE_INDEX={} to test only that case", idx);
        }

        Err(format!("{} tests failed", failed).into())
    } else {
        Ok(())
    }
}

// Individual test for each instruction
// Start with simple instructions first

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_moveq() {
    run_instruction_tests("MOVE.q").expect("MOVE.q tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_move_b() {
    run_instruction_tests("MOVE.b").expect("MOVE.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_add_b() {
    run_instruction_tests("ADD.b").expect("ADD.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_nop() {
    run_instruction_tests("NOP").expect("NOP tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_clr_b() {
    run_instruction_tests("CLR.b").expect("CLR.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_add_l() {
    run_instruction_tests("ADD.l").expect("ADD.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_add_w() {
    run_instruction_tests("ADD.w").expect("ADD.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_adda_w() {
    run_instruction_tests("ADDA.w").expect("ADDA.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_adda_l() {
    run_instruction_tests("ADDA.l").expect("ADDA.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_addx_b() {
    run_instruction_tests("ADDX.b").expect("ADDX.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_addx_w() {
    run_instruction_tests("ADDX.w").expect("ADDX.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_addx_l() {
    run_instruction_tests("ADDX.l").expect("ADDX.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_move_w() {
    run_instruction_tests("MOVE.w").expect("MOVE.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_move_l() {
    run_instruction_tests("MOVE.l").expect("MOVE.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_movea_w() {
    run_instruction_tests("MOVEA.w").expect("MOVEA.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_movea_l() {
    run_instruction_tests("MOVEA.l").expect("MOVEA.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_movem_w() {
    run_instruction_tests("MOVEM.w").expect("MOVEM.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_movem_l() {
    run_instruction_tests("MOVEM.l").expect("MOVEM.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_movep_w() {
    run_instruction_tests("MOVEP.w").expect("MOVEP.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_movep_l() {
    run_instruction_tests("MOVEP.l").expect("MOVEP.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_clr_w() {
    run_instruction_tests("CLR.w").expect("CLR.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_clr_l() {
    run_instruction_tests("CLR.l").expect("CLR.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_exg() {
    run_instruction_tests("EXG").expect("EXG tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_swap() {
    run_instruction_tests("SWAP").expect("SWAP tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_link() {
    run_instruction_tests("LINK").expect("LINK tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_unlink() {
    run_instruction_tests("UNLINK").expect("UNLINK tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_sub_b() {
    run_instruction_tests("SUB.b").expect("SUB.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_sub_w() {
    run_instruction_tests("SUB.w").expect("SUB.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_sub_l() {
    run_instruction_tests("SUB.l").expect("SUB.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_suba_w() {
    run_instruction_tests("SUBA.w").expect("SUBA.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_suba_l() {
    run_instruction_tests("SUBA.l").expect("SUBA.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_subx_b() {
    run_instruction_tests("SUBX.b").expect("SUBX.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_subx_w() {
    run_instruction_tests("SUBX.w").expect("SUBX.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_subx_l() {
    run_instruction_tests("SUBX.l").expect("SUBX.l tests failed");
}

// ============================================================================
// CMP Family Tests
// ============================================================================

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_cmp_b() {
    run_instruction_tests("CMP.b").expect("CMP.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_cmp_w() {
    run_instruction_tests("CMP.w").expect("CMP.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_cmp_l() {
    run_instruction_tests("CMP.l").expect("CMP.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_cmpa_w() {
    run_instruction_tests("CMPA.w").expect("CMPA.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_cmpa_l() {
    run_instruction_tests("CMPA.l").expect("CMPA.l tests failed");
}

// ============================================================================
// NEG Family Tests
// ============================================================================

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_neg_b() {
    run_instruction_tests("NEG.b").expect("NEG.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_neg_w() {
    run_instruction_tests("NEG.w").expect("NEG.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_neg_l() {
    run_instruction_tests("NEG.l").expect("NEG.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_negx_b() {
    run_instruction_tests("NEGX.b").expect("NEGX.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_negx_w() {
    run_instruction_tests("NEGX.w").expect("NEGX.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_negx_l() {
    run_instruction_tests("NEGX.l").expect("NEGX.l tests failed");
}

// ============================================================================
// MUL/DIV Tests
// ============================================================================

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_mulu() {
    run_instruction_tests("MULU").expect("MULU tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_muls() {
    run_instruction_tests("MULS").expect("MULS tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_divu() {
    run_instruction_tests("DIVU").expect("DIVU tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_divs() {
    run_instruction_tests("DIVS").expect("DIVS tests failed");
}

// ============================================================================
// BCD Tests
// ============================================================================

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_abcd() {
    run_instruction_tests("ABCD").expect("ABCD tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_sbcd() {
    run_instruction_tests("SBCD").expect("SBCD tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_nbcd() {
    run_instruction_tests("NBCD").expect("NBCD tests failed");
}

// ============================================================================
// LEA / PEA Tests
// ============================================================================

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_lea() {
    run_instruction_tests("LEA").expect("LEA tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_pea() {
    run_instruction_tests("PEA").expect("PEA tests failed");
}

// ============================================================================
// Logical Family Tests
// ============================================================================
#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_and_b() {
    run_instruction_tests("AND.b").expect("AND.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_and_w() {
    run_instruction_tests("AND.w").expect("AND.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_and_l() {
    run_instruction_tests("AND.l").expect("AND.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_or_b() {
    run_instruction_tests("OR.b").expect("OR.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_or_w() {
    run_instruction_tests("OR.w").expect("OR.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_or_l() {
    run_instruction_tests("OR.l").expect("OR.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_eor_b() {
    run_instruction_tests("EOR.b").expect("EOR.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_eor_w() {
    run_instruction_tests("EOR.w").expect("EOR.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_eor_l() {
    run_instruction_tests("EOR.l").expect("EOR.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_not_b() {
    run_instruction_tests("NOT.b").expect("NOT.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_not_w() {
    run_instruction_tests("NOT.w").expect("NOT.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_not_l() {
    run_instruction_tests("NOT.l").expect("NOT.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_tst_b() {
    run_instruction_tests("TST.b").expect("TST.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_tst_w() {
    run_instruction_tests("TST.w").expect("TST.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_tst_l() {
    run_instruction_tests("TST.l").expect("TST.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_btst() {
    run_instruction_tests("BTST").expect("BTST tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_bset() {
    run_instruction_tests("BSET").expect("BSET tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_bclr() {
    run_instruction_tests("BCLR").expect("BCLR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_bchg() {
    run_instruction_tests("BCHG").expect("BCHG tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_tas() {
    run_instruction_tests("TAS").expect("TAS tests failed");
}

// ============================================================================
// Shifts Tests
// ============================================================================

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_lsl_b() {
    run_instruction_tests("LSL.b").expect("LSL.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_lsl_w() {
    run_instruction_tests("LSL.w").expect("LSL.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_lsl_l() {
    run_instruction_tests("LSL.l").expect("LSL.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_lsr_b() {
    run_instruction_tests("LSR.b").expect("LSR.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_lsr_w() {
    run_instruction_tests("LSR.w").expect("LSR.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_lsr_l() {
    run_instruction_tests("LSR.l").expect("LSR.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_asl_b() {
    run_instruction_tests("ASL.b").expect("ASL.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_asl_w() {
    run_instruction_tests("ASL.w").expect("ASL.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_asl_l() {
    run_instruction_tests("ASL.l").expect("ASL.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_asr_b() {
    run_instruction_tests("ASR.b").expect("ASR.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_asr_w() {
    run_instruction_tests("ASR.w").expect("ASR.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_asr_l() {
    run_instruction_tests("ASR.l").expect("ASR.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_rol_b() {
    run_instruction_tests("ROL.b").expect("ROL.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_rol_w() {
    run_instruction_tests("ROL.w").expect("ROL.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_rol_l() {
    run_instruction_tests("ROL.l").expect("ROL.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_ror_b() {
    run_instruction_tests("ROR.b").expect("ROR.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_ror_w() {
    run_instruction_tests("ROR.w").expect("ROR.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_ror_l() {
    run_instruction_tests("ROR.l").expect("ROR.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_roxl_b() {
    run_instruction_tests("ROXL.b").expect("ROXL.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_roxl_w() {
    run_instruction_tests("ROXL.w").expect("ROXL.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_roxl_l() {
    run_instruction_tests("ROXL.l").expect("ROXL.l tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_roxr_b() {
    run_instruction_tests("ROXR.b").expect("ROXR.b tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_roxr_w() {
    run_instruction_tests("ROXR.w").expect("ROXR.w tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_roxr_l() {
    run_instruction_tests("ROXR.l").expect("ROXR.l tests failed");
}

// ============================================================================
// Branch Family Tests
// ============================================================================

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_bcc() {
    run_instruction_tests("Bcc").expect("Bcc tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_bsr() {
    run_instruction_tests("BSR").expect("BSR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_dbcc() {
    run_instruction_tests("DBcc").expect("DBcc tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_scc() {
    run_instruction_tests("Scc").expect("Scc tests failed");
}

// ============================================================================
// Control Flow Tests
// ============================================================================

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_rts() {
    run_instruction_tests("RTS").expect("RTS tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_rtr() {
    run_instruction_tests("RTR").expect("RTR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_jmp() {
    run_instruction_tests("JMP").expect("JMP tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_jsr() {
    run_instruction_tests("JSR").expect("JSR tests failed");
}

// ============================================================================
// System Family Tests
// ============================================================================

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_reset() {
    run_instruction_tests("RESET").expect("RESET tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_rte() {
    run_instruction_tests("RTE").expect("RTE tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_trap() {
    run_instruction_tests("TRAP").expect("TRAP tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_trapv() {
    run_instruction_tests("TRAPV").expect("TRAPV tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_chk() {
    run_instruction_tests("CHK").expect("CHK tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_move_to_sr() {
    run_instruction_tests("MOVEtoSR").expect("MOVEtoSR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_move_from_sr() {
    run_instruction_tests("MOVEfromSR").expect("MOVEfromSR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_move_to_ccr() {
    run_instruction_tests("MOVEtoCCR").expect("MOVEtoCCR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_move_usp() {
    run_instruction_tests("MOVEtoUSP").expect("MOVEtoUSP tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_move_from_usp() {
    run_instruction_tests("MOVEfromUSP").expect("MOVEfromUSP tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_andi_to_sr() {
    run_instruction_tests("ANDItoSR").expect("ANDItoSR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_andi_to_ccr() {
    run_instruction_tests("ANDItoCCR").expect("ANDItoCCR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_ori_to_sr() {
    run_instruction_tests("ORItoSR").expect("ORItoSR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_ori_to_ccr() {
    run_instruction_tests("ORItoCCR").expect("ORItoCCR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_eor_to_sr() {
    run_instruction_tests("EORItoSR").expect("EORItoSR tests failed");
}

#[test]
#[ignore = "Tom Harte test - run manually with --ignored"]
fn test_harte_eor_to_ccr() {
    run_instruction_tests("EORItoCCR").expect("EORItoCCR tests failed");
}

/// Smoke test: just load one file to verify infrastructure works
#[test]
fn test_harte_infrastructure() {
    let repo_path = PathBuf::from("../../../tests/tom_harte_repo");
    let test_files = find_test_files(&repo_path);

    // The Tom Harte vectors are downloaded into `tests/tom_harte_repo` (the
    // README says how). Without them the per-instruction cases are all
    // `#[ignore]`d and never run; this smoke test must likewise skip rather
    // than fail, so a plain `cargo test --workspace` stays green.
    if test_files.is_empty() {
        eprintln!(
            "skipping test_harte_infrastructure: Tom Harte vectors not downloaded \
             (tests/tom_harte_repo is empty)"
        );
        return;
    }

    // The vectors are present: verify the loader actually parses the first
    // file so infra breakage is caught.
    let first_file = &test_files[0];
    let tests = load_test_file(first_file).expect("Failed to load test file");
    assert!(!tests.is_empty(), "Test file was empty");

    println!("Tom Harte infrastructure working!");
    println!("   Found {} test files", test_files.len());
    println!("   First file has {} tests", tests.len());
}
