use retro_core::cpu::Addressable;

use sg1_core::cpu68k::disasm;

/// Minimal big-endian memory for 68k disassembly tests.
struct BigEndianRam {
    data: Vec<u8>,
}

impl BigEndianRam {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }
}

impl Addressable for BigEndianRam {
    fn read(&mut self, addr: u32) -> u8 {
        self.data[addr as usize % self.data.len()]
    }

    fn write(&mut self, addr: u32, value: u8) {
        let idx = addr as usize % self.data.len();
        self.data[idx] = value;
    }

    fn read_word(&mut self, addr: u32) -> u16 {
        let hi = self.read(addr) as u16;
        let lo = self.read(addr + 1) as u16;
        (hi << 8) | lo
    }

    fn write_word(&mut self, addr: u32, value: u16) {
        self.write(addr, ((value >> 8) & 0xFF) as u8);
        self.write(addr + 1, (value & 0xFF) as u8);
    }
}

fn write_words(mem: &mut dyn Addressable, addr: u32, words: &[u16]) {
    for (i, &w) in words.iter().enumerate() {
        mem.write_word(addr + (i as u32) * 2, w);
    }
}

#[test]
fn disasm_bra_word_disp_consumes_4_bytes() {
    let mut ram = BigEndianRam::new(0x1000);
    // BRA.w with 16-bit displacement: opcode 0x6000, ext = 0x0004
    // target = addr + 2 + displacement = 0 + 2 + 4 = 0x0006
    write_words(&mut ram, 0x0000, &[0x6000, 0x0004]);

    let (line, bytes) = disasm::disassemble_one(&mut ram, 0x0000);
    assert_eq!(bytes, 4);
    assert!(line.contains("BRA"));
    assert!(line.contains("$00000006"));

    let parts = disasm::disassemble_one_parts(&mut ram, 0x0000);
    assert_eq!(parts.bytes_consumed, 4);
    assert_eq!(parts.bytes, vec![0x60, 0x00, 0x00, 0x04]);
}

#[test]
fn disasm_jmp_pc_disp_formats_pc() {
    let mut ram = BigEndianRam::new(0x1000);
    // JMP d16(PC): opcode 0x4EC0 | (7<<3) | 2 = 0x4EFA, ext = 0x0006, added
    // to the extension word's address (0x0002): 0x0008
    write_words(&mut ram, 0x0000, &[0x4EFA, 0x0006]);

    let (line, bytes) = disasm::disassemble_one(&mut ram, 0x0000);
    assert_eq!(bytes, 4);
    assert!(line.contains("JMP"));
    assert!(line.contains("(PC)"));

    let parts = disasm::disassemble_one_parts(&mut ram, 0x0000);
    assert_eq!(parts.bytes_consumed, 4);
    assert_eq!(parts.comment.as_deref(), Some("ea=$000008"));
}

#[test]
fn disasm_jmp_abs_long_endianness() {
    let mut ram = BigEndianRam::new(0x1000);
    // JMP (xxx).L: opcode 0x4EC0 | (7<<3) | 1 = 0x4EF9, ext long = 0x00123456
    write_words(&mut ram, 0x0000, &[0x4EF9, 0x0012, 0x3456]);

    let (line, bytes) = disasm::disassemble_one(&mut ram, 0x0000);
    assert_eq!(bytes, 6);
    assert!(line.contains("JMP"));
    assert!(line.contains("$00123456"));
}

#[test]
fn disasm_dbcc_comment_has_target() {
    let mut ram = BigEndianRam::new(0x1000);
    // DBNE D0, +2 (displacement relative to displacement word = addr+2 => 0x0002 + 2 = 0x0004)
    write_words(&mut ram, 0x0000, &[0x56C8, 0x0002]);

    let parts = disasm::disassemble_one_parts(&mut ram, 0x0000);
    assert_eq!(parts.bytes_consumed, 4);
    assert_eq!(parts.comment.as_deref(), Some("target=$000004"));
}

#[test]
fn disasm_lea_pc_disp_comment_has_ea() {
    let mut ram = BigEndianRam::new(0x1000);
    // LEA d16(PC), A0:
    // opcode: 0x41C0 | (A0<<9) | (mode=7<<3) | (reg=2) = 0x41FA
    // ext: +6, added to the extension word's address (0x0002): 0x0008
    write_words(&mut ram, 0x0000, &[0x41FA, 0x0006]);

    let parts = disasm::disassemble_one_parts(&mut ram, 0x0000);
    assert_eq!(parts.mnemonic, "LEA");
    assert_eq!(parts.bytes_consumed, 4);
    assert_eq!(parts.comment.as_deref(), Some("ea=$000008"));
}

#[test]
fn disasm_pc_index_comment_has_base() {
    let mut ram = BigEndianRam::new(0x1000);
    // LEA d8(PC, D1.W), A0: opcode 0x41FB, ext 0x1004 (D1.W, disp +4), the
    // displacement added to the extension word's address (0x0002): 0x0006
    write_words(&mut ram, 0x0000, &[0x41FB, 0x1004]);

    let parts = disasm::disassemble_one_parts(&mut ram, 0x0000);
    assert_eq!(parts.mnemonic, "LEA");
    assert_eq!(parts.bytes_consumed, 4);
    assert_eq!(parts.comment.as_deref(), Some("ea_base=$000006"));
}

#[test]
fn disasm_movea_long_immediate_is_32bit() {
    let mut ram = BigEndianRam::new(0x1000);
    // MOVEA.L #$10204FFF, A0
    // opcode: 0x207C, ext long: 0x1020_4FFF
    write_words(&mut ram, 0x0000, &[0x207C, 0x1020, 0x4FFF]);

    let parts = disasm::disassemble_one_parts(&mut ram, 0x0000);
    assert_eq!(parts.bytes_consumed, 6);
    assert_eq!(parts.mnemonic, "MOVEA.L");
    assert!(parts.operands.contains("#$10204FFF"));
    assert!(parts.operands.contains("A0"));
}

#[test]
#[ignore]
fn disasm_coverage_report() {
    let mut ram = BigEndianRam::new(0x1000);

    let mut dcw = 0u32;
    let mut other = 0u32;

    let mut examples_dcw: Vec<u16> = Vec::new();
    let mut missing: Vec<u16> = Vec::new();

    for opcode in 0u32..=0xFFFF {
        let opcode = opcode as u16;

        // Provide some deterministic extension words (all zeros).
        write_words(&mut ram, 0x0000, &[opcode, 0x0000, 0x0000, 0x0000]);

        let parts = disasm::disassemble_one_parts(&mut ram, 0x0000);

        match parts.mnemonic.as_str() {
            "DC.W" => {
                dcw += 1;
                missing.push(opcode);
                if examples_dcw.len() < 32 {
                    examples_dcw.push(opcode);
                }
            }
            _ => other += 1,
        }
    }

    println!("disasm coverage report:");
    println!("  DC.W  : {dcw}");
    println!("  other : {other}");
    println!("  sample DC.W  : {:04X?}", examples_dcw);

    // Compress missing opcodes into ranges for easier decoding work.
    missing.sort_unstable();
    let mut ranges: Vec<(u16, u16)> = Vec::new();
    for &op in &missing {
        if let Some((_start, end)) = ranges.last_mut() {
            if *end == op.wrapping_sub(1) {
                *end = op;
                continue;
            }
        }
        ranges.push((op, op));
    }

    println!("  DC.W ranges (first 64):");
    for (i, (s, e)) in ranges.iter().take(64).enumerate() {
        if s == e {
            println!("    {:02}: {:04X}", i, s);
        } else {
            println!(
                "    {:02}: {:04X}-{:04X} ({} opcodes)",
                i,
                s,
                e,
                (*e as u32 - *s as u32 + 1)
            );
        }
    }
}
