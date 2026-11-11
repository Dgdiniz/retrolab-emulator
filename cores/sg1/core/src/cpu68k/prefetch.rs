//! Prefetch queue for reading opcodes, immediates, and extension words.
//!
//! This separates the architectural PC (which only changes once per instruction)
//! from the read cursor that tracks how many bytes we've consumed during decode.
//!
//! The M68000 has a 2-word prefetch queue in hardware. This implementation
//! simulates that behavior by tracking how many extension words have been read.

use retro_core::cpu::Addressable;

/// Prefetch queue for reading instruction bytes (immediates and extension words).
///
/// The prefetch starts at PC+2 (after the opcode) and tracks how many bytes
/// have been read. This allows us to know exactly where we are during decode,
/// which is critical for Address Error exception handling.
#[derive(Debug)]
pub struct Prefetch {
    /// Base address (normally start_pc + 2, after opcode)
    base: u32,
    /// Bytes consumed from base
    cursor: u32,
    /// Extra bytes read from the prefetch queue, like immediates in some instructions.
    extra_bytes_read: u32,
}

impl Prefetch {
    /// Create a new prefetch queue starting after the opcode at start_pc.
    #[inline]
    pub fn new(start_pc: u32) -> Self {
        Self {
            base: start_pc.wrapping_add(2),
            cursor: 0,
            extra_bytes_read: 0,
        }
    }

    #[inline]
    pub fn inc_cursor(&mut self, bytes: u32) {
        self.cursor = self.cursor.wrapping_add(bytes);
    }

    /// Get the current address in the prefetch queue.
    #[inline]
    pub fn addr(&self) -> u32 {
        self.base.wrapping_add(self.cursor)
    }

    /// Read a word (16-bit) from the prefetch queue and advance cursor.
    #[inline]
    pub fn read_word(&mut self, bus: &mut dyn Addressable) -> u16 {
        let w = bus.read_word(self.addr());
        self.cursor = self.cursor.wrapping_add(2);
        w
    }

    /// Read a long (32-bit) from the prefetch queue and advance cursor.
    #[inline]
    pub fn read_long(&mut self, bus: &mut dyn Addressable) -> u32 {
        let hi = self.read_word(bus) as u32;
        let lo = self.read_word(bus) as u32;
        (hi << 16) | lo
    }

    /// Get the number of bytes consumed from the prefetch queue.
    #[inline]
    pub fn consumed(&self) -> u32 {
        self.cursor
    }

    /// Add extra bytes read to the prefetch queue.
    #[inline]
    pub fn add_extra_bytes_read(&mut self, bytes: u32) {
        self.extra_bytes_read = self.extra_bytes_read.wrapping_add(bytes);
    }

    /// Get the PC value to save in an exception stack frame.
    ///
    /// As Tom Harte's vectors expect it:
    /// - cursor == 0: PC = instruction_pc (base - 2)
    /// - cursor > 0:  PC = instruction_pc + cursor (base + cursor - 2)
    ///
    /// Examples:
    /// - cursor = 0: base - 2 = instruction_pc
    /// - cursor = 2: base + 0 = instruction_pc + 2
    /// - cursor = 4: base + 2 = instruction_pc + 4
    /// - cursor = 6: base + 4 = instruction_pc + 6
    ///
    /// So the PC is the address of the last word read: the opcode's own when no
    /// extension word was read, else the last extension word's.
    #[inline]
    pub fn pc_for_stack(&self) -> u32 {
        if self.cursor == 0 {
            self.base.wrapping_sub(2)
        } else {
            self.base.wrapping_add(self.cursor.wrapping_sub(2))
        }
    }

    /// Get the PC value for write-only operations (like MOVE destination).
    ///
    /// Uses the same formula as `pc_for_stack`, which Tom Harte's vectors expect for
    /// write-only operations too.
    #[inline]
    pub fn pc_for_stack_write_only(&self) -> u32 {
        if self.cursor == 0 {
            self.base.wrapping_sub(2)
        } else {
            self.base.wrapping_add(self.cursor.wrapping_sub(2))
        }
    }

    /// Get the base address: instruction_pc + 2, plus the bytes recorded with
    /// `add_extra_bytes_read`.
    /// This is used for PC-relative addressing modes.
    #[inline]
    pub fn base(&self) -> u32 {
        self.base + self.extra_bytes_read
    }
}
