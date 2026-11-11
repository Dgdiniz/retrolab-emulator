//! 65C816 addressing modes and operand resolution.
//!
//! All resolved addresses are 24-bit (`u32`). Operand sizes vary with
//! the M and X flags for immediate-mode instructions.

use crate::Wdc65816;
use retro_core::cpu::Addressable;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressingMode {
    Implied,
    Accumulator,
    Immediate,     // 1 or 2 bytes depending on M/X flag
    ImmediateX,    // immediate sized by X flag (CPX/CPY/LDX/LDY)
    ImmediateByte, // always a fixed 1-byte immediate (REP/SEP flag mask)
    Direct,        // Direct Page (replaces 6502 ZeroPage)
    DirectX,
    DirectY,
    DirectIndirect,      // (dp)
    DirectIndirectLong,  // [dp] -- 24-bit pointer
    DirectXIndirect,     // (dp,X)
    DirectIndirectY,     // (dp),Y
    DirectIndirectLongY, // [dp],Y
    Absolute,
    AbsoluteX,
    AbsoluteY,
    AbsoluteLong,           // 3-byte address
    AbsoluteLongX,          // 3-byte address + X
    AbsoluteIndirect,       // (abs) -- JMP only
    AbsoluteXIndirect,      // (abs,X) -- JMP/JSR
    AbsoluteIndirectLong,   // [abs] -- JML only
    StackRelative,          // sr,S
    StackRelativeIndirectY, // (sr,S),Y
    Relative,               // 1-byte signed offset
    RelativeLong,           // 2-byte signed offset (BRL)
    BlockMove,              // 2 bytes: src_bank, dst_bank
}

#[derive(Debug, Clone, Copy)]
pub struct ResolvedAddress {
    pub addr: u32,
    pub page_crossed: bool,
}

impl AddressingMode {
    /// Number of operand bytes following the opcode.
    /// `m8`/`x8` indicate whether accumulator/index are 8-bit.
    pub fn operand_size(self, m8: bool, x8: bool) -> u8 {
        match self {
            Self::Implied | Self::Accumulator => 0,

            Self::Immediate => {
                if m8 {
                    1
                } else {
                    2
                }
            }
            Self::ImmediateX => {
                if x8 {
                    1
                } else {
                    2
                }
            }
            // REP/SEP always carry a single 8-bit flag mask regardless of M/X.
            Self::ImmediateByte => 1,

            Self::Direct
            | Self::DirectX
            | Self::DirectY
            | Self::DirectIndirect
            | Self::DirectIndirectLong
            | Self::DirectXIndirect
            | Self::DirectIndirectY
            | Self::DirectIndirectLongY
            | Self::StackRelative
            | Self::StackRelativeIndirectY
            | Self::Relative => 1,

            Self::Absolute
            | Self::AbsoluteX
            | Self::AbsoluteY
            | Self::AbsoluteIndirect
            | Self::AbsoluteXIndirect
            | Self::AbsoluteIndirectLong
            | Self::RelativeLong
            | Self::BlockMove => 2,

            Self::AbsoluteLong | Self::AbsoluteLongX => 3,
        }
    }
}

impl Wdc65816 {
    /// Compute a direct-page effective offset for `D + (operand + index)`.
    ///
    /// In emulation mode with `D` aligned to a page boundary (`D & 0xFF == 0`),
    /// the 65C816 wraps the result inside the 256-byte direct page, the
    /// high byte of `D` is preserved untouched. Outside that case, the
    /// sum wraps inside a full 16-bit pointer (bank 0).
    #[inline]
    pub(crate) fn dp_offset(&self, operand: u8, idx: u16) -> u16 {
        if self.emulation && (self.d & 0xFF) == 0 {
            (self.d & 0xFF00) | ((operand as u16).wrapping_add(idx) & 0x00FF)
        } else {
            self.d.wrapping_add(operand as u16).wrapping_add(idx)
        }
    }

    /// Resolve the effective address for the current instruction's addressing mode.
    /// PC must already point past the opcode. This method advances PC past the operand.
    /// Returns (24-bit address, extra cycles from page crossing).
    pub(crate) fn resolve_address(
        &mut self,
        bus: &mut dyn Addressable,
        mode: AddressingMode,
    ) -> ResolvedAddress {
        match mode {
            AddressingMode::Implied | AddressingMode::Accumulator => ResolvedAddress {
                addr: 0,
                page_crossed: false,
            },

            AddressingMode::Immediate
            | AddressingMode::ImmediateX
            | AddressingMode::ImmediateByte => {
                let addr = Wdc65816::bank_addr(self.pbr, self.pc);
                let size = match mode {
                    // REP/SEP: fixed 8-bit flag mask.
                    AddressingMode::ImmediateByte => 1,
                    AddressingMode::ImmediateX => {
                        if self.xy_is_8bit() {
                            1
                        } else {
                            2
                        }
                    }
                    // Immediate, sized by the accumulator (M) flag.
                    _ => {
                        if self.a_is_8bit() {
                            1
                        } else {
                            2
                        }
                    }
                };
                self.pc = self.pc.wrapping_add(size);
                ResolvedAddress {
                    addr,
                    page_crossed: false,
                }
            }

            AddressingMode::Direct => {
                let operand = self.fetch_byte(bus);
                let addr = self.dp_offset(operand, 0) as u32;
                ResolvedAddress {
                    addr,
                    page_crossed: false,
                }
            }

            AddressingMode::DirectX => {
                let operand = self.fetch_byte(bus);
                let idx = if self.xy_is_8bit() {
                    self.x & 0xFF
                } else {
                    self.x
                };
                let addr = self.dp_offset(operand, idx) as u32;
                ResolvedAddress {
                    addr,
                    page_crossed: false,
                }
            }

            AddressingMode::DirectY => {
                let operand = self.fetch_byte(bus);
                let idx = if self.xy_is_8bit() {
                    self.y & 0xFF
                } else {
                    self.y
                };
                let addr = self.dp_offset(operand, idx) as u32;
                ResolvedAddress {
                    addr,
                    page_crossed: false,
                }
            }

            AddressingMode::DirectIndirect => {
                let operand = self.fetch_byte(bus);
                let ptr_addr = self.dp_offset(operand, 0) as u32;
                let target = Self::read_word_dp(bus, ptr_addr);
                let addr = Wdc65816::bank_addr(self.dbr, target);
                ResolvedAddress {
                    addr,
                    page_crossed: false,
                }
            }

            AddressingMode::DirectIndirectLong => {
                let operand = self.fetch_byte(bus);
                let ptr_addr = self.dp_offset(operand, 0) as u32;
                let addr = Self::read_long_dp(bus, ptr_addr);
                ResolvedAddress {
                    addr,
                    page_crossed: false,
                }
            }

            AddressingMode::DirectXIndirect => {
                let operand = self.fetch_byte(bus);
                let idx = if self.xy_is_8bit() {
                    self.x & 0xFF
                } else {
                    self.x
                };
                let ptr_addr = self.dp_offset(operand, idx) as u32;
                let target = Self::read_word_dp(bus, ptr_addr);
                let addr = Wdc65816::bank_addr(self.dbr, target);
                ResolvedAddress {
                    addr,
                    page_crossed: false,
                }
            }

            AddressingMode::DirectIndirectY => {
                let operand = self.fetch_byte(bus);
                let ptr_addr = self.dp_offset(operand, 0) as u32;
                let base = Self::read_word_dp(bus, ptr_addr);
                let idx = if self.xy_is_8bit() {
                    self.y & 0xFF
                } else {
                    self.y
                };
                let base_full = Wdc65816::bank_addr(self.dbr, base);
                let addr = base_full.wrapping_add(idx as u32);
                ResolvedAddress {
                    addr,
                    page_crossed: !self.xy_is_8bit() || (base_full & 0xFF00) != (addr & 0xFF00),
                }
            }

            AddressingMode::DirectIndirectLongY => {
                let operand = self.fetch_byte(bus);
                let ptr_addr = self.dp_offset(operand, 0) as u32;
                let base = Self::read_long_dp(bus, ptr_addr);
                let idx = if self.xy_is_8bit() {
                    self.y & 0xFF
                } else {
                    self.y
                };
                let addr = base.wrapping_add(idx as u32);
                ResolvedAddress {
                    addr,
                    page_crossed: false,
                }
            }

            AddressingMode::Absolute => {
                let offset = self.fetch_word(bus);
                let addr = Wdc65816::bank_addr(self.dbr, offset);
                ResolvedAddress {
                    addr,
                    page_crossed: false,
                }
            }

            AddressingMode::AbsoluteX => {
                let offset = self.fetch_word(bus);
                let base = Wdc65816::bank_addr(self.dbr, offset);
                let idx = if self.xy_is_8bit() {
                    self.x & 0xFF
                } else {
                    self.x
                };
                let addr = base.wrapping_add(idx as u32);
                ResolvedAddress {
                    addr,
                    page_crossed: !self.xy_is_8bit() || (base & 0xFF00) != (addr & 0xFF00),
                }
            }

            AddressingMode::AbsoluteY => {
                let offset = self.fetch_word(bus);
                let base = Wdc65816::bank_addr(self.dbr, offset);
                let idx = if self.xy_is_8bit() {
                    self.y & 0xFF
                } else {
                    self.y
                };
                let addr = base.wrapping_add(idx as u32);
                ResolvedAddress {
                    addr,
                    page_crossed: !self.xy_is_8bit() || (base & 0xFF00) != (addr & 0xFF00),
                }
            }

            AddressingMode::AbsoluteLong => {
                let addr = self.fetch_long(bus);
                ResolvedAddress {
                    addr,
                    page_crossed: false,
                }
            }

            AddressingMode::AbsoluteLongX => {
                let base = self.fetch_long(bus);
                let idx = if self.xy_is_8bit() {
                    self.x & 0xFF
                } else {
                    self.x
                };
                let addr = base.wrapping_add(idx as u32);
                ResolvedAddress {
                    addr,
                    page_crossed: false,
                }
            }

            AddressingMode::AbsoluteIndirect => {
                let ptr = self.fetch_word(bus);
                let target = Self::read_word_dp(bus, ptr as u32);
                ResolvedAddress {
                    addr: target as u32,
                    page_crossed: false,
                }
            }

            AddressingMode::AbsoluteXIndirect => {
                let ptr = self.fetch_word(bus);
                let idx = if self.xy_is_8bit() {
                    self.x & 0xFF
                } else {
                    self.x
                };
                let ptr_addr = Wdc65816::bank_addr(self.pbr, ptr.wrapping_add(idx));
                let target = Self::read_word_dp(bus, ptr_addr);
                ResolvedAddress {
                    addr: Wdc65816::bank_addr(self.pbr, target),
                    page_crossed: false,
                }
            }

            AddressingMode::AbsoluteIndirectLong => {
                let ptr = self.fetch_word(bus);
                let addr = Self::read_long_dp(bus, ptr as u32);
                ResolvedAddress {
                    addr,
                    page_crossed: false,
                }
            }

            AddressingMode::StackRelative => {
                let offset = self.fetch_byte(bus) as u16;
                let addr = self.sp.wrapping_add(offset) as u32;
                ResolvedAddress {
                    addr,
                    page_crossed: false,
                }
            }

            AddressingMode::StackRelativeIndirectY => {
                let offset = self.fetch_byte(bus) as u16;
                let ptr_addr = self.sp.wrapping_add(offset) as u32;
                let base = Self::read_word_dp(bus, ptr_addr);
                let idx = if self.xy_is_8bit() {
                    self.y & 0xFF
                } else {
                    self.y
                };
                let addr = Wdc65816::bank_addr(self.dbr, base).wrapping_add(idx as u32);
                ResolvedAddress {
                    addr,
                    page_crossed: false,
                }
            }

            AddressingMode::Relative => {
                let offset = self.fetch_byte(bus) as i8;
                let target = self.pc.wrapping_add(offset as u16);
                ResolvedAddress {
                    addr: Wdc65816::bank_addr(self.pbr, target),
                    page_crossed: (self.pc & 0xFF00) != (target & 0xFF00),
                }
            }

            AddressingMode::RelativeLong => {
                let offset = self.fetch_word(bus) as i16;
                let target = self.pc.wrapping_add(offset as u16);
                ResolvedAddress {
                    addr: Wdc65816::bank_addr(self.pbr, target),
                    page_crossed: false,
                }
            }

            AddressingMode::BlockMove => {
                // Block move operands are read by the MVN/MVP handlers directly
                ResolvedAddress {
                    addr: 0,
                    page_crossed: false,
                }
            }
        }
    }
}
