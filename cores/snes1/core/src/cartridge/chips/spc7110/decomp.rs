//! SPC7110 data decompressor (`$4800-$480C`).
//!
//! A byte-faithful port of the decompressor of the SPC7110 emulator by
//! byuu and neviksti (its notice is at the end of `LICENSE`), read in the
//! copy bsnes-plus carries. It is an adaptive binary arithmetic ("range") decoder
//! driving three bitplane packers:
//!
//! - **mode 0**: 1 bpp: one binary decision per bit, eight per output byte.
//! - **mode 1**: 2 bpp: a move-to-front pixel predictor over 4 colours, two
//!   symbols per pixel, packed via [`morton_2x8`] into 2 bytes per 8 pixels.
//! - **mode 2**: 4 bpp: the same predictor over 16 colours, four symbols per
//!   pixel, packed via [`morton_4x8`] with a 16-byte bitplane reorder buffer.
//!
//! mode 3 is invalid and always yields `0x00` (the power-on state, so reading
//! the FIFO before the first `$4806` trigger returns `0`).
//!
//! ## Savestate hazard (why the decoder state lives in fields)
//!
//! bsnes keeps the per-decompression decoder state (`val`/`in`/`span`/`out`/
//! `inverts`/`lps`/`in_count`, the pixel-order arrays, mode 2's dual
//! accumulators + reorder buffer) in **`static` locals** inside `mode0/1/2`;
//! its `serialization.cpp` does **not** save them, so a savestate captured
//! mid-decompression would resume with stale decoder state. This port promotes
//! every one of those to a struct field so the whole decoder round-trips.
//!
//! The unit owns an `Arc<[u8]>` clone of the cartridge ROM purely so
//! [`Self::dataread`] can stream compressed bytes without threading a closure
//! through the recursive mode functions; the ROM is never serialized.

use crate::savestate::Spc7110DecompState;
use std::sync::Arc;

/// Ring-buffer size (bytes). Must be a power of two and `>= 64`; `read()`
/// refills half of it per decompression batch (bsnes `decomp_buffer_size`).
const BUFFER_SIZE: usize = 64;

/// Copy `src` into the fixed-size `dst`, up to the shorter length (the
/// remaining bytes of a short `src` keep their prior value). Keeps the
/// savestate restore length-tolerant.
fn copy_bytes(dst: &mut [u8], src: &[u8]) {
    let n = dst.len().min(src.len());
    dst[..n].copy_from_slice(&src[..n]);
}

/// One adaptive-context slot: an index into [`EVOLUTION_TABLE`] plus the
/// current MPS-invert bit (bsnes `ContextState`).
#[derive(Clone, Copy, Default)]
struct ContextState {
    index: u8,
    invert: u8,
}

/// The SPC7110 arithmetic decompressor.
#[derive(Clone)]
pub(super) struct Spc7110Decomp {
    /// Cartridge ROM (shared, never serialized), the compressed source.
    rom: Arc<[u8]>,
    /// Unpadded ROM length, for the [`Self::dataread`] modulo wrap.
    real_rom_len: usize,

    /// Active decompression mode (`0/1/2`, or `3` = invalid -> `0x00`).
    mode: u32,
    /// Running read cursor into the DATA ROM (bsnes `decomp_offset`).
    offset: u32,

    /// Output ring buffer + its read/write cursors and fill level.
    buffer: [u8; BUFFER_SIZE],
    rd: usize,
    wr: usize,
    len: usize,

    /// 32 adaptive-probability contexts (reset per `init`).
    context: [ContextState; 32],

    // ---- range-decoder state (bsnes per-mode `static` locals) ----
    /// Range decoder value / input shift register / current span. `u8` so the
    /// exact bsnes `uint8` wrap semantics reproduce.
    val: u8,
    input: u8,
    span: u8,
    /// Symbol/pixel accumulator for modes 0/1 (`out`).
    out: u32,
    /// Running LPS and invert bit histories (accumulate left-shifted).
    inverts: u32,
    lps: u32,
    /// Bits remaining in `input` before the next `dataread` (bsnes `in_count`).
    in_count: i32,

    /// Move-to-front pixel predictor (mode 1 uses `[0..4]`, mode 2 `[0..16]`).
    pixelorder: [i32; 16],
    realorder: [i32; 16],

    /// Mode 2 dual pixel accumulators + its 16-byte bitplane reorder buffer.
    out0: u32,
    out1: u32,
    bitplanebuffer: [u8; 16],
    buffer_index: usize,
}

impl Spc7110Decomp {
    /// Build a decompressor over the cartridge ROM in the power-on (`reset`)
    /// state.
    pub(super) fn new(rom: Arc<[u8]>, real_rom_len: usize) -> Self {
        let mut d = Self {
            rom,
            real_rom_len,
            mode: 3,
            offset: 0,
            buffer: [0; BUFFER_SIZE],
            rd: 0,
            wr: 0,
            len: 0,
            context: [ContextState::default(); 32],
            val: 0,
            input: 0,
            span: 0,
            out: 0,
            inverts: 0,
            lps: 0,
            in_count: 0,
            pixelorder: [0; 16],
            realorder: [0; 16],
            out0: 0,
            out1: 0,
            bitplanebuffer: [0; 16],
            buffer_index: 0,
        };
        d.reset();
        d
    }

    /// Return to the power-on state: mode 3 (invalid) and an empty FIFO. Does
    /// not touch the ROM. bsnes `SPC7110Decomp::reset`.
    pub(super) fn reset(&mut self) {
        self.mode = 3;
        self.rd = 0;
        self.wr = 0;
        self.len = 0;
    }

    /// Start a decompression: latch the mode + DATA-ROM offset, reset the FIFO
    /// and contexts, prime the active mode, then discard `index` output bytes
    /// (the pre-skip). bsnes `SPC7110Decomp::init`.
    pub(super) fn init(&mut self, mode: u32, offset: u32, index: u32) {
        self.mode = mode;
        self.offset = offset;
        self.rd = 0;
        self.wr = 0;
        self.len = 0;
        self.context = [ContextState::default(); 32];

        match mode {
            0 => self.mode0(true),
            1 => self.mode1(true),
            2 => self.mode2(true),
            _ => {}
        }

        let mut index = index;
        while index > 0 {
            self.read();
            index -= 1;
        }
    }

    /// Read one decompressed byte from the FIFO, refilling it (a half-buffer
    /// batch) on demand. Mode 3 (invalid) always returns `0x00`. bsnes
    /// `SPC7110Decomp::read`.
    pub(super) fn read(&mut self) -> u8 {
        if self.len == 0 {
            match self.mode {
                0 => self.mode0(false),
                1 => self.mode1(false),
                2 => self.mode2(false),
                _ => return 0x00,
            }
        }
        let data = self.buffer[self.rd];
        self.rd = (self.rd + 1) & (BUFFER_SIZE - 1);
        self.len -= 1;
        data
    }

    /// Active mode (`0/1/2`, or `3` = idle/invalid), for the debug view.
    pub(super) fn mode(&self) -> u32 {
        self.mode
    }

    /// Current DATA-ROM read offset, for the debug view.
    pub(super) fn offset(&self) -> u32 {
        self.offset
    }

    /// Number of bytes currently buffered in the FIFO, for the debug view.
    pub(super) fn buffered(&self) -> usize {
        self.len
    }

    /// Capture the full decoder state for the savestate, every field bsnes
    /// would leave in a `static` local, so a mid-decompression save resumes
    /// bit-for-bit. The shared ROM `Arc` is *not* captured (the live chip keeps
    /// its own across a restore).
    pub(super) fn snapshot(&self) -> Spc7110DecompState {
        Spc7110DecompState {
            mode: self.mode,
            offset: self.offset,
            buffer: self.buffer.to_vec(),
            rd: self.rd as u32,
            wr: self.wr as u32,
            len: self.len as u32,
            context_index: self.context.iter().map(|c| c.index).collect(),
            context_invert: self.context.iter().map(|c| c.invert).collect(),
            val: self.val,
            input: self.input,
            span: self.span,
            out: self.out,
            inverts: self.inverts,
            lps: self.lps,
            in_count: self.in_count,
            pixelorder: self.pixelorder.to_vec(),
            realorder: self.realorder.to_vec(),
            out0: self.out0,
            out1: self.out1,
            bitplanebuffer: self.bitplanebuffer.to_vec(),
            buffer_index: self.buffer_index as u32,
        }
    }

    /// Restore the decoder state from a savestate. Length-tolerant so a
    /// truncated / oversized blob can't panic; the ring cursors are masked into
    /// range and the ROM `Arc` is left as-is.
    pub(super) fn restore_state(&mut self, s: &Spc7110DecompState) {
        self.mode = s.mode;
        self.offset = s.offset;
        copy_bytes(&mut self.buffer, &s.buffer);
        self.rd = (s.rd as usize) & (BUFFER_SIZE - 1);
        self.wr = (s.wr as usize) & (BUFFER_SIZE - 1);
        self.len = (s.len as usize).min(BUFFER_SIZE);
        for (i, ctx) in self.context.iter_mut().enumerate() {
            ctx.index = s.context_index.get(i).copied().unwrap_or(0);
            ctx.invert = s.context_invert.get(i).copied().unwrap_or(0);
        }
        self.val = s.val;
        self.input = s.input;
        self.span = s.span;
        self.out = s.out;
        self.inverts = s.inverts;
        self.lps = s.lps;
        self.in_count = s.in_count;
        for (i, v) in self.pixelorder.iter_mut().enumerate() {
            *v = s.pixelorder.get(i).copied().unwrap_or(0);
        }
        for (i, v) in self.realorder.iter_mut().enumerate() {
            *v = s.realorder.get(i).copied().unwrap_or(0);
        }
        self.out0 = s.out0;
        self.out1 = s.out1;
        copy_bytes(&mut self.bitplanebuffer, &s.bitplanebuffer);
        self.buffer_index = (s.buffer_index as usize).min(self.bitplanebuffer.len());
    }

    /// Append a decompressed byte to the ring buffer.
    fn write(&mut self, data: u8) {
        self.buffer[self.wr] = data;
        self.wr = (self.wr + 1) & (BUFFER_SIZE - 1);
        self.len += 1;
    }

    /// Stream the next compressed byte from the DATA ROM, wrapping the offset
    /// modulo the data-ROM size (bsnes `dataread`). Uses the *unpadded* ROM
    /// length so the power-of-two pad tail is never read.
    fn dataread(&mut self) -> u8 {
        let base = super::SPC7110_DATA_ROM_OFFSET;
        let size = (self.real_rom_len as u32).saturating_sub(base);
        if size == 0 {
            return 0;
        }
        while self.offset >= size {
            self.offset -= size;
        }
        let phys = (base + self.offset) as usize;
        let data = self.rom.get(phys).copied().unwrap_or(0);
        self.offset = self.offset.wrapping_add(1);
        data
    }

    fn probability(&self, n: usize) -> u32 {
        EVOLUTION_TABLE[self.context[n].index as usize][0] as u32
    }

    fn next_lps(&self, n: usize) -> u8 {
        EVOLUTION_TABLE[self.context[n].index as usize][1]
    }

    fn next_mps(&self, n: usize) -> u8 {
        EVOLUTION_TABLE[self.context[n].index as usize][2]
    }

    fn toggle_invert(&self, n: usize) -> bool {
        EVOLUTION_TABLE[self.context[n].index as usize][3] != 0
    }

    /// Decode one binary symbol against context `con`, returning `flag_lps`
    /// (0/1) and the renormalisation shift count. Shared by all three modes;
    /// callers decide whether to fold the result into `out`.
    fn decode_symbol(&mut self, con: usize) -> (u32, u32) {
        let prob = self.probability(con);

        let diff = (self.span as u32).wrapping_sub(prob);
        let flag_lps = if (self.val as u32) <= diff {
            // MPS
            self.span = diff as u8;
            0
        } else {
            // LPS
            let sub = (self.span as u32).wrapping_sub(prob.wrapping_sub(1));
            self.val = (self.val as u32).wrapping_sub(sub) as u8;
            self.span = prob.wrapping_sub(1) as u8;
            1
        };

        let mut shift = 0u32;
        while self.span < 0x7f {
            shift += 1;
            self.span = self.span.wrapping_shl(1).wrapping_add(1);
            self.val = self.val.wrapping_shl(1).wrapping_add(self.input >> 7);
            self.input = self.input.wrapping_shl(1);
            self.in_count -= 1;
            if self.in_count == 0 {
                self.input = self.dataread();
                self.in_count = 8;
            }
        }

        (flag_lps, shift)
    }

    fn mode0(&mut self, init: bool) {
        if init {
            self.out = 0;
            self.inverts = 0;
            self.lps = 0;
            self.span = 0xff;
            self.val = self.dataread();
            self.input = self.dataread();
            self.in_count = 8;
            return;
        }

        while self.len < BUFFER_SIZE / 2 {
            for bit in 0..8u32 {
                let mask = ((1u32 << (bit & 3)) - 1) as u8;
                let mut con = u32::from(mask)
                    + u32::from((self.inverts as u8 & mask) ^ (self.lps as u8 & mask));
                if bit > 3 {
                    con += 15;
                }
                let con = con as usize;

                let mps = ((self.out >> 15) & 1) ^ u32::from(self.context[con].invert);
                let (flag_lps, shift) = self.decode_symbol(con);
                if flag_lps == 0 {
                    self.out = self.out.wrapping_shl(1).wrapping_add(mps);
                } else {
                    self.out = self.out.wrapping_shl(1).wrapping_add(1).wrapping_sub(mps);
                }

                self.update_context(con, flag_lps, shift);
            }
            let byte = self.out as u8;
            self.write(byte);
        }
    }

    fn mode1(&mut self, init: bool) {
        if init {
            for i in 0..4 {
                self.pixelorder[i] = i as i32;
            }
            self.out = 0;
            self.inverts = 0;
            self.lps = 0;
            self.span = 0xff;
            self.val = self.dataread();
            self.input = self.dataread();
            self.in_count = 8;
            return;
        }

        while self.len < BUFFER_SIZE / 2 {
            for _pixel in 0..8 {
                let a = ((self.out >> 2) & 3) as i32;
                let b = ((self.out >> 14) & 3) as i32;
                let c = ((self.out >> 16) & 3) as i32;
                let mut con = pixel_context(a, b, c);

                move_to_front(&mut self.pixelorder, 4, a);
                self.realorder[..4].copy_from_slice(&self.pixelorder[..4]);
                move_to_front(&mut self.realorder, 4, c);
                move_to_front(&mut self.realorder, 4, b);
                move_to_front(&mut self.realorder, 4, a);

                for _bit in 0..2 {
                    let cur = con as usize;
                    let (flag_lps, shift) = self.decode_symbol(cur);
                    self.update_context(cur, flag_lps, shift);
                    con = 5 + (con << 1) + ((self.lps ^ self.inverts) & 1);
                }

                let pixel = self.realorder[((self.lps ^ self.inverts) & 3) as usize];
                self.out = self.out.wrapping_shl(2).wrapping_add(pixel as u32);
            }

            let data = morton_2x8(self.out);
            let hi = (data >> 8) as u8;
            let lo = data as u8;
            self.write(hi);
            self.write(lo);
        }
    }

    fn mode2(&mut self, init: bool) {
        if init {
            for i in 0..16 {
                self.pixelorder[i] = i as i32;
            }
            self.buffer_index = 0;
            self.out0 = 0;
            self.out1 = 0;
            self.inverts = 0;
            self.lps = 0;
            self.span = 0xff;
            self.val = self.dataread();
            self.input = self.dataread();
            self.in_count = 8;
            return;
        }

        while self.len < BUFFER_SIZE / 2 {
            for _pixel in 0..8 {
                let a = (self.out0 & 15) as i32;
                let b = ((self.out0 >> 28) & 15) as i32;
                let c = (self.out1 & 15) as i32;
                let refcon = pixel_context(a, b, c);
                let mut con = 0u32;

                move_to_front(&mut self.pixelorder, 16, a);
                self.realorder = self.pixelorder;
                move_to_front(&mut self.realorder, 16, c);
                move_to_front(&mut self.realorder, 16, b);
                move_to_front(&mut self.realorder, 16, a);

                for _bit in 0..4 {
                    let cur = con as usize;
                    let invertbit = u32::from(self.context[cur].invert);
                    let (flag_lps, shift) = self.decode_symbol(cur);
                    self.update_context(cur, flag_lps, shift);
                    con = u32::from(MODE2_CONTEXT_TABLE[cur][(flag_lps ^ invertbit) as usize])
                        + if cur == 1 { refcon } else { 0 };
                }

                let pixel = self.realorder[((self.lps ^ self.inverts) & 0x0f) as usize];
                self.out1 = self
                    .out1
                    .wrapping_shl(4)
                    .wrapping_add((self.out0 >> 28) & 0x0f);
                self.out0 = self.out0.wrapping_shl(4).wrapping_add(pixel as u32);
            }

            let data = morton_4x8(self.out0);
            let b0 = (data >> 24) as u8;
            let b1 = (data >> 16) as u8;
            self.write(b0);
            self.write(b1);
            self.bitplanebuffer[self.buffer_index] = (data >> 8) as u8;
            self.buffer_index += 1;
            self.bitplanebuffer[self.buffer_index] = data as u8;
            self.buffer_index += 1;

            if self.buffer_index == 16 {
                for i in 0..16 {
                    let byte = self.bitplanebuffer[i];
                    self.write(byte);
                }
                self.buffer_index = 0;
            }
        }
    }

    /// Fold a decoded symbol's `flag_lps`/`shift` into the shared history and
    /// the adaptive context (invert toggle + probability-index evolution).
    /// Note the history update reads `context[con].invert` *before* the
    /// possible toggle, matching bsnes.
    fn update_context(&mut self, con: usize, flag_lps: u32, shift: u32) {
        self.lps = self.lps.wrapping_shl(1).wrapping_add(flag_lps);
        self.inverts = self
            .inverts
            .wrapping_shl(1)
            .wrapping_add(u32::from(self.context[con].invert));

        if (flag_lps & u32::from(self.toggle_invert(con))) != 0 {
            self.context[con].invert ^= 1;
        }
        if flag_lps != 0 {
            self.context[con].index = self.next_lps(con);
        } else if shift != 0 {
            self.context[con].index = self.next_mps(con);
        }
    }
}

/// First-symbol context selector shared by modes 1 and 2 (bsnes: `(a == b) ?
/// (b != c) : (b == c) ? 2 : 4 - (a == c)`).
fn pixel_context(a: i32, b: i32, c: i32) -> u32 {
    if a == b {
        (b != c) as u32
    } else if b == c {
        2
    } else {
        4 - (a == c) as u32
    }
}

/// Move `value` to the front of `order[..len]`, shifting the preceding entries
/// down one slot (the move-to-front pixel predictor). `value` is always present
/// for valid streams; if absent the array is left unchanged.
fn move_to_front(order: &mut [i32], len: usize, value: i32) {
    let mut m = 0;
    while m < len && order[m] != value {
        m += 1;
    }
    if m >= len {
        return;
    }
    while m > 0 {
        order[m] = order[m - 1];
        m -= 1;
    }
    order[0] = value;
}

/// `((i >> x) & 1) << y`: one term of the reverse-morton bit shuffle.
fn morton_bit(i: u32, x: u32, y: u32) -> u32 {
    ((i >> x) & 1) << y
}

/// De-interleave two 8-bit bitplanes from a 16-bit pixel word (bsnes
/// `morton_2x8`).
fn morton_2x8(data: u32) -> u32 {
    let lo = data & 0xff;
    let hi = (data >> 8) & 0xff;
    morton16_0(lo) + morton16_1(hi)
}

fn morton16_0(i: u32) -> u32 {
    morton_bit(i, 7, 11)
        + morton_bit(i, 6, 3)
        + morton_bit(i, 5, 10)
        + morton_bit(i, 4, 2)
        + morton_bit(i, 3, 9)
        + morton_bit(i, 2, 1)
        + morton_bit(i, 1, 8)
        + morton_bit(i, 0, 0)
}

fn morton16_1(i: u32) -> u32 {
    morton_bit(i, 7, 15)
        + morton_bit(i, 6, 7)
        + morton_bit(i, 5, 14)
        + morton_bit(i, 4, 6)
        + morton_bit(i, 3, 13)
        + morton_bit(i, 2, 5)
        + morton_bit(i, 1, 12)
        + morton_bit(i, 0, 4)
}

/// De-interleave four 8-bit bitplanes from a 32-bit pixel word (bsnes
/// `morton_4x8`).
fn morton_4x8(data: u32) -> u32 {
    morton32_0(data & 0xff)
        + morton32_1((data >> 8) & 0xff)
        + morton32_2((data >> 16) & 0xff)
        + morton32_3((data >> 24) & 0xff)
}

fn morton32_0(i: u32) -> u32 {
    morton_bit(i, 7, 25)
        + morton_bit(i, 6, 17)
        + morton_bit(i, 5, 9)
        + morton_bit(i, 4, 1)
        + morton_bit(i, 3, 24)
        + morton_bit(i, 2, 16)
        + morton_bit(i, 1, 8)
        + morton_bit(i, 0, 0)
}

fn morton32_1(i: u32) -> u32 {
    morton_bit(i, 7, 27)
        + morton_bit(i, 6, 19)
        + morton_bit(i, 5, 11)
        + morton_bit(i, 4, 3)
        + morton_bit(i, 3, 26)
        + morton_bit(i, 2, 18)
        + morton_bit(i, 1, 10)
        + morton_bit(i, 0, 2)
}

fn morton32_2(i: u32) -> u32 {
    morton_bit(i, 7, 29)
        + morton_bit(i, 6, 21)
        + morton_bit(i, 5, 13)
        + morton_bit(i, 4, 5)
        + morton_bit(i, 3, 28)
        + morton_bit(i, 2, 20)
        + morton_bit(i, 1, 12)
        + morton_bit(i, 0, 4)
}

fn morton32_3(i: u32) -> u32 {
    morton_bit(i, 7, 31)
        + morton_bit(i, 6, 23)
        + morton_bit(i, 5, 15)
        + morton_bit(i, 4, 7)
        + morton_bit(i, 3, 30)
        + morton_bit(i, 2, 22)
        + morton_bit(i, 1, 14)
        + morton_bit(i, 0, 6)
}

/// `{ prob, next_lps, next_mps, toggle_invert }`: bsnes `evolution_table`.
#[rustfmt::skip]
const EVOLUTION_TABLE: [[u8; 4]; 53] = [
    [0x5a,  1,  1, 1],
    [0x25,  6,  2, 0],
    [0x11,  8,  3, 0],
    [0x08, 10,  4, 0],
    [0x03, 12,  5, 0],
    [0x01, 15,  5, 0],

    [0x5a,  7,  7, 1],
    [0x3f, 19,  8, 0],
    [0x2c, 21,  9, 0],
    [0x20, 22, 10, 0],
    [0x17, 23, 11, 0],
    [0x11, 25, 12, 0],
    [0x0c, 26, 13, 0],
    [0x09, 28, 14, 0],
    [0x07, 29, 15, 0],
    [0x05, 31, 16, 0],
    [0x04, 32, 17, 0],
    [0x03, 34, 18, 0],
    [0x02, 35,  5, 0],

    [0x5a, 20, 20, 1],
    [0x48, 39, 21, 0],
    [0x3a, 40, 22, 0],
    [0x2e, 42, 23, 0],
    [0x26, 44, 24, 0],
    [0x1f, 45, 25, 0],
    [0x19, 46, 26, 0],
    [0x15, 25, 27, 0],
    [0x11, 26, 28, 0],
    [0x0e, 26, 29, 0],
    [0x0b, 27, 30, 0],
    [0x09, 28, 31, 0],
    [0x08, 29, 32, 0],
    [0x07, 30, 33, 0],
    [0x05, 31, 34, 0],
    [0x04, 33, 35, 0],
    [0x04, 33, 36, 0],
    [0x03, 34, 37, 0],
    [0x02, 35, 38, 0],
    [0x02, 36,  5, 0],

    [0x58, 39, 40, 1],
    [0x4d, 47, 41, 0],
    [0x43, 48, 42, 0],
    [0x3b, 49, 43, 0],
    [0x34, 50, 44, 0],
    [0x2e, 51, 45, 0],
    [0x29, 44, 46, 0],
    [0x25, 45, 24, 0],

    [0x56, 47, 48, 1],
    [0x4f, 47, 49, 0],
    [0x47, 48, 50, 0],
    [0x41, 49, 51, 0],
    [0x3c, 50, 52, 0],
    [0x37, 51, 43, 0],
];

/// `{ next 0, next 1 }`: bsnes `mode2_context_table`.
#[rustfmt::skip]
const MODE2_CONTEXT_TABLE: [[u8; 2]; 32] = [
    [ 1,  2],

    [ 3,  8],
    [13, 14],

    [15, 16],
    [17, 18],
    [19, 20],
    [21, 22],
    [23, 24],
    [25, 26],
    [25, 26],
    [25, 26],
    [25, 26],
    [25, 26],
    [27, 28],
    [29, 30],

    [31, 31],
    [31, 31],
    [31, 31],
    [31, 31],
    [31, 31],
    [31, 31],
    [31, 31],
    [31, 31],
    [31, 31],
    [31, 31],
    [31, 31],
    [31, 31],
    [31, 31],
    [31, 31],
    [31, 31],
    [31, 31],

    [31, 31],
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a data ROM: the directly-mapped 1 MiB program region, faked as
    /// `0x100000` bytes of zero, followed by the compressed payload at
    /// DATA-ROM offset 0.
    fn rom_with_payload(payload: &[u8]) -> (Arc<[u8]>, usize) {
        let base = 0x10_0000;
        let mut bytes = vec![0u8; base + payload.len()];
        bytes[base..].copy_from_slice(payload);
        let len = bytes.len();
        (Arc::from(bytes.into_boxed_slice()), len)
    }

    fn decompress(mode: u32, payload: &[u8], count: usize) -> Vec<u8> {
        let (rom, len) = rom_with_payload(payload);
        let mut d = Spc7110Decomp::new(rom, len);
        d.init(mode, 0, 0);
        (0..count).map(|_| d.read()).collect()
    }

    #[test]
    fn mode3_is_invalid_and_returns_zero() {
        let (rom, len) = rom_with_payload(&[0xAA; 32]);
        let mut d = Spc7110Decomp::new(rom, len);
        // Power-on state is mode 3.
        assert_eq!(d.mode(), 3);
        for _ in 0..8 {
            assert_eq!(d.read(), 0x00);
        }
    }

    #[test]
    fn dataread_wraps_modulo_data_rom_size() {
        // 0x100000 program + 4 payload bytes -> data-ROM size 4.
        let (rom, len) = rom_with_payload(&[0x11, 0x22, 0x33, 0x44]);
        let mut d = Spc7110Decomp::new(rom, len);
        d.offset = 0;
        assert_eq!(d.dataread(), 0x11);
        assert_eq!(d.dataread(), 0x22);
        assert_eq!(d.dataread(), 0x33);
        assert_eq!(d.dataread(), 0x44);
        // Wraps back to the first payload byte.
        assert_eq!(d.dataread(), 0x11);
    }

    #[test]
    fn morton_round_trips_known_values() {
        // All-zero and all-one pixel words map to the extremes.
        assert_eq!(morton_2x8(0x0000), 0x0000);
        assert_eq!(morton_2x8(0xFFFF), 0xFFFF);
        assert_eq!(morton_4x8(0x0000_0000), 0x0000_0000);
        assert_eq!(morton_4x8(0xFFFF_FFFF), 0xFFFF_FFFF);
        // A single set bit de-interleaves to a known plane/pixel position.
        // bit0 of the low byte -> morton16_0 map(0,0) -> bit0.
        assert_eq!(morton_2x8(0x0001), 0x0001);
        // bit0 of the high byte -> morton16_1 map(0,4) -> bit4.
        assert_eq!(morton_2x8(0x0100), 0x0010);
    }

    #[test]
    fn evolution_and_context_tables_have_expected_shape() {
        // Anchors verifying the tables were transcribed intact.
        assert_eq!(EVOLUTION_TABLE[0], [0x5a, 1, 1, 1]);
        assert_eq!(EVOLUTION_TABLE[52], [0x37, 51, 43, 0]);
        assert_eq!(MODE2_CONTEXT_TABLE[0], [1, 2]);
        assert_eq!(MODE2_CONTEXT_TABLE[1], [3, 8]);
        assert_eq!(MODE2_CONTEXT_TABLE[31], [31, 31]);
    }

    #[test]
    fn all_modes_produce_deterministic_output() {
        // Determinism guard: the exact bytes are whatever the faithful
        // port yields for this payload; two runs must yield the same 40.
        let payload: Vec<u8> = (0..64u16)
            .map(|i| (i.wrapping_mul(37) ^ 0x5b) as u8)
            .collect();
        for mode in 0..3u32 {
            let a = decompress(mode, &payload, 40);
            let b = decompress(mode, &payload, 40);
            assert_eq!(a, b, "mode {mode} is deterministic");
            assert_eq!(a.len(), 40);
        }
    }

    #[test]
    fn preskip_advances_past_initial_bytes() {
        let payload: Vec<u8> = (0..64u16)
            .map(|i| (i.wrapping_mul(29) ^ 0x17) as u8)
            .collect();
        let (rom, len) = rom_with_payload(&payload);

        // Full stream.
        let mut full = Spc7110Decomp::new(Arc::clone(&rom), len);
        full.init(0, 0, 0);
        let full_bytes: Vec<u8> = (0..24).map(|_| full.read()).collect();

        // Pre-skipping 8 bytes must equal the full stream from index 8.
        let mut skipped = Spc7110Decomp::new(rom, len);
        skipped.init(0, 0, 8);
        let skipped_bytes: Vec<u8> = (0..16).map(|_| skipped.read()).collect();

        assert_eq!(&full_bytes[8..24], &skipped_bytes[..]);
    }

    // ---- Independent oracle -------------------------------------------------
    //
    // A second, structurally-independent transcription of byuu's `decomp.cpp`
    // (using local `u32`/array state rather than the production struct fields),
    // used as a differential oracle: a transcription error in the production
    // port that the oracle does not share will diverge on random input.

    mod oracle {
        /// Compressed-source reader with the same modulo wrap as `dataread`.
        struct Src<'a> {
            data: &'a [u8],
            offset: usize,
        }
        impl<'a> Src<'a> {
            fn read(&mut self) -> u32 {
                let size = self.data.len();
                if size == 0 {
                    return 0;
                }
                while self.offset >= size {
                    self.offset -= size;
                }
                let d = self.data[self.offset] as u32;
                self.offset += 1;
                d
            }
        }

        #[derive(Clone, Copy, Default)]
        struct Ctx {
            index: usize,
            invert: u32,
        }

        pub struct Decoder<'a> {
            src: Src<'a>,
            mode: u32,
            buf: Vec<u8>,
            rd: usize,
            ctx: [Ctx; 32],
            val: u32,
            input: u32,
            span: u32,
            out: u32,
            out0: u32,
            out1: u32,
            inverts: u32,
            lps: u32,
            in_count: i32,
            pixelorder: [u32; 16],
            realorder: [u32; 16],
            bitplane: [u8; 16],
            buffer_index: usize,
        }

        fn ev(i: usize, j: usize) -> u32 {
            super::EVOLUTION_TABLE[i][j] as u32
        }

        impl<'a> Decoder<'a> {
            pub fn new(data: &'a [u8], mode: u32) -> Self {
                let mut d = Decoder {
                    src: Src { data, offset: 0 },
                    mode,
                    buf: Vec::new(),
                    rd: 0,
                    ctx: [Ctx::default(); 32],
                    val: 0,
                    input: 0,
                    span: 0,
                    out: 0,
                    out0: 0,
                    out1: 0,
                    inverts: 0,
                    lps: 0,
                    in_count: 0,
                    pixelorder: [0; 16],
                    realorder: [0; 16],
                    bitplane: [0; 16],
                    buffer_index: 0,
                };
                match mode {
                    0 => d.mode0(true),
                    1 => d.mode1(true),
                    2 => d.mode2(true),
                    _ => {}
                }
                d
            }

            pub fn read(&mut self) -> u8 {
                if self.rd == self.buf.len() {
                    self.buf.clear();
                    self.rd = 0;
                    match self.mode {
                        0 => self.mode0(false),
                        1 => self.mode1(false),
                        2 => self.mode2(false),
                        _ => return 0,
                    }
                }
                let d = self.buf[self.rd];
                self.rd += 1;
                d
            }

            fn symbol(&mut self, con: usize) -> (u32, u32) {
                let prob = ev(self.ctx[con].index, 0);

                // `val`/`span` are kept in `0..=0xff`, so `span - prob`
                // underflowing to a huge `u32` reproduces the C unsigned
                // comparison (val always wins -> MPS) exactly.
                let flag_lps = if self.val <= self.span.wrapping_sub(prob) {
                    self.span = self.span.wrapping_sub(prob) & 0xff;
                    0
                } else {
                    self.val = self
                        .val
                        .wrapping_sub(self.span.wrapping_sub(prob.wrapping_sub(1)))
                        & 0xff;
                    self.span = prob.wrapping_sub(1) & 0xff;
                    1
                };
                let mut shift = 0;
                while self.span < 0x7f {
                    shift += 1;
                    self.span = ((self.span << 1) + 1) & 0xff;
                    self.val = ((self.val << 1) + (self.input >> 7)) & 0xff;
                    self.input = (self.input << 1) & 0xff;
                    self.in_count -= 1;
                    if self.in_count == 0 {
                        self.input = self.src.read();
                        self.in_count = 8;
                    }
                }
                (flag_lps, shift)
            }

            fn update(&mut self, con: usize, flag_lps: u32, shift: u32) {
                self.lps = self.lps.wrapping_shl(1).wrapping_add(flag_lps);
                self.inverts = self
                    .inverts
                    .wrapping_shl(1)
                    .wrapping_add(self.ctx[con].invert);
                if flag_lps & ev(self.ctx[con].index, 3) != 0 {
                    self.ctx[con].invert ^= 1;
                }
                if flag_lps != 0 {
                    self.ctx[con].index = ev(self.ctx[con].index, 1) as usize;
                } else if shift != 0 {
                    self.ctx[con].index = ev(self.ctx[con].index, 2) as usize;
                }
            }

            fn mode0(&mut self, init: bool) {
                if init {
                    self.out = 0;
                    self.inverts = 0;
                    self.lps = 0;
                    self.span = 0xff;
                    self.val = self.src.read();
                    self.input = self.src.read();
                    self.in_count = 8;
                    return;
                }
                while self.buf.len() < 32 {
                    for bit in 0..8u32 {
                        let mask = (1u32 << (bit & 3)) - 1;
                        let mut con = mask + ((self.inverts & mask) ^ (self.lps & mask));
                        if bit > 3 {
                            con += 15;
                        }
                        let con = con as usize;
                        let mps = ((self.out >> 15) & 1) ^ self.ctx[con].invert;
                        let (flag_lps, shift) = self.symbol(con);
                        if flag_lps == 0 {
                            self.out = self.out.wrapping_shl(1).wrapping_add(mps);
                        } else {
                            self.out = self.out.wrapping_shl(1).wrapping_add(1).wrapping_sub(mps);
                        }
                        self.update(con, flag_lps, shift);
                    }
                    self.buf.push(self.out as u8);
                }
            }

            fn mtf(order: &mut [u32], len: usize, value: u32) {
                let mut m = 0;
                while m < len && order[m] != value {
                    m += 1;
                }
                if m >= len {
                    return;
                }
                while m > 0 {
                    order[m] = order[m - 1];
                    m -= 1;
                }
                order[0] = value;
            }

            fn context01(a: u32, b: u32, c: u32) -> u32 {
                if a == b {
                    (b != c) as u32
                } else if b == c {
                    2
                } else {
                    4 - (a == c) as u32
                }
            }

            fn mode1(&mut self, init: bool) {
                if init {
                    for i in 0..4 {
                        self.pixelorder[i] = i as u32;
                    }
                    self.out = 0;
                    self.inverts = 0;
                    self.lps = 0;
                    self.span = 0xff;
                    self.val = self.src.read();
                    self.input = self.src.read();
                    self.in_count = 8;
                    return;
                }
                while self.buf.len() < 32 {
                    for _pixel in 0..8 {
                        let a = (self.out >> 2) & 3;
                        let b = (self.out >> 14) & 3;
                        let c = (self.out >> 16) & 3;
                        let mut con = Self::context01(a, b, c);
                        Self::mtf(&mut self.pixelorder, 4, a);
                        for m in 0..4 {
                            self.realorder[m] = self.pixelorder[m];
                        }
                        Self::mtf(&mut self.realorder, 4, c);
                        Self::mtf(&mut self.realorder, 4, b);
                        Self::mtf(&mut self.realorder, 4, a);
                        for _ in 0..2 {
                            let cur = con as usize;
                            let (flag_lps, shift) = self.symbol(cur);
                            self.update(cur, flag_lps, shift);
                            con = 5 + (con << 1) + ((self.lps ^ self.inverts) & 1);
                        }
                        let pixel = self.realorder[((self.lps ^ self.inverts) & 3) as usize];
                        self.out = self.out.wrapping_shl(2).wrapping_add(pixel);
                    }
                    let data = super::morton_2x8(self.out);
                    self.buf.push((data >> 8) as u8);
                    self.buf.push(data as u8);
                }
            }

            fn mode2(&mut self, init: bool) {
                if init {
                    for i in 0..16 {
                        self.pixelorder[i] = i as u32;
                    }
                    self.buffer_index = 0;
                    self.out0 = 0;
                    self.out1 = 0;
                    self.inverts = 0;
                    self.lps = 0;
                    self.span = 0xff;
                    self.val = self.src.read();
                    self.input = self.src.read();
                    self.in_count = 8;
                    return;
                }
                while self.buf.len() < 32 {
                    for _pixel in 0..8 {
                        let a = self.out0 & 15;
                        let b = (self.out0 >> 28) & 15;
                        let c = self.out1 & 15;
                        let refcon = Self::context01(a, b, c);
                        let mut con = 0u32;
                        Self::mtf(&mut self.pixelorder, 16, a);
                        self.realorder = self.pixelorder;
                        Self::mtf(&mut self.realorder, 16, c);
                        Self::mtf(&mut self.realorder, 16, b);
                        Self::mtf(&mut self.realorder, 16, a);
                        for _ in 0..4 {
                            let cur = con as usize;
                            let invertbit = self.ctx[cur].invert;
                            let (flag_lps, shift) = self.symbol(cur);
                            self.update(cur, flag_lps, shift);
                            con = super::MODE2_CONTEXT_TABLE[cur][(flag_lps ^ invertbit) as usize]
                                as u32
                                + if cur == 1 { refcon } else { 0 };
                        }
                        let pixel = self.realorder[((self.lps ^ self.inverts) & 0x0f) as usize];
                        self.out1 = self
                            .out1
                            .wrapping_shl(4)
                            .wrapping_add((self.out0 >> 28) & 0x0f);
                        self.out0 = self.out0.wrapping_shl(4).wrapping_add(pixel);
                    }
                    let data = super::morton_4x8(self.out0);
                    self.buf.push((data >> 24) as u8);
                    self.buf.push((data >> 16) as u8);
                    self.bitplane[self.buffer_index] = (data >> 8) as u8;
                    self.buffer_index += 1;
                    self.bitplane[self.buffer_index] = data as u8;
                    self.buffer_index += 1;
                    if self.buffer_index == 16 {
                        for i in 0..16 {
                            self.buf.push(self.bitplane[i]);
                        }
                        self.buffer_index = 0;
                    }
                }
            }
        }
    }

    #[test]
    fn matches_independent_oracle_all_modes() {
        // Several pseudo-random payloads across all three modes must agree with
        // the independent transcription byte-for-byte.
        for seed in [0x1234_5678u32, 0xDEAD_BEEF, 0x0F0F_0F0F, 0x9E37_79B9] {
            let mut s = seed;
            let payload: Vec<u8> = (0..256)
                .map(|_| {
                    // xorshift32
                    s ^= s << 13;
                    s ^= s >> 17;
                    s ^= s << 5;
                    s as u8
                })
                .collect();

            for mode in 0..3u32 {
                let mut oracle = oracle::Decoder::new(&payload, mode);
                let (rom, len) = rom_with_payload(&payload);
                let mut port = Spc7110Decomp::new(rom, len);
                port.init(mode, 0, 0);

                let port_bytes: Vec<u8> = (0..200).map(|_| port.read()).collect();
                let oracle_bytes: Vec<u8> = (0..200).map(|_| oracle.read()).collect();
                assert_eq!(
                    port_bytes, oracle_bytes,
                    "mode {mode} seed {seed:#x} diverged from oracle"
                );
            }
        }
    }
}
