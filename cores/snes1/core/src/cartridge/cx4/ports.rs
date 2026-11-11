//! The chip as the S-CPU sees it, at `$6000-$7FFF`.
//!
//! The window holds the data RAM, 3 KiB at `$6000-$6BFF`, seen again at
//! `$7000-$7BFF`; and, at `$7F40-$7FFF`, the chip's registers: the DMA's
//! source, length and target, the program's offset, page and entry point,
//! the cache locks, the wait states, the interrupt mask, a status byte, the
//! control writes that halt, suspend and resume the core, thirty-two vector
//! bytes, and the sixteen general registers three bytes each. While the core
//! runs or a DMA does, the data RAM half of the window reads `$FF` and drops
//! writes; the registers stay reachable.

use super::hg51b::Hg51b;

/// The 24 bits of a register.
const MASK_24: u32 = 0xFF_FFFF;

/// The status byte's bits.
const STATUS_BUS_IN_USE: u8 = 0x80;
const STATUS_BUSY: u8 = 0x40;
const STATUS_IRQ: u8 = 0x02;
const STATUS_SUSPENDED: u8 = 0x01;

/// What an offset of the window holds.
enum Window {
    /// The data RAM, at this index.
    DataRam(usize),
    /// Between the data RAM and the registers: open bus.
    Gap,
    /// The chip's registers.
    Registers,
}

impl Window {
    /// The part of the window `address` (`$6000-$7FFF`) falls in.
    fn at(address: u16) -> Window {
        match address & 0x1FFF {
            0x1F40..=0x1FFF => Window::Registers,
            offset if offset & 0x0C00 == 0x0C00 => Window::Gap,
            offset => Window::DataRam(usize::from(offset & 0x0FFF)),
        }
    }
}

/// A register of `$7F40-$7FFF`. A register several bytes long is reached a
/// byte at a time; `lane` 0 is its low byte.
enum Port {
    /// `$7F40-$7F42`.
    DmaSource { lane: usize },
    /// `$7F43-$7F44`.
    DmaLength { lane: usize },
    /// `$7F45-$7F47`; writing the top byte starts the DMA.
    DmaTarget { lane: usize },
    /// `$7F48`; writing it starts a preload of that cache page.
    PreloadPage,
    /// `$7F49-$7F4B`.
    ProgramOffset { lane: usize },
    /// `$7F4C`: bit 0 locks cache page 0, bit 1 page 1.
    CacheLocks,
    /// `$7F4D-$7F4E` (15 bits).
    ProgramPage { lane: usize },
    /// `$7F4F`; writing it starts the program there.
    EntryPc,
    /// `$7F50`: the ROM's wait states in the high nibble, the RAM's in the
    /// low one.
    WaitStates,
    /// `$7F51`.
    IrqMask,
    /// `$7F52`.
    Register7f52,
    /// `$7F53-$7F5F`: read, the status byte (except at `$58` and `$5A`,
    /// which read 0); written, a control.
    Control { low: u8 },
    /// `$7F60-$7F7F`.
    Vector { index: usize },
    /// `$7F80-$7FAF` and `$7FC0-$7FEF`: R0-R15, three bytes each.
    GeneralRegister { index: usize, lane: usize },
    /// Nothing.
    Unmapped,
}

impl Port {
    /// The register at `address`. Only the low ten bits count: the registers
    /// are the top 192 bytes of a 1 KiB block whose start names nothing.
    fn at(address: u16) -> Port {
        let offset = address & 0x03FF;
        let low = offset as u8;
        let lane_from = |first: u16| usize::from(offset - first);
        match offset {
            0x340..=0x342 => Port::DmaSource {
                lane: lane_from(0x340),
            },
            0x343..=0x344 => Port::DmaLength {
                lane: lane_from(0x343),
            },
            0x345..=0x347 => Port::DmaTarget {
                lane: lane_from(0x345),
            },
            0x348 => Port::PreloadPage,
            0x349..=0x34B => Port::ProgramOffset {
                lane: lane_from(0x349),
            },
            0x34C => Port::CacheLocks,
            0x34D..=0x34E => Port::ProgramPage {
                lane: lane_from(0x34D),
            },
            0x34F => Port::EntryPc,
            0x350 => Port::WaitStates,
            0x351 => Port::IrqMask,
            0x352 => Port::Register7f52,
            0x353..=0x35F => Port::Control { low },
            0x360..=0x37F => Port::Vector {
                index: usize::from(low & 0x1F),
            },
            0x380..=0x3AF | 0x3C0..=0x3EF => {
                let position = usize::from(low & 0x3F);
                Port::GeneralRegister {
                    index: position / 3,
                    lane: position % 3,
                }
            }
            _ => Port::Unmapped,
        }
    }
}

/// Byte `lane` of `value`.
fn byte_of(value: u32, lane: usize) -> u8 {
    (value >> (8 * lane)) as u8
}

/// `value` with byte `lane` replaced by `byte`.
fn with_byte(value: u32, lane: usize, byte: u8) -> u32 {
    let shift = 8 * lane;
    (value & !(0xFF << shift)) | (u32::from(byte) << shift)
}

impl Hg51b {
    /// The S-CPU's view of the window is shut over the data RAM: the core
    /// is running, or a DMA is.
    fn data_ram_locked(&self) -> bool {
        self.io.dma_pending || !self.halted
    }

    /// The S-CPU reads `address` (`$6000-$7FFF`); `open_bus` is what the gap
    /// between the data RAM and the registers gives.
    pub fn read_window(&self, address: u16, open_bus: u8) -> u8 {
        match Window::at(address) {
            Window::Registers => self.read_port(address),
            Window::DataRam(_) | Window::Gap if self.data_ram_locked() => 0xFF,
            Window::DataRam(index) => self.data_ram[index],
            Window::Gap => open_bus,
        }
    }

    /// The S-CPU writes `value` to `address` (`$6000-$7FFF`).
    pub fn write_window(&mut self, address: u16, value: u8) {
        match Window::at(address) {
            Window::Registers => self.write_port(address, value),
            Window::DataRam(_) | Window::Gap if self.data_ram_locked() => {}
            Window::DataRam(index) => self.data_ram[index] = value,
            Window::Gap => {}
        }
    }

    /// The status byte: the cart bus in use, busy, the interrupt pending,
    /// suspended.
    pub fn status(&self) -> u8 {
        [
            (self.bus_in_use(), STATUS_BUS_IN_USE),
            (self.busy(), STATUS_BUSY),
            (self.irq_pending, STATUS_IRQ),
            (self.io.suspended, STATUS_SUSPENDED),
        ]
        .into_iter()
        .filter(|&(set, _)| set)
        .fold(0, |status, (_, bit)| status | bit)
    }

    /// The S-CPU reads register `address` (`$7F40-$7FFF`).
    pub fn read_port(&self, address: u16) -> u8 {
        let io = &self.io;
        match Port::at(address) {
            Port::DmaSource { lane } => byte_of(io.dma_source, lane),
            Port::DmaLength { lane } => byte_of(u32::from(io.dma_length), lane),
            Port::DmaTarget { lane } => byte_of(io.dma_target, lane),
            Port::PreloadPage => io.preload_page,
            Port::ProgramOffset { lane } => byte_of(io.program_offset, lane),
            Port::CacheLocks => self
                .cache
                .iter()
                .enumerate()
                .fold(0, |locks, (slot, page)| {
                    locks | u8::from(page.locked) << slot
                }),
            Port::ProgramPage { lane } => byte_of(u32::from(io.program_page), lane),
            Port::EntryPc => io.entry_pc,
            Port::WaitStates => (io.rom_wait << 4) | io.ram_wait,
            Port::IrqMask => u8::from(io.irq_masked),
            Port::Register7f52 => io.register_7f52,
            Port::Control { low: 0x58 | 0x5A } => 0,
            Port::Control { .. } => self.status(),
            Port::Vector { index } => io.vectors[index],
            Port::GeneralRegister { index, lane } => {
                // While the core runs, the bytes show the last value through
                // its register file instead.
                let value = if self.halted || io.suspended {
                    self.registers[index]
                } else {
                    self.last_value
                };
                byte_of(value, lane)
            }
            Port::Unmapped => 0,
        }
    }

    /// The S-CPU writes `value` to register `address` (`$7F40-$7FFF`). The
    /// writes that start work (the DMA's top byte, the preload page, the
    /// entry point) start it only while the core is halted; the work itself
    /// runs as the chip catches up with the S-CPU.
    pub fn write_port(&mut self, address: u16, value: u8) {
        match Port::at(address) {
            Port::DmaSource { lane } => {
                self.io.dma_source = with_byte(self.io.dma_source, lane, value) & MASK_24;
            }
            Port::DmaLength { lane } => {
                self.io.dma_length = with_byte(u32::from(self.io.dma_length), lane, value) as u16;
            }
            Port::DmaTarget { lane } => {
                self.io.dma_target = with_byte(self.io.dma_target, lane, value) & MASK_24;
                if lane == 2 {
                    self.request_dma();
                }
            }
            Port::PreloadPage => self.request_preload(value),
            Port::ProgramOffset { lane } => {
                self.io.program_offset = with_byte(self.io.program_offset, lane, value) & MASK_24;
            }
            Port::CacheLocks => {
                if self.halted {
                    for (slot, page) in self.cache.iter_mut().enumerate() {
                        page.locked = (value >> slot) & 1 != 0;
                    }
                }
            }
            Port::ProgramPage { lane } => {
                self.io.program_page =
                    (with_byte(u32::from(self.io.program_page), lane, value) & 0x7FFF) as u16;
            }
            Port::EntryPc => {
                self.io.entry_pc = value;
                self.start_program();
            }
            Port::WaitStates => {
                self.io.rom_wait = (value >> 4) & 7;
                self.io.ram_wait = value & 7;
            }
            Port::IrqMask => {
                // Masking also drops an interrupt already pending.
                self.io.irq_masked = value & 1 != 0;
                if self.io.irq_masked {
                    self.irq_pending = false;
                }
            }
            Port::Register7f52 => self.io.register_7f52 = value & 1,
            Port::Control { low } => self.control(low),
            Port::Vector { index } => self.io.vectors[index] = value,
            Port::GeneralRegister { index, lane } => {
                // A running core's registers cannot be written.
                if self.halted || self.io.suspended {
                    self.registers[index] = with_byte(self.registers[index], lane, value) & MASK_24;
                }
            }
            Port::Unmapped => {}
        }
    }

    /// A write to `$7F53-$7F5F`: `$53` halts the core, `$55-$5C` suspend it
    /// for 32 clocks per step past `$55` (`$55` until `$5D` resumes it),
    /// `$5E` acknowledges the interrupt.
    fn control(&mut self, low: u8) {
        match low {
            0x53 => self.halted = true,
            0x55..=0x5C => {
                self.io.suspended = true;
                self.io.suspend_remaining = 32 * u32::from(low - 0x55);
            }
            0x5D => self.io.suspended = false,
            0x5E => self.irq_pending = false,
            _ => {}
        }
    }
}
