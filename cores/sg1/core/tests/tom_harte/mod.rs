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

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

/// Load and parse a gzipped JSON test file
pub fn load_test_file(
    path: &std::path::Path,
) -> Result<Vec<HarteTest>, Box<dyn std::error::Error>> {
    use flate2::read::GzDecoder;
    use std::io::Read;

    let file = std::fs::File::open(path)?;
    let mut decoder = GzDecoder::new(file);
    let mut json_string = String::new();
    decoder.read_to_string(&mut json_string)?;

    let tests: Vec<HarteTest> = serde_json::from_str(&json_string)?;
    Ok(tests)
}

/// Find all test files in the Tom Harte repository
pub fn find_test_files(base_path: &std::path::Path) -> Vec<std::path::PathBuf> {
    let test_dir = base_path.join("680x0/68000/v1");

    if !test_dir.exists() {
        return Vec::new();
    }

    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(test_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("gz") {
                files.push(path);
            }
        }
    }

    files.sort();
    files
}
