//! The VDP's ports: reads and writes of the data and control ports, the
//! status word, the register writes, the HV counter, and the write FIFO.

use super::recording::PaletteRedraw;
use super::writes::PaletteStore;
use super::{
    DmaTransfer, FillState, Interlace, Vdp, FIFO_TIMING_H32, FIFO_TIMING_H40,
    MASTER_CLOCKS_PER_LINE,
};

/// VDP access codes (CD0-CD5 bits from control port)
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VdpAccessMode {
    VramRead = 0x00,
    VramWrite = 0x01,
    CramWrite = 0x03,
    VsramWrite = 0x05,
    CramRead = 0x08,
    VsramRead = 0x04,
}

impl Vdp {
    /// Read from VDP ports.
    /// Read a 16-bit word from VDP ports.
    /// This is the primary read interface - the Genesis VDP returns word-sized values.
    pub fn read_word_port(&mut self, addr: u32) -> u16 {
        let masked = addr & 0x1F;
        match masked {
            // Data port (0xC00000-0xC00003)
            0x00..=0x03 => {
                self.pending_write = false;
                self.read_data_port()
            }
            // Control port / Status register (0xC00004-0xC00007)
            0x04..=0x07 => {
                self.pending_write = false;
                let status = self.get_status_register();
                // Clear sprite flags after reading (as MAME and Genesis Plus GX do).
                // The F flag (vblank_pending, bit 7) is NOT cleared by status reads -
                // it's cleared by interrupt acknowledge (IACK) when the 68K takes the VINT.
                self.sprite_overflow = false;
                self.sprite_collision = false;
                status
            }
            // HV counter (0xC00008-0xC0000B)
            0x08..=0x0B => {
                if let Some(latched) = self.hvc_latch {
                    latched
                } else {
                    let v = self.hv_v_counter() as u16;
                    let h = self.hv_h_counter() as u16;
                    (v << 8) | h
                }
            }
            _ => 0xFFFF,
        }
    }

    pub fn read(&mut self, addr: u32) -> u8 {
        let masked = addr & 0x1F;
        match masked {
            // Data port (0xC00000-0xC00003)
            0x00 | 0x02 => {
                self.pending_write = false; // Cancel pending write

                (if (addr & 1) == 0 {
                    // Even address: high byte
                    self.read_data_port() >> 8
                } else {
                    // Odd address: low byte
                    self.read_data_port() & 0xFF
                }) as u8
            }
            // Control port / Status register (0xC00004-0xC00007)
            // Status is 16-bit, return high/low byte based on address
            // Reading status register clears sprite overflow/collision only.
            // The F flag (vblank_pending, bit 7) is NOT cleared by status reads -
            // it's cleared by interrupt acknowledge (IACK) when the 68K takes the VINT.
            0x04 | 0x06 => {
                self.pending_write = false;
                let status = self.get_status_register();
                self.sprite_overflow = false;
                self.sprite_collision = false;
                (status >> 8) as u8
            }
            0x05 | 0x07 => {
                self.pending_write = false;
                let status = self.get_status_register();
                self.sprite_overflow = false;
                self.sprite_collision = false;
                (status & 0xFF) as u8
            }
            // HV counter (0xC00008-0xC0000B)
            0x08..=0x0B => {
                let is_v = (addr & 1) == 0;
                if let Some(latched) = self.hvc_latch {
                    if is_v {
                        (latched >> 8) as u8
                    } else {
                        (latched & 0xFF) as u8
                    }
                } else if is_v {
                    self.hv_v_counter()
                } else {
                    self.hv_h_counter()
                }
            }
            _ => 0xFF,
        }
    }

    /// Read from data port. Returns the prefetch buffer and loads the next word.
    fn read_data_port(&mut self) -> u16 {
        let value = self.read_buffer;
        self.prefetch_read_buffer();
        value
    }

    /// Prefetch the word at the current address into the read buffer,
    /// then auto-increment the address.
    fn prefetch_read_buffer(&mut self) {
        self.read_buffer = match self.code & 0x0F {
            0x00 => {
                // VRAM read
                let addr = (self.address as usize) & 0xFFFE;
                if addr + 1 < self.vram.len() {
                    let high = self.vram[addr] as u16;
                    let low = self.vram[addr + 1] as u16;
                    (high << 8) | low
                } else {
                    0
                }
            }
            0x08 => {
                // CRAM read. CRAM is stored in the bus layout BBB0GGG0RRR0
                // (0x0EEE valid). The unused bits float to the most recent FIFO
                // entry (open bus; checked against Genesis Plus GX).
                let index = ((self.address & 0x7E) >> 1) as usize;
                let stored = if index < 64 { self.cram[index] } else { 0 };
                (stored & 0x0EEE) | (self.fifo[self.fifo_idx] & !0x0EEE)
            }
            0x04 => {
                // VSRAM read (11-bit valid). VSRAM is physically 40 words: a read
                // whose byte offset reaches 0x50 (word index >= 40) wraps back to
                // word 0 on hardware (checked against Genesis Plus GX). Writes
                // still land in the upper slots, but they are not readable there.
                // Unused upper 5 bits float to the most recent FIFO entry.
                let byte_index = (self.address & 0x7E) as usize;
                let index = if byte_index >= 0x50 {
                    0
                } else {
                    byte_index >> 1
                };
                let stored = self.vsram[index];
                (stored & 0x07FF) | (self.fifo[self.fifo_idx] & !0x07FF)
            }
            0x0C => {
                // Undocumented 8-bit VRAM read: one byte from the adjacent
                // address, upper 8 bits from the FIFO open bus.
                let addr = (self.address as usize) ^ 1;
                let byte = if addr < self.vram.len() {
                    self.vram[addr] as u16
                } else {
                    0
                };
                (byte & 0x00FF) | (self.fifo[self.fifo_idx] & !0x00FF)
            }
            _ => self.read_buffer,
        };
        self.address = self.address.wrapping_add(self.registers[15] as u16);
    }

    /// Side-effect-free peek of the 16-bit VDP status word.
    ///
    /// A real status-port read resets the control-port write latch;
    /// this does not. It is the read the platform's debugger and its
    /// lessons use to sample `vblank`/`hblank`/sprite flags every frame
    /// without perturbing a running game.
    pub fn status_word(&self) -> u16 {
        self.get_status_register()
    }

    /// Get VDP status register (16-bit).
    ///
    /// Status register bits:
    /// - Bit 15-10: Unused (return open bus / fixed value)
    /// - Bit 9: FIFO Empty
    /// - Bit 8: FIFO Full
    /// - Bit 7: V Interrupt Pending
    /// - Bit 6: Sprite Overflow
    /// - Bit 5: Sprite Collision
    /// - Bit 4: Odd Frame (interlace)
    /// - Bit 3: V Blank
    /// - Bit 2: H Blank
    /// - Bit 1: DMA Busy
    /// - Bit 0: PAL Mode (0=NTSC, 1=PAL)
    pub(super) fn get_status_register(&self) -> u16 {
        // Base value: bits 13, 12 and 10 set (0x3400); bit 9 (FIFO empty) is set below
        // This matches observed MAME behavior for unused bits
        let mut status: u16 = 0x3400;

        // Bit 9: FIFO Empty
        if self.is_fifo_empty() {
            status |= 0x0200;
        }

        // Bit 8: FIFO Full
        if self.is_fifo_full() {
            status |= 0x0100;
        }

        // Bit 7: V Interrupt Pending
        if self.timing.vblank_pending {
            status |= 0x0080;
        }

        // Bit 6: Sprite Overflow (>20 sprites on a scanline, >16 in H32)
        if self.sprite_overflow {
            status |= 0x0040;
        }

        // Bit 5: Sprite Collision (two non-transparent sprite pixels overlap)
        if self.sprite_collision {
            status |= 0x0020;
        }

        // Bit 4: Odd Frame. Set in odd fields while interlace is on
        // (register 12 bit 1, in either interlace mode), and clear otherwise,
        // as Genesis Plus GX and MAME report it.
        let interlaced = (self.registers[12] & 0x02) != 0;
        if interlaced && self.odd_frame {
            status |= 0x0010;
        }

        // Bit 3: V Blank.
        // Hardware reports VBLANK whenever the beam is in the vertical blanking
        // region OR whenever the display is disabled (reg1 bit 6 = 0): with the
        // display off the VDP is permanently "blanked" and accessible. Games
        // that load VRAM with the display off (e.g. Gunstar Heroes' intro) poll
        // this bit to know the VDP is writable and spin forever if it never
        // sets. (Genesis Plus GX sets the flag with the display off as well.)
        let display_enabled = (self.registers[1] & 0x40) != 0;
        if self.timing.in_vblank || !display_enabled {
            status |= 0x0008;
        }

        // Bit 2: H Blank
        if self.timing.in_hblank {
            status |= 0x0004;
        }

        // Bit 1: DMA Busy
        if self.dma_busy || self.dma.is_active() {
            status |= 0x0002;
        }

        // Bit 0: PAL Mode (0=NTSC)
        // status |= 0x0001; // Not set for NTSC

        status
    }

    /// Write a byte to VDP ports (used for byte-level access).
    /// Note: VDP control port should use write_word() for proper command handling.
    pub fn write(&mut self, addr: u32, value: u8) {
        match addr & 0x1F {
            // Data port. A 68000 byte write to an even address drives D15-D8
            // (the high lane); an odd address drives D7-D0. Contra triggers a
            // VRAM fill with `MOVE.B Dn, $C00000`, and the fill replicates
            // `value >> 8`, so a byte put in the low lane would fill every
            // trailing cell with 0.
            0x00..=0x03 => {
                self.pending_write = false;
                let word = if (addr & 1) == 0 {
                    (value as u16) << 8
                } else {
                    value as u16
                };
                self.write_data_port(word, (addr & 1) == 0);
            }
            // Control port - byte writes are unusual but should work
            0x04..=0x07 => {
                // Byte writes to control port are rare; treat as word with 0 for other byte
                let word = if (addr & 1) == 0 {
                    (value as u16) << 8
                } else {
                    value as u16
                };
                self.write_control_word(word);
            }
            _ => {}
        }
    }

    /// Write a 16-bit word to VDP ports.
    /// This is the primary write interface - the Genesis VDP expects word-sized accesses.
    pub fn write_word_port(&mut self, addr: u32, value: u16) {
        match addr & 0x1F {
            // Data port (0xC00000-0xC00003)
            0x00..=0x03 => {
                self.pending_write = false;
                self.write_data_port(value, true);
            }
            // Control port (0xC00004-0xC00007)
            0x04..=0x07 => {
                self.write_control_word(value);
            }
            _ => {}
        }
    }

    /// Write a 32-bit long word to VDP control port (two consecutive word writes).
    pub fn write_long_port(&mut self, addr: u32, value: u32) {
        let high_word = (value >> 16) as u16;
        let low_word = (value & 0xFFFF) as u16;
        self.write_word_port(addr, high_word);
        self.write_word_port(addr, low_word);
    }

    /// Process a word write to the VDP control port.
    fn write_control_word(&mut self, value: u16) {
        if !self.pending_write {
            // First word of a command.
            //
            // Per hardware testing (Charles MacDonald), checked against
            // Genesis Plus GX: the first write IMMEDIATELY updates the low 14
            // address bits and CD0-CD1, *unconditionally*, before the VDP even
            // decides whether this is a register write. There is no internal
            // staging. Games like Batman & Robin and EA titles depend on this to
            // change the VRAM address with a single control word, and the
            // VDPFIFOTesting "Register Write ... / Code Reg" subtests verify that
            // a register write (bits 15-14 = 10) ALSO latches these bits.
            self.address = (self.address & 0xC000) | (value & 0x3FFF);
            self.code = (self.code & 0x3C) | ((value >> 14) as u8 & 0x03);
            self.control_word = (self.control_word & 0xFFFF0000) | (value as u32);

            // Register write: bits 15-14 = 10 -> `100R RRRR DDDD DDDD`. The
            // address/code latch above already happened; the register number is
            // bits 12-8 only (bit 13 is masked out, going to the address latch),
            // and the operation completes in a single word (no pending second).
            if (value & 0xC000) == 0x8000 {
                let reg_num = ((value >> 8) & 0x1F) as usize;
                let reg_value = (value & 0xFF) as u8;
                self.write_register(reg_num, reg_value);
                self.pending_write = false;
            } else {
                self.pending_write = true;
            }
        } else {
            // Second word of a two-word command.
            // Updates address high 2 bits (A15-A14) and code bits CD2-CD5.
            self.control_word = (self.control_word & 0x0000FFFF) | ((value as u32) << 16);
            self.pending_write = false;

            self.address = (self.address & 0x3FFF) | ((value & 0x03) << 14);
            self.code = (self.code & 0x03) | (((value >> 2) & 0x3C) as u8);

            let cd5 = (self.code >> 5) & 1;

            // Check if DMA should be triggered (CD5=1 indicates DMA).
            // A control write that arrives while a fill is still streaming must
            // NOT re-arm DMA: that would clobber the running fill's metered
            // duration with the `u64::MAX` sentinel and hang the busy bit. The
            // address/code latch above still applies, so such a write redirects
            // the in-flight fill instead (VDPFIFOTesting "DMA Fill Control Port
            // Writes").
            if cd5 == 1 && (self.registers[1] & 0x10) != 0 && self.fill_state.is_none() {
                let dma_mode = (self.registers[23] >> 6) & 0x03;
                match dma_mode {
                    0b00 | 0b01 => {
                        // Memory to VRAM - needs CPU memory access
                        self.dma_pending = true;
                    }
                    0b11 => {
                        // VRAM Copy - VDP-internal operation; the CPU keeps
                        // running while the VDP reports DMA-busy until it finishes.
                        let raw_length =
                            ((self.registers[20] as u16) << 8) | (self.registers[19] as u16);
                        // DMA length 0 means 65536 (pre-decrementing counter)
                        let length: u32 = if raw_length == 0 {
                            0x10000
                        } else {
                            raw_length as u32
                        };
                        let source =
                            ((self.registers[22] as u16) << 8) | (self.registers[21] as u16);
                        self.execute_dma_vram_copy(source, self.address, length);
                        // Assert DMA-busy for the metered duration.
                        self.dma_busy = true;
                        self.dma_remaining_clocks =
                            self.dma_busy_master_clocks(DmaTransfer::Copy, length);
                    }
                    _ => {
                        // 0b10 = VRAM Fill. The fill *data* is supplied by a
                        // subsequent data-port write, but the DMA-busy status
                        // bit is asserted by THIS control-port write (the CD5
                        // arm), not by that later data write (verified on
                        // hardware; Genesis Plus GX does the same). The
                        // fill duration is unknown until the data write provides
                        // the length, so hold the busy bit with a sentinel that
                        // `tick_dma` refuses to drain; the data write replaces it
                        // with the real metered duration. VDPFIFOTesting "DMA
                        // Busy Flag - DMA Fill" reads the bit in the gap between
                        // the two writes and expects it already set. The one-shot
                        // `dma_fill_pending` is what the next
                        // data write consumes to actually start the fill.
                        self.dma_fill_pending = true;
                        self.dma_busy = true;
                        self.dma_remaining_clocks = u64::MAX;
                    }
                }
            } else if cd5 == 0 {
                // Non-DMA read command: prefetch the first word into the read
                // buffer and auto-increment address. Only for read codes
                // (VRAM 0x00, VSRAM 0x04, CRAM 0x08, undocumented 8-bit VRAM 0x0C).
                let code_low = self.code & 0x0F;
                if code_low == 0x00 || code_low == 0x04 || code_low == 0x08 || code_low == 0x0C {
                    self.prefetch_read_buffer();
                }
            }
        }
    }

    /// Write to data port based on current access mode.
    pub(super) fn write_data_port(&mut self, value: u16, _is_high_byte: bool) {
        // Store value for potential DMA fill
        self.dma_fill_value = value;

        // Reserve this write's VDP FIFO access slot (drain timestamp) before it
        // joins the ring, so the empty/full status bits track it. Only the CPU
        // data-port path records a slot; DMA pushes (`execute_pending_dma`)
        // account for their own access slots separately (Genesis Plus GX too
        // times the FIFO in its data-port write, not in its DMA loop).
        self.fifo_record_slot();

        // Push the written word into the FIFO data ring (open-bus source for
        // subsequent VSRAM/CRAM/byte-VRAM reads). Every data-port write does
        // this *before* the DMA-fill check (as in Genesis Plus GX), so a fill's
        // source word and the post-write open-bus both see it.
        self.push_fifo_word(value);

        // DMA Fill triggers off the one-shot `dma_fill_pending` flag armed by
        // the control-port write (CD5 + mode 0b10 while DMA was enabled).
        // Consuming it here means a *second* data-port write (the
        // ones the "DP Writes During DMA Fill" subtests issue while the fill is
        // still streaming), and any write after a short fill has completed, never
        // re-arms a fresh fill; it falls through to the normal write path,
        // landing at the fill's partially-advanced (shared) address and
        // re-sourcing the remaining cells via the FIFO. Clearing DMA-enable
        // between the arm and this write does NOT cancel the pending fill (the
        // "Toggle Fill" subtest): the fill was latched at the arm.
        if self.dma_fill_pending {
            self.dma_fill_pending = false;
            // DMA Fill triggered by data port write.
            let raw_length = ((self.registers[20] as u16) << 8) | (self.registers[19] as u16);
            // DMA length 0 means 65536 (pre-decrementing counter)
            let length: u32 = if raw_length == 0 {
                0x10000
            } else {
                raw_length as u32
            };

            // Step 1: the triggering data-port write deposits the FULL word at
            // the current address, exactly like a normal write, *before* the
            // fill runs (with the bytes swapped on an odd VRAM address; checked
            // against Genesis Plus GX). The fill then only writes the high byte to the
            // *subsequent* cells, so the initial cell keeps the whole word. This
            // is what makes a post-fill read see e.g. `1234` (not `1212`) in the
            // first cell, VDPFIFOTesting "DMA Fill FIFO Usage".
            let auto_increment = self.registers[15] as u16;
            let init_addr = self.address;
            match self.code & 0x0F {
                0x01 => {
                    // VRAM: write the full word (byteswapped on odd address).
                    let raw_addr = init_addr as usize;
                    let wa = raw_addr & 0xFFFE;
                    if wa + 1 < self.vram.len() {
                        self.note_port_vram_store(raw_addr, value, true);
                        if (raw_addr & 1) == 0 {
                            self.vram[wa] = (value >> 8) as u8;
                            self.vram[wa + 1] = (value & 0xFF) as u8;
                        } else {
                            self.vram[wa] = (value & 0xFF) as u8;
                            self.vram[wa + 1] = (value >> 8) as u8;
                        }
                        self.trace_vram_write(wa as u16, value, "dma-fill");
                    }
                }
                0x03 => {
                    // CRAM: write full word
                    let index = ((init_addr & 0x7E) >> 1) as usize;
                    if index < 64 {
                        self.cram[index] = value & 0x0EEE;
                        self.set_palette_colour(index, value & 0x0EEE);
                        self.note_palette_change(
                            index,
                            value & 0x0EEE,
                            PaletteRedraw::Nothing,
                            PaletteStore::FillStart,
                        );
                    }
                }
                0x05 => {
                    // VSRAM: write full word
                    let index = ((init_addr & 0x7E) >> 1) as usize;
                    if index < 64 {
                        self.vsram[index] = value & 0x07FF;
                    }
                }
                _ => {}
            }

            // The source-register writeback + length-clear happen now, at the
            // trigger, so "DMA Fill Source/Length Reg Update" observe the final
            // register values. Only the cell writes are deferred to
            // `advance_fill`/`tick_dma` below.
            self.dma_fill_source_writeback();

            // Step 2: the remaining `length` cells stream out over the metered
            // duration (advanced from `tick_dma`), starting one increment past
            // the preserved initial cell: `length + 1` cells in all, as the
            // instant `execute_dma_vram_fill` writes.
            self.address = init_addr.wrapping_add(auto_increment);
            self.fill_state = Some(FillState {
                remaining: length,
                cell_accum: 0,
            });
            self.note_fill_start(init_addr);

            // Front-load the cells the chip would have written from this trigger
            // cycle to the end of the current scanline (Genesis Plus GX runs its
            // DMA to the write's time the same way), so a back-to-back data/control write
            // observes the same partially-advanced address the hardware does,
            // VDPFIFOTesting "DP Writes During DMA Fill" / "DMA Fill Control Port
            // Writes". In VBLANK the whole fill lands here; otherwise the
            // remainder streams from `tick_dma`.
            let chunk = self.fill_initial_chunk(length);
            self.advance_fill(chunk);

            // VRAM Fill is a VDP-internal transfer: the 68000 keeps running
            // while the VDP reports DMA-busy (status bit 1) for the metered
            // duration. The busy bit clears once the fill's
            // duration elapses; the F flag (bit 7) is unaffected.
            self.dma_busy = true;
            self.dma_remaining_clocks = self.dma_busy_master_clocks(DmaTransfer::Fill, length);
            return;
        }

        // FIFO entry counter, kept because the savestate carries it; the status
        // FIFO-empty/full bits derive from the `fifo_slot_mclks` drain timestamps
        // recorded above. The CPU is NOT stalled on a full FIFO: MAME's
        // totalcycles counter does not include FIFO wait states, and the
        // timestamp model needs no stall to report full.
        if self.fifo_entries < 4 {
            self.fifo_entries += 1;
        }

        match self.code & 0x0F {
            0x01 => {
                // VRAM write - always writes a 16-bit word.
                // The VDP clears bit 0 for the base address; if the original
                // address had bit 0 set, the written bytes are swapped.
                let raw_addr = self.address as usize;
                let addr = raw_addr & 0xFFFE; // Word-align
                if addr + 1 < self.vram.len() {
                    self.note_port_vram_store(raw_addr, value, false);
                    if (raw_addr & 1) == 0 {
                        self.vram[addr] = (value >> 8) as u8;
                        self.vram[addr + 1] = (value & 0xFF) as u8;
                    } else {
                        self.vram[addr] = (value & 0xFF) as u8;
                        self.vram[addr + 1] = (value >> 8) as u8;
                    }
                    self.trace_vram_write(addr as u16, value, "data-port");
                }
                self.address = self.address.wrapping_add(self.registers[15] as u16);
            }
            0x03 => {
                // CRAM write
                let index = ((self.address & 0x7E) >> 1) as usize;
                if index < 64 {
                    let masked = value & 0x0EEE;
                    self.cram[index] = masked;
                    self.set_palette_colour(index, masked);
                    // A palette change during active display re-tints the
                    // current line (as in Genesis Plus GX). Display on -> re-tint only from
                    // the beam rightward (beam-X segment: pixels already scanned
                    // keep their colour, so multiple writes build colour segments,
                    // beyond Genesis Plus GX's whole-line update). Display off -> only a
                    // backdrop-pen change is visible, so re-blank the line.
                    let mut redraw = PaletteRedraw::Nothing;
                    if self.midline_render_window() {
                        let line = self.timing.scanline;
                        if (self.registers[1] & 0x40) != 0 {
                            let x0 = self.beam_active_x();
                            let w = self.display_width() as usize;
                            self.remap_scanline_segment(line, x0, w);
                            redraw = PaletteRedraw::Columns {
                                from: x0 as u16,
                                to: w as u16,
                                shadow_highlight: (self.registers[12] & 0x08) != 0,
                            };
                        } else if index == self.backdrop_index() {
                            self.render_scanline(line);
                            redraw = PaletteRedraw::WholeLine {
                                width: self.display_width() as u16,
                                backdrop: index as u8,
                            };
                        }
                    }
                    self.note_palette_change(index, masked, redraw, PaletteStore::Port);
                }
                self.address = self.address.wrapping_add(self.registers[15] as u16);
            }
            0x05 => {
                // VSRAM write
                let index = ((self.address & 0x7E) >> 1) as usize;
                if index < 64 {
                    self.vsram[index] = value & 0x07FF; // 11-bit value
                }
                self.address = self.address.wrapping_add(self.registers[15] as u16);
                // NB: no mid-line VSRAM re-composite hook. Genesis Plus GX
                // re-renders the line on a 2-cell-mode VSRAM write within its
                // 860-mclk window,
                // but SG1's beam lands HINT-driven scroll writes at a different
                // sub-frame position, so a whole-line re-composite here repaints
                // boundary lines that should stay as first rendered (it breaks the
                // Gunstar Heroes intro). SG1's per-line scroll sampling at
                // line start already applies the new value from the next line on.
            }
            _ => {}
        }
    }

    /// Write to a VDP register.
    pub(super) fn write_register(&mut self, reg_num: usize, value: u8) {
        // VDP registers 11-23 cannot be updated while in Mode 4 (the M5 bit,
        // register 1 bit 2, is clear). Real games depend on this: Captain
        // Planet and Bass Master Classic write high register indices in Mode 4
        // expecting them to be ignored (checked against Genesis Plus GX). The DMA engine
        // updates registers 19-23 directly (not through this path), so its
        // post-transfer writeback is unaffected.
        if (self.registers[1] & 0x04) == 0 && reg_num > 10 {
            return;
        }
        self.apply_register_write(reg_num, value);
    }

    /// Commit a register value and run its side effects, bypassing the Mode-4
    /// access mask. This is the shared body of `write_register`; the test/debug
    /// register override (`set_test_register`) calls it directly so it can force
    /// any register regardless of the current display mode.
    pub(super) fn apply_register_write(&mut self, reg_num: usize, value: u8) {
        if reg_num < 24 {
            let prev = self.registers[reg_num];
            self.registers[reg_num] = value;

            // Handle special cases
            if reg_num == 0 {
                // HV-counter latch (reg0 bit1 / M3). On a *change* of the bit
                // while Mode 5 (reg1 bit2) is active, the current HV counter
                // is latched or freed again (checked against Genesis Plus
                // GX). Reads then return the frozen
                // value (lightgun / Sunset Riders / VDPFIFOTesting "HV Counter
                // Latch").
                if ((prev ^ value) & 0x02) != 0 && (self.registers[1] & 0x04) != 0 {
                    if (value & 0x02) != 0 {
                        let v = self.hv_v_counter() as u16;
                        let h = self.hv_h_counter() as u16;
                        self.hvc_latch = Some((v << 8) | h);
                    } else {
                        self.hvc_latch = None;
                    }
                }
            } else if reg_num == 1 {
                // Register 1: Mode register 2
                // If VINT enable (bit 5) is being set and the F flag (vblank_pending)
                // is already active from a previous VBlank, re-assert vint_irq_pending.
                // On real hardware, the VDP's /INT output = F_FLAG AND VINT_ENABLE,
                // so enabling VINT while the F flag is set immediately asserts the
                // interrupt line. Without this, interrupts are lost when VBlank occurs
                // while VINT is disabled and VINT is enabled later.
                let vint_enabled = (value & 0x20) != 0;
                if vint_enabled && self.timing.vblank_pending {
                    self.timing.vint_irq_pending = true;
                }
                // NB: deliberately NO mid-line display-enable (bit 6) re-render
                // here. Genesis Plus GX redraws the line when display
                // is re-enabled within its 860-mclk window, but SG1 already
                // reproduces display-enable *windows* at line granularity (the
                // toggle takes effect from the next line's start), which gives
                // that emulator's picture for the Gunstar Heroes intro. An
                // exact-cycle hook here would over-trigger: SG1's display-enable
                // write lands at a different sub-frame beam position, so it would
                // re-render a boundary line that Genesis Plus GX leaves blank.
                //
                // For the same reason there is no mid-line re-render hook on the
                // plane-base registers (2/3/4) or backdrop (7): Genesis Plus GX
                // re-renders the line on those mid-active writes, but SG1's coarser sub-frame
                // timing makes a whole-line re-composite repaint boundary lines a
                // game intended for the *next* line. SG1's line-granular register
                // handling (the change takes effect from the next line) already
                // gives the same picture on the validated scenes; only palette re-tints
                // (CRAM, in `write_data_port`) are safe to reflect on the current line, because
                // they never re-composite, they only recolour resolved indices.
            } else if reg_num == 7 {
                // Register 7: Backdrop color
                // Bits 5-4: palette, bits 3-0: color
                // Note: Do NOT call render_backdrop() here - it would overwrite
                // already-rendered scanlines. render_scanline() reads the backdrop
                // color from CRAM on every line it draws, through `backdrop_index()`.
            } else if reg_num == 10 {
                // Register 10: H interrupt counter reload value.
                // Writing R10 updates ONLY the reload value. The live counter
                // keeps decrementing and is reloaded from R10 the next time it
                // underflows (or during vblank); it is NOT reset on write.
                // Resetting the live counter here desynchronises raster splits
                // that rewrite R10 mid-frame (e.g. the Gunstar Heroes intro's
                // HINT-driven display-enable window), pushing the display-enable
                // HINT far down the frame so only a bottom strip renders.
                self.timing.h_counter_reload = value;
            }
        }
    }

    /// V-counter overflow value for the current mode. The counter counts up to
    /// this value, then wraps by `lines_per_frame` so the low byte jumps (the
    /// documented jump points: NTSC 224-line = 0xEA, 240-line = 0x106; PAL
    /// 224 = 0x102, 240 = 0x10A).
    fn vc_max(&self) -> i32 {
        // PAL runs 312 lines/frame, NTSC 262.
        let pal = self.timing.total_scanlines() > 300;
        let mode_240 = (self.registers[1] & 0x08) != 0;
        match (mode_240, pal) {
            (false, false) => 0xEA,
            (false, true) => 0x102,
            (true, false) => 0x106,
            (true, true) => 0x10A,
        }
    }

    /// Genesis V counter for `0xC00008` reads. Reflects the current scanline
    /// with the hardware's mid-frame counter jump (the byte is not a plain
    /// `line & 0xFF`): once the line passes `vc_max`, the counter wraps by the
    /// total line count so e.g. NTSC 224-line reads 0x00..0xEA then 0xE5..0xFF.
    ///
    /// In an interlace mode (the frame's, [`Vdp::interlace`]) the counter's
    /// bit 0 shows its bit 8, and in mode 2 the counter is doubled first,
    /// counting the picture's rows: there the byte is the line's low byte
    /// turned one bit left. As Genesis Plus GX gives it.
    pub(super) fn hv_v_counter(&self) -> u8 {
        let lines_per_frame = self.timing.total_scanlines() as i32;
        let vc_max = self.vc_max();
        let mut vc = self.timing.scanline as i32;
        if vc > vc_max {
            vc -= lines_per_frame;
        }
        let vc = match self.interlace {
            Interlace::Off => vc,
            Interlace::Normal => (vc & !1) | ((vc >> 8) & 1),
            Interlace::Double => {
                let rows = vc << 1;
                (rows & !1) | ((rows >> 8) & 1)
            }
        };
        (vc & 0xFF) as u8
    }

    /// Genesis H counter for `0xC00008` reads, approximated from the 68K beam
    /// position within the line. Real hardware exposes a two-segment ramp with a
    /// gap (H40: 0x00..0xB6 then 0xE5..0xFF; H32: 0x00..0x93 then 0xE9..0xFF);
    /// we map the beam fraction onto that ramp. This is a per-line approximation
    /// (SG1 advances the beam per instruction, not per pixel), but it makes the
    /// low byte meaningful for code that polls it instead of returning ~0.
    pub(super) fn hv_h_counter(&self) -> u8 {
        let per_line = self.beam_cycles_per_line.max(1) as u64;
        let pos = (self.beam_cycles_into_line as u64).min(per_line);
        let h40 = (self.registers[12] & 0x01) != 0;
        if h40 {
            // 183 values 0x00..=0xB6, then 27 values 0xE5..=0xFF (210 steps).
            // Clamp to the last index so a beam exactly at line end reads 0xFF,
            // not a wrapped 0x00.
            let p = ((pos * 210 / per_line) as u32).min(209);
            if p <= 0xB6 {
                p as u8
            } else {
                (0xE5 + (p - 0xB7)) as u8
            }
        } else {
            // 148 values 0x00..=0x93, then 23 values 0xE9..=0xFF (171 steps).
            let p = ((pos * 171 / per_line) as u32).min(170);
            if p <= 0x93 {
                p as u8
            } else {
                (0xE9 + (p - 0x94)) as u8
            }
        }
    }

    /// Get the master clocks per access slot based on current display state.
    /// During active display, fewer slots are available (VDP uses most for rendering).
    /// During VBLANK many more slots are available (HBLANK keeps the active rate here).
    pub(super) fn fifo_mclks_per_slot(&self) -> u64 {
        let is_h40 = (self.registers[12] & 0x01) != 0;
        let is_blanking = self.timing.in_vblank;

        // Access slot rates from hardware documentation (SpriteMind.Net):
        // H40 active display: 18 external access slots per scanline
        // H40 blanking: ~204 external access slots per scanline
        // H32 active display: 16 external access slots per scanline
        // H32 blanking: ~167 external access slots per scanline
        // Master clocks per scanline = 3420
        if is_blanking {
            if is_h40 {
                3420 / 204
            } else {
                3420 / 167
            } // ~16-20 mclks per slot
        } else if is_h40 {
            3420 / 18
        } else {
            3420 / 16
        } // ~190-214 mclks per slot
    }

    /// Tick the FIFO: drain entries based on elapsed master clocks.
    /// Called by the system before each CPU instruction.
    pub fn tick_fifo(&mut self, master_clocks: u64) {
        // Advance the monotonic FIFO reference clock first: the empty/full
        // drain timestamps are measured against it, so it must move every
        // instruction even once the entry counter has drained to 0.
        self.fifo_clock = self.fifo_clock.wrapping_add(master_clocks);

        if self.fifo_entries == 0 {
            self.fifo_mclk_counter = 0;
            return;
        }

        self.fifo_mclk_counter += master_clocks;
        let mclks_per_slot = self.fifo_mclks_per_slot();

        while self.fifo_entries > 0 && self.fifo_mclk_counter >= mclks_per_slot {
            self.fifo_mclk_counter -= mclks_per_slot;
            self.fifo_entries -= 1;
        }

        // If FIFO drained completely, reset counter
        if self.fifo_entries == 0 {
            self.fifo_mclk_counter = 0;
        }
    }

    /// Record a written word in the FIFO data ring (the open-bus source for
    /// later partial-width reads). Advances `fifo_idx` so `fifo[fifo_idx]` is the
    /// oldest of the last four words.
    pub(super) fn push_fifo_word(&mut self, value: u16) {
        self.fifo[self.fifo_idx] = value;
        self.fifo_idx = (self.fifo_idx + 1) & 3;
    }

    /// Push a data port write into the FIFO.
    /// Returns the number of CPU wait cycles if the FIFO was full (CPU stalls).
    /// Returns 0 if there was room in the FIFO.
    pub fn push_fifo(&mut self) -> u32 {
        if self.fifo_entries < 4 {
            // Room in FIFO, no stall
            self.fifo_entries += 1;
            return 0;
        }

        // FIFO is full - CPU must wait until one slot drains
        let mclks_per_slot = self.fifo_mclks_per_slot();
        let remaining = mclks_per_slot.saturating_sub(self.fifo_mclk_counter);

        // Drain one entry
        self.fifo_mclk_counter = 0;
        // FIFO stays at 4 since we're pushing one and draining one

        // Convert master clocks to CPU cycles (round up)
        remaining.div_ceil(7) as u32
    }

    /// Reserve the VDP FIFO access slot for the data-port write about to be
    /// pushed at `fifo_idx`, recording its absolute drain time in
    /// `fifo_slot_mclks[fifo_idx]`. During active display the VDP only
    /// services the FIFO at fixed dots (`FIFO_TIMING_*`), so consecutive
    /// writes chain onto successive future slots and the status full/empty
    /// bits then read off those timestamps (the model Genesis Plus GX uses;
    /// the status bits were checked against it and against VDPFIFOTesting).
    /// Outside active display the FIFO drains effectively
    /// instantly, so the slot is left untouched (its stale, past timestamp
    /// reads as empty).
    ///
    /// On hardware a write that lands on an already-full FIFO holds the 68k
    /// until the oldest entry has been served. We apply
    /// that wait to the **VDP-internal `fifo_clock` only**, never the CPU
    /// cycle count, so two invariants both hold: a long back-to-back burst's
    /// drain progression is tracked (so the FIFO reads empty again a few slots
    /// after it ends; VDPFIFOTesting "DMA Busy Flag" probes read the empty bit
    /// after 16 writes), yet 68k instruction timing and MAME-trace parity are
    /// left completely untouched (no `pending_cpu_wait_cycles` is raised). The
    /// wait also bounds the chain to ~4 slots ahead of the clock, so the model
    /// never pretends more than the real four entries are in flight.
    fn fifo_record_slot(&mut self) {
        let display_on = (self.registers[1] & 0x40) != 0;
        if !display_on || self.timing.in_vblank {
            return;
        }

        let slots: &[u64] = if (self.registers[12] & 0x01) != 0 {
            &FIFO_TIMING_H40
        } else {
            &FIFO_TIMING_H32
        };

        let newest_served_at = self.fifo_slot_mclks[(self.fifo_idx + 3) & 3];
        let oldest_served_at = self.fifo_slot_mclks[self.fifo_idx];

        // While the write before this one still waits, this one queues
        // behind it. If all four entries are waiting, it cannot enter before
        // the oldest has left, so the VDP-internal clock moves to that
        // moment (the 68k FIFO wait, applied to status timing only, see
        // the doc comment).
        let queued = self.fifo_clock < newest_served_at;
        if queued {
            self.fifo_clock = self.fifo_clock.max(oldest_served_at);
        }
        let ready_at = if queued {
            newest_served_at
        } else {
            self.fifo_clock
        };

        // The first access slot after `ready_at`, counted from the start of
        // the scanline the clock is on. VRAM is byte-wide internally (a word
        // write is two accesses), so it is served one slot later; CRAM and
        // VSRAM take the word in one. The table runs on into the next line,
        // and the clamp guards an index past its end.
        let line_start = self.fifo_clock - (self.fifo_clock % MASTER_CLOCKS_PER_LINE);
        let into_line = ready_at.saturating_sub(line_start);
        let next_slot = slots.partition_point(|&at| at <= into_line);
        let second_access = usize::from((self.code & 0x06) == 0);
        let served = (next_slot + second_access).min(slots.len() - 1);
        self.fifo_slot_mclks[self.fifo_idx] = line_start + slots[served];
    }

    /// Check if FIFO is empty (for status register bit 9).
    pub fn is_fifo_empty(&self) -> bool {
        // Empty once the most-recently-queued entry has been serviced.
        self.fifo_clock >= self.fifo_slot_mclks[(self.fifo_idx + 3) & 3]
    }

    /// Check if FIFO is full (for status register bit 8).
    pub fn is_fifo_full(&self) -> bool {
        // Full while the oldest queued entry has not yet been serviced.
        self.fifo_clock < self.fifo_slot_mclks[self.fifo_idx]
    }

    // ======================================================================
    // DMA Halt Timing
    // ======================================================================
}
