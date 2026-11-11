//! Bitmap-aware disassembly window, anchored on PC.
//!
//! Returns a stable-sized slice of [`DisasmRow`]s centered on the
//! current Program Counter, suitable for rendering the IDE-style
//! "disassembly panel" that scrolls smoothly as the user steps. The
//! algorithm is CPU-agnostic, it walks instruction boundaries by
//! probing a small set of plausible lengths and decoding only at
//! addresses the [`DisasmDriver`] has observed as instruction starts.
//!
//! Bounding the lookup to the immediate neighborhood of PC prevents
//! stale coverage (from a previous code path, a reset, or a far
//! branch) from surfacing above the new PC.
//!
//! SG1 (68K + Z80) and SNES1 (65816 + SPC700) share this one algorithm.
//! The function returns [`retro_core::types::DisasmRow`] directly, so
//! cores don't have to marshal between an intermediate representation.

use crate::types::{DisasmRow, DisasmRowKind};

/// Driver the disassembly window calls into. Each core implements this
/// for the CPU it's rendering. All methods are scoped to the same CPU:
/// instantiate one driver per `(emulator, cpu_id)` pair.
pub trait DisasmDriver {
    /// Plausible instruction lengths the backward walk probes when
    /// looking for the predecessor of the current target address.
    /// Should be sorted smallest-first; the smallest matching offset
    /// wins.
    ///
    /// Examples:
    /// - 68K: `&[2, 4, 6, 8, 10]` (always even, 2..=10 bytes).
    /// - Z80: `&[1, 2, 3, 4]` (1..=4 bytes including `DD/FD CB` prefix).
    /// - 65816: `&[1, 2, 3, 4]` (mostly).
    fn probe_offsets(&self) -> &'static [u32];

    /// `true` iff `addr` was previously observed as an instruction
    /// start by the CPU executing on this driver. Backed by a
    /// [`crate::debug::CoverageBitmap`] in most cores.
    fn is_covered(&self, addr: u32) -> bool;

    /// Decode the instruction at `addr`. Implementations should always
    /// return a [`DisasmRowKind::Code`] row when decoding succeeds; the
    /// window helper handles wrapping uncovered regions in
    /// [`DisasmRowKind::Unknown`].
    fn decode_at(&mut self, addr: u32) -> DisasmRow;
}

/// Produce a window of `before_count + after_count` rows around `pc`.
///
/// Layout (in row-array order):
///
/// ```text
/// [oldest backward row, ..., row immediately below pc, pc row, row after pc, ...]
/// ```
///
/// The output is always `before_count + after_count` long. When the
/// backward walk hits address 0 before filling its slot, the gap is
/// padded with `Unknown` rows at address 0. The forward walk decodes
/// the PC row unconditionally (the CPU is about to execute there, so
/// the user wants to see it even if coverage hasn't recorded it yet).
pub fn disassemble_window<D: DisasmDriver>(
    driver: &mut D,
    pc: u32,
    before_count: usize,
    after_count: usize,
) -> Vec<DisasmRow> {
    let mut rows: Vec<DisasmRow> = Vec::with_capacity(before_count + after_count);

    // -- Backward walk ------------------------------------------------
    // Each iteration produces the row that lands immediately below
    // `target`. We try the few possible instruction lengths for this
    // CPU; the smallest matching offset wins (longer matches would
    // require interior bytes to also be instruction starts, which
    // contradicts a bitmap entry at the shorter offset).
    let mut before: Vec<DisasmRow> = Vec::with_capacity(before_count);
    let mut target = pc;
    for _ in 0..before_count {
        if target == 0 {
            break;
        }
        let mut matched: Option<(u32, DisasmRow)> = None;
        for &offset in driver.probe_offsets() {
            let Some(candidate) = target.checked_sub(offset) else {
                continue;
            };
            if !driver.is_covered(candidate) {
                continue;
            }
            let row = driver.decode_at(candidate);
            if row.bytes.len() as u32 == offset {
                matched = Some((candidate, row));
                break;
            }
        }
        match matched {
            Some((candidate, row)) => {
                before.push(row);
                target = candidate;
            }
            None => {
                let addr = target - 1;
                before.push(unknown_row(addr));
                target = addr;
            }
        }
    }
    // Pad to exactly `before_count` when the walk bailed out early
    // (target reached 0 before we filled the buffer) so callers always
    // get a stable-sized window.
    while before.len() < before_count {
        before.push(unknown_row(0));
    }
    before.reverse();
    rows.extend(before);

    // -- Forward walk -------------------------------------------------
    // The PC row is always decoded, the CPU is *about* to execute it,
    // so the user always wants to see what's there even if the bitmap
    // hasn't recorded it yet. Every other address: decode if covered,
    // otherwise emit a single-byte Unknown so gaps render byte-by-byte
    // (mirrors the backward walk and matches the user's mental model
    // of "show the neighborhood").
    let mut current_addr = pc;
    for _ in 0..after_count {
        let covered = driver.is_covered(current_addr);
        if covered || current_addr == pc {
            let row = driver.decode_at(current_addr);
            let next = current_addr.saturating_add(row.bytes.len().max(1) as u32);
            rows.push(row);
            current_addr = next;
        } else {
            rows.push(unknown_row(current_addr));
            current_addr = current_addr.saturating_add(1);
        }
    }

    rows
}

#[inline]
fn unknown_row(address: u32) -> DisasmRow {
    DisasmRow {
        address,
        kind: DisasmRowKind::Unknown,
        bytes: Vec::new(),
        mnemonic: String::new(),
        operands: String::new(),
        comment: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Minimal driver for unit tests: simulates a 2-byte fixed-width
    /// "CPU" with a HashSet-backed coverage set. Enough to exercise
    /// every branch of the walk logic without dragging a real CPU into
    /// the retro-core crate's test surface.
    struct FixedWidthDriver {
        covered: HashSet<u32>,
        instruction_bytes: usize,
    }

    impl FixedWidthDriver {
        fn new(instruction_bytes: usize, covered: &[u32]) -> Self {
            Self {
                covered: covered.iter().copied().collect(),
                instruction_bytes,
            }
        }
    }

    impl DisasmDriver for FixedWidthDriver {
        fn probe_offsets(&self) -> &'static [u32] {
            &[2, 4, 6, 8, 10]
        }

        fn is_covered(&self, addr: u32) -> bool {
            self.covered.contains(&addr)
        }

        fn decode_at(&mut self, addr: u32) -> DisasmRow {
            DisasmRow {
                address: addr,
                kind: DisasmRowKind::Code,
                bytes: vec![0xAB; self.instruction_bytes],
                mnemonic: "NOP".to_string(),
                operands: String::new(),
                comment: None,
            }
        }
    }

    #[test]
    fn window_size_matches_request() {
        let mut driver = FixedWidthDriver::new(2, &[]);
        let rows = disassemble_window(&mut driver, 0x1000, 5, 5);
        assert_eq!(rows.len(), 10);
    }

    #[test]
    fn empty_coverage_emits_unknown_before_and_pc_only_forward() {
        let mut driver = FixedWidthDriver::new(2, &[]);
        let rows = disassemble_window(&mut driver, 0x1000, 3, 3);
        assert_eq!(rows.len(), 6);
        // Backward rows are Unknown.
        for row in rows.iter().take(3) {
            assert!(matches!(row.kind, DisasmRowKind::Unknown));
        }
        // Forward starts at PC and is always decoded.
        assert!(matches!(rows[3].kind, DisasmRowKind::Code));
        assert_eq!(rows[3].address, 0x1000);
        // Subsequent forward rows are Unknown (not covered).
        assert!(matches!(rows[4].kind, DisasmRowKind::Unknown));
        assert!(matches!(rows[5].kind, DisasmRowKind::Unknown));
    }

    #[test]
    fn fully_covered_walks_back_and_forward_cleanly() {
        let mut driver =
            FixedWidthDriver::new(2, &[0x0FF6, 0x0FF8, 0x0FFA, 0x0FFC, 0x0FFE, 0x1000, 0x1002]);
        let rows = disassemble_window(&mut driver, 0x1000, 3, 3);
        assert_eq!(rows.len(), 6);
        // Backward walk: 0x0FFA, 0x0FFC, 0x0FFE
        assert_eq!(rows[0].address, 0x0FFA);
        assert_eq!(rows[1].address, 0x0FFC);
        assert_eq!(rows[2].address, 0x0FFE);
        for row in rows.iter().take(3) {
            assert!(matches!(row.kind, DisasmRowKind::Code));
        }
        // Forward walk: 0x1000, 0x1002, then PC+4 not covered -> Unknown
        assert_eq!(rows[3].address, 0x1000);
        assert_eq!(rows[4].address, 0x1002);
    }

    #[test]
    fn pc_at_zero_does_not_walk_backward_past_origin() {
        let mut driver = FixedWidthDriver::new(2, &[0]);
        let rows = disassemble_window(&mut driver, 0, 3, 1);
        assert_eq!(rows.len(), 4);
        // Backward bailed at the very start -> all 3 padding rows at 0.
        for row in rows.iter().take(3) {
            assert!(matches!(row.kind, DisasmRowKind::Unknown));
            assert_eq!(row.address, 0);
        }
        assert!(matches!(rows[3].kind, DisasmRowKind::Code));
    }

    #[test]
    fn after_count_zero_returns_only_backward_rows() {
        let mut driver = FixedWidthDriver::new(2, &[0x0FFE, 0x1000]);
        let rows = disassemble_window(&mut driver, 0x1000, 2, 0);
        assert_eq!(rows.len(), 2);
        // The latest backward row is 0x0FFE.
        assert_eq!(rows[1].address, 0x0FFE);
    }
}
