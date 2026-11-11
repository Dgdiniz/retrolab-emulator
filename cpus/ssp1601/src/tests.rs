//! Opcode-level unit tests for the SSP1601 interpreter.
//!
//! Each test hand-assembles a tiny program into the program-ROM region
//! (word `0x400`, where `reset` points PC), runs a fixed cycle budget, and
//! checks the resulting register / RAM / DRAM state against the semantics of
//! the reference (`ssp16.c`, see the crate docs). A [`TestHost`] provides the
//! shared DRAM and cartridge ROM the interpreter reaches through [`SspHost`].

use crate::{
    Ssp1601, SspHost, FLAG_N, FLAG_Z, SSP_A, SSP_PC, SSP_STACK, SSP_WAIT_PM0, SSP_X, SSP_Y,
};

/// A minimal host: 128 KiB DRAM (word-addressed) + a 2 MiB cartridge ROM.
struct TestHost {
    dram: Vec<u16>,
    rom: Vec<u16>,
}

impl Default for TestHost {
    fn default() -> Self {
        Self {
            dram: vec![0u16; 0x1_0000],
            rom: vec![0u16; 0x10_0000],
        }
    }
}

impl SspHost for TestHost {
    fn read_dram(&self, word_addr: u16) -> u16 {
        self.dram[word_addr as usize]
    }
    fn write_dram(&mut self, word_addr: u16, value: u16) {
        self.dram[word_addr as usize] = value;
    }
    fn read_cart_rom(&self, word_addr: u32) -> u16 {
        self.rom.get(word_addr as usize).copied().unwrap_or(0)
    }
}

/// Assemble `ops` at word `0x400`, reset, and run `cycles`.
fn run(ops: &[u16], cycles: i32) -> (Ssp1601, TestHost) {
    let mut ssp = Ssp1601::new();
    ssp.reset();
    for (i, &w) in ops.iter().enumerate() {
        ssp.iram_rom[0x400 + i] = w;
    }
    let mut host = TestHost::default();
    ssp.run(cycles, &mut host);
    (ssp, host)
}

/// Like [`run`] but exposes the host so the caller can seed / inspect it.
fn run_with(ops: &[u16], cycles: i32, host: &mut TestHost) -> Ssp1601 {
    let mut ssp = Ssp1601::new();
    ssp.reset();
    for (i, &w) in ops.iter().enumerate() {
        ssp.iram_rom[0x400 + i] = w;
    }
    ssp.run(cycles, host);
    ssp
}

// -- reset / load ---------------------------------------------------------

#[test]
fn reset_sets_pc_and_gr0() {
    let mut ssp = Ssp1601::new();
    ssp.reset();
    assert_eq!(ssp.program_counter(), 0x400);
    assert_eq!(ssp.reg16(crate::SSP_GR0), 0xFFFF);
}

#[test]
fn load_program_copies_words_from_rom_0x800() {
    let mut rom = vec![0u8; 0x2_0000];
    // Program word 0x400 == cartridge byte 0x800 (big-endian).
    rom[0x800] = 0x12;
    rom[0x801] = 0x34;
    let mut ssp = Ssp1601::new();
    ssp.load_program(&rom);
    assert_eq!(ssp.iram_rom[0x400], 0x1234);
    // IRAM (words 0..0x400) is untouched by load_program.
    assert_eq!(ssp.iram_rom[0], 0);
}

// -- ld / ldi -------------------------------------------------------------

#[test]
fn ldi_immediate_into_accumulator() {
    // ldi A, 0xAB00
    let (ssp, _) = run(&[0x0830, 0xAB00], 1);
    assert_eq!(ssp.reg16(SSP_A), 0xAB00);
}

#[test]
fn ld_register_to_register() {
    // ldi X, 0x1234 ; ld A, X   (op 0x0031 = ld d=A, s=X)
    let (ssp, _) = run(&[0x0810, 0x1234, 0x0031], 2);
    assert_eq!(ssp.reg16(SSP_A), 0x1234);
}

// -- arithmetic + flags -----------------------------------------------------

#[test]
fn sub_immediate() {
    // ldi A, 0x0005 ; sub a, 0x0002
    let (ssp, _) = run(&[0x0830, 0x0005, 0x2800, 0x0002], 2);
    assert_eq!(ssp.reg16(SSP_A), 0x0003);
    assert_eq!(ssp.reg16(crate::SSP_ST) & FLAG_Z, 0, "non-zero -> Z clear");
}

#[test]
fn sub_to_zero_sets_z_flag() {
    // ldi A, 0x0002 ; sub a, 0x0002
    let (ssp, _) = run(&[0x0830, 0x0002, 0x2800, 0x0002], 2);
    assert_eq!(ssp.gr[SSP_A], 0);
    assert_ne!(ssp.reg16(crate::SSP_ST) & FLAG_Z, 0, "zero -> Z set");
}

#[test]
fn sub_negative_sets_n_flag() {
    // ldi A, 0x0001 ; sub a, 0x0002  -> 0xFFFF0000 (negative high word)
    let (ssp, _) = run(&[0x0830, 0x0001, 0x2800, 0x0002], 2);
    assert_ne!(ssp.reg16(crate::SSP_ST) & FLAG_N, 0, "MSB set -> N");
}

#[test]
fn add_immediate() {
    // ldi A, 0x0010 ; add a, 0x0005   (add a, imm = 0x44<<9 = 0x8800)
    let (ssp, _) = run(&[0x0830, 0x0010, 0x8800, 0x0005], 2);
    assert_eq!(ssp.reg16(SSP_A), 0x0015);
}

#[test]
fn and_immediate() {
    // ldi A, 0x00FF ; and a, 0x000F   (and a, imm = 0x54<<9 = 0xA800)
    let (ssp, _) = run(&[0x0830, 0x00FF, 0xA800, 0x000F], 2);
    assert_eq!(ssp.reg16(SSP_A), 0x000F);
}

#[test]
fn or_and_eor_immediate() {
    // ldi A, 0x000F ; or a, 0x00F0  (or = 0x64<<9 = 0xC800)
    let (ssp, _) = run(&[0x0830, 0x000F, 0xC800, 0x00F0], 2);
    assert_eq!(ssp.reg16(SSP_A), 0x00FF);
    // ldi A, 0x00FF ; eor a, 0x000F (eor = 0x74<<9 = 0xE800)
    let (ssp, _) = run(&[0x0830, 0x00FF, 0xE800, 0x000F], 2);
    assert_eq!(ssp.reg16(SSP_A), 0x00F0);
}

#[test]
fn cmp_immediate_sets_flags_without_touching_a() {
    // ldi A, 0x0002 ; cmp a, 0x0002  (cmp a, imm = 0x34<<9 = 0x6800)
    let (ssp, _) = run(&[0x0830, 0x0002, 0x6800, 0x0002], 2);
    assert_eq!(ssp.reg16(SSP_A), 0x0002, "cmp must not modify A");
    assert_ne!(ssp.reg16(crate::SSP_ST) & FLAG_Z, 0, "equal -> Z set");
}

#[test]
fn add_simm_short_form() {
    // ldi A, 0x0002 ; add a, 5   (OP simm add = 0x4c<<9 = 0x9800)
    let (ssp, _) = run(&[0x0830, 0x0002, 0x9805], 2);
    assert_eq!(ssp.reg16(SSP_A), 0x0007);
}

// -- mod: shifts / neg / abs ------------------------------------------------

#[test]
fn mod_shr_is_arithmetic() {
    // ldi A, 0x0008 ; mod always, shr  (0x48<<9 | shr(2) = 0x9002)
    let (ssp, _) = run(&[0x0830, 0x0008, 0x9002], 2);
    assert_eq!(ssp.gr[SSP_A], 0x0004_0000);
}

#[test]
fn mod_shl() {
    // ldi A, 0x0004 ; mod always, shl (0x9003)
    let (ssp, _) = run(&[0x0830, 0x0004, 0x9003], 2);
    assert_eq!(ssp.gr[SSP_A], 0x0008_0000);
}

#[test]
fn mod_neg_and_abs() {
    // ldi A, 0x0001 ; mod always, neg (0x9006) -> 0xFFFF0000
    let (ssp, _) = run(&[0x0830, 0x0001, 0x9006], 2);
    assert_eq!(ssp.gr[SSP_A], 0xFFFF_0000);
    // continue: mod always, abs (0x9007) -> back to 0x00010000
    let (ssp, _) = run(&[0x0830, 0x0001, 0x9006, 0x9007], 3);
    assert_eq!(ssp.gr[SSP_A], 0x0001_0000);
}

// -- pointer registers ------------------------------------------------------

#[test]
fn ldi_pointer_then_indirect_store_and_load() {
    // ldi r0, 5 ; ldi A, 0xAB00 ; ld (r0), A ; ld X, (r0)
    let (ssp, _) = run(&[0x1805, 0x0830, 0xAB00, 0x0430, 0x0210], 4);
    assert_eq!(ssp.ram[5], 0xAB00, "stored through (r0)");
    assert_eq!(ssp.reg16(SSP_X), 0xAB00, "loaded back through (r0)");
    assert_eq!(ssp.ptr[0], 5, "no modifier -> pointer unchanged");
}

#[test]
fn post_increment_pointer() {
    // ldi r0, 0 ; ld X, (r0+) ; ld Y, (r0+)  reads ram[0], ram[1], r0 -> 2
    let mut ssp = Ssp1601::new();
    ssp.reset();
    ssp.ram[0] = 0x1111;
    ssp.ram[1] = 0x2222;
    // ld X,(r0+): 0x200 | (X<<4=0x10) | ptr(r0,mod=+ -> bits2,3=11 -> 0x0C) = 0x21C
    // ld Y,(r0+): 0x200 | (Y<<4=0x20) | 0x0C = 0x22C
    let ops = [0x1800u16, 0x021C, 0x022C];
    for (i, &w) in ops.iter().enumerate() {
        ssp.iram_rom[0x400 + i] = w;
    }
    let mut host = TestHost::default();
    ssp.run(3, &mut host);
    assert_eq!(ssp.reg16(SSP_X), 0x1111);
    assert_eq!(ssp.reg16(SSP_Y), 0x2222);
    assert_eq!(ssp.ptr[0], 2, "two post-increments");
}

// -- multiply ----------------------------------------------------------------

#[test]
fn ld_a_p_computes_product() {
    // ldi X, 3 ; ldi Y, 4 ; ld A, P   (P = X*Y*2 = 24)
    let (ssp, _) = run(&[0x0810, 0x0003, 0x0820, 0x0004, 0x0037], 3);
    assert_eq!(ssp.gr[SSP_A], 24);
}

#[test]
fn mld_loads_operands_and_clears_acc() {
    // mld (r4),(r0) with b bit -> A=0, X=ram[0], Y=ram[256]; then ld A,P
    let mut ssp = Ssp1601::new();
    ssp.reset();
    ssp.ram[0] = 5;
    ssp.ram[256] = 6;
    ssp.gr[SSP_A] = 0xDEAD_BEEF;
    let ops = [0xB700u16, 0x0037];
    for (i, &w) in ops.iter().enumerate() {
        ssp.iram_rom[0x400 + i] = w;
    }
    let mut host = TestHost::default();
    ssp.run(2, &mut host);
    assert_eq!(ssp.reg16(SSP_X), 5);
    assert_eq!(ssp.reg16(SSP_Y), 6);
    assert_eq!(ssp.gr[SSP_A], 60, "P = 5*6*2 = 60 loaded into A");
}

#[test]
fn mld_sets_z_and_clears_the_other_flags() {
    // The accumulator ends at zero, so the flags describe a zero result
    // whatever they held; the control bits below them are left alone.
    let mut ssp = Ssp1601::new();
    ssp.reset();
    ssp.gr[SSP_A] = 0x8000_0000;
    ssp.set_reg16(
        crate::SSP_ST,
        FLAG_N | crate::FLAG_V | crate::FLAG_L | 0x0005,
    );
    ssp.iram_rom[0x400] = 0xB700; // mld (r4),(r0)
    let mut host = TestHost::default();
    ssp.run(1, &mut host);
    assert_eq!(ssp.gr[SSP_A], 0);
    assert_eq!(ssp.reg16(crate::SSP_ST), FLAG_Z | 0x0005);
}

#[test]
fn mpya_accumulates_product() {
    // ldi X,2; ldi Y,3; ld A,P (=12); mpya (r4),(r0): A += P(12) then reload X,Y
    let mut ssp = Ssp1601::new();
    ssp.reset();
    ssp.ram[0] = 0;
    ssp.ram[256] = 0;
    // 0x0810,2 ; 0x0820,3 ; 0x0037 ; mpya=0x4b<<9|b = 0x9700
    let ops = [0x0810u16, 0x0002, 0x0820, 0x0003, 0x0037, 0x9700];
    for (i, &w) in ops.iter().enumerate() {
        ssp.iram_rom[0x400 + i] = w;
    }
    let mut host = TestHost::default();
    ssp.run(4, &mut host);
    // A was 12 (2*3*2); mpya adds P (still 12) -> 24, then loads X=Y=0.
    assert_eq!(ssp.gr[SSP_A], 24);
}

// -- control flow ------------------------------------------------------------

#[test]
fn bra_always_taken() {
    // bra always,0x404 ; (skipped) ; ... ; @0x404 ldi A,0x00AA
    let ops = [
        0x4C00, 0x0404, // bra always -> word 0x404
        0x0830, 0xDEAD, // word 0x402/403: skipped ldi
        0x0830, 0x00AA, // word 0x404/405: ldi A, 0x00AA
    ];
    let (ssp, _) = run(&ops, 5);
    assert_eq!(ssp.reg16(SSP_A), 0x00AA);
}

#[test]
fn call_always_pushes_return_and_jumps() {
    // call always, 0x405
    let ops = [0x4800, 0x0405];
    let (ssp, _) = run(&ops, 1);
    assert_eq!(ssp.stack[0], 0x402, "return address pushed");
    assert_eq!(ssp.reg16(SSP_STACK), 1, "stack pointer advanced");
    assert_eq!(ssp.reg16(SSP_PC), 0x405, "jumped to target");
}

#[test]
fn conditional_bra_on_z_flag() {
    // ldi A,2 ; sub a,2 (Z=1) ; bra (Z set),0x408 ; @0x406 ldi A,0xDEAD (skipped) ; @0x408 ldi A,0x00BB
    // bra Z-set: 0x26<<9 | 0x50 | 0x100 = 0x4D50
    let ops = [
        0x0830, 0x0002, // ldi A, 2       (word 0x400/401)
        0x2800, 0x0002, // sub a, 2 -> Z   (word 0x402/403)
        0x4D50, 0x0408, // bra Z, 0x408   (word 0x404/405)
        0x0830, 0xDEAD, // skipped        (word 0x406/407)
        0x0830, 0x00BB, // ldi A, 0x00BB  (word 0x408/409)
    ];
    let (ssp, _) = run(&ops, 8);
    assert_eq!(ssp.reg16(SSP_A), 0x00BB, "branch taken when Z matches");
}

#[test]
fn conditional_bra_not_taken_when_flag_mismatches() {
    // Same but require Z clear (op bit8 = 0 -> 0x4C50). Z is set, so NOT taken.
    let ops = [
        0x0830, 0x0002, // ldi A, 2
        0x2800, 0x0002, // sub a, 2 -> Z=1
        0x4C50, 0x0408, // bra (Z clear), 0x408 -> NOT taken
        0x0830, 0x00CC, // ldi A, 0x00CC   (falls through here)
        0x0830, 0xDEAD, // 0x408 (not reached: budget stops at the fall-through)
    ];
    // Budget exactly 4 instructions: ldi, sub, bra-not-taken, ldi 0xCC.
    let (ssp, _) = run(&ops, 4);
    assert_eq!(ssp.reg16(SSP_A), 0x00CC, "fell through the untaken branch");
}

// -- PMAC / programmable memory ---------------------------------------------

#[test]
fn pm_reads_cartridge_rom() {
    // Program PM4 to read ROM at 0x1234, then read it into A.
    let mut host = TestHost::default();
    host.rom[0x1234] = 0xCAFE;
    let ops = [
        0x08E0, 0x1234, // ldi PMC, 0x1234 (address)
        0x08E0, 0x0800, // ldi PMC, 0x0800 (mode = ROM, inc 1)
        0x000C, // ld -, PM4  (blind read -> program pmac[0][4])
        0x003C, // ld A, PM4  (read rom[0x1234])
    ];
    let ssp = run_with(&ops, 4, &mut host);
    assert_eq!(ssp.reg16(SSP_A), 0xCAFE);
}

#[test]
fn pm_reads_shared_dram() {
    let mut host = TestHost::default();
    host.dram[0x0100] = 0xBEEF;
    let ops = [
        0x08E0, 0x0100, // ldi PMC, 0x0100 (address)
        0x08E0, 0x0018, // ldi PMC, 0x0018 (mode = DRAM)
        0x000C, // ld -, PM4  (blind read program)
        0x003C, // ld A, PM4  (read dram[0x100])
    ];
    let ssp = run_with(&ops, 4, &mut host);
    assert_eq!(ssp.reg16(SSP_A), 0xBEEF);
}

#[test]
fn pm_writes_shared_dram() {
    let mut host = TestHost::default();
    let ops = [
        0x08E0, 0x0100, // ldi PMC, 0x0100 (address)
        0x08E0, 0x0018, // ldi PMC, 0x0018 (mode = DRAM write)
        0x00C0, // ld PM4, -   (blind write -> program pmac[1][4])
        0x0830, 0x00CD, // ldi A, 0x00CD
        0x00C3, // ld PM4, A   (write A -> dram[0x100])
    ];
    run_with(&ops, 5, &mut host);
    assert_eq!(host.dram[0x0100], 0x00CD);
}

// -- wait loops --------------------------------------------------------------

#[test]
fn pm0_tight_loop_yields_early() {
    // A raw PM0 read at word 0x400 (ppc offset 0x800) with PM0 bit1 clear
    // sets SSP_WAIT_PM0 and makes run() yield after a single instruction.
    let ops = [0x0038u16, 0x0038, 0x0038, 0x0038]; // ld A, PM0 (x4)
    let mut ssp = Ssp1601::new();
    ssp.reset();
    for (i, &w) in ops.iter().enumerate() {
        ssp.iram_rom[0x400 + i] = w;
    }
    let mut host = TestHost::default();
    let retired = ssp.run(10, &mut host);
    assert_ne!(ssp.emu_status & SSP_WAIT_PM0, 0, "wait flag latched");
    assert_eq!(
        retired, 1,
        "yielded after one instruction despite budget 10"
    );
}

#[test]
fn nothing_runs_while_a_wait_flag_is_pending() {
    // The 68000 side clears a wait flag; until it does, a run retires no
    // instruction and the program counter stays where it was.
    let mut ssp = Ssp1601::new();
    ssp.reset();
    ssp.iram_rom[0x400] = 0x0830; // ldi A, 0x1234
    ssp.iram_rom[0x401] = 0x1234;
    ssp.emu_status = SSP_WAIT_PM0;
    let mut host = TestHost::default();
    assert_eq!(ssp.run(10, &mut host), 0);
    assert_eq!(ssp.program_counter(), 0x400);
    assert_eq!(ssp.reg16(SSP_A), 0);

    // Once cleared, the same call runs the program: the load, then nops
    // until the budget is spent.
    ssp.emu_status = 0;
    assert_eq!(ssp.run(10, &mut host), 10);
    assert_eq!(ssp.reg16(SSP_A), 0x1234);
}

#[test]
fn an_empty_budget_runs_nothing() {
    let (ssp, _) = run(&[0x0830, 0x1234], 0);
    assert_eq!(ssp.instructions, 0);
    assert_eq!(ssp.reg16(SSP_A), 0);
    assert_eq!(ssp.program_counter(), 0x400);
}

#[test]
fn a_taken_branch_costs_a_second_cycle() {
    // Three cycles pay for a taken `bra` and one instruction after it; an
    // untaken one leaves room for two.
    let taken = [
        0x4C00, 0x0404, // bra always, 0x404
        0x0000, 0x0000, // (skipped)
        0x0830, 0x00AA, // 0x404: ldi A, 0x00AA
        0x0810, 0x00BB, // 0x406: ldi X, 0x00BB
    ];
    let (ssp, _) = run(&taken, 3);
    assert_eq!(ssp.instructions, 2);
    assert_eq!(ssp.reg16(SSP_A), 0x00AA);
    assert_eq!(ssp.reg16(SSP_X), 0);

    let untaken = [
        0x4D50, 0x0404, // bra (Z set), 0x404, Z is clear after reset
        0x0830, 0x00AA, // 0x402: ldi A, 0x00AA
        0x0810, 0x00BB, // 0x404: ldi X, 0x00BB
    ];
    let (ssp, _) = run(&untaken, 3);
    assert_eq!(ssp.instructions, 3);
    assert_eq!(ssp.reg16(SSP_A), 0x00AA);
    assert_eq!(ssp.reg16(SSP_X), 0x00BB);
}

// -- ld adr, a / ld a, adr --------------------------------------------------

#[test]
fn ram_direct_store_and_load() {
    // ldi A, 0x1234 ; ld adr(0x10), a ; ldi A, 0 ; ld a, adr(0x10)
    // ld adr,a = 0x07<<9 | adr = 0xE00|0x10 = 0xE10
    // ld a,adr = 0x03<<9 | adr = 0x600|0x10 = 0x610
    let (ssp, _) = run(&[0x0830, 0x1234, 0x0E10, 0x0830, 0x0000, 0x0610], 4);
    assert_eq!(ssp.ram[0x10], 0x1234, "stored to RAM[0x10]");
    assert_eq!(
        ssp.reg16(SSP_A),
        0x1234,
        "ld a,adr does not touch flags, loads value"
    );
}
