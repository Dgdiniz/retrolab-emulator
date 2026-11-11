//! The 5A22's DMA controller: eight channels that move bytes between the
//! A-bus (the CPU's address space) and the B-bus (the `$2100-$21FF`
//! window of PPU and APU ports), either as one block started by a write
//! to `$420B` (general DMA) or as a few bytes per scanline read from a
//! table while `$420C` enables the channel (HDMA).
//!
//! This module holds what a channel *is*: its registers (`$43x0-$43xB`),
//! the two flags HDMA keeps for itself, and the arithmetic that needs no
//! bus, which B-bus port the k-th byte of a transfer goes to, how the
//! A-bus address steps, which addresses the engine cannot reach. The
//! transfers themselves, which read one bus and write the other, are in
//! `bus/dma.rs`.
//!
//! Written from the register documentation (the `$43xN` layout, the eight
//! transfer units, the HDMA table format); its behaviour was checked
//! against bsnes-plus.

/// The eight transfer units: which B-bus ports, counted from `BBAD`, the
/// bytes of one unit go to, in order. A general DMA repeats the unit for
/// the whole block; an HDMA line moves exactly one unit.
///
/// | mode | ports            | meaning                    |
/// |------|------------------|----------------------------|
/// | 0    | p                | one port                   |
/// | 1    | p, p+1           | two ports                  |
/// | 2    | p, p             | one port, twice            |
/// | 3    | p, p, p+1, p+1   | two ports, twice each      |
/// | 4    | p, p+1, p+2, p+3 | four ports                 |
/// | 5    | p, p+1, p, p+1   | two ports, alternating     |
/// | 6    | p, p             | as mode 2                  |
/// | 7    | p, p, p+1, p+1   | as mode 3                  |
const UNIT_PORTS: [[u8; 4]; 8] = [
    [0, 0, 0, 0],
    [0, 1, 0, 1],
    [0, 0, 0, 0],
    [0, 0, 1, 1],
    [0, 1, 2, 3],
    [0, 1, 0, 1],
    [0, 0, 0, 0],
    [0, 0, 1, 1],
];

/// Bytes in one unit of each transfer mode.
const UNIT_LENGTH: [u8; 8] = [1, 2, 2, 4, 4, 4, 2, 4];

/// How the A-bus address moves after each byte of a general DMA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressStep {
    Increment,
    Decrement,
    Fixed,
}

/// Which way the bytes go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// From the A-bus to a B-bus port, the usual upload to the PPU or APU.
    AToB,
    /// From a B-bus port to the A-bus.
    BToA,
}

/// One channel: its registers and the two HDMA flags.
///
/// `byte_count` serves two purposes the hardware never needs at once: the byte
/// count of a general DMA, and in HDMA indirect mode the address the
/// current entry's bytes are read from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DmaChannel {
    /// `$43x0` DMAP: bit 7 direction (0 = A->B), bit 6 HDMA indirect, bit 4
    /// A-bus decrement, bit 3 A-bus fixed, bits 0-2 the transfer unit.
    /// Every bit reads back, so it is kept whole.
    pub control: u8,
    /// `$43x1` BBAD: the low byte of the B-bus port, `$21xx`.
    pub b_bus_port: u8,
    /// `$43x2-$43x3` A1T: the A-bus address a general DMA walks, and the
    /// address an HDMA table starts at.
    pub a_bus_address: u16,
    /// `$43x4` A1B: the bank of that address. Never stepped.
    pub a_bus_bank: u8,
    /// `$43x5-$43x6` DAS: a general DMA's byte count (0 counts as 65536,
    /// and it reads 0 once the block is done), or an HDMA indirect entry's
    /// data address.
    pub byte_count: u16,
    /// `$43x7` DASB: the bank of that indirect data.
    pub indirect_bank: u8,
    /// `$43x8-$43x9` A2A: where the HDMA table is being read.
    pub table_address: u16,
    /// `$43xA` NTRL: the HDMA line counter, bit 7 repeat, bits 0-6 the
    /// lines left in the current entry.
    pub line_counter: u8,
    /// `$43xB` (also at `$43xF`): a byte with no function; games use it.
    pub unused: u8,
    /// HDMA: the table reached its end (a 0 entry) this frame.
    pub table_done: bool,
    /// HDMA: this line gets a transfer.
    pub transfer_this_line: bool,
}

impl DmaChannel {
    /// DMAP bit 7: true when bytes go from a B-bus port to the A-bus.
    #[inline]
    pub fn b_to_a(&self) -> bool {
        self.control & 0x80 != 0
    }

    /// The channel's direction.
    #[inline]
    pub fn direction(&self) -> Direction {
        if self.b_to_a() {
            Direction::BToA
        } else {
            Direction::AToB
        }
    }

    /// DMAP bit 6: HDMA reads an address from each table entry instead of
    /// the data itself.
    #[inline]
    pub fn indirect(&self) -> bool {
        self.control & 0x40 != 0
    }

    /// DMAP bit 3: the A-bus address stays put during a general DMA.
    #[inline]
    pub fn a_fixed(&self) -> bool {
        self.control & 0x08 != 0
    }

    /// DMAP bit 4: the A-bus address counts down (unless fixed).
    #[inline]
    pub fn a_decrement(&self) -> bool {
        self.control & 0x10 != 0
    }

    /// How the A-bus address moves after each byte of a general DMA.
    #[inline]
    pub fn a_step(&self) -> AddressStep {
        match (self.a_fixed(), self.a_decrement()) {
            (true, _) => AddressStep::Fixed,
            (false, true) => AddressStep::Decrement,
            (false, false) => AddressStep::Increment,
        }
    }

    /// DMAP bits 0-2: the transfer unit.
    #[inline]
    pub fn mode(&self) -> u8 {
        self.control & 0x07
    }

    /// Read the register at `offset` (`$43x0 + offset`, low nibble).
    /// `$43xC-$43xE` have no register, so the bus answers with open bus:
    /// `None`. `$43xF` is `$43xB` again.
    pub fn read_reg(&self, offset: u8) -> Option<u8> {
        let [count_lo, count_hi] = self.byte_count.to_le_bytes();
        let [a_bus_lo, a_bus_hi] = self.a_bus_address.to_le_bytes();
        let [table_lo, table_hi] = self.table_address.to_le_bytes();
        let value = match offset & 0x0F {
            0x0 => self.control,
            0x1 => self.b_bus_port,
            0x2 => a_bus_lo,
            0x3 => a_bus_hi,
            0x4 => self.a_bus_bank,
            0x5 => count_lo,
            0x6 => count_hi,
            0x7 => self.indirect_bank,
            0x8 => table_lo,
            0x9 => table_hi,
            0xA => self.line_counter,
            0xB | 0xF => self.unused,
            _ => return None,
        };
        Some(value)
    }

    /// Write the register at `offset`. A write to `$43xC-$43xE` lands
    /// nowhere; `$43xF` is `$43xB` again.
    pub fn write_reg(&mut self, offset: u8, value: u8) {
        match offset & 0x0F {
            0x0 => self.control = value,
            0x1 => self.b_bus_port = value,
            0x2 => self.a_bus_address = with_low_byte(self.a_bus_address, value),
            0x3 => self.a_bus_address = with_high_byte(self.a_bus_address, value),
            0x4 => self.a_bus_bank = value,
            0x5 => self.byte_count = with_low_byte(self.byte_count, value),
            0x6 => self.byte_count = with_high_byte(self.byte_count, value),
            0x7 => self.indirect_bank = value,
            0x8 => self.table_address = with_low_byte(self.table_address, value),
            0x9 => self.table_address = with_high_byte(self.table_address, value),
            0xA => self.line_counter = value,
            0xB | 0xF => self.unused = value,
            _ => {}
        }
    }

    /// The 24-bit A-bus address a general DMA reads or writes next.
    #[inline]
    pub fn a_bus_pointer(&self) -> u32 {
        (u32::from(self.a_bus_bank) << 16) | u32::from(self.a_bus_address)
    }

    /// The 24-bit address of the next HDMA table byte.
    #[inline]
    pub fn table_pointer(&self) -> u32 {
        (u32::from(self.a_bus_bank) << 16) | u32::from(self.table_address)
    }

    /// The 24-bit address of the next byte of an indirect HDMA entry.
    #[inline]
    pub fn indirect_data_pointer(&self) -> u32 {
        (u32::from(self.indirect_bank) << 16) | u32::from(self.byte_count)
    }
}

#[inline]
fn with_low_byte(word: u16, byte: u8) -> u16 {
    u16::from_le_bytes([byte, word.to_le_bytes()[1]])
}

#[inline]
fn with_high_byte(word: u16, byte: u8) -> u16 {
    u16::from_le_bytes([word.to_le_bytes()[0], byte])
}

/// The eight channels. The enable masks (`$420B` MDMAEN, `$420C` HDMAEN)
/// are 5A22 system registers and live in [`crate::system_regs::SystemRegs`];
/// the engine in `bus/dma.rs` reads them from there.
#[derive(Debug, Clone)]
pub struct DmaController {
    /// Channels 0-7.
    pub channels: [DmaChannel; 8],
}

impl Default for DmaController {
    fn default() -> Self {
        Self::new()
    }
}

impl DmaController {
    /// All eight channels zeroed. The hardware powers up with its registers
    /// undefined; every game writes a channel before using it, and a zero
    /// start keeps the core deterministic.
    pub fn new() -> Self {
        Self {
            channels: [DmaChannel::default(); 8],
        }
    }

    /// Back to the power-on state, HDMA flags included.
    pub fn reset(&mut self) {
        self.channels = [DmaChannel::default(); 8];
    }

    /// The channels as the savestate records them.
    pub fn to_savestate(&self) -> crate::savestate::DmaControllerState {
        crate::savestate::DmaControllerState {
            channels: core::array::from_fn(|i| {
                let c = &self.channels[i];
                crate::savestate::DmaChannelState {
                    params: c.control,
                    dest_addr: c.b_bus_port,
                    source_addr: c.a_bus_address,
                    source_bank: c.a_bus_bank,
                    das: c.byte_count,
                    indirect_bank: c.indirect_bank,
                    hdma_addr: c.table_address,
                    line_counter: c.line_counter,
                    unused: c.unused,
                    hdma_completed: c.table_done,
                    hdma_do_transfer: c.transfer_this_line,
                }
            }),
        }
    }

    /// The channels from a savestate.
    pub fn from_savestate(&mut self, s: &crate::savestate::DmaControllerState) {
        for (c, saved) in self.channels.iter_mut().zip(s.channels.iter()) {
            *c = DmaChannel {
                control: saved.params,
                b_bus_port: saved.dest_addr,
                a_bus_address: saved.source_addr,
                a_bus_bank: saved.source_bank,
                byte_count: saved.das,
                indirect_bank: saved.indirect_bank,
                table_address: saved.hdma_addr,
                line_counter: saved.line_counter,
                unused: saved.unused,
                table_done: saved.hdma_completed,
                transfer_this_line: saved.hdma_do_transfer,
            };
        }
    }

    /// Read register `offset` of channel `channel` (0-7). `None` is open bus.
    pub fn read_reg(&self, channel: usize, offset: u8) -> Option<u8> {
        self.channels[channel & 0x7].read_reg(offset)
    }

    /// Write register `offset` of channel `channel` (0-7).
    pub fn write_reg(&mut self, channel: usize, offset: u8, value: u8) {
        self.channels[channel & 0x7].write_reg(offset, value);
    }
}

// -- Arithmetic that needs no bus ---------------------------------------

/// The B-bus port (as an offset from `BBAD`) the `index`-th byte of a
/// transfer in `mode` goes to.
#[inline]
pub fn bbus_offset(mode: u8, index: u32) -> u8 {
    let mode = (mode & 0x07) as usize;
    UNIT_PORTS[mode][(index % u32::from(UNIT_LENGTH[mode])) as usize]
}

/// Bytes one HDMA line moves in `mode`: the length of one unit.
#[inline]
pub fn hdma_bytes_per_line(mode: u8) -> u8 {
    UNIT_LENGTH[(mode & 0x07) as usize]
}

/// The A-bus address after one byte of a general DMA. Only the 16-bit
/// offset moves; the bank never does.
#[inline]
pub fn step_source_addr(addr: u16, fixed: bool, decrement: bool) -> u16 {
    let step = match (fixed, decrement) {
        (true, _) => AddressStep::Fixed,
        (false, true) => AddressStep::Decrement,
        (false, false) => AddressStep::Increment,
    };
    stepped(addr, step)
}

/// The A-bus address after one byte, by [`AddressStep`].
#[inline]
pub fn stepped(addr: u16, step: AddressStep) -> u16 {
    match step {
        AddressStep::Increment => addr.wrapping_add(1),
        AddressStep::Decrement => addr.wrapping_sub(1),
        AddressStep::Fixed => addr,
    }
}

/// The current HDMA entry is spent: the next table byte starts a new one.
#[inline]
pub fn hdma_reload_due(line_counter: u8) -> bool {
    line_counter & 0x7F == 0
}

/// The entry's repeat bit: a transfer on every one of its lines rather
/// than on the first only.
#[inline]
pub fn hdma_repeat(line_counter: u8) -> bool {
    line_counter & 0x80 != 0
}

/// The system-bank addresses the engine cannot drive on the A-bus: the
/// B-bus window (its other side), the joypad ports and the 5A22's own
/// registers, DMA's included. Offsets within banks `$00-$3F` and
/// `$80-$BF`; every other bank is plain memory to the engine.
const UNREACHABLE_SYSTEM_OFFSETS: [core::ops::RangeInclusive<u32>; 4] = [
    0x2100..=0x21FF,
    0x4000..=0x41FF,
    0x4200..=0x421F,
    0x4300..=0x437F,
];

/// Whether banks `$00-$3F` / `$80-$BF`, the ones whose low half is the
/// system area, hold this address.
#[inline]
fn in_system_banks(abus: u32) -> bool {
    abus & 0x40_0000 == 0
}

/// Whether the engine can read or write this A-bus address. A read from
/// an address it cannot reach gives 0; a write there is lost.
#[inline]
pub fn dma_addr_valid(abus: u32) -> bool {
    if !in_system_banks(abus) {
        return true;
    }
    let offset = abus & 0xFFFF;
    !UNREACHABLE_SYSTEM_OFFSETS
        .iter()
        .any(|range| range.contains(&offset))
}

/// Whether this A-bus address is WRAM: banks `$7E-$7F`, or the first
/// 8 KiB of a system bank, which mirrors the start of WRAM.
#[inline]
fn is_wram(abus: u32) -> bool {
    let bank = abus >> 16;
    (bank == 0x7E || bank == 0x7F) || (in_system_banks(abus) && abus & 0xFFFF < 0x2000)
}

/// Whether a transfer between this B-bus port and this A-bus address can
/// happen at all. WRAM has one port and one bus: a transfer between the
/// `$2180` WRAM data port and WRAM itself would need both at once, so the
/// engine does nothing for it.
#[inline]
pub fn dma_transfer_valid(bbus: u8, abus: u32) -> bool {
    !(bbus == 0x80 && is_wram(abus))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_unit_lists_its_ports_in_order() {
        let ports = |mode: u8| -> Vec<u8> {
            (0..u32::from(hdma_bytes_per_line(mode)))
                .map(|k| bbus_offset(mode, k))
                .collect()
        };
        assert_eq!(ports(0), [0]);
        assert_eq!(ports(1), [0, 1]);
        assert_eq!(ports(2), [0, 0]);
        assert_eq!(ports(3), [0, 0, 1, 1]);
        assert_eq!(ports(4), [0, 1, 2, 3]);
        assert_eq!(ports(5), [0, 1, 0, 1]);
        assert_eq!(ports(6), ports(2));
        assert_eq!(ports(7), ports(3));
        // Mode bits above 2 are not part of the mode.
        assert_eq!(bbus_offset(0xF9, 1), bbus_offset(1, 1));
    }

    #[test]
    fn a_block_repeats_its_unit() {
        // Mode 3 over nine bytes: p p p+1 p+1 p p p+1 p+1 p.
        let seq: Vec<u8> = (0..9).map(|k| bbus_offset(3, k)).collect();
        assert_eq!(seq, [0, 0, 1, 1, 0, 0, 1, 1, 0]);
        // Mode 1 keeps alternating however far the block goes.
        assert_eq!(bbus_offset(1, 65_535), 1);
        assert_eq!(bbus_offset(1, 65_534), 0);
    }

    #[test]
    fn the_a_bus_address_steps_within_its_bank() {
        assert_eq!(stepped(0x1000, AddressStep::Increment), 0x1001);
        assert_eq!(stepped(0x1000, AddressStep::Decrement), 0x0FFF);
        assert_eq!(stepped(0x1000, AddressStep::Fixed), 0x1000);
        assert_eq!(stepped(0xFFFF, AddressStep::Increment), 0x0000);
        assert_eq!(stepped(0x0000, AddressStep::Decrement), 0xFFFF);
        // Fixed wins over decrement, as the DMAP bits say.
        assert_eq!(step_source_addr(0x1000, true, true), 0x1000);
        assert_eq!(step_source_addr(0x1000, false, true), 0x0FFF);
        assert_eq!(step_source_addr(0x1000, false, false), 0x1001);
    }

    #[test]
    fn dmap_bits_decode() {
        let mut ch = DmaChannel {
            control: 0b1100_1010,
            ..Default::default()
        };
        assert_eq!(ch.direction(), Direction::BToA);
        assert!(ch.indirect());
        assert_eq!(ch.a_step(), AddressStep::Fixed);
        assert_eq!(ch.mode(), 2);

        ch.control = 0b0001_0100;
        assert_eq!(ch.direction(), Direction::AToB);
        assert!(!ch.indirect());
        assert_eq!(ch.a_step(), AddressStep::Decrement);
        assert_eq!(ch.mode(), 4);

        ch.control = 0;
        assert_eq!(ch.a_step(), AddressStep::Increment);
    }

    #[test]
    fn an_hdma_entry_ends_when_its_low_bits_run_out() {
        assert!(hdma_reload_due(0x00));
        assert!(hdma_reload_due(0x80));
        assert!(!hdma_reload_due(0x01));
        assert!(!hdma_reload_due(0x7F));
        assert!(!hdma_reload_due(0x85));
        assert!(hdma_repeat(0x80));
        assert!(hdma_repeat(0x85));
        assert!(!hdma_repeat(0x05));
        assert!(!hdma_repeat(0x7F));
    }

    #[test]
    fn registers_read_back_what_was_written() {
        let mut ch = DmaChannel::default();
        let written: [(u8, u8); 12] = [
            (0x0, 0xA3),
            (0x1, 0x18),
            (0x2, 0x34),
            (0x3, 0x12),
            (0x4, 0x7E),
            (0x5, 0x00),
            (0x6, 0x01),
            (0x7, 0x80),
            (0x8, 0xCD),
            (0x9, 0xAB),
            (0xA, 0x05),
            (0xB, 0x42),
        ];
        for (offset, value) in written {
            ch.write_reg(offset, value);
        }
        for (offset, value) in written {
            assert_eq!(ch.read_reg(offset), Some(value), "offset {offset:#x}");
        }
        assert_eq!(ch.a_bus_address, 0x1234);
        assert_eq!(ch.byte_count, 0x0100);
        assert_eq!(ch.table_address, 0xABCD);
        assert_eq!(ch.a_bus_pointer(), 0x7E_1234);
        assert_eq!(ch.table_pointer(), 0x7E_ABCD);
        assert_eq!(ch.indirect_data_pointer(), 0x80_0100);
    }

    #[test]
    fn the_empty_slots_are_open_bus_and_f_is_b_again() {
        let mut ch = DmaChannel::default();
        ch.write_reg(0xB, 0x99);
        assert_eq!(ch.read_reg(0xC), None);
        assert_eq!(ch.read_reg(0xD), None);
        assert_eq!(ch.read_reg(0xE), None);
        assert_eq!(ch.read_reg(0xF), Some(0x99));
        ch.write_reg(0xF, 0x77);
        assert_eq!(ch.read_reg(0xB), Some(0x77));
        ch.write_reg(0xC, 0x11);
        ch.write_reg(0xD, 0x22);
        ch.write_reg(0xE, 0x33);
        assert_eq!(
            ch,
            DmaChannel {
                unused: 0x77,
                ..Default::default()
            }
        );
    }

    #[test]
    fn reset_returns_every_channel_to_power_on() {
        let mut dma = DmaController::new();
        dma.write_reg(3, 0x2, 0xAB);
        dma.channels[3].table_done = true;
        dma.channels[7].transfer_this_line = true;
        dma.reset();
        assert_eq!(dma.read_reg(3, 0x2), Some(0x00));
        assert!(!dma.channels[3].table_done);
        assert!(!dma.channels[7].transfer_this_line);
    }

    #[test]
    fn a_savestate_round_trip_keeps_every_field() {
        let mut dma = DmaController::new();
        for (i, ch) in dma.channels.iter_mut().enumerate() {
            let i = i as u8;
            *ch = DmaChannel {
                control: 0x40 | i,
                b_bus_port: 0x18 + i,
                a_bus_address: 0x1000 + u16::from(i),
                a_bus_bank: 0x7E,
                byte_count: 0x2000 + u16::from(i),
                indirect_bank: 0x7F,
                table_address: 0x3000 + u16::from(i),
                line_counter: 0x80 | i,
                unused: i,
                table_done: i.is_multiple_of(2),
                transfer_this_line: i.is_multiple_of(3),
            };
        }
        let saved = dma.to_savestate();
        let mut restored = DmaController::new();
        restored.from_savestate(&saved);
        assert_eq!(restored.channels, dma.channels);
        // The wire names are the savestate's own; they do not move.
        assert_eq!(saved.channels[5].params, 0x45);
        assert_eq!(saved.channels[5].hdma_addr, 0x3005);
        assert!(saved.channels[6].hdma_completed);
        assert!(saved.channels[6].hdma_do_transfer);
    }

    /// The engine cannot reach the B-bus window, the joypad ports or the
    /// 5A22 registers in the system banks, and can reach everything else.
    #[test]
    fn reachability_follows_the_four_register_ranges() {
        let blocked = |addr: u32| {
            let bank = addr >> 16;
            let system = bank < 0x40 || (0x80..0xC0).contains(&bank);
            let off = addr & 0xFFFF;
            system
                && ((0x2100..=0x21FF).contains(&off)
                    || (0x4000..=0x41FF).contains(&off)
                    || (0x4200..=0x421F).contains(&off)
                    || (0x4300..=0x437F).contains(&off))
        };
        for bank in [
            0x00u32, 0x01, 0x3F, 0x40, 0x7D, 0x7E, 0x7F, 0x80, 0xBF, 0xC0, 0xFF,
        ] {
            for off in 0..=0xFFFFu32 {
                let addr = (bank << 16) | off;
                assert_eq!(dma_addr_valid(addr), !blocked(addr), "{addr:#08x}");
            }
        }
    }

    #[test]
    fn wram_cannot_be_copied_through_its_own_port() {
        for addr in [
            0x7E_0000, 0x7F_FFFF, 0x00_0000, 0x00_1FFF, 0x80_1000, 0x3F_0800,
        ] {
            assert!(!dma_transfer_valid(0x80, addr), "{addr:#08x}");
        }
        for addr in [0x80_8000, 0x00_4000, 0x00_2000, 0x40_0000, 0xC0_0000] {
            assert!(dma_transfer_valid(0x80, addr), "{addr:#08x}");
        }
        // Any other port is unrestricted, WRAM source or not.
        assert!(dma_transfer_valid(0x18, 0x7E_0000));
        assert!(dma_transfer_valid(0x22, 0x00_0000));
        assert!(dma_transfer_valid(0x81, 0x7E_0000));
    }
}
