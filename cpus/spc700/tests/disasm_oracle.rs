//! The disassembler's output, pinned: for every opcode, a sample of the
//! operand byte pairs (below), two values of `PSW.P` and three addresses
//! (one in the middle of the address space, one at its very start and
//! one at its very end, where a branch target would wrap), the text of
//! `disassemble_at` and the fields of `disassemble_one` are hashed into
//! one number per (opcode, P, address) and compared with the recorded
//! file. The format is a behaviour, the trace comparison against the
//! bsnes-plus reference logs depends on it, so a rewrite of the
//! disassembler must reproduce every byte.
//!
//! `SPC700_DISASM_RECORD=1` writes the fixture (and a one-line sample
//! per opcode) instead of comparing.

use retro_core::cpu::Addressable;
use spc700::disasm::{disassemble_at, disassemble_one, DisasmState};
use std::fmt::Write as _;
use std::path::PathBuf;

struct Bus([u8; 0x10000]);

impl Addressable for Bus {
    fn read(&mut self, addr: u32) -> u8 {
        self.0[(addr & 0xffff) as usize]
    }
    fn write(&mut self, addr: u32, value: u8) {
        self.0[(addr & 0xffff) as usize] = value;
    }
}

const ADDRESSES: [u16; 3] = [0x1000, 0x0000, 0xfffe];

/// The operand byte values each position is paired with while the other
/// position runs through all 256: a zero, a plain value, and the two sign
/// edges a branch offset turns on, plus the top value. So every byte
/// value is seen in each operand position, and each sign edge meets the
/// extremes of the other byte. (A full sweep of all 65 536 pairs takes
/// three minutes in a debug build, so the fixture pins this sample.)
const PAIRED_WITH: [u8; 5] = [0x00, 0x12, 0x7f, 0x80, 0xff];

/// The operand pairs hashed for one (opcode, P, address).
fn operand_pairs() -> impl Iterator<Item = (u8, u8)> {
    let first_runs = (0u32..256).flat_map(|lo| PAIRED_WITH.iter().map(move |&hi| (lo as u8, hi)));
    let second_runs = (0u32..256).flat_map(|hi| PAIRED_WITH.iter().map(move |&lo| (lo, hi as u8)));
    first_runs.chain(second_runs)
}

fn fnv1a(hash: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *hash ^= u64::from(b);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/disasm-oracle.txt")
}

fn sample_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/disasm-sample.txt")
}

/// One hash over the sampled operand pairs at `addr` with `p`.
fn hash_opcode(bus: &mut Bus, opcode: u8, p: bool, addr: u16) -> u64 {
    let state = DisasmState { p };
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    let mut line = String::new();
    for (lo, hi) in operand_pairs() {
        bus.write(u32::from(addr), opcode);
        bus.write(u32::from(addr.wrapping_add(1)), lo);
        bus.write(u32::from(addr.wrapping_add(2)), hi);
        let (text, len) = disassemble_at(bus, u32::from(addr), &state);
        let one = disassemble_one(bus, u32::from(addr), &state);
        line.clear();
        let _ = writeln!(
            line,
            "{text}|{len}|{}|{}|{}|{:?}|{}",
            one.mnemonic,
            one.operands,
            one.comment.as_deref().unwrap_or(""),
            one.bytes,
            one.bytes_consumed
        );
        fnv1a(&mut hash, line.as_bytes());
    }
    hash
}

#[test]
fn the_disassembler_output_is_the_recorded_one() {
    let record = std::env::var_os("SPC700_DISASM_RECORD").is_some();
    let mut bus = Bus([0; 0x10000]);
    let mut lines = Vec::new();
    for opcode in 0u16..256 {
        for p in [false, true] {
            for addr in ADDRESSES {
                let hash = hash_opcode(&mut bus, opcode as u8, p, addr);
                lines.push(format!(
                    "{opcode:02x} {} {addr:04x} {hash:016x}",
                    u8::from(p)
                ));
            }
        }
    }
    if record {
        std::fs::write(fixture_path(), lines.join("\n") + "\n").unwrap();
        // The sample: each opcode with the operand bytes $12 $34 at
        // $1000 with P=0, as a readable record of the format.
        let mut sample = String::new();
        for opcode in 0u16..256 {
            bus.write(0x1000, opcode as u8);
            bus.write(0x1001, 0x12);
            bus.write(0x1002, 0x34);
            let (text, len) = disassemble_at(&mut bus, 0x1000, &DisasmState { p: false });
            let one = disassemble_one(&mut bus, 0x1000, &DisasmState { p: false });
            let _ = writeln!(
                sample,
                "{opcode:02x} {len} {text:?} {:?} {:?} {:?}",
                one.mnemonic,
                one.operands,
                one.comment.as_deref().unwrap_or("")
            );
        }
        std::fs::write(sample_path(), sample).unwrap();
        return;
    }
    let recorded = std::fs::read_to_string(fixture_path())
        .expect("tests/fixtures/disasm-oracle.txt - record it with SPC700_DISASM_RECORD=1");
    let recorded: Vec<&str> = recorded.lines().collect();
    assert_eq!(recorded.len(), lines.len(), "the fixture has another shape");
    let mut wrong = Vec::new();
    for (have, want) in lines.iter().zip(recorded.iter()) {
        if have != want {
            wrong.push(format!("have {have}, recorded {want}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} (opcode, P, address) hashes differ:\n{}",
        wrong.len(),
        lines.len(),
        wrong.join("\n")
    );
}
