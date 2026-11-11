//! The VRAM port: `$2115` VMAIN, `$2116/$2117` VMADD, `$2118/$2119`
//! VMDATA and `$2139/$213A` VMDATAREAD.
//!
//! VRAM is 32 K words, and the CPU reaches it one byte at a time through
//! a word address and a pair of data registers, one for each half of the
//! word. VMAIN says which half's access moves the address on (bit 7: the
//! high half; otherwise the low), by how much (bits 0-1: 1, 32 or 128
//! words), and whether the address is translated first (bits 2-3): the
//! three translations rotate the low 8, 9 or 10 bits of the word address
//! left by three, which turns a run of consecutive addresses into the
//! rows of a tile's bitplanes, so a bitmap can be written row by row.
//!
//! Reads go through a one-word buffer: setting the address fills it, a
//! read returns the buffer's half and, on the half that moves the
//! address, refills the buffer from the address before moving it. So
//! the first read after setting the address returns the word at that
//! address, and the buffer stays one word behind the address from then
//! on.
//!
//! While the picture is being drawn the port is closed: writes are lost
//! and reads give 0 (not the bus's last value). Forced blank and the
//! vertical blank open it.
//!
//! Written from fullsnes's VRAM port description; checked against
//! bsnes-plus.

use super::{Ppu, PpuAccessCtx};

/// Words the address moves by for each VMAIN step code.
const ADDRESS_STEPS: [u16; 4] = [1, 32, 128, 128];

/// The word address after VMAIN's translation: the low `bits` bits
/// rotated left by three, the rest left alone.
#[inline]
fn rotate_low_bits(address: u16, bits: u32) -> u16 {
    let mask = (1u16 << bits) - 1;
    let low = address & mask;
    let rotated = ((low << 3) | (low >> (bits - 3))) & mask;
    (address & !mask) | rotated
}

impl Ppu {
    /// The word the data registers address, after VMAIN's translation.
    pub(super) fn vram_word_address(&self) -> u16 {
        match self.vram_mapping & 3 {
            0 => self.vram_addr,
            1 => rotate_low_bits(self.vram_addr, 8),
            2 => rotate_low_bits(self.vram_addr, 9),
            _ => rotate_low_bits(self.vram_addr, 10),
        }
    }

    /// The same as a byte address into the 64 KiB VRAM.
    pub(super) fn get_vram_byte_address(&self) -> u32 {
        u32::from(self.vram_word_address()) << 1
    }

    /// Whether the CPU can reach VRAM now: during forced blank, or
    /// outside the lines being drawn.
    fn vram_open(&self, line: u16) -> bool {
        self.display_disable || !self.in_active_scanline(line)
    }

    /// One half of a word, through the gate: 0 while the port is closed.
    pub(super) fn vram_read_byte(&self, byte_address: u32, line: u16) -> u8 {
        if !self.vram_open(line) {
            return 0;
        }
        let [low, high] = self.vram[(byte_address as usize >> 1) & 0x7fff].to_le_bytes();
        if byte_address & 1 == 0 {
            low
        } else {
            high
        }
    }

    /// One half of a word written, through the gate: lost while the port
    /// is closed. A write that lands moves the renderer's VRAM generation
    /// on, which is what tells its tilemap-row cache to rebuild.
    pub(super) fn vram_write_byte(&mut self, byte_address: u32, value: u8, line: u16) {
        if !self.vram_open(line) {
            return;
        }
        let word = &mut self.vram[(byte_address as usize >> 1) & 0x7fff];
        let [mut low, mut high] = word.to_le_bytes();
        if byte_address & 1 == 0 {
            low = value;
        } else {
            high = value;
        }
        *word = u16::from_le_bytes([low, high]);
        self.vram_generation = self.vram_generation.wrapping_add(1);
    }

    /// Fill the read buffer from the word the address names.
    fn prefetch(&mut self, line: u16) {
        let at = self.get_vram_byte_address();
        self.vram_readbuffer = u16::from_le_bytes([
            self.vram_read_byte(at, line),
            self.vram_read_byte(at + 1, line),
        ]);
    }

    /// Whether an access to this half of the word moves the address.
    #[inline]
    fn moves_address(&self, high: bool) -> bool {
        high == self.vram_incmode
    }

    /// Move the address on by the step.
    #[inline]
    fn step_address(&mut self) {
        self.vram_addr = self.vram_addr.wrapping_add(self.vram_incsize);
    }

    /// `$2115` VMAIN.
    pub(super) fn write_2115(&mut self, value: u8) {
        self.vmain_byte = value;
        self.vram_incmode = value & 0x80 != 0;
        self.vram_mapping = (value >> 2) & 3;
        self.vram_incsize = ADDRESS_STEPS[usize::from(value & 3)];
    }

    /// `$2116` VMADDL: the address's low byte, and the buffer filled.
    pub(super) fn write_2116(&mut self, value: u8, ctx: &PpuAccessCtx) {
        self.vram_addr = u16::from_le_bytes([value, self.vram_addr.to_le_bytes()[1]]);
        self.prefetch(ctx.v_scanline);
    }

    /// `$2117` VMADDH: the address's high byte, and the buffer filled.
    pub(super) fn write_2117(&mut self, value: u8, ctx: &PpuAccessCtx) {
        self.vram_addr = u16::from_le_bytes([self.vram_addr.to_le_bytes()[0], value]);
        self.prefetch(ctx.v_scanline);
    }

    /// A write to one half of the addressed word (`$2118` low, `$2119`
    /// high), then the step when this is the half that moves the
    /// address.
    fn vram_data_write(&mut self, high: bool, value: u8, ctx: &PpuAccessCtx) {
        let at = self.get_vram_byte_address() + u32::from(high);
        self.vram_write_byte(at, value, ctx.v_scanline);
        if self.moves_address(high) {
            self.step_address();
        }
    }

    /// A read of one half of the buffered word (`$2139` low, `$213A`
    /// high); on the half that moves the address, the buffer is refilled
    /// from the address first and the address stepped after.
    fn vram_data_read(&mut self, high: bool, ctx: &PpuAccessCtx) -> u8 {
        let [low_byte, high_byte] = self.vram_readbuffer.to_le_bytes();
        let value = if high { high_byte } else { low_byte };
        self.ppu1_mdr = value;
        if self.moves_address(high) {
            self.prefetch(ctx.v_scanline);
            self.step_address();
        }
        value
    }

    /// `$2118` VMDATAL.
    pub(super) fn write_2118(&mut self, value: u8, ctx: &PpuAccessCtx) {
        self.vram_data_write(false, value, ctx);
    }

    /// `$2119` VMDATAH.
    pub(super) fn write_2119(&mut self, value: u8, ctx: &PpuAccessCtx) {
        self.vram_data_write(true, value, ctx);
    }

    /// `$2139` VMDATALREAD.
    pub(super) fn read_2139(&mut self, ctx: &PpuAccessCtx) -> u8 {
        self.vram_data_read(false, ctx)
    }

    /// `$213A` VMDATAHREAD.
    pub(super) fn read_213a(&mut self, ctx: &PpuAccessCtx) -> u8 {
        self.vram_data_read(true, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A context for a chip in forced blank (the reset state), where the
    /// port is open whatever the line.
    fn open_ctx() -> PpuAccessCtx {
        PpuAccessCtx::ZERO
    }

    #[test]
    fn the_translations_rotate_the_low_bits_left_by_three() {
        // Translation 1 on the eight low bits: 0b00100011 -> 0b00011001.
        assert_eq!(rotate_low_bits(0x0023, 8), 0x0019);
        assert_eq!(rotate_low_bits(0x1f23, 8), 0x1f19);
        // Translation 2 on nine bits, 3 on ten.
        assert_eq!(rotate_low_bits(0x0043, 9), 0x0019);
        assert_eq!(rotate_low_bits(0x0083, 10), 0x0019);
        // The whole address space, against the translations written as
        // fullsnes prints them (the field moved up, the top bits down).
        for address in 0..=0xffffu16 {
            let a = u32::from(address);
            let t1 = (a & 0xff00) | ((a & 0x1f) << 3) | ((a >> 5) & 7);
            let t2 = (a & 0xfe00) | ((a & 0x3f) << 3) | ((a >> 6) & 7);
            let t3 = (a & 0xfc00) | ((a & 0x7f) << 3) | ((a >> 7) & 7);
            assert_eq!(u32::from(rotate_low_bits(address, 8)), t1, "{address:#06x}");
            assert_eq!(u32::from(rotate_low_bits(address, 9)), t2, "{address:#06x}");
            assert_eq!(
                u32::from(rotate_low_bits(address, 10)),
                t3,
                "{address:#06x}"
            );
        }
    }

    #[test]
    fn vmain_sets_the_step_the_translation_and_the_moving_half() {
        let mut ppu = Ppu::new();
        for (value, step) in [(0x00, 1), (0x01, 32), (0x02, 128), (0x03, 128)] {
            ppu.write_2115(value);
            assert_eq!(ppu.vram_incsize, step);
        }
        assert!(!ppu.vram_incmode);
        assert_eq!(ppu.vram_mapping, 0);
        ppu.write_2115(0x8c);
        assert!(ppu.vram_incmode);
        assert_eq!(ppu.vram_mapping, 3);
        assert_eq!(ppu.vram_incsize, 1);
        assert_eq!(ppu.vmain_byte, 0x8c);
    }

    #[test]
    fn a_word_written_through_the_port_reads_back_through_it() {
        let mut ppu = Ppu::new();
        let ctx = open_ctx();
        ppu.write_2115(0x80); // the high half moves the address, by one
        ppu.write_2116(0x00, &ctx);
        ppu.write_2117(0x10, &ctx);
        ppu.write_2118(0x34, &ctx);
        ppu.write_2119(0x12, &ctx);
        assert_eq!(ppu.vram_addr, 0x1001);
        assert_eq!(ppu.vram[0x1000], 0x1234);

        ppu.write_2116(0x00, &ctx);
        ppu.write_2117(0x10, &ctx);
        assert_eq!(ppu.read_2139(&ctx), 0x34);
        assert_eq!(ppu.read_213a(&ctx), 0x12);
        assert_eq!(ppu.ppu1_mdr, 0x12);
    }

    #[test]
    fn only_the_chosen_half_moves_the_address() {
        let mut ppu = Ppu::new();
        let ctx = open_ctx();
        ppu.write_2115(0x00); // the low half moves it
        ppu.write_2116(0x00, &ctx);
        ppu.write_2117(0x00, &ctx);
        ppu.write_2119(0xaa, &ctx);
        assert_eq!(ppu.vram_addr, 0x0000);
        ppu.write_2118(0x55, &ctx);
        assert_eq!(ppu.vram_addr, 0x0001);

        ppu.write_2115(0x80); // the high half
        ppu.write_2116(0x10, &ctx);
        ppu.write_2117(0x00, &ctx);
        ppu.write_2118(0x11, &ctx);
        assert_eq!(ppu.vram_addr, 0x0010);
        ppu.write_2119(0x22, &ctx);
        assert_eq!(ppu.vram_addr, 0x0011);
    }

    #[test]
    fn the_read_buffer_stays_one_word_behind_the_address() {
        let mut ppu = Ppu::new();
        let ctx = open_ctx();
        ppu.vram[0x0100] = 0x1111;
        ppu.vram[0x0101] = 0x2222;
        ppu.vram[0x0102] = 0x3333;
        ppu.write_2115(0x00);
        ppu.write_2116(0x00, &ctx);
        ppu.write_2117(0x01, &ctx);
        // Setting the address filled the buffer from $0100. The low read
        // returns it, refills from $0100 again (the address has not
        // moved yet) and only then steps.
        assert_eq!(ppu.read_2139(&ctx), 0x11);
        assert_eq!(ppu.vram_addr, 0x0101);
        // The high half neither refills nor steps.
        assert_eq!(ppu.read_213a(&ctx), 0x11);
        assert_eq!(ppu.vram_addr, 0x0101);
        // From here each low read returns the word before the address.
        assert_eq!(ppu.read_2139(&ctx), 0x11);
        assert_eq!(ppu.vram_addr, 0x0102);
        assert_eq!(ppu.read_2139(&ctx), 0x22);
        assert_eq!(ppu.vram_addr, 0x0103);
    }

    #[test]
    fn the_translations_place_a_run_of_writes_in_rotated_slots() {
        let ctx = open_ctx();
        for (vmain, address, slot) in [
            (0x84, 0x0023, 0x0019),
            (0x88, 0x0043, 0x0019),
            (0x8c, 0x0083, 0x0019),
        ] {
            let mut ppu = Ppu::new();
            ppu.write_2115(vmain);
            ppu.write_2116(address as u8, &ctx);
            ppu.write_2117((address >> 8) as u8, &ctx);
            ppu.write_2118(0xab, &ctx);
            ppu.write_2119(0xcd, &ctx);
            assert_eq!(ppu.vram[slot], 0xcdab, "VMAIN {vmain:#04x}");
        }
    }

    #[test]
    fn the_port_is_closed_while_the_picture_is_drawn() {
        let mut ppu = Ppu::new();
        ppu.display_disable = false;
        let drawing = PpuAccessCtx {
            mdr: 0xff,
            h_dot: 0,
            v_scanline: 50,
        };
        ppu.write_2115(0x80);
        ppu.write_2116(0x00, &drawing);
        ppu.write_2117(0x00, &drawing);
        ppu.write_2118(0xaa, &drawing);
        ppu.write_2119(0x55, &drawing);
        assert_eq!(ppu.vram[0x0000], 0x0000, "the write was lost");
        assert_eq!(ppu.vram_addr, 0x0001, "but the address moved");
        ppu.write_2116(0x00, &drawing);
        ppu.write_2117(0x00, &drawing);
        assert_eq!(
            ppu.read_2139(&drawing),
            0,
            "a read gives 0, not the bus value"
        );

        let blanking = PpuAccessCtx {
            mdr: 0,
            h_dot: 0,
            v_scanline: 226,
        };
        ppu.write_2116(0x00, &blanking);
        ppu.write_2117(0x00, &blanking);
        ppu.write_2118(0x11, &blanking);
        ppu.write_2119(0x22, &blanking);
        assert_eq!(ppu.vram[0x0000], 0x2211);
        // With overscan the picture runs to line 239.
        ppu.overscan = true;
        let line_230 = PpuAccessCtx {
            mdr: 0,
            h_dot: 0,
            v_scanline: 230,
        };
        ppu.write_2116(0x02, &line_230);
        ppu.write_2117(0x00, &line_230);
        ppu.write_2118(0x33, &line_230);
        ppu.write_2119(0x44, &line_230);
        assert_eq!(ppu.vram[0x0002], 0x0000);
    }

    #[test]
    fn the_address_wraps_at_64k_words() {
        let mut ppu = Ppu::new();
        let ctx = open_ctx();
        ppu.write_2115(0x80);
        ppu.vram_addr = 0xfffe;
        ppu.write_2118(0xaa, &ctx);
        ppu.write_2119(0xbb, &ctx);
        assert_eq!(ppu.vram_addr, 0xffff);
        assert_eq!(
            ppu.vram[0x7ffe], 0xbbaa,
            "the top bit of the address is ignored"
        );
        ppu.write_2118(0xcc, &ctx);
        ppu.write_2119(0xdd, &ctx);
        assert_eq!(ppu.vram_addr, 0x0000);
    }
}
