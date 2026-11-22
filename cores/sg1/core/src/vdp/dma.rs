//! DMA (Direct Memory Access) operations for VDP.
//!
//! The Genesis VDP supports three types of DMA:
//! 1. Memory to VRAM: Copy from 68k address space to VRAM
//! 2. VRAM Fill: Fill VRAM with a constant value
//! 3. VRAM Copy: Copy data within VRAM
//!
//! The VDP's side of a transfer is here too: starting it, running it (a
//! fill, a copy, a transfer from 68000 memory, a CRAM transfer spread over
//! the lines of the picture), its timing, and the stall it puts on the
//! 68000.

use super::recording::PaletteRedraw;
use super::writes::{MemoryTransfer, PaletteStore};
use super::{dma, DmaTransfer, ProgressiveCramDma, Vdp, MASTER_CLOCKS_PER_LINE};

/// DMA operation types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaOperation {
    /// Copy from 68k memory to VRAM.
    MemoryToVram {
        source: u32, // 68k address space
        dest: u16,   // VRAM address
        length: u16, // words
    },
    /// Fill VRAM with a constant value.
    VramFill { dest: u16, value: u16, length: u16 },
    /// Copy within VRAM.
    VramCopy { source: u16, dest: u16, length: u16 },
}

/// DMA state for the VDP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmaState {
    /// Current DMA operation (None if idle).
    pub operation: Option<DmaOperation>,
    /// Cycles remaining for current DMA.
    pub cycles_remaining: u32,
    /// Whether DMA is currently blocking the CPU.
    pub cpu_blocked: bool,
}

impl DmaState {
    /// Create a new idle DMA state.
    pub fn new() -> Self {
        Self {
            operation: None,
            cycles_remaining: 0,
            cpu_blocked: false,
        }
    }

    /// Check if DMA is active.
    pub fn is_active(&self) -> bool {
        self.operation.is_some()
    }

    /// Start a new DMA operation.
    pub fn start(&mut self, operation: DmaOperation, in_vblank: bool) {
        self.operation = Some(operation);

        // Calculate cycles based on operation type and timing
        self.cycles_remaining = match operation {
            DmaOperation::MemoryToVram { length, .. } => {
                // Memory to VRAM: 2 cycles/word during VBLANK, 16 cycles/word during active display
                if in_vblank {
                    (length as u32) * 2
                } else {
                    (length as u32) * 16
                }
            }
            DmaOperation::VramFill { length, .. } => {
                // VRAM Fill: 1 cycle/byte
                (length as u32) * 2 // length is in words, so * 2 for bytes
            }
            DmaOperation::VramCopy { length, .. } => {
                // VRAM Copy: 8 cycles/word
                (length as u32) * 8
            }
        };

        self.cpu_blocked = true;
    }

    /// Complete the current DMA operation.
    pub fn complete(&mut self) {
        self.operation = None;
        self.cycles_remaining = 0;
        self.cpu_blocked = false;
    }

    /// Execute DMA for the given number of cycles.
    /// Returns true if DMA completed.
    pub fn execute(&mut self, cycles: u32) -> bool {
        if !self.is_active() {
            return false;
        }

        if self.cycles_remaining <= cycles {
            self.complete();
            true
        } else {
            self.cycles_remaining -= cycles;
            false
        }
    }
}

impl Default for DmaState {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse DMA operation from VDP registers.
///
/// DMA registers (Genesis VDP):
/// - R19 (0x13): DMA Length Counter Low
/// - R20 (0x14): DMA Length Counter High
/// - R21 (0x15): DMA Source Address Low
/// - R22 (0x16): DMA Source Address Mid
/// - R23 (0x17): DMA Source Address High (bits 0-6) + DMA Type (bits 6-7);
///   bit 6 is an address bit when bit 7 is clear (a 68K-memory source)
///
/// DMA types (R23 bits 6-7):
/// - 00/01: 68K Memory to VRAM/CRAM/VSRAM
/// - 10: VRAM Fill
/// - 11: VRAM Copy
pub fn parse_dma_registers(registers: &[u8; 24], address: u16, _code: u8) -> Option<DmaOperation> {
    // Check if DMA is enabled (bit 4 of register 1)
    if (registers[1] & 0x10) == 0 {
        return None;
    }

    // Parse length from R19 (low) and R20 (high)
    let length = ((registers[20] as u16) << 8) | (registers[19] as u16);
    if length == 0 {
        return None;
    }

    // Check DMA mode from R23 bits 6-7
    let dma_mode = (registers[23] >> 6) & 0x03;

    match dma_mode {
        0b10 => {
            // VRAM Fill mode
            // In fill mode, the value to fill is the last value written to data port
            // We'll handle that in the VDP, so just return a fill operation
            Some(DmaOperation::VramFill {
                dest: address,
                value: 0, // Will be set by VDP from data port write
                length,
            })
        }
        0b11 => {
            // VRAM Copy mode
            // Source is from R21 (low) and R22 (mid)
            let source = ((registers[22] as u16) << 8) | (registers[21] as u16);
            Some(DmaOperation::VramCopy {
                source,
                dest: address,
                length,
            })
        }
        _ => {
            // Memory to VRAM mode (0b00, 0b01)
            // Source address from R21 (low), R22 (mid), R23 (high, bits 0-6)
            // These registers store the word address (byte address / 2)
            let addr_low = registers[21] as u32;
            let addr_mid = registers[22] as u32;
            let addr_high = (registers[23] & 0x7F) as u32; // Bit 6 is A23, bit 7 is mode bit

            // Reconstruct word address and convert to byte address
            let source = ((addr_high << 16) | (addr_mid << 8) | addr_low) << 1;

            // Check if this is a valid memory region
            // Genesis memory map: ROM 0x000000-0x3FFFFF, RAM 0xE00000-0xFFFFFF
            if source < 0x400000 || (0xE00000..=0xFFFFFF).contains(&source) {
                Some(DmaOperation::MemoryToVram {
                    source,
                    dest: address,
                    length,
                })
            } else {
                None
            }
        }
    }
}

/// Where a DMA from 68000 memory writes, from the VDP's access code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaTarget {
    Vram,
    Cram,
    Vsram,
    /// An access code that names none of the three: the transfer reads its
    /// source and writes nothing.
    Nowhere,
}

impl DmaTarget {
    /// The memory an access code's low four bits (CD3-CD0) select.
    pub fn from_access_code(code: u8) -> Self {
        match code & 0x0F {
            0x01 => Self::Vram,
            0x03 => Self::Cram,
            0x05 => Self::Vsram,
            _ => Self::Nowhere,
        }
    }
}

/// A DMA transfer from 68000 memory into the VDP, as the bus started it.
/// The instruction access log keeps one for each transfer an instruction's
/// write to the control port starts, so a tool can tell which instruction
/// copied which bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryDma {
    /// First source byte, a 68000 address.
    pub source: u32,
    /// Words copied (a length register of 0 copies 0x10000).
    pub words: u32,
    /// The memory written.
    pub target: DmaTarget,
    /// First address written in it.
    pub destination: u16,
}

impl MemoryDma {
    /// The source bytes read, as inclusive address spans: one, or two when
    /// the transfer wraps inside its 128 KB source window, whose base
    /// (register 23) a DMA never advances.
    pub fn source_spans(&self) -> Vec<(u32, u32)> {
        if self.words == 0 {
            return Vec::new();
        }
        let window_base = self.source & 0x00FE_0000;
        let start_word = (self.source & 0x1_FFFF) >> 1;
        let first_words = (0x1_0000 - start_word).min(self.words);
        let mut spans = vec![(
            window_base | (start_word << 1),
            window_base | ((start_word + first_words - 1) << 1) | 1,
        )];
        let rest = self.words - first_words;
        if rest > 0 {
            spans.push((window_base, window_base | ((rest - 1) << 1) | 1));
        }
        spans
    }
}

impl Vdp {
    /// Check if DMA is currently active (includes background VRAM Fill/Copy).
    pub fn is_dma_active(&self) -> bool {
        self.dma_busy || self.dma.is_active()
    }

    /// Check if a Memory-to-VRAM DMA is pending (needs CPU memory access).
    pub fn is_dma_pending(&self) -> bool {
        self.dma_pending
    }

    /// Execute pending Memory-to-VRAM DMA with a pre-read buffer of CPU memory.
    /// The buffer contains the bytes to be written, already read from the 68K address space.
    pub fn execute_pending_dma(&mut self, dma_buffer: &[u8]) {
        if !self.dma_pending {
            return;
        }
        self.dma_pending = false;

        let raw_length = ((self.registers[20] as u16) << 8) | (self.registers[19] as u16);
        // DMA length 0 means 65536 words (pre-decrementing counter on hardware)
        let length: u32 = if raw_length == 0 {
            0x10000
        } else {
            raw_length as u32
        };

        let auto_increment = self.registers[15] as u16;
        let dest_start = self.address;

        // Compute source address for tracking and post-DMA register update
        let addr_low = self.registers[21] as u32;
        let addr_mid = self.registers[22] as u32;
        // Use 7 bits of R23 (& 0x7F): bit 6 is the A23 address bit
        // DMA mode 00: A23=0 (ROM/cartridge space 0x000000-0x7FFFFF)
        // DMA mode 01: A23=1 (work RAM/IO space 0x800000-0xFFFFFF)
        let addr_high = (self.registers[23] & 0x7F) as u32;
        let source_start = ((addr_high << 16) | (addr_mid << 8) | addr_low) << 1;

        let code = self.code & 0x0F;
        // bus.rs pre-reads `length` words, so this normally equals `length`;
        // clamp to the buffer to stay in bounds.
        let words = (length as usize).min(dma_buffer.len() / 2);

        // A CRAM transfer issued while the beam is in active display is a raster
        // palette/backdrop effect (e.g. Direct Color DMA): meter it across the
        // remaining visible scanlines (see apply_progressive_cram_for_line)
        // instead of collapsing every per-line color into one final value.
        // Everything else (all VRAM/VSRAM, and any CRAM DMA during blanking,
        // which is what ordinary games do) lands instantly here.
        let progressive = code == 0x03
            && !self.timing.in_vblank
            && self.timing.scanline < self.timing.visible_scanlines();

        if progressive {
            // Words per line to CRAM: the blank rate applies when
            // the display is forcibly off (reg1 bit6=0), which is exactly how
            // Direct Color DMA streams a full picture in one frame, and the
            // active rate applies with the display on. H40 (RS0=reg12 bit0) is
            // wider than H32. Blank rates are the refresh-adjusted CRAM values
            // (204->198 H40, 166->161 H32).
            let h40 = (self.registers[12] & 0x01) != 0;
            let display_off = (self.registers[1] & 0x40) == 0;
            let rate = match (display_off, h40) {
                (true, true) => 198,
                (true, false) => 161,
                (false, true) => 18,
                (false, false) => 16,
            };
            // ~83% into the line's slot group (164/198), where the backdrop
            // stands when Genesis Plus GX draws the line (see `sample_offset`).
            let sample_offset = (rate * 164) / 198;
            self.progressive_cram = Some(ProgressiveCramDma {
                buffer: dma_buffer[..words * 2].to_vec(),
                dest_start,
                auto_inc: auto_increment,
                rate,
                sample_offset,
                length: words,
                cursor: 0,
            });
            // The whole transfer still streams through the FIFO, so the last
            // words remain the open-bus source for any post-DMA read.
            for i in 0..words {
                let bo = i * 2;
                self.push_fifo_word(((dma_buffer[bo] as u16) << 8) | (dma_buffer[bo + 1] as u16));
            }
            // The 68000 is frozen for the whole 68k-bus DMA, so from its point of
            // view the transfer completes atomically: a CRAM data-port read-back
            // taken right after it (VDPFIFOTesting "DMA Transfer to CRAM ...")
            // must observe the FINAL palette. Commit the logical CRAM now. The
            // per-line appliers below (`apply_progressive_cram_for_line` /
            // `render_dcdma_line_beamx`) only re-tint the *render* table
            // (`cram_rgb`) for the raster effect; they deliberately leave this
            // read-back state (`cram`) at its final value.
            let mut commit_dest = dest_start;
            for i in 0..words {
                let bo = i * 2;
                let word = ((dma_buffer[bo] as u16) << 8) | (dma_buffer[bo + 1] as u16);
                let index = ((commit_dest & 0x7E) >> 1) as usize;
                if index < 64 {
                    self.cram[index] = word & 0x0EEE;
                }
                commit_dest = commit_dest.wrapping_add(auto_increment);
            }
        } else {
            if code == 0x01 {
                self.note_memory_transfer_start(dest_start, auto_increment, words);
            }
            let mut dest = dest_start;
            for i in 0..words {
                let byte_offset = i * 2;
                let word =
                    ((dma_buffer[byte_offset] as u16) << 8) | (dma_buffer[byte_offset + 1] as u16);

                // Each DMA word passes through the FIFO, like a data-port
                // write, so it becomes the open-bus source for any read
                // issued after the transfer.
                self.push_fifo_word(word);

                match code {
                    0x01 => {
                        // VRAM write: an odd address swaps the word's bytes (as in MAME)
                        let raw_addr = dest as usize;
                        let addr = raw_addr & 0xFFFE; // Word-align
                        if addr + 1 < self.vram.len() {
                            if (raw_addr & 1) != 0 {
                                self.vram[addr] = (word & 0xFF) as u8;
                                self.vram[addr + 1] = (word >> 8) as u8;
                            } else {
                                self.vram[addr] = (word >> 8) as u8;
                                self.vram[addr + 1] = (word & 0xFF) as u8;
                            }
                            self.trace_vram_write(addr as u16, word, "dma-mem");
                        }
                    }
                    0x03 => {
                        // CRAM write (blanking path, instant)
                        let index = ((dest & 0x7E) >> 1) as usize;
                        if index < 64 {
                            let masked = word & 0x0EEE;
                            self.cram[index] = masked;
                            self.set_palette_colour(index, masked);
                            self.note_palette_change(
                                index,
                                masked,
                                PaletteRedraw::Nothing,
                                PaletteStore::Transfer,
                            );
                        }
                    }
                    0x05 => {
                        // VSRAM write
                        let index = ((dest & 0x7E) >> 1) as usize;
                        if index < 64 {
                            self.vsram[index] = word & 0x07FF;
                        }
                    }
                    _ => {}
                }
                dest = dest.wrapping_add(auto_increment);
            }
        }

        // Registers reflect the completed transfer regardless of path: the 68000
        // is frozen for the DMA's full duration, so it never observes the
        // intermediate state of a metered CRAM transfer.
        self.address = dest_start.wrapping_add((words as u16).wrapping_mul(auto_increment));

        // Clear DMA length registers after completion (per MAME)
        self.registers[19] = 0;
        self.registers[20] = 0;

        // Update the DMA source registers (21/22) with the post-transfer value.
        // The source advances inside a fixed 128 KB window: only the low 16
        // bits of the word address count, so the counter wraps at 0x10000
        // while register 23, the window base, is NEVER modified by the
        // transfer (checked against Genesis Plus GX). So when a transfer's source
        // crosses a 128 KB boundary, registers 21/22 wrap to the start of the
        // window (VDPFIFOTesting "DMA source register update / wrapping").
        let orig_word_src = ((self.registers[22] as u32) << 8) | (self.registers[21] as u32);
        let new_word_src = orig_word_src.wrapping_add(words as u32) & 0xFFFF;
        self.registers[21] = (new_word_src & 0xFF) as u8;
        self.registers[22] = ((new_word_src >> 8) & 0xFF) as u8;
        // Reconstruct the full byte source only for the debug bookkeeping below.
        let source = (source_start & !0x1FFFF) | ((new_word_src << 1) & 0x1FFFF);

        // Bookkeeping for the instant data copy (kept for debug inspection).
        let raw_length_u16 = raw_length;
        self.dma.start(
            dma::DmaOperation::MemoryToVram {
                source,
                dest: self.address,
                length: raw_length_u16,
            },
            self.in_vblank(),
        );
        self.dma.complete();

        // The bytes land instantly above, but a 68k-bus DMA freezes the 68000
        // for the transfer's duration on real hardware. Record the master-clock
        // cost (a word to CRAM/VSRAM, or to VRAM) so the frame loop drains it
        // into the per-line CPU budget. No DMA-busy bit is asserted for 68k-bus
        // DMA: the CPU is halted instead, so it can never observe one.
        let transfer = self.dma_bus_transfer();
        self.pending_cpu_dma_mclks += self.dma_busy_master_clocks(transfer, length);

        self.note_memory_transfer(MemoryTransfer {
            source: source_start,
            destination: dest_start,
            step: auto_increment,
            words,
            code,
        });
    }

    /// Advance a deferred mid-active CRAM DMA up to the slot that line `line`
    /// renders with (the picture was checked against Genesis Plus GX).
    /// The transfer runs at `rate` words/line (the 68000 is frozen for its whole
    /// duration), and the line shows the backdrop as of `line*rate +
    /// sample_offset`, ~83% across the line rather
    /// than on the black right-edge word that ends each picture row. Applying
    /// every word up to that slot keeps non-backdrop CRAM indices correct for
    /// auto-incrementing transfers too. True per-pixel fidelity (PicoDrive)
    /// would need cycle-accurate mid-line CRAM painting, beyond a scanline
    /// renderer.
    pub(super) fn apply_progressive_cram_for_line(&mut self, line: u16) {
        let mut prog = match self.progressive_cram.take() {
            Some(p) => p,
            None => return,
        };

        let target = ((line as usize) * prog.rate + prog.sample_offset + 1).min(prog.length);
        while prog.cursor < target {
            let byte_offset = prog.cursor * 2;
            if byte_offset + 1 >= prog.buffer.len() {
                prog.cursor = prog.length; // buffer exhausted; nothing left to apply
                break;
            }
            let word =
                ((prog.buffer[byte_offset] as u16) << 8) | (prog.buffer[byte_offset + 1] as u16);
            // dest wraps mod 65536; (cursor as u16) preserves the value mod 65536.
            let dest = prog
                .dest_start
                .wrapping_add((prog.cursor as u16).wrapping_mul(prog.auto_inc));
            let index = ((dest & 0x7E) >> 1) as usize;
            if index < 64 {
                // Only re-tint the render table; `cram` (read-back state) was
                // already committed to its final value when the DMA executed.
                self.set_palette_colour(index, word & 0x0EEE);
            }
            prog.cursor += 1;
        }

        // Keep the transfer alive while later visible lines still draw from it;
        // it is re-armed every frame by the demo, so this never leaks.
        if prog.cursor < prog.length {
            self.progressive_cram = Some(prog);
        }
    }

    /// Paint one Direct-Color-DMA line straight from the DMA source buffer,
    /// approximating per-pixel CRAM by beam-X: with the display off the whole
    /// line is backdrop, each DMA word shows for two pixels, the source
    /// advances one word per pair, and the words that land during horizontal
    /// sync (38 in H40, 33 in H32) are skipped at the line end, which sums to
    /// the same per-line word `rate` SG1 already meters. (PicoDrive shows these
    /// pictures the same way.)
    /// This resolves the streamed picture (the spaceship) that a whole-line
    /// remap can only render as horizontal bands, with no cycle-accurate CPU.
    ///
    /// The line's every row of the picture shows the same colours: in
    /// interlace mode 2 both fields' rows ([`Vdp::line_rows`]).
    pub(super) fn render_dcdma_line_beamx(&mut self, line: u16) {
        let mut prog = match self.progressive_cram.take() {
            Some(p) => p,
            None => {
                self.render_line(line);
                return;
            }
        };

        let width = self.display_width() as usize;
        let internal_width = 320usize;
        let rows = self.line_rows(line);
        // The first word of this line sits at the running cursor. Two pixels per
        // word.
        let base = prog.cursor;
        let backdrop = self.backdrop_rgb();
        if self.recording.is_some() {
            let painted = (base..base + width.div_ceil(2))
                .take_while(|&slot| slot * 2 + 1 < prog.buffer.len())
                .map(|slot| {
                    u16::from_be_bytes([prog.buffer[slot * 2], prog.buffer[slot * 2 + 1]]) & 0x0EEE
                });
            self.record_direct_colours(line, painted);
        }
        for x in 0..width {
            let slot = base + (x / 2);
            let bo = slot * 2;
            let (r, g, b) = if bo + 1 < prog.buffer.len() {
                let word = ((prog.buffer[bo] as u16) << 8) | (prog.buffer[bo + 1] as u16);
                Self::convert_genesis_color(word & 0x0EEE)
            } else {
                backdrop
            };
            for row in rows.clone() {
                let idx = (row * internal_width + x) * 4;
                if idx + 3 < self.framebuffer.len() {
                    self.framebuffer[idx] = r;
                    self.framebuffer[idx + 1] = g;
                    self.framebuffer[idx + 2] = b;
                    self.framebuffer[idx + 3] = 255;
                }
            }
        }
        // Keep the index buffers consistent (backdrop) so any later mid-line
        // remap on this line stays well-defined rather than reusing a stale
        // composite, as `render_blanked_line` does.
        let bd_idx = self.backdrop_index().min(63) as u8;
        for x in 0..width {
            self.line_index[x] = bd_idx;
            self.line_sh_op[x] = 0;
            self.line_high_pri[x] = false;
            self.line_plane_a[x] = -1;
            self.line_window[x] = -1;
            self.line_plane_b[x] = -1;
        }

        // Advance the running CRAM state by this line's whole word group (painted
        // words + HSYNC words = `rate`), applying auto-increment so reads and the
        // next line start from the correct CRAM, exactly like the band path.
        let end = (base + prog.rate).min(prog.length);
        while prog.cursor < end {
            let bo = prog.cursor * 2;
            if bo + 1 >= prog.buffer.len() {
                prog.cursor = prog.length;
                break;
            }
            let word = ((prog.buffer[bo] as u16) << 8) | (prog.buffer[bo + 1] as u16);
            let dest = prog
                .dest_start
                .wrapping_add((prog.cursor as u16).wrapping_mul(prog.auto_inc));
            let index = ((dest & 0x7E) >> 1) as usize;
            if index < 64 {
                // Render table only; `cram` read-back state stays at its
                // DMA-final value (committed in `execute_pending_dma`).
                self.set_palette_colour(index, word & 0x0EEE);
                self.note_palette_change(
                    index,
                    word & 0x0EEE,
                    PaletteRedraw::Nothing,
                    PaletteStore::Transfer,
                );
            }
            prog.cursor += 1;
        }

        if prog.cursor < prog.length {
            self.progressive_cram = Some(prog);
        }
    }

    /// Get DMA state (for debugging).
    pub fn dma_state(&self) -> &dma::DmaState {
        &self.dma
    }

    /// Start a DMA operation based on current registers and control word.
    pub fn start_dma(&mut self, cpu_memory: &[u8]) {
        // Parse DMA operation from registers
        if let Some(mut operation) =
            dma::parse_dma_registers(&self.registers, self.address, self.code)
        {
            // For VRAM fill, use the last value written to data port
            if let dma::DmaOperation::VramFill { ref mut value, .. } = operation {
                *value = self.dma_fill_value;
            }

            // Execute the DMA operation immediately
            match operation {
                dma::DmaOperation::MemoryToVram {
                    source,
                    dest,
                    length,
                } => {
                    self.execute_dma_memory_to_vram(source, dest, length, cpu_memory);
                }
                dma::DmaOperation::VramFill {
                    dest,
                    value,
                    length,
                } => {
                    self.execute_dma_vram_fill(dest, value, length as u32);
                }
                dma::DmaOperation::VramCopy {
                    source,
                    dest,
                    length,
                } => {
                    self.execute_dma_vram_copy(source, dest, length as u32);
                }
            }

            // Start DMA state tracking
            self.dma.start(operation, self.in_vblank());
        }
    }

    /// Execute Memory to VRAM DMA.
    pub(super) fn execute_dma_memory_to_vram(
        &mut self,
        source: u32,
        dest: u16,
        length: u16,
        cpu_memory: &[u8],
    ) {
        let auto_increment = self.registers[15] as u16;
        let mut addr = dest;
        let mut src = source as usize;

        for _ in 0..length {
            if src + 1 < cpu_memory.len() {
                let high = cpu_memory[src];
                let low = cpu_memory[src + 1];

                let vram_addr = (addr as usize) & 0xFFFE;
                if vram_addr + 1 < self.vram.len() {
                    if (addr as usize & 1) != 0 {
                        // Odd destination: byte-swap
                        self.vram[vram_addr] = low;
                        self.vram[vram_addr + 1] = high;
                    } else {
                        self.vram[vram_addr] = high;
                        self.vram[vram_addr + 1] = low;
                    }
                    self.trace_vram_write(
                        vram_addr as u16,
                        ((high as u16) << 8) | (low as u16),
                        "dma-mem-old",
                    );
                }

                src += 2;
                addr = addr.wrapping_add(auto_increment);
            } else {
                break;
            }
        }

        self.address = addr;
    }

    /// Source triple `(fill_byte, cram_word, vsram_word)` for a fill triggered
    /// by data-port word `value`. VRAM replicates the *newest* FIFO entry's high
    /// byte (which equals `value >> 8`, since
    /// the triggering write pushed `value` last). CRAM/VSRAM do NOT use the
    /// just-written word: they replicate the *oldest* FIFO entry
    /// (checked against Genesis Plus GX). The initial cell still holds
    /// the written value (deposited by the data-port write before the fill);
    /// only the trailing cells take the FIFO ring's oldest entry.
    fn fill_source_words(&self, value: u16) -> (u8, u16, u16) {
        let fifo_oldest = self.fifo[self.fifo_idx];
        (
            (value >> 8) as u8,
            fifo_oldest & 0x0EEE,
            fifo_oldest & 0x07FF,
        )
    }

    /// Source triple for the *metered* fill, re-read live from the FIFO ring on
    /// every cell: the newest entry for VRAM, the oldest for CRAM/VSRAM (the
    /// documented hardware quirk). A data-port write that lands
    /// mid-fill pushes a new entry and the *rest* of the fill picks up the just
    /// written value. At the trigger this equals `fill_source_words(value)`
    /// (that write was the newest entry); after a concurrent write it tracks the
    /// new value, VDPFIFOTesting "DP Writes During DMA Fill".
    pub(super) fn fill_source_words_live(&self) -> (u8, u16, u16) {
        let newest = self.fifo[(self.fifo_idx + 3) & 3];
        let oldest = self.fifo[self.fifo_idx];
        ((newest >> 8) as u8, oldest & 0x0EEE, oldest & 0x07FF)
    }

    /// Number of fill cells the real chip would have written between the
    /// triggering data-port write and the point a back-to-back follow-up access
    /// observes: the trigger line's remaining slots, served at once. In VBLANK
    /// the whole transfer lands now. On a forced-blank active line only
    /// the rest-of-line chunk lands now and the remainder streams from
    /// `tick_dma`. A fill starts two slots after its trigger, hence the
    /// 2-cell trim (the figure Genesis Plus GX uses).
    pub(super) fn fill_initial_chunk(&self, length: u32) -> u64 {
        let len = length as u64;
        let display_on = (self.registers[1] & 0x40) != 0;
        let blanking = self.timing.in_vblank || !display_on;
        if self.timing.in_vblank || self.beam_cycles_per_line == 0 {
            return len;
        }
        let rate = self.dma_line_rate(DmaTransfer::Fill, blanking);
        let per_line = self.beam_cycles_per_line as u64;
        let into = (self.beam_cycles_into_line as u64).min(per_line);
        let cells_to_eol = (per_line - into) * rate / per_line;
        cells_to_eol.saturating_sub(2).min(len)
    }

    /// Write one fill cell at the live `self.address` (target chosen by the live
    /// `self.code`), then advance `self.address` by the auto-increment. Shared by
    /// the instant `execute_dma_vram_fill` (the `start_dma` path and unit tests)
    /// and the metered `advance_fill` (the `write_data_port` path) so both emit
    /// byte-for-byte identical cells. Reading address/code live is what lets a
    /// control-port write redirect a fill in flight (VDPFIFOTesting "DMA Fill
    /// Control Port Writes").
    fn fill_one_cell(&mut self, fill_byte: u8, cram_word: u16, vsram_word: u16) {
        let auto_increment = self.registers[15] as u16;
        let addr = self.address;
        match self.code & 0x0F {
            0x01 => {
                // VRAM fill: write high byte
                let vram_addr = addr as usize;
                self.note_fill_vram_cell(vram_addr, fill_byte);
                if (vram_addr & 1) != 0 {
                    let word_addr = vram_addr ^ 1;
                    if word_addr < self.vram.len() {
                        self.vram[word_addr] = fill_byte;
                    }
                } else if vram_addr + 1 < self.vram.len() {
                    self.vram[vram_addr + 1] = fill_byte;
                }
                let wa = vram_addr & 0xFFFE;
                if wa + 1 < self.vram.len() {
                    let w = ((self.vram[wa] as u16) << 8) | (self.vram[wa + 1] as u16);
                    self.trace_vram_write(wa as u16, w, "dma-fill");
                }
            }
            0x03 => {
                // CRAM fill: source is the oldest FIFO entry, not `value`.
                let index = ((addr & 0x7E) >> 1) as usize;
                if index < 64 {
                    self.cram[index] = cram_word;
                    self.set_palette_colour(index, cram_word);
                    self.note_palette_change(
                        index,
                        cram_word,
                        PaletteRedraw::Nothing,
                        PaletteStore::FillCell,
                    );
                }
            }
            0x05 => {
                // VSRAM fill: source is the oldest FIFO entry, not `value`.
                let index = ((addr & 0x7E) >> 1) as usize;
                if index < 64 {
                    self.vsram[index] = vsram_word;
                    self.note_fill_vsram_cell(index);
                }
            }
            _ => {
                // Invalid target: the address still advances.
            }
        }
        self.address = addr.wrapping_add(auto_increment);
    }

    /// DMA source-address registers (21/22) are incremented during a DMA *even
    /// for a Fill*, which never reads the source. On completion they have
    /// advanced by the length-register value, and the length registers read
    /// zero. VDPFIFOTesting "DMA Fill Source Reg Update" checks this (and
    /// Genesis Plus GX does the same).
    pub(super) fn dma_fill_source_writeback(&mut self) {
        let length_reg = ((self.registers[20] as u16) << 8) | (self.registers[19] as u16);
        let src = ((self.registers[22] as u16) << 8) | (self.registers[21] as u16);
        let end_src = src.wrapping_add(length_reg);
        self.registers[21] = (end_src & 0xFF) as u8;
        self.registers[22] = ((end_src >> 8) & 0xFF) as u8;
        // Clear DMA length registers after completion (per MAME)
        self.registers[19] = 0;
        self.registers[20] = 0;
    }

    /// Execute VRAM Fill DMA **instantly** (the `start_dma` path and the direct
    /// unit tests). The metered `write_data_port` path uses `advance_fill`
    /// instead. Fills with the high byte of `value`; supports VRAM/CRAM/VSRAM.
    /// Uses inclusive loop count (length + 1 iterations) per MAME behavior.
    pub(super) fn execute_dma_vram_fill(&mut self, dest: u16, value: u16, length: u32) {
        self.address = dest;
        let (fill_byte, cram_word, vsram_word) = self.fill_source_words(value);

        // Fill loop: length + 1 iterations, as in MAME (James Pond 3 depends on it).
        for _ in 0..=length {
            self.fill_one_cell(fill_byte, cram_word, vsram_word);
        }

        self.dma_fill_source_writeback();
    }

    /// Stream up to `max_cells` pending fill cells through the live address/code
    /// (the metered counterpart of `execute_dma_vram_fill`'s loop). On
    /// completion clears `fill_state`; the source-register writeback and
    /// length-clear already ran at trigger in `write_data_port`.
    pub(super) fn advance_fill(&mut self, max_cells: u64) {
        let Some(state) = self.fill_state else {
            return;
        };
        let n = max_cells.min(state.remaining as u64) as u32;
        for _ in 0..n {
            let (fill_byte, cram_word, vsram_word) = self.fill_source_words_live();
            self.fill_one_cell(fill_byte, cram_word, vsram_word);
        }
        let remaining = state.remaining - n;
        if remaining == 0 {
            self.fill_state = None;
            self.note_fill_end();
        } else if let Some(s) = self.fill_state.as_mut() {
            s.remaining = remaining;
        }
    }

    /// Execute VRAM Copy DMA.
    pub(super) fn execute_dma_vram_copy(&mut self, source: u16, dest: u16, length: u32) {
        let auto_increment = self.registers[15] as u16;
        let mut src = source;
        let mut dst = dest;

        self.note_copy_start(dest, auto_increment, length as usize);

        // Copy byte-by-byte IN PLACE (read one byte, immediately write it, then
        // advance), NOT through a temp buffer. Overlapping src/dst regions must
        // propagate: a byte just written can become the source for a later
        // iteration, which is what the VDPFIFOTesting "DMA Copy" overlap
        // subtests verify. Source advances by one byte; destination advances by
        // the auto-increment register (checked against Genesis Plus GX).
        for _ in 0..length {
            let src_addr = (src ^ 1) as usize; // VRAM is kept byte-swapped
            let byte = if src_addr < self.vram.len() {
                self.vram[src_addr]
            } else {
                0
            };

            let dst_addr = (dst ^ 1) as usize; // VRAM is kept byte-swapped
            if dst_addr < self.vram.len() {
                self.vram[dst_addr] = byte;
                let wa = dst_addr & 0xFFFE;
                if wa + 1 < self.vram.len() {
                    let w = ((self.vram[wa] as u16) << 8) | (self.vram[wa + 1] as u16);
                    self.trace_vram_write(wa as u16, w, "dma-copy");
                }
            }

            src = src.wrapping_add(1);
            dst = dst.wrapping_add(auto_increment);
        }
        self.note_copy(source, dest, auto_increment, length as usize);

        self.address = dst;

        // Clear DMA length registers after completion (per MAME)
        self.registers[19] = 0;
        self.registers[20] = 0;

        // Update source address registers
        self.registers[21] = (src & 0xFF) as u8;
        self.registers[22] = ((src >> 8) & 0xFF) as u8;
    }

    /// Execute DMA for a given number of cycles.
    /// Returns true if DMA completed.
    pub fn execute_dma_cycles(&mut self, cycles: u32) -> bool {
        self.dma.execute(cycles)
    }

    /// Single-rate reference for the DMA duration of `length` length-register
    /// units: `length * 3420 / rate`, the rate evaluated once at the
    /// current beam state. The emulator uses the beam-aware
    /// [`Self::dma_busy_master_clocks`] (which integrates the rate as the beam
    /// crosses active display into VBLANK); this single-rate formula is what
    /// the rate-model unit tests assert against.
    #[cfg(test)]
    pub(super) fn dma_master_clocks(&self, transfer: DmaTransfer, length: u32) -> u64 {
        let display_on = (self.registers[1] & 0x40) != 0;
        let blanking = self.timing.in_vblank || !display_on;
        let rate = self.dma_line_rate(transfer, blanking);
        let len = if length == 0 { 0x10000 } else { length as u64 };
        len * MASTER_CLOCKS_PER_LINE / rate
    }

    /// Units of the DMA length counter that one scanline carries, for a
    /// transfer of the given kind, while the line is `blanking` (vertical
    /// blank, or the display forced off) or drawing the picture.
    ///
    /// A line offers the transfer 16 access slots in H32 and 18 in H40
    /// while the picture is drawn, and 166 or 204 while it is blanked. A
    /// fill writes a byte in each. A word to VRAM takes two, and so does a
    /// copied byte (a read and a write), so those move half as many units.
    /// A word to CRAM or VSRAM takes one, and a blanked line carries 161 or
    /// 198 of them, the memory refresh taking five or six slots. (The
    /// counts are the model Genesis Plus GX uses; completion times were
    /// checked against it.)
    pub(super) fn dma_line_rate(&self, transfer: DmaTransfer, blanking: bool) -> u64 {
        let h40 = (self.registers[12] & 0x01) != 0;
        let slots: u64 = match (blanking, h40) {
            (false, false) => 16,
            (false, true) => 18,
            (true, false) => 166,
            (true, true) => 204,
        };
        match transfer {
            DmaTransfer::Fill => slots,
            DmaTransfer::BusToVram | DmaTransfer::Copy => slots / 2,
            DmaTransfer::BusToCramOrVsram if !blanking => slots,
            DmaTransfer::BusToCramOrVsram if h40 => 198,
            DmaTransfer::BusToCramOrVsram => 161,
        }
    }

    /// Master clocks a DMA transfer of `length` length-register units occupies
    /// **from the current beam position**, adding up the rate of each line
    /// it crosses instead of locking one rate for the whole transfer.
    ///
    /// The access rate belongs to the line the beam is on: a
    /// transfer kicked off during active display drains at the slow active
    /// rate only until the beam reaches VBLANK, where the ~10x faster blanking
    /// rate empties the remainder. The naive `length * 3420 / rate` (what
    /// [`Self::dma_master_clocks`] returns) assumes the trigger-time rate holds
    /// for the entire transfer, which overstates the DMA-busy window by up to
    /// ~10x for a large fill started mid-active-display with the display on.
    /// Contra Hard Corps' boot VRAM-clear fills (`0x1FFF` units, display on,
    /// lines ~133/202) would each stall for ~1.7 frames that way, and its
    /// attract demo would part from Genesis Plus GX's. Walking the scanlines
    /// gives the completion time that emulator arrives at.
    pub(super) fn dma_busy_master_clocks(&self, transfer: DmaTransfer, length: u32) -> u64 {
        let display_on = (self.registers[1] & 0x40) != 0;
        let total_lines = (self.timing.total_scanlines() as u32).max(1);
        let active_lines = self.timing.visible_scanlines() as u32;
        let mut remaining = if length == 0 {
            0x10000u64
        } else {
            length as u64
        };
        let mut mclks: u64 = 0;
        let mut line = self.timing.scanline as u32 % total_lines;
        // A 0x10000 unit transfer at the slowest active rate spans a few
        // hundred lines; cap the walk generously so a pathological state can
        // never spin forever, then approximate any residual tail.
        for _ in 0..(total_lines * 64) {
            let blanking = !display_on || line >= active_lines;
            let rate = self.dma_line_rate(transfer, blanking);
            if remaining <= rate {
                mclks += remaining * MASTER_CLOCKS_PER_LINE / rate;
                remaining = 0;
                break;
            }
            remaining -= rate;
            mclks += MASTER_CLOCKS_PER_LINE;
            line += 1;
            if line >= total_lines {
                line = 0;
            }
        }
        if remaining > 0 {
            mclks += remaining * MASTER_CLOCKS_PER_LINE / self.dma_line_rate(transfer, true);
        }
        mclks
    }

    /// What a 68k-bus transfer to the current destination moves: words to
    /// CRAM or VSRAM (either of the code's bits 1 and 2 set), or to VRAM.
    fn dma_bus_transfer(&self) -> DmaTransfer {
        if (self.code & 0x06) != 0 {
            DmaTransfer::BusToCramOrVsram
        } else {
            DmaTransfer::BusToVram
        }
    }

    /// Advance a background VRAM Fill/Copy by `master_clocks` and drop the
    /// DMA-busy status bit once the metered duration elapses. A Copy writes its
    /// data instantly, so the busy bit is a pure duration timer; a Fill streams
    /// its cells here proportionally to elapsed clocks (`fill_state`) so a
    /// data/control-port access that lands mid-fill observes the partially
    /// advanced address. Either way
    /// games that poll the busy bit see it asserted for the whole window, and a
    /// fill that nobody observes still completes (and writes every cell) by the
    /// time the duration runs out. 68k-bus DMA does not use this path; it
    /// stalls the CPU via `pending_cpu_dma_mclks`.
    pub fn tick_dma(&mut self, master_clocks: u64) {
        if !self.dma_busy {
            return;
        }
        // A fill armed by the control-port write but not yet started by its
        // data-port write holds the busy bit with the `u64::MAX` sentinel.
        // The transfer is not running
        // yet, so don't drain time against it; the data write installs the
        // real duration.
        if self.dma_remaining_clocks == u64::MAX {
            return;
        }
        self.dma_remaining_clocks = self.dma_remaining_clocks.saturating_sub(master_clocks);

        // Stream the in-flight fill. The per-tick cell count uses the same
        // fill rate (`dma_line_rate(DmaTransfer::Fill, ..)`) the busy duration was derived from,
        // so the two drain in step; when the duration runs out, flush whatever
        // is left so the cells and the busy timer always finish together (keeps
        // the post-fill address/VRAM final for a non-observing caller).
        if self.fill_state.is_some() {
            if self.dma_remaining_clocks == 0 {
                let left = self.fill_state.map_or(0, |s| s.remaining as u64);
                self.advance_fill(left);
            } else {
                let display_on = (self.registers[1] & 0x40) != 0;
                let blanking = self.timing.in_vblank || !display_on;
                let rate = self.dma_line_rate(DmaTransfer::Fill, blanking);
                let mut accum = self.fill_state.map_or(0, |s| s.cell_accum);
                accum += master_clocks * rate;
                let cells = accum / MASTER_CLOCKS_PER_LINE;
                if let Some(s) = self.fill_state.as_mut() {
                    s.cell_accum = accum - cells * MASTER_CLOCKS_PER_LINE;
                }
                self.advance_fill(cells);
            }
        }

        if self.dma_remaining_clocks == 0 {
            self.dma_busy = false;
        }
    }

    /// Drain up to `budget_mclks` master clocks of the pending 68k-bus DMA
    /// stall, returning the amount drained and leaving the remainder for the
    /// next call. The frame loop calls this once per scanline (budget = one
    /// line) so a multi-line transfer freezes the CPU line-by-line instead of
    /// stalling a whole frame at once (Genesis Plus GX meters its DMA per
    /// line too). The leftover persists across lines and frames in this field.
    pub fn drain_dma_stall_mclks(&mut self, budget_mclks: u64) -> u64 {
        let drained = self.pending_cpu_dma_mclks.min(budget_mclks);
        self.pending_cpu_dma_mclks -= drained;
        drained
    }

    // ======================================================================
    // VDP FIFO (Write FIFO)
    // ======================================================================

    /// Compute CPU halt cycles for Memory-to-VRAM DMA.
    /// The 68K is completely halted during this DMA type while data is transferred
    /// through the VDP's access slots.
    pub fn compute_dma_halt_cycles(&self, word_count: u16) -> u32 {
        let mclks_per_word = self.fifo_mclks_per_slot();
        let total_mclks = (word_count as u64) * mclks_per_word;
        // Convert to CPU cycles (round up)
        total_mclks.div_ceil(7) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dma_state_new() {
        let dma = DmaState::new();
        assert!(!dma.is_active());
        assert!(!dma.cpu_blocked);
        assert_eq!(dma.cycles_remaining, 0);
    }

    #[test]
    fn test_dma_memory_to_vram_timing() {
        let mut dma = DmaState::new();

        // During active display (slower)
        dma.start(
            DmaOperation::MemoryToVram {
                source: 0x1000,
                dest: 0x0,
                length: 100,
            },
            false,
        );

        assert!(dma.is_active());
        assert!(dma.cpu_blocked);
        assert_eq!(dma.cycles_remaining, 100 * 16);
    }

    #[test]
    fn test_dma_memory_to_vram_vblank_faster() {
        let mut dma = DmaState::new();

        // During VBLANK (faster)
        dma.start(
            DmaOperation::MemoryToVram {
                source: 0x1000,
                dest: 0x0,
                length: 100,
            },
            true,
        );

        assert_eq!(dma.cycles_remaining, 100 * 2);
    }

    #[test]
    fn test_dma_vram_fill_timing() {
        let mut dma = DmaState::new();

        dma.start(
            DmaOperation::VramFill {
                dest: 0x0,
                value: 0xAA,
                length: 100,
            },
            false,
        );

        assert_eq!(dma.cycles_remaining, 100 * 2);
    }

    #[test]
    fn test_dma_vram_copy_timing() {
        let mut dma = DmaState::new();

        dma.start(
            DmaOperation::VramCopy {
                source: 0x0,
                dest: 0x1000,
                length: 100,
            },
            false,
        );

        assert_eq!(dma.cycles_remaining, 100 * 8);
    }

    #[test]
    fn test_dma_execute_partial() {
        let mut dma = DmaState::new();

        dma.start(
            DmaOperation::VramFill {
                dest: 0x0,
                value: 0xAA,
                length: 100,
            },
            false,
        );

        let initial_cycles = dma.cycles_remaining;
        let completed = dma.execute(50);

        assert!(!completed);
        assert!(dma.is_active());
        assert_eq!(dma.cycles_remaining, initial_cycles - 50);
    }

    #[test]
    fn test_dma_execute_complete() {
        let mut dma = DmaState::new();

        dma.start(
            DmaOperation::VramFill {
                dest: 0x0,
                value: 0xAA,
                length: 10,
            },
            false,
        );

        let completed = dma.execute(1000); // Execute more cycles than needed

        assert!(completed);
        assert!(!dma.is_active());
        assert!(!dma.cpu_blocked);
        assert_eq!(dma.cycles_remaining, 0);
    }

    #[test]
    fn test_parse_dma_memory_to_vram() {
        let mut regs = [0u8; 24];

        // Enable DMA (bit 4 of R1)
        regs[1] = 0x10;

        // Set length: 256 words (R19=low, R20=high)
        regs[19] = 0x00; // Length low
        regs[20] = 0x01; // Length high -> 0x0100 = 256

        // Set source address 0x1000 in 68k address space
        // Source is stored as word address: 0x1000 >> 1 = 0x800
        // R21=low, R22=mid, R23=high+mode
        regs[21] = 0x00; // Source low byte
        regs[22] = 0x08; // Source mid byte -> word address = 0x0800
        regs[23] = 0x00; // Source high (bits 0-6) + mode 00 = Memory to VRAM

        let dma = parse_dma_registers(&regs, 0x0, 0x01);

        assert!(dma.is_some());
        if let Some(DmaOperation::MemoryToVram {
            source,
            dest,
            length,
        }) = dma
        {
            assert_eq!(source, 0x1000); // 0x0800 << 1 = 0x1000
            assert_eq!(dest, 0x0);
            assert_eq!(length, 0x0100);
        } else {
            panic!("Expected MemoryToVram operation");
        }
    }

    #[test]
    fn test_parse_dma_vram_fill() {
        let mut regs = [0u8; 24];

        // Enable DMA
        regs[1] = 0x10;

        // Set length: 128 (R19=low, R20=high)
        regs[19] = 0x80; // Length low = 0x80
        regs[20] = 0x00; // Length high = 0x00 -> length = 0x0080 = 128

        // Set DMA mode to VRAM Fill (R23 bits 6-7 = 10)
        regs[23] = 0x80;

        let dma = parse_dma_registers(&regs, 0x1000, 0x01);

        assert!(dma.is_some());
        if let Some(DmaOperation::VramFill { dest, length, .. }) = dma {
            assert_eq!(dest, 0x1000);
            assert_eq!(length, 0x80);
        } else {
            panic!("Expected VramFill operation");
        }
    }

    #[test]
    fn test_parse_dma_vram_copy() {
        let mut regs = [0u8; 24];

        // Enable DMA
        regs[1] = 0x10;

        // Set length: 64 (R19=low, R20=high)
        regs[19] = 0x40; // Length low = 0x40
        regs[20] = 0x00; // Length high = 0x00 -> length = 0x0040 = 64

        // Set source address in VRAM: 0x0800 (R21=low, R22=mid)
        regs[21] = 0x00; // Source low
        regs[22] = 0x08; // Source mid -> source = 0x0800

        // VRAM Copy mode: bits 6-7 = 11
        regs[23] = 0xC0;

        let dma = parse_dma_registers(&regs, 0x2000, 0x01);

        assert!(dma.is_some());
        if let Some(DmaOperation::VramCopy {
            source,
            dest,
            length,
        }) = dma
        {
            assert_eq!(source, 0x0800);
            assert_eq!(dest, 0x2000);
            assert_eq!(length, 0x40);
        } else {
            panic!("Expected VramCopy operation");
        }
    }

    #[test]
    fn test_parse_dma_disabled() {
        let mut regs = [0u8; 24];

        // DMA disabled (bit 4 of R1 = 0)
        regs[1] = 0x00;

        // Set length (R19=low, R20=high)
        regs[19] = 0x40;
        regs[20] = 0x01;

        let dma = parse_dma_registers(&regs, 0x0, 0x01);
        assert!(dma.is_none());
    }

    #[test]
    fn test_parse_dma_zero_length() {
        let mut regs = [0u8; 24];

        // Enable DMA
        regs[1] = 0x10;

        // Length = 0 (R19=low, R20=high)
        regs[19] = 0x00;
        regs[20] = 0x00;

        let dma = parse_dma_registers(&regs, 0x0, 0x01);
        assert!(dma.is_none());
    }

    #[test]
    fn a_memory_dma_reads_one_span_inside_its_window() {
        let dma = MemoryDma {
            source: 0x0D_6000,
            words: 0x100,
            target: DmaTarget::Vram,
            destination: 0x4000,
        };
        assert_eq!(dma.source_spans(), vec![(0x0D_6000, 0x0D_61FF)]);
    }

    #[test]
    fn a_memory_dma_wraps_inside_its_128kb_window() {
        // The window's last word, then its first.
        let dma = MemoryDma {
            source: 0xFF_FFFE,
            words: 2,
            target: DmaTarget::Cram,
            destination: 0,
        };
        assert_eq!(
            dma.source_spans(),
            vec![(0xFF_FFFE, 0xFF_FFFF), (0xFE_0000, 0xFE_0001)]
        );
    }

    #[test]
    fn the_access_code_names_the_dma_target() {
        assert_eq!(DmaTarget::from_access_code(0x21), DmaTarget::Vram);
        assert_eq!(DmaTarget::from_access_code(0x23), DmaTarget::Cram);
        assert_eq!(DmaTarget::from_access_code(0x25), DmaTarget::Vsram);
        assert_eq!(DmaTarget::from_access_code(0x20), DmaTarget::Nowhere);
    }
}
