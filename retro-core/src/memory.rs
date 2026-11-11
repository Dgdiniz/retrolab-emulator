//! Memory-related traits and utilities.

use crate::cpu::Addressable;

/// Trait for debuggable memory regions.
pub trait DebugMemory {
    /// Read bytes without side effects (for debugging).
    fn debug_read(&self, addr: u32, size: u32) -> Vec<u8>;

    /// Write bytes (for debugging/cheating).
    fn debug_write(&mut self, addr: u32, data: &[u8]);
}

/// Simple RAM implementation.
#[derive(Clone)]
pub struct Ram {
    data: Vec<u8>,
    mask: u32,
}

impl Ram {
    pub fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
            mask: (size - 1) as u32,
        }
    }

    pub fn with_data(data: Vec<u8>) -> Self {
        let size = data.len();
        Self {
            data,
            mask: (size - 1) as u32,
        }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

impl Addressable for Ram {
    fn read(&mut self, addr: u32) -> u8 {
        self.data[(addr & self.mask) as usize]
    }

    fn write(&mut self, addr: u32, value: u8) {
        self.data[(addr & self.mask) as usize] = value;
    }
}

impl DebugMemory for Ram {
    fn debug_read(&self, addr: u32, size: u32) -> Vec<u8> {
        let start = (addr & self.mask) as usize;
        let end = std::cmp::min(start + size as usize, self.data.len());
        self.data[start..end].to_vec()
    }

    fn debug_write(&mut self, addr: u32, data: &[u8]) {
        let start = (addr & self.mask) as usize;
        let end = std::cmp::min(start + data.len(), self.data.len());
        self.data[start..end].copy_from_slice(&data[..(end - start)]);
    }
}

/// Read-only memory (ROM).
#[derive(Clone)]
pub struct Rom {
    data: Vec<u8>,
    mask: u32,
}

impl Rom {
    pub fn new(data: Vec<u8>) -> Self {
        let size = data.len();
        Self {
            data,
            mask: (size - 1) as u32,
        }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }
}

impl Addressable for Rom {
    fn read(&mut self, addr: u32) -> u8 {
        self.data[(addr & self.mask) as usize]
    }

    fn write(&mut self, _addr: u32, _value: u8) {
        // ROM is read-only
    }
}

impl DebugMemory for Rom {
    fn debug_read(&self, addr: u32, size: u32) -> Vec<u8> {
        let start = (addr & self.mask) as usize;
        let end = std::cmp::min(start + size as usize, self.data.len());
        self.data[start..end].to_vec()
    }

    fn debug_write(&mut self, _addr: u32, _data: &[u8]) {
        // ROM is read-only
    }
}
