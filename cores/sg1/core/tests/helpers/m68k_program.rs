//! A hand-assembled 68000 program for a test: words placed at addresses,
//! labels, branches resolved to them, the exception vectors, and a Mega
//! Drive header, built into a ROM image.
//!
//! Shared by several test binaries, each using a different part of it.
#![allow(dead_code)]

use std::collections::HashMap;

/// A program being put together.
pub struct M68kProgram {
    rom: Vec<u8>,
    /// Where the next word goes.
    at: usize,
    labels: HashMap<&'static str, usize>,
    /// Displacements to fill in: where the displacement word is, the label
    /// it reaches, and the address it counts from.
    branches: Vec<(usize, &'static str, usize)>,
}

impl M68kProgram {
    /// An empty ROM of `size` bytes, starting the program at `start` with
    /// the stack at the top of work RAM.
    pub fn new(size: usize, start: u32) -> Self {
        let mut rom = vec![0xFF_u8; size];
        rom[0x000..0x004].copy_from_slice(&0x00FF_FE00_u32.to_be_bytes());
        rom[0x004..0x008].copy_from_slice(&start.to_be_bytes());
        let header = &mut rom[0x100..0x200];
        header[0x00..0x10].copy_from_slice(b"SEGA MEGA DRIVE ");
        header[0x10..0x20].copy_from_slice(b"(C)TEST 2026.NOV");
        let mut title = [b' '; 48];
        title[..12].copy_from_slice(b"TEST PROGRAM");
        header[0x20..0x50].copy_from_slice(&title);
        header[0x50..0x80].copy_from_slice(&title);
        header[0x80..0x8E].copy_from_slice(b"GM 00000000-00");
        header[0x90..0xA0].copy_from_slice(b"J               ");
        header[0xA0..0xA4].copy_from_slice(&0x0000_0000_u32.to_be_bytes());
        header[0xA4..0xA8].copy_from_slice(&(size as u32 - 1).to_be_bytes());
        header[0xA8..0xAC].copy_from_slice(&0x00FF_0000_u32.to_be_bytes());
        header[0xAC..0xB0].copy_from_slice(&0x00FF_FFFF_u32.to_be_bytes());
        header[0xB0..0xF0].fill(b' ');
        header[0xF0..0x100].copy_from_slice(b"JUE             ");
        Self {
            rom,
            at: start as usize,
            labels: HashMap::new(),
            branches: Vec::new(),
        }
    }

    /// Point exception vector `index` (28 is the level 4 interrupt, 30 the
    /// level 6) at `address`.
    pub fn vector(&mut self, index: usize, address: u32) -> &mut Self {
        self.rom[index * 4..index * 4 + 4].copy_from_slice(&address.to_be_bytes());
        self
    }

    /// Go on placing code at `address`.
    pub fn org(&mut self, address: usize) -> &mut Self {
        self.at = address;
        self
    }

    /// Place `words`.
    pub fn words(&mut self, words: &[u16]) -> &mut Self {
        for word in words {
            self.rom[self.at..self.at + 2].copy_from_slice(&word.to_be_bytes());
            self.at += 2;
        }
        self
    }

    /// Name the address the next word goes to.
    pub fn label(&mut self, name: &'static str) -> &mut Self {
        self.labels.insert(name, self.at);
        self
    }

    /// A branch with a 16-bit displacement to `target`: `opcode` is the
    /// condition's opcode with a zero displacement (`$6000` BRA, `$6600`
    /// BNE, `$6700` BEQ).
    pub fn branch(&mut self, opcode: u16, target: &'static str) -> &mut Self {
        let base = self.at + 2;
        self.words(&[opcode, 0]);
        self.branches.push((self.at - 2, target, base));
        self
    }

    /// DBRA on data register `register` to `target`.
    pub fn dbra(&mut self, register: u16, target: &'static str) -> &mut Self {
        self.branch(0x51C8 | register, target)
    }

    /// Place `bytes` at `address`.
    pub fn data(&mut self, address: usize, bytes: &[u8]) -> &mut Self {
        self.rom[address..address + bytes.len()].copy_from_slice(bytes);
        self
    }

    /// The ROM, its branches resolved.
    pub fn build(&mut self) -> Vec<u8> {
        for &(at, target, base) in &self.branches {
            let to = *self
                .labels
                .get(target)
                .unwrap_or_else(|| panic!("no label {target}"));
            let displacement = to as i64 - base as i64;
            let displacement = i16::try_from(displacement).expect("a branch in reach");
            self.rom[at..at + 2].copy_from_slice(&displacement.to_be_bytes());
        }
        self.rom.clone()
    }
}
