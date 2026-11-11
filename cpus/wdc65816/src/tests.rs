//! Unit tests for the WDC 65C816 CPU.

use crate::disasm::{disassemble_at, DisasmState};
use crate::instructions::*;
use crate::interrupts;
use crate::Wdc65816;
use retro_core::cpu::Addressable;

struct TestBus {
    mem: Box<[u8]>,
}

impl TestBus {
    fn new() -> Self {
        Self {
            mem: vec![0u8; 0x100_0000].into_boxed_slice(),
        }
    }

    fn load(&mut self, addr: u32, data: &[u8]) {
        for (i, &b) in data.iter().enumerate() {
            self.mem[(addr as usize + i) & 0xFF_FFFF] = b;
        }
    }
}

impl Addressable for TestBus {
    fn read(&mut self, addr: u32) -> u8 {
        self.mem[(addr as usize) & 0xFF_FFFF]
    }
    fn write(&mut self, addr: u32, val: u8) {
        self.mem[(addr as usize) & 0xFF_FFFF] = val;
    }
}

fn make() -> (Wdc65816, TestBus) {
    let cpu = Wdc65816::new();
    let bus = TestBus::new();
    (cpu, bus)
}

/// Helper to switch CPU to native mode with given M/X flags.
fn enter_native(cpu: &mut Wdc65816, m16: bool, x16: bool) {
    cpu.emulation = false;
    cpu.p &= !(FLAG_M | FLAG_X);
    if !m16 {
        cpu.p |= FLAG_M; // M=1 means 8-bit
    }
    if !x16 {
        cpu.p |= FLAG_X;
    }
    // When M=0 => 16-bit A; when X=0 => 16-bit X/Y
    // m16=true means we want 16-bit accum => M flag = 0
    // x16=true means we want 16-bit index => X flag = 0
}

// ========================================================================
// Emulation mode tests (6502 compatibility)
// ========================================================================

#[test]
fn test_lda_immediate_emu() {
    let (mut cpu, mut bus) = make();
    bus.load(0x0000, &[0xA9, 0x42]); // LDA #$42
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x42);
    assert_eq!(cycles, 2);
    assert!(!cpu.get_flag(FLAG_Z));
    assert!(!cpu.get_flag(FLAG_N));
}

#[test]
fn test_lda_immediate_zero() {
    let (mut cpu, mut bus) = make();
    bus.load(0x0000, &[0xA9, 0x00]);
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x00);
    assert!(cpu.get_flag(FLAG_Z));
    assert!(!cpu.get_flag(FLAG_N));
}

#[test]
fn test_lda_immediate_negative() {
    let (mut cpu, mut bus) = make();
    bus.load(0x0000, &[0xA9, 0x80]);
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x80);
    assert!(!cpu.get_flag(FLAG_Z));
    assert!(cpu.get_flag(FLAG_N));
}

#[test]
fn test_ldx_immediate_emu() {
    let (mut cpu, mut bus) = make();
    bus.load(0x0000, &[0xA2, 0x10]); // LDX #$10
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.x, 0x10);
}

#[test]
fn test_ldy_immediate_emu() {
    let (mut cpu, mut bus) = make();
    bus.load(0x0000, &[0xA0, 0x20]); // LDY #$20
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.y, 0x20);
}

#[test]
fn test_sta_direct_emu() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x0055;
    bus.load(0x0000, &[0x85, 0x10]); // STA $10
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(bus.mem[0x10], 0x55);
}

#[test]
fn test_stz_direct_emu() {
    let (mut cpu, mut bus) = make();
    bus.mem[0x10] = 0xFF;
    bus.load(0x0000, &[0x64, 0x10]); // STZ $10
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(bus.mem[0x10], 0x00);
}

#[test]
fn test_adc_simple_emu() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x0010;
    cpu.set_flag(FLAG_C, false);
    bus.load(0x0000, &[0x69, 0x20]); // ADC #$20
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x30);
    assert!(!cpu.get_flag(FLAG_C));
    assert!(!cpu.get_flag(FLAG_V));
}

#[test]
fn test_adc_carry_emu() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x00FF;
    cpu.set_flag(FLAG_C, false);
    bus.load(0x0000, &[0x69, 0x01]); // ADC #$01
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x00);
    assert!(cpu.get_flag(FLAG_C));
    assert!(cpu.get_flag(FLAG_Z));
}

#[test]
fn test_adc_overflow_emu() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x007F; // +127
    cpu.set_flag(FLAG_C, false);
    bus.load(0x0000, &[0x69, 0x01]); // ADC #$01 = +1
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x80);
    assert!(cpu.get_flag(FLAG_V));
    assert!(cpu.get_flag(FLAG_N));
}

#[test]
fn test_sbc_simple_emu() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x0050;
    cpu.set_flag(FLAG_C, true); // no borrow
    bus.load(0x0000, &[0xE9, 0x20]); // SBC #$20
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x30);
    assert!(cpu.get_flag(FLAG_C)); // no borrow
}

#[test]
fn test_sbc_borrow_emu() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x0010;
    cpu.set_flag(FLAG_C, true);
    bus.load(0x0000, &[0xE9, 0x20]); // SBC #$20
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0xF0);
    assert!(!cpu.get_flag(FLAG_C)); // borrow occurred
}

#[test]
fn test_and_emu() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x00FF;
    bus.load(0x0000, &[0x29, 0x0F]); // AND #$0F
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x0F);
}

#[test]
fn test_ora_emu() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x00F0;
    bus.load(0x0000, &[0x09, 0x0F]); // ORA #$0F
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0xFF);
}

#[test]
fn test_eor_emu() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x00FF;
    bus.load(0x0000, &[0x49, 0x0F]); // EOR #$0F
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0xF0);
}

#[test]
fn test_cmp_equal_emu() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x0042;
    bus.load(0x0000, &[0xC9, 0x42]); // CMP #$42
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(cpu.get_flag(FLAG_Z));
    assert!(cpu.get_flag(FLAG_C));
}

#[test]
fn test_cmp_less_emu() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x0010;
    bus.load(0x0000, &[0xC9, 0x20]); // CMP #$20
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(!cpu.get_flag(FLAG_Z));
    assert!(!cpu.get_flag(FLAG_C));
}

#[test]
fn test_bit_emu() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x00FF;
    bus.mem[0x10] = 0xC0;
    bus.load(0x0000, &[0x24, 0x10]); // BIT $10
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(!cpu.get_flag(FLAG_Z));
    assert!(cpu.get_flag(FLAG_N));
    assert!(cpu.get_flag(FLAG_V));
}

#[test]
fn test_asl_accumulator_emu() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x0081;
    bus.load(0x0000, &[0x0A]); // ASL A
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x02);
    assert!(cpu.get_flag(FLAG_C));
}

#[test]
fn test_lsr_accumulator_emu() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x0003;
    bus.load(0x0000, &[0x4A]); // LSR A
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x01);
    assert!(cpu.get_flag(FLAG_C));
}

#[test]
fn test_rol_accumulator_emu() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x0080;
    cpu.set_flag(FLAG_C, true);
    bus.load(0x0000, &[0x2A]); // ROL A
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x01);
    assert!(cpu.get_flag(FLAG_C));
}

#[test]
fn test_ror_accumulator_emu() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x0001;
    cpu.set_flag(FLAG_C, true);
    bus.load(0x0000, &[0x6A]); // ROR A
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x80);
    assert!(cpu.get_flag(FLAG_C));
}

#[test]
fn test_inc_dec_xy_emu() {
    let (mut cpu, mut bus) = make();
    cpu.x = 5;
    cpu.y = 10;
    bus.load(0x0000, &[0xE8, 0xC8, 0xCA, 0x88]); // INX, INY, DEX, DEY
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.x, 6);
    cpu.step(&mut bus);
    assert_eq!(cpu.y, 11);
    cpu.step(&mut bus);
    assert_eq!(cpu.x, 5);
    cpu.step(&mut bus);
    assert_eq!(cpu.y, 10);
}

#[test]
fn test_inc_a_emu() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x00FF;
    bus.load(0x0000, &[0x1A]); // INC A
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x00);
    assert!(cpu.get_flag(FLAG_Z));
}

#[test]
fn test_dec_a_emu() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x0001;
    bus.load(0x0000, &[0x3A]); // DEC A
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x00);
    assert!(cpu.get_flag(FLAG_Z));
}

// ---- Branch tests ----

#[test]
fn test_bne_taken_emu() {
    let (mut cpu, mut bus) = make();
    cpu.set_flag(FLAG_Z, false);
    bus.load(0x0000, &[0xD0, 0x05]); // BNE +5
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0007); // 0x0002 + 5
    assert!(cycles >= 3); // base 2 + taken 1
}

#[test]
fn test_bne_not_taken_emu() {
    let (mut cpu, mut bus) = make();
    cpu.set_flag(FLAG_Z, true);
    bus.load(0x0000, &[0xD0, 0x05]);
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0002);
    assert_eq!(cycles, 2);
}

#[test]
fn test_bra_emu() {
    let (mut cpu, mut bus) = make();
    bus.load(0x0000, &[0x80, 0x10]); // BRA +16
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0012);
}

#[test]
fn test_brl() {
    let (mut cpu, mut bus) = make();
    // BRL with 16-bit offset of +0x100
    bus.load(0x0000, &[0x82, 0x00, 0x01]); // BRL +$0100
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0103); // 0x0003 + 0x0100
}

// ---- Stack tests ----

#[test]
fn test_pha_pla_emu() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x0042;
    cpu.sp = 0x01FF;
    bus.load(0x0000, &[0x48, 0xA9, 0x00, 0x68]); // PHA, LDA #$00, PLA
    cpu.pc = 0x0000;
    cpu.step(&mut bus); // PHA
    cpu.step(&mut bus); // LDA #$00
    assert_eq!(cpu.a_lo(), 0x00);
    cpu.step(&mut bus); // PLA
    assert_eq!(cpu.a_lo(), 0x42);
}

#[test]
fn test_php_plp_emu() {
    let (mut cpu, mut bus) = make();
    cpu.sp = 0x01FF;
    cpu.p = 0xFF;
    bus.load(0x0000, &[0x08, 0x28]); // PHP, PLP
    cpu.pc = 0x0000;
    cpu.step(&mut bus); // PHP
    cpu.p = 0x00;
    cpu.step(&mut bus); // PLP
                        // In emulation mode M and X are always forced
    assert_eq!(cpu.p & (FLAG_M | FLAG_X), FLAG_M | FLAG_X);
}

// ---- JMP/JSR/RTS tests ----

#[test]
fn test_jmp_absolute_emu() {
    let (mut cpu, mut bus) = make();
    bus.load(0x0000, &[0x4C, 0x00, 0x10]); // JMP $1000
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x1000);
}

#[test]
fn test_jsr_rts_emu() {
    let (mut cpu, mut bus) = make();
    cpu.sp = 0x01FF;
    bus.load(0x0000, &[0x20, 0x00, 0x10]); // JSR $1000
    bus.load(0x1000, &[0x60]); // RTS
    cpu.pc = 0x0000;
    cpu.step(&mut bus); // JSR
    assert_eq!(cpu.pc, 0x1000);
    cpu.step(&mut bus); // RTS
    assert_eq!(cpu.pc, 0x0003);
}

// ---- Transfer tests ----

#[test]
fn test_tax_tay_emu() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x0042;
    bus.load(0x0000, &[0xAA, 0xA8]); // TAX, TAY
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.x, 0x42);
    cpu.step(&mut bus);
    assert_eq!(cpu.y, 0x42);
}

#[test]
fn test_txa_tya_emu() {
    let (mut cpu, mut bus) = make();
    cpu.x = 0x55;
    cpu.y = 0xAA;
    bus.load(0x0000, &[0x8A, 0x98]); // TXA, TYA
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x55);
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0xAA);
}

// ---- Flag manipulation ----

#[test]
fn test_clc_sec_emu() {
    let (mut cpu, mut bus) = make();
    bus.load(0x0000, &[0x38, 0x18]); // SEC, CLC
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(cpu.get_flag(FLAG_C));
    cpu.step(&mut bus);
    assert!(!cpu.get_flag(FLAG_C));
}

#[test]
fn test_sei_cli_emu() {
    let (mut cpu, mut bus) = make();
    cpu.set_flag(FLAG_I, false);
    bus.load(0x0000, &[0x78, 0x58]); // SEI, CLI
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(cpu.get_flag(FLAG_I));
    cpu.step(&mut bus);
    assert!(!cpu.get_flag(FLAG_I));
}

// ---- NOP / STP / WAI ----

#[test]
fn test_nop_emu() {
    let (mut cpu, mut bus) = make();
    bus.load(0x0000, &[0xEA]); // NOP
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0001);
    assert_eq!(cycles, 2);
}

#[test]
fn test_stp() {
    let (mut cpu, mut bus) = make();
    bus.load(0x0000, &[0xDB]); // STP
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(cpu.stopped);
    // Further steps should return 1 cycle
    let cycles = cpu.step(&mut bus);
    assert_eq!(cycles, 1);
}

#[test]
fn test_wai() {
    let (mut cpu, mut bus) = make();
    bus.load(0x0000, &[0xCB]); // WAI
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(cpu.waiting);
    // Without interrupts pending, step returns 1
    let cycles = cpu.step(&mut bus);
    assert_eq!(cycles, 1);
}

/// An IRQ arriving with the I flag *set* still wakes WAI, but is not
/// serviced: execution resumes at the instruction after WAI. This is the
/// `SEI ... WAI` raster-sync pattern (Star Fox / Doom 3D loops), gating the
/// wake on the I flag would deadlock the CPU forever.
#[test]
fn test_wai_wakes_on_irq_with_i_set_but_does_not_vector() {
    let (mut cpu, mut bus) = make();
    // Emulation mode defaults to I set; point the IRQ vector somewhere
    // recognisable so we can prove we did *not* vector to it.
    cpu.set_flag(FLAG_I, true);
    bus.load(interrupts::EMU_IRQ_VECTOR as u32, &[0x00, 0x90]); // -> $9000
    bus.load(0x0000, &[0xCB, 0xEA]); // WAI ; NOP
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(cpu.waiting, "WAI arms the wait state");

    cpu.irq_pending = true;
    cpu.step(&mut bus);
    assert!(
        !cpu.waiting,
        "an asserted IRQ line wakes WAI even with I set"
    );
    assert_eq!(
        cpu.pc, 0x0002,
        "with I set the CPU resumes past WAI (ran the NOP), not the IRQ vector"
    );
    assert!(cpu.get_flag(FLAG_I), "no vectoring, so I is untouched");
}

/// An IRQ arriving with the I flag *clear* both wakes WAI and vectors to the
/// handler.
#[test]
fn test_wai_wakes_and_vectors_on_irq_with_i_clear() {
    let (mut cpu, mut bus) = make();
    cpu.set_flag(FLAG_I, false);
    bus.load(interrupts::EMU_IRQ_VECTOR as u32, &[0x00, 0x90]); // -> $9000
    bus.load(0x0000, &[0xCB]); // WAI
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(cpu.waiting);

    cpu.irq_pending = true;
    cpu.step(&mut bus);
    assert!(!cpu.waiting, "the IRQ wakes WAI");
    assert_eq!(
        cpu.pc, 0x9000,
        "with I clear the CPU vectors to the handler"
    );
    assert!(cpu.get_flag(FLAG_I), "servicing the IRQ sets I");
}

// ========================================================================
// Native mode tests (16-bit)
// ========================================================================

#[test]
fn test_lda_16bit() {
    let (mut cpu, mut bus) = make();
    enter_native(&mut cpu, true, false); // 16-bit A
    bus.load(0x0000, &[0xA9, 0x34, 0x12]); // LDA #$1234
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.c, 0x1234);
}

#[test]
fn test_lda_16bit_zero() {
    let (mut cpu, mut bus) = make();
    enter_native(&mut cpu, true, false);
    bus.load(0x0000, &[0xA9, 0x00, 0x00]); // LDA #$0000
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.c, 0x0000);
    assert!(cpu.get_flag(FLAG_Z));
}

#[test]
fn test_ldx_16bit() {
    let (mut cpu, mut bus) = make();
    enter_native(&mut cpu, false, true); // 16-bit X/Y
    bus.load(0x0000, &[0xA2, 0xCD, 0xAB]); // LDX #$ABCD
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.x, 0xABCD);
}

#[test]
fn test_sta_16bit() {
    let (mut cpu, mut bus) = make();
    enter_native(&mut cpu, true, false);
    cpu.c = 0x1234;
    bus.load(0x0000, &[0x8D, 0x00, 0x20]); // STA $2000
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(bus.mem[0x2000], 0x34); // low byte
    assert_eq!(bus.mem[0x2001], 0x12); // high byte
}

#[test]
fn test_adc_16bit() {
    let (mut cpu, mut bus) = make();
    enter_native(&mut cpu, true, false);
    cpu.c = 0x1000;
    cpu.set_flag(FLAG_C, false);
    bus.load(0x0000, &[0x69, 0x00, 0x20]); // ADC #$2000
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.c, 0x3000);
    assert!(!cpu.get_flag(FLAG_C));
}

#[test]
fn test_adc_16bit_carry() {
    let (mut cpu, mut bus) = make();
    enter_native(&mut cpu, true, false);
    cpu.c = 0xFFFF;
    cpu.set_flag(FLAG_C, false);
    bus.load(0x0000, &[0x69, 0x01, 0x00]); // ADC #$0001
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.c, 0x0000);
    assert!(cpu.get_flag(FLAG_C));
    assert!(cpu.get_flag(FLAG_Z));
}

#[test]
fn test_sbc_16bit() {
    let (mut cpu, mut bus) = make();
    enter_native(&mut cpu, true, false);
    cpu.c = 0x5000;
    cpu.set_flag(FLAG_C, true);
    bus.load(0x0000, &[0xE9, 0x00, 0x20]); // SBC #$2000
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.c, 0x3000);
    assert!(cpu.get_flag(FLAG_C));
}

#[test]
fn test_and_16bit() {
    let (mut cpu, mut bus) = make();
    enter_native(&mut cpu, true, false);
    cpu.c = 0xFF00;
    bus.load(0x0000, &[0x29, 0x0F, 0xFF]); // AND #$FF0F
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.c, 0xFF00);
}

#[test]
fn test_ora_16bit() {
    let (mut cpu, mut bus) = make();
    enter_native(&mut cpu, true, false);
    cpu.c = 0xF000;
    bus.load(0x0000, &[0x09, 0x0F, 0x00]); // ORA #$000F
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.c, 0xF00F);
}

#[test]
fn test_eor_16bit() {
    let (mut cpu, mut bus) = make();
    enter_native(&mut cpu, true, false);
    cpu.c = 0xFFFF;
    bus.load(0x0000, &[0x49, 0xFF, 0x00]); // EOR #$00FF
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.c, 0xFF00);
}

#[test]
fn test_cmp_16bit() {
    let (mut cpu, mut bus) = make();
    enter_native(&mut cpu, true, false);
    cpu.c = 0x1234;
    bus.load(0x0000, &[0xC9, 0x34, 0x12]); // CMP #$1234
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(cpu.get_flag(FLAG_Z));
    assert!(cpu.get_flag(FLAG_C));
}

#[test]
fn test_asl_16bit() {
    let (mut cpu, mut bus) = make();
    enter_native(&mut cpu, true, false);
    cpu.c = 0x8001;
    bus.load(0x0000, &[0x0A]); // ASL A
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.c, 0x0002);
    assert!(cpu.get_flag(FLAG_C));
}

#[test]
fn test_lsr_16bit() {
    let (mut cpu, mut bus) = make();
    enter_native(&mut cpu, true, false);
    cpu.c = 0x0003;
    bus.load(0x0000, &[0x4A]); // LSR A
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.c, 0x0001);
    assert!(cpu.get_flag(FLAG_C));
}

#[test]
fn test_inc_16bit() {
    let (mut cpu, mut bus) = make();
    enter_native(&mut cpu, true, false);
    cpu.c = 0xFFFF;
    bus.load(0x0000, &[0x1A]); // INC A
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.c, 0x0000);
    assert!(cpu.get_flag(FLAG_Z));
}

#[test]
fn test_dec_16bit() {
    let (mut cpu, mut bus) = make();
    enter_native(&mut cpu, true, false);
    cpu.c = 0x0000;
    bus.load(0x0000, &[0x3A]); // DEC A
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.c, 0xFFFF);
    assert!(cpu.get_flag(FLAG_N));
}

#[test]
fn test_inx_16bit() {
    let (mut cpu, mut bus) = make();
    enter_native(&mut cpu, false, true); // 16-bit X/Y
    cpu.x = 0x00FF;
    bus.load(0x0000, &[0xE8]); // INX
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.x, 0x0100);
}

#[test]
fn test_dex_16bit() {
    let (mut cpu, mut bus) = make();
    enter_native(&mut cpu, false, true);
    cpu.x = 0x0100;
    bus.load(0x0000, &[0xCA]); // DEX
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.x, 0x00FF);
}

// ---- 16-bit stack ----

#[test]
fn test_pha_pla_16bit() {
    let (mut cpu, mut bus) = make();
    enter_native(&mut cpu, true, false);
    cpu.sp = 0x01FF;
    cpu.c = 0x1234;
    bus.load(0x0000, &[0x48]); // PHA
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.sp, 0x01FD);

    cpu.c = 0x0000;
    bus.load(0x0002, &[0x68]); // PLA (at PC 0x0002 after PHA advanced 1 byte)
                               // PC is already at 0x0001, but we need to continue
    cpu.pc = 0x0002;
    cpu.step(&mut bus);
    assert_eq!(cpu.c, 0x1234);
}

#[test]
fn test_phx_plx_16bit() {
    let (mut cpu, mut bus) = make();
    enter_native(&mut cpu, false, true);
    cpu.sp = 0x01FF;
    cpu.x = 0xABCD;
    bus.load(0x0000, &[0xDA, 0xA2, 0x00, 0x00, 0xFA]); // PHX, LDX #$0000, PLX
    cpu.pc = 0x0000;
    cpu.step(&mut bus); // PHX
    cpu.step(&mut bus); // LDX #$0000
    assert_eq!(cpu.x, 0x0000);
    cpu.step(&mut bus); // PLX
    assert_eq!(cpu.x, 0xABCD);
}

// ========================================================================
// Mode switching
// ========================================================================

#[test]
fn test_xce_to_native() {
    let (mut cpu, mut bus) = make();
    assert!(cpu.emulation);
    cpu.set_flag(FLAG_C, false); // C=0 => XCE will set E=0 (native)
    bus.load(0x0000, &[0xFB]); // XCE
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(!cpu.emulation);
    assert!(cpu.get_flag(FLAG_C)); // old E=1 moved to C
}

#[test]
fn test_xce_to_emulation() {
    let (mut cpu, mut bus) = make();
    cpu.emulation = false;
    cpu.p &= !(FLAG_M | FLAG_X); // 16-bit mode
    cpu.x = 0x1234;
    cpu.y = 0x5678;
    cpu.sp = 0x0200;
    cpu.set_flag(FLAG_C, true); // C=1 => XCE will set E=1 (emulation)
    bus.load(0x0000, &[0xFB]); // XCE
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(cpu.emulation);
    assert!(!cpu.get_flag(FLAG_C)); // old E=0 moved to C
                                    // X/Y high bytes zeroed
    assert_eq!(cpu.x, 0x0034);
    assert_eq!(cpu.y, 0x0078);
    // SP forced to page $01
    assert_eq!(cpu.sp & 0xFF00, 0x0100);
    // M and X flags forced
    assert!(cpu.get_flag(FLAG_M));
    assert!(cpu.get_flag(FLAG_X));
}

#[test]
fn test_rep_sep() {
    let (mut cpu, mut bus) = make();
    cpu.emulation = false;
    cpu.p = FLAG_M | FLAG_X | FLAG_I; // Start with M=1, X=1, I=1

    // REP #$30 => clear M and X flags
    bus.load(0x0000, &[0xC2, 0x30]); // REP #$30
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(!cpu.get_flag(FLAG_M));
    assert!(!cpu.get_flag(FLAG_X));
    assert!(cpu.get_flag(FLAG_I)); // I not cleared

    // SEP #$20 => set M flag
    bus.load(0x0002, &[0xE2, 0x20]);
    cpu.step(&mut bus);
    assert!(cpu.get_flag(FLAG_M));
    assert!(!cpu.get_flag(FLAG_X));
}

#[test]
fn test_sep_zeroes_index_high() {
    let (mut cpu, mut bus) = make();
    cpu.emulation = false;
    cpu.p &= !(FLAG_M | FLAG_X); // 16-bit
    cpu.x = 0x1234;
    cpu.y = 0x5678;

    // SEP #$10 => set X flag (8-bit index)
    bus.load(0x0000, &[0xE2, 0x10]);
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(cpu.get_flag(FLAG_X));
    assert_eq!(cpu.x, 0x0034); // high byte zeroed
    assert_eq!(cpu.y, 0x0078);
}

// ========================================================================
// New 65C816 instructions
// ========================================================================

#[test]
fn test_xba() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x1234;
    bus.load(0x0000, &[0xEB]); // XBA
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.c, 0x3412);
    // N/Z based on new low byte (0x12)
    assert!(!cpu.get_flag(FLAG_Z));
    assert!(!cpu.get_flag(FLAG_N));
}

#[test]
fn test_xba_zero_low() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x0042;
    bus.load(0x0000, &[0xEB]); // XBA
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.c, 0x4200);
    assert!(cpu.get_flag(FLAG_Z)); // new low byte is 0
}

#[test]
fn test_tcd_tdc() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x2000;
    bus.load(0x0000, &[0x5B, 0x7B]); // TCD, TDC
    cpu.pc = 0x0000;
    cpu.step(&mut bus); // TCD: C -> D
    assert_eq!(cpu.d, 0x2000);
    cpu.d = 0x3000;
    cpu.step(&mut bus); // TDC: D -> C
    assert_eq!(cpu.c, 0x3000);
}

#[test]
fn test_tcs_tsc() {
    let (mut cpu, mut bus) = make();
    cpu.emulation = false;
    cpu.c = 0x1FF0;
    bus.load(0x0000, &[0x1B]); // TCS
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.sp, 0x1FF0);

    bus.load(0x0001, &[0x3B]); // TSC
    cpu.step(&mut bus);
    assert_eq!(cpu.c, 0x1FF0);
}

#[test]
fn test_txy_tyx() {
    let (mut cpu, mut bus) = make();
    cpu.x = 0x42;
    cpu.y = 0x00;
    bus.load(0x0000, &[0x9B, 0xBB]); // TXY, TYX
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.y, 0x42);
    cpu.x = 0x00;
    cpu.step(&mut bus);
    assert_eq!(cpu.x, 0x42);
}

#[test]
fn test_phb_plb() {
    let (mut cpu, mut bus) = make();
    cpu.sp = 0x01FF;
    cpu.dbr = 0x7E;
    bus.load(0x0000, &[0x8B, 0xAB]); // PHB, PLB
    cpu.pc = 0x0000;
    cpu.step(&mut bus); // PHB
    cpu.dbr = 0x00;
    cpu.step(&mut bus); // PLB
    assert_eq!(cpu.dbr, 0x7E);
}

#[test]
fn test_phd_pld() {
    let (mut cpu, mut bus) = make();
    cpu.sp = 0x01FF;
    cpu.d = 0x2100;
    bus.load(0x0000, &[0x0B, 0x2B]); // PHD, PLD
    cpu.pc = 0x0000;
    cpu.step(&mut bus); // PHD
    cpu.d = 0x0000;
    cpu.step(&mut bus); // PLD
    assert_eq!(cpu.d, 0x2100);
}

#[test]
fn test_phk() {
    let (mut cpu, mut bus) = make();
    cpu.sp = 0x01FF;
    cpu.pbr = 0x80;
    bus.load(0x80_0000, &[0x4B]); // PHK (code in bank $80)
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(bus.mem[0x01FF], 0x80);
}

#[test]
fn test_pea() {
    let (mut cpu, mut bus) = make();
    cpu.sp = 0x01FF;
    bus.load(0x0000, &[0xF4, 0x34, 0x12]); // PEA $1234
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    // Stack should have $1234 (hi then lo)
    assert_eq!(bus.mem[0x01FF], 0x12);
    assert_eq!(bus.mem[0x01FE], 0x34);
}

// ---- TRB / TSB ----

#[test]
fn test_trb_8bit() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x00F0;
    bus.mem[0x10] = 0xFF;
    bus.load(0x0000, &[0x14, 0x10]); // TRB $10
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(bus.mem[0x10], 0x0F); // $FF & ~$F0
    assert!(!cpu.get_flag(FLAG_Z)); // $F0 & $FF != 0
}

#[test]
fn test_tsb_8bit() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x00F0;
    bus.mem[0x10] = 0x0F;
    bus.load(0x0000, &[0x04, 0x10]); // TSB $10
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(bus.mem[0x10], 0xFF); // $0F | $F0
    assert!(cpu.get_flag(FLAG_Z)); // $F0 & $0F == 0
}

// ---- Block move ----

#[test]
fn test_mvn() {
    let (mut cpu, mut bus) = make();
    for i in 0..3u8 {
        bus.mem[0x1000 + i as usize] = 0x10 + i;
    }
    cpu.x = 0x1000;
    cpu.y = 0x2000;
    cpu.c = 0x0002;

    bus.load(0x0000, &[0x54, 0x00, 0x00]);
    cpu.pc = 0x0000;
    // MVN does one byte per step and rewinds PC by 3 until C wraps.
    // Loop until the instruction has fully completed.
    while cpu.c != 0xFFFF {
        cpu.step(&mut bus);
    }

    assert_eq!(bus.mem[0x2000], 0x10);
    assert_eq!(bus.mem[0x2001], 0x11);
    assert_eq!(bus.mem[0x2002], 0x12);
    assert_eq!(cpu.c, 0xFFFF);
    assert_eq!(cpu.pc, 0x0003);
}

#[test]
fn test_mvp() {
    let (mut cpu, mut bus) = make();
    for i in 0..3u8 {
        bus.mem[0x1000 + i as usize] = 0xA0 + i;
    }
    cpu.x = 0x1002;
    cpu.y = 0x2002;
    cpu.c = 0x0002;

    bus.load(0x0000, &[0x44, 0x00, 0x00]);
    cpu.pc = 0x0000;
    while cpu.c != 0xFFFF {
        cpu.step(&mut bus);
    }

    assert_eq!(bus.mem[0x2000], 0xA0);
    assert_eq!(bus.mem[0x2001], 0xA1);
    assert_eq!(bus.mem[0x2002], 0xA2);
    assert_eq!(cpu.c, 0xFFFF);
    assert_eq!(cpu.pc, 0x0003);
}

// ========================================================================
// Long addressing
// ========================================================================

#[test]
fn test_lda_absolute_long() {
    let (mut cpu, mut bus) = make();
    bus.mem[0x7E_2000] = 0xAB;
    // LDA $7E2000 (opcode $AF)
    bus.load(0x0000, &[0xAF, 0x00, 0x20, 0x7E]);
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0xAB);
}

#[test]
fn test_sta_absolute_long() {
    let (mut cpu, mut bus) = make();
    cpu.c = 0x0055;
    bus.load(0x0000, &[0x8F, 0x00, 0x30, 0x7E]); // STA $7E3000
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(bus.mem[0x7E_3000], 0x55);
}

#[test]
fn test_jml_absolute_long() {
    let (mut cpu, mut bus) = make();
    // JML $018000 (opcode $5C)
    bus.load(0x0000, &[0x5C, 0x00, 0x80, 0x01]);
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x8000);
    assert_eq!(cpu.pbr, 0x01);
}

#[test]
fn test_jsl_rtl() {
    let (mut cpu, mut bus) = make();
    cpu.sp = 0x01FF;
    // JSL $018000 (opcode $22)
    bus.load(0x0000, &[0x22, 0x00, 0x80, 0x01]);
    bus.load(0x01_8000, &[0x6B]); // RTL
    cpu.pc = 0x0000;
    cpu.pbr = 0x00;
    cpu.step(&mut bus); // JSL
    assert_eq!(cpu.pc, 0x8000);
    assert_eq!(cpu.pbr, 0x01);
    cpu.step(&mut bus); // RTL
    assert_eq!(cpu.pc, 0x0004);
    assert_eq!(cpu.pbr, 0x00);
}

// ---- Stack relative addressing ----

#[test]
fn test_lda_stack_relative() {
    let (mut cpu, mut bus) = make();
    cpu.sp = 0x01F0;
    bus.mem[0x01F3] = 0x77; // SP + 3
    bus.load(0x0000, &[0xA3, 0x03]); // LDA $03,S
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x77);
}

// ---- Direct Page ----

#[test]
fn test_lda_direct_page_relocated() {
    let (mut cpu, mut bus) = make();
    cpu.d = 0x2100; // DP relocated
    bus.mem[0x2110] = 0x99;
    bus.load(0x0000, &[0xA5, 0x10]); // LDA $10 (DP)
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x99);
}

// ========================================================================
// Interrupts
// ========================================================================

#[test]
fn test_nmi_emulation_mode() {
    let (mut cpu, mut bus) = make();
    cpu.sp = 0x01FF;
    cpu.pc = 0x1234;
    // Set NMI vector in emulation mode
    bus.mem[interrupts::EMU_NMI_VECTOR as usize] = 0x00;
    bus.mem[(interrupts::EMU_NMI_VECTOR + 1) as usize] = 0x80; // $8000
    cpu.nmi_pending = true;

    bus.load(0x1234, &[0xEA]); // NOP (won't execute, NMI takes priority)
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x8000);
    assert!(cpu.get_flag(FLAG_I));
    assert_eq!(cycles, 7);
}

#[test]
fn test_nmi_native_mode() {
    let (mut cpu, mut bus) = make();
    cpu.emulation = false;
    cpu.p = FLAG_M | FLAG_X;
    cpu.sp = 0x01FF;
    cpu.pc = 0x1234;
    cpu.pbr = 0x02;
    bus.mem[interrupts::NAT_NMI_VECTOR as usize] = 0x00;
    bus.mem[(interrupts::NAT_NMI_VECTOR + 1) as usize] = 0x90; // $9000
    cpu.nmi_pending = true;

    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x9000);
    assert_eq!(cpu.pbr, 0x00); // PBR set to 0 on interrupt
    assert!(cpu.get_flag(FLAG_I));
    assert_eq!(cycles, 8);
}

#[test]
fn test_irq_masked() {
    let (mut cpu, mut bus) = make();
    cpu.sp = 0x01FF;
    cpu.pc = 0x0000;
    cpu.set_flag(FLAG_I, true);
    cpu.irq_pending = true;

    bus.load(0x0000, &[0xEA]); // NOP
    cpu.step(&mut bus);
    // IRQ should be masked, NOP executed
    assert_eq!(cpu.pc, 0x0001);
}

#[test]
fn test_irq_unmasked() {
    let (mut cpu, mut bus) = make();
    cpu.sp = 0x01FF;
    cpu.pc = 0x1000;
    cpu.set_flag(FLAG_I, false);
    cpu.irq_pending = true;
    bus.mem[interrupts::EMU_IRQ_VECTOR as usize] = 0x00;
    bus.mem[(interrupts::EMU_IRQ_VECTOR + 1) as usize] = 0xC0;

    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0xC000);
    assert_eq!(cycles, 7);
}

#[test]
fn test_brk_emulation() {
    let (mut cpu, mut bus) = make();
    cpu.sp = 0x01FF;
    cpu.pc = 0x0000;
    bus.mem[interrupts::EMU_IRQ_VECTOR as usize] = 0x00;
    bus.mem[(interrupts::EMU_IRQ_VECTOR + 1) as usize] = 0xD0;
    bus.load(0x0000, &[0x00, 0x00]); // BRK
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0xD000);
}

#[test]
fn test_brk_native() {
    let (mut cpu, mut bus) = make();
    cpu.emulation = false;
    cpu.p = FLAG_M | FLAG_X;
    cpu.sp = 0x01FF;
    cpu.pc = 0x0000;
    cpu.pbr = 0x01;
    bus.mem[interrupts::NAT_BRK_VECTOR as usize] = 0x00;
    bus.mem[(interrupts::NAT_BRK_VECTOR + 1) as usize] = 0xE0;
    bus.load(0x01_0000, &[0x00, 0x00]); // BRK
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0xE000);
    assert_eq!(cpu.pbr, 0x00);
}

#[test]
fn test_cop_native() {
    let (mut cpu, mut bus) = make();
    cpu.emulation = false;
    cpu.p = FLAG_M | FLAG_X;
    cpu.sp = 0x01FF;
    cpu.pc = 0x0000;
    bus.mem[interrupts::NAT_COP_VECTOR as usize] = 0x00;
    bus.mem[(interrupts::NAT_COP_VECTOR + 1) as usize] = 0xF0;
    bus.load(0x0000, &[0x02, 0x00]); // COP
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0xF000);
}

#[test]
fn test_rti_emulation() {
    let (mut cpu, mut bus) = make();
    cpu.sp = 0x01FC;
    // Manually push P, PCL, PCH onto stack
    bus.mem[0x01FD] = FLAG_C | FLAG_M | FLAG_X; // P
    bus.mem[0x01FE] = 0x34; // PCL
    bus.mem[0x01FF] = 0x12; // PCH
    bus.load(0x0000, &[0x40]); // RTI
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x1234);
    assert!(cpu.get_flag(FLAG_C));
}

#[test]
fn test_rti_native() {
    let (mut cpu, mut bus) = make();
    cpu.emulation = false;
    cpu.p = FLAG_M | FLAG_X;
    cpu.sp = 0x01FB;
    // Stack: P, PCL, PCH, PBR
    bus.mem[0x01FC] = FLAG_M | FLAG_X | FLAG_C; // P
    bus.mem[0x01FD] = 0x00; // PCL
    bus.mem[0x01FE] = 0x80; // PCH
    bus.mem[0x01FF] = 0x02; // PBR
    bus.load(0x0000, &[0x40]); // RTI
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x8000);
    assert_eq!(cpu.pbr, 0x02);
    assert!(cpu.get_flag(FLAG_C));
}

// ========================================================================
// Reset
// ========================================================================

#[test]
fn test_reset() {
    let (mut cpu, mut bus) = make();
    bus.mem[0xFFFC] = 0x00;
    bus.mem[0xFFFD] = 0x80;
    cpu.reset_with_bus(&mut bus);
    assert_eq!(cpu.pc, 0x8000);
    assert!(cpu.emulation);
    assert_eq!(cpu.pbr, 0x00);
    assert_eq!(cpu.dbr, 0x00);
    assert_eq!(cpu.d, 0x0000);
}

// ========================================================================
// Disassembler
// ========================================================================

// The output follows the format bsnes-plus's debugger writes: lowercase
// mnemonics and lowercase suffixes (`,x` / `,y` / `,s`), uppercase hex
// digits, and a `mvn $src, $dst` ordering for block moves. See the
// doc-comment at the top of `src/disasm.rs`.

#[test]
fn test_disasm_lda_immediate_8bit() {
    let (mut _cpu, mut bus) = make();
    bus.load(0x0000, &[0xA9, 0x42]);
    let state = DisasmState {
        emulation: true,
        flag_m: true,
        flag_x: true,
    };
    let (text, size) = disassemble_at(&mut bus, 0x0000, &state);
    assert_eq!(text, "lda #$42");
    assert_eq!(size, 2);
}

#[test]
fn test_disasm_lda_immediate_16bit() {
    let (mut _cpu, mut bus) = make();
    bus.load(0x0000, &[0xA9, 0x34, 0x12]);
    let state = DisasmState {
        emulation: false,
        flag_m: false,
        flag_x: true,
    };
    let (text, size) = disassemble_at(&mut bus, 0x0000, &state);
    assert_eq!(text, "lda #$1234");
    assert_eq!(size, 3);
}

#[test]
fn test_disasm_absolute_long() {
    let (mut _cpu, mut bus) = make();
    bus.load(0x0000, &[0xAF, 0x00, 0x20, 0x7E]);
    let state = DisasmState {
        emulation: false,
        flag_m: true,
        flag_x: true,
    };
    let (text, size) = disassemble_at(&mut bus, 0x0000, &state);
    assert_eq!(text, "lda $7E2000");
    assert_eq!(size, 4);
}

#[test]
fn test_disasm_stack_relative() {
    let (mut _cpu, mut bus) = make();
    bus.load(0x0000, &[0xA3, 0x05]);
    let state = DisasmState {
        emulation: false,
        flag_m: true,
        flag_x: true,
    };
    let (text, _size) = disassemble_at(&mut bus, 0x0000, &state);
    assert_eq!(text, "lda $05,s");
}

#[test]
fn test_disasm_dp_indirect_long() {
    let (mut _cpu, mut bus) = make();
    bus.load(0x0000, &[0xA7, 0x10]);
    let state = DisasmState {
        emulation: false,
        flag_m: true,
        flag_x: true,
    };
    let (text, _size) = disassemble_at(&mut bus, 0x0000, &state);
    assert_eq!(text, "lda [$10]");
}

#[test]
fn test_disasm_block_move() {
    let (mut _cpu, mut bus) = make();
    // Bytes after the opcode: dst_bank first ($7E), src_bank second ($7F).
    // The format prints them in `src, dst` order, as the WDC mnemonic reads.
    bus.load(0x0000, &[0x54, 0x7E, 0x7F]);
    let state = DisasmState {
        emulation: true,
        flag_m: true,
        flag_x: true,
    };
    let (text, size) = disassemble_at(&mut bus, 0x0000, &state);
    assert_eq!(text, "mvn $7F, $7E");
    assert_eq!(size, 3);
}

#[test]
fn test_disasm_brl() {
    let (mut _cpu, mut bus) = make();
    bus.load(0x0000, &[0x82, 0xFD, 0xFF]); // BRL -3 (offset $FFFD)
    let state = DisasmState {
        emulation: false,
        flag_m: true,
        flag_x: true,
    };
    let (text, _size) = disassemble_at(&mut bus, 0x0000, &state);
    assert_eq!(text, "brl $0000");
}

// ========================================================================
// Disassembler, systematic coverage (256 opcodes x M/X flag combinations)
// ========================================================================

/// Disassemble `bytes` at address 0 with the given flag state and return
/// the text. Helper that keeps each per-opcode case readable.
fn disasm_at_zero(bytes: &[u8], emulation: bool, m: bool, x: bool) -> String {
    let (mut _cpu, mut bus) = make();
    bus.load(0x0000, bytes);
    let state = DisasmState {
        emulation,
        flag_m: m,
        flag_x: x,
    };
    let (text, _size) = disassemble_at(&mut bus, 0x0000, &state);
    text
}

/// Same as [`disasm_at_zero`] but also returns the byte size consumed.
fn disasm_at_zero_with_size(bytes: &[u8], emulation: bool, m: bool, x: bool) -> (String, u8) {
    let (mut _cpu, mut bus) = make();
    bus.load(0x0000, bytes);
    let state = DisasmState {
        emulation,
        flag_m: m,
        flag_x: x,
    };
    disassemble_at(&mut bus, 0x0000, &state)
}

/// All 256 opcodes: assert the mnemonic our disassembler emits is the
/// 65C816's (checked against bsnes-plus's opcode listing). Catches any
/// regression where an `OPCODE_TABLE` entry gets the wrong `Mnemonic`.
///
/// The only intentional deviation from bsnes-plus is opcode `0xDC`:
/// bsnes-plus prints it as `jmp [$abs]`, snes9x and WDC print it as
/// `jml [$abs]`. We follow the WDC convention because the underlying
/// instruction is a JuMp Long (24-bit indirect).
#[test]
fn test_disasm_all_256_opcodes_mnemonic() {
    // (opcode, expected_lowercase_mnemonic)
    // Checked against bsnes-plus's opcode listing.
    let expected: [&str; 256] = [
        // 0x00..0x0F
        "brk", "ora", "cop", "ora", "tsb", "ora", "asl", "ora", "php", "ora", "asl", "phd", "tsb",
        "ora", "asl", "ora", // 0x10..0x1F
        "bpl", "ora", "ora", "ora", "trb", "ora", "asl", "ora", "clc", "ora", "inc", "tcs", "trb",
        "ora", "asl", "ora", // 0x20..0x2F
        "jsr", "and", "jsl", "and", "bit", "and", "rol", "and", "plp", "and", "rol", "pld", "bit",
        "and", "rol", "and", // 0x30..0x3F
        "bmi", "and", "and", "and", "bit", "and", "rol", "and", "sec", "and", "dec", "tsc", "bit",
        "and", "rol", "and", // 0x40..0x4F
        "rti", "eor", "wdm", "eor", "mvp", "eor", "lsr", "eor", "pha", "eor", "lsr", "phk", "jmp",
        "eor", "lsr", "eor", // 0x50..0x5F
        "bvc", "eor", "eor", "eor", "mvn", "eor", "lsr", "eor", "cli", "eor", "phy", "tcd", "jml",
        "eor", "lsr", "eor", // 0x60..0x6F
        "rts", "adc", "per", "adc", "stz", "adc", "ror", "adc", "pla", "adc", "ror", "rtl", "jmp",
        "adc", "ror", "adc", // 0x70..0x7F
        "bvs", "adc", "adc", "adc", "stz", "adc", "ror", "adc", "sei", "adc", "ply", "tdc", "jmp",
        "adc", "ror", "adc", // 0x80..0x8F
        "bra", "sta", "brl", "sta", "sty", "sta", "stx", "sta", "dey", "bit", "txa", "phb", "sty",
        "sta", "stx", "sta", // 0x90..0x9F
        "bcc", "sta", "sta", "sta", "sty", "sta", "stx", "sta", "tya", "sta", "txs", "txy", "stz",
        "sta", "stz", "sta", // 0xA0..0xAF
        "ldy", "lda", "ldx", "lda", "ldy", "lda", "ldx", "lda", "tay", "lda", "tax", "plb", "ldy",
        "lda", "ldx", "lda", // 0xB0..0xBF
        "bcs", "lda", "lda", "lda", "ldy", "lda", "ldx", "lda", "clv", "lda", "tsx", "tyx", "ldy",
        "lda", "ldx", "lda", // 0xC0..0xCF
        "cpy", "cmp", "rep", "cmp", "cpy", "cmp", "dec", "cmp", "iny", "cmp", "dex", "wai", "cpy",
        "cmp", "dec", "cmp", // 0xD0..0xDF
        // 0xDC: we emit `jml` (WDC/snes9x convention) where bsnes-plus emits `jmp`.
        "bne", "cmp", "cmp", "cmp", "pei", "cmp", "dec", "cmp", "cld", "cmp", "phx", "stp", "jml",
        "cmp", "dec", "cmp", // 0xE0..0xEF
        "cpx", "sbc", "sep", "sbc", "cpx", "sbc", "inc", "sbc", "inx", "sbc", "nop", "xba", "cpx",
        "sbc", "inc", "sbc", // 0xF0..0xFF
        "beq", "sbc", "sbc", "sbc", "pea", "sbc", "inc", "sbc", "sed", "sbc", "plx", "xce", "jsr",
        "sbc", "inc", "sbc",
    ];

    for (opcode, &want_mnem) in expected.iter().enumerate() {
        // Pad with non-zero operands so the mnemonic survives a zero-stripping
        // formatter bug; emulation mode + 8-bit M/X gives the smallest
        // operand size so the read at addr+1..3 stays in bounds.
        let text = disasm_at_zero(
            &[opcode as u8, 0x11, 0x22, 0x33],
            /* emulation */ true,
            /* M */ true,
            /* X */ true,
        );
        let got_mnem = text.split_whitespace().next().unwrap_or("");
        assert_eq!(
            got_mnem, want_mnem,
            "opcode {:02X}: expected mnemonic `{}`, got `{}` (full text: `{}`)",
            opcode, want_mnem, got_mnem, text
        );
    }
}

/// M flag controls the operand width of the `Immediate` addressing mode
/// (8 opcodes: ORA / AND / EOR / ADC / BIT / LDA / CMP / SBC immediate).
/// With M=1 (8-bit accumulator) the operand is 1 byte; with M=0 (16-bit)
/// it is 2 bytes printed in little-endian-decoded form.
#[test]
fn test_disasm_immediate_m_flag_affects_size() {
    // (opcode, mnem) for every accumulator-immediate variant.
    let cases: [(u8, &str); 8] = [
        (0x09, "ora"),
        (0x29, "and"),
        (0x49, "eor"),
        (0x69, "adc"),
        (0x89, "bit"),
        (0xA9, "lda"),
        (0xC9, "cmp"),
        (0xE9, "sbc"),
    ];

    for (opcode, mnem) in cases {
        // M=1 (8-bit): operand is `#$11`.
        let (text8, size8) =
            disasm_at_zero_with_size(&[opcode, 0x11, 0x22], false, /* M */ true, true);
        assert_eq!(text8, format!("{} #$11", mnem), "opcode {:02X} M=1", opcode);
        assert_eq!(size8, 2, "opcode {:02X} M=1 size", opcode);

        // M=0 (16-bit): operand is `#$2211`.
        let (text16, size16) =
            disasm_at_zero_with_size(&[opcode, 0x11, 0x22], false, /* M */ false, true);
        assert_eq!(
            text16,
            format!("{} #$2211", mnem),
            "opcode {:02X} M=0",
            opcode
        );
        assert_eq!(size16, 3, "opcode {:02X} M=0 size", opcode);
    }
}

/// X flag controls the operand width of the `ImmediateX` mode (4 opcodes:
/// LDY / LDX / CPY / CPX immediate). Emulation mode forces X=1 so the
/// 16-bit form is reachable only in native (E=0).
#[test]
fn test_disasm_immediatex_x_flag_affects_size() {
    let cases: [(u8, &str); 4] = [(0xA0, "ldy"), (0xA2, "ldx"), (0xC0, "cpy"), (0xE0, "cpx")];

    for (opcode, mnem) in cases {
        // X=1 (8-bit): operand is `#$11`.
        let (text8, size8) =
            disasm_at_zero_with_size(&[opcode, 0x11, 0x22], false, true, /* X */ true);
        assert_eq!(text8, format!("{} #$11", mnem), "opcode {:02X} X=1", opcode);
        assert_eq!(size8, 2, "opcode {:02X} X=1 size", opcode);

        // X=0 (16-bit): operand is `#$2211`.
        let (text16, size16) =
            disasm_at_zero_with_size(&[opcode, 0x11, 0x22], false, true, /* X */ false);
        assert_eq!(
            text16,
            format!("{} #$2211", mnem),
            "opcode {:02X} X=0",
            opcode
        );
        assert_eq!(size16, 3, "opcode {:02X} X=0 size", opcode);
    }
}

/// REP (`0xC2`) and SEP (`0xE2`) take a fixed 8-bit flag mask. Unlike the
/// accumulator-/index-immediate opcodes above, their operand width does
/// NOT track the M/X flags, bsnes-plus disassembles them as `rep #$30`
/// even in native 16-bit mode. A table entry giving them the `Immediate`
/// mode would widen the operand to 2 bytes when M=0 and throw off PC/byte
/// alignment for every later traced instruction.
#[test]
fn test_disasm_rep_sep_fixed_byte_immediate() {
    for (opcode, mnem) in [(0xC2u8, "rep"), (0xE2u8, "sep")] {
        // The classic `rep #$30` / `sep #$20` flag masks, with a trailing
        // byte that must NOT be absorbed into the operand.
        for (mask, m, x) in [
            (0x30u8, false, false), // native, 16-bit A and X
            (0x20u8, true, false),
            (0x10u8, false, true),
            (0xFFu8, true, true), // 8-bit A and X
        ] {
            let (text, size) =
                disasm_at_zero_with_size(&[opcode, mask, 0xAB], /* emulation */ false, m, x);
            assert_eq!(
                text,
                format!("{} #${:02X}", mnem, mask),
                "opcode {:02X} M={} X={} must stay 8-bit",
                opcode,
                m,
                x
            );
            assert_eq!(
                size, 2,
                "opcode {:02X} M={} X={} must consume exactly 2 bytes",
                opcode, m, x
            );
        }
    }
}

/// Emulation mode forces M=1 and X=1 inside the disassembler too, so both
/// `Immediate` and `ImmediateX` collapse to the 8-bit form even when the
/// caller's `DisasmState.flag_m` / `flag_x` are clear.
#[test]
fn test_disasm_emulation_forces_8bit_immediate() {
    // LDA #imm in emulation with caller-supplied M=0 still emits 8-bit.
    let (text, size) =
        disasm_at_zero_with_size(&[0xA9, 0x11, 0x22], /* emu */ true, false, false);
    assert_eq!(text, "lda #$11");
    assert_eq!(size, 2);

    // LDX #imm in emulation with caller-supplied X=0 still emits 8-bit.
    let (text, size) =
        disasm_at_zero_with_size(&[0xA2, 0x11, 0x22], /* emu */ true, false, false);
    assert_eq!(text, "ldx #$11");
    assert_eq!(size, 2);
}

/// Shape coverage for every addressing mode. Uses a representative opcode
/// per mode and pads operand bytes with the fixed pattern `0x11, 0x22,
/// 0x33` so the expected text is unambiguous.
#[test]
fn test_disasm_addressing_mode_shapes() {
    // 8-bit M/X to keep immediate-shaped modes at 1-byte operands.
    let m8 = true;
    let x8 = true;

    // Implied family, `clc` (0x18) is Implied; `asl A` (0x0A) is
    // Accumulator. Both emit just the mnemonic in our (and bsnes-plus')
    // format.
    assert_eq!(disasm_at_zero(&[0x18], false, m8, x8), "clc");
    assert_eq!(disasm_at_zero(&[0x0A], false, m8, x8), "asl");

    // Direct-page family: $11 padding.
    assert_eq!(disasm_at_zero(&[0xA5, 0x11], false, m8, x8), "lda $11"); // Direct
    assert_eq!(disasm_at_zero(&[0xB5, 0x11], false, m8, x8), "lda $11,x"); // DirectX
    assert_eq!(disasm_at_zero(&[0xB6, 0x11], false, m8, x8), "ldx $11,y"); // DirectY
    assert_eq!(disasm_at_zero(&[0xB2, 0x11], false, m8, x8), "lda ($11)"); // DirectIndirect
    assert_eq!(disasm_at_zero(&[0xA7, 0x11], false, m8, x8), "lda [$11]"); // DirectIndirectLong
    assert_eq!(disasm_at_zero(&[0xA1, 0x11], false, m8, x8), "lda ($11,x)"); // DirectXIndirect
    assert_eq!(disasm_at_zero(&[0xB1, 0x11], false, m8, x8), "lda ($11),y"); // DirectIndirectY
    assert_eq!(disasm_at_zero(&[0xB7, 0x11], false, m8, x8), "lda [$11],y"); // DirectIndirectLongY

    // Absolute family: $11, $22 padding -> little-endian-decoded $2211.
    assert_eq!(
        disasm_at_zero(&[0xAD, 0x11, 0x22], false, m8, x8),
        "lda $2211"
    ); // Absolute
    assert_eq!(
        disasm_at_zero(&[0xBD, 0x11, 0x22], false, m8, x8),
        "lda $2211,x"
    ); // AbsoluteX
    assert_eq!(
        disasm_at_zero(&[0xB9, 0x11, 0x22], false, m8, x8),
        "lda $2211,y"
    ); // AbsoluteY
    assert_eq!(
        disasm_at_zero(&[0x6C, 0x11, 0x22], false, m8, x8),
        "jmp ($2211)"
    ); // AbsoluteIndirect
    assert_eq!(
        disasm_at_zero(&[0x7C, 0x11, 0x22], false, m8, x8),
        "jmp ($2211,x)"
    ); // AbsoluteXIndirect
    assert_eq!(
        disasm_at_zero(&[0xDC, 0x11, 0x22], false, m8, x8),
        "jml [$2211]"
    ); // AbsoluteIndirectLong (WDC mnemonic; bsnes-plus uses `jmp`)

    // Long family: $11, $22, $33 padding -> $332211.
    assert_eq!(
        disasm_at_zero(&[0xAF, 0x11, 0x22, 0x33], false, m8, x8),
        "lda $332211"
    ); // AbsoluteLong
    assert_eq!(
        disasm_at_zero(&[0xBF, 0x11, 0x22, 0x33], false, m8, x8),
        "lda $332211,x"
    ); // AbsoluteLongX

    // Stack-relative family.
    assert_eq!(disasm_at_zero(&[0xA3, 0x11], false, m8, x8), "lda $11,s"); // StackRelative
    assert_eq!(
        disasm_at_zero(&[0xB3, 0x11], false, m8, x8),
        "lda ($11,s),y"
    ); // StackRelativeIndirectY

    // Block move: 0x44 MVP, operands `dst=0x11, src=0x22` -> `mvp $22, $11`.
    assert_eq!(
        disasm_at_zero(&[0x44, 0x11, 0x22], false, m8, x8),
        "mvp $22, $11"
    );
}

/// Relative branches: target is `PC + 2 + offset_signed`. Tests both
/// directions plus the page-crossing edge.
#[test]
fn test_disasm_relative_branch_target() {
    let (mut _cpu, mut bus) = make();
    let state = DisasmState {
        emulation: false,
        flag_m: true,
        flag_x: true,
    };

    // BNE +0x10 at $1000 -> target = $1002 + $10 = $1012.
    bus.load(0x1000, &[0xD0, 0x10]);
    let (text, _size) = disassemble_at(&mut bus, 0x1000, &state);
    assert_eq!(text, "bne $1012");

    // BEQ -2 at $2000 -> target = $2002 + $FFFE = $2000.
    bus.load(0x2000, &[0xF0, 0xFE]);
    let (text, _size) = disassemble_at(&mut bus, 0x2000, &state);
    assert_eq!(text, "beq $2000");

    // BPL -0x80 at $3080 -> target = $3082 + $FF80 = $3002 (page cross).
    bus.load(0x3080, &[0x10, 0x80]);
    let (text, _size) = disassemble_at(&mut bus, 0x3080, &state);
    assert_eq!(text, "bpl $3002");

    // BCC +0x7F at $40F0 -> target = $40F2 + $7F = $4171 (page cross).
    bus.load(0x40F0, &[0x90, 0x7F]);
    let (text, _size) = disassemble_at(&mut bus, 0x40F0, &state);
    assert_eq!(text, "bcc $4171");
}

/// BRL/PER use a signed 16-bit offset; the target is `PC + 3 + offset_signed`
/// inside the current PBR.
#[test]
fn test_disasm_relative_long_target() {
    let (mut _cpu, mut bus) = make();
    let state = DisasmState {
        emulation: false,
        flag_m: true,
        flag_x: true,
    };

    // BRL +0x1000 at $0000 -> target = $0003 + $1000 = $1003.
    bus.load(0x0000, &[0x82, 0x00, 0x10]);
    let (text, _size) = disassemble_at(&mut bus, 0x0000, &state);
    assert_eq!(text, "brl $1003");

    // PER -3 at $8000 -> target = $8003 + $FFFD = $8000.
    bus.load(0x8000, &[0x62, 0xFD, 0xFF]);
    let (text, _size) = disassemble_at(&mut bus, 0x8000, &state);
    assert_eq!(text, "per $8000");
}

/// Operand-size table for every addressing mode. Confirms that the
/// instruction length reported by `disassemble_at` follows
/// `AddressingMode::operand_size(m8, x8)`. Picks one representative
/// opcode per mode.
#[test]
fn test_disasm_instruction_size_per_mode() {
    let m8 = true;
    let x8 = true;
    // (opcode, padding bytes, expected total size)
    let cases: [(u8, &[u8], u8); 22] = [
        (0x18, &[], 1),                 // Implied (clc)
        (0x0A, &[], 1),                 // Accumulator (asl)
        (0xA9, &[0x11], 2),             // Immediate (lda), M=1
        (0xA0, &[0x11], 2),             // ImmediateX (ldy), X=1
        (0xA5, &[0x11], 2),             // Direct
        (0xB5, &[0x11], 2),             // DirectX
        (0xB6, &[0x11], 2),             // DirectY
        (0xB2, &[0x11], 2),             // DirectIndirect
        (0xA7, &[0x11], 2),             // DirectIndirectLong
        (0xA1, &[0x11], 2),             // DirectXIndirect
        (0xB1, &[0x11], 2),             // DirectIndirectY
        (0xB7, &[0x11], 2),             // DirectIndirectLongY
        (0xAD, &[0x11, 0x22], 3),       // Absolute
        (0xBD, &[0x11, 0x22], 3),       // AbsoluteX
        (0xB9, &[0x11, 0x22], 3),       // AbsoluteY
        (0x6C, &[0x11, 0x22], 3),       // AbsoluteIndirect
        (0xDC, &[0x11, 0x22], 3),       // AbsoluteIndirectLong
        (0xAF, &[0x11, 0x22, 0x33], 4), // AbsoluteLong
        (0xA3, &[0x11], 2),             // StackRelative
        (0x44, &[0x11, 0x22], 3),       // BlockMove
        (0xD0, &[0x11], 2),             // Relative
        (0x82, &[0x11, 0x22], 3),       // RelativeLong
    ];

    for (opcode, padding, want_size) in cases {
        let mut bytes = vec![opcode];
        bytes.extend_from_slice(padding);
        let (_text, size) = disasm_at_zero_with_size(&bytes, false, m8, x8);
        assert_eq!(size, want_size, "opcode {:02X} size mismatch", opcode);
    }
}

// ========================================================================
// BCD mode (65C816 supports BCD unlike 2A03)
// ========================================================================

#[test]
fn test_adc_bcd_8bit() {
    let (mut cpu, mut bus) = make();
    cpu.set_flag(FLAG_D, true);
    cpu.set_flag(FLAG_C, false);
    cpu.c = 0x0019; // 19 BCD
    bus.load(0x0000, &[0x69, 0x01]); // ADC #$01
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x20); // 19+01=20 in BCD
    assert!(!cpu.get_flag(FLAG_C));
}

#[test]
fn test_adc_bcd_8bit_carry() {
    let (mut cpu, mut bus) = make();
    cpu.set_flag(FLAG_D, true);
    cpu.set_flag(FLAG_C, false);
    cpu.c = 0x0099;
    bus.load(0x0000, &[0x69, 0x01]); // ADC #$01
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x00); // 99+01=00 with carry
    assert!(cpu.get_flag(FLAG_C));
}

// ========================================================================
// Direct Page indirect addressing
// ========================================================================

#[test]
fn test_lda_dp_indirect() {
    let (mut cpu, mut bus) = make();
    cpu.d = 0x0000;
    cpu.dbr = 0x00;
    // Pointer at DP+$10 => $2000
    bus.mem[0x10] = 0x00;
    bus.mem[0x11] = 0x20;
    bus.mem[0x2000] = 0x77;
    bus.load(0x0000, &[0xB2, 0x10]); // LDA ($10)
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x77);
}

#[test]
fn test_lda_dp_indirect_long() {
    let (mut cpu, mut bus) = make();
    cpu.d = 0x0000;
    // 24-bit pointer at DP+$10 => $7E:2000
    bus.mem[0x10] = 0x00;
    bus.mem[0x11] = 0x20;
    bus.mem[0x12] = 0x7E;
    bus.mem[0x7E_2000] = 0x88;
    bus.load(0x0000, &[0xA7, 0x10]); // LDA [$10]
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a_lo(), 0x88);
}

// ========================================================================
// STZ (65C816 new instruction)
// ========================================================================

#[test]
fn test_stz_absolute() {
    let (mut cpu, mut bus) = make();
    bus.mem[0x2000] = 0xFF;
    bus.load(0x0000, &[0x9C, 0x00, 0x20]); // STZ $2000
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(bus.mem[0x2000], 0x00);
}
