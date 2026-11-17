//! S-DD1 bitstream decompressor, Andreas Naive's algorithm.
//!
//! The S-DD1 decompresses graphics on the fly during a DMA read from ROM.
//! This module is the pure, self-contained decoder: given the raw
//! (compressed) ROM bytes at a transfer's source address and the transfer
//! length, it returns the decompressed bytes. It knows nothing about the
//! bus, the MMC, or DMA, the read path hands it a slice and a length.
//!
//! Written from Andreas Naive's public-domain decompressor, the reference
//! every emulator carries. He splits the decoder into six cooperating
//! objects, the Input Manager, the Golomb-Code Decoder, eight Bit
//! Generators, the Probability Estimation Module, the Context Model, and
//! the Output Logic. Here they collapse into one [`Decompressor`] whose
//! methods keep those names, because a single owner of the shared state
//! is far cleaner in Rust than the pointer graph. The tests pin every byte
//! on synthetic streams in every mode against a recorded fixture
//! (`tests/fixtures/sdd1-decomp-oracle.txt`), and the golden runs of Star
//! Ocean and Street Fighter Alpha 2 cover the real cartridges.
//!
//! Each call rebuilds all state, so the decoder is stateless between
//! transfers, no bitstream state survives a call. Reads past the end of
//! the input slice yield `0` (a valid stream terminates well before then;
//! this only keeps a malformed or truncated stream from panicking).

/// Probability-state evolution table (33 entries), `(code_num,
/// next_if_mps, next_if_lps)`: the chip's adaptive probability machine as
/// Andreas Naive documented it, a state names the Golomb code to use and
/// the states a run of more-probable symbols or a less-probable one moves
/// it to.
const EVOLUTION_TABLE: [(u8, u8, u8); 33] = [
    (0, 25, 25),
    (0, 2, 1),
    (0, 3, 1),
    (0, 4, 2),
    (0, 5, 3),
    (1, 6, 4),
    (1, 7, 5),
    (1, 8, 6),
    (1, 9, 7),
    (2, 10, 8),
    (2, 11, 9),
    (2, 12, 10),
    (2, 13, 11),
    (3, 14, 12),
    (3, 15, 13),
    (3, 16, 14),
    (3, 17, 15),
    (4, 18, 16),
    (4, 19, 17),
    (5, 20, 18),
    (5, 21, 19),
    (6, 22, 20),
    (6, 23, 21),
    (7, 24, 22),
    (7, 24, 23),
    (0, 26, 1),
    (1, 27, 2),
    (2, 28, 4),
    (3, 29, 8),
    (4, 30, 12),
    (5, 31, 16),
    (6, 32, 18),
    (7, 24, 22),
];

/// The Golomb run lengths, indexed by `codeword >> (code_num ^ 7)`: the
/// codeword's top bit says the run ended in a less-probable symbol, and
/// the `code_num` bits after it name, in reverse order, how many
/// more-probable symbols came first. So in the block of entries
/// `2^k..2^(k+1)` the entry `2^k + i` is `2^k - 1 - reverse(i, k)`, the
/// bit-reversal of `i` on `k` bits; entries 0 and 1 are 0.
const RUN_COUNT: [u8; 256] = golomb_run_counts();

const fn golomb_run_counts() -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut k = 0;
    while k < 8 {
        let block = 1usize << k;
        let mut i = 0;
        while i < block {
            let mut reversed = 0usize;
            let mut bit = 0;
            while bit < k {
                reversed |= ((i >> bit) & 1) << (k - 1 - bit);
                bit += 1;
            }
            table[block + i] = (block - 1 - reversed) as u8;
            i += 1;
        }
        k += 1;
    }
    table
}

/// Decompress `input` (raw compressed ROM at the transfer's source
/// address) into `out_len` bytes; `out_len == 0` means the hardware's full
/// `0x10000`-byte transfer. The returned vector is always exactly that
/// length.
pub(crate) fn decompress(input: &[u8], out_len: usize) -> Vec<u8> {
    Decompressor::new(input).run(out_len)
}

/// All decoder state for one transfer (the six S-DD1 sub-blocks flattened
/// into a single owner). Rebuilt per [`decompress`] call.
struct Decompressor<'a> {
    input: &'a [u8],

    // Input Manager
    byte_ptr: usize,
    bit_count: u8,

    // Bit Generators (one run per `code_num` 0..7)
    bg_mpscount: [u8; 8],
    bg_lpsind: [bool; 8],

    // Probability Estimation Module (32 contexts)
    ctx_status: [u8; 32],
    ctx_mps: [u8; 32],

    // Context Model
    bitplanes_info: u8,
    bit_number: u8,
    curr_bitplane: u8,
    prev_bits: [u16; 8],
    high_mask: u16,
    low_mask: u16,
}

impl<'a> Decompressor<'a> {
    fn new(input: &'a [u8]) -> Self {
        let first = input.first().copied().unwrap_or(0);
        let bitplanes_info = first & 0xC0;
        let (high_mask, low_mask) = match first & 0x30 {
            0x00 => (0x01C0, 0x0001),
            0x10 => (0x0180, 0x0001),
            0x20 => (0x00C0, 0x0001),
            _ => (0x0180, 0x0003), // 0x30
        };
        // The Context Model seeds `currBitplane` so that its first XOR lands
        // on the right plane; the `0xC0` (linear) mode overwrites it from
        // `bit_number` on every bit, so its seed is irrelevant.
        let curr_bitplane = match bitplanes_info {
            0x00 => 1,
            0x40 => 7,
            0x80 => 3,
            _ => 0,
        };
        Self {
            input,
            byte_ptr: 0,
            bit_count: 4,
            bg_mpscount: [0; 8],
            bg_lpsind: [false; 8],
            ctx_status: [0; 32],
            ctx_mps: [0; 32],
            bitplanes_info,
            bit_number: 0,
            curr_bitplane,
            prev_bits: [0; 8],
            high_mask,
            low_mask,
        }
    }

    #[inline]
    fn read_in(&self, idx: usize) -> u8 {
        self.input.get(idx).copied().unwrap_or(0)
    }

    /// Input Manager: pull the next codeword (`SDD1_IM::getCodeword`). The
    /// `<< bit_count` / `>> (9 - bit_count)` arithmetic is intentionally
    /// 8-bit-truncating, matching the C `uint8` semantics.
    fn get_codeword(&mut self, code_len: u8) -> u8 {
        let mut codeword = self
            .read_in(self.byte_ptr)
            .wrapping_shl(u32::from(self.bit_count));
        self.bit_count += 1;
        if (codeword & 0x80) != 0 {
            let shift = 9u32 - u32::from(self.bit_count);
            codeword |= (u32::from(self.read_in(self.byte_ptr + 1)) >> shift) as u8;
            self.bit_count += code_len;
        }
        if (self.bit_count & 0x08) != 0 {
            self.byte_ptr += 1;
            self.bit_count &= 0x07;
        }
        codeword
    }

    /// Golomb-Code Decoder: refresh one bit generator's run
    /// (`SDD1_GCD::getRunCount`).
    fn gcd_run_count(&mut self, code_num: u8) {
        let codeword = self.get_codeword(code_num);
        let cn = usize::from(code_num);
        if (codeword & 0x80) != 0 {
            self.bg_lpsind[cn] = true;
            self.bg_mpscount[cn] = RUN_COUNT[usize::from(codeword >> (code_num ^ 0x07))];
        } else {
            self.bg_mpscount[cn] = 1 << code_num;
        }
    }

    /// Bit Generator: next raw bit + end-of-run flag (`SDD1_BG::getBit`).
    fn bg_get_bit(&mut self, code_num: u8) -> (u8, bool) {
        let cn = usize::from(code_num);
        if self.bg_mpscount[cn] == 0 && !self.bg_lpsind[cn] {
            self.gcd_run_count(code_num);
        }
        let bit = if self.bg_mpscount[cn] != 0 {
            self.bg_mpscount[cn] -= 1;
            0
        } else {
            self.bg_lpsind[cn] = false;
            1
        };
        let end_of_run = self.bg_mpscount[cn] == 0 && !self.bg_lpsind[cn];
        (bit, end_of_run)
    }

    /// Probability Estimation Module: one context-modelled bit
    /// (`SDD1_PEM::getBit`).
    fn pem_get_bit(&mut self, context: u8) -> u8 {
        let c = usize::from(context);
        let curr_status = self.ctx_status[c];
        let (code_num, next_if_mps, next_if_lps) = EVOLUTION_TABLE[usize::from(curr_status)];
        let current_mps = self.ctx_mps[c];
        let (bit, end_of_run) = self.bg_get_bit(code_num);
        if end_of_run {
            if bit != 0 {
                if (curr_status & 0xFE) == 0 {
                    self.ctx_mps[c] ^= 0x01;
                }
                self.ctx_status[c] = next_if_lps;
            } else {
                self.ctx_status[c] = next_if_mps;
            }
        }
        bit ^ current_mps
    }

    /// Context Model: derive the context, fetch a bit, and update history
    /// (`SDD1_CM::getBit`).
    fn cm_get_bit(&mut self) -> u8 {
        match self.bitplanes_info {
            0x00 => self.curr_bitplane ^= 0x01,
            0x40 => {
                self.curr_bitplane ^= 0x01;
                if (self.bit_number & 0x7F) == 0 {
                    self.curr_bitplane = (self.curr_bitplane + 2) & 0x07;
                }
            }
            0x80 => {
                self.curr_bitplane ^= 0x01;
                if (self.bit_number & 0x7F) == 0 {
                    self.curr_bitplane ^= 0x02;
                }
            }
            _ => self.curr_bitplane = self.bit_number & 0x07, // 0xC0
        }

        let plane = usize::from(self.curr_bitplane);
        let cb = self.prev_bits[plane];
        let curr_context = ((self.curr_bitplane & 0x01) << 4)
            | (((cb & self.high_mask) >> 5) as u8)
            | ((cb & self.low_mask) as u8);

        let bit = self.pem_get_bit(curr_context);

        self.prev_bits[plane] = (cb << 1) | u16::from(bit);
        self.bit_number = self.bit_number.wrapping_add(1);
        bit
    }

    /// Output Logic: assemble the decompressed bytes (`SDD1_OL::launch`).
    fn run(mut self, out_len: usize) -> Vec<u8> {
        let n = if out_len == 0 { 0x1_0000 } else { out_len };
        let mut out = vec![0u8; n];

        if self.bitplanes_info == 0xC0 {
            // Linear: one bit per ascending plane, LSB-first.
            for slot in out.iter_mut() {
                let mut byte = 0u8;
                let mut bit = 0x01u8;
                while bit != 0 {
                    if self.cm_get_bit() != 0 {
                        byte |= bit;
                    }
                    bit = bit.wrapping_shl(1);
                }
                *slot = byte;
            }
            return out;
        }

        // Two interleaved bitplanes: each build decodes a pair of bytes
        // (register1 from the even plane, register2 from the odd), emitting
        // both before the next build. An odd `n` stops after register1 of
        // the final pair, discarding register2, matching the reference,
        // which decrements its length counter once per emitted byte.
        let mut pos = 0usize;
        while pos < n {
            let mut reg1 = 0u8;
            let mut reg2 = 0u8;
            let mut bit = 0x80u8;
            while bit != 0 {
                if self.cm_get_bit() != 0 {
                    reg1 |= bit;
                }
                if self.cm_get_bit() != 0 {
                    reg2 |= bit;
                }
                bit >>= 1;
            }
            out[pos] = reg1;
            pos += 1;
            if pos == n {
                break;
            }
            out[pos] = reg2;
            pos += 1;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Deterministic xorshift64*, a seeded body generator so the sweep is
    /// reproducible without pulling in a PRNG crate.
    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            Rng(seed)
        }
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn next_u8(&mut self) -> u8 {
            (self.next_u64() >> 33) as u8
        }
        fn next_usize(&mut self, bound: usize) -> usize {
            (self.next_u64() >> 1) as usize % bound
        }
    }

    const BITPLANE_MODES: [u8; 4] = [0x00, 0x40, 0x80, 0xC0];
    const CONTEXT_MODES: [u8; 4] = [0x00, 0x10, 0x20, 0x30];

    /// Build a synthetic compressed block whose header nibble selects the
    /// mode and whose remaining bytes (plus the header's low nibble) are
    /// random bitstream.
    fn synth_block(rng: &mut Rng, bp: u8, cb: u8, body_len: usize) -> Vec<u8> {
        let mut input = vec![0u8; body_len.max(1)];
        input[0] = bp | cb | (rng.next_u8() & 0x0F);
        for b in input.iter_mut().skip(1) {
            *b = rng.next_u8();
        }
        input
    }

    fn fixture_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sdd1-decomp-oracle.txt")
    }

    /// One hash over a set of outputs: each output's length, then its bytes.
    fn hash_outputs(outputs: &[Vec<u8>]) -> u64 {
        let mut buf = Vec::new();
        for out in outputs {
            buf.extend_from_slice(&(out.len() as u32).to_le_bytes());
            buf.extend_from_slice(out);
        }
        retro_core::fnv1a64(&buf)
    }

    /// The cases the fixture records, as `label hash` lines: for every
    /// bitplane and context mode, thirty random bodies decoded to ten
    /// output lengths each (one hash per body); one full 0x10000-byte
    /// transfer per mode (the `out_len == 0` rule); and the short and
    /// empty inputs.
    fn oracle_lines() -> Vec<String> {
        let mut lines = Vec::new();
        let mut rng = Rng::new(0xC0FF_EE00_1234_5678);
        let out_lens = [1usize, 2, 3, 7, 8, 63, 64, 255, 256, 511];
        for &bp in &BITPLANE_MODES {
            for &cb in &CONTEXT_MODES {
                for body in 0..30 {
                    let body_len = 2 + rng.next_usize(220);
                    let input = synth_block(&mut rng, bp, cb, body_len);
                    let outputs: Vec<Vec<u8>> =
                        out_lens.iter().map(|&n| decompress(&input, n)).collect();
                    lines.push(format!(
                        "modes bp={bp:02x} cb={cb:02x} body={body:02} {:016x}",
                        hash_outputs(&outputs)
                    ));
                }
            }
        }
        let mut rng = Rng::new(0x1234_5678_9ABC_DEF0);
        for &bp in &BITPLANE_MODES {
            for &cb in &CONTEXT_MODES {
                let input = synth_block(&mut rng, bp, cb, 4096);
                let out = decompress(&input, 0);
                assert_eq!(out.len(), 0x1_0000);
                lines.push(format!(
                    "full bp={bp:02x} cb={cb:02x} {:016x}",
                    hash_outputs(&[out])
                ));
            }
        }
        let shorts: [&[u8]; 4] = [&[], &[0x00], &[0xFF], &[0xC0, 0xAA]];
        for (j, input) in shorts.iter().enumerate() {
            let outputs: Vec<Vec<u8>> = [1usize, 2, 9, 64]
                .iter()
                .map(|&n| decompress(input, n))
                .collect();
            lines.push(format!("short input={j} {:016x}", hash_outputs(&outputs)));
        }
        lines
    }

    /// `SNES1_SDD1_RECORD=1` writes the fixture instead of comparing.
    #[test]
    fn the_decoder_output_is_the_recorded_one() {
        let lines = oracle_lines();
        if std::env::var_os("SNES1_SDD1_RECORD").is_some() {
            std::fs::write(fixture_path(), lines.join("\n") + "\n").unwrap();
            return;
        }
        let recorded = std::fs::read_to_string(fixture_path())
            .expect("tests/fixtures/sdd1-decomp-oracle.txt: record it with SNES1_SDD1_RECORD=1");
        let recorded: Vec<&str> = recorded.lines().collect();
        assert_eq!(recorded.len(), lines.len(), "the fixture has another shape");
        let wrong: Vec<String> = lines
            .iter()
            .zip(&recorded)
            .filter(|(have, want)| have.as_str() != **want)
            .map(|(have, want)| format!("have {have}, recorded {want}"))
            .collect();
        assert!(
            wrong.is_empty(),
            "{} of {} cases differ:\n{}",
            wrong.len(),
            lines.len(),
            wrong.join("\n")
        );
    }

    #[test]
    fn golomb_run_counts_follow_the_bit_reversal() {
        for (index, want) in [
            (0x00, 0x00),
            (0x01, 0x00),
            (0x02, 0x01),
            (0x03, 0x00),
            (0x04, 0x03),
            (0x05, 0x01),
            (0x10, 0x0f),
            (0x11, 0x07),
            (0x12, 0x0b),
            (0x80, 0x7f),
            (0x81, 0x3f),
            (0xfe, 0x40),
            (0xff, 0x00),
        ] {
            assert_eq!(RUN_COUNT[index], want, "entry {index:#04x}");
        }
    }

    #[test]
    fn out_len_zero_yields_full_transfer() {
        let input = [0x00u8, 0x11, 0x22, 0x33, 0x44, 0x55];
        assert_eq!(decompress(&input, 0).len(), 0x1_0000);
        assert_eq!(decompress(&input, 5).len(), 5);
        assert_eq!(decompress(&input, 1).len(), 1);
    }

    #[test]
    fn short_or_empty_input_does_not_panic() {
        for input in [&[][..], &[0x00][..], &[0xFF][..], &[0xC0, 0xAA][..]] {
            for out_len in [1usize, 2, 9, 64] {
                assert_eq!(decompress(input, out_len).len(), out_len);
            }
        }
    }

    #[test]
    fn deterministic_and_stateless_between_calls() {
        let input = synth_block(&mut Rng::new(0xABCD_1234_5678_9F01), 0x80, 0x20, 128);
        let a = decompress(&input, 500);
        let b = decompress(&input, 500);
        assert_eq!(a, b, "same input must yield identical output");

        // A prior call on unrelated data must not affect a later one.
        let _ = decompress(&synth_block(&mut Rng::new(1), 0x40, 0x30, 64), 777);
        let c = decompress(&input, 500);
        assert_eq!(a, c, "no state may leak between calls");
    }

    #[test]
    fn frozen_snapshots() {
        // Regression anchors: exact output prefixes for two fixed inputs.
        // Any change to the decoder that alters its output trips these.
        let zeros = decompress(&[0u8; 32], 16);
        assert_eq!(
            zeros,
            [
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00,
            ]
        );

        let ones = decompress(&[0xFFu8; 32], 16);
        assert_eq!(
            ones,
            [
                0xC3, 0x33, 0xC3, 0xCC, 0x00, 0x0F, 0x33, 0xFF, 0xC3, 0xF3, 0x30, 0xFF, 0x03, 0x30,
                0xCC, 0xC3,
            ]
        );
    }
}
