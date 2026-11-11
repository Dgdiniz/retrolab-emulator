//! An opcode's name and operand, for the disassembler.
//!
//! The GSU reads each opcode in up to four ways: plain, after ALT1, after
//! ALT2, or after both (ALT3). Most opcodes ignore the prefixes; the
//! arithmetic families take a second operation under ALT1 and the opcode's
//! low nibble as an immediate under ALT2; the memory families use them to
//! choose between a load, a store and an immediate. [`decode`] takes the
//! prefixes as they are when the opcode runs. Written from the instruction
//! table of the Super FX documentation.

/// What follows an opcode's name in the disassembly, and so how many bytes
/// the instruction takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operand {
    /// Nothing.
    None,
    /// A branch: the next byte, a signed displacement from the byte after
    /// it.
    Displacement,
    /// LINK: the return address, R15 plus the opcode's low nibble.
    LinkOffset,
    /// The register the opcode's low nibble names, `rN`.
    Register,
    /// The RAM address in that register, `(rN)`.
    Indirect,
    /// That register and the next byte, `rN,#$pp`.
    RegisterByte,
    /// That register and the next word, `rN,#$xxxx`.
    RegisterWord,
    /// That register and the RAM address twice the next byte, `rN,$yyy`.
    RegisterShortAddress,
    /// That register and the RAM address in the next word, `rN,$xxxx`.
    RegisterAddress,
    /// The opcode's low nibble as a number, `#$0n`.
    Nibble,
}

impl Operand {
    /// The instruction's length in bytes, the opcode included.
    pub const fn length(self) -> u8 {
        match self {
            Operand::Displacement | Operand::RegisterByte | Operand::RegisterShortAddress => 2,
            Operand::RegisterWord | Operand::RegisterAddress => 3,
            Operand::None
            | Operand::LinkOffset
            | Operand::Register
            | Operand::Indirect
            | Operand::Nibble => 1,
        }
    }
}

/// An opcode as the disassembler shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decoded {
    /// The mnemonic, lowercase.
    pub mnemonic: &'static str,
    /// What follows it.
    pub operand: Operand,
}

const fn shown(mnemonic: &'static str, operand: Operand) -> Decoded {
    Decoded { mnemonic, operand }
}

/// The conditional branches `$06-$0F`, in opcode order.
const BRANCHES: [&str; 10] = [
    "bge", "blt", "bne", "beq", "bpl", "bmi", "bcc", "bcs", "bvc", "bvs",
];

/// The opcode `opcode` with the prefixes `alt1` and `alt2` set.
pub fn decode(opcode: u8, alt1: bool, alt2: bool) -> Decoded {
    use Operand::*;
    // An arithmetic family: ALT1 names the second operation, ALT2 makes
    // the operand the opcode's nibble.
    let arithmetic = |plain, second| Decoded {
        mnemonic: if alt1 { second } else { plain },
        operand: if alt2 { Nibble } else { Register },
    };
    // A family that only ALT1 changes.
    let by_alt1 = |plain, second, operand| shown(if alt1 { second } else { plain }, operand);

    match opcode {
        0x00 => shown("stop", None),
        0x01 => shown("nop", None),
        0x02 => shown("cache", None),
        0x03 => shown("lsr", None),
        0x04 => shown("rol", None),
        0x05 => shown("bra", Displacement),
        0x06..=0x0f => shown(BRANCHES[usize::from(opcode - 0x06)], Displacement),
        0x10..=0x1f => shown("to", Register),
        0x20..=0x2f => shown("with", Register),
        0x30..=0x3b => by_alt1("stw", "stb", Indirect),
        0x3c => shown("loop", None),
        0x3d => shown("alt1", None),
        0x3e => shown("alt2", None),
        0x3f => shown("alt3", None),
        0x40..=0x4b => by_alt1("ldw", "ldb", Indirect),
        0x4c => by_alt1("plot", "rpix", None),
        0x4d => shown("swap", None),
        0x4e => by_alt1("color", "cmode", None),
        0x4f => shown("not", None),
        0x50..=0x5f => arithmetic("add", "adc"),
        // CMP (ALT3) compares with a register; ALT2 alone subtracts the
        // nibble.
        0x60..=0x6f if alt1 && alt2 => shown("cmp", Register),
        0x60..=0x6f => arithmetic("sub", "sbc"),
        0x70 => shown("merge", None),
        0x71..=0x7f => arithmetic("and", "bic"),
        0x80..=0x8f => arithmetic("mult", "umult"),
        0x90 => shown("sbk", None),
        0x91..=0x94 => shown("link", LinkOffset),
        0x95 => shown("sex", None),
        0x96 => by_alt1("asr", "div2", None),
        0x97 => shown("ror", None),
        0x98..=0x9d => by_alt1("jmp", "ljmp", Register),
        0x9e => shown("lob", None),
        0x9f => by_alt1("fmult", "lmult", None),
        0xa0..=0xaf => match (alt1, alt2) {
            (false, false) => shown("ibt", RegisterByte),
            // ALT1 makes it the load, with ALT2 too (ALT3), as the GSU runs
            // it: the address byte follows either way.
            (true, _) => shown("lms", RegisterShortAddress),
            (false, true) => shown("sms", RegisterShortAddress),
        },
        0xb0..=0xbf => shown("from", Register),
        0xc0 => shown("hib", None),
        0xc1..=0xcf => arithmetic("or", "xor"),
        0xd0..=0xde => shown("inc", Register),
        0xdf => match (alt1, alt2) {
            (_, false) => shown("getc", None),
            (false, true) => shown("ramb", None),
            (true, true) => shown("romb", None),
        },
        0xe0..=0xee => shown("dec", Register),
        0xef => match (alt1, alt2) {
            (false, false) => shown("getb", None),
            (true, false) => shown("getbh", None),
            (false, true) => shown("getbl", None),
            (true, true) => shown("getbs", None),
        },
        0xf0..=0xff => match (alt1, alt2) {
            (false, false) => shown("iwt", RegisterWord),
            // ALT1 makes it the load, with ALT2 too (ALT3), as the GSU runs it.
            (true, _) => shown("lm", RegisterAddress),
            (false, true) => shown("sm", RegisterAddress),
        },
    }
}
