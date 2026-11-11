//! Per-CPU code-coverage bitmap.
//!
//! The disassembly panel needs to know which addresses are *actual*
//! instruction starts (vs. raw data the linear disassembler would
//! gladly decode into bogus rows). We track that incrementally by
//! setting a single bit for every address the CPU executes from. The
//! bitmap is sized to cover the CPU's full address space (a 24-bit
//! bus -> 16 MiB packed into 2 MiB of bits, a 16-bit bus -> 64 KiB
//! packed into 8 KiB of bits) so any address lookup is O(1).
//!
//! The bitmap is in-memory only, no persistence, no serialization.
//! Lifecycle is owned by the host (typical pattern: clear when a
//! different ROM is loaded, preserve across `reset()` and same-ROM
//! hot-reload). Recording cost is one
//! `bits[addr/8] |= 1 << (addr & 7)` per executed instruction
//! (~2 ns), negligible compared to a decoder step.

/// A packed bitmap over a contiguous address range starting at 0.
/// Each byte holds 8 bits; bit `i` of byte `n` corresponds to address
/// `n * 8 + i`.
#[derive(Debug, Clone)]
pub struct CoverageBitmap {
    bits: Vec<u8>,
    address_space_size: u32,
}

impl CoverageBitmap {
    /// Create a bitmap covering `[0, address_space_size)`. Bits are
    /// allocated lazily-init'd to zero. Allocates
    /// `ceil(address_space_size / 8)` bytes.
    pub fn new(address_space_size: u32) -> Self {
        let byte_len = (address_space_size as usize).div_ceil(8);
        Self {
            bits: vec![0u8; byte_len],
            address_space_size,
        }
    }

    /// Address-space size this bitmap was sized for. Addresses at or
    /// above this value are silently ignored by `set` / treated as
    /// uncovered by `get`.
    pub fn size(&self) -> u32 {
        self.address_space_size
    }

    /// Mark `addr` as covered. Addresses outside the bitmap's range are
    /// silently dropped, callers handle wrap-around / mirroring before
    /// recording.
    #[inline]
    pub fn set(&mut self, addr: u32) {
        if addr >= self.address_space_size {
            return;
        }
        let byte_idx = (addr >> 3) as usize;
        let bit_idx = (addr & 7) as u8;
        self.bits[byte_idx] |= 1u8 << bit_idx;
    }

    /// True iff `addr` was previously `set`. Out-of-range addresses
    /// return `false`.
    #[inline]
    pub fn get(&self, addr: u32) -> bool {
        if addr >= self.address_space_size {
            return false;
        }
        let byte_idx = (addr >> 3) as usize;
        let bit_idx = (addr & 7) as u8;
        (self.bits[byte_idx] & (1u8 << bit_idx)) != 0
    }

    /// Reset every bit to zero. Allocation is preserved.
    pub fn clear(&mut self) {
        for byte in self.bits.iter_mut() {
            *byte = 0;
        }
    }

    /// Number of set bits. O(n_bytes), uses native popcount.
    pub fn count(&self) -> u32 {
        let mut total: u32 = 0;
        for &byte in &self.bits {
            total += byte.count_ones();
        }
        total
    }

    /// Largest set address `<= addr`, or `None` if no bit at or below
    /// `addr` is set. Used by `disassembleWindow` to walk backward from
    /// PC to find the most recent real instruction start.
    pub fn nearest_set_at_or_before(&self, addr: u32) -> Option<u32> {
        if self.address_space_size == 0 {
            return None;
        }
        let mut a = addr.min(self.address_space_size.saturating_sub(1));
        let mut byte_idx = (a >> 3) as usize;
        let mut bit_idx = (a & 7) as i32;

        // Walk the current byte from bit_idx down to 0.
        let byte = self.bits[byte_idx];
        while bit_idx >= 0 {
            if (byte & (1u8 << bit_idx)) != 0 {
                return Some(a);
            }
            if a == 0 {
                return None;
            }
            a -= 1;
            bit_idx -= 1;
        }

        // Then walk preceding bytes (whole-byte step at a time, then
        // bit-scan inside the first non-zero byte).
        if byte_idx == 0 {
            return None;
        }
        byte_idx -= 1;
        loop {
            let b = self.bits[byte_idx];
            if b != 0 {
                // Highest set bit of b
                for bi in (0..8u8).rev() {
                    if (b & (1u8 << bi)) != 0 {
                        return Some(((byte_idx as u32) << 3) | bi as u32);
                    }
                }
            }
            if byte_idx == 0 {
                return None;
            }
            byte_idx -= 1;
        }
    }

    /// Smallest set address `>= addr`, or `None` if no bit at or above
    /// `addr` is set. Used by `disassembleWindow` to skip over gaps
    /// when scanning forward past unknown data regions.
    pub fn nearest_set_at_or_after(&self, addr: u32) -> Option<u32> {
        if addr >= self.address_space_size {
            return None;
        }
        let start_byte_idx = (addr >> 3) as usize;
        let start_bit_idx = (addr & 7) as u8;
        let max_addr = self.address_space_size.saturating_sub(1);

        // Walk the current byte from start_bit_idx up to 7.
        let byte = self.bits[start_byte_idx];
        for bi in start_bit_idx..8 {
            let cand = ((start_byte_idx as u32) << 3) | bi as u32;
            if cand > max_addr {
                return None;
            }
            if (byte & (1u8 << bi)) != 0 {
                return Some(cand);
            }
        }

        // Then walk forward through full bytes; bit-scan the first
        // non-zero one.
        for byte_idx in (start_byte_idx + 1)..self.bits.len() {
            let b = self.bits[byte_idx];
            if b == 0 {
                continue;
            }
            for bi in 0u8..8 {
                let cand = ((byte_idx as u32) << 3) | bi as u32;
                if cand > max_addr {
                    return None;
                }
                if (b & (1u8 << bi)) != 0 {
                    return Some(cand);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_starts_empty() {
        let b = CoverageBitmap::new(256);
        assert_eq!(b.count(), 0);
        assert!(!b.get(0));
        assert!(!b.get(255));
    }

    #[test]
    fn set_and_get_round_trip() {
        let mut b = CoverageBitmap::new(256);
        b.set(0);
        b.set(7);
        b.set(8);
        b.set(255);
        assert!(b.get(0));
        assert!(b.get(7));
        assert!(b.get(8));
        assert!(b.get(255));
        assert!(!b.get(1));
        assert!(!b.get(9));
        assert_eq!(b.count(), 4);
    }

    #[test]
    fn out_of_range_set_is_silently_dropped() {
        let mut b = CoverageBitmap::new(256);
        b.set(256);
        b.set(0xFFFF_FFFF);
        assert_eq!(b.count(), 0);
        assert!(!b.get(256));
        assert!(!b.get(0xFFFF_FFFF));
    }

    #[test]
    fn set_is_idempotent() {
        let mut b = CoverageBitmap::new(256);
        b.set(42);
        b.set(42);
        b.set(42);
        assert_eq!(b.count(), 1);
    }

    #[test]
    fn clear_resets_all_bits_but_keeps_size() {
        let mut b = CoverageBitmap::new(256);
        b.set(0);
        b.set(100);
        b.clear();
        assert_eq!(b.count(), 0);
        assert_eq!(b.size(), 256);
        assert!(!b.get(0));
        assert!(!b.get(100));
    }

    #[test]
    fn count_uses_popcount_across_bytes() {
        let mut b = CoverageBitmap::new(64);
        for a in 0..64u32 {
            b.set(a);
        }
        assert_eq!(b.count(), 64);
    }

    #[test]
    fn nearest_set_at_or_before_empty_bitmap_returns_none() {
        let b = CoverageBitmap::new(256);
        assert_eq!(b.nearest_set_at_or_before(0), None);
        assert_eq!(b.nearest_set_at_or_before(100), None);
        assert_eq!(b.nearest_set_at_or_before(255), None);
    }

    #[test]
    fn nearest_set_at_or_before_finds_exact_match() {
        let mut b = CoverageBitmap::new(256);
        b.set(100);
        assert_eq!(b.nearest_set_at_or_before(100), Some(100));
    }

    #[test]
    fn nearest_set_at_or_before_walks_backward_through_current_byte() {
        let mut b = CoverageBitmap::new(64);
        b.set(8); // bit 0 of byte 1
        b.set(13); // bit 5 of byte 1
        assert_eq!(b.nearest_set_at_or_before(15), Some(13));
        assert_eq!(b.nearest_set_at_or_before(13), Some(13));
        assert_eq!(b.nearest_set_at_or_before(12), Some(8));
        assert_eq!(b.nearest_set_at_or_before(8), Some(8));
        assert_eq!(b.nearest_set_at_or_before(7), None);
    }

    #[test]
    fn nearest_set_at_or_before_walks_across_byte_boundaries() {
        let mut b = CoverageBitmap::new(256);
        b.set(15); // last bit of byte 1
        b.set(64); // first bit of byte 8
        assert_eq!(b.nearest_set_at_or_before(100), Some(64));
        assert_eq!(b.nearest_set_at_or_before(64), Some(64));
        assert_eq!(b.nearest_set_at_or_before(63), Some(15));
        assert_eq!(b.nearest_set_at_or_before(16), Some(15));
        assert_eq!(b.nearest_set_at_or_before(15), Some(15));
        assert_eq!(b.nearest_set_at_or_before(14), None);
    }

    #[test]
    fn nearest_set_at_or_before_clamps_above_max() {
        let mut b = CoverageBitmap::new(256);
        b.set(255);
        assert_eq!(b.nearest_set_at_or_before(0xFFFF), Some(255));
    }

    #[test]
    fn nearest_set_at_or_after_empty_bitmap_returns_none() {
        let b = CoverageBitmap::new(256);
        assert_eq!(b.nearest_set_at_or_after(0), None);
        assert_eq!(b.nearest_set_at_or_after(100), None);
    }

    #[test]
    fn nearest_set_at_or_after_finds_exact_match() {
        let mut b = CoverageBitmap::new(256);
        b.set(100);
        assert_eq!(b.nearest_set_at_or_after(100), Some(100));
    }

    #[test]
    fn nearest_set_at_or_after_walks_forward_through_current_byte() {
        let mut b = CoverageBitmap::new(64);
        b.set(13);
        b.set(15);
        assert_eq!(b.nearest_set_at_or_after(8), Some(13));
        assert_eq!(b.nearest_set_at_or_after(13), Some(13));
        assert_eq!(b.nearest_set_at_or_after(14), Some(15));
        assert_eq!(b.nearest_set_at_or_after(15), Some(15));
        assert_eq!(b.nearest_set_at_or_after(16), None);
    }

    #[test]
    fn nearest_set_at_or_after_walks_across_byte_boundaries() {
        let mut b = CoverageBitmap::new(256);
        b.set(15);
        b.set(64);
        assert_eq!(b.nearest_set_at_or_after(0), Some(15));
        assert_eq!(b.nearest_set_at_or_after(16), Some(64));
        assert_eq!(b.nearest_set_at_or_after(63), Some(64));
        assert_eq!(b.nearest_set_at_or_after(65), None);
    }

    #[test]
    fn nearest_set_at_or_after_above_max_returns_none() {
        let mut b = CoverageBitmap::new(256);
        b.set(255);
        assert_eq!(b.nearest_set_at_or_after(256), None);
        assert_eq!(b.nearest_set_at_or_after(0xFFFF), None);
    }

    #[test]
    fn nearest_set_handles_24_bit_address_space() {
        // 16 MiB address space, exercises larger bitmaps to make
        // sure the byte/bit walks scale.
        let mut b = CoverageBitmap::new(16 * 1024 * 1024);
        b.set(0x000200);
        b.set(0x005000);
        b.set(0xFF0000);
        assert_eq!(b.nearest_set_at_or_before(0xFFFFFF), Some(0xFF0000));
        assert_eq!(b.nearest_set_at_or_before(0x100000), Some(0x005000));
        assert_eq!(b.nearest_set_at_or_before(0x004FFF), Some(0x000200));
        assert_eq!(b.nearest_set_at_or_before(0x000100), None);
        assert_eq!(b.nearest_set_at_or_after(0x000000), Some(0x000200));
        assert_eq!(b.nearest_set_at_or_after(0x000201), Some(0x005000));
        assert_eq!(b.nearest_set_at_or_after(0xFF0001), None);
    }
}
