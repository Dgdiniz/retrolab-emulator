//! The 65C816's registers by the names a breakpoint's condition uses.
//!
//! A condition (`a == $12 && [$7E0010].w > 3`) is evaluated by retro-core's
//! expression engine against a [`retro_core::debug::CpuExprContext`]; the
//! emulator builds one around the CPU and the bus's side-effect-free peek,
//! and [`wdc65816_register`] answers its register names.

use wdc65816::Wdc65816;

/// The value of the 65C816's register `name` (lowercase), `None` for a name
/// it does not have.
///
/// The names are those [`crate::debugger::cpu_state_65816`] reports,
/// lowercased: `a` (the full 16-bit accumulator, `C`), `x`, `y`, `s`, `d`,
/// `db`, `pb`, `p` and `e` (1 in emulation mode); plus `sp`, the stack
/// pointer again, and `pc`, the 24-bit address `PBR:PC` of the instruction
/// about to run.
pub fn wdc65816_register(cpu: &Wdc65816, name: &str) -> Option<i64> {
    let value = match name {
        "a" => i64::from(cpu.c),
        "x" => i64::from(cpu.x),
        "y" => i64::from(cpu.y),
        "s" | "sp" => i64::from(cpu.sp),
        "d" => i64::from(cpu.d),
        "db" => i64::from(cpu.dbr),
        "pb" => i64::from(cpu.pbr),
        "p" => i64::from(cpu.p),
        "e" => i64::from(cpu.emulation),
        "pc" => (i64::from(cpu.pbr) << 16) | i64::from(cpu.pc),
        _ => return None,
    };
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_register_answers_by_its_lowercase_name() {
        let mut cpu = Wdc65816::new();
        cpu.c = 0x1234;
        cpu.x = 0x0056;
        cpu.y = 0x0078;
        cpu.sp = 0x01FD;
        cpu.d = 0x0300;
        cpu.dbr = 0x7E;
        cpu.pbr = 0x80;
        cpu.pc = 0x8123;
        cpu.p = 0x34;
        cpu.emulation = true;
        let names = [
            ("a", 0x1234),
            ("x", 0x56),
            ("y", 0x78),
            ("s", 0x01FD),
            ("sp", 0x01FD),
            ("d", 0x0300),
            ("db", 0x7E),
            ("pb", 0x80),
            ("p", 0x34),
            ("e", 1),
            ("pc", 0x80_8123),
        ];
        for (name, value) in names {
            assert_eq!(wdc65816_register(&cpu, name), Some(value), "{name}");
        }
        assert_eq!(wdc65816_register(&cpu, "d0"), None);
        assert_eq!(wdc65816_register(&cpu, "A"), None, "the engine lowercases");
    }
}
