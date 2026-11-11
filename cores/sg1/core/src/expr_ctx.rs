//! Bridges the generic [`ExprContext`] expression evaluator to the
//! Genesis 68000 CPU and address bus.
//!
//! A conditional breakpoint/watchpoint carries an expression string (see
//! [`retro_core::debug::expr`]); when the base address/value match holds,
//! the breakpoint machinery evaluates that expression against live machine
//! state through this adapter. It borrows the CPU immutably (registers are
//! read-only here) and the bus mutably (memory peeks route through
//! [`AddressBus::peek_byte`], which is side-effect free but takes `&mut`).
//!
//! Register names are matched case-insensitively by the evaluator (it
//! lowercases identifiers before calling [`ExprContext::register`]):
//!
//! | name(s)        | value                                  |
//! |----------------|----------------------------------------|
//! | `d0`..`d7`     | data registers (32-bit, zero-extended) |
//! | `a0`..`a7`     | address registers (32-bit)             |
//! | `sp`           | alias for `a7` (active stack pointer)   |
//! | `pc`           | program counter                        |
//! | `sr`           | status register (16-bit)               |
//! | `ccr`          | condition-code byte (`sr & 0xFF`)      |
//! | `usp` / `ssp`  | user / supervisor stack pointers       |
//!
//! Memory reads are big-endian (the 68000's byte order): `read_mem(addr,
//! width)` assembles `width` consecutive bytes most-significant first.

use crate::bus::AddressBus;
use crate::cpu68k::M68k;
use retro_core::debug::expr::ExprContext;

/// Expression-evaluation context over the 68000 CPU and Genesis bus.
pub struct M68kExprContext<'a> {
    cpu: &'a M68k,
    bus: &'a mut AddressBus,
}

impl<'a> M68kExprContext<'a> {
    /// Borrow the CPU (for registers) and bus (for memory peeks) for the
    /// duration of one breakpoint evaluation.
    pub fn new(cpu: &'a M68k, bus: &'a mut AddressBus) -> Self {
        Self { cpu, bus }
    }

    /// Resolve a `dN` / `aN` register name to its array index (`0..=7`),
    /// or `None` when the suffix is not a single digit in range.
    fn indexed(name: &str, prefix: char) -> Option<usize> {
        let rest = name.strip_prefix(prefix)?;
        match rest.as_bytes() {
            [d @ b'0'..=b'7'] => Some((d - b'0') as usize),
            _ => None,
        }
    }
}

impl ExprContext for M68kExprContext<'_> {
    fn register(&mut self, name: &str) -> Option<i64> {
        if let Some(i) = Self::indexed(name, 'd') {
            return Some(self.cpu.d[i] as i64);
        }
        if let Some(i) = Self::indexed(name, 'a') {
            return Some(self.cpu.a[i] as i64);
        }
        match name {
            "sp" => Some(self.cpu.a[7] as i64),
            "pc" => Some(self.cpu.pc as i64),
            "sr" => Some(self.cpu.sr as i64),
            "ccr" => Some((self.cpu.sr & 0xFF) as i64),
            // A7 is the live stack pointer of the current mode; the CPU
            // copies it back to `usp` or `ssp` only at the end of an
            // instruction, so that copy is stale at the first instruction of
            // an exception handler (the exception pushed its frame onto A7).
            "usp" => Some(if self.cpu.is_supervisor() {
                self.cpu.usp as i64
            } else {
                self.cpu.a[7] as i64
            }),
            "ssp" => Some(if self.cpu.is_supervisor() {
                self.cpu.a[7] as i64
            } else {
                self.cpu.ssp as i64
            }),
            _ => None,
        }
    }

    fn read_mem(&mut self, addr: u32, width: u8) -> i64 {
        let mut value: i64 = 0;
        for i in 0..width as u32 {
            let byte = self.bus.peek_byte(addr.wrapping_add(i));
            value = (value << 8) | byte as i64;
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retro_core::cpu::Addressable;
    use retro_core::debug::expr::eval_str;

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

    /// A bus seeded with a known byte pattern in work RAM so memory reads
    /// are deterministic.
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
        let mut bus = AddressBus::new();
        let mut ctx = M68kExprContext::new(&cpu, &mut bus);
        assert_eq!(ctx.register("d0"), Some(10));
        assert_eq!(ctx.register("d7"), Some(17));
        assert_eq!(ctx.register("a0"), Some(20));
        assert_eq!(ctx.register("a7"), Some(27));
        assert_eq!(ctx.register("sp"), Some(27)); // a7 alias
    }

    #[test]
    fn resolves_named_registers() {
        let cpu = cpu_with([0; 8], [0; 8]);
        let mut bus = AddressBus::new();
        let mut ctx = M68kExprContext::new(&cpu, &mut bus);
        assert_eq!(ctx.register("pc"), Some(0x00C0_FFEE));
        assert_eq!(ctx.register("sr"), Some(0x2715));
        assert_eq!(ctx.register("ccr"), Some(0x15));
    }

    #[test]
    fn stack_pointers_read_the_live_a7_of_the_current_mode() {
        // Supervisor mode right after an exception pushed its six-byte frame:
        // A7 has moved and the saved `ssp` has not been refreshed yet.
        let mut a = [0; 8];
        a[7] = 0x00FF_EFFA;
        let cpu = cpu_with([0; 8], a);
        let mut bus = AddressBus::new();
        let mut ctx = M68kExprContext::new(&cpu, &mut bus);
        assert_eq!(ctx.register("ssp"), Some(0x00FF_EFFA));
        assert_eq!(ctx.register("usp"), Some(0x00FF_E000));

        // User mode: A7 is the user stack pointer.
        let mut cpu = cpu_with([0; 8], a);
        cpu.sr = 0x0715;
        let mut ctx = M68kExprContext::new(&cpu, &mut bus);
        assert_eq!(ctx.register("usp"), Some(0x00FF_EFFA));
        assert_eq!(ctx.register("ssp"), Some(0x00FF_F000));
    }

    #[test]
    fn rejects_unknown_and_out_of_range_registers() {
        let cpu = cpu_with([0; 8], [0; 8]);
        let mut bus = AddressBus::new();
        let mut ctx = M68kExprContext::new(&cpu, &mut bus);
        assert_eq!(ctx.register("d8"), None); // index out of range
        assert_eq!(ctx.register("a9"), None);
        assert_eq!(ctx.register("d"), None); // no index
        assert_eq!(ctx.register("zz"), None);
    }

    #[test]
    fn reads_memory_big_endian() {
        let cpu = cpu_with([0; 8], [0; 8]);
        let mut bus = bus_with_ram(&[
            (0xFF_0000, 0x12),
            (0xFF_0001, 0x34),
            (0xFF_0002, 0x56),
            (0xFF_0003, 0x78),
        ]);
        let mut ctx = M68kExprContext::new(&cpu, &mut bus);
        assert_eq!(ctx.read_mem(0xFF_0000, 1), 0x12);
        assert_eq!(ctx.read_mem(0xFF_0000, 2), 0x1234);
        assert_eq!(ctx.read_mem(0xFF_0000, 4), 0x1234_5678);
    }

    #[test]
    fn evaluates_full_condition_against_machine_state() {
        let mut d = [0u32; 8];
        d[0] = 5;
        let mut a = [0u32; 8];
        a[1] = 0xFF_8000;
        let cpu = cpu_with(d, a);
        let mut bus = bus_with_ram(&[(0xFF_0000, 0x01), (0xFF_0001, 0x00)]);
        let mut ctx = M68kExprContext::new(&cpu, &mut bus);
        // d0 == 5 && a1 > 0xFF0000 && [0xFF0000].w == 0x100
        assert_eq!(
            eval_str(
                "d0 == 5 && a1 > 0xFF0000 && [0xFF0000].w == 0x100",
                &mut ctx
            ),
            Ok(true)
        );
        // Flip one clause false.
        assert_eq!(eval_str("d0 == 6", &mut ctx), Ok(false));
    }
}
