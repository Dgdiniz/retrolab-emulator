//! The GSU's registers as the S-CPU sees them, at `$3000-$32FF`.
//!
//! The S-CPU reads and writes R0-R15 a byte at a time, starts the GSU by
//! writing R15's high byte, sets the banks, the clock and the screen
//! registers, reads the status (which clears the interrupt), and reaches
//! the instruction cache at `$3100-$32FF`, for example to load code into
//! it before starting the GSU. The registers the GSU sets for itself (the
//! ROM and RAM banks, CBR) read back; the configuration ones are write-only
//! and read as 0, like every address the chip does not decode.

use crate::registers::status;
use crate::Gsu;

/// What an S-CPU address in the chip's register window names.
#[derive(Clone, Copy)]
enum Port {
    /// `$3000-$301F`: one byte of R0-R15, little-endian.
    Register { index: usize, high: bool },
    /// `$3030`/`$3031`: one byte of SFR.
    Status { high: bool },
    /// `$3033`: BRAMR.
    BackupRam,
    /// `$3034`: PBR.
    ProgramBank,
    /// `$3036`: ROMBR.
    RomBank,
    /// `$3037`: CFGR.
    Config,
    /// `$3038`: SCBR.
    ScreenBase,
    /// `$3039`: CLSR.
    ClockSelect,
    /// `$303A`: SCMR.
    ScreenMode,
    /// `$303B`: VCR.
    Version,
    /// `$303C`: RAMBR.
    RamBank,
    /// `$303E`/`$303F`: one byte of CBR.
    CacheBase { high: bool },
    /// `$3100-$32FF`: a byte of the instruction cache, counted from CBR.
    Cache { offset: u16 },
    /// Anything else.
    Unmapped,
}

impl Port {
    fn at(address: u16) -> Port {
        match address {
            0x3000..=0x301f => Port::Register {
                index: usize::from((address >> 1) & 15),
                high: address & 1 != 0,
            },
            0x3030 | 0x3031 => Port::Status {
                high: address == 0x3031,
            },
            0x3033 => Port::BackupRam,
            0x3034 => Port::ProgramBank,
            0x3036 => Port::RomBank,
            0x3037 => Port::Config,
            0x3038 => Port::ScreenBase,
            0x3039 => Port::ClockSelect,
            0x303a => Port::ScreenMode,
            0x303b => Port::Version,
            0x303c => Port::RamBank,
            0x303e | 0x303f => Port::CacheBase {
                high: address == 0x303f,
            },
            0x3100..=0x32ff => Port::Cache {
                offset: address - 0x3100,
            },
            _ => Port::Unmapped,
        }
    }
}

/// One byte of a 16-bit value.
fn byte(value: u16, high: bool) -> u8 {
    let [low, high_byte] = value.to_le_bytes();
    if high {
        high_byte
    } else {
        low
    }
}

/// `value` with one of its bytes replaced.
fn with_byte(value: u16, high: bool, data: u8) -> u16 {
    let [low, high_byte] = value.to_le_bytes();
    if high {
        u16::from_le_bytes([low, data])
    } else {
        u16::from_le_bytes([data, high_byte])
    }
}

impl Gsu {
    /// The cache byte `offset` bytes past CBR, in the 512-byte cache.
    fn cache_index(&self, offset: u16) -> usize {
        usize::from(offset.wrapping_add(self.cache_base) & 0x1ff)
    }

    /// The S-CPU reads the chip's register `address` (`$3000-$37FF`).
    /// Reading the status register's high byte clears its interrupt.
    pub fn read_port(&mut self, address: u16) -> u8 {
        match Port::at(address) {
            Port::Register { index, high } => byte(self.registers[index], high),
            Port::Status { high } => {
                let value = byte(self.status, high);
                if high {
                    self.set_flag(status::IRQ, false);
                }
                value
            }
            Port::ProgramBank => self.program_bank,
            Port::RomBank => self.rom_bank,
            Port::Version => self.version,
            Port::RamBank => u8::from(self.ram_bank),
            Port::CacheBase { high } => byte(self.cache_base, high),
            Port::Cache { offset } => self.cache.bytes[self.cache_index(offset)],
            Port::BackupRam
            | Port::Config
            | Port::ScreenBase
            | Port::ClockSelect
            | Port::ScreenMode
            | Port::Unmapped => 0,
        }
    }

    /// The S-CPU writes `value` to the chip's register `address`
    /// (`$3000-$37FF`).
    ///
    /// Writing R15's high byte starts the GSU. A register written this way
    /// is only stored: unlike an instruction's write, it starts no ROM read
    /// and is no jump. Stopping the GSU through the status register moves
    /// the cache back to address 0 and empties it, and a new program bank
    /// empties it too. Filling the last byte of a cache line from here
    /// marks the line as loaded.
    pub fn write_port(&mut self, address: u16, value: u8) {
        match Port::at(address) {
            Port::Register { index, high } => {
                self.registers[index] = with_byte(self.registers[index], high, value);
                if index == 15 && high {
                    self.set_flag(status::GO, true);
                }
            }
            Port::Status { high: false } => {
                let was_running = self.is_running();
                self.status = with_byte(self.status, false, value);
                if was_running && !self.is_running() {
                    self.cache_base = 0;
                    self.flush_cache();
                }
            }
            Port::Status { high: true } => self.status = with_byte(self.status, true, value),
            Port::BackupRam => self.backup_ram_enabled = value & 1 != 0,
            Port::ProgramBank => {
                self.program_bank = value;
                self.flush_cache();
            }
            Port::Config => self.config = value,
            Port::ScreenBase => self.screen_base = value,
            Port::ClockSelect => self.clock_divider = if value & 1 != 0 { 1 } else { 2 },
            Port::ScreenMode => self.screen_mode = value,
            Port::Cache { offset } => {
                let index = self.cache_index(offset);
                self.cache.bytes[index] = value;
                if index % 16 == 15 {
                    self.cache.valid[index / 16] = true;
                }
            }
            Port::RomBank
            | Port::Version
            | Port::RamBank
            | Port::CacheBase { .. }
            | Port::Unmapped => {}
        }
    }
}
