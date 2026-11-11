//! DMA (Direct Memory Access) operations for VDP.
//!
//! The Genesis VDP supports three types of DMA:
//! 1. Memory to VRAM: Copy from 68k address space to VRAM
//! 2. VRAM Fill: Fill VRAM with a constant value
//! 3. VRAM Copy: Copy data within VRAM

/// DMA operation types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaOperation {
    /// Copy from 68k memory to VRAM.
    MemoryToVram {
        source: u32, // 68k address space
        dest: u16,   // VRAM address
        length: u16, // words
    },
    /// Fill VRAM with a constant value.
    VramFill { dest: u16, value: u16, length: u16 },
    /// Copy within VRAM.
    VramCopy { source: u16, dest: u16, length: u16 },
}

/// DMA state for the VDP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmaState {
    /// Current DMA operation (None if idle).
    pub operation: Option<DmaOperation>,
    /// Cycles remaining for current DMA.
    pub cycles_remaining: u32,
    /// Whether DMA is currently blocking the CPU.
    pub cpu_blocked: bool,
}

impl DmaState {
    /// Create a new idle DMA state.
    pub fn new() -> Self {
        Self {
            operation: None,
            cycles_remaining: 0,
            cpu_blocked: false,
        }
    }

    /// Check if DMA is active.
    pub fn is_active(&self) -> bool {
        self.operation.is_some()
    }

    /// Start a new DMA operation.
    pub fn start(&mut self, operation: DmaOperation, in_vblank: bool) {
        self.operation = Some(operation);

        // Calculate cycles based on operation type and timing
        self.cycles_remaining = match operation {
            DmaOperation::MemoryToVram { length, .. } => {
                // Memory to VRAM: 2 cycles/word during VBLANK, 16 cycles/word during active display
                if in_vblank {
                    (length as u32) * 2
                } else {
                    (length as u32) * 16
                }
            }
            DmaOperation::VramFill { length, .. } => {
                // VRAM Fill: 1 cycle/byte
                (length as u32) * 2 // length is in words, so * 2 for bytes
            }
            DmaOperation::VramCopy { length, .. } => {
                // VRAM Copy: 8 cycles/word
                (length as u32) * 8
            }
        };

        self.cpu_blocked = true;
    }

    /// Complete the current DMA operation.
    pub fn complete(&mut self) {
        self.operation = None;
        self.cycles_remaining = 0;
        self.cpu_blocked = false;
    }

    /// Execute DMA for the given number of cycles.
    /// Returns true if DMA completed.
    pub fn execute(&mut self, cycles: u32) -> bool {
        if !self.is_active() {
            return false;
        }

        if self.cycles_remaining <= cycles {
            self.complete();
            true
        } else {
            self.cycles_remaining -= cycles;
            false
        }
    }
}

impl Default for DmaState {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse DMA operation from VDP registers.
///
/// DMA registers (Genesis VDP):
/// - R19 (0x13): DMA Length Counter Low
/// - R20 (0x14): DMA Length Counter High
/// - R21 (0x15): DMA Source Address Low
/// - R22 (0x16): DMA Source Address Mid
/// - R23 (0x17): DMA Source Address High (bits 0-6) + DMA Type (bits 6-7);
///   bit 6 is an address bit when bit 7 is clear (a 68K-memory source)
///
/// DMA types (R23 bits 6-7):
/// - 00/01: 68K Memory to VRAM/CRAM/VSRAM
/// - 10: VRAM Fill
/// - 11: VRAM Copy
pub fn parse_dma_registers(registers: &[u8; 24], address: u16, _code: u8) -> Option<DmaOperation> {
    // Check if DMA is enabled (bit 4 of register 1)
    if (registers[1] & 0x10) == 0 {
        return None;
    }

    // Parse length from R19 (low) and R20 (high)
    let length = ((registers[20] as u16) << 8) | (registers[19] as u16);
    if length == 0 {
        return None;
    }

    // Check DMA mode from R23 bits 6-7
    let dma_mode = (registers[23] >> 6) & 0x03;

    match dma_mode {
        0b10 => {
            // VRAM Fill mode
            // In fill mode, the value to fill is the last value written to data port
            // We'll handle that in the VDP, so just return a fill operation
            Some(DmaOperation::VramFill {
                dest: address,
                value: 0, // Will be set by VDP from data port write
                length,
            })
        }
        0b11 => {
            // VRAM Copy mode
            // Source is from R21 (low) and R22 (mid)
            let source = ((registers[22] as u16) << 8) | (registers[21] as u16);
            Some(DmaOperation::VramCopy {
                source,
                dest: address,
                length,
            })
        }
        _ => {
            // Memory to VRAM mode (0b00, 0b01)
            // Source address from R21 (low), R22 (mid), R23 (high, bits 0-6)
            // These registers store the word address (byte address / 2)
            let addr_low = registers[21] as u32;
            let addr_mid = registers[22] as u32;
            let addr_high = (registers[23] & 0x7F) as u32; // Bit 6 is A23, bit 7 is mode bit

            // Reconstruct word address and convert to byte address
            let source = ((addr_high << 16) | (addr_mid << 8) | addr_low) << 1;

            // Check if this is a valid memory region
            // Genesis memory map: ROM 0x000000-0x3FFFFF, RAM 0xE00000-0xFFFFFF
            if source < 0x400000 || (0xE00000..=0xFFFFFF).contains(&source) {
                Some(DmaOperation::MemoryToVram {
                    source,
                    dest: address,
                    length,
                })
            } else {
                None
            }
        }
    }
}

/// Where a DMA from 68000 memory writes, from the VDP's access code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaTarget {
    Vram,
    Cram,
    Vsram,
    /// An access code that names none of the three: the transfer reads its
    /// source and writes nothing.
    Nowhere,
}

impl DmaTarget {
    /// The memory an access code's low four bits (CD3-CD0) select.
    pub fn from_access_code(code: u8) -> Self {
        match code & 0x0F {
            0x01 => Self::Vram,
            0x03 => Self::Cram,
            0x05 => Self::Vsram,
            _ => Self::Nowhere,
        }
    }
}

/// A DMA transfer from 68000 memory into the VDP, as the bus started it.
/// The instruction access log keeps one for each transfer an instruction's
/// write to the control port starts, so a tool can tell which instruction
/// copied which bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryDma {
    /// First source byte, a 68000 address.
    pub source: u32,
    /// Words copied (a length register of 0 copies 0x10000).
    pub words: u32,
    /// The memory written.
    pub target: DmaTarget,
    /// First address written in it.
    pub destination: u16,
}

impl MemoryDma {
    /// The source bytes read, as inclusive address spans: one, or two when
    /// the transfer wraps inside its 128 KB source window, whose base
    /// (register 23) a DMA never advances.
    pub fn source_spans(&self) -> Vec<(u32, u32)> {
        if self.words == 0 {
            return Vec::new();
        }
        let window_base = self.source & 0x00FE_0000;
        let start_word = (self.source & 0x1_FFFF) >> 1;
        let first_words = (0x1_0000 - start_word).min(self.words);
        let mut spans = vec![(
            window_base | (start_word << 1),
            window_base | ((start_word + first_words - 1) << 1) | 1,
        )];
        let rest = self.words - first_words;
        if rest > 0 {
            spans.push((window_base, window_base | ((rest - 1) << 1) | 1));
        }
        spans
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dma_state_new() {
        let dma = DmaState::new();
        assert!(!dma.is_active());
        assert!(!dma.cpu_blocked);
        assert_eq!(dma.cycles_remaining, 0);
    }

    #[test]
    fn test_dma_memory_to_vram_timing() {
        let mut dma = DmaState::new();

        // During active display (slower)
        dma.start(
            DmaOperation::MemoryToVram {
                source: 0x1000,
                dest: 0x0,
                length: 100,
            },
            false,
        );

        assert!(dma.is_active());
        assert!(dma.cpu_blocked);
        assert_eq!(dma.cycles_remaining, 100 * 16);
    }

    #[test]
    fn test_dma_memory_to_vram_vblank_faster() {
        let mut dma = DmaState::new();

        // During VBLANK (faster)
        dma.start(
            DmaOperation::MemoryToVram {
                source: 0x1000,
                dest: 0x0,
                length: 100,
            },
            true,
        );

        assert_eq!(dma.cycles_remaining, 100 * 2);
    }

    #[test]
    fn test_dma_vram_fill_timing() {
        let mut dma = DmaState::new();

        dma.start(
            DmaOperation::VramFill {
                dest: 0x0,
                value: 0xAA,
                length: 100,
            },
            false,
        );

        assert_eq!(dma.cycles_remaining, 100 * 2);
    }

    #[test]
    fn test_dma_vram_copy_timing() {
        let mut dma = DmaState::new();

        dma.start(
            DmaOperation::VramCopy {
                source: 0x0,
                dest: 0x1000,
                length: 100,
            },
            false,
        );

        assert_eq!(dma.cycles_remaining, 100 * 8);
    }

    #[test]
    fn test_dma_execute_partial() {
        let mut dma = DmaState::new();

        dma.start(
            DmaOperation::VramFill {
                dest: 0x0,
                value: 0xAA,
                length: 100,
            },
            false,
        );

        let initial_cycles = dma.cycles_remaining;
        let completed = dma.execute(50);

        assert!(!completed);
        assert!(dma.is_active());
        assert_eq!(dma.cycles_remaining, initial_cycles - 50);
    }

    #[test]
    fn test_dma_execute_complete() {
        let mut dma = DmaState::new();

        dma.start(
            DmaOperation::VramFill {
                dest: 0x0,
                value: 0xAA,
                length: 10,
            },
            false,
        );

        let completed = dma.execute(1000); // Execute more cycles than needed

        assert!(completed);
        assert!(!dma.is_active());
        assert!(!dma.cpu_blocked);
        assert_eq!(dma.cycles_remaining, 0);
    }

    #[test]
    fn test_parse_dma_memory_to_vram() {
        let mut regs = [0u8; 24];

        // Enable DMA (bit 4 of R1)
        regs[1] = 0x10;

        // Set length: 256 words (R19=low, R20=high)
        regs[19] = 0x00; // Length low
        regs[20] = 0x01; // Length high -> 0x0100 = 256

        // Set source address 0x1000 in 68k address space
        // Source is stored as word address: 0x1000 >> 1 = 0x800
        // R21=low, R22=mid, R23=high+mode
        regs[21] = 0x00; // Source low byte
        regs[22] = 0x08; // Source mid byte -> word address = 0x0800
        regs[23] = 0x00; // Source high (bits 0-6) + mode 00 = Memory to VRAM

        let dma = parse_dma_registers(&regs, 0x0, 0x01);

        assert!(dma.is_some());
        if let Some(DmaOperation::MemoryToVram {
            source,
            dest,
            length,
        }) = dma
        {
            assert_eq!(source, 0x1000); // 0x0800 << 1 = 0x1000
            assert_eq!(dest, 0x0);
            assert_eq!(length, 0x0100);
        } else {
            panic!("Expected MemoryToVram operation");
        }
    }

    #[test]
    fn test_parse_dma_vram_fill() {
        let mut regs = [0u8; 24];

        // Enable DMA
        regs[1] = 0x10;

        // Set length: 128 (R19=low, R20=high)
        regs[19] = 0x80; // Length low = 0x80
        regs[20] = 0x00; // Length high = 0x00 -> length = 0x0080 = 128

        // Set DMA mode to VRAM Fill (R23 bits 6-7 = 10)
        regs[23] = 0x80;

        let dma = parse_dma_registers(&regs, 0x1000, 0x01);

        assert!(dma.is_some());
        if let Some(DmaOperation::VramFill { dest, length, .. }) = dma {
            assert_eq!(dest, 0x1000);
            assert_eq!(length, 0x80);
        } else {
            panic!("Expected VramFill operation");
        }
    }

    #[test]
    fn test_parse_dma_vram_copy() {
        let mut regs = [0u8; 24];

        // Enable DMA
        regs[1] = 0x10;

        // Set length: 64 (R19=low, R20=high)
        regs[19] = 0x40; // Length low = 0x40
        regs[20] = 0x00; // Length high = 0x00 -> length = 0x0040 = 64

        // Set source address in VRAM: 0x0800 (R21=low, R22=mid)
        regs[21] = 0x00; // Source low
        regs[22] = 0x08; // Source mid -> source = 0x0800

        // VRAM Copy mode: bits 6-7 = 11
        regs[23] = 0xC0;

        let dma = parse_dma_registers(&regs, 0x2000, 0x01);

        assert!(dma.is_some());
        if let Some(DmaOperation::VramCopy {
            source,
            dest,
            length,
        }) = dma
        {
            assert_eq!(source, 0x0800);
            assert_eq!(dest, 0x2000);
            assert_eq!(length, 0x40);
        } else {
            panic!("Expected VramCopy operation");
        }
    }

    #[test]
    fn test_parse_dma_disabled() {
        let mut regs = [0u8; 24];

        // DMA disabled (bit 4 of R1 = 0)
        regs[1] = 0x00;

        // Set length (R19=low, R20=high)
        regs[19] = 0x40;
        regs[20] = 0x01;

        let dma = parse_dma_registers(&regs, 0x0, 0x01);
        assert!(dma.is_none());
    }

    #[test]
    fn test_parse_dma_zero_length() {
        let mut regs = [0u8; 24];

        // Enable DMA
        regs[1] = 0x10;

        // Length = 0 (R19=low, R20=high)
        regs[19] = 0x00;
        regs[20] = 0x00;

        let dma = parse_dma_registers(&regs, 0x0, 0x01);
        assert!(dma.is_none());
    }

    #[test]
    fn a_memory_dma_reads_one_span_inside_its_window() {
        let dma = MemoryDma {
            source: 0x0D_6000,
            words: 0x100,
            target: DmaTarget::Vram,
            destination: 0x4000,
        };
        assert_eq!(dma.source_spans(), vec![(0x0D_6000, 0x0D_61FF)]);
    }

    #[test]
    fn a_memory_dma_wraps_inside_its_128kb_window() {
        // The window's last word, then its first.
        let dma = MemoryDma {
            source: 0xFF_FFFE,
            words: 2,
            target: DmaTarget::Cram,
            destination: 0,
        };
        assert_eq!(
            dma.source_spans(),
            vec![(0xFF_FFFE, 0xFF_FFFF), (0xFE_0000, 0xFE_0001)]
        );
    }

    #[test]
    fn the_access_code_names_the_dma_target() {
        assert_eq!(DmaTarget::from_access_code(0x21), DmaTarget::Vram);
        assert_eq!(DmaTarget::from_access_code(0x23), DmaTarget::Cram);
        assert_eq!(DmaTarget::from_access_code(0x25), DmaTarget::Vsram);
        assert_eq!(DmaTarget::from_access_code(0x20), DmaTarget::Nowhere);
    }
}
