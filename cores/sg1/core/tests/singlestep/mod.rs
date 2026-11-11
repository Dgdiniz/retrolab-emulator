//! SingleStepTests Z80 integration
//!
//! Test suite from: https://github.com/SingleStepTests/z80
//!
//! Each test provides full CPU and memory state before and after executing a single instruction.
//!
//! Shared by several independent test binaries, each of which compiles this
//! module separately and uses a different subset of it, so per-binary
//! dead-code lints fire on items a sibling binary depends on.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
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

/// Load and parse a JSON test file (uncompressed)
pub fn load_test_file(path: &std::path::Path) -> Result<Vec<Z80Test>, Box<dyn std::error::Error>> {
    let json_string = std::fs::read_to_string(path)?;
    let tests: Vec<Z80Test> = serde_json::from_str(&json_string)?;
    Ok(tests)
}

/// Find all test files in the SingleStepTests repository
pub fn find_test_files(base_path: &std::path::Path) -> Vec<std::path::PathBuf> {
    let test_dir = base_path.join("v1");

    if !test_dir.exists() {
        return Vec::new();
    }

    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(test_dir) {
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
