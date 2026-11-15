//! The 68000's registers by name, for the conditions of breakpoints and
//! watchpoints.
//!
//! A condition (see [`retro_core::debug::expr`]) is evaluated through a
//! [`retro_core::debug::CpuExprContext`] the system builds around the
//! 68000 and the bus: its registers through [`m68k_register`], its memory
//! through [`crate::bus::AddressBus::peek_byte`] (no side effects), values
//! big-endian, the 68000's byte order.
//!
//! The evaluator lowercases a name before it asks for it:
//!
//! | name(s)        | value                                  |
//! |----------------|----------------------------------------|
//! | `d0`..`d7`     | data registers (32-bit, zero-extended) |
//! | `a0`..`a7`     | address registers (32-bit)             |
//! | `sp`           | `a7`, the active stack pointer          |
//! | `pc`           | program counter                        |
//! | `sr`           | status register (16-bit)               |
//! | `ccr`          | condition-code byte (`sr & 0xFF`)      |
//! | `usp` / `ssp`  | user / supervisor stack pointers       |

use crate::cpu68k::M68k;

/// The value of the 68000 register `name` (lowercase), `None` for a name
/// the 68000 does not have.
pub fn m68k_register(cpu: &M68k, name: &str) -> Option<i64> {
    if let Some(index) = indexed(name, 'd') {
        return Some(i64::from(cpu.d[index]));
    }
    if let Some(index) = indexed(name, 'a') {
        return Some(i64::from(cpu.a[index]));
    }
    match name {
        "sp" => Some(i64::from(cpu.a[7])),
        "pc" => Some(i64::from(cpu.pc)),
        "sr" => Some(i64::from(cpu.sr)),
        "ccr" => Some(i64::from(cpu.sr & 0xFF)),
        // A7 is the live stack pointer of the current mode; the CPU copies
        // it back to `usp` or `ssp` only at the end of an instruction, so
        // that copy is stale at the first instruction of an exception
        // handler (the exception pushed its frame onto A7).
        "usp" => Some(i64::from(if cpu.is_supervisor() {
            cpu.usp
        } else {
            cpu.a[7]
        })),
        "ssp" => Some(i64::from(if cpu.is_supervisor() {
            cpu.a[7]
        } else {
            cpu.ssp
        })),
        _ => None,
    }
}

/// The index (`0..=7`) a `dN` / `aN` name gives, or `None` when the name has
/// another prefix or its suffix is not one digit in range.
fn indexed(name: &str, prefix: char) -> Option<usize> {
    let rest = name.strip_prefix(prefix)?;
    match rest.as_bytes() {
        [digit @ b'0'..=b'7'] => Some(usize::from(digit - b'0')),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::AddressBus;
    use retro_core::cpu::Addressable;
    use retro_core::debug::expr::{eval_str, ByteOrder, CpuExprContext, ExprContext};

    fn cpu_with(d: [u32; 8], a: [u32; 8]) -> M68k {
        let mut cpu = M68k::new();
        cpu.d = d;
        cpu.a = a;
        cpu.pc = 0x00C0_FFEE;
        cpu.sr = 0x2715;
        cpu.usp = 0x00FF_E000;
        cpu.ssp = 0x00FF_F000;
        cpu
    }

    /// A bus with known bytes in work RAM, so memory reads are known.
    fn bus_with_ram(bytes: &[(u32, u8)]) -> AddressBus {
        let mut bus = AddressBus::new();
        for &(addr, val) in bytes {
            bus.write(addr, val);
        }
        bus
    }

    #[test]
    fn resolves_data_and_address_registers() {
        let cpu = cpu_with(
            [10, 11, 12, 13, 14, 15, 16, 17],
            [20, 21, 22, 23, 24, 25, 26, 27],
        );
        assert_eq!(m68k_register(&cpu, "d0"), Some(10));
        assert_eq!(m68k_register(&cpu, "d7"), Some(17));
        assert_eq!(m68k_register(&cpu, "a0"), Some(20));
        assert_eq!(m68k_register(&cpu, "a7"), Some(27));
        assert_eq!(m68k_register(&cpu, "sp"), Some(27));
    }

    #[test]
    fn resolves_named_registers() {
        let cpu = cpu_with([0; 8], [0; 8]);
        assert_eq!(m68k_register(&cpu, "pc"), Some(0x00C0_FFEE));
        assert_eq!(m68k_register(&cpu, "sr"), Some(0x2715));
        assert_eq!(m68k_register(&cpu, "ccr"), Some(0x15));
    }

    #[test]
    fn stack_pointers_read_the_live_a7_of_the_current_mode() {
        // Supervisor mode right after an exception pushed its six-byte frame:
        // A7 has moved and the saved `ssp` has not been refreshed yet.
        let mut a = [0; 8];
        a[7] = 0x00FF_EFFA;
        let cpu = cpu_with([0; 8], a);
        assert_eq!(m68k_register(&cpu, "ssp"), Some(0x00FF_EFFA));
        assert_eq!(m68k_register(&cpu, "usp"), Some(0x00FF_E000));

        // User mode: A7 is the user stack pointer.
        let mut cpu = cpu_with([0; 8], a);
        cpu.sr = 0x0715;
        assert_eq!(m68k_register(&cpu, "usp"), Some(0x00FF_EFFA));
        assert_eq!(m68k_register(&cpu, "ssp"), Some(0x00FF_F000));
    }

    #[test]
    fn rejects_unknown_and_out_of_range_registers() {
        let cpu = cpu_with([0; 8], [0; 8]);
        assert_eq!(m68k_register(&cpu, "d8"), None);
        assert_eq!(m68k_register(&cpu, "a9"), None);
        assert_eq!(m68k_register(&cpu, "d"), None);
        assert_eq!(m68k_register(&cpu, "zz"), None);
    }

    #[test]
    fn reads_memory_big_endian() {
        let cpu = cpu_with([0; 8], [0; 8]);
        let bus = bus_with_ram(&[
            (0xFF_0000, 0x12),
            (0xFF_0001, 0x34),
            (0xFF_0002, 0x56),
            (0xFF_0003, 0x78),
        ]);
        let mut context = CpuExprContext::new(
            |name| m68k_register(&cpu, name),
            |address| bus.peek_byte(address),
            ByteOrder::BigEndian,
        );
        assert_eq!(context.read_mem(0xFF_0000, 1), 0x12);
        assert_eq!(context.read_mem(0xFF_0000, 2), 0x1234);
        assert_eq!(context.read_mem(0xFF_0000, 4), 0x1234_5678);
    }

    #[test]
    fn evaluates_full_condition_against_machine_state() {
        let mut d = [0u32; 8];
        d[0] = 5;
        let mut a = [0u32; 8];
        a[1] = 0xFF_8000;
        let cpu = cpu_with(d, a);
        let bus = bus_with_ram(&[(0xFF_0000, 0x01), (0xFF_0001, 0x00)]);
        let mut context = CpuExprContext::new(
            |name| m68k_register(&cpu, name),
            |address| bus.peek_byte(address),
            ByteOrder::BigEndian,
        );
        assert_eq!(
            eval_str(
                "d0 == 5 && a1 > 0xFF0000 && [0xFF0000].w == 0x100",
                &mut context
            ),
            Ok(true)
        );
        assert_eq!(eval_str("d0 == 6", &mut context), Ok(false));
    }
}
