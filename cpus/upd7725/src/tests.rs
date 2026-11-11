//! Tests on short programs written here; no firmware dump is needed.

use super::*;
use crate::registers::status::{EIGHT_BIT_TRANSFERS, HIGH_BYTE_NEXT, HOST_REQUEST};

// -- Instruction words ---------------------------------------------------------

/// An OP word from its fields.
#[allow(clippy::too_many_arguments)]
fn op(
    alu_input: u32,
    alu_function: u32,
    accumulator: u32,
    ram_pointer_step: u32,
    ram_pointer_flip: u32,
    rom_pointer_down: u32,
    source: u32,
    destination: u32,
) -> u32 {
    alu_input << 20
        | alu_function << 16
        | accumulator << 15
        | ram_pointer_step << 13
        | ram_pointer_flip << 9
        | rom_pointer_down << 8
        | source << 4
        | destination
}

/// An RT whose OP does nothing.
fn rt() -> u32 {
    1 << 22
}

/// A JP word: the branch code and the target.
fn jp(branch: u32, target: u32) -> u32 {
    2 << 22 | branch << 13 | target << 2
}

/// An LD word: the value into the destination.
fn ld(value: u16, destination: u32) -> u32 {
    3 << 22 | u32::from(value) << 6 | destination
}

fn chip(program: &[u32], data_rom: &[u16]) -> Upd7725 {
    Upd7725::with_firmware(program, data_rom)
}

fn run(chip: &mut Upd7725, steps: usize) {
    for _ in 0..steps {
        chip.step();
    }
}

// -- LD ------------------------------------------------------------------------

#[test]
fn ld_puts_the_value_in_an_accumulator() {
    let mut dsp = chip(&[ld(0x1234, 1)], &[]);
    dsp.step();
    assert_eq!(dsp.accumulators[0], 0x1234);
    assert_eq!(dsp.program_counter, 1);
    assert_eq!(dsp.clocks, 1);
}

#[test]
fn ld_cuts_the_pointers_to_their_width() {
    let mut dsp = chip(&[ld(0x1FF, 4), ld(0xFFFF, 5)], &[]);
    dsp.step();
    assert_eq!(dsp.ram_pointer, 0xFF, "DP is 8 bits");
    dsp.step();
    assert_eq!(dsp.rom_pointer, 0x3FF, "RP is 10 bits");
}

#[test]
fn ld_of_sr_leaves_the_transfer_bits_alone() {
    let mut dsp = chip(&[ld(0xFFFF, 7)], &[]);
    dsp.status = HIGH_BYTE_NEXT;
    dsp.step();
    // RQM stays clear and DRS set, bits 2-6 stay clear, and the other bits
    // take the value's: $6F83.
    assert_eq!(dsp.status, 0x6F83 | HIGH_BYTE_NEXT);
}

// -- Multiplier ----------------------------------------------------------------

#[test]
fn the_multiplier_runs_on_every_instruction() {
    // K=3, L=5: 15, so M = 15 >> 15 = 0 and N = 15 << 1 = 30.
    let mut dsp = chip(&[ld(3, 10), ld(5, 13)], &[]);
    run(&mut dsp, 2);
    assert_eq!((dsp.multiplicand, dsp.multiplier), (3, 5));
    assert_eq!((dsp.product_high, dsp.product_low), (0, 30));
}

#[test]
fn the_product_is_signed() {
    // K=-1, L=2: -2, so M = -1 and N = -4.
    let mut dsp = chip(&[ld(0xFFFF, 10), ld(2, 13)], &[]);
    run(&mut dsp, 2);
    assert_eq!((dsp.product_high, dsp.product_low), (0xFFFF, 0xFFFC));
}

#[test]
fn the_largest_product_fills_m() {
    // $8000 * $8000 = 2^30: M = $8000, and N's bits all shift out.
    let mut dsp = chip(&[ld(0x8000, 10), ld(0x8000, 13)], &[]);
    run(&mut dsp, 2);
    assert_eq!((dsp.product_high, dsp.product_low), (0x8000, 0));
}

#[test]
fn klr_takes_l_from_the_data_rom() {
    // K=3, L = data ROM at RP 0 = 7: 21, N = 42.
    let mut dsp = chip(&[ld(3, 11)], &[7]);
    dsp.step();
    assert_eq!(
        (dsp.multiplicand, dsp.multiplier, dsp.product_low),
        (3, 7, 42)
    );
}

#[test]
fn klm_takes_k_from_the_data_ram_with_bit_6_set() {
    let mut dsp = chip(&[ld(0x0002, 12)], &[]);
    dsp.ram_pointer = 0x85;
    dsp.data_ram[0xC5] = 0x0004;
    dsp.step();
    assert_eq!((dsp.multiplicand, dsp.multiplier), (4, 2));
}

// -- ALU -----------------------------------------------------------------------

#[test]
fn alu_adds_the_bus_to_an_accumulator() {
    // A=$10, B=$20, then A = A + B.
    let mut dsp = chip(
        &[ld(0x0010, 1), ld(0x0020, 2), op(1, 5, 0, 0, 0, 0, 2, 0)],
        &[],
    );
    run(&mut dsp, 3);
    assert_eq!(dsp.accumulators[0], 0x30);
    assert!(!dsp.flags[0].zero);
    assert!(!dsp.flags[0].carry);
}

#[test]
fn a_subtraction_to_zero_sets_z() {
    let mut dsp = chip(&[ld(5, 1), ld(5, 2), op(1, 4, 0, 0, 0, 0, 2, 0)], &[]);
    run(&mut dsp, 3);
    assert_eq!(dsp.accumulators[0], 0);
    assert!(dsp.flags[0].zero);
    assert!(!dsp.flags[0].carry);
}

#[test]
fn a_logical_operation_clears_carry_and_overflow() {
    let mut dsp = chip(
        &[ld(0xFF0F, 1), ld(0x0FF0, 2), op(1, 2, 0, 0, 0, 0, 2, 0)],
        &[],
    );
    dsp.flags[0].carry = true;
    dsp.flags[0].latched_overflow = true;
    run(&mut dsp, 3);
    assert_eq!(dsp.accumulators[0], 0x0F00);
    assert!(!dsp.flags[0].carry);
    assert!(!dsp.flags[0].overflow);
    assert!(!dsp.flags[0].latched_overflow);
}

#[test]
fn adc_takes_the_carry_of_the_other_accumulator() {
    // A = $0001 + $0001 + B's carry.
    let mut dsp = chip(&[op(1, 7, 0, 0, 0, 0, 1, 0)], &[]);
    dsp.accumulators[0] = 1;
    dsp.flags[1].carry = true;
    dsp.step();
    assert_eq!(dsp.accumulators[0], 3);
}

#[test]
fn adc_of_ffff_with_a_carry_in_leaves_the_carry_clear() {
    // The result equals the accumulator, so the comparison that gives the
    // carry finds no wrap (bsnes-plus has it so, and this interpreter keeps
    // it; the true carry is set).
    let mut dsp = chip(&[op(1, 7, 0, 0, 0, 0, 2, 0)], &[]);
    dsp.accumulators = [0x1234, 0xFFFF];
    dsp.flags[1].carry = true;
    dsp.step();
    assert_eq!(dsp.accumulators[0], 0x1234);
    assert!(!dsp.flags[0].carry);
}

#[test]
fn an_overflow_latches_and_an_opposite_one_cancels_it() {
    // $7FFF + 1 overflows to $8000; then $8000 + $8000 overflows back.
    let mut dsp = chip(
        &[op(1, 9, 0, 0, 0, 0, 0, 0), op(1, 5, 0, 0, 0, 0, 1, 0)],
        &[],
    );
    dsp.accumulators[0] = 0x7FFF;
    dsp.step();
    let flags = dsp.flags[0];
    assert!(flags.overflow && flags.latched_overflow && flags.sign && flags.latched_sign);
    dsp.step();
    let flags = dsp.flags[0];
    assert_eq!(dsp.accumulators[0], 0);
    assert!(flags.overflow, "this operation overflowed too");
    assert!(!flags.latched_overflow, "the two overflows cancel");
    assert!(flags.latched_sign, "S1 kept the first overflow's sign");
}

#[test]
fn sgn_gives_the_value_to_saturate_to() {
    // A's latched sign set: $7FFF; clear: $8000.
    let mut dsp = chip(
        &[op(0, 0, 0, 0, 0, 0, 7, 2), op(0, 0, 0, 0, 0, 0, 7, 3)],
        &[],
    );
    dsp.flags[0].latched_sign = true;
    dsp.step();
    assert_eq!(dsp.accumulators[1], 0x7FFF);
    dsp.flags[0].latched_sign = false;
    dsp.step();
    assert_eq!(dsp.temporary, 0x8000);
}

#[test]
fn the_shifts_fill_and_carry_as_the_chip_does() {
    // SHR1 keeps bit 15; SHL1 brings B's carry in; SHL2 and SHL4 fill with
    // ones; XCHG swaps the bytes.
    let cases: [(u32, u16, u16, bool); 5] = [
        (11, 0x8003, 0xC001, true),
        (12, 0x8001, 0x0003, true),
        (13, 0x0001, 0x0007, false),
        (14, 0x0001, 0x001F, false),
        (15, 0x12AB, 0xAB12, false),
    ];
    for (function, before, after, carry) in cases {
        let mut dsp = chip(&[op(0, function, 0, 0, 0, 0, 0, 0)], &[]);
        dsp.accumulators[0] = before;
        dsp.flags[1].carry = true;
        dsp.step();
        assert_eq!(dsp.accumulators[0], after, "function {function}");
        assert_eq!(dsp.flags[0].carry, carry, "function {function}");
    }
}

// -- Moves and pointers -----------------------------------------------------------

#[test]
fn a_move_reads_and_writes_the_data_ram() {
    let mut dsp = chip(&[ld(0x1234, 15), op(0, 0, 0, 0, 0, 0, 15, 1)], &[]);
    dsp.step();
    assert_eq!(dsp.data_ram[0], 0x1234);
    dsp.step();
    assert_eq!(dsp.accumulators[0], 0x1234);
}

#[test]
fn the_move_lands_after_the_alu_and_carries_the_old_value() {
    // A = A + 1 and A = A at once: the move writes A's old value over the
    // sum, and the flags stay the sum's.
    let mut dsp = chip(&[op(1, 9, 0, 0, 0, 0, 1, 1)], &[]);
    dsp.accumulators[0] = 0xFFFF;
    dsp.step();
    assert_eq!(dsp.accumulators[0], 0xFFFF);
    assert!(dsp.flags[0].zero && dsp.flags[0].carry);
}

#[test]
fn dp_steps_in_its_low_nibble_and_flips_its_high_one() {
    // DPINC from $3F wraps to $30; DPHM 5 then flips bits 4 and 6.
    let mut dsp = chip(&[op(0, 0, 0, 1, 5, 0, 0, 0)], &[]);
    dsp.ram_pointer = 0x3F;
    dsp.step();
    assert_eq!(dsp.ram_pointer, 0x30 ^ 0x50);

    let mut dsp = chip(
        &[op(0, 0, 0, 2, 0, 0, 0, 0), op(0, 0, 0, 3, 0, 0, 0, 0)],
        &[],
    );
    dsp.ram_pointer = 0x40;
    dsp.step();
    assert_eq!(dsp.ram_pointer, 0x4F, "DPDEC wraps in the nibble");
    dsp.step();
    assert_eq!(dsp.ram_pointer, 0x40, "DPCLR");
}

#[test]
fn a_move_into_a_pointer_wins_over_its_step() {
    // DPINC and RPDCR with the move writing DP, then RP.
    let mut dsp = chip(
        &[op(0, 0, 0, 1, 0xF, 1, 1, 4), op(0, 0, 0, 1, 0xF, 1, 1, 5)],
        &[],
    );
    dsp.accumulators[0] = 0x0012;
    dsp.rom_pointer = 0x100;
    dsp.step();
    assert_eq!(dsp.ram_pointer, 0x12, "DP takes the move");
    assert_eq!(dsp.rom_pointer, 0x0FF, "RP still steps down");
    dsp.step();
    assert_eq!(dsp.rom_pointer, 0x012, "RP takes the move");
    assert_eq!(dsp.ram_pointer, 0x13 ^ 0xF0, "DP still steps");
}

// -- Jumps and calls ----------------------------------------------------------------

#[test]
fn ljmp_goes_to_the_target() {
    let mut dsp = chip(&[jp(0x100, 5)], &[]);
    dsp.step();
    assert_eq!(dsp.program_counter, 5);
}

#[test]
fn lcall_pushes_the_return_address_and_rt_pops_it() {
    // [0] LCALL 3; [1] LD #$55 -> A; [3] RT.
    let mut program = vec![0u32; 5];
    program[0] = jp(0x140, 3);
    program[1] = ld(0x55, 1);
    program[3] = rt();
    let mut dsp = chip(&program, &[]);

    dsp.step();
    assert_eq!((dsp.program_counter, dsp.stack_pointer), (3, 1));
    assert_eq!(dsp.call_stack[0], 1);

    dsp.step();
    assert_eq!((dsp.program_counter, dsp.stack_pointer), (1, 0));

    dsp.step();
    assert_eq!(dsp.accumulators[0], 0x55);
}

#[test]
fn jmpso_goes_to_so() {
    let mut dsp = chip(&[jp(0x000, 0)], &[]);
    dsp.serial_out = 0x1234;
    dsp.step();
    assert_eq!(dsp.program_counter, 0x234);
}

/// A flag jump's case: its code, the accumulator whose flag is set, how
/// it is set, and whether the jump is taken.
type FlagJump = (u32, usize, fn(&mut AluFlags), bool);

#[test]
fn the_flag_jumps_test_the_flag_and_accumulator_their_code_names() {
    let cases: [FlagJump; 10] = [
        (0x080, 0, |f: &mut AluFlags| f.carry = true, false),
        (0x082, 0, |f: &mut AluFlags| f.carry = true, true),
        (0x086, 1, |f: &mut AluFlags| f.carry = true, true),
        (0x08A, 0, |f: &mut AluFlags| f.zero = true, true),
        (0x08C, 1, |f: &mut AluFlags| f.zero = true, false),
        (0x092, 0, |f: &mut AluFlags| f.overflow = true, true),
        (0x09E, 1, |f: &mut AluFlags| f.latched_overflow = true, true),
        (0x0A0, 0, |f: &mut AluFlags| f.sign = true, false),
        (0x0A6, 1, |f: &mut AluFlags| f.sign = true, true),
        (0x0AA, 0, |f: &mut AluFlags| f.latched_sign = true, true),
    ];
    for (code, accumulator, set, taken) in cases {
        let mut dsp = chip(&[jp(code, 9)], &[]);
        set(&mut dsp.flags[accumulator]);
        dsp.step();
        assert_eq!(dsp.program_counter == 9, taken, "code {code:#05x}");
    }
}

#[test]
fn the_dp_jumps_test_its_low_nibble() {
    for (code, ram_pointer, taken) in [
        (0x0B0, 0x30, true),
        (0x0B0, 0x31, false),
        (0x0B1, 0x31, true),
        (0x0B2, 0x3F, true),
        (0x0B3, 0x3F, false),
        (0x0B3, 0x3E, true),
    ] {
        let mut dsp = chip(&[jp(code, 9)], &[]);
        dsp.ram_pointer = ram_pointer;
        dsp.step();
        assert_eq!(
            dsp.program_counter == 9,
            taken,
            "code {code:#05x}, DP {ram_pointer:#04x}"
        );
    }
}

#[test]
fn jnrqm_spins_until_the_request_is_set() {
    // JNRQM 0, the DSP-1's idle loop.
    let mut dsp = chip(&[jp(0x0BC, 0)], &[]);
    run(&mut dsp, 2);
    assert_eq!(dsp.program_counter, 0, "loops while RQM is clear");
    dsp.status |= HOST_REQUEST;
    dsp.step();
    assert_eq!(dsp.program_counter, 1, "falls through once it is set");
}

#[test]
fn the_serial_acknowledge_jumps_and_odd_flag_codes_never_jump() {
    // JNSIAK, JSIAK, JNSOAK, JSOAK, and an odd code among the flag jumps.
    for code in [0x0B4, 0x0B6, 0x0B8, 0x0BA, 0x081] {
        let mut dsp = chip(&[jp(code, 9)], &[]);
        dsp.step();
        assert_eq!(dsp.program_counter, 1, "code {code:#05x}");
    }
}

// -- DR and SR -----------------------------------------------------------------------

#[test]
fn the_s_cpu_reads_sr_high_byte() {
    let mut dsp = Upd7725::new();
    dsp.status = HOST_REQUEST;
    assert_eq!(dsp.read_status(), 0x80);
    dsp.status = 0x1234;
    assert_eq!(dsp.read_status(), 0x12);
}

#[test]
fn sr_shows_drs_as_stored_in_byte_mode() {
    // DRS no longer means anything once DRC is set, and still reads as it
    // is (bsnes-plus has it so, and this interpreter keeps it).
    let mut dsp = Upd7725::new();
    dsp.status = EIGHT_BIT_TRANSFERS | HIGH_BYTE_NEXT;
    assert_eq!(dsp.read_status(), 0x14);
}

#[test]
fn a_word_crosses_dr_low_byte_first() {
    let mut dsp = Upd7725::new();

    dsp.write_data(0x34);
    assert_ne!(dsp.status & HIGH_BYTE_NEXT, 0);
    dsp.write_data(0x12);
    assert_eq!(dsp.data_port, 0x1234);
    assert_eq!(dsp.status & (HIGH_BYTE_NEXT | HOST_REQUEST), 0);

    dsp.data_port = 0xABCD;
    dsp.status |= HOST_REQUEST;
    assert_eq!(dsp.read_data(), 0xCD);
    assert_ne!(dsp.status & HIGH_BYTE_NEXT, 0);
    assert_eq!(dsp.read_data(), 0xAB);
    assert_eq!(dsp.status & (HIGH_BYTE_NEXT | HOST_REQUEST), 0);
}

#[test]
fn byte_mode_moves_the_low_byte_alone() {
    let mut dsp = Upd7725::new();
    dsp.status = EIGHT_BIT_TRANSFERS | HOST_REQUEST;
    dsp.data_port = 0x12CD;
    assert_eq!(dsp.read_data(), 0xCD);
    assert_eq!(dsp.status & HOST_REQUEST, 0, "one byte ends the transfer");

    dsp.status |= HOST_REQUEST;
    dsp.write_data(0x56);
    assert_eq!(dsp.data_port, 0x1256);
    assert_eq!(dsp.status & HOST_REQUEST, 0);
}

#[test]
fn the_program_writing_dr_raises_the_request() {
    let mut dsp = chip(&[ld(0x00AA, 6)], &[]);
    dsp.step();
    assert_eq!(dsp.data_port, 0x00AA);
    assert_ne!(dsp.status & HOST_REQUEST, 0);
}

#[test]
fn the_program_reading_dr_raises_the_request_and_drnf_does_not() {
    let mut dsp = chip(
        &[op(0, 0, 0, 0, 0, 0, 9, 2), op(0, 0, 0, 0, 0, 0, 8, 1)],
        &[],
    );
    dsp.data_port = 0x7777;
    dsp.step();
    assert_eq!(dsp.accumulators[1], 0x7777);
    assert_eq!(dsp.status & HOST_REQUEST, 0);
    dsp.step();
    assert_eq!(dsp.accumulators[0], 0x7777);
    assert_ne!(dsp.status & HOST_REQUEST, 0);
}

#[test]
fn peek_leaves_the_transfer_where_it_was() {
    let mut dsp = Upd7725::new();
    dsp.data_port = 0x1234;
    assert_eq!(dsp.peek_data(), 0x34);
    assert_eq!(dsp.status, 0);
}

// -- Flags, firmware, reset ------------------------------------------------------------

#[test]
fn the_flags_pack_ov0_in_bit_0_up_to_s1_in_bit_5() {
    let flags = AluFlags {
        carry: true,
        latched_sign: true,
        ..AluFlags::default()
    };
    assert_eq!(flags.to_bits(), 0b10_1000);
    for bits in 0..64 {
        assert_eq!(AluFlags::from_bits(bits).to_bits(), bits);
    }
    assert_eq!(AluFlags::from_bits(0xFFC0), AluFlags::default());
}

#[test]
fn firmware_keeps_24_bits_and_what_fits() {
    let program: Vec<u32> = (0..3000).map(|n| 0xFF00_0000 | n).collect();
    let data_rom: Vec<u16> = (0..1500).collect();
    let dsp = chip(&program, &data_rom);
    assert_eq!(dsp.program.len(), PROGRAM_WORDS);
    assert_eq!(dsp.program[2047], 2047);
    assert_eq!(dsp.data_rom.len(), DATA_ROM_WORDS);
    assert_eq!(dsp.data_rom[1023], 1023);
}

#[test]
fn reset_clears_the_registers_and_keeps_the_memories() {
    let mut dsp = chip(&[ld(0x1234, 1)], &[7]);
    dsp.data_ram[5] = 0x5555;
    dsp.step();
    assert_eq!(dsp.accumulators[0], 0x1234);

    dsp.reset();
    assert_eq!(dsp.accumulators[0], 0);
    assert_eq!(dsp.program_counter, 0);
    assert_eq!(dsp.clocks, 0);
    assert_eq!(dsp.program[0], ld(0x1234, 1), "the program survives");
    assert_eq!(dsp.data_rom[0], 7);
    assert_eq!(dsp.data_ram[5], 0x5555, "so does the data RAM");

    dsp.step();
    assert_eq!(dsp.accumulators[0], 0x1234, "and runs again from the top");
}
