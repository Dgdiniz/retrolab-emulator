//! Unit tests for the MOS 6502 CPU.

use crate::instructions::*;
use crate::Mos6502;
use retro_core::cpu::Addressable;

/// Simple 64KB flat memory bus for testing.
struct TestBus {
    mem: [u8; 0x10000],
}

impl TestBus {
    fn new() -> Self {
        Self { mem: [0; 0x10000] }
    }

    fn load(&mut self, addr: u16, data: &[u8]) {
        for (i, &b) in data.iter().enumerate() {
            self.mem[addr.wrapping_add(i as u16) as usize] = b;
        }
    }
}

impl Addressable for TestBus {
    fn read(&mut self, addr: u32) -> u8 {
        self.mem[(addr & 0xFFFF) as usize]
    }
    fn write(&mut self, addr: u32, val: u8) {
        self.mem[(addr & 0xFFFF) as usize] = val;
    }
}

fn make_cpu_bus() -> (Mos6502, TestBus) {
    (Mos6502::new(), TestBus::new())
}

// ====================================================================
// Load / Store / Transfer tests
// ====================================================================

#[test]
fn test_lda_immediate() {
    let (mut cpu, mut bus) = make_cpu_bus();
    bus.load(0x0000, &[0xA9, 0x42]); // LDA #$42
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x42);
    assert_eq!(cycles, 2);
    assert!(!cpu.get_flag(FLAG_Z));
    assert!(!cpu.get_flag(FLAG_N));
}

#[test]
fn test_lda_zero_flag() {
    let (mut cpu, mut bus) = make_cpu_bus();
    bus.load(0x0000, &[0xA9, 0x00]); // LDA #$00
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x00);
    assert!(cpu.get_flag(FLAG_Z));
    assert!(!cpu.get_flag(FLAG_N));
}

#[test]
fn test_lda_negative_flag() {
    let (mut cpu, mut bus) = make_cpu_bus();
    bus.load(0x0000, &[0xA9, 0x80]); // LDA #$80
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x80);
    assert!(!cpu.get_flag(FLAG_Z));
    assert!(cpu.get_flag(FLAG_N));
}

#[test]
fn test_ldx_immediate() {
    let (mut cpu, mut bus) = make_cpu_bus();
    bus.load(0x0000, &[0xA2, 0x10]); // LDX #$10
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.x, 0x10);
}

#[test]
fn test_ldy_immediate() {
    let (mut cpu, mut bus) = make_cpu_bus();
    bus.load(0x0000, &[0xA0, 0x20]); // LDY #$20
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.y, 0x20);
}

#[test]
fn test_sta_zeropage() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0x55;
    bus.load(0x0000, &[0x85, 0x10]); // STA $10
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(bus.mem[0x10], 0x55);
    assert_eq!(cycles, 3);
}

#[test]
fn test_stx_zeropage() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.x = 0xAA;
    bus.load(0x0000, &[0x86, 0x20]); // STX $20
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(bus.mem[0x20], 0xAA);
}

#[test]
fn test_sty_zeropage() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.y = 0xBB;
    bus.load(0x0000, &[0x84, 0x30]); // STY $30
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(bus.mem[0x30], 0xBB);
}

#[test]
fn test_tax() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0x42;
    bus.load(0x0000, &[0xAA]); // TAX
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.x, 0x42);
}

#[test]
fn test_tay() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0x42;
    bus.load(0x0000, &[0xA8]); // TAY
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.y, 0x42);
}

#[test]
fn test_txa() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.x = 0x99;
    bus.load(0x0000, &[0x8A]); // TXA
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x99);
}

#[test]
fn test_tya() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.y = 0x77;
    bus.load(0x0000, &[0x98]); // TYA
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x77);
}

#[test]
fn test_tsx() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.sp = 0xEF;
    bus.load(0x0000, &[0xBA]); // TSX
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.x, 0xEF);
}

#[test]
fn test_txs() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.x = 0xFE;
    bus.load(0x0000, &[0x9A]); // TXS
    cpu.pc = 0x0000;
    let old_p = cpu.p;
    cpu.step(&mut bus);
    assert_eq!(cpu.sp, 0xFE);
    assert_eq!(cpu.p, old_p); // TXS does not affect flags
}

// ====================================================================
// Addressing mode tests
// ====================================================================

#[test]
fn test_lda_zeropage() {
    let (mut cpu, mut bus) = make_cpu_bus();
    bus.mem[0x10] = 0x42;
    bus.load(0x0000, &[0xA5, 0x10]); // LDA $10
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x42);
    assert_eq!(cycles, 3);
}

#[test]
fn test_lda_zeropage_x() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.x = 0x05;
    bus.mem[0x15] = 0x99;
    bus.load(0x0000, &[0xB5, 0x10]); // LDA $10,X
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x99);
    assert_eq!(cycles, 4);
}

#[test]
fn test_lda_zeropage_x_wrap() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.x = 0x10;
    bus.mem[0x05] = 0xAB; // $F5 + $10 = $105 wraps to $05
    bus.load(0x0000, &[0xB5, 0xF5]); // LDA $F5,X
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0xAB);
}

#[test]
fn test_lda_absolute() {
    let (mut cpu, mut bus) = make_cpu_bus();
    bus.mem[0x1234] = 0x77;
    bus.load(0x0000, &[0xAD, 0x34, 0x12]); // LDA $1234
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x77);
    assert_eq!(cycles, 4);
}

#[test]
fn test_lda_absolute_x_no_page_cross() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.x = 0x02;
    bus.mem[0x1236] = 0x55;
    bus.load(0x0000, &[0xBD, 0x34, 0x12]); // LDA $1234,X
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x55);
    assert_eq!(cycles, 4); // no page cross
}

#[test]
fn test_lda_absolute_x_page_cross() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.x = 0xFF;
    bus.mem[0x1333] = 0x88; // $1234 + $FF = $1333
    bus.load(0x0000, &[0xBD, 0x34, 0x12]); // LDA $1234,X
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x88);
    assert_eq!(cycles, 5); // page cross penalty
}

#[test]
fn test_lda_indirect_x() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.x = 0x02;
    bus.mem[0x12] = 0x00; // pointer lo at ZP $10+$02
    bus.mem[0x13] = 0x30; // pointer hi
    bus.mem[0x3000] = 0xDD;
    bus.load(0x0000, &[0xA1, 0x10]); // LDA ($10,X)
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.a, 0xDD);
    assert_eq!(cycles, 6);
}

#[test]
fn test_lda_indirect_y_no_cross() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.y = 0x05;
    bus.mem[0x20] = 0x00; // pointer lo at ZP $20
    bus.mem[0x21] = 0x40; // pointer hi -> $4000
    bus.mem[0x4005] = 0xEE;
    bus.load(0x0000, &[0xB1, 0x20]); // LDA ($20),Y
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.a, 0xEE);
    assert_eq!(cycles, 5);
}

#[test]
fn test_lda_indirect_y_page_cross() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.y = 0xFF;
    bus.mem[0x20] = 0x80; // pointer -> $4080
    bus.mem[0x21] = 0x40;
    bus.mem[0x417F] = 0xCC; // $4080 + $FF = $417F (page cross)
    bus.load(0x0000, &[0xB1, 0x20]); // LDA ($20),Y
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.a, 0xCC);
    assert_eq!(cycles, 6); // page cross penalty
}

// ====================================================================
// Arithmetic tests
// ====================================================================

#[test]
fn test_adc_simple() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0x10;
    cpu.set_flag(FLAG_C, false);
    bus.load(0x0000, &[0x69, 0x20]); // ADC #$20
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x30);
    assert!(!cpu.get_flag(FLAG_C));
    assert!(!cpu.get_flag(FLAG_V));
}

#[test]
fn test_adc_carry_out() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0xFF;
    cpu.set_flag(FLAG_C, false);
    bus.load(0x0000, &[0x69, 0x01]); // ADC #$01
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x00);
    assert!(cpu.get_flag(FLAG_C));
    assert!(cpu.get_flag(FLAG_Z));
}

#[test]
fn test_adc_carry_in() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0x10;
    cpu.set_flag(FLAG_C, true);
    bus.load(0x0000, &[0x69, 0x20]); // ADC #$20
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x31);
}

#[test]
fn test_adc_overflow() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0x7F; // +127
    cpu.set_flag(FLAG_C, false);
    bus.load(0x0000, &[0x69, 0x01]); // ADC #$01
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x80); // -128, overflow!
    assert!(cpu.get_flag(FLAG_V));
    assert!(cpu.get_flag(FLAG_N));
}

#[test]
fn test_sbc_simple() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0x30;
    cpu.set_flag(FLAG_C, true); // no borrow
    bus.load(0x0000, &[0xE9, 0x10]); // SBC #$10
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x20);
    assert!(cpu.get_flag(FLAG_C)); // no borrow
}

#[test]
fn test_sbc_borrow() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0x10;
    cpu.set_flag(FLAG_C, true); // no borrow
    bus.load(0x0000, &[0xE9, 0x20]); // SBC #$20
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0xF0);
    assert!(!cpu.get_flag(FLAG_C)); // borrow occurred
    assert!(cpu.get_flag(FLAG_N));
}

#[test]
fn test_inc_zeropage() {
    let (mut cpu, mut bus) = make_cpu_bus();
    bus.mem[0x10] = 0x41;
    bus.load(0x0000, &[0xE6, 0x10]); // INC $10
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(bus.mem[0x10], 0x42);
    assert_eq!(cycles, 5);
}

#[test]
fn test_inc_wrap() {
    let (mut cpu, mut bus) = make_cpu_bus();
    bus.mem[0x10] = 0xFF;
    bus.load(0x0000, &[0xE6, 0x10]); // INC $10
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(bus.mem[0x10], 0x00);
    assert!(cpu.get_flag(FLAG_Z));
}

#[test]
fn test_dec_zeropage() {
    let (mut cpu, mut bus) = make_cpu_bus();
    bus.mem[0x10] = 0x42;
    bus.load(0x0000, &[0xC6, 0x10]); // DEC $10
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(bus.mem[0x10], 0x41);
}

#[test]
fn test_inx() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.x = 0x0F;
    bus.load(0x0000, &[0xE8]); // INX
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.x, 0x10);
}

#[test]
fn test_dex() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.x = 0x00;
    bus.load(0x0000, &[0xCA]); // DEX
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.x, 0xFF);
    assert!(cpu.get_flag(FLAG_N));
}

#[test]
fn test_iny() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.y = 0x0F;
    bus.load(0x0000, &[0xC8]); // INY
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.y, 0x10);
}

#[test]
fn test_dey() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.y = 0x01;
    bus.load(0x0000, &[0x88]); // DEY
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.y, 0x00);
    assert!(cpu.get_flag(FLAG_Z));
}

// ====================================================================
// Logical tests
// ====================================================================

#[test]
fn test_and_immediate() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0xFF;
    bus.load(0x0000, &[0x29, 0x0F]); // AND #$0F
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x0F);
}

#[test]
fn test_ora_immediate() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0xF0;
    bus.load(0x0000, &[0x09, 0x0F]); // ORA #$0F
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0xFF);
    assert!(cpu.get_flag(FLAG_N));
}

#[test]
fn test_eor_immediate() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0xFF;
    bus.load(0x0000, &[0x49, 0xFF]); // EOR #$FF
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x00);
    assert!(cpu.get_flag(FLAG_Z));
}

// ====================================================================
// Compare / BIT tests
// ====================================================================

#[test]
fn test_cmp_equal() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0x42;
    bus.load(0x0000, &[0xC9, 0x42]); // CMP #$42
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(cpu.get_flag(FLAG_Z));
    assert!(cpu.get_flag(FLAG_C));
    assert!(!cpu.get_flag(FLAG_N));
}

#[test]
fn test_cmp_greater() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0x50;
    bus.load(0x0000, &[0xC9, 0x40]); // CMP #$40
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(!cpu.get_flag(FLAG_Z));
    assert!(cpu.get_flag(FLAG_C));
}

#[test]
fn test_cmp_less() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0x10;
    bus.load(0x0000, &[0xC9, 0x20]); // CMP #$20
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(!cpu.get_flag(FLAG_Z));
    assert!(!cpu.get_flag(FLAG_C));
    assert!(cpu.get_flag(FLAG_N));
}

#[test]
fn test_cpx() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.x = 0x10;
    bus.load(0x0000, &[0xE0, 0x10]); // CPX #$10
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(cpu.get_flag(FLAG_Z));
    assert!(cpu.get_flag(FLAG_C));
}

#[test]
fn test_cpy() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.y = 0x20;
    bus.load(0x0000, &[0xC0, 0x10]); // CPY #$10
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(!cpu.get_flag(FLAG_Z));
    assert!(cpu.get_flag(FLAG_C));
}

#[test]
fn test_bit_zeropage() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0xFF;
    bus.mem[0x10] = 0xC0; // bits 7 and 6 set
    bus.load(0x0000, &[0x24, 0x10]); // BIT $10
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(!cpu.get_flag(FLAG_Z)); // A & $C0 = $C0, not zero
    assert!(cpu.get_flag(FLAG_N)); // bit 7 of memory
    assert!(cpu.get_flag(FLAG_V)); // bit 6 of memory
}

#[test]
fn test_bit_zero_result() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0x0F;
    bus.mem[0x10] = 0xF0;
    bus.load(0x0000, &[0x24, 0x10]); // BIT $10
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(cpu.get_flag(FLAG_Z)); // A & $F0 = $00
    assert!(cpu.get_flag(FLAG_N)); // bit 7 of memory
    assert!(cpu.get_flag(FLAG_V)); // bit 6 of memory
}

// ====================================================================
// Shift / Rotate tests
// ====================================================================

#[test]
fn test_asl_accumulator() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0x81;
    bus.load(0x0000, &[0x0A]); // ASL A
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x02);
    assert!(cpu.get_flag(FLAG_C));
    assert!(!cpu.get_flag(FLAG_N));
}

#[test]
fn test_lsr_accumulator() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0x03;
    bus.load(0x0000, &[0x4A]); // LSR A
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x01);
    assert!(cpu.get_flag(FLAG_C));
}

#[test]
fn test_rol_accumulator() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0x80;
    cpu.set_flag(FLAG_C, true);
    bus.load(0x0000, &[0x2A]); // ROL A
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x01); // old carry in, bit 7 out
    assert!(cpu.get_flag(FLAG_C));
}

#[test]
fn test_ror_accumulator() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0x01;
    cpu.set_flag(FLAG_C, true);
    bus.load(0x0000, &[0x6A]); // ROR A
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x80); // old carry into bit 7, bit 0 out
    assert!(cpu.get_flag(FLAG_C));
    assert!(cpu.get_flag(FLAG_N));
}

#[test]
fn test_asl_memory() {
    let (mut cpu, mut bus) = make_cpu_bus();
    bus.mem[0x10] = 0x40;
    bus.load(0x0000, &[0x06, 0x10]); // ASL $10
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(bus.mem[0x10], 0x80);
    assert_eq!(cycles, 5);
    assert!(!cpu.get_flag(FLAG_C));
    assert!(cpu.get_flag(FLAG_N));
}

// ====================================================================
// Branch tests
// ====================================================================

#[test]
fn test_bne_taken_same_page() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.set_flag(FLAG_Z, false);
    bus.load(0x0000, &[0xD0, 0x05]); // BNE +5 -> $0007
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0007);
    assert_eq!(cycles, 3); // 2 base + 1 taken
}

#[test]
fn test_bne_not_taken() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.set_flag(FLAG_Z, true);
    bus.load(0x0000, &[0xD0, 0x05]); // BNE +5
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0002);
    assert_eq!(cycles, 2); // not taken
}

#[test]
fn test_beq_taken() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.set_flag(FLAG_Z, true);
    bus.load(0x0000, &[0xF0, 0x10]); // BEQ +16 -> $0012
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0012);
    assert_eq!(cycles, 3);
}

#[test]
fn test_branch_backward() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.set_flag(FLAG_Z, false);
    bus.load(0x0010, &[0xD0, 0xFE]); // BNE -2 -> loops back to $0010
    cpu.pc = 0x0010;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0010);
    assert_eq!(cycles, 3);
}

#[test]
fn test_branch_page_cross() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.set_flag(FLAG_C, true);
    // Place BCS at $00FD so that a forward branch of +5 crosses from page $00 to page $01
    bus.load(0x00FD, &[0xB0, 0x05]); // BCS +5 -> $0104 (page cross)
    cpu.pc = 0x00FD;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0104);
    assert_eq!(cycles, 4); // 2 base + 1 taken + 1 page cross
}

// ====================================================================
// Stack tests
// ====================================================================

#[test]
fn test_pha_pla() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.a = 0x42;
    bus.load(0x0000, &[0x48, 0xA9, 0x00, 0x68]); // PHA; LDA #$00; PLA
    cpu.pc = 0x0000;

    cpu.step(&mut bus); // PHA
    assert_eq!(cpu.sp, 0xFC);

    cpu.step(&mut bus); // LDA #$00
    assert_eq!(cpu.a, 0x00);

    cpu.step(&mut bus); // PLA
    assert_eq!(cpu.a, 0x42);
    assert_eq!(cpu.sp, 0xFD);
}

#[test]
fn test_php_plp() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.p = FLAG_U; // clear all flags, unused set
    cpu.set_flag(FLAG_C, true);
    cpu.set_flag(FLAG_N, true);
    let expected = cpu.p | FLAG_B | FLAG_U; // PHP sets B and U

    bus.load(0x0000, &[0x08, 0x28]); // PHP; PLP
    cpu.pc = 0x0000;

    cpu.step(&mut bus); // PHP
    assert_eq!(bus.mem[0x01FD], expected);

    // Modify flags before PLP
    cpu.p = 0x00;
    cpu.step(&mut bus); // PLP
                        // PLP restores but always sets U and clears B
    assert!(cpu.get_flag(FLAG_C));
    assert!(cpu.get_flag(FLAG_N));
    assert!(cpu.get_flag(FLAG_U));
    assert!(!cpu.get_flag(FLAG_B));
}

// ====================================================================
// System instruction tests
// ====================================================================

#[test]
fn test_jmp_absolute() {
    let (mut cpu, mut bus) = make_cpu_bus();
    bus.load(0x0000, &[0x4C, 0x00, 0x80]); // JMP $8000
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x8000);
    assert_eq!(cycles, 3);
}

#[test]
fn test_jmp_indirect() {
    let (mut cpu, mut bus) = make_cpu_bus();
    bus.load(0x0000, &[0x6C, 0x00, 0x10]); // JMP ($1000)
    bus.mem[0x1000] = 0x00;
    bus.mem[0x1001] = 0x80;
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x8000);
    assert_eq!(cycles, 5);
}

#[test]
fn test_jmp_indirect_page_bug() {
    let (mut cpu, mut bus) = make_cpu_bus();
    bus.load(0x0000, &[0x6C, 0xFF, 0x10]); // JMP ($10FF)
    bus.mem[0x10FF] = 0x34;
    bus.mem[0x1100] = 0x99; // "correct" hi byte if no bug
    bus.mem[0x1000] = 0x12; // buggy hi byte (wraps within page)
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x1234); // should use $1000 for hi, not $1100
}

#[test]
fn test_jsr_rts() {
    let (mut cpu, mut bus) = make_cpu_bus();
    bus.load(0x0000, &[0x20, 0x00, 0x80]); // JSR $8000
    bus.load(0x8000, &[0x60]); // RTS
    cpu.pc = 0x0000;

    cpu.step(&mut bus); // JSR
    assert_eq!(cpu.pc, 0x8000);

    cpu.step(&mut bus); // RTS
    assert_eq!(cpu.pc, 0x0003); // returns to byte after JSR
}

#[test]
fn test_nop() {
    let (mut cpu, mut bus) = make_cpu_bus();
    bus.load(0x0000, &[0xEA]); // NOP
    cpu.pc = 0x0000;
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0001);
    assert_eq!(cycles, 2);
}

// ====================================================================
// Flag instruction tests
// ====================================================================

#[test]
fn test_flag_instructions() {
    let (mut cpu, mut bus) = make_cpu_bus();
    // SEC, CLC, SED, CLD, SEI, CLI
    bus.load(0x0000, &[0x38, 0x18, 0xF8, 0xD8, 0x78, 0x58]);
    cpu.pc = 0x0000;

    cpu.step(&mut bus); // SEC
    assert!(cpu.get_flag(FLAG_C));
    cpu.step(&mut bus); // CLC
    assert!(!cpu.get_flag(FLAG_C));
    cpu.step(&mut bus); // SED
    assert!(cpu.get_flag(FLAG_D));
    cpu.step(&mut bus); // CLD
    assert!(!cpu.get_flag(FLAG_D));
    cpu.step(&mut bus); // SEI
    assert!(cpu.get_flag(FLAG_I));
    cpu.step(&mut bus); // CLI
    assert!(!cpu.get_flag(FLAG_I));
}

#[test]
fn test_clv() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.set_flag(FLAG_V, true);
    bus.load(0x0000, &[0xB8]); // CLV
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert!(!cpu.get_flag(FLAG_V));
}

// ====================================================================
// Interrupt tests
// ====================================================================

#[test]
fn test_brk() {
    let (mut cpu, mut bus) = make_cpu_bus();
    // Set IRQ vector
    bus.mem[0xFFFE] = 0x00;
    bus.mem[0xFFFF] = 0x80;

    cpu.pc = 0x0200;
    cpu.p = FLAG_U; // only unused bit set
    bus.load(0x0200, &[0x00, 0x00]); // BRK (+ padding byte)

    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x8000);
    assert!(cpu.get_flag(FLAG_I));
    assert_eq!(cycles, 7);

    // Check that PC was pushed (BRK pushes PC+2)
    let pushed_hi = bus.mem[0x01FD];
    let pushed_lo = bus.mem[0x01FC];
    assert_eq!((pushed_hi as u16) << 8 | pushed_lo as u16, 0x0202);

    // Check that status was pushed with B flag set
    let pushed_p = bus.mem[0x01FB];
    assert!(pushed_p & FLAG_B != 0);
}

#[test]
fn test_rti() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.pc = 0x0000;

    // Set up stack as if an interrupt pushed P, PCL, PCH
    // Pull order: P from $01FB, PCL from $01FC, PCH from $01FD
    cpu.sp = 0xFA;
    bus.mem[0x01FB] = FLAG_C | FLAG_U; // saved P
    bus.mem[0x01FC] = 0x34; // saved PCL
    bus.mem[0x01FD] = 0x12; // saved PCH

    bus.load(0x0000, &[0x40]); // RTI
    cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x1234);
    assert!(cpu.get_flag(FLAG_C));
    assert!(cpu.get_flag(FLAG_U));
    assert!(!cpu.get_flag(FLAG_B)); // RTI clears B
}

#[test]
fn test_nmi() {
    let (mut cpu, mut bus) = make_cpu_bus();
    bus.mem[0xFFFA] = 0x00; // NMI vector lo
    bus.mem[0xFFFB] = 0x90; // NMI vector hi

    cpu.pc = 0x0200;
    cpu.p = FLAG_U;
    cpu.nmi_pending = true;

    bus.load(0x0200, &[0xEA]); // NOP (will be pre-empted by NMI)
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x9000);
    assert_eq!(cycles, 7);
    assert!(!cpu.nmi_pending);
}

#[test]
fn test_irq_when_enabled() {
    let (mut cpu, mut bus) = make_cpu_bus();
    bus.mem[0xFFFE] = 0x00;
    bus.mem[0xFFFF] = 0xA0;

    cpu.pc = 0x0200;
    cpu.p = FLAG_U; // I flag clear -> IRQ enabled
    cpu.irq_pending = true;

    bus.load(0x0200, &[0xEA]); // NOP (pre-empted)
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0xA000);
    assert_eq!(cycles, 7);
    assert!(cpu.get_flag(FLAG_I)); // IRQ sets I flag
}

#[test]
fn test_irq_when_disabled() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.pc = 0x0200;
    cpu.set_flag(FLAG_I, true); // IRQ disabled
    cpu.irq_pending = true;

    bus.load(0x0200, &[0xEA]); // NOP
    let cycles = cpu.step(&mut bus);
    assert_eq!(cpu.pc, 0x0201); // NOP executed, no interrupt
    assert_eq!(cycles, 2);
}

// ====================================================================
// Reset test
// ====================================================================

#[test]
fn test_reset_with_bus() {
    let (mut cpu, mut bus) = make_cpu_bus();
    bus.mem[0xFFFC] = 0x00;
    bus.mem[0xFFFD] = 0xC0;
    cpu.reset_with_bus(&mut bus);
    assert_eq!(cpu.pc, 0xC000);
    assert_eq!(cpu.sp, 0xFD);
    assert_eq!(cpu.p, 0x24);
}

// ====================================================================
// BCD mode tests (standard 6502, not 2A03)
// ====================================================================

#[test]
fn test_adc_bcd() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.bcd_disabled = false;
    cpu.set_flag(FLAG_D, true);
    cpu.set_flag(FLAG_C, false);
    cpu.a = 0x15; // BCD 15
    bus.load(0x0000, &[0x69, 0x27]); // ADC #$27 -> 15+27=42 BCD
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x42);
    assert!(!cpu.get_flag(FLAG_C));
}

#[test]
fn test_adc_bcd_carry() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.bcd_disabled = false;
    cpu.set_flag(FLAG_D, true);
    cpu.set_flag(FLAG_C, false);
    cpu.a = 0x99;
    bus.load(0x0000, &[0x69, 0x01]); // ADC #$01 -> 99+01=100, carry
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    assert_eq!(cpu.a, 0x00);
    assert!(cpu.get_flag(FLAG_C));
}

#[test]
fn test_adc_bcd_disabled_2a03() {
    let (mut cpu, mut bus) = make_cpu_bus();
    cpu.bcd_disabled = true; // 2A03 mode
    cpu.set_flag(FLAG_D, true);
    cpu.set_flag(FLAG_C, false);
    cpu.a = 0x15;
    bus.load(0x0000, &[0x69, 0x27]); // ADC #$27
    cpu.pc = 0x0000;
    cpu.step(&mut bus);
    // In binary mode: 0x15 + 0x27 = 0x3C
    assert_eq!(cpu.a, 0x3C);
}

// ====================================================================
// Disassembler tests
// ====================================================================

#[test]
fn test_disasm_lda_immediate() {
    let mut bus = TestBus::new();
    bus.load(0x0000, &[0xA9, 0x42]);
    let (text, size) = crate::disasm::disassemble_at(&mut bus, 0x0000);
    assert_eq!(text, "LDA #$42");
    assert_eq!(size, 2);
}

#[test]
fn test_disasm_sta_absolute() {
    let mut bus = TestBus::new();
    bus.load(0x0000, &[0x8D, 0x00, 0x20]);
    let (text, size) = crate::disasm::disassemble_at(&mut bus, 0x0000);
    assert_eq!(text, "STA $2000");
    assert_eq!(size, 3);
}

#[test]
fn test_disasm_jmp_indirect() {
    let mut bus = TestBus::new();
    bus.load(0x0000, &[0x6C, 0xFF, 0x10]);
    let (text, size) = crate::disasm::disassemble_at(&mut bus, 0x0000);
    assert_eq!(text, "JMP ($10FF)");
    assert_eq!(size, 3);
}

#[test]
fn test_disasm_bne_relative() {
    let mut bus = TestBus::new();
    bus.load(0x0010, &[0xD0, 0x05]); // BNE +5 -> $0017
    let (text, _) = crate::disasm::disassemble_at(&mut bus, 0x0010);
    assert_eq!(text, "BNE $0017");
}

#[test]
fn test_disasm_asl_accumulator() {
    let mut bus = TestBus::new();
    bus.load(0x0000, &[0x0A]);
    let (text, size) = crate::disasm::disassemble_at(&mut bus, 0x0000);
    assert_eq!(text, "ASL A");
    assert_eq!(size, 1);
}

#[test]
fn test_disasm_indirect_indexed() {
    let mut bus = TestBus::new();
    bus.load(0x0000, &[0xB1, 0x20]); // LDA ($20),Y
    let (text, size) = crate::disasm::disassemble_at(&mut bus, 0x0000);
    assert_eq!(text, "LDA ($20),Y");
    assert_eq!(size, 2);
}

#[test]
fn test_disasm_indexed_indirect() {
    let mut bus = TestBus::new();
    bus.load(0x0000, &[0xA1, 0x10]); // LDA ($10,X)
    let (text, size) = crate::disasm::disassemble_at(&mut bus, 0x0000);
    assert_eq!(text, "LDA ($10,X)");
    assert_eq!(size, 2);
}

// ====================================================================
// Multi-instruction sequence test
// ====================================================================

#[test]
fn test_simple_loop() {
    let (mut cpu, mut bus) = make_cpu_bus();
    // A simple program: load 10 into X, decrement until zero
    // LDX #$05; DEX; BNE -2
    bus.load(0x0000, &[0xA2, 0x05, 0xCA, 0xD0, 0xFD]);
    cpu.pc = 0x0000;

    cpu.step(&mut bus); // LDX #$05
    assert_eq!(cpu.x, 0x05);

    for expected in (0..5).rev() {
        cpu.step(&mut bus); // DEX
        assert_eq!(cpu.x, expected);
        cpu.step(&mut bus); // BNE
        if expected > 0 {
            assert_eq!(cpu.pc, 0x0002); // loops back
        } else {
            assert_eq!(cpu.pc, 0x0005); // falls through
        }
    }
}

#[test]
fn test_subroutine_with_stack() {
    let (mut cpu, mut bus) = make_cpu_bus();
    // Main: JSR $8000; result in A
    // Sub at $8000: LDA #$42; RTS
    bus.load(0x0000, &[0x20, 0x00, 0x80]); // JSR $8000
    bus.load(0x8000, &[0xA9, 0x42, 0x60]); // LDA #$42; RTS
    cpu.pc = 0x0000;

    cpu.step(&mut bus); // JSR $8000
    assert_eq!(cpu.pc, 0x8000);

    cpu.step(&mut bus); // LDA #$42
    assert_eq!(cpu.a, 0x42);

    cpu.step(&mut bus); // RTS
    assert_eq!(cpu.pc, 0x0003);
}
