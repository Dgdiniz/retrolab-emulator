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
//!
//! While the PPU notes who writes video memory (`crate::ppu::writes`: a
//! live recorder or a debugger watch armed), a block and an HDMA line run
//! through a copy of their loop, the `NOTE` instance of the same generic
//! code, that names the writer of every byte it moves, sends the bytes
//! through the PPU's noting path and logs the block, the unit and the
//! frame's HDMA set-up. With nothing armed the loops run as they were: the
//! choice is one test per block and per HDMA line, none per byte.

use retro_core::capture::{FrameWrite, WriteKind, WriteOrigin};

use super::SnesBus;
use crate::dma::{
    bbus_offset, dma_addr_valid, dma_transfer_valid, hdma_bytes_per_line, hdma_reload_due,
    hdma_repeat, stepped, Direction, DmaChannel,
};
use crate::ppu::writes::port;

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

    /// Move one byte between an A-bus address and a B-bus port, and
    /// return the byte moved. With `NOTE`, the PPU notes what its ports
    /// take.
    fn move_byte<const NOTE: bool>(&mut self, direction: Direction, abus: u32, port: u8) -> u8 {
        let b_addr = b_bus_address(port);
        match direction {
            Direction::AToB => {
                let value = self.dma_read_abus(abus);
                if dma_transfer_valid(port, abus) {
                    self.write_inner::<NOTE>(b_addr, value);
                }
                value
            }
            Direction::BToA => {
                let value = if dma_transfer_valid(port, abus) {
                    self.read_inner(b_addr)
                } else {
                    0
                };
                if dma_addr_valid(abus) {
                    self.write_inner::<NOTE>(abus, value);
                }
                value
            }
        }
    }

    /// Before a noted transfer moves a byte: name the writer, and note the
    /// places the PPU's ports do not see (work RAM through its port, the
    /// A bus of a transfer from the B bus).
    fn name_transfer_byte(
        &mut self,
        origin: WriteOrigin,
        direction: Direction,
        abus: u32,
        port: u8,
    ) {
        let place = match direction {
            Direction::AToB if port == port::WORK_RAM && dma_transfer_valid(port, abus) => {
                Some(self.wmdata.addr())
            }
            Direction::BToA if dma_addr_valid(abus) => Some(abus),
            _ => None,
        };
        let ppu = self.b_bus.ppu_mut();
        ppu.set_writer(origin);
        if let Some(place) = place {
            ppu.extend_write_span(place);
        }
    }

    /// A noted transfer is about to start on `channel`: keep room for its
    /// record and start noting the places it writes. Returns what the record
    /// needs from the start.
    fn begin_noted_transfer(&mut self, channel: &DmaChannel) -> TransferStart {
        let port_place = match channel.direction() {
            Direction::AToB if channel.b_bus_port == port::WORK_RAM => Some(self.wmdata.addr()),
            Direction::AToB => self.b_bus.ppu().port_place(channel.b_bus_port),
            Direction::BToA => None,
        };
        let ppu = self.b_bus.ppu_mut();
        let room = ppu.reserve_frame_write();
        ppu.open_write_span();
        TransferStart {
            room,
            frame: ppu.recording_frame(),
            line: self.cached_v_scanline,
            dot: self.cached_h_dot,
            port_place,
        }
    }

    /// The record a noted transfer on `channel` starts with: what every kind
    /// takes from the channel and from the transfer's start.
    fn transfer_record(
        kind: WriteKind,
        index: usize,
        channel: &DmaChannel,
        start: &TransferStart,
    ) -> FrameWrite {
        FrameWrite {
            kind: kind as u8,
            channel: index as u8,
            port_to_memory: channel.b_to_a(),
            unit: channel.mode(),
            frame: start.frame,
            line: start.line,
            position: start.dot,
            target: channel.b_bus_port,
            ..FrameWrite::default()
        }
    }

    /// The processor writes again, from the instruction last stamped.
    fn name_processor_writer(&mut self) {
        let pc = self.trace_pc;
        self.b_bus.ppu_mut().set_writer(WriteOrigin::processor(pc));
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
        if self.b_bus.ppu().notes_port_writes() {
            self.general_dma::<true>(mask)
        } else {
            self.general_dma::<false>(mask)
        }
    }

    /// [`Self::run_general_dma`]'s loop; with `NOTE`, each byte names its
    /// writer and each channel's block is logged.
    fn general_dma<const NOTE: bool>(&mut self, mask: u8) -> u64 {
        let mut stolen = SLOT;
        for channel in (0..8).filter(|c| mask & (1 << c) != 0) {
            stolen += SLOT;
            let plan = self.dma.channels[channel];
            let direction = plan.direction();
            let mode = plan.mode();
            let step = plan.a_step();
            let bank = u32::from(plan.a_bus_bank) << 16;
            let mut offset = plan.a_bus_address;
            let start = if NOTE {
                Some(self.begin_noted_transfer(&plan))
            } else {
                None
            };
            let length = block_length(plan.byte_count);
            for k in 0..length {
                let port = plan.b_bus_port.wrapping_add(bbus_offset(mode, k));
                let abus = bank | u32::from(offset);
                if NOTE {
                    let origin = WriteOrigin::transfer(channel as u32, abus);
                    self.name_transfer_byte(origin, direction, abus, port);
                }
                self.move_byte::<NOTE>(direction, abus, port);
                offset = stepped(offset, step);
                stolen += SLOT;
            }
            if let Some(start) = start {
                self.log_dma_block(channel, &plan, &start, length);
            }
            let done = &mut self.dma.channels[channel];
            done.a_bus_address = offset;
            done.byte_count = 0;
        }
        if NOTE {
            self.name_processor_writer();
        }
        self.system_regs.clear_mdmaen();
        stolen
    }

    /// Fill a DMA block's record: the first and the last place it wrote, or
    /// where its port pointed when nothing reached the memory.
    fn log_dma_block(
        &mut self,
        index: usize,
        channel: &DmaChannel,
        start: &TransferStart,
        length: u32,
    ) {
        let ppu = self.b_bus.ppu_mut();
        let (first, last, nothing_landed) = match (ppu.close_write_span(), start.port_place) {
            (Some((first, last)), _) => (first, last, false),
            (None, Some(place)) => (place, place, true),
            (None, None) => (0, 0, false),
        };
        if let Some(room) = start.room {
            let record = FrameWrite {
                source: channel.a_bus_pointer(),
                count: length,
                start: first,
                end_or_value: last,
                nothing_landed,
                ..Self::transfer_record(WriteKind::Transfer, index, channel, start)
            };
            ppu.fill_frame_write(room, &record);
        }
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
    /// With `NOTE`, each byte names its writer and the unit is logged.
    fn hdma_transfer_unit<const NOTE: bool>(&mut self, channel: usize) -> u64 {
        let mut ch = self.dma.channels[channel];
        let direction = ch.direction();
        let mode = ch.mode();
        let length = u32::from(hdma_bytes_per_line(mode));
        let start = if NOTE {
            Some(self.begin_noted_transfer(&ch))
        } else {
            None
        };
        // Where the unit's first byte is read: the record's source.
        let first_byte = match (NOTE, ch.indirect()) {
            (false, _) => 0,
            (true, true) => ch.indirect_data_pointer(),
            (true, false) => ch.table_pointer(),
        };
        let mut moved = 0u32;
        // A byte written into a B-bus register is seen as the processor's
        // store to it is: the debugger's records and the traces.
        let observed = matches!(direction, Direction::AToB) && self.records_accesses();
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
            if NOTE {
                let origin = WriteOrigin::line_transfer(channel as u32, abus);
                self.name_transfer_byte(origin, direction, abus, port);
            }
            let value = self.move_byte::<NOTE>(direction, abus, port);
            if observed && dma_transfer_valid(port, abus) {
                self.record_access(true, b_bus_address(port), value);
            }
            if NOTE {
                moved |= u32::from(value) << (8 * k);
            }
        }
        self.dma.channels[channel] = ch;
        if let Some(start) = start {
            let ppu = self.b_bus.ppu_mut();
            let first_place = ppu.close_write_span().map_or(0, |(first, _)| first);
            if let Some(room) = start.room {
                let record = FrameWrite {
                    indirect: ch.indirect(),
                    source: first_byte,
                    count: length,
                    start: first_place,
                    end_or_value: moved,
                    extra: if ch.indirect() { first_byte } else { 0 },
                    ..Self::transfer_record(WriteKind::LineTransferUnit, channel, &ch, &start)
                };
                ppu.fill_frame_write(room, &record);
            }
        }
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
        if self.b_bus.ppu().records_writes() {
            self.log_hdma_setup(enabled);
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

    /// Log one set-up record for each channel in `enabled`: its table's
    /// start on the A bus, its port, unit and direction.
    fn log_hdma_setup(&mut self, enabled: u8) {
        let frame = self.b_bus.ppu().recording_frame();
        let (line, dot) = (self.cached_v_scanline, self.cached_h_dot);
        for index in (0..8).filter(|c| enabled & (1 << c) != 0) {
            let channel = self.dma.channels[index];
            let record = FrameWrite {
                kind: WriteKind::LineTransferSetUp as u8,
                channel: index as u8,
                port_to_memory: channel.b_to_a(),
                unit: channel.mode(),
                indirect: channel.indirect(),
                frame,
                line,
                position: dot,
                source: channel.a_bus_pointer(),
                target: channel.b_bus_port,
                ..FrameWrite::default()
            };
            self.b_bus.ppu_mut().push_frame_write(&record);
        }
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
        if self.b_bus.ppu().notes_port_writes() {
            self.hdma_line::<true>(enabled, running)
        } else {
            self.hdma_line::<false>(enabled, running)
        }
    }

    /// [`Self::hdma_run_line`]'s two passes over the `running` channels;
    /// with `NOTE`, each unit is logged and names the writer of its bytes.
    fn hdma_line<const NOTE: bool>(&mut self, enabled: u8, running: u8) -> u64 {
        let mut stolen = SLOT;
        for channel in (0..8).filter(|c| running & (1 << c) != 0) {
            if self.dma.channels[channel].transfer_this_line {
                stolen += self.hdma_transfer_unit::<NOTE>(channel);
            }
        }
        for channel in (0..8).filter(|c| running & (1 << c) != 0) {
            let ch = &mut self.dma.channels[channel];
            ch.line_counter = ch.line_counter.wrapping_sub(1);
            ch.transfer_this_line = hdma_repeat(ch.line_counter);
            stolen += self.hdma_read_table(channel, enabled);
        }
        if NOTE {
            self.name_processor_writer();
        }
        stolen
    }
}

/// What the record of a noted transfer takes from the transfer's start.
struct TransferStart {
    /// The room kept in the log for the record (its place in the log), or
    /// `None` when the log is full or not recorded.
    room: Option<usize>,
    /// The frame being built.
    frame: u64,
    /// The raster position the bus held.
    line: u16,
    dot: u16,
    /// Where the block's port pointed in its memory, for a port into
    /// VRAM, CGRAM, OAM or work RAM.
    port_place: Option<u32>,
}
