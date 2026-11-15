//! Which 68000 instructions call a subroutine and which return, for the
//! debugger's call stack.
//!
//! The debugger reads an instruction's opcode before the instruction runs
//! and asks [`call_step`] what it is: a call pushes a frame whose return
//! address is the address after the call, which the call's own length
//! gives (the PC after the call is the subroutine's address, not the
//! return address); a return pops the innermost frame.

/// What an instruction does to the call stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallStep {
    /// A subroutine call (`JSR`, `BSR`) of `length` bytes: it returns to
    /// the address `length` bytes after its own.
    Call {
        /// The instruction's length in bytes, its opcode included.
        length: u32,
    },
    /// A return (`RTS`, `RTR`, `RTE`): it takes the innermost frame off.
    Return,
    /// Anything else, a `JSR` with an addressing mode the 68000 refuses
    /// included.
    Other,
}

/// `JSR <ea>`: `0100 1110 10 mmm rrr`.
const JSR_MASK: u16 = 0xFFC0;
const JSR: u16 = 0x4E80;
/// `BSR <displacement>`: `0110 0001 dddd dddd`; a displacement byte of 0
/// says a 16-bit displacement word follows.
const BSR_MASK: u16 = 0xFF00;
const BSR: u16 = 0x6100;
/// `RTE`, `RTS`, `RTR`.
const RTE: u16 = 0x4E73;
const RTS: u16 = 0x4E75;
const RTR: u16 = 0x4E77;

/// What the instruction whose first word is `opcode` does to the call
/// stack. A `JSR` is as long as its addressing mode makes it: `(An)` 2
/// bytes; `d16(An)`, `d8(An,Xn)`, an absolute word, `d16(PC)` and
/// `d8(PC,Xn)` 4; an absolute long 6. A `BSR` is 2 bytes, or 4 when its
/// displacement is a word.
pub fn call_step(opcode: u16) -> CallStep {
    if opcode & JSR_MASK == JSR {
        let mode = (opcode >> 3) & 0x7;
        let register = opcode & 0x7;
        let length = match (mode, register) {
            (2, _) => 2,
            (5, _) | (6, _) => 4,
            (7, 0) | (7, 2) | (7, 3) => 4,
            (7, 1) => 6,
            _ => return CallStep::Other,
        };
        return CallStep::Call { length };
    }
    if opcode & BSR_MASK == BSR {
        let length = if opcode & 0x00FF == 0 { 4 } else { 2 };
        return CallStep::Call { length };
    }
    match opcode {
        RTS | RTR | RTE => CallStep::Return,
        _ => CallStep::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_jsr_is_as_long_as_its_addressing_mode() {
        let cases = [
            (0x4E90, 2), // JSR (A0)
            (0x4EAD, 4), // JSR d16(A5)
            (0x4EB3, 4), // JSR d8(A3,Xn)
            (0x4EB8, 4), // JSR abs.W
            (0x4EB9, 6), // JSR abs.L
            (0x4EBA, 4), // JSR d16(PC)
            (0x4EBB, 4), // JSR d8(PC,Xn)
        ];
        for (opcode, length) in cases {
            assert_eq!(call_step(opcode), CallStep::Call { length }, "{opcode:04X}");
        }
    }

    #[test]
    fn a_jsr_with_a_refused_addressing_mode_is_no_call() {
        // Dn, An, (An)+, -(An) and an immediate cannot be a JSR's target.
        for opcode in [0x4E80, 0x4E88, 0x4E98, 0x4EA0, 0x4EBC, 0x4EBF] {
            assert_eq!(call_step(opcode), CallStep::Other, "{opcode:04X}");
        }
    }

    #[test]
    fn a_bsr_is_a_word_long_or_two() {
        assert_eq!(call_step(0x6126), CallStep::Call { length: 2 });
        assert_eq!(call_step(0x61FE), CallStep::Call { length: 2 });
        assert_eq!(call_step(0x6100), CallStep::Call { length: 4 });
    }

    #[test]
    fn rts_rtr_and_rte_return_and_the_rest_does_nothing() {
        for opcode in [0x4E75, 0x4E77, 0x4E73] {
            assert_eq!(call_step(opcode), CallStep::Return, "{opcode:04X}");
        }
        // NOP, BRA, BEQ, JMP (A0), TRAP #0.
        for opcode in [0x4E71, 0x6000, 0x6700, 0x4ED0, 0x4E40] {
            assert_eq!(call_step(opcode), CallStep::Other, "{opcode:04X}");
        }
    }
}
