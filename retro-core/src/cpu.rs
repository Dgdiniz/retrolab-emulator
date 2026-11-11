//! CPU-related traits and types.

/// Trait for components that can be clocked.
pub trait Clockable {
    /// Execute for a number of cycles.
    /// Returns the actual number of cycles consumed.
    fn clock(&mut self, cycles: u32) -> u32;
}

/// Trait for components with addressable memory.
pub trait Addressable {
    /// Read a byte from an address.
    fn read(&mut self, addr: u32) -> u8;

    /// Write a byte to an address.
    fn write(&mut self, addr: u32, value: u8);

    /// Read a 16-bit word (endianness depends on system).
    fn read_word(&mut self, addr: u32) -> u16 {
        let low = self.read(addr) as u16;
        let high = self.read(addr + 1) as u16;
        (high << 8) | low
    }

    /// Write a 16-bit word (endianness depends on system).
    fn write_word(&mut self, addr: u32, value: u16) {
        self.write(addr, (value & 0xFF) as u8);
        self.write(addr + 1, ((value >> 8) & 0xFF) as u8);
    }

    fn cycles(&self) -> Option<u64> {
        None
    }

    /// Read from I/O port (Z80 has separate I/O address space).
    /// The full 16-bit address bus value is provided (upper 8 bits may contain
    /// register values depending on instruction: A for IN A,(n), B for IN r,(C)).
    /// Default returns 0xFF (floating bus).
    fn io_read(&mut self, _port: u16) -> u8 {
        0xFF
    }

    /// Write to I/O port (Z80 has separate I/O address space).
    /// The full 16-bit address bus value is provided.
    /// Default is no-op.
    fn io_write(&mut self, _port: u16, _value: u8) {
        // No-op
    }
}

// Trait for components that can track cycles executed.
pub trait CyclesTrackable {
    /// Get the number of cycles executed.
    fn cycles_executed(&self) -> u64;
}

/// Trait for components that can be reset.
pub trait Resettable {
    /// Reset to initial state.
    fn reset(&mut self);
}

/// Trait for I/O port access (Z80, x86, etc.)
///
/// Systems like Z80 have separate I/O address space from memory.
/// This trait allows CPUs to access I/O ports independently.
pub trait IoAddressable {
    fn io_read(&mut self, port: u8) -> u8;
    fn io_write(&mut self, port: u8, value: u8);
}
