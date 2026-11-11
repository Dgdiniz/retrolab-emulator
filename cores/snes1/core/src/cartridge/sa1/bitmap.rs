//! The SA-1 BW-RAM bitmap projection, the SA-1's view of BW-RAM as a stream of
//! 2bpp or 4bpp pixels rather than raw bytes.
//!
//! It is reached two ways on the SA-1 bus: the `$60-6F` banks (always bitmap),
//! and the `$6000-7FFF` 8 KiB window when `$2225` bit 7 (`sw46`) selects it. The
//! pixel depth is `$223F` bit 7 (BBF): clear -> 4bpp (2 pixels a byte), set ->
//! 2bpp (4 pixels a byte), the first pixel in the low bits. Each access reads
//! or writes one pixel; a write keeps the other pixels of its byte. The
//! packing is kept apart from the bus so it can be tested on its own;
//! behaviour checked against bsnes-plus and ares, which agree.

/// Resolve a bitmap pixel address to `(byte_index, shift_bits, pixel_mask)`
/// against a BW-RAM buffer of `len` bytes (`len > 0`). `two_bpp` (= `bbf`):
/// `false` -> 4bpp (2 pixels/byte, nibbles), `true` -> 2bpp (4 pixels/byte).
fn locate(len: usize, pixel_addr: u32, two_bpp: bool) -> (usize, u8, u8) {
    if two_bpp {
        let shift = ((pixel_addr & 3) * 2) as u8;
        let idx = ((pixel_addr >> 2) as usize) % len;
        (idx, shift, 0x03)
    } else {
        let shift = ((pixel_addr & 1) * 4) as u8;
        let idx = ((pixel_addr >> 1) as usize) % len;
        (idx, shift, 0x0F)
    }
}

/// Read one pixel through the bitmap view. With no BW-RAM it reads as `0`.
pub(super) fn bitmap_read(bwram: &[u8], pixel_addr: u32, two_bpp: bool) -> u8 {
    if bwram.is_empty() {
        return 0;
    }
    let (idx, shift, mask) = locate(bwram.len(), pixel_addr, two_bpp);
    (bwram[idx] >> shift) & mask
}

/// Write one pixel through the bitmap view, keeping the other pixels of its
/// byte. Returns whether the byte changed, so the caller marks BW-RAM as
/// needing a save only then. With no BW-RAM the write goes nowhere.
pub(super) fn bitmap_write(bwram: &mut [u8], pixel_addr: u32, pixel: u8, two_bpp: bool) -> bool {
    if bwram.is_empty() {
        return false;
    }
    let (idx, shift, mask) = locate(bwram.len(), pixel_addr, two_bpp);
    let new = (bwram[idx] & !(mask << shift)) | ((pixel & mask) << shift);
    let changed = bwram[idx] != new;
    bwram[idx] = new;
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 4bpp packs two pixels per byte (low nibble = even address, high nibble =
    /// odd) and round-trips each independently.
    #[test]
    fn bitmap_4bpp_packs_two_pixels_per_byte() {
        let mut bwram = [0u8; 8];
        bitmap_write(&mut bwram, 0, 0x0A, false); // low nibble of byte 0
        bitmap_write(&mut bwram, 1, 0x0B, false); // high nibble of byte 0
        assert_eq!(
            bwram[0], 0xBA,
            "high nibble = pixel 1, low nibble = pixel 0"
        );
        assert_eq!(bitmap_read(&bwram, 0, false), 0x0A);
        assert_eq!(bitmap_read(&bwram, 1, false), 0x0B);
        // Next byte is addressed by pixels 2/3.
        bitmap_write(&mut bwram, 2, 0x0C, false);
        assert_eq!(bwram[1], 0x0C);
        // Values are masked to 4 bits.
        bitmap_write(&mut bwram, 3, 0xFF, false);
        assert_eq!(bitmap_read(&bwram, 3, false), 0x0F);
    }

    /// 2bpp packs four pixels per byte, LSB-first, and round-trips each.
    #[test]
    fn bitmap_2bpp_packs_four_pixels_per_byte() {
        let mut bwram = [0u8; 8];
        bitmap_write(&mut bwram, 0, 0b01, true);
        bitmap_write(&mut bwram, 1, 0b10, true);
        bitmap_write(&mut bwram, 2, 0b11, true);
        bitmap_write(&mut bwram, 3, 0b01, true);
        assert_eq!(bwram[0], 0b01_11_10_01, "pixel 3..0 packed high->low");
        for (addr, want) in [(0, 0b01), (1, 0b10), (2, 0b11), (3, 0b01)] {
            assert_eq!(bitmap_read(&bwram, addr, true), want);
        }
        // Pixel 4 rolls onto byte 1.
        bitmap_write(&mut bwram, 4, 0b11, true);
        assert_eq!(bwram[1], 0b11);
        // Values are masked to 2 bits.
        bitmap_write(&mut bwram, 5, 0xFF, true);
        assert_eq!(bitmap_read(&bwram, 5, true), 0b11);
    }

    /// A write only touches its own pixel, the neighbours it shares the byte
    /// with are read-modify-write preserved (both depths). The `changed` return
    /// is like `store_bwram`'s: `true` on a real change, `false` on a no-op.
    #[test]
    fn bitmap_write_preserves_neighbours() {
        let mut bwram = [0u8; 4];
        // 4bpp: seed both nibbles, overwrite just the low one.
        bwram[0] = 0xCD;
        assert!(bitmap_write(&mut bwram, 0, 0x02, false));
        assert_eq!(bwram[0], 0xC2, "high nibble C preserved");
        // Rewriting the same pixel value reports no change.
        assert!(!bitmap_write(&mut bwram, 0, 0x02, false));

        // 2bpp: seed all four (pixel3..0 = 11,10,01,00), overwrite just pixel 2.
        bwram[1] = 0b11_10_01_00;
        assert!(bitmap_write(&mut bwram, (1 << 2) + 2, 0b00, true)); // byte 1, pixel 2
        assert_eq!(bwram[1], 0b11_00_01_00, "pixels 0,1,3 preserved");
    }

    /// Reads/writes wrap the pixel address modulo the buffer, and an empty
    /// buffer is inert.
    #[test]
    fn bitmap_wraps_and_guards_empty() {
        let mut bwram = [0u8; 4]; // 8 bytes of 4bpp pixels
        bitmap_write(&mut bwram, 8, 0x07, false); // pixel 8 -> byte 4 -> wraps to byte 0
        assert_eq!(bwram[0], 0x07);
        assert_eq!(bitmap_read(&bwram, 8, false), 0x07);

        let mut empty: [u8; 0] = [];
        bitmap_write(&mut empty, 0, 0x0F, false);
        assert_eq!(bitmap_read(&empty, 0, false), 0);
        assert_eq!(bitmap_read(&empty, 0, true), 0);
    }
}
