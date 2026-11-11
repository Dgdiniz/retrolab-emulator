//! Unit tests for the [`Spc700`] register file, helper accessors,
//! and the `retro_core::cpu` trait impls (`Clockable`, `Resettable`).
//!
//! These tests use a minimal in-memory bus so they can run without
//! the SNES address map. Bus-driven scenarios (full instruction
//! execution) live in `tests/singlestep_runner.rs`.

use retro_core::cpu::{Addressable, Clockable, Resettable};

use super::instructions::{FLAG_C, FLAG_H, FLAG_N, FLAG_P, FLAG_V, FLAG_Z};
use super::interrupts::RESET_VECTOR;
use super::Spc700;

/// Minimal 64 KiB linear bus used by the helper tests. Reads/writes
/// are unchecked because the address space is exactly `u16`.
struct TinyBus {
    ram: [u8; 0x10000],
}

impl TinyBus {
    fn new() -> Self {
        Self { ram: [0; 0x10000] }
    }
}

impl Addressable for TinyBus {
    fn read(&mut self, addr: u32) -> u8 {
        self.ram[(addr & 0xFFFF) as usize]
    }

    fn write(&mut self, addr: u32, value: u8) {
        self.ram[(addr & 0xFFFF) as usize] = value;
    }
}

// -- Register-pair helpers ------------------------------------------------

#[test]
fn ya_packs_y_high_a_low() {
    let mut cpu = Spc700::new();
    cpu.y = 0xAB;
    cpu.a = 0xCD;
    assert_eq!(cpu.ya(), 0xABCD);
}

#[test]
fn set_ya_splits_into_y_and_a() {
    let mut cpu = Spc700::new();
    cpu.set_ya(0x1234);
    assert_eq!(cpu.y, 0x12);
    assert_eq!(cpu.a, 0x34);
}

// -- Direct-page base / stack address -------------------------------------

#[test]
fn dp_base_follows_psw_p() {
    let mut cpu = Spc700::new();
    cpu.psw = 0;
    assert_eq!(cpu.dp_base(), 0x0000);

    cpu.set_flag(FLAG_P, true);
    assert_eq!(cpu.dp_base(), 0x0100);

    cpu.set_flag(FLAG_P, false);
    assert_eq!(cpu.dp_base(), 0x0000);
}

#[test]
fn stack_addr_lives_in_page_one() {
    let mut cpu = Spc700::new();
    cpu.sp = 0x42;
    assert_eq!(cpu.stack_addr(), 0x0142);

    cpu.sp = 0xFF;
    assert_eq!(cpu.stack_addr(), 0x01FF);

    cpu.sp = 0x00;
    assert_eq!(cpu.stack_addr(), 0x0100);
}

// -- Push / pull byte -----------------------------------------------------

#[test]
fn push_writes_then_decrements_sp() {
    let mut cpu = Spc700::new();
    cpu.sp = 0xFF;
    let mut bus = TinyBus::new();

    cpu.push_byte(&mut bus, 0xAB);
    assert_eq!(bus.read(0x01FF), 0xAB);
    assert_eq!(cpu.sp, 0xFE);

    cpu.push_byte(&mut bus, 0xCD);
    assert_eq!(bus.read(0x01FE), 0xCD);
    assert_eq!(cpu.sp, 0xFD);
}

#[test]
fn push_wraps_through_zero() {
    let mut cpu = Spc700::new();
    cpu.sp = 0x00;
    let mut bus = TinyBus::new();

    cpu.push_byte(&mut bus, 0x77);
    assert_eq!(bus.read(0x0100), 0x77);
    assert_eq!(cpu.sp, 0xFF, "SP must wrap from 0x00 to 0xFF on push");
}

#[test]
fn pull_increments_sp_then_reads() {
    let mut cpu = Spc700::new();
    cpu.sp = 0xFD;
    let mut bus = TinyBus::new();
    bus.write(0x01FE, 0x11);
    bus.write(0x01FF, 0x22);

    let first = cpu.pull_byte(&mut bus);
    assert_eq!(first, 0x11);
    assert_eq!(cpu.sp, 0xFE);

    let second = cpu.pull_byte(&mut bus);
    assert_eq!(second, 0x22);
    assert_eq!(cpu.sp, 0xFF);
}

#[test]
fn pull_wraps_at_top_of_page() {
    let mut cpu = Spc700::new();
    cpu.sp = 0xFF;
    let mut bus = TinyBus::new();
    bus.write(0x0100, 0x42);

    let v = cpu.pull_byte(&mut bus);
    assert_eq!(v, 0x42);
    assert_eq!(cpu.sp, 0x00, "SP must wrap from 0xFF to 0x00 on pull");
}

// -- Flag helpers ---------------------------------------------------------

#[test]
fn flag_get_and_set_are_independent_bits() {
    let mut cpu = Spc700::new();
    cpu.psw = 0;
    cpu.set_flag(FLAG_C, true);
    cpu.set_flag(FLAG_N, true);

    assert!(cpu.get_flag(FLAG_C));
    assert!(cpu.get_flag(FLAG_N));
    assert!(!cpu.get_flag(FLAG_Z));
    assert!(!cpu.get_flag(FLAG_V));

    cpu.set_flag(FLAG_C, false);
    assert!(!cpu.get_flag(FLAG_C));
    assert!(cpu.get_flag(FLAG_N), "Clearing C must not clear N");
}

#[test]
fn update_nz_u8_handles_zero_negative_and_positive() {
    let mut cpu = Spc700::new();
    cpu.psw = FLAG_H | FLAG_P;

    cpu.update_nz_u8(0);
    assert!(cpu.get_flag(FLAG_Z));
    assert!(!cpu.get_flag(FLAG_N));
    assert!(cpu.get_flag(FLAG_H), "update_nz must not touch H");
    assert!(cpu.get_flag(FLAG_P), "update_nz must not touch P");

    cpu.update_nz_u8(0x80);
    assert!(!cpu.get_flag(FLAG_Z));
    assert!(cpu.get_flag(FLAG_N));

    cpu.update_nz_u8(0x7F);
    assert!(!cpu.get_flag(FLAG_Z));
    assert!(!cpu.get_flag(FLAG_N));
}

#[test]
fn update_nz_u16_uses_bit_15_for_negative() {
    let mut cpu = Spc700::new();

    cpu.update_nz_u16(0);
    assert!(cpu.get_flag(FLAG_Z));
    assert!(!cpu.get_flag(FLAG_N));

    cpu.update_nz_u16(0x8000);
    assert!(!cpu.get_flag(FLAG_Z));
    assert!(cpu.get_flag(FLAG_N));

    cpu.update_nz_u16(0x7FFF);
    assert!(!cpu.get_flag(FLAG_Z));
    assert!(!cpu.get_flag(FLAG_N));

    cpu.update_nz_u16(0x00FF);
    assert!(
        !cpu.get_flag(FLAG_N),
        "Low-byte bit 7 must not drive N for u16"
    );
}

// -- retro_core::cpu trait impls ------------------------------------------

#[test]
fn clockable_returns_zero_sentinel() {
    let mut cpu = Spc700::new();
    let before = cpu.cycles_executed;
    let consumed = cpu.clock(42);
    assert_eq!(
        consumed, 0,
        "Clockable on SPC700 is a sentinel (see lib.rs)"
    );
    assert_eq!(
        cpu.cycles_executed, before,
        "clock() must not mutate cycles_executed"
    );
}

#[test]
fn resettable_reinits_registers_without_bus() {
    let mut cpu = Spc700::new();
    cpu.a = 0xAA;
    cpu.x = 0xBB;
    cpu.y = 0xCC;
    cpu.sp = 0x00;
    cpu.pc = 0x1234;
    cpu.psw = 0xFF;
    cpu.cycles_executed = 999_999;

    cpu.reset();

    assert_eq!(cpu.a, 0);
    assert_eq!(cpu.x, 0);
    assert_eq!(cpu.y, 0);
    assert_eq!(cpu.sp, 0xEF);
    assert_eq!(cpu.psw, 0x02);
    assert_eq!(cpu.cycles_executed, 0);
    assert_eq!(
        cpu.pc, 0xFFC0,
        "Resettable::reset must leave PC at the IPL entry"
    );
}

#[test]
fn reset_with_bus_honours_vector() {
    let mut cpu = Spc700::new();
    let mut bus = TinyBus::new();
    bus.write(RESET_VECTOR as u32, 0x34);
    bus.write((RESET_VECTOR + 1) as u32, 0x12);

    cpu.reset_with_bus(&mut bus);

    assert_eq!(cpu.pc, 0x1234);
    assert_eq!(cpu.sp, 0xEF);
    assert_eq!(cpu.psw, 0x02);
}

#[test]
fn clrv_clears_v_and_h_but_not_other_flags() {
    let mut cpu = Spc700::new();
    cpu.psw = FLAG_V | FLAG_H | FLAG_C | FLAG_N;
    cpu.set_flag(FLAG_V, false);
    cpu.set_flag(FLAG_H, false);
    assert!(!cpu.get_flag(FLAG_V));
    assert!(!cpu.get_flag(FLAG_H));
    assert!(cpu.get_flag(FLAG_C));
    assert!(cpu.get_flag(FLAG_N));
}
