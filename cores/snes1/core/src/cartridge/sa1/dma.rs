//! The SA-1's DMA (`$2230-$2239`, `$223F`, `$2240-$224F`): a copy between
//! ROM, BW-RAM and I-RAM, and two character conversions that turn a bitmap
//! of packed pixels into SNES characters, whose bits are stored by plane.
//!
//! - **Copy.** From ROM, BW-RAM or I-RAM (DCNT bits 0-1) into I-RAM or BW-RAM
//!   (bit 2), DTC bytes. It starts with the write of the destination
//!   address's middle byte when it goes to I-RAM, its high byte when it goes
//!   to BW-RAM, and is done at once; the chip carries it out
//!   ([`super::Sa1Chip`]'s `run_copy`), since it reaches all three memories.
//!   A copy within one memory moves nothing.
//! - **Conversion as the S-CPU reads** (type 1). The middle-byte write only
//!   arms it. From then on, each time the S-CPU reads BW-RAM at the start of
//!   a character, the character is converted from the bitmap that starts at
//!   SDA into I-RAM at DDA, and every read is answered from there, until
//!   CDMA bit 7 ends it ([`Sa1Dma::convert_on_read`]).
//! - **Conversion from the bitmap registers** (type 2). The program writes
//!   eight pixels, one byte each, into a half of BRF; the write of the
//!   half's last byte converts that row into I-RAM ([`Sa1Dma`]'s
//!   `convert_row`). Sixteen rows fill two characters.
//!
//! A character with `n` bits a pixel takes eight bytes per plane:
//! planes 0 and 1 interleaved row by row in its first 16 bytes, planes 2 and
//! 3 in the next 16, and so on, the layout the PPU reads.
//!
//! The two conversions follow ares's SA-1 (`dmaCC1Read` and `dmaCC2`,
//! `sfc/coprocessor/sa1/dma.cpp`, ISC).

use super::registers::{field, flag, pack, Bytes};
use crate::savestate::Sa1DmaState;

/// Where a copy reads from (DCNT bits 0-1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Source {
    #[default]
    Rom,
    Bwram,
    Iram,
    /// The fourth value, which names no memory: such a copy reads the open
    /// bus.
    None,
}

impl Source {
    fn from_bits(bits: u8) -> Self {
        match bits & 3 {
            0 => Self::Rom,
            1 => Self::Bwram,
            2 => Self::Iram,
            _ => Self::None,
        }
    }

    fn bits(self) -> u8 {
        match self {
            Self::Rom => 0,
            Self::Bwram => 1,
            Self::Iram => 2,
            Self::None => 3,
        }
    }
}

/// What a write of the destination address starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Start {
    /// A copy, which the chip carries out at once.
    Copy,
    /// The type-1 conversion, which waits for the S-CPU's reads.
    ConversionOnRead,
}

/// A copy, as the registers describe it when it starts.
#[derive(Clone, Copy, Debug)]
pub(super) struct CopyJob {
    pub(super) source: Source,
    pub(super) to_bwram: bool,
    /// The first source address (24 bits).
    pub(super) from: u32,
    /// The first destination address (24 bits).
    pub(super) to: u32,
    pub(super) bytes: u16,
}

impl CopyJob {
    /// A copy within one memory, which moves nothing.
    pub(super) fn within_one_memory(&self) -> bool {
        matches!(
            (self.source, self.to_bwram),
            (Source::Bwram, true) | (Source::Iram, false)
        )
    }
}

/// The plane at `plane` of row `row`, as an offset into a character: two
/// planes share each 16 bytes, one byte each per row.
fn plane_offset(row: usize, plane: usize) -> usize {
    plane / 2 * 16 + row * 2 + plane % 2
}

/// The DMA registers and the two character conversions.
#[derive(Default)]
pub(super) struct Sa1Dma {
    /// DCNT (`$2230`) bit 7: DMA enabled; while clear, nothing starts.
    enabled: bool,
    /// DCNT bit 6: priority between DMA and the SA-1 CPU. Kept for the
    /// debugger and savestates; a copy here takes no time, so nothing waits.
    priority: bool,
    /// DCNT bit 5: a character conversion, not a copy.
    converts_characters: bool,
    /// DCNT bit 4: the conversion runs as the S-CPU reads (type 1), not from
    /// the bitmap registers (type 2).
    converts_on_read: bool,
    /// DCNT bit 2: a copy goes to BW-RAM (to I-RAM when clear).
    to_bwram: bool,
    /// DCNT bits 0-1: where a copy reads from.
    source: Source,
    /// CDMA (`$2231`) bit 7: the end of a type-1 conversion.
    conversion_ended: bool,
    /// CDMA bits 2-4: the bitmap's width in characters, as a power of two
    /// (0 to 5: 1 to 32 characters); a larger value is held to 5.
    width_log2: u8,
    /// CDMA bits 0-1: the depth, 0 for 8 bits a pixel, 1 for 4, 2 for 2; a
    /// larger value is held to 2.
    depth: u8,
    /// SDA (`$2232-$2234`): where a copy reads, or where the bitmap starts.
    source_address: u32,
    /// DDA (`$2235-$2237`): where a copy writes, or where in I-RAM the
    /// converted characters go.
    destination_address: u32,
    /// DTC (`$2238-$2239`): how many bytes a copy moves.
    count: u16,
    /// BBF (`$223F`) bit 7: the BW-RAM bitmap view packs 2 bits a pixel
    /// (4 when clear).
    bitmap_2bpp: bool,
    /// BRF (`$2240-$224F`): two rows of eight pixels for the type-2
    /// conversion, one byte a pixel.
    bitmap_registers: [u8; 16],
    /// Type 2: the row (0-15) the next conversion writes; rows 0-7 fill the
    /// first character of a pair, rows 8-15 the second.
    row: u8,
    /// Type 1 is armed: the S-CPU's reads of BW-RAM are answered from
    /// converted characters.
    armed: bool,
}

impl Sa1Dma {
    /// DCNT (`$2230`). Clearing bit 7 also sends the type-2 conversion back
    /// to its first row.
    pub(super) fn write_control(&mut self, value: u8) {
        self.enabled = flag(value, 7);
        self.priority = flag(value, 6);
        self.converts_characters = flag(value, 5);
        self.converts_on_read = flag(value, 4);
        self.to_bwram = flag(value, 2);
        self.source = Source::from_bits(field(value, 0, 2));
        if !self.enabled {
            self.row = 0;
        }
    }

    /// CDMA (`$2231`): depth and width of the conversion; bit 7 ends a
    /// type-1 conversion.
    pub(super) fn write_conversion(&mut self, value: u8) {
        self.conversion_ended = flag(value, 7);
        self.width_log2 = field(value, 2, 3).min(5);
        self.depth = field(value, 0, 2).min(2);
        if self.conversion_ended {
            self.armed = false;
        }
    }

    /// One byte of SDA (`$2232` + `index`).
    pub(super) fn write_source_address(&mut self, index: usize, value: u8) {
        self.source_address = self.source_address.with_byte(index, value);
    }

    /// One byte of DDA (`$2235` + `index`), and what it starts: the middle
    /// byte starts a copy to I-RAM or arms the type-1 conversion, the high
    /// byte starts a copy to BW-RAM.
    pub(super) fn write_destination_address(&mut self, index: usize, value: u8) -> Option<Start> {
        self.destination_address = self.destination_address.with_byte(index, value);
        if !self.enabled {
            return None;
        }
        match (index, self.converts_characters) {
            (1, false) if !self.to_bwram => Some(Start::Copy),
            (1, true) if self.converts_on_read => Some(Start::ConversionOnRead),
            (2, false) if self.to_bwram => Some(Start::Copy),
            _ => None,
        }
    }

    /// One byte of DTC (`$2238` + `index`).
    pub(super) fn write_count(&mut self, index: usize, value: u8) {
        self.count = self.count.with_byte(index, value);
    }

    /// BBF (`$223F`).
    pub(super) fn write_bitmap_format(&mut self, value: u8) {
        self.bitmap_2bpp = flag(value, 7);
    }

    /// One bitmap register (`$2240` + `index`). The last register of a half
    /// converts that row into I-RAM, when the type-2 conversion is chosen.
    pub(super) fn write_bitmap_register(&mut self, index: usize, value: u8, iram: &mut [u8]) {
        if let Some(register) = self.bitmap_registers.get_mut(index) {
            *register = value;
        }
        let half_complete = index == 7 || index == 15;
        if half_complete && self.enabled && self.converts_characters && !self.converts_on_read {
            self.convert_row(iram);
        }
    }

    /// The copy the registers describe, which the caller carries out byte by
    /// byte. The registers are left as the copy leaves them: both addresses
    /// moved on by the count, the count run out.
    pub(super) fn take_copy(&mut self) -> CopyJob {
        let copy = CopyJob {
            source: self.source,
            to_bwram: self.to_bwram,
            from: self.source_address,
            to: self.destination_address,
            bytes: self.count,
        };
        let moved = u32::from(self.count);
        self.source_address = (self.source_address + moved) & 0xff_ffff;
        self.destination_address = (self.destination_address + moved) & 0xff_ffff;
        self.count = 0xffff;
        copy
    }

    /// Arm the type-1 conversion.
    pub(super) fn arm_conversion(&mut self) {
        self.armed = true;
    }

    /// Whether the type-1 conversion is armed.
    pub(super) fn conversion_armed(&self) -> bool {
        self.armed
    }

    /// BBF bit 7: the BW-RAM bitmap view packs 2 bits a pixel.
    pub(super) fn bitmap_2bpp(&self) -> bool {
        self.bitmap_2bpp
    }

    /// Bits a pixel holds: 8, 4 or 2.
    fn bits_per_pixel(&self) -> usize {
        8 >> self.depth
    }

    /// Bytes a character takes: eight rows of one byte per plane.
    fn character_bytes(&self) -> usize {
        8 * self.bits_per_pixel()
    }

    /// The type-1 conversion, at a read of BW-RAM at `offset` (from its
    /// start). A read at the start of a character first converts the next
    /// character of the bitmap, its eight rows of `depth`-bit pixels, packed
    /// low bits first, into I-RAM at DDA; every read is then answered from
    /// I-RAM, where the character waits.
    pub(super) fn convert_on_read(&self, bwram: &[u8], iram: &mut [u8], offset: u32) -> u8 {
        if iram.is_empty() {
            return 0;
        }
        let iram_mask = iram.len() - 1;
        let character_bytes = self.character_bytes();
        let within_character = offset as usize & (character_bytes - 1);
        let destination = self.destination_address as usize;

        if within_character == 0 && !bwram.is_empty() {
            let bwram_mask = bwram.len() - 1;
            let bits = self.bits_per_pixel();
            let columns = 1usize << self.width_log2;
            let line_bytes = columns * bits;
            let index =
                (offset.wrapping_sub(self.source_address) as usize & bwram_mask) / character_bytes;
            let (row_of_characters, column) = (index / columns, index % columns);
            let first =
                self.source_address as usize + row_of_characters * 8 * line_bytes + column * bits;

            for row in 0..8 {
                let start = first + row * line_bytes;
                let mut pixels = 0u64;
                for byte in 0..bits {
                    pixels |= u64::from(bwram[(start + byte) & bwram_mask]) << (8 * byte);
                }
                for plane in 0..bits {
                    let mut out = 0u8;
                    for x in 0..8 {
                        let bit = (pixels >> (x * bits + plane)) & 1;
                        out |= (bit as u8) << (7 - x);
                    }
                    iram[(destination + plane_offset(row, plane)) & iram_mask] = out;
                }
            }
        }

        iram[(destination + within_character) & iram_mask]
    }

    /// The type-2 conversion of the row just completed: eight pixels from a
    /// half of BRF (rows alternate halves) into the row's place in the pair
    /// of characters at DDA, which is taken down to a pair's boundary.
    fn convert_row(&mut self, iram: &mut [u8]) {
        if !iram.is_empty() {
            let mask = iram.len() - 1;
            let row = usize::from(self.row);
            let pixels = &self.bitmap_registers[(row % 2) * 8..][..8];
            let character_bytes = self.character_bytes();
            let pair = (self.destination_address as usize & 0x7ff) & !(2 * character_bytes - 1);
            let character = pair + (row / 8) * character_bytes;
            for plane in 0..self.bits_per_pixel() {
                let out = pixels.iter().enumerate().fold(0u8, |acc, (x, &pixel)| {
                    acc | ((pixel >> plane) & 1) << (7 - x)
                });
                iram[(character + plane_offset(row % 8, plane)) & mask] = out;
            }
        }
        self.row = (self.row + 1) & 15;
    }

    // -- For the debugger -------------------------------------------------

    /// DCNT as written (bit 3 aside).
    pub(super) fn control_byte(&self) -> u8 {
        let bits = [
            (self.enabled, 7),
            (self.priority, 6),
            (self.converts_characters, 5),
            (self.converts_on_read, 4),
            (self.to_bwram, 2),
        ];
        pack(bits, self.source.bits())
    }

    /// SDA (24 bits).
    pub(super) fn source_address(&self) -> u32 {
        self.source_address
    }

    /// DDA (24 bits).
    pub(super) fn destination_address(&self) -> u32 {
        self.destination_address
    }

    /// DTC.
    pub(super) fn count(&self) -> u16 {
        self.count
    }

    /// The registers for a savestate.
    pub(super) fn snapshot(&self) -> Sa1DmaState {
        Sa1DmaState {
            dmaen: self.enabled,
            dprio: self.priority,
            cden: self.converts_characters,
            cdsel: self.converts_on_read,
            dd: self.to_bwram,
            sd: self.source.bits(),
            chdend: self.conversion_ended,
            dmasize: self.width_log2,
            dmacb: self.depth,
            dsa: self.source_address,
            dda: self.destination_address,
            dtc: self.count,
            bbf: self.bitmap_2bpp,
            brf: self.bitmap_registers,
            dma_line: self.row,
            cc1_armed: self.armed,
        }
    }

    /// The registers from a savestate, each held to its range, so a damaged
    /// savestate cannot take a conversion outside its tables.
    pub(super) fn restore(&mut self, state: &Sa1DmaState) {
        self.enabled = state.dmaen;
        self.priority = state.dprio;
        self.converts_characters = state.cden;
        self.converts_on_read = state.cdsel;
        self.to_bwram = state.dd;
        self.source = Source::from_bits(state.sd);
        self.conversion_ended = state.chdend;
        self.width_log2 = state.dmasize.min(5);
        self.depth = state.dmacb.min(2);
        self.source_address = state.dsa & 0xff_ffff;
        self.destination_address = state.dda & 0xff_ffff;
        self.count = state.dtc;
        self.bitmap_2bpp = state.bbf;
        self.bitmap_registers = state.brf;
        self.row = state.dma_line & 15;
        self.armed = state.cc1_armed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bitmap_register_writes(d: &mut Sa1Dma, values: &[(usize, u8)], iram: &mut [u8]) {
        for &(index, value) in values {
            d.write_bitmap_register(index, value, iram);
        }
    }

    #[test]
    fn control_unpacks_every_field_and_clearing_bit_7_resets_the_row() {
        let mut d = Sa1Dma::default();
        d.write_control(0xf7);
        assert_eq!(d.control_byte(), 0xf7);
        assert_eq!(d.source, Source::None);
        let mut iram = [0u8; 2048];
        d.write_control(0xa0); // enabled, type 2
        bitmap_register_writes(&mut d, &[(7, 0)], &mut iram);
        assert_eq!(d.snapshot().dma_line, 1);
        d.write_control(0x00);
        assert_eq!(d.snapshot().dma_line, 0);
    }

    #[test]
    fn depth_and_width_are_held_to_their_ranges() {
        let mut d = Sa1Dma::default();
        d.write_conversion(0xff);
        let s = d.snapshot();
        assert_eq!((s.dmasize, s.dmacb, s.chdend), (5, 2, true));
    }

    #[test]
    fn a_copy_moves_both_addresses_on_and_runs_the_count_out() {
        let mut d = Sa1Dma::default();
        for index in 0..3 {
            d.write_source_address(index, 0xff);
        }
        d.write_count(0, 2);
        let copy = d.take_copy();
        assert_eq!((copy.from, copy.bytes), (0xff_ffff, 2));
        assert_eq!(
            d.source_address(),
            0x00_0001,
            "the address wraps at 24 bits"
        );
        assert_eq!(d.count(), 0xffff);
    }

    #[test]
    fn the_destination_writes_start_what_the_control_asks() {
        let mut d = Sa1Dma::default();
        assert_eq!(d.write_destination_address(1, 0), None, "disabled");
        d.write_control(0x80); // copy to I-RAM
        assert_eq!(d.write_destination_address(1, 0), Some(Start::Copy));
        assert_eq!(d.write_destination_address(2, 0), None);
        d.write_control(0x84); // copy to BW-RAM
        assert_eq!(d.write_destination_address(1, 0), None);
        assert_eq!(d.write_destination_address(2, 0), Some(Start::Copy));
        d.write_control(0xb0); // type-1 conversion
        assert_eq!(
            d.write_destination_address(1, 0),
            Some(Start::ConversionOnRead)
        );
        assert_eq!(d.write_destination_address(0, 0), None);
    }

    #[test]
    fn a_row_of_2bpp_pixels_lands_in_planes_0_and_1() {
        let mut d = Sa1Dma::default();
        d.write_control(0xa0);
        d.write_conversion(0x02); // 2 bits a pixel
        let mut iram = [0u8; 2048];
        bitmap_register_writes(&mut d, &[(0, 0x01), (7, 0)], &mut iram);
        assert_eq!(iram[0], 0x80, "plane 0, first pixel");
        assert_eq!(iram[1], 0x00, "plane 1 empty");
        assert_eq!(d.snapshot().dma_line, 1);
    }

    #[test]
    fn an_8bpp_row_fills_all_eight_planes() {
        let mut d = Sa1Dma::default();
        d.write_control(0xa0);
        let mut iram = [0u8; 2048];
        bitmap_register_writes(&mut d, &[(0, 0xff), (7, 0)], &mut iram);
        for &p in &[0usize, 1, 16, 17, 32, 33, 48, 49] {
            assert_eq!(iram[p], 0x80, "plane at {p}");
        }
    }

    #[test]
    fn a_4bpp_row_fills_four_planes() {
        let mut d = Sa1Dma::default();
        d.write_control(0xa0);
        d.write_conversion(0x01);
        let mut iram = [0u8; 2048];
        bitmap_register_writes(&mut d, &[(0, 0x0f), (7, 0)], &mut iram);
        for &p in &[0usize, 1, 16, 17] {
            assert_eq!(iram[p], 0x80, "plane at {p}");
        }
        assert_eq!(iram[32], 0x00, "no fifth plane at 4 bits");
    }

    #[test]
    fn the_registers_survive_a_savestate() {
        let mut d = Sa1Dma::default();
        d.write_control(0xa5);
        d.write_conversion(0x16);
        d.write_source_address(0, 0x11);
        d.write_source_address(1, 0x22);
        d.write_source_address(2, 0x33);
        d.write_destination_address(0, 0x44);
        d.write_count(0, 0x55);
        d.write_bitmap_format(0x80);
        let mut iram = [0u8; 2048];
        for i in 0..16 {
            d.write_bitmap_register(i, i as u8, &mut iram);
        }
        let snap = d.snapshot();
        let mut restored = Sa1Dma::default();
        restored.restore(&snap);
        assert_eq!(restored.snapshot(), snap);
        assert_eq!(restored.source_address(), 0x33_2211);
    }

    #[test]
    fn a_read_at_a_character_start_converts_it_at_2bpp() {
        let mut d = Sa1Dma::default();
        d.write_conversion(0x02);
        d.arm_conversion();
        let mut bwram = [0u8; 2048];
        bwram[0] = 0x01;
        let mut iram = [0u8; 2048];
        assert_eq!(d.convert_on_read(&bwram, &mut iram, 0), 0x80);
        assert_eq!((iram[0], iram[1]), (0x80, 0x00));
    }

    #[test]
    fn a_read_at_a_character_start_converts_it_at_4bpp() {
        let mut d = Sa1Dma::default();
        d.write_conversion(0x01);
        d.arm_conversion();
        let mut bwram = [0u8; 2048];
        bwram[0] = 0x0f;
        let mut iram = [0u8; 2048];
        d.convert_on_read(&bwram, &mut iram, 0);
        for &p in &[0usize, 1, 16, 17] {
            assert_eq!(iram[p], 0x80, "plane at {p}");
        }
        assert_eq!(iram[32], 0x00);
    }

    #[test]
    fn a_read_inside_a_character_is_answered_from_i_ram() {
        let d = Sa1Dma::default();
        let bwram = [0u8; 2048];
        let mut iram = [0u8; 2048];
        iram[5] = 0x5a;
        assert_eq!(d.convert_on_read(&bwram, &mut iram, 5), 0x5a);
        assert_eq!(iram[5], 0x5a);
    }

    #[test]
    fn cdma_bit_7_ends_an_armed_conversion_and_the_arming_survives_a_savestate() {
        let mut d = Sa1Dma::default();
        assert!(!d.conversion_armed());
        d.arm_conversion();
        let mut restored = Sa1Dma::default();
        restored.restore(&d.snapshot());
        assert!(restored.conversion_armed());
        d.write_conversion(0x80);
        assert!(!d.conversion_armed());
    }

    #[test]
    fn a_copy_within_one_memory_is_recognised() {
        let copy = |source, to_bwram| CopyJob {
            source,
            to_bwram,
            from: 0,
            to: 0,
            bytes: 1,
        };
        assert!(copy(Source::Bwram, true).within_one_memory());
        assert!(copy(Source::Iram, false).within_one_memory());
        assert!(!copy(Source::Rom, false).within_one_memory());
        assert!(!copy(Source::Iram, true).within_one_memory());
    }
}
