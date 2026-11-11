//! Shared operand-fetch helpers for the SPC700's "12-mode binary ALU"
//! pattern. `ADC`, `SBC`, `AND`, `OR`, `EOR`, and `CMP A` each have
//! exactly the same 12 opcode variants (low row: `$X4` `$X5` `$X6`
//! `$X7` `$X8` `$X9`, high row: `$Y4` `$Y5` `$Y6` `$Y7` `$Y8` `$Y9`,
//! where the X/Y first-nibbles depend on the mnemonic). Nine of those
//! modes target `A` (the implicit destination); the other three target
//! a memory location with a separate source.
//!
//! Separating the fetch helpers here keeps each per-mnemonic handler
//! down to ~30 lines of dispatch + ALU call instead of repeating the
//! same 12-mode `match` block six times. The split also makes the
//! encoding's operand order (source first, destination second for
//! `$dp,$dp`; immediate first, destination second for `$dp,#imm`)
//! live in exactly one place.

use retro_core::cpu::Addressable;

use crate::addressing::{
    dp_addr, dp_addr_x, fetch_pc_byte, fetch_pc_word, indirect_indirect_x, indirect_indirect_y,
    x_indirect_addr, y_indirect_addr, AddressingMode,
};
use crate::Spc700;

/// Read the source operand for an "A-target" binary-ALU opcode.
///
/// Returns just the source byte; the caller computes
/// `cpu.a = op(cpu.a, src)` and writes the result back to `A`. The
/// modes covered here are the 9 that use `A` as the implicit
/// destination across `ADC`/`SBC`/`AND`/`OR`/`EOR`/`CMP A`:
/// `Immediate`, `DirectPage`, `DirectPageX`, `Absolute`, `AbsoluteX`,
/// `AbsoluteY`, `IndirectX`, `IndirectIndirectX`, `IndirectIndirectY`.
pub(super) fn read_a_target_operand(
    cpu: &mut Spc700,
    bus: &mut dyn Addressable,
    mode: AddressingMode,
) -> u8 {
    match mode {
        AddressingMode::Immediate => fetch_pc_byte(cpu, bus),
        AddressingMode::DirectPage => {
            let dp = fetch_pc_byte(cpu, bus);
            bus.read(dp_addr(cpu, dp) as u32)
        }
        AddressingMode::DirectPageX => {
            let dp = fetch_pc_byte(cpu, bus);
            bus.read(dp_addr_x(cpu, dp) as u32)
        }
        AddressingMode::Absolute => {
            let addr = fetch_pc_word(cpu, bus);
            bus.read(addr as u32)
        }
        AddressingMode::AbsoluteX => {
            let base = fetch_pc_word(cpu, bus);
            bus.read(base.wrapping_add(cpu.x as u16) as u32)
        }
        AddressingMode::AbsoluteY => {
            let base = fetch_pc_word(cpu, bus);
            bus.read(base.wrapping_add(cpu.y as u16) as u32)
        }
        AddressingMode::IndirectX => bus.read(x_indirect_addr(cpu) as u32),
        AddressingMode::IndirectIndirectX => {
            let dp = fetch_pc_byte(cpu, bus);
            let addr = indirect_indirect_x(cpu, bus, dp);
            bus.read(addr as u32)
        }
        AddressingMode::IndirectIndirectY => {
            let dp = fetch_pc_byte(cpu, bus);
            let addr = indirect_indirect_y(cpu, bus, dp);
            bus.read(addr as u32)
        }
        _ => unreachable!(
            "read_a_target_operand called with non-A-target mode: {:?}",
            mode
        ),
    }
}

/// Resolve `(dst_addr, dst_val, src_val)` for a "memory-target"
/// binary-ALU opcode.
///
/// The three modes covered (`DirectPageDirectPage`, `DirectPageImm`,
/// `IxIy`) all compute the result against an in-memory destination
/// rather than `A`. Caller pattern:
///
/// ```text
/// let (dst_addr, dst_val, src_val) = read_mem_target_operands(cpu, bus, info.mode);
/// let result = add_with_carry(cpu, dst_val, src_val);
/// bus.write(dst_addr as u32, result);   // CMP skips this write
/// ```
///
/// The encoding's operand order:
/// - `$dp,$dp` (mode `DirectPageDirectPage`): source byte is encoded
///   FIRST in the instruction stream, destination SECOND.
/// - `$dp,#imm` (mode `DirectPageImm`): immediate is FIRST, destination
///   SECOND.
/// - `(X),(Y)` (mode `IxIy`): destination is `(X)`, source is `(Y)`.
pub(super) fn read_mem_target_operands(
    cpu: &mut Spc700,
    bus: &mut dyn Addressable,
    mode: AddressingMode,
) -> (u16, u8, u8) {
    let dp_base = cpu.dp_base();
    match mode {
        AddressingMode::DirectPageDirectPage => {
            let src_dp = fetch_pc_byte(cpu, bus);
            let dst_dp = fetch_pc_byte(cpu, bus);
            let src_val = bus.read((dp_base | src_dp as u16) as u32);
            let dst_addr = dp_base | dst_dp as u16;
            let dst_val = bus.read(dst_addr as u32);
            (dst_addr, dst_val, src_val)
        }
        AddressingMode::DirectPageImm => {
            let imm = fetch_pc_byte(cpu, bus);
            let dst_dp = fetch_pc_byte(cpu, bus);
            let dst_addr = dp_base | dst_dp as u16;
            let dst_val = bus.read(dst_addr as u32);
            (dst_addr, dst_val, imm)
        }
        AddressingMode::IxIy => {
            let src_val = bus.read(y_indirect_addr(cpu) as u32);
            let dst_addr = x_indirect_addr(cpu);
            let dst_val = bus.read(dst_addr as u32);
            (dst_addr, dst_val, src_val)
        }
        _ => unreachable!(
            "read_mem_target_operands called with non-mem-target mode: {:?}",
            mode
        ),
    }
}

/// Returns `true` when `mode` is one of the three "memory-target"
/// modes (`DirectPageDirectPage`, `DirectPageImm`, `IxIy`); the
/// remaining 9 modes in the binary-ALU set are "A-target".
pub(super) fn is_mem_target(mode: AddressingMode) -> bool {
    matches!(
        mode,
        AddressingMode::DirectPageDirectPage | AddressingMode::DirectPageImm | AddressingMode::IxIy
    )
}
