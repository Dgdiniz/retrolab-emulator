//! 6502 addressing modes and operand resolution.

use retro_core::cpu::Addressable;

/// All 6502 addressing modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressingMode {
    Implied,
    Accumulator,
    Immediate,
    ZeroPage,
    ZeroPageX,
    ZeroPageY,
    Absolute,
    AbsoluteX,
    AbsoluteY,
    Indirect,
    IndirectX,
    IndirectY,
    Relative,
}

/// Resolved operand address plus page-crossing information.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedAddress {
    pub addr: u16,
    pub page_crossed: bool,
}

impl AddressingMode {
    /// Number of operand bytes following the opcode.
    pub fn operand_size(self) -> u8 {
        match self {
            Self::Implied | Self::Accumulator => 0,
            Self::Immediate
            | Self::ZeroPage
            | Self::ZeroPageX
            | Self::ZeroPageY
            | Self::IndirectX
            | Self::IndirectY
            | Self::Relative => 1,
            Self::Absolute | Self::AbsoluteX | Self::AbsoluteY | Self::Indirect => 2,
        }
    }
}

/// Resolve the effective address for a given addressing mode.
/// `pc` should point to the first operand byte (i.e. opcode address + 1).
/// Returns the resolved address and whether a page boundary was crossed.
pub fn resolve_address(
    bus: &mut dyn Addressable,
    mode: AddressingMode,
    pc: u16,
    x: u8,
    y: u8,
) -> ResolvedAddress {
    match mode {
        AddressingMode::Implied | AddressingMode::Accumulator => ResolvedAddress {
            addr: 0,
            page_crossed: false,
        },

        AddressingMode::Immediate => ResolvedAddress {
            addr: pc,
            page_crossed: false,
        },

        AddressingMode::ZeroPage => {
            let addr = bus.read(pc as u32) as u16;
            ResolvedAddress {
                addr,
                page_crossed: false,
            }
        }

        AddressingMode::ZeroPageX => {
            let base = bus.read(pc as u32);
            let addr = base.wrapping_add(x) as u16;
            ResolvedAddress {
                addr,
                page_crossed: false,
            }
        }

        AddressingMode::ZeroPageY => {
            let base = bus.read(pc as u32);
            let addr = base.wrapping_add(y) as u16;
            ResolvedAddress {
                addr,
                page_crossed: false,
            }
        }

        AddressingMode::Absolute => {
            let lo = bus.read(pc as u32) as u16;
            let hi = bus.read(pc.wrapping_add(1) as u32) as u16;
            ResolvedAddress {
                addr: (hi << 8) | lo,
                page_crossed: false,
            }
        }

        AddressingMode::AbsoluteX => {
            let lo = bus.read(pc as u32) as u16;
            let hi = bus.read(pc.wrapping_add(1) as u32) as u16;
            let base = (hi << 8) | lo;
            let addr = base.wrapping_add(x as u16);
            ResolvedAddress {
                addr,
                page_crossed: (base & 0xFF00) != (addr & 0xFF00),
            }
        }

        AddressingMode::AbsoluteY => {
            let lo = bus.read(pc as u32) as u16;
            let hi = bus.read(pc.wrapping_add(1) as u32) as u16;
            let base = (hi << 8) | lo;
            let addr = base.wrapping_add(y as u16);
            ResolvedAddress {
                addr,
                page_crossed: (base & 0xFF00) != (addr & 0xFF00),
            }
        }

        AddressingMode::Indirect => {
            let ptr_lo = bus.read(pc as u32) as u16;
            let ptr_hi = bus.read(pc.wrapping_add(1) as u32) as u16;
            let ptr = (ptr_hi << 8) | ptr_lo;
            // Page-boundary bug: high byte wraps within the same page
            let lo = bus.read(ptr as u32) as u16;
            let hi_ptr = (ptr & 0xFF00) | ((ptr.wrapping_add(1)) & 0x00FF);
            let hi = bus.read(hi_ptr as u32) as u16;
            ResolvedAddress {
                addr: (hi << 8) | lo,
                page_crossed: false,
            }
        }

        AddressingMode::IndirectX => {
            let zp = bus.read(pc as u32);
            let ptr = zp.wrapping_add(x);
            let lo = bus.read(ptr as u32) as u16;
            let hi = bus.read(ptr.wrapping_add(1) as u32) as u16;
            ResolvedAddress {
                addr: (hi << 8) | lo,
                page_crossed: false,
            }
        }

        AddressingMode::IndirectY => {
            let zp = bus.read(pc as u32);
            let lo = bus.read(zp as u32) as u16;
            let hi = bus.read(zp.wrapping_add(1) as u32) as u16;
            let base = (hi << 8) | lo;
            let addr = base.wrapping_add(y as u16);
            ResolvedAddress {
                addr,
                page_crossed: (base & 0xFF00) != (addr & 0xFF00),
            }
        }

        AddressingMode::Relative => {
            let offset = bus.read(pc as u32) as i8;
            // The branch target is relative to the byte AFTER the operand
            let target = pc.wrapping_add(1).wrapping_add(offset as u16);
            ResolvedAddress {
                addr: target,
                page_crossed: (pc.wrapping_add(1) & 0xFF00) != (target & 0xFF00),
            }
        }
    }
}
