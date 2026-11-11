//! Unit tests for the GSU: reset state, the decoder (all 256 opcodes x 4
//! prefix states), the disassembler, and the execution of every
//! instruction family. The expected values were taken from bsnes-plus's
//! behaviour.

use crate::decode::{decode, Decoded, Operand};
use crate::disasm::{disassemble_at, disassemble_one};
use crate::registers::{config, plot_options, screen_mode, status};
use crate::Gsu;
use retro_core::cpu::{Addressable, Resettable};

/// Flat 8 MB GSU bus for the executor tests. 8 MB spans both the program /
/// ROM window (`$00-5f`) and the Game Pak RAM window the buffers address
/// (`0x700000 + (RAMBR << 16) + addr`), so a store lands somewhere distinct
/// from the code it ran from. Addresses wrap with `& 0x7f_ffff`.
struct FlatBus {
    mem: Vec<u8>,
}

impl FlatBus {
    fn new(bytes: &[u8]) -> Self {
        let mut mem = vec![0u8; 0x80_0000];
        mem[..bytes.len()].copy_from_slice(bytes);
        Self { mem }
    }
}

impl Addressable for FlatBus {
    fn read(&mut self, addr: u32) -> u8 {
        self.mem[(addr as usize) & 0x7F_FFFF]
    }
    fn write(&mut self, addr: u32, value: u8) {
        self.mem[(addr as usize) & 0x7F_FFFF] = value;
    }
}

// -- Reset ------------------------------------------------------------------

#[test]
fn reset_sets_power_on_state() {
    let gsu = Gsu::new();
    assert_eq!(gsu.version, 0x04, "version code register reads $04");
    assert_eq!(gsu.next_opcode, 0x01, "pipeline primed with NOP");
    assert_eq!(
        gsu.clock_divider, 2,
        "default clock divider is 10.7 MHz (/2)"
    );
    assert_eq!(gsu.status, 0, "status flags cleared");
    assert_eq!(gsu.registers, [0; 16], "all registers cleared");
    assert!(!gsu.is_running(), "GSU is stopped after reset");
    assert!(!gsu.cache.valid.iter().any(|&v| v), "cache invalidated");
}

#[test]
fn reset_restores_dirty_state() {
    let mut gsu = Gsu::new();
    gsu.registers[3] = 0xBEEF;
    gsu.status = 0xFFFF;
    gsu.cache.valid[7] = true;
    gsu.set_pc(0x1234);

    gsu.reset();

    assert_eq!(gsu.registers[3], 0);
    assert_eq!(gsu.pc(), 0);
    assert_eq!(gsu.status, 0);
    assert!(!gsu.cache.valid[7]);
    assert_eq!(gsu.version, 0x04);
}

#[test]
fn status_and_screen_mode_accessors() {
    let mut gsu = Gsu::new();
    gsu.status = status::GO | status::ALT1 | status::IRQ;
    assert!(gsu.is_running());
    assert!(gsu.alt1());
    assert!(!gsu.alt2());
    assert!(gsu.irq_pending());

    gsu.screen_mode = screen_mode::ROM_BUS | screen_mode::RAM_BUS;
    assert!(gsu.owns_rom_bus());
    assert!(gsu.owns_ram_bus());
}

// -- SCMR height packing ----------------------------------------------------

#[test]
fn screen_height_recovers_split_bits() {
    // ht low bit = SCMR bit 2, high bit = SCMR bit 5.
    assert_eq!(screen_mode::height(0x00), 0);
    assert_eq!(screen_mode::height(0x04), 1); // low bit only
    assert_eq!(screen_mode::height(0x20), 2); // high bit only
    assert_eq!(screen_mode::height(0x24), 3); // both
}

// -- Decode: exhaustive sweep -----------------------------------------------

/// Every opcode must decode to a real mnemonic (never `ILL`) under all four
/// prefix states, and the operand length must be 1..=3.
#[test]
fn decode_never_illegal_and_length_bounded() {
    for opcode in 0u16..=0xFF {
        for &(a1, a2) in &[(false, false), (true, false), (false, true), (true, true)] {
            let decoded = decode(opcode as u8, a1, a2);
            assert!(
                !decoded.mnemonic.is_empty(),
                "opcode {opcode:#04x} alt1={a1} alt2={a2} has no mnemonic"
            );
            let len = decoded.operand.length();
            assert!(
                (1..=3).contains(&len),
                "opcode {opcode:#04x} length {len} out of range"
            );
        }
    }
}

/// Spot-check the ALT-prefix remapping (the names are bsnes-plus's; under
/// ALT3 the memory forms of $A0 and $F0 are what the GSU runs, the loads).
#[test]
fn decode_alt_prefix_remapping() {
    // $50: ADD -> ADC (alt1) -> ADD #imm (alt2) -> ADC #imm (alt3).
    assert_eq!(decode(0x50, false, false), shown("add", Operand::Register));
    assert_eq!(decode(0x50, true, false), shown("adc", Operand::Register));
    assert_eq!(decode(0x50, false, true), shown("add", Operand::Nibble));
    assert_eq!(decode(0x50, true, true), shown("adc", Operand::Nibble));

    // $4C: PLOT -> RPIX (alt1).
    assert_eq!(decode(0x4c, false, false), shown("plot", Operand::None));
    assert_eq!(decode(0x4c, true, false), shown("rpix", Operand::None));

    // $9F: FMULT -> LMULT (alt1).
    assert_eq!(decode(0x9f, false, false), shown("fmult", Operand::None));
    assert_eq!(decode(0x9f, true, false), shown("lmult", Operand::None));

    // $F0: IWT -> LM (alt1) -> SM (alt2) -> LM (alt3: ALT1 makes it the load).
    assert_eq!(
        decode(0xf0, false, false),
        shown("iwt", Operand::RegisterWord)
    );
    assert_eq!(
        decode(0xf0, true, false),
        shown("lm", Operand::RegisterAddress)
    );
    assert_eq!(
        decode(0xf0, false, true),
        shown("sm", Operand::RegisterAddress)
    );
    assert_eq!(
        decode(0xf0, true, true),
        shown("lm", Operand::RegisterAddress)
    );

    // $A0: IBT -> LMS (alt1) -> SMS (alt2) -> LMS (alt3), the address byte
    // following under every prefix.
    assert_eq!(
        decode(0xa0, false, false),
        shown("ibt", Operand::RegisterByte)
    );
    assert_eq!(
        decode(0xa0, true, false),
        shown("lms", Operand::RegisterShortAddress)
    );
    assert_eq!(
        decode(0xa0, false, true),
        shown("sms", Operand::RegisterShortAddress)
    );
    assert_eq!(
        decode(0xa0, true, true),
        shown("lms", Operand::RegisterShortAddress)
    );

    // $EF: GETB -> GETBH (alt1) -> GETBL (alt2) -> GETBS (alt3).
    assert_eq!(decode(0xef, false, false).mnemonic, "getb");
    assert_eq!(decode(0xef, true, false).mnemonic, "getbh");
    assert_eq!(decode(0xef, false, true).mnemonic, "getbl");
    assert_eq!(decode(0xef, true, true).mnemonic, "getbs");

    // $DF: GETC -> RAMB (alt2) -> ROMB (alt3).
    assert_eq!(decode(0xdf, false, false).mnemonic, "getc");
    assert_eq!(decode(0xdf, false, true).mnemonic, "ramb");
    assert_eq!(decode(0xdf, true, true).mnemonic, "romb");
}

/// The prefix / control opcodes must be prefix-independent.
#[test]
fn decode_fixed_opcodes() {
    assert_eq!(decode(0x00, false, false).mnemonic, "stop");
    assert_eq!(decode(0x01, true, true).mnemonic, "nop");
    assert_eq!(decode(0x3c, true, true).mnemonic, "loop");
    assert_eq!(decode(0x3d, false, false).mnemonic, "alt1");
    assert_eq!(decode(0x3e, false, false).mnemonic, "alt2");
    assert_eq!(decode(0x3f, false, false).mnemonic, "alt3");
    // Register blocks are fixed regardless of prefix.
    assert_eq!(decode(0x15, true, true).mnemonic, "to");
    assert_eq!(decode(0x2a, true, true).mnemonic, "with");
    assert_eq!(decode(0xb7, true, true).mnemonic, "from");
    assert_eq!(decode(0xd5, true, true).mnemonic, "inc");
    assert_eq!(decode(0xe5, true, true).mnemonic, "dec");
}

// -- Disassembler -----------------------------------------------------------

#[test]
fn disasm_relative_branch_resolves_target() {
    // bra +$10 at $0000 -> target $0000 + 2 + $10 = $0012.
    let mut bus = FlatBus::new(&[0x05, 0x10]);
    let d = disassemble_one(&mut bus, 0x0000, false, false);
    assert_eq!(d.mnemonic, "bra");
    assert_eq!(d.operands, "$0012");
    assert_eq!(d.bytes_consumed, 2);
    assert_eq!(d.bytes, vec![0x05, 0x10]);
}

#[test]
fn disasm_relative_branch_negative_offset() {
    // bra -2 at $0100 -> $0100 + 2 - 2 = $0100.
    let mut bus = FlatBus::new(&[0; 4]);
    bus.write(0x0100, 0x05);
    bus.write(0x0101, 0xFE);
    let d = disassemble_one(&mut bus, 0x0100, false, false);
    assert_eq!(d.operands, "$0100");
}

#[test]
fn disasm_register_and_indirect() {
    // to r5
    let mut bus = FlatBus::new(&[0x15]);
    let d = disassemble_one(&mut bus, 0, false, false);
    assert_eq!((d.mnemonic.as_str(), d.operands.as_str()), ("to", "r5"));

    // ldw (r3)  ($43, no alt)
    let mut bus = FlatBus::new(&[0x43]);
    let d = disassemble_one(&mut bus, 0, false, false);
    assert_eq!((d.mnemonic.as_str(), d.operands.as_str()), ("ldw", "(r3)"));

    // ldb (r3)  ($43, alt1)
    let mut bus = FlatBus::new(&[0x43]);
    let d = disassemble_one(&mut bus, 0, true, false);
    assert_eq!((d.mnemonic.as_str(), d.operands.as_str()), ("ldb", "(r3)"));
}

#[test]
fn disasm_immediate_forms() {
    // ibt r0,#$05  ($A0 $05)
    let mut bus = FlatBus::new(&[0xa0, 0x05]);
    let d = disassemble_one(&mut bus, 0, false, false);
    assert_eq!(
        (d.mnemonic.as_str(), d.operands.as_str()),
        ("ibt", "r0,#$05")
    );
    assert_eq!(d.bytes_consumed, 2);

    // iwt r1,#$1234  ($F1 $34 $12), little-endian bytes, big-endian print.
    let mut bus = FlatBus::new(&[0xf1, 0x34, 0x12]);
    let d = disassemble_one(&mut bus, 0, false, false);
    assert_eq!(
        (d.mnemonic.as_str(), d.operands.as_str()),
        ("iwt", "r1,#$1234")
    );
    assert_eq!(d.bytes_consumed, 3);

    // adc #$0a  ($5A alt3), ImmediateShort takes the low nibble.
    let mut bus = FlatBus::new(&[0x5a]);
    let d = disassemble_one(&mut bus, 0, true, true);
    assert_eq!((d.mnemonic.as_str(), d.operands.as_str()), ("adc", "#$0A"));
}

#[test]
fn disasm_mem_forms() {
    // lm r2,$1234  ($F2 $34 $12, alt1), RAM absolute.
    let mut bus = FlatBus::new(&[0xf2, 0x34, 0x12]);
    let d = disassemble_one(&mut bus, 0, true, false);
    assert_eq!(
        (d.mnemonic.as_str(), d.operands.as_str()),
        ("lm", "r2,$1234")
    );

    // lms r4,$00A ($A4 $05, alt1), RegDirect: op1 << 1.
    let mut bus = FlatBus::new(&[0xa4, 0x05]);
    let d = disassemble_one(&mut bus, 0, true, false);
    assert_eq!(
        (d.mnemonic.as_str(), d.operands.as_str()),
        ("lms", "r4,$00A")
    );
}

#[test]
fn disasm_implied_has_no_operand() {
    let mut bus = FlatBus::new(&[0x4c]);
    let d = disassemble_one(&mut bus, 0, false, false);
    assert_eq!(d.mnemonic, "plot");
    assert_eq!(d.operands, "");
    assert_eq!(d.bytes_consumed, 1);
}

#[test]
fn disassemble_at_joins_line() {
    let mut bus = FlatBus::new(&[0xf1, 0x34, 0x12]);
    let (line, len) = disassemble_at(&mut bus, 0, false, false);
    assert_eq!(line, "iwt r1,#$1234");
    assert_eq!(len, 3);

    let mut bus = FlatBus::new(&[0x01]);
    let (line, len) = disassemble_at(&mut bus, 0, false, false);
    assert_eq!(line, "nop");
    assert_eq!(len, 1);
}

fn shown(mnemonic: &'static str, operand: Operand) -> Decoded {
    Decoded { mnemonic, operand }
}

// -- Execution: harness -----------------------------------------------------

/// Load `program` at address 0 on a flat bus, build a running GSU (`GO`
/// set), and prime the pipeline so the next [`Gsu::step`] executes
/// `program[0]`. Priming fills cache block 0, so the first `step` fetch is
/// a hit.
fn running(program: &[u8]) -> (Gsu, FlatBus) {
    let mut bus = FlatBus::new(program);
    let mut gsu = Gsu::new();
    gsu.status |= status::GO;
    gsu.prime(&mut bus, 0);
    (gsu, bus)
}

// -- Execution: idle gate ---------------------------------------------------

#[test]
fn step_idles_six_clocks_when_stopped() {
    let mut bus = FlatBus::new(&[0xd0]); // inc r0, must NOT run while stopped
    let mut gsu = Gsu::new(); // GO clear after reset
    let clocks = gsu.step(&mut bus);
    assert_eq!(clocks, 6, "a stopped GSU idles for 6 clocks");
    assert_eq!(gsu.registers[0], 0, "no instruction executes while stopped");
    assert_eq!(gsu.pc(), 0, "PC does not advance while stopped");
}

// -- Execution: arithmetic flags --------------------------------------------

#[test]
fn add_sets_carry_and_zero_on_wraparound() {
    let (mut gsu, mut bus) = running(&[0x52]); // add r2 -> r0 = r0 + r2
    gsu.registers[0] = 0xFFFF;
    gsu.registers[2] = 0x0001;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0x0000);
    assert!(gsu.flag(status::CARRY), "carry out of bit 15");
    assert!(gsu.flag(status::ZERO), "result is zero");
    assert!(!gsu.flag(status::SIGN));
    assert!(!gsu.flag(status::OVERFLOW));
}

#[test]
fn add_sets_overflow_and_sign() {
    let (mut gsu, mut bus) = running(&[0x52]);
    gsu.registers[0] = 0x7FFF;
    gsu.registers[2] = 0x0001;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0x8000);
    assert!(gsu.flag(status::SIGN), "bit 15 set");
    assert!(gsu.flag(status::OVERFLOW), "signed overflow + -> -");
    assert!(!gsu.flag(status::CARRY));
    assert!(!gsu.flag(status::ZERO));
}

#[test]
fn adc_adds_carry_in_under_alt1() {
    let (mut gsu, mut bus) = running(&[0x52]); // adc r2 (alt1)
    gsu.status |= status::ALT1 | status::CARRY;
    gsu.registers[0] = 0x0001;
    gsu.registers[2] = 0x0001;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0x0003, "1 + 1 + carry-in");
    assert!(!gsu.flag(status::ALT1), "ALT1 retired after the op");
}

#[test]
fn alt1_opcode_then_adc_carry_chain() {
    // alt1 ($3d) then adc r2 ($52): the prefix opcode arms ALT1, the adc
    // consumes it, and clearing the prefixes retires it.
    let (mut gsu, mut bus) = running(&[0x3d, 0x52]);
    gsu.status |= status::CARRY;
    gsu.registers[0] = 0x0001;
    gsu.registers[2] = 0x0001;
    gsu.step(&mut bus); // alt1
    assert!(gsu.alt1(), "alt1 opcode latched the prefix");
    gsu.step(&mut bus); // adc r2
    assert_eq!(gsu.registers[0], 0x0003);
    assert!(!gsu.alt1(), "prefix retired after the adc");
}

#[test]
fn sub_borrow_and_carry_semantics() {
    // 5 - 3 = 2, no borrow -> carry set (65xx convention).
    let (mut gsu, mut bus) = running(&[0x62]); // sub r2
    gsu.registers[0] = 0x0005;
    gsu.registers[2] = 0x0003;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0x0002);
    assert!(gsu.flag(status::CARRY), "no borrow -> carry set");
    assert!(!gsu.flag(status::SIGN));
    assert!(!gsu.flag(status::ZERO));

    // 0 - 1 = -1, borrow -> carry clear, sign set.
    let (mut gsu, mut bus) = running(&[0x62]);
    gsu.registers[0] = 0x0000;
    gsu.registers[2] = 0x0001;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0xFFFF);
    assert!(!gsu.flag(status::CARRY), "borrow -> carry clear");
    assert!(gsu.flag(status::SIGN));
}

#[test]
fn sbc_subtracts_borrow_under_alt1() {
    // 5 - 3 - (borrow=!cy) with cy=0 -> 1.
    let (mut gsu, mut bus) = running(&[0x62]); // sbc r2 (alt1)
    gsu.status |= status::ALT1;
    gsu.registers[0] = 0x0005;
    gsu.registers[2] = 0x0003;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0x0001, "5 - 3 - 1");
    assert!(gsu.flag(status::CARRY));
}

#[test]
fn cmp_sets_flags_without_writing() {
    let (mut gsu, mut bus) = running(&[0x62]); // cmp r2 (alt3)
    gsu.status |= status::ALT1 | status::ALT2;
    gsu.registers[0] = 0x0005;
    gsu.registers[2] = 0x0005;
    gsu.step(&mut bus);
    assert_eq!(
        gsu.registers[0], 0x0005,
        "cmp does not write the destination"
    );
    assert!(gsu.flag(status::ZERO), "equal -> zero");
    assert!(gsu.flag(status::CARRY), "no borrow -> carry");
    assert!(!gsu.flag(status::SIGN));
}

#[test]
fn add_immediate_uses_low_nibble_under_alt2() {
    let (mut gsu, mut bus) = running(&[0x55]); // add #5 (alt2)
    gsu.status |= status::ALT2;
    gsu.registers[0] = 0x0010;
    gsu.step(&mut bus);
    assert_eq!(
        gsu.registers[0], 0x0015,
        "immediate operand is the opcode nibble"
    );
}

// -- Execution: logic -------------------------------------------------------

#[test]
fn and_bic_logic() {
    let (mut gsu, mut bus) = running(&[0x72]); // and r2
    gsu.registers[0] = 0xFF0F;
    gsu.registers[2] = 0x0FF0;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0x0F00);

    // bic (alt1) complements the operand: r0 & ~r2.
    let (mut gsu, mut bus) = running(&[0x72]);
    gsu.status |= status::ALT1;
    gsu.registers[0] = 0xFFFF;
    gsu.registers[2] = 0x000F;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0xFFF0);
    assert!(gsu.flag(status::SIGN));
}

#[test]
fn or_xor_logic() {
    let (mut gsu, mut bus) = running(&[0xc2]); // or r2
    gsu.registers[0] = 0x00F0;
    gsu.registers[2] = 0x0F00;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0x0FF0);

    let (mut gsu, mut bus) = running(&[0xc2]); // xor r2 (alt1)
    gsu.status |= status::ALT1;
    gsu.registers[0] = 0xFFFF;
    gsu.registers[2] = 0x0F0F;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0xF0F0);
    assert!(gsu.flag(status::SIGN));
}

#[test]
fn not_complements_source() {
    let (mut gsu, mut bus) = running(&[0x4f]);
    gsu.registers[0] = 0x0F0F;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0xF0F0);
    assert!(gsu.flag(status::SIGN));

    let (mut gsu, mut bus) = running(&[0x4f]);
    gsu.registers[0] = 0xFFFF;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0x0000);
    assert!(gsu.flag(status::ZERO));
}

// -- Execution: shift / rotate ----------------------------------------------

#[test]
fn lsr_shifts_low_bit_into_carry() {
    let (mut gsu, mut bus) = running(&[0x03]);
    gsu.registers[0] = 0x0003;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0x0001);
    assert!(gsu.flag(status::CARRY), "lost bit 0 -> carry");
    assert!(!gsu.flag(status::SIGN), "lsr always clears the sign bit");
}

#[test]
fn rol_rotates_through_carry() {
    let (mut gsu, mut bus) = running(&[0x04]);
    gsu.registers[0] = 0x8001;
    // carry-in clear
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0x0002, "(0x8001 << 1) | cy=0");
    assert!(gsu.flag(status::CARRY), "old bit 15 -> carry");

    let (mut gsu, mut bus) = running(&[0x04]);
    gsu.status |= status::CARRY;
    gsu.registers[0] = 0x0001;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0x0003, "(0x0001 << 1) | cy=1");
    assert!(!gsu.flag(status::CARRY));
}

#[test]
fn ror_rotates_through_carry() {
    let (mut gsu, mut bus) = running(&[0x97]);
    gsu.status |= status::CARRY;
    gsu.registers[0] = 0x0002;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0x8001, "(cy=1 << 15) | (0x0002 >> 1)");
    assert!(!gsu.flag(status::CARRY), "old bit 0 -> carry (was 0)");
    assert!(gsu.flag(status::SIGN));
}

#[test]
fn asr_is_arithmetic_and_div2_corrects_minus_one() {
    // asr of -2 -> -1 (sign preserved).
    let (mut gsu, mut bus) = running(&[0x96]);
    gsu.registers[0] = 0xFFFE;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0xFFFF);
    assert!(gsu.flag(status::SIGN));

    // div2 (alt1) of -1 -> 0, where the shift leaves -1.
    let (mut gsu, mut bus) = running(&[0x96]);
    gsu.status |= status::ALT1;
    gsu.registers[0] = 0xFFFF;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0x0000);
    assert!(gsu.flag(status::CARRY), "low bit of -1 -> carry");
    assert!(gsu.flag(status::ZERO));

    // asr (alt0) of -1 -> -1 (no correction).
    let (mut gsu, mut bus) = running(&[0x96]);
    gsu.registers[0] = 0xFFFF;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0xFFFF);

    // div2 of -3 -> -2, as the shift gives: -1 is the only value it changes,
    // so it does not round toward zero.
    let (mut gsu, mut bus) = running(&[0x96]);
    gsu.status |= status::ALT1;
    gsu.registers[0] = 0xFFFD;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0xFFFE);
    assert!(gsu.flag(status::CARRY), "low bit of -3 -> carry");
}

// -- Execution: inc / dec ---------------------------------------------------

#[test]
fn inc_dec_registers_with_flags() {
    let (mut gsu, mut bus) = running(&[0xd5]); // inc r5
    gsu.registers[5] = 0xFFFF;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[5], 0x0000);
    assert!(gsu.flag(status::ZERO));

    let (mut gsu, mut bus) = running(&[0xd5]);
    gsu.registers[5] = 0x7FFF;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[5], 0x8000);
    assert!(gsu.flag(status::SIGN));

    let (mut gsu, mut bus) = running(&[0xe5]); // dec r5
    gsu.registers[5] = 0x0001;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[5], 0x0000);
    assert!(gsu.flag(status::ZERO));

    let (mut gsu, mut bus) = running(&[0xe5]);
    gsu.registers[5] = 0x0000;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[5], 0xFFFF);
    assert!(gsu.flag(status::SIGN));
}

// -- Execution: byte ops ----------------------------------------------------

#[test]
fn sex_sign_extends_low_byte() {
    let (mut gsu, mut bus) = running(&[0x95]);
    gsu.registers[0] = 0x0080; // low byte 0x80 = -128
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0xFF80);
    assert!(gsu.flag(status::SIGN));

    let (mut gsu, mut bus) = running(&[0x95]);
    gsu.registers[0] = 0x007F;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0x007F);
    assert!(!gsu.flag(status::SIGN));
}

#[test]
fn lob_hib_extract_bytes() {
    let (mut gsu, mut bus) = running(&[0x9e]); // lob
    gsu.registers[0] = 0x12FF;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0x00FF);
    assert!(gsu.flag(status::SIGN), "bit 7 of the byte -> sign");

    let (mut gsu, mut bus) = running(&[0xc0]); // hib
    gsu.registers[0] = 0x80FF;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0x0080);
    assert!(gsu.flag(status::SIGN));
}

#[test]
fn swap_exchanges_bytes() {
    let (mut gsu, mut bus) = running(&[0x4d]);
    gsu.registers[0] = 0x1234;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0x3412);
}

#[test]
fn merge_packs_high_bytes_and_tests_masks() {
    let (mut gsu, mut bus) = running(&[0x70]);
    gsu.registers[7] = 0xAB00;
    gsu.registers[8] = 0xCD00;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0xABCD, "(r7 & 0xff00) | (r8 >> 8)");
    // Every mask overlaps 0xABCD, so all four flags latch.
    assert!(gsu.flag(status::OVERFLOW));
    assert!(gsu.flag(status::SIGN));
    assert!(gsu.flag(status::CARRY));
    assert!(gsu.flag(status::ZERO));
}

// -- Execution: transfers (to / from / with / move / moves) -----------------

#[test]
fn from_to_prefixes_select_source_and_dest() {
    // from r3; to r5; add r2 -> r5 = r3 + r2, all prefix-transparent.
    let (mut gsu, mut bus) = running(&[0xb3, 0x15, 0x52]);
    gsu.registers[3] = 0x0010;
    gsu.registers[2] = 0x0005;
    gsu.step(&mut bus); // from r3
    gsu.step(&mut bus); // to r5
    gsu.step(&mut bus); // add r2
    assert_eq!(gsu.registers[5], 0x0015);
    assert_eq!(gsu.registers[3], 0x0010, "source register untouched");
}

#[test]
fn with_makes_source_and_dest_the_same() {
    // with r1; add r2 -> r1 = r1 + r2.
    let (mut gsu, mut bus) = running(&[0x21, 0x52]);
    gsu.registers[1] = 0x0003;
    gsu.registers[2] = 0x0004;
    gsu.step(&mut bus); // with r1
    gsu.step(&mut bus); // add r2
    assert_eq!(gsu.registers[1], 0x0007);
}

#[test]
fn move_copies_when_with_flag_set() {
    // with r1; to r2 -> move r1 into r2.
    let (mut gsu, mut bus) = running(&[0x21, 0x12]);
    gsu.registers[1] = 0xBEEF;
    gsu.step(&mut bus); // with r1
    gsu.step(&mut bus); // move r1 -> r2
    assert_eq!(gsu.registers[2], 0xBEEF);
}

#[test]
fn moves_copies_and_sets_flags() {
    // with r1; from r2 -> moves r2 into r1 (dr = dreg = 1) + flags.
    let (mut gsu, mut bus) = running(&[0x21, 0xb2]);
    gsu.registers[2] = 0x0080;
    gsu.step(&mut bus); // with r1
    gsu.step(&mut bus); // moves
    assert_eq!(gsu.registers[1], 0x0080);
    assert!(gsu.flag(status::OVERFLOW), "moves sets OV from bit 7");
    assert!(!gsu.flag(status::SIGN));
}

// -- Execution: an instruction retires the prefix state ---------------------

#[test]
fn an_instruction_clears_the_prefixes_and_selectors() {
    let (mut gsu, mut bus) = running(&[0x01]); // nop
    gsu.source = 3;
    gsu.destination = 5;
    gsu.status |= status::ALT1 | status::ALT2 | status::WITH;
    gsu.step(&mut bus);
    assert_eq!(gsu.source, 0);
    assert_eq!(gsu.destination, 0);
    assert!(!gsu.alt1());
    assert!(!gsu.alt2());
    assert_eq!(gsu.status & status::WITH, 0, "WITH flag cleared");
}

// -- Execution: pipeline / auto-increment -----------------------------------

#[test]
fn sequential_ops_auto_increment_pc() {
    let (mut gsu, mut bus) = running(&[0x01, 0x01, 0x01]); // three nops
    assert_eq!(gsu.pc(), 1, "primed one ahead of the entry byte");
    gsu.step(&mut bus);
    assert_eq!(gsu.pc(), 2);
    gsu.step(&mut bus);
    assert_eq!(gsu.pc(), 3);
}

#[test]
fn iwt_loads_word_and_computed_jump_suppresses_increment() {
    // iwt r15,#$1234 ($ff): writing R15 is a computed jump, no auto-inc.
    let (mut gsu, mut bus) = running(&[0xff, 0x34, 0x12]);
    gsu.step(&mut bus);
    assert_eq!(gsu.pc(), 0x1234, "PC took the loaded value, not +1");
    assert!(gsu.jumped);
}

#[test]
fn iwt_loads_ordinary_register() {
    let (mut gsu, mut bus) = running(&[0xf1, 0x34, 0x12]); // iwt r1,#$1234
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[1], 0x1234);
    assert_eq!(gsu.pc(), 4, "3-byte op leaves PC one past the operands");
}

#[test]
fn ibt_loads_sign_extended_byte() {
    let (mut gsu, mut bus) = running(&[0xa0, 0x80]); // ibt r0,#$80
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0xFF80, "byte sign-extended to 16 bits");
    assert_eq!(gsu.pc(), 3);
}

#[test]
fn computed_jump_via_to_r15() {
    // to r15; add r2 -> r15 = r0 + r2, suppressing auto-inc.
    let (mut gsu, mut bus) = running(&[0x1f, 0x52]);
    gsu.registers[0] = 0x1000;
    gsu.registers[2] = 0x0234;
    gsu.step(&mut bus); // to r15
    gsu.step(&mut bus); // add r2 (dest = r15)
    assert_eq!(gsu.pc(), 0x1234);
    assert!(gsu.jumped);
}

// -- Execution: branches (delay slot) ---------------------------------------

#[test]
fn bra_targets_relative_to_delay_slot() {
    // bra +$10 at 0 -> target = (0 + 2) + 0x10 = 0x12.
    let (mut gsu, mut bus) = running(&[0x05, 0x10]);
    gsu.step(&mut bus);
    assert_eq!(gsu.pc(), 0x12);
    assert!(gsu.jumped);
}

#[test]
fn bra_negative_offset_wraps_backward() {
    // bra -2 at 0 -> (0 + 2) - 2 = 0.
    let (mut gsu, mut bus) = running(&[0x05, 0xFE]);
    gsu.step(&mut bus);
    assert_eq!(gsu.pc(), 0x0000);
}

#[test]
fn delay_slot_runs_then_branch_target() {
    // 0: bra +2 -> target 4 ; 2: inc r0 (delay slot) ; 3: inc r1 (skipped)
    // 4: inc r2 (target)
    let (mut gsu, mut bus) = running(&[0x05, 0x02, 0xd0, 0xd1, 0xd2]);
    gsu.step(&mut bus); // bra: PC -> 4, delay-slot inc r0 prefetched
    assert_eq!(gsu.pc(), 4);
    gsu.step(&mut bus); // delay slot: inc r0
    gsu.step(&mut bus); // target: inc r2
    assert_eq!(gsu.registers[0], 1, "delay-slot instruction executed");
    assert_eq!(
        gsu.registers[1], 0,
        "instruction after the delay slot was skipped"
    );
    assert_eq!(gsu.registers[2], 1, "branch target executed");
}

#[test]
fn conditional_branch_taken_and_not_taken() {
    // bne ($08) taken when Z clear.
    let (mut gsu, mut bus) = running(&[0x08, 0x10]);
    gsu.step(&mut bus);
    assert_eq!(gsu.pc(), 0x12, "bne taken -> branch to delay slot + offset");

    // bne not taken when Z set -> falls through past the 2-byte branch.
    let (mut gsu, mut bus) = running(&[0x08, 0x7F]);
    gsu.status |= status::ZERO;
    gsu.step(&mut bus);
    assert_eq!(
        gsu.pc(),
        3,
        "untaken branch still consumes the displacement"
    );
}

#[test]
fn branch_conditions_match_flags() {
    // bge ($06): taken when S == OV. Set neither (both false) -> taken.
    let (mut gsu, mut bus) = running(&[0x06, 0x04]);
    gsu.step(&mut bus);
    assert_eq!(gsu.pc(), 0x06, "bge taken when S == OV");

    // blt ($07): taken when S != OV. Set S only -> taken.
    let (mut gsu, mut bus) = running(&[0x07, 0x04]);
    gsu.status |= status::SIGN;
    gsu.step(&mut bus);
    assert_eq!(gsu.pc(), 0x06, "blt taken when S != OV");
}

// -- Execution: loop --------------------------------------------------------

#[test]
fn loop_branches_to_r13_until_counter_zero() {
    // Counter > 1: decrement and branch to R13.
    let (mut gsu, mut bus) = running(&[0x3c]); // loop
    gsu.registers[12] = 2;
    gsu.registers[13] = 0x0050;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[12], 1);
    assert_eq!(gsu.pc(), 0x0050, "loop back to R13 while non-zero");
    assert!(!gsu.flag(status::ZERO));

    // Counter reaches zero: no branch, fall through.
    let (mut gsu, mut bus) = running(&[0x3c]);
    gsu.registers[12] = 1;
    gsu.registers[13] = 0x0050;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[12], 0);
    assert!(gsu.flag(status::ZERO));
    assert_eq!(
        gsu.pc(),
        2,
        "counter hit zero -> no branch, PC falls through"
    );
}

// -- Execution: jmp / ljmp / link -------------------------------------------

#[test]
fn jmp_is_a_computed_jump() {
    let (mut gsu, mut bus) = running(&[0x98]); // jmp r8
    gsu.registers[8] = 0x0456;
    gsu.step(&mut bus);
    assert_eq!(gsu.pc(), 0x0456);
    assert!(gsu.jumped);
}

#[test]
fn ljmp_sets_bank_pc_and_flushes_cache() {
    let (mut gsu, mut bus) = running(&[0x98]); // ljmp r8 (alt1)
    gsu.status |= status::ALT1;
    gsu.source = 1; // offset comes from sr()
    gsu.registers[1] = 0x1234;
    gsu.registers[8] = 0x05; // bank
    gsu.cache.valid[0] = true;
    gsu.step(&mut bus);
    assert_eq!(gsu.program_bank, 0x05, "program bank <- rN");
    assert_eq!(gsu.pc(), 0x1234, "PC <- source register");
    assert_eq!(gsu.cache_base, 0x1230, "cache re-anchored to PC & 0xfff0");
    assert!(
        !gsu.cache.valid.iter().any(|&v| v),
        "ljmp flushes the cache"
    );
}

#[test]
fn link_stores_return_address() {
    let (mut gsu, mut bus) = running(&[0x92]); // link #2
                                               // During execution R15 points at the delay slot (entry + 1 = 1).
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[11], 1 + 2, "R11 = R15 + N");
}

// -- Execution: cache -------------------------------------------------------

#[test]
fn fetch_miss_fills_a_line_hit_is_cheap() {
    let program: Vec<u8> = (0..32u8).collect();
    let mut bus = FlatBus::new(&program);
    let mut gsu = Gsu::new(); // divider = 2 -> mem speed 6, cache speed 2

    gsu.clocks = 0;
    let b0 = gsu.fetch(&mut bus, 0);
    assert_eq!(b0, 0, "first byte of the block");
    assert_eq!(gsu.clocks, 16 * 6, "cold block fills 16 bytes @ 6 clocks");
    assert!(gsu.cache.valid[0]);

    gsu.clocks = 0;
    let b1 = gsu.fetch(&mut bus, 1);
    assert_eq!(b1, 1);
    assert_eq!(gsu.clocks, 2, "hit in a valid block costs the divider");

    gsu.clocks = 0;
    gsu.fetch(&mut bus, 16);
    assert_eq!(gsu.clocks, 16 * 6, "a different block misses again");
    assert!(gsu.cache.valid[1]);
}

#[test]
fn cache_opcode_reanchors_and_flushes() {
    let mut gsu = Gsu::new();
    gsu.cache_base = 0x0100;
    gsu.cache.valid[0] = true;
    gsu.set_pc(0x0234);
    gsu.start_cache_here();
    assert_eq!(gsu.cache_base, 0x0230, "CBR <- R15 & 0xfff0");
    assert!(
        !gsu.cache.valid.iter().any(|&v| v),
        "moving CBR flushes the cache"
    );
}

#[test]
fn cache_opcode_no_flush_when_base_unchanged() {
    let mut gsu = Gsu::new();
    gsu.cache_base = 0x0230;
    gsu.cache.valid[3] = true;
    gsu.set_pc(0x0234); // 0x0234 & 0xfff0 == 0x0230
    gsu.start_cache_here();
    assert!(gsu.cache.valid[3], "unchanged CBR leaves the cache intact");
}

// -- Execution: multiplier --------------------------------------------------

#[test]
fn mult_is_signed_umult_is_unsigned() {
    // mult r2: (int8)0xFF * (int8)2 = -1 * 2 = -2.
    let (mut gsu, mut bus) = running(&[0x82]);
    gsu.registers[0] = 0x00FF;
    gsu.registers[2] = 0x0002;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0xFFFE);
    assert!(gsu.flag(status::SIGN));

    // umult r2 (alt1): 255 * 2 = 510.
    let (mut gsu, mut bus) = running(&[0x82]);
    gsu.status |= status::ALT1;
    gsu.registers[0] = 0x00FF;
    gsu.registers[2] = 0x0002;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0x01FE);
    assert!(!gsu.flag(status::SIGN));
}

#[test]
fn mult_high_speed_saves_the_divider_penalty() {
    // ms0 clear costs one extra divider tick over ms0 set.
    let (mut slow, mut bus) = running(&[0x82]);
    slow.registers[0] = 0x0002;
    slow.registers[2] = 0x0003;
    let slow_clocks = slow.step(&mut bus);

    let (mut fast, mut bus) = running(&[0x82]);
    fast.config |= config::FAST_MULTIPLY;
    fast.registers[0] = 0x0002;
    fast.registers[2] = 0x0003;
    let fast_clocks = fast.step(&mut bus);

    assert_eq!(slow.registers[0], 6);
    assert_eq!(fast.registers[0], 6);
    assert_eq!(
        slow_clocks - fast_clocks,
        slow.clock_divider as u32,
        "ms0=0 adds one divider tick"
    );
}

#[test]
fn fmult_returns_high_word_lmult_also_writes_r4() {
    // fmult: (int16)0x8000 * (int16)2 = -65536 -> high word 0xFFFF.
    let (mut gsu, mut bus) = running(&[0x9f]);
    gsu.registers[0] = 0x8000;
    gsu.registers[6] = 0x0002;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0xFFFF, "destination = result >> 16");
    assert!(gsu.flag(status::SIGN));
    assert!(
        !gsu.flag(status::CARRY),
        "CY = bit 15 of the low word (0 here)"
    );

    // lmult (alt1): same product, low word lands in R4.
    let (mut gsu, mut bus) = running(&[0x9f]);
    gsu.status |= status::ALT1;
    gsu.registers[0] = 0x8000;
    gsu.registers[6] = 0x0002;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0xFFFF);
    assert_eq!(gsu.registers[4], 0x0000, "lmult writes the low word to R4");
}

#[test]
fn fmult_timing_depends_on_ms0() {
    let (mut slow, mut bus) = running(&[0x9f]);
    slow.registers[0] = 0x0100;
    slow.registers[6] = 0x0100;
    let slow_clocks = slow.step(&mut bus);

    let (mut fast, mut bus) = running(&[0x9f]);
    fast.config |= config::FAST_MULTIPLY;
    fast.registers[0] = 0x0100;
    fast.registers[6] = 0x0100;
    let fast_clocks = fast.step(&mut bus);

    // (7 - 3) * divider difference between the normal and high-speed paths.
    assert_eq!(slow_clocks - fast_clocks, 4 * slow.clock_divider as u32);
}

// -- Execution: stop --------------------------------------------------------

#[test]
fn stop_clears_go_and_raises_irq() {
    let (mut gsu, mut bus) = running(&[0x00]);
    gsu.step(&mut bus);
    assert!(!gsu.is_running(), "STOP clears GO");
    assert!(gsu.irq_pending(), "STOP raises the interrupt flag");
    assert_eq!(gsu.next_opcode, 0x01, "pipeline reloaded with NOP");
}

#[test]
fn stop_irq_masked_by_cfgr() {
    let (mut gsu, mut bus) = running(&[0x00]);
    gsu.config |= config::IRQ_MASKED; // mask the STOP interrupt
    gsu.step(&mut bus);
    assert!(!gsu.is_running());
    assert!(!gsu.irq_pending(), "CFGR.irq suppresses the STOP interrupt");
}

// -- Execution: ROM/RAM buffers ---------------------------------------------

#[test]
fn writing_r14_arms_rom_buffer() {
    // iwt r14,#$1234 ($fe): any write to R14 arms the ROM buffer.
    let (mut gsu, mut bus) = running(&[0xfe, 0x34, 0x12]);
    assert_eq!(gsu.rom_read_delay, 0);
    assert!(!gsu.flag(status::ROM_READ));
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[14], 0x1234, "R14 loaded");
    assert!(gsu.flag(status::ROM_READ), "R14 write arms SFR.r");
    assert_eq!(
        gsu.rom_read_delay,
        gsu.memory_clocks() as u8,
        "romcl = memory access latency"
    );
}

#[test]
fn rom_buffer_latency_counts_down_as_clocks_pass() {
    let (mut gsu, mut bus) = running(&[0x01]);
    bus.write(0x2000, 0x77);
    gsu.write_register(14, 0x2000); // arms the buffer: romcl = 6, SFR.r set
    assert_eq!(gsu.rom_read_delay, 6);
    assert!(gsu.flag(status::ROM_READ));

    gsu.spend(&mut bus, 4); // spend 4 of 6 clocks
    assert_eq!(gsu.rom_read_delay, 2, "romcl counts down");
    assert!(gsu.flag(status::ROM_READ), "still pending");

    gsu.spend(&mut bus, 2); // spend the remaining 2
    assert_eq!(gsu.rom_read_delay, 0);
    assert!(!gsu.flag(status::ROM_READ), "read completed, SFR.r clears");
    assert_eq!(gsu.rom_data, 0x77, "romdr valid after the latency");
}

#[test]
fn fetch_outside_the_cache_waits_for_the_rom_buffer() {
    let (mut gsu, mut bus) = running(&[0x01]);
    bus.write(0x2000, 0x99);
    gsu.write_register(14, 0x2000); // arm the ROM buffer (romcl = 6)
    assert!(gsu.flag(status::ROM_READ));
    // Reading outside the 512-byte cache window (offset >= 512) with PBR in
    // ROM space forces the pending ROM read to resolve first.
    let _ = gsu.fetch(&mut bus, 0x0300);
    assert!(
        !gsu.flag(status::ROM_READ),
        "outside-cache read synced the ROM buffer"
    );
    assert_eq!(gsu.rom_data, 0x99);
}

#[test]
fn getb_reads_rom_buffer_byte() {
    // getb ($ef): dr() <- ROM buffer byte at (ROMBR << 16) + R14.
    let (mut gsu, mut bus) = running(&[0xef]);
    bus.write(0x01_2000, 0x5a); // bank 1, offset 0x2000
    gsu.rom_bank = 1;
    gsu.write_register(14, 0x2000);
    assert!(gsu.flag(status::ROM_READ));
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0x005a, "getb loads the buffered ROM byte");
    assert!(!gsu.flag(status::ROM_READ), "buffer read clears SFR.r");
    assert_eq!(gsu.rom_read_delay, 0);
}

#[test]
fn getbh_getbl_getbs_merge_rom_byte() {
    // getbh ($ef alt1): ROM byte into the high byte, keep the source low.
    let (mut gsu, mut bus) = running(&[0xef]);
    bus.write(0x2000, 0xa5);
    gsu.write_register(14, 0x2000);
    gsu.status |= status::ALT1;
    gsu.registers[0] = 0x1234;
    gsu.step(&mut bus);
    assert_eq!(
        gsu.registers[0], 0xa534,
        "high = ROM byte, low = source low"
    );

    // getbl ($ef alt2): ROM byte into the low byte, keep the source high.
    let (mut gsu, mut bus) = running(&[0xef]);
    bus.write(0x2000, 0xa5);
    gsu.write_register(14, 0x2000);
    gsu.status |= status::ALT2;
    gsu.registers[0] = 0x1234;
    gsu.step(&mut bus);
    assert_eq!(
        gsu.registers[0], 0x12a5,
        "low = ROM byte, high = source high"
    );

    // getbs ($ef alt3): sign-extend the ROM byte.
    let (mut gsu, mut bus) = running(&[0xef]);
    bus.write(0x2000, 0x80);
    gsu.write_register(14, 0x2000);
    gsu.status |= status::ALT1 | status::ALT2;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0xff80, "byte sign-extended to 16 bits");
}

#[test]
fn getc_loads_color_register_with_por_filter() {
    // getc ($df): plain, COLR <- ROM buffer byte.
    let (mut gsu, mut bus) = running(&[0xdf]);
    bus.write(0x2000, 0x3c);
    gsu.write_register(14, 0x2000);
    gsu.step(&mut bus);
    assert_eq!(gsu.color, 0x3c, "getc copies the ROM byte to COLR");

    // getc + POR high-nibble: COLR <- (COLR & 0xf0) | (source >> 4).
    let (mut gsu, mut bus) = running(&[0xdf]);
    bus.write(0x2000, 0x3c);
    gsu.write_register(14, 0x2000);
    gsu.color = 0xa0;
    gsu.plot_options = plot_options::HIGH_NIBBLE;
    gsu.step(&mut bus);
    assert_eq!(gsu.color, 0xa3, "(0xa0 & 0xf0) | (0x3c >> 4)");

    // getc + POR freeze-high: COLR <- (COLR & 0xf0) | (source & 0x0f).
    let (mut gsu, mut bus) = running(&[0xdf]);
    bus.write(0x2000, 0x3c);
    gsu.write_register(14, 0x2000);
    gsu.color = 0xa0;
    gsu.plot_options = plot_options::FREEZE_HIGH;
    gsu.step(&mut bus);
    assert_eq!(gsu.color, 0xac, "(0xa0 & 0xf0) | (0x3c & 0x0f)");
}

#[test]
fn ramb_sets_ram_bank_and_romb_sets_rom_bank() {
    // ramb ($df alt2): RAMBR <- source bit 0.
    let (mut gsu, mut bus) = running(&[0xdf]);
    gsu.status |= status::ALT2;
    gsu.registers[0] = 0x0001;
    gsu.step(&mut bus);
    assert!(gsu.ram_bank, "RAMBR <- bit 0 of the source");

    // romb ($df alt3): ROMBR <- source & 0x7f (7-bit).
    let (mut gsu, mut bus) = running(&[0xdf]);
    gsu.status |= status::ALT1 | status::ALT2;
    gsu.registers[0] = 0x00c5;
    gsu.step(&mut bus);
    assert_eq!(gsu.rom_bank, 0x45, "ROMBR is 7-bit (0xc5 & 0x7f)");
}

// -- Execution: load / store ------------------------------------------------

const RAM: u32 = 0x70_0000;

#[test]
fn stw_stores_word_to_ram() {
    // stw (r1) ($31): RAM[r1] <- source word (sreg = 0 -> source = r0).
    let (mut gsu, mut bus) = running(&[0x31]);
    gsu.registers[1] = 0x0040;
    gsu.registers[0] = 0xbeef;
    gsu.step(&mut bus);
    gsu.finish_ram_write(&mut bus); // flush the pending high byte
    assert_eq!(bus.read(RAM + 0x40), 0xef, "low byte");
    assert_eq!(bus.read(RAM + 0x41), 0xbe, "high byte");
    assert_eq!(
        gsu.last_ram_address, 0x0040,
        "ramaddr latched for a later sbk"
    );
}

#[test]
fn stb_stores_only_low_byte() {
    // stb (r1) ($31 alt1): RAM[r1] <- source low byte only.
    let (mut gsu, mut bus) = running(&[0x31]);
    gsu.status |= status::ALT1;
    gsu.registers[1] = 0x0040;
    gsu.registers[0] = 0xbeef;
    bus.write(RAM + 0x41, 0x11); // must stay untouched
    gsu.step(&mut bus);
    gsu.finish_ram_write(&mut bus);
    assert_eq!(bus.read(RAM + 0x40), 0xef, "low byte written");
    assert_eq!(bus.read(RAM + 0x41), 0x11, "stb leaves the high byte alone");
}

#[test]
fn ldw_loads_word_from_ram() {
    // ldw (r1) ($41): dr() <- RAM word at r1 (little-endian).
    let (mut gsu, mut bus) = running(&[0x41]);
    gsu.registers[1] = 0x0050;
    bus.write(RAM + 0x50, 0xcd);
    bus.write(RAM + 0x51, 0xab);
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0xabcd, "word load into dr() (r0)");
}

#[test]
fn ldb_loads_zero_extended_byte() {
    // ldb (r1) ($41 alt1): dr() <- RAM low byte only.
    let (mut gsu, mut bus) = running(&[0x41]);
    gsu.status |= status::ALT1;
    gsu.registers[1] = 0x0050;
    bus.write(RAM + 0x50, 0xcd);
    bus.write(RAM + 0x51, 0xab); // ignored
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0x00cd, "ldb loads only the low byte");
}

#[test]
fn lms_and_sms_use_short_shifted_address() {
    // lms r1,(yy) ($a1 alt1): r1 <- RAM word at (yy << 1). yy = 0x08 -> $0010.
    let (mut gsu, mut bus) = running(&[0xa1, 0x08]);
    gsu.status |= status::ALT1;
    bus.write(RAM + 0x10, 0x22);
    bus.write(RAM + 0x11, 0x11);
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[1], 0x1122);
    assert_eq!(gsu.last_ram_address, 0x0010, "short address is yy << 1");
    assert_eq!(gsu.pc(), 3, "2-byte op");

    // sms (yy),r1 ($a1 alt2): RAM[yy << 1] <- r1.
    let (mut gsu, mut bus) = running(&[0xa1, 0x08]);
    gsu.status |= status::ALT2;
    gsu.registers[1] = 0x3344;
    gsu.step(&mut bus);
    gsu.finish_ram_write(&mut bus);
    assert_eq!(bus.read(RAM + 0x10), 0x44);
    assert_eq!(bus.read(RAM + 0x11), 0x33);
}

#[test]
fn lm_and_sm_use_absolute_address() {
    // lm r1,$1234 ($f1 alt1): r1 <- RAM word at absolute $1234.
    let (mut gsu, mut bus) = running(&[0xf1, 0x34, 0x12]);
    gsu.status |= status::ALT1;
    bus.write(RAM + 0x1234, 0x78);
    bus.write(RAM + 0x1235, 0x56);
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[1], 0x5678);
    assert_eq!(gsu.last_ram_address, 0x1234, "absolute address latched");
    assert_eq!(gsu.pc(), 4, "3-byte op leaves PC past the operands");

    // sm $1234,r1 ($f1 alt2): RAM[$1234] <- r1.
    let (mut gsu, mut bus) = running(&[0xf1, 0x34, 0x12]);
    gsu.status |= status::ALT2;
    gsu.registers[1] = 0x9abc;
    gsu.step(&mut bus);
    gsu.finish_ram_write(&mut bus);
    assert_eq!(bus.read(RAM + 0x1234), 0xbc);
    assert_eq!(bus.read(RAM + 0x1235), 0x9a);
}

#[test]
fn sbk_rewrites_the_last_ram_address() {
    // stw (r1) latches ramaddr; sbk stores back to it with a new source.
    let (mut gsu, mut bus) = running(&[0x31, 0x90]); // stw (r1) ; sbk
    gsu.registers[1] = 0x0060;
    gsu.registers[0] = 0x1111;
    gsu.step(&mut bus); // stw (r1): ramaddr <- 0x60
    gsu.registers[0] = 0x2222; // change the source before sbk
    gsu.step(&mut bus); // sbk: RAM[ramaddr] <- 0x2222
    gsu.finish_ram_write(&mut bus);
    assert_eq!(bus.read(RAM + 0x60), 0x22);
    assert_eq!(bus.read(RAM + 0x61), 0x22);
}

#[test]
fn store_then_load_round_trips_through_ram() {
    // sm $0400,r1 ($f1 alt2) writes; lm r2,$0400 ($f2 alt1) reads it back,
    // the load's `finish_ram_write` flushes the still-pending store first.
    let (mut gsu, mut bus) = running(&[0xf1, 0x00, 0x04, 0xf2, 0x00, 0x04]);
    gsu.registers[1] = 0xdead;
    gsu.status |= status::ALT2;
    gsu.step(&mut bus); // sm $0400,r1
    gsu.status |= status::ALT1;
    gsu.step(&mut bus); // lm r2,$0400
    assert_eq!(
        gsu.registers[2], 0xdead,
        "value survives the RAM round-trip"
    );
}

// -- Execution: plot engine -------------------------------------------------

/// Oracle: pack eight colour indices into `bpp` bitplane bytes exactly as the
/// GSU flush does, plane `n`, bit `(x & 7) ^ 7` holds bit `n` of `colors[x]`.
fn pack_planes(colors: [u8; 8], bpp: usize) -> Vec<u8> {
    (0..bpp)
        .map(|n| {
            let mut byte = 0u8;
            for (x, &c) in colors.iter().enumerate() {
                let bitpos = (x as u8 & 7) ^ 7;
                byte |= ((c >> n) & 1) << bitpos;
            }
            byte
        })
        .collect()
}

/// The Game Pak RAM byte offset of bitplane `n` within a character (as bsnes-plus computes it,
/// `byte = ((n >> 1) << 4) + (n & 1)` = 0,1,16,17,32,33,48,49).
fn plane_byte(n: usize) -> u32 {
    (((n >> 1) << 4) + (n & 1)) as u32
}

/// Plot the eight pixels `x = 0..8` at `y = 0` (screen column 0) with the
/// given colours, then flush the completed column and return the bus.
fn plot_column(scmr: u8, scbr: u8, por: u8, colors: [u8; 8]) -> FlatBus {
    let mut gsu = Gsu::new();
    let mut bus = FlatBus::new(&[]);
    gsu.screen_mode = scmr;
    gsu.screen_base = scbr;
    gsu.plot_options = por;
    for (x, &c) in colors.iter().enumerate() {
        gsu.color = c;
        gsu.plot(&mut bus, x as u8, 0);
    }
    // The eighth pixel fills the line (pending == 0xff) and moves it into
    // pixel_cache[1]; flush that line to commit the bitplanes to RAM.
    gsu.flush_pixels(&mut bus, 1);
    bus
}

#[test]
fn plot_2bpp_packs_a_full_column_into_two_planes() {
    let colors = [0u8, 1, 2, 3, 3, 2, 1, 0];
    let mut bus = plot_column(0x00, 0, plot_options::PLOT_TRANSPARENT, colors); // md 0 = 2bpp
    let planes = pack_planes(colors, 2);
    assert_eq!(bus.read(RAM + plane_byte(0)), planes[0], "plane 0");
    assert_eq!(bus.read(RAM + plane_byte(1)), planes[1], "plane 1");
}

#[test]
fn plot_4bpp_packs_a_full_column_into_four_planes() {
    let colors = [0u8, 1, 2, 4, 8, 15, 10, 5];
    let mut bus = plot_column(0x01, 0, plot_options::PLOT_TRANSPARENT, colors); // md 1 = 4bpp
    let planes = pack_planes(colors, 4);
    for (n, &want) in planes.iter().enumerate() {
        assert_eq!(bus.read(RAM + plane_byte(n)), want, "plane {n}");
    }
}

#[test]
fn plot_8bpp_packs_a_full_column_into_eight_planes() {
    let colors = [0x01u8, 0x80, 0x24, 0xff, 0x00, 0x7e, 0x81, 0x42];
    let mut bus = plot_column(0x03, 0, plot_options::PLOT_TRANSPARENT, colors); // md 3 = 8bpp
    let planes = pack_planes(colors, 8);
    for (n, &want) in planes.iter().enumerate() {
        assert_eq!(bus.read(RAM + plane_byte(n)), want, "plane {n}");
    }
}

#[test]
fn plot_honours_scbr_and_y_within_the_character() {
    // scbr shifts the base by <<10; the y&7 row picks a 2-byte line.
    let colors = [1u8; 8];
    let mut gsu = Gsu::new();
    let mut bus = FlatBus::new(&[]);
    gsu.screen_mode = 0x00; // 2bpp, ht 0
    gsu.screen_base = 2;
    gsu.plot_options = plot_options::PLOT_TRANSPARENT;
    for x in 0..8u8 {
        gsu.color = colors[x as usize];
        gsu.plot(&mut bus, x, 3); // y = 3 -> (y & 7) * 2 = 6
    }
    gsu.flush_pixels(&mut bus, 1);
    // cn = 0 (x,y < 8), addr = 0x700000 + (2 << 10) + 6.
    let addr = RAM + (2 << 10) + 6;
    assert_eq!(
        bus.read(addr),
        0xff,
        "plane 0 at the scbr/y-shifted address"
    );
}

#[test]
fn plot_dither_alternates_the_colr_nibble() {
    // Dither (outside 8bpp) uses the high nibble on odd (x^y), low on even.
    let mut gsu = Gsu::new();
    let mut bus = FlatBus::new(&[]);
    gsu.screen_mode = 0x01; // 4bpp
    gsu.plot_options = plot_options::DITHER | plot_options::PLOT_TRANSPARENT;
    gsu.color = 0xab;
    gsu.plot(&mut bus, 0, 0); // (0^0)&1 = 0 -> low nibble 0x0b
    gsu.plot(&mut bus, 1, 0); // (1^0)&1 = 1 -> high nibble 0x0a
    assert_eq!(
        gsu.read_pixel(&mut bus, 0, 0),
        0x0b,
        "even pixel keeps the low nibble"
    );
    assert_eq!(
        gsu.read_pixel(&mut bus, 1, 0),
        0x0a,
        "odd pixel takes the high nibble"
    );
}

#[test]
fn plot_transparency_skips_colour_zero() {
    let mut gsu = Gsu::new();
    let mut bus = FlatBus::new(&[]);
    gsu.screen_mode = 0x01; // 4bpp
    gsu.plot_options = 0; // transparency active (POR.transparent clear)
    gsu.color = 0x00;
    gsu.plot(&mut bus, 0, 0);
    assert_eq!(gsu.pixel_cache[0].pending, 0, "colour 0 is not plotted");
    gsu.color = 0x05;
    gsu.plot(&mut bus, 0, 0);
    assert_ne!(
        gsu.pixel_cache[0].pending, 0,
        "a non-zero colour is plotted"
    );
}

#[test]
fn plot_8bpp_transparency_uses_freezehigh_nibble() {
    // In 8bpp: without freezehigh the whole COLR gates; with freezehigh only
    // the low nibble gates (a high-nibble-only colour becomes transparent).
    let mut gsu = Gsu::new();
    let mut bus = FlatBus::new(&[]);
    gsu.screen_mode = 0x03; // 8bpp
    gsu.plot_options = 0; // transparency active
    gsu.color = 0xf0; // high nibble only
    gsu.plot(&mut bus, 0, 0);
    assert_ne!(
        gsu.pixel_cache[0].pending, 0,
        "0xf0 is opaque in plain 8bpp"
    );

    let mut gsu = Gsu::new();
    let mut bus = FlatBus::new(&[]);
    gsu.screen_mode = 0x03;
    gsu.plot_options = plot_options::FREEZE_HIGH; // transparency active + freezehigh
    gsu.color = 0xf0;
    gsu.plot(&mut bus, 0, 0);
    assert_eq!(
        gsu.pixel_cache[0].pending, 0,
        "0xf0 is transparent under freezehigh"
    );
}

#[test]
fn read_pixel_round_trips_a_plotted_column() {
    let colors = [0u8, 3, 5, 9, 12, 15, 1, 7];
    let mut gsu = Gsu::new();
    let mut bus = FlatBus::new(&[]);
    gsu.screen_mode = 0x01; // 4bpp
    gsu.plot_options = plot_options::PLOT_TRANSPARENT;
    for x in 0..8u8 {
        gsu.color = colors[x as usize];
        gsu.plot(&mut bus, x, 0);
    }
    for x in 0..8u8 {
        assert_eq!(
            gsu.read_pixel(&mut bus, x, 0),
            colors[x as usize],
            "rpix reads back the plotted colour at x={x}"
        );
    }
}

#[test]
fn a_partial_line_keeps_the_pixels_not_drawn() {
    // A partial line (pending != 0xff) must preserve the untouched bits of
    // the destination bytes.
    let mut gsu = Gsu::new();
    let mut bus = FlatBus::new(&[]);
    bus.write(RAM, 0xff);
    bus.write(RAM + 1, 0xff);
    gsu.screen_mode = 0x00; // 2bpp
    gsu.plot_options = plot_options::PLOT_TRANSPARENT;
    gsu.color = 0x01; // plane 0 bit set, plane 1 clear
    gsu.plot(&mut bus, 0, 0); // only x=0 -> bit (0^7)=7 pending
    gsu.flush_pixels(&mut bus, 0);
    // Plane 0: bit 7 = 1, bits 0-6 preserved from 0xff -> 0xff.
    // Plane 1: bit 7 = 0, bits 0-6 preserved from 0xff -> 0x7f.
    assert_eq!(bus.read(RAM), 0xff, "plane 0 RMW keeps the untouched bits");
    assert_eq!(bus.read(RAM + 1), 0x7f, "plane 1 RMW clears only bit 7");
}

#[test]
fn character_index_matches_each_height_layout() {
    let mut gsu = Gsu::new();
    let (x, y) = (0x10u8, 0x08u8);
    gsu.screen_mode = 0x00; // ht 0
    assert_eq!(gsu.character_index(x, y), 33, "ht 0");
    gsu.screen_mode = 0x04; // ht 1 (low bit = SCMR bit 2)
    assert_eq!(gsu.character_index(x, y), 41, "ht 1");
    gsu.screen_mode = 0x20; // ht 2 (high bit = SCMR bit 5)
    assert_eq!(gsu.character_index(x, y), 49, "ht 2");
    gsu.screen_mode = 0x24; // ht 3 (obj layout)
    assert_eq!(gsu.character_index(x, y), 18, "ht 3");
}

#[test]
fn character_index_sprite_bit_forces_the_sprite_layout() {
    let mut gsu = Gsu::new();
    let (x, y) = (0x10u8, 0x08u8);
    gsu.screen_mode = 0x00; // ht 0 ...
    gsu.plot_options = plot_options::SPRITE_LAYOUT; // ... but POR.obj forces the ht-3 (obj) layout
    assert_eq!(gsu.character_index(x, y), 18, "POR.obj overrides SCMR.ht");
}

#[test]
fn planes_match_each_colour_depth() {
    let mut gsu = Gsu::new();
    for (md, want) in [(0u8, 2usize), (1, 4), (2, 4), (3, 8)] {
        gsu.screen_mode = md;
        assert_eq!(gsu.planes(), want, "md {md} -> {want} bpp");
    }
}

#[test]
fn pixel_row_address_computes_the_bitplane_address() {
    let mut gsu = Gsu::new();
    gsu.screen_mode = 0x01; // 4bpp, ht 0
    gsu.screen_base = 2;
    let addr = gsu.pixel_row_address(0x10, 0x08); // character 33, y & 7 = 0
    assert_eq!(gsu.planes(), 4);
    assert_eq!(
        addr,
        RAM + 33 * (4 << 3) + (2 << 10),
        "0x700000 + cn*32 + scbr<<10"
    );
}

#[test]
fn plot_rotates_the_pixel_cache_on_a_column_change() {
    let mut gsu = Gsu::new();
    let mut bus = FlatBus::new(&[]);
    gsu.screen_mode = 0x00;
    gsu.plot_options = plot_options::PLOT_TRANSPARENT;
    gsu.color = 0x03;
    gsu.plot(&mut bus, 0, 0); // group 0 -> pixel_cache[0]
    gsu.plot(&mut bus, 8, 0); // offset (8 >> 3) = 1 differs -> rotate
    assert_eq!(gsu.pixel_cache[1].group, 0, "old group moved to line 1");
    assert_eq!(gsu.pixel_cache[0].group, 1, "new group opened in line 0");
}

#[test]
fn op_plot_post_increments_r1() {
    let (mut gsu, mut bus) = running(&[0x4c]); // plot
    gsu.screen_mode = 0x00;
    gsu.plot_options = plot_options::PLOT_TRANSPARENT;
    gsu.registers[1] = 5;
    gsu.registers[2] = 0;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[1], 6, "plot post-increments R1");
}

#[test]
fn op_rpix_reads_blank_ram_as_zero_and_sets_z() {
    let (mut gsu, mut bus) = running(&[0x4c]); // rpix ($4c alt1)
    gsu.status |= status::ALT1;
    gsu.screen_mode = 0x01; // 4bpp
    gsu.registers[1] = 0;
    gsu.registers[2] = 0;
    gsu.step(&mut bus);
    assert_eq!(gsu.registers[0], 0, "rpix of blank RAM is 0 (into dr = r0)");
    assert!(gsu.flag(status::ZERO), "Z set when rpix reads zero");
    assert!(!gsu.flag(status::SIGN), "S clear - rpix returns a byte");
    assert!(!gsu.alt1(), "ALT1 retired after rpix");
}

#[test]
fn op_color_filters_through_por() {
    let (mut gsu, mut bus) = running(&[0x4e]); // color ($4e alt0)
    gsu.color = 0xa0;
    gsu.plot_options = plot_options::HIGH_NIBBLE;
    gsu.registers[0] = 0x3c; // sr() (default sreg 0)
    gsu.step(&mut bus);
    assert_eq!(gsu.color, 0xa3, "(0xa0 & 0xf0) | (0x3c >> 4)");
}

#[test]
fn op_cmode_sets_por_from_source() {
    let (mut gsu, mut bus) = running(&[0x4e]); // cmode ($4e alt1)
    gsu.status |= status::ALT1;
    gsu.registers[0] = (plot_options::DITHER | plot_options::PLOT_TRANSPARENT) as u16; // sr()
    gsu.step(&mut bus);
    assert_eq!(
        gsu.plot_options,
        plot_options::DITHER | plot_options::PLOT_TRANSPARENT,
        "cmode copies sr() to POR"
    );
    assert!(!gsu.alt1(), "ALT1 retired after cmode");
}
