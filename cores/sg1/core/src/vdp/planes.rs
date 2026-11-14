//! The nametable entry of the VDP's planes.
//!
//! Plane A, plane B and the window are each a table of 16-bit entries in
//! VRAM, one per cell, naming the cell's tile, its flips, its palette line
//! and its priority. The renderer (`Vdp::composite_scanline`) reads the
//! bits straight from the word as it draws; this is the same decode, for
//! code that wants the fields by name.

/// Nametable entry (2 bytes in VRAM).
///
/// Format (big-endian):
/// ```text
/// Bit 15: Priority
/// Bit 14-13: Palette (0-3)
/// Bit 12: Vertical flip
/// Bit 11: Horizontal flip
/// Bit 10-0: Pattern index (0-2047)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NametableEntry {
    pub pattern_index: u16, // bits 0-10 (2048 patterns)
    pub flip_h: bool,       // bit 11
    pub flip_v: bool,       // bit 12
    pub palette: u8,        // bits 13-14 (4 palettes)
    pub priority: bool,     // bit 15
}

impl NametableEntry {
    /// Decode from 16-bit VRAM word.
    pub fn from_word(word: u16) -> Self {
        Self {
            pattern_index: word & 0x07FF,
            flip_h: (word & 0x0800) != 0,
            flip_v: (word & 0x1000) != 0,
            palette: ((word >> 13) & 0x03) as u8,
            priority: (word & 0x8000) != 0,
        }
    }

    /// Encode to 16-bit VRAM word.
    pub fn to_word(&self) -> u16 {
        self.pattern_index
            | if self.flip_h { 0x0800 } else { 0 }
            | if self.flip_v { 0x1000 } else { 0 }
            | ((self.palette as u16) << 13)
            | if self.priority { 0x8000 } else { 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nametable_entry_decode() {
        // Pattern 0x123, no flips, palette 0, no priority
        let word = 0x0123;
        let entry = NametableEntry::from_word(word);

        assert_eq!(entry.pattern_index, 0x123);
        assert!(!entry.flip_h);
        assert!(!entry.flip_v);
        assert_eq!(entry.palette, 0);
        assert!(!entry.priority);
    }

    #[test]
    fn test_nametable_entry_priority() {
        let word = 0x8000; // Priority bit set
        let entry = NametableEntry::from_word(word);
        assert!(entry.priority);
    }

    #[test]
    fn test_nametable_entry_palette() {
        // Palette 3 (bits 13-14)
        let word = 0x6000;
        let entry = NametableEntry::from_word(word);
        assert_eq!(entry.palette, 3);
    }

    #[test]
    fn test_nametable_entry_flip_v() {
        let word = 0x1000; // V-flip bit set
        let entry = NametableEntry::from_word(word);
        assert!(entry.flip_v);
        assert!(!entry.flip_h);
    }

    #[test]
    fn test_nametable_entry_flip_h() {
        let word = 0x0800; // H-flip bit set
        let entry = NametableEntry::from_word(word);
        assert!(entry.flip_h);
        assert!(!entry.flip_v);
    }

    #[test]
    fn test_nametable_entry_all_flags() {
        // Pattern 0x7FF (max 11-bit value), both flips, palette 2, priority
        let word = 0xDFFF; // 1101 1111 1111 1111
        let entry = NametableEntry::from_word(word);

        assert_eq!(entry.pattern_index, 0x7FF); // 2047 (max pattern index)
        assert!(entry.flip_h);
        assert!(entry.flip_v);
        assert_eq!(entry.palette, 2);
        assert!(entry.priority);
    }

    #[test]
    fn test_nametable_entry_encode() {
        let entry = NametableEntry {
            pattern_index: 0x123,
            flip_h: true,
            flip_v: false,
            palette: 1,
            priority: true,
        };

        let word = entry.to_word();
        assert_eq!(word, 0xA923); // 1010 1001 0010 0011
    }

    #[test]
    fn test_nametable_entry_roundtrip() {
        let original = NametableEntry {
            pattern_index: 0x456,
            flip_h: true,
            flip_v: true,
            palette: 3,
            priority: false,
        };

        let word = original.to_word();
        let decoded = NametableEntry::from_word(word);

        assert_eq!(decoded, original);
    }
}
