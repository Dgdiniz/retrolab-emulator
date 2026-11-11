//! The DMA engine's transfers: a general DMA block, and HDMA's per-frame
//! set-up and per-line step. They live under `bus` because every byte is
//! read from one bus and written to the other through the bus's own
//! dispatch (`read_inner` / `write_inner`, private to the parent module).
//!
//! The engine runs in slots of 8 master clocks: one per byte moved, one
//! per channel and one per block of general DMA, one per HDMA table byte
//! read and one to set an HDMA frame or line up. Every entry point returns
//! the clocks it spent as cycles stolen from the CPU; `system.rs` folds
//! them into the master clock.
//!
//! What the engine cannot do, it skips rather than faults: it never drives
//! the A-bus at the B-bus window or the 5A22's registers (such a read gives
//! 0, such a write is lost), and it cannot copy WRAM through WRAM's own
//! `$2180` port (see [`crate::dma::dma_addr_valid`] and
//! [`crate::dma::dma_transfer_valid`]).

use super::SnesBus;
use crate::dma::{
    bbus_offset, dma_addr_valid, dma_transfer_valid, hdma_bytes_per_line, hdma_reload_due,
    hdma_repeat, stepped, Direction,
};

/// One slot of the engine's clock, in master cycles.
const SLOT: u64 = 8;

/// The B-bus address of a port given as an offset from `$2100`.
#[inline]
fn b_bus_address(port: u8) -> u32 {
    0x2100 | u32::from(port)
}

/// Bytes a general DMA moves for a `DAS` value: 0 means 65536.
#[inline]
fn block_length(byte_count: u16) -> u32 {
    if byte_count == 0 {
        0x1_0000
    } else {
        u32::from(byte_count)
    }
}

impl SnesBus {
    /// The channel mask the last `$420B` write asked for, cleared on the
    /// way out. Zero when nothing is pending.
    pub fn take_pending_general_dma(&mut self) -> u8 {
        core::mem::take(&mut self.pending_general_dma)
    }

    /// A byte the engine reads from the A-bus: 0, without a bus access,
    /// where the engine cannot reach.
    fn dma_read_abus(&mut self, abus: u32) -> u8 {
        if dma_addr_valid(abus) {
            self.read_inner(abus)
        } else {
            0
        }
    }

    /// Move one byte between an A-bus address and a B-bus port.
    fn move_byte(&mut self, direction: Direction, abus: u32, port: u8) {
        let b_addr = b_bus_address(port);
        match direction {
            Direction::AToB => {
                let value = self.dma_read_abus(abus);
                if dma_transfer_valid(port, abus) {
                    self.write_inner(b_addr, value);
                }
            }
            Direction::BToA => {
                let value = if dma_transfer_valid(port, abus) {
                    self.read_inner(b_addr)
                } else {
                    0
                };
                if dma_addr_valid(abus) {
                    self.write_inner(abus, value);
                }
            }
        }
    }

    /// Run the general DMA of every channel in `mask` (the `$420B` value),
    /// lowest channel first, each block whole, and return the clocks spent.
    ///
    /// A block moves `DAS` bytes (0 for 65536), each to the B-bus port the
    /// channel's unit names for it, the A-bus address stepping as DMAP
    /// says. When the block is done the channel's `DAS` reads 0 and its
    /// `A1T` holds the address after the last byte, as on the hardware,
    /// and `$420B` is cleared.
    pub fn run_general_dma(&mut self, mask: u8) -> u64 {
        let mut stolen = SLOT;
        for channel in (0..8).filter(|c| mask & (1 << c) != 0) {
            stolen += SLOT;
            let plan = self.dma.channels[channel];
            let direction = plan.direction();
            let mode = plan.mode();
            let step = plan.a_step();
            let bank = u32::from(plan.a_bus_bank) << 16;
            let mut offset = plan.a_bus_address;
            for k in 0..block_length(plan.byte_count) {
                let port = plan.b_bus_port.wrapping_add(bbus_offset(mode, k));
                self.move_byte(direction, bank | u32::from(offset), port);
                offset = stepped(offset, step);
                stolen += SLOT;
            }
            let done = &mut self.dma.channels[channel];
            done.a_bus_address = offset;
            done.byte_count = 0;
        }
        self.system_regs.clear_mdmaen();
        stolen
    }

    /// Whether channel `channel` runs HDMA this frame and has not reached
    /// the end of its table.
    #[inline]
    fn hdma_running(&self, channel: usize, enabled: u8) -> bool {
        enabled & (1 << channel) != 0 && !self.dma.channels[channel].table_done
    }

    /// The channels still running their tables, as a bit per channel.
    fn hdma_running_mask(&self, enabled: u8) -> u8 {
        (0..8)
            .filter(|&c| self.hdma_running(c, enabled))
            .fold(0, |mask, c| mask | (1 << c))
    }

    /// Whether any channel above `channel` is still running its table.
    fn hdma_running_after(&self, channel: usize, enabled: u8) -> bool {
        (channel + 1..8).any(|c| self.hdma_running(c, enabled))
    }

    /// Read a channel's HDMA table at the line's pace and return the
    /// clocks spent.
    ///
    /// The engine fetches one table byte every line whether it needs it
    /// or not. When the current entry is spent, that byte starts the next
    /// one: its line count and repeat bit, or 0 for the end of the table.
    /// In indirect mode the entry's two address bytes follow, except on
    /// the entry that ends the table, where the second is fetched only
    /// while a later channel is still running; otherwise the first byte
    /// lands in the upper half of `DAS`, which is what the hardware does.
    fn hdma_read_table(&mut self, channel: usize, enabled: u8) -> u64 {
        let mut stolen = SLOT;
        let mut ch = self.dma.channels[channel];
        let byte = self.dma_read_abus(ch.table_pointer());
        if hdma_reload_due(ch.line_counter) {
            ch.line_counter = byte;
            ch.table_address = ch.table_address.wrapping_add(1);
            ch.table_done = byte == 0;
            ch.transfer_this_line = !ch.table_done;
            if ch.indirect() {
                stolen += SLOT;
                let first = self.dma_read_abus(ch.table_pointer());
                ch.table_address = ch.table_address.wrapping_add(1);
                let fetch_second = !ch.table_done || self.hdma_running_after(channel, enabled);
                ch.byte_count = if fetch_second {
                    stolen += SLOT;
                    let second = self.dma_read_abus(ch.table_pointer());
                    ch.table_address = ch.table_address.wrapping_add(1);
                    u16::from_le_bytes([first, second])
                } else {
                    u16::from(first) << 8
                };
            }
        }
        self.dma.channels[channel] = ch;
        stolen
    }

    /// Move one unit for channel `channel`, from the table itself, or
    /// from the entry's indirect address, and return the clocks spent.
    fn hdma_transfer_unit(&mut self, channel: usize) -> u64 {
        let mut ch = self.dma.channels[channel];
        let direction = ch.direction();
        let mode = ch.mode();
        let length = u32::from(hdma_bytes_per_line(mode));
        for k in 0..length {
            let port = ch.b_bus_port.wrapping_add(bbus_offset(mode, k));
            let abus = if ch.indirect() {
                let a = ch.indirect_data_pointer();
                ch.byte_count = ch.byte_count.wrapping_add(1);
                a
            } else {
                let a = ch.table_pointer();
                ch.table_address = ch.table_address.wrapping_add(1);
                a
            };
            self.move_byte(direction, abus, port);
        }
        self.dma.channels[channel] = ch;
        u64::from(length) * SLOT
    }

    /// Set HDMA up for a frame and return the clocks spent: every channel
    /// forgets last frame's end and line, and each enabled one starts its
    /// table from `A1T` and reads the first entry. Nothing is charged when
    /// no channel is enabled.
    pub fn hdma_init_frame(&mut self) -> u64 {
        for ch in self.dma.channels.iter_mut() {
            ch.table_done = false;
            ch.transfer_this_line = false;
        }
        let enabled = self.system_regs.hdmaen();
        if enabled == 0 {
            return 0;
        }
        let mut stolen = SLOT;
        for channel in (0..8).filter(|c| enabled & (1 << c) != 0) {
            let ch = &mut self.dma.channels[channel];
            ch.table_address = ch.a_bus_address;
            ch.line_counter = 0;
            stolen += self.hdma_read_table(channel, enabled);
        }
        stolen
    }

    /// One scanline of HDMA, and the clocks spent. First every running
    /// channel whose line calls for it moves its unit; then every running
    /// channel counts the line down, decides from the repeat bit whether
    /// the next line transfers, and reads its table. The two halves are
    /// separate because a channel's table read must not change what a
    /// later channel moves on this line. Nothing is charged when no
    /// channel is running.
    pub fn hdma_run_line(&mut self) -> u64 {
        let enabled = self.system_regs.hdmaen();
        let running = self.hdma_running_mask(enabled);
        if running == 0 {
            return 0;
        }
        let mut stolen = SLOT;
        for channel in (0..8).filter(|c| running & (1 << c) != 0) {
            if self.dma.channels[channel].transfer_this_line {
                stolen += self.hdma_transfer_unit(channel);
            }
        }
        for channel in (0..8).filter(|c| running & (1 << c) != 0) {
            let ch = &mut self.dma.channels[channel];
            ch.line_counter = ch.line_counter.wrapping_sub(1);
            ch.transfer_this_line = hdma_repeat(ch.line_counter);
            stolen += self.hdma_read_table(channel, enabled);
        }
        stolen
    }
}
